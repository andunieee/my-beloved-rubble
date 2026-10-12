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

use crate::db::{BlobRecord, Db, PathRecord, Remote};
use crate::repo::{self, Repo};
use crate::{rclone, remotes, util};
use age::secrecy::SecretString;
use std::collections::{HashMap, HashSet};
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

/// Where [`Repo::restore_metadata`] keeps the database as it was before,
/// so a restore can be undone by putting it back as `mbr.db`.
pub const BEFORE_RESTORE_FILE: &str = ".mbr/mbr.db.before-restore";

/// What [`Repo::restore_metadata`] did.
#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Files only the backup knew, whose names (symlinks) were recreated.
    pub files: usize,
    /// Older versions and removed files added to the history.
    pub history: usize,
    /// Remotes only the backup knew, now added.
    pub remotes: usize,
    /// Remotes named in both but storing in different places: ours was kept,
    /// and the backup's records of what it stores were ignored.
    pub conflicts: Vec<String>,
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

/// Download the sealed backup a remote holds. Run with the folder's rclone
/// config selected.
///
/// A remote set up in a new folder points at that folder's own directory
/// (see [`crate::remotes`]), while the backup of the lost folder sits in
/// the lost folder's directory next to it. So when there is none at
/// `target`, the other folder directories beside it are tried too.
pub fn download(target: &str, scratch: &Path) -> Result<Sealed, String> {
    if let Some(sealed) = download_at(target, scratch)? {
        return Ok(sealed);
    }
    for sibling in sibling_targets(target)? {
        if let Ok(Some(sealed)) = download_at(&sibling, scratch) {
            return Ok(sealed);
        }
    }
    Err("there is no metadata backup on this remote".to_owned())
}

/// The sealed backup under `target`, if there is one.
fn download_at(target: &str, scratch: &Path) -> Result<Option<Sealed>, String> {
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let fetch = |name: &str| -> Result<Option<Vec<u8>>, String> {
        let file = scratch.join(name);
        let result = match rclone::download_file(target, &object_path(name), &file) {
            Err(e) if rclone::is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
            Ok(()) => std::fs::read(&file)
                .map(Some)
                .map_err(|e| format!("cannot read {}: {e}", file.display())),
        };
        std::fs::remove_file(&file).ok();
        result
    };
    let Some(db) = fetch(DB_OBJECT)? else {
        return Ok(None);
    };
    let rclone_conf = fetch(CONFIG_OBJECT)?
        .ok_or_else(|| "the metadata backup lacks its rclone config".to_owned())?;
    Ok(Some(Sealed { db, rclone_conf }))
}

