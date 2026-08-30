mod ci;
mod git;
mod progress;
mod registry;
mod ui;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};

use progress::Progress;
use ui::{Cell, Column, Style, Table};

/// Where the commands built for a pipeline are documented, mentioned in
/// `--help` because that is where somebody wiring up CI will look first.
const CI_HELP: &str = "\
Commands for CI/CD, which take a repository URL and a path inside it:
  avc fetch  --repo <git-url> models/bert -o .   download just that path
  avc list   --repo <git-url> models/            see what is stored there
  avc verify --repo <git-url> models/bert -o .   check it against the pointers

The object store is read from the repository's own .avc/config.toml, so a
consumer never names a bucket. See docs/ci-cd.md.";

#[derive(Debug, Parser)]
#[command(
    name = "avc",
    version,
    about = "Artifact Version Control",
    after_help = CI_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// When to colorize output.
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        env = "AVC_COLOR",
        default_value = "auto"
    )]
    color: ui::ColorChoice,

    /// When to draw a progress bar for transfers. `auto` draws one at a
    /// terminal and reports periodic lines in a CI pipeline instead.
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        env = "AVC_PROGRESS",
        default_value = "auto"
    )]
    progress: progress::ProgressChoice,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize AVC in the current Git worktree.
    Init,
    /// Configure the object stores this repository pushes to and pulls from.
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Start tracking a file or directory as an artifact.
    Add(Paths),
    /// List what a repository stores, at a path or in full.
    List(ListArgs),
    /// Report working-tree and cache state for every tracked artifact.
    Status(StatusArgs),
    /// Record a new version of an already-tracked artifact.
    Commit(CommitArgs),
    /// Upload cached objects to a remote.
    Push(SyncArgs),
    /// Download objects from a remote into the cache, then materialize them.
    Pull(SyncArgs),
    /// Materialize artifacts from the local cache, without touching the network.
    Checkout(CheckoutArgs),
    /// Stop tracking an artifact, keeping the file and its cached bytes.
    Remove(Paths),
    /// Delete cache objects no pointer references.
    Gc(GcArgs),
    /// Verify repository integrity: pointers, manifests, and cached objects.
    Doctor,

    /// [CI/CD] Download one path out of a repository: no clone, no cache.
    ///
    /// Reads the pointers at a Git reference, downloads exactly the objects the
    /// selected paths name, verifies each as it streams, and writes it where
    /// the pointer says. The object store comes from the repository's own
    /// `.avc/config.toml`, so only a repository URL, a path, and credentials
    /// are needed - never a bucket. See docs/ci-cd.md.
    Fetch(ci::FetchArgs),

    /// [CI/CD] Check artifacts on disk against their pointers.
    ///
    /// Re-hashes what is on disk and compares it with what the pointers claim,
    /// using nothing but the two - no object store, no credentials. Exits 1 if
    /// any artifact is missing or differs, which makes it a gate a pipeline can
    /// fail on. See docs/ci-cd.md.
    Verify(ci::VerifyArgs),
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Register a remote by URL, replacing any remote of the same name.
    Add(RemoteAddArgs),
    /// Show the configured remotes, marking the default.
    List,
}

