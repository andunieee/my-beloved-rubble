//! In-process rclone integration through `librclone`.
//!
//! Blobs live on a remote under the same sharded layout as the local
//! `.mbr/blobs` store: `<target>/aa/bb/<full-hash>`, with no metadata at all.
//! Encryption is rclone's concern (use an rclone `crypt` remote if wanted).

use crate::util::is_hash;
use librclone::{initialize, rpc};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Mutex, Once, OnceLock};

static INITIALIZED: Once = Once::new();
static RPC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> &'static Mutex<()> {
    RPC_LOCK.get_or_init(|| Mutex::new(()))
}

fn ensure_initialized() {
    INITIALIZED.call_once(initialize);
}

/// Run one librclone RPC call while protecting rclone's process-global state.
pub fn call(method: &str, input: Value) -> Result<Value, String> {
    let _guard = lock().lock().map_err(|_| "rclone lock poisoned".to_owned())?;
    ensure_initialized();
    let output = rpc(method, input.to_string())?;
    serde_json::from_str(&output).map_err(|e| format!("invalid rclone response: {e}"))
}

/// Point librclone at the config owned by a repository.
pub fn set_config_path(path: &Path) -> Result<(), String> {
    call("config/setpath", json!({"path": path}))?;
    Ok(())
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
/// typically an OAuth flow, which backends with a full hardcoded form
/// ([`crate::backends`]) never reach.
pub fn begin_setup(name: &str, backend: &str, parameters: Value) -> Result<SetupOutcome, String> {
    let response = call(
        "config/create",
        json!({
            "name": name,
            "type": backend,
            "parameters": parameters,
            "opt": {
                "nonInteractive": true,
                "all": true,
                "obscure": true,
            },
        }),
    )?;
    parse_setup_response(response)
}

/// Answer one [`SetupQuestion`] and continue. `parameters` carries any
/// default config values again, as the protocol requires; `answer` is
/// passed in the clear and obscured here.
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
    )?;
    parse_setup_response(response)
}

/// Abort an in-progress setup conversation.
pub fn cancel_setup(state: &str) -> Result<(), String> {
    call(
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
    )?;
    Ok(())
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