/// Targets for the other folders' directories next to the one `target`
/// stores in (only for remotes mbr set up).
fn sibling_targets(target: &str) -> Result<Vec<String>, String> {
    // Where the folder directory sits, and for an encrypted remote the
    // crypt settings to reach a sibling with, on the fly.
    let (location, crypt_options) = match remotes::protection(target) {
        remotes::Protection::Plain => (target.to_owned(), None),
        remotes::Protection::Encrypted => {
            let section = rclone::target_section(target).unwrap_or_default();
            let Some(config) = rclone::remote_config(section)? else {
                return Ok(Vec::new());
            };
            let location = config
                .get("remote")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let options: String = config
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "type" | "remote"))
                .map(|(key, value)| {
                    let value = rclone::display_value_for_ui(value);
                    format!(",{key}={}", rclone::quote_param(&value))
                })
                .collect();
            (location, Some(options))
        }
        remotes::Protection::Unknown => return Ok(Vec::new()),
    };
    // `section:dir`, `section:bucket/dir` or `section:/path/dir`.
    let Some(cut) = location.rfind(['/', ':']) else {
        return Ok(Vec::new());
    };
    let (parent, own) = (&location[..=cut], &location[cut + 1..]);
    if remotes::parse_folder_dir(own).is_none() {
        return Ok(Vec::new());
    }
    let mut siblings = Vec::new();
    for dir in rclone::list_dirs(parent)? {
        if dir != own && remotes::parse_folder_dir(&dir).is_some() {
            let sibling = format!("{parent}{dir}");
            siblings.push(match &crypt_options {
                Some(options) => format!(":crypt,remote={}{options}:", rclone::quote_param(&sibling)),
                None => sibling,
            });
        }
    }
    Ok(siblings)
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

    /// Whether this folder holds no files and no blobs yet.
    pub fn is_empty_folder(&mut self) -> Result<bool, String> {
        Ok(self.db.list_paths()?.is_empty() && self.db.list_blobs()?.is_empty())
    }

    /// Merge a decrypted backup into this folder's metadata, adding what
    /// only the backup knows: files and their history, blobs, remotes and
    /// what they store, metadata backup targets, settings and rclone
    /// sections. Where the two disagree, this folder wins:
    ///
    /// - a remote named in both but with different targets keeps ours, and
    ///   the backup's records of what that remote stores are ignored;
    /// - a file the backup has but this folder doesn't comes back as a
    ///   symlink into the blob store (dangling until its blob is
    ///   downloaded), unless we know of something newer at that path —
    ///   then the backup's version only joins the path's history.
    ///
    /// Into an empty folder, this restores the backup whole. The database
    /// as it was before is kept in [`BEFORE_RESTORE_FILE`].
    pub fn restore_metadata(
        &mut self,
        db_bytes: &[u8],
        rclone_conf: &[u8],
    ) -> Result<RestoreReport, String> {
        let rclone_conf = std::str::from_utf8(rclone_conf)
            .map_err(|_| "the backed-up rclone config is not text".to_owned())?;
        let backup = self.read_backup(db_bytes)?;

        // A new folder takes on the lost one's id, so the remotes set up
        // here reach what the lost folder stored.
        if self.is_empty_folder()?
            && let Some((_, id)) = backup
                .settings
                .iter()
                .find(|(key, _)| key == remotes::FOLDER_ID_SETTING)
        {
            self.adopt_folder_id(id)?;
        }

        // Credentials first: adding rclone sections can't break anything.
        let current = std::fs::read_to_string(self.rclone_config_path()).unwrap_or_default();
        // A crypt layer of the same name over a different place is a
        // different remote, even with the same target.
        let mut moved: HashSet<String> = HashSet::new();
        for remote in &backup.remotes {
            if let Some(section) = rclone::target_section(&remote.target)
                && let (Some(ours), Some(theirs)) = (
                    section_value(&current, section, "remote"),
                    section_value(rclone_conf, section, "remote"),
                )
                && ours != theirs
            {
                moved.insert(remote.name.clone());
            }
        }
        let merged = merge_rclone_configs(&current, rclone_conf);
        std::fs::write(self.rclone_config_path(), merged)
            .map_err(|e| format!("cannot write {}: {e}", repo::RCLONE_CONFIG_FILE))?;
        repo::ensure_rclone_config(&self.rclone_config_path())?;
        rclone::forget_cached_remotes()?;

        let before = self.root.join(BEFORE_RESTORE_FILE);
        std::fs::remove_file(&before).ok();
        self.db
            .snapshot_to(&before)
            .map_err(|e| format!("cannot save {BEFORE_RESTORE_FILE}: {e}"))?;

        let now = util::now();
        let mut report = RestoreReport::default();
        let relink = self
            .db
            .transaction(|db| merge_backup(db, &backup, &moved, now, &mut report))?;

        for path in relink {
            let Some(record) = self.db.active_path(&path)? else {
                continue;
            };
            match self.recreate_link(&record.path, &record.hash) {
                Ok(()) => report.files += 1,
                Err(e) => {
                    // Keep the database true to the folder.
                    self.db.mark_path_removed(record.id, now)?;
                    report.skipped.push((path, e));
                }
            }
        }
        Ok(report)
    }

    /// Everything in a decrypted backup database, checking on the way that
    /// it really is an mbr database.
    fn read_backup(&self, db_bytes: &[u8]) -> Result<BackupContents, String> {
        let dir = self.scratch_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let staged = dir.join("restore.db");
        std::fs::remove_file(&staged).ok();
        std::fs::write(&staged, db_bytes)
            .map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
        let contents = Db::open(&staged).and_then(|mut db| {
            Ok(BackupContents {
                paths: db.list_paths()?,
                blobs: db.list_blobs()?,
                remotes: db.list_remotes()?,
                blob_remotes: db.list_blob_remotes()?,
                metadata_remotes: db.metadata_remotes()?.into_iter().map(|r| r.name).collect(),
                settings: db.list_settings()?,
            })
        });
        std::fs::remove_file(&staged).ok();
        contents.map_err(|e| format!("the backup is not a usable mbr database: {e}"))
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

/// The tables of a backed-up database.
struct BackupContents {
    paths: Vec<PathRecord>,
    blobs: Vec<BlobRecord>,
    remotes: Vec<Remote>,
    blob_remotes: Vec<(String, String)>,
    metadata_remotes: Vec<String>,
    settings: Vec<(String, String)>,
}

/// The database half of [`Repo::restore_metadata`], run in one transaction.
/// Returns the paths made active, whose symlinks are still to be created.
fn merge_backup(
    db: &mut Db,
    backup: &BackupContents,
    moved: &HashSet<String>,
    now: i64,
    report: &mut RestoreReport,
) -> Result<Vec<String>, String> {
    let mut conflicts: HashSet<&str> = HashSet::new();
    for remote in &backup.remotes {
        match db.remote(&remote.name)? {
            None => {
                db.add_remote(&remote.name, &remote.target)?;
                report.remotes += 1;
            }
            Some(ours) if ours.target != remote.target || moved.contains(&remote.name) => {
                conflicts.insert(&remote.name);
                report.conflicts.push(remote.name.clone());
            }
            Some(_) => {}
        }
    }
    let trusted: HashSet<&str> = backup
        .remotes
        .iter()
        .map(|r| r.name.as_str())
        .filter(|name| !conflicts.contains(name))
        .collect();

    for blob in &backup.blobs {
        if !db.has_blob(&blob.hash)? {
            db.upsert_blob(&blob.hash, blob.size)?;
        }
    }
    for (hash, remote) in &backup.blob_remotes {
        if trusted.contains(remote.as_str()) {
            db.set_blob_on_remote(hash, remote)?;
        }
    }
    for name in &backup.metadata_remotes {
        if trusted.contains(name.as_str()) {
            db.set_metadata_remote(name, true)?;
        }
    }
    let settings: HashSet<String> = db.list_settings()?.into_iter().map(|(k, _)| k).collect();
    for (key, value) in &backup.settings {
        if !settings.contains(key) {
            db.set_setting(key, value)?;
        }
    }

    let ours = db.list_paths()?;
    let known: HashSet<(&str, &str, i64)> = ours
        .iter()
        .map(|r| (r.path.as_str(), r.hash.as_str(), r.added_at))
        .collect();
    let active: HashMap<&str, &PathRecord> = ours
        .iter()
        .filter(|r| r.is_active())
        .map(|r| (r.path.as_str(), r))
        .collect();
    // The latest time anything happened at each path, as far as we know.
    let mut latest: HashMap<&str, i64> = HashMap::new();
    for r in &ours {
        let t = r.removed_at.unwrap_or(r.added_at).max(r.added_at);
        let entry = latest.entry(r.path.as_str()).or_insert(t);
        *entry = (*entry).max(t);
    }

    let mut relink = Vec::new();
    for record in &backup.paths {
        let (path, hash) = (record.path.as_str(), record.hash.as_str());
        if known.contains(&(path, hash, record.added_at)) {
            continue;
        }
        if record.removed_at.is_some() {
            db.insert_path_record(path, hash, record.added_at, record.removed_at)?;
            report.history += 1;
            continue;
        }
        if let Some(current) = active.get(path).filter(|c| c.hash == record.hash) {
            // The same file: remember when it was first added.
            if record.added_at < current.added_at {
                db.set_path_added_at(current.id, record.added_at)?;
            }
            continue;
        }
        let newer = latest.get(path).copied().filter(|&t| t >= record.added_at);
        if newer.is_none() && !active.contains_key(path) {
            db.insert_path_record(path, hash, record.added_at, None)?;
            relink.push(record.path.clone());
        } else {
            // This folder has moved on: the backup's version is history.
            db.insert_path_record(path, hash, record.added_at, Some(newer.unwrap_or(now)))?;
            report.history += 1;
        }
    }
    Ok(relink)
}

/// `preferred` with every section of `other` it lacks appended:
/// `preferred` wins where both define a remote.
fn merge_rclone_configs(preferred: &str, other: &str) -> String {
    let known: HashSet<&str> = sections(preferred).map(|(name, _)| name).collect();
    let mut merged = preferred.trim_end().to_owned();
    for (name, text) in sections(other) {
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

/// The value of `key` in section `[section]` of an rclone config.
fn section_value<'a>(config: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let (_, text) = sections(config).find(|(name, _)| *name == section)?;
    text.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim())
    })
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
    fn merging_configs_keeps_ours_and_adds_the_rest() {
        let current = "[drive]\ntype = drive\ntoken = new\n[usb]\ntype = local\n";
        let backup = "[drive]\ntype = drive\ntoken = old\n\n[s3]\ntype = s3\n";
        let merged = merge_rclone_configs(current, backup);
        assert_eq!(
            merged,
            "[drive]\ntype = drive\ntoken = new\n[usb]\ntype = local\n\n[s3]\ntype = s3\n"
        );
        assert_eq!(merge_rclone_configs("", current), current.replace("new\n[", "new\n\n["));
    }
}
