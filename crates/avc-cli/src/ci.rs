//! Commands built for a pipeline rather than a workstation.
//!
//! A build agent is not a developer's machine. It has no clone of the artifact
//! history, no `.avc/cache` to warm, often no Git repository at all — a deploy
//! job may hold nothing but a pointer file and a set of credentials — and it is
//! thrown away when the job ends. `avc pull` is the wrong shape for that: it
//! assumes a repository, populates a cache the runner will delete, and writes
//! every artifact twice.
//!
//! [`fetch`] downloads straight from the remote to the path the pointer names,
//! streaming and verifying as it goes, with no repository and no cache in the
//! middle. [`verify`] re-checks a directory against its pointers using nothing
//! but what is already on disk, so a job can assert it built against the exact
//! bytes the commit claims.
//!
//! Both take their remote from a URL or an environment variable, both accept
//! pointer files on the command line or on stdin, and both offer `--porcelain`
//! for a script that has to read the result rather than a human.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::ui::{self, Cell, Column, Style, Table};
use crate::{Failure, State};

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Pointer files, or directories to scan for them. Defaults to the current
    /// directory; `-` reads newline-separated paths from stdin.
    #[arg(value_name = "POINTER")]
    pub paths: Vec<String>,

    /// Remote to download from, as a URL. Needs no repository and no `avc init`.
    #[arg(long, value_name = "URL", env = "AVC_REMOTE_URL")]
    pub remote_url: Option<String>,

    /// Named remote from `.avc/config.toml`, for a job that did clone the repository.
    #[arg(long, value_name = "NAME", conflicts_with = "remote_url")]
    pub remote: Option<String>,

    /// Directory to write artifacts into, at the paths their pointers name.
    #[arg(long, short, value_name = "DIR", default_value = ".")]
    pub output: PathBuf,

    /// Reuse and populate a cache directory, for a runner that caches it between jobs.
    #[arg(long, value_name = "DIR", env = "AVC_CACHE_DIR")]
    pub cache: Option<PathBuf>,

    /// Overwrite files whose contents differ from their pointer.
    #[arg(long)]
    pub force: bool,

    /// Report what would be transferred, without downloading artifact bytes.
    #[arg(long)]
    pub dry_run: bool,

    /// Stable tab-separated output for scripts: STATE, OBJECTS, BYTES, PATH.
    #[arg(long)]
    pub porcelain: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Pointer files, or directories to scan for them. Defaults to the current
    /// directory; `-` reads newline-separated paths from stdin.
    #[arg(value_name = "POINTER")]
    pub paths: Vec<String>,

    /// Directory the artifacts were written into.
    #[arg(long, short, value_name = "DIR", default_value = ".")]
    pub output: PathBuf,

    /// Stable tab-separated output for scripts: STATE, BYTES, PATH.
    #[arg(long)]
    pub porcelain: bool,
}

/// A pointer and the file it was read from, so an error can name the file a
/// pipeline actually has on disk.
struct Located {
    source: PathBuf,
    pointer: avc_core::Pointer,
}

/// What happened to one artifact.
///
/// Only artifact content is counted. Reading a directory's manifest is not a
/// transfer a pipeline should see reported: it is a few bytes of metadata that
/// `fetch` must read to know what else to ask for, exactly as `avc list` reads
/// one to report a size. Counting it would make a directory whose files are all
/// already on disk report itself as downloaded on every run.
#[derive(Default)]
struct Transfer {
    objects: usize,
    bytes: u64,
    /// Objects that were materialized without a transfer, from a configured
    /// cache or from somewhere this run had already written them.
    restored: usize,
    files: usize,
    total_bytes: u64,
}

impl Transfer {
    /// Count one object as moved over the network.
    fn record(&mut self, size: u64) {
        self.objects += 1;
        self.bytes += size;
    }

    /// The word for what a pipeline's log should say happened here.
    ///
    /// A cache hit is called out rather than folded into `up-to-date`, because
    /// the two mean different things to whoever is reading the log: one wrote
    /// files, the other found them already correct.
    fn state(&self, dry_run: bool) -> (&'static str, Style) {
        match (self.objects, self.restored, dry_run) {
            (0, 0, _) => ("up-to-date", Style::Dim),
            (0, _, _) => ("from-cache", Style::Ok),
            (_, _, true) => ("would-fetch", Style::Warn),
            (_, _, false) => ("downloaded", Style::Ok),
        }
    }
}

