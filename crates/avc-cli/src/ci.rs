//! Commands built for a pipeline rather than a workstation.
//!
//! A build agent is not a developer's machine. It has no clone of the artifact
//! history, no `.avc/cache` to warm, and it is thrown away when the job ends.
//! It also rarely wants everything: an AVC repository is an artifact registry,
//! and a job that needs one model out of a hundred should pay for one model.
//!
//! [`fetch`] is built around that. It is given a *repository* and a *path
//! inside it* — never a bucket. The pointers come from Git, shallow and
//! text-only; the object store comes from the `.avc/config.toml` that came with
//! them, so a consumer never has to know or repeat where the bytes live; and
//! only the objects the selected paths name are downloaded, streamed and
//! verified straight to their destination with no repository and no cache in
//! the middle.
//!
//! [`verify`] re-checks artifacts against their pointers using nothing but what
//! is on disk, so a job can assert it built against the exact bytes a commit
//! named.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::progress::Progress;
use crate::registry::Registry;
use crate::ui::{self, Cell, Column, Style, Table};
use crate::{Failure, State};

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Paths inside the repository to fetch: one artifact, or a prefix naming
    /// every artifact beneath it. Defaults to all of them; `-` reads
    /// newline-separated paths from stdin.
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,

    /// Git URL of the repository to fetch from. Needs no clone and no checkout.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    pub repo: Option<String>,

    /// Branch, tag, or commit to read pointers at.
    #[arg(
        long = "ref",
        value_name = "REF",
        env = "AVC_REF",
        default_value = "HEAD"
    )]
    pub reference: String,

    /// Named object store, when the repository configures more than one.
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,

    /// Object store URL, overriding the one the repository configures. Rarely
    /// needed: the repository already knows where its bytes are.
    #[arg(long, value_name = "URL")]
    pub remote_url: Option<String>,

    /// Directory to write artifacts into, at the paths their pointers name.
    #[arg(long, short, value_name = "DIR")]
    pub output: Option<PathBuf>,

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
    /// Paths inside the repository to check. Defaults to all of them; `-` reads
    /// newline-separated paths from stdin.
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,

    /// Git URL of the repository whose pointers to check against.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    pub repo: Option<String>,

    /// Branch, tag, or commit to read pointers at.
    #[arg(
        long = "ref",
        value_name = "REF",
        env = "AVC_REF",
        default_value = "HEAD"
    )]
    pub reference: String,

    /// Directory the artifacts were written into.
    #[arg(long, short, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Stable tab-separated output for scripts: STATE, BYTES, PATH.
    #[arg(long)]
    pub porcelain: bool,
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
    /// Root the pointers' paths are resolved against.
    output: PathBuf,
    /// What the run has got through so far.
    ///
    /// Unlike `push` and `pull`, this counts every object the selection names,
    /// not only the ones that move: `fetch` earns its keep by re-hashing what
    /// is already on disk, and on a directory of large files that is most of
    /// the wall clock. A bar that ignored it would sit still through the part
    /// of the run somebody is actually waiting on.
    progress: Progress,
    /// Where each object has already landed during this run.
    ///
    /// A directory holding the same bytes at two paths names one object twice,
    /// and with no cache to fall back on the second entry would download it
    /// again. Copying from the first landing site instead keeps the transfer
    /// count equal to the number of distinct objects, which is what
    /// deduplication has to mean when there is nowhere else to put them.
    placed: std::collections::HashMap<String, PathBuf>,
}

