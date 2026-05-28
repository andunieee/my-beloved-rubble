//! Keeping data safe: read-only blobs, verification, dropping local copies
//! under the minimum-copies rule, garbage collection, and encrypted
//! metadata backups restored into a fresh folder or merged into one in use. Remotes are plain local
//! directories driven through the embedded rclone.

#![cfg(unix)]

use mbr::Repo;
use mbr::db::Remote;
use mbr::metadata;
use mbr::repo::{LocalHealth, RemoteHealth};
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

fn add_disk(repo: &mut Repo, name: &str, dir: &Path) -> Remote {
    repo.db.add_remote(name, &dir.display().to_string()).unwrap();
    repo.db.remote(name).unwrap().unwrap()
}

#[test]
fn blobs_are_read_only_and_verification_catches_corruption() {
    let tmp = TempDir::new("verify");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("a.txt"), "apple").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let hash = repo.resolve_spec("a.txt").unwrap();
    let blob = repo.blob_path(&hash);

    // Writing through the symlink fails instead of changing the blob.
    assert!(fs::metadata(&blob).unwrap().permissions().readonly());
    assert!(fs::write(folder.join("a.txt"), "apricot").is_err());
    assert_eq!(repo.verify_local(&hash).unwrap(), LocalHealth::Ok);

    let remote = add_disk(&mut repo, "disk", &store);
    repo.push_blob(&remote, &hash).unwrap();
    assert_eq!(repo.verify_on_remote(&remote, &hash).unwrap(), RemoteHealth::Ok);

    // Rot both copies behind mbr's back.
    let remote_copy = store.join(mbr::rclone::blob_path(&hash));
    fs::write(&remote_copy, "applf").unwrap();
    assert_eq!(repo.verify_on_remote(&remote, &hash).unwrap(), RemoteHealth::Corrupt);
    assert!(!remote_copy.exists(), "corrupt remote copy is deleted");
    assert!(repo.db.remotes_for_blob(&hash).unwrap().is_empty());

    let mut permissions = fs::metadata(&blob).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&blob, permissions).unwrap();
    fs::write(&blob, "applf").unwrap();
    match repo.verify_local(&hash).unwrap() {
        LocalHealth::Corrupt { moved_to } => assert!(moved_to.is_file()),
        other => panic!("expected corruption, got {other:?}"),
    }
    assert!(!repo.blob_present(&hash));
    assert_eq!(repo.verify_local(&hash).unwrap(), LocalHealth::Missing);
}

#[test]
fn dropping_needs_enough_verified_remote_copies() {
    let tmp = TempDir::new("drop");
    let folder = tmp.sub("folder");
    let one = tmp.sub("one");
    let two = tmp.sub("two");
    fs::write(folder.join("a.txt"), "apple").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let hash = repo.resolve_spec("a.txt").unwrap();
    assert_eq!(repo.min_copies().unwrap(), mbr::repo::DEFAULT_MIN_COPIES);

    let one = add_disk(&mut repo, "one", &one);
    let two = add_disk(&mut repo, "two", &two);
    repo.push_blob(&one, &hash).unwrap();

    // One copy elsewhere is not enough for the default of two.
    let outcome = repo.drop_blob(&hash).unwrap();
    assert!(!outcome.dropped);
    assert_eq!(outcome.verified, ["one"]);
    assert!(repo.blob_present(&hash));

    // A record the remote can't back up doesn't count, and is corrected.
    repo.db.set_blob_on_remote(&hash, "two").unwrap();
    let outcome = repo.drop_blob(&hash).unwrap();
    assert!(!outcome.dropped);
    assert_eq!(outcome.absent, ["two"]);
    assert_eq!(repo.db.remotes_for_blob(&hash).unwrap(), ["one"]);

    repo.push_blob(&two, &hash).unwrap();
    let outcome = repo.drop_blob(&hash).unwrap();
    assert!(outcome.dropped, "{}", outcome.kept_reason(2));
    assert!(!repo.blob_present(&hash));
    assert!(repo.drop_blob(&hash).is_err(), "nothing left to drop");

    // Fetching brings it back, read-only again.
    repo.fetch_blob(&hash).unwrap();
    assert!(fs::metadata(repo.blob_path(&hash)).unwrap().permissions().readonly());
    assert_eq!(fs::read_to_string(folder.join("a.txt")).unwrap(), "apple");

    repo.set_min_copies(1).unwrap();
    assert_eq!(repo.min_copies().unwrap(), 1);
    assert!(repo.set_min_copies(0).is_err());
}

