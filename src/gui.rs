#![cfg_attr(windows, windows_subsystem = "windows")]

//! `my-beloved-rubble` — the Slint GUI.
//!
//! Four tabs: **Blobs** (the annotated file listing, with multi-selection
//! and a sidebar acting on the selected blobs), **Remotes** (listing,
//! scanning, downloading from, setting up and editing remotes),
//! **Settings** (the master password, and which remotes receive the
//! encrypted metadata backups) and **Peers** (the other devices the
//! folder's metadata syncs with, over iroh). A folder that isn't an mbr
//! folder yet first asks whether to start anew there or restore a lost
//! folder into it.
//!
//! Slow work — rclone operations (push / fetch / list / stat / config) and
//! the filesystem half of a scan (hashing, moving files into the store) —
//! runs on background threads and reports back through an mpsc channel
//! drained by a UI timer, so the window stays responsive; database writes
//! always happen on the UI thread.

use age::secrecy::{ExposeSecret, SecretString};
use mbr::backends::{self, Backend, Field, Kind};
use mbr::db::Remote;
use mbr::metadata;
use mbr::peers::{self, Node};
use mbr::rclone::RemoteBlob;
use mbr::repo::{self, DropOutcome, LocalHealth, RemoteHealth, Repo};
use mbr::state::{AppState, BackupRecord, DEFAULT_FOLDER, Peer, RepoState};
use mbr::thumbs;
use mbr::util;
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

slint::include_modules!();

/// Screenshots for the README and the user guide, rendered offscreen over
/// made-up data: `cargo run --features screenshots --bin screenshots`.
#[cfg(feature = "screenshots")]
pub mod screenshots;

/// How long a status bar message stays up.
const STATUS_SECONDS: u64 = 10;

/// What a status bar message is about, for its color.
#[derive(Clone, Copy)]
enum Level {
    Info = 0,
    Success = 1,
    Warning = 2,
    Error = 3,
}

/// A status bar message.
struct Note(Level, String);

impl From<String> for Note {
    fn from(text: String) -> Self {
        Note(Level::Success, text)
    }
}

impl From<(Level, String)> for Note {
    fn from((level, text): (Level, String)) -> Self {
        Note(level, text)
    }
}

/// Completion messages from background threads.
enum Msg {
    /// One blob finished uploading; on success, record it in the db.
    Pushed {
        remote: String,
        hash: String,
        result: Result<(), String>,
    },
    /// One blob finished downloading (already verified and in place).
    Fetched {
        hash: String,
        result: Result<String, String>,
    },
    /// A remote listing finished; on success, reconcile the db with it.
    Listed {
        remote: String,
        result: Result<Vec<RemoteBlob>, String>,
    },
    /// One blob was looked up on a remote: its size if stored there.
    Checked {
        remote: String,
        hash: String,
        result: Result<Option<u64>, String>,
    },
    /// Final summary of a batch, for the status bar.
    Status(Level, String),
    /// The filesystem half of a scan of `root` finished; on success,
    /// record it in the db.
    Scanned {
        root: PathBuf,
        result: Result<Vec<repo::ScannedEntry>, String>,
    },
    /// A background task ended.
    Done,
    /// A remote configuration step returned from librclone.
    Setup {
        pending: PendingSetup,
        result: Result<mbr::rclone::SetupOutcome, String>,
    },
    /// The OAuth auth URL rclone logged while the browser flow runs.
    OAuthUrl(String),
    /// The user aborted an in-progress configuration.
    Cancelled { result: Result<(), String> },
    /// A metadata backup of the folder at `root` was uploaded to `remote`
    /// (or not); on success, record when, and which database state.
    MetadataUploaded {
        root: PathBuf,
        remote: String,
        db_hash: String,
        result: Result<(), String>,
    },
    /// A metadata backup was read and decrypted; on success, restore it
    /// into `dir`.
    BackupOpened {
        dir: PathBuf,
        password: SecretString,
        result: Result<Vec<u8>, String>,
    },
    /// An attempt to drop the local copy of a blob of `size` bytes.
    Dropped {
        size: u64,
        result: Result<DropOutcome, String>,
    },
    /// A local copy was re-hashed.
    VerifiedLocal {
        hash: String,
        result: Result<LocalHealth, String>,
    },
    /// A remote's copy was downloaded and checked.
    VerifiedRemote {
        remote: String,
        hash: String,
        result: Result<RemoteHealth, String>,
    },
    /// The rclone settings of the remote being edited were read.
    RemoteConfig {
        remote: String,
        result: Result<Option<serde_json::Map<String, serde_json::Value>>, String>,
    },
    /// The rclone half of saving a remote edit finished; on success,
    /// record the new settings and name.
    RemoteEdited {
        old: String,
        name: String,
        result: Result<(), String>,
    },
    /// Something happened syncing with the peers.
    Peer(peers::Event),
    /// A thumbnail for the file list was made (or not) in the folder at
    /// `root`.
    Thumb {
        root: PathBuf,
        hash: String,
        outcome: thumbs::Outcome,
    },
    /// The large preview of the selected file was made (or not).
    Preview {
        hash: String,
        outcome: thumbs::Outcome,
    },
}

impl Msg {
    /// Whether handling the message may change what the views show
    /// beyond what it updates itself.
    fn changes_the_folder(&self) -> bool {
        !matches!(
            self,
            Msg::Thumb { .. }
                | Msg::Preview { .. }
                | Msg::Peer(peers::Event::Discovered { .. } | peers::Event::Gone { .. })
        )
    }
}

/// Which rows of the blob list are selected, by path (so the selection
/// survives refreshes), plus the state of an ongoing click or drag.
#[derive(Default)]
struct Selection {
    paths: BTreeSet<String>,
    /// Where Shift+click and drags extend from.
    anchor: Option<String>,
    /// What a drag adds its range to: the previous selection for a
    /// Ctrl+drag, nothing otherwise.
    drag_base: BTreeSet<String>,
    /// Row the current press started on.
    pressed: Option<usize>,
    /// The current press has turned into a drag.
    dragging: bool,
}

impl Selection {
    /// A press on row `idx` of `rows` (paths in display order). `toggle`
    /// is Ctrl/Cmd, `extend` is Shift.
    fn press(&mut self, rows: &[String], idx: usize, toggle: bool, extend: bool) {
        let Some(path) = rows.get(idx) else { return };
        self.drag_base = if toggle {
            self.paths.clone()
        } else {
            BTreeSet::new()
        };
        match self.anchor_index(rows) {
            Some(anchor) if extend => {
                let range = range_of(rows, anchor, idx);
                if toggle {
                    self.paths.extend(range);
                } else {
                    self.paths = range;
                }
            }
            _ if toggle => {
                if !self.paths.remove(path) {
                    self.paths.insert(path.clone());
                }
                self.anchor = Some(path.clone());
            }
            _ => {
                self.paths = BTreeSet::from([path.clone()]);
                self.anchor = Some(path.clone());
            }
        }
        self.pressed = Some(idx);
        self.dragging = false;
    }

    /// The pointer, still pressed, is over row `target` (may be out of
    /// range when dragged past either end).
    fn drag(&mut self, rows: &[String], target: i32) {
        let (Some(start), false) = (self.pressed, rows.is_empty()) else {
            return;
        };
        let target = target.clamp(0, rows.len() as i32 - 1) as usize;
        if !self.dragging && target == start {
            return;
        }
        self.dragging = true;
        let anchor = self.anchor_index(rows).unwrap_or(start);
        self.paths = self.drag_base.clone();
        self.paths.extend(range_of(rows, anchor, target));
    }

    fn select_all(&mut self, rows: &[String]) {
        self.paths = rows.iter().cloned().collect();
    }

    fn clear(&mut self) {
        self.paths.clear();
        self.anchor = None;
    }

    fn anchor_index(&self, rows: &[String]) -> Option<usize> {
        let anchor = self.anchor.as_ref()?;
        rows.iter().position(|p| p == anchor)
    }
}

/// Paths of rows `a..=b` (in either order).
fn range_of(rows: &[String], a: usize, b: usize) -> BTreeSet<String> {
    rows[a.min(b)..=a.max(b)].iter().cloned().collect()
}

/// The remote being edited in the Remotes tab.
struct EditState {
    /// Name of the remote when editing started.
    original: String,
    /// The rclone section holding its backend settings, if any.
    section: Option<String>,
    /// Its backend, when mbr has a form for it.
    backend: Option<&'static Backend>,
    /// Form fields shown (the backend's rclone settings), and the values
    /// they started with.
    fields: Vec<&'static Field>,
    initial: Vec<String>,
}

struct App {
    window: AppWindow,
    repo: Rc<RefCell<Option<Repo>>>,
    tx: mpsc::Sender<Msg>,
    tasks: Rc<Cell<u32>>,
    /// A scan is running in the background.
    scanning: Cell<bool>,
    /// The listing shown in the Blobs tab, row for row.
    statuses: RefCell<Vec<repo::FileStatus>>,
    files: Rc<VecModel<FileRow>>,
    selection: RefCell<Selection>,
    edit: RefCell<Option<EditState>>,
    /// Local app state (`state.json` and each folder's `local.json`).
    state: RefCell<AppState>,
    /// The open folder's master password, saved on this machine or typed
    /// since the folder was opened.
    password: RefCell<Option<SecretString>>,
    /// Syncing the open folder with its peers, while it has any.
    node: RefCell<Option<Node>>,
    /// How the last sync with each peer went: `(text, ok)`.
    peer_status: RefCell<BTreeMap<String, (String, bool)>>,
    /// Devices that tried to sync but aren't peers.
    knocking: RefCell<BTreeSet<String>>,
    /// mbr devices found on the local network, by id: their suggested
    /// names, if they say.
    nearby: RefCell<BTreeMap<String, String>>,
    /// A sync brought changes in during a scan: applied once it ends.
    sync_pending: Cell<bool>,
    /// Previews of image files, by blob hash, for the file list.
    thumbs: RefCell<HashMap<String, slint::Image>>,
    /// Blobs with no thumbnail: not images, or neither here nor cached.
    no_thumb: RefCell<HashSet<String>>,
    /// Blobs whose thumbnail is being made.
    thumbs_pending: RefCell<HashSet<String>>,
    /// The blob the detail panel shows (or is making) a preview of.
    previewed: RefCell<Option<String>>,
    /// The remote being set up, while rclone still asks questions.
    setup: RefCell<Option<PendingSetup>>,
    /// A folder that isn't an mbr folder yet, while the user chooses to
    /// start anew there or to restore into it.
    pending_folder: RefCell<Option<PathBuf>>,
    /// Clears the status bar a while after each message.
    status_timer: slint::Timer,
}

/// A remote in the middle of its setup.
#[derive(Clone)]
struct PendingSetup {
    /// The rclone config it is set up in.
    config_path: PathBuf,
    /// The section rclone asks questions about: the backend itself.
    section: String,
    /// The bucket, share or folder named in the form.
    spec: String,
    /// The master password: keys the crypt layer, or names the backup.
    password: SecretString,
    purpose: Purpose,
    /// Set when the user cancels while a step runs: a browser sign-in is
    /// then cut short, and whatever the step returns is abandoned.
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
enum Purpose {
    /// A new remote of the open folder.
    Remote {
        layout: mbr::remotes::Layout,
        folder_id: String,
    },
    /// Reaching the remote holding the backup to restore `dir` from, in a
    /// throwaway rclone config.
    Restore { dir: PathBuf },
}

/// A step of a remote's setup.
enum SetupStep {
    Begin {
        backend: &'static str,
        parameters: serde_json::Value,
    },
    /// Answer rclone's pending question.
    Continue {
        state: String,
        answer: String,
        parameters: serde_json::Value,
    },
}

/// The answer the GUI gives rclone's `question` itself, if any: the GUI
/// runs on the user's own desktop, so OAuth sign-in always happens in a
/// browser here (rclone's "Use web browser to automatically authenticate?").
fn automatic_answer(question: &mbr::rclone::SetupQuestion) -> Option<&'static str> {
    (question.name == "config_is_local").then_some("true")
}

/// Keep answering `outcome`'s question while [`automatic_answer`] knows
/// the answer.
fn answer_automatically(
    section: &str,
    mut outcome: Result<mbr::rclone::SetupOutcome, String>,
    parameters: serde_json::Value,
    on_url: &mut dyn FnMut(String),
    cancel: &AtomicBool,
) -> Result<mbr::rclone::SetupOutcome, String> {
    loop {
        let (state, answer) = match &outcome {
            Ok(mbr::rclone::SetupOutcome::Question(q)) => match automatic_answer(q) {
                Some(answer) => (q.state.clone(), answer),
                None => return outcome,
            },
            _ => return outcome,
        };
        outcome = mbr::rclone::continue_setup_watching(
            section,
            &state,
            answer,
            parameters.clone(),
            on_url,
            cancel,
        );
    }
}

/// Where a setup step got to.
enum Reached {
    Step(mbr::rclone::SetupOutcome),
    /// The backup to restore from, downloaded.
    Backup(Vec<u8>),
}