/// Fetch the artifacts a repository path names, straight to where they belong.
pub fn fetch(args: &FetchArgs) -> Result<(), Failure> {
    let registry = Registry::open(args.repo.as_deref(), &args.reference)?;
    let selected = select(&registry, &args.paths)?;
    if selected.is_empty() {
        return report_nothing_found(args.porcelain);
    }
    // The object store is the repository's own, read from the configuration
    // that arrived with the pointers. A consumer names the repository and the
    // path; where the bytes live was decided once, by whoever set it up.
    let store = registry.store(args.remote_url.as_deref(), args.remote.as_deref())?;
    let mut fetcher = Fetcher {
        store,
        args,
        output: output_root(args.output.as_ref(), &registry),
        // Replaced below, once the plan says how much there is to get through.
        progress: Progress::off(),
        placed: std::collections::HashMap::new(),
    };

    if !args.porcelain {
        ui::heading(&format!(
            "fetching {} from {}",
            ui::plural(selected.len(), "artifact"),
            registry.describe()
        ));
        ui::field("objects", &fetcher.store.describe());
        ui::field("into", &fetcher.output.display().to_string());
        if let Some(cache) = &args.cache {
            ui::field("cache", &cache.display().to_string());
        }
        println!();
    }

    let plan = fetcher.plan(&selected)?;
    // `--porcelain` is a contract, and a progress line written into it is
    // corruption rather than decoration.
    if !args.porcelain {
        fetcher.progress = Progress::start(
            "fetching",
            plan.iter().map(|item| item.files()).sum(),
            plan.iter().map(|item| item.bytes()).sum(),
        );
    }

    let mut objects = 0;
    let mut bytes = 0;
    for item in &plan {
        let transfer = fetcher.artifact(item)?;
        objects += transfer.objects;
        bytes += transfer.bytes;
        let (state, style) = transfer.state(args.dry_run);
        let path = crate::display_path(&item.pointer);
        if args.porcelain {
            println!("{state}\t{}\t{}\t{path}", transfer.objects, transfer.bytes);
            continue;
        }
        let detail = if item.pointer.is_directory() {
            format!(
                "{}, {}",
                ui::plural(transfer.files, "file"),
                ui::size(transfer.total_bytes)
            )
        } else {
            ui::size(transfer.total_bytes)
        };
        fetcher.progress.clear();
        ui::action(state, style, &path, Some(&detail));
    }

    if !args.porcelain {
        fetcher.progress.finish();
        ui::summary(&format!(
            "{} {} ({}) for {}",
            if args.dry_run {
                "would fetch"
            } else {
                "fetched"
            },
            ui::plural(objects, "object"),
            ui::size(bytes),
            ui::plural(selected.len(), "artifact"),
        ));
    }
    Ok(())
}

/// One selected artifact, resolved: what it is and, for a directory, what it
/// contains.
struct Planned {
    pointer: avc_core::Pointer,
    tree: Option<avc_core::Tree>,
}

impl Planned {
    /// Files this artifact is made of — one, or however many its manifest
    /// names. The manifest itself is not one of them; see [`Transfer`].
    fn files(&self) -> usize {
        self.tree.as_ref().map_or(1, |tree| tree.entries.len())
    }

    fn bytes(&self) -> u64 {
        self.tree
            .as_ref()
            .map_or(self.pointer.object.size, |tree| tree.total_size())
    }
}

