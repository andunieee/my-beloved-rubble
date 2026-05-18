//! `my-beloved-rubble` — the Slint GUI.
//!
//! Two tabs: **Blobs** (the annotated file listing, with multi-selection
//! and a sidebar acting on the selected blobs) and **Remotes** (listing,
//! scanning, downloading from, adding, setting up and editing remotes).
//!
//! Slow work — rclone operations (push / fetch / list / stat / config) and
//! the filesystem half of a scan (hashing, moving files into the store) —
//! runs on background threads and reports back through an mpsc channel
//! drained by a UI timer, so the window stays responsive; database writes
//! always happen on the UI thread.

use mbr::backends::{self, Backend, Field, Kind};
use mbr::db::Remote;
use mbr::rclone::RemoteBlob;
use mbr::repo::{self, Repo};
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
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();

    let window = AppWindow::new()?;
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
    if let Some(dir) = load_last_folder() {
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
                save_last_folder(&repo.root);
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
                form_field(f, value, placeholder)
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
            .map(|b| b.fields.to_vec())
            .unwrap_or_default()
            .iter()
            .map(|field| form_field(field, field.default, field.default))
            .collect();
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
        self.window.set_setup_password(false);
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
                    self.window.set_setup_password(q.password);
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

        match repo.status() {
            Ok(statuses) => {
                let mut selection = self.selection.borrow_mut();
                let listed: HashSet<&str> = statuses.iter().map(|s| s.path.as_str()).collect();
                selection.paths.retain(|p| listed.contains(p.as_str()));
                let rows: Vec<FileRow> = statuses
                    .iter()
                    .map(|s| file_row(s, selection.paths.contains(&s.path)))
                    .collect();
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
        self.show_selection();
    }
}

/// One form row for `field`, prefilled with `value` (for choices: the
/// option preselected), showing `placeholder` while empty.
fn form_field(field: &Field, value: &str, placeholder: &str) -> FormField {
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
        help: field.help.into(),
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

fn file_row(s: &repo::FileStatus, selected: bool) -> FileRow {
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
    }
}

// ── remembering the last opened folder ──

fn last_folder_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("mbr").join("last-folder"))
}

fn load_last_folder() -> Option<PathBuf> {
    let path = PathBuf::from(std::fs::read_to_string(last_folder_file()?).ok()?.trim());
    path.join(repo::DB_FILE).is_file().then_some(path)
}

fn save_last_folder(dir: &std::path::Path) {
    if let Some(file) = last_folder_file() {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(file, dir.display().to_string()).ok();
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
