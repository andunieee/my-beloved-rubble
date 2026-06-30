//! The actions in the Activity view: uploads, downloads, scans of remotes,
//! checks and verifications. They queue up and run one at a time on a
//! background thread, in the order they were started, so starting one
//! while another runs is always fine. The most recent finished ones are
//! kept in memory and written now and then to `<cache dir>/mbr/actions.json`
//! ([`AppState::cache_dir`]), so the list survives a restart.

use super::{ActionRow, Level, Msg};
use mbr::state::AppState;
use mbr::util;
use serde::{Deserialize, Serialize};
use slint::SharedString;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

/// Finished actions kept.
const HISTORY_LEN: usize = 100;

/// What an action does on its background thread: returns its summary.
pub type Work = Box<dyn FnOnce(&Ctx) -> (Level, String) + Send>;

/// What a running action reports through.
pub struct Ctx {
    id: u64,
    tx: mpsc::Sender<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Ctx {
    pub fn send(&self, msg: Msg) {
        self.tx.send(msg).ok();
    }

    /// The user cancelled the action: stop before the next item.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `done` of `total` items are done; `doing` is the next one.
    pub fn progress(&self, done: usize, total: usize, doing: impl Into<String>) {
        self.send(Msg::Progress {
            id: self.id,
            done,
            total,
            doing: doing.into(),
        });
    }
}

/// How a finished action went.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    Warning,
    Failed,
    Cancelled,
}

/// A finished action, as the history keeps it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub title: String,
    /// The name of the folder it was about.
    pub folder: String,
    pub started: i64,
    pub finished: i64,
    pub outcome: Outcome,
    pub summary: String,
}

struct Queued {
    id: u64,
    title: String,
    root: PathBuf,
    queued: i64,
    work: Work,
}

struct Running {
    id: u64,
    title: String,
    root: PathBuf,
    started: i64,
    done: usize,
    total: usize,
    doing: String,
    /// What it found, to summarize it with (see [`Actions::note`]).
    notes: Vec<(Level, String)>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Actions {
    next_id: u64,
    running: Option<Running>,
    queued: VecDeque<Queued>,
    /// Newest first.
    history: VecDeque<Record>,
    /// The history changed since it was saved.
    unsaved: bool,
}

fn history_path() -> Option<PathBuf> {
    Some(AppState::cache_dir()?.join("actions.json"))
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .unwrap_or(root.as_os_str())
        .to_string_lossy()
        .into_owned()
}

impl Actions {
    /// No actions running or queued, and the saved history.
    pub fn load() -> Self {
        let history = history_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| {
                serde_json::from_str(&text)
                    .map_err(|e| log::warn!("ignoring unreadable action history: {e}"))
                    .ok()
            })
            .unwrap_or_default();
        Actions {
            history,
            ..Self::default()
        }
    }

