//! Migrating a DVC project into an AVC one.
//!
//! DVC and AVC solve the same problem and agree on almost nothing about how:
//! DVC addresses content by MD5 and AVC by SHA-256, their pointer files share
//! no fields, and their object keys share no layout. Nothing on either side can
//! read the other. What they *do* share is the shape of the idea — a small file
//! in Git naming a large object in a bucket — and that is enough to translate
//! one into the other completely.
//!
//! This is a translation of a whole project, not of its tip. Every commit on
//! every branch is replayed with its `.dvc` files rewritten as `.avc` pointers,
//! so the history keeps its shape and an artifact can still be fetched as of an
//! old tag. Every object any of those commits references is moved, not just the
//! ones the current checkout happens to name.
//!
//! # Why an object's identity is preserved
//!
//! AVC addresses objects by SHA-256. Computing one for a DVC object means
//! reading every byte of it, and a migration reads *every version of every
//! artifact* — for a real remote that is a download measured in terabytes,
//! which is exactly the cost that stops a migration happening at all.
//!
//! So by default it does not do that. AVC records the algorithm alongside every
//! digest, in the pointer, in the manifest, and in the object key, which means a
//! migrated artifact can keep the MD5 identity DVC already gave it. The object
//! moves without being read: when the destination is on the same S3 service, it
//! is a server-side copy and no bytes travel at all. `--rehash` buys SHA-256
//! identities at the price of reading everything, and says so.
//!
//! # Resuming
//!
//! Every phase records what it finished. An interrupted migration re-run with
//! the same arguments picks up where it stopped: the listing is not repeated,
//! transferred objects are not re-sent, and rewritten commits are not rebuilt.
//! See [`journal`].

mod dvc;
mod history;
mod journal;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::path::PathBuf;

use clap::Args;

use crate::progress::{self, Progress};
use crate::ui::{self, Style};
use crate::{io_error, Failure};

use history::{Change, Git};
use journal::{Journal, Needed, Phase};

/// Files DVC owns that an AVC project has no use for.
///
/// `dvc.yaml` is deliberately absent: it is the user's pipeline definition,
/// which AVC has no equivalent of and no business deleting. `dvc.lock` is
/// absent for a different reason — it is a tracking file, so it is removed
/// where the pointers it replaced are, not here.
const DVC_ONLY_FILES: [&str; 1] = [".dvcignore"];

#[derive(Debug, Args)]
pub(crate) struct MigrateArgs {
    /// Git URL or local path of the DVC repository to migrate.
    ///
    /// Read, never written. Every branch and tag it has is replayed.
    #[arg(value_name = "DVC_REPO")]
    from_repo: String,

    /// Object store URL of the DVC remote holding that repository's data,
    /// spelled the way `avc remote add` spells one: `s3://bucket/prefix`.
    #[arg(value_name = "DVC_REMOTE")]
    from_remote: String,

    /// Directory for the AVC repository.
    ///
    /// Created, with a fresh Git repository in it, when it does not exist or
    /// holds no commits — and the DVC history is then replayed as it stands,
    /// branch for branch. When it already has commits, the migrated branches
    /// are given a prefix so that nothing already there is touched.
    #[arg(long, value_name = "DIR")]
    into: PathBuf,

    /// Object store URL the migrated repository will use, including any key
    /// prefix: `s3://my-bucket/artifacts`.
    ///
    /// Naming the DVC remote's own bucket here is the cheap path, and the
    /// recommended one for a store too large to copy across a network: objects
    /// are then moved by the storage service itself, and no artifact bytes
    /// travel through this machine at all.
    #[arg(long = "to", value_name = "URL")]
    to: String,

    /// Name to record the migrated remote under.
    #[arg(long, value_name = "NAME", default_value = "origin")]
    remote_name: String,

    /// Prefix put on migrated branch and tag names when the destination
    /// repository already has commits of its own.
    #[arg(long, value_name = "PREFIX", default_value = "dvc-")]
    branch_prefix: String,

    /// Re-address every object with SHA-256 instead of keeping DVC's MD5.
    ///
    /// Leaves a repository with no MD5 in it, at the cost of reading every
    /// byte of every version of every artifact over the network. Worth it for
    /// a small store; think twice for a large one.
    #[arg(long)]
    rehash: bool,

    /// Leave DVC's own files in the rewritten history.
    ///
    /// By default `.dvc` pointer files, the `.dvc/` directory, `.dvcignore`
    /// and `dvc.lock` are dropped, since the `.avc` pointers replace them.
    #[arg(long)]
    keep_dvc_files: bool,

    /// Key layout of the DVC remote. Detected by default.
    #[arg(long, value_name = "LAYOUT", default_value = "auto")]
    dvc_layout: dvc::Layout,

    /// SigV4 region for the DVC remote, when it is not in the environment.
    #[arg(long, value_name = "REGION")]
    from_region: Option<String>,