impl PendingSetup {
    /// Run `step` (on a background thread): finish the remote's layers once
    /// rclone is done with its backend, or for a restore, download the
    /// backup made with the password from the place named.
    fn run(&self, step: SetupStep, on_url: &mut dyn FnMut(String)) -> Result<Reached, String> {
        mbr::rclone::with_config(&self.config_path, || {
            let outcome = match step {
                SetupStep::Begin {
                    backend,
                    parameters,
                } => {
                    if let Purpose::Remote { layout, .. } = &self.purpose {
                        layout.check_unused()?;
                    }
                    let begun =
                        mbr::rclone::begin_setup(&self.section, backend, parameters.clone());
                    answer_automatically(&self.section, begun, parameters, on_url, &self.cancelled)
                }
                SetupStep::Continue {
                    state,
                    answer,
                    parameters,
                } => {
                    let step = mbr::rclone::continue_setup_watching(
                        &self.section,
                        &state,
                        &answer,
                        parameters.clone(),
                        on_url,
                        &self.cancelled,
                    );
                    answer_automatically(&self.section, step, parameters, on_url, &self.cancelled)
                }
            };
            let password = self.password.expose_secret();
            match &self.purpose {
                Purpose::Remote { layout, folder_id } => layout
                    .finish(outcome, &self.spec, folder_id, password)
                    .map(Reached::Step),
                Purpose::Restore { dir } => match outcome? {
                    mbr::rclone::SetupOutcome::Done => {
                        let location = mbr::remotes::root_location(&self.section, &self.spec);
                        let name = metadata::backup_name(password)?;
                        metadata::download(&location, &name, &dir.join(repo::TMP_DIR))
                            .map(Reached::Backup)
                    }
                    question => Ok(Reached::Step(question)),
                },
            }
        })
    }

    /// The throwaway rclone config of a restore, to delete when done.
    fn throwaway_config(&self) -> Option<&PathBuf> {
        matches!(self.purpose, Purpose::Restore { .. }).then_some(&self.config_path)
    }
}

/// The user guide, built from `docs/` and published to GitHub Pages.
const DOCS_URL: &str = "https://andunieee.github.io/my-beloved-rubble/";

/// `page` as a URL: itself if it already is one, else a page of the docs.
fn docs_url(page: &str) -> String {
    if page.starts_with("https://") {
        page.to_owned()
    } else {
        format!("{DOCS_URL}{page}")
    }
}

#[cfg(not(target_os = "android"))]
#[allow(dead_code)]
fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    run()
}

/// What differs between the desktop and Android: how links open, and
/// how folders and files are picked.
mod platform {
    use std::path::PathBuf;

    /// Open `url` in the browser.
    #[cfg(not(target_os = "android"))]
    pub fn open_url(url: &str) -> Result<(), String> {
        open::that_detached(url).map_err(|e| e.to_string())
    }

    /// Open `url` in the browser, through an Android intent.
    #[cfg(target_os = "android")]
    pub fn open_url(url: &str) -> Result<(), String> {
        use jni::objects::{JObject, JValue};
        let fail = |e: jni::errors::Error| e.to_string();
        let ctx = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(fail)?;
        let mut env = vm.attach_current_thread().map_err(fail)?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        let url = env.new_string(url).map_err(fail)?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&url)],
            )
            .and_then(|v| v.l())
            .map_err(fail)?;
        let action = env.new_string("android.intent.action.VIEW").map_err(fail)?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValue::Object(&action), JValue::Object(&uri)],
            )
            .map_err(fail)?;
        env.call_method(
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )
        .map_err(fail)?;
        Ok(())
    }

    /// Ask for a folder (none on Android: the app has its own).
    pub fn pick_folder() -> Option<PathBuf> {
        #[cfg(not(target_os = "android"))]
        return rfd::FileDialog::new().pick_folder();
        #[cfg(target_os = "android")]
        None
    }

    /// Ask for a file (none on Android yet).
    pub fn pick_file() -> Option<PathBuf> {
        #[cfg(not(target_os = "android"))]
        return rfd::FileDialog::new().pick_file();
        #[cfg(target_os = "android")]
        None
    }
}

/// The names of the folders the app keeps in its own storage (Android),
/// sorted; none on the desktop.
fn app_folders() -> Vec<String> {
    let Some(dir) = AppState::folders_dir() else {
        return Vec::new();
    };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Run the app: on the desktop from `main`, on Android once the activity
/// is set up (see `android/`).
pub fn run() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    window.set_desktop(cfg!(not(target_os = "android")));
    window.global::<Docs>().on_open(|page| {
        let url = docs_url(&page);
        if let Err(e) = platform::open_url(&url) {
            log::warn!("cannot open {url}: {e}");
        }
    });
    let (tx, rx) = mpsc::channel::<Msg>();
    let files = Rc::new(VecModel::default());
    window.set_files(ModelRc::from(files.clone()));
    let app = Rc::new(App {
        window,
        repo: Rc::new(RefCell::new(None)),
        tx,
        tasks: Rc::new(Cell::new(0)),
        scanning: Cell::new(false),
        statuses: RefCell::new(Vec::new()),
        files,
        selection: RefCell::new(Selection::default()),
        edit: RefCell::new(None),
        state: RefCell::new(AppState::load()),
        password: RefCell::new(None),
        node: RefCell::new(None),
        peer_status: RefCell::new(BTreeMap::new()),
        knocking: RefCell::new(BTreeSet::new()),
        nearby: RefCell::new(BTreeMap::new()),
        sync_pending: Cell::new(false),
        thumbs: RefCell::default(),
        no_thumb: RefCell::default(),
        thumbs_pending: RefCell::default(),
        previewed: RefCell::new(None),
        setup: RefCell::new(None),
        pending_folder: RefCell::new(None),
        status_timer: slint::Timer::default(),
    });

    // ── message pump for background tasks ──
    let timer = slint::Timer::default();
    {
        let app = app.clone();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let mut refresh_needed = false;
                while let Ok(msg) = rx.try_recv() {
                    refresh_needed |= msg.changes_the_folder();
                    app.handle(msg);
                }
                if refresh_needed {
                    app.absorb_rclone_config();
                }
                if refresh_needed {
                    app.refresh();
                }
            },
        );
    }

    // ── syncing with the peers, every minute ──
    let peer_timer = slint::Timer::default();
    {
        let app = app.clone();
        peer_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(60),
            move || app.sync_peers(),
        );
    }

    // ── callbacks ──
    let w = &app.window;
    {
        let app = app.clone();
        w.on_open_file(move |index| app.open_file(index as usize));
    }
    {
        let app = app.clone();
        w.on_open_folder(move || {
            if let Some(dir) = platform::pick_folder() {
                app.attach(dir);
            }
        });
    }
    {
        let app = app.clone();
        w.on_choose_folder(move |name| app.choose_folder(&name));
    }
    {
        let app = app.clone();
        w.on_create_folder(move |name| app.create_folder(&name));
    }
    {
        let app = app.clone();
        w.on_rescan(move || app.rescan());
    }

    // blob selection
    {
        let app = app.clone();
        w.on_row_pressed(move |idx, toggle, extend| {
            let rows = app.row_paths();
            app.selection
                .borrow_mut()
                .press(&rows, idx.max(0) as usize, toggle, extend);
            app.show_selection();
        });
    }
    {
        let app = app.clone();
        w.on_row_dragged(move |target| {
            let rows = app.row_paths();
            app.selection.borrow_mut().drag(&rows, target);
            app.show_selection();
        });
    }
    {
        let app = app.clone();
        w.on_select_all(move || {
            let rows = app.row_paths();
            app.selection.borrow_mut().select_all(&rows);
            app.show_selection();
        });
    }
    {
        let app = app.clone();
        w.on_clear_selection(move || {
            app.selection.borrow_mut().clear();
            app.show_selection();
        });
    }

    // actions on the selected blobs
    {
        let app = app.clone();
        w.on_upload_selected(move |remote| app.upload_selected(&remote));
    }
    {
        let app = app.clone();
        w.on_check_selected(move |remote| app.check_selected(&remote));
    }
    {
        let app = app.clone();
        w.on_download_selected(move |remote| app.download_selected(&remote));
    }

    // remote management
    {
        let app = app.clone();
        w.on_scan_remote(move |name| app.scan_remotes(Some(name.as_str())));
    }
    {
        let app = app.clone();
        w.on_scan_all_remotes(move || app.scan_remotes(None));
    }
    {
        let app = app.clone();
        w.on_download_all(move |name| app.download_all(&name));
    }
    {
        let app = app.clone();
        w.on_push_all(move |remote| {
            let missing = {
                let mut repo = app.repo.borrow_mut();
                let Some(repo) = repo.as_mut() else { return };
                match repo.blobs_missing_on_remote(&remote) {
                    Ok(missing) => missing,
                    Err(e) => {
                        app.error(format!("Error: {e}"));
                        return;
                    }
                }
            };
            if missing.is_empty() {
                app.status(format!("Every local blob is already on '{remote}'"));
                return;
            }
            app.push_blobs(&remote, missing);
        });
    }
    {
        let app = app.clone();
        w.on_remove_remote(move |name| app.remove_remote(&name));
    }
    {
        let app = app.clone();
        w.on_edit_remote(move |name| app.edit_remote(&name));
    }
    {
        let app = app.clone();
        w.on_save_remote_edit(move || app.save_remote_edit());
    }
    {
        let app = app.clone();
        w.on_cancel_remote_edit(move || app.close_editor());
    }
    {
        let app = app.clone();
        w.on_select_backend(move |name| app.select_backend(&name));
    }
    {
        let app = app.clone();
        w.on_submit_form(move |name, backend, values| {
            let values: Vec<SharedString> = values.iter().collect();
            app.submit_form(&name, &backend, &values);
        });
    }
    {
        let app = app.clone();
        w.on_submit_remote_setup(move |backend, state, answer| {
            app.submit_remote_setup(&backend, &state, &answer);
        });
    }
    {
        let app = app.clone();
        w.on_cancel_remote_setup(move || app.cancel_remote_setup());
    }
    w.on_open_url(|url| {
        if let Err(e) = platform::open_url(url.as_str()) {
            log::warn!("cannot open {url}: {e}");
        }
    });

    // keeping copies safe
    {
        let app = app.clone();
        w.on_drop_selected(move || {
            let hashes = app.selected_hashes();
            app.drop_blobs(hashes);
        });
    }
    {
        let app = app.clone();
        w.on_verify_selected(move || {
            let hashes = app.selected_hashes();
            app.verify_local(hashes);
        });
    }
    {
        let app = app.clone();
        w.on_verify_selected_on(move |remote| {
            let hashes = app.selected_hashes();
            app.verify_on_remote(&remote, hashes);
        });
    }
    {
        let app = app.clone();
        w.on_verify_all_local(move || {
            let hashes = {
                let mut repo = app.repo.borrow_mut();
                let Some(repo) = repo.as_mut() else { return };
                match repo.db.list_blobs() {
                    Ok(blobs) => blobs
                        .into_iter()
                        .map(|b| b.hash)
                        .filter(|h| repo.blob_present(h))
                        .collect(),
                    Err(e) => {
                        app.error(format!("Error: {e}"));
                        return;
                    }
                }
            };
            app.verify_local(hashes);
        });
    }
    {
        let app = app.clone();
        w.on_clean_up_old(move || {
            let hashes = {
                let mut repo = app.repo.borrow_mut();
                let Some(repo) = repo.as_mut() else { return };
                match repo.unreferenced_blobs() {
                    Ok(old) => old
                        .into_iter()
                        .filter(|u| u.present_locally)
                        .map(|u| u.hash)
                        .collect(),
                    Err(e) => {
                        app.error(format!("Error: {e}"));
                        return;
                    }
                }
            };
            app.drop_blobs(hashes);
        });
    }
    {
        let app = app.clone();
        w.on_set_min_copies(move |n| {
            app.with_repo("change copies", |repo| {
                repo.set_min_copies(n.max(1) as u32)?;
                Ok(format!("Keeping at least {n} copies of every file"))
            });
            app.refresh();
        });
    }

    // metadata backup
    {
        let app = app.clone();
        w.on_toggle_metadata_remote(move |name, enabled| {
            app.with_repo("choose remote", |repo| {
                repo.db.set_metadata_remote(&name, enabled)?;
                Ok(if enabled {
                    format!("Metadata will be backed up to {name}")
                } else {
                    format!("Metadata will no longer be backed up to {name}")
                })
            });
            app.refresh();
        });
    }
    {
        let app = app.clone();
        w.on_set_password(move |password, repeat| app.set_password(&password, &repeat));
    }
    {
        let app = app.clone();
        w.on_test_password(move |password| app.test_password(&password));
    }
    {
        let app = app.clone();
        w.on_unlock(move |password| app.unlock(&password));
    }

    // peers
    {
        let app = app.clone();
        w.on_add_peer(move |id, name| app.add_peer(&id, &name));
    }
    {
        let app = app.clone();
        w.on_peers_visible(move |visible| app.peers_visible(visible));
    }
    {
        let app = app.clone();
        w.on_remove_peer(move |id| app.remove_peer(&id));
    }
    {
        let app = app.clone();
        w.on_sync_peers(move || app.sync_peers());
    }
    {
        let app = app.clone();
        w.on_backup_metadata(move || app.backup_metadata());
    }

    // a folder that isn't an mbr folder yet
    {
        let app = app.clone();
        w.on_start_new(move || app.start_new());
    }
    {
        let app = app.clone();
        w.on_pick_restore_file(move || app.pick_restore_file());
    }
    {
        let app = app.clone();
        w.on_restore_from_backup(move |password| app.restore_from_backup(&password));
    }
    {
        let app = app.clone();
        w.on_restore_from_remote(move |backend, values, password| {
            let values: Vec<SharedString> = values.iter().collect();
            app.restore_from_remote(&backend, &values, &password);
        });
    }
    {
        let app = app.clone();
        w.on_cancel_restore(move || app.cancel_restore());
    }

    // ── backend picker + initial form ──
    let titles: Vec<SharedString> = backends::offered()
        .map(|b| SharedString::from(b.title))
        .collect();
    app.window
        .set_backend_titles(ModelRc::new(VecModel::from(titles)));
    if let Some(first) = backends::offered().next() {
        app.select_backend(first.title);
    }

    // ── reopen last folder ──
    let last = app.state.borrow().last_folder.clone();
    if let Some(dir) = last.filter(|dir| Repo::is_initialized(dir)) {
        app.attach(dir);
    } else if let Some(dir) = AppState::folders_dir() {
        // Android: the app keeps its folders in its own storage.
        let name = app_folders()
            .into_iter()
            .next()
            .unwrap_or_else(|| DEFAULT_FOLDER.to_owned());
        let dir = dir.join(name);
        std::fs::create_dir_all(&dir).ok();
        app.attach(dir);
    }
    app.show_app_folders();

    app.window.run()
}

