//! Remotes set up the mbr way: a backend wrapped in an rclone crypt layer
//! keyed with the master password, storing under the folder's own
//! `mbr-<id>` directory, with metadata backups next to it. Backends are
//! plain local directories driven through the embedded rclone.

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
    let folder_id = repo.folder_id().unwrap();
    let spec = store.display().to_string();
    repo.with_rclone(|| {
        layout.check_unused()?;
        let begun = rclone::begin_setup(&layout.root_section(), "local", json!({}));
        match layout.finish(begun, &spec, &folder_id, PASSWORD)? {
            SetupOutcome::Done => Ok(()),
            SetupOutcome::Question(q) => Err(format!("unexpected question {}", q.name)),
        }
    })
    .unwrap();
    repo.db.add_remote(name, &layout.target()).unwrap();
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
    assert_eq!(remote.target, "_crypt-mbr-disk:");
    assert_eq!(repo.encrypted_remotes().unwrap(), ["disk"]);
    let config = fs::read_to_string(repo.rclone_config_path()).unwrap();
    for section in ["[_root-mbr-disk]", "[_plain-mbr-disk]", "[_crypt-mbr-disk]"] {
        assert!(config.contains(section), "{config}");
    }
    assert!(!config.contains(PASSWORD), "the password is stored obscured");

    repo.push_blob(&remote, &hash).unwrap();
    let dir = store.join(remotes::folder_dir(&repo.folder_id().unwrap()));
    let stored = files_under(&dir);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert!(!stored[0].contains(&hash[..8]), "names are encrypted: {stored:?}");
    let raw = fs::read(dir.join(&stored[0])).unwrap();
    assert!(!raw.windows(4).any(|w| w == b"meow"), "content is encrypted");

    // It reads back through the crypt layer.
    assert!(repo.verify_on_remote(&remote, &hash).is_ok());
    assert_eq!(repo.scan_remote(&remote).unwrap().present, [hash]);
    // Metadata backups go next to the folder's directory.
    assert_eq!(
        repo.with_rclone(|| remotes::backup_location(&remote.target)).unwrap(),
        format!("_root-mbr-disk:{}", store.display())
    );

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
    assert_eq!(remote.target, "_dangerouslyplain-mbr-usb:");
    assert!(repo.encrypted_remotes().unwrap().is_empty());
    repo.push_blob(&remote, &hash).unwrap();
    let dir = store.join(remotes::folder_dir(&repo.folder_id().unwrap()));
    assert_eq!(
        files_under(&dir),
        [rclone::blob_path(&hash)],
        "stored by hash, readable by anyone"
    );
    repo.remove_remote("usb").unwrap();
    let config = fs::read_to_string(repo.rclone_config_path()).unwrap();
    assert!(!config.contains("mbr-usb"), "{config}");
}

#[test]
fn a_lost_folder_is_found_again_from_the_remote_and_the_password() {
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
    let folder_id = repo.folder_id().unwrap();
    let sealed = metadata::seal(&repo.metadata_snapshot().unwrap(), PASSWORD).unwrap();
    let name = metadata::backup_name(PASSWORD).unwrap();
    let scratch = repo.scratch_dir();
    repo.with_rclone(|| {
        let location = remotes::backup_location(&remote.target)?;
        metadata::upload(&location, &name, &sealed, &scratch)
    })
    .unwrap();
    assert!(store.join(&name).is_file());
    drop(repo);
    fs::remove_dir_all(&folder).unwrap();

    // A new folder: the remote's settings go in a throwaway rclone config,
    // and the backup is found by the password's backup name alone.
    let fresh = tmp.sub("fresh");
    let scratch = fresh.join(mbr::repo::TMP_DIR);
    let config = scratch.join("restore-rclone.conf");
    mbr::repo::ensure_rclone_config(&config).unwrap();
    let section = format!("{}restore", remotes::ROOT_PREFIX);
    let sealed = rclone::with_config(&config, || {
        match rclone::begin_setup(&section, "local", json!({}))? {
            SetupOutcome::Done => {}
            SetupOutcome::Question(q) => return Err(format!("unexpected question {}", q.name)),
        }
        let location = remotes::root_location(&section, &store.display().to_string());
        assert!(metadata::download(&location, "mbr-metadata-0.tar.gz.encrypted", &scratch).is_err());
        metadata::download(&location, &metadata::backup_name(PASSWORD)?, &scratch)
    })
    .unwrap();
    let (db, conf) = metadata::unseal(&sealed, PASSWORD).unwrap();
    let (mut repo, report) = metadata::restore(&fresh, &db, &conf).unwrap();
    assert_eq!(report.files, 1, "{:?}", report.skipped);

    // It is the lost folder again, remotes and all.
    assert_eq!(repo.folder_id().unwrap(), folder_id);
    let disk = repo.db.remote("disk").unwrap().unwrap();
    for hash in repo.blobs_missing_locally("disk").unwrap() {
        repo.fetch_blob_from(&disk, &hash).unwrap();
    }
    assert_eq!(fs::read_to_string(fresh.join("cat.txt")).unwrap(), "meow");
}