/// One `avc fetch` run.
///
/// Holds the remote, the options, and what has already been written, so the
/// per-object logic can be read as a sequence of decisions rather than a
/// parameter list.
struct Fetcher<'a> {
    store: Box<dyn avc_core::ObjectStore>,
    args: &'a FetchArgs,
    /// Where each object has already landed during this run.
    ///
    /// A directory holding the same bytes at two paths names one object twice,
    /// and with no cache to fall back on the second entry would download it
    /// again. Copying from the first landing site instead keeps the transfer
    /// count equal to the number of distinct objects, which is what
    /// deduplication has to mean when there is nowhere else to put them.
    placed: std::collections::HashMap<String, PathBuf>,
}

/// Download artifacts straight from a remote into `--output`.
pub fn fetch(args: &FetchArgs) -> Result<(), Failure> {
    let mut fetcher = Fetcher {
        store: open_remote(args.remote_url.as_deref(), args.remote.as_deref())?,
        args,
        placed: std::collections::HashMap::new(),
    };
    let located = collect_pointers(&args.paths)?;
    if located.is_empty() {
        return report_nothing_found(args.porcelain);
    }

    if !args.porcelain {
        ui::heading(&format!(
            "fetching {} from {}",
            ui::plural(located.len(), "artifact"),
            fetcher.store.describe()
        ));
        ui::field("into", &args.output.display().to_string());
        if let Some(cache) = &args.cache {
            ui::field("cache", &cache.display().to_string());
        }
        println!();
    }

    let mut objects = 0;
    let mut bytes = 0;
    for entry in &located {
        let transfer = fetcher.artifact(&entry.pointer)?;
        objects += transfer.objects;
        bytes += transfer.bytes;
        let (state, style) = transfer.state(args.dry_run);
        let path = crate::display_path(&entry.pointer);
        if args.porcelain {
            println!("{state}\t{}\t{}\t{path}", transfer.objects, transfer.bytes);
            continue;
        }
        let detail = if entry.pointer.is_directory() {
            format!(
                "{}, {}",
                ui::plural(transfer.files, "file"),
                ui::size(transfer.total_bytes)
            )
        } else {
            ui::size(transfer.total_bytes)
        };
        ui::action(state, style, &path, Some(&detail));
    }

    if !args.porcelain {
        ui::summary(&format!(
            "{} {} ({}) for {} from {}",
            if args.dry_run {
                "would fetch"
            } else {
                "fetched"
            },
            ui::plural(objects, "object"),
            ui::size(bytes),
            ui::plural(located.len(), "artifact"),
            fetcher.store.describe()
        ));
    }
    Ok(())
}

