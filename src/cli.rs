//! `mbr` — the My Beloved Rubble command-line interface.

use clap::{Parser, Subcommand};
use mbr::db::Remote;
use mbr::repo::Repo;
use mbr::util;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "mbr",
    about = "my beloved rubble: content-addressed folders with rclone remotes",
    after_help = "Commands other than `init` run inside an attached folder (or below one)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// attach mbr to a folder
    Init {
        /// folder to attach (defaults to the current directory)
        dir: Option<PathBuf>,
    },
    /// ingest new files, record removals
    Scan,
    /// list tracked files with metadata
    Ls,
    /// full detail for one file or blob
    Info { spec: String },
    /// manage rclone remotes
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// push blobs (default: all not yet there)
    Push {
        remote: String,
        /// optional paths or hashes to push
        specs: Vec<String>,
    },
    /// download blobs missing locally, from any remote that has them
    Fetch {
        /// paths or hashes to fetch
        #[arg(required = true)]
        specs: Vec<String>,
    },
    /// download blobs from one remote (default: all it has that are missing here)
    Pull {
        remote: String,
        /// optional paths or hashes to download
        specs: Vec<String>,
    },
    /// ask a remote whether it stores blobs (default: scan it entirely)
    Check {
        remote: String,
        /// optional paths or hashes to check; without them this is `remote scan`
        specs: Vec<String>,
    },
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// add an existing rclone target
    Add { name: String, target: String },
    /// list the rclone backends mbr can set up
    Types,
    /// configure an rclone remote interactively
    Setup { name: String, backend: String },
    /// remove a remote
    Rm { name: String },
    /// list remotes
    Ls,
    /// show a remote's target and rclone settings (secrets hidden)
    Show { name: String },
    /// rename a remote, change its target, or change rclone settings
    Edit {
        name: String,
        /// new mbr name for the remote
        #[arg(long)]
        rename: Option<String>,
        /// new rclone target (e.g. `backup:otherbucket`)
        #[arg(long)]
        target: Option<String>,
        /// set an rclone config key of the target's remote (repeatable)
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// list remotes fully and record every blob found there (default: all
    /// remotes); blobs never seen before are named unnamed/<hash>
    Scan { names: Vec<String> },
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Init { dir } => init(&dir.unwrap_or_else(|| PathBuf::from("."))),
        Command::Scan => scan(&mut open()?),
        Command::Ls => ls(&mut open()?),
        Command::Info { spec } => info(&mut open()?, &spec),
        Command::Remote { command } => match command {
            RemoteCommand::Add { name, target } => remote_add(&mut open()?, &name, &target),
            RemoteCommand::Types => remote_types(),
            RemoteCommand::Setup { name, backend } => remote_setup(&mut open()?, &name, &backend),
            RemoteCommand::Rm { name } => remote_rm(&mut open()?, &name),
            RemoteCommand::Ls => remote_ls(&mut open()?),
            RemoteCommand::Show { name } => remote_show(&mut open()?, &name),
            RemoteCommand::Edit {
                name,
                rename,
                target,
                set,
            } => remote_edit(&mut open()?, &name, rename, target, &set),
            RemoteCommand::Scan { names } => remote_scan(&mut open()?, &names),
        },
        Command::Push { remote, specs } => push(&mut open()?, &remote, &specs),
        Command::Fetch { specs } => fetch(&mut open()?, &specs),
        Command::Pull { remote, specs } => pull(&mut open()?, &remote, &specs),
        Command::Check { remote, specs } if specs.is_empty() => {
            remote_scan(&mut open()?, &[remote])
        }
        Command::Check { remote, specs } => check(&mut open()?, &remote, &specs),
    }
}

fn open() -> Result<Repo, String> {
    Repo::discover(&std::env::current_dir().map_err(|e| e.to_string())?)
}

fn lookup_remote(repo: &mut Repo, name: &str) -> Result<Remote, String> {
    repo.db
        .remote(name)?
        .ok_or_else(|| format!("no remote named '{name}' (see `mbr remote ls`)"))
}

