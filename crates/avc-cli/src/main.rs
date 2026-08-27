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

/// Contents of the gitignored `.avc/config.local.toml`.
#[derive(Debug, Deserialize, Default)]
struct LocalConfig {
    #[serde(default)]
    remotes: Vec<avc_core::LocalRemoteOverride>,
}

struct Repo {
    root: PathBuf,
    config: Config,
}

/// Exit code for expected user, data, or state errors. See `SPEC.md`.
const EXIT_USER_ERROR: i32 = 1;
/// Exit code for provider or operational failures. See `SPEC.md`.
const EXIT_PROVIDER_ERROR: i32 = 3;

/// A failure, carrying the exit code it should produce.
struct Failure {
    message: String,
    code: i32,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            code: EXIT_USER_ERROR,
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<avc_core::Error> for Failure {
    fn from(error: avc_core::Error) -> Self {
        // Now that transfers can fail for reasons outside the repository,
        // there is a real distinction to draw between a bad request and an
        // unreachable or unauthorized remote.
        let code = if error.is_provider_failure() {
            EXIT_PROVIDER_ERROR
        } else {
            EXIT_USER_ERROR
        };
        Self {
            message: error.to_string(),
            code,
        }
    }
}

fn main() {
    if let Err(failure) = run(Cli::parse().command) {
        eprintln!("avc: {}", failure.message);
        std::process::exit(failure.code);
    }
}

fn run(command: Command) -> Result<(), Failure> {
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

fn init() -> Result<(), Failure> {
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

fn remote(command: RemoteCommand) -> Result<(), Failure> {
    let mut repo = load_repo()?;
    match command {
        RemoteCommand::Add(args) => {
            let parsed = avc_core::RemoteConfig::from_url(&args.name, &args.provider_url)?;
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

fn add(paths: &[String]) -> Result<(), Failure> {
    let repo = load_repo()?;
    for value in paths {
        add_one(&repo, value, false)?;
    }
    Ok(())
}

fn commit(paths: &[String], force: bool) -> Result<(), Failure> {
    let repo = load_repo()?;
    for value in paths {
        add_one(&repo, value, force)?;
    }
    Ok(())
}

fn add_one(repo: &Repo, value: &str, require_pointer: bool) -> Result<(), Failure> {
    let relative = avc_core::normalize_repo_path(Path::new(value))?;
    let source = repo.root.join(&relative);
    if !source.is_file() {
        return Err(format!("artifact is not a regular file: {value}").into());
    }
    let pointer = repo.root.join(avc_core::pointer_path(&relative)?);
    if require_pointer && !pointer.exists() {
        return Err(format!("no pointer exists for {value}").into());
    }
    let hash = avc_core::hash_file(&source)?;
    let pointer_value = avc_core::Pointer::new(&relative, hash.object.clone(), hash.size, None)?;
    let object = cache_path(repo, &hash.object);
    if !object.exists() {
        copy_atomic(&source, &object)?;
    }
    append_ignore_path(&repo.root, &relative)?;
    write_atomic(&pointer, pointer_value.serialize_canonical()?.as_bytes())?;
    println!("tracked {relative} ({})", hash.object);
    Ok(())
}

fn status() -> Result<(), Failure> {
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
            let actual = avc_core::hash_file(&artifact)?;
            if actual.object != pointer.object_id()? || actual.size != pointer.object.size {
                "modified"
            } else {
                "ok"
            }
        };
        let cache = if cache_path(&repo, &pointer.object_id()?).exists() {
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

fn list(remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    let pointers = pointer_files(&repo.root)?;
    if pointers.is_empty() {
        println!("no AVC pointers found");
        return Ok(());
    }
    // One listing answers every pointer, so a repository with a thousand
    // artifacts costs one round trip rather than a thousand HEAD requests.
    let present: std::collections::HashSet<String> = store
        .list()?
        .into_iter()
        .map(|found| found.object.hash().to_owned())
        .collect();
    println!("PATH\tSIZE\tOBJECT\tREMOTE");
    for pointer_path in pointers {
        let pointer = parse_pointer(&pointer_path)?;
        let object = pointer.object_id()?;
        let remote_state = if present.contains(object.hash()) {
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

fn checkout(paths: &[String], force: bool) -> Result<(), Failure> {
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
        let object = cache_path(&repo, &pointer.object_id()?);
        if !object.is_file() {
            return Err(format!("cache object missing for {}", pointer.path).into());
        }
        let target = repo.root.join(&pointer.path);
        if target.exists() && !force {
            let actual = avc_core::hash_file(&target)?;
            if actual.object.hash() != pointer.object.hash {
                return Err(format!(
                    "refusing to replace modified file {}; use --force",
                    pointer.path
                )
                .into());
            }
        }
        copy_atomic(&object, &target)?;
        println!("checked out {}", pointer.path);
    }
    Ok(())
}

fn push(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    let mut uploaded = 0;
    for pointer in selected_pointers(&repo, paths)? {
        let object_id = pointer.object_id()?;
        let source = cache_path(&repo, &object_id);
        if !source.is_file() {
            return Err(format!("cache object missing for {}", pointer.path).into());
        }
        // Objects are immutable, so re-uploading identical bytes is pure cost.
        // Asking first turns a repeated push into a cheap no-op.
        if store.exists(&object_id)? {
            println!("up to date {}", pointer.path);
            continue;
        }
        let mut file = File::open(&source).map_err(io_error)?;
        store.put(&object_id, pointer.object.size, &mut file)?;
        println!("pushed {} ({})", pointer.path, object_id);
        uploaded += 1;
    }
    println!("pushed {uploaded} object(s) to {}", store.describe());
    Ok(())
}

fn pull(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    for pointer in selected_pointers(&repo, paths)? {
        let object_id = pointer.object_id()?;
        let destination = cache_path(&repo, &object_id);
        if destination.is_file() {
            continue;
        }
        let mut body = store.get(&object_id)?;
        download_verified(&mut body, &destination, &pointer)?;
        println!("pulled {}", pointer.path);
    }
    checkout(paths, false)
}

/// Stream a download into the cache, verifying size and digest before the
/// object becomes visible.
///
/// The hash is computed while the bytes are written, so a 40 GB artifact is
/// never read twice and a truncated or corrupted transfer never lands in the
/// cache under a name that claims it is intact.
fn download_verified(
    body: &mut dyn std::io::Read,
    destination: &Path,
    pointer: &avc_core::Pointer,
) -> Result<(), Failure> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
    let outcome = (|| -> Result<avc_core::HashResult, String> {
        let mut file = File::create(&temporary).map_err(io_error)?;
        let mut hasher = avc_core::StreamHasher::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = body.read(&mut buffer).map_err(io_error)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read]).map_err(io_error)?;
        }
        file.sync_all().map_err(io_error)?;
        hasher.finish().map_err(|error| error.to_string())
    })();

    let actual = match outcome {
        Ok(actual) => actual,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
    };
    if actual.size != pointer.object.size || actual.object.hash() != pointer.object.hash {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "remote object for {} does not match its pointer: expected {} bytes of {}, got {} bytes of {}",
            pointer.path, pointer.object.size, pointer.object.hash, actual.size, actual.object.hash()
        )
        .into());
    }
    fs::rename(&temporary, destination).map_err(io_error)?;
    Ok(())
}

fn remove(paths: &[String]) -> Result<(), Failure> {
    let repo = load_repo()?;
    for value in paths {
        let relative = avc_core::normalize_repo_path(Path::new(value))?;
        let pointer = repo.root.join(avc_core::pointer_path(&relative)?);
        if !pointer.exists() {
            return Err(format!("no pointer exists for {value}").into());
        }
        fs::remove_file(pointer).map_err(io_error)?;
        println!("untracked {relative}; artifact retained");
    }
    Ok(())
}

fn gc(_remote: Option<&str>, dry_run: bool) -> Result<(), Failure> {
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

fn doctor() -> Result<(), Failure> {
    let repo = load_repo()?;
    if !repo.root.join(".git").exists() {
        return Err("Git worktree not found".into());
    }
    for path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&path)?;
        let object = cache_path(&repo, &pointer.object_id()?);
        if object.exists() {
            let actual = avc_core::hash_file(&object)?;
            if actual.size != pointer.object.size || actual.object != pointer.object_id()? {
                return Err(format!("corrupt cache object for {}", pointer.path).into());
            }
        }
    }
    println!("doctor: repository, pointers, and available cache objects are valid");
    Ok(())
}

fn find_root() -> Result<PathBuf, Failure> {
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
fn load_repo() -> Result<Repo, Failure> {
    let root = find_root()?;
    let path = root.join(".avc/config.toml");
    if !path.exists() {
        return Err("AVC is not initialized; run `avc init`".into());
    }
    let text = fs::read_to_string(path).map_err(io_error)?;
    let config = if text.trim().is_empty() {
        Config::default()
    } else {
        toml::from_str(&text).map_err(io_error)?
    };
    Ok(Repo { root, config })
}
fn save_config(repo: &Repo) -> Result<(), Failure> {
    let path = repo.root.join(".avc/config.toml");
    let text = toml::to_string_pretty(&repo.config).map_err(io_error)?;
    write_atomic(&path, text.as_bytes())
}
fn append_ignore(root: &Path) -> Result<(), Failure> {
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
fn append_ignore_path(root: &Path, relative: &str) -> Result<(), Failure> {
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
fn pointer_files(root: &Path) -> Result<Vec<PathBuf>, Failure> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    Ok(files)
}
fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), Failure> {
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
fn parse_pointer(relative: &Path) -> Result<avc_core::Pointer, Failure> {
    let root = find_root()?;
    avc_core::Pointer::parse(&fs::read_to_string(root.join(relative)).map_err(io_error)?)
        .map_err(|error| format!("{}: {error}", relative.display()).into())
}
fn cache_path(repo: &Repo, object: &avc_core::ObjectId) -> PathBuf {
    repo.root.join(".avc/cache").join(object.cache_key())
}
/// Build the transport for the selected remote.
///
/// Provider dispatch lives in `avc-core`; this only assembles the tracked
/// configuration with any machine-local overrides before handing both over.
fn open_store(repo: &Repo, name: Option<&str>) -> Result<Box<dyn avc_core::ObjectStore>, Failure> {
    let remote = choose_remote(repo, name)?;
    let local = load_local_override(repo, &remote.name)?;
    let config = avc_core::RemoteConfig {
        name: remote.name.clone(),
        provider: remote.provider.clone(),
        bucket_or_container: remote.bucket_or_container.clone(),
        prefix: remote.prefix.clone(),
        endpoint_url: remote.endpoint_url.clone(),
    };
    Ok(avc_core::remote::open(&config, local.as_ref())?)
}

/// Read `.avc/config.local.toml`, which is gitignored and holds credentials.
///
/// A malformed file is an error rather than a silent fallback: quietly ignoring
/// it would send a request to the wrong endpoint, or with no credentials at all.
fn load_local_override(
    repo: &Repo,
    name: &str,
) -> Result<Option<avc_core::LocalRemoteOverride>, Failure> {
    let path = repo.root.join(".avc/config.local.toml");
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(io_error)?;
    let local: LocalConfig =
        toml::from_str(&text).map_err(|error| format!(".avc/config.local.toml: {error}"))?;
    Ok(local.remotes.into_iter().find(|remote| remote.name == name))
}

/// The pointers a command should act on: all of them, or just the paths named.
fn selected_pointers(repo: &Repo, paths: &[String]) -> Result<Vec<avc_core::Pointer>, Failure> {
    let mut selected = Vec::new();
    for pointer_path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&pointer_path)?;
        if paths.is_empty() || paths.iter().any(|value| value == &pointer.path) {
            selected.push(pointer);
        }
    }
    if !paths.is_empty() {
        // A typo in a path should not be reported as "nothing to do".
        for value in paths {
            if !selected.iter().any(|pointer| &pointer.path == value) {
                return Err(format!("no pointer exists for {value}").into());
            }
        }
    }
    Ok(selected)
}

fn choose_remote<'a>(repo: &'a Repo, name: Option<&str>) -> Result<&'a Remote, Failure> {
    let selected = name
        .or(repo.config.default_remote.as_deref())
        .ok_or("no remote configured")?;
    repo.config
        .remotes
        .iter()
        .find(|remote| remote.name == selected)
        .ok_or_else(|| format!("remote not found: {selected}").into())
}
fn cache_objects(repo: &Repo) -> Result<Vec<(String, PathBuf)>, Failure> {
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
fn copy_atomic(source: &Path, destination: &Path) -> Result<(), Failure> {
    let mut input = File::open(source).map_err(io_error)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
    let mut output = File::create(&temporary).map_err(io_error)?;
    std::io::copy(&mut input, &mut output).map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    fs::rename(&temporary, destination).map_err(io_error)?;
    Ok(())
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = File::create(&temporary).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    fs::rename(&temporary, path).map_err(io_error)?;
    Ok(())
}
fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
