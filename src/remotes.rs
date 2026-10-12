//! The rclone remotes mbr sets up.
//!
//! A remote the user names `drive` becomes rclone config sections that are
//! never shown by name:
//!
//! ```text
//! [_plain-mbr-drive]       the backend itself (Google Drive, S3, …)
//! [_autocrypt-mbr-drive]   type = crypt
//!                          remote = _plain-mbr-drive:<bucket/>mbr-<folder id>
//!                          password = the master password (obscured)
//! ```
//!
//! and the mbr remote `drive` targets `_autocrypt-mbr-drive:`, so every blob
//! and metadata backup is encrypted, names included. A remote deliberately
//! created without encryption is the single section
//! `_dangerouslyplain-mbr-drive`, targeted at
//! `_dangerouslyplain-mbr-drive:<bucket/>mbr-<folder id>`.
//!
//! The folder id ([`Repo::folder_id`]) is random and kept in the database,
//! so several folders can share one storage account without mixing.

use crate::rclone::{self, SetupOutcome};
use crate::repo::{self, Repo};
use serde_json::json;

pub const PLAIN_PREFIX: &str = "_plain-mbr-";
pub const CRYPT_PREFIX: &str = "_autocrypt-mbr-";
pub const DANGEROUS_PREFIX: &str = "_dangerouslyplain-mbr-";
/// Database setting holding the folder id.
pub const FOLDER_ID_SETTING: &str = "folder_id";

/// Refuse names that rclone can't use in a section name, or that would
/// look like one of mbr's own prefixes.
pub fn check_name(name: &str) -> Result<(), String> {
    let valid = name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return Err(format!(
            "'{name}' can't name a remote: use letters, digits, '-', '_' and '.', \
             starting with a letter or digit"
        ));
    }
    Ok(())
}

/// How a new remote named by the user is laid out in rclone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The name the user chose, and the mbr remote's name.
    pub name: String,
    /// Wrap the backend in a crypt remote (the default).
    pub encrypt: bool,
}

impl Layout {
    pub fn new(name: &str, encrypt: bool) -> Result<Self, String> {
        check_name(name)?;
        Ok(Self {
            name: name.to_owned(),
            encrypt,
        })
    }

    /// The section holding the backend (credentials, endpoint, …).
    pub fn backend_section(&self) -> String {
        let prefix = if self.encrypt { PLAIN_PREFIX } else { DANGEROUS_PREFIX };
        format!("{prefix}{}", self.name)
    }

    pub fn crypt_section(&self) -> String {
        format!("{CRYPT_PREFIX}{}", self.name)
    }

    /// Every section this remote is made of.
    pub fn sections(&self) -> Vec<String> {
        let mut sections = vec![self.backend_section()];
        if self.encrypt {
            sections.push(self.crypt_section());
        }
        sections
    }

    /// The mbr target of the remote, given where in the backend it stores.
    pub fn target(&self, location: &str) -> String {
        if self.encrypt {
            format!("{}:", self.crypt_section())
        } else {
            format!("{}:{location}", self.backend_section())
        }
    }

    /// Refuse to set up a remote whose sections exist already. Run with
    /// the folder's rclone config selected.
    pub fn check_unused(&self) -> Result<(), String> {
        for section in self.sections() {
            if rclone::remote_exists(&section)? {
                return Err(format!(
                    "the name '{}' is still used by an earlier remote's settings",
                    self.name
                ));
            }
        }
        Ok(())
    }

    /// Complete a setup step of the backend section: once rclone is done
    /// with it, add the crypt layer keyed with `password`. If that fails,
    /// the backend section is deleted again, so no half-made remote stays.
    pub fn finish(
        &self,
        outcome: Result<SetupOutcome, String>,
        location: &str,
        password: &str,
    ) -> Result<SetupOutcome, String> {
        match outcome {
            Ok(SetupOutcome::Done) if self.encrypt => {
                let created = self.create_crypt(location, password);
                if created.is_err() {
                    rclone::delete_remote(&self.backend_section()).ok();
                }
                created.map(|()| SetupOutcome::Done)
            }
            other => other,
        }
    }

    fn create_crypt(&self, location: &str, password: &str) -> Result<(), String> {
        let parameters = json!({
            "remote": format!("{}:{location}", self.backend_section()),
            "password": password,
            "filename_encryption": "standard",
            "directory_name_encryption": "true",
        });
        match rclone::begin_setup(&self.crypt_section(), "crypt", parameters)? {
            SetupOutcome::Done => Ok(()),
            SetupOutcome::Question(q) => {
                rclone::cancel_setup(&self.crypt_section(), &q.state).ok();
                Err(format!("rclone asked '{}' while setting up encryption", q.name))
            }
        }
    }
}