fn init(dir: &Path) -> Result<(), String> {
    let mut repo = Repo::init(dir)?;
    let report = repo.scan()?;
    println!("attached mbr to {}", repo.root.display());
    if !report.is_empty() {
        print_scan_report(&report);
    }
    Ok(())
}

fn scan(repo: &mut Repo) -> Result<(), String> {
    let report = repo.scan()?;
    if report.is_empty() {
        println!("nothing changed");
    } else {
        print_scan_report(&report);
    }
    Ok(())
}

fn print_scan_report(report: &mbr::repo::ScanReport) {
    for p in &report.added {
        println!("added    {p}");
    }
    for p in &report.replaced {
        println!("replaced {p}  (previous version kept in history)");
    }
    for p in &report.removed {
        println!("removed  {p}");
    }
}

fn ls(repo: &mut Repo) -> Result<(), String> {
    let statuses = repo.status()?;
    if statuses.is_empty() {
        println!("no tracked files (drop files into the folder and run `mbr scan`)");
        return Ok(());
    }
    for s in &statuses {
        let local = if s.present_locally { "here" } else { "MISSING" };
        let remotes = if s.remotes.is_empty() {
            "-".to_owned()
        } else {
            s.remotes.join(",")
        };
        let mut notes = Vec::new();
        if !s.other_names.is_empty() {
            notes.push(format!("={}", s.other_names.join("=")));
        }
        if !s.previous_versions.is_empty() {
            notes.push(format!("{} old version(s)", s.previous_versions.len()));
        }
        println!(
            "{:<40} {:>10}  {:<7} {:<12} {}  {}",
            s.path,
            util::format_size(s.size),
            local,
            util::short_hash(&s.hash),
            remotes,
            notes.join("; "),
        );
    }
    Ok(())
}

fn info(repo: &mut Repo, spec: &str) -> Result<(), String> {
    let hash = repo.resolve_spec(spec)?;
    let statuses = repo.status()?;
    let Some(s) = statuses
        .iter()
        .find(|s| s.path == spec)
        .or_else(|| statuses.iter().find(|s| s.hash == hash))
    else {
        return Err(format!(
            "no active file uses blob {}",
            util::short_hash(&hash)
        ));
    };

    println!("path:     {}", s.path);
    println!("hash:     {}", s.hash);
    println!("size:     {} ({} bytes)", util::format_size(s.size), s.size);
    println!("added:    {}", util::format_time(s.added_at));
    println!(
        "local:    {}",
        if s.present_locally {
            "present"
        } else {
            "missing"
        }
    );
    println!(
        "remotes:  {}",
        if s.remotes.is_empty() {
            "none".to_owned()
        } else {
            s.remotes.join(", ")
        }
    );
    if !s.other_names.is_empty() {
        println!("same blob also tracked as: {}", s.other_names.join(", "));
    }
    if !s.past_names.is_empty() {
        println!("same blob was previously:  {}", s.past_names.join(", "));
    }
    for v in &s.previous_versions {
        println!(
            "previous version: {} ({}, removed {}, stored: {})",
            util::short_hash(&v.hash),
            util::format_size(v.size),
            util::format_time(v.removed_at),
            v.stored_summary(),
        );
    }
    Ok(())
}

fn remote_add(repo: &mut Repo, name: &str, target: &str) -> Result<(), String> {
    if repo.db.remote(name)?.is_some() {
        return Err(format!("remote '{name}' already exists"));
    }
    repo.db.add_remote(name, target)?;
    println!("added remote {name} -> {target}");
    Ok(())
}

/// List the hardcoded backends with their form fields.
fn remote_types() -> Result<(), String> {
    for backend in mbr::backends::BACKENDS {
        println!(
            "{:<12} {} — {}",
            backend.name, backend.title, backend.description
        );
        for field in backend.fields {
            let kind = match field.kind {
                mbr::backends::Kind::Secret => "secret",
                mbr::backends::Kind::Choice => "choice",
                mbr::backends::Kind::Text => "text",
            };
            let mut extras = Vec::new();
            if field.required {
                extras.push("required".to_owned());
            }
            if !field.default.is_empty() {
                extras.push(format!("default: {}", field.default));
            }
            if !field.examples.is_empty() {
                extras.push(format!("one of: {}", field.examples.join(", ")));
            }
            let extras = if extras.is_empty() {
                String::new()
            } else {
                format!(" ({})", extras.join(", "))
            };
            println!("    {:<18} {kind:<7} {}{extras}", field.name, field.help);
        }
    }
    Ok(())
}

