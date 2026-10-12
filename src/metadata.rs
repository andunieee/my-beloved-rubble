//! Encrypted metadata backups.
//!
//! Blobs on a remote are named only by hash, so without `.mbr/mbr.db` a
//! remote is a heap of anonymous files. The database is therefore backed
//! up on its own terms: not content-addressed like a blob, but encrypted
//! with the folder's master password and stored at a fixed name on each
//! remote chosen for it, next to the blobs:
//!
//! ```text
//! <target>/mbr-metadata/mbr.db.age        the database
//! <target>/mbr-metadata/rclone.conf.age   the rclone config (credentials
//!                                         of every remote, to restore with)
//! ```
//!
//! Both are [age](https://age-encryption.org) files encrypted to the
//! password (scrypt), so they can also be opened without mbr:
//! `age -d mbr.db.age > mbr.db`.
//!
//! Each new backup replaces the previous one. When each remote was last
//! backed up to is local state ([`crate::state`]), not part of the backup.

use crate::db::Db;
use crate::repo::{self, Repo};
use crate::{rclone, util};
use age::secrecy::SecretString;
use std::collections::HashSet;
use std::path::{Component, Path};

/// Folder on each remote that holds the backup.
pub const REMOTE_DIR: &str = "mbr-metadata";
pub const DB_OBJECT: &str = "mbr.db.age";
pub const CONFIG_OBJECT: &str = "rclone.conf.age";
/// Shortest master password accepted.
pub const MIN_PASSWORD_LEN: usize = 8;

/// A consistent copy of the folder's metadata, in the clear.
pub struct Snapshot {
    pub db: Vec<u8>,
    pub rclone_conf: Vec<u8>,
    /// SHA-256 of `mbr.db` at the time of the snapshot.
    pub db_hash: String,
}

/// The encrypted form of a [`Snapshot`], as stored on remotes.
#[derive(Clone)]
pub struct Sealed {
    pub db: Vec<u8>,
    pub rclone_conf: Vec<u8>,
}

/// What [`Repo::restore_metadata`] did.
#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Active files whose names (symlinks) were recreated.
    pub files: usize,
    /// Remotes known after the restore.
    pub remotes: usize,
    /// Active paths that could not be recreated, with the reason.
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

/// Encrypt a snapshot with the master password. Slow on purpose (scrypt):
/// run it off the UI thread.
pub fn seal(snapshot: &Snapshot, password: &str) -> Result<Sealed, String> {
    Ok(Sealed {
        db: encrypt(password, &snapshot.db)?,
        rclone_conf: encrypt(password, &snapshot.rclone_conf)?,
    })
}

/// Decrypt a backup: `(database, rclone config)`.
pub fn unseal(sealed: &Sealed, password: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    Ok((
        decrypt(password, &sealed.db)?,
        decrypt(password, &sealed.rclone_conf)?,
    ))
}

// ── moving backups (rclone; safe off-thread) ──

fn object_path(name: &str) -> String {
    format!("{REMOTE_DIR}/{name}")
}

/// Upload a sealed backup to `target`, using `scratch` (a local directory)
/// for the files rclone copies from. The rclone config goes first, so a
/// remote never holds a database whose credentials it lacks.
pub fn upload(target: &str, sealed: &Sealed, scratch: &Path) -> Result<(), String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    for (name, bytes) in [(CONFIG_OBJECT, &sealed.rclone_conf), (DB_OBJECT, &sealed.db)] {
        let file = scratch.join(name);
        let result = std::fs::write(&file, bytes)
            .map_err(|e| format!("cannot write {}: {e}", file.display()))
            .and_then(|()| rclone::upload_file(target, &file, &object_path(name)));
        std::fs::remove_file(&file).ok();
        result?;
    }
    Ok(())
}