#[derive(Debug, Args)]
struct RemoteAddArgs {
    name: String,
    /// Object store URL. The path after the bucket, if any, becomes the key
    /// prefix: `s3://my-bucket/artifacts/v1`.
    provider_url: String,
    /// SigV4 signing region for S3, recorded in `.avc/config.toml`.
    ///
    /// `AWS_REGION` and `.avc/config.local.toml` still win over it, so a
    /// repository can name its bucket's region without pinning anyone's
    /// machine to it.
    #[arg(long, value_name = "REGION")]
    region: Option<String>,
    /// Named profile to read from `~/.aws/config` and `~/.aws/credentials`.
    ///
    /// A name only — never a key. `AWS_PROFILE` and `.avc/config.local.toml`
    /// still win over it.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
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
    /// Paths inside the repository. A path naming a tracked directory lists the
    /// files inside it; a prefix lists the artifacts beneath it.
    #[arg(value_name = "PATH")]
    paths: Vec<String>,
    /// Git URL of the repository to list. Needs no clone and no local checkout.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    repo: Option<String>,
    /// Revision to read pointers at: a branch, a tag, a commit, or a fully
    /// qualified `refs/...` name. Defaults to the repository's default branch,
    /// or, in a checkout, to the pointers on disk.
    #[arg(long = "ref", value_name = "REV", env = "AVC_REF")]
    reference: Option<String>,
    #[arg(long)]
    remote: Option<String>,
    /// Object store URL, overriding the one the repository configures.
    #[arg(long, value_name = "URL")]
    remote_url: Option<String>,
    /// Stable tab-separated output for scripts: PATH, SIZE, OBJECT, REMOTE.
    #[arg(long)]
    porcelain: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Stable tab-separated output for scripts: STATE, CACHE, PATH.
    #[arg(long)]
    porcelain: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct Config {
    #[serde(default)]
    default_remote: Option<String>,
    #[serde(default)]
    pub(crate) remotes: Vec<Remote>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Remote {
    name: String,
    provider: avc_core::Provider,
    bucket_or_container: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    endpoint_url: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    profile: Option<String>,
}

/// Contents of the gitignored `.avc/config.local.toml`.
#[derive(Debug, Deserialize, Default)]
struct LocalConfig {
    #[serde(default)]
    remotes: Vec<avc_core::LocalRemoteOverride>,
}

pub(crate) struct Repo {
    pub(crate) root: PathBuf,
    pub(crate) config: Config,
}

impl Repo {
    /// Read the repository rooted at `root`.
    ///
    /// A missing `.avc/config.toml` is not an error here, only an empty
    /// configuration: `avc verify` needs no object store at all, and a caller
    /// that does need one reports its absence with more context than this
    /// function has. A malformed one is still an error — silently treating it
    /// as empty would send a transfer to the wrong place, or nowhere.
    pub(crate) fn at(root: PathBuf) -> Result<Self, Failure> {
        let path = root.join(".avc/config.toml");
        let config = match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => {
                toml::from_str(&text).map_err(|error| format!(".avc/config.toml: {error}"))?
            }
            _ => Config::default(),
        };
        Ok(Self { root, config })
    }
}

/// Exit code for expected user, data, or state errors. See `SPEC.md`.
const EXIT_USER_ERROR: i32 = 1;
/// Exit code for provider or operational failures. See `SPEC.md`.
const EXIT_PROVIDER_ERROR: i32 = 3;

/// A failure, carrying the exit code it should produce.
#[derive(Debug)]
pub(crate) struct Failure {
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

impl Failure {
    /// A provider or operational failure: unreachable, unauthorized, or a tool
    /// that is not installed. `SPEC.md` reserves exit code 3 for these, and a
    /// pipeline may reasonably retry one.
    pub(crate) fn provider(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: EXIT_PROVIDER_ERROR,
        }
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
    let cli = Cli::parse();
    ui::init(cli.color);
    progress::init(cli.progress);
    if let Err(failure) = run(cli.command) {
        eprintln!(
            "{} {}",
            ui::paint_err("avc:", Style::Error),
            failure.message
        );
        std::process::exit(failure.code);
    }
}

fn run(command: Command) -> Result<(), Failure> {
    match command {
        Command::Init => init(),
        Command::Remote { command } => remote(command),
        Command::Add(args) => add(&args.paths),
        Command::List(args) => list(&args),
        Command::Status(args) => status(args.porcelain),
        Command::Commit(args) => commit(&args.paths.paths, args.force),
        Command::Push(args) => push(&args.paths, args.remote.as_deref()),
        Command::Pull(args) => pull(&args.paths, args.remote.as_deref()),
        Command::Checkout(args) => checkout(&args.paths, args.force),
        Command::Remove(args) => remove(&args.paths),
        Command::Gc(args) => gc(args.remote.as_deref(), args.dry_run),
        Command::Doctor => doctor(),
        Command::Fetch(args) => ci::fetch(&args),
        Command::Verify(args) => ci::verify(&args),
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
    ui::heading(&format!("initialized AVC in {}", root.display()));
    ui::field("config", ".avc/config.toml");
    ui::field("cache", ".avc/cache");
    ui::field("ignored", ".avc/cache/, .avc/config.local.toml");
    ui::summary("next: avc remote add origin <url>, then avc add <path>");
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
                // An empty flag value would be recorded as a pinned choice and
                // then resolve to nothing, so treat it as "not given".
                region: args.region.filter(|value| !value.trim().is_empty()),
                profile: args.profile.filter(|value| !value.trim().is_empty()),
            });
            let default = repo.config.default_remote.is_none();
            if default {
                repo.config.default_remote = Some(args.name.clone());
            }
            save_config(&repo)?;
            let added = repo
                .config
                .remotes
                .last()
                .expect("the remote just pushed is present");
            ui::heading(&format!("configured remote {}", args.name));
            ui::field("provider", provider_name(&added.provider));
            ui::field("location", &remote_location(added));
            if let Some(endpoint) = &added.endpoint_url {
                ui::field("endpoint", endpoint);
            }
            if let Some(region) = &added.region {
                ui::field("region", region);
            }
            if let Some(profile) = &added.profile {
                ui::field("profile", profile);
            }
            ui::field("default", if default { "yes" } else { "no" });
        }
        RemoteCommand::List => {
            if repo.config.remotes.is_empty() {
                ui::line("no remotes configured", Style::Warn);
                ui::note("add one with `avc remote add origin <url>`");
                return Ok(());
            }
            // Region and profile are shown only when something sets one, so
            // the common case stays a three-column table.
            let show_region = repo
                .config
                .remotes
                .iter()
                .any(|remote| remote.region.is_some());
            let show_profile = repo
                .config
                .remotes
                .iter()
                .any(|remote| remote.profile.is_some());
            let mut columns = vec![
                Column::left(""),
                Column::left("NAME"),
                Column::left("PROVIDER"),
                Column::left("LOCATION"),
            ];
            if show_region {
                columns.push(Column::left("REGION"));
            }
            if show_profile {
                columns.push(Column::left("PROFILE"));
            }
            let mut table = Table::new(columns);
            for remote in &repo.config.remotes {
                let default = repo.config.default_remote.as_deref() == Some(&remote.name);
                let mut cells = vec![
                    Cell::new(if default { "*" } else { " " }, Style::Ok),
                    Cell::plain(remote.name.clone()),
                    Cell::plain(provider_name(&remote.provider)),
                    Cell::plain(remote_location(remote)),
                ];
                if show_region {
                    cells.push(optional_cell(remote.region.as_deref()));
                }
                if show_profile {
                    cells.push(optional_cell(remote.profile.as_deref()));
                }
                table.row(cells);
            }
            table.print();
            ui::summary("* marks the remote used when --remote is omitted");
        }
    }
    Ok(())
}