impl Fetcher<'_> {
    /// Resolve every selected artifact down to the files it is made of.
    ///
    /// Manifests are read here rather than in the middle of the transfer, which
    /// costs nothing — each is read exactly once either way — and buys an
    /// honest total to measure against. A manifest is metadata of a few bytes
    /// per file, never artifact content, so reading one is not a transfer a
    /// pipeline should see reported, and it is read even on a dry run because
    /// it is what decides the answer a dry run gives.
    fn plan(&self, selected: &[avc_core::Pointer]) -> Result<Vec<Planned>, Failure> {
        // Suppressed under `--porcelain` along with everything else that is not
        // a record, even though this one is transient and lands on stderr.
        let _status = (!self.args.porcelain)
            .then(|| crate::progress::Status::show("reading directory manifests"));
        selected
            .iter()
            .map(|pointer| {
                let tree = match pointer.is_directory() {
                    true => Some(self.manifest(pointer)?),
                    false => None,
                };
                Ok(Planned {
                    pointer: pointer.clone(),
                    tree,
                })
            })
            .collect()
    }

    /// Download one artifact: a file, or every file its manifest names.
    fn artifact(&mut self, item: &Planned) -> Result<Transfer, Failure> {
        let pointer = &item.pointer;
        let mut transfer = Transfer {
            files: item.files(),
            total_bytes: item.bytes(),
            ..Transfer::default()
        };
        let Some(tree) = &item.tree else {
            let target = self.output.join(&pointer.path);
            self.place(
                &pointer.object_id()?,
                pointer.object.size,
                &target,
                &pointer.path,
                &mut transfer,
            )?;
            return Ok(transfer);
        };

        for entry in &tree.entries {
            let label = format!("{}/{}", pointer.path, entry.path);
            let target = self.output.join(&pointer.path).join(&entry.path);
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
        self.progress.item(label);
        if target.exists() {
            let actual = avc_core::hash_file(target)?;
            if actual.size == size && actual.object == *object {
                // Already correct, and proving it cost a full read of the file.
                self.progress.done(size);
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
            self.progress.done(size);
            return Ok(());
        }

        match source {
            Source::Copy(path) => {
                crate::copy_atomic(&path, target)?;
                self.progress.done(size);
                Ok(())
            }
            // The cacheless path, and the default: bytes go from the remote to
            // where they belong, hashed on the way, written exactly once.
            Source::Remote if self.args.cache.is_none() => {
                let mut body = self.store.get(object)?;
                // Metered, so a single large object still moves the bar.
                crate::download_verified(
                    &mut self.progress.meter(&mut *body),
                    target,
                    object,
                    size,
                    label,
                )?;
                self.progress.object_done();
                Ok(())
            }
            // With a cache, the object lands there first so the next job can
            // skip the network, and the worktree copy comes off local disk.
            Source::Remote => {
                let cached = self.cache_path(object).expect("a cache is configured");
                let mut body = self.store.get(object)?;
                crate::download_verified(
                    &mut self.progress.meter(&mut *body),
                    &cached,
                    object,
                    size,
                    label,
                )?;
                crate::copy_atomic(&cached, target)?;
                self.progress.object_done();
                Ok(())
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

/// Check artifacts on disk against their pointers.
///
/// Everything needed is the pointers and the bytes: no object store is
/// contacted and no credentials are read. That makes it usable as the last step
/// of a job that fetched, or the first step of one that inherited a workspace
/// from another — and, with `--repo`, as a check that a deployed directory
/// still matches a particular commit of the registry.
pub fn verify(args: &VerifyArgs) -> Result<(), Failure> {
    let registry = Registry::open(args.repo.as_deref(), &args.reference)?;
    let selected = select(&registry, &args.paths)?;
    if selected.is_empty() {
        return report_nothing_found(args.porcelain);
    }
    let output = output_root(args.output.as_ref(), &registry);
    if !args.porcelain {
        ui::heading(&format!(
            "verifying {} against {}",
            ui::plural(selected.len(), "artifact"),
            registry.describe()
        ));
        ui::field("in", &output.display().to_string());
        println!();
    }

    let mut table = Table::new(vec![
        Column::left("STATUS"),
        Column::right("SIZE"),
        Column::left("ARTIFACT"),
    ]);
    let mut failed = 0;
    for pointer in &selected {
        let (state, bytes) = crate::artifact_state(&output, pointer)?;
        if state != State::Ok {
            failed += 1;
        }
        let path = crate::display_path(pointer);
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
            ui::plural(selected.len(), "artifact"),
            selected.len() - failed
        ));
    }
    if failed > 0 {
        return Err(format!(
            "{failed} of {} do not match their pointers",
            ui::plural(selected.len(), "artifact")
        )
        .into());
    }
    Ok(())
}

/// The artifacts a run should act on.
///
/// Path selection is the registry's, so `avc fetch models/bert` means the same
/// thing here as `avc pull models/bert` does in a checkout. `-` is expanded
/// first, which lets a pipeline choose with the tools it already has:
/// `git diff --name-only -- '*.avc' | avc fetch -`.
fn select(registry: &Registry, paths: &[String]) -> Result<Vec<avc_core::Pointer>, Failure> {
    registry.select(&expand_stdin(paths)?)
}

/// Where artifacts are written, or looked for.
///
/// Defaulting to the repository root rather than the current directory is what
/// makes a pointer's path mean the same thing in a pipeline as it does in a
/// checkout. A registry read from a Git URL has no meaningful root of its own —
/// its checkout is a temporary directory — so that case falls back to here.
fn output_root(explicit: Option<&PathBuf>, registry: &Registry) -> PathBuf {
    match explicit {
        Some(path) => path.clone(),
        None if registry.is_local() => registry.root().to_path_buf(),
        None => PathBuf::from("."),
    }
}

/// Replace a `-` argument with the newline-separated paths on stdin.
fn expand_stdin(paths: &[String]) -> Result<Vec<String>, Failure> {
    if !paths.iter().any(|value| value == "-") {
        return Ok(paths.to_vec());
    }
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

fn report_nothing_found(porcelain: bool) -> Result<(), Failure> {
    if !porcelain {
        ui::line("no AVC pointers found", Style::Warn);
        ui::note("name the repository with --repo <git-url>, or run inside a checkout");
    }
    Ok(())
}
