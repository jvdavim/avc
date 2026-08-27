use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "avc", version, about = "Artifact Version Control")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init,
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    Add(Paths),
    /// List tracked artifact paths without downloading artifact bytes.
    List(ListArgs),
    Status,
    Commit(CommitArgs),
    Push(SyncArgs),
    Pull(SyncArgs),
    Checkout(CheckoutArgs),
    Remove(Paths),
    Gc(GcArgs),
    Doctor,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    Add(RemoteAddArgs),
    List,
}

#[derive(Debug, Args)]
struct RemoteAddArgs {
    name: String,
    provider_url: String,
}

#[derive(Debug, Args)]
struct Paths {
    #[arg(required = true)]
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct CommitArgs {
    #[command(flatten)]
    paths: Paths,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct SyncArgs {
    paths: Vec<String>,
    #[arg(long)]
    remote: Option<String>,
}

#[derive(Debug, Args)]
struct CheckoutArgs {
    paths: Vec<String>,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct GcArgs {
    #[arg(long)]
    remote: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long)]
    remote: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    default_remote: Option<String>,
    #[serde(default)]
    remotes: Vec<Remote>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Remote {
    name: String,
    provider: avc_core::Provider,
    bucket_or_container: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    endpoint_url: Option<String>,
}

struct Repo {
    root: PathBuf,
    config: Config,
}

fn main() {
    if let Err(error) = run(Cli::parse().command) {
        eprintln!("avc: {error}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Init => init(),
        Command::Remote { command } => remote(command),
        Command::Add(args) => add(&args.paths),
        Command::List(args) => list(args.remote.as_deref()),
        Command::Status => status(),
        Command::Commit(args) => commit(&args.paths.paths, args.force),
        Command::Push(args) => push(&args.paths, args.remote.as_deref()),
        Command::Pull(args) => pull(&args.paths, args.remote.as_deref()),
        Command::Checkout(args) => checkout(&args.paths, args.force),
        Command::Remove(args) => remove(&args.paths),
        Command::Gc(args) => gc(args.remote.as_deref(), args.dry_run),
        Command::Doctor => doctor(),
    }
}

fn init() -> Result<(), String> {
    let root = find_root()?;
    if !root.join(".git").exists() {
        return Err("current directory is not a Git worktree".into());
    }
    fs::create_dir_all(root.join(".avc/cache")).map_err(io_error)?;
    fs::create_dir_all(root.join(".avc/state")).map_err(io_error)?;
    let config = root.join(".avc/config.toml");
    if !config.exists() {
        write_atomic(&config, b"# AVC repository configuration\n")?;
    }
    append_ignore(&root)?;
    println!("initialized AVC in {}", root.display());
    Ok(())
}

fn remote(command: RemoteCommand) -> Result<(), String> {
    let mut repo = load_repo()?;
    match command {
        RemoteCommand::Add(args) => {
            let parsed = avc_core::RemoteConfig::from_url(&args.name, &args.provider_url)
                .map_err(|e| e.to_string())?;
            repo.config
                .remotes
                .retain(|remote| remote.name != args.name);
            repo.config.remotes.push(Remote {
                name: args.name.clone(),
                provider: parsed.provider,
                bucket_or_container: parsed.bucket_or_container,
                prefix: parsed.prefix,
                endpoint_url: parsed.endpoint_url,
            });
            if repo.config.default_remote.is_none() {
                repo.config.default_remote = Some(args.name);
            }
            save_config(&repo)?;
            println!("remote configured");
        }
        RemoteCommand::List => {
            for remote in repo.config.remotes {
                let marker = if repo.config.default_remote.as_deref() == Some(&remote.name) {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{marker} {} {:?} {}",
                    remote.name, remote.provider, remote.bucket_or_container
                );
            }
        }
    }
    Ok(())
}

fn add(paths: &[String]) -> Result<(), String> {
    let repo = load_repo()?;
    for value in paths {
        add_one(&repo, value, false)?;
    }
    Ok(())
}

fn commit(paths: &[String], force: bool) -> Result<(), String> {
    let repo = load_repo()?;
    for value in paths {
        add_one(&repo, value, force)?;
    }
    Ok(())
}

fn add_one(repo: &Repo, value: &str, require_pointer: bool) -> Result<(), String> {
    let relative = avc_core::normalize_repo_path(Path::new(value)).map_err(|e| e.to_string())?;
    let source = repo.root.join(&relative);
    if !source.is_file() {
        return Err(format!("artifact is not a regular file: {value}"));
    }
    let pointer = repo
        .root
        .join(avc_core::pointer_path(&relative).map_err(|e| e.to_string())?);
    if require_pointer && !pointer.exists() {
        return Err(format!("no pointer exists for {value}"));
    }
    let hash = avc_core::hash_file(&source).map_err(|e| e.to_string())?;
    let pointer_value = avc_core::Pointer::new(&relative, hash.object.clone(), hash.size, None)
        .map_err(|e| e.to_string())?;
    let object = cache_path(repo, &hash.object);
    if !object.exists() {
        copy_atomic(&source, &object)?;
    }
    append_ignore_path(&repo.root, &relative)?;
    write_atomic(
        &pointer,
        pointer_value
            .serialize_canonical()
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    println!("tracked {relative} ({})", hash.object);
    Ok(())
}

fn status() -> Result<(), String> {
    let repo = load_repo()?;
    let mut count = 0;
    for pointer_path in pointer_files(&repo.root)? {
        let pointer = match parse_pointer(&pointer_path) {
            Ok(value) => value,
            Err(error) => {
                println!("invalid {}: {error}", pointer_path.display());
                continue;
            }
        };
        count += 1;
        let artifact = repo.root.join(&pointer.path);
        let state = if !artifact.exists() {
            "missing"
        } else {
            let actual = avc_core::hash_file(&artifact).map_err(|e| e.to_string())?;
            if actual.object != pointer.object_id().map_err(|e| e.to_string())?
                || actual.size != pointer.object.size
            {
                "modified"
            } else {
                "ok"
            }
        };
        let cache = if cache_path(&repo, &pointer.object_id().map_err(|e| e.to_string())?).exists()
        {
            "cached"
        } else {
            "cache-missing"
        };
        println!("{state}\t{cache}\t{}", pointer.path);
    }
    if count == 0 {
        println!("no AVC pointers found");
    }
    Ok(())
}

fn list(remote_name: Option<&str>) -> Result<(), String> {
    let repo = load_repo()?;
    let remote = choose_remote(&repo, remote_name)?;
    let pointers = pointer_files(&repo.root)?;
    if pointers.is_empty() {
        println!("no AVC pointers found");
        return Ok(());
    }
    if !matches!(remote.provider, avc_core::Provider::File) {
        return Err(format!(
            "remote '{}' uses {:?}; remote listing is unavailable until provider adapter is implemented",
            remote.name, remote.provider
        ));
    }
    println!("PATH\tSIZE\tOBJECT\tREMOTE");
    for pointer_path in pointers {
        let pointer = parse_pointer(&pointer_path)?;
        let object = pointer.object_id().map_err(|e| e.to_string())?;
        let remote_state = if remote_path(remote, &object).is_file() {
            "available"
        } else {
            "missing"
        };
        println!(
            "{}\t{}\t{}\t{}",
            pointer.path, pointer.object.size, object, remote_state
        );
    }
    Ok(())
}

fn checkout(paths: &[String], force: bool) -> Result<(), String> {
    let repo = load_repo()?;
    let selected = pointer_files(&repo.root)?.into_iter().filter(|path| {
        paths.is_empty()
            || paths.iter().any(|value| {
                avc_core::pointer_path(value)
                    .map(|pointer| pointer == *path)
                    .unwrap_or(false)
            })
    });
    for pointer_path in selected {
        let pointer = parse_pointer(&pointer_path)?;
        let object = cache_path(&repo, &pointer.object_id().map_err(|e| e.to_string())?);
        if !object.is_file() {
            return Err(format!("cache object missing for {}", pointer.path));
        }
        let target = repo.root.join(&pointer.path);
        if target.exists() && !force {
            let actual = avc_core::hash_file(&target).map_err(|e| e.to_string())?;
            if actual.object.hash() != pointer.object.hash {
                return Err(format!(
                    "refusing to replace modified file {}; use --force",
                    pointer.path
                ));
            }
        }
        copy_atomic(&object, &target)?;
        println!("checked out {}", pointer.path);
    }
    Ok(())
}

fn push(paths: &[String], remote_name: Option<&str>) -> Result<(), String> {
    let repo = load_repo()?;
    let remote = choose_remote(&repo, remote_name)?;
    if !matches!(remote.provider, avc_core::Provider::File) {
        return Err("only file:// remotes are implemented; cloud adapters are planned next".into());
    }
    for pointer_path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&pointer_path)?;
        if !paths.is_empty() && !paths.iter().any(|p| p == &pointer.path) {
            continue;
        }
        let object = cache_path(&repo, &pointer.object_id().map_err(|e| e.to_string())?);
        if !object.is_file() {
            return Err(format!("cache object missing for {}", pointer.path));
        }
        let destination = remote_path(remote, &pointer.object_id().map_err(|e| e.to_string())?);
        copy_atomic(&object, &destination)?;
        println!("pushed {}", pointer.object.hash);
    }
    Ok(())
}

fn pull(paths: &[String], remote_name: Option<&str>) -> Result<(), String> {
    let repo = load_repo()?;
    let remote = choose_remote(&repo, remote_name)?;
    if !matches!(remote.provider, avc_core::Provider::File) {
        return Err("only file:// remotes are implemented; cloud adapters are planned next".into());
    }
    for pointer_path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&pointer_path)?;
        if !paths.is_empty() && !paths.iter().any(|p| p == &pointer.path) {
            continue;
        }
        let object_id = pointer.object_id().map_err(|e| e.to_string())?;
        let source = remote_path(remote, &object_id);
        if !source.is_file() {
            return Err(format!("remote object missing for {}", pointer.path));
        }
        let destination = cache_path(&repo, &object_id);
        copy_atomic(&source, &destination)?;
        println!("pulled {}", pointer.path);
    }
    checkout(paths, false)
}