/// A provider as it is spelled in a URL scheme, which is how a user named it.
fn provider_name(provider: &avc_core::Provider) -> &'static str {
    match provider {
        avc_core::Provider::File => "file",
        avc_core::Provider::S3 => "s3",
        avc_core::Provider::Gcs => "gcs",
        avc_core::Provider::Azure => "azure",
    }
}

/// A configured value, or a dash where the resolver falls back to the
/// environment and `~/.aws`.
fn optional_cell(value: Option<&str>) -> Cell {
    match value {
        Some(value) => Cell::plain(value.to_owned()),
        None => Cell::dim("-"),
    }
}

/// Where a remote points, as `bucket/prefix` — never including credentials.
fn remote_location(remote: &Remote) -> String {
    if remote.prefix.is_empty() {
        remote.bucket_or_container.clone()
    } else {
        format!("{}/{}", remote.bucket_or_container, remote.prefix)
    }
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
    let (pointer_value, detail) = if source.is_dir() {
        track_directory(repo, &relative)?
    } else if source.is_file() {
        track_file(repo, &relative)?
    } else {
        return Err(format!("artifact is not a regular file or directory: {value}").into());
    };
    let display = ignore_line(&relative, pointer_value.is_directory());
    append_ignore_path(&repo.root, &display)?;
    write_atomic(&pointer, pointer_value.serialize_canonical()?.as_bytes())?;
    ui::action("tracked", Style::Ok, &display, Some(&detail));
    Ok(())
}

/// Hash a file, store its bytes, and describe it.
fn track_file(repo: &Repo, relative: &str) -> Result<(avc_core::Pointer, String), Failure> {
    let source = repo.root.join(relative);
    let hash = avc_core::hash_file(&source)?;
    store_in_cache(repo, &source, &hash.object)?;
    let pointer = avc_core::Pointer::new(relative, hash.object.clone(), hash.size, None)?;
    let detail = format!(
        "{}, {}",
        ui::size(hash.size),
        ui::short_hash(hash.object.hash())
    );
    Ok((pointer, detail))
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
    let detail = format!(
        "{}, {}, {}",
        ui::plural(tree.entries.len(), "file"),
        ui::size(tree.total_size()),
        ui::short_hash(manifest.object.hash())
    );
    Ok((pointer, detail))
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

/// How an artifact on disk compares with the pointer that describes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum State {
    Ok,
    Modified,
    Missing,
}

impl State {
    /// The word printed for this state, in human and porcelain output alike.
    pub(crate) fn label(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Modified => "modified",
            State::Missing => "missing",
        }
    }

    pub(crate) fn style(self) -> Style {
        match self {
            State::Ok => Style::Ok,
            State::Modified => Style::Warn,
            State::Missing => Style::Bad,
        }
    }
}

