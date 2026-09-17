//! Hardcoded descriptions of the rclone backends mbr can configure.
//!
//! Each entry lists the config questions a user must answer to set up that
//! backend, so both frontends can show a ready-made form instead of walking
//! through rclone's one-question-at-a-time protocol. Field names are rclone
//! config keys (see the per-backend pages at https://rclone.org/#providers);
//! a test cross-checks them against the embedded rclone's own metadata.
//!
//! Fields flagged [`Field::to_target`] are not config keys at all: their
//! answer names the bucket or folder in the mbr target (`name:bucket`,
//! `name:bucket/path`, `name:share/folder`) and is never sent to rclone.
//!
//! Backends whose setup needs an interactive browser flow (OAuth) or
//! bespoke prompts are intentionally absent: `rclone` handles those
//! through the question fallback in [`crate::rclone`].

/// How a field's value is sent to rclone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Free-text value.
    Text,
    /// Password-like value; rclone obscures it before storing.
    Secret,
    /// One of [`Field::examples`].
    Choice,
}

/// One form field of a backend.
#[derive(Debug, Clone)]
pub struct Field {
    /// rclone config key (e.g. `host` for smb's `--smb-host`).
    pub name: &'static str,
    /// Human label shown in forms.
    pub label: &'static str,
    /// Longer hint shown under the label.
    pub help: &'static str,
    pub kind: Kind,
    /// Value used when the user leaves the field empty.
    pub default: &'static str,
    /// Allowed values for [`Kind::Choice`] fields.
    pub examples: &'static [&'static str],
    /// Whether a value must be non-empty.
    pub required: bool,
    /// Not a config key: consumed into the mbr target (`name:spec`).
    pub to_target: bool,
}

impl Field {
    const fn new(name: &'static str, label: &'static str, help: &'static str, kind: Kind) -> Self {
        Self {
            name,
            label,
            help,
            kind,
            default: "",
            examples: &[],
            required: false,
            to_target: false,
        }
    }

    const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    const fn secret(mut self) -> Self {
        self.kind = Kind::Secret;
        self
    }

    const fn default(mut self, default: &'static str) -> Self {
        self.default = default;
        self
    }

    const fn examples(mut self, examples: &'static [&'static str]) -> Self {
        self.kind = Kind::Choice;
        self.examples = examples;
        self
    }

    /// Answer names the bucket/folder in the target, not a config key.
    const fn to_target(mut self) -> Self {
        self.to_target = true;
        self
    }
}

/// One configurable backend.
#[derive(Debug, Clone)]
pub struct Backend {
    /// rclone backend type name (as in `rclone config create <name> <type>`).
    pub name: &'static str,
    /// Human title shown in the picker.
    pub title: &'static str,
    /// One-line description of what the backend connects to.
    pub description: &'static str,
    /// Whether an empty folder/bucket answer falls back to [`DEFAULT_PATH`].
    pub needs_path: bool,
    /// Form fields, in the order they are asked.
    pub fields: &'static [Field],
}

const TARGET_NOTE: &str = "where mbr stores blobs (created on push if missing)";

