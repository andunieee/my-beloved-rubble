//! SQLite metadata store for a rubble folder.
//!
//! Four tables:
//! - `paths`: every name the folder has ever held — which blob it pointed
//!   to, when it was added, and (once gone or replaced) when it was removed.
//!   A path is *active* while `removed_at` is NULL.
//! - `blobs`: content-addressed blobs (hash → size).
//! - `remotes`: configured rclone remotes (name → rclone target).
//! - `blob_remotes`: which blobs are known to be stored on which remotes.
//!
//! minisqlite has no parameter binding, so values are inlined with
//! [`quote`]; hashes and integers are inlined directly (always safe).

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
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let mut db = Self { conn };
        db.init_schema()?;
        Ok(db)
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
            "INSERT INTO paths (path, hash, added_at, removed_at) VALUES ({}, '{hash}', {added_at}, NULL)",
            quote(path)
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
                "DELETE FROM blobs WHERE hash = '{hash}';
                 INSERT INTO blobs (hash, size) VALUES ('{hash}', {size})"
            ))
            .map_err(|e| e.to_string())
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
                 DELETE FROM remotes WHERE name = {q}"
            ))
            .map_err(|e| e.to_string())
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
        Ok(self
            .list_remotes()?
            .into_iter()
            .find(|r| r.name == name))
    }

    // ── blob ↔ remote presence ──

    pub fn set_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        let q = quote(remote);
        self.conn
            .execute(&format!(
                "DELETE FROM blob_remotes WHERE hash = '{hash}' AND remote = {q};
                 INSERT INTO blob_remotes (hash, remote) VALUES ('{hash}', {q})"
            ))
            .map_err(|e| e.to_string())
    }

    pub fn unset_blob_on_remote(&mut self, hash: &str, remote: &str) -> Result<(), String> {
        let sql = format!(
            "DELETE FROM blob_remotes WHERE hash = '{hash}' AND remote = {}",
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
        let sql =
            format!("SELECT remote FROM blob_remotes WHERE hash = '{hash}' ORDER BY remote");
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
}
