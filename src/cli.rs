//! `mbr` — the My Beloved Rubble command-line interface.

use clap::{Parser, Subcommand};
use mbr::db::Remote;
use mbr::metadata;
use mbr::peers::{self, Event, Node};
use mbr::rclone::SetupOutcome;
use mbr::repo::{LocalHealth, RemoteHealth, Repo};
use mbr::state::{AppState, BackupRecord, Peer, RepoState};
use mbr::util;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "mbr",
    about = "my beloved rubble: content-addressed folders with rclone remotes",
    after_help = "Commands other than `init` and `restore` run inside an attached folder (or below one)."
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
    /// make a folder the one a metadata backup describes, instead of `init`
    #[command(group(clap::ArgGroup::new("source").required(true).args(["backup", "from"])))]
    Restore {
        /// folder to restore into (defaults to the current directory)
        dir: Option<PathBuf>,
        /// an encrypted backup file (mbr-metadata-….tar.gz.encrypted)
        #[arg(long, value_name = "FILE")]
        backup: Option<PathBuf>,
        /// download the backup from a remote of this backend (see `mbr
        /// remote types`), asking for its settings
        #[arg(long, value_name = "BACKEND")]
        from: Option<String>,
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
    /// devices this folder syncs with
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// scan, then sync the metadata with every peer (over iroh)
    Sync {
        /// keep running: accept the peers' syncs, and sync every minute
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Subcommand)]
enum PeerCommand {
    /// show this device's id, to add it as a peer on the others
    Id,
    /// sync with a device (which has to add this one too)
    Add {
        /// its id (`mbr peer id` there)
        id: String,
        /// what to call it
        #[arg(long)]
        name: Option<String>,
    },
    /// stop syncing with a device
    Rm {
        /// its id or name
        peer: String,
    },
    /// list the peers
    Ls,
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
    /// encrypt the metadata and upload it to every chosen remote
    Backup,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// copies of each file to keep (local + remotes); shows it without N
    MinCopies { n: Option<u32> },
    /// set the master password once (needed before adding remotes), or
    /// type it on this device if it is already set
    Password,
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// list the rclone backends mbr can set up
    Types,
    /// set up a new remote, encrypted with the master password
    Setup {
        name: String,
        backend: String,
        /// store files unencrypted (whoever runs the remote can read them)
        #[arg(long)]
        unencrypted: bool,
        /// set an rclone config key of the backend (repeatable); form
        /// fields given this way aren't asked
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// remove a remote
    Rm { name: String },
    /// list remotes
    Ls,
    /// show a remote's settings (secrets hidden)
    Show { name: String },
    /// rename a remote or change its settings
    Edit {
        name: String,
        /// new name for the remote
        #[arg(long)]
        rename: Option<String>,
        /// set an rclone config key of the remote's backend (repeatable)
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
        Command::Restore { dir, backup, from } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from("."));
            let source = match (backup, from) {
                (Some(file), _) => Source::Backup(file),
                (_, Some(backend)) => Source::Remote(backend),
                _ => unreachable!("clap requires one source"),
            };
            restore(&dir, source)
        }
        Command::Scan => scan(&mut open()?),
        Command::Ls => ls(&mut open()?),
        Command::Info { spec } => info(&mut open()?, &spec),
        Command::Remote { command } => match command {
            RemoteCommand::Types => remote_types(),
            RemoteCommand::Setup {
                name,
                backend,
                unencrypted,
                set,
            } => remote_setup(&mut open()?, &name, &backend, !unencrypted, &set),
            RemoteCommand::Rm { name } => remote_rm(&mut open()?, &name),
            RemoteCommand::Ls => remote_ls(&mut open()?),
            RemoteCommand::Show { name } => remote_show(&mut open()?, &name),
            RemoteCommand::Edit { name, rename, set } => {
                remote_edit(&mut open()?, &name, rename, &set)
            }
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
            MetadataCommand::Backup => metadata_backup(&mut open()?),
        },
        Command::Config { command } => match command {
            ConfigCommand::MinCopies { n } => config_min_copies(&mut open()?, n),
            ConfigCommand::Password => config_password(&mut open()?),
        },
        Command::Peer { command } => match command {
            PeerCommand::Id => peer_id(&open()?),
            PeerCommand::Add { id, name } => peer_add(&open()?, &id, name),
            PeerCommand::Rm { peer } => peer_rm(&open()?, &peer),
            PeerCommand::Ls => peer_ls(&open()?),
        },
        Command::Sync { watch } => sync(&mut open()?, watch),
    }
}