    /// `~/.aws` profile to read credentials for the DVC remote from.
    #[arg(long, value_name = "NAME")]
    from_profile: Option<String>,

    /// SigV4 region for the destination remote.
    #[arg(long, value_name = "REGION")]
    to_region: Option<String>,

    /// `~/.aws` profile to read credentials for the destination remote from.
    #[arg(long, value_name = "NAME")]
    to_profile: Option<String>,

    /// Discard any recorded progress and migrate from the beginning.
    #[arg(long)]
    restart: bool,
}

/// Everything the phases below share.
struct Migration {
    args: MigrateArgs,
    git: Git,
    journal: Journal,
    source: Box<dyn avc_core::KeyStore>,
    target: Box<dyn avc_core::ObjectStore>,
    target_config: avc_core::RemoteConfig,
    layout: dvc::Layout,
    /// Sizes of every object on the DVC remote, by MD5.
    inventory: HashMap<String, u64>,
    /// Whether the destination had a history before this ran.
    existing: bool,
    /// Reasons outs were left behind, deduplicated across the whole history.
    skipped: BTreeMap<String, ()>,
}

impl Migration {
    /// Which algorithm a migrated artifact is addressed with.
    fn algorithm(&self) -> avc_core::Algorithm {
        if self.args.rehash {
            avc_core::Algorithm::Sha256
        } else {
            avc_core::Algorithm::Md5
        }
    }

    /// The object identity a DVC hash becomes.
    ///
    /// Without `--rehash` this is the MD5 itself, which is what makes the
    /// transfer a copy rather than a read. With it, the SHA-256 recorded when
    /// the object was streamed.
    fn object_for(&self, md5: &str) -> Result<avc_core::ObjectId, Failure> {
        if !self.args.rehash {
            return Ok(avc_core::ObjectId::new(avc_core::Algorithm::Md5, md5)?);
        }
        let hash = self.journal.rehashed(md5).ok_or_else(|| {
            Failure::from(format!(
                "object {md5} has not been re-hashed yet; re-run the migration to finish it"
            ))
        })?;
        Ok(avc_core::ObjectId::sha256(hash.clone())?)
    }
}

pub(crate) fn migrate(args: MigrateArgs) -> Result<(), Failure> {
    let mut migration = prepare(args)?;
    inventory(&mut migration)?;
    survey(&mut migration)?;
    transfer(&mut migration)?;
    manifests(&mut migration)?;
    let rewritten = replay(&mut migration)?;
    let refs = publish(&mut migration)?;
    finish(migration, rewritten, refs)
}

/// Open both ends, decide what kind of destination this is, and load any
/// record of an earlier attempt.
fn prepare(args: MigrateArgs) -> Result<Migration, Failure> {
    let source_config = remote_config(
        "dvc",
        &args.from_remote,
        &args.from_region,
        &args.from_profile,
    )?;
    let target_config = remote_config(
        &args.remote_name,
        &args.to,
        &args.to_region,
        &args.to_profile,
    )?;

    let git = Git::at(&args.into);
    // Asked before anything is created, because creating the repository would
    // otherwise make every destination look new.
    let existing = git.has_commits();
    git.init()?;

    let state = args.into.join(".avc/state/migrate");
    let journal = Journal::open(&state, &fingerprint(&args)?, args.restart)?;
    fs::create_dir_all(args.into.join(".avc/cache")).map_err(io_error)?;

    // What an earlier run decided wins over what this one would decide, so an
    // interrupted migration resumes into the same branch names it started
    // with.
    let existing = match journal.load_destination_existing() {
        Some(recorded) => recorded,
        None => {
            journal.save_destination_existing(existing)?;
            existing
        }
    };

    // Machine-local credentials and endpoint overrides are read from the
    // destination repository, which is the only one of the two this command is
    // standing in.
    let local = crate::load_local_override_at(&args.into, &args.remote_name)?;
    let source = avc_core::remote::open_source(&source_config, None)?;
    let target = avc_core::remote::open(&target_config, local.as_ref())?;

    ui::heading(&format!(
        "migrating {} into {}",
        crate::git::redact(&args.from_repo),
        args.into.display()
    ));
    ui::field("from", &source.describe());
    ui::field("to", &target.describe());
    ui::field(
        "addressing",
        if args.rehash {
            "sha256 (every object is read and re-hashed)"
        } else {
            "md5, preserved from DVC (objects are moved, not read)"
        },
    );
    ui::field(
        "destination",
        if existing {
            "existing repository; migrated refs get a prefix"
        } else {
            "new repository; branches and tags keep their names"
        },
    );
    println!();

    Ok(Migration {
        args,
        git,
        journal,
        source,
        target,
        target_config,
        layout: dvc::Layout::Auto,
        inventory: HashMap::new(),
        existing,
        skipped: BTreeMap::new(),
    })
}

