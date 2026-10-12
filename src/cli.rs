//! `mbr` — the My Beloved Rubble command-line interface.

use clap::{Parser, Subcommand};
use mbr::db::Remote;
use mbr::metadata;
use mbr::repo::{LocalHealth, RemoteHealth, Repo};
use mbr::state::{AppState, BackupRecord};
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
    /// verify blobs against their hashes (default: every local blob)
    Fsck {
        /// verify the copies on this remote instead (downloads them)
        #[arg(long)]
        remote: Option<String>,
        /// optional paths or hashes to verify
        specs: Vec<String>,
    },
    /// free local space: remove local copies that enough remotes hold
    Drop {
        /// every local blob that is safely stored elsewhere
        #[arg(long, conflicts_with = "specs")]
        all: bool,
        /// paths or hashes to drop
        #[arg(required_unless_present = "all")]
        specs: Vec<String>,
    },
    /// drop local copies of old versions and deleted files
    Gc {
        /// delete them everywhere instead, remotes included (irreversible)
        #[arg(long)]
        purge: bool,
        /// only those whose last name went away at least DAYS ago
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u32>,
        /// don't ask before purging
        #[arg(long, requires = "purge")]
        yes: bool,
    },
    /// encrypted backups of this folder's metadata
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// folder settings
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Subcommand)]
enum MetadataCommand {
    /// show where metadata is backed up and when it last was
    Status,
    /// back up metadata to these remotes from now on
    Add {
        #[arg(required = true)]
        remotes: Vec<String>,
    },
    /// stop backing up metadata to these remotes
    Rm {
        #[arg(required = true)]
        remotes: Vec<String>,
    },
    /// set or change the master password
    Password,
    /// encrypt the metadata and upload it to every chosen remote
    Backup,
    /// restore metadata from a remote into this (empty) folder
    Restore { remote: String },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// copies of each file to keep (local + remotes); shows it without N
    MinCopies { n: Option<u32> },
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
        Command::Fsck { remote, specs } => fsck(&mut open()?, remote.as_deref(), &specs),
        Command::Drop { all, specs } => drop_blobs(&mut open()?, all, &specs),
        Command::Gc {
            purge,
            older_than,
            yes,
        } => gc(&mut open()?, purge, older_than, yes),
        Command::Metadata { command } => match command {
            MetadataCommand::Status => metadata_status(&mut open()?),
            MetadataCommand::Add { remotes } => metadata_use(&mut open()?, &remotes, true),
            MetadataCommand::Rm { remotes } => metadata_use(&mut open()?, &remotes, false),
            MetadataCommand::Password => metadata_password(&mut open()?),
            MetadataCommand::Backup => metadata_backup(&mut open()?),
            MetadataCommand::Restore { remote } => metadata_restore(&mut open()?, &remote),
        },
        Command::Config { command } => match command {
            ConfigCommand::MinCopies { n } => config_min_copies(&mut open()?, n),
        },
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
    let min_copies = repo.min_copies()? as usize;
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
        if s.copies() < min_copies {
            notes.push(format!("AT RISK: {} of {min_copies} copies", s.copies()));
        }
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

/// Distinct hashes for `specs`, in order.
fn resolve_all(repo: &mut Repo, specs: &[String]) -> Result<Vec<String>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut hashes = Vec::new();
    for spec in specs {
        let hash = repo.resolve_spec(spec)?;
        if seen.insert(hash.clone()) {
            hashes.push(hash);
        }
    }
    Ok(hashes)
}

fn fsck(repo: &mut Repo, remote: Option<&str>, specs: &[String]) -> Result<(), String> {
    let hashes = if specs.is_empty() {
        match remote {
            Some(name) => repo.db.blobs_on_remote(name)?,
            None => repo
                .db
                .list_blobs()?
                .into_iter()
                .map(|b| b.hash)
                .filter(|h| repo.blob_present(h))
                .collect(),
        }
    } else {
        resolve_all(repo, specs)?
    };
    let remote = remote.map(|name| lookup_remote(repo, name)).transpose()?;
    let mut bad = 0;
    for hash in &hashes {
        let short = util::short_hash(hash);
        match &remote {
            None => match repo.verify_local(hash)? {
                LocalHealth::Ok => println!("{short}  ok"),
                LocalHealth::Missing => println!("{short}  not present locally"),
                LocalHealth::Corrupt { moved_to } => {
                    bad += 1;
                    println!(
                        "{short}  CORRUPT: moved to {} (fetch it again from a remote)",
                        moved_to.display()
                    );
                }
            },
            Some(remote) => match repo.verify_on_remote(remote, hash)? {
                RemoteHealth::Ok => println!("{short}  ok on {}", remote.name),
                RemoteHealth::Missing => {
                    bad += 1;
                    println!("{short}  MISSING on {} (record dropped)", remote.name);
                }
                RemoteHealth::Corrupt => {
                    bad += 1;
                    println!(
                        "{short}  CORRUPT on {}: deleted there (push it again)",
                        remote.name
                    );
                }
            },
        }
    }
    println!("verified {} blob(s), {bad} problem(s)", hashes.len());
    if bad > 0 {
        return Err(format!("{bad} blob(s) failed verification"));
    }
    Ok(())
}

