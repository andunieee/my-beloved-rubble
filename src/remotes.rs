//! The rclone remotes mbr sets up.
//!
//! A remote the user names `drive` becomes rclone config sections that are
//! never shown by name:
//!
//! ```text
//! [_root-mbr-drive]    the backend itself (Google Drive, S3, …), at its root
//! [_plain-mbr-drive]   type = alias
//!                      remote = _root-mbr-drive:<bucket/>mbr-<folder id>
//! [_crypt-mbr-drive]   type = crypt
//!                      remote = _plain-mbr-drive:
//!                      password = the master password (obscured)
//! ```
//!
//! The mbr remote `drive` targets `_crypt-mbr-drive:`, so every blob is
//! encrypted, names included. A remote deliberately created without
//! encryption has `_dangerouslyplain-mbr-drive` (an alias like
//! `_plain-mbr-drive`) in place of the last two, and targets that.
//!
//! The folder id ([`Repo::folder_id`]) is random and kept in the database,
//! so several folders can share one storage account without mixing. Metadata
//! backups don't go in the folder's directory but next to it, in the
//! bucket or folder the user named (see [`backup_location`]): a lost folder
//! is found again from the remote's settings and the master password alone.

use crate::rclone::{self, SetupOutcome};
use crate::repo::Repo;
use serde_json::{Value, json};

pub const ROOT_PREFIX: &str = "_root-mbr-";
pub const PLAIN_PREFIX: &str = "_plain-mbr-";
pub const CRYPT_PREFIX: &str = "_crypt-mbr-";
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
    pub fn root_section(&self) -> String {
        format!("{ROOT_PREFIX}{}", self.name)
    }

    /// The alias for the folder's own directory in the backend.
    pub fn store_section(&self) -> String {
        let prefix = if self.encrypt { PLAIN_PREFIX } else { DANGEROUS_PREFIX };
        format!("{prefix}{}", self.name)
    }

    pub fn crypt_section(&self) -> String {
        format!("{CRYPT_PREFIX}{}", self.name)
    }

    /// Every section this remote is made of, in creation order.
    pub fn sections(&self) -> Vec<String> {
        let mut sections = vec![self.root_section(), self.store_section()];
        if self.encrypt {
            sections.push(self.crypt_section());
        }
        sections
    }

    /// The mbr target of the remote.
    pub fn target(&self) -> String {
        let section = if self.encrypt {
            self.crypt_section()
        } else {
            self.store_section()
        };
        format!("{section}:")
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

    /// Complete a setup step of the root section: once rclone is done with
    /// it, add the alias for the folder's directory (inside `spec`, the
    /// bucket or folder the user named) and the crypt layer keyed with
    /// `password`. If that fails, every section made so far is deleted
    /// again, so no half-made remote stays.
    pub fn finish(
        &self,
        outcome: Result<SetupOutcome, String>,
        spec: &str,
        folder_id: &str,
        password: &str,
    ) -> Result<SetupOutcome, String> {
        match outcome {
            Ok(SetupOutcome::Done) => {
                let created = self.create_layers(spec, folder_id, password);
                if created.is_err() {
                    for section in self.sections() {
                        rclone::delete_remote(&section).ok();
                    }
                }
                created.map(|()| SetupOutcome::Done)
            }
            other => other,
        }
    }

    fn create_layers(&self, spec: &str, folder_id: &str, password: &str) -> Result<(), String> {
        let store = format!("{}:{}", self.root_section(), location(spec, folder_id));
        create(&self.store_section(), "alias", json!({ "remote": store }))?;
        if self.encrypt {
            create(
                &self.crypt_section(),
                "crypt",
                json!({
                    "remote": format!("{}:", self.store_section()),
                    "password": password,
                    "filename_encryption": "standard",
                    "directory_name_encryption": "true",
                }),
            )?;
        }
        Ok(())
    }
}

/// Create a section that needs no questions answered.
fn create(section: &str, backend: &str, parameters: Value) -> Result<(), String> {
    match rclone::begin_setup(section, backend, parameters)? {
        SetupOutcome::Done => Ok(()),
        SetupOutcome::Question(q) => {
            rclone::cancel_setup(section, &q.state).ok();
            Err(format!("rclone asked '{}' while setting up {backend}", q.name))
        }
    }
}

/// The folder's own directory on every remote.
pub fn folder_dir(folder_id: &str) -> String {
    format!("mbr-{folder_id}")
}