/// The folder's own directory on every remote.
pub fn folder_dir(folder_id: &str) -> String {
    format!("mbr-{folder_id}")
}

/// The folder id in a directory name made by [`folder_dir`].
pub fn parse_folder_dir(dir: &str) -> Option<&str> {
    let id = dir.strip_prefix("mbr-")?;
    let valid = id.len() == 12 && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    valid.then_some(id)
}

/// Where in a backend mbr stores: the folder's directory, inside the
/// bucket, share or local folder the user named, if any.
pub fn location(spec: &str, folder_id: &str) -> String {
    let dir = folder_dir(folder_id);
    let spec = spec.trim();
    match spec.trim_end_matches('/') {
        "" if spec.starts_with('/') => format!("/{dir}"),
        "" => dir,
        parent => format!("{parent}/{dir}"),
    }
}

/// How a remote's target protects what it stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Through an `_autocrypt-mbr-` section.
    Encrypted,
    /// A `_dangerouslyplain-mbr-` section, by the user's choice.
    Plain,
    /// Not set up by mbr this way (an older or hand-made remote).
    Unknown,
}

pub fn protection(target: &str) -> Protection {
    match rclone::target_section(target) {
        Some(s) if s.starts_with(CRYPT_PREFIX) => Protection::Encrypted,
        Some(s) if s.starts_with(DANGEROUS_PREFIX) => Protection::Plain,
        _ => Protection::Unknown,
    }
}

/// A short description of a target, for listings: mbr's own sections are
/// never shown by name.
pub fn describe(target: &str) -> String {
    match protection(target) {
        Protection::Encrypted => "encrypted".to_owned(),
        Protection::Plain => "NOT encrypted".to_owned(),
        Protection::Unknown => target.to_owned(),
    }
}

/// The section holding the backend settings behind a target: for an
/// encrypted remote, the one under the crypt layer.
pub fn settings_section(target: &str) -> Option<String> {
    let section = rclone::target_section(target)?;
    Some(match section.strip_prefix(CRYPT_PREFIX) {
        Some(name) => format!("{PLAIN_PREFIX}{name}"),
        None => section.to_owned(),
    })
}

/// The sections mbr made for a target, to delete along with the remote.
pub fn owned_sections(target: &str) -> Vec<String> {
    let Some(section) = rclone::target_section(target) else {
        return Vec::new();
    };
    if let Some(name) = section.strip_prefix(CRYPT_PREFIX) {
        vec![section.to_owned(), format!("{PLAIN_PREFIX}{name}")]
    } else if section.starts_with(DANGEROUS_PREFIX) {
        vec![section.to_owned()]
    } else {
        Vec::new()
    }
}

/// `value` with its last path component `old_dir` replaced by `new_dir`,
/// if it ends with it.
fn retarget(value: &str, old_dir: &str, new_dir: &str) -> Option<String> {
    let head = value.strip_suffix(old_dir)?;
    (head.ends_with(':') || head.ends_with('/')).then(|| format!("{head}{new_dir}"))
}

/// An rclone config with the `remote` of every crypt section mbr made
/// moved from `old_dir` to `new_dir`.
fn retarget_crypt_sections(config: &str, old_dir: &str, new_dir: &str) -> String {
    let mut out = String::with_capacity(config.len());
    let mut in_crypt = false;
    for line in config.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_crypt = name.starts_with(CRYPT_PREFIX);
        } else if in_crypt
            && let Some((key, value)) = trimmed.split_once('=')
            && key.trim() == "remote"
            && let Some(value) = retarget(value.trim(), old_dir, new_dir)
        {
            out.push_str(&format!("remote = {value}\n"));
            continue;
        }
        out.push_str(line);
    }
    out
}

impl Repo {
    /// This folder's id (12 hex digits), made on first use.
    pub fn folder_id(&mut self) -> Result<String, String> {
        if let Some(id) = self.db.setting(FOLDER_ID_SETTING)?
            && parse_folder_dir(&folder_dir(&id)).is_some()
        {
            return Ok(id);
        }
        use argon2::password_hash::rand_core::{OsRng, RngCore};
        let mut bytes = [0u8; 6];
        OsRng.fill_bytes(&mut bytes);
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.db.set_setting(FOLDER_ID_SETTING, &id)?;
        Ok(id)
    }

