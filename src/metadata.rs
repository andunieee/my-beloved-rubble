//! Encrypted metadata backups.
//!
//! Blobs on a remote are named only by hash, so without `.mbr/mbr.db` a
//! remote is a heap of anonymous files. The database is therefore backed
//! up on its own terms, together with the rclone config (the credentials
//! of every remote, to restore with): both go in one tar.gz, encrypted
//! with the folder's master password, stored at the root of each remote
//! chosen for it (next to the folder's own `mbr-<id>` directory) as
//!
//! ```text
//! mbr-metadata-<backup id>.tar.gz.encrypted
//! ```
//!
//! where the backup id ([`backup_name`]) is derived from the master
//! password alone: a lost folder is found again with the password and the
//! remote's settings, without knowing anything else about it.
//!
//! The file is an [age](https://age-encryption.org) file encrypted to the
//! password (scrypt), so it can also be opened without mbr:
//! `age -d mbr-metadata-….tar.gz.encrypted | tar xz`.
//!
//! Each new backup replaces the previous one. When each remote was last
//! backed up to is local state ([`crate::state`]), not part of the backup.
//!
//! A backup is restored into a folder that is not an mbr folder yet
//! ([`restore`]), never merged into one in use.

use crate::db::Db;
use crate::repo::{self, Repo};
use crate::{rclone, util};
use age::secrecy::SecretString;
use std::io::Read;
use std::path::{Component, Path};

/// Shortest master password accepted.
pub const MIN_PASSWORD_LEN: usize = 8;
/// The database's name inside the tarball.
pub const DB_ENTRY: &str = "mbr.db";
/// The rclone config's name inside the tarball.
pub const CONFIG_ENTRY: &str = "rclone.conf";

/// A consistent copy of the folder's metadata, in the clear.
pub struct Snapshot {
    pub db: Vec<u8>,
    pub rclone_conf: Vec<u8>,
    /// SHA-256 of `mbr.db` at the time of the snapshot.
    pub db_hash: String,
}

/// What [`restore`] did.
#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Files whose names (symlinks) were recreated.
    pub files: usize,
    /// Files whose names were taken by something already in the folder:
    /// the next scan records that as a new version.
    pub kept: Vec<String>,
    /// Files that could not be recreated, with the reason.
    pub skipped: Vec<(String, String)>,
}

// ── the master password ──

/// Refuse passwords too weak to protect a backup stored in the cloud.
pub fn check_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!(
            "the master password must have at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

/// A slow hash of `password` (Argon2id, PHC string) to recognize it by later
/// without storing it.
pub fn password_check(password: &str) -> Result<String, String> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|e| format!("cannot hash password: {e}"))
}

/// Whether `password` is the one `check` was made from.
pub fn password_matches(check: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(check).is_ok_and(|hash| {
        argon2::Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// The file name of the backups made with `password`:
/// `mbr-metadata-<16 hex digits>.tar.gz.encrypted`, the digits being 8
/// bytes of a hash of the password. The hash is Argon2id, as slow as the
/// encryption itself, so the name doesn't make the password any easier to
/// guess than the content does.
pub fn backup_name(password: &str) -> Result<String, String> {
    let mut id = [0u8; 8];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), b"mbr-metadata-backup-name", &mut id)
        .map_err(|e| format!("cannot hash password: {e}"))?;
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("mbr-metadata-{hex}.tar.gz.encrypted"))
}

// ── encryption ──

pub fn encrypt(password: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let recipient = age::scrypt::Recipient::new(SecretString::from(password.to_owned()));
    age::encrypt(&recipient, plaintext).map_err(|e| format!("encryption failed: {e}"))
}

pub fn decrypt(password: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let identity = age::scrypt::Identity::new(SecretString::from(password.to_owned()));
    age::decrypt(&identity, ciphertext).map_err(|e| match e {
        age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys => {
            "wrong master password (or a damaged backup)".to_owned()
        }
        e => format!("cannot decrypt the backup: {e}"),
    })
}

/// Pack and encrypt a snapshot with the master password. Slow on purpose
/// (scrypt): run it off the UI thread.
pub fn seal(snapshot: &Snapshot, password: &str) -> Result<Vec<u8>, String> {
    encrypt(password, &pack(&snapshot.db, &snapshot.rclone_conf)?)
}

