//! Local, per-machine app state, kept in `<config dir>/mbr/state.json` and
//! loaded on every start. Unlike `.mbr/mbr.db`, it is never pushed or
//! backed up anywhere: it records facts about *this* machine — the last
//! folder opened, and for each folder its master password and when its
//! metadata was last backed up to which remote.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// The folder the GUI reopens on start.
    #[serde(default)]
    pub last_folder: Option<PathBuf>,
    /// Per-folder state, keyed by the folder's canonical path.
    #[serde(default)]
    pub repos: BTreeMap<String, RepoState>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RepoState {
    /// Argon2 hash of the master password (PHC string), so a mistyped
    /// password is caught before anything is encrypted with it.
    #[serde(default)]
    pub password_check: Option<String>,
    /// The master password itself, so it is asked for only once per
    /// machine: remotes are encrypted with it and need it at every setup.
    #[serde(default)]
    pub password: Option<String>,
    /// The last successful metadata backup, per remote name.
    #[serde(default)]
    pub metadata_backups: BTreeMap<String, BackupRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRecord {
    /// When the backup finished (unix seconds).
    pub at: i64,
    /// SHA-256 of `mbr.db` as backed up, to tell whether it changed since.
    pub db_hash: String,
}

impl RepoState {
    /// Make `password` the folder's master password, saved on this machine.
    pub fn set_password(&mut self, password: &str) -> Result<(), String> {
        self.password_check = Some(crate::metadata::password_check(password)?);
        self.password = Some(password.to_owned());
        Ok(())
    }
}

impl AppState {
    /// `<config dir>/mbr/state.json`.
    pub fn default_path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("mbr").join("state.json"))
    }

    /// Load the state from its default location (empty if there is none).
    pub fn load() -> Self {
        let Some(path) = Self::default_path() else {
            return Self::default();
        };
        let mut state = Self::load_from(&path);
        // Before state.json, the last folder lived in its own file.
        if state.last_folder.is_none() {
            let legacy = path.with_file_name("last-folder");
            if let Ok(text) = std::fs::read_to_string(legacy) {
                state.last_folder = Some(PathBuf::from(text.trim()));
            }
        }
        state
    }

    /// Load the state at `path`; a missing or unreadable file is empty
    /// state (logged, not fatal: nothing in it is irreplaceable).
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Save to the default location.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::default_path().ok_or("no config directory on this system")?;
        self.save_to(&path)
    }

    /// Write atomically to `path`, readable only by the owner (it holds the
    /// master password).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("cannot secure {}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// The state of the folder at `root` (empty if never recorded).
    pub fn repo(&self, root: &Path) -> RepoState {
        self.repos.get(&key(root)).cloned().unwrap_or_default()
    }

    pub fn repo_mut(&mut self, root: &Path) -> &mut RepoState {
        self.repos.entry(key(root)).or_default()
    }
}

fn key(root: &Path) -> String {
    root.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = std::env::temp_dir().join(format!("mbr-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut state = AppState {
            last_folder: Some(PathBuf::from("/x")),
            ..AppState::default()
        };
        state.repo_mut(Path::new("/x")).metadata_backups.insert(
            "r".into(),
            BackupRecord {
                at: 5,
                db_hash: "h".into(),
            },
        );
        state.save_to(&path).unwrap();
        let loaded = AppState::load_from(&path);
        assert_eq!(loaded.last_folder.as_deref(), Some(Path::new("/x")));
        assert_eq!(loaded.repo(Path::new("/x")).metadata_backups["r"].at, 5);
        assert!(loaded.repo(Path::new("/y")).password_check.is_none());

        std::fs::write(&path, "{not json").unwrap();
        assert!(AppState::load_from(&path).repos.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