#[test]
fn gc_finds_old_versions_and_purge_removes_them_everywhere() {
    let tmp = TempDir::new("gc");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("a.txt"), "v1").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let old = repo.resolve_spec("a.txt").unwrap();
    let remote = add_disk(&mut repo, "disk", &store);
    repo.push_blob(&remote, &old).unwrap();

    fs::remove_file(folder.join("a.txt")).unwrap();
    fs::write(folder.join("a.txt"), "v2").unwrap();
    repo.scan().unwrap();
    let new = repo.resolve_spec("a.txt").unwrap();

    let unreferenced = repo.unreferenced_blobs().unwrap();
    assert_eq!(unreferenced.len(), 1);
    assert_eq!(unreferenced[0].hash, old);
    assert!(unreferenced[0].removed_at > 0);
    assert_eq!(unreferenced[0].remotes, ["disk"]);

    assert!(repo.purge_blob(&new).is_err(), "in-use blobs are refused");
    repo.purge_blob(&old).unwrap();
    assert!(!repo.blob_present(&old));
    assert!(!store.join(mbr::rclone::blob_path(&old)).exists());
    assert!(repo.unreferenced_blobs().unwrap().is_empty());
    // The history row stays, now stored nowhere.
    let status = repo.status().unwrap();
    assert_eq!(status[0].previous_versions[0].stored_summary(), "NOT STORED ANYWHERE");
}

#[test]
fn metadata_backup_restores_a_lost_folder() {
    let tmp = TempDir::new("metadata");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::create_dir_all(folder.join("photos")).unwrap();
    fs::write(folder.join("photos/cat.jpg"), "meow").unwrap();
    fs::write(folder.join("notes.txt"), "hello").unwrap();

    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let remote = add_disk(&mut repo, "disk", &store);
    repo.db.set_metadata_remote("disk", true).unwrap();
    for hash in repo.blobs_missing_on_remote("disk").unwrap() {
        repo.push_blob(&remote, &hash).unwrap();
    }

    let snapshot = repo.metadata_snapshot().unwrap();
    assert_eq!(snapshot.db_hash, repo.db_hash().unwrap());
    let sealed = metadata::seal(&snapshot, "correct horse").unwrap();
    let scratch = repo.scratch_dir();
    repo.with_rclone(|| metadata::upload(&remote.target, &sealed, &scratch))
        .unwrap();
    let on_remote = store.join(metadata::REMOTE_DIR).join(metadata::DB_OBJECT);
    assert!(on_remote.is_file());
    assert!(!fs::read(&on_remote).unwrap().windows(4).any(|w| w == b"meow"));
    // Backups are not blobs: scanning the remote ignores them.
    let check = repo.scan_remote(&remote).unwrap();
    assert!(check.adopted.is_empty());
    drop(repo);

    // The folder is lost. A new one is set up with the same remote.
    fs::remove_dir_all(&folder).unwrap();
    let fresh = tmp.sub("fresh");
    let mut repo = Repo::init(&fresh).unwrap();
    let remote = add_disk(&mut repo, "mine", &store);
    let scratch = repo.scratch_dir();
    let sealed = repo
        .with_rclone(|| metadata::download(&remote.target, &scratch))
        .unwrap();
    assert!(metadata::unseal(&sealed, "wrong horse").is_err());
    let (db, conf) = metadata::unseal(&sealed, "correct horse").unwrap();
    let report = repo.restore_metadata(&db, &conf).unwrap();
    assert_eq!(report.files, 2, "{:?}", report.skipped);
    // The backed-up remote joins the one used to restore.
    assert_eq!(report.remotes, 1);
    assert_eq!(repo.db.list_remotes().unwrap().len(), 2);
    assert_eq!(repo.db.metadata_remotes().unwrap()[0].name, "disk");

    let status = repo.status().unwrap();
    let paths: Vec<&str> = status.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["notes.txt", "photos/cat.jpg"]);
    assert!(status.iter().all(|s| !s.present_locally && s.remotes == ["disk"]));
    assert!(repo.scan().unwrap().is_empty(), "restored links match the db");

    let disk = repo.db.remote("disk").unwrap().unwrap();
    for hash in repo.blobs_missing_locally("disk").unwrap() {
        repo.fetch_blob_from(&disk, &hash).unwrap();
    }
    assert_eq!(fs::read_to_string(fresh.join("photos/cat.jpg")).unwrap(), "meow");

    // Restoring again changes nothing.
    let again = repo.restore_metadata(&db, &conf).unwrap();
    assert_eq!((again.files, again.history, again.remotes), (0, 0, 0));
}