fn status(porcelain: bool) -> Result<(), Failure> {
    let repo = load_repo()?;
    let mut table = Table::new(vec![
        Column::left("STATUS"),
        Column::left("CACHE"),
        Column::right("SIZE"),
        Column::left("ARTIFACT"),
    ]);
    let mut counts = [0_usize; 3];
    let mut invalid = Vec::new();

    for pointer_path in pointer_files(&repo.root)? {
        let pointer = match parse_pointer(&pointer_path) {
            Ok(value) => value,
            // One unreadable pointer should not hide the state of every other
            // artifact, so it is collected and reported after the table.
            Err(error) => {
                invalid.push(format!("{}: {error}", pointer_path.display()));
                continue;
            }
        };
        let (state, bytes) = artifact_state(&repo.root, &pointer)?;
        counts[state as usize] += 1;
        let cached = cached_completely(&repo, &pointer)?;
        let path = display_path(&pointer);
        if porcelain {
            let cache = if cached { "cached" } else { "cache-missing" };
            println!("{}\t{cache}\t{path}", state.label());
            continue;
        }
        table.row(vec![
            Cell::new(state.label(), state.style()),
            Cell::new(
                if cached { "cached" } else { "cache-missing" },
                if cached { Style::Dim } else { Style::Warn },
            ),
            Cell::plain(if state == State::Missing {
                "-".to_owned()
            } else {
                ui::size(bytes)
            }),
            Cell::plain(path),
        ]);
    }

    if porcelain {
        for message in &invalid {
            println!("invalid\t-\t{message}");
        }
        return Ok(());
    }

    let total: usize = counts.iter().sum();
    if total == 0 && invalid.is_empty() {
        ui::line("no AVC pointers found", Style::Warn);
        ui::note("track something with `avc add <path>`");
        return Ok(());
    }
    table.print();
    if total > 0 {
        ui::summary(&format!(
            "{}: {} ok, {} modified, {} missing",
            ui::plural(total, "artifact"),
            counts[State::Ok as usize],
            counts[State::Modified as usize],
            counts[State::Missing as usize]
        ));
    }
    if !invalid.is_empty() {
        println!();
        ui::line("invalid pointers:", Style::Bad);
        for message in &invalid {
            println!("  {message}");
        }
    }
    Ok(())
}

