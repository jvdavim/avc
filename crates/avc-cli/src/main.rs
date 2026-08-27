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
    let pointer = repo.root.join(avc_core::pointer_path(&relative)?);
    if require_pointer && !pointer.exists() {
        return Err(format!("no pointer exists for {value}").into());
    }
    // A directory is checked first: `is_file` follows symlinks, and the two
    // tests are mutually exclusive, so the order only decides the message a
    // path that is neither gets.
    let (pointer_value, summary) = if source.is_dir() {
        track_directory(repo, &relative)?
    } else if source.is_file() {
        track_file(repo, &relative)?
    } else {
        return Err(format!("artifact is not a regular file or directory: {value}").into());
    };
    append_ignore_path(
        &repo.root,
        &ignore_line(&relative, pointer_value.is_directory()),
    )?;
    write_atomic(&pointer, pointer_value.serialize_canonical()?.as_bytes())?;
    println!("tracked {summary}");
    Ok(())
}

/// Hash a file, store its bytes, and describe it.
fn track_file(repo: &Repo, relative: &str) -> Result<(avc_core::Pointer, String), Failure> {
    let source = repo.root.join(relative);
    let hash = avc_core::hash_file(&source)?;
    store_in_cache(repo, &source, &hash.object)?;
    let pointer = avc_core::Pointer::new(relative, hash.object.clone(), hash.size, None)?;
    Ok((pointer, format!("{relative} ({})", hash.object)))
}

/// Hash every file beneath a directory, store them, and store the manifest
/// that names them.
///
/// The manifest is an object like any other, so a directory costs one extra
/// object and travels through push, pull, and gc unchanged. Files that already
/// exist in the cache — including identical files inside the same directory —
/// are not copied twice.
fn track_directory(repo: &Repo, relative: &str) -> Result<(avc_core::Pointer, String), Failure> {
    let root = repo.root.join(relative);
    let scanned = scan_directory(&root)?;
    if scanned.is_empty() {
        return Err(format!("directory contains no files to track: {relative}").into());
    }
    // Pointers are discovered by scanning the worktree for `.avc` files, so
    // one inside a tracked directory would be both content and pointer. Say so
    // now rather than leaving a repository whose `push` cannot parse itself.
    if let Some((_, entry)) = scanned
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".avc"))
    {
        return Err(format!(
            "refusing to track {relative}: it contains the pointer file {}/{}",
            relative, entry.path
        )
        .into());
    }
    for (source, entry) in &scanned {
        store_in_cache(repo, source, &entry.object_id()?)?;
    }
    let tree = avc_core::Tree::new(scanned.into_iter().map(|(_, entry)| entry).collect())?;
    let manifest = write_manifest(repo, &tree)?;
    let pointer =
        avc_core::Pointer::new_directory(relative, manifest.object.clone(), manifest.size)?;
    Ok((
        pointer,
        format!(
            "{relative}/ ({} file(s), {}, {})",
            tree.entries.len(),
            human_size(tree.total_size()),
            manifest.object
        ),
    ))
}

/// Every regular file beneath `root`, paired with the manifest entry that
/// describes it.
///
/// Entry paths are relative to `root`, and ordering is left to
/// `Tree::new`, so the manifest never depends on directory-iteration order.
fn scan_directory(root: &Path) -> Result<Vec<(PathBuf, avc_core::TreeEntry)>, Failure> {
    let mut files = Vec::new();
    collect_artifact_files(root, root, &mut files)?;
    let mut scanned = Vec::with_capacity(files.len());
    for (source, relative) in files {
        let hash = avc_core::hash_file(&source)?;
        scanned.push((
            source,
            avc_core::TreeEntry::new(relative, hash.object, hash.size)?,
        ));
    }
    Ok(scanned)
}

