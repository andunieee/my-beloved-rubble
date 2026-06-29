//! Screenshots for the README and the user guide: the real UI, rendered
//! offscreen by Slint's software renderer, over a made-up folder. Nothing
//! touches the network; the demo folder and its database live in the
//! system temp directory and are deleted afterwards.
//!
//! `cargo run --release --features screenshots --bin screenshots -- [dir]`

use super::*;
use mbr::db::RemoteRecord;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, PhysicalSize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The demo folder's screen size, in logical pixels, rendered at twice that.
const WINDOW: (u32, u32) = (1100, 720);
const SCALE: f32 = 2.0;
const PASSWORD: &str = "correct horse battery staple";
/// What the title bar shows instead of the temp directory holding the demo.
const DISPLAY_PATH: &str = "~/Pictures/family-archive";

type Queue = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

/// A Slint platform with one offscreen window, whose event loop is
/// whatever [`Shooter::settle`] drains.
struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

struct Proxy(Queue);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

struct Shooter {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
    ui: AppWindow,
    out: PathBuf,
}

impl Shooter {
    /// Run what the app sent to the event loop, until it has been quiet for
    /// a moment (enough for layout and the few short animations).
    fn settle(&self) {
        let mut quiet_since = Instant::now();
        while quiet_since.elapsed() < Duration::from_millis(250) {
            let jobs = std::mem::take(&mut *self.queue.lock().unwrap());
            if !jobs.is_empty() {
                quiet_since = Instant::now();
            }
            for job in jobs {
                job();
            }
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn resize(&self, (w, h): (u32, u32)) {
        self.window.set_size(PhysicalSize::new(
            (w as f32 * SCALE) as u32,
            (h as f32 * SCALE) as u32,
        ));
    }

    fn shoot(&self, name: &str) {
        self.settle();
        let shot = self
            .ui
            .window()
            .take_snapshot()
            .expect("failed to render the window");
        let path = self.out.join(format!("{name}.png"));
        let file = std::fs::File::create(&path).expect("failed to create the screenshot file");
        let mut png = png::Encoder::new(std::io::BufWriter::new(file), shot.width(), shot.height());
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .and_then(|mut w| w.write_image_data(shot.as_bytes()))
            .expect("failed to write the screenshot");
        println!("{}", path.display());
    }
}

pub fn run(out: &Path) -> Result<(), slint::PlatformError> {
    std::fs::create_dir_all(out).expect("failed to create the screenshot directory");

    let data = std::env::temp_dir().join(format!(
        "mybelovedrubble-screenshots-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("failed to create the demo folder");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    let queue = Queue::default();
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        queue: queue.clone(),
    }))
    .expect("a platform was already set");

    let (repo, state) = demo(&data);
    let app = new_app();
    *app.state.borrow_mut() = state;
    *app.password.borrow_mut() = Some(SecretString::from(PASSWORD));
    *app.repo.borrow_mut() = Some(repo);
    app.refresh();
    app.window.set_folder(DISPLAY_PATH.into());
    app.window.show()?;

    let s = Shooter {
        window,
        queue,
        ui: app.window.clone_strong(),
        out: out.to_path_buf(),
    };
    s.resize(WINDOW);

    // ── the Blobs tab ──
    app.window
        .set_status_text("Scan finished: 14 added, 1 replaced, 2 removed".into());
    s.shoot("blobs");

    select(&app, &["notes/recipes.md"]);
    s.shoot("blob-detail");

    select(&app, &["notes/todo.md"]);
    s.shoot("blob-versions");

    select(
        &app,
        &[
            "photos/2024-06 Lisbon/beach.jpg",
            "photos/2024-06 Lisbon/pasteis.jpg",
            "notes/recipes.md",
            "videos/midsummer.mp4",
        ],
    );
    s.shoot("blobs-selected");

    // ── the Remotes tab, with the form for a new one ──
    app.window.set_active_tab(1);
    app.select_backend("Backblaze B2");
    app.window.set_new_remote_name("cold-storage".into());
    app.window.set_form_values(ModelRc::new(VecModel::from(vec![
        SharedString::from("0051a2b3c4d5"),
        SharedString::from("K005a1b2c3d4e5f6"),
        SharedString::from("false"),
        SharedString::from("family-archive"),
    ])));
    app.window
        .set_status_text("3 remotes · two also receive metadata backups".into());
    s.shoot("remotes");

    // ── the Settings tab ──
    s.resize((1280, 720));
    app.window.set_active_tab(2);
    app.window
        .set_status_text("One backup is up to date; one is older than the latest change".into());
    s.shoot("settings");

    // ── the Peers tab ──
    s.resize(WINDOW);
    let root = app.root().expect("a folder is open");
    let [laptop, phone, desktop] = [1u8, 2, 3].map(|b| peers::device_id(&[b; 32]));
    {
        let mut state = app.state.borrow_mut();
        let peers = &mut state.repo_mut(&root).peers;
        peers.insert(
            laptop.clone(),
            Peer {
                name: "laptop".into(),
            },
        );
        peers.insert(
            phone.clone(),
            Peer {
                name: "phone".into(),
            },
        );
    }
    let synced = format!("synced {}", util::format_time(util::now() - 120));
    app.peer_status.borrow_mut().insert(laptop, (synced, true));
    app.peer_status.borrow_mut().insert(
        phone,
        ("unreachable (offline, or not running mbr)".into(), false),
    );
    app.knocking.borrow_mut().insert(desktop);
    app.refresh();
    app.window.set_folder(DISPLAY_PATH.into());
    app.window.set_active_tab(3);
    app.window
        .set_status_text("Synced: 3 file(s) updated".into());
    s.shoot("peers");

    // ── a folder that isn't an mbr folder yet ──
    s.resize(WINDOW);
    *app.repo.borrow_mut() = None;
    *app.password.borrow_mut() = None;
    app.refresh();
    app.selection.borrow_mut().clear();
    app.show_selection();
    app.window.set_pending_folder(DISPLAY_PATH.into());
    app.window.set_active_tab(0);
    app.window
        .set_status_text("Start a new mbr folder here, or restore a lost one into it".into());
    s.shoot("welcome");

    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// An [`App`] like `main` makes, with no folder open yet.
fn new_app() -> App {
    let window = AppWindow::new().expect("failed to create the window");
    window.global::<Docs>().on_open(|_| {});
    let files = Rc::new(VecModel::default());
    window.set_files(ModelRc::from(files.clone()));
    let (tx, _rx) = mpsc::channel::<Msg>();
    let app = App {
        window,
        repo: Rc::new(RefCell::new(None)),
        tx,
        tasks: Rc::new(Cell::new(0)),
        scanning: Cell::new(false),
        statuses: RefCell::new(Vec::new()),
        files,
        selection: RefCell::new(Selection::default()),
        edit: RefCell::new(None),
        state: RefCell::new(AppState::default()),
        password: RefCell::new(None),
        node: RefCell::new(None),
        peer_status: RefCell::new(BTreeMap::new()),
        knocking: RefCell::new(BTreeSet::new()),
        sync_pending: Cell::new(false),
        setup: RefCell::new(None),
        pending_folder: RefCell::new(None),
    };

    let titles: Vec<SharedString> = backends::offered()
        .map(|b| SharedString::from(b.title))
        .collect();
    app.window
        .set_backend_titles(ModelRc::new(VecModel::from(titles)));
    if let Some(first) = backends::offered().next() {
        app.select_backend(first.title);
    }
    app
}

fn select(app: &App, paths: &[&str]) {
    app.selection.borrow_mut().paths = paths.iter().map(|p| (*p).to_owned()).collect();
    app.selection.borrow_mut().anchor = paths.first().map(|p| (*p).to_owned());
    app.show_selection();
}

/// Fill a folder with plausible files, scan it, add some old versions and
/// a few remotes, and return the open repo plus this machine's state.
fn demo(dir: &Path) -> (Repo, AppState) {
    let root = dir.join("family-archive");
    std::fs::create_dir_all(&root).unwrap();

    // name, size, first bytes; the beach appears twice, so the same content
    // is reachable under two names.
    let files: &[(&str, u64, &str)] = &[
        ("photos/2024-06 Lisbon/beach.jpg", 4_823_112, "beach"),
        ("photos/2024-06 Lisbon/pasteis.jpg", 2_147_883, "pasteis"),
        ("photos/2024-06 Lisbon/tram-28.jpg", 3_601_220, "tram"),
        ("photos/best-of-2024/beach.jpg", 4_823_112, "beach"),
        ("photos/cat-on-keyboard.png", 842_501, "cat"),
        ("scans/passport-2024.pdf", 1_248_392, "passport"),
        ("scans/lease-agreement.pdf", 2_904_370, "lease"),
        ("videos/midsummer.mp4", 8_874_003, "midsummer"),
        ("music/live-set.flac", 6_408_221, "live set"),
        ("documents/tax-return-2023.pdf", 2_204_112, "tax"),
        ("notes/old-name.md", 12_408, "recipes"),
        ("notes/todo.md", 3_120, "old todo"),
        ("projects/website/index.html", 48_204, "website"),
        ("projects/website/draft.html", 18_220, "draft"),
    ];
    for (path, size, prefix) in files {
        write_sized(&root.join(path), *size, prefix);
    }

    let mut repo = Repo::init(&root).unwrap();
    repo.scan().unwrap();

    // A rename: the old name stays in the blob's history.
    std::fs::rename(
        root.join("notes/old-name.md"),
        root.join("notes/recipes.md"),
    )
    .unwrap();
    repo.scan().unwrap();

    // A new version of a note: the old blob becomes an old local version.
    std::fs::remove_file(root.join("notes/todo.md")).unwrap();
    write_sized(&root.join("notes/todo.md"), 5_412, "new todo");
    repo.scan().unwrap();

    // A deleted file, whose blob is now unreferenced (and still here).
    std::fs::remove_file(root.join("projects/website/draft.html")).unwrap();
    repo.scan().unwrap();

    for (name, backend, spec, metadata) in [
        ("nas", "sftp", "home-nas", false),
        ("backup", "b2", "family-archive", true),
        ("offsite", "s3", "offsite-bucket", true),
    ] {
        repo.db
            .add_remote(
                name,
                &RemoteRecord {
                    backend: backend.to_owned(),
                    spec: spec.to_owned(),
                    encrypt: true,
                    metadata_backup: metadata,
                    config: BTreeMap::new(),
                },
            )
            .unwrap();
    }

    // Which remotes are recorded as holding each blob. todo.md stays local
    // only, so it is flagged at risk; midsummer.mp4 is then removed from
    // this computer to show a file that must be downloaded.
    let statuses = repo.status().unwrap();
    for s in &statuses {
        let remotes: &[&str] = match s.path.as_str() {
            "photos/2024-06 Lisbon/beach.jpg" | "photos/best-of-2024/beach.jpg" => {
                &["nas", "backup"]
            }
            "photos/2024-06 Lisbon/pasteis.jpg" => &["nas", "offsite"],
            "photos/2024-06 Lisbon/tram-28.jpg" => &["backup"],
            "photos/cat-on-keyboard.png" => &["backup", "offsite"],
            "scans/passport-2024.pdf" => &["nas"],
            "scans/lease-agreement.pdf" => &["backup", "offsite"],
            "videos/midsummer.mp4" => &["backup", "offsite"],
            "music/live-set.flac" => &["nas"],
            "documents/tax-return-2023.pdf" => &["backup"],
            "notes/recipes.md" => &["nas", "backup"],
            "projects/website/index.html" => &["nas"],
            _ => &[],
        };
        for remote in remotes {
            repo.db.set_blob_on_remote(&s.hash, remote).unwrap();
        }
    }
    let midsummer = repo.resolve_spec("videos/midsummer.mp4").unwrap();
    std::fs::remove_file(repo.blob_path(&midsummer)).unwrap();

    repo.set_password(PASSWORD).unwrap();
    let current = repo.db_hash().unwrap();
    let now = util::now();
    let mut state = AppState::default();
    {
        let repo_state = state.repo_mut(&root);
        repo_state.keep_password(PASSWORD);
        // A fixed key, so this computer's id is the same in every render.
        repo_state.node_key = Some("07".repeat(32));
        repo_state.metadata_backups.insert(
            "backup".into(),
            BackupRecord {
                at: now - 900,
                db_hash: current.clone(),
            },
        );
        repo_state.metadata_backups.insert(
            "offsite".into(),
            BackupRecord {
                at: now - 3 * 86_400,
                db_hash: "an older database".into(),
            },
        );
    }
    (repo, state)
}

/// Create a file of `size` bytes starting with `prefix`, without actually
/// writing megabytes of zeroes.
fn write_sized(path: &Path, size: u64, prefix: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(prefix.as_bytes()).unwrap();
    file.set_len(size).unwrap();
}