/// Build a remote configuration from a URL and the flags that refine it.
fn remote_config(
    name: &str,
    url: &str,
    region: &Option<String>,
    profile: &Option<String>,
) -> Result<avc_core::RemoteConfig, Failure> {
    let mut config = avc_core::RemoteConfig::from_url(name, url)?;
    config.region = region.clone().filter(|value| !value.trim().is_empty());
    config.profile = profile.clone().filter(|value| !value.trim().is_empty());
    Ok(config)
}

/// What this run was asked to do, as one string.
///
/// A journal recording different arguments describes a different migration, and
/// resuming it would mix two of them into one repository.
fn fingerprint(args: &MigrateArgs) -> Result<String, Failure> {
    let text = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        args.from_repo,
        args.from_remote,
        args.to,
        args.remote_name,
        args.rehash,
        args.keep_dvc_files
    );
    let hash = avc_core::hash_reader(&mut text.as_bytes(), avc_core::Algorithm::Sha256)?;
    Ok(hash.object.hash().to_owned())
}

/// Phase one: learn what the DVC remote holds.
///
/// One listing answers two questions that would otherwise cost a request per
/// object: which key layout this remote uses, and how large every object is.
/// DVC's directory manifests do not record sizes, so without this the migration
/// could not report a total before starting, or plan a transfer at all.
fn inventory(migration: &mut Migration) -> Result<(), Failure> {
    if migration.journal.is_complete(Phase::Inventory) {
        migration.inventory = migration.journal.load_inventory()?;
        migration.layout = match migration.journal.load_layout().as_deref() {
            Some("legacy") => dvc::Layout::Legacy,
            _ => dvc::Layout::FilesMd5,
        };
        ui::action(
            "inventory",
            Style::Dim,
            "already taken",
            Some(&ui::plural(migration.inventory.len(), "object")),
        );
        return Ok(());
    }

    let status = progress::Status::show("listing the DVC remote");
    let candidates: Vec<dvc::Layout> = match migration.args.dvc_layout {
        dvc::Layout::Auto => vec![dvc::Layout::FilesMd5, dvc::Layout::Legacy],
        chosen => vec![chosen],
    };
    let mut resolved = None;
    for layout in candidates {
        let keys = migration.source.list_keys(layout.prefix())?;
        let sizes: HashMap<String, u64> = keys
            .into_iter()
            .filter_map(|(key, size)| Some((layout.md5_from_key(&key)?.0, size)))
            .collect();
        if !sizes.is_empty() {
            resolved = Some((layout, sizes));
            break;
        }
    }
    drop(status);

    let (layout, sizes) = resolved.ok_or_else(|| {
        Failure::from(format!(
            "found no DVC objects on {}; check the remote URL and its prefix, or name the \
             layout with --dvc-layout if this remote was written by a DVC older than 3.0",
            migration.source.describe()
        ))
    })?;
    migration.layout = layout;
    migration.inventory = sizes;
    migration.journal.save_inventory(&migration.inventory)?;
    migration.journal.save_layout(match layout {
        dvc::Layout::Legacy => "legacy",
        _ => "files-md5",
    })?;
    migration.journal.complete(Phase::Inventory)?;

    let total: u64 = migration.inventory.values().sum();
    ui::action(
        "inventory",
        Style::Ok,
        layout.label(),
        Some(&format!(
            "{}, {}",
            ui::plural(migration.inventory.len(), "object"),
            ui::size(total)
        )),
    );
    Ok(())
}