/// Walk `directory`, collecting regular files as (absolute path, path relative
/// to `root`).
///
/// Symlinks are skipped rather than followed: following them would let a link
/// out of the directory pull unrelated bytes in, and a link back into it loop
/// forever.
fn collect_artifact_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(PathBuf, String)>,
) -> Result<(), Failure> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        let kind = fs::symlink_metadata(&path).map_err(io_error)?.file_type();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_artifact_files(root, &path, output)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(io_error)?;
            output.push((path.clone(), avc_core::normalize_repo_path(relative)?));
        }
    }
    Ok(())
}

/// Copy an artifact's bytes into the cache unless that object is already there.
fn store_in_cache(repo: &Repo, source: &Path, object: &avc_core::ObjectId) -> Result<(), Failure> {
    let destination = cache_path(repo, object);
    if !destination.exists() {
        copy_atomic(source, &destination)?;
    }
    Ok(())
}

/// Serialize a manifest into the cache and report the object it became.
fn write_manifest(repo: &Repo, tree: &avc_core::Tree) -> Result<avc_core::HashResult, Failure> {
    let bytes = tree.serialize_canonical()?.into_bytes();
    let manifest = avc_core::hash_reader(&mut bytes.as_slice())?;
    let destination = cache_path(repo, &manifest.object);
    if !destination.exists() {
        write_atomic(&destination, &bytes)?;
    }
    Ok(manifest)
}

/// Read a directory pointer's manifest out of the cache.
///
/// The bytes are verified against the pointer before they are parsed: a
/// manifest decides where `checkout` writes, so it is treated as untrusted
/// input even coming off the local disk.
fn load_tree(repo: &Repo, pointer: &avc_core::Pointer) -> Result<avc_core::Tree, Failure> {
    let object = pointer.object_id()?;
    let path = cache_path(repo, &object);
    if !path.is_file() {
        return Err(format!(
            "cache object missing for {}; run `avc pull {}`",
            pointer.path, pointer.path
        )
        .into());
    }
    let bytes = fs::read(&path).map_err(io_error)?;
    let actual = avc_core::hash_reader(&mut bytes.as_slice())?;
    if actual.size != pointer.object.size || actual.object != object {
        return Err(format!("corrupt cache object for {}", pointer.path).into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("directory manifest for {} is not UTF-8", pointer.path))?;
    Ok(avc_core::Tree::parse(&text)?)
}

/// Every object a pointer needs, manifest first.
///
/// A file needs one object; a directory needs its manifest plus one object per
/// file it names. Duplicates are collapsed, so a directory holding the same
/// bytes twice transfers them once.
fn required_objects(
    repo: &Repo,
    pointer: &avc_core::Pointer,
) -> Result<Vec<(avc_core::ObjectId, u64)>, Failure> {
    let object = pointer.object_id()?;
    let mut required = vec![(object, pointer.object.size)];
    if pointer.is_directory() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in load_tree(repo, pointer)?.entries {
            if seen.insert(entry.hash.clone()) {
                required.push((entry.object_id()?, entry.size));
            }
        }
    }
    Ok(required)
}

/// The `.gitignore` line for a tracked artifact.
///
/// A directory gets a trailing slash so the pattern cannot also match a file
/// of the same name elsewhere in the tree.
fn ignore_line(relative: &str, directory: bool) -> String {
    if directory {
        format!("{relative}/")
    } else {
        relative.to_owned()
    }
}

/// Byte counts for humans, so a directory summary is readable at a glance.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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
        let state = worktree_state(&repo, &pointer)?;
        let cache = if cached_completely(&repo, &pointer)? {
            "cached"
        } else {
            "cache-missing"
        };
        println!("{state}\t{cache}\t{}", display_path(&pointer));
    }
    if count == 0 {
        println!("no AVC pointers found");
    }
    Ok(())
}