impl Fetcher<'_> {
    /// Download one artifact: a file, or a directory's manifest and every file
    /// that manifest names.
    fn artifact(&mut self, pointer: &avc_core::Pointer) -> Result<Transfer, Failure> {
        let mut transfer = Transfer::default();
        if !pointer.is_directory() {
            transfer.files = 1;
            transfer.total_bytes = pointer.object.size;
            let target = self.args.output.join(&pointer.path);
            self.place(
                &pointer.object_id()?,
                pointer.object.size,
                &target,
                &pointer.path,
                &mut transfer,
            )?;
            return Ok(transfer);
        }

        // The manifest decides what else to download, so it is read even on a
        // dry run. It is metadata of a few bytes per file, never artifact
        // content, and it is verified against the pointer before it is parsed.
        let tree = self.manifest(pointer)?;
        transfer.files = tree.entries.len();
        transfer.total_bytes = tree.total_size();
        for entry in &tree.entries {
            let label = format!("{}/{}", pointer.path, entry.path);
            let target = self.args.output.join(&pointer.path).join(&entry.path);
            self.place(
                &entry.object_id()?,
                entry.size,
                &target,
                &label,
                &mut transfer,
            )?;
        }
        Ok(transfer)
    }

    /// Read a directory's manifest, from the cache when one is configured and
    /// holds it, otherwise from the remote.
    ///
    /// The bytes are held in memory rather than written into `--output`: a
    /// manifest is AVC's own bookkeeping, and the tree it describes should
    /// contain the artifact's files and nothing else.
    fn manifest(&self, pointer: &avc_core::Pointer) -> Result<avc_core::Tree, Failure> {
        let object = pointer.object_id()?;
        let cached = self
            .args
            .cache
            .as_ref()
            .map(|cache| cache.join(object.cache_key()));

        let bytes = match cached.as_ref().filter(|path| path.is_file()) {
            Some(path) => fs::read(path).map_err(crate::io_error)?,
            None => {
                let mut bytes = Vec::new();
                std::io::copy(&mut self.store.get(&object)?, &mut bytes)
                    .map_err(crate::io_error)?;
                bytes
            }
        };

        // Verified whichever side it came from: a manifest decides where
        // `fetch` writes, so it is untrusted input even off a local disk.
        let actual = avc_core::hash_reader(&mut bytes.as_slice())?;
        if actual.size != pointer.object.size || actual.object != object {
            return Err(format!(
                "directory manifest for {} does not match its pointer",
                pointer.path
            )
            .into());
        }
        // Populating the cache is a write, and a dry run makes none.
        if let Some(path) = cached.filter(|path| !path.is_file()) {
            if !self.args.dry_run {
                crate::write_atomic(&path, &bytes)?;
            }
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("directory manifest for {} is not UTF-8", pointer.path))?;
        Ok(avc_core::Tree::parse(&text)?)
    }

    /// Ensure `target` holds exactly `object`'s bytes, transferring them only
    /// if it does not already.
    fn place(
        &mut self,
        object: &avc_core::ObjectId,
        size: u64,
        target: &Path,
        label: &str,
        transfer: &mut Transfer,
    ) -> Result<(), Failure> {
        if target.exists() {
            let actual = avc_core::hash_file(target)?;
            if actual.size == size && actual.object == *object {
                return Ok(());
            }
            if !self.args.force {
                return Err(format!(
                    "refusing to replace {label}: it differs from its pointer; use --force"
                )
                .into());
            }
        }

        // Deciding before acting is what lets `--dry-run` report the same
        // numbers the real run produces: the accounting below happens either
        // way, and only the writing is skipped.
        let source = self.locate(object, size)?;
        match source {
            Source::Copy(_) => transfer.restored += 1,
            Source::Remote => transfer.record(size),
        }
        self.placed
            .insert(object.hash().to_owned(), target.to_path_buf());
        if self.args.dry_run {
            return Ok(());
        }

        match source {
            Source::Copy(path) => crate::copy_atomic(&path, target),
            // The cacheless path, and the default: bytes go from the remote to
            // where they belong, hashed on the way, written exactly once.
            Source::Remote if self.args.cache.is_none() => {
                let mut body = self.store.get(object)?;
                crate::download_verified(&mut body, target, object, size, label)
            }
            // With a cache, the object lands there first so the next job can
            // skip the network, and the worktree copy comes off local disk.
            Source::Remote => {
                let cached = self.cache_path(object).expect("a cache is configured");
                let mut body = self.store.get(object)?;
                crate::download_verified(&mut body, &cached, object, size, label)?;
                crate::copy_atomic(&cached, target)
            }
        }
    }

    /// Where `object`'s bytes would come from.
    ///
    /// The order is what makes a re-run cheap: an object already written
    /// somewhere during this run is copied rather than fetched twice — which is
    /// how a directory holding identical bytes at two paths transfers them
    /// once with no cache at all — and a configured cache is consulted before
    /// the network.
    fn locate(&self, object: &avc_core::ObjectId, size: u64) -> Result<Source, Failure> {
        if let Some(path) = self.placed.get(object.hash()) {
            return Ok(Source::Copy(path.clone()));
        }
        let Some(cached) = self.cache_path(object) else {
            return Ok(Source::Remote);
        };
        if !cached.is_file() {
            return Ok(Source::Remote);
        }
        // A cache is a saving only if reading it beats the network, and safe
        // only if it is verified; re-hashing locally is both.
        let actual = avc_core::hash_file(&cached)?;
        if actual.size == size && actual.object == *object {
            return Ok(Source::Copy(cached));
        }
        // An entry that fails verification is treated as absent, and dropped
        // so the next run does not pay to read it again. A dry run leaves it:
        // reporting is not license to modify.
        if !self.args.dry_run {
            fs::remove_file(&cached).map_err(crate::io_error)?;
        }
        Ok(Source::Remote)
    }

    fn cache_path(&self, object: &avc_core::ObjectId) -> Option<PathBuf> {
        self.args
            .cache
            .as_ref()
            .map(|cache| cache.join(object.cache_key()))
    }
}

/// Where the bytes for one object come from.
enum Source {
    /// A local file already holding them: a cache entry, or somewhere this run
    /// has already written the same object.
    Copy(PathBuf),
    Remote,
}

/// Check artifacts on disk against their pointers, using nothing else.
///
/// No remote, no cache, no repository: everything needed is the pointer and the
/// bytes. That makes it usable as the last step of a job that fetched, or the
/// first step of one that received a workspace from another.
pub fn verify(args: &VerifyArgs) -> Result<(), Failure> {
    let located = collect_pointers(&args.paths)?;
    if located.is_empty() {
        return report_nothing_found(args.porcelain);
    }

    let mut table = Table::new(vec![
        Column::left("STATUS"),
        Column::right("SIZE"),
        Column::left("ARTIFACT"),
    ]);
    let mut failed = 0;
    for entry in &located {
        let (state, bytes) = crate::artifact_state(&args.output, &entry.pointer)?;
        if state != State::Ok {
            failed += 1;
        }
        let path = crate::display_path(&entry.pointer);
        if args.porcelain {
            println!("{}\t{bytes}\t{path}", state.label());
            continue;
        }
        let size = if state == State::Missing {
            "-".to_owned()
        } else {
            ui::size(bytes)
        };
        table.row(vec![
            Cell::new(state.label(), state.style()),
            Cell::plain(size),
            Cell::plain(path),
        ]);
    }

    if !args.porcelain {
        table.print();
        ui::summary(&format!(
            "{} checked: {} ok, {failed} not matching",
            ui::plural(located.len(), "artifact"),
            located.len() - failed
        ));
    }
    if failed > 0 {
        return Err(format!(
            "{failed} of {} do not match their pointers",
            ui::plural(located.len(), "artifact")
        )
        .into());
    }
    Ok(())
}