impl App {
    /// Show `text` in the status bar until it is tapped, or for
    /// STATUS_SECONDS.
    fn notify(&self, level: Level, text: impl Into<SharedString>) {
        self.window.set_status_text(text.into());
        self.window.set_status_level(level as i32);
        let window = self.window.as_weak();
        self.status_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_secs(STATUS_SECONDS),
            move || {
                if let Some(w) = window.upgrade() {
                    w.set_status_text(SharedString::default());
                }
            },
        );
    }

    fn status(&self, text: impl Into<SharedString>) {
        self.notify(Level::Info, text);
    }

    fn success(&self, text: impl Into<SharedString>) {
        self.notify(Level::Success, text);
    }

    fn warn(&self, text: impl Into<SharedString>) {
        self.notify(Level::Warning, text);
    }

    fn error(&self, text: impl Into<SharedString>) {
        self.notify(Level::Error, text);
    }

    /// Run a database operation against the open repo, reporting the
    /// outcome in the status bar.
    fn with_repo<T: Into<Note>>(&self, what: &str, f: impl FnOnce(&mut Repo) -> Result<T, String>) {
        let mut repo = self.repo.borrow_mut();
        match repo.as_mut() {
            Some(repo) => match f(repo) {
                Ok(note) => {
                    let Note(level, text) = note.into();
                    self.notify(level, text)
                }
                Err(e) => self.error(format!("Error: {e}")),
            },
            None => self.error(format!("Cannot {what}: no folder open")),
        }
    }

    /// Look up remote `name` in the open repo, reporting failures.
    fn remote(&self, name: &str) -> Option<Remote> {
        let mut repo = self.repo.borrow_mut();
        match repo.as_mut()?.db.remote(name) {
            Ok(Some(remote)) => Some(remote),
            Ok(None) => {
                self.error(format!("No remote named '{name}'"));
                None
            }
            Err(e) => {
                self.error(format!("Error: {e}"));
                None
            }
        }
    }

    fn config_path(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(Repo::rclone_config_path)
    }

    /// Open the mbr folder `dir`; for a folder that isn't one yet, offer to
    /// start anew there or to restore a lost folder into it.
    fn attach(&self, dir: PathBuf) {
        self.cancel_restore();
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        self.window.set_app_folder(name.as_ref().into());
        if Repo::is_initialized(&dir) {
            match Repo::init(&dir) {
                Ok(repo) => self.open_repo(repo),
                Err(e) => self.error(format!("Error: {e}")),
            }
            return;
        }
        let dir = dir.canonicalize().unwrap_or(dir);
        self.close_repo();
        self.window
            .set_pending_folder(dir.display().to_string().into());
        self.status("Start a new mbr folder here, or restore a lost one into it");
        *self.pending_folder.borrow_mut() = Some(dir);
    }

    /// Show `repo` as the open folder, writing its rclone settings again
    /// if some remote's are missing.
    fn open_repo(&self, mut repo: Repo) {
        *self.pending_folder.borrow_mut() = None;
        self.window.set_pending_folder(SharedString::default());
        self.state.borrow_mut().last_folder = Some(repo.root.clone());
        self.save_state();
        let saved = self.state.borrow().repo(&repo.root).password;
        let rewritten = repo.rclone_config_incomplete().and_then(|incomplete| {
            if incomplete {
                repo.write_rclone_config(saved.as_deref())?;
            }
            Ok(())
        });
        if let Err(e) = rewritten {
            self.error(format!("Cannot write the rclone settings: {e}"));
        }
        *self.password.borrow_mut() = saved.map(SecretString::from);
        self.reset_setup_ui();
        *self.node.borrow_mut() = None;
        *self.repo.borrow_mut() = Some(repo);
        *self.selection.borrow_mut() = Selection::default();
        self.peer_status.borrow_mut().clear();
        self.knocking.borrow_mut().clear();
        self.nearby.borrow_mut().clear();
        self.sync_pending.set(false);
        self.close_editor();
        self.start_node();
        self.refresh();
        self.rescan();
    }

    fn close_repo(&self) {
        // The node shares the database: it stops first.
        *self.node.borrow_mut() = None;
        *self.repo.borrow_mut() = None;
        *self.password.borrow_mut() = None;
        *self.selection.borrow_mut() = Selection::default();
        self.close_editor();
        self.reset_setup_ui();
        self.peer_status.borrow_mut().clear();
        self.knocking.borrow_mut().clear();
        self.nearby.borrow_mut().clear();
        self.refresh();
    }

    // ── the folders in the app's own storage (Android) ──

    fn show_app_folders(&self) {
        let names: Vec<SharedString> = app_folders().into_iter().map(Into::into).collect();
        self.window
            .set_app_folders(ModelRc::new(VecModel::from(names)));
    }

    /// Open the app's folder called `name`.
    fn choose_folder(&self, name: &str) {
        let Some(dir) = AppState::folders_dir() else {
            return;
        };
        self.window.set_choosing_folder(false);
        self.attach(dir.join(name));
    }

    /// Make a new folder called `name` in the app's storage, and open it.
    fn create_folder(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
            self.warn("A folder's name can't be empty, start with a dot or have a slash");
            return;
        }
        let Some(dir) = AppState::folders_dir() else {
            return;
        };
        let dir = dir.join(name);
        if dir.exists() {
            self.warn(format!("There already is a folder named {name}"));
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error(format!("Cannot create {name}: {e}"));
            return;
        }
        self.window.set_new_folder_name(SharedString::default());
        self.show_app_folders();
        self.choose_folder(name);
    }

    // ── a folder that isn't an mbr folder yet ──

    fn start_new(&self) {
        let Some(dir) = self.pending_folder.borrow().clone() else {
            return;
        };
        match Repo::init(&dir) {
            Ok(repo) => {
                self.open_repo(repo);
                self.window.set_active_tab(2);
                self.success(format!("{} is now an mbr folder", dir.display()));
            }
            Err(e) => self.error(format!("Error: {e}")),
        }
    }

    /// Pick the encrypted backup file to restore from.
    fn pick_restore_file(&self) {
        if let Some(file) = platform::pick_file() {
            self.window
                .set_restore_backup_file(file.display().to_string().into());
        }
    }

    /// Decrypt the chosen backup file in the background; it is restored
    /// when it arrives (see [`Msg::BackupOpened`]).
    fn restore_from_backup(&self, password: &str) {
        let Some(dir) = self.pending_folder.borrow().clone() else {
            return;
        };
        let file = PathBuf::from(self.window.get_restore_backup_file().as_str());
        let password = SecretString::from(password.to_owned());
        self.status("Decrypting the backup…");
        self.spawn(move |tx| {
            let result = std::fs::read(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))
                .and_then(|sealed| metadata::unseal(&sealed, password.expose_secret()));
            tx.send(Msg::BackupOpened {
                dir,
                password,
                result,
            })
            .ok();
        });
    }

    /// Set up the remote described by the form in a throwaway rclone config
    /// and download the backup made with `password` from it.
    fn restore_from_remote(&self, backend_name: &str, values: &[SharedString], password: &str) {
        let Some(dir) = self.pending_folder.borrow().clone() else {
            return;
        };
        let Some((backend, spec)) = self.check_form(backend_name, values) else {
            return;
        };
        let config_path = dir.join(repo::TMP_DIR).join("restore-rclone.conf");
        std::fs::remove_file(&config_path).ok();
        if let Err(e) = repo::ensure_rclone_config(&config_path) {
            self.error(format!("Error: {e}"));
            return;
        }
        let pending = PendingSetup {
            config_path,
            section: format!("{}restore", mbr::remotes::ROOT_PREFIX),
            spec,
            password: SecretString::from(password.to_owned()),
            purpose: Purpose::Restore { dir },
            cancelled: Arc::default(),
        };
        let parameters = backends::form_parameters(backend, values);
        self.run_setup_step(
            pending,
            SetupStep::Begin {
                backend: backend.name,
                parameters,
            },
        );
    }

    /// Make the pending folder the one a decrypted backup describes.
    fn finish_restore(&self, dir: &Path, dump: &[u8], password: SecretString) {
        let (mut repo, report) = match metadata::restore(dir, dump, password.expose_secret()) {
            Ok(restored) => restored,
            Err(e) => {
                self.error(format!("Restore failed: {e}"));
                return;
            }
        };
        // A backup made before the check was part of the metadata.
        let checked = repo.password_check().and_then(|check| match check {
            Some(_) => Ok(()),
            None => repo.set_password(password.expose_secret()),
        });
        if let Err(e) = checked {
            log::warn!("cannot record the master password: {e}");
        }
        {
            // Nothing this machine knew about a folder at that path holds.
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&repo.root);
            *repo_state = RepoState::default();
            repo_state.keep_password(password.expose_secret());
        }
        self.open_repo(repo);
        let mut text = format!("Restored {} file(s)", report.files);
        if report.files > 0 {
            text += "; download them from the Remotes tab";
        }
        if !report.kept.is_empty() {
            text += &format!("; {} already there kept as new versions", report.kept.len());
        }
        if !report.skipped.is_empty() {
            text += &format!("; {} skipped", report.skipped.len());
        }
        self.success(text);
    }

    /// Back to choosing how to start in the pending folder.
    fn cancel_restore(&self) {
        if self
            .setup
            .borrow()
            .as_ref()
            .is_some_and(|p| p.throwaway_config().is_some())
        {
            self.cancel_remote_setup();
        }
        let w = &self.window;
        w.set_restore_mode(0);
        w.set_restore_backup_file(SharedString::default());
        w.set_pw_restore(SharedString::default());
    }

    /// Start a scan: ingest on a background thread, then record the result
    /// on the UI thread (see [`Msg::Scanned`]).
    fn rescan(&self) {
        let Some(root) = self.repo.borrow().as_ref().map(|r| r.root.clone()) else {
            self.error("Cannot scan: no folder open");
            return;
        };
        if self.scanning.replace(true) {
            self.warn("A scan is already running");
            return;
        }
        self.status("Scanning…");
        self.spawn(move |tx| {
            let result = repo::ingest_tree(&root);
            tx.send(Msg::Scanned { root, result }).ok();
        });
    }

    // ── blob selection ──

    /// Paths of the listed rows, in display order.
    fn row_paths(&self) -> Vec<String> {
        self.statuses
            .borrow()
            .iter()
            .map(|s| s.path.clone())
            .collect()
    }

    /// Distinct hashes of the selected rows, in display order.
    fn selected_hashes(&self) -> Vec<String> {
        let selection = self.selection.borrow();
        let mut seen = HashSet::new();
        self.statuses
            .borrow()
            .iter()
            .filter(|s| selection.paths.contains(&s.path))
            .filter(|s| seen.insert(s.hash.clone()))
            .map(|s| s.hash.clone())
            .collect()
    }

    /// Push the selection into the row models and the sidebar summary.
    fn show_selection(&self) {
        let selection = self.selection.borrow();
        for i in 0..self.files.row_count() {
            let Some(mut row) = self.files.row_data(i) else {
                continue;
            };
            let selected = selection.paths.contains(row.path.as_str());
            if row.selected != selected {
                row.selected = selected;
                self.files.set_row_data(i, row);
            }
        }

        let statuses = self.statuses.borrow();
        let chosen: Vec<(usize, &repo::FileStatus)> = statuses
            .iter()
            .enumerate()
            .filter(|(_, s)| selection.paths.contains(&s.path))
            .collect();
        let mut seen = HashSet::new();
        let blobs: Vec<&repo::FileStatus> = chosen
            .iter()
            .map(|(_, s)| *s)
            .filter(|s| seen.insert(s.hash.as_str()))
            .collect();
        let local = blobs.iter().filter(|s| s.present_locally).count();
        let size: u64 = blobs.iter().map(|s| s.size).sum();
        let unstored = blobs.iter().filter(|s| s.remotes.is_empty()).count();

        self.window.set_selected_count(chosen.len() as i32);
        self.window.set_detail_index(match chosen[..] {
            [(i, _)] => i as i32,
            _ => -1,
        });
        self.window.set_selection_local(local as i32);
        self.window
            .set_selection_missing((blobs.len() - local) as i32);
        let mut summary = format!(
            "{} file(s), {} distinct blob(s), {} total\n{local} present locally, {} missing locally",
            chosen.len(),
            blobs.len(),
            util::format_size(size),
            blobs.len() - local,
        );
        if unstored > 0 {
            summary += &format!("\n{unstored} not stored on any remote");
        }
        self.window.set_selection_summary(summary.into());
        drop(statuses);
        drop(selection);
        self.show_detail_preview();
    }

    // ── previews ──

    /// Make the thumbnails the file list lacks, in the background.
    fn make_thumbs(&self) {
        let Some(root) = self.root() else { return };
        let wanted: Vec<String> = {
            let thumbs = self.thumbs.borrow();
            let none = self.no_thumb.borrow();
            let pending = self.thumbs_pending.borrow();
            let mut seen = HashSet::new();
            self.statuses
                .borrow()
                .iter()
                .map(|s| s.hash.clone())
                .filter(|h| {
                    !thumbs.contains_key(h)
                        && !none.contains(h)
                        && !pending.contains(h)
                        && seen.insert(h.clone())
                })
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        self.thumbs_pending
            .borrow_mut()
            .extend(wanted.iter().cloned());
        // Not a task: the buttons stay usable while previews come in.
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for hash in wanted {
                let outcome = thumbs::thumbnail(&root, &hash);
                let sent = tx.send(Msg::Thumb {
                    root: root.clone(),
                    hash,
                    outcome,
                });
                if sent.is_err() {
                    break;
                }
            }
        });
    }

    fn thumb_made(&self, root: &Path, hash: String, outcome: thumbs::Outcome) {
        self.thumbs_pending.borrow_mut().remove(&hash);
        if self.root().as_deref() != Some(root) {
            return;
        }
        let image = match outcome {
            thumbs::Outcome::Ready(preview) => to_image(&preview),
            _ => {
                self.no_thumb.borrow_mut().insert(hash);
                return;
            }
        };
        for i in 0..self.files.row_count() {
            if let Some(mut row) = self.files.row_data(i)
                && row.hash.as_str() == hash
            {
                row.thumb = image.clone();
                row.has_thumb = true;
                self.files.set_row_data(i, row);
            }
        }
        let detail = self.previewed.borrow().as_deref() == Some(hash.as_str());
        if detail && !self.window.get_has_detail_preview() {
            self.window.set_detail_preview(image.clone());
            self.window.set_has_detail_preview(true);
        }
        self.thumbs.borrow_mut().insert(hash, image);
    }

    /// Show a large preview of the one file selected, if it is an image
    /// (its thumbnail at once, then the image itself once decoded) or
    /// text (its first lines).
    fn show_detail_preview(&self) {
        let hash = match self.window.get_detail_index() {
            i if i >= 0 => self
                .statuses
                .borrow()
                .get(i as usize)
                .map(|s| (s.hash.clone(), s.present_locally)),
            _ => None,
        };
        if self.previewed.borrow().as_ref() == hash.as_ref().map(|(h, _)| h) {
            return;
        }
        *self.previewed.borrow_mut() = hash.as_ref().map(|(h, _)| h.clone());
        let thumb = hash
            .as_ref()
            .and_then(|(h, _)| self.thumbs.borrow().get(h).cloned());
        self.window.set_has_detail_preview(thumb.is_some());
        self.window.set_detail_preview(thumb.unwrap_or_default());
        self.window.set_has_detail_text(false);
        self.window.set_detail_text(Default::default());
        let (Some((hash, true)), Some(root)) = (hash, self.root()) else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = thumbs::preview(&root, &hash, PREVIEW_SIZE);
            tx.send(Msg::Preview { hash, outcome }).ok();
        });
    }

    fn preview_made(&self, hash: String, outcome: thumbs::Outcome) {
        if self.previewed.borrow().as_deref() != Some(hash.as_str()) {
            return;
        }
        match outcome {
            thumbs::Outcome::Ready(preview) => {
                self.window.set_detail_preview(to_image(&preview));
                self.window.set_has_detail_preview(true);
            }
            thumbs::Outcome::Text(text) => {
                self.window.set_detail_text(text.into());
                self.window.set_has_detail_text(true);
            }
            _ => {}
        }
    }

    /// Open the file listed at `index`: with the app the system chooses
    /// on the desktop; images in the app's own viewer on Android, where
    /// other apps can't read the folder.
    fn open_file(&self, index: usize) {
        let Some(status) = self.statuses.borrow().get(index).cloned() else {
            return;
        };
        if !status.present_locally {
            self.warn(format!(
                "{} isn't on this device: download it first",
                status.path
            ));
            return;
        }
        #[cfg(target_os = "android")]
        if self.window.get_detail_index() == index as i32 && self.window.get_has_detail_preview() {
            self.window.set_viewer_open(true);
        } else {
            self.warn("Only images can be opened on Android for now");
        }
        #[cfg(not(target_os = "android"))]
        if let Some(root) = self.root()
            && let Err(e) = open::that_detached(root.join(&status.path))
        {
            self.error(format!("Cannot open {}: {e}", status.path));
        }
    }

    fn upload_selected(&self, remote: &str) {
        let hashes = self.selected_hashes();
        let (present, absent): (Vec<String>, Vec<String>) = {
            let repo = self.repo.borrow();
            let Some(repo) = repo.as_ref() else { return };
            hashes.into_iter().partition(|h| repo.blob_present(h))
        };
        if present.is_empty() {
            self.warn("None of the selected blobs is present locally; nothing to upload");
            return;
        }
        if !absent.is_empty() {
            self.warn(format!(
                "Skipping {} blob(s) not present locally",
                absent.len()
            ));
        }
        self.push_blobs(remote, present);
    }

    fn check_selected(&self, remote_name: &str) {
        let hashes = self.selected_hashes();
        let (Some(remote), Some(config_path)) = (self.remote(remote_name), self.config_path())
        else {
            return;
        };
        self.status(format!(
            "Checking {} blob(s) on '{remote_name}'…",
            hashes.len()
        ));
        self.spawn(move |tx| {
            let mut stored = 0;
            let mut absent = 0;
            let mut failed = None;
            let configured = mbr::rclone::with_config(&config_path, || {
                for hash in hashes {
                    let result = mbr::rclone::stat_blob(&remote.target, &hash);
                    match &result {
                        Ok(Some(_)) => stored += 1,
                        Ok(None) => absent += 1,
                        Err(e) => {
                            failed.get_or_insert_with(|| e.clone());
                        }
                    }
                    tx.send(Msg::Checked {
                        remote: remote.name.clone(),
                        hash,
                        result,
                    })
                    .ok();
                }
                Ok(())
            });
            let mut text = format!("'{}': {stored} stored, {absent} not stored", remote.name);
            let mut level = Level::Success;
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    /// Download the selected blobs missing locally, trying `preferred`
    /// first and then any other remote recorded as holding each.
    fn download_selected(&self, preferred: &str) {
        let hashes = self.selected_hashes();
        let jobs = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let missing: Vec<String> = hashes
                .into_iter()
                .filter(|h| !repo.blob_present(h))
                .collect();
            let mut jobs = Vec::new();
            for hash in missing {
                let mut names = repo.db.remotes_for_blob(&hash).unwrap_or_default();
                names.sort_by_key(|n| n != preferred);
                let holders: Vec<Remote> = names
                    .iter()
                    .filter_map(|n| repo.db.remote(n).ok().flatten())
                    .collect();
                jobs.push((hash, holders));
            }
            jobs
        };
        if jobs.is_empty() {
            self.status("Every selected blob is already present locally");
            return;
        }
        self.fetch_blobs(jobs);
    }

    // ── keeping copies safe ──

    /// Drop the local copies of `hashes` that enough remotes hold.
    fn drop_blobs(&self, hashes: Vec<String>) {
        let (jobs, min_copies, root) = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let min_copies = match repo.min_copies() {
                Ok(n) => n,
                Err(e) => {
                    self.error(format!("Error: {e}"));
                    return;
                }
            };
            let jobs: Vec<(String, Vec<Remote>, u64)> = hashes
                .into_iter()
                .filter_map(|h| {
                    let size = std::fs::metadata(repo.blob_path(&h)).ok()?.len();
                    Some((h.clone(), repo.holders(&h).unwrap_or_default(), size))
                })
                .collect();
            (jobs, min_copies, repo.root.clone())
        };
        let Some(config_path) = self.config_path() else {
            return;
        };
        if jobs.is_empty() {
            self.warn("None of these blobs is present locally; nothing to free");
            return;
        }
        self.status(format!(
            "Checking the remotes of {} blob(s) before freeing space…",
            jobs.len()
        ));
        self.spawn(move |tx| {
            let total = jobs.len();
            let (mut dropped, mut freed) = (0, 0);
            let configured = mbr::rclone::with_config(&config_path, || {
                for (hash, holders, size) in jobs {
                    let result = repo::drop_local_blob(&root, &hash, &holders, min_copies);
                    if result.as_ref().is_ok_and(|o| o.dropped) {
                        dropped += 1;
                        freed += size;
                    }
                    tx.send(Msg::Dropped { size, result }).ok();
                }
                Ok(())
            });
            let mut text = format!("Freed {} ({dropped} blob(s))", util::format_size(freed));
            let mut level = Level::Success;
            if dropped < total {
                text += &format!(
                    "; kept {} that fewer than {min_copies} remote(s) are confirmed to hold",
                    total - dropped
                );
                level = Level::Warning;
            }
            if let Err(e) = configured {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    /// Re-hash the local copies of `hashes`.
    fn verify_local(&self, hashes: Vec<String>) {
        let Some(root) = self.repo.borrow().as_ref().map(|r| r.root.clone()) else {
            return;
        };
        if hashes.is_empty() {
            self.warn("Nothing stored here to verify");
            return;
        }
        self.status(format!("Verifying {} local blob(s)…", hashes.len()));
        self.spawn(move |tx| {
            let total = hashes.len();
            let mut damaged = 0;
            for hash in hashes {
                let result = repo::verify_local_blob(&root, &hash);
                if matches!(result, Ok(LocalHealth::Corrupt { .. })) {
                    damaged += 1;
                }
                tx.send(Msg::VerifiedLocal { hash, result }).ok();
            }
            tx.send(if damaged == 0 {
                Msg::Status(Level::Success, format!("Verified {total} blob(s): all intact"))
            } else {
                Msg::Status(Level::Error, format!(
                    "Verified {total} blob(s): {damaged} DAMAGED, set aside in {} — download them again",
                    repo::CORRUPT_DIR
                ))
            })
            .ok();
        });
    }

    /// Download the copies of `hashes` on `remote_name` and check them.
    fn verify_on_remote(&self, remote_name: &str, hashes: Vec<String>) {
        let (Some(remote), Some(config_path)) = (self.remote(remote_name), self.config_path())
        else {
            return;
        };
        let Some(root) = self.repo.borrow().as_ref().map(|r| r.root.clone()) else {
            return;
        };
        self.status(format!(
            "Downloading {} blob(s) from '{remote_name}' to verify them…",
            hashes.len()
        ));
        self.spawn(move |tx| {
            let (mut ok, mut missing, mut corrupt) = (0, 0, 0);
            let mut failed = None;
            let configured = mbr::rclone::with_config(&config_path, || {
                for hash in hashes {
                    let result = repo::verify_remote_blob(&root, &remote.target, &hash);
                    match &result {
                        Ok(RemoteHealth::Ok) => ok += 1,
                        Ok(RemoteHealth::Missing) => missing += 1,
                        Ok(RemoteHealth::Corrupt) => corrupt += 1,
                        Err(e) => {
                            failed.get_or_insert_with(|| e.clone());
                        }
                    }
                    tx.send(Msg::VerifiedRemote {
                        remote: remote.name.clone(),
                        hash,
                        result,
                    })
                    .ok();
                }
                Ok(())
            });
            let mut text = format!("'{}': {ok} intact, {missing} missing", remote.name);
            let mut level = Level::Success;
            if missing > 0 {
                level = Level::Warning;
            }
            if corrupt > 0 {
                text += &format!(", {corrupt} DAMAGED (deleted there; upload them again)");
                level = Level::Error;
            }
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    // ── metadata backup ──

    fn save_state(&self) {
        if let Err(e) = self.state.borrow().save() {
            self.error(format!("Could not save app state: {e}"));
        }
    }

    fn root(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(|r| r.root.clone())
    }

    /// Set the folder's master password once (for every device).
    fn set_password(&self, password: &str, repeat: &str) {
        let Some(root) = self.root() else { return };
        if let Err(e) = metadata::check_new_password(password) {
            self.error(format!("Error: {e}"));
            return;
        }
        if password != repeat {
            self.error("Error: the passwords differ");
            return;
        }
        {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            if let Err(e) = repo.set_password(password) {
                self.error(format!("Error: {e}"));
                return;
            }
        }
        {
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&root);
            repo_state.keep_password(password);
            // Backups made so far are encrypted with the old password.
            repo_state.metadata_backups.clear();
        }
        self.save_state();
        *self.password.borrow_mut() = Some(SecretString::from(password.to_owned()));
        self.window.set_pw_new(SharedString::default());
        self.window.set_pw_repeat(SharedString::default());
        self.success("Master password set and saved on this computer");
        self.refresh();
    }

    fn test_password(&self, password: &str) {
        let verified = match self.repo.borrow_mut().as_mut() {
            Some(repo) => repo.verify_password(password),
            None => return,
        };
        if let Err(e) = verified {
            self.error(format!("Error: {e}"));
            return;
        }
        self.window.set_testing_password(false);
        self.window.set_test_password_entry(SharedString::default());
        self.success("Master password is correct");
        self.refresh();
    }

    /// Check the master password typed on this computer (set on another
    /// device), and save it here. Remotes synced from another device get
    /// their rclone settings now.
    fn unlock(&self, password: &str) {
        let Some(root) = self.root() else { return };
        let ready = match self.repo.borrow_mut().as_mut() {
            Some(repo) => repo.verify_password(password).and_then(|()| {
                if repo.rclone_config_incomplete()? {
                    repo.write_rclone_config(Some(password))?;
                }
                Ok(())
            }),
            None => return,
        };
        if let Err(e) = ready {
            self.error(format!("Error: {e}"));
            return;
        }
        self.state
            .borrow_mut()
            .repo_mut(&root)
            .keep_password(password);
        self.save_state();
        *self.password.borrow_mut() = Some(SecretString::from(password.to_owned()));
        self.window.set_unlock_entry(SharedString::default());
        self.success("Master password is correct");
        self.refresh();
    }

    // ── peers ──

    /// Listen for devices on the local network while the Peers tab is
    /// shown, even with no peers yet, and stop again when it is left (a
    /// folder with peers keeps syncing regardless).
    fn peers_visible(&self, visible: bool) {
        if visible {
            if self.node.borrow().is_none() {
                self.start_node();
            } else {
                self.refresh_peers();
            }
            return;
        }
        let has_peers = self
            .root()
            .is_some_and(|root| !self.state.borrow().repo(&root).peers.is_empty());
        if !has_peers {
            *self.node.borrow_mut() = None;
            self.nearby.borrow_mut().clear();
        }
        self.refresh_peers();
    }

    /// Start syncing the open folder with its peers (and, while the Peers
    /// tab is open, listening for devices on the local network).
    fn start_node(&self) {
        *self.node.borrow_mut() = None;
        let Some(root) = self.root() else { return };
        let (key, ids) = {
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&root);
            let key = peers::node_key(repo_state);
            (key, repo_state.peers.keys().cloned().collect::<Vec<_>>())
        };
        self.save_state();
        // No network activity for folders that aren't synced, unless the
        // Peers tab is open: then listen for devices to suggest on the
        // local network.
        if ids.is_empty() && self.window.get_active_tab() != 3 {
            return;
        }
        let Some(shared) = self.repo.borrow().as_ref().map(|r| r.db.shared()) else {
            return;
        };
        let tx = self.tx.clone();
        match Node::start(key, shared, &ids, move |event| {
            tx.send(Msg::Peer(event)).ok();
        }) {
            Ok(node) => {
                *self.node.borrow_mut() = Some(node);
                self.sync_peers();
            }
            Err(e) => self.error(format!("Cannot sync with the peers: {e}")),
        }
    }

    /// Sync with every peer, in the background.
    fn sync_peers(&self) {
        let Some(root) = self.root() else { return };
        {
            let node = self.node.borrow();
            let Some(node) = node.as_ref() else { return };
            for id in self.state.borrow().repo(&root).peers.keys() {
                node.sync(id);
                self.peer_status
                    .borrow_mut()
                    .entry(id.clone())
                    .or_insert_with(|| ("syncing…".to_owned(), true));
            }
        }
        self.refresh_peers();
    }

    fn add_peer(&self, id: &str, name: &str) {
        let Some(root) = self.root() else { return };
        let id = match peers::check_device_id(id) {
            Ok(id) => id,
            Err(e) => {
                self.error(format!("Error: {e}"));
                return;
            }
        };
        let name = match name.trim() {
            "" => format!("device {}", &id[..8]),
            name => name.to_owned(),
        };
        {
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&root);
            if peers::device_id(&peers::node_key(repo_state)) == id {
                drop(state);
                self.error("Error: that is this computer's own id");
                return;
            }
            repo_state
                .peers
                .insert(id.clone(), Peer { name: name.clone() });
        }
        self.save_state();
        self.knocking.borrow_mut().remove(&id);
        self.window.set_peer_new_id(SharedString::default());
        self.window.set_peer_new_name(SharedString::default());
        self.success(format!(
            "Added {name}: syncing starts once it adds this computer too"
        ));
        let ids: Vec<String> = self
            .state
            .borrow()
            .repo(&root)
            .peers
            .keys()
            .cloned()
            .collect();
        if let Some(node) = self.node.borrow().as_ref() {
            node.set_peers(&ids);
        }
        if self.node.borrow().is_some() {
            self.sync_peers();
        } else {
            self.start_node();
        }
        self.refresh_peers();
    }

    fn remove_peer(&self, id: &str) {
        let Some(root) = self.root() else { return };
        let removed = self.state.borrow_mut().repo_mut(&root).peers.remove(id);
        self.save_state();
        self.peer_status.borrow_mut().remove(id);
        let ids: Vec<String> = self
            .state
            .borrow()
            .repo(&root)
            .peers
            .keys()
            .cloned()
            .collect();
        if ids.is_empty() && self.window.get_active_tab() != 3 {
            *self.node.borrow_mut() = None;
        } else if let Some(node) = self.node.borrow().as_ref() {
            node.set_peers(&ids);
        }
        if let Some(peer) = removed {
            self.success(format!("No longer syncing with {}", peer.name));
        }
        self.refresh_peers();
    }

    fn peer_event(&self, event: peers::Event) {
        match event {
            peers::Event::Synced { peer, report } => {
                if report.joined {
                    // The folder is now another device's: what this one
                    // knew of its own is moot, the password included.
                    if let Some(root) = self.root() {
                        self.state.borrow_mut().repo_mut(&root).forget_folder();
                        self.save_state();
                    }
                    *self.password.borrow_mut() = None;
                    self.warn(
                        "This folder joined the peer's; type its master password in Settings",
                    );
                }
                let text = format!("synced {}", util::format_time(util::now()));
                self.peer_status.borrow_mut().insert(peer, (text, true));
                if report.received > 0 || report.joined {
                    if self.scanning.get() {
                        self.sync_pending.set(true);
                    } else {
                        self.apply_synced();
                    }
                }
            }
            peers::Event::Failed { peer, error } => {
                self.peer_status.borrow_mut().insert(peer, (error, false));
            }
            peers::Event::Refused { peer } => {
                let known = self
                    .root()
                    .is_some_and(|root| self.state.borrow().repo(&root).peers.contains_key(&peer));
                if !known {
                    self.knocking.borrow_mut().insert(peer);
                }
            }
            peers::Event::Discovered { peer, name } => {
                let changed = match self.nearby.borrow_mut().insert(peer, name.clone()) {
                    None => true,
                    Some(previous) => previous != name,
                };
                if changed {
                    self.refresh_peers();
                }
            }
            peers::Event::Gone { peer } => {
                if self.nearby.borrow_mut().remove(&peer).is_some() {
                    self.refresh_peers();
                }
            }
        }
    }

    /// Bring the folder in line with what syncs brought in.
    fn apply_synced(&self) {
        self.sync_pending.set(false);
        let password = self.password.borrow().clone();
        self.with_repo("apply the sync", |repo| {
            let applied = repo.apply_synced()?;
            let mut text = format!("Synced: {} file(s) updated", applied.updated.len());
            if !applied.kept.is_empty() {
                text += &format!(
                    "; {} changed here kept for the next scan",
                    applied.kept.len()
                );
            }
            if !applied.skipped.is_empty() {
                text += &format!("; {} skipped", applied.skipped.len());
            }
            if applied.remotes_changed {
                let password = password.as_ref().map(|p| p.expose_secret());
                if repo.write_rclone_config(password).is_err() {
                    text += "; type the master password in Settings to use the synced remotes";
                }
            }
            Ok(text)
        });
    }

    /// Rebuild the Peers tab.
    fn refresh_peers(&self) {
        let w = &self.window;
        let Some(root) = self.root() else {
            w.set_device_id(SharedString::default());
            w.set_peer_rows(ModelRc::default());
            w.set_nearby_peers(ModelRc::default());
            w.set_discovering(false);
            w.set_peer_count(0);
            return;
        };
        let (key, known) = {
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&root);
            (peers::node_key(repo_state), repo_state.peers.clone())
        };
        w.set_device_id(peers::device_id(&key).into());
        let status = self.peer_status.borrow();
        let mut rows: Vec<PeerRow> = self
            .knocking
            .borrow()
            .iter()
            .filter(|id| !known.contains_key(*id))
            .map(|id| PeerRow {
                id: id.as_str().into(),
                name: SharedString::default(),
                status: SharedString::default(),
                ok: false,
                knocking: true,
            })
            .collect();
        rows.extend(known.iter().map(|(id, peer)| {
            let (text, ok) = status
                .get(id)
                .cloned()
                .unwrap_or_else(|| ("not synced yet".to_owned(), true));
            PeerRow {
                id: id.as_str().into(),
                name: peer.name.as_str().into(),
                status: text.into(),
                ok,
                knocking: false,
            }
        }));
        w.set_peer_count(known.len() as i32);
        w.set_peer_rows(ModelRc::new(VecModel::from(rows)));

        let self_id = peers::device_id(&key);
        let knocking = self.knocking.borrow();
        let nearby: Vec<NearbyPeer> = self
            .nearby
            .borrow()
            .iter()
            .filter(|(id, _)| {
                **id != self_id && !known.contains_key(*id) && !knocking.contains(*id)
            })
            .map(|(id, name)| NearbyPeer {
                id: id.as_str().into(),
                name: name.as_str().into(),
            })
            .collect();
        w.set_nearby_peers(ModelRc::new(VecModel::from(nearby)));
        w.set_discovering(self.node.borrow().is_some() && w.get_active_tab() == 3);
    }

    /// Snapshot the metadata here, then encrypt and upload it to every
    /// chosen remote in the background.
    fn backup_metadata(&self) {
        let Some(password) = self.password.borrow().clone() else {
            self.warn("Set the master password in the Settings tab first");
            return;
        };
        let (remotes, snapshot, root, scratch) = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let prepared = repo
                .db
                .metadata_remotes()
                .and_then(|remotes| Ok((remotes, repo.metadata_snapshot()?)));
            match prepared {
                Ok((remotes, snapshot)) => {
                    (remotes, snapshot, repo.root.clone(), repo.scratch_dir())
                }
                Err(e) => {
                    self.error(format!("Error: {e}"));
                    return;
                }
            }
        };
        let Some(config_path) = self.config_path() else {
            return;
        };
        if remotes.is_empty() {
            self.warn("Tick at least one remote to back up to");
            return;
        }
        self.status("Encrypting metadata…");
        self.spawn(move |tx| {
            let sealed = metadata::seal(&snapshot, password.expose_secret())
                .and_then(|sealed| Ok((sealed, metadata::backup_name(password.expose_secret())?)));
            let (sealed, name) = match sealed {
                Ok(sealed) => sealed,
                Err(e) => {
                    tx.send(Msg::Status(
                        Level::Error,
                        format!("Metadata backup failed: {e}"),
                    ))
                    .ok();
                    return;
                }
            };
            let total = remotes.len();
            let mut done = 0;
            let configured = mbr::rclone::with_config(&config_path, || {
                for remote in remotes {
                    let result = mbr::remotes::backup_location(&remote.target)
                        .and_then(|location| metadata::upload(&location, &name, &sealed, &scratch));
                    if result.is_ok() {
                        done += 1;
                    }
                    tx.send(Msg::MetadataUploaded {
                        root: root.clone(),
                        remote: remote.name,
                        db_hash: snapshot.db_hash.clone(),
                        result,
                    })
                    .ok();
                }
                Ok(())
            });
            let mut text = format!("Metadata backed up to {done} of {total} remote(s)");
            let mut level = Level::Success;
            if let Err(e) = configured {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    // ── background tasks ──

    fn begin_task(&self) {
        self.tasks.set(self.tasks.get() + 1);
        self.window.set_busy(true);
    }

    /// Run `work` on a background thread as one task; [`Msg::Done`] is sent
    /// when it returns.
    fn spawn(&self, work: impl FnOnce(&mpsc::Sender<Msg>) + Send + 'static) {
        self.begin_task();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            work(&tx);
            tx.send(Msg::Done).ok();
        });
    }

    fn push_blobs(&self, remote_name: &str, hashes: Vec<String>) {
        let (Some(remote), Some(config_path)) = (self.remote(remote_name), self.config_path())
        else {
            return;
        };
        let blobs: Vec<(String, PathBuf)> = {
            let repo = self.repo.borrow();
            let Some(repo) = repo.as_ref() else { return };
            hashes
                .iter()
                .map(|h| (h.clone(), repo.blob_path(h)))
                .collect()
        };

        self.status(format!(
            "Uploading {} blob(s) to {remote_name}…",
            blobs.len()
        ));
        self.spawn(move |tx| {
            let total = blobs.len();
            let mut done = 0;
            let mut failed = None;
            let configured = mbr::rclone::with_config(&config_path, || {
                for (hash, blob) in blobs {
                    let result = mbr::rclone::push_blob(&remote.target, &blob, &hash);
                    match &result {
                        Ok(()) => done += 1,
                        Err(e) => {
                            failed.get_or_insert_with(|| e.clone());
                        }
                    }
                    tx.send(Msg::Pushed {
                        remote: remote.name.clone(),
                        hash,
                        result,
                    })
                    .ok();
                }
                Ok(())
            });
            let mut text = format!("Uploaded {done} of {total} blob(s) to {}", remote.name);
            let mut level = Level::Success;
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    /// Download each `(hash, holders)` job, trying its holders in order.
    fn fetch_blobs(&self, jobs: Vec<(String, Vec<Remote>)>) {
        let (Some(config_path), Some(root)) = (
            self.config_path(),
            self.repo.borrow().as_ref().map(|r| r.root.clone()),
        ) else {
            return;
        };
        let unheld = jobs
            .iter()
            .filter(|(_, holders)| holders.is_empty())
            .count();
        let jobs: Vec<(String, Vec<Remote>)> = jobs
            .into_iter()
            .filter(|(_, holders)| !holders.is_empty())
            .collect();
        if jobs.is_empty() {
            self.warn("None of these blobs is recorded on any remote; nothing to download");
            return;
        }

        self.status(format!("Downloading {} blob(s)…", jobs.len()));
        self.spawn(move |tx| {
            let total = jobs.len();
            let mut done = 0;
            let mut failed = None;
            let configured = mbr::rclone::with_config(&config_path, || {
                for (hash, holders) in jobs {
                    let blob = repo::blob_path_in(&root, &hash);
                    let result = repo::download_blob(&holders, &hash, &blob);
                    match &result {
                        Ok(_) => done += 1,
                        Err(e) => {
                            failed.get_or_insert_with(|| e.clone());
                        }
                    }
                    tx.send(Msg::Fetched { hash, result }).ok();
                }
                Ok(())
            });
            let mut text = format!("Downloaded {done} of {total} blob(s)");
            let mut level = Level::Success;
            if unheld > 0 {
                text += &format!("; {unheld} not recorded on any remote");
                level = Level::Warning;
            }
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
                level = Level::Error;
            }
            tx.send(Msg::Status(level, text)).ok();
        });
    }

    /// Download everything recorded on remote `name` that is missing here.
    fn download_all(&self, name: &str) {
        let Some(remote) = self.remote(name) else {
            return;
        };
        let missing = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            match repo.blobs_missing_locally(name) {
                Ok(missing) => missing,
                Err(e) => {
                    self.error(format!("Error: {e}"));
                    return;
                }
            }
        };
        if missing.is_empty() {
            self.status(format!(
                "Every blob recorded on '{name}' is already here (scan it to find more)"
            ));
            return;
        }
        self.fetch_blobs(
            missing
                .into_iter()
                .map(|h| (h, vec![remote.clone()]))
                .collect(),
        );
    }

    /// List one remote (or all of them) in the background; each listing is
    /// reconciled with the database as it arrives (see [`Msg::Listed`]).
    fn scan_remotes(&self, only: Option<&str>) {
        let remotes = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            match repo.db.list_remotes() {
                Ok(remotes) => remotes,
                Err(e) => {
                    self.error(format!("Error: {e}"));
                    return;
                }
            }
        };
        let remotes: Vec<Remote> = remotes
            .into_iter()
            .filter(|r| only.is_none_or(|name| r.name == name))
            .collect();
        let Some(config_path) = self.config_path() else {
            return;
        };
        if remotes.is_empty() {
            self.warn("No remotes to scan");
            return;
        }

        self.status(match only {
            Some(name) => format!("Scanning remote '{name}'…"),
            None => format!("Scanning {} remote(s)…", remotes.len()),
        });
        self.spawn(move |tx| {
            for remote in remotes {
                let result = mbr::rclone::with_config(&config_path, || {
                    mbr::rclone::list_blobs(&remote.target)
                });
                tx.send(Msg::Listed {
                    remote: remote.name,
                    result,
                })
                .ok();
            }
        });
    }

    /// Forget a remote, then delete the rclone settings mbr made for it in
    /// the background. What it stores stays where it is.
    fn remove_remote(&self, name: &str) {
        let (Some(remote), Some(config_path)) = (self.remote(name), self.config_path()) else {
            return;
        };
        let mut removed = false;
        self.with_repo("remove remote", |repo| {
            repo.db.remove_remote(name)?;
            removed = true;
            Ok(format!("Removed remote {name}"))
        });
        if self.window.get_edit_original() == name {
            self.close_editor();
        }
        self.refresh();
        if removed {
            self.spawn(move |tx| {
                let deleted = mbr::rclone::with_config(&config_path, || {
                    mbr::remotes::delete_sections(&remote.target)
                });
                if let Err(e) = deleted {
                    let text = format!(
                        "Removed remote {}; its rclone settings stay: {e}",
                        remote.name
                    );
                    tx.send(Msg::Status(Level::Warning, text)).ok();
                }
            });
        }
    }

    // ── editing a remote ──

    /// Open the editor for remote `name` and read its rclone settings.
    fn edit_remote(&self, name: &str) {
        let Some(remote) = self.remote(name) else {
            return;
        };
        let section = mbr::remotes::settings_section(&remote.target);
        *self.edit.borrow_mut() = Some(EditState {
            original: remote.name.clone(),
            section: section.clone(),
            backend: None,
            fields: Vec::new(),
            initial: Vec::new(),
        });
        let w = &self.window;
        w.set_edit_original(remote.name.as_str().into());
        w.set_edit_name(remote.name.as_str().into());
        w.set_edit_description(mbr::remotes::describe(&remote.target).into());
        w.set_edit_fields(ModelRc::default());
        w.set_edit_values(ModelRc::default());
        w.set_editing(true);

        let (Some(section), Some(config_path)) = (section, self.config_path()) else {
            w.set_edit_note("This remote has no rclone settings.".into());
            return;
        };
        w.set_edit_note("Reading rclone settings…".into());
        self.spawn(move |tx| {
            let result =
                mbr::rclone::with_config(&config_path, || mbr::rclone::remote_config(&section));
            tx.send(Msg::RemoteConfig {
                remote: remote.name,
                result,
            })
            .ok();
        });
    }

    /// Show the settings read for the remote being edited.
    fn show_remote_config(&self, config: Option<serde_json::Map<String, serde_json::Value>>) {
        let mut edit = self.edit.borrow_mut();
        let Some(edit) = edit.as_mut() else { return };
        let w = &self.window;
        let Some(config) = config else {
            w.set_edit_note(
                "Its settings are missing from this folder's rclone.conf; only the name can \
                 be changed."
                    .into(),
            );
            return;
        };
        let kind = config
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let Some(backend) = backends::backend(kind) else {
            w.set_edit_note(
                format!(
                    "It uses the '{kind}' backend, which has no form here; change its \
                     settings with `mbr remote edit --set`."
                )
                .into(),
            );
            return;
        };

        let fields: Vec<&'static Field> = backend.fields.iter().filter(|f| !f.to_target).collect();
        let current: Vec<Option<String>> = fields
            .iter()
            .map(|f| config.get(f.name).map(mbr::rclone::display_value_for_ui))
            .collect();
        // Secrets come back obscured: show them blank, meaning "unchanged".
        let initial: Vec<String> = fields
            .iter()
            .zip(&current)
            .map(|(f, value)| match (f.kind, value) {
                (Kind::Secret, _) => String::new(),
                (_, Some(value)) => value.clone(),
                (Kind::Choice, None) => f.default.to_owned(),
                (Kind::Text, None) => String::new(),
            })
            .collect();
        let rows: Vec<FormField> = fields
            .iter()
            .zip(&initial)
            .zip(&current)
            .map(|((f, value), stored)| {
                let placeholder = match (f.kind, stored) {
                    (Kind::Secret, Some(_)) => "(set; type to replace)",
                    (Kind::Secret, None) => "(not set)",
                    _ => f.default,
                };
                form_field(backend, f, value, placeholder)
            })
            .collect();
        w.set_edit_note(format!("{} settings.", backend.title).into());
        w.set_edit_fields(ModelRc::new(VecModel::from(rows)));
        w.set_edit_values(ModelRc::new(VecModel::from(
            initial
                .iter()
                .map(|v| SharedString::from(v.as_str()))
                .collect::<Vec<_>>(),
        )));
        edit.backend = Some(backend);
        edit.fields = fields;
        edit.initial = initial;
    }

    fn save_remote_edit(&self) {
        let w = &self.window;
        let name = w.get_edit_name().trim().to_owned();
        let renamed = self
            .edit
            .borrow()
            .as_ref()
            .is_some_and(|e| e.original != name);
        if renamed && let Err(e) = mbr::remotes::check_name(&name) {
            self.error(format!("Error: {e}"));
            return;
        }
        let values: Vec<SharedString> = w.get_edit_values().iter().collect();
        let (old, section, parameters) = {
            let edit = self.edit.borrow();
            let Some(edit) = edit.as_ref() else { return };
            // Send only what changed; empty answers can't be told apart
            // from "leave it", so they are never sent.
            let mut parameters = serde_json::Map::new();
            for ((field, value), initial) in edit.fields.iter().zip(&values).zip(&edit.initial) {
                let value = value.trim();
                if !value.is_empty() && value != initial.trim() {
                    parameters.insert(field.name.to_owned(), value.into());
                }
            }
            (edit.original.clone(), edit.section.clone(), parameters)
        };

        match section {
            Some(section) if !parameters.is_empty() => {
                let Some(config_path) = self.config_path() else {
                    return;
                };
                self.status(format!("Saving the settings of '{old}'…"));
                self.spawn(move |tx| {
                    let result = mbr::rclone::with_config(&config_path, || {
                        mbr::rclone::update_remote(&section, serde_json::Value::Object(parameters))
                    });
                    tx.send(Msg::RemoteEdited { old, name, result }).ok();
                });
            }
            _ => self.finish_remote_edit(&old, &name),
        }
    }

    /// Record a remote's new settings and name, and close the editor.
    fn finish_remote_edit(&self, old: &str, name: &str) {
        let mut ok = false;
        let password = self.password.borrow().clone();
        self.with_repo("save remote", |repo| {
            repo.absorb_rclone_config()?;
            if name != old {
                let password = password.as_ref().map(|p| p.expose_secret());
                repo.rename_remote(old, name, password)?;
            }
            ok = true;
            Ok(format!("Saved remote {name}"))
        });
        if ok {
            self.close_editor();
        }
    }

    fn close_editor(&self) {
        *self.edit.borrow_mut() = None;
        let w = &self.window;
        w.set_editing(false);
        w.set_edit_original(SharedString::default());
        w.set_edit_fields(ModelRc::default());
        w.set_edit_values(ModelRc::default());
        w.set_edit_note(SharedString::default());
    }

    // ── guided remote setup ──

    /// Show the hardcoded form for the backend picked by `title`.
    fn select_backend(&self, title: &str) {
        let Some((index, backend)) = backends::offered()
            .enumerate()
            .find(|(_, b)| b.title == title)
        else {
            return;
        };
        self.window
            .set_setup_backend(SharedString::from(backend.name));
        // The picker is made anew each time it is shown: it starts here.
        self.window.set_setup_backend_index(index as i32);
        if self.window.get_setup_state().is_empty() {
            self.show_form(backend.name);
        }
    }

    /// Populate the UI with the hardcoded form fields of `backend_name`.
    fn show_form(&self, backend_name: &str) {
        let fields: Vec<FormField> = backends::backend(backend_name)
            .map(|b| {
                b.fields
                    .iter()
                    .map(|field| form_field(b, field, field.default, field.default))
                    .collect()
            })
            .unwrap_or_default();
        let values: Vec<SharedString> = backends::backend(backend_name)
            .map(|b| {
                b.fields
                    .iter()
                    .map(|field| SharedString::from(field.default))
                    .collect()
            })
            .unwrap_or_default();
        self.window
            .set_form_fields(ModelRc::new(VecModel::from(fields)));
        self.window
            .set_form_values(ModelRc::new(VecModel::from(values)));
        self.window.set_setup_active(true);
    }

    /// The backend picked and the bucket/path named in its form, if the
    /// form is filled in well enough to submit.
    fn check_form(
        &self,
        backend_name: &str,
        values: &[SharedString],
    ) -> Option<(&'static Backend, String)> {
        let Some(backend) = backends::offered().find(|b| b.name == backend_name) else {
            self.error(format!("Unknown backend '{backend_name}'"));
            return None;
        };
        for (field, value) in backend.fields.iter().zip(values.iter()) {
            if field.required && value.trim().is_empty() {
                self.error(format!("Error: '{}' is required", field.label));
                return None;
            }
        }
        let spec = backends::target_spec(backend, values);
        if let Err(e) = backends::check_target_spec(backend, &spec) {
            self.error(format!("Error: {e}"));
            return None;
        }
        Some((backend, spec))
    }

    /// Submit the filled form: hand every answer to rclone in one call,
    /// then add the layers mbr stores through (see [`mbr::remotes`]).
    /// Always encrypted; unencrypted remotes are CLI-only.
    fn submit_form(&self, name: &str, backend_name: &str, values: &[SharedString]) {
        let Some(password) = self.password.borrow().clone() else {
            self.warn("Set a master password in the Settings tab first");
            return;
        };
        let layout = match mbr::remotes::Layout::new(name.trim(), true) {
            Ok(layout) => layout,
            Err(e) => {
                self.error(format!("Error: {e}"));
                return;
            }
        };
        let Some((backend, spec)) = self.check_form(backend_name, values) else {
            return;
        };
        let prepared = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            match repo.db.remote(&layout.name) {
                Ok(Some(_)) => Err(format!("remote '{}' already exists", layout.name)),
                Ok(None) => repo
                    .folder_id()
                    .map(|folder_id| (folder_id, repo.rclone_config_path())),
                Err(e) => Err(e),
            }
        };
        let (folder_id, config_path) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.error(format!("Error: {e}"));
                return;
            }
        };
        let pending = PendingSetup {
            config_path,
            section: layout.root_section(),
            spec,
            password,
            purpose: Purpose::Remote { layout, folder_id },
            cancelled: Arc::default(),
        };
        let parameters = backends::form_parameters(backend, values);
        self.run_setup_step(
            pending,
            SetupStep::Begin {
                backend: backend.name,
                parameters,
            },
        );
    }

    /// Continue after answering a fallback question.
    fn submit_remote_setup(&self, backend_name: &str, state: &str, answer: &str) {
        let Some(pending) = self.setup.borrow().clone() else {
            return;
        };
        let values: Vec<SharedString> = self.window.get_form_values().iter().collect();
        let parameters = match backends::backend(backend_name) {
            Some(b) => backends::form_parameters(b, &values),
            None => serde_json::json!({}),
        };
        self.run_setup_step(
            pending,
            SetupStep::Continue {
                state: state.to_owned(),
                answer: answer.to_owned(),
                parameters,
            },
        );
    }

    /// Run one config-protocol step on a background thread.
    fn run_setup_step(&self, pending: PendingSetup, step: SetupStep) {
        *self.setup.borrow_mut() = Some(pending.clone());
        self.window.set_setup_connecting(true);
        self.status(match pending.purpose {
            Purpose::Remote { .. } => "Talking to rclone…",
            Purpose::Restore { .. } => "Looking for the backup…",
        });
        self.spawn(move |tx| {
            let mut on_url = |url| {
                let _ = tx.send(Msg::OAuthUrl(url));
            };
            let reached = pending.run(step, &mut on_url);
            let finished = !matches!(
                reached,
                Ok(Reached::Step(mbr::rclone::SetupOutcome::Question(_)))
            );
            if finished && let Some(config) = pending.throwaway_config() {
                std::fs::remove_file(config).ok();
            }
            match (reached, &pending.purpose) {
                (Ok(Reached::Backup(sealed)), Purpose::Restore { dir }) => {
                    tx.send(Msg::Status(
                        Level::Info,
                        "Decrypting the backup…".to_owned(),
                    ))
                    .ok();
                    let result = metadata::unseal(&sealed, pending.password.expose_secret());
                    tx.send(Msg::BackupOpened {
                        dir: dir.clone(),
                        password: pending.password.clone(),
                        result,
                    })
                    .ok();
                }
                (reached, _) => {
                    let result = reached.and_then(|reached| match reached {
                        Reached::Step(outcome) => Ok(outcome),
                        Reached::Backup(_) => Err("unexpected backup".to_owned()),
                    });
                    tx.send(Msg::Setup { pending, result }).ok();
                }
            }
        });
    }

    fn cancel_remote_setup(&self) {
        let state = self.window.get_setup_state().to_string();
        let connecting = self.window.get_setup_connecting();
        let pending = self.setup.borrow().clone();
        self.reset_setup_ui();
        let Some(pending) = pending else { return };
        if connecting {
            // A step is running (maybe waiting for a browser sign-in): it
            // stops and is cleaned up when it returns, see `Msg::Setup`.
            pending.cancelled.store(true, Ordering::Relaxed);
            self.status("Cancelling…");
            return;
        }
        self.abandon_setup(pending, state);
    }

    /// Abort `pending`'s setup conversation, now at `state` (empty when
    /// there is none to abort), in the background.
    fn abandon_setup(&self, pending: PendingSetup, state: String) {
        if state.is_empty() && pending.throwaway_config().is_none() {
            return;
        }
        self.spawn(move |tx| {
            let result = if state.is_empty() {
                Ok(())
            } else {
                mbr::rclone::with_config(&pending.config_path, || {
                    mbr::rclone::cancel_setup(&pending.section, &state)
                })
            };
            if let Some(config) = pending.throwaway_config() {
                std::fs::remove_file(config).ok();
            }
            tx.send(Msg::Cancelled { result }).ok();
        });
    }

    /// Record the remote `layout` just set up in rclone, in `spec`.
    fn save_new_remote(&self, layout: &mbr::remotes::Layout, spec: &str) {
        self.with_repo("save remote", |repo| {
            let remote = repo.add_remote(layout, spec)?;
            Ok(format!(
                "Added remote {} ({})",
                remote.name,
                mbr::remotes::describe(&remote.target)
            ))
        });
        self.refresh();
    }

    /// Record in the database whatever rclone changed in the open folder's
    /// rclone settings (sign-in tokens it refreshed, …).
    fn absorb_rclone_config(&self) {
        let mut repo = self.repo.borrow_mut();
        if let Some(repo) = repo.as_mut()
            && let Err(e) = repo.absorb_rclone_config()
        {
            log::warn!("cannot record the rclone settings: {e}");
        }
    }

    /// Clear the form and question panels.
    fn reset_setup_ui(&self) {
        *self.setup.borrow_mut() = None;
        self.window.set_setup_connecting(false);
        self.window.set_setup_active(false);
        self.window.set_setup_state(SharedString::default());
        self.window.set_setup_question(SharedString::default());
        self.window.set_setup_help(SharedString::default());
        self.window.set_setup_answer(SharedString::default());
        self.window.set_setup_default(SharedString::default());
        self.window.set_setup_url(SharedString::default());
        self.window.set_form_fields(ModelRc::default());
        self.window.set_form_values(ModelRc::default());
        self.window.set_new_remote_name(SharedString::default());
        // Offer a fresh form for the backend still selected in the picker.
        let backend = self.window.get_setup_backend();
        if !backend.is_empty() {
            self.show_form(&backend);
        }
    }

    /// Apply a completion message from a background task (UI thread).
    fn handle(&self, msg: Msg) {
        match msg {
            Msg::Pushed {
                remote,
                hash,
                result,
            } => match result {
                Ok(()) => self.with_repo("record upload", |repo| {
                    repo.db.set_blob_on_remote(&hash, &remote)?;
                    Ok(format!("Uploaded {} to {remote}", util::short_hash(&hash)))
                }),
                Err(e) => self.error(format!("Upload to {remote} failed: {e}")),
            },
            Msg::Fetched { hash, result } => match result {
                Ok(remote) => {
                    // Here now: its preview can be made.
                    self.no_thumb.borrow_mut().remove(&hash);
                    self.success(format!(
                        "Downloaded {} from {remote}",
                        util::short_hash(&hash)
                    ))
                }
                Err(e) => self.error(format!("Download failed: {e}")),
            },
            Msg::Thumb {
                root,
                hash,
                outcome,
            } => self.thumb_made(&root, hash, outcome),
            Msg::Preview { hash, outcome } => self.preview_made(hash, outcome),
            Msg::Checked {
                remote,
                hash,
                result,
            } => match result {
                Ok(size) => self.with_repo("record check", |repo| {
                    repo.record_blob_check(&remote, &hash, size)?;
                    let verdict = if size.is_some() {
                        "stored"
                    } else {
                        "not stored"
                    };
                    Ok(format!(
                        "{} is {verdict} on {remote}",
                        util::short_hash(&hash)
                    ))
                }),
                Err(e) => self.error(format!("Check on {remote} failed: {e}")),
            },
            Msg::Status(level, text) => self.notify(level, text),
            Msg::Listed { remote, result } => match result {
                Ok(found) => self.with_repo("reconcile remote", |repo| {
                    let Some(r) = repo.db.remote(&remote)? else {
                        return Err(format!("remote '{remote}' vanished"));
                    };
                    let check = repo.apply_remote_listing(&r, found)?;
                    let mut text = format!(
                        "Remote '{remote}' reachable; {} expected blob(s) present",
                        check.present.len()
                    );
                    let mut level = Level::Success;
                    if !check.missing.is_empty() {
                        text += &format!(", {} MISSING", check.missing.len());
                        level = Level::Error;
                    }
                    if !check.discovered.is_empty() {
                        text += &format!(", {} newly recorded", check.discovered.len());
                    }
                    if !check.adopted.is_empty() {
                        text += &format!(
                            ", {} unknown blob(s) added under {}/",
                            check.adopted.len(),
                            repo::UNNAMED_DIR
                        );
                    }
                    Ok((level, text))
                }),
                Err(e) => self.error(format!("Remote '{remote}' scan failed: {e}")),
            },
            Msg::RemoteConfig { remote, result } => {
                let current = self.edit.borrow().as_ref().map(|e| e.original.clone());
                if current.as_deref() != Some(remote.as_str()) {
                    return; // the editor was closed or moved on meanwhile
                }
                match result {
                    Ok(config) => self.show_remote_config(config),
                    Err(e) => self
                        .window
                        .set_edit_note(format!("Could not read rclone settings: {e}").into()),
                }
            }
            Msg::RemoteEdited { old, name, result } => match result {
                Ok(()) => self.finish_remote_edit(&old, &name),
                Err(e) => self.error(format!("Saving rclone settings failed: {e}")),
            },
            Msg::Setup { pending, result } if pending.cancelled.load(Ordering::Relaxed) => {
                // The UI was reset when the user cancelled; only clean up.
                match result {
                    Err(_) => self.status("Setup cancelled"),
                    Ok(mbr::rclone::SetupOutcome::Question(q)) => {
                        self.status("Setup cancelled");
                        self.abandon_setup(pending, q.state);
                    }
                    // Finished before the cancel took effect: the remote
                    // exists in rclone's config now, so keep it.
                    Ok(mbr::rclone::SetupOutcome::Done) => {
                        if let Purpose::Remote { layout, .. } = &pending.purpose {
                            self.save_new_remote(layout, &pending.spec);
                        }
                    }
                }
            }
            Msg::Setup { pending, result } => {
                self.window.set_setup_connecting(false);
                match result {
                    Err(e) => {
                        self.window
                            .set_setup_help(SharedString::from(format!("Error: {e}")));
                        self.window.set_setup_state(SharedString::default());
                        self.window.set_setup_url(SharedString::default());
                        self.error(match pending.purpose {
                            Purpose::Remote { .. } => format!("Remote setup failed: {e}"),
                            Purpose::Restore { .. } => format!("Restore failed: {e}"),
                        });
                    }
                    Ok(mbr::rclone::SetupOutcome::Done) => {
                        if let Purpose::Remote { layout, .. } = &pending.purpose {
                            self.save_new_remote(layout, &pending.spec);
                            self.reset_setup_ui();
                        }
                    }
                    Ok(mbr::rclone::SetupOutcome::Question(q)) => {
                        self.window.set_setup_state(SharedString::from(q.state));
                        self.window.set_setup_question(SharedString::from(q.name));
                        self.window.set_setup_help(SharedString::from(q.help));
                        self.window.set_setup_boolean(q.boolean);
                        // A checkbox shows its answer, so start from the default.
                        self.window.set_setup_answer(if q.boolean {
                            SharedString::from(q.default.as_str())
                        } else {
                            SharedString::default()
                        });
                        self.window.set_setup_default(SharedString::from(q.default));
                        self.window.set_setup_url(SharedString::default());
                        self.status("rclone needs one more answer to finish this remote");
                    }
                }
            }
            Msg::OAuthUrl(url) => {
                self.window.set_setup_url(SharedString::from(url.as_str()));
                self.status(
                    "Waiting for browser sign-in… (use the link below if no browser opened)",
                );
            }
            Msg::Cancelled { result } => {
                if let Err(e) = result {
                    self.warn(format!("Note: {e}"));
                }
            }
            Msg::MetadataUploaded {
                root,
                remote,
                db_hash,
                result,
            } => match result {
                Ok(()) => {
                    self.state
                        .borrow_mut()
                        .repo_mut(&root)
                        .metadata_backups
                        .insert(
                            remote.clone(),
                            BackupRecord {
                                at: util::now(),
                                db_hash,
                            },
                        );
                    self.save_state();
                    self.success(format!("Metadata backed up to {remote}"));
                }
                Err(e) => self.error(format!("Metadata backup to {remote} failed: {e}")),
            },
            Msg::BackupOpened {
                dir,
                password,
                result,
            } => {
                if self.pending_folder.borrow().as_ref() != Some(&dir) {
                    return; // another folder was opened meanwhile
                }
                match result {
                    Ok(dump) => self.finish_restore(&dir, &dump, password),
                    Err(e) => self.error(format!("Restore failed: {e}")),
                }
            }
            Msg::Dropped { size, result } => match result {
                Ok(outcome) => {
                    let min_copies = self
                        .repo
                        .borrow_mut()
                        .as_mut()
                        .and_then(|r| r.min_copies().ok())
                        .unwrap_or(repo::DEFAULT_MIN_COPIES);
                    self.with_repo("record drop", |repo| {
                        repo.record_drop(&outcome)?;
                        Ok(if outcome.dropped {
                            (
                                Level::Success,
                                format!(
                                    "Freed {} ({} is on {})",
                                    util::format_size(size),
                                    util::short_hash(&outcome.hash),
                                    outcome.verified.join(", ")
                                ),
                            )
                        } else {
                            (Level::Warning, outcome.kept_reason(min_copies))
                        })
                    });
                }
                Err(e) => self.error(format!("Could not free space: {e}")),
            },
            Msg::VerifiedLocal { hash, result } => match result {
                Ok(LocalHealth::Corrupt { moved_to }) => self.error(format!(
                    "{} is DAMAGED; moved to {}",
                    util::short_hash(&hash),
                    moved_to.display()
                )),
                Ok(_) => self.success(format!("{} verified", util::short_hash(&hash))),
                Err(e) => self.error(format!("Verifying failed: {e}")),
            },
            Msg::VerifiedRemote {
                remote,
                hash,
                result,
            } => match result {
                Ok(health) => self.with_repo("record verification", |repo| {
                    repo.record_remote_health(&remote, &hash, &health)?;
                    let (level, verdict) = match health {
                        RemoteHealth::Ok => (Level::Success, "intact"),
                        RemoteHealth::Missing => (Level::Error, "MISSING"),
                        RemoteHealth::Corrupt => (Level::Error, "DAMAGED (deleted there)"),
                    };
                    Ok((
                        level,
                        format!("{} is {verdict} on {remote}", util::short_hash(&hash)),
                    ))
                }),
                Err(e) => self.error(format!("Verifying on {remote} failed: {e}")),
            },
            Msg::Scanned { root, result } => {
                self.scanning.set(false);
                let current = self.repo.borrow().as_ref().map(|r| r.root.clone());
                if current.as_ref() != Some(&root) {
                    // Another folder was opened meanwhile; its own scan
                    // will be (or was) started by `attach`.
                    if current.is_some() {
                        self.rescan();
                    }
                    return;
                }
                match result {
                    Ok(entries) => self.with_repo("scan", |repo| {
                        let report = repo.apply_scan(entries)?;
                        Ok(if report.is_empty() {
                            "Scan finished: nothing changed".to_owned()
                        } else {
                            format!(
                                "Scan finished: {} added, {} replaced, {} removed",
                                report.added.len(),
                                report.replaced.len(),
                                report.removed.len()
                            )
                        })
                    }),
                    Err(e) => self.error(format!("Scan failed: {e}")),
                }
                if self.sync_pending.get() {
                    self.apply_synced();
                }
                // What the scan found goes to the peers.
                self.sync_peers();
            }
            Msg::Done => {
                let left = self.tasks.get().saturating_sub(1);
                self.tasks.set(left);
                self.window.set_busy(left > 0);
            }
            Msg::Peer(event) => self.peer_event(event),
        }
    }

    // ── view refresh ──

    /// Rebuild every model from the repo (cheap; local data only).
    fn refresh(&self) {
        let mut guard = self.repo.borrow_mut();
        let Some(repo) = guard.as_mut() else {
            let w = &self.window;
            w.set_folder(SharedString::default());
            self.files.set_vec(Vec::new());
            self.statuses.borrow_mut().clear();
            w.set_remotes(ModelRc::default());
            w.set_remote_names(ModelRc::default());
            w.set_metadata_rows(ModelRc::default());
            drop(guard);
            self.refresh_peers();
            return;
        };
        self.window
            .set_folder(repo.root.display().to_string().into());
        let min_copies = repo.min_copies().unwrap_or(repo::DEFAULT_MIN_COPIES);
        self.window.set_min_copies(min_copies as i32);

        match repo.status() {
            Ok(statuses) => {
                let mut selection = self.selection.borrow_mut();
                let listed: HashSet<&str> = statuses.iter().map(|s| s.path.as_str()).collect();
                selection.paths.retain(|p| listed.contains(p.as_str()));
                let thumbs = self.thumbs.borrow();
                let rows: Vec<FileRow> = statuses
                    .iter()
                    .map(|s| {
                        let mut row = file_row(s, selection.paths.contains(&s.path), min_copies);
                        if let Some(thumb) = thumbs.get(&s.hash) {
                            row.thumb = thumb.clone();
                            row.has_thumb = true;
                        }
                        row
                    })
                    .collect();
                drop(thumbs);
                self.window
                    .set_at_risk_count(rows.iter().filter(|r| r.at_risk).count() as i32);
                // Same model, new contents: the list keeps its scroll position.
                self.files.set_vec(rows);
                *self.statuses.borrow_mut() = statuses;
            }
            Err(e) => self.error(format!("Error listing files: {e}")),
        }

        match repo.db.list_remotes() {
            Ok(remotes) => {
                let names: Vec<SharedString> =
                    remotes.iter().map(|r| r.name.as_str().into()).collect();
                let rows: Vec<RemoteRow> = remotes
                    .iter()
                    .map(|r| RemoteRow {
                        name: r.name.as_str().into(),
                        storage: mbr::remotes::describe(&r.target).into(),
                        blobs: format!(
                            "{} blob(s) recorded",
                            repo.db
                                .blobs_on_remote(&r.name)
                                .map(|b| b.len())
                                .unwrap_or(0)
                        )
                        .into(),
                        missing_locally: repo
                            .blobs_missing_locally(&r.name)
                            .map(|b| b.len() as i32)
                            .unwrap_or(0),
                    })
                    .collect();
                self.window
                    .set_remote_names(ModelRc::new(VecModel::from(names)));
                self.window.set_remotes(ModelRc::new(VecModel::from(rows)));
            }
            Err(e) => self.error(format!("Error listing remotes: {e}")),
        }

        match repo.unreferenced_blobs() {
            Ok(old) => {
                let here: Vec<_> = old.iter().filter(|u| u.present_locally).collect();
                self.window.set_old_local_count(here.len() as i32);
                self.window.set_old_local_size(
                    util::format_size(here.iter().map(|u| u.size).sum()).into(),
                );
            }
            Err(e) => self.error(format!("Error listing old versions: {e}")),
        }

        self.refresh_metadata(repo);
        drop(guard);
        self.refresh_peers();
        self.show_selection();
        self.make_thumbs();
    }

    /// Rebuild the Settings tab.
    fn refresh_metadata(&self, repo: &mut Repo) {
        let w = &self.window;
        let state = self.state.borrow().repo(&repo.root);
        w.set_password_set(repo.password_check().is_ok_and(|c| c.is_some()));
        w.set_password_available(self.password.borrow().is_some());

        let (remotes, chosen) = match repo.db.list_remotes().and_then(|all| {
            let chosen: HashSet<String> = repo
                .db
                .metadata_remotes()?
                .into_iter()
                .map(|r| r.name)
                .collect();
            Ok((all, chosen))
        }) {
            Ok(found) => found,
            Err(e) => {
                self.error(format!("Error listing metadata remotes: {e}"));
                return;
            }
        };
        let current = repo.db_hash().unwrap_or_default();
        let rows: Vec<MetadataRow> = remotes
            .iter()
            .map(|r| {
                let record = state.metadata_backups.get(&r.name);
                let up_to_date = record.is_some_and(|b| b.db_hash == current);
                let status = match record {
                    None => "never backed up".to_owned(),
                    Some(b) if up_to_date => format!("up to date · {}", util::format_time(b.at)),
                    Some(b) => format!("out of date · last {}", util::format_time(b.at)),
                };
                MetadataRow {
                    name: r.name.as_str().into(),
                    storage: mbr::remotes::describe(&r.target).into(),
                    enabled: chosen.contains(&r.name),
                    status: status.into(),
                    up_to_date,
                }
            })
            .collect();
        let chosen_rows: Vec<&MetadataRow> = rows.iter().filter(|r| r.enabled).collect();
        let latest = chosen_rows
            .iter()
            .filter_map(|r| state.metadata_backups.get(r.name.as_str()))
            .map(|b| b.at)
            .max();
        let summary = match (chosen_rows.len(), latest) {
            (0, _) => "Metadata is not backed up anywhere yet.".to_owned(),
            (_, None) => "Metadata has never been backed up from this computer.".to_owned(),
            (n, Some(at)) => {
                let fresh = chosen_rows.iter().filter(|r| r.up_to_date).count();
                if fresh == n {
                    format!(
                        "Metadata is backed up and up to date (last backup {}).",
                        util::format_time(at)
                    )
                } else {
                    format!(
                        "Metadata changed since the last backup ({}): {fresh} of {n} remote(s) up to date.",
                        util::format_time(at)
                    )
                }
            }
        };
        w.set_metadata_summary(summary.into());
        w.set_metadata_chosen(chosen_rows.len() as i32);
        w.set_metadata_rows(ModelRc::new(VecModel::from(rows)));
    }
}

/// The longest side of the detail panel's preview, in pixels.
const PREVIEW_SIZE: u32 = 1600;

fn to_image(preview: &thumbs::Preview) -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
        &preview.rgba,
        preview.width,
        preview.height,
    ))
}