/// Compare an artifact against its pointer.
///
/// A directory is re-scanned and re-hashed into a manifest: its identity is
/// that manifest's hash, so a file added, removed, renamed, or edited anywhere
/// beneath it reads as `modified` exactly as an edited file does.
fn worktree_state(repo: &Repo, pointer: &avc_core::Pointer) -> Result<&'static str, Failure> {
    let artifact = repo.root.join(&pointer.path);
    if pointer.is_directory() {
        if !artifact.is_dir() {
            return Ok("missing");
        }
        let scanned = scan_directory(&artifact)?;
        let tree = avc_core::Tree::new(scanned.into_iter().map(|(_, entry)| entry).collect())?;
        let bytes = tree.serialize_canonical()?.into_bytes();
        let actual = avc_core::hash_reader(&mut bytes.as_slice())?;
        return Ok(if actual.object.hash() == pointer.object.hash {
            "ok"
        } else {
            "modified"
        });
    }
    if !artifact.exists() {
        return Ok("missing");
    }
    let actual = avc_core::hash_file(&artifact)?;
    if actual.object != pointer.object_id()? || actual.size != pointer.object.size {
        Ok("modified")
    } else {
        Ok("ok")
    }
}

/// Whether every object the artifact needs is in the cache.
///
/// A directory whose manifest is cached but whose files are not cannot be
/// checked out, so it is reported as `cache-missing` rather than `cached`.
fn cached_completely(repo: &Repo, pointer: &avc_core::Pointer) -> Result<bool, Failure> {
    if !cache_path(repo, &pointer.object_id()?).exists() {
        return Ok(false);
    }
    if !pointer.is_directory() {
        return Ok(true);
    }
    Ok(required_objects(repo, pointer)?
        .iter()
        .all(|(object, _)| cache_path(repo, object).exists()))
}

/// How a pointer's path is shown, with a trailing slash for a directory.
fn display_path(pointer: &avc_core::Pointer) -> String {
    ignore_line(&pointer.path, pointer.is_directory())
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
        // A directory is available only when its manifest *and* every file it
        // names are on the remote; a half-uploaded directory is not restorable.
        let (size, remote_state) = if pointer.is_directory() {
            match remote_tree(&repo, store.as_ref(), &pointer, &present)? {
                Some(tree) => {
                    let complete = present.contains(object.hash())
                        && tree
                            .entries
                            .iter()
                            .all(|entry| present.contains(&entry.hash));
                    (
                        tree.total_size().to_string(),
                        if complete { "available" } else { "missing" },
                    )
                }
                // Without the manifest the file list is unknowable, and the
                // remote demonstrably cannot restore the directory.
                None => ("-".to_string(), "missing"),
            }
        } else {
            (
                pointer.object.size.to_string(),
                if present.contains(object.hash()) {
                    "available"
                } else {
                    "missing"
                },
            )
        };
        println!(
            "{}\t{}\t{}\t{}",
            display_path(&pointer),
            size,
            object,
            remote_state
        );
    }
    Ok(())
}