/// Build the store to download from.
///
/// A URL is the pipeline-shaped answer and needs nothing on disk; a name falls
/// back to repository configuration for a job that did clone. Credentials come
/// from the environment either way, which is where a CI system puts them.
fn open_remote(
    url: Option<&str>,
    name: Option<&str>,
) -> Result<Box<dyn avc_core::ObjectStore>, Failure> {
    if let Some(url) = url {
        let config = avc_core::RemoteConfig::from_url("--remote-url", url)?;
        return Ok(avc_core::remote::open(&config, None)?);
    }
    let repo = crate::load_repo().map_err(|error| {
        Failure::from(format!(
            "{error}; outside a repository, name the remote with --remote-url <url> \
             or set AVC_REMOTE_URL"
        ))
    })?;
    crate::open_store(&repo, name)
}

/// Gather the pointers a run should act on.
///
/// Results are sorted by artifact path, so two runs of the same job produce the
/// same log regardless of the order a filesystem happened to enumerate.
fn collect_pointers(paths: &[String]) -> Result<Vec<Located>, Failure> {
    let mut sources = Vec::new();
    if paths.is_empty() {
        collect_from(Path::new("."), &mut sources)?;
    }
    for value in expand_stdin(paths)? {
        collect_from(Path::new(&value), &mut sources)?;
    }
    sources.sort();
    sources.dedup();

    let mut located = Vec::with_capacity(sources.len());
    for source in sources {
        let text = fs::read_to_string(&source)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        let pointer = avc_core::Pointer::parse(&text)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        located.push(Located { source, pointer });
    }
    located.sort_by(|left, right| left.pointer.path.cmp(&right.pointer.path));

    // Two pointer files claiming one path would race to write the same bytes,
    // and the loser would be silently discarded.
    if let Some(window) = located
        .windows(2)
        .find(|pair| pair[0].pointer.path == pair[1].pointer.path)
    {
        return Err(format!(
            "{} and {} both track {}",
            window[0].source.display(),
            window[1].source.display(),
            window[0].pointer.path
        )
        .into());
    }
    Ok(located)
}

/// Replace a `-` argument with the newline-separated paths on stdin, so a
/// pipeline can select artifacts with the tools it already has:
/// `git ls-files '*.avc' | avc fetch -`.
fn expand_stdin(paths: &[String]) -> Result<Vec<String>, Failure> {
    let mut expanded = Vec::with_capacity(paths.len());
    for value in paths {
        if value != "-" {
            expanded.push(value.clone());
            continue;
        }
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(crate::io_error)?;
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                expanded.push(trimmed.to_owned());
            }
        }
    }
    Ok(expanded)
}

/// Add the pointer files named by one argument: a pointer file, or every
/// pointer beneath a directory.
fn collect_from(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), Failure> {
    if path.is_file() {
        output.push(path.to_path_buf());
        return Ok(());
    }
    if path.is_dir() {
        return walk(path, output);
    }
    Err(format!("no such pointer file or directory: {}", path.display()).into())
}

/// Every `.avc` file beneath `directory`, as paths that can be opened directly.
///
/// Symlinks are skipped rather than followed: a link back into the tree would
/// loop, and a link out of it would pull in pointers the job never asked for.
fn walk(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), Failure> {
    for entry in fs::read_dir(directory).map_err(crate::io_error)? {
        let path = entry.map_err(crate::io_error)?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // None of these hold a pointer worth finding, and all three can hold
        // an enormous number of files.
        if matches!(name, ".git" | ".avc" | "target") {
            continue;
        }
        let kind = fs::symlink_metadata(&path)
            .map_err(crate::io_error)?
            .file_type();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(&path, output)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("avc") {
            output.push(path);
        }
    }
    Ok(())
}

fn report_nothing_found(porcelain: bool) -> Result<(), Failure> {
    if !porcelain {
        ui::line("no AVC pointers found", Style::Warn);
        ui::note("pass pointer files or a directory to scan, or run inside a checkout");
    }
    Ok(())
}
