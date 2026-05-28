//! Remotes set up the mbr way: a backend wrapped in an rclone crypt layer
//! keyed with the master password, storing under the folder's own
//! `mbr-<id>` directory. Backends are plain local directories driven
//! through the embedded rclone.

#![cfg(unix)]

use mbr::Repo;
use mbr::db::Remote;
use mbr::metadata;
use mbr::rclone::{self, SetupOutcome};
use mbr::remotes::{self, Layout};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const PASSWORD: &str = "correct horse";

/// A fresh empty directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mbr-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn sub(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Set up remote `name` on the local folder `store`, as the frontends do.
fn set_up(repo: &mut Repo, name: &str, store: &Path, encrypt: bool) -> Remote {
    let layout = Layout::new(name, encrypt).unwrap();
    let location = remotes::location(&store.display().to_string(), &repo.folder_id().unwrap());
    repo.with_rclone(|| {
        layout.check_unused()?;
        let begun = rclone::begin_setup(&layout.backend_section(), "local", json!({}));
        match layout.finish(begun, &location, PASSWORD)? {
            SetupOutcome::Done => Ok(()),
            SetupOutcome::Question(q) => Err(format!("unexpected question {}", q.name)),
        }
    })
    .unwrap();
    repo.db.add_remote(name, &layout.target(&location)).unwrap();
    repo.db.remote(name).unwrap().unwrap()
}

/// Every file under `dir`, relative to it.
fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    found
}

#[test]
fn encrypted_remotes_hide_content_and_names() {
    let tmp = TempDir::new("autocrypt");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("cat.txt"), "meow").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let hash = repo.db.active_path("cat.txt").unwrap().unwrap().hash;

    let remote = set_up(&mut repo, "disk", &store, true);
    assert_eq!(remote.target, "_autocrypt-mbr-disk:");
    assert_eq!(repo.encrypted_remotes().unwrap(), ["disk"]);
    let config = fs::read_to_string(repo.rclone_config_path()).unwrap();
    assert!(config.contains("[_plain-mbr-disk]"), "{config}");
    assert!(config.contains("[_autocrypt-mbr-disk]"), "{config}");
    assert!(!config.contains(PASSWORD), "the password is stored obscured");

    repo.push_blob(&remote, &hash).unwrap();
    let id = repo.folder_id().unwrap();
    let stored = files_under(&store.join(remotes::folder_dir(&id)));
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert!(!stored[0].contains(&hash[..8]), "names are encrypted: {stored:?}");
    let raw = fs::read(store.join(remotes::folder_dir(&id)).join(&stored[0])).unwrap();
    assert!(!raw.windows(4).any(|w| w == b"meow"), "content is encrypted");

    // It reads back through the crypt layer.
    assert!(repo.verify_on_remote(&remote, &hash).is_ok());
    assert_eq!(repo.scan_remote(&remote).unwrap().present, [hash]);

    // Removing the remote deletes its rclone settings, not what it stores.
    repo.remove_remote("disk").unwrap();
    let config = fs::read_to_string(repo.rclone_config_path()).unwrap();
    assert!(!config.contains("mbr-disk"), "{config}");
    assert_eq!(files_under(&store).len(), 1);
}

#[test]
fn unencrypted_remotes_store_in_the_clear() {
    let tmp = TempDir::new("dangerouslyplain");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("cat.txt"), "meow").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let hash = repo.db.active_path("cat.txt").unwrap().unwrap().hash;

    let remote = set_up(&mut repo, "usb", &store, false);
    let dir = remotes::folder_dir(&repo.folder_id().unwrap());
    assert_eq!(
        remote.target,
        format!("_dangerouslyplain-mbr-usb:{}/{dir}", store.display())
    );
    assert!(repo.encrypted_remotes().unwrap().is_empty());
    repo.push_blob(&remote, &hash).unwrap();
    assert_eq!(
        files_under(&store.join(&dir)),
        [rclone::blob_path(&hash)],
        "stored by hash, readable by anyone"
    );
}

#[test]
fn a_lost_folder_is_found_again_through_its_encrypted_remote() {
    let tmp = TempDir::new("autocrypt-restore");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("cat.txt"), "meow").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let remote = set_up(&mut repo, "disk", &store, true);
    for hash in repo.blobs_missing_on_remote("disk").unwrap() {
        repo.push_blob(&remote, &hash).unwrap();
    }
    repo.db.set_metadata_remote("disk", true).unwrap();
    let old_id = repo.folder_id().unwrap();
    let sealed = metadata::seal(&repo.metadata_snapshot().unwrap(), PASSWORD).unwrap();
    let scratch = repo.scratch_dir();
    repo.with_rclone(|| metadata::upload(&remote.target, &sealed, &scratch))
        .unwrap();
    drop(repo);
    fs::remove_dir_all(&folder).unwrap();

    // A new folder, with the same remote set up again: it gets its own id,
    // so its remote points next to the lost folder's directory.
    let fresh = tmp.sub("fresh");
    let mut repo = Repo::init(&fresh).unwrap();
    let remote = set_up(&mut repo, "disk", &store, true);
    assert_ne!(repo.folder_id().unwrap(), old_id);

    let scratch = repo.scratch_dir();
    let sealed = repo
        .with_rclone(|| metadata::download(&remote.target, &scratch))
        .unwrap();
    let (db, conf) = metadata::unseal(&sealed, PASSWORD).unwrap();
    let report = repo.restore_metadata(&db, &conf).unwrap();
    assert_eq!(report.files, 1, "{:?}", report.skipped);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);

    // The new folder took on the old id, and its remote moved with it.
    assert_eq!(repo.folder_id().unwrap(), old_id);
    let disk = repo.db.remote("disk").unwrap().unwrap();
    for hash in repo.blobs_missing_locally("disk").unwrap() {
        repo.fetch_blob_from(&disk, &hash).unwrap();
    }
    assert_eq!(fs::read_to_string(fresh.join("cat.txt")).unwrap(), "meow");
}
