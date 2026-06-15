//! The metadata store of a rubble folder: an [iroh-docs] replica whose
//! entry values live in an [iroh-blobs] store, both under `.mbr/db/`, laid
//! out as iroh-docs' own `Docs::persistent` would, so the same files can
//! later be synced with other devices.
//!
//! An iroh-docs entry is identified by `(key, author)`, each device writing
//! as its own author, and a newer entry replaces an older one with the same
//! key and author. Writing an entry also deletes the same author's entries
//! whose keys it prefixes, so every key segment here ends with a NUL byte:
//! no key is ever a prefix of another. An empty entry is a deletion.
//!
//! Keys (segments shown separated by `/`, each actually NUL-terminated):
//!
//! ```text
//! setting/<key>                 → value
//! remote/<name>                 → JSON (RemoteRecord); read from this device's author only
//! blob/<sha256>/<size>          → "1"
//! on/<remote>/<sha256>          → "1"   the blob is stored on the remote
//! path/<path>/<µs>/<sha256|->   → "1"   from then on, the path names the blob ("-": nothing)
//! ```
//!
//! Everything but settings and remotes is carried by the key itself, with
//! the same one-byte value, so loading never reads more than a handful of
//! values. A path's history is a log of events that are never rewritten:
//! each event closes the path's previous one.
//!
//! The whole store is read into memory on open and kept in step with every
//! write, so reads never touch the disk.
//!
//! [iroh-docs]: https://docs.rs/iroh-docs
//! [iroh-blobs]: https://docs.rs/iroh-blobs

use iroh_blobs::store::fs::FsStore;
use iroh_docs::store::Query;
use iroh_docs::store::fs::Store as DocStore;
use iroh_docs::{Author, AuthorId, NamespaceId, NamespaceSecret};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The iroh-docs store, inside the database directory.
const DOCS_FILE: &str = "docs.redb";
/// This device's author id, as iroh-docs' `Docs::persistent` keeps it.
const AUTHOR_FILE: &str = "default-author";
/// The iroh-blobs store holding entry values, inside the database directory.
const BLOBS_DIR: &str = "blobs";
/// The value of entries that carry everything in their key.
const MARKER: &[u8] = b"1";
/// The path event segment meaning "the path names nothing any more".
const REMOVED: &str = "-";

pub struct Db {
    rt: tokio::runtime::Runtime,
    docs: DocStore,
    blobs: FsStore,
    namespace: NamespaceId,
    author: Author,
    /// The hash of [`MARKER`], stored once.
    marker: iroh_blobs::Hash,
    cache: Cache,
    /// Inside [`Db::batch`]: writes are committed when it ends.
    batching: bool,
    /// The last path event timestamp handed out (µs), to keep them unique.
    clock: u64,
}

/// One name a path held: from `added_at` until `removed_at` (unix seconds).
#[derive(Debug, Clone)]
pub struct PathRecord {
    pub path: String,
    pub hash: String,
    pub added_at: i64,
    pub removed_at: Option<i64>,
}

impl PathRecord {
    pub fn is_active(&self) -> bool {
        self.removed_at.is_none()
    }
}

/// A content-addressed blob the folder knows of.
#[derive(Debug, Clone)]
pub struct BlobRecord {
    pub hash: String,
    pub size: u64,
}

/// A remote, as the rest of mbr uses it.
#[derive(Debug, Clone)]
pub struct Remote {
    pub name: String,
    /// rclone destination this remote maps to, e.g. `_crypt-mbr-drive:`.
    pub target: String,
}

/// Everything known about a remote: enough to write its rclone settings
/// again (see [`crate::remotes::render_config`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRecord {
    /// rclone backend of the storage itself (`drive`, `s3`, `local`, …).
    pub backend: String,
    /// The bucket, share or folder named when setting it up.
    pub spec: String,
    /// Wrapped in a crypt layer keyed with the master password.
    pub encrypt: bool,
    /// Receives metadata backups.
    #[serde(default)]
    pub metadata_backup: bool,
    /// The settings of the storage's rclone section, besides its `type`,
    /// exactly as they are in `rclone.conf` (secrets obscured).
    #[serde(default)]
    pub config: BTreeMap<String, String>,
}

impl RemoteRecord {
    fn target(&self, name: &str) -> String {
        crate::remotes::Layout {
            name: name.to_owned(),
            encrypt: self.encrypt,
        }
        .target()
    }
}