/// One form row for `field`, prefilled with `value` (for choices: the
/// option preselected), showing `placeholder` while empty.
fn form_field(backend: &Backend, field: &Field, value: &str, placeholder: &str) -> FormField {
    let mut values = match field.kind {
        Kind::Choice => backends::choice_values(field),
        _ => field.examples.to_vec(),
    };
    let mut owned: Vec<String> = values.drain(..).map(str::to_owned).collect();
    if field.kind == Kind::Choice && !owned.iter().any(|v| v == value) {
        // A stored value outside the known choices stays selectable.
        owned.push(value.to_owned());
    }
    let default_index = owned
        .iter()
        .position(|v| v == value)
        .map(|i| i as i32)
        .unwrap_or(0);
    let labels: Vec<SharedString> = owned
        .iter()
        .map(|v| SharedString::from(if v.is_empty() { "(rclone default)" } else { v }))
        .collect();
    let examples: Vec<SharedString> = owned
        .iter()
        .map(|v| SharedString::from(v.as_str()))
        .collect();
    FormField {
        key: field.name.into(),
        label: field.label.into(),
        docs: backends::rclone_docs_url(backend, field)
            .unwrap_or_else(|| "remotes/#setting-up-a-new-remote".into())
            .into(),
        kind: match field.kind {
            Kind::Text => 0,
            Kind::Secret => 1,
            Kind::Choice => 2,
        },
        default: placeholder.into(),
        default_index,
        examples: ModelRc::new(VecModel::from(examples)),
        labels: ModelRc::new(VecModel::from(labels)),
        required: field.required,
    }
}

