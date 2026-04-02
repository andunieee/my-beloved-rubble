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

/// Configure a named rclone remote through rclone's non-interactive config API.
///
/// The API deliberately returns one question at a time. This keeps provider
/// details (including OAuth) in rclone while allowing both frontends to share
/// the configuration protocol.
pub fn setup_remote(name: &str, backend: &str) -> Result<(), String> {
    let mut response = call(
        "config/create",
        json!({
            "name": name,
            "type": backend,
            "parameters": {},
            "opt": {"nonInteractive": true, "all": true},
        }),
    )?;

    loop {
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
            return Ok(());
        }

        let option = response
            .get("Option")
            .ok_or_else(|| "rclone returned a config state without a question".to_owned())?;
        let option_name = option
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or("value");
        if let Some(help) = option.get("Help").and_then(Value::as_str)
            && !help.is_empty()
        {
            println!("{help}");
        }
        if let Some(examples) = option.get("Examples").and_then(Value::as_array) {
            for example in examples {
                let value = example
                    .get("Value")
                    .map(display_value_for_ui)
                    .unwrap_or_default();
                let help = example
                    .get("Help")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("  {value}: {help}");
            }
        }
        let default = option
            .get("Default")
            .map(display_value_for_ui)
            .unwrap_or_default();
        let prompt = if default.is_empty() {
            format!("{option_name}: ")
        } else {
            format!("{option_name} [{default}]: ")
        };
        let answer = if option
            .get("IsPassword")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            rpassword::prompt_password(prompt).map_err(|e| e.to_string())?
        } else {
            use std::io::Write;
            print!("{prompt}");
            std::io::stdout().flush().map_err(|e| e.to_string())?;
            let mut answer = String::new();
            std::io::stdin()
                .read_line(&mut answer)
                .map_err(|e| e.to_string())?;
            answer.trim_end().to_owned()
        };
        let answer = if answer.is_empty() { default } else { answer };
        response = call(
            "config/update",
            json!({
                "name": name,
                "parameters": {},
                "opt": {
                    "nonInteractive": true,
                    "continue": true,
                    "state": state,
                    "result": answer,
                },
            }),
        )?;
    }
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