/// One event in a path's history.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PathEvent {
    /// When it happened (unix µs).
    pub at: u64,
    /// The blob the path names from then on; none once it is removed.
    pub hash: Option<String>,
}

/// The whole metadata, as backed up (see [`Db::export`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dump {
    pub settings: BTreeMap<String, String>,
    pub remotes: BTreeMap<String, RemoteRecord>,
    /// Blob sizes by hash.
    pub blobs: BTreeMap<String, u64>,
    /// The blobs stored on each remote.
    pub locations: BTreeMap<String, BTreeSet<String>>,
    /// Every path's history, oldest event first.
    pub paths: BTreeMap<String, Vec<PathEvent>>,
}

/// The store, in memory.
#[derive(Default)]
struct Cache {
    settings: BTreeMap<String, String>,
    remotes: BTreeMap<String, RemoteRecord>,
    /// Every size recorded for each blob (normally exactly one).
    blobs: BTreeMap<String, BTreeSet<u64>>,
    /// `(hash, remote)` pairs.
    locations: BTreeSet<(String, String)>,
    paths: BTreeMap<String, Vec<PathEvent>>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A key made of `segments`, each NUL-terminated.
fn key(segments: &[&str]) -> Vec<u8> {
    let mut key = Vec::new();
    for segment in segments {
        key.extend_from_slice(segment.as_bytes());
        key.push(0);
    }
    key
}

/// The segments of a key made by [`key`].
fn segments(key: &[u8]) -> Option<Vec<&str>> {
    let body = key.strip_suffix(&[0])?;
    body.split(|&b| b == 0)
        .map(|s| std::str::from_utf8(s).ok())
        .collect()
}

/// A segment may not hold the separator.
fn check_segment(s: &str) -> Result<(), String> {
    if s.contains('\0') {
        return Err(format!("{s:?} contains a NUL byte"));
    }
    Ok(())
}

fn random_bytes() -> [u8; 32] {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn seconds(micros: u64) -> i64 {
    (micros / 1_000_000) as i64
}

impl Db {
    /// Open the store in the directory `dir`, creating it if need be.
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err)?;
        let mut docs = DocStore::persistent(dir.join(DOCS_FILE)).map_err(err)?;
        let blobs = rt
            .block_on(FsStore::load(dir.join(BLOBS_DIR)))
            .map_err(err)?;

        let existing = docs
            .list_namespaces()
            .map_err(err)?
            .next()
            .transpose()
            .map_err(err)?;
        let namespace = match existing {
            Some((id, _)) => id,
            None => {
                let secret = NamespaceSecret::from_bytes(&random_bytes());
                let id = secret.id();
                docs.new_replica(secret).map_err(err)?;
                id
            }
        };

        let author_file = dir.join(AUTHOR_FILE);
        let saved = match std::fs::read_to_string(&author_file) {
            Ok(text) => {
                let id: AuthorId = text.trim().parse().map_err(err)?;
                docs.get_author(&id).map_err(err)?
            }
            Err(_) => None,
        };
        let author = match saved {
            Some(author) => author,
            None => {
                let author = Author::from_bytes(&random_bytes());
                docs.import_author(author.clone()).map_err(err)?;
                std::fs::write(&author_file, author.id().to_string())
                    .map_err(|e| format!("cannot write {}: {e}", author_file.display()))?;
                author
            }
        };

        let marker = rt
            .block_on(blobs.add_bytes(MARKER).with_named_tag(b"marker"))
            .map_err(err)?
            .hash;
        let mut db = Self {
            rt,
            docs,
            blobs,
            namespace,
            author,
            marker,
            cache: Cache::default(),
            batching: false,
            clock: 0,
        };
        db.reload()?;
        Ok(db)
    }