/// Phase two: read the whole history and work out which objects it needs.
///
/// Every commit, not just the tips: an artifact that was replaced five years
/// ago is still referenced by the commit that replaced it, and a migration that
/// left it behind would produce a history whose old revisions cannot be
/// restored.
fn survey(migration: &mut Migration) -> Result<(), Failure> {
    history::fetch_source(&migration.git, &migration.args.from_repo)?;
    if migration.journal.is_complete(Phase::Survey) {
        ui::action("survey", Style::Dim, "already taken", None);
        return Ok(());
    }

    let commits = history::commits_in_order(&migration.git)?;
    let progress = Progress::start("surveying", commits.len(), 0);
    let mut needed: BTreeMap<String, Needed> = BTreeMap::new();
    let mut parsed_blobs: HashMap<String, dvc::Parsed> = HashMap::new();

    for commit in &commits {
        progress.item(&commit[..12.min(commit.len())]);
        for out in outs_in_commit(migration, commit, &mut parsed_blobs)? {
            let Some(size) = migration.inventory.get(&out.md5).copied() else {
                // The pointer says the object should be there and it is not.
                // Reported once, by hash, rather than failing: a DVC remote
                // that has been garbage-collected legitimately no longer holds
                // every version its history mentions.
                migration.skipped.insert(
                    format!("{}: object {} is not on the DVC remote", out.path, out.md5),
                    (),
                );
                continue;
            };
            if out.directory {
                fetch_dir_manifest(migration, &out.md5)?;
                let entries =
                    dvc::parse_dir_manifest(&migration.journal.load_dir_manifest(&out.md5)?)?;
                for entry in entries {
                    let Some(size) = migration.inventory.get(&entry.md5).copied() else {
                        migration.skipped.insert(
                            format!(
                                "{}/{}: object {} is not on the DVC remote",
                                out.path, entry.relpath, entry.md5
                            ),
                            (),
                        );
                        continue;
                    };
                    needed.entry(entry.md5.clone()).or_insert(Needed {
                        md5: entry.md5,
                        size,
                        directory: false,
                    });
                }
            }
            needed.entry(out.md5.clone()).or_insert(Needed {
                md5: out.md5,
                size,
                directory: out.directory,
            });
        }
        progress.object_done();
    }
    progress.finish();

    let all: Vec<Needed> = needed.into_values().collect();
    migration.journal.save_survey(&all)?;
    migration.journal.complete(Phase::Survey)?;

    let bytes: u64 = all
        .iter()
        .filter(|object| !object.directory)
        .map(|object| object.size)
        .sum();
    ui::action(
        "survey",
        Style::Ok,
        &format!("{} of history", ui::plural(commits.len(), "commit")),
        Some(&format!(
            "{} to move, {}",
            ui::plural(all.iter().filter(|o| !o.directory).count(), "object"),
            ui::size(bytes)
        )),
    );
    Ok(())
}

/// Every DVC out referenced by one commit.
///
/// Blobs are parsed once and reused: one `.dvc` file usually survives unchanged
/// across hundreds of commits, and re-reading it for each of them is most of
/// what a naive survey spends its time on.
fn outs_in_commit(
    migration: &mut Migration,
    commit: &str,
    cache: &mut HashMap<String, dvc::Parsed>,
) -> Result<Vec<dvc::Out>, Failure> {
    let mut outs = Vec::new();
    for file in history::list_tree(&migration.git, commit)? {
        let Some(kind) = classify(&file.path) else {
            continue;
        };
        if !file.is_blob() {
            continue;
        }
        if !cache.contains_key(&file.id) {
            let bytes = history::read_blob(&migration.git, &file.id)?;
            let text = String::from_utf8_lossy(&bytes);
            let directory = parent_of(&file.path);
            let parsed = match kind {
                DvcFile::Pointer => dvc::parse_dvc_file(&text, directory)?,
                DvcFile::Lock => dvc::parse_dvc_lock(&text, directory)?,
            };
            cache.insert(file.id.clone(), parsed);
        }
        let parsed = &cache[&file.id];
        for reason in &parsed.skipped {
            migration.skipped.insert(reason.clone(), ());
        }
        outs.extend(parsed.outs.iter().cloned());
    }
    Ok(outs)
}

/// Which of DVC's two tracking files a path is, if either.
#[derive(Clone, Copy)]
enum DvcFile {
    /// A `.dvc` file: data tracked by hand, the direct analogue of `avc add`.
    Pointer,
    /// A `dvc.lock`: what a pipeline stage produced. Tracked in the same cache
    /// and on the same remote, so just as much a part of the migration.
    Lock,
}

