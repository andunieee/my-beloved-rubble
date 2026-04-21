//! `my-beloved-rubble` — the Slint GUI.
//!
//! rclone operations (push / fetch / check) run on background threads and
//! report back through an mpsc channel drained by a UI timer, so the window
//! stays responsive; database writes always happen on the UI thread.

use mbr::backends::{self, Kind};
use mbr::db::Remote;
use mbr::repo::{self, Repo};
use mbr::util;
use slint::{Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

slint::include_modules!();

/// Completion messages from background rclone threads.
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
        result: Result<Vec<String>, String>,
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
}

struct App {
    window: AppWindow,
    repo: Rc<RefCell<Option<Repo>>>,
    tx: mpsc::Sender<Msg>,
    tasks: Rc<Cell<u32>>,
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();

    let window = AppWindow::new()?;
    let (tx, rx) = mpsc::channel::<Msg>();
    let app = Rc::new(App {
        window,
        repo: Rc::new(RefCell::new(None)),
        tx,
        tasks: Rc::new(Cell::new(0)),
    });

    // ── message pump for background rclone tasks ──
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
    {
        let app = app.clone();
        app.clone().window.on_open_folder(move || {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                app.attach(dir);
            }
        });
    }
    {
        let app = app.clone();
        app.clone().window.on_rescan(move || app.rescan());
    }
    {
        let app = app.clone();
        app.clone().window.on_add_remote(move |name, target| {
            app.with_repo("add remote", |repo| {
                if repo.db.remote(&name)?.is_some() {
                    return Err(format!("remote '{name}' already exists"));
                }
                repo.db.add_remote(&name, &target)?;
                Ok(format!("Added remote {name} -> {target}"))
            });
            app.window.set_new_remote_name(SharedString::default());
            app.window.set_new_remote_target(SharedString::default());
            app.refresh();
        });
    }
    {
        let app = app.clone();
        app.clone().window.on_select_backend(move |name| {
            app.select_backend(&name);
        });
    }
    {
        let app = app.clone();
        app.clone()
            .window
            .on_submit_form(move |name, backend, values| {
                let values: Vec<SharedString> = values.iter().collect();
                app.submit_form(&name, &backend, &values);
            });
    }
    {
        let app = app.clone();
        app.clone()
            .window
            .on_submit_remote_setup(move |name, backend, state, answer| {
                app.submit_remote_setup(&name, &backend, &state, &answer);
            });
    }
    {
        let app = app.clone();
        app.clone().window.on_cancel_remote_setup(move || {
            app.cancel_remote_setup();
        });
    }
    {
        let app = app.clone();
        app.clone().window.on_remove_remote(move |name| {
            app.with_repo("remove remote", |repo| {
                repo.db.remove_remote(&name)?;
                Ok(format!("Removed remote {name}"))
            });
            app.refresh();
        });
    }
    {
        let app = app.clone();
        app.clone()
            .window
            .on_check_remote(move |name| app.check_remote(&name));
    }
    {
        let app = app.clone();
        app.clone()
            .window
            .on_push_file(move |hash, remote| app.push_blobs(&remote, vec![hash.to_string()]));
    }
    {
        let app = app.clone();
        app.clone().window.on_push_all(move |remote| {
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
        app.clone()
            .window
            .on_fetch_file(move |hash| app.fetch_blob(&hash));
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

    /// Attach `dir`, initializing it first if it is not an mbr folder.
    fn attach(&self, dir: PathBuf) {
        if !Repo::is_initialized(&dir) && !confirm_init(&dir) {
            self.status(format!("Did not attach {}", dir.display()));
            return;
        }
        match Repo::init(&dir) {
            Ok(repo) => {
                *self.repo.borrow_mut() = Some(repo);
                save_last_folder(&dir);
                self.rescan();
            }
            Err(e) => self.status(format!("Error: {e}")),
        }
    }

    fn rescan(&self) {
        self.with_repo("scan", |repo| {
            let report = repo.scan()?;
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
        });
        self.refresh();
    }

    // ── background rclone tasks ──

    fn begin_task(&self) {
        self.tasks.set(self.tasks.get() + 1);
        self.window.set_busy(true);
    }

    fn push_blobs(&self, remote_name: &str, hashes: Vec<String>) {
        let (remote, blobs, config_path) = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let remote = match repo.db.remote(remote_name) {
                Ok(Some(remote)) => remote,
                Ok(None) => {
                    self.status(format!("No remote named '{remote_name}'"));
                    return;
                }
                Err(e) => {
                    self.status(format!("Error: {e}"));
                    return;
                }
            };
            let blobs: Vec<(String, PathBuf)> = hashes
                .iter()
                .map(|h| (h.clone(), repo.blob_path(h)))
                .collect();
            let config_path = repo.rclone_config_path();
            (remote, blobs, config_path)
        };

        self.begin_task();
        self.status(format!("Pushing {} blob(s) to {remote_name}…", blobs.len()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = mbr::rclone::set_config_path(&config_path) {
                tx.send(Msg::Pushed {
                    remote: remote.name.clone(),
                    hash: String::new(),
                    result: Err(e),
                })
                .ok();
                tx.send(Msg::Done).ok();
                return;
            }
            for (hash, blob) in blobs {
                let result = mbr::rclone::push_blob(&remote.target, &blob, &hash);
                tx.send(Msg::Pushed {
                    remote: remote.name.clone(),
                    hash,
                    result,
                })
                .ok();
            }
            tx.send(Msg::Done).ok();
        });
    }

    fn fetch_blob(&self, hash: &str) {
        let (holders, blob, config_path) = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let names = match repo.db.remotes_for_blob(hash) {
                Ok(names) => names,
                Err(e) => {
                    self.status(format!("Error: {e}"));
                    return;
                }
            };
            let mut holders: Vec<Remote> = Vec::new();
            for name in names {
                if let Ok(Some(remote)) = repo.db.remote(&name) {
                    holders.push(remote);
                }
            }
            (holders, repo.blob_path(hash), repo.rclone_config_path())
        };
        if holders.is_empty() {
            self.status("This blob is not recorded on any remote; nothing to fetch from");
            return;
        }

        self.begin_task();
        self.status(format!("Fetching {}…", util::short_hash(hash)));
        let tx = self.tx.clone();
        let hash = hash.to_owned();
        std::thread::spawn(move || {
            let result = mbr::rclone::set_config_path(&config_path)
                .and_then(|()| repo::download_blob(&holders, &hash, &blob));
            tx.send(Msg::Fetched { hash, result }).ok();
            tx.send(Msg::Done).ok();
        });
    }

    fn check_remote(&self, name: &str) {
        let (remote, config_path) = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            let remote = match repo.db.remote(name) {
                Ok(Some(remote)) => remote,
                _ => return,
            };
            let config_path = repo.rclone_config_path();
            (remote, config_path)
        };

        self.begin_task();
        self.status(format!("Checking remote '{name}'…"));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = mbr::rclone::set_config_path(&config_path)
                .and_then(|()| mbr::rclone::list_blobs(&remote.target));
            tx.send(Msg::Listed {
                remote: remote.name,
                result,
            })
            .ok();
            tx.send(Msg::Done).ok();
        });
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
            .map(|field| {
                let examples: Vec<SharedString> = field
                    .examples
                    .iter()
                    .map(|e| SharedString::from(*e))
                    .collect();
                let default_index = field
                    .examples
                    .iter()
                    .position(|e| *e == field.default)
                    .map(|i| i as i32)
                    .unwrap_or(0);
                FormField {
                    key: field.name.into(),
                    label: field.label.into(),
                    help: field.help.into(),
                    kind: match field.kind {
                        Kind::Text => 0,
                        Kind::Secret => 1,
                        Kind::Choice => 2,
                    },
                    default: field.default.into(),
                    default_index,
                    examples: ModelRc::new(VecModel::from(examples)),
                    required: field.required,
                }
            })
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
        let parameters = form_parameters(backend, values);
        let target = backends::target_for(backend, &bucket_value(backend, values));
        let backend_name = backend_name.to_owned();
        self.run_setup_step(name, |tx, name, config_path| {
            let parameters = parameters.clone();
            let target = target.clone();
            Box::new(move || {
                let result = mbr::rclone::set_config_path(&config_path).and_then(|()| {
                    if mbr::rclone::remote_exists(&name)? {
                        return Err(format!("rclone remote '{name}' already exists"));
                    }
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
            Some(b) => form_parameters(b, &values),
            None => serde_json::json!({}),
        };
        let target = match backend {
            Some(b) => backends::target_for(b, &bucket_value(b, &values)),
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
                let result = mbr::rclone::set_config_path(&config_path).and_then(|()| {
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
        let step = step(tx, name.clone(), config_path);
        std::thread::spawn(step);
    }

    fn cancel_remote_setup(&self) {
        let state = self.window.get_setup_state().to_string();
        self.reset_setup_ui();
        if !state.is_empty() {
            self.begin_task();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let result = mbr::rclone::cancel_setup(&state);
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
    }

    /// Apply a completion message from a background task (UI thread).
    fn handle(&self, msg: Msg) {
        match msg {
            Msg::Pushed {
                remote,
                hash,
                result,
            } => match result {
                Ok(()) => self.with_repo("record push", |repo| {
                    repo.db.set_blob_on_remote(&hash, &remote)?;
                    Ok(format!("Pushed {} to {remote}", util::short_hash(&hash)))
                }),
                Err(e) => self.status(format!("Push to {remote} failed: {e}")),
            },
            Msg::Fetched { hash, result } => match result {
                Ok(remote) => {
                    self.status(format!("Fetched {} from {remote}", util::short_hash(&hash)))
                }
                Err(e) => self.status(format!("Fetch failed: {e}")),
            },
            Msg::Listed { remote, result } => match result {
                Ok(found) => self.with_repo("reconcile remote", |repo| {
                    let Some(r) = repo.db.remote(&remote)? else {
                        return Err(format!("remote '{remote}' vanished"));
                    };
                    let check = repo.apply_remote_listing(&r, found.into_iter().collect())?;
                    let mut text = format!(
                        "Remote '{remote}' reachable; {} expected blob(s) present",
                        check.present.len()
                    );
                    if !check.missing.is_empty() {
                        text += &format!(", {} MISSING", check.missing.len());
                    }
                    if !check.discovered.is_empty() {
                        text += &format!(", {} discovered", check.discovered.len());
                    }
                    Ok(text)
                }),
                Err(e) => self.status(format!("Remote '{remote}' check failed: {e}")),
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

        // Preserve selection by path across refreshes.
        let files = self.window.get_files();
        let selected_path = {
            use slint::Model;
            let idx = self.window.get_selected_index();
            (idx >= 0)
                .then(|| files.row_data(idx as usize))
                .flatten()
                .map(|row| row.path)
        };

        match repo.status() {
            Ok(statuses) => {
                let selected = selected_path
                    .as_ref()
                    .and_then(|p| statuses.iter().position(|s| s.path == p.as_str()));
                let rows: Vec<FileRow> = statuses.iter().map(file_row).collect();
                self.window.set_files(ModelRc::new(VecModel::from(rows)));
                self.window
                    .set_selected_index(selected.map(|i| i as i32).unwrap_or(-1));
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
                            "{} blob(s)",
                            repo.db
                                .blobs_on_remote(&r.name)
                                .map(|b| b.len())
                                .unwrap_or(0)
                        )
                        .into(),
                    })
                    .collect();
                self.window
                    .set_remote_names(ModelRc::new(VecModel::from(names)));
                self.window.set_remotes(ModelRc::new(VecModel::from(rows)));
            }
            Err(e) => self.status(format!("Error listing remotes: {e}")),
        }
    }
}

/// Build the rclone `parameters` object from the form answers: non-empty
/// values keyed by config name, in the clear (rclone obscures secrets).
fn form_parameters(backend: &mbr::backends::Backend, values: &[SharedString]) -> serde_json::Value {
    let mut parameters = serde_json::Map::new();
    for (field, value) in backend.fields.iter().zip(values.iter()) {
        let value = value.trim();
        if !value.is_empty() {
            parameters.insert(field.name.to_owned(), serde_json::Value::from(value));
        }
    }
    serde_json::Value::Object(parameters)
}

/// The bucket/path answer used to build the mbr target: the first field
/// flagged [`mbr::backends::Field::to_target`], else the mbr default.
fn bucket_value(backend: &mbr::backends::Backend, values: &[SharedString]) -> String {
    for (field, value) in backend.fields.iter().zip(values.iter()) {
        if field.to_target {
            return value.trim().to_owned();
        }
    }
    String::new()
}

fn file_row(s: &repo::FileStatus) -> FileRow {
    let versions = s
        .previous_versions
        .iter()
        .map(|v| {
            let stored = if v.present_locally && v.remotes.is_empty() {
                "local only".to_owned()
            } else if v.present_locally {
                format!("local, {}", v.remotes.join(", "))
            } else if v.remotes.is_empty() {
                "NOT STORED ANYWHERE".to_owned()
            } else {
                v.remotes.join(", ")
            };
            format!(
                "{} · {} · removed {} · {stored}",
                util::short_hash(&v.hash),
                util::format_size(v.size),
                util::format_time(v.removed_at),
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
