//! Remote scanning, per-blob checks, downloads and remote editing against
//! the embedded rclone, using plain local directories as remotes.

#![cfg(unix)]

use mbr::Repo;
use mbr::db::Remote;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

/// A fresh empty directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mbr-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Put `content` on the remote directory `dir` the way mbr lays blobs out.
fn plant_blob(dir: &std::path::Path, content: &str) -> String {
    let tmp = dir.join("plant.tmp");
    fs::write(&tmp, content).unwrap();
    let hash = mbr::util::sha256_file(&tmp).unwrap();
    let dest = dir.join(mbr::rclone::blob_path(&hash));
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::rename(&tmp, &dest).unwrap();
    hash
}

#[test]
fn scanning_a_remote_adopts_unknown_blobs_and_pull_downloads_them() {
    let tmp = TempDir::new("remote-scan");
    let folder = tmp.0.join("folder");
    let store = tmp.0.join("store");
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(folder.join("a.txt"), "apple").unwrap();

    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    repo.db.add_remote("disk", &store.display().to_string()).unwrap();
    let remote = repo.db.remote("disk").unwrap().unwrap();

    // Nothing there yet; a scan of an empty remote is fine.
    let check = repo.scan_remote(&remote).unwrap();
    assert!(check.present.is_empty() && check.adopted.is_empty());

    let a = repo.resolve_spec("a.txt").unwrap();
    repo.push_blob(&remote, &a).unwrap();
    let stranger = plant_blob(&store, "from elsewhere");

    let check = repo.scan_remote(&remote).unwrap();
    assert_eq!(check.present, [a.clone()]);
    assert_eq!(check.adopted, [stranger.clone()]);

    // The stranger got a name, a size, and a remote record.
    let unnamed = format!("unnamed/{stranger}");
    let status = repo.status().unwrap();
    let s = status.iter().find(|s| s.path == unnamed).expect("adopted name");
    assert_eq!(s.size, "from elsewhere".len() as u64);
    assert_eq!(s.remotes, ["disk"]);
    assert!(!s.present_locally);
    // It is a real (dangling) blob symlink, so a local scan keeps it.
    assert!(repo.scan().unwrap().is_empty());

    // Scanning again adopts nothing new.
    let check = repo.scan_remote(&remote).unwrap();
    assert!(check.adopted.is_empty() && check.discovered.is_empty());

    // Download everything recorded on the remote that is missing here.
    assert_eq!(repo.blobs_missing_locally("disk").unwrap(), [stranger.clone()]);
    repo.fetch_blob_from(&remote, &stranger).unwrap();
    assert_eq!(
        fs::read_to_string(folder.join(&unnamed)).unwrap(),
        "from elsewhere"
    );
    assert!(repo.blobs_missing_locally("disk").unwrap().is_empty());

    // A blob deleted from the remote loses its record on the next scan.
    fs::remove_file(store.join(mbr::rclone::blob_path(&a))).unwrap();
    let check = repo.scan_remote(&remote).unwrap();
    assert_eq!(check.missing, [a.clone()]);
    assert_eq!(repo.db.remotes_for_blob(&a).unwrap(), Vec::<String>::new());
}

#[test]
fn checking_specific_blobs_updates_their_records() {
    let tmp = TempDir::new("remote-check");
    let folder = tmp.0.join("folder");
    let store = tmp.0.join("store");
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(folder.join("a"), "a").unwrap();
    fs::write(folder.join("b"), "b").unwrap();

    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let a = repo.resolve_spec("a").unwrap();
    let b = repo.resolve_spec("b").unwrap();
    let remote = Remote {
        name: "disk".into(),
        target: store.display().to_string(),
    };
    repo.db.add_remote(&remote.name, &remote.target).unwrap();

    // Present on the remote, but not recorded: the check records it.
    fs::create_dir_all(store.join(mbr::rclone::blob_path(&a)).parent().unwrap()).unwrap();
    fs::copy(repo.blob_path(&a), store.join(mbr::rclone::blob_path(&a))).unwrap();
    // Recorded, but not actually there: the check drops the record.
    repo.db.set_blob_on_remote(&b, "disk").unwrap();

    let result = repo.check_blobs(&remote, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(result, [(a.clone(), true), (b.clone(), false)]);
    assert_eq!(repo.db.blobs_on_remote("disk").unwrap(), [a]);
}

#[test]
fn editing_a_configured_remote_keeps_other_settings() {
    let config = std::env::temp_dir().join(format!("mbr-test-edit-{}.conf", std::process::id()));
    fs::write(&config, "").unwrap();

    mbr::rclone::with_config(&config, || {
        assert!(mbr::rclone::remote_config("dav")?.is_none());
        assert!(mbr::rclone::update_remote("dav", json!({"vendor": "owncloud"})).is_err());

        mbr::rclone::begin_setup(
            "dav",
            "webdav",
            json!({"url": "https://example.com/dav", "vendor": "nextcloud"}),
        )?;
        mbr::rclone::update_remote("dav", json!({"vendor": "owncloud", "pass": "hunter2"}))?;
        let stored = mbr::rclone::remote_config("dav")?.unwrap();
        assert_eq!(stored["type"], "webdav");
        assert_eq!(stored["vendor"], "owncloud");
        assert_eq!(stored["url"], "https://example.com/dav");
        // Secrets are stored obscured, never in the clear.
        assert_ne!(stored["pass"], "hunter2");
        assert!(!stored["pass"].as_str().unwrap().is_empty());
        Ok(())
    })
    .unwrap();

    fs::remove_file(&config).ok();
}