/// Whether `id` looks like a folder id: 12 lowercase hex digits.
pub fn is_folder_id(id: &str) -> bool {
    id.len() == 12 && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Where in a backend the folder's directory is: inside the bucket, share
/// or local folder the user named, if any.
pub fn location(spec: &str, folder_id: &str) -> String {
    let dir = folder_dir(folder_id);
    let spec = spec.trim();
    match spec.trim_end_matches('/') {
        "" if spec.starts_with('/') => format!("/{dir}"),
        "" => dir,
        parent => format!("{parent}/{dir}"),
    }
}

/// The rclone path of the place the user named (bucket, share, local
/// folder or nothing) in root section `section`: where metadata backups go.
pub fn root_location(section: &str, spec: &str) -> String {
    format!("{section}:{}", spec.trim())
}

/// `value` (an alias's `remote`) without its folder directory: the
/// [`root_location`] it was made from.
fn parent_location(value: &str) -> Option<String> {
    let cut = value.rfind(['/', ':'])?;
    let (head, own) = (&value[..cut], &value[cut + 1..]);
    if !own.strip_prefix("mbr-").is_some_and(is_folder_id) {
        return None;
    }
    Some(if value[cut..].starts_with(':') {
        format!("{head}:")
    } else if head.ends_with(':') {
        format!("{head}/")
    } else {
        head.to_owned()
    })
}

/// Where metadata backups go for a remote targeted at `target`: the root
/// of its backend, next to the folder's directory. Remotes not set up by
/// mbr this way keep them in the target itself. Run with the folder's
/// rclone config selected.
pub fn backup_location(target: &str) -> Result<String, String> {
    let Some(store) = store_section(target) else {
        return Ok(target.to_owned());
    };
    let config = rclone::remote_config(&store)?
        .ok_or_else(|| format!("the rclone settings '{store}' are missing"))?;
    let value = config
        .get("remote")
        .and_then(Value::as_str)
        .unwrap_or_default();
    parent_location(value)
        .ok_or_else(|| format!("the rclone settings '{store}' point at an unexpected '{value}'"))
}

/// How a remote's target protects what it stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Through a `_crypt-mbr-` section.
    Encrypted,
    /// A `_dangerouslyplain-mbr-` section, by the user's choice.
    Plain,
    /// Not set up by mbr this way (an older or hand-made remote).
    Unknown,
}

/// How `target` protects what it stores, and the user's name for the
/// remote behind it, if mbr laid it out.
fn base_name(target: &str) -> Option<(Protection, &str)> {
    let section = rclone::target_section(target)?;
    if let Some(name) = section.strip_prefix(CRYPT_PREFIX) {
        Some((Protection::Encrypted, name))
    } else {
        Some((Protection::Plain, section.strip_prefix(DANGEROUS_PREFIX)?))
    }
}

pub fn protection(target: &str) -> Protection {
    base_name(target).map_or(Protection::Unknown, |(protection, _)| protection)
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

/// The alias section for the folder's directory behind a target.
fn store_section(target: &str) -> Option<String> {
    let (protection, name) = base_name(target)?;
    Some(match protection {
        Protection::Encrypted => format!("{PLAIN_PREFIX}{name}"),
        _ => format!("{DANGEROUS_PREFIX}{name}"),
    })
}

/// The section holding the backend settings behind a target.
pub fn settings_section(target: &str) -> Option<String> {
    match base_name(target) {
        Some((_, name)) => Some(format!("{ROOT_PREFIX}{name}")),
        None => rclone::target_section(target).map(str::to_owned),
    }
}

/// The sections mbr made for a target, to delete along with the remote.
pub fn owned_sections(target: &str) -> Vec<String> {
    let Some((protection, name)) = base_name(target) else {
        return Vec::new();
    };
    let mut sections = Vec::new();
    if protection == Protection::Encrypted {
        sections.push(format!("{CRYPT_PREFIX}{name}"));
    }
    sections.extend(store_section(target));
    sections.push(format!("{ROOT_PREFIX}{name}"));
    sections
}

impl Repo {
    /// This folder's id (12 hex digits), made on first use.
    pub fn folder_id(&mut self) -> Result<String, String> {
        if let Some(id) = self.db.setting(FOLDER_ID_SETTING)?
            && is_folder_id(&id)
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
        assert_eq!(
            encrypted.sections(),
            ["_root-mbr-drive", "_plain-mbr-drive", "_crypt-mbr-drive"]
        );
        assert_eq!(encrypted.target(), "_crypt-mbr-drive:");
        let plain = Layout::new("usb", false).unwrap();
        assert_eq!(plain.sections(), ["_root-mbr-usb", "_dangerouslyplain-mbr-usb"]);
        assert_eq!(plain.target(), "_dangerouslyplain-mbr-usb:");
        for bad in ["", "_x", "-x", "a b", "a:b", "a/b"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert!(check_name("my.drive_2-b").is_ok());

        assert_eq!(
            owned_sections("_crypt-mbr-drive:"),
            ["_crypt-mbr-drive", "_plain-mbr-drive", "_root-mbr-drive"]
        );
        assert_eq!(
            owned_sections("_dangerouslyplain-mbr-usb:"),
            ["_dangerouslyplain-mbr-usb", "_root-mbr-usb"]
        );
        assert_eq!(owned_sections("old:bucket"), Vec::<String>::new());
        assert_eq!(
            settings_section("_crypt-mbr-drive:").as_deref(),
            Some("_root-mbr-drive")
        );
        assert_eq!(settings_section("old:bucket").as_deref(), Some("old"));
    }

    #[test]
    fn locations_and_descriptions() {
        let id = "0123456789ab";
        assert_eq!(location("", id), "mbr-0123456789ab");
        assert_eq!(location("bucket/", id), "bucket/mbr-0123456789ab");
        assert_eq!(location("/mnt/disk", id), "/mnt/disk/mbr-0123456789ab");
        assert_eq!(location("/", id), "/mbr-0123456789ab");
        assert!(is_folder_id(id));
        assert!(!is_folder_id("0123456789AB"));

        // Backups go where the folder directory was made from.
        for spec in ["", "bucket", "/mnt/disk", "/"] {
            let alias = format!("_root-mbr-d:{}", location(spec, id));
            assert_eq!(
                parent_location(&alias).unwrap(),
                root_location("_root-mbr-d", spec),
                "{spec}"
            );
        }
        assert_eq!(parent_location("_root-mbr-d:bucket/other"), None);

        assert_eq!(describe("_crypt-mbr-drive:"), "encrypted");
        assert_eq!(describe("_dangerouslyplain-mbr-usb:"), "NOT encrypted");
        assert_eq!(describe("old:bucket"), "old:bucket");
    }
}