/// The folder around the current directory, its rclone settings written
/// again if some remote's are missing (with the master password saved on
/// this machine, if any).
fn open() -> Result<Repo, String> {
    let mut repo = Repo::discover(&std::env::current_dir().map_err(|e| e.to_string())?)?;
    if repo.rclone_config_incomplete()? {
        let password = AppState::load().repo(&repo.root).password;
        match repo.write_rclone_config(password.as_deref()) {
            Err(_) if password.is_none() && repo.password_check()?.is_some() => eprintln!(
                "warning: the remotes need the master password on this device: `mbr config password`"
            ),
            Err(e) => eprintln!("warning: cannot write the rclone settings: {e}"),
            Ok(()) => {}
        }
    }
    Ok(repo)
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
            notes.push(format!("{} of {min_copies} copies", s.copies()));
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

/// List the hardcoded backends with their form fields.
fn remote_types() -> Result<(), String> {
    for backend in mbr::backends::offered() {
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

fn remote_setup(
    repo: &mut Repo,
    name: &str,
    backend: &str,
    encrypt: bool,
    set: &[String],
) -> Result<(), String> {
    let layout = mbr::remotes::Layout::new(name, encrypt)?;
    let mut given = parse_set(set)?;
    if repo.db.remote(name)?.is_some() {
        return Err(format!("remote '{name}' already exists"));
    }
    let Some(backend) = mbr::backends::offered().find(|b| b.name == backend) else {
        return Err(format!(
            "unknown backend '{backend}' (see `mbr remote types`)"
        ));
    };
    let mut state = AppState::load();
    let password = master_password(repo, &mut state)?;
    repo.configure_rclone()?;
    layout.check_unused()?;

    println!("Setting up remote '{name}' ({})", backend.title);
    println!("{}", backend.description);
    if !encrypt {
        println!(
            "WARNING: files will be stored unencrypted; whoever runs the remote can read them"
        );
    }

    let values = ask_backend_form(backend, &mut given)?;
    let spec = mbr::backends::target_spec(backend, &values);
    mbr::backends::check_target_spec(backend, &spec)?;
    let folder_id = repo.folder_id()?;
    let mut parameters = mbr::backends::form_parameters(backend, &values);
    // What --set gave beyond the form goes to rclone as is.
    parameters.as_object_mut().expect("an object").extend(given);
    let section = layout.root_section();
    let begun = mbr::rclone::begin_setup(&section, backend.name, parameters.clone());
    answer_questions(&section, begun, &parameters, |outcome| {
        layout.finish(outcome, &spec, &folder_id, &password)
    })?;

    let remote = repo.add_remote(&layout, &spec)?;
    println!(
        "added remote {name} ({})",
        mbr::remotes::describe(&remote.target)
    );
    Ok(())
}

/// Ask the hardcoded form fields of `backend`, in order, except those in
/// `given` (taken out of it and used as is).
fn ask_backend_form(
    backend: &mbr::backends::Backend,
    given: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<String>, String> {
    let mut values: Vec<String> = Vec::new();
    for field in backend.fields {
        if let Some(value) = given.remove(field.name) {
            values.push(value.as_str().unwrap_or_default().to_owned());
            continue;
        }
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
    Ok(values)
}

/// Answer what rclone still asks to finish setting up `section` (OAuth,
/// verification) through the generic question protocol, passing each
/// step's outcome through `finish`.
fn answer_questions(
    section: &str,
    begun: Result<SetupOutcome, String>,
    parameters: &serde_json::Value,
    finish: impl Fn(Result<SetupOutcome, String>) -> Result<SetupOutcome, String>,
) -> Result<(), String> {
    let mut outcome = finish(begun)?;
    while let SetupOutcome::Question(question) = outcome {
        println!("{}", question.help);
        let answer = if question.password {
            rpassword::prompt_password(format!("{}: ", question.name)).map_err(|e| e.to_string())
        } else if question.boolean {
            prompt_yes_no(&question.name, &question.default)
        } else {
            prompt(&question.name, &question.default)
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(e) => {
                mbr::rclone::cancel_setup(section, &question.state).ok();
                return Err(e);
            }
        };
        let step = mbr::rclone::continue_setup_watching(
            section,
            &question.state,
            &answer,
            parameters.clone(),
            &mut print_auth_url,
            &AtomicBool::new(false),
        );
        outcome = finish(step)?;
    }
    Ok(())
}

fn print_auth_url(url: String) {
    println!("\nIf your browser didn't open, open this link:\n  {url}\n");
}

/// Ask a yes/no question; the answer is `true` or `false`, as rclone
/// expects.
fn prompt_yes_no(label: &str, default: &str) -> Result<String, String> {
    let default = if default == "true" { "y" } else { "n" };
    loop {
        match prompt(&format!("{label} (y/n)"), default)?
            .to_lowercase()
            .as_str()
        {
            "y" | "yes" | "true" => return Ok("true".to_owned()),
            "n" | "no" | "false" => return Ok("false".to_owned()),
            _ => println!("answer y or n"),
        }
    }
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
    repo.remove_remote(name)?;
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
        let protection = mbr::remotes::describe(&r.target);
        println!("{:<16} {protection}  ({count} blob(s){missing})", r.name);
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
    println!("storage: {}", mbr::remotes::describe(&remote.target));
    println!("blobs:   {} recorded", repo.db.blobs_on_remote(name)?.len());
    let record = repo
        .db
        .remote_record(name)?
        .ok_or_else(|| format!("no remote named '{name}'"))?;
    if !record.spec.is_empty() {
        println!("place:   {}", record.spec);
    }
    println!("backend: {}", record.backend);
    for (key, value) in &record.config {
        let value = if is_secret_key(&record.backend, key) {
            "(hidden)"
        } else {
            value
        };
        println!("    {key:<24} {value}");
    }
    Ok(())
}

/// The rclone config keys and values of `--set KEY=VALUE` arguments.
fn parse_set(set: &[String]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut parameters = serde_json::Map::new();
    for pair in set {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("--set expects KEY=VALUE, got '{pair}'"));
        };
        parameters.insert(key.trim().to_owned(), value.into());
    }
    Ok(parameters)
}

fn remote_edit(
    repo: &mut Repo,
    name: &str,
    rename: Option<String>,
    set: &[String],
) -> Result<(), String> {
    let remote = lookup_remote(repo, name)?;
    let new_name = rename.unwrap_or_else(|| remote.name.clone());
    if new_name != remote.name {
        mbr::remotes::check_name(&new_name)?;
    }
    if new_name == remote.name && set.is_empty() {
        return Err("nothing to change (see --rename, --set)".to_owned());
    }

    if !set.is_empty() {
        let parameters = parse_set(set)?;
        let section = mbr::remotes::settings_section(&remote.target)
            .ok_or_else(|| format!("remote '{name}' has no rclone settings"))?;
        repo.with_rclone(|| {
            mbr::rclone::update_remote(&section, serde_json::Value::Object(parameters))
        })?;
        println!("updated the settings of '{name}'");
    }
    if new_name != remote.name {
        let password = AppState::load().repo(&repo.root).password;
        repo.rename_remote(&remote.name, &new_name, password.as_deref())?;
        println!("renamed remote {name} to {new_name}");
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
                println!("remote '{}': FAILED: {e}", remote.name);
            }
        }
    }
    if failed > 0 {
        return Err(format!("{failed} remote(s) could not be scanned"));
    }
    Ok(())
}

fn print_remote_check(remote: &Remote, check: &mbr::repo::RemoteCheck) {
    println!("remote '{}' is reachable", remote.name);
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
        match (repo.password_check()?.is_some(), state.password.is_some()) {
            (false, _) => "not set (`mbr config password`)",
            (true, true) => "set, saved on this device",
            (true, false) => "set on another device: type it here with `mbr config password`",
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
        "The master password encrypts your remotes and the metadata backups. Without it \
         they can't be read: there is no way to recover it. Write it down."
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

/// The master password of this folder: saved on this machine, or asked
/// for (and saved) if it was set on another device.
fn master_password(repo: &mut Repo, state: &mut AppState) -> Result<String, String> {
    if let Some(password) = state.repo(&repo.root).password {
        return Ok(password);
    }
    if repo.password_check()?.is_none() {
        return Err("set a master password first: `mbr config password`".to_owned());
    }
    let password = rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
    unlock(repo, state, &password)?;
    Ok(password)
}

/// Check `password` is the master password, save it on this machine, and
/// set up the remotes that wait for it (synced from another device).
fn unlock(repo: &mut Repo, state: &mut AppState, password: &str) -> Result<(), String> {
    repo.verify_password(password)?;
    state.repo_mut(&repo.root).keep_password(password);
    state.save()?;
    if repo.rclone_config_incomplete()? {
        repo.write_rclone_config(Some(password))?;
    }
    Ok(())
}

fn config_password(repo: &mut Repo) -> Result<(), String> {
    let mut state = AppState::load();
    if repo.password_check()?.is_some() {
        println!(
            "The master password is already set (it cannot be changed): type it to use it here."
        );
        let password =
            rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
        unlock(repo, &mut state, &password)?;
        println!("master password correct, and saved on this device");
        return Ok(());
    }
    let password = prompt_new_password()?;
    repo.set_password(&password)?;
    let repo_state = state.repo_mut(&repo.root);
    repo_state.keep_password(&password);
    // Existing backups still use the old password until replaced.
    let stale = !repo_state.metadata_backups.is_empty();
    repo_state.metadata_backups.clear();
    state.save()?;
    println!("master password set and saved on this device");
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
    let name = metadata::backup_name(&password)?;
    let scratch = repo.scratch_dir();
    let mut failed = 0;
    for remote in &remotes {
        print!("uploading to {}... ", remote.name);
        use std::io::Write;
        std::io::stdout().flush().ok();
        let uploaded = repo.with_rclone(|| {
            let location = mbr::remotes::backup_location(&remote.target)?;
            metadata::upload(&location, &name, &sealed, &scratch)
        });
        match uploaded {
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

/// Where `mbr restore` gets the backup from.
enum Source {
    /// An encrypted backup file.
    Backup(PathBuf),
    /// A remote of this backend, whose settings are asked for.
    Remote(String),
}

fn restore(dir: &Path, source: Source) -> Result<(), String> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let (dump, password) = match source {
        Source::Backup(file) => {
            let sealed = read(&file)?;
            let password =
                rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
            println!("decrypting…");
            (metadata::unseal(&sealed, &password)?, password)
        }
        Source::Remote(backend) => download_backup(dir, &backend)?,
    };
    let (repo, report) = metadata::restore(dir, &dump, &password)?;

    // Nothing this machine knew about the folder at that path still holds.
    let mut repo = repo;
    if repo.password_check()?.is_none() {
        // A backup made before the check was part of the metadata.
        repo.set_password(&password)?;
    }
    let mut state = AppState::load();
    let repo_state = state.repo_mut(&repo.root);
    *repo_state = RepoState::default();
    repo_state.keep_password(&password);
    state.save()?;

    println!(
        "restored {} into {}",
        plural(report.files, "file name"),
        repo.root.display()
    );
    for path in &report.kept {
        println!("  kept the {path} already there (`mbr scan` records it as a new version)");
    }
    for (path, why) in &report.skipped {
        println!("  skipped {path}: {why}");
    }
    if report.files > 0 {
        println!("the files' content is still on the remotes: `mbr pull <remote>` downloads it");
    }
    Ok(())
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// Ask for the master password and the settings of a remote of `backend`,
/// and download the backup made with that password from it:
/// `(metadata, password)`. The remote is set up in a throwaway rclone
/// config, since the backup brings the folder's own settings.
fn download_backup(dir: &Path, backend: &str) -> Result<(Vec<u8>, String), String> {
    let Some(backend) = mbr::backends::offered().find(|b| b.name == backend) else {
        return Err(format!(
            "unknown backend '{backend}' (see `mbr remote types`)"
        ));
    };
    if Repo::is_initialized(dir) {
        return Err(format!(
            "{} is already an mbr folder: restore into a new one",
            dir.display()
        ));
    }
    let password = rpassword::prompt_password("master password: ").map_err(|e| e.to_string())?;
    println!("Where the backup is ({})", backend.title);
    let values = ask_backend_form(backend, &mut serde_json::Map::new())?;
    let spec = mbr::backends::target_spec(backend, &values);
    mbr::backends::check_target_spec(backend, &spec)?;
    let parameters = mbr::backends::form_parameters(backend, &values);

    let scratch = dir.join(mbr::repo::TMP_DIR);
    let config = scratch.join("restore-rclone.conf");
    std::fs::remove_file(&config).ok();
    mbr::repo::ensure_rclone_config(&config)?;
    let section = format!("{}restore", mbr::remotes::ROOT_PREFIX);
    let sealed = mbr::rclone::with_config(&config, || {
        let begun = mbr::rclone::begin_setup(&section, backend.name, parameters.clone());
        answer_questions(&section, begun, &parameters, |outcome| outcome)?;
        let name = metadata::backup_name(&password)?;
        println!("downloading {name}…");
        let location = mbr::remotes::root_location(&section, &spec);
        metadata::download(&location, &name, &scratch)
    });
    std::fs::remove_file(&config).ok();
    println!("decrypting…");
    Ok((metadata::unseal(&sealed?, &password)?, password))
}

// ── peers ──

fn peer_id(repo: &Repo) -> Result<(), String> {
    let mut state = AppState::load();
    let key = peers::node_key(state.repo_mut(&repo.root));
    state.save()?;
    println!("{}", peers::device_id(&key));
    Ok(())
}

fn peer_add(repo: &Repo, id: &str, name: Option<String>) -> Result<(), String> {
    let id = peers::check_device_id(id)?;
    let mut state = AppState::load();
    let repo_state = state.repo_mut(&repo.root);
    if peers::device_id(&peers::node_key(repo_state)) == id {
        return Err("that is this device's own id".to_owned());
    }
    let name = name.unwrap_or_else(|| format!("device {}", &id[..8]));
    repo_state
        .peers
        .insert(id.clone(), Peer { name: name.clone() });
    state.save()?;
    println!("added {name} ({id})");
    println!(
        "syncing starts once it adds this device too: `mbr peer add {}` there",
        { peers::device_id(&peers::node_key(state.repo_mut(&repo.root))) }
    );
    Ok(())
}

fn peer_rm(repo: &Repo, peer: &str) -> Result<(), String> {
    let mut state = AppState::load();
    let peers = &mut state.repo_mut(&repo.root).peers;
    let id = peers
        .iter()
        .find(|(id, p)| *id == peer || p.name == peer)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| format!("no peer '{peer}' (see `mbr peer ls`)"))?;
    let removed = peers.remove(&id).expect("just found");
    state.save()?;
    println!("no longer syncing with {}", removed.name);
    Ok(())
}

fn peer_ls(repo: &Repo) -> Result<(), String> {
    let state = AppState::load().repo(&repo.root);
    if state.peers.is_empty() {
        println!("no peers (`mbr peer add <id>`; `mbr peer id` shows this device's)");
    }
    for (id, peer) in &state.peers {
        println!("{:<20} {id}", peer.name);
    }
    Ok(())
}

/// How often `mbr sync --watch` scans and syncs.
const SYNC_EVERY: Duration = Duration::from_secs(60);

fn sync(repo: &mut Repo, watch: bool) -> Result<(), String> {
    let mut state = AppState::load();
    let known = state.repo(&repo.root).peers;
    if known.is_empty() && !watch {
        return Err(
            "no peers yet: `mbr peer add <id>` (`mbr peer id` shows this device's)".to_owned(),
        );
    }
    let key = peers::node_key(state.repo_mut(&repo.root));
    state.save()?;
    let ids: Vec<String> = known.keys().cloned().collect();
    let name = |id: &str| known.get(id).map_or(id.to_owned(), |p| p.name.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let node = Node::start(key, repo.db.shared(), &ids, move |event| {
        tx.send(event).ok();
    })?;

    if !watch {
        print_scan_report(&repo.scan()?);
        let mut joined = false;
        for id in &ids {
            match node.sync_now(id) {
                Ok(report) => {
                    joined |= report.joined;
                    println!("{}: {}", name(id), describe_sync(&report));
                }
                Err(e) => println!("{}: {e}", name(id)),
            }
        }
        drop(node);
        return apply_sync(repo, &mut state, joined);
    }

    println!("syncing as {} (Ctrl+C to stop)", node.id());
    let mut next_round = std::time::Instant::now();
    loop {
        if std::time::Instant::now() >= next_round {
            print_scan_report(&repo.scan()?);
            for id in &ids {
                node.sync(id);
            }
            next_round = std::time::Instant::now() + SYNC_EVERY;
        }
        let wait = next_round.saturating_duration_since(std::time::Instant::now());
        let Ok(event) = rx.recv_timeout(wait) else {
            continue;
        };
        match event {
            Event::Synced { peer, report } => {
                println!("{}: {}", name(&peer), describe_sync(&report));
                if let Err(e) = apply_sync(repo, &mut state, report.joined) {
                    println!("error: {e}");
                }
            }
            Event::Failed { peer, error } => println!("{}: {error}", name(&peer)),
            Event::Refused { peer } => {
                println!("device {peer} tried to sync: `mbr peer add {peer}` to sync with it")
            }
        }
    }
}

fn describe_sync(report: &peers::SyncReport) -> String {
    let mut text = format!(
        "synced ({} entries received, {} sent)",
        report.received, report.sent
    );
    if report.joined {
        text += "; this folder joined theirs";
    }
    text
}

/// Bring the folder in line with what syncs brought in.
fn apply_sync(repo: &mut Repo, state: &mut AppState, joined: bool) -> Result<(), String> {
    if joined {
        // The folder is now another device's: what this one knew is moot.
        state.repo_mut(&repo.root).forget_folder();
        state.save()?;
    }
    let applied = repo.apply_synced()?;
    for path in &applied.updated {
        println!("updated  {path}");
    }
    for path in &applied.kept {
        println!("kept     {path}  (changed here: `mbr scan` records it)");
    }
    for (path, why) in &applied.skipped {
        println!("skipped  {path}: {why}");
    }
    if applied.remotes_changed {
        let password = state.repo(&repo.root).password;
        if repo.write_rclone_config(password.as_deref()).is_err() {
            println!("the remotes synced need the master password here: `mbr config password`");
        }
    }
    Ok(())
}