/// Decrypt and unpack a backup: `(database, rclone config)`.
pub fn unseal(sealed: &[u8], password: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    unpack(&decrypt(password, sealed)?)
}

/// The database and rclone config as a tar.gz.
fn pack(db: &[u8], rclone_conf: &[u8]) -> Result<Vec<u8>, String> {
    let fail = |e: std::io::Error| format!("cannot pack the metadata: {e}");
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (name, bytes) in [(DB_ENTRY, db), (CONFIG_ENTRY, rclone_conf)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(util::now().max(0) as u64);
        tar.append_data(&mut header, name, bytes).map_err(fail)?;
    }
    tar.into_inner().and_then(|gz| gz.finish()).map_err(fail)
}

/// The database and rclone config in a tar.gz made by [`pack`].
fn unpack(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let fail = |e: std::io::Error| format!("the backup is not a metadata tarball: {e}");
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let (mut db, mut rclone_conf) = (None, None);
    for entry in archive.entries().map_err(fail)? {
        let mut entry = entry.map_err(fail)?;
        let name = entry.path().map_err(fail)?.to_string_lossy().into_owned();
        let slot = match name.as_str() {
            DB_ENTRY => &mut db,
            CONFIG_ENTRY => &mut rclone_conf,
            _ => continue,
        };
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(fail)?;
        *slot = Some(data);
    }
    match (db, rclone_conf) {
        (Some(db), Some(rclone_conf)) => Ok((db, rclone_conf)),
        _ => Err(format!("the backup lacks {DB_ENTRY} or {CONFIG_ENTRY}")),
    }
}

// ── moving backups (rclone; safe off-thread) ──

/// Upload a sealed backup to `location` (see
/// [`crate::remotes::backup_location`]) as `name`, using `scratch` (a local
/// directory) for the file rclone copies from.
pub fn upload(location: &str, name: &str, sealed: &[u8], scratch: &Path) -> Result<(), String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let file = scratch.join(name);
    let result = std::fs::write(&file, sealed)
        .map_err(|e| format!("cannot write {}: {e}", file.display()))
        .and_then(|()| rclone::upload_file(location, &file, name));
    std::fs::remove_file(&file).ok();
    result
}

/// Download the sealed backup named `name` from `location`.
pub fn download(location: &str, name: &str, scratch: &Path) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let file = scratch.join(name);
    let result = match rclone::download_file(location, name, &file) {
        Err(e) if rclone::is_not_found(&e) => Err(
            "there is no metadata backup made with this master password there".to_owned(),
        ),
        Err(e) => Err(e),
        Ok(()) => std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display())),
    };
    std::fs::remove_file(&file).ok();
    result
}

// ── restoring ──

/// Make `dir` the mbr folder the backup describes: its database and rclone
/// config become the folder's, and every file it tracks comes back as a
/// symlink into the blob store, dangling until its blob is downloaded.
/// `dir` must not be an mbr folder already.
pub fn restore(dir: &Path, db: &[u8], rclone_conf: &[u8]) -> Result<(Repo, RestoreReport), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if dir.join(repo::DB_FILE).exists() {
        return Err(format!(
            "{} is already an mbr folder: restore into a new one",
            dir.display()
        ));
    }
    if std::str::from_utf8(rclone_conf).is_err() {
        return Err("the rclone config is not a text file".to_owned());
    }

    // Check it really is an mbr database before putting anything in place.
    let scratch = dir.join(repo::TMP_DIR);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let staged = scratch.join("restore.db");
    std::fs::remove_file(&staged).ok();
    std::fs::write(&staged, db).map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    if let Err(e) = Db::open(&staged).and_then(|mut db| db.list_paths()) {
        std::fs::remove_file(&staged).ok();
        return Err(format!("not an mbr database: {e}"));
    }

    // The database last: until it is in place, this is no mbr folder.
    let config = dir.join(repo::RCLONE_CONFIG_FILE);
    std::fs::write(&config, rclone_conf)
        .map_err(|e| format!("cannot write {}: {e}", config.display()))?;
    repo::ensure_rclone_config(&config)?;
    std::fs::create_dir_all(dir.join(repo::BLOBS_DIR))
        .map_err(|e| format!("cannot create {}: {e}", repo::BLOBS_DIR))?;
    std::fs::rename(&staged, dir.join(repo::DB_FILE))
        .map_err(|e| format!("cannot put the database in place: {e}"))?;
    // rclone may still hold remotes of an earlier config at this path.
    rclone::forget_cached_remotes()?;

    let mut repo = Repo::open(dir)?;
    let mut report = RestoreReport::default();
    for record in repo.db.list_paths()? {
        if !record.is_active() {
            continue;
        }
        match recreate_link(&repo.root, &record.path, &record.hash) {
            Ok(true) => report.files += 1,
            Ok(false) => report.kept.push(record.path),
            Err(e) => report.skipped.push((record.path, e)),
        }
    }
    Ok((repo, report))
}

