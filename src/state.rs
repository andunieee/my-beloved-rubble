//! Local, per-machine app state. The last folder opened lives in
//! `<config dir>/mbr/state.json`, loaded on every start. Each folder's own
//! state — its master password, when
//! its metadata was last backed up to which remote, this device's key for
//! syncing it and the peers it syncs with — lives in
//! `<folder>/.mbr/local.json`, so it goes away with the folder. Unlike the database in `.mbr/db`, none of it is ever pushed
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
    /// Where earlier versions kept the check of the master password, which
    /// now lives in the folder's metadata, synced with its peers
    /// ([`crate::Repo::password_check`]). Only read, to carry it over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_check: Option<String>,
    /// The master password itself, so it is asked for only once per
    /// machine: remotes are encrypted with it and need it at every setup.
    /// Written in base64, so it doesn't show at a glance: that hides it
    /// from someone looking over your shoulder, not from anyone who can
    /// read the file.
    #[serde(
        rename = "password_base64",
        with = "base64_password",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub password: Option<String>,
    /// Where earlier versions kept the password, in the clear. Only read,
    /// to carry it over (see [`read_local`]).
    #[serde(rename = "password", default, skip_serializing)]
    plain_password: Option<String>,
    /// The last successful metadata backup, per remote name.
    #[serde(default)]
    pub metadata_backups: BTreeMap<String, BackupRecord>,
    /// This device's secret key for syncing the folder (hex), which its
    /// peers know it by; made on first use (see [`crate::peers`]).
    #[serde(default)]
    pub node_key: Option<String>,
    /// The devices this one syncs the folder with, by their id (an iroh
    /// endpoint id). Syncing starts once both have added each other.
    #[serde(default)]
    pub peers: BTreeMap<String, Peer>,
}

/// The master password as `local.json` stores it: base64.
mod base64_password {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(password: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
        match password {
            Some(password) => s.serialize_some(&STANDARD.encode(password)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        let Some(encoded) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        let bytes = STANDARD.decode(encoded).map_err(D::Error::custom)?;
        String::from_utf8(bytes).map(Some).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// What the user calls the device.
    pub name: String,
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
    /// Save the (verified) master password on this machine.
    pub fn keep_password(&mut self, password: &str) {
        self.password = Some(password.to_owned());
    }

    /// Forget what this machine knew about the folder's contents: it now
    /// holds another device's folder (see [`crate::peers`]). Its key and
    /// its peers stay.
    pub fn forget_folder(&mut self) {
        self.password_check = None;
        self.password = None;
        self.metadata_backups.clear();
    }
}

/// The app's own storage, where it has no config directory of its own
/// (Android): see [`AppState::use_app_dir`].
static APP_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

impl AppState {
    /// Keep the app state, and the app's folder, in `dir`: the app's own
    /// storage on Android, which has no config directory.
    pub fn use_app_dir(dir: PathBuf) {
        APP_DIR.set(dir).ok();
    }

    /// The folder the app keeps in its own storage (Android), if any.
    pub fn folder_dir() -> Option<PathBuf> {
        Some(APP_DIR.get()?.join("folder"))
    }

    /// `<config dir>/mbr/state.json` (or in the app's own storage).
    pub fn default_path() -> Option<PathBuf> {
        if let Some(dir) = APP_DIR.get() {
            return Some(dir.join("state.json"));
        }
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

pub(crate) fn read_local(root: &Path) -> RepoState {
    let file = root.join(LOCAL_STATE_FILE);
    let mut state = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring unreadable {}: {e}", file.display());
            RepoState::default()
        }),
        Err(_) => RepoState::default(),
    };
    // Written in base64 from the next save on.
    if let Some(plain) = state.plain_password.take() {
        state.password.get_or_insert(plain);
    }
    state
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
    fn the_password_is_stored_in_base64() {
        let dir = std::env::temp_dir().join(format!("mbr-state-b64-{}", std::process::id()));
        let path = dir.join("state.json");
        let folder = dir.join("folder");
        std::fs::create_dir_all(folder.join(".mbr")).unwrap();

        let mut state = AppState::default();
        state.repo_mut(&folder).keep_password("correct horse");
        state.save_to(&path).unwrap();
        let text = std::fs::read_to_string(folder.join(LOCAL_STATE_FILE)).unwrap();
        assert!(!text.contains("correct horse"), "{text}");
        assert!(text.contains("Y29ycmVjdCBob3JzZQ=="), "{text}");
        assert_eq!(
            AppState::load_from(&path).repo(&folder).password.as_deref(),
            Some("correct horse")
        );

        // Earlier versions wrote it in the clear: read, then rewritten.
        std::fs::write(folder.join(LOCAL_STATE_FILE), r#"{"password": "old one"}"#).unwrap();
        let mut state = AppState::load_from(&path);
        assert_eq!(state.repo_mut(&folder).password.as_deref(), Some("old one"));
        state.save_to(&path).unwrap();
        let text = std::fs::read_to_string(folder.join(LOCAL_STATE_FILE)).unwrap();
        assert!(!text.contains("old one"), "{text}");
        assert_eq!(read_local(&folder).password.as_deref(), Some("old one"));
        std::fs::remove_dir_all(dir).ok();
    }
}
