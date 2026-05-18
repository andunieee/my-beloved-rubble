//! A rubble folder: scanning, the `.mbr/blobs` content store, and the
//! metadata-backed file listing.

use crate::db::{Db, PathRecord, Remote};
use crate::{rclone, util};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const BLOBS_DIR: &str = ".mbr/blobs";
pub const DB_FILE: &str = ".mbr/mbr.db";
pub const RCLONE_CONFIG_FILE: &str = ".mbr/rclone.conf";
/// Folder (relative to the root) where blobs found on a remote, but never
/// seen before, get a name: `unnamed/<hash>`.
pub const UNNAMED_DIR: &str = "unnamed";

pub struct Repo {
    pub root: PathBuf,
    pub db: Db,
}

/// What a scan did.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Paths seen for the first time (moved into the blob store now, or
    /// pre-existing blob symlinks adopted into the database).
    pub added: Vec<String>,
    /// Paths that already existed but now hold different content; the old
    /// row was closed and a new one opened ("previous version").
    pub replaced: Vec<String>,
    /// Tracked paths that no longer exist on disk, now marked removed.
    pub removed: Vec<String>,
}

impl ScanReport {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.replaced.is_empty() && self.removed.is_empty()
    }
}

/// Everything the UIs display about one active file.
#[derive(Debug, Clone)]
pub struct FileStatus {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub added_at: i64,
    /// The blob file exists in the local `.mbr/blobs` store.
    pub present_locally: bool,
    /// Remotes recorded as storing this blob.
    pub remotes: Vec<String>,
    /// Other active paths pointing at the same blob.
    pub other_names: Vec<String>,
    /// Paths that pointed at the same blob in the past (now removed).
    pub past_names: Vec<String>,
    /// Blobs this path pointed at before the current one, newest first.
    pub previous_versions: Vec<PreviousVersion>,
}

/// A blob a path pointed at in the past.
#[derive(Debug, Clone)]
pub struct PreviousVersion {
    pub hash: String,
    pub added_at: i64,
    pub removed_at: i64,
    pub size: u64,
    pub present_locally: bool,
    pub remotes: Vec<String>,
}

impl PreviousVersion {
    /// Where this old blob is still stored, for display.
    pub fn stored_summary(&self) -> String {
        match (self.present_locally, self.remotes.is_empty()) {
            (true, true) => "local only".to_owned(),
            (true, false) => format!("local, {}", self.remotes.join(", ")),
            (false, true) => "NOT STORED ANYWHERE".to_owned(),
            (false, false) => self.remotes.join(", "),
        }
    }
}

/// Result of scanning a remote and reconciling the database with it.
#[derive(Debug, Default)]
pub struct RemoteCheck {
    /// Blobs we expected on the remote and found.
    pub present: Vec<String>,
    /// Blobs we expected on the remote but did not find (unrecorded in db).
    pub missing: Vec<String>,
    /// Known blobs found on the remote that we did not expect there
    /// (recorded in db).
    pub discovered: Vec<String>,
    /// Blobs found on the remote that the database had never heard of:
    /// recorded, and given the name `unnamed/<hash>`.
    pub adopted: Vec<String>,
}

/// One active path found on disk by [`ingest_tree`].
#[derive(Debug, Clone)]
pub struct ScannedEntry {
    /// Path relative to the root, `/`-separated.
    pub path: String,
    pub hash: String,
    /// Size of the local blob, if it is present.
    pub size: Option<u64>,
}

enum Entry {
    File(PathBuf),
    /// A symlink into `.mbr/blobs`, carrying the hash it names.
    BlobLink(PathBuf, String),
}

/// Suffix of the hidden symlink staged next to a file while it is being
/// ingested (`dir/.name.mbr-ingest`). If mbr dies after moving the file
/// into the store but before renaming the link into place, the next scan
/// finds the staged link and finishes the job.
const STAGED_SUFFIX: &str = ".mbr-ingest";