/// All backends mbr can configure with a form, in picker order.
pub const BACKENDS: &[Backend] = &[
    Backend {
        name: "local",
        title: "Local disk or directory",
        description: "Another folder on this machine or a mounted disk",
        needs_path: true,
        fields: &[Field::new("path", "Folder", TARGET_NOTE, Kind::Text)
            .default("mbr")
            .to_target()],
    },
    Backend {
        name: "sftp",
        title: "SFTP (SSH)",
        description: "Connect over SSH to another machine",
        needs_path: true,
        fields: &[
            Field::new(
                "host",
                "Host",
                "Hostname or IP address, e.g. server.example.com",
                Kind::Text,
            )
            .required(),
            Field::new("user", "User", "SSH username (defaults to the current user)", Kind::Text),
            Field::new("port", "Port", "SSH port", Kind::Text).default("22"),
            Field::new("pass", "Password", "SSH password", Kind::Text).secret(),
            Field::new(
                "key_file",
                "Key file",
                "Path to a private key (leave empty to use the SSH agent)",
                Kind::Text,
            ),
            Field::new("path", "Folder on the server", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .to_target(),
        ],
    },
    Backend {
        name: "smb",
        title: "SMB / CIFS",
        description: "Windows shares and Samba servers",
        needs_path: true,
        fields: &[
            Field::new("host", "Host", "Server hostname, e.g. example.com", Kind::Text).required(),
            Field::new(
                "user",
                "User",
                "SMB username (use guest with an empty password for anonymous access)",
                Kind::Text,
            )
            .default("$USER"),
            Field::new("port", "Port", "SMB port", Kind::Text).default("445"),
            Field::new("pass", "Password", "SMB password", Kind::Text).secret(),
            Field::new("domain", "Domain", "Domain for NTLM authentication", Kind::Text)
                .default("WORKGROUP"),
            Field::new(
                "path",
                "Share and folder",
                "Share name first, then folder, e.g. share/mbr",
                Kind::Text,
            )
            .default("mbr")
            .to_target(),
        ],
    },
    Backend {
        name: "webdav",
        title: "WebDAV",
        description: "Nextcloud, ownCloud, Sharepoint and other WebDAV servers",
        needs_path: true,
        fields: &[
            Field::new(
                "url",
                "URL",
                "Server URL including the WebDAV path, e.g. https://example.com/remote.php/webdav/",
                Kind::Text,
            )
            .required(),
            Field::new("vendor", "Vendor", "Which server software is running", Kind::Choice).examples(&[
                "nextcloud",
                "owncloud",
                "sharepoint",
                "sharepoint-ntlm",
                "fastmail",
                "rclone",
                "other",
            ]),
            Field::new(
                "user",
                "User",
                "User name (DOMAIN\\User for sharepoint-ntlm)",
                Kind::Text,
            ),
            Field::new("pass", "Password", "Password", Kind::Text).secret(),
            Field::new(
                "bearer_token",
                "Bearer token",
                "Token instead of user/password (e.g. a Macaroon)",
                Kind::Text,
            )
            .secret(),
            Field::new("path", "Folder on the server", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .to_target(),
        ],
    },
    Backend {
        name: "s3",
        title: "Amazon S3",
        description: "AWS S3 buckets (see the S3-compatible providers for other servers)",
        needs_path: false,
        fields: &[
            Field::new("region", "Region", "AWS region, e.g. us-east-1 (leave empty for env credentials)", Kind::Text),
            Field::new(
                "access_key_id",
                "Access key ID",
                "AWS access key ID (leave empty for env credentials)",
                Kind::Text,
            ),
            Field::new(
                "secret_access_key",
                "Secret access key",
                "AWS secret access key",
                Kind::Text,
            )
            .secret(),
            Field::new(
                "endpoint",
                "Endpoint",
                "Leave empty for AWS; set for S3-compatible servers",
                Kind::Text,
            ),
            Field::new("bucket", "Bucket", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .required()
                .to_target(),
        ],
    },
    Backend {
        name: "b2",
        title: "Backblaze B2",
        description: "Backblaze B2 buckets",
        needs_path: false,
        fields: &[
            Field::new(
                "account",
                "Application key ID",
                "Application key ID (not the master account ID)",
                Kind::Text,
            )
            .required(),
            Field::new("key", "Application key", "Application key", Kind::Text)
                .secret()
                .required(),
            Field::new("bucket", "Bucket", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .required()
                .to_target(),
        ],
    },
    Backend {
        name: "azureblob",
        title: "Microsoft Azure Blob Storage",
        description: "Azure containers",
        needs_path: false,
        fields: &[
            Field::new(
                "account",
                "Account",
                "Azure storage account name (leave empty with a SAS URL)",
                Kind::Text,
            ),
            Field::new("key", "Key", "Storage account key", Kind::Text).secret(),
            Field::new("sas_url", "SAS URL", "SAS URL instead of account/key", Kind::Text),
            Field::new("container", "Container", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .to_target(),
        ],
    },
    Backend {
        name: "gcs",
        title: "Google Cloud Storage",
        description: "GCS buckets (uses the embedded rclone's OAuth client)",
        needs_path: false,
        fields: &[
            Field::new(
                "project_number",
                "Project number",
                "Google Cloud project number (needed to create buckets)",
                Kind::Text,
            ),
            Field::new(
                "service_account_file",
                "Service account file",
                "Path to a service-account JSON key",
                Kind::Text,
            ),
            Field::new("bucket", "Bucket", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .required()
                .to_target(),
        ],
    },
    Backend {
        name: "swift",
        title: "OpenStack Swift",
        description: "OpenStack and compatible object stores",
        needs_path: false,
        fields: &[
            Field::new(
                "auth",
                "Auth URL",
                "Identity (Keystone) authentication URL",
                Kind::Text,
            ),
            Field::new("user", "User", "API user name", Kind::Text),
            Field::new("key", "Password", "API password", Kind::Text).secret(),
            Field::new("region", "Region", "Region name", Kind::Text),
            Field::new("container", "Container", TARGET_NOTE, Kind::Text)
                .default("mbr")
                .to_target(),
        ],
    },
    Backend {
        name: "pcloud",
        title: "pCloud",
        description: "pCloud account: sign in via rclone's browser flow after saving",
        needs_path: false,
        fields: &[],
    },
];

/// The backend with `name`, if any.
pub fn backend(name: &str) -> Option<&'static Backend> {
    BACKENDS.iter().find(|b| b.name == name)
}

/// Backend names for pickers and `mbr remote types`.
pub fn backend_names() -> Vec<&'static str> {
    BACKENDS.iter().map(|b| b.name).collect()
}

/// Default path prefix mbr uses when no user path is supplied.
pub const DEFAULT_PATH: &str = "mbr";

/// Build the rclone target for a configured remote. `spec` is the user's
/// bucket or path answer; empty answers fall back to [`DEFAULT_PATH`].
pub fn target_for(backend: &Backend, spec: &str) -> String {
    let spec = spec.trim().trim_matches('/');
    let spec = if spec.is_empty() && backend.needs_path {
        DEFAULT_PATH
    } else {
        spec
    };
    format!("{}:{}", backend.name, spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backends_have_unique_names() {
        let mut names: Vec<_> = backend_names();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BACKENDS.len());
    }

    #[test]
    fn every_field_has_help() {
        for b in BACKENDS {
            for f in b.fields {
                assert!(!f.label.is_empty(), "{}.{}", b.name, f.name);
                assert!(!f.help.is_empty(), "{}.{} lacks help text", b.name, f.name);
                match f.kind {
                    Kind::Choice => assert!(
                        !f.examples.is_empty(),
                        "{}.{} is a choice without examples",
                        b.name,
                        f.name
                    ),
                    Kind::Secret => assert!(
                        f.default.is_empty(),
                        "secret {}.{} has a hardcoded default",
                        b.name,
                        f.name
                    ),
                    Kind::Text => {}
                }
            }
        }
    }

    #[test]
    fn target_fields_are_named_like_paths() {
        for b in BACKENDS {
            for f in b.fields {
                if f.to_target {
                    assert!(
                        matches!(f.name, "bucket" | "container" | "path"),
                        "{}.{}: unexpected to_target field name",
                        b.name,
                        f.name
                    );
                }
            }
        }
    }

    #[test]
    fn backend_lookup() {
        assert!(backend("smb").is_some());
        assert!(backend("nope").is_none());
    }

    #[test]
    fn target_building() {
        let smb = backend("smb").unwrap();
        assert_eq!(target_for(smb, "share/sub"), "smb:share/sub");
        assert_eq!(target_for(smb, ""), "smb:mbr");
        let s3 = backend("s3").unwrap();
        assert_eq!(target_for(s3, "mybucket"), "s3:mybucket");
        assert_eq!(target_for(s3, "mybucket/"), "s3:mybucket");
    }
}
