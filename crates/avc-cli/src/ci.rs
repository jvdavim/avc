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
//! Two consequences of "a job wants one thing" run through this module. What a
//! path names lands in the output directory under its own name, rather than at
//! the end of the directories the repository happens to file it under — a
//! consumer asked for `models/bert`, not for a `models/` to be created in its
//! workspace. And a path may reach *into* a tracked directory, naming one file
//! or one subdirectory of it, because the publisher's decision to group a
//! hundred files under one pointer should not force a consumer to take all of
//! them. Both are the [`Selection`]'s doing; neither changes what is stored,
//! since every file inside a tracked directory is already an object of its own.
//!
//! [`verify`] re-checks artifacts against their pointers using nothing but what
//! is on disk, so a job can assert it built against the exact bytes a commit
//! named. It looks where [`fetch`] writes, so the two agree by construction.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::progress::Progress;
use crate::registry::{Registry, Selection};
use crate::ui::{self, Cell, Column, Style, Table};
use crate::{Failure, State};

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Paths inside the repository to fetch: one artifact, a prefix naming
    /// every artifact beneath it, or a file or subdirectory inside a tracked
    /// directory. Defaults to all of them; `-` reads newline-separated paths
    /// from stdin.
    ///
    /// What a path names lands directly in the output directory: `avc fetch
    /// models/bert -o .` writes `./bert`, not `./models/bert`.
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,

    /// Git URL of the repository to fetch from. Needs no clone and no checkout.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    pub repo: Option<String>,

    /// Revision to read pointers at: a branch, a tag, a commit, or a fully
    /// qualified `refs/...` name. Defaults to the repository's default branch,
    /// or, in a checkout, to the pointers on disk.
    #[arg(long = "ref", value_name = "REV", env = "AVC_REF")]
    pub reference: Option<String>,

    /// Named object store, when the repository configures more than one.
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,

    /// Object store URL, overriding the one the repository configures. Rarely
    /// needed: the repository already knows where its bytes are.
    #[arg(long, value_name = "URL")]
    pub remote_url: Option<String>,

    /// Directory to write into. What each path named lands here under its own
    /// name; the directories walked to reach it are not recreated.
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
    ///
    /// Looked for where `avc fetch` would have written them: what a path names
    /// is expected directly in the output directory.
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,

    /// Git URL of the repository whose pointers to check against.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    pub repo: Option<String>,

    /// Revision to read pointers at: a branch, a tag, a commit, or a fully
    /// qualified `refs/...` name. Defaults to the repository's default branch,
    /// or, in a checkout, to the pointers on disk.
    #[arg(long = "ref", value_name = "REV", env = "AVC_REF")]
    pub reference: Option<String>,

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
    /// Root the planned destinations are resolved against.
    output: PathBuf,
    /// Whether to deliver what was named into [`Self::output`], or restore
    /// artifacts to the repository paths they belong at. See [`relocates`].
    relocate: bool,
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
    let registry = Registry::open(args.repo.as_deref(), args.reference.as_deref())?;
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
        relocate: relocates(args.output.as_ref(), &registry),
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
    reject_collisions(&plan)?;
    // `--porcelain` is a contract, and a progress line written into it is
    // corruption rather than decoration.
    if !args.porcelain {
        fetcher.progress = Progress::start(
            "fetching",
            plan.iter().map(|item| item.file_count()).sum(),
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
        let path = &item.label;
        if args.porcelain {
            println!("{state}\t{}\t{}\t{path}", transfer.objects, transfer.bytes);
            continue;
        }
        let detail = if item.directory {
            format!(
                "{}, {}",
                ui::plural(transfer.files, "file"),
                ui::size(transfer.total_bytes)
            )
        } else {
            ui::size(transfer.total_bytes)
        };
        fetcher.progress.clear();
        ui::action(state, style, path, Some(&detail));
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

/// One selection, resolved down to the individual files it is made of.
///
/// Resolving this far up front is what lets a directory, part of a directory,
/// and a plain file all travel through the same loop: by the time anything is
/// transferred there are only files, each with an object to get and a place to
/// put it.
struct Planned {
    /// What the user named, for the log line.
    label: String,
    /// Whether the artifact behind this is a tracked directory, which decides
    /// only how the line is worded.
    directory: bool,
    files: Vec<PlannedFile>,
}

/// One file to place: which object, and where it goes.
struct PlannedFile {
    object: avc_core::ObjectId,
    size: u64,
    /// The repository path of this file, for messages.
    logical: String,
    /// Where it lands, relative to the output root.
    destination: PathBuf,
}

impl Planned {
    /// How many files this selection is made of — one, or however many of a
    /// manifest's entries it named. The manifest itself is not one of them;
    /// see [`Transfer`].
    fn file_count(&self) -> usize {
        self.files.len()
    }

    fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

impl Fetcher<'_> {
    /// Resolve every selection down to the files it is made of, and where each
    /// of them goes.
    ///
    /// Manifests are read here rather than in the middle of the transfer, which
    /// costs nothing — each is read exactly once either way — and buys an
    /// honest total to measure against. A manifest is metadata of a few bytes
    /// per file, never artifact content, so reading one is not a transfer a
    /// pipeline should see reported, and it is read even on a dry run because
    /// it is what decides the answer a dry run gives.
    fn plan(&self, selected: &[Selection]) -> Result<Vec<Planned>, Failure> {
        // Suppressed under `--porcelain` along with everything else that is not
        // a record, even though this one is transient and lands on stderr.
        let _status = (!self.args.porcelain)
            .then(|| crate::progress::Status::show("reading directory manifests"));
        selected
            .iter()
            .map(|selection| {
                let pointer = &selection.pointer;
                if !pointer.is_directory() {
                    return Ok(Planned {
                        label: selection.label(),
                        directory: false,
                        files: vec![PlannedFile {
                            object: pointer.object_id()?,
                            size: pointer.object.size,
                            destination: destination(selection, &pointer.path, self.relocate),
                            logical: pointer.path.clone(),
                        }],
                    });
                }
                let tree = self.manifest(pointer)?;
                let mut files = Vec::new();
                for entry in &tree.entries {
                    if !selection.includes(&entry.path) {
                        continue;
                    }
                    let logical = format!("{}/{}", pointer.path, entry.path);
                    files.push(PlannedFile {
                        object: entry.object_id()?,
                        size: entry.size,
                        destination: destination(selection, &logical, self.relocate),
                        logical,
                    });
                }
                if files.is_empty() {
                    // Only a selector that reached *into* the directory can
                    // name nothing; the directory itself always has entries.
                    return Err(crate::registry::missing_inside(selection).into());
                }
                Ok(Planned {
                    label: selection.label(),
                    directory: true,
                    files,
                })
            })
            .collect()
    }

    /// Download one selection: a file, or every file of a manifest it named.
    fn artifact(&mut self, item: &Planned) -> Result<Transfer, Failure> {
        let mut transfer = Transfer {
            files: item.file_count(),
            total_bytes: item.bytes(),
            ..Transfer::default()
        };
        for file in &item.files {
            let target = self.output.join(&file.destination);
            self.place(
                &file.object,
                file.size,
                &target,
                &file.logical,
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
        let actual = avc_core::hash_reader(&mut bytes.as_slice(), pointer.algorithm())?;
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
            let actual = avc_core::hash_file(target, object.algorithm())?;
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
        let actual = avc_core::hash_file(&cached, object.algorithm())?;
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

/// Refuse a run in which two different files would be written to one path.
///
/// Dropping the directories a selector walked through is what makes `-o` mean
/// what a caller expects, and it is also the one way this command can be asked
/// to do something incoherent: `avc fetch a/model.bin b/model.bin` names two
/// artifacts that would both become `./model.bin`. Silently letting the second
/// overwrite the first would leave a workspace that passes `avc verify` for one
/// of them and lies about the other.
fn reject_collisions(plan: &[Planned]) -> Result<(), Failure> {
    let mut claimed: std::collections::HashMap<&Path, &str> = std::collections::HashMap::new();
    for item in plan {
        for file in &item.files {
            if let Some(other) = claimed.insert(&file.destination, &file.logical) {
                if other != file.logical {
                    return Err(format!(
                        "{other} and {} would both be written to {}; \
                         fetch them in separate runs, or name a parent they share \
                         so their paths stay distinct",
                        file.logical,
                        file.destination.display()
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

/// Check artifacts on disk against their pointers.
///
/// Everything needed is the pointers and the bytes: no object store is
/// contacted and no credentials are read. That makes it usable as the last step
/// of a job that fetched, or the first step of one that inherited a workspace
/// from another — and, with `--repo`, as a check that a deployed directory
/// still matches a particular commit of the registry.
pub fn verify(args: &VerifyArgs) -> Result<(), Failure> {
    let registry = Registry::open(args.repo.as_deref(), args.reference.as_deref())?;
    let selected = select(&registry, &args.paths)?;
    if selected.is_empty() {
        return report_nothing_found(args.porcelain);
    }
    let output = output_root(args.output.as_ref(), &registry);
    let relocate = relocates(args.output.as_ref(), &registry);
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
    for selection in &selected {
        let (state, bytes) = verify_one(&registry, &output, relocate, selection)?;
        if state != State::Ok {
            failed += 1;
        }
        let path = selection.label();
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

/// Check one selection where `fetch` would have put it.
///
/// A whole artifact is compared against its pointer, which needs nothing but
/// the two. Part of a tracked directory is compared against its manifest
/// entries instead — the pointer describes the directory as a whole, and a
/// workspace holding one file out of it will never match that. The manifest is
/// an object, and `verify` contacts no store, so it has to come from a local
/// cache; without one, say so rather than guess.
fn verify_one(
    registry: &Registry,
    output: &Path,
    relocate: bool,
    selection: &Selection,
) -> Result<(State, u64), Failure> {
    let pointer = &selection.pointer;
    if selection.inside.is_none() {
        let path = output.join(destination(selection, &pointer.path, relocate));
        return crate::artifact_state_at(&path, pointer);
    }

    let tree = crate::load_tree(registry.repo(), pointer).map_err(|_| {
        format!(
            "checking {} needs the manifest of the tracked directory {}, which is not on \
             this machine; `avc verify {}` checks the whole directory using nothing but \
             the pointer",
            selection.label(),
            pointer.path,
            pointer.path
        )
    })?;

    let mut bytes = 0;
    let mut missing = 0;
    let mut modified = 0;
    let mut checked = 0;
    for entry in tree.entries.iter().filter(|entry| {
        // Entry paths are relative to the directory; the selection knows which
        // of them it named.
        selection.includes(&entry.path)
    }) {
        let logical = format!("{}/{}", pointer.path, entry.path);
        let path = output.join(destination(selection, &logical, relocate));
        let (state, size) = crate::file_state_at(&path, entry)?;
        checked += 1;
        bytes += size;
        match state {
            State::Ok => {}
            State::Missing => missing += 1,
            State::Modified => modified += 1,
        }
    }
    if checked == 0 {
        return Err(crate::registry::missing_inside(selection).into());
    }
    // `missing` is reserved for nothing being there at all. A selection that is
    // partly present is `modified`, because a workspace holding half of what was
    // asked for is wrong in a way a gate must not pass.
    let state = match (missing, modified) {
        (0, 0) => State::Ok,
        (missing, 0) if missing == checked => State::Missing,
        _ => State::Modified,
    };
    Ok((state, bytes))
}

/// The artifacts a run should act on.
///
/// Path selection is the registry's, so `avc fetch models/bert` means the same
/// thing here as `avc pull models/bert` does in a checkout. `-` is expanded
/// first, which lets a pipeline choose with the tools it already has:
/// `git diff --name-only -- '*.avc' | avc fetch -`.
fn select(registry: &Registry, paths: &[String]) -> Result<Vec<Selection>, Failure> {
    registry.select(&expand_stdin(paths)?)
}

/// Where artifacts are written, or looked for.
///
/// Defaulting to the repository root rather than the current directory is what
/// makes a pointer's path mean the same thing in a pipeline as it does in a
/// checkout. The root is the worktree the registry belongs to, which is not
/// where its pointers were read from: `--ref v1.0.0` in a checkout reads
/// pointers out of a temporary directory, but the artifacts they name still
/// belong in the worktree. A registry named by URL has no worktree, so that
/// case falls back to here.
///
fn output_root(explicit: Option<&PathBuf>, registry: &Registry) -> PathBuf {
    match explicit {
        Some(path) => path.clone(),
        None => registry
            .worktree()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    }
}

/// Where one repository path lands beneath the output root.
///
/// Delivering drops the directories the selector walked through, so what was
/// named arrives under its own name; restoring keeps the path a pointer gives,
/// because that is where the artifact belongs in the worktree.
fn destination(selection: &Selection, repository_path: &str, relocate: bool) -> PathBuf {
    PathBuf::from(if relocate {
        selection.destination(repository_path)
    } else {
        repository_path.to_owned()
    })
}

/// Whether paths beneath the output root come from the selection or from the
/// pointers.
///
/// These are two different jobs wearing one command. *Delivering* — a build
/// agent asking for `models/bert` and a directory to put it in — should produce
/// exactly what was asked for, named as it was asked for, with no `models/`
/// invented along the way. *Restoring* — someone in a checkout running `avc
/// fetch models/gpt --ref v1.0.0` to put an older version back — must write
/// artifacts exactly where their pointers say they live, or it has not restored
/// anything.
///
/// Naming an output directory says which of the two this is: it is a place to
/// deliver to. Without one, a checkout is being restored, and a registry read
/// by URL has no worktree to restore into, so it is delivering to the current
/// directory. `verify` asks the same question so it looks where `fetch` wrote.
fn relocates(explicit: Option<&PathBuf>, registry: &Registry) -> bool {
    explicit.is_some() || registry.worktree().is_none()
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
