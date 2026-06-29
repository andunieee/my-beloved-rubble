//! Local, per-machine app state. The last folder opened lives in
//! `<config dir>/mbr/state.json`, loaded on every start. Each folder's own
//! state — its master password and when its metadata was last backed up to
//! which remote — lives in `<folder>/.mbr/local.json`, so it goes away with
//! the folder. Unlike the database in `.mbr/db`, none of it is ever pushed
//! or backed up anywhere: it records facts about *this* machine.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppState {
    /// The folder the GUI reopens on start.
    #[serde(default)]
    pub last_folder: Option<PathBuf>,
    /// Folder state read or changed this run, written back on save.
    #[serde(skip)]
    local: BTreeMap<String, RepoState>,
}

/// Where a folder keeps its [`RepoState`], relative to its root.
pub const LOCAL_STATE_FILE: &str = ".mbr/local.json";

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
    /// SHA-256 of the metadata snapshot as backed up, to tell whether it
    /// changed since.
    pub db_hash: String,
}

impl RepoState {
    /// Make `password` the folder's master password, saved on this machine.
    pub fn set_password(&mut self, password: &str) -> Result<(), String> {
        if self.password_check.is_some() {
            return Err("the master password is already set and cannot be changed".to_owned());
        }
        self.password_check = Some(crate::metadata::password_check(password)?);
        self.password = Some(password.to_owned());
        Ok(())
    }

    /// Remember a password after verifying it against an existing check.
    pub fn remember_password(&mut self, password: &str) -> Result<(), String> {
        let check = self
            .password_check
            .as_deref()
            .ok_or_else(|| "the master password is not set".to_owned())?;
        if !crate::metadata::password_matches(check, password) {
            return Err("wrong master password".to_owned());
        }
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
        match Self::default_path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
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

    /// Save to the default location (and every changed folder's state).
    pub fn save(&self) -> Result<(), String> {
        let path = Self::default_path().ok_or("no config directory on this system")?;
        self.save_to(&path)
    }

    /// Write the app state atomically to `path`, and each folder whose
    /// state changed into its own `.mbr`. Both are readable only by the
    /// owner (a folder's holds the master password).
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        for (root, repo) in &self.local {
            let file = Path::new(root).join(LOCAL_STATE_FILE);
            // A folder deleted since it was opened has nowhere to keep it.
            if file.parent().is_some_and(Path::is_dir) {
                write_private(&file, repo)?;
            }
        }
        write_private(path, self)
    }

    /// The state of the folder at `root` (empty if never recorded).
    pub fn repo(&self, root: &Path) -> RepoState {
        match self.local.get(&key(root)) {
            Some(repo) => repo.clone(),
            None => read_local(root),
        }
    }

    pub fn repo_mut(&mut self, root: &Path) -> &mut RepoState {
        self.local
            .entry(key(root))
            .or_insert_with(|| read_local(root))
    }
}

fn read_local(root: &Path) -> RepoState {
    let file = root.join(LOCAL_STATE_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring unreadable {}: {e}", file.display());
            RepoState::default()
        }),
        Err(_) => RepoState::default(),
    }
}

fn write_private(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
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
        let folder = dir.join("folder");
        std::fs::create_dir_all(folder.join(".mbr")).unwrap();
        let mut state = AppState {
            last_folder: Some(PathBuf::from("/x")),
            ..AppState::default()
        };
        state.repo_mut(&folder).metadata_backups.insert(
            "r".into(),
            BackupRecord {
                at: 5,
                db_hash: "h".into(),
            },
        );
        state.save_to(&path).unwrap();
        let loaded = AppState::load_from(&path);
        assert_eq!(loaded.last_folder.as_deref(), Some(Path::new("/x")));
        assert_eq!(loaded.repo(&folder).metadata_backups["r"].at, 5);
        assert!(loaded.repo(Path::new("/y")).password_check.is_none());

        // State goes with the folder: a new one at the same path is clean.
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(
            AppState::load_from(&path)
                .repo(&folder)
                .metadata_backups
                .is_empty()
        );

        std::fs::write(&path, "{not json").unwrap();
        assert!(AppState::load_from(&path).last_folder.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn master_password_can_only_be_set_once() {
        let mut repo = RepoState::default();
        repo.set_password("first password").unwrap();
        assert!(repo.set_password("second password").is_err());
        assert_eq!(repo.password.as_deref(), Some("first password"));
        assert!(repo.remember_password("wrong password").is_err());
        repo.remember_password("first password").unwrap();
        assert_eq!(repo.password.as_deref(), Some("first password"));
    }
}
