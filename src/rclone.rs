//! In-process rclone integration through `librclone`.
//!
//! Blobs live on a remote under the same sharded layout as the local
//! `.mbr/blobs` store: `<target>/aa/bb/<full-hash>`, with no metadata at all.
//! Encryption is rclone's concern (use an rclone `crypt` remote if wanted).

use crate::util::is_hash;
use librclone::{initialize, rpc};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

static INITIALIZED: Once = Once::new();
static RPC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
/// Held for a whole multi-call operation, so its config path can't be
/// switched underneath it by another thread.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn lock() -> &'static Mutex<()> {
    RPC_LOCK.get_or_init(|| Mutex::new(()))
}

/// Where rclone writes its log. mbr redirects it here so the frontends can
/// surface the OAuth URL instead of losing it to stderr. The file is
/// per-process, so concurrent mbr processes never read each other's URLs.
pub fn log_path() -> PathBuf {
    LOG_PATH
        .get_or_init(|| std::env::temp_dir().join(format!("mbr-rclone-{}.log", std::process::id())))
        .clone()
}

/// Current length of the rclone log, to read only what comes after it.
fn log_len() -> u64 {
    std::fs::metadata(log_path()).map(|m| m.len()).unwrap_or(0)
}

fn ensure_initialized() {
    INITIALIZED.call_once(|| {
        // Must be set before rclone reads its log-file option at init time.
        unsafe { std::env::set_var("RCLONE_LOG_FILE", log_path()) };
        initialize();
    });
}

/// The OAuth auth URL rclone most recently wrote to its log after byte
/// offset `since`, if any.
pub fn auth_url(since: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(log_path()).ok()?;
    file.seek(SeekFrom::Start(since)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    String::from_utf8_lossy(&bytes).lines().rev().find_map(extract_url)
}

fn extract_url(line: &str) -> Option<String> {
    let pos = line.find("/auth?state=")?;
    let start = line[..pos].rfind("http://")?;
    let rest = &line[pos..];
    let end = rest
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest.len());
    Some(format!("{}{}", &line[start..pos], &rest[..end]))
}

/// Run one librclone RPC call while protecting rclone's process-global state.
pub fn call(method: &str, input: Value) -> Result<Value, String> {
    let _guard = lock().lock().map_err(|_| "rclone lock poisoned".to_owned())?;
    ensure_initialized();
    let output = rpc(method, input.to_string())?;
    serde_json::from_str(&output).map_err(|e| format!("invalid rclone response: {e}"))
}

/// Point librclone at the config owned by a repository. Prefer
/// [`with_config`] when other threads may use a different config.
pub fn set_config_path(path: &Path) -> Result<(), String> {
    call("config/setpath", json!({"path": path}))?;
    Ok(())
}

/// Run `f` with librclone pointed at the config at `path`, keeping other
/// [`with_config`] callers (possibly using another repository's config)
/// out until it returns.
pub fn with_config<T>(path: &Path, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _guard = CONFIG_LOCK
        .lock()
        .map_err(|_| "rclone config lock poisoned".to_owned())?;
    set_config_path(path)?;
    f()
}

/// One pending question in rclone's non-interactive config protocol.
#[derive(Debug, Clone)]
pub struct SetupQuestion {
    /// Opaque protocol state; pass back to [`continue_setup`].
    pub state: String,
    /// Option name (e.g. `config_is_local`).
    pub name: String,
    /// Help text, including any answer examples.
    pub help: String,
    /// Default answer as display text (empty if none).
    pub default: String,
    /// Whether the answer should be hidden while typing.
    pub password: bool,
}

/// Result of a setup step: either the next question or completion.
#[derive(Debug)]
pub enum SetupOutcome {
    Done,
    Question(SetupQuestion),
}

fn parse_setup_response(response: Value) -> Result<SetupOutcome, String> {
    if let Some(error) = response.get("Error").and_then(Value::as_str)
        && !error.is_empty()
    {
        return Err(error.to_owned());
    }
    let state = response
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if state.is_empty() {
        return Ok(SetupOutcome::Done);
    }
    let option = response
        .get("Option")
        .ok_or_else(|| "rclone returned a config state without a question".to_owned())?;
    let mut help = option
        .get("Help")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(examples) = option.get("Examples").and_then(Value::as_array) {
        for example in examples {
            let value = example
                .get("Value")
                .map(display_value_for_ui)
                .unwrap_or_default();
            let example_help = example
                .get("Help")
                .and_then(Value::as_str)
                .unwrap_or_default();
            help.push_str(&format!("\n  {value}: {example_help}"));
        }
    }
    Ok(SetupOutcome::Question(SetupQuestion {
        state: state.to_owned(),
        name: option
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or("value")
            .to_owned(),
        help,
        default: option
            .get("Default")
            .map(display_value_for_ui)
            .unwrap_or_default(),
        password: option
            .get("IsPassword")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }))
}

/// Start configuring a new remote, supplying `parameters` (form answers,
/// config-key → value) as prefilled defaults. Password values are passed in
/// the clear; rclone obscures them before storing (`obscure` opt).
///
/// Returns either completion or the first question rclone still needs —
/// typically an OAuth flow. Refuses to touch an existing remote `name`; if
/// the step fails, the half-created remote is deleted again.
pub fn begin_setup(name: &str, backend: &str, parameters: Value) -> Result<SetupOutcome, String> {
    if remote_exists(name)? {
        return Err(format!("rclone remote '{name}' already exists"));
    }
    let response = call(
        "config/create",
        json!({
            "name": name,
            "type": backend,
            "parameters": parameters,
            "opt": {
                "nonInteractive": true,
                "obscure": true,
            },
        }),
    );
    discard_on_error(name, response.and_then(parse_setup_response))
}