impl Repo {
    /// Attach mbr to `root`: create the `.mbr` store, database, and
    /// repository-local rclone config (fresh folders only; no migration).
    pub fn init(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        std::fs::create_dir_all(root.join(BLOBS_DIR))
            .map_err(|e| format!("cannot create {BLOBS_DIR}: {e}"))?;
        let root = root
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
        ensure_rclone_config(&root.join(RCLONE_CONFIG_FILE))?;
        let db = Db::open(&root.join(DB_FILE))?;
        Ok(Self { root, db })
    }

    /// Whether `root` already has the `.mbr` store and database.
    pub fn is_initialized(root: &Path) -> bool {
        root.join(BLOBS_DIR).is_dir() && root.join(DB_FILE).is_file()
    }

    /// Open an already-attached folder (or one being initialized).
    pub fn open(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
        if !Self::is_initialized(&root) {
            return Err(format!(
                "{} is not an mbr folder (run `mbr init` there first)",
                root.display()
            ));
        }
        let db = Db::open(&root.join(DB_FILE))?;
        Ok(Self { root, db })
    }

    /// Walk up from `start` to find the nearest attached folder.
    pub fn discover(start: &Path) -> Result<Self, String> {
        let start = start
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", start.display()))?;
        let mut dir = start.as_path();
        loop {
            if dir.join(DB_FILE).is_file() {
                return Self::open(dir);
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => {
                    return Err(format!(
                        "no mbr folder found at or above {} (run `mbr init` first)",
                        start.display()
                    ));
                }
            }
        }
    }

    // ── blob store ──

    /// Local path of a blob: `.mbr/blobs/aa/bb/<hash>`.
    pub fn blob_path(&self, hash: &str) -> PathBuf {
        blob_path_in(&self.root, hash)
    }

    /// The rclone config owned by this repository.
    pub fn rclone_config_path(&self) -> PathBuf {
        self.root.join(RCLONE_CONFIG_FILE)
    }

    /// Select this repository's rclone configuration for librclone.
    pub fn configure_rclone(&self) -> Result<(), String> {
        let path = self.rclone_config_path();
        ensure_rclone_config(&path)?;
        rclone::set_config_path(&path)
    }

