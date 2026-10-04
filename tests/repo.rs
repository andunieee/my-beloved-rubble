//! Scanning and spec resolution against real temporary folders. No rclone
//! calls are made, so these run without network or remote configuration.

use mbr::Repo;
use std::fs;
use std::path::{Path, PathBuf};

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

fn active(repo: &mut Repo, path: &str) -> mbr::repo::FileStatus {
    repo.status()
        .unwrap()
        .into_iter()
        .find(|s| s.path == path)
        .unwrap_or_else(|| panic!("{path} is not tracked"))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).unwrap().file_type().is_symlink()
}

#[test]
fn scan_ingests_dedups_replaces_and_removes() {
    let tmp = TempDir::new("scan");
    let root = &tmp.0;
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), "apple").unwrap();
    fs::write(root.join("sub/b.txt"), "apple").unwrap();
    fs::write(root.join(".hidden"), "ignored").unwrap();

    let mut repo = Repo::init(root).unwrap();
    let report = repo.scan().unwrap();
    assert_eq!(report.added, ["a.txt", "sub/b.txt"]);
    assert!(is_symlink(&root.join("a.txt")));
    assert!(is_symlink(&root.join("sub/b.txt")));
    assert!(!is_symlink(&root.join(".hidden")));
    // Symlinks resolve to the original content.
    assert_eq!(fs::read_to_string(root.join("sub/b.txt")).unwrap(), "apple");

    let a = active(&mut repo, "a.txt");
    assert!(a.present_locally);
    assert_eq!(a.size, 5);
    assert_eq!(a.other_names, ["sub/b.txt"]);

    // Nothing changed: rescanning is a no-op.
    assert!(repo.scan().unwrap().is_empty());

    // Replace a.txt's content: old row closes, a previous version appears.
    let old_hash = a.hash;
    fs::remove_file(root.join("a.txt")).unwrap();
    fs::write(root.join("a.txt"), "banana").unwrap();
    let report = repo.scan().unwrap();
    assert_eq!(report.replaced, ["a.txt"]);
    let a = active(&mut repo, "a.txt");
    assert_ne!(a.hash, old_hash);
    assert_eq!(a.previous_versions.len(), 1);
    assert_eq!(a.previous_versions[0].hash, old_hash);
    assert_eq!(a.previous_versions[0].stored_summary(), "local only");

    // Remove sub/b.txt; the row closes but the blob stays in the store.
    fs::remove_file(root.join("sub/b.txt")).unwrap();
    let report = repo.scan().unwrap();
    assert_eq!(report.removed, ["sub/b.txt"]);
    assert!(repo.blob_present(&old_hash));
}

#[test]
fn resolve_spec_accepts_paths_and_hash_prefixes() {
    let tmp = TempDir::new("resolve");
    fs::write(tmp.0.join("x"), "x").unwrap();
    let mut repo = Repo::init(&tmp.0).unwrap();
    repo.scan().unwrap();
    let hash = active(&mut repo, "x").hash;

    assert_eq!(repo.resolve_spec("x").unwrap(), hash);
    assert_eq!(repo.resolve_spec(&hash).unwrap(), hash);
    assert_eq!(repo.resolve_spec(&hash[..8]).unwrap(), hash);
    assert_eq!(repo.resolve_spec(&hash.to_uppercase()).unwrap(), hash);
    assert_eq!(repo.resolve_spec(&hash[..8].to_uppercase()).unwrap(), hash);
    assert!(repo.resolve_spec(&hash[..5]).is_err());
    assert!(repo.resolve_spec("nope").is_err());
}

#[test]
fn discover_walks_up_from_subdirectories() {
    let tmp = TempDir::new("discover");
    fs::create_dir_all(tmp.0.join("deep/er")).unwrap();
    Repo::init(&tmp.0).unwrap();
    let repo = Repo::discover(&tmp.0.join("deep/er")).unwrap();
    assert_eq!(repo.root, tmp.0.canonicalize().unwrap());
}