/// Create the symlink for an active path: `false` if something is there.
fn recreate_link(root: &Path, path: &str, hash: &str) -> Result<bool, String> {
    let rel = Path::new(path);
    let safe = rel
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !safe || !util::is_hash(hash) {
        return Err("not a valid tracked path".to_owned());
    }
    let abs = root.join(rel);
    if std::fs::symlink_metadata(&abs).is_ok() {
        return Ok(false);
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    repo::link_blob(root, rel, hash).map(|()| true)
}

// ── the repository side ──

impl Repo {
    /// Where sealed backups are staged before upload.
    pub fn scratch_dir(&self) -> std::path::PathBuf {
        self.root.join(repo::TMP_DIR)
    }

    /// SHA-256 of `mbr.db` as it is on disk now.
    pub fn db_hash(&self) -> Result<String, String> {
        util::sha256_file(&self.root.join(repo::DB_FILE))
    }

    /// A consistent copy of the database and rclone config. Run it on the
    /// thread that owns the database, between writes.
    pub fn metadata_snapshot(&mut self) -> Result<Snapshot, String> {
        let dir = self.scratch_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let copy = dir.join(format!("snapshot-{}.db", std::process::id()));
        std::fs::remove_file(&copy).ok();
        let db_hash = self.db_hash()?;
        let db = self
            .db
            .snapshot_to(&copy)
            .and_then(|()| std::fs::read(&copy).map_err(|e| format!("cannot read snapshot: {e}")));
        std::fs::remove_file(&copy).ok();
        let rclone_conf = std::fs::read(self.rclone_config_path())
            .map_err(|e| format!("cannot read {}: {e}", repo::RCLONE_CONFIG_FILE))?;
        Ok(Snapshot {
            db: db?,
            rclone_conf,
            db_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_recognized() {
        let check = password_check("correct horse").unwrap();
        assert!(password_matches(&check, "correct horse"));
        assert!(!password_matches(&check, "correct house"));
        assert!(!password_matches("garbage", "correct horse"));
        assert!(check_new_password("short").is_err());
        assert!(check_new_password("long enough").is_ok());
    }

    #[test]
    fn backup_names_depend_on_the_password_only() {
        let name = backup_name("correct horse").unwrap();
        assert_eq!(name, backup_name("correct horse").unwrap());
        assert_ne!(name, backup_name("correct house").unwrap());
        let id = name
            .strip_prefix("mbr-metadata-")
            .and_then(|rest| rest.strip_suffix(".tar.gz.encrypted"))
            .unwrap();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sealing_round_trips_and_rejects_wrong_passwords() {
        let snapshot = Snapshot {
            db: b"database".to_vec(),
            rclone_conf: b"[remote]\n".to_vec(),
            db_hash: String::new(),
        };
        let sealed = seal(&snapshot, "correct horse").unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"database"));
        let (db, conf) = unseal(&sealed, "correct horse").unwrap();
        assert_eq!((db.as_slice(), conf.as_slice()), (&b"database"[..], &b"[remote]\n"[..]));
        let err = unseal(&sealed, "correct house").unwrap_err();
        assert!(err.contains("wrong master password"), "{err}");
        let err = unseal(&encrypt("correct horse", b"not a tarball").unwrap(), "correct horse");
        assert!(err.is_err());
    }
}