    /// Run `f` with librclone using this repository's configuration (see
    /// [`rclone::with_config`]).
    pub fn with_rclone<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let path = self.rclone_config_path();
        ensure_rclone_config(&path)?;
        rclone::with_config(&path, f)
    }

    pub fn blob_present(&self, hash: &str) -> bool {
        self.blob_path(hash).is_file()
    }

    // ── scanning ──

    /// Inspect the folder: move new regular files into the blob store
    /// (leaving symlinks behind), adopt untracked blob symlinks, and mark
    /// vanished paths as removed.
    pub fn scan(&mut self) -> Result<ScanReport, String> {
        let entries = ingest_tree(&self.root)?;
        self.apply_scan(entries)
    }

    /// The database half of [`Repo::scan`]: record what [`ingest_tree`]
    /// found, in one transaction. Paths tracked but absent from `entries`
    /// are marked removed.
    pub fn apply_scan(&mut self, entries: Vec<ScannedEntry>) -> Result<ScanReport, String> {
        let now = util::now();
        self.db.transaction(|db| {
            let mut report = ScanReport::default();
            let mut active: HashMap<String, PathRecord> = db
                .list_paths()?
                .into_iter()
                .filter(PathRecord::is_active)
                .map(|r| (r.path.clone(), r))
                .collect();

            for entry in entries {
                if let Some(size) = entry.size {
                    db.upsert_blob(&entry.hash, size)?;
                }
                match active.remove(&entry.path) {
                    Some(record) if record.hash == entry.hash => {}
                    Some(record) => {
                        db.mark_path_removed(record.id, now)?;
                        db.insert_path(&entry.path, &entry.hash, now)?;
                        report.replaced.push(entry.path);
                    }
                    None => {
                        db.insert_path(&entry.path, &entry.hash, now)?;
                        report.added.push(entry.path);
                    }
                }
            }

            // Anything still tracked but not seen on disk is a removal.
            let mut gone: Vec<PathRecord> = active.into_values().collect();
            gone.sort_by(|a, b| a.path.cmp(&b.path));
            for record in gone {
                db.mark_path_removed(record.id, now)?;
                report.removed.push(record.path);
            }
            Ok(report)
        })
    }

    // ── status ──

    /// The full annotated listing of active files.
    pub fn status(&mut self) -> Result<Vec<FileStatus>, String> {
        // Ordered by path, then added_at.
        let paths = self.db.list_paths()?;
        let sizes: HashMap<String, u64> = self
            .db
            .list_blobs()?
            .into_iter()
            .map(|b| (b.hash, b.size))
            .collect();
        let mut remotes_of: HashMap<String, Vec<String>> = HashMap::new();
        for (hash, remote) in self.db.list_blob_remotes()? {
            remotes_of.entry(hash).or_default().push(remote);
        }
        let mut by_hash: HashMap<&str, Vec<&PathRecord>> = HashMap::new();
        let mut by_path: HashMap<&str, Vec<&PathRecord>> = HashMap::new();
        for record in &paths {
            by_hash.entry(&record.hash).or_default().push(record);
            by_path.entry(&record.path).or_default().push(record);
        }

        let mut statuses = Vec::new();
        for record in paths.iter().filter(|r| r.is_active()) {
            let mut other_names = Vec::new();
            let mut past_names = Vec::new();
            for other in &by_hash[record.hash.as_str()] {
                if other.path == record.path {
                    continue;
                }
                if other.is_active() {
                    other_names.push(other.path.clone());
                } else if !past_names.contains(&other.path) {
                    past_names.push(other.path.clone());
                }
            }
            // A name that is current elsewhere isn't interesting as history.
            past_names.retain(|p| !other_names.contains(p));

            let mut previous_versions: Vec<PreviousVersion> = Vec::new();
            for old in &by_path[record.path.as_str()] {
                if old.is_active() || old.hash == record.hash {
                    continue;
                }
                if previous_versions.iter().any(|v| v.hash == old.hash) {
                    continue;
                }
                previous_versions.push(PreviousVersion {
                    hash: old.hash.clone(),
                    added_at: old.added_at,
                    removed_at: old.removed_at.unwrap_or(0),
                    size: sizes.get(&old.hash).copied().unwrap_or(0),
                    present_locally: self.blob_present(&old.hash),
                    remotes: remotes_of.get(&old.hash).cloned().unwrap_or_default(),
                });
            }
            previous_versions.sort_by_key(|v| std::cmp::Reverse(v.removed_at));

            statuses.push(FileStatus {
                path: record.path.clone(),
                hash: record.hash.clone(),
                size: sizes.get(&record.hash).copied().unwrap_or(0),
                added_at: record.added_at,
                present_locally: self.blob_present(&record.hash),
                remotes: remotes_of.get(&record.hash).cloned().unwrap_or_default(),
                other_names,
                past_names,
                previous_versions,
            });
        }
        statuses.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(statuses)
    }

    /// Resolve a user-supplied spec (active path, full hash, or unique hash
    /// prefix) to a blob hash.
    pub fn resolve_spec(&mut self, spec: &str) -> Result<String, String> {
        if let Some(record) = self.db.active_path(spec)? {
            return Ok(record.hash);
        }
        // Stored hashes are lowercase hex.
        let hex = spec.to_ascii_lowercase();
        if util::is_hash(&hex) {
            return Ok(hex);
        }
        if hex.len() >= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let matches: Vec<String> = self
                .db
                .list_blobs()?
                .into_iter()
                .map(|b| b.hash)
                .filter(|h| h.starts_with(&hex))
                .collect();
            match matches.len() {
                1 => return Ok(matches.into_iter().next().unwrap()),
                n if n > 1 => return Err(format!("hash prefix '{spec}' is ambiguous")),
                _ => {}
            }
        }
        Err(format!("'{spec}' matches no tracked file or blob"))
    }

    // ── remote operations ──

    /// Push one blob to a remote and record it there.
    pub fn push_blob(&mut self, remote: &Remote, hash: &str) -> Result<(), String> {
        let blob = self.blob_path(hash);
        if !blob.is_file() {
            return Err(format!(
                "blob {} is not present locally; fetch it first",
                util::short_hash(hash)
            ));
        }
        self.with_rclone(|| rclone::push_blob(&remote.target, &blob, hash))?;
        self.db.set_blob_on_remote(hash, &remote.name)
    }

    /// Blobs not yet recorded on `remote` (candidates for "push all").
    pub fn blobs_missing_on_remote(&mut self, remote: &str) -> Result<Vec<String>, String> {
        let stored: HashSet<String> = self.db.blobs_on_remote(remote)?.into_iter().collect();
        Ok(self
            .db
            .list_blobs()?
            .into_iter()
            .map(|b| b.hash)
            .filter(|h| !stored.contains(h) && self.blob_present(h))
            .collect())
    }

    /// Download a blob from the first configured remote that has it,
    /// verifying its hash. Returns the name of the remote used.
    pub fn fetch_blob(&mut self, hash: &str) -> Result<String, String> {
        if self.blob_present(hash) {
            return Err(format!(
                "blob {} is already present",
                util::short_hash(hash)
            ));
        }
        let holders = self.db.remotes_for_blob(hash)?;
        if holders.is_empty() {
            return Err(format!(
                "blob {} is not recorded on any remote",
                util::short_hash(hash)
            ));
        }
        let mut remotes = Vec::new();
        for name in holders {
            if let Some(remote) = self.db.remote(&name)? {
                remotes.push(remote);
            }
        }
        let blob = self.blob_path(hash);
        self.with_rclone(|| download_blob(&remotes, hash, &blob))
    }

    /// Download a blob from one specific remote, verifying its hash.
    pub fn fetch_blob_from(&mut self, remote: &Remote, hash: &str) -> Result<(), String> {
        if self.blob_present(hash) {
            return Err(format!(
                "blob {} is already present",
                util::short_hash(hash)
            ));
        }
        let blob = self.blob_path(hash);
        let holders = [remote.clone()];
        self.with_rclone(|| download_blob(&holders, hash, &blob))?;
        Ok(())
    }

    /// Blobs recorded on `remote` but missing locally (candidates for
    /// "download everything from this remote").
    pub fn blobs_missing_locally(&mut self, remote: &str) -> Result<Vec<String>, String> {
        Ok(self
            .db
            .blobs_on_remote(remote)?
            .into_iter()
            .filter(|h| !self.blob_present(h))
            .collect())
    }

    /// List everything on a remote (verifying it is reachable) and
    /// reconcile the database with it; see [`Repo::apply_remote_listing`].
    pub fn scan_remote(&mut self, remote: &Remote) -> Result<RemoteCheck, String> {
        let found = self.with_rclone(|| rclone::list_blobs(&remote.target))?;
        self.apply_remote_listing(remote, found)
    }

    /// Reconcile the database with the blobs actually found on a remote
    /// (the second half of [`Repo::scan_remote`], split out so the slow
    /// rclone listing can run on another thread):
    ///
    /// - recorded blobs that are gone lose their record;
    /// - every blob found is recorded as stored there;
    /// - blobs the database never heard of are added, and named
    ///   `unnamed/<hash>` (a symlink into the blob store, like any other
    ///   tracked file, dangling until the blob is fetched).
    pub fn apply_remote_listing(
        &mut self,
        remote: &Remote,
        found: Vec<rclone::RemoteBlob>,
    ) -> Result<RemoteCheck, String> {
        let root = self.root.clone();
        let now = util::now();
        self.db.transaction(|db| {
            let found: HashMap<String, u64> =
                found.into_iter().map(|b| (b.hash, b.size)).collect();
            let expected: HashSet<String> = db.blobs_on_remote(&remote.name)?.into_iter().collect();
            let sized: HashSet<String> = db.list_blobs()?.into_iter().map(|b| b.hash).collect();
            let known: HashSet<String> = db.known_hashes()?.into_iter().collect();

            let mut check = RemoteCheck::default();
            for hash in &expected {
                if found.contains_key(hash) {
                    check.present.push(hash.clone());
                } else {
                    db.unset_blob_on_remote(hash, &remote.name)?;
                    check.missing.push(hash.clone());
                }
            }
            for (hash, &size) in &found {
                if !sized.contains(hash) {
                    db.upsert_blob(hash, size)?;
                }
                if !known.contains(hash) {
                    let path = name_unnamed_blob(&root, hash)?;
                    if db.active_path(&path)?.is_none() {
                        db.insert_path(&path, hash, now)?;
                    }
                    check.adopted.push(hash.clone());
                } else if !expected.contains(hash) {
                    check.discovered.push(hash.clone());
                }
                if !expected.contains(hash) {
                    db.set_blob_on_remote(hash, &remote.name)?;
                }
            }
            check.present.sort();
            check.missing.sort();
            check.discovered.sort();
            check.adopted.sort();
            Ok(check)
        })
    }

    /// Ask a remote whether it stores each of `hashes`, updating the
    /// database to match. Returns `(hash, stored)` pairs.
    pub fn check_blobs(
        &mut self,
        remote: &Remote,
        hashes: &[String],
    ) -> Result<Vec<(String, bool)>, String> {
        let found = self.with_rclone(|| {
            hashes
                .iter()
                .map(|h| Ok((h.clone(), rclone::stat_blob(&remote.target, h)?)))
                .collect::<Result<Vec<_>, String>>()
        })?;
        found
            .into_iter()
            .map(|(hash, size)| {
                self.record_blob_check(&remote.name, &hash, size)?;
                Ok((hash, size.is_some()))
            })
            .collect()
    }

    /// Record the outcome of [`rclone::stat_blob`] for one blob: stored on
    /// `remote` with `size`, or not stored there.
    pub fn record_blob_check(
        &mut self,
        remote: &str,
        hash: &str,
        size: Option<u64>,
    ) -> Result<(), String> {
        match size {
            Some(size) => {
                if !self.db.has_blob(hash)? {
                    self.db.upsert_blob(hash, size)?;
                }
                self.db.set_blob_on_remote(hash, remote)
            }
            None => self.db.unset_blob_on_remote(hash, remote),
        }
    }
}