fn classify(path: &str) -> Option<DvcFile> {
    // Anything under `.dvc/` is DVC's own configuration, not a pointer, even
    // though `.dvc/config` ends in neither.
    if path == ".dvc" || path.starts_with(".dvc/") {
        return None;
    }
    if path.ends_with(".dvc") {
        return Some(DvcFile::Pointer);
    }
    if path == "dvc.lock" || path.ends_with("/dvc.lock") {
        return Some(DvcFile::Lock);
    }
    None
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Download one DVC directory manifest and keep it for the phases that follow.
fn fetch_dir_manifest(migration: &Migration, md5: &str) -> Result<(), Failure> {
    if migration.journal.dir_manifest_path(md5).is_file() {
        return Ok(());
    }
    let key = migration.layout.key(md5, true);
    let mut bytes = Vec::new();
    std::io::copy(&mut migration.source.get_key(&key)?, &mut bytes).map_err(io_error)?;
    // A manifest decides what gets downloaded and where it is written, so it
    // is verified against the name it was fetched under before it is trusted.
    let actual = avc_core::hash_reader(&mut bytes.as_slice(), avc_core::Algorithm::Md5)?;
    if actual.object.hash() != md5 {
        return Err(format!(
            "the DVC directory manifest at {key} does not hash to {md5}; the remote is corrupt"
        )
        .into());
    }
    migration.journal.save_dir_manifest(md5, &bytes)
}

/// Phase three: move the artifact bytes.
fn transfer(migration: &mut Migration) -> Result<(), Failure> {
    if migration.journal.is_complete(Phase::Transfer) {
        ui::action("transfer", Style::Dim, "already done", None);
        return Ok(());
    }
    let survey = migration.journal.load_survey()?;
    // A DVC directory manifest is not artifact content and never reaches the
    // destination: AVC's own manifest replaces it, and is built in the next
    // phase out of the files this one moves.
    let wanted: Vec<Needed> = survey
        .into_iter()
        .filter(|object| !object.directory)
        .collect();

    // What is left, so the bar has a denominator that a resumed run does not
    // exaggerate. Either way the question is answerable without a request:
    // without a rehash the destination key is the MD5 itself, and with one the
    // journal remembers what each object was re-addressed to.
    let outstanding: Vec<&Needed> = wanted
        .iter()
        .filter(|object| !is_moved(migration, object))
        .collect();
    let bytes: u64 = outstanding.iter().map(|object| object.size).sum();
    let progress = Progress::start(
        if migration.args.rehash {
            "re-hashing"
        } else {
            "moving"
        },
        outstanding.len(),
        bytes,
    );

    let mut copied = 0_usize;
    let mut streamed = 0_usize;
    let mut moved_bytes = 0_u64;
    for object in &wanted {
        if is_moved(migration, object) {
            continue;
        }
        progress.item(&object.md5);
        let key = migration.layout.key(&object.md5, false);

        if migration.args.rehash {
            let hash = stream_rehashed(migration, &key, object, &progress)
                .map_err(|error| moving(error, &object.md5))?;
            // Both records: the mapping the manifests and pointers will need,
            // and the destination hash, so `stored` means one thing — "this
            // object is on the AVC remote" — whichever mode wrote it.
            migration.journal.mark_rehashed(&object.md5, &hash)?;
            migration.journal.mark_stored(&hash)?;
            streamed += 1;
        } else {
            let id = avc_core::ObjectId::new(avc_core::Algorithm::Md5, &object.md5)?;
            // The whole point of preserving identity: ask the service to move
            // the bytes itself. Where it cannot — a different service at the
            // far end, or an object too large for a single copy — fall back to
            // streaming it through, which always works and is only slower.
            let source = migration.source.copy_source(&key);
            let moved = migration
                .target
                .put_copy(&source, &id, object.size)
                .map_err(|error| moving(error, &object.md5))?;
            if moved {
                progress.done(object.size);
                copied += 1;
            } else {
                let mut body = migration
                    .source
                    .get_key(&key)
                    .map_err(|error| moving(error, &object.md5))?;
                migration
                    .target
                    .put(&id, object.size, &mut progress.meter(&mut *body))
                    .map_err(|error| moving(error, &object.md5))?;
                progress.object_done();
                streamed += 1;
            }
            migration.journal.mark_stored(&object.md5)?;
        }
        moved_bytes += object.size;
    }
    progress.finish();
    migration.journal.complete(Phase::Transfer)?;

    let detail = if copied > 0 && streamed == 0 {
        format!(
            "{} copied by the storage service, no bytes over the network",
            ui::plural(copied, "object")
        )
    } else if copied > 0 {
        format!(
            "{} copied in place, {} streamed, {}",
            copied,
            ui::plural(streamed, "object"),
            ui::size(moved_bytes)
        )
    } else {
        format!(
            "{}, {}",
            ui::plural(streamed, "object"),
            ui::size(moved_bytes)
        )
    };
    ui::action("transferred", Style::Ok, &detail, None);
    Ok(())
}

/// Whether this object is already on the destination.
fn is_moved(migration: &Migration, object: &Needed) -> bool {
    if migration.args.rehash {
        return migration.journal.rehashed(&object.md5).is_some();
    }
    // The MD5 is the destination object's own hash, so this is the same
    // question `stored` answers everywhere else.
    migration.journal.is_stored(&object.md5)
}

/// Name the object a transfer failure happened on.
///
/// A bare "permission denied" from halfway through a migration of ten thousand
/// objects says nothing about what to fix.
fn moving(error: impl std::fmt::Display, md5: &str) -> Failure {
    Failure::provider(format!("{error}\n  while moving DVC object {md5}"))
}

/// Stream one object through this machine, re-addressing it as it passes.
fn stream_rehashed(
    migration: &Migration,
    key: &str,
    object: &Needed,
    progress: &Progress,
) -> Result<String, Failure> {
    // The SHA-256 is only known once the last byte has been read, and the
    // upload needs a name before it can start, so the bytes land in a
    // temporary file rather than being held in memory.
    let temporary = migration
        .args
        .into
        .join(".avc/cache")
        .join(format!("migrate-{}", std::process::id()));
    let result = (|| -> Result<avc_core::HashResult, Failure> {
        let mut body = migration.source.get_key(key)?;
        let mut file = File::create(&temporary).map_err(io_error)?;
        let hash = avc_core::hash_copy(
            &mut progress.meter(&mut *body),
            &mut file,
            avc_core::Algorithm::Sha256,
        )?;
        if hash.size != object.size {
            return Err(format!(
                "object {} is {} bytes on the remote but the listing said {}",
                object.md5, hash.size, object.size
            )
            .into());
        }
        let mut stored = File::open(&temporary).map_err(io_error)?;
        migration.target.put(&hash.object, hash.size, &mut stored)?;
        Ok(hash)
    })();
    let _ = fs::remove_file(&temporary);
    let hash = result?;
    progress.object_done();
    Ok(hash.object.hash().to_owned())
}

/// Phase four: replace every DVC directory manifest with an AVC one.
///
/// DVC's manifest is a JSON list of hashes and paths with no sizes in it; AVC's
/// records a size per file, because `avc list` and the progress of a pull are
/// both answerable without downloading anything only if the sizes are in the
/// manifest. So this is a new object rather than a copied one — a small one,
/// a hundred bytes or so per file it names.
fn manifests(migration: &mut Migration) -> Result<(), Failure> {
    if migration.journal.is_complete(Phase::Manifests) {
        ui::action("manifests", Style::Dim, "already built", None);
        return Ok(());
    }
    let directories: Vec<Needed> = migration
        .journal
        .load_survey()?
        .into_iter()
        .filter(|object| object.directory)
        .collect();
    let progress = Progress::start("building manifests", directories.len(), 0);

    let mut built = 0;
    for directory in &directories {
        if migration.journal.manifest(&directory.md5).is_some() {
            progress.object_done();
            continue;
        }
        progress.item(&directory.md5);
        let tree = build_tree(migration, &directory.md5)?;
        let bytes = tree.serialize_canonical()?.into_bytes();
        let manifest = avc_core::hash_reader(&mut bytes.as_slice(), migration.algorithm())?;
        // Uploaded after the objects it names, never before: a manifest
        // visible on a remote must never name bytes that are not there.
        if !migration.journal.is_stored(manifest.object.hash()) {
            migration
                .target
                .put(&manifest.object, manifest.size, &mut bytes.as_slice())?;
            migration.journal.mark_stored(manifest.object.hash())?;
        }
        migration
            .journal
            .mark_manifest(&directory.md5, manifest.object.hash(), manifest.size)?;
        built += 1;
        progress.object_done();
    }
    progress.finish();
    migration.journal.complete(Phase::Manifests)?;
    ui::action(
        "manifests",
        Style::Ok,
        &ui::plural(built, "directory manifest"),
        Some("rewritten in AVC's format"),
    );
    Ok(())
}

/// Turn one DVC directory manifest into an AVC one.
fn build_tree(migration: &Migration, md5: &str) -> Result<avc_core::Tree, Failure> {
    let entries = dvc::parse_dir_manifest(&migration.journal.load_dir_manifest(md5)?)?;
    let mut converted = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(size) = migration.inventory.get(&entry.md5).copied() else {
            // Surveyed as absent, and reported then. A manifest naming an
            // object that is not there would produce a directory that can
            // never be checked out, so the file is left out of it instead.
            continue;
        };
        converted.push(avc_core::TreeEntry::new(
            &entry.relpath,
            migration.object_for(&entry.md5)?,
            size,
        )?);
    }
    Ok(avc_core::Tree::new(converted)?)
}

