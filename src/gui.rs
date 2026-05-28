//! `my-beloved-rubble` — the Slint GUI.
//!
//! Three tabs: **Blobs** (the annotated file listing, with multi-selection
//! and a sidebar acting on the selected blobs), **Remotes** (listing,
//! scanning, downloading from, adding, setting up and editing remotes) and
//! **Metadata backup** (the master password, which remotes receive the
//! encrypted database, and restoring a lost folder from one of them).
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
use mbr::rclone::RemoteBlob;
use mbr::repo::{self, DropOutcome, LocalHealth, RemoteHealth, Repo};
use mbr::state::{AppState, BackupRecord};
use mbr::util;
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

slint::include_modules!();

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
    Status(String),
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
        name: String,
        /// mbr target to record once configuration completes.
        target: String,
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
    /// A metadata backup was downloaded and decrypted; on success, restore
    /// it into the folder at `root`.
    MetadataFetched {
        root: PathBuf,
        password: SecretString,
        result: Result<(Vec<u8>, Vec<u8>), String>,
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
        section: String,
        result: Result<Option<serde_json::Map<String, serde_json::Value>>, String>,
    },
    /// The rclone half of saving a remote edit finished; on success,
    /// record the new name and target.
    RemoteEdited {
        old: String,
        name: String,
        target: String,
        result: Result<(), String>,
    },
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
    /// The rclone remote the target refers to, if configured here.
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
    /// Local app state (`state.json`), loaded on start.
    state: RefCell<AppState>,
    /// The open folder's master password, once unlocked this session.
    password: RefCell<Option<SecretString>>,
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

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();

    let window = AppWindow::new()?;
    window.global::<Docs>().on_open(|page| {
        let url = docs_url(&page);
        if let Err(e) = open::that_detached(&url) {
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
                    app.handle(msg);
                    refresh_needed = true;
                }
                if refresh_needed {
                    app.refresh();
                }
            },
        );
    }

    // ── callbacks ──
    let w = &app.window;
    {
        let app = app.clone();
        w.on_open_folder(move || {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                app.attach(dir);
            }
        });
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
                        app.status(format!("Error: {e}"));
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
        w.on_remove_remote(move |name| {
            app.with_repo("remove remote", |repo| {
                repo.db.remove_remote(&name)?;
                Ok(format!("Removed remote {name}"))
            });
            if app.window.get_edit_original() == name {
                app.close_editor();
            }
            app.refresh();
        });
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
        w.on_add_remote(move |name, target| {
            app.with_repo("add remote", |repo| {
                if repo.db.remote(&name)?.is_some() {
                    return Err(format!("remote '{name}' already exists"));
                }
                repo.db.add_remote(&name, &target)?;
                Ok(format!("Added remote {name} -> {target}"))
            });
            app.window.set_existing_remote_name(SharedString::default());
            app.window.set_new_remote_target(SharedString::default());
            app.refresh();
        });
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
        w.on_submit_remote_setup(move |name, backend, state, answer| {
            app.submit_remote_setup(&name, &backend, &state, &answer);
        });
    }
    {
        let app = app.clone();
        w.on_cancel_remote_setup(move || app.cancel_remote_setup());
    }

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
                        app.status(format!("Error: {e}"));
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
                        app.status(format!("Error: {e}"));
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
        w.on_unlock(move |password| app.unlock(&password));
    }
    {
        let app = app.clone();
        w.on_lock(move || {
            *app.password.borrow_mut() = None;
            app.status("Locked: the master password was forgotten");
            app.refresh();
        });
    }
    {
        let app = app.clone();
        w.on_backup_metadata(move || app.backup_metadata());
    }
    {
        let app = app.clone();
        w.on_restore_metadata(move |remote, password| app.restore_metadata(&remote, &password));
    }

    // ── backend picker + initial form ──
    let titles: Vec<SharedString> = backends::BACKENDS
        .iter()
        .map(|b| SharedString::from(b.title))
        .collect();
    app.window
        .set_backend_titles(ModelRc::new(VecModel::from(titles)));
    if let Some(first) = backends::BACKENDS.first() {
        app.select_backend(first.title);
    }

    // ── reopen last folder ──
    let last = app.state.borrow().last_folder.clone();
    if let Some(dir) = last.filter(|dir| dir.join(repo::DB_FILE).is_file()) {
        app.attach(dir);
    }

    app.window.run()
}