/// Create the `unnamed/<hash>` symlink for a blob first seen on a remote,
/// unless something already occupies that name. Returns the path.
fn name_unnamed_blob(root: &Path, hash: &str) -> Result<String, String> {
    let rel = Path::new(UNNAMED_DIR).join(hash);
    let abs = root.join(&rel);
    if std::fs::symlink_metadata(&abs).is_err() {
        std::fs::create_dir_all(root.join(UNNAMED_DIR))
            .map_err(|e| format!("cannot create {UNNAMED_DIR}/: {e}"))?;
        symlink(&link_target(root, &rel, hash), &abs)
            .map_err(|e| format!("cannot symlink {}: {e}", abs.display()))?;
    }
    Ok(rel_str(&rel))
}

/// The relative symlink target that makes `rel` (relative to `root`)
/// point at the blob `hash`.
fn link_target(root: &Path, rel: &Path, hash: &str) -> PathBuf {
    let depth = rel.components().count() - 1;
    PathBuf::from("../".repeat(depth)).join(
        blob_path_in(root, hash)
            .strip_prefix(root)
            .expect("blob path is under root"),
    )
}

/// Download `hash` from the first of `holders` that delivers intact data,
/// verifying the content hash before the blob lands in place. Returns the
/// name of the remote used. Pure filesystem + rclone — safe off-thread.
pub fn download_blob(holders: &[Remote], hash: &str, blob: &Path) -> Result<String, String> {
    if holders.is_empty() {
        return Err(format!(
            "blob {} is not recorded on any remote",
            util::short_hash(hash)
        ));
    }
    std::fs::create_dir_all(blob.parent().unwrap())
        .map_err(|e| format!("cannot create blob directory: {e}"))?;
    let tmp = blob.with_extension("part");

    let mut last_err = String::new();
    for remote in holders {
        match rclone::fetch_blob(&remote.target, hash, &tmp) {
            Ok(()) => match util::sha256_file(&tmp) {
                Ok(actual) if actual == hash => {
                    return std::fs::rename(&tmp, blob)
                        .map(|()| remote.name.clone())
                        .map_err(|e| {
                            std::fs::remove_file(&tmp).ok();
                            format!("cannot finalize blob: {e}")
                        });
                }
                Ok(_) => last_err = format!("remote '{}' returned corrupt data", remote.name),
                Err(e) => last_err = e,
            },
            Err(e) => last_err = format!("remote '{}': {e}", remote.name),
        }
        std::fs::remove_file(&tmp).ok();
    }
    Err(format!("could not fetch blob: {last_err}"))
}

