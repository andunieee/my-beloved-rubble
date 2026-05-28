//! SQLite metadata store for a rubble folder.
//!
//! Six tables:
//! - `paths`: every name the folder has ever held — which blob it pointed
//!   to, when it was added, and (once gone or replaced) when it was removed.
//!   A path is *active* while `removed_at` is NULL.
//! - `blobs`: content-addressed blobs (hash → size).
//! - `remotes`: configured rclone remotes (name → rclone target).
//! - `blob_remotes`: which blobs are known to be stored on which remotes.
//! - `metadata_remotes`: remotes chosen to hold encrypted backups of this
//!   database (see [`crate::metadata`]).
//! - `settings`: folder-wide key → value settings (e.g. `min_copies`).
//!
//! minisqlite has no parameter binding, so every string value is inlined
//! through [`quote`] (integers are inlined directly, which is always safe).

use minisqlite::{Connection, Value};
use std::path::Path;

pub struct Db {
    conn: Connection,
}

/// One row of `paths`.
#[derive(Debug, Clone)]
pub struct PathRecord {
    pub id: i64,
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

/// One row of `blobs`.
#[derive(Debug, Clone)]
pub struct BlobRecord {
    pub hash: String,
    pub size: u64,
}

/// One row of `remotes`.
#[derive(Debug, Clone)]
pub struct Remote {
    pub name: String,
    /// rclone destination this remote maps to, e.g. `backup:bucket/mbr`.
    pub target: String,
}

/// Single-quote a string literal for inlining into SQL.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        _ => None,
    }
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    }
}

impl Db {
    /// Run `f` inside one transaction: committed if it returns `Ok`, rolled
    /// back otherwise. Much faster than autocommitting each statement, and
    /// keeps a multi-statement update all-or-nothing.
    pub fn transaction<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.conn.execute("BEGIN").map_err(|e| e.to_string())?;
        match f(self) {
            Ok(value) => {
                self.conn.execute("COMMIT").map_err(|e| e.to_string())?;
                Ok(value)
            }
            Err(e) => {
                self.conn.execute("ROLLBACK").ok();
                Err(e)
            }
        }
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let mut db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// A throwaway database, for when the real one has to be closed.
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        let mut db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Write a consistent copy of the whole database to `dest`, which must
    /// not exist yet.
    pub fn snapshot_to(&mut self, dest: &Path) -> Result<(), String> {
        let dest = dest
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", dest.display()))?;
        self.conn
            .execute(&format!("VACUUM INTO {}", quote(dest)))
            .map_err(|e| e.to_string())
    }