/// Compare an artifact beneath `root` against its pointer, reporting how it
/// differs and how many of its bytes are on disk.
///
/// A directory is re-scanned and re-hashed into a manifest: its identity is
/// that manifest's hash, so a file added, removed, renamed, or edited anywhere
/// beneath it reads as `modified` exactly as an edited file does. `root` is a
/// parameter rather than the repository, because `avc verify` runs against a
/// pipeline's output directory with no repository anywhere near it.
pub(crate) fn artifact_state(
    root: &Path,
    pointer: &avc_core::Pointer,
) -> Result<(State, u64), Failure> {
    let artifact = root.join(&pointer.path);
    if pointer.is_directory() {
        if !artifact.is_dir() {
            return Ok((State::Missing, 0));
        }
        let scanned = scan_directory(&artifact)?;
        let tree = avc_core::Tree::new(scanned.into_iter().map(|(_, entry)| entry).collect())?;
        let bytes = tree.serialize_canonical()?.into_bytes();
        let actual = avc_core::hash_reader(&mut bytes.as_slice())?;
        let state = if actual.object.hash() == pointer.object.hash {
            State::Ok
        } else {
            State::Modified
        };
        return Ok((state, tree.total_size()));
    }
    if !artifact.exists() {
        return Ok((State::Missing, 0));
    }
    let actual = avc_core::hash_file(&artifact)?;
    let state = if actual.object != pointer.object_id()? || actual.size != pointer.object.size {
        State::Modified
    } else {
        State::Ok
    };
    Ok((state, actual.size))
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
pub(crate) fn display_path(pointer: &avc_core::Pointer) -> String {
    ignore_line(&pointer.path, pointer.is_directory())
}

/// Show what a repository tracks, and whether the remote can supply it.
///
/// With no path this lists every artifact. With a path it lists what is stored
/// *at* that path, which is the difference between browsing a registry and
/// dumping it: a prefix shows the artifacts beneath it, and a tracked directory
/// shows the files inside it.
fn list(args: &ListArgs) -> Result<(), Failure> {
    let registry = registry::Registry::open(args.repo.as_deref(), args.reference.as_deref())?;
    let selected = registry.select(&args.paths)?;
    if selected.is_empty() {
        if !args.porcelain {
            ui::line("no AVC pointers found", Style::Warn);
            ui::note("track something with `avc add <path>`");
        }
        return Ok(());
    }
    let store = registry.store(args.remote_url.as_deref(), args.remote.as_deref())?;
    if !args.porcelain {
        ui::heading(&format!(
            "{} in {}",
            if args.paths.is_empty() {
                "everything".to_owned()
            } else {
                args.paths.join(", ")
            },
            registry.describe()
        ));
        ui::field("objects", &store.describe());
        println!();
    }
    // One listing answers every pointer, so a repository with a thousand
    // artifacts costs one round trip rather than a thousand HEAD requests.
    let present: std::collections::HashSet<String> = store
        .list()?
        .into_iter()
        .map(|found| found.object.hash().to_owned())
        .collect();

    let mut table = Table::new(vec![
        Column::left("PATH"),
        Column::right("SIZE"),
        Column::left("OBJECT"),
        Column::left("REMOTE"),
    ]);
    let mut rows = 0;
    let mut available = 0;
    let mut total_bytes = 0;
    let mut listed_files = false;

    for pointer in &selected {
        // A path that names a tracked directory exactly is a request to look
        // inside it, so the rows become its files rather than the one artifact
        // they add up to. Reaching that list needs the manifest, which is
        // metadata; artifact bytes are still never downloaded.
        let inside = pointer.is_directory()
            && args
                .paths
                .iter()
                .map(|value| registry::normalize_selector(value))
                .collect::<Result<Vec<String>, Failure>>()?
                .iter()
                .any(|value| value == &pointer.path);
        if inside {
            let Some(tree) = remote_tree(registry.repo(), store.as_ref(), pointer, &present)?
            else {
                return Err(format!(
                    "the manifest for {} is on neither the remote nor this machine, \
                     so its contents are unknown",
                    pointer.path
                )
                .into());
            };
            listed_files = true;
            for entry in &tree.entries {
                let on_remote = present.contains(&entry.hash);
                rows += 1;
                available += usize::from(on_remote);
                total_bytes += entry.size;
                emit_row(
                    &mut table,
                    args.porcelain,
                    &format!("{}/{}", pointer.path, entry.path),
                    Some(entry.size),
                    &entry.object_id()?,
                    on_remote,
                );
            }
            continue;
        }

        let object = pointer.object_id()?;
        // A directory is available only when its manifest *and* every file it
        // names are on the remote; a half-uploaded directory is not restorable.
        let (size, on_remote) = if pointer.is_directory() {
            match remote_tree(registry.repo(), store.as_ref(), pointer, &present)? {
                Some(tree) => {
                    let complete = present.contains(object.hash())
                        && tree
                            .entries
                            .iter()
                            .all(|entry| present.contains(&entry.hash));
                    (Some(tree.total_size()), complete)
                }
                // Without the manifest the file list is unknowable, and the
                // remote demonstrably cannot restore the directory.
                None => (None, false),
            }
        } else {
            (Some(pointer.object.size), present.contains(object.hash()))
        };
        rows += 1;
        available += usize::from(on_remote);
        total_bytes += size.unwrap_or(0);
        emit_row(
            &mut table,
            args.porcelain,
            &display_path(pointer),
            size,
            &object,
            on_remote,
        );
    }

    if !args.porcelain {
        table.print();
        ui::summary(&format!(
            "{}, {}: {available} available, {} missing",
            ui::plural(rows, if listed_files { "file" } else { "artifact" }),
            ui::size(total_bytes),
            rows - available
        ));
    }
    Ok(())
}

/// One `list` row, in whichever form was asked for.
fn emit_row(
    table: &mut Table,
    porcelain: bool,
    path: &str,
    size: Option<u64>,
    object: &avc_core::ObjectId,
    on_remote: bool,
) {
    let state = if on_remote { "available" } else { "missing" };
    if porcelain {
        println!(
            "{path}\t{}\t{object}\t{state}",
            size.map_or("-".to_owned(), |bytes| bytes.to_string())
        );
        return;
    }
    table.row(vec![
        Cell::plain(path.to_owned()),
        Cell::plain(size.map_or("-".to_owned(), ui::size)),
        Cell::dim(ui::short_hash(object.hash())),
        Cell::new(state, if on_remote { Style::Ok } else { Style::Bad }),
    ]);
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
    let count = checkout_selected(paths, force)?;
    ui::summary(&format!(
        "{} materialized from the cache",
        ui::plural(count, "artifact")
    ));
    Ok(())
}

/// Materialize the selected artifacts, reporting each as it lands, and answer
/// how many there were.
///
/// Split out so `pull` can materialize without printing a second summary of
/// its own underneath the transfer it just reported.
fn checkout_selected(paths: &[String], force: bool) -> Result<usize, Failure> {
    let repo = load_repo()?;
    let selected = selected_pointers(&repo, paths)?;
    for pointer in &selected {
        if pointer.is_directory() {
            let tree = load_tree(&repo, pointer)?;
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
            ui::action(
                "checked out",
                Style::Ok,
                &display_path(pointer),
                Some(&ui::plural(tree.entries.len(), "file")),
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
        ui::action(
            "checked out",
            Style::Ok,
            &pointer.path,
            Some(&ui::size(pointer.object.size)),
        );
    }
    Ok(selected.len())
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

/// What one artifact still has to send.
struct Upload {
    pointer: avc_core::Pointer,
    /// In upload order: a directory's files first, its manifest last.
    objects: Vec<(avc_core::ObjectId, u64)>,
}

fn push(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    let selected = selected_pointers(&repo, paths)?;
    ui::heading(&format!(
        "pushing {} to {}",
        ui::plural(selected.len(), "artifact"),
        store.describe()
    ));
    println!();

    // Deciding what to send before sending any of it is what gives the progress
    // report a denominator. It costs no extra requests: asking the remote what
    // it already holds is the same question the upload loop used to ask inline,
    // one object at a time.
    let plan = plan_upload(&repo, store.as_ref(), selected)?;
    let objects: usize = plan.iter().map(|upload| upload.objects.len()).sum();
    let planned_bytes = plan
        .iter()
        .flat_map(|upload| &upload.objects)
        .map(|(_, size)| size)
        .sum();
    let progress = Progress::start("uploading", objects, planned_bytes);

    let mut uploaded = 0;
    let mut bytes = 0;
    for upload in plan {
        let path = display_path(&upload.pointer);
        let mut sent = 0;
        let mut sent_bytes = 0;
        for (object_id, size) in &upload.objects {
            progress.item(&path);
            let source = cache_path(&repo, object_id);
            let mut file = File::open(&source).map_err(io_error)?;
            store.put(object_id, *size, &mut progress.meter(&mut file))?;
            progress.object_done();
            sent += 1;
            sent_bytes += size;
        }
        uploaded += sent;
        bytes += sent_bytes;
        // The bar sits on the line this one is about to occupy.
        progress.clear();
        if sent == 0 {
            ui::action("up-to-date", Style::Dim, &path, None);
        } else {
            ui::action(
                "uploaded",
                Style::Ok,
                &path,
                Some(&format!(
                    "{}, {}",
                    ui::plural(sent, "object"),
                    ui::size(sent_bytes)
                )),
            );
        }
    }
    progress.finish();
    ui::summary(&format!(
        "pushed {} ({}) to {}",
        ui::plural(uploaded, "object"),
        ui::size(bytes),
        store.describe()
    ));
    Ok(())
}

/// Work out which objects actually have to be uploaded.
///
/// Two things are dropped here. Objects the remote already holds: they are
/// immutable and content-addressed, so re-uploading identical bytes is pure
/// cost, and asking first turns a repeated push into a cheap no-op. And objects
/// an earlier artifact in this same run is already sending, which is what the
/// old inline `exists` check achieved by accident — the second artifact asked
/// after the first had uploaded — and what has to be explicit now that every
/// question is asked before any answer changes.
fn plan_upload(
    repo: &Repo,
    store: &dyn avc_core::ObjectStore,
    selected: Vec<avc_core::Pointer>,
) -> Result<Vec<Upload>, Failure> {
    let _status = progress::Status::show("checking the remote for objects it already has");
    let mut plan = Vec::with_capacity(selected.len());
    let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pointer in selected {
        // A directory expands into its manifest plus one object per file; the
        // manifest is uploaded last, so it never names bytes that are not
        // there yet.
        let mut required = required_objects(repo, &pointer)?;
        required.reverse();
        let mut objects = Vec::new();
        for (object_id, size) in required {
            if !cache_path(repo, &object_id).is_file() {
                return Err(format!("cache object missing for {}", pointer.path).into());
            }
            if !claimed.insert(object_id.hash().to_owned()) {
                continue;
            }
            if store.exists(&object_id)? {
                continue;
            }
            objects.push((object_id, size));
        }
        plan.push(Upload { pointer, objects });
    }
    Ok(plan)
}

/// One object a pull expects to download, and the artifact path it belongs to.
type Wanted = (avc_core::ObjectId, u64, String);

/// What one artifact still has to receive.
struct Download {
    pointer: avc_core::Pointer,
    /// In download order: a directory's manifest first, since nothing else
    /// about it is known until that has landed and been verified.
    objects: Vec<Wanted>,
    /// Whether the files this directory names are still uncounted, because its
    /// manifest was not in the cache when the plan was drawn up.
    expands: bool,
}

fn pull(paths: &[String], remote_name: Option<&str>) -> Result<(), Failure> {
    let repo = load_repo()?;
    let store = open_store(&repo, remote_name)?;
    let selected = selected_pointers(&repo, paths)?;
    ui::heading(&format!(
        "pulling {} from {}",
        ui::plural(selected.len(), "artifact"),
        store.describe()
    ));
    println!();

    // Planned from the cache alone, so it costs nothing and needs no network.
    let plan = plan_download(&repo, selected)?;
    let objects: usize = plan.iter().map(|download| download.objects.len()).sum();
    let planned_bytes = plan
        .iter()
        .flat_map(|download| &download.objects)
        .map(|(_, size, _)| size)
        .sum();
    let progress = Progress::start("downloading", objects, planned_bytes);

    let mut downloaded = 0;
    let mut bytes = 0;
    for download in plan {
        let mut received = 0;
        let mut received_bytes = 0;
        for (object_id, size, label) in &download.objects {
            progress.item(label);
            if fetch_object(&repo, store.as_ref(), object_id, *size, label, &progress)? {
                received += 1;
                received_bytes += size;
            }
        }
        // The manifest has arrived, so the files it names can be counted and
        // added to a total that was drawn up without them.
        if download.expands {
            let wanted = wanted_entries(&repo, &download.pointer)?;
            progress.add(
                wanted.len(),
                wanted.iter().map(|(_, size, _)| size).sum::<u64>(),
            );
            for (object_id, size, label) in &wanted {
                progress.item(label);
                if fetch_object(&repo, store.as_ref(), object_id, *size, label, &progress)? {
                    received += 1;
                    received_bytes += size;
                }
            }
        }
        downloaded += received;
        bytes += received_bytes;
        progress.clear();
        if received == 0 {
            ui::action(
                "up-to-date",
                Style::Dim,
                &display_path(&download.pointer),
                None,
            );
        } else {
            ui::action(
                "downloaded",
                Style::Ok,
                &display_path(&download.pointer),
                Some(&format!(
                    "{}, {}",
                    ui::plural(received, "object"),
                    ui::size(received_bytes)
                )),
            );
        }
    }
    progress.finish();
    println!();
    checkout_selected(paths, false)?;
    ui::summary(&format!(
        "pulled {} ({}) from {}",
        ui::plural(downloaded, "object"),
        ui::size(bytes),
        store.describe()
    ));
    Ok(())
}

/// Work out which objects are missing from the cache, reading nothing but the
/// cache itself.
///
/// A directory is only expanded when its manifest is already here. When it is
/// not, the manifest is the one thing that can be planned for, and the files it
/// names are counted the moment it arrives — which is honest about a total that
/// genuinely is not knowable yet, and is the only case where the total grows
/// mid-run.
fn plan_download(repo: &Repo, selected: Vec<avc_core::Pointer>) -> Result<Vec<Download>, Failure> {
    let mut plan = Vec::with_capacity(selected.len());
    for pointer in selected {
        // A file's own object, or a directory's manifest.
        let object_id = pointer.object_id()?;
        let cached = cache_path(repo, &object_id).is_file();
        let mut objects = Vec::new();
        if !cached {
            objects.push((object_id, pointer.object.size, pointer.path.clone()));
        }
        // A directory can be read only once its manifest is on disk. Until then
        // the files it names cannot be planned for at all.
        let expands = pointer.is_directory() && !cached;
        if pointer.is_directory() && !expands {
            objects.extend(wanted_entries(repo, &pointer)?);
        }
        plan.push(Download {
            pointer,
            objects,
            expands,
        });
    }
    Ok(plan)
}

/// The files a tracked directory names that are not in the cache yet.
fn wanted_entries(repo: &Repo, pointer: &avc_core::Pointer) -> Result<Vec<Wanted>, Failure> {
    let mut wanted = Vec::new();
    for entry in load_tree(repo, pointer)?.entries {
        let object_id = entry.object_id()?;
        if cache_path(repo, &object_id).is_file() {
            continue;
        }
        wanted.push((
            object_id,
            entry.size,
            format!("{}/{}", pointer.path, entry.path),
        ));
    }
    Ok(wanted)
}

/// Ensure one object is in the cache, downloading it if it is not.
///
/// Reports whether a transfer actually happened, so a pull that had nothing to
/// do says nothing rather than claiming work. A planned object can still turn
/// out to be here — a directory holding the same bytes at two paths names one
/// object twice — and that counts as progress even though nothing moved,
/// because the bar measures a plan being worked through, not bytes for their
/// own sake.
fn fetch_object(
    repo: &Repo,
    store: &dyn avc_core::ObjectStore,
    object: &avc_core::ObjectId,
    size: u64,
    label: &str,
    progress: &Progress,
) -> Result<bool, Failure> {
    let destination = cache_path(repo, object);
    if destination.is_file() {
        progress.done(size);
        return Ok(false);
    }
    let mut body = store.get(object)?;
    download_verified(
        &mut progress.meter(&mut *body),
        &destination,
        object,
        size,
        label,
    )?;
    progress.object_done();
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
pub(crate) fn download_verified(
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
        ui::action("untracked", Style::Warn, &relative, None);
    }
    ui::note("the artifact and its cached bytes are kept; reclaim them with `avc gc`");
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
    let mut removed = 0;
    let mut bytes = 0;
    for (hash, path) in cache_objects(&repo)? {
        if reachable.contains(&hash) {
            continue;
        }
        bytes += fs::metadata(&path).map(|data| data.len()).unwrap_or(0);
        removed += 1;
        if dry_run {
            ui::action("removable", Style::Warn, &ui::short_hash(&hash), None);
        } else {
            fs::remove_file(&path).map_err(io_error)?;
            ui::action("removed", Style::Warn, &ui::short_hash(&hash), None);
        }
    }
    if removed == 0 {
        ui::line(
            "nothing to reclaim: every cache object is still referenced",
            Style::Ok,
        );
        return Ok(());
    }
    ui::summary(&format!(
        "{} {} ({})",
        if dry_run { "reclaimable:" } else { "reclaimed" },
        ui::plural(removed, "object"),
        ui::size(bytes)
    ));
    if dry_run {
        ui::note("re-run without --dry-run to delete them");
    }
    Ok(())
}

fn doctor() -> Result<(), Failure> {
    let repo = load_repo()?;
    if !repo.root.join(".git").exists() {
        return Err("Git worktree not found".into());
    }
    let mut pointers = 0;
    let mut objects = 0;
    for path in pointer_files(&repo.root)? {
        let pointer = parse_pointer(&path)?;
        pointers += 1;
        objects += usize::from(verify_cached(
            &repo,
            &pointer.object_id()?,
            pointer.object.size,
            &pointer.path,
        )?);
        if pointer.is_directory() && cache_path(&repo, &pointer.object_id()?).exists() {
            // `load_tree` re-verifies and parses the manifest, so a manifest
            // that is intact but unreadable is caught here too.
            for entry in load_tree(&repo, &pointer)?.entries {
                let label = format!("{}/{}", pointer.path, entry.path);
                objects += usize::from(verify_cached(
                    &repo,
                    &entry.object_id()?,
                    entry.size,
                    &label,
                )?);
            }
        }
    }
    ui::line(
        "doctor: repository, pointers, and available cache objects are valid",
        Style::Ok,
    );
    ui::summary(&format!(
        "re-hashed {} named by {}",
        ui::plural(objects, "cache object"),
        ui::plural(pointers, "pointer")
    ));
    Ok(())
}

/// Re-hash one cache object, if it is present, against what a pointer or
/// manifest entry claims it is, and report whether there was one to check.
/// Absent objects are not an error; `status` reports those.
fn verify_cached(
    repo: &Repo,
    object: &avc_core::ObjectId,
    size: u64,
    label: &str,
) -> Result<bool, Failure> {
    let path = cache_path(repo, object);
    if !path.exists() {
        return Ok(false);
    }
    let actual = avc_core::hash_file(&path)?;
    if actual.size != size || actual.object != *object {
        return Err(format!("corrupt cache object for {label}").into());
    }
    Ok(true)
}

pub(crate) fn find_root() -> Result<PathBuf, Failure> {
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
pub(crate) fn load_repo() -> Result<Repo, Failure> {
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
/// Every pointer in the worktree, in a stable order.
///
/// Directory iteration order is whatever the filesystem feels like, which
/// would make two runs of `avc status` on the same repository — or the same
/// pipeline on two runners — print the same artifacts in different orders.
pub(crate) fn pointer_files(root: &Path) -> Result<Vec<PathBuf>, Failure> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
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
pub(crate) fn open_store(
    repo: &Repo,
    name: Option<&str>,
) -> Result<Box<dyn avc_core::ObjectStore>, Failure> {
    let remote = choose_remote(repo, name)?;
    let local = load_local_override(repo, &remote.name)?;
    let config = avc_core::RemoteConfig {
        name: remote.name.clone(),
        provider: remote.provider.clone(),
        bucket_or_container: remote.bucket_or_container.clone(),
        prefix: remote.prefix.clone(),
        endpoint_url: remote.endpoint_url.clone(),
        region: remote.region.clone(),
        profile: remote.profile.clone(),
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
/// The artifacts a repository command should act on.
///
/// Selection is the registry's, so one path language serves every command: an
/// exact artifact path, a prefix naming everything beneath it, or nothing at
/// all for the lot. `avc push models/bert` and `avc fetch models/bert` therefore
/// mean the same thing, in a checkout and in a pipeline alike.
fn selected_pointers(repo: &Repo, paths: &[String]) -> Result<Vec<avc_core::Pointer>, Failure> {
    let mut artifacts = Vec::new();
    for pointer_path in pointer_files(&repo.root)? {
        artifacts.push(parse_pointer(&pointer_path)?);
    }
    registry::select(artifacts, paths)
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
pub(crate) fn copy_atomic(source: &Path, destination: &Path) -> Result<(), Failure> {
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
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
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
pub(crate) fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