/// Local path of a blob under `root`: `.mbr/blobs/aa/bb/<hash>`.
pub fn blob_path_in(root: &Path, hash: &str) -> PathBuf {
    root.join(BLOBS_DIR)
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash)
}

/// The filesystem half of [`Repo::scan`]: move new regular files under
/// `root` into the blob store (leaving symlinks behind) and list every
/// tracked-looking path with its hash. Touches no database, so it is safe
/// to run off the UI thread; feed the result to [`Repo::apply_scan`].
pub fn ingest_tree(root: &Path) -> Result<Vec<ScannedEntry>, String> {
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries)?;
    let mut scanned = Vec::with_capacity(entries.len());
    for entry in entries {
        let (rel, hash) = match entry {
            Entry::File(rel) => {
                let hash = ingest_file(root, &rel)?;
                (rel, hash)
            }
            Entry::BlobLink(rel, hash) => (rel, hash),
        };
        let size = std::fs::metadata(blob_path_in(root, &hash))
            .ok()
            .map(|m| m.len());
        scanned.push(ScannedEntry {
            path: rel_str(&rel),
            hash,
            size,
        });
    }
    Ok(scanned)
}

/// Move a regular file into the blob store and symlink it back. Returns
/// the content hash.
///
/// The symlink is staged under a hidden name first, so at every instant
/// either the original file or a link to its blob exists (or, after a
/// crash, a staged link that [`collect_entries`] recovers).
fn ingest_file(root: &Path, rel: &Path) -> Result<String, String> {
    let abs = root.join(rel);
    let hash = util::sha256_file(&abs)?;
    let blob = blob_path_in(root, &hash);
    let target = link_target(root, rel, &hash);
    let staged = staged_link_path(&abs);
    std::fs::remove_file(&staged).ok();
    symlink(&target, &staged).map_err(|e| format!("cannot symlink {}: {e}", abs.display()))?;

    let moved = !blob.is_file();
    let stored = if moved {
        std::fs::create_dir_all(blob.parent().unwrap())
            .and_then(|()| std::fs::rename(&abs, &blob))
            .map_err(|e| format!("cannot move {} into blob store: {e}", abs.display()))
    } else {
        // Content already stored; this file is a duplicate.
        std::fs::remove_file(&abs)
            .map_err(|e| format!("cannot remove duplicate {}: {e}", abs.display()))
    };
    if let Err(e) = stored {
        std::fs::remove_file(&staged).ok();
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&staged, &abs) {
        // Put the content back where the user left it.
        if moved {
            std::fs::rename(&blob, &abs).ok();
        } else {
            std::fs::copy(&blob, &abs).ok();
        }
        std::fs::remove_file(&staged).ok();
        return Err(format!("cannot symlink {}: {e}", abs.display()));
    }
    Ok(hash)
}