/// Phase five: rewrite every commit.
fn replay(migration: &mut Migration) -> Result<usize, Failure> {
    let commits = history::commits_in_order(&migration.git)?;
    if migration.journal.is_complete(Phase::Replay) {
        ui::action("replay", Style::Dim, "already done", None);
        return Ok(commits.len());
    }
    let index = migration.args.into.join(".avc/state/migrate/index");
    let progress = Progress::start("rewriting", commits.len(), 0);
    let mut blobs: HashMap<String, dvc::Parsed> = HashMap::new();

    for original in &commits {
        if migration.journal.commit(original).is_some() {
            progress.object_done();
            continue;
        }
        progress.item(&original[..12.min(original.len())]);
        let commit = history::read_commit(&migration.git, original)?;
        let changes = rewrite_tree(migration, original, &mut blobs)?;
        let tree = history::build_tree(&migration.git, &index, original, &changes)?;
        let parents = commit
            .parents
            .iter()
            .map(|parent| {
                migration.journal.commit(parent).cloned().ok_or_else(|| {
                    Failure::from(format!(
                        "commit {parent} should have been rewritten before its child {original}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let rewritten = history::commit_tree(&migration.git, &tree, &parents, &commit)?;
        migration.journal.mark_commit(original, &rewritten)?;
        progress.object_done();
    }
    progress.finish();
    let _ = fs::remove_file(&index);
    migration.journal.complete(Phase::Replay)?;
    ui::action(
        "rewrote",
        Style::Ok,
        &ui::plural(commits.len(), "commit"),
        Some("pointers translated, history preserved"),
    );
    Ok(commits.len())
}

/// The edits that turn one DVC commit into an AVC one.
fn rewrite_tree(
    migration: &mut Migration,
    commit: &str,
    blobs: &mut HashMap<String, dvc::Parsed>,
) -> Result<Vec<Change>, Failure> {
    let files = history::list_tree(&migration.git, commit)?;
    let mut changes = Vec::new();
    let mut ignore = Vec::new();
    let mut existing_ignore = None;

    for file in &files {
        // A symlinked `.gitignore` is legal and rare; reading one as a blob
        // would yield its target path, and folding AVC's rules into *that*
        // would produce nonsense. Left alone, and AVC's own entries replace it.
        if file.path == ".gitignore" && matches!(file.mode.as_str(), "100644" | "100755") {
            existing_ignore = Some(file.id.clone());
        }
        // DVC's own configuration directory has no counterpart and no meaning
        // once the pointers are gone.
        let dvc_internal = file.path == ".dvc" || file.path.starts_with(".dvc/");
        let dvc_only = DVC_ONLY_FILES.contains(&file.path.as_str())
            || DVC_ONLY_FILES
                .iter()
                .any(|name| file.path.ends_with(&format!("/{name}")));
        let Some(kind) = classify(&file.path) else {
            if (dvc_internal || dvc_only) && !migration.args.keep_dvc_files {
                changes.push(Change::Remove {
                    path: file.path.clone(),
                });
            }
            continue;
        };
        if !file.is_blob() {
            continue;
        }
        if !blobs.contains_key(&file.id) {
            let bytes = history::read_blob(&migration.git, &file.id)?;
            let text = String::from_utf8_lossy(&bytes);
            let directory = parent_of(&file.path);
            let parsed = match kind {
                DvcFile::Pointer => dvc::parse_dvc_file(&text, directory)?,
                DvcFile::Lock => dvc::parse_dvc_lock(&text, directory)?,
            };
            blobs.insert(file.id.clone(), parsed);
        }
        let mut all_translated = true;
        for out in blobs[&file.id].outs.clone() {
            let Some(pointer) = pointer_for(migration, &out)? else {
                // Its object never reached the destination — garbage-collected
                // from the DVC remote, most likely. Reported by the survey.
                all_translated = false;
                continue;
            };
            let blob =
                history::write_blob(&migration.git, pointer.serialize_canonical()?.as_bytes())?;
            changes.push(Change::Add {
                path: format!("{}.avc", out.path),
                // A pointer is an ordinary text file, never executable.
                mode: "100644".to_owned(),
                id: blob,
            });
            ignore.push(if out.directory {
                format!("{}/", out.path)
            } else {
                out.path.clone()
            });
        }
        // Both kinds are superseded by the `.avc` pointers just written: a
        // `.dvc` file entirely, and a `dvc.lock` for the tracking half of what
        // it recorded. `dvc.yaml` survives either way, so a pipeline
        // definition is never silently deleted.
        //
        // A file with an out that could not be translated is kept instead. It
        // is the only surviving record of what that artifact was, and deleting
        // it would turn a reported gap into a silent one.
        if !migration.args.keep_dvc_files && all_translated {
            changes.push(Change::Remove {
                path: file.path.clone(),
            });
        }
    }

    // The repository configuration is committed at every revision, because a
    // consumer reading pointers at an old tag has to be able to find the
    // object store from that same revision.
    let config = crate::render_remote_config(&migration.target_config)?;
    changes.push(Change::Add {
        path: ".avc/config.toml".to_owned(),
        mode: "100644".to_owned(),
        id: history::write_blob(&migration.git, config.as_bytes())?,
    });

    let previous = match existing_ignore {
        Some(id) => history::read_blob(&migration.git, &id)?,
        None => Vec::new(),
    };
    let merged = merge_ignore(&String::from_utf8_lossy(&previous), &ignore);
    changes.push(Change::Add {
        path: ".gitignore".to_owned(),
        mode: "100644".to_owned(),
        id: history::write_blob(&migration.git, merged.as_bytes())?,
    });
    Ok(changes)
}

/// The pointer one DVC out becomes, or nothing when its object never arrived.
fn pointer_for(
    migration: &Migration,
    out: &dvc::Out,
) -> Result<Option<avc_core::Pointer>, Failure> {
    if out.directory {
        // The manifest AVC built for this directory, which is what its pointer
        // names — not the DVC manifest, which is not on the destination at all.
        let Some(manifest) = migration.journal.manifest(&out.md5) else {
            return Ok(None);
        };
        let object = avc_core::ObjectId::new(migration.algorithm(), manifest.hash.clone())?;
        return Ok(Some(avc_core::Pointer::new_directory(
            &out.path,
            object,
            manifest.size,
        )?));
    }
    let Some(size) = migration.inventory.get(&out.md5).copied() else {
        return Ok(None);
    };
    let object = migration.object_for(&out.md5)?;
    Ok(Some(avc_core::Pointer::new(&out.path, object, size, None)?))
}

/// Fold AVC's ignore rules and this commit's artifacts into an existing file.
///
/// DVC has already ignored the artifact paths, usually in a `.gitignore` beside
/// each one, and those entries stay correct. What is added is what AVC needs
/// and DVC never wrote, without disturbing a line that is already there.
fn merge_ignore(existing: &str, artifacts: &[String]) -> String {
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();
    let mut present: HashSet<String> = lines.iter().cloned().collect();
    let mut added: Vec<String> = Vec::new();
    for entry in [".avc/cache/", ".avc/config.local.toml", ".avc/state/"]
        .iter()
        .map(|value| value.to_string())
        .chain(artifacts.iter().cloned())
    {
        // DVC anchors its entries to the directory the file sits in, so the
        // rule it wrote for a root-level artifact is `/model.bin` where AVC
        // would write `model.bin`. They ignore the same path, and adding the
        // second form would leave a migrated `.gitignore` saying everything
        // twice.
        let anchored = format!("/{}", entry.trim_end_matches('/'));
        if present.contains(&entry) || present.contains(&anchored) {
            continue;
        }
        present.insert(entry.clone());
        added.push(entry);
    }
    lines.extend(added);
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Phase six: name the rewritten commits.
fn publish(migration: &mut Migration) -> Result<Vec<(String, String)>, Failure> {
    let (heads, tags) = history::source_refs(&migration.git)?;
    let mut published = Vec::new();

    for (kind, references) in [("heads", heads), ("tags", tags)] {
        for reference in references {
            // A tag may point at a tag object rather than at a commit; what was
            // rewritten is the commit underneath it.
            let target = migration
                .git
                .run(&[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", reference.id),
                ])
                .map(|value| value.trim().to_owned())
                .ok();
            let Some(rewritten) = target.and_then(|id| migration.journal.commit(&id).cloned())
            else {
                continue;
            };
            let name = migration.rename(&reference.name);
            history::set_ref(&migration.git, &format!("refs/{kind}/{name}"), &rewritten)?;
            published.push((format!("{kind}/{name}"), reference.name));
        }
    }
    migration.journal.complete(Phase::Refs)?;
    Ok(published)
}

impl Migration {
    /// What a source ref is called in the destination.
    ///
    /// A brand-new repository is the DVC project, so its branches keep their
    /// names. A repository that already has history is somebody else's, and the
    /// migrated refs are prefixed so that nothing already in it is touched or
    /// shadowed.
    fn rename(&self, name: &str) -> String {
        if self.existing {
            format!("{}{name}", self.args.branch_prefix)
        } else {
            name.to_owned()
        }
    }
}

/// Point the new repository at its default branch and report what happened.
fn finish(
    migration: Migration,
    commits: usize,
    published: Vec<(String, String)>,
) -> Result<(), Failure> {
    let default = history::source_default_branch(&migration.git, &migration.args.from_repo)
        .map(|name| migration.rename(&name));

    if !migration.existing {
        if let Some(branch) = &default {
            // A fresh repository has an unborn HEAD pointing at whatever this
            // machine's `init.defaultBranch` says; the migrated project's own
            // default is the honest thing for it to point at.
            migration
                .git
                .run(&["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])?;
            migration
                .git
                .run(&["checkout", "--quiet", "--force", branch])?;
        }
    }
    history::clear_source_refs(&migration.git)?;

    println!();
    let branches = published
        .iter()
        .filter(|(name, _)| name.starts_with("heads/"));
    let mut table = crate::ui::Table::new(vec![
        crate::ui::Column::left("DVC"),
        crate::ui::Column::left("AVC"),
    ]);
    for (name, original) in branches {
        table.row(vec![
            crate::ui::Cell::dim(original.clone()),
            crate::ui::Cell::new(name.trim_start_matches("heads/").to_owned(), Style::Ok),
        ]);
    }
    table.print();

    if !migration.skipped.is_empty() {
        println!();
        ui::line("not migrated:", Style::Warn);
        for reason in migration.skipped.keys() {
            println!("  {reason}");
        }
    }

    let tags = published
        .iter()
        .filter(|(name, _)| name.starts_with("tags/"))
        .count();
    let branches = published.len() - tags;
    ui::summary(&format!(
        "migrated {}, {branches} {}, and {}",
        ui::plural(commits, "commit"),
        if branches == 1 { "branch" } else { "branches" },
        ui::plural(tags, "tag")
    ));
    if migration.existing {
        if let Some(branch) = &default {
            ui::note(&format!(
                "the migrated history is on `{branch}`; nothing already in this repository was changed"
            ));
        }
    } else {
        ui::note(
            "next: `avc status` to see what came across, then push this repository to a Git remote",
        );
    }
    // Only once everything has been named: until the refs exist, the journal is
    // the only record that the work was done.
    migration.journal.discard()
}