    /// Read the whole store into memory again.
    pub fn reload(&mut self) -> Result<(), String> {
        let mut cache = Cache::default();
        let mut values: Vec<(String, iroh_blobs::Hash)> = Vec::new();
        // The newest entry of every key, whoever wrote it, except remotes:
        // each device sees its own version of those.
        let entries = self
            .docs
            .get_many(self.namespace, Query::single_latest_per_key().build())
            .map_err(err)?;
        for entry in entries {
            let entry = entry.map_err(err)?;
            let Some(parts) = segments(entry.key()) else {
                continue;
            };
            match parts[..] {
                ["setting", name] => {
                    values.push((format!("setting\0{name}"), entry.content_hash()))
                }
                ["blob", hash, size] => {
                    if let Ok(size) = size.parse() {
                        cache.blobs.entry(hash.to_owned()).or_default().insert(size);
                    }
                }
                ["on", remote, hash] => {
                    cache.locations.insert((hash.to_owned(), remote.to_owned()));
                }
                ["path", path, at, hash] => {
                    let Ok(at) = at.parse() else { continue };
                    let hash = (hash != REMOVED).then(|| hash.to_owned());
                    cache
                        .paths
                        .entry(path.to_owned())
                        .or_default()
                        .push(PathEvent { at, hash });
                    self.clock = self.clock.max(at);
                }
                _ => {}
            }
        }
        let own = self
            .docs
            .get_many(
                self.namespace,
                Query::author(self.author.id())
                    .key_prefix(key(&["remote"]))
                    .build(),
            )
            .map_err(err)?;
        for entry in own {
            let entry = entry.map_err(err)?;
            if let Some(["remote", name]) = segments(entry.key()).as_deref() {
                values.push((format!("remote\0{name}"), entry.content_hash()));
            }
        }

        for (what, hash) in values {
            let bytes = self.rt.block_on(self.blobs.get_bytes(hash)).map_err(err)?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            match what.split_once('\0') {
                Some(("setting", name)) => {
                    cache.settings.insert(name.to_owned(), text);
                }
                Some(("remote", name)) => match serde_json::from_str(&text) {
                    Ok(record) => {
                        cache.remotes.insert(name.to_owned(), record);
                    }
                    Err(e) => log::warn!("ignoring unreadable remote '{name}': {e}"),
                },
                _ => {}
            }
        }
        for events in cache.paths.values_mut() {
            events.sort();
        }
        self.cache = cache;
        Ok(())
    }

    // ── writing entries ──