/// `dir/.name.mbr-ingest` for `dir/name`.
fn staged_link_path(abs: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(abs.file_name().expect("ingested path has a file name"));
    name.push(STAGED_SUFFIX);
    abs.with_file_name(name)
}

/// For a leftover staged link (see [`STAGED_SUFFIX`]), the path it was
/// meant to replace.
fn staged_link_destination(staged: &Path) -> Option<PathBuf> {
    let name = staged.file_name()?.to_str()?;
    let original = name.strip_prefix('.')?.strip_suffix(STAGED_SUFFIX)?;
    (!original.is_empty()).then(|| staged.with_file_name(original))
}

/// Finish or discard an interrupted ingest. Returns the restored path if
/// the staged link was moved into place.
fn recover_staged_link(staged: &Path) -> Option<PathBuf> {
    let dest = staged_link_destination(staged)?;
    let dest_free = std::fs::symlink_metadata(&dest).is_err();
    // `exists` follows the link: the blob made it into the store.
    if dest_free && staged.exists() && std::fs::rename(staged, &dest).is_ok() {
        return Some(dest);
    }
    // The original is still there (crash before the move): drop the link.
    std::fs::remove_file(staged).ok();
    None
}

fn rel_str(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Recursively list ingestable entries under `dir`, as paths relative to
/// `root`. Dot-entries (including `.mbr`) are skipped, as are symlinks
/// that don't point into the blob store.
fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let mut abs = entry.path();
        if name.to_string_lossy().starts_with('.') {
            let is_link = entry.file_type().is_ok_and(|t| t.is_symlink());
            if !(is_link && name.to_string_lossy().ends_with(STAGED_SUFFIX)) {
                continue;
            }
            match recover_staged_link(&abs) {
                Some(restored) => abs = restored,
                None => continue,
            }
        }
        let meta = std::fs::symlink_metadata(&abs)
            .map_err(|e| format!("cannot stat {}: {e}", abs.display()))?;
        let rel = abs.strip_prefix(root).unwrap().to_path_buf();
        if meta.is_dir() {
            collect_entries(root, &abs, out)?;
        } else if meta.file_type().is_symlink() {
            if let Some(hash) = blob_link_hash(&abs) {
                out.push(Entry::BlobLink(rel, hash));
            }
        } else if meta.is_file() {
            out.push(Entry::File(rel));
        }
    }
    Ok(())
}

/// If `link` is a symlink into the `.mbr/blobs` store, the hash it names.
fn blob_link_hash(link: &Path) -> Option<String> {
    let target = std::fs::read_link(link).ok()?;
    let name = target.file_name()?.to_string_lossy().into_owned();
    let components: Vec<_> = target.components().collect();
    let in_blobs = components.windows(2).any(|pair| {
        pair[0].as_os_str() == std::ffi::OsStr::new(".mbr")
            && pair[1].as_os_str() == std::ffi::OsStr::new("blobs")
    });
    (in_blobs && util::is_hash(&name)).then_some(name)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Ensure the repository-local rclone config exists, with restrictive
/// permissions since it holds OAuth tokens and other secrets.
fn ensure_rclone_config(path: &Path) -> Result<(), String> {
    if path.is_file() {
        restrict_config_permissions(path)?;
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, "").map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    restrict_config_permissions(path)
}

fn restrict_config_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot secure {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
