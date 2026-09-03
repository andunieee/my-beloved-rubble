//! Thin wrapper around the `rclone` CLI.
//!
//! Blobs live on a remote under the same sharded layout as the local
//! `.blobs` store: `<target>/aa/bb/<full-hash>`, with no metadata at all.
//! Encryption is rclone's concern (use an rclone `crypt` remote if wanted).

use crate::util::is_hash;
use std::path::Path;
use std::process::Command;

/// Remote path of a blob under `target`.
pub fn blob_url(target: &str, hash: &str) -> String {
    format!("{}/{}/{}/{hash}", target.trim_end_matches('/'), &hash[..2], &hash[2..4])
}

fn run(mut cmd: Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run rclone (is it installed?): {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("rclone failed: {}", stderr.trim()))
    }
}

/// Upload one blob file to `target`.
pub fn push_blob(target: &str, blob_file: &Path, hash: &str) -> Result<(), String> {
    let mut cmd = Command::new("rclone");
    cmd.arg("copyto").arg(blob_file).arg(blob_url(target, hash));
    run(cmd).map(|_| ())
}

/// Download one blob from `target` into `dest`.
pub fn fetch_blob(target: &str, hash: &str, dest: &Path) -> Result<(), String> {
    let mut cmd = Command::new("rclone");
    cmd.arg("copyto").arg(blob_url(target, hash)).arg(dest);
    run(cmd).map(|_| ())
}

/// List all blob hashes stored under `target`.
///
/// Errors when the remote is unreachable, which doubles as the
/// accessibility check. An empty or missing directory is fine (no blobs).
pub fn list_blobs(target: &str) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("rclone");
    cmd.args(["lsf", "--recursive", "--files-only", target]);
    let listing = match run(cmd) {
        Ok(listing) => listing,
        // A prefix that was never pushed to may not exist yet; rclone
        // reports that as "directory not found" rather than a real failure.
        Err(e) if e.contains("directory not found") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(listing
        .lines()
        .filter_map(|line| line.rsplit('/').next())
        .filter(|name| is_hash(name))
        .map(str::to_owned)
        .collect())
}