/// The manifest for a directory pointer, from the cache or, failing that, from
/// the remote.
///
/// A manifest is metadata measured in bytes per file, not artifact content, so
/// fetching one keeps `list` honest about a directory's size and availability
/// without breaking its promise not to download artifacts.
fn remote_tree(
    repo: &Repo,
    store: &dyn avc_core::ObjectStore,
    pointer: &avc_core::Pointer,
    present: &std::collections::HashSet<String>,
) -> Result<Option<avc_core::Tree>, Failure> {
    let object = pointer.object_id()?;
    if cache_path(repo, &object).is_file() {
        return Ok(Some(load_tree(repo, pointer)?));
    }
    if !present.contains(object.hash()) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::io::copy(&mut store.get(&object)?, &mut bytes).map_err(io_error)?;
    let actual = avc_core::hash_reader(&mut bytes.as_slice())?;
    if actual.size != pointer.object.size || actual.object != object {
        return Err(format!(
            "remote object for {} does not match its pointer",
            pointer.path
        )
        .into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("directory manifest for {} is not UTF-8", pointer.path))?;
    Ok(Some(avc_core::Tree::parse(&text)?))
}

fn checkout(paths: &[String], force: bool) -> Result<(), Failure> {
    let repo = load_repo()?;
    for pointer in selected_pointers(&repo, paths)? {
        if pointer.is_directory() {
            let tree = load_tree(&repo, &pointer)?;
            for entry in &tree.entries {
                let label = format!("{}/{}", pointer.path, entry.path);
                materialize(
                    &repo,
                    &entry.object_id()?,
                    &repo.root.join(&pointer.path).join(&entry.path),
                    &entry.hash,
                    force,
                    &label,
                )?;
            }
            println!(
                "checked out {}/ ({} file(s))",
                pointer.path,
                tree.entries.len()
            );
            continue;
        }
        materialize(
            &repo,
            &pointer.object_id()?,
            &repo.root.join(&pointer.path),
            &pointer.object.hash,
            force,
            &pointer.path,
        )?;
        println!("checked out {}", pointer.path);
    }
    Ok(())
}

/// Write one cached object into the working tree.
///
/// The refusal to clobber differing content applies per file, so a directory
/// checkout stops on the first locally modified file rather than overwriting
/// the ones before it and then complaining.
fn materialize(
    repo: &Repo,
    object: &avc_core::ObjectId,
    target: &Path,
    expected_hash: &str,
    force: bool,
    label: &str,
) -> Result<(), Failure> {
    let source = cache_path(repo, object);
    if !source.is_file() {
        return Err(format!("cache object missing for {label}").into());
    }
    if target.exists() && !force {
        let actual = avc_core::hash_file(target)?;
        if actual.object.hash() != expected_hash {
            return Err(format!("refusing to replace modified file {label}; use --force").into());
        }
    }
    copy_atomic(&source, target)
}

fn push(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    let mut uploaded = 0;
    for pointer in selected_pointers(&repo, paths)? {
        // A directory expands into its manifest plus one object per file; the
        // manifest is uploaded last, so it never names bytes that are not
        // there yet.
        let mut required = required_objects(&repo, &pointer)?;
        required.reverse();
        let mut sent = 0;
        for (object_id, size) in required {
            let source = cache_path(&repo, &object_id);
            if !source.is_file() {
                return Err(format!("cache object missing for {}", pointer.path).into());
            }
            // Objects are immutable, so re-uploading identical bytes is pure
            // cost. Asking first turns a repeated push into a cheap no-op.
            if store.exists(&object_id)? {
                continue;
            }
            let mut file = File::open(&source).map_err(io_error)?;
            store.put(&object_id, size, &mut file)?;
            sent += 1;
        }
        uploaded += sent;
        if sent == 0 {
            println!("up to date {}", display_path(&pointer));
        } else if pointer.is_directory() {
            println!("pushed {}/ ({sent} object(s))", pointer.path);
        } else {
            println!("pushed {} ({})", pointer.path, pointer.object_id()?);
        }
    }
    println!("pushed {uploaded} object(s) to {}", store.describe());
    Ok(())
}

fn pull(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    for pointer in selected_pointers(&repo, paths)? {
        // The manifest has to land first: until it is here and verified, the
        // rest of a directory's objects are unknown.
        let object_id = pointer.object_id()?;
        let fetched = fetch_object(
            &repo,
            store.as_ref(),
            &object_id,
            pointer.object.size,
            &pointer.path,
        )?;
        if pointer.is_directory() {
            let tree = load_tree(&repo, &pointer)?;
            let mut files = 0;
            for entry in &tree.entries {
                let label = format!("{}/{}", pointer.path, entry.path);
                if fetch_object(
                    &repo,
                    store.as_ref(),
                    &entry.object_id()?,
                    entry.size,
                    &label,
                )? {
                    files += 1;
                }
            }
            let total = files + usize::from(fetched);
            if total > 0 {
                println!("pulled {}/ ({total} object(s))", pointer.path);
            }
        } else if fetched {
            println!("pulled {}", pointer.path);
        }
    }
    checkout(paths, false)
}

/// Ensure one object is in the cache, downloading it if it is not.
///
/// Reports whether a transfer actually happened, so a pull that had nothing to
/// do says nothing rather than claiming work.
fn fetch_object(
    repo: &Repo,
    store: &dyn avc_core::ObjectStore,
    object: &avc_core::ObjectId,
    size: u64,
    label: &str,
) -> Result<bool, Failure> {
    let destination = cache_path(repo, object);
    if destination.is_file() {
        return Ok(false);
    }
    let mut body = store.get(object)?;
    download_verified(&mut body, &destination, object, size, label)?;
    Ok(true)
}

/// Stream a download into the cache, verifying size and digest before the
/// object becomes visible.
///
/// The hash is computed while the bytes are written, so a 40 GB artifact is
/// never read twice and a truncated or corrupted transfer never lands in the
/// cache under a name that claims it is intact. `label` names the artifact the
/// object belongs to, which for a file inside a tracked directory is not the
/// pointer's own path.
fn download_verified(
    body: &mut dyn std::io::Read,
    destination: &Path,
    expected: &avc_core::ObjectId,
    size: u64,
    label: &str,
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
    if actual.size != size || actual.object != *expected {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "remote object for {label} does not match its pointer: expected {size} bytes of {}, got {} bytes of {}",
            expected.hash(), actual.size, actual.object.hash()
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
    // Reachability now spans manifests, so a pointer that cannot be read or
    // expanded aborts the run: guessing would delete objects a directory still
    // needs, and that is not recoverable from the cache.
    let mut reachable: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&path)?;
        let required = required_objects(&repo, &pointer).map_err(|error| {
            format!("{error}; refusing to delete objects that may still be referenced")
        })?;
        for (object, _) in required {
            reachable.insert(object.hash().to_owned());
        }
    }
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
        verify_cached(
            &repo,
            &pointer.object_id()?,
            pointer.object.size,
            &pointer.path,
        )?;
        if pointer.is_directory() && cache_path(&repo, &pointer.object_id()?).exists() {
            // `load_tree` re-verifies and parses the manifest, so a manifest
            // that is intact but unreadable is caught here too.
            for entry in load_tree(&repo, &pointer)?.entries {
                let label = format!("{}/{}", pointer.path, entry.path);
                verify_cached(&repo, &entry.object_id()?, entry.size, &label)?;
            }
        }
    }
    println!("doctor: repository, pointers, and available cache objects are valid");
    Ok(())
}