fn drop_blobs(repo: &mut Repo, all: bool, specs: &[String]) -> Result<(), String> {
    let hashes: Vec<String> = if all {
        repo.db
            .list_blobs()?
            .into_iter()
            .map(|b| b.hash)
            .filter(|h| repo.blob_present(h))
            .collect()
    } else {
        resolve_all(repo, specs)?
    };
    let min_copies = repo.min_copies()?;
    let mut freed = 0u64;
    let mut kept = 0;
    for hash in &hashes {
        if !repo.blob_present(hash) {
            if !all {
                println!("{}  not present locally", util::short_hash(hash));
            }
            continue;
        }
        let size = std::fs::metadata(repo.blob_path(hash)).map_or(0, |m| m.len());
        let outcome = repo.drop_blob(hash)?;
        if outcome.dropped {
            freed += size;
            println!(
                "{}  dropped (on {})",
                util::short_hash(hash),
                outcome.verified.join(", ")
            );
        } else {
            kept += 1;
            println!("{}", outcome.kept_reason(min_copies));
        }
    }
    println!("freed {}", util::format_size(freed));
    if kept > 0 {
        println!(
            "{kept} blob(s) kept: push them to more remotes, or lower the requirement with \
             `mbr config min-copies`"
        );
    }
    Ok(())
}

fn gc(repo: &mut Repo, purge: bool, older_than: Option<u32>, yes: bool) -> Result<(), String> {
    let cutoff = older_than.map(|days| util::now() - i64::from(days) * 86_400);
    let candidates: Vec<_> = repo
        .unreferenced_blobs()?
        .into_iter()
        .filter(|u| cutoff.is_none_or(|cutoff| u.removed_at <= cutoff))
        .collect();
    if candidates.is_empty() {
        println!("no old versions or deleted files to clean up");
        return Ok(());
    }

    if purge {
        let total: u64 = candidates.iter().map(|u| u.size).sum();
        println!(
            "about to delete {} old version(s) / deleted file(s), {}, from this folder AND \
             every remote:",
            candidates.len(),
            util::format_size(total)
        );
        for u in &candidates {
            println!(
                "  {}  {:>10}  gone since {}",
                util::short_hash(&u.hash),
                util::format_size(u.size),
                util::format_time(u.removed_at)
            );
        }
        if !yes && prompt("type 'delete' to confirm", "")? != "delete" {
            return Err("not confirmed; nothing deleted".to_owned());
        }
        for u in &candidates {
            repo.purge_blob(&u.hash)?;
        }
        println!("deleted {} blob(s)", candidates.len());
        return Ok(());
    }

    let min_copies = repo.min_copies()?;
    let mut freed = 0;
    let mut kept = 0;
    for u in candidates.iter().filter(|u| u.present_locally) {
        let outcome = repo.drop_blob(&u.hash)?;
        if outcome.dropped {
            freed += u.size;
        } else {
            kept += 1;
            println!("{}", outcome.kept_reason(min_copies));
        }
    }
    println!("freed {} locally", util::format_size(freed));
    if kept > 0 {
        println!("{kept} old blob(s) kept here: they are not stored on enough remotes yet");
    }
    Ok(())
}

fn config_min_copies(repo: &mut Repo, n: Option<u32>) -> Result<(), String> {
    if let Some(n) = n {
        repo.set_min_copies(n)?;
    }
    println!("min-copies: {}", repo.min_copies()?);
    Ok(())
}

// ── metadata backups ──

fn metadata_status(repo: &mut Repo) -> Result<(), String> {
    let state = AppState::load().repo(&repo.root);
    println!(
        "master password: {}",
        if state.password_check.is_some() {
            "set"
        } else {
            "not set (`mbr metadata password`)"
        }
    );
    let remotes = repo.db.metadata_remotes()?;
    if remotes.is_empty() {
        println!("no remotes chosen for metadata backups (`mbr metadata add <remote>`)");
        return Ok(());
    }
    let current = repo.db_hash()?;
    for remote in remotes {
        let line = match state.metadata_backups.get(&remote.name) {
            None => "never backed up".to_owned(),
            Some(r) if r.db_hash == current => {
                format!("up to date (backed up {})", util::format_time(r.at))
            }
            Some(r) => format!("OUT OF DATE (last backed up {})", util::format_time(r.at)),
        };
        println!("{:<16} {line}", remote.name);
    }
    Ok(())
}