fn remove(paths: &[String]) -> Result<(), String> {
    let repo = load_repo()?;
    for value in paths {
        let relative =
            avc_core::normalize_repo_path(Path::new(value)).map_err(|e| e.to_string())?;
        let pointer = repo
            .root
            .join(avc_core::pointer_path(&relative).map_err(|e| e.to_string())?);
        if !pointer.exists() {
            return Err(format!("no pointer exists for {value}"));
        }
        fs::remove_file(pointer).map_err(io_error)?;
        println!("untracked {relative}; artifact retained");
    }
    Ok(())
}

fn gc(_remote: Option<&str>, dry_run: bool) -> Result<(), String> {
    let repo = load_repo()?;
    let reachable: std::collections::HashSet<_> = pointer_files(&repo.root)?
        .into_iter()
        .filter_map(|path| {
            parse_pointer(&path)
                .ok()
                .and_then(|p| p.object_id().ok().map(|id| id.hash().to_owned()))
        })
        .collect();
    let objects = cache_objects(&repo)?;
    for object in objects {
        if !reachable.contains(&object.0) {
            if dry_run {
                println!("would remove {}", object.1.display());
            } else {
                fs::remove_file(object.1).map_err(io_error)?;
                println!("removed {}", object.0);
            }
        }
    }
    Ok(())
}