impl App {
    fn status(&self, text: impl Into<SharedString>) {
        self.window.set_status_text(text.into());
    }

    /// Run a database operation against the open repo, reporting the
    /// outcome in the status bar.
    fn with_repo(&self, what: &str, f: impl FnOnce(&mut Repo) -> Result<String, String>) {
        let mut repo = self.repo.borrow_mut();
        match repo.as_mut() {
            Some(repo) => match f(repo) {
                Ok(status) => self.status(status),
                Err(e) => self.status(format!("Error: {e}")),
            },
            None => self.status(format!("Cannot {what}: no folder open")),
        }
    }

    /// Look up remote `name` in the open repo, reporting failures.
    fn remote(&self, name: &str) -> Option<Remote> {
        let mut repo = self.repo.borrow_mut();
        match repo.as_mut()?.db.remote(name) {
            Ok(Some(remote)) => Some(remote),
            Ok(None) => {
                self.status(format!("No remote named '{name}'"));
                None
            }
            Err(e) => {
                self.status(format!("Error: {e}"));
                None
            }
        }
    }

    fn config_path(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(Repo::rclone_config_path)
    }

    /// Attach `dir`, initializing it first if it is not an mbr folder.
    fn attach(&self, dir: PathBuf) {
        if !Repo::is_initialized(&dir) && !confirm_init(&dir) {
            self.status(format!("Did not attach {}", dir.display()));
            return;
        }
        match Repo::init(&dir) {
            Ok(repo) => {
                self.state.borrow_mut().last_folder = Some(repo.root.clone());
                self.save_state();
                *self.password.borrow_mut() = None;
                *self.repo.borrow_mut() = Some(repo);
                *self.selection.borrow_mut() = Selection::default();
                self.close_editor();
                self.refresh();
                self.rescan();
            }
            Err(e) => self.status(format!("Error: {e}")),
        }
    }