fn remote_setup(repo: &mut Repo, name: &str, backend: &str) -> Result<(), String> {
    if repo.db.remote(name)?.is_some() {
        return Err(format!("remote '{name}' already exists"));
    }
    repo.configure_rclone()?;
    let Some(backend) = mbr::backends::backend(backend) else {
        return Err(format!(
            "unknown backend '{backend}' (see `mbr remote types`)"
        ));
    };

    println!("Configuring rclone remote '{name}' ({})", backend.title);
    println!("{}", backend.description);

    // Ask the hardcoded form fields, then hand everything to rclone in one go.
    let mut values: Vec<String> = Vec::new();
    for field in backend.fields {
        let label = match field.kind {
            mbr::backends::Kind::Choice => {
                format!("{} [{}]", field.label, field.examples.join("/"))
            }
            _ => field.label.to_owned(),
        };
        let answer = if field.kind == mbr::backends::Kind::Secret {
            rpassword::prompt_password(format!("{label}: ")).map_err(|e| e.to_string())?
        } else {
            prompt(&label, field.default)?
        };
        if field.required && answer.trim().is_empty() {
            return Err(format!("'{}' is required", field.label));
        }
        values.push(answer);
    }
    let spec = mbr::backends::target_spec(backend, &values);
    mbr::backends::check_target_spec(backend, &spec)?;
    let target = mbr::backends::target_for(name, backend, &spec);

    let parameters = mbr::backends::form_parameters(backend, &values);
    let mut outcome = mbr::rclone::begin_setup(name, backend.name, parameters.clone())?;
    // Anything rclone still asks (OAuth, verification) goes through the
    // generic question protocol.
    while let mbr::rclone::SetupOutcome::Question(question) = outcome {
        println!("{}", question.help);
        let answer = if question.password {
            rpassword::prompt_password(format!("{}: ", question.name)).map_err(|e| e.to_string())
        } else {
            prompt(&question.name, &question.default)
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(e) => {
                mbr::rclone::cancel_setup(name, &question.state).ok();
                return Err(e);
            }
        };
        outcome = mbr::rclone::continue_setup_watching(
            name,
            &question.state,
            &answer,
            parameters.clone(),
            &mut |url| println!("\nIf your browser didn't open, open this link:\n  {url}\n"),
        )?;
    }

    repo.db.add_remote(name, &target)?;
    println!("added remote {name} -> {target}");
    Ok(())
}