    fn init_schema(&mut self) -> Result<(), String> {
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS paths (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    path TEXT NOT NULL,
                    hash TEXT NOT NULL,
                    added_at INTEGER NOT NULL,
                    removed_at INTEGER
                );
                CREATE TABLE IF NOT EXISTS blobs (
                    hash TEXT PRIMARY KEY,
                    size INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS remotes (
                    name TEXT PRIMARY KEY,
                    target TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS blob_remotes (
                    hash TEXT NOT NULL,
                    remote TEXT NOT NULL,
                    PRIMARY KEY (hash, remote)
                );
                CREATE TABLE IF NOT EXISTS metadata_remotes (
                    name TEXT PRIMARY KEY
                );
                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )
            .map_err(|e| e.to_string())
    }

    // ── paths ──

    /// Every path row ever recorded, active and removed alike.
    pub fn list_paths(&mut self) -> Result<Vec<PathRecord>, String> {
        let r = self
            .conn
            .query("SELECT id, path, hash, added_at, removed_at FROM paths ORDER BY path, added_at")
            .map_err(|e| e.to_string())?;
        Ok(r.rows
            .iter()
            .filter_map(|row| {
                Some(PathRecord {
                    id: as_i64(&row[0])?,
                    path: as_text(&row[1])?,
                    hash: as_text(&row[2])?,
                    added_at: as_i64(&row[3])?,
                    removed_at: as_i64(&row[4]),
                })
            })
            .collect())
    }

    /// The active (not removed) row for `path`, if any.
    pub fn active_path(&mut self, path: &str) -> Result<Option<PathRecord>, String> {
        let sql = format!(
            "SELECT id, path, hash, added_at, removed_at FROM paths
             WHERE path = {} AND removed_at IS NULL",
            quote(path)
        );
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(r.rows.first().and_then(|row| {
            Some(PathRecord {
                id: as_i64(&row[0])?,
                path: as_text(&row[1])?,
                hash: as_text(&row[2])?,
                added_at: as_i64(&row[3])?,
                removed_at: as_i64(&row[4]),
            })
        }))
    }

    pub fn insert_path(&mut self, path: &str, hash: &str, added_at: i64) -> Result<(), String> {
        let sql = format!(
            "INSERT INTO paths (path, hash, added_at, removed_at) VALUES ({}, {}, {added_at}, NULL)",
            quote(path),
            quote(hash)
        );
        self.conn.execute(&sql).map_err(|e| e.to_string())
    }

    pub fn mark_path_removed(&mut self, id: i64, removed_at: i64) -> Result<(), String> {
        let sql = format!("UPDATE paths SET removed_at = {removed_at} WHERE id = {id}");
        self.conn.execute(&sql).map_err(|e| e.to_string())
    }

    // ── blobs ──

    pub fn upsert_blob(&mut self, hash: &str, size: u64) -> Result<(), String> {
        self.conn
            .execute(&format!(
                "INSERT INTO blobs (hash, size) VALUES ({}, {size})
                 ON CONFLICT (hash) DO UPDATE SET size = excluded.size",
                quote(hash)
            ))
            .map_err(|e| e.to_string())
    }

    /// Forget a blob entirely: its size and every remote record. Path rows
    /// pointing at it stay, as history.
    pub fn forget_blob(&mut self, hash: &str) -> Result<(), String> {
        let q = quote(hash);
        self.conn
            .execute(&format!(
                "DELETE FROM blob_remotes WHERE hash = {q};
                 DELETE FROM blobs WHERE hash = {q}"
            ))
            .map_err(|e| e.to_string())
    }

    pub fn has_blob(&mut self, hash: &str) -> Result<bool, String> {
        let sql = format!("SELECT 1 FROM blobs WHERE hash = {}", quote(hash));
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(!r.rows.is_empty())
    }

    pub fn list_blobs(&mut self) -> Result<Vec<BlobRecord>, String> {
        let r = self
            .conn
            .query("SELECT hash, size FROM blobs ORDER BY hash")
            .map_err(|e| e.to_string())?;
        Ok(r.rows
            .iter()
            .filter_map(|row| {
                Some(BlobRecord {
                    hash: as_text(&row[0])?,
                    size: as_i64(&row[1])? as u64,
                })
            })
            .collect())
    }

    /// Every hash the database knows of: stored blobs and blobs any path
    /// (current or past) ever pointed at.
    pub fn known_hashes(&mut self) -> Result<Vec<String>, String> {
        let r = self
            .conn
            .query("SELECT hash FROM blobs UNION SELECT hash FROM paths")
            .map_err(|e| e.to_string())?;
        Ok(r.rows.iter().filter_map(|row| as_text(&row[0])).collect())
    }

    // ── remotes ──

    pub fn add_remote(&mut self, name: &str, target: &str) -> Result<(), String> {
        let sql = format!(
            "INSERT INTO remotes (name, target) VALUES ({}, {})",
            quote(name),
            quote(target)
        );
        self.conn.execute(&sql).map_err(|e| e.to_string())
    }

    pub fn remove_remote(&mut self, name: &str) -> Result<(), String> {
        let q = quote(name);
        self.conn
            .execute(&format!(
                "DELETE FROM blob_remotes WHERE remote = {q};
                 DELETE FROM metadata_remotes WHERE name = {q};
                 DELETE FROM remotes WHERE name = {q}"
            ))
            .map_err(|e| e.to_string())
    }

    /// Rename and/or retarget the remote `old`, carrying its blob records
    /// over to the new name.
    pub fn update_remote(&mut self, old: &str, name: &str, target: &str) -> Result<(), String> {
        if name != old && self.remote(name)?.is_some() {
            return Err(format!("remote '{name}' already exists"));
        }
        if self.remote(old)?.is_none() {
            return Err(format!("no remote named '{old}'"));
        }
        let (old, name, target) = (quote(old), quote(name), quote(target));
        self.transaction(|db| {
            db.conn
                .execute(&format!(
                    "UPDATE remotes SET name = {name}, target = {target} WHERE name = {old};
                     UPDATE blob_remotes SET remote = {name} WHERE remote = {old};
                     UPDATE metadata_remotes SET name = {name} WHERE name = {old}"
                ))
                .map_err(|e| e.to_string())
        })
    }

    pub fn list_remotes(&mut self) -> Result<Vec<Remote>, String> {
        let r = self
            .conn
            .query("SELECT name, target FROM remotes ORDER BY name")
            .map_err(|e| e.to_string())?;
        Ok(r.rows
            .iter()
            .filter_map(|row| {
                Some(Remote {
                    name: as_text(&row[0])?,
                    target: as_text(&row[1])?,
                })
            })
            .collect())
    }

    pub fn remote(&mut self, name: &str) -> Result<Option<Remote>, String> {
        let sql = format!("SELECT name, target FROM remotes WHERE name = {}", quote(name));
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(r.rows.first().and_then(|row| {
            Some(Remote {
                name: as_text(&row[0])?,
                target: as_text(&row[1])?,
            })
        }))
    }

    // ── blob ↔ remote presence ──

    pub fn set_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        self.conn
            .execute(&format!(
                "INSERT INTO blob_remotes (hash, remote) VALUES ({}, {})
                 ON CONFLICT DO NOTHING",
                quote(hash),
                quote(remote)
            ))
            .map_err(|e| e.to_string())
    }