#[test]
fn metadata_backup_merges_into_a_folder_in_use() {
    let tmp = TempDir::new("merge");
    let folder = tmp.sub("folder");
    let store = tmp.sub("store");
    fs::write(folder.join("a.txt"), "one").unwrap();
    fs::write(folder.join("b.txt"), "bee").unwrap();

    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    // A second version of a.txt, so the backup has history.
    fs::remove_file(folder.join("a.txt")).unwrap();
    fs::write(folder.join("a.txt"), "two").unwrap();
    assert_eq!(repo.scan().unwrap().replaced, ["a.txt"]);
    let remote = add_disk(&mut repo, "disk", &store);
    for hash in repo.blobs_missing_on_remote("disk").unwrap() {
        repo.push_blob(&remote, &hash).unwrap();
    }
    repo.set_min_copies(3).unwrap();
    let a_added = repo.db.active_path("a.txt").unwrap().unwrap().added_at;
    let snapshot = repo.metadata_snapshot().unwrap();
    drop(repo);

    // The database is lost; b.txt is gone too, and c.txt is new. A fresh
    // database adopts what is left, with the remote set up again.
    fs::remove_file(folder.join(".mbr/mbr.db")).unwrap();
    fs::remove_file(folder.join("b.txt")).unwrap();
    fs::write(folder.join("c.txt"), "sea").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    add_disk(&mut repo, "disk", &store);

    let report = repo
        .restore_metadata(&snapshot.db, &snapshot.rclone_conf)
        .unwrap();
    assert_eq!(report.files, 1, "{:?}", report.skipped);
    assert_eq!(report.history, 1);
    assert_eq!(report.remotes, 0);
    assert!(report.conflicts.is_empty());
    assert!(folder.join(metadata::BEFORE_RESTORE_FILE).is_file());

    let status = repo.status().unwrap();
    let paths: Vec<&str> = status.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(status[0].remotes, ["disk"]);
    assert_eq!(status[1].remotes, ["disk"]);
    assert!(status[2].remotes.is_empty());
    assert!(status.iter().all(|s| s.present_locally));
    assert_eq!(fs::read_to_string(folder.join("b.txt")).unwrap(), "bee");
    assert_eq!(
        repo.db.active_path("a.txt").unwrap().unwrap().added_at,
        a_added
    );
    assert_eq!(repo.min_copies().unwrap(), 3);
    assert!(repo.scan().unwrap().is_empty(), "merged links match the db");

    // Merging again changes nothing.
    let again = repo
        .restore_metadata(&snapshot.db, &snapshot.rclone_conf)
        .unwrap();
    assert_eq!((again.files, again.history, again.remotes), (0, 0, 0));

    // A remote whose target differs here keeps ours.
    let elsewhere = tmp.sub("elsewhere").display().to_string();
    repo.db.update_remote("disk", "disk", &elsewhere).unwrap();
    let report = repo
        .restore_metadata(&snapshot.db, &snapshot.rclone_conf)
        .unwrap();
    assert_eq!(report.conflicts, ["disk"]);
    assert_eq!(repo.db.remote("disk").unwrap().unwrap().target, elsewhere);
}

#[test]
fn metadata_merge_keeps_newer_local_versions() {
    let tmp = TempDir::new("merge-newer");
    let folder = tmp.sub("folder");
    fs::write(folder.join("a.txt"), "one").unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let snapshot = repo.metadata_snapshot().unwrap();

    // a.txt changes after the backup was made.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::remove_file(folder.join("a.txt")).unwrap();
    fs::write(folder.join("a.txt"), "two").unwrap();
    repo.scan().unwrap();
    let report = repo
        .restore_metadata(&snapshot.db, &snapshot.rclone_conf)
        .unwrap();
    assert_eq!((report.files, report.history), (0, 0), "already known");

    // With the history lost, the backup's version still loses to ours.
    drop(repo);
    fs::remove_file(folder.join(".mbr/mbr.db")).unwrap();
    let mut repo = Repo::init(&folder).unwrap();
    repo.scan().unwrap();
    let report = repo
        .restore_metadata(&snapshot.db, &snapshot.rclone_conf)
        .unwrap();
    assert_eq!((report.files, report.history), (0, 1));
    assert_eq!(fs::read_to_string(folder.join("a.txt")).unwrap(), "two");
    assert!(repo.scan().unwrap().is_empty());
}