fn doctor() -> Result<(), String> {
    let repo = load_repo()?;
    if !repo.root.join(".git").exists() {
        return Err("Git worktree not found".into());
    }
    for path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&path)?;
        let object = cache_path(&repo, &pointer.object_id().map_err(|e| e.to_string())?);
        if object.exists() {
            let actual = avc_core::hash_file(&object).map_err(|e| e.to_string())?;
            if actual.size != pointer.object.size
                || actual.object != pointer.object_id().map_err(|e| e.to_string())?
            {
                return Err(format!("corrupt cache object for {}", pointer.path));
            }
        }
    }
    println!("doctor: repository, pointers, and available cache objects are valid");
    Ok(())
}

fn find_root() -> Result<PathBuf, String> {
    let mut current = std::env::current_dir().map_err(io_error)?;
    loop {
        if current.join(".git").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Err("not inside a Git worktree".into());
        }
    }
}
fn load_repo() -> Result<Repo, String> {
    let root = find_root()?;
    let path = root.join(".avc/config.toml");
    if !path.exists() {
        return Err("AVC is not initialized; run `avc init`".into());
    }
    let text = fs::read_to_string(path).map_err(io_error)?;
    let config = if text.trim().is_empty() {
        Config::default()
    } else {
        toml::from_str(&text).map_err(|e| e.to_string())?
    };
    Ok(Repo { root, config })
}
fn save_config(repo: &Repo) -> Result<(), String> {
    let path = repo.root.join(".avc/config.toml");
    let text = toml::to_string_pretty(&repo.config).map_err(|e| e.to_string())?;
    write_atomic(&path, text.as_bytes())
}
fn append_ignore(root: &Path) -> Result<(), String> {
    let path = root.join(".gitignore");
    let old = fs::read_to_string(&path).unwrap_or_default();
    let marker = ".avc/cache/";
    if !old.lines().any(|line| line == marker) {
        let suffix = if old.ends_with('\n') || old.is_empty() {
            ""
        } else {
            "\n"
        };
        let text = format!("{old}{suffix}.avc/cache/\n.avc/config.local.toml\n");
        write_atomic(&path, text.as_bytes())?;
    }
    Ok(())
}
fn append_ignore_path(root: &Path, relative: &str) -> Result<(), String> {
    let path = root.join(".gitignore");
    let old = fs::read_to_string(&path).unwrap_or_default();
    if old.lines().any(|line| line == relative) {
        return Ok(());
    }
    let suffix = if old.ends_with('\n') || old.is_empty() {
        ""
    } else {
        "\n"
    };
    write_atomic(&path, format!("{old}{suffix}{relative}\n").as_bytes())
}
fn pointer_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    Ok(files)
}
fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path.file_name().and_then(|n| n.to_str()) == Some(".git")
            || path.file_name().and_then(|n| n.to_str()) == Some("target")
        {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, output)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("avc") {
            output.push(path.strip_prefix(root).map_err(io_error)?.to_path_buf());
        }
    }
    Ok(())
}
fn parse_pointer(relative: &Path) -> Result<avc_core::Pointer, String> {
    let root = find_root()?;
    avc_core::Pointer::parse(&fs::read_to_string(root.join(relative)).map_err(io_error)?)
        .map_err(|e| format!("{}: {e}", relative.display()))
}
fn cache_path(repo: &Repo, object: &avc_core::ObjectId) -> PathBuf {
    repo.root.join(".avc/cache").join(object.cache_key())
}
fn remote_path(remote: &Remote, object: &avc_core::ObjectId) -> PathBuf {
    PathBuf::from(&remote.bucket_or_container)
        .join(&remote.prefix)
        .join(object.cache_key())
}
fn choose_remote<'a>(repo: &'a Repo, name: Option<&str>) -> Result<&'a Remote, String> {
    let selected = name
        .or(repo.config.default_remote.as_deref())
        .ok_or("no remote configured")?;
    repo.config
        .remotes
        .iter()
        .find(|remote| remote.name == selected)
        .ok_or_else(|| format!("remote not found: {selected}"))
}
fn cache_objects(repo: &Repo) -> Result<Vec<(String, PathBuf)>, String> {
    let root = repo.root.join(".avc/cache/objects/sha256");
    let mut result = Vec::new();
    if !root.exists() {
        return Ok(result);
    }
    for prefix in fs::read_dir(root).map_err(io_error)? {
        let dir = prefix.map_err(io_error)?.path();
        if dir.is_dir() {
            for entry in fs::read_dir(dir).map_err(io_error)? {
                let path = entry.map_err(io_error)?.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    result.push((name.to_owned(), path));
                }
            }
        }
    }
    Ok(result)
}
fn copy_atomic(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = File::open(source).map_err(io_error)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
    let mut output = File::create(&temporary).map_err(io_error)?;
    std::io::copy(&mut input, &mut output).map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    fs::rename(&temporary, destination).map_err(io_error)
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = File::create(&temporary).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    fs::rename(&temporary, path).map_err(io_error)
}
fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