    pub fn unset_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        let sql = format!(
            "DELETE FROM blob_remotes WHERE hash = {} AND remote = {}",
            quote(hash),
            quote(remote)
        );
        self.conn.execute(&sql).map_err(|e| e.to_string())
    }

    /// All `(hash, remote)` pairs.
    pub fn list_blob_remotes(&mut self) -> Result<Vec<(String, String)>, String> {
        let r = self
            .conn
            .query("SELECT hash, remote FROM blob_remotes ORDER BY hash, remote")
            .map_err(|e| e.to_string())?;
        Ok(r.rows
            .iter()
            .filter_map(|row| Some((as_text(&row[0])?, as_text(&row[1])?)))
            .collect())
    }

    /// Remotes recorded as storing `hash`.
    pub fn remotes_for_blob(&mut self, hash: &str) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT remote FROM blob_remotes WHERE hash = {} ORDER BY remote",
            quote(hash)
        );
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(r.rows.iter().filter_map(|row| as_text(&row[0])).collect())
    }

    /// Hashes recorded as stored on `remote`.
    pub fn blobs_on_remote(&mut self, remote: &str) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT hash FROM blob_remotes WHERE remote = {} ORDER BY hash",
            quote(remote)
        );
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(r.rows.iter().filter_map(|row| as_text(&row[0])).collect())
    }

    // ── metadata backup targets ──

    /// Choose whether remote `name` receives metadata backups.
    pub fn set_metadata_remote(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        if enabled && self.remote(name)?.is_none() {
            return Err(format!("no remote named '{name}'"));
        }
        let q = quote(name);
        let sql = if enabled {
            format!("INSERT INTO metadata_remotes (name) VALUES ({q}) ON CONFLICT DO NOTHING")
        } else {
            format!("DELETE FROM metadata_remotes WHERE name = {q}")
        };
        self.conn.execute(&sql).map_err(|e| e.to_string())
    }

    /// Remotes chosen to receive metadata backups.
    pub fn metadata_remotes(&mut self) -> Result<Vec<Remote>, String> {
        let r = self
            .conn
            .query(
                "SELECT r.name, r.target FROM remotes r
                 JOIN metadata_remotes m ON m.name = r.name ORDER BY r.name",
            )
            .map_err(|e| e.to_string())?;
        Ok(r.rows
            .iter()
            .filter_map(|row| {
                Some(Remote {
                    name: as_text(&row[0])?,
                    target: as_text(&row[1])?,
                })
            })
            .collect())
    }

    // ── settings ──

    pub fn setting(&mut self, key: &str) -> Result<Option<String>, String> {
        let sql = format!("SELECT value FROM settings WHERE key = {}", quote(key));
        let r = self.conn.query(&sql).map_err(|e| e.to_string())?;
        Ok(r.rows.first().and_then(|row| as_text(&row[0])))
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(&format!(
                "INSERT INTO settings (key, value) VALUES ({}, {})
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                quote(key),
                quote(value)
            ))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        let mut db = Db {
            conn: Connection::open_in_memory().unwrap(),
        };
        db.init_schema().unwrap();
        db
    }

    #[test]
    fn upserts_are_idempotent() {
        let mut db = db();
        db.upsert_blob("ab", 1).unwrap();
        db.upsert_blob("ab", 2).unwrap();
        assert_eq!(db.list_blobs().unwrap()[0].size, 2);
        db.set_blob_on_remote("ab", "r").unwrap();
        db.set_blob_on_remote("ab", "r").unwrap();
        assert_eq!(db.remotes_for_blob("ab").unwrap(), ["r"]);
    }

    #[test]
    fn transactions_roll_back_on_error() {
        let mut db = db();
        let result: Result<(), String> = db.transaction(|db| {
            db.add_remote("r", "x:")?;
            Err("boom".into())
        });
        assert!(result.is_err());
        assert!(db.remote("r").unwrap().is_none());
        db.transaction(|db| db.add_remote("r", "x:")).unwrap();
        assert_eq!(db.remote("r").unwrap().unwrap().target, "x:");
    }

    #[test]
    fn renaming_a_remote_keeps_its_blobs() {
        let mut db = db();
        db.add_remote("a", "x:").unwrap();
        db.add_remote("taken", "y:").unwrap();
        db.set_blob_on_remote("h", "a").unwrap();
        assert!(db.update_remote("a", "taken", "x:").is_err());
        assert!(db.update_remote("nope", "b", "x:").is_err());
        db.update_remote("a", "b", "z:").unwrap();
        assert!(db.remote("a").unwrap().is_none());
        assert_eq!(db.remote("b").unwrap().unwrap().target, "z:");
        assert_eq!(db.remotes_for_blob("h").unwrap(), ["b"]);
        // Retargeting alone keeps the name.
        db.update_remote("b", "b", "w:").unwrap();
        assert_eq!(db.remote("b").unwrap().unwrap().target, "w:");
    }

    #[test]
    fn metadata_remotes_follow_renames_and_removals() {
        let mut db = db();
        assert!(db.set_metadata_remote("nope", true).is_err());
        db.add_remote("a", "x:").unwrap();
        db.add_remote("b", "y:").unwrap();
        db.set_metadata_remote("a", true).unwrap();
        db.set_metadata_remote("a", true).unwrap();
        db.update_remote("a", "c", "x:").unwrap();
        let names = |db: &mut Db| -> Vec<String> {
            db.metadata_remotes().unwrap().into_iter().map(|r| r.name).collect()
        };
        assert_eq!(names(&mut db), ["c"]);
        db.set_metadata_remote("b", true).unwrap();
        db.remove_remote("c").unwrap();
        assert_eq!(names(&mut db), ["b"]);
        db.set_metadata_remote("b", false).unwrap();
        assert!(names(&mut db).is_empty());
    }

    #[test]
    fn settings_round_trip() {
        let mut db = db();
        assert_eq!(db.setting("k").unwrap(), None);
        db.set_setting("k", "1").unwrap();
        db.set_setting("k", "2").unwrap();
        assert_eq!(db.setting("k").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn known_hashes_include_path_only_blobs() {
        let mut db = db();
        db.upsert_blob("stored", 1).unwrap();
        db.insert_path("p", "named", 0).unwrap();
        let mut known = db.known_hashes().unwrap();
        known.sort();
        assert_eq!(known, ["named", "stored"]);
    }

    #[test]
    fn quoting_survives_hostile_names() {
        let mut db = db();
        let name = "it's'; DROP TABLE remotes; --";
        db.add_remote(name, "t:").unwrap();
        assert!(db.remote(name).unwrap().is_some());
    }
}