fn metadata_use(repo: &mut Repo, remotes: &[String], enabled: bool) -> Result<(), String> {
    for name in remotes {
        lookup_remote(repo, name)?;
        repo.db.set_metadata_remote(name, enabled)?;
        if enabled {
            println!("metadata will be backed up to {name}");
        } else {
            println!("metadata will no longer be backed up to {name}");
        }
    }
    Ok(())
}

/// Ask for a new master password, twice.
fn prompt_new_password() -> Result<String, String> {
    println!(
        "The master password encrypts the metadata backups. Without it they can't be read: \
         there is no way to recover it."
    );
    let password =
        rpassword::prompt_password("new master password: ").map_err(|e| e.to_string())?;
    metadata::check_new_password(&password)?;
    let again = rpassword::prompt_password("repeat it: ").map_err(|e| e.to_string())?;
    if password != again {
        return Err("the passwords differ".to_owned());
    }
    Ok(password)
}

/// The master password of this folder: asked and checked, or set now.
fn master_password(repo: &Repo, state: &mut AppState) -> Result<String, String> {
    let saved = state.repo(&repo.root).password_check;
    match saved {
        Some(check) => {
            let password =
                rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
            if !metadata::password_matches(&check, &password) {
                return Err("wrong master password".to_owned());
            }
            Ok(password)
        }
        None => {
            let password = prompt_new_password()?;
            state.repo_mut(&repo.root).password_check = Some(metadata::password_check(&password)?);
            state.save()?;
            Ok(password)
        }
    }
}

fn metadata_password(repo: &mut Repo) -> Result<(), String> {
    let mut state = AppState::load();
    if let Some(check) = state.repo(&repo.root).password_check {
        let current =
            rpassword::prompt_password("current master password: ").map_err(|e| e.to_string())?;
        if !metadata::password_matches(&check, &current) {
            return Err("wrong master password".to_owned());
        }
    }
    let password = prompt_new_password()?;
    let repo_state = state.repo_mut(&repo.root);
    repo_state.password_check = Some(metadata::password_check(&password)?);
    // Existing backups still use the old password until replaced.
    let stale = !repo_state.metadata_backups.is_empty();
    repo_state.metadata_backups.clear();
    state.save()?;
    println!("master password set");
    if stale {
        println!("existing backups still use the old one: run `mbr metadata backup`");
    }
    Ok(())
}

fn metadata_backup(repo: &mut Repo) -> Result<(), String> {
    let remotes = repo.db.metadata_remotes()?;
    if remotes.is_empty() {
        return Err("no remotes chosen for metadata backups (`mbr metadata add <remote>`)".into());
    }
    let mut state = AppState::load();
    let password = master_password(repo, &mut state)?;
    let snapshot = repo.metadata_snapshot()?;
    println!("encrypting…");
    let sealed = metadata::seal(&snapshot, &password)?;
    let scratch = repo.scratch_dir();
    let mut failed = 0;
    for remote in &remotes {
        print!("uploading to {}... ", remote.name);
        use std::io::Write;
        std::io::stdout().flush().ok();
        match repo.with_rclone(|| metadata::upload(&remote.target, &sealed, &scratch)) {
            Ok(()) => {
                println!("ok");
                state.repo_mut(&repo.root).metadata_backups.insert(
                    remote.name.clone(),
                    BackupRecord {
                        at: util::now(),
                        db_hash: snapshot.db_hash.clone(),
                    },
                );
            }
            Err(e) => {
                failed += 1;
                println!("FAILED: {e}");
            }
        }
    }
    state.save()?;
    if failed > 0 {
        return Err(format!("{failed} remote(s) could not be backed up to"));
    }
    Ok(())
}

fn metadata_restore(repo: &mut Repo, remote_name: &str) -> Result<(), String> {
    let remote = lookup_remote(repo, remote_name)?;
    if !repo.is_empty_folder()? {
        return Err("metadata can only be restored into a folder that tracks no files yet".into());
    }
    let scratch = repo.scratch_dir();
    let sealed = repo.with_rclone(|| metadata::download(&remote.target, &scratch))?;
    let password = rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
    println!("decrypting…");
    let (db, conf) = metadata::unseal(&sealed, &password)?;
    let report = repo.restore_metadata(&db, &conf)?;

    let mut state = AppState::load();
    state.repo_mut(&repo.root).password_check = Some(metadata::password_check(&password)?);
    state.save()?;

    println!(
        "restored {} file name(s) and {} remote(s)",
        report.files, report.remotes
    );
    for (path, why) in &report.skipped {
        println!("  skipped {path}: {why}");
    }
    println!("the files' content is still on the remotes: `mbr pull <remote>` downloads it");
    Ok(())
}