fn prompt(label: &str, default: &str) -> Result<String, String> {
    use std::io::Write;
    let suffix = if default.is_empty() {
        String::new()
    } else {
        format!(" [{default}]")
    };
    print!("{label}{suffix}: ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    let answer = answer.trim_end();
    Ok(if answer.is_empty() {
        default.to_owned()
    } else {
        answer.to_owned()
    })
}

fn remote_rm(repo: &mut Repo, name: &str) -> Result<(), String> {
    lookup_remote(repo, name)?;
    repo.db.remove_remote(name)?;
    println!("removed remote {name}");
    Ok(())
}

fn remote_ls(repo: &mut Repo) -> Result<(), String> {
    let remotes = repo.db.list_remotes()?;
    if remotes.is_empty() {
        println!(
            "no remotes configured (see `mbr remote types` and `mbr remote setup <name> <type>`)"
        );
    }
    for r in remotes {
        let count = repo.db.blobs_on_remote(&r.name)?.len();
        let missing = repo.blobs_missing_locally(&r.name)?.len();
        let missing = if missing > 0 {
            format!(", {missing} not here")
        } else {
            String::new()
        };
        println!("{:<16} {}  ({count} blob(s){missing})", r.name, r.target);
    }
    Ok(())
}

/// Whether the rclone config key `key` of `backend` holds a secret.
fn is_secret_key(backend: &str, key: &str) -> bool {
    let registered = mbr::backends::backend(backend)
        .and_then(|b| b.fields.iter().find(|f| f.name == key))
        .map(|f| f.kind == mbr::backends::Kind::Secret);
    registered.unwrap_or_else(|| {
        ["pass", "secret", "token", "key"]
            .iter()
            .any(|word| key.contains(word))
    })
}

fn remote_show(repo: &mut Repo, name: &str) -> Result<(), String> {
    let remote = lookup_remote(repo, name)?;
    println!("name:    {}", remote.name);
    println!("target:  {}", remote.target);
    println!("blobs:   {} recorded", repo.db.blobs_on_remote(name)?.len());
    let Some(section) = mbr::rclone::target_section(&remote.target) else {
        println!("(target is a plain path; no rclone settings)");
        return Ok(());
    };
    let Some(config) = repo.with_rclone(|| mbr::rclone::remote_config(section))? else {
        println!("(no rclone remote '{section}' in this folder's rclone.conf)");
        return Ok(());
    };
    let backend = config
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    println!("rclone remote '{section}' ({backend}):");
    for (key, value) in &config {
        if key == "type" {
            continue;
        }
        let value = if is_secret_key(&backend, key) {
            "(hidden)".to_owned()
        } else {
            mbr::rclone::display_value_for_ui(value)
        };
        println!("    {key:<24} {value}");
    }
    Ok(())
}

fn remote_edit(
    repo: &mut Repo,
    name: &str,
    rename: Option<String>,
    target: Option<String>,
    set: &[String],
) -> Result<(), String> {
    let remote = lookup_remote(repo, name)?;
    let new_name = rename.unwrap_or_else(|| remote.name.clone());
    let new_target = target.unwrap_or_else(|| remote.target.clone());
    if new_name.trim().is_empty() || new_target.trim().is_empty() {
        return Err("name and target must not be empty".to_owned());
    }
    if new_name == remote.name && new_target == remote.target && set.is_empty() {
        return Err("nothing to change (see --rename, --target, --set)".to_owned());
    }

    if !set.is_empty() {
        let mut parameters = serde_json::Map::new();
        for pair in set {
            let Some((key, value)) = pair.split_once('=') else {
                return Err(format!("--set expects KEY=VALUE, got '{pair}'"));
            };
            parameters.insert(key.trim().to_owned(), value.into());
        }
        let section = mbr::rclone::target_section(&new_target)
            .ok_or_else(|| format!("target '{new_target}' names no rclone remote to configure"))?
            .to_owned();
        repo.with_rclone(|| {
            mbr::rclone::update_remote(&section, serde_json::Value::Object(parameters))
        })?;
        println!("updated rclone remote '{section}'");
    }
    if new_name != remote.name || new_target != remote.target {
        repo.db.update_remote(&remote.name, &new_name, &new_target)?;
        println!("remote {new_name} -> {new_target}");
    }
    Ok(())
}

fn remote_scan(repo: &mut Repo, names: &[String]) -> Result<(), String> {
    let remotes = if names.is_empty() {
        repo.db.list_remotes()?
    } else {
        names
            .iter()
            .map(|n| lookup_remote(repo, n))
            .collect::<Result<_, _>>()?
    };
    if remotes.is_empty() {
        println!("no remotes configured");
        return Ok(());
    }
    let mut failed = 0;
    for remote in &remotes {
        match repo.scan_remote(remote) {
            Ok(check) => print_remote_check(remote, &check),
            Err(e) => {
                failed += 1;
                println!("remote '{}' ({}): FAILED: {e}", remote.name, remote.target);
            }
        }
    }
    if failed > 0 {
        return Err(format!("{failed} remote(s) could not be scanned"));
    }
    Ok(())
}

fn print_remote_check(remote: &Remote, check: &mbr::repo::RemoteCheck) {
    println!("remote '{}' ({}) is reachable", remote.name, remote.target);
    println!("  {} expected blob(s) present", check.present.len());
    for hash in &check.missing {
        println!("  MISSING: {} (was recorded there; record dropped)", hash);
    }
    for hash in &check.discovered {
        println!(
            "  found unrecorded blob {} (record added)",
            util::short_hash(hash)
        );
    }
    for hash in &check.adopted {
        println!(
            "  found unknown blob {} (added as {}/{hash})",
            util::short_hash(hash),
            mbr::repo::UNNAMED_DIR
        );
    }
    if check.missing.is_empty() && check.discovered.is_empty() && check.adopted.is_empty() {
        println!("  database and remote agree");
    }
}

fn push(repo: &mut Repo, remote_name: &str, specs: &[String]) -> Result<(), String> {
    let remote = lookup_remote(repo, remote_name)?;
    let hashes: Vec<String> = if specs.is_empty() {
        repo.blobs_missing_on_remote(remote_name)?
    } else {
        specs
            .iter()
            .map(|s| repo.resolve_spec(s))
            .collect::<Result<_, _>>()?
    };
    if hashes.is_empty() {
        println!("nothing to push; every local blob is already on '{remote_name}'");
        return Ok(());
    }
    for hash in &hashes {
        print!("pushing {} to {remote_name}... ", util::short_hash(hash));
        use std::io::Write;
        std::io::stdout().flush().ok();
        repo.push_blob(&remote, hash)?;
        println!("ok");
    }
    println!("pushed {} blob(s)", hashes.len());
    Ok(())
}

fn fetch(repo: &mut Repo, specs: &[String]) -> Result<(), String> {
    for spec in specs {
        let hash = repo.resolve_spec(spec)?;
        if repo.blob_present(&hash) {
            println!("{spec}: already present locally");
            continue;
        }
        let remote = repo.fetch_blob(&hash)?;
        println!("{spec}: fetched {} from {remote}", util::short_hash(&hash));
    }
    Ok(())
}

fn pull(repo: &mut Repo, remote_name: &str, specs: &[String]) -> Result<(), String> {
    let remote = lookup_remote(repo, remote_name)?;
    let hashes: Vec<String> = if specs.is_empty() {
        repo.blobs_missing_locally(remote_name)?
    } else {
        let mut hashes = Vec::new();
        for spec in specs {
            let hash = repo.resolve_spec(spec)?;
            if repo.blob_present(&hash) {
                println!("{spec}: already present locally");
            } else {
                hashes.push(hash);
            }
        }
        hashes
    };
    if hashes.is_empty() {
        if specs.is_empty() {
            println!("nothing to pull; every blob recorded on '{remote_name}' is already here");
        }
        return Ok(());
    }
    let mut failed = 0;
    for hash in &hashes {
        print!("pulling {} from {remote_name}... ", util::short_hash(hash));
        use std::io::Write;
        std::io::stdout().flush().ok();
        match repo.fetch_blob_from(&remote, hash) {
            Ok(()) => println!("ok"),
            Err(e) => {
                failed += 1;
                println!("FAILED: {e}");
            }
        }
    }
    println!("pulled {} blob(s)", hashes.len() - failed);
    if failed > 0 {
        return Err(format!("{failed} blob(s) could not be pulled"));
    }
    Ok(())
}

fn check(repo: &mut Repo, remote_name: &str, specs: &[String]) -> Result<(), String> {
    let remote = lookup_remote(repo, remote_name)?;
    let mut hashes: Vec<String> = specs
        .iter()
        .map(|s| repo.resolve_spec(s))
        .collect::<Result<_, _>>()?;
    let mut seen = std::collections::HashSet::new();
    hashes.retain(|h| seen.insert(h.clone()));
    for (hash, stored) in repo.check_blobs(&remote, &hashes)? {
        let verdict = if stored { "stored" } else { "NOT STORED" };
        println!("{}  {verdict} on {remote_name}", util::short_hash(&hash));
    }
    Ok(())
}