    /// Set `key` to `value`, keeping the value in the blob store under a tag
    /// named after the key (so it is never garbage collected).
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), String> {
        let hash = if value == MARKER {
            self.marker
        } else {
            self.rt
                .block_on(self.blobs.add_bytes(value.to_vec()).with_named_tag(key))
                .map_err(err)?
                .hash
        };
        let mut replica = self.docs.open_replica(&self.namespace).map_err(err)?;
        self.rt
            .block_on(replica.insert(key, &self.author, hash, value.len() as u64))
            .map_err(err)?;
        self.commit()
    }

    /// Delete `key` (for every device: the deletion is the newest entry).
    fn delete(&mut self, key: &[u8]) -> Result<(), String> {
        let mut replica = self.docs.open_replica(&self.namespace).map_err(err)?;
        self.rt
            .block_on(replica.delete_prefix(key, &self.author))
            .map_err(err)?;
        if value_tagged(key) {
            self.rt
                .block_on(self.blobs.tags().delete(key))
                .map_err(err)?;
        }
        self.commit()
    }

    /// Commit what was written, unless a batch is running.
    fn commit(&mut self) -> Result<(), String> {
        if self.batching {
            return Ok(());
        }
        self.docs.flush().map_err(err)?;
        self.rt.block_on(self.blobs.sync_db()).map_err(err)
    }

    /// Run `f`, committing its writes once at the end (much faster than
    /// one by one). Not a transaction: writes made before an error stay.
    pub fn batch<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let nested = std::mem::replace(&mut self.batching, true);
        let result = f(self);
        self.batching = nested;
        let committed = self.commit();
        let value = result?;
        committed.map(|()| value)
    }

    /// A timestamp for a new path event: now, but always after the last.
    fn tick(&mut self) -> u64 {
        self.clock = now_micros().max(self.clock + 1);
        self.clock
    }

    // ── paths ──

    /// Every name ever recorded, active and removed alike, ordered by path
    /// and then by when it was added.
    pub fn list_paths(&mut self) -> Result<Vec<PathRecord>, String> {
        Ok(self
            .cache
            .paths
            .iter()
            .flat_map(|(path, events)| records(path, events))
            .collect())
    }

    /// The active record for `path`, if any.
    pub fn active_path(&mut self, path: &str) -> Result<Option<PathRecord>, String> {
        Ok(self
            .cache
            .paths
            .get(path)
            .and_then(|events| records(path, events).pop())
            .filter(PathRecord::is_active))
    }

    /// From now on, `path` names `hash` (closing what it named before).
    pub fn insert_path(&mut self, path: &str, hash: &str) -> Result<(), String> {
        let at = self.tick();
        self.record_path_event(path, at, Some(hash))
    }

    /// From now on, `path` names nothing.
    pub fn remove_path(&mut self, path: &str) -> Result<(), String> {
        let at = self.tick();
        self.record_path_event(path, at, None)
    }

    fn record_path_event(&mut self, path: &str, at: u64, hash: Option<&str>) -> Result<(), String> {
        check_segment(path)?;
        let stamp = format!("{at:020}");
        let named = hash.unwrap_or(REMOVED);
        check_segment(named)?;
        self.put(&key(&["path", path, &stamp, named]), MARKER)?;
        let events = self.cache.paths.entry(path.to_owned()).or_default();
        events.push(PathEvent {
            at,
            hash: hash.map(str::to_owned),
        });
        events.sort();
        Ok(())
    }

    // ── blobs ──

    /// Record blob `hash` with `size`, replacing any other size recorded.
    pub fn upsert_blob(&mut self, hash: &str, size: u64) -> Result<(), String> {
        check_segment(hash)?;
        let known = self.cache.blobs.get(hash).cloned().unwrap_or_default();
        if known.len() == 1 && known.contains(&size) {
            return Ok(());
        }
        self.put(&key(&["blob", hash, &size.to_string()]), MARKER)?;
        for old in known.iter().filter(|&&s| s != size) {
            self.delete(&key(&["blob", hash, &old.to_string()]))?;
        }
        self.cache
            .blobs
            .insert(hash.to_owned(), BTreeSet::from([size]));
        Ok(())
    }

    /// Forget a blob entirely: its size and every remote record. Path
    /// events naming it stay, as history.
    pub fn forget_blob(&mut self, hash: &str) -> Result<(), String> {
        for size in self.cache.blobs.remove(hash).unwrap_or_default() {
            self.delete(&key(&["blob", hash, &size.to_string()]))?;
        }
        for remote in self.remotes_for_blob(hash)? {
            self.unset_blob_on_remote(hash, &remote)?;
        }
        Ok(())
    }

    pub fn has_blob(&mut self, hash: &str) -> Result<bool, String> {
        Ok(self.cache.blobs.contains_key(hash))
    }

    pub fn list_blobs(&mut self) -> Result<Vec<BlobRecord>, String> {
        Ok(self
            .cache
            .blobs
            .iter()
            .filter_map(|(hash, sizes)| {
                Some(BlobRecord {
                    hash: hash.clone(),
                    size: *sizes.last()?,
                })
            })
            .collect())
    }

    /// Every hash the store knows of: recorded blobs, and blobs any path
    /// (current or past) ever named.
    pub fn known_hashes(&mut self) -> Result<Vec<String>, String> {
        let mut known: BTreeSet<String> = self.cache.blobs.keys().cloned().collect();
        for events in self.cache.paths.values() {
            known.extend(events.iter().filter_map(|e| e.hash.clone()));
        }
        Ok(known.into_iter().collect())
    }

    // ── remotes ──

    /// Record a new remote.
    pub fn add_remote(&mut self, name: &str, record: &RemoteRecord) -> Result<(), String> {
        if self.cache.remotes.contains_key(name) {
            return Err(format!("remote '{name}' already exists"));
        }
        self.set_remote_record(name, record)
    }

    /// Replace everything recorded about remote `name`.
    pub fn set_remote_record(&mut self, name: &str, record: &RemoteRecord) -> Result<(), String> {
        check_segment(name)?;
        if self.cache.remotes.get(name) == Some(record) {
            return Ok(());
        }
        let json = serde_json::to_vec(record).map_err(err)?;
        self.put(&key(&["remote", name]), &json)?;
        self.cache.remotes.insert(name.to_owned(), record.clone());
        Ok(())
    }

    /// Forget remote `name` and which blobs it stores.
    pub fn remove_remote(&mut self, name: &str) -> Result<(), String> {
        if !self.cache.remotes.contains_key(name) {
            return Err(format!("no remote named '{name}'"));
        }
        self.batch(|db| {
            for hash in db.blobs_on_remote(name)? {
                db.unset_blob_on_remote(&hash, name)?;
            }
            db.delete(&key(&["remote", name]))?;
            db.cache.remotes.remove(name);
            Ok(())
        })
    }

    /// Rename remote `old` to `name`, carrying its blob records over.
    pub fn rename_remote(&mut self, old: &str, name: &str) -> Result<(), String> {
        if name == old {
            return Ok(());
        }
        if self.cache.remotes.contains_key(name) {
            return Err(format!("remote '{name}' already exists"));
        }
        let record = self
            .remote_record(old)?
            .ok_or_else(|| format!("no remote named '{old}'"))?;
        let stored = self.blobs_on_remote(old)?;
        self.batch(|db| {
            db.add_remote(name, &record)?;
            for hash in &stored {
                db.set_blob_on_remote(hash, name)?;
            }
            db.remove_remote(old)
        })
    }

    pub fn remote_record(&mut self, name: &str) -> Result<Option<RemoteRecord>, String> {
        Ok(self.cache.remotes.get(name).cloned())
    }

    /// Every remote's record, by name.
    pub fn remote_records(&mut self) -> Result<BTreeMap<String, RemoteRecord>, String> {
        Ok(self.cache.remotes.clone())
    }

    pub fn list_remotes(&mut self) -> Result<Vec<Remote>, String> {
        Ok(self
            .cache
            .remotes
            .iter()
            .map(|(name, record)| Remote {
                name: name.clone(),
                target: record.target(name),
            })
            .collect())
    }

    pub fn remote(&mut self, name: &str) -> Result<Option<Remote>, String> {
        Ok(self.cache.remotes.get(name).map(|record| Remote {
            name: name.to_owned(),
            target: record.target(name),
        }))
    }

    // ── blob ↔ remote presence ──

    pub fn set_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        check_segment(hash)?;
        check_segment(remote)?;
        let pair = (hash.to_owned(), remote.to_owned());
        if self.cache.locations.contains(&pair) {
            return Ok(());
        }
        self.put(&key(&["on", remote, hash]), MARKER)?;
        self.cache.locations.insert(pair);
        Ok(())
    }

    pub fn unset_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        let pair = (hash.to_owned(), remote.to_owned());
        if !self.cache.locations.contains(&pair) {
            return Ok(());
        }
        self.delete(&key(&["on", remote, hash]))?;
        self.cache.locations.remove(&pair);
        Ok(())
    }

    /// All `(hash, remote)` pairs.
    pub fn list_blob_remotes(&mut self) -> Result<Vec<(String, String)>, String> {
        Ok(self.cache.locations.iter().cloned().collect())
    }

    /// Remotes recorded as storing `hash`.
    pub fn remotes_for_blob(&mut self, hash: &str) -> Result<Vec<String>, String> {
        Ok(self
            .cache
            .locations
            .range((hash.to_owned(), String::new())..)
            .take_while(|(h, _)| h == hash)
            .map(|(_, remote)| remote.clone())
            .collect())
    }

    /// Hashes recorded as stored on `remote`.
    pub fn blobs_on_remote(&mut self, remote: &str) -> Result<Vec<String>, String> {
        Ok(self
            .cache
            .locations
            .iter()
            .filter(|(_, r)| r == remote)
            .map(|(hash, _)| hash.clone())
            .collect())
    }

    // ── metadata backup targets ──

    /// Choose whether remote `name` receives metadata backups.
    pub fn set_metadata_remote(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        let mut record = self
            .remote_record(name)?
            .ok_or_else(|| format!("no remote named '{name}'"))?;
        record.metadata_backup = enabled;
        self.set_remote_record(name, &record)
    }

    /// Remotes chosen to receive metadata backups.
    pub fn metadata_remotes(&mut self) -> Result<Vec<Remote>, String> {
        Ok(self
            .cache
            .remotes
            .iter()
            .filter(|(_, record)| record.metadata_backup)
            .map(|(name, record)| Remote {
                name: name.clone(),
                target: record.target(name),
            })
            .collect())
    }

    // ── settings ──

    pub fn setting(&mut self, key: &str) -> Result<Option<String>, String> {
        Ok(self.cache.settings.get(key).cloned())
    }

    pub fn set_setting(&mut self, name: &str, value: &str) -> Result<(), String> {
        check_segment(name)?;
        if self.cache.settings.get(name).map(String::as_str) == Some(value) {
            return Ok(());
        }
        self.put(&key(&["setting", name]), value.as_bytes())?;
        self.cache
            .settings
            .insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    // ── export ──

    /// The whole metadata, for a backup.
    pub fn export(&mut self) -> Result<Dump, String> {
        let mut locations: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (hash, remote) in &self.cache.locations {
            locations
                .entry(remote.clone())
                .or_default()
                .insert(hash.clone());
        }
        Ok(Dump {
            settings: self.cache.settings.clone(),
            remotes: self.cache.remotes.clone(),
            blobs: self
                .list_blobs()?
                .into_iter()
                .map(|b| (b.hash, b.size))
                .collect(),
            locations,
            paths: self.cache.paths.clone(),
        })
    }

    /// Write everything in `dump` into this store (meant to be empty).
    pub fn import(&mut self, dump: &Dump) -> Result<(), String> {
        self.batch(|db| {
            for (name, value) in &dump.settings {
                db.set_setting(name, value)?;
            }
            for (name, record) in &dump.remotes {
                db.set_remote_record(name, record)?;
            }
            for (hash, &size) in &dump.blobs {
                db.upsert_blob(hash, size)?;
            }
            for (remote, hashes) in &dump.locations {
                for hash in hashes {
                    db.set_blob_on_remote(hash, remote)?;
                }
            }
            for (path, events) in &dump.paths {
                for event in events {
                    db.record_path_event(path, event.at, event.hash.as_deref())?;
                    db.clock = db.clock.max(event.at);
                }
            }
            Ok(())
        })
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Err(e) = self.commit() {
            log::warn!("cannot commit the database: {e}");
        }
        self.rt.block_on(self.blobs.shutdown()).ok();
    }
}

