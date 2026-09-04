//! `mbr` — the My Beloved Rubble command-line interface.

use mbr::db::Remote;
use mbr::repo::Repo;
use mbr::util;
use std::path::Path;

const USAGE: &str = "\
mbr — my beloved rubble: content-addressed folders with rclone remotes

USAGE:
  mbr init [dir]                    attach mbr to a folder
  mbr scan                          ingest new files, record removals
  mbr ls                            list tracked files with metadata
  mbr info <path|hash>              full detail for one file or blob
  mbr remote add <name> <target>    add an existing rclone target
  mbr remote setup <name> <type>    configure an rclone remote interactively
  mbr remote rm <name>              remove a remote
  mbr remote ls                     list remotes
  mbr push <remote> [path|hash...]  push blobs (default: all not yet there)
  mbr fetch <path|hash>...          download blobs missing locally
  mbr check <remote>                verify remote is reachable and has our blobs

Commands other than `init` run inside an attached folder (or below one).
";

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(args: &[&str]) -> Result<(), String> {
    match args {
        [] | ["help"] | ["--help"] | ["-h"] => {
            print!("{USAGE}");
            Ok(())
        }
        ["init"] => init(Path::new(".")),
        ["init", dir] => init(Path::new(dir)),
        ["scan"] => scan(&mut open()?),
        ["ls"] => ls(&mut open()?),
        ["info", spec] => info(&mut open()?, spec),
        ["remote", "add", name, target] => remote_add(&mut open()?, name, target),
        ["remote", "setup", name, backend] => remote_setup(&mut open()?, name, backend),
        ["remote", "rm", name] => remote_rm(&mut open()?, name),
        ["remote", "ls"] => remote_ls(&mut open()?),
        ["push", remote, specs @ ..] => push(&mut open()?, remote, specs),
        ["fetch", specs @ ..] if !specs.is_empty() => fetch(&mut open()?, specs),
        ["check", remote] => check(&mut open()?, remote),
        _ => Err(format!(
            "unrecognized command: {}\nrun `mbr help` for usage",
            args.join(" ")
        )),
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
        return Err(format!("no active file uses blob {}", util::short_hash(&hash)));
    };

    println!("path:     {}", s.path);
    println!("hash:     {}", s.hash);
    println!("size:     {} ({} bytes)", util::format_size(s.size), s.size);
    println!("added:    {}", util::format_time(s.added_at));
    println!("local:    {}", if s.present_locally { "present" } else { "missing" });
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
        let stored = if v.present_locally && v.remotes.is_empty() {
            "local only".to_owned()
        } else if v.present_locally {
            format!("local, {}", v.remotes.join(", "))
        } else if v.remotes.is_empty() {
            "NOT STORED ANYWHERE".to_owned()
        } else {
            v.remotes.join(", ")
        };
        println!(
            "previous version: {} ({}, removed {}, stored: {stored})",
            util::short_hash(&v.hash),
            util::format_size(v.size),
            util::format_time(v.removed_at),
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

fn remote_setup(repo: &mut Repo, name: &str, backend: &str) -> Result<(), String> {
    if repo.db.remote(name)?.is_some() {
        return Err(format!("remote '{name}' already exists"));
    }
    repo.configure_rclone()?;
    if mbr::rclone::call("config/listremotes", serde_json::json!({}))?
        .get("remotes")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|remotes| remotes.iter().any(|remote| remote.as_str() == Some(name)))
    {
        return Err(format!("rclone remote '{name}' already exists"));
    }

    println!("Configuring rclone remote '{name}' ({backend})");
    mbr::rclone::setup_remote(name, backend)?;
    let path = prompt("remote path within the backend", "mbr")?;
    let target = format!("{name}:{path}");
    repo.db.add_remote(name, &target)?;
    println!("added remote {name} -> {target}");
    Ok(())
}

fn prompt(label: &str, default: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{label} [{default}]: ");
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
        println!("no remotes configured (use `mbr remote setup <name> <type>`)");
    }
    for r in remotes {
        let count = repo.db.blobs_on_remote(&r.name)?.len();
        println!("{:<16} {}  ({count} blob(s))", r.name, r.target);
    }
    Ok(())
}

fn push(repo: &mut Repo, remote_name: &str, specs: &[&str]) -> Result<(), String> {
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

fn fetch(repo: &mut Repo, specs: &[&str]) -> Result<(), String> {
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

fn check(repo: &mut Repo, remote_name: &str) -> Result<(), String> {
    let remote = lookup_remote(repo, remote_name)?;
    let check = repo.check_remote(&remote)?;
    println!("remote '{remote_name}' ({}) is reachable", remote.target);
    println!("  {} expected blob(s) present", check.present.len());
    for hash in &check.missing {
        println!("  MISSING: {} (was recorded there; record dropped)", hash);
    }
    for hash in &check.discovered {
        println!("  found unrecorded blob {} (record added)", util::short_hash(hash));
    }
    if check.missing.is_empty() && check.discovered.is_empty() {
        println!("  database and remote agree");
    }
    Ok(())
}