    /// Start a scan: ingest on a background thread, then record the result
    /// on the UI thread (see [`Msg::Scanned`]).
    fn rescan(&self) {
        let Some(root) = self.repo.borrow().as_ref().map(|r| r.root.clone()) else {
            self.status("Cannot scan: no folder open");
            return;
        };
        if self.scanning.replace(true) {
            self.status("A scan is already running");
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
        self.statuses.borrow().iter().map(|s| s.path.clone()).collect()
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
            let Some(mut row) = self.files.row_data(i) else { continue };
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
    }

    fn upload_selected(&self, remote: &str) {
        let hashes = self.selected_hashes();
        let (present, absent): (Vec<String>, Vec<String>) = {
            let repo = self.repo.borrow();
            let Some(repo) = repo.as_ref() else { return };
            hashes.into_iter().partition(|h| repo.blob_present(h))
        };
        if present.is_empty() {
            self.status("None of the selected blobs is present locally; nothing to upload");
            return;
        }
        if !absent.is_empty() {
            self.status(format!(
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
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
        });
    }

    /// Download the selected blobs missing locally, trying `preferred`
    /// first and then any other remote recorded as holding each.
    fn download_selected(&self, preferred: &str) {
        let hashes = self.selected_hashes();
        let jobs = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let missing: Vec<String> =
                hashes.into_iter().filter(|h| !repo.blob_present(h)).collect();
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
                    self.status(format!("Error: {e}"));
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
        let Some(config_path) = self.config_path() else { return };
        if jobs.is_empty() {
            self.status("None of these blobs is present locally; nothing to free");
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
            if dropped < total {
                text += &format!(
                    "; kept {} that fewer than {min_copies} remote(s) are confirmed to hold",
                    total - dropped
                );
            }
            if let Err(e) = configured {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
        });
    }

    /// Re-hash the local copies of `hashes`.
    fn verify_local(&self, hashes: Vec<String>) {
        let Some(root) = self.repo.borrow().as_ref().map(|r| r.root.clone()) else {
            return;
        };
        if hashes.is_empty() {
            self.status("Nothing stored here to verify");
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
            tx.send(Msg::Status(if damaged == 0 {
                format!("Verified {total} blob(s): all intact")
            } else {
                format!(
                    "Verified {total} blob(s): {damaged} DAMAGED, set aside in {} — download them again",
                    repo::CORRUPT_DIR
                )
            }))
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
            if corrupt > 0 {
                text += &format!(", {corrupt} DAMAGED (deleted there; upload them again)");
            }
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
        });
    }

    // ── metadata backup ──

    fn save_state(&self) {
        if let Err(e) = self.state.borrow().save() {
            self.status(format!("Could not save app state: {e}"));
        }
    }

    fn root(&self) -> Option<PathBuf> {
        self.repo.borrow().as_ref().map(|r| r.root.clone())
    }

    /// Set the master password, or change it once unlocked.
    fn set_password(&self, password: &str, repeat: &str) {
        let Some(root) = self.root() else { return };
        let had_one = self.state.borrow().repo(&root).password_check.is_some();
        if had_one && self.password.borrow().is_none() {
            self.status("Unlock with the current master password first");
            return;
        }
        if let Err(e) = metadata::check_new_password(password) {
            self.status(format!("Error: {e}"));
            return;
        }
        if password != repeat {
            self.status("Error: the passwords differ");
            return;
        }
        let check = match metadata::password_check(password) {
            Ok(check) => check,
            Err(e) => {
                self.status(format!("Error: {e}"));
                return;
            }
        };
        {
            let mut state = self.state.borrow_mut();
            let repo_state = state.repo_mut(&root);
            repo_state.password_check = Some(check);
            // Backups made so far are encrypted with the old password.
            repo_state.metadata_backups.clear();
        }
        self.save_state();
        *self.password.borrow_mut() = Some(SecretString::from(password.to_owned()));
        self.window.set_pw_new(SharedString::default());
        self.window.set_pw_repeat(SharedString::default());
        self.status(if had_one {
            "Master password changed; back up again to re-encrypt the backups with it"
        } else {
            "Master password set"
        });
        self.refresh();
    }

    fn unlock(&self, password: &str) {
        let Some(root) = self.root() else { return };
        let Some(check) = self.state.borrow().repo(&root).password_check else {
            return;
        };
        if !metadata::password_matches(&check, password) {
            self.status("Wrong master password");
            return;
        }
        *self.password.borrow_mut() = Some(SecretString::from(password.to_owned()));
        self.window.set_pw_entry(SharedString::default());
        self.status("Unlocked");
        self.refresh();
    }

    /// Snapshot the metadata here, then encrypt and upload it to every
    /// chosen remote in the background.
    fn backup_metadata(&self) {
        let Some(password) = self.password.borrow().clone() else {
            self.status("Unlock with the master password first");
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
                    self.status(format!("Error: {e}"));
                    return;
                }
            }
        };
        let Some(config_path) = self.config_path() else { return };
        if remotes.is_empty() {
            self.status("Tick at least one remote to back up to");
            return;
        }
        self.status("Encrypting metadata…");
        self.spawn(move |tx| {
            let sealed = match metadata::seal(&snapshot, password.expose_secret()) {
                Ok(sealed) => sealed,
                Err(e) => {
                    tx.send(Msg::Status(format!("Metadata backup failed: {e}"))).ok();
                    return;
                }
            };
            let total = remotes.len();
            let mut done = 0;
            let configured = mbr::rclone::with_config(&config_path, || {
                for remote in remotes {
                    let result = metadata::upload(&remote.target, &sealed, &scratch);
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
            if let Err(e) = configured {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
        });
    }

    /// Download and decrypt the backup on `remote_name` in the background;
    /// it is merged in when it arrives (see [`Msg::MetadataFetched`]). An
    /// empty `password` means the one this folder is unlocked with.
    fn restore_metadata(&self, remote_name: &str, password: &str) {
        let Some(remote) = self.remote(remote_name) else { return };
        let Some((root, scratch)) = self
            .repo
            .borrow()
            .as_ref()
            .map(|repo| (repo.root.clone(), repo.scratch_dir()))
        else {
            return;
        };
        let Some(config_path) = self.config_path() else { return };
        let password = if password.is_empty() {
            match self.password.borrow().clone() {
                Some(unlocked) => unlocked,
                None => {
                    self.status("Enter the backup's master password");
                    return;
                }
            }
        } else {
            SecretString::from(password.to_owned())
        };
        self.status(format!("Fetching the metadata backup from '{remote_name}'…"));
        self.spawn(move |tx| {
            let result = mbr::rclone::with_config(&config_path, || {
                metadata::download(&remote.target, &scratch)
            })
            .and_then(|sealed| metadata::unseal(&sealed, password.expose_secret()));
            tx.send(Msg::MetadataFetched {
                root,
                password,
                result,
            })
            .ok();
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

        self.status(format!("Uploading {} blob(s) to {remote_name}…", blobs.len()));
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
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
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
        let unheld = jobs.iter().filter(|(_, holders)| holders.is_empty()).count();
        let jobs: Vec<(String, Vec<Remote>)> = jobs
            .into_iter()
            .filter(|(_, holders)| !holders.is_empty())
            .collect();
        if jobs.is_empty() {
            self.status("None of these blobs is recorded on any remote; nothing to download");
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
            if unheld > 0 {
                text += &format!("; {unheld} not recorded on any remote");
            }
            if let Some(e) = configured.err().or(failed) {
                text += &format!("; error: {e}");
            }
            tx.send(Msg::Status(text)).ok();
        });
    }

    /// Download everything recorded on remote `name` that is missing here.
    fn download_all(&self, name: &str) {
        let Some(remote) = self.remote(name) else { return };
        let missing = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            match repo.blobs_missing_locally(name) {
                Ok(missing) => missing,
                Err(e) => {
                    self.status(format!("Error: {e}"));
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
                    self.status(format!("Error: {e}"));
                    return;
                }
            }
        };
        let remotes: Vec<Remote> = remotes
            .into_iter()
            .filter(|r| only.is_none_or(|name| r.name == name))
            .collect();
        let Some(config_path) = self.config_path() else { return };
        if remotes.is_empty() {
            self.status("No remotes to scan");
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

    // ── editing a remote ──

    /// Open the editor for remote `name` and read its rclone settings.
    fn edit_remote(&self, name: &str) {
        let Some(remote) = self.remote(name) else { return };
        let section = mbr::rclone::target_section(&remote.target).map(str::to_owned);
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
        w.set_edit_target(remote.target.as_str().into());
        w.set_edit_section(section.as_deref().unwrap_or_default().into());
        w.set_edit_fields(ModelRc::default());
        w.set_edit_values(ModelRc::default());
        w.set_editing(true);

        let (Some(section), Some(config_path)) = (section, self.config_path()) else {
            w.set_edit_note("The target is a plain path, so there are no rclone settings.".into());
            return;
        };
        w.set_edit_note("Reading rclone settings…".into());
        self.spawn(move |tx| {
            let result = mbr::rclone::with_config(&config_path, || {
                mbr::rclone::remote_config(&section)
            });
            tx.send(Msg::RemoteConfig {
                remote: remote.name,
                section,
                result,
            })
            .ok();
        });
    }

    /// Show the settings read for the remote being edited.
    fn show_remote_config(
        &self,
        section: &str,
        config: Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        let mut edit = self.edit.borrow_mut();
        let Some(edit) = edit.as_mut() else { return };
        let w = &self.window;
        let Some(config) = config else {
            w.set_edit_note(
                format!(
                    "There is no rclone remote '{section}' in this folder's rclone.conf; \
                     only the name and target can be changed."
                )
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
                    "rclone remote '{section}' uses the '{kind}' backend, which has no form \
                     here; change its settings with `mbr remote edit --set`."
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
        w.set_edit_note(
            format!("rclone remote '{section}' ({}).", backend.title).into(),
        );
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
        let target = w.get_edit_target().trim().to_owned();
        if name.is_empty() || target.is_empty() {
            self.status("Error: the name and target must not be empty");
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
                let Some(config_path) = self.config_path() else { return };
                self.status(format!("Saving rclone settings of '{section}'…"));
                self.spawn(move |tx| {
                    let result = mbr::rclone::with_config(&config_path, || {
                        mbr::rclone::update_remote(
                            &section,
                            serde_json::Value::Object(parameters),
                        )
                    });
                    tx.send(Msg::RemoteEdited {
                        old,
                        name,
                        target,
                        result,
                    })
                    .ok();
                });
            }
            _ => self.finish_remote_edit(&old, &name, &target),
        }
    }

    /// Record a remote's new name and target, and close the editor.
    fn finish_remote_edit(&self, old: &str, name: &str, target: &str) {
        let mut ok = false;
        self.with_repo("save remote", |repo| {
            repo.db.update_remote(old, name, target)?;
            ok = true;
            Ok(format!("Saved remote {name} -> {target}"))
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
        let Some(backend) = backends::BACKENDS.iter().find(|b| b.title == title) else {
            return;
        };
        self.window
            .set_setup_backend(SharedString::from(backend.name));
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

    /// Submit the filled form: hand every answer to rclone in one call.
    fn submit_form(&self, name: &str, backend_name: &str, values: &[SharedString]) {
        let Some(backend) = backends::backend(backend_name) else {
            self.status(format!("Unknown backend '{backend_name}'"));
            return;
        };
        for (field, value) in backend.fields.iter().zip(values.iter()) {
            if field.required && value.trim().is_empty() {
                self.status(format!("Error: '{}' is required", field.label));
                return;
            }
        }
        let spec = backends::target_spec(backend, values);
        if let Err(e) = backends::check_target_spec(backend, &spec) {
            self.status(format!("Error: {e}"));
            return;
        }
        let parameters = backends::form_parameters(backend, values);
        let target = backends::target_for(name, backend, &spec);
        let backend_name = backend_name.to_owned();
        self.run_setup_step(name, |tx, name, config_path| {
            let parameters = parameters.clone();
            let target = target.clone();
            Box::new(move || {
                let result = mbr::rclone::with_config(&config_path, || {
                    mbr::rclone::begin_setup(&name, &backend_name, parameters)
                });
                tx.send(Msg::Setup {
                    name,
                    target,
                    result,
                })
                .ok();
            })
        });
    }

    /// Continue after answering a fallback question.
    fn submit_remote_setup(&self, name: &str, backend_name: &str, state: &str, answer: &str) {
        let values: Vec<SharedString> = self.window.get_form_values().iter().collect();
        let backend = backends::backend(backend_name);
        let parameters = match backend {
            Some(b) => backends::form_parameters(b, &values),
            None => serde_json::json!({}),
        };
        let target = match backend {
            Some(b) => backends::target_for(name, b, &backends::target_spec(b, &values)),
            None => format!("{name}:{}", backends::DEFAULT_PATH),
        };
        let state = state.to_owned();
        let answer = answer.to_owned();
        self.run_setup_step(name, |tx, name, config_path| {
            let parameters = parameters.clone();
            let state = state.clone();
            let answer = answer.clone();
            let target = target.clone();
            Box::new(move || {
                let mut on_url = |url| {
                    let _ = tx.send(Msg::OAuthUrl(url));
                };
                let result = mbr::rclone::with_config(&config_path, || {
                    mbr::rclone::continue_setup_watching(
                        &name,
                        &state,
                        &answer,
                        parameters,
                        &mut on_url,
                    )
                });
                tx.send(Msg::Setup {
                    name,
                    target,
                    result,
                })
                .ok();
            })
        });
    }

    /// Run one config-protocol step on a background thread.
    fn run_setup_step(
        &self,
        name: &str,
        step: impl FnOnce(mpsc::Sender<Msg>, String, PathBuf) -> Box<dyn FnOnce() + Send>,
    ) {
        let config_path = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else {
                self.status("Cannot configure remote: no folder open");
                return;
            };
            if repo.db.remote(name).ok().flatten().is_some() {
                self.status(format!("Error: remote '{name}' already exists"));
                return;
            }
            repo.rclone_config_path()
        };
        self.begin_task();
        self.status("Talking to rclone…");
        let tx = self.tx.clone();
        let name = name.to_owned();
        let step = step(tx.clone(), name.clone(), config_path);
        std::thread::spawn(move || {
            step();
            tx.send(Msg::Done).ok();
        });
    }

    fn cancel_remote_setup(&self) {
        let state = self.window.get_setup_state().to_string();
        let name = self.window.get_new_remote_name().to_string();
        let config_path = self.repo.borrow().as_ref().map(Repo::rclone_config_path);
        self.reset_setup_ui();
        if let (false, Some(config_path)) = (state.is_empty(), config_path) {
            self.begin_task();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let result = mbr::rclone::with_config(&config_path, || {
                    mbr::rclone::cancel_setup(&name, &state)
                });
                tx.send(Msg::Cancelled { result }).ok();
                tx.send(Msg::Done).ok();
            });
        }
    }

    /// Clear the form and question panels.
    fn reset_setup_ui(&self) {
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
                Err(e) => self.status(format!("Upload to {remote} failed: {e}")),
            },
            Msg::Fetched { hash, result } => match result {
                Ok(remote) => {
                    self.status(format!("Downloaded {} from {remote}", util::short_hash(&hash)))
                }
                Err(e) => self.status(format!("Download failed: {e}")),
            },
            Msg::Checked {
                remote,
                hash,
                result,
            } => match result {
                Ok(size) => self.with_repo("record check", |repo| {
                    repo.record_blob_check(&remote, &hash, size)?;
                    let verdict = if size.is_some() { "stored" } else { "not stored" };
                    Ok(format!("{} is {verdict} on {remote}", util::short_hash(&hash)))
                }),
                Err(e) => self.status(format!("Check on {remote} failed: {e}")),
            },
            Msg::Status(text) => self.status(text),
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
                    if !check.missing.is_empty() {
                        text += &format!(", {} MISSING", check.missing.len());
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
                    Ok(text)
                }),
                Err(e) => self.status(format!("Remote '{remote}' scan failed: {e}")),
            },
            Msg::RemoteConfig {
                remote,
                section,
                result,
            } => {
                let current = self.edit.borrow().as_ref().map(|e| e.original.clone());
                if current.as_deref() != Some(remote.as_str()) {
                    return; // the editor was closed or moved on meanwhile
                }
                match result {
                    Ok(config) => self.show_remote_config(&section, config),
                    Err(e) => self
                        .window
                        .set_edit_note(format!("Could not read rclone settings: {e}").into()),
                }
            }
            Msg::RemoteEdited {
                old,
                name,
                target,
                result,
            } => match result {
                Ok(()) => self.finish_remote_edit(&old, &name, &target),
                Err(e) => self.status(format!("Saving rclone settings failed: {e}")),
            },
            Msg::Setup {
                name,
                target,
                result,
            } => match result {
                Err(e) => {
                    self.window
                        .set_setup_help(SharedString::from(format!("Error: {e}")));
                    self.window.set_setup_state(SharedString::default());
                    self.status(format!("Remote setup failed: {e}"));
                }
                Ok(mbr::rclone::SetupOutcome::Done) => {
                    self.with_repo("save remote", |repo| {
                        repo.db.add_remote(&name, &target)?;
                        Ok(format!("Added remote {name} -> {target}"))
                    });
                    self.reset_setup_ui();
                    self.refresh();
                }
                Ok(mbr::rclone::SetupOutcome::Question(q)) => {
                    self.window.set_setup_state(SharedString::from(q.state));
                    self.window.set_setup_question(SharedString::from(q.name));
                    self.window.set_setup_help(SharedString::from(q.help));
                    self.window.set_setup_default(SharedString::from(q.default));
                    self.window.set_setup_answer(SharedString::default());
                    self.window.set_setup_url(SharedString::default());
                    self.status("rclone needs one more answer to finish this remote");
                }
            },
            Msg::OAuthUrl(url) => {
                self.window.set_setup_url(SharedString::from(url.as_str()));
                self.status(
                    "Waiting for browser sign-in… (use the link below if no browser opened)",
                );
            }
            Msg::Cancelled { result } => {
                if let Err(e) = result {
                    self.status(format!("Note: {e}"));
                }
            }
            Msg::MetadataUploaded {
                root,
                remote,
                db_hash,
                result,
            } => match result {
                Ok(()) => {
                    self.state.borrow_mut().repo_mut(&root).metadata_backups.insert(
                        remote.clone(),
                        BackupRecord {
                            at: util::now(),
                            db_hash,
                        },
                    );
                    self.save_state();
                    self.status(format!("Metadata backed up to {remote}"));
                }
                Err(e) => self.status(format!("Metadata backup to {remote} failed: {e}")),
            },
            Msg::MetadataFetched {
                root,
                password,
                result,
            } => {
                if self.root().as_ref() != Some(&root) {
                    return; // another folder was opened meanwhile
                }
                let (db, conf) = match result {
                    Ok(plain) => plain,
                    Err(e) => {
                        self.status(format!("Restore failed: {e}"));
                        return;
                    }
                };
                let mut restored = false;
                self.with_repo("restore", |repo| {
                    let report = repo.restore_metadata(&db, &conf)?;
                    restored = true;
                    let mut text = format!(
                        "Restored {} file(s) and {} remote(s), {} older version(s)",
                        report.files, report.remotes, report.history
                    );
                    if report.files > 0 {
                        text += "; download the files from the Remotes tab";
                    }
                    if !report.conflicts.is_empty() {
                        text += &format!(
                            "; kept this folder's target for {}",
                            report.conflicts.join(", ")
                        );
                    }
                    if !report.skipped.is_empty() {
                        text += &format!("; {} skipped", report.skipped.len());
                    }
                    Ok(text)
                });
                if restored {
                    // A folder without a password adopts the backup's; one
                    // with its own keeps it (the backup may predate a change).
                    let check = self.state.borrow().repo(&root).password_check;
                    match check {
                        None => {
                            if let Ok(check) = metadata::password_check(password.expose_secret()) {
                                self.state.borrow_mut().repo_mut(&root).password_check =
                                    Some(check);
                                self.save_state();
                                *self.password.borrow_mut() = Some(password);
                            }
                        }
                        Some(check)
                            if metadata::password_matches(&check, password.expose_secret()) =>
                        {
                            *self.password.borrow_mut() = Some(password);
                        }
                        Some(_) => {}
                    }
                    self.window.set_pw_restore(SharedString::default());
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
                            format!(
                                "Freed {} ({} is on {})",
                                util::format_size(size),
                                util::short_hash(&outcome.hash),
                                outcome.verified.join(", ")
                            )
                        } else {
                            outcome.kept_reason(min_copies)
                        })
                    });
                }
                Err(e) => self.status(format!("Could not free space: {e}")),
            },
            Msg::VerifiedLocal { hash, result } => match result {
                Ok(LocalHealth::Corrupt { moved_to }) => self.status(format!(
                    "{} is DAMAGED; moved to {}",
                    util::short_hash(&hash),
                    moved_to.display()
                )),
                Ok(_) => self.status(format!("{} verified", util::short_hash(&hash))),
                Err(e) => self.status(format!("Verifying failed: {e}")),
            },
            Msg::VerifiedRemote {
                remote,
                hash,
                result,
            } => match result {
                Ok(health) => self.with_repo("record verification", |repo| {
                    repo.record_remote_health(&remote, &hash, &health)?;
                    let verdict = match health {
                        RemoteHealth::Ok => "intact",
                        RemoteHealth::Missing => "MISSING",
                        RemoteHealth::Corrupt => "DAMAGED (deleted there)",
                    };
                    Ok(format!("{} is {verdict} on {remote}", util::short_hash(&hash)))
                }),
                Err(e) => self.status(format!("Verifying on {remote} failed: {e}")),
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
                    Err(e) => self.status(format!("Scan failed: {e}")),
                }
            }
            Msg::Done => {
                let left = self.tasks.get().saturating_sub(1);
                self.tasks.set(left);
                self.window.set_busy(left > 0);
            }
        }
    }

    // ── view refresh ──

    /// Rebuild every model from the repo (cheap; local data only).
    fn refresh(&self) {
        let mut repo = self.repo.borrow_mut();
        let Some(repo) = repo.as_mut() else {
            self.window.set_folder(SharedString::default());
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
                let rows: Vec<FileRow> = statuses
                    .iter()
                    .map(|s| file_row(s, selection.paths.contains(&s.path), min_copies))
                    .collect();
                self.window.set_at_risk_count(rows.iter().filter(|r| r.at_risk).count() as i32);
                // Same model, new contents: the list keeps its scroll position.
                self.files.set_vec(rows);
                *self.statuses.borrow_mut() = statuses;
            }
            Err(e) => self.status(format!("Error listing files: {e}")),
        }

        match repo.db.list_remotes() {
            Ok(remotes) => {
                let names: Vec<SharedString> =
                    remotes.iter().map(|r| r.name.as_str().into()).collect();
                let rows: Vec<RemoteRow> = remotes
                    .iter()
                    .map(|r| RemoteRow {
                        name: r.name.as_str().into(),
                        target: r.target.as_str().into(),
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
            Err(e) => self.status(format!("Error listing remotes: {e}")),
        }

        match repo.unreferenced_blobs() {
            Ok(old) => {
                let here: Vec<_> = old.iter().filter(|u| u.present_locally).collect();
                self.window.set_old_local_count(here.len() as i32);
                self.window.set_old_local_size(
                    util::format_size(here.iter().map(|u| u.size).sum()).into(),
                );
            }
            Err(e) => self.status(format!("Error listing old versions: {e}")),
        }

        self.refresh_metadata(repo);
        self.show_selection();
    }

    /// Rebuild the Metadata backup tab.
    fn refresh_metadata(&self, repo: &mut Repo) {
        let w = &self.window;
        let state = self.state.borrow().repo(&repo.root);
        w.set_password_set(state.password_check.is_some());
        w.set_unlocked(self.password.borrow().is_some());
        w.set_folder_empty(repo.is_empty_folder().unwrap_or(false));

        let (remotes, chosen) = match repo.db.list_remotes().and_then(|all| {
            let chosen: HashSet<String> =
                repo.db.metadata_remotes()?.into_iter().map(|r| r.name).collect();
            Ok((all, chosen))
        }) {
            Ok(found) => found,
            Err(e) => {
                self.status(format!("Error listing metadata remotes: {e}"));
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
                    target: r.target.as_str().into(),
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
                    format!("Metadata is backed up and up to date (last backup {}).", util::format_time(at))
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
    let examples: Vec<SharedString> = owned.iter().map(|v| SharedString::from(v.as_str())).collect();
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
    }
}

fn confirm_init(dir: &std::path::Path) -> bool {
    use rfd::MessageButtons;
    rfd::MessageDialog::new()
        .set_title("Attach to mbr?")
        .set_description(format!(
            "{} is not an mbr folder.\n\nAttaching it will move its files into a hidden .mbr/blob store and replace them with symlinks.\n\nInitialize it?",
            dir.display()
        ))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
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