fn file_row(s: &repo::FileStatus, selected: bool, min_copies: u32) -> FileRow {
    let versions = s
        .previous_versions
        .iter()
        .map(|v| {
            format!(
                "{} · {} · removed {} · {}",
                util::short_hash(&v.hash),
                util::format_size(v.size),
                util::format_time(v.removed_at),
                v.stored_summary(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    FileRow {
        path: s.path.as_str().into(),
        hash: s.hash.as_str().into(),
        short_hash: util::short_hash(&s.hash).into(),
        size: util::format_size(s.size).into(),
        added: util::format_time(s.added_at).into(),
        present: s.present_locally,
        remotes: s.remotes.join(", ").into(),
        other_names: s.other_names.join(", ").into(),
        past_names: s.past_names.join(", ").into(),
        versions: versions.into(),
        selected,
        copies: s.copies() as i32,
        at_risk: s.copies() < min_copies as usize,
        thumb: slint::Image::default(),
        has_thumb: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<String> {
        ["a", "b", "c", "d", "e"].map(String::from).to_vec()
    }

    fn selected(s: &Selection) -> Vec<&str> {
        s.paths.iter().map(String::as_str).collect()
    }

    #[test]
    fn click_ctrl_click_and_shift_click() {
        let rows = rows();
        let mut s = Selection::default();
        s.press(&rows, 1, false, false);
        assert_eq!(selected(&s), ["b"]);
        s.press(&rows, 3, true, false);
        assert_eq!(selected(&s), ["b", "d"]);
        s.press(&rows, 1, true, false);
        assert_eq!(selected(&s), ["d"]);
        // Shift extends from the last plain or Ctrl click.
        s.press(&rows, 0, false, true);
        assert_eq!(selected(&s), ["a", "b"]);
        s.press(&rows, 4, false, false);
        s.press(&rows, 2, true, true);
        assert_eq!(selected(&s), ["c", "d", "e"]);
    }

    #[test]
    fn dragging_selects_a_range() {
        let rows = rows();
        let mut s = Selection::default();
        s.press(&rows, 1, false, false);
        // Wiggling inside the pressed row is still a click.
        s.drag(&rows, 1);
        assert_eq!(selected(&s), ["b"]);
        s.drag(&rows, 3);
        assert_eq!(selected(&s), ["b", "c", "d"]);
        // Dragging back shrinks the range; past the ends clamps.
        s.drag(&rows, -5);
        assert_eq!(selected(&s), ["a", "b"]);
        // Ctrl+drag adds to what was selected before.
        s.press(&rows, 4, true, false);
        s.drag(&rows, 3);
        assert_eq!(selected(&s), ["a", "b", "d", "e"]);
    }
}
