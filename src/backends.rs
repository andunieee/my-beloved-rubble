//! The rclone backends mbr can configure.
//!
//! Each entry lists the config questions a user must answer to set up that
//! backend, so both frontends can show a ready-made form instead of walking
//! through rclone's one-question-at-a-time protocol. Field names are rclone
//! config keys (see the per-backend pages at https://rclone.org/#providers).
//!
//! The only mbr-specific additions are the [`Field::to_target`] fields:
//! their answer names the bucket or folder in the mbr target (`name:bucket`,
//! `name:bucket/path`, `name:share/folder`) and is never sent to rclone.
//!
//! Backends whose setup needs an interactive browser flow (OAuth) still
//! appear here; `rclone` handles their extra prompts through the question
//! fallback in [`crate::rclone`].

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

// All backends mbr can configure with a form, in picker order.
pub const BACKENDS: &[Backend] = &[
    Backend { name: "local", title: "Local disk or directory", description: "Another folder on this machine or a mounted disk", needs_path: true, fields: &[
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "fichier", title: "1Fichier", description: "1Fichier", needs_path: true, fields: &[
        Field { name: "api_key", label: "API Key", help: "Your API Key, get it from https://1fichier.com/console/params.pl.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "netstorage", title: "Akamai NetStorage", description: "Akamai NetStorage", needs_path: true, fields: &[
        Field { name: "host", label: "Host", help: "Domain+path of NetStorage host to connect to. Format should be `<domain>/<internal folders>`", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "account", label: "Account", help: "Set the NetStorage account name", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "secret", label: "Secret", help: "Set the NetStorage account secret/G2O key for authentication. Please choose the 'y' option to set your own password then enter your secret.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "alias", title: "Alias", description: "Alias for an existing remote", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote or path to alias. Can be \"myremote:path/to/dir\", \"myremote:bucket\", \"myremote:\" or \"/local/path\".", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "s3", title: "Amazon S3", description: "AWS S3 buckets and S3-compatible object stores", needs_path: false, fields: &[
        Field { name: "provider", label: "Provider", help: "Choose your S3 provider.", kind: Kind::Choice, default: "", examples: &["AWS", "Alibaba", "ArvanCloud", "Ceph", "ChinaMobile", "Cloudflare", "DigitalOcean", "Dreamhost", "GCS", "HuaweiOBS", "IBMCOS", "IDrive", "IONOS", "LyveCloud", "Leviia", "Liara", "Linode", "Magalu", "Minio", "Netease", "Outscale", "Petabox", "RackCorp", "Rclone", "Scaleway", "SeaweedFS", "Selectel", "StackPath", "Storj", "Synology", "TencentCOS", "Wasabi", "Qiniu", "Other"], required: false, to_target: false },
        Field { name: "env_auth", label: "Env Auth", help: "Get AWS credentials from runtime (environment variables or EC2/ECS meta data if no env vars). Only applies if access_key_id and secret_access_key is blank.", kind: Kind::Choice, default: "false", examples: &["false", "true"], required: false, to_target: false },
        Field { name: "access_key_id", label: "Access Key ID", help: "AWS Access Key ID. Leave blank for anonymous access or runtime credentials.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "secret_access_key", label: "Secret Access Key", help: "AWS Secret Access Key (password). Leave blank for anonymous access or runtime credentials.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "region", label: "Region", help: "Region to connect to. Leave blank if you are using an S3 clone and you don't have a region.", kind: Kind::Text, default: "", examples: &["", "other-v2-signature"], required: false, to_target: false },
        Field { name: "endpoint", label: "Endpoint", help: "Endpoint for S3 API. Required when using an S3 clone.", kind: Kind::Text, default: "", examples: &["objects-us-east-1.dream.io", "syd1.digitaloceanspaces.com", "sfo3.digitaloceanspaces.com", "fra1.digitaloceanspaces.com", "nyc3.digitaloceanspaces.com", "ams3.digitaloceanspaces.com", "sgp1.digitaloceanspaces.com", "localhost:8333", "s3.us-east-1.lyvecloud.seagate.com", "s3.us-west-1.lyvecloud.seagate.com", "s3.ap-southeast-1.lyvecloud.seagate.com", "oos.eu-west-2.outscale.com", "oos.us-east-2.outscale.com", "oos.us-west-1.outscale.com", "oos.cloudgouv-eu-west-1.outscale.com", "oos.ap-northeast-1.outscale.com", "s3.wasabisys.com", "s3.us-east-2.wasabisys.com", "s3.us-central-1.wasabisys.com", "s3.us-west-1.wasabisys.com", "s3.ca-central-1.wasabisys.com", "s3.eu-central-1.wasabisys.com", "s3.eu-central-2.wasabisys.com", "s3.eu-west-1.wasabisys.com", "s3.eu-west-2.wasabisys.com", "s3.eu-south-1.wasabisys.com", "s3.ap-northeast-1.wasabisys.com", "s3.ap-northeast-2.wasabisys.com", "s3.ap-southeast-1.wasabisys.com", "s3.ap-southeast-2.wasabisys.com", "storage.iran.liara.space", "s3.ir-thr-at1.arvanstorage.ir", "s3.ir-tbz-sh1.arvanstorage.ir", "br-se1.magaluobjects.com", "br-ne1.magaluobjects.com"], required: false, to_target: false },
        Field { name: "location_constraint", label: "Location Constraint", help: "Location constraint - must be set to match the Region. Leave blank if not sure. Used when creating buckets only.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "acl", label: "ACL", help: "Canned ACL used when creating buckets and storing or copying objects. This ACL is used for creating objects and if bucket_acl isn't set, for creating buckets too. For more info visit https://docs.aws.amazon.com/AmazonS3/latest/dev/acl-overview.html#canned-acl Note that this ACL is applied when serve…", kind: Kind::Text, default: "", examples: &["default", "private", "public-read", "public-read-write", "authenticated-read", "bucket-owner-read", "bucket-owner-full-control", "private", "public-read", "public-read-write", "authenticated-read"], required: false, to_target: false },
        Field { name: "server_side_encryption", label: "Server Side Encryption", help: "The server-side encryption algorithm used when storing this object in S3.", kind: Kind::Text, default: "", examples: &["", "AES256", "aws:kms"], required: false, to_target: false },
        Field { name: "sse_kms_key_id", label: "SSE KMS Key ID", help: "If using KMS ID you must provide the ARN of Key.", kind: Kind::Secret, default: "", examples: &["", "arn:aws:kms:us-east-1:*"], required: false, to_target: false },
        Field { name: "storage_class", label: "Storage Class", help: "The storage class to use when storing new objects in S3.", kind: Kind::Text, default: "", examples: &["", "STANDARD", "REDUCED_REDUNDANCY", "STANDARD_IA", "ONEZONE_IA", "GLACIER", "DEEP_ARCHIVE", "INTELLIGENT_TIERING", "GLACIER_IR"], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: true, to_target: true },
    ] },
    Backend { name: "b2", title: "Backblaze B2", description: "Backblaze B2 buckets", needs_path: false, fields: &[
        Field { name: "account", label: "Account", help: "Account ID or Application Key ID.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "key", label: "Key", help: "Application Key.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "hard_delete", label: "Hard Delete", help: "Permanently delete files on remote removal, otherwise hide files.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: true, to_target: true },
    ] },
    Backend { name: "box", title: "Box", description: "Box", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "box_config_file", label: "Box Config File", help: "Box App config.json location Leave blank normally. Leading `~` will be expanded in the file name as will environment variables such as `${RCLONE_CONFIG_DIR}`.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "access_token", label: "Access Token", help: "Box App Primary Access Token Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "box_sub_type", label: "Box Sub Type", help: "Box Sub Type", kind: Kind::Choice, default: "user", examples: &["user", "enterprise"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "cache", title: "Cache", description: "Cache a remote", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote to cache. Normally should contain a ':' and a path, e.g. \"myremote:path/to/dir\", \"myremote:bucket\" or maybe \"myremote:\" (not recommended).", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "plex_url", label: "Plex URL", help: "The URL of the Plex server.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "plex_username", label: "Plex Username", help: "The username of the Plex user.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "plex_password", label: "Plex Password", help: "The password of the Plex user.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "chunk_size", label: "Chunk Size", help: "The size of a chunk (partial file data). Use lower numbers for slower connections. If the chunk size is changed, any downloaded chunks will be invalid and cache-chunk-path will need to be cleared or unexpected EOF errors will occur.", kind: Kind::Choice, default: "5Mi", examples: &["1M", "5M", "10M"], required: false, to_target: false },
        Field { name: "info_age", label: "Info Age", help: "How long to cache file structure information (directory listings, file size, times, etc.). If all write operations are done through the cache then you can safely make this value very large as the cache store will also be updated in real time.", kind: Kind::Choice, default: "6h0m0s", examples: &["1h", "24h", "48h"], required: false, to_target: false },
        Field { name: "chunk_total_size", label: "Chunk Total Size", help: "The total size that the chunks can take up on the local disk. If the cache exceeds this value then it will start to delete the oldest chunks until it goes under this value.", kind: Kind::Choice, default: "10Gi", examples: &["500M", "1G", "10G"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "chunker", title: "Chunker", description: "Transparently chunk/split large files", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote to chunk/unchunk. Normally should contain a ':' and a path, e.g. \"myremote:path/to/dir\", \"myremote:bucket\" or maybe \"myremote:\" (not recommended).", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "chunk_size", label: "Chunk Size", help: "Files larger than chunk size will be split in chunks.", kind: Kind::Text, default: "2Gi", examples: &[], required: false, to_target: false },
        Field { name: "hash_type", label: "Hash Type", help: "Choose how chunker handles hash sums. All modes but \"none\" require metadata.", kind: Kind::Choice, default: "md5", examples: &["none", "md5", "sha1", "md5all", "sha1all", "md5quick", "sha1quick"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "sharefile", title: "Citrix ShareFile", description: "Citrix Sharefile", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "root_folder_id", label: "Root Folder ID", help: "ID of the root folder. Leave blank to access \"Personal Folders\". You can use one of the standard values here or any folder ID (long hex number ID).", kind: Kind::Secret, default: "", examples: &["", "favorites", "allshared", "connectors", "top"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "cloudinary", title: "Cloudinary", description: "Cloudinary", needs_path: true, fields: &[
        Field { name: "cloud_name", label: "Cloud Name", help: "Cloudinary Environment Name", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "api_key", label: "API Key", help: "Cloudinary API Key", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "api_secret", label: "API Secret", help: "Cloudinary API Secret", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "upload_prefix", label: "Upload Prefix", help: "Specify the API endpoint for environments out of the US", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "upload_preset", label: "Upload Preset", help: "Upload Preset to select asset manipulation on upload", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "combine", title: "Combine", description: "Combine several remotes into one", needs_path: true, fields: &[
        Field { name: "upstreams", label: "Upstreams", help: "Upstreams for combining These should be in the form dir=remote:path dir2=remote2:path Where before the = is specified the root directory and after is the remote to put there. Embedded spaces can be added using quotes \"dir=remote:path with space\" \"dir2=remote2:path with space\"", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "compress", title: "Compress", description: "Compress a remote", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote to compress.", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "mode", label: "Mode", help: "Compression mode.", kind: Kind::Text, default: "gzip", examples: &["gzip"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "crypt", title: "Crypt", description: "Encrypt/Decrypt a remote", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote to encrypt/decrypt. Normally should contain a ':' and a path, e.g. \"myremote:path/to/dir\", \"myremote:bucket\" or maybe \"myremote:\" (not recommended).", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "filename_encryption", label: "Filename Encryption", help: "How to encrypt the filenames.", kind: Kind::Choice, default: "standard", examples: &["standard", "obfuscate", "off"], required: false, to_target: false },
        Field { name: "directory_name_encryption", label: "Directory Name Encryption", help: "Option to either encrypt directory names or leave them intact. NB If filename_encryption is \"off\" then this option will do nothing.", kind: Kind::Choice, default: "true", examples: &["true", "false"], required: false, to_target: false },
        Field { name: "password", label: "Password", help: "Password or pass phrase for encryption.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "password2", label: "Password2", help: "Password or pass phrase for salt. Optional but recommended. Should be different to the previous password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "dropbox", title: "Dropbox", description: "Dropbox", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "filefabric", title: "Enterprise File Fabric", description: "Enterprise File Fabric", needs_path: true, fields: &[
        Field { name: "url", label: "URL", help: "URL of the Enterprise File Fabric to connect to.", kind: Kind::Choice, default: "", examples: &["https://storagemadeeasy.com", "https://eu.storagemadeeasy.com", "https://yourfabric.smestorage.com"], required: true, to_target: false },
        Field { name: "root_folder_id", label: "Root Folder ID", help: "ID of the root folder. Leave blank normally. Fill in to make rclone start with directory of a given ID.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "permanent_token", label: "Permanent Token", help: "Permanent Authentication Token. A Permanent Authentication Token can be created in the Enterprise File Fabric, on the users Dashboard under Security, there is an entry you'll see called \"My Authentication Tokens\". Click the Manage button to create one. These tokens are normally valid for several yea…", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "ftp", title: "FTP", description: "FTP", needs_path: true, fields: &[
        Field { name: "host", label: "Host", help: "FTP host to connect to. E.g. \"ftp.example.com\".", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "user", label: "User", help: "FTP username.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "port", label: "Port", help: "FTP port number.", kind: Kind::Text, default: "21", examples: &[], required: false, to_target: false },
        Field { name: "pass", label: "Pass", help: "FTP password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tls", label: "TLS", help: "Use Implicit FTPS (FTP over TLS). When using implicit FTP over TLS the client connects using TLS right from the start which breaks compatibility with non-TLS-aware servers. This is usually served over port 990 rather than port 21. Cannot be used in combination with explicit FTPS.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "explicit_tls", label: "Explicit TLS", help: "Use Explicit FTPS (FTP over TLS). When using explicit FTP over TLS the client explicitly requests security from the server in order to upgrade a plain text connection to an encrypted one. Cannot be used in combination with implicit FTPS.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "filescom", title: "Files.com", description: "Files.com", needs_path: true, fields: &[
        Field { name: "site", label: "Site", help: "Your site subdomain (e.g. mysite) or custom domain (e.g. myfiles.customdomain.com).", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "username", label: "Username", help: "The username used to authenticate with Files.com.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "password", label: "Password", help: "The password used to authenticate with Files.com.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "gofile", title: "Gofile", description: "Gofile", needs_path: true, fields: &[
        Field { name: "access_token", label: "Access Token", help: "API Access token You can get this from the web control panel.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "gcs", title: "Google Cloud Storage", description: "GCS buckets (uses the embedded rclone's OAuth client)", needs_path: false, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "project_number", label: "Project Number", help: "Project number. Optional - needed only for list/create/delete buckets - see your developer console.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "user_project", label: "User Project", help: "User project. Optional - needed only for requester pays.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "service_account_file", label: "Service Account File", help: "Service Account Credentials JSON file path. Leave blank normally. Needed only if you want use SA instead of interactive login. Leading `~` will be expanded in the file name as will environment variables such as `${RCLONE_CONFIG_DIR}`.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "anonymous", label: "Anonymous", help: "Access public buckets and objects without credentials. Set to 'true' if you just want to download files and don't configure credentials.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "object_acl", label: "Object ACL", help: "Access Control List for new objects.", kind: Kind::Choice, default: "", examples: &["authenticatedRead", "bucketOwnerFullControl", "bucketOwnerRead", "private", "projectPrivate", "publicRead"], required: false, to_target: false },
        Field { name: "bucket_acl", label: "Bucket ACL", help: "Access Control List for new buckets.", kind: Kind::Choice, default: "", examples: &["authenticatedRead", "private", "projectPrivate", "publicRead", "publicReadWrite"], required: false, to_target: false },
        Field { name: "bucket_policy_only", label: "Bucket Policy Only", help: "Access checks should use bucket-level IAM policies. If you want to upload objects to a bucket with Bucket Policy Only set then you will need to set this. When it is set, rclone: - ignores ACLs set on buckets - ignores ACLs set on objects - creates buckets with Bucket Policy Only set Docs: https://cl…", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "location", label: "Location", help: "Location for the newly created buckets.", kind: Kind::Choice, default: "", examples: &["", "asia", "eu", "us", "asia-east1", "asia-east2", "asia-northeast1", "asia-northeast2", "asia-northeast3", "asia-south1", "asia-south2", "asia-southeast1", "asia-southeast2", "australia-southeast1", "australia-southeast2", "europe-north1", "europe-west1", "europe-west2", "europe-west3", "europe-west4", "europe-west6", "europe-central2", "us-central1", "us-east1", "us-east4", "us-west1", "us-west2", "us-west3", "us-west4", "northamerica-northeast1", "northamerica-northeast2", "southamerica-east1", "southamerica-west1", "asia1", "eur4", "nam4"], required: false, to_target: false },
        Field { name: "storage_class", label: "Storage Class", help: "The storage class to use when storing objects in Google Cloud Storage.", kind: Kind::Choice, default: "", examples: &["", "MULTI_REGIONAL", "REGIONAL", "NEARLINE", "COLDLINE", "ARCHIVE", "DURABLE_REDUCED_AVAILABILITY"], required: false, to_target: false },
        Field { name: "env_auth", label: "Env Auth", help: "Get GCP IAM credentials from runtime (environment variables or instance meta data if no env vars). Only applies if service_account_file and service_account_credentials is blank.", kind: Kind::Choice, default: "false", examples: &["false", "true"], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: true, to_target: true },
    ] },
    Backend { name: "drive", title: "Google Drive", description: "Google Drive", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "Google Application Client Id Setting your own is recommended. See https://rclone.org/drive/#making-your-own-client-id for how to create your own. If you leave this blank, it will use an internal key which is low performance.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "scope", label: "Scope", help: "Comma separated list of scopes that rclone should use when requesting access from drive.", kind: Kind::Choice, default: "", examples: &["drive", "drive.readonly", "drive.file", "drive.appfolder", "drive.metadata.readonly"], required: false, to_target: false },
        Field { name: "service_account_file", label: "Service Account File", help: "Service Account Credentials JSON file path. Leave blank normally. Needed only if you want use SA instead of interactive login. Leading `~` will be expanded in the file name as will environment variables such as `${RCLONE_CONFIG_DIR}`.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "gphotos", title: "Google Photos", description: "Google Photos", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "read_only", label: "Read Only", help: "Set to make the Google Photos backend read only. If you choose read only then rclone will only request read only access to your photos, otherwise rclone will request full access.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "hdfs", title: "HDFS", description: "Hadoop distributed file system", needs_path: true, fields: &[
        Field { name: "namenode", label: "Namenode", help: "Hadoop name nodes and ports. E.g. \"namenode-1:8020,namenode-2:8020,...\" to connect to host namenodes at port 8020.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "username", label: "Username", help: "Hadoop user name.", kind: Kind::Secret, default: "", examples: &["root"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "http", title: "HTTP", description: "HTTP", needs_path: false, fields: &[
        Field { name: "url", label: "URL", help: "URL of HTTP host to connect to. E.g. \"https://example.com\", or \"https://user:pass@example.com\" to use a username and password.", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "no_escape", label: "No Escape", help: "Do not escape URL metacharacters in path names.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
    ] },
    Backend { name: "hasher", title: "Hasher", description: "Better checksums for other remotes", needs_path: true, fields: &[
        Field { name: "remote", label: "Remote", help: "Remote to cache checksums for (e.g. myRemote:path).", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "hashes", label: "Hashes", help: "Comma separated list of supported checksum types.", kind: Kind::Text, default: "md5,sha1", examples: &[], required: false, to_target: false },
        Field { name: "max_age", label: "Max Age", help: "Maximum time to keep checksums in cache (0 = no cache, off = cache forever).", kind: Kind::Text, default: "off", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "hidrive", title: "HiDrive", description: "HiDrive", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "scope_access", label: "Scope Access", help: "Access permissions that rclone should use when requesting access from HiDrive.", kind: Kind::Choice, default: "rw", examples: &["rw", "ro"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "imagekit", title: "ImageKit", description: "ImageKit.io", needs_path: true, fields: &[
        Field { name: "endpoint", label: "Endpoint", help: "You can find your ImageKit.io URL endpoint in your [dashboard](https://imagekit.io/dashboard/developer/api-keys)", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "public_key", label: "Public Key", help: "You can find your ImageKit.io public key in your [dashboard](https://imagekit.io/dashboard/developer/api-keys)", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "private_key", label: "Private Key", help: "You can find your ImageKit.io private key in your [dashboard](https://imagekit.io/dashboard/developer/api-keys)", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "internetarchive", title: "Internet Archive", description: "Internet Archive", needs_path: false, fields: &[
        Field { name: "access_key_id", label: "Access Key ID", help: "IAS3 Access Key. Leave blank for anonymous access. You can find one here: https://archive.org/account/s3.php", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "secret_access_key", label: "Secret Access Key", help: "IAS3 Secret Key (password). Leave blank for anonymous access.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "jottacloud", title: "Jottacloud", description: "Jottacloud", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "koofr", title: "Koofr", description: "Koofr, Digi Storage and other Koofr-compatible storage providers", needs_path: true, fields: &[
        Field { name: "provider", label: "Provider", help: "Choose your storage provider.", kind: Kind::Choice, default: "", examples: &["koofr", "digistorage", "other"], required: false, to_target: false },
        Field { name: "endpoint", label: "Endpoint", help: "The Koofr API endpoint to use.", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "user", label: "User", help: "Your user name.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "password", label: "Password", help: "Your password for rclone generate one at https://app.koofr.net/app/admin/preferences/password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "linkbox", title: "Linkbox", description: "Linkbox", needs_path: true, fields: &[
        Field { name: "token", label: "Token", help: "Token from https://www.linkbox.to/admin/account", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "mailru", title: "Mail.ru Cloud", description: "Mail.ru Cloud", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "user", label: "User", help: "User name (usually email).", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "pass", label: "Pass", help: "Password. This must be an app password - rclone will not work with your normal password. See the Configuration section in the docs for how to make an app password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "speedup_enable", label: "Speedup Enable", help: "Skip full upload if there is another file with same data hash. This feature is called \"speedup\" or \"put by hash\". It is especially efficient in case of generally available files like popular books, video or audio clips, because files are searched by hash in all accounts of all mailru users. It is me…", kind: Kind::Choice, default: "true", examples: &["true", "false"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "mega", title: "Mega", description: "Mega", needs_path: true, fields: &[
        Field { name: "user", label: "User", help: "User name.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "pass", label: "Pass", help: "Password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "memory", title: "Memory", description: "In memory object storage system.", needs_path: false, fields: &[
    ] },
    Backend { name: "azureblob", title: "Microsoft Azure Blob Storage", description: "Azure containers", needs_path: false, fields: &[
        Field { name: "account", label: "Account", help: "Azure Storage Account Name. Set this to the Azure Storage Account Name in use. Leave blank to use SAS URL or Emulator, otherwise it needs to be set. If this is blank and if env_auth is set it will be read from the environment variable `AZURE_STORAGE_ACCOUNT_NAME` if possible.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "env_auth", label: "Env Auth", help: "Read credentials from runtime (environment variables, CLI or MSI). See the [authentication docs](/azureblob#authentication) for full info.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "key", label: "Key", help: "Storage Account Shared Key. Leave blank to use SAS URL or Emulator.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "sas_url", label: "Sas URL", help: "SAS URL for container level access only. Leave blank if using account/key or Emulator.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tenant", label: "Tenant", help: "ID of the service principal's tenant. Also called its directory ID. Set this if using - Service principal with client secret - Service principal with certificate - User with username and password", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_id", label: "Client ID", help: "The ID of the client in use. Set this if using - Service principal with client secret - Service principal with certificate - User with username and password", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "One of the service principal's client secrets Set this if using - Service principal with client secret", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_certificate_path", label: "Client Certificate Path", help: "Path to a PEM or PKCS12 certificate file including the private key. Set this if using - Service principal with certificate", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_certificate_password", label: "Client Certificate Password", help: "Password for the certificate file (optional). Optionally set this if using - Service principal with certificate And the certificate has a password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "container", label: "Container", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "azurefiles", title: "Microsoft Azure Files", description: "Microsoft Azure Files", needs_path: true, fields: &[
        Field { name: "account", label: "Account", help: "Azure Storage Account Name. Set this to the Azure Storage Account Name in use. Leave blank to use SAS URL or connection string, otherwise it needs to be set. If this is blank and if env_auth is set it will be read from the environment variable `AZURE_STORAGE_ACCOUNT_NAME` if possible.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "share_name", label: "Share Name", help: "Azure Files Share Name. This is required and is the name of the share to access.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "env_auth", label: "Env Auth", help: "Read credentials from runtime (environment variables, CLI or MSI). See the [authentication docs](/azurefiles#authentication) for full info.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "key", label: "Key", help: "Storage Account Shared Key. Leave blank to use SAS URL or connection string.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "sas_url", label: "Sas URL", help: "SAS URL. Leave blank if using account/key or connection string.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "connection_string", label: "Connection String", help: "Azure Files Connection String.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tenant", label: "Tenant", help: "ID of the service principal's tenant. Also called its directory ID. Set this if using - Service principal with client secret - Service principal with certificate - User with username and password", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_id", label: "Client ID", help: "The ID of the client in use. Set this if using - Service principal with client secret - Service principal with certificate - User with username and password", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "One of the service principal's client secrets Set this if using - Service principal with client secret", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_certificate_path", label: "Client Certificate Path", help: "Path to a PEM or PKCS12 certificate file including the private key. Set this if using - Service principal with certificate", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_certificate_password", label: "Client Certificate Password", help: "Password for the certificate file (optional). Optionally set this if using - Service principal with certificate And the certificate has a password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "onedrive", title: "Microsoft OneDrive", description: "Microsoft OneDrive", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "region", label: "Region", help: "Choose national cloud region for OneDrive.", kind: Kind::Choice, default: "global", examples: &["global", "us", "de", "cn"], required: false, to_target: false },
        Field { name: "tenant", label: "Tenant", help: "ID of the service principal's tenant. Also called its directory ID. Set this if using - Client Credential flow", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "opendrive", title: "OpenDrive", description: "OpenDrive", needs_path: true, fields: &[
        Field { name: "username", label: "Username", help: "Username.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "password", label: "Password", help: "Password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "swift", title: "OpenStack Swift", description: "OpenStack and compatible object stores", needs_path: false, fields: &[
        Field { name: "env_auth", label: "Env Auth", help: "Get swift credentials from environment variables in standard OpenStack form.", kind: Kind::Choice, default: "false", examples: &["false", "true"], required: false, to_target: false },
        Field { name: "user", label: "User", help: "User name to log in (OS_USERNAME).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "key", label: "Key", help: "API key or password (OS_PASSWORD).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "auth", label: "Auth", help: "Authentication URL for server (OS_AUTH_URL).", kind: Kind::Choice, default: "", examples: &["https://auth.api.rackspacecloud.com/v1.0", "https://lon.auth.api.rackspacecloud.com/v1.0", "https://identity.api.rackspacecloud.com/v2.0", "https://auth.storage.memset.com/v1.0", "https://auth.storage.memset.com/v2.0", "https://auth.cloud.ovh.net/v3", "https://authenticate.ain.net"], required: false, to_target: false },
        Field { name: "user_id", label: "User ID", help: "User ID to log in - optional - most swift systems use user and leave this blank (v3 auth) (OS_USER_ID).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "domain", label: "Domain", help: "User domain - optional (v3 auth) (OS_USER_DOMAIN_NAME)", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tenant", label: "Tenant", help: "Tenant name - optional for v1 auth, this or tenant_id required otherwise (OS_TENANT_NAME or OS_PROJECT_NAME).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tenant_id", label: "Tenant ID", help: "Tenant ID - optional for v1 auth, this or tenant required otherwise (OS_TENANT_ID).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "tenant_domain", label: "Tenant Domain", help: "Tenant domain - optional (v3 auth) (OS_PROJECT_DOMAIN_NAME).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "region", label: "Region", help: "Region name - optional (OS_REGION_NAME).", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "storage_url", label: "Storage URL", help: "Storage URL - optional (OS_STORAGE_URL).", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "auth_token", label: "Auth Token", help: "Auth Token from alternate authentication - optional (OS_AUTH_TOKEN).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "application_credential_id", label: "Application Credential ID", help: "Application Credential ID (OS_APPLICATION_CREDENTIAL_ID).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "application_credential_name", label: "Application Credential Name", help: "Application Credential Name (OS_APPLICATION_CREDENTIAL_NAME).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "application_credential_secret", label: "Application Credential Secret", help: "Application Credential Secret (OS_APPLICATION_CREDENTIAL_SECRET).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "auth_version", label: "Auth Version", help: "AuthVersion - optional - set to (1,2,3) if your auth URL has no version (ST_AUTH_VERSION).", kind: Kind::Text, default: "0", examples: &[], required: false, to_target: false },
        Field { name: "endpoint_type", label: "Endpoint Type", help: "Endpoint type to choose from the service catalogue (OS_ENDPOINT_TYPE).", kind: Kind::Choice, default: "public", examples: &["public", "internal", "admin"], required: false, to_target: false },
        Field { name: "storage_policy", label: "Storage Policy", help: "The storage policy to use when creating a new container. This applies the specified storage policy when creating a new container. The policy cannot be changed afterwards. The allowed configuration values and their meaning depend on your Swift storage provider.", kind: Kind::Choice, default: "", examples: &["", "pcs", "pca"], required: false, to_target: false },
        Field { name: "container", label: "Container", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "oos", title: "Oracle Object Storage", description: "Oracle Cloud Infrastructure Object Storage", needs_path: false, fields: &[
        Field { name: "provider", label: "Provider", help: "Choose your Auth Provider", kind: Kind::Choice, default: "env_auth", examples: &["env_auth", "user_principal_auth", "instance_principal_auth", "workload_identity_auth", "resource_principal_auth", "no_auth"], required: true, to_target: false },
        Field { name: "namespace", label: "Namespace", help: "Object storage namespace", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "compartment", label: "Compartment", help: "Specify compartment OCID, if you need to list buckets. List objects works without compartment OCID.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "region", label: "Region", help: "Object storage Region", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "endpoint", label: "Endpoint", help: "Endpoint for Object storage API. Leave blank to use the default endpoint for the region.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "config_file", label: "Config File", help: "Path to OCI config file", kind: Kind::Text, default: "~/.oci/config", examples: &["~/.oci/config"], required: false, to_target: false },
        Field { name: "config_profile", label: "Config Profile", help: "Profile name inside the oci config file", kind: Kind::Text, default: "Default", examples: &["Default"], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "pikpak", title: "PikPak", description: "PikPak", needs_path: true, fields: &[
        Field { name: "user", label: "User", help: "Pikpak username.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "pass", label: "Pass", help: "Pikpak password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "pixeldrain", title: "Pixeldrain", description: "Pixeldrain Filesystem", needs_path: true, fields: &[
        Field { name: "api_key", label: "API Key", help: "API key for your pixeldrain account. Found on https://pixeldrain.com/user/api_keys.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "root_folder_id", label: "Root Folder ID", help: "Root of the filesystem to use. Set to 'me' to use your personal filesystem. Set to a shared directory ID to use a shared directory.", kind: Kind::Text, default: "me", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "protondrive", title: "Proton Drive", description: "Proton Drive", needs_path: true, fields: &[
        Field { name: "username", label: "Username", help: "The username of your proton account", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "password", label: "Password", help: "The password of your proton account.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "2fa", label: "2fa", help: "The 2FA code The value can also be provided with --protondrive-2fa=000000 The 2FA code of your proton drive account if the account is set up with two-factor authentication", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "qingstor", title: "QingStor", description: "QingCloud Object Storage", needs_path: false, fields: &[
        Field { name: "env_auth", label: "Env Auth", help: "Get QingStor credentials from runtime. Only applies if access_key_id and secret_access_key is blank.", kind: Kind::Choice, default: "false", examples: &["false", "true"], required: false, to_target: false },
        Field { name: "access_key_id", label: "Access Key ID", help: "QingStor Access Key ID. Leave blank for anonymous access or runtime credentials.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "secret_access_key", label: "Secret Access Key", help: "QingStor Secret Access Key (password). Leave blank for anonymous access or runtime credentials.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "endpoint", label: "Endpoint", help: "Enter an endpoint URL to connection QingStor API. Leave blank will use the default value \"https://qingstor.com:443\".", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "zone", label: "Zone", help: "Zone to connect to. Default is \"pek3a\".", kind: Kind::Choice, default: "", examples: &["pek3a", "sh1a", "gd2a"], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "quatrix", title: "Quatrix", description: "Quatrix by Maytech", needs_path: true, fields: &[
        Field { name: "api_key", label: "API Key", help: "API key for accessing Quatrix account", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "host", label: "Host", help: "Host name of Quatrix account", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "sftp", title: "SFTP (SSH)", description: "Connect over SSH to another machine", needs_path: true, fields: &[
        Field { name: "host", label: "Host", help: "SSH host to connect to. E.g. \"example.com\".", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "user", label: "User", help: "SSH username.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "port", label: "Port", help: "SSH port number.", kind: Kind::Text, default: "22", examples: &[], required: false, to_target: false },
        Field { name: "pass", label: "Pass", help: "SSH password, leave blank to use ssh-agent.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "key_pem", label: "Key Pem", help: "Raw PEM-encoded private key. Note that this should be on a single line with line endings replaced with '\\n', eg key_pem = -----BEGIN RSA PRIVATE KEY-----\\nMaMbaIXtE\\n0gAMbMbaSsd\\nMbaass\\n-----END RSA PRIVATE KEY----- This will generate the single line correctly: awk '{printf \"%s\\\\n\", $0}' < ~/.ssh/i…", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "key_file", label: "Key File", help: "Path to PEM-encoded private key file. Leave blank or set key-use-agent to use ssh-agent. Leading `~` will be expanded in the file name as will environment variables such as `${RCLONE_CONFIG_DIR}`.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "key_file_pass", label: "Key File Pass", help: "The passphrase to decrypt the PEM-encoded private key file. Only PEM encrypted key files (old OpenSSH format) are supported. Encrypted keys in the new OpenSSH format can't be used.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "pubkey", label: "Pubkey", help: "SSH public certificate for public certificate based authentication. Set this if you have a signed certificate you want to use for authentication. If specified will override pubkey_file.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "pubkey_file", label: "Pubkey File", help: "Optional path to public key file. Set this if you have a signed certificate you want to use for authentication. Leading `~` will be expanded in the file name as will environment variables such as `${RCLONE_CONFIG_DIR}`.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "key_use_agent", label: "Key Use Agent", help: "When set forces the usage of the ssh-agent. When key-file is also set, the \".pub\" file of the specified key-file is read and only the associated key is requested from the ssh-agent. This allows to avoid `Too many authentication failures for *username*` errors when the ssh-agent contains many keys.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "use_insecure_cipher", label: "Use Insecure Cipher", help: "Enable the use of insecure ciphers and key exchange methods. This enables the use of the following insecure ciphers and key exchange methods: - aes128-cbc - aes192-cbc - aes256-cbc - 3des-cbc - diffie-hellman-group-exchange-sha256 - diffie-hellman-group-exchange-sha1 Those algorithms are insecure an…", kind: Kind::Choice, default: "false", examples: &["false", "true"], required: false, to_target: false },
        Field { name: "disable_hashcheck", label: "Disable Hashcheck", help: "Disable the execution of SSH commands to determine if remote file hashing is available. Leave blank or set to false to enable hashing (recommended), set to true to disable hashing.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "ssh", label: "SSH", help: "Path and arguments to external ssh binary. Normally rclone will use its internal ssh library to connect to the SFTP server. However it does not implement all possible ssh options so it may be desirable to use an external ssh binary. Rclone ignores all the internal config if you use this option and e…", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder on the server", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "smb", title: "SMB / CIFS", description: "Windows shares and Samba servers", needs_path: true, fields: &[
        Field { name: "host", label: "Host", help: "SMB server hostname to connect to. E.g. \"example.com\".", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "user", label: "User", help: "SMB username.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "port", label: "Port", help: "SMB port number.", kind: Kind::Text, default: "445", examples: &[], required: false, to_target: false },
        Field { name: "pass", label: "Pass", help: "SMB password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "domain", label: "Domain", help: "Domain name for NTLM authentication.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "spn", label: "Spn", help: "Service principal name. Rclone presents this name to the server. Some servers use this as further authentication, and it often needs to be set for clusters. For example: cifs/remotehost:1020 Leave blank if not sure.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Share and folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "seafile", title: "Seafile", description: "seafile", needs_path: true, fields: &[
        Field { name: "url", label: "URL", help: "URL of seafile host to connect to.", kind: Kind::Secret, default: "", examples: &["https://cloud.seafile.com/"], required: true, to_target: false },
        Field { name: "user", label: "User", help: "User name (usually email address).", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "pass", label: "Pass", help: "Password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "2fa", label: "2fa", help: "Two-factor authentication ('true' if the account has 2FA enabled).", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "library", label: "Library", help: "Name of the library. Leave blank to access all non-encrypted libraries.", kind: Kind::Text, default: "", examples: &[], required: false, to_target: false },
        Field { name: "library_key", label: "Library Key", help: "Library password (for encrypted libraries only). Leave blank if you pass it through the command line.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "sia", title: "Sia", description: "Sia Decentralized Cloud", needs_path: true, fields: &[
        Field { name: "api_url", label: "API URL", help: "Sia daemon API URL, like http://sia.daemon.host:9980. Note that siad must run with --disable-api-security to open API port for other hosts (not recommended). Keep default if Sia daemon runs on localhost.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "api_password", label: "API Password", help: "Sia Daemon API Password. Can be found in the apipassword file located in HOME/.sia/ or in the daemon directory.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "storj", title: "Storj", description: "Storj Decentralized Cloud Storage", needs_path: false, fields: &[
        Field { name: "provider", label: "Provider", help: "Choose an authentication method.", kind: Kind::Choice, default: "existing", examples: &["existing", "new"], required: false, to_target: false },
        Field { name: "access_grant", label: "Access Grant", help: "Access grant.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "satellite_address", label: "Satellite Address", help: "Satellite address. Custom satellite address should match the format: `<nodeid>@<address>:<port>`.", kind: Kind::Text, default: "us1.storj.io", examples: &["us1.storj.io", "eu1.storj.io", "ap1.storj.io"], required: false, to_target: false },
        Field { name: "api_key", label: "API Key", help: "API key.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "passphrase", label: "Passphrase", help: "Encryption passphrase. To access existing objects enter passphrase used for uploading.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "bucket", label: "Bucket", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "sugarsync", title: "SugarSync", description: "Sugarsync", needs_path: true, fields: &[
        Field { name: "app_id", label: "App ID", help: "Sugarsync App ID. Leave blank to use rclone's.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "access_key_id", label: "Access Key ID", help: "Sugarsync Access Key ID. Leave blank to use rclone's.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "private_access_key", label: "Private Access Key", help: "Sugarsync Private Access Key. Leave blank to use rclone's.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "hard_delete", label: "Hard Delete", help: "Permanently delete files if true otherwise put them in the deleted files.", kind: Kind::Text, default: "false", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "ulozto", title: "Uloz.to", description: "Uloz.to", needs_path: true, fields: &[
        Field { name: "app_token", label: "App Token", help: "The application token identifying the app. An app API key can be either found in the API doc https://uloz.to/upload-resumable-api-beta or obtained from customer service.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "username", label: "Username", help: "The username of the principal to operate as.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "password", label: "Password", help: "The password for the user.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "union", title: "Union", description: "Union merges the contents of several upstream fs", needs_path: true, fields: &[
        Field { name: "upstreams", label: "Upstreams", help: "List of space separated upstreams. Can be 'upstreama:test/dir upstreamb:', '\"upstreama:test/space:ro dir\" upstreamb:', etc.", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "action_policy", label: "Action Policy", help: "Policy to choose upstream on ACTION category.", kind: Kind::Text, default: "epall", examples: &[], required: false, to_target: false },
        Field { name: "create_policy", label: "Create Policy", help: "Policy to choose upstream on CREATE category.", kind: Kind::Text, default: "epmfs", examples: &[], required: false, to_target: false },
        Field { name: "search_policy", label: "Search Policy", help: "Policy to choose upstream on SEARCH category.", kind: Kind::Text, default: "ff", examples: &[], required: false, to_target: false },
        Field { name: "cache_time", label: "Cache Time", help: "Cache time of usage and free space (in seconds). This option is only useful when a path preserving policy is used.", kind: Kind::Text, default: "120", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "uptobox", title: "Uptobox", description: "Uptobox", needs_path: true, fields: &[
        Field { name: "access_token", label: "Access Token", help: "Your access token. Get it from https://uptobox.com/my_account.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "webdav", title: "WebDAV", description: "Nextcloud, ownCloud, Sharepoint and other WebDAV servers", needs_path: true, fields: &[
        Field { name: "url", label: "URL", help: "URL of http host to connect to. E.g. https://example.com.", kind: Kind::Text, default: "", examples: &[], required: true, to_target: false },
        Field { name: "vendor", label: "Vendor", help: "Name of the WebDAV site/service/software you are using.", kind: Kind::Choice, default: "", examples: &["fastmail", "nextcloud", "owncloud", "sharepoint", "sharepoint-ntlm", "rclone", "other"], required: false, to_target: false },
        Field { name: "user", label: "User", help: "User name. In case NTLM authentication is used, the username should be in the format 'Domain\\User'.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "pass", label: "Pass", help: "Password.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "bearer_token", label: "Bearer Token", help: "Bearer token instead of user/pass (e.g. a Macaroon).", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder on the server", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "yandex", title: "Yandex Disk", description: "Yandex Disk", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "zoho", title: "Zoho WorkDrive", description: "Zoho", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "region", label: "Region", help: "Zoho region to connect to. You'll have to use the region your organization is registered in. If not sure use the same top level domain as you connect to in your browser.", kind: Kind::Choice, default: "", examples: &["com", "eu", "in", "jp", "com.cn", "com.au"], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "iclouddrive", title: "iCloud Drive", description: "iCloud Drive", needs_path: true, fields: &[
        Field { name: "apple_id", label: "Apple ID", help: "Apple ID.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "password", label: "Password", help: "Password.", kind: Kind::Secret, default: "", examples: &[], required: true, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "pcloud", title: "pCloud", description: "pCloud account: sign in via rclone's browser flow after saving", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "premiumizeme", title: "premiumize.me", description: "premiumize.me", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
    Backend { name: "putio", title: "put.io", description: "Put.io", needs_path: true, fields: &[
        Field { name: "client_id", label: "Client ID", help: "OAuth Client Id. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "client_secret", label: "Client Secret", help: "OAuth Client Secret. Leave blank normally.", kind: Kind::Secret, default: "", examples: &[], required: false, to_target: false },
        Field { name: "path", label: "Folder", help: "where mbr stores blobs (created on push if missing)", kind: Kind::Text, default: "mbr", examples: &[], required: false, to_target: true },
    ] },
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

/// Build the mbr target for the rclone remote `remote_name`. `spec` is the
/// user's bucket or path answer; empty answers fall back to [`DEFAULT_PATH`].
pub fn target_for(remote_name: &str, backend: &Backend, spec: &str) -> String {
    let spec = spec.trim().trim_matches('/');
    let spec = if spec.is_empty() && backend.needs_path {
        DEFAULT_PATH
    } else {
        spec
    };
    format!("{remote_name}:{spec}")
}

/// The bucket/path answer among form `values` (in field order): the first
/// field flagged [`Field::to_target`], or empty if there is none.
pub fn target_spec<S: AsRef<str>>(backend: &Backend, values: &[S]) -> String {
    backend
        .fields
        .iter()
        .zip(values)
        .find(|(field, _)| field.to_target)
        .map(|(_, value)| value.as_ref().trim().to_owned())
        .unwrap_or_default()
}

/// Build the rclone `parameters` object from form `values` (in field
/// order): non-empty answers keyed by config name, in the clear (rclone
/// obscures secrets). [`Field::to_target`] answers are not rclone config
/// and are left out.
pub fn form_parameters<S: AsRef<str>>(backend: &Backend, values: &[S]) -> serde_json::Value {
    let mut parameters = serde_json::Map::new();
    for (field, value) in backend.fields.iter().zip(values) {
        let value = value.as_ref().trim();
        if !field.to_target && !value.is_empty() {
            parameters.insert(field.name.to_owned(), serde_json::Value::from(value));
        }
    }
    serde_json::Value::Object(parameters)
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
        assert_eq!(target_for("nas", smb, "share/sub"), "nas:share/sub");
        assert_eq!(target_for("nas", smb, ""), "nas:mbr");
        let s3 = backend("s3").unwrap();
        assert_eq!(target_for("backup", s3, "mybucket"), "backup:mybucket");
        assert_eq!(target_for("backup", s3, "mybucket/"), "backup:mybucket");
    }

    #[test]
    fn form_answers() {
        let s3 = backend("s3").unwrap();
        let mut values = vec![String::new(); s3.fields.len()];
        let at = |name| s3.fields.iter().position(|f| f.name == name).unwrap();
        values[at("provider")] = "AWS".into();
        values[at("bucket")] = " photos/ ".into();
        assert_eq!(target_spec(s3, &values), "photos/");
        let parameters = form_parameters(s3, &values);
        assert_eq!(parameters, serde_json::json!({"provider": "AWS"}));
    }
}
