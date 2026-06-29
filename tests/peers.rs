//! Two folders syncing their metadata over iroh, each with its own
//! endpoint, reaching the other over loopback (no relay or lookup needed).

#![cfg(unix)]

use mbr::Repo;
use mbr::db::RemoteRecord;
use mbr::peers::{self, Event, Node};
use mbr::state::RepoState;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A fresh empty directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mbr-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A folder with its node, recording what the node reports.
struct Device {
    repo: Repo,
    node: Node,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Device {
    fn new(root: &Path) -> Self {
        let repo = Repo::init(root).unwrap();
        let key = peers::node_key(&mut RepoState::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let log = events.clone();
        let node = Node::start(key, repo.db.shared(), &[], move |e| {
            log.lock().unwrap().push(e)
        })
        .unwrap();
        Self { repo, node, events }
    }

    /// Add `other` as a peer, knowing where it is.
    fn trust(&self, other: &Device) {
        self.node.set_peers(&[other.node.id()]);
        self.node.learn(other.node.addr());
    }

    fn refused(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Refused { peer } => Some(peer.clone()),
                _ => None,
            })
            .collect()
    }
}

fn link_hash(path: &Path) -> Option<String> {
    let target = fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

fn active_paths(repo: &mut Repo) -> Vec<String> {
    repo.status().unwrap().into_iter().map(|s| s.path).collect()
}

#[test]
fn folders_sync_once_both_sides_agree() {
    let (ta, tb) = (TempDir::new("peers-a"), TempDir::new("peers-b"));
    let (a_root, b_root) = (&ta.0, &tb.0);
    fs::create_dir_all(a_root.join("sub")).unwrap();
    fs::write(a_root.join("one.txt"), "one").unwrap();
    fs::write(a_root.join("sub/two.txt"), "two").unwrap();

    let mut a = Device::new(a_root);
    let mut b = Device::new(b_root);
    a.repo.scan().unwrap();
    a.repo.set_password("correct horse battery").unwrap();
    let record = RemoteRecord {
        backend: "local".into(),
        spec: "/mnt/disk".into(),
        encrypt: true,
        ..RemoteRecord::default()
    };
    a.repo.db.add_remote("disk", &record).unwrap();
    a.repo
        .db
        .set_blob_on_remote(&link_hash(&a_root.join("one.txt")).unwrap(), "disk")
        .unwrap();

    // Only A has added B: B refuses, and says who knocked.
    a.trust(&b);
    let refused = a.node.sync_now(&b.node.id()).unwrap_err();
    assert!(refused.contains("add this device"), "{refused}");
    assert_eq!(b.refused(), [a.node.id()]);

    // Once B adds A, B (which holds no files) joins A's folder.
    b.trust(&a);
    let report = a.node.sync_now(&b.node.id()).unwrap();
    assert!(!report.joined, "A keeps its folder");
    let applied = b.repo.apply_synced().unwrap();
    assert_eq!(applied.updated, ["one.txt", "sub/two.txt"]);
    assert!(applied.remotes_changed);
    assert_eq!(active_paths(&mut b.repo), ["one.txt", "sub/two.txt"]);
    // Linked, but the content is on A (or its remotes) only.
    assert_eq!(
        link_hash(&b_root.join("one.txt")),
        link_hash(&a_root.join("one.txt"))
    );
    assert!(fs::metadata(b_root.join("one.txt")).is_err(), "dangling");
    assert_eq!(b.repo.db.remote_record("disk").unwrap(), Some(record));
    assert_eq!(b.repo.db.blobs_on_remote("disk").unwrap().len(), 1);
    assert_eq!(b.repo.folder_id().unwrap(), a.repo.folder_id().unwrap());
    // The password is typed on B, and checked against A's.
    assert!(b.repo.verify_password("wrong password").is_err());
    b.repo.verify_password("correct horse battery").unwrap();
    assert!(b.repo.set_password("another password").is_err());

    // B's changes reach A: a new file, and a removed one.
    fs::write(b_root.join("three.txt"), "three").unwrap();
    fs::remove_file(b_root.join("sub/two.txt")).unwrap();
    b.repo.scan().unwrap();
    b.node.sync_now(&a.node.id()).unwrap();
    let applied = a.repo.apply_synced().unwrap();
    assert_eq!(applied.updated, ["sub/two.txt", "three.txt"]);
    assert!(!applied.remotes_changed);
    assert!(!a_root.join("sub").exists(), "emptied folders go too");
    assert_eq!(
        link_hash(&a_root.join("three.txt")),
        link_hash(&b_root.join("three.txt"))
    );
    // History came along: two.txt's past is known on both sides.
    assert!(
        a.repo
            .db
            .list_paths()
            .unwrap()
            .iter()
            .any(|r| r.path == "sub/two.txt" && !r.is_active())
    );

    // A file changed on A but not scanned yet is left for the scan.
    fs::remove_file(a_root.join("one.txt")).unwrap();
    fs::write(a_root.join("one.txt"), "one, edited on A").unwrap();
    fs::remove_file(b_root.join("one.txt")).unwrap();
    b.repo.scan().unwrap();
    b.node.sync_now(&a.node.id()).unwrap();
    let applied = a.repo.apply_synced().unwrap();
    assert_eq!(applied.kept, ["one.txt"]);
    assert_eq!(
        fs::read_to_string(a_root.join("one.txt")).unwrap(),
        "one, edited on A"
    );
    a.repo.scan().unwrap();
    a.node.sync_now(&b.node.id()).unwrap();
    b.repo.apply_synced().unwrap();
    assert_eq!(
        link_hash(&b_root.join("one.txt")),
        link_hash(&a_root.join("one.txt")),
        "the newest change wins"
    );

    // A remote removed on one side goes on the other.
    a.repo.db.remove_remote("disk").unwrap();
    a.node.sync_now(&b.node.id()).unwrap();
    let applied = b.repo.apply_synced().unwrap();
    assert!(applied.remotes_changed);
    assert!(b.repo.db.remote_record("disk").unwrap().is_none());
}

#[test]
fn two_folders_with_files_never_merge() {
    let (ta, tb) = (TempDir::new("peers-x"), TempDir::new("peers-y"));
    fs::write(ta.0.join("x.txt"), "x").unwrap();
    fs::write(tb.0.join("y.txt"), "y").unwrap();
    let mut a = Device::new(&ta.0);
    let mut b = Device::new(&tb.0);
    a.repo.scan().unwrap();
    b.repo.scan().unwrap();
    a.trust(&b);
    b.trust(&a);
    let error = a.node.sync_now(&b.node.id()).unwrap_err();
    assert!(error.contains("different one"), "{error}");
    b.repo.apply_synced().unwrap();
    assert_eq!(active_paths(&mut b.repo), ["y.txt"]);
}

#[test]
fn devices_dialing_each_other_at_once_both_sync() {
    let (ta, tb) = (TempDir::new("peers-both-a"), TempDir::new("peers-both-b"));
    fs::write(ta.0.join("a.txt"), "a").unwrap();
    let mut a = Device::new(&ta.0);
    let mut b = Device::new(&tb.0);
    a.repo.scan().unwrap();
    a.trust(&b);
    b.trust(&a);

    // Each dials the other while the other's dial is under way.
    a.node.sync(&b.node.id());
    b.node.sync(&a.node.id());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let finished = |d: &Device| {
        d.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Synced { .. } | Event::Failed { .. }))
            .count()
    };
    // Every sync ends twice: once on the side that dialed, once on the other.
    while finished(&a) < 2 || finished(&b) < 2 {
        assert!(std::time::Instant::now() < deadline, "the syncs are stuck");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let failures: Vec<String> = [&a, &b]
        .iter()
        .flat_map(|d| d.events.lock().unwrap().clone())
        .filter_map(|e| match e {
            Event::Failed { error, .. } => Some(error),
            _ => None,
        })
        .collect();
    assert!(failures.is_empty(), "{failures:?}");
    b.repo.apply_synced().unwrap();
    assert_eq!(active_paths(&mut b.repo), ["a.txt"]);
}