/// Whether the value of `key` is kept under a tag of its own (see
/// [`Db::put`]).
fn value_tagged(key: &[u8]) -> bool {
    key.starts_with(b"setting\0") || key.starts_with(b"remote\0")
}

/// The names `path` held, from its history: each event closes the one
/// before it.
fn records(path: &str, events: &[PathEvent]) -> Vec<PathRecord> {
    let mut records: Vec<PathRecord> = Vec::new();
    for event in events {
        if let Some(open) = records.last_mut().filter(|r| r.is_active()) {
            open.removed_at = Some(seconds(event.at));
        }
        if let Some(hash) = &event.hash {
            records.push(PathRecord {
                path: path.to_owned(),
                hash: hash.clone(),
                added_at: seconds(event.at),
                removed_at: None,
            });
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh directory, removed when dropped.
    struct TestDb {
        db: Option<Db>,
        dir: std::path::PathBuf,
    }

    impl TestDb {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mbr-db-{name}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            let db = Db::open(&dir).unwrap();
            Self { db: Some(db), dir }
        }

        /// Close and open the store again.
        fn reopen(&mut self) {
            drop(self.db.take());
            self.db = Some(Db::open(&self.dir).unwrap());
        }
    }

    impl std::ops::Deref for TestDb {
        type Target = Db;
        fn deref(&self) -> &Db {
            self.db.as_ref().unwrap()
        }
    }

    impl std::ops::DerefMut for TestDb {
        fn deref_mut(&mut self) -> &mut Db {
            self.db.as_mut().unwrap()
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            drop(self.db.take());
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn record(backend: &str) -> RemoteRecord {
        RemoteRecord {
            backend: backend.to_owned(),
            encrypt: true,
            ..RemoteRecord::default()
        }
    }

    #[test]
    fn keys_never_prefix_each_other() {
        let a = key(&["remote", "drive"]);
        let b = key(&["remote", "drive2"]);
        assert!(!b.starts_with(&a));
        assert_eq!(segments(&b).unwrap(), ["remote", "drive2"]);
        assert!(check_segment("a\0b").is_err());
    }

    #[test]
    fn everything_survives_reopening() {
        let mut db = TestDb::new("reopen");
        db.set_setting("min_copies", "3").unwrap();
        db.add_remote("drive", &record("drive")).unwrap();
        db.add_remote("drive2", &record("s3")).unwrap();
        db.upsert_blob("ab", 1).unwrap();
        db.upsert_blob("ab", 2).unwrap();
        db.set_blob_on_remote("ab", "drive").unwrap();
        db.set_blob_on_remote("ab", "drive").unwrap();
        db.insert_path("p", "ab").unwrap();
        db.reopen();

        assert_eq!(db.setting("min_copies").unwrap().as_deref(), Some("3"));
        let names: Vec<String> = db
            .list_remotes()
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(
            names,
            ["drive", "drive2"],
            "a key prefixing another deletes nothing"
        );
        assert_eq!(
            db.remote("drive").unwrap().unwrap().target,
            "_crypt-mbr-drive:"
        );
        assert_eq!(db.list_blobs().unwrap()[0].size, 2);
        assert_eq!(db.remotes_for_blob("ab").unwrap(), ["drive"]);
        assert_eq!(db.active_path("p").unwrap().unwrap().hash, "ab");
    }

    #[test]
    fn paths_keep_their_history() {
        let mut db = TestDb::new("history");
        db.insert_path("p", "one").unwrap();
        db.insert_path("p", "two").unwrap();
        db.remove_path("p").unwrap();
        assert!(db.active_path("p").unwrap().is_none());
        db.insert_path("p", "one").unwrap();
        db.reopen();
        let history: Vec<(String, bool)> = db
            .list_paths()
            .unwrap()
            .into_iter()
            .map(|r| (r.hash.clone(), r.is_active()))
            .collect();
        assert_eq!(
            history,
            [
                ("one".to_owned(), false),
                ("two".to_owned(), false),
                ("one".to_owned(), true)
            ]
        );
    }

    #[test]
    fn renaming_a_remote_keeps_its_blobs_and_settings() {
        let mut db = TestDb::new("rename");
        db.add_remote("a", &record("drive")).unwrap();
        db.add_remote("taken", &record("s3")).unwrap();
        db.set_blob_on_remote("h", "a").unwrap();
        db.set_metadata_remote("a", true).unwrap();
        assert!(db.rename_remote("a", "taken").is_err());
        assert!(db.rename_remote("nope", "b").is_err());
        db.rename_remote("a", "b").unwrap();
        db.reopen();
        assert!(db.remote("a").unwrap().is_none());
        assert_eq!(db.remote_record("b").unwrap().unwrap().backend, "drive");
        assert_eq!(db.remotes_for_blob("h").unwrap(), ["b"]);
        assert_eq!(db.metadata_remotes().unwrap()[0].name, "b");
    }

    #[test]
    fn removed_remotes_and_blobs_stay_removed() {
        let mut db = TestDb::new("remove");
        assert!(db.set_metadata_remote("nope", true).is_err());
        db.add_remote("a", &record("drive")).unwrap();
        db.upsert_blob("h", 1).unwrap();
        db.set_blob_on_remote("h", "a").unwrap();
        db.remove_remote("a").unwrap();
        db.upsert_blob("g", 1).unwrap();
        db.forget_blob("g").unwrap();
        db.reopen();
        assert!(db.list_remotes().unwrap().is_empty());
        assert!(db.list_blob_remotes().unwrap().is_empty());
        assert!(!db.has_blob("g").unwrap());
        assert!(db.has_blob("h").unwrap());
    }

    #[test]
    fn known_hashes_include_path_only_blobs() {
        let mut db = TestDb::new("known");
        db.upsert_blob("stored", 1).unwrap();
        db.insert_path("p", "named").unwrap();
        assert_eq!(db.known_hashes().unwrap(), ["named", "stored"]);
    }

    #[test]
    fn exports_import_into_an_identical_store() {
        let mut db = TestDb::new("export");
        db.set_setting("folder_id", "0123456789ab").unwrap();
        db.add_remote("a", &record("drive")).unwrap();
        db.upsert_blob("h", 5).unwrap();
        db.set_blob_on_remote("h", "a").unwrap();
        db.insert_path("dir/it's here", "h").unwrap();
        db.remove_path("dir/it's here").unwrap();
        let dump = db.export().unwrap();

        let mut copy = TestDb::new("import");
        copy.import(&dump).unwrap();
        copy.reopen();
        assert_eq!(copy.export().unwrap(), dump);
        // New events still come after the imported ones.
        copy.insert_path("dir/it's here", "g").unwrap();
        assert_eq!(
            copy.active_path("dir/it's here").unwrap().unwrap().hash,
            "g"
        );
    }
}