    /// Take on `id` as this folder's id, moving the remotes set up for the
    /// old one over to it. Used when restoring a lost folder into a new
    /// one: its remotes were set up before the old id was known.
    pub fn adopt_folder_id(&mut self, id: &str) -> Result<(), String> {
        if parse_folder_dir(&folder_dir(id)).is_none() {
            return Err(format!("'{id}' is not a folder id"));
        }
        let old = match self.db.setting(FOLDER_ID_SETTING)? {
            Some(old) if old == id => return Ok(()),
            Some(old) => old,
            None => return self.db.set_setting(FOLDER_ID_SETTING, id),
        };
        let (old_dir, new_dir) = (folder_dir(&old), folder_dir(id));

        let path = self.rclone_config_path();
        let config = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, retarget_crypt_sections(&config, &old_dir, &new_dir))
            .map_err(|e| format!("cannot write {}: {e}", repo::RCLONE_CONFIG_FILE))?;
        repo::ensure_rclone_config(&path)?;
        rclone::forget_cached_remotes()?;

        self.db.transaction(|db| {
            for remote in db.list_remotes()? {
                if let Some(target) = retarget(&remote.target, &old_dir, &new_dir) {
                    db.update_remote(&remote.name, &remote.name, &target)?;
                }
            }
            db.set_setting(FOLDER_ID_SETTING, id)
        })
    }

    /// Remotes encrypted with the master password, which therefore can't
    /// change: they couldn't read what they stored before.
    pub fn encrypted_remotes(&mut self) -> Result<Vec<String>, String> {
        Ok(self
            .db
            .list_remotes()?
            .into_iter()
            .filter(|r| protection(&r.target) == Protection::Encrypted)
            .map(|r| r.name)
            .collect())
    }

    /// Forget remote `name`, deleting the rclone sections mbr made for it.
    /// What it stores stays where it is.
    pub fn remove_remote(&mut self, name: &str) -> Result<(), String> {
        let remote = self
            .db
            .remote(name)?
            .ok_or_else(|| format!("no remote named '{name}'"))?;
        self.with_rclone(|| delete_sections(&remote.target))?;
        self.db.remove_remote(name)
    }
}

/// Delete the rclone sections mbr made for `target`. Run with the folder's
/// rclone config selected.
pub fn delete_sections(target: &str) -> Result<(), String> {
    for section in owned_sections(target) {
        if rclone::remote_exists(&section)? {
            rclone::delete_remote(&section)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sections() {
        let encrypted = Layout::new("drive", true).unwrap();
        assert_eq!(encrypted.sections(), ["_plain-mbr-drive", "_autocrypt-mbr-drive"]);
        assert_eq!(encrypted.target("x/mbr-0123456789ab"), "_autocrypt-mbr-drive:");
        let plain = Layout::new("usb", false).unwrap();
        assert_eq!(plain.sections(), ["_dangerouslyplain-mbr-usb"]);
        assert_eq!(plain.target("/mnt/mbr-0"), "_dangerouslyplain-mbr-usb:/mnt/mbr-0");
        for bad in ["", "_x", "-x", "a b", "a:b", "a/b"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check_name("my.drive_2-b").is_ok());
    }

    #[test]
    fn locations_and_descriptions() {
        let id = "0123456789ab";
        assert_eq!(location("", id), "mbr-0123456789ab");
        assert_eq!(location("bucket/", id), "bucket/mbr-0123456789ab");
        assert_eq!(location("/mnt/disk", id), "/mnt/disk/mbr-0123456789ab");
        assert_eq!(location("/", id), "/mbr-0123456789ab");
        assert_eq!(parse_folder_dir("mbr-0123456789ab"), Some(id));
        assert_eq!(parse_folder_dir("mbr-0123456789AB"), None);
        assert_eq!(parse_folder_dir("mbr-metadata"), None);

        assert_eq!(describe("_autocrypt-mbr-drive:"), "encrypted");
        assert_eq!(describe("_dangerouslyplain-mbr-usb:mbr-x"), "NOT encrypted");
        assert_eq!(describe("old:bucket"), "old:bucket");
        assert_eq!(
            settings_section("_autocrypt-mbr-drive:").as_deref(),
            Some("_plain-mbr-drive")
        );
        assert_eq!(owned_sections("old:bucket"), Vec::<String>::new());
    }

    #[test]
    fn retargeting_moves_only_the_folder_directory() {
        let (old, new) = ("mbr-000000000000", "mbr-111111111111");
        assert_eq!(retarget("s:b/mbr-000000000000", old, new).unwrap(), "s:b/mbr-111111111111");
        assert_eq!(retarget("s:mbr-000000000000", old, new).unwrap(), "s:mbr-111111111111");
        assert_eq!(retarget("s:xmbr-000000000000", old, new), None);
        let config = "[_autocrypt-mbr-d]\ntype = crypt\nremote = _plain-mbr-d:b/mbr-000000000000\n\n\
                      [other]\nremote = x:mbr-000000000000\n";
        assert_eq!(
            retarget_crypt_sections(config, old, new),
            "[_autocrypt-mbr-d]\ntype = crypt\nremote = _plain-mbr-d:b/mbr-111111111111\n\n\
             [other]\nremote = x:mbr-000000000000\n"
        );
    }
}