/// Answer one [`SetupQuestion`] and continue. `parameters` carries any
/// default config values again, as the protocol requires; `answer` is
/// passed in the clear and obscured here. If the step fails, the
/// half-configured remote is deleted (the conversation can't be resumed).
pub fn continue_setup(
    name: &str,
    state: &str,
    answer: &str,
    parameters: Value,
) -> Result<SetupOutcome, String> {
    let response = call(
        "config/update",
        json!({
            "name": name,
            "parameters": parameters,
            "opt": {
                "nonInteractive": true,
                "continue": true,
                "obscure": true,
                "state": state,
                "result": answer,
            },
        }),
    );
    discard_on_error(name, response.and_then(parse_setup_response))
}

/// Delete the remote `name` from the selected config.
pub fn delete_remote(name: &str) -> Result<(), String> {
    call("config/delete", json!({"name": name}))?;
    Ok(())
}

/// Pass `result` through, deleting the half-configured remote `name` if it
/// is an error.
fn discard_on_error<T>(name: &str, result: Result<T, String>) -> Result<T, String> {
    if result.is_err() {
        delete_remote(name).ok();
    }
    result
}

/// Abort an in-progress setup conversation for remote `name` and delete
/// the half-configured remote.
pub fn cancel_setup(name: &str, state: &str) -> Result<(), String> {
    let cancelled = call(
        "config/update",
        json!({
            "parameters": {},
            "opt": {
                "nonInteractive": true,
                "continue": true,
                "state": state,
                "result": "\u{0}cancel",
            },
        }),
    );
    delete_remote(name)?;
    cancelled.map(|_| ())
}

/// Like [`continue_setup`], but while rclone blocks doing OAuth (browser
/// flow) it watches rclone's log for the auth URL and reports it through
/// `on_url` — so the UI can show the link if the browser fails to open.
pub fn continue_setup_watching(
    name: &str,
    state: &str,
    answer: &str,
    parameters: Value,
    on_url: &mut dyn FnMut(String),
) -> Result<SetupOutcome, String> {
    // Only URLs logged by this step count, not ones from earlier setups.
    let since = log_len();
    let (tx, rx) = std::sync::mpsc::channel::<Result<SetupOutcome, String>>();
    let name = name.to_owned();
    let state = state.to_owned();
    let answer = answer.to_owned();
    std::thread::spawn(move || {
        let result = continue_setup(&name, &state, &answer, parameters);
        let _ = tx.send(result);
    });

    let mut reported: Option<String> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(url) = auth_url(since)
                    && reported.as_deref() != Some(url.as_str())
                {
                    reported = Some(url.clone());
                    on_url(url);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("rclone setup thread vanished".to_owned());
            }
        }
    }
}

/// The remote `name` already exists in the selected config.
pub fn remote_exists(name: &str) -> Result<bool, String> {
    let remotes = call("config/listremotes", json!({}))?;
    Ok(remotes
        .get("remotes")
        .and_then(Value::as_array)
        .is_some_and(|remotes| remotes.iter().any(|remote| remote.as_str() == Some(name))))
}

pub fn display_value_for_ui(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Remote path of a blob under `target`.
pub fn blob_path(hash: &str) -> String {
    format!("{}/{}/{hash}", &hash[..2], &hash[2..4])
}

/// Upload one blob file to `target`.
pub fn push_blob(target: &str, blob_file: &Path, hash: &str) -> Result<(), String> {
    call(
        "operations/copyfile",
        json!({
            "srcFs": blob_file.parent().unwrap_or_else(|| Path::new(".")).to_string_lossy(),
            "srcRemote": blob_file.file_name().unwrap_or_default().to_string_lossy(),
            "dstFs": target,
            "dstRemote": blob_path(hash),
        }),
    )?;
    Ok(())
}

/// Download one blob from `target` into `dest`.
pub fn fetch_blob(target: &str, hash: &str, dest: &Path) -> Result<(), String> {
    call(
        "operations/copyfile",
        json!({
            "srcFs": target,
            "srcRemote": blob_path(hash),
            "dstFs": dest.parent().unwrap_or_else(|| Path::new(".")).to_string_lossy(),
            "dstRemote": dest.file_name().unwrap_or_default().to_string_lossy(),
        }),
    )?;
    Ok(())
}

/// List all blob hashes stored under `target`.
///
/// Errors when the remote is unreachable, which doubles as the
/// accessibility check. An empty or missing directory is fine (no blobs).
pub fn list_blobs(target: &str) -> Result<Vec<String>, String> {
    let result = match call(
        "operations/list",
        json!({
            "fs": target,
            "remote": "",
            "opt": {
                "recurse": true,
                "filesOnly": true,
                "noModTime": true,
                "noMimeType": true,
            },
        }),
    ) {
        Ok(result) => result,
        // A prefix that was never pushed to may not exist yet; rclone
        // reports that as "directory not found" rather than a real failure.
        Err(e) if e.contains("directory not found") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let list = result
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| "rclone list response has no list array".to_owned())?;
    Ok(list
        .iter()
        .filter_map(|item| item.get("Path").and_then(Value::as_str))
        .filter_map(|path| path.rsplit('/').next())
        .filter(|name| is_hash(name))
        .map(str::to_owned)
        .collect())
}