/// Re-hash one cache object, if it is present, against what a pointer or
/// manifest entry claims it is. Absent objects are not an error; `status`
/// reports those.
fn verify_cached(
    repo: &Repo,
    object: &avc_core::ObjectId,
    size: u64,
    label: &str,
) -> Result<(), Failure> {
    let path = cache_path(repo, object);
    if !path.exists() {
        return Ok(());
    }
    let actual = avc_core::hash_file(&path)?;
    if actual.size != size || actual.object != *object {
        return Err(format!("corrupt cache object for {label}").into());
    }
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
fn append_ignore_path(root: &Path, entry: &str) -> Result<(), Failure> {
    let path = root.join(".gitignore");
    let old = fs::read_to_string(&path).unwrap_or_default();
    if old.lines().any(|line| line == entry) {
        return Ok(());
    }
    let suffix = if old.ends_with('\n') || old.is_empty() {
        ""
    } else {
        "\n"
    };
    write_atomic(&path, format!("{old}{suffix}{entry}\n").as_bytes())
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
    // Normalizing first is what lets `avc push data/` reach the pointer whose
    // path is `data`, the way a shell completes a directory name.
    let wanted = paths
        .iter()
        .map(|value| avc_core::normalize_repo_path(Path::new(value)).map_err(Failure::from))
        .collect::<Result<Vec<String>, Failure>>()?;
    let mut selected = Vec::new();
    for pointer_path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&pointer_path)?;
        if wanted.is_empty() || wanted.iter().any(|value| value == &pointer.path) {
            selected.push(pointer);
        }
    }
    // A typo in a path should not be reported as "nothing to do".
    for value in &wanted {
        if !selected.iter().any(|pointer| &pointer.path == value) {
            return Err(format!("no pointer exists for {value}").into());
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