    /// Write the history, if it changed since the last time.
    pub fn save(&mut self) {
        if !self.unsaved {
            return;
        }
        let Some(path) = history_path() else { return };
        let written = serde_json::to_string(&self.history)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
            });
        match written {
            Ok(()) => self.unsaved = false,
            Err(e) => log::warn!("cannot save the action history to {}: {e}", path.display()),
        }
    }

    /// Queue `work` on the folder at `root`; how many actions are ahead of it.
    pub fn push(&mut self, title: String, root: PathBuf, work: Work) -> usize {
        self.next_id += 1;
        self.queued.push_back(Queued {
            id: self.next_id,
            title,
            root,
            queued: util::now(),
            work,
        });
        self.queued.len() - 1 + usize::from(self.running.is_some())
    }

    /// Start the next queued action if none is running: its title.
    pub fn start_next(&mut self, tx: &mpsc::Sender<Msg>) -> Option<String> {
        if self.running.is_some() {
            return None;
        }
        let Queued {
            id,
            title,
            root,
            work,
            ..
        } = self.queued.pop_front()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = Ctx {
            id,
            tx: tx.clone(),
            cancel: cancel.clone(),
        };
        std::thread::spawn(move || {
            let (level, text) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)))
                    .unwrap_or_else(|_| (Level::Error, "The action crashed".to_owned()));
            ctx.send(Msg::ActionDone { id, level, text });
        });
        self.running = Some(Running {
            id,
            title: title.clone(),
            root,
            started: util::now(),
            done: 0,
            total: 0,
            doing: String::new(),
            notes: Vec::new(),
            cancel,
        });
        Some(title)
    }

    /// The folder of the running action: the one its messages are about.
    pub fn running_root(&self) -> Option<&Path> {
        self.running.as_ref().map(|r| r.root.as_path())
    }

    pub fn progress(&mut self, id: u64, done: usize, total: usize, doing: String) {
        if let Some(r) = self.running.as_mut().filter(|r| r.id == id) {
            (r.done, r.total, r.doing) = (done, total, doing);
        }
    }

    /// What the running action found, worked out on the UI thread: its
    /// summary, rather than what its work returns.
    pub fn note(&mut self, level: Level, text: String) {
        if let Some(r) = self.running.as_mut() {
            r.notes.push((level, text));
        }
    }

    /// The running action `id` returned `summary`: into the history. Its
    /// outcome, and the summary and level to show.
    pub fn finish(&mut self, id: u64, level: Level, summary: String) -> (Outcome, Level, String) {
        let Some(r) = self.running.take_if(|r| r.id == id) else {
            return (Outcome::Done, level, summary);
        };
        let (level, summary) = if r.notes.is_empty() {
            (level, summary)
        } else {
            let worst = r
                .notes
                .iter()
                .map(|(l, _)| *l)
                .fold(level, |a, b| if (b as u8) > (a as u8) { b } else { a });
            let texts: Vec<&str> = r.notes.iter().map(|(_, t)| t.as_str()).collect();
            (worst, texts.join("; "))
        };
        let outcome = if r.cancel.load(Ordering::Relaxed) {
            Outcome::Cancelled
        } else {
            match level {
                Level::Info | Level::Success => Outcome::Done,
                Level::Warning => Outcome::Warning,
                Level::Error => Outcome::Failed,
            }
        };
        self.record(Record {
            title: r.title,
            folder: folder_name(&r.root),
            started: r.started,
            finished: util::now(),
            outcome,
            summary: summary.clone(),
        });
        (outcome, level, summary)
    }

    /// Cancel action `id`: one queued never runs, the running one stops
    /// after the item it is on.
    pub fn cancel(&mut self, id: u64) {
        if let Some(r) = self.running.as_ref().filter(|r| r.id == id) {
            r.cancel.store(true, Ordering::Relaxed);
        } else if let Some(i) = self.queued.iter().position(|q| q.id == id) {
            let q = self.queued.remove(i).unwrap();
            self.record_unstarted(q, "Cancelled before it started");
        }
    }

    /// Cancel every action, running or queued, because `why`.
    pub fn cancel_all(&mut self, why: &str) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
        while let Some(q) = self.queued.pop_back() {
            self.record_unstarted(q, why);
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.unsaved = true;
    }

    /// Actions running or queued.
    pub fn pending(&self) -> usize {
        self.queued.len() + usize::from(self.running.is_some())
    }

    fn record_unstarted(&mut self, q: Queued, why: &str) {
        self.record(Record {
            title: q.title,
            folder: folder_name(&q.root),
            started: q.queued,
            finished: util::now(),
            outcome: Outcome::Cancelled,
            summary: why.to_owned(),
        });
    }

    fn record(&mut self, record: Record) {
        self.history.push_front(record);
        self.history.truncate(HISTORY_LEN);
        self.unsaved = true;
    }

    /// The rows of the running action, then the queued ones.
    pub fn queue_rows(&self) -> Vec<ActionRow> {
        let running = self.running.iter().map(|r| ActionRow {
            id: r.id as i32,
            title: r.title.as_str().into(),
            detail: match (r.cancel.load(Ordering::Relaxed), r.total) {
                (true, _) => "Stopping after the current item…".into(),
                (false, 0) => "Starting…".into(),
                (false, total) if r.doing.is_empty() => format!("{} of {total}", r.done).into(),
                (false, total) => format!("{} of {total} · {}", r.done, r.doing).into(),
            },
            when: format!("started {}", util::format_time(r.started)).into(),
            state: 0,
            progress: if r.total == 0 {
                -1.0
            } else {
                r.done as f32 / r.total as f32
            },
        });
        let queued = self.queued.iter().map(|q| ActionRow {
            id: q.id as i32,
            title: q.title.as_str().into(),
            detail: "Waiting for the actions above".into(),
            when: format!("queued {}", util::format_time(q.queued)).into(),
            state: 1,
            progress: -1.0,
        });
        running.chain(queued).collect()
    }

    /// The finished actions, newest first.
    pub fn history_rows(&self) -> Vec<ActionRow> {
        self.history
            .iter()
            .map(|r| ActionRow {
                id: 0,
                title: r.title.as_str().into(),
                detail: r.summary.as_str().into(),
                when: SharedString::from(format!(
                    "{} · {} · took {}",
                    r.folder,
                    util::format_time(r.finished),
                    format_duration(r.finished - r.started)
                )),
                state: match r.outcome {
                    Outcome::Done => 2,
                    Outcome::Warning => 3,
                    Outcome::Failed => 4,
                    Outcome::Cancelled => 5,
                },
                progress: -1.0,
            })
            .collect()
    }
}

/// `secs` as "12s", "3m 4s" or "2h 5m".
fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> Work {
        Box::new(|_| (Level::Success, "ok".to_owned()))
    }

    #[test]
    fn runs_one_at_a_time_in_order() {
        let (tx, rx) = mpsc::channel();
        let mut actions = Actions::default();
        assert_eq!(actions.push("a".into(), "/f".into(), idle()), 0);
        assert_eq!(actions.push("b".into(), "/f".into(), idle()), 1);
        assert_eq!(actions.start_next(&tx).as_deref(), Some("a"));
        assert_eq!(actions.start_next(&tx), None);
        let Ok(Msg::ActionDone { id, level, text }) = rx.recv() else {
            panic!("expected the first action to finish");
        };
        assert!(actions.finish(id, level, text).0 == Outcome::Done);
        assert_eq!(actions.start_next(&tx).as_deref(), Some("b"));
        assert_eq!(actions.history_rows()[0].title, "a");
    }

    #[test]
    fn cancelling_queued_actions_records_them() {
        let (tx, _rx) = mpsc::channel();
        let mut actions = Actions::default();
        actions.push("a".into(), "/f".into(), idle());
        actions.push("b".into(), "/f".into(), idle());
        actions.push("c".into(), "/f".into(), idle());
        actions.start_next(&tx);
        actions.cancel(2);
        assert_eq!(actions.pending(), 2);
        actions.cancel_all("closed");
        assert_eq!(actions.queue_rows().len(), 1); // "a" stops on its own
        assert_eq!(actions.history_rows().len(), 2);
        assert_eq!(
            actions.queue_rows()[0].detail,
            "Stopping after the current item…"
        );
    }
}