/// Download the sealed backup stored under `target`.
pub fn download(target: &str, scratch: &Path) -> Result<Sealed, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let fetch = |name: &str| -> Result<Vec<u8>, String> {
        let file = scratch.join(name);
        let result = match rclone::download_file(target, &object_path(name), &file) {
            Err(e) if rclone::is_not_found(&e) => {
                Err(format!("there is no metadata backup at {target}"))
            }
            other => other,
        }
        .and_then(|()| {
            std::fs::read(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))
        });
        std::fs::remove_file(&file).ok();
        result
    };
    Ok(Sealed {
        db: fetch(DB_OBJECT)?,
        rclone_conf: fetch(CONFIG_OBJECT)?,
    })
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

    /// Whether this folder holds no files and no blobs yet, so restoring a
    /// backup into it can't lose anything.
    pub fn is_empty_folder(&mut self) -> Result<bool, String> {
        Ok(self.db.list_paths()?.is_empty() && self.db.list_blobs()?.is_empty())
    }

    /// Replace this (empty) folder's metadata with a decrypted backup, and
    /// recreate every active file as a symlink into the blob store —
    /// dangling until its blob is downloaded.
    ///
    /// Remotes configured here before the restore (typically the one the
    /// backup was fetched from) are kept when the backup doesn't know them,
    /// and so are their rclone settings.
    pub fn restore_metadata(
        &mut self,
        db_bytes: &[u8],
        rclone_conf: &[u8],
    ) -> Result<RestoreReport, String> {
        if !self.is_empty_folder()? {
            return Err(
                "metadata can only be restored into a folder that tracks no files yet".to_owned(),
            );
        }
        let rclone_conf = std::str::from_utf8(rclone_conf)
            .map_err(|_| "the backed-up rclone config is not text".to_owned())?;
        let before = self.db.list_remotes()?;

        // Check the backup really is an mbr database before replacing ours.
        let dir = self.scratch_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let staged = dir.join("restore.db");
        std::fs::write(&staged, db_bytes)
            .map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
        if let Err(e) = Db::open(&staged).and_then(|mut db| db.list_paths()) {
            std::fs::remove_file(&staged).ok();
            return Err(format!("the backup is not a usable mbr database: {e}"));
        }

        let db_path = self.root.join(repo::DB_FILE);
        self.db = Db::open_in_memory()?;
        let swapped = std::fs::rename(&staged, &db_path)
            .map_err(|e| format!("cannot replace {}: {e}", repo::DB_FILE));
        // Reopen whichever database is now in place, even after a failure.
        self.db = Db::open(&db_path)?;
        swapped?;

        let current = std::fs::read_to_string(self.rclone_config_path()).unwrap_or_default();
        let merged = merge_rclone_configs(rclone_conf, &current);
        std::fs::write(self.rclone_config_path(), merged)
            .map_err(|e| format!("cannot write {}: {e}", repo::RCLONE_CONFIG_FILE))?;
        repo::ensure_rclone_config(&self.rclone_config_path())?;

        for remote in before {
            if self.db.remote(&remote.name)?.is_none() {
                self.db.add_remote(&remote.name, &remote.target)?;
            }
        }

        let mut report = RestoreReport {
            remotes: self.db.list_remotes()?.len(),
            ..RestoreReport::default()
        };
        for record in self.db.list_paths()?.into_iter().filter(|p| p.is_active()) {
            match self.recreate_link(&record.path, &record.hash) {
                Ok(()) => report.files += 1,
                Err(e) => report.skipped.push((record.path, e)),
            }
        }
        Ok(report)
    }

    /// Create the symlink for an active path, unless something is there.
    fn recreate_link(&self, path: &str, hash: &str) -> Result<(), String> {
        let rel = Path::new(path);
        let safe = rel
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        if !safe || !util::is_hash(hash) {
            return Err("not a valid tracked path".to_owned());
        }
        let abs = self.root.join(rel);
        if std::fs::symlink_metadata(&abs).is_ok() {
            return Err("something already exists there".to_owned());
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        repo::link_blob(&self.root, rel, hash)
    }
}

/// `backup` with every section of `current` it lacks appended: the backup
/// wins where both define a remote.
fn merge_rclone_configs(backup: &str, current: &str) -> String {
    let known: HashSet<&str> = sections(backup).map(|(name, _)| name).collect();
    let mut merged = backup.trim_end().to_owned();
    for (name, text) in sections(current) {
        if !known.contains(name) {
            if !merged.is_empty() {
                merged.push_str("\n\n");
            }
            merged.push_str(text.trim_end());
        }
    }
    merged.push('\n');
    merged
}

/// The `[name]` sections of an rclone config, with their full text.
fn sections(config: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut starts: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in config.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            starts.push((offset, name));
        }
        offset += line.len();
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|(start, _)| *start)
        .chain(std::iter::once(config.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .map(move |((start, name), end)| (name, &config[start..end]))
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
    fn encryption_round_trips_and_rejects_wrong_passwords() {
        let sealed = encrypt("correct horse", b"metadata").unwrap();
        assert_ne!(sealed, b"metadata");
        assert_eq!(decrypt("correct horse", &sealed).unwrap(), b"metadata");
        let err = decrypt("correct house", &sealed).unwrap_err();
        assert!(err.contains("wrong master password"), "{err}");
    }

    #[test]
    fn merging_configs_keeps_local_only_remotes() {
        let backup = "[drive]\ntype = drive\ntoken = old\n\n[s3]\ntype = s3\n";
        let current = "[drive]\ntype = drive\ntoken = new\n[usb]\ntype = local\n";
        let merged = merge_rclone_configs(backup, current);
        assert_eq!(
            merged,
            "[drive]\ntype = drive\ntoken = old\n\n[s3]\ntype = s3\n\n[usb]\ntype = local\n"
        );
        assert_eq!(merge_rclone_configs("", current), current.replace("new\n[", "new\n\n["));
    }
}
