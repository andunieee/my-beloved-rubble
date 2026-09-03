//! `my-beloved-rubble` — the Slint GUI.
//!
//! rclone operations (push / fetch / check) run on background threads and
//! report back through an mpsc channel drained by a UI timer, so the window
//! stays responsive; database writes always happen on the UI thread.

use mbr::db::Remote;
use mbr::repo::{self, Repo};
use mbr::util;
use slint::{ModelRc, SharedString, VecModel};
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
        app.clone().window.on_check_remote(move |name| app.check_remote(&name));
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
        app.clone().window.on_fetch_file(move |hash| app.fetch_blob(&hash));
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

    /// Attach (initializing if needed), scan, and display a folder.
    fn attach(&self, dir: PathBuf) {
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
        let (remote, blobs) = {
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
            (remote, blobs)
        };

        self.begin_task();
        self.status(format!("Pushing {} blob(s) to {remote_name}…", blobs.len()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
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
        let (holders, blob) = {
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
            (holders, repo.blob_path(hash))
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
            let result = repo::download_blob(&holders, &hash, &blob);
            tx.send(Msg::Fetched { hash, result }).ok();
            tx.send(Msg::Done).ok();
        });
    }

    fn check_remote(&self, name: &str) {
        let remote = {
            let mut repo = self.repo.borrow_mut();
            let Some(repo) = repo.as_mut() else { return };
            match repo.db.remote(name) {
                Ok(Some(remote)) => remote,
                _ => return,
            }
        };

        self.begin_task();
        self.status(format!("Checking remote '{name}'…"));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = mbr::rclone::list_blobs(&remote.target);
            tx.send(Msg::Listed {
                remote: remote.name,
                result,
            })
            .ok();
            tx.send(Msg::Done).ok();
        });
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
                Ok(remote) => self.status(format!(
                    "Fetched {} from {remote}",
                    util::short_hash(&hash)
                )),
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
                            repo.db.blobs_on_remote(&r.name).map(|b| b.len()).unwrap_or(0)
                        )
                        .into(),
                    })
                    .collect();
                self.window
                    .set_remote_names(ModelRc::new(VecModel::from(names)));
                self.window
                    .set_remotes(ModelRc::new(VecModel::from(rows)));
            }
            Err(e) => self.status(format!("Error listing remotes: {e}")),
        }
    }
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
    let path = PathBuf::from(
        std::fs::read_to_string(last_folder_file()?)
            .ok()?
            .trim(),
    );
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
