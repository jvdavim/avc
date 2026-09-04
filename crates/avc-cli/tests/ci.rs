//! End-to-end coverage of the commands built for CI/CD.
//!
//! The fixture below is an *artifact registry*: one Git repository holding
//! artifacts for two unrelated projects plus a shared dataset, with its object
//! store configured once in the tracked `.avc/config.toml`. That is the shape
//! these commands exist for, and what the tests assert is that a consumer can
//! name a repository and a path inside it — never a bucket, never everything —
//! from a directory that is not a checkout of anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that removes itself, so a failing test leaves no litter.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-ci-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn avc(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(arguments)
        // A pipeline's log is not a terminal, but a runner may still export
        // these; pinning them keeps the assertions about text, not escapes.
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("AVC_REPO")
        .env_remove("AVC_REF")
        .env_remove("AVC_CACHE_DIR")
        .current_dir(directory)
        .output()
        .expect("the avc binary should run")
}

fn run(directory: &Path, arguments: &[&str]) -> String {
    let output = avc(directory, arguments);
    assert!(
        output.status.success(),
        "avc {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("these tests need the git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("these tests need the git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Commit everything, with an identity that does not depend on the machine.
fn commit(directory: &Path, message: &str) {
    git(directory, &["add", "--all"]);
    git(
        directory,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A `file://` URL for a local path, spelled so it parses on Windows too.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A published artifact registry: a real Git repository whose commits hold the
/// pointers and the configuration, and an object store holding the bytes.
struct Registry {
    directory: TempDir,
}

impl Registry {
    fn new(label: &str) -> Self {
        let directory = TempDir::new(label);
        let source = directory.0.join("registry");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(directory.0.join("store")).unwrap();

        git(&source, &["init", "--quiet", "-b", "main"]);
        write(&source.join("models/bert/weights.bin"), "bert weights\n");
        write(&source.join("models/bert/tokenizer.json"), "{}\n");
        write(&source.join("models/gpt/weights.bin"), "gpt weights\n");
        write(&source.join("data/a.bin"), "alpha\n");
        write(&source.join("data/nested/b.bin"), "beta\n");
        // Identical content twice: one object must serve both paths, even with
        // no cache to deduplicate against.
        write(&source.join("data/nested/dup.bin"), "alpha\n");

        run(&source, &["init"]);
        run(
            &source,
            &[
                "add",
                "models/bert/weights.bin",
                "models/bert/tokenizer.json",
                "models/gpt/weights.bin",
                "data",
            ],
        );
        // The object store is set up once, here, and committed. No consumer in
        // any test below ever names it.
        run(
            &source,
            &[
                "remote",
                "add",
                "origin",
                &file_url(&directory.0.join("store")),
            ],
        );
        run(&source, &["push"]);
        commit(&source, "Publish artifacts");
        Self { directory }
    }

    fn source(&self) -> PathBuf {
        self.directory.0.join("registry")
    }

    /// The Git URL a consumer is given. This is the only address anyone needs.
    fn url(&self) -> String {
        file_url(&self.source())
    }

    /// A consumer's working directory: empty, not a checkout of anything.
    fn job(&self, name: &str) -> PathBuf {
        let path = self.directory.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn store(&self) -> PathBuf {
        self.directory.0.join("store")
    }
}

/// The headline case: name a repository and one path inside it, from a
/// directory that is not a checkout, and get exactly that path's bytes — under
/// the name that was asked for, in the directory that was asked for.
#[test]
fn fetches_one_path_out_of_a_shared_registry() {
    let registry = Registry::new("path");
    let job = registry.job("job");

    let output = run(
        &job,
        &[
            "fetch",
            "--repo",
            &registry.url(),
            "models/bert",
            "-o",
            "out",
        ],
    );
    assert!(output.contains("downloaded"), "{output}");

    // `models/` is how the publisher files its artifacts, not part of what was
    // asked for, so it is not recreated in the consumer's workspace.
    assert_eq!(
        fs::read_to_string(job.join("out/bert/weights.bin")).unwrap(),
        "bert weights\n"
    );
    assert_eq!(
        fs::read_to_string(job.join("out/bert/tokenizer.json")).unwrap(),
        "{}\n"
    );
    assert!(
        !job.join("out/models").exists(),
        "the path walked to reach the artifact must not be recreated"
    );
    // The rest of the registry was not fetched, which is the point.
    assert!(!job.join("out/gpt").exists(), "another project's model");
    assert!(!job.join("out/data").exists(), "an unrelated dataset");
    assert_eq!(walk_files(&job.join("out")).len(), 2);

    // Nothing but the artifacts: no checkout of the repository, no cache.
    assert!(!job.join(".git").exists());
    assert!(!job.join(".avc").exists());
    assert!(!job.join("out/.avc").exists());
}

/// A tracked directory is how the publisher grouped its files, not a limit on
/// what a consumer may ask for. Every file inside one is an object of its own,
/// so one of them can be fetched alone.
#[test]
fn fetches_one_file_out_of_a_tracked_directory() {
    let registry = Registry::new("inside");
    let job = registry.job("job");
    let url = registry.url();

    let output = run(
        &job,
        &[
            "fetch",
            "--repo",
            &url,
            "data/nested/b.bin",
            "-o",
            ".",
            "--porcelain",
        ],
    );
    assert_eq!(output, "downloaded\t1\t5\tdata/nested/b.bin\n");
    assert_eq!(fs::read_to_string(job.join("b.bin")).unwrap(), "beta\n");
    // One file, not the directory it lives in and not the path to it.
    assert_eq!(walk_files(&job).len(), 1);

    // A subdirectory of a tracked directory takes everything beneath it, and
    // arrives keeping its own shape.
    let sub = registry.job("sub");
    run(&sub, &["fetch", "--repo", &url, "data/nested", "-o", "."]);
    assert_eq!(
        fs::read_to_string(sub.join("nested/b.bin")).unwrap(),
        "beta\n"
    );
    assert_eq!(
        fs::read_to_string(sub.join("nested/dup.bin")).unwrap(),
        "alpha\n"
    );
    assert!(!sub.join("a.bin").exists(), "a sibling outside the subtree");
    assert!(!sub.join("data").exists());

    // A path the directory does not contain is a typo, answered with what is.
    let missing = avc(
        &job,
        &["fetch", "--repo", &url, "data/nested/absent.bin", "-o", "."],
    );
    assert_eq!(missing.status.code(), Some(1));
    let message = String::from_utf8_lossy(&missing.stderr);
    assert!(message.contains("data/nested/absent.bin"), "{message}");
    assert!(message.contains("avc list data"), "{message}");
}

/// Dropping the path walked to an artifact is what makes `-o` predictable, and
/// also the one way this command can be asked for something incoherent.
#[test]
fn refuses_to_write_two_artifacts_to_one_path() {
    let registry = Registry::new("collide");
    let job = registry.job("job");

    // Both are named `weights.bin`; both would become `./weights.bin`.
    let collision = avc(
        &job,
        &[
            "fetch",
            "--repo",
            &registry.url(),
            "models/bert/weights.bin",
            "models/gpt/weights.bin",
            "-o",
            ".",
        ],
    );
    assert_eq!(collision.status.code(), Some(1));
    let message = String::from_utf8_lossy(&collision.stderr);
    assert!(message.contains("would both be written to"), "{message}");
    assert!(message.contains("models/bert/weights.bin"), "{message}");
    assert!(message.contains("models/gpt/weights.bin"), "{message}");
    // Refused before anything was written, not halfway through.
    assert!(walk_files(&job).is_empty(), "a refused run writes nothing");

    // Naming the parent they share keeps them apart, and works.
    run(
        &job,
        &["fetch", "--repo", &registry.url(), "models", "-o", "."],
    );
    assert_eq!(
        fs::read_to_string(job.join("models/bert/weights.bin")).unwrap(),
        "bert weights\n"
    );
    assert_eq!(
        fs::read_to_string(job.join("models/gpt/weights.bin")).unwrap(),
        "gpt weights\n"
    );
}

/// A consumer never names the bucket. The repository does, once.
#[test]
fn takes_the_object_store_from_the_repository() {
    let registry = Registry::new("store");
    let job = registry.job("job");
    run(
        &job,
        &["fetch", "--repo", &registry.url(), "data", "-o", "out"],
    );
    assert_eq!(
        fs::read_to_string(job.join("out/data/nested/b.bin")).unwrap(),
        "beta\n"
    );

    // A repository with no `avc remote add` cannot serve a fetch, and says so
    // rather than asking the caller to guess a URL.
    let bare = registry.directory.0.join("bare");
    fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--quiet", "-b", "main"]);
    let pointer = fs::read_to_string(registry.source().join("models/gpt/weights.bin.avc")).unwrap();
    write(&bare.join("model.bin.avc"), &pointer);
    commit(&bare, "No AVC configuration");

    let unconfigured = avc(&job, &["fetch", "--repo", &file_url(&bare), "-o", "out2"]);
    assert_eq!(unconfigured.status.code(), Some(1));
    let message = String::from_utf8_lossy(&unconfigured.stderr);
    assert!(message.contains("configures no object store"), "{message}");
    assert!(message.contains("avc remote add"), "{message}");
}

/// Listing is browsing: a prefix shows the artifacts under it, and a tracked
/// directory shows the files inside it.
#[test]
fn lists_what_is_stored_at_a_path() {
    let registry = Registry::new("list");
    let job = registry.job("job");
    let url = registry.url();
    let names = |output: &str| -> Vec<String> {
        output
            .lines()
            .map(|line| line.split('\t').next().unwrap().to_owned())
            .collect()
    };

    // The whole registry: four artifacts, the directory collapsed to one row.
    let all = run(&job, &["list", "--repo", &url, "--porcelain"]);
    assert_eq!(
        names(&all),
        [
            "data/",
            "models/bert/tokenizer.json",
            "models/bert/weights.bin",
            "models/gpt/weights.bin"
        ]
    );

    // One project's corner of it.
    let scoped = run(
        &job,
        &["list", "--repo", &url, "models/bert", "--porcelain"],
    );
    assert_eq!(
        names(&scoped),
        ["models/bert/tokenizer.json", "models/bert/weights.bin"]
    );

    // Naming the directory artifact exactly looks inside it, listing the files
    // stored there rather than the one artifact they add up to.
    let inside = run(&job, &["list", "--repo", &url, "data", "--porcelain"]);
    assert_eq!(
        names(&inside),
        ["data/a.bin", "data/nested/b.bin", "data/nested/dup.bin"]
    );
    assert!(inside.contains("data/a.bin\t6\t"), "{inside}");
    assert!(
        inside.lines().all(|line| line.ends_with("available")),
        "{inside}"
    );

    // And the human output counts files rather than artifacts.
    let human = run(&job, &["list", "--repo", &url, "data"]);
    assert!(human.contains("3 files, 17 B"), "{human}");
}

/// `verify` checks a directory against a particular commit of the registry,
/// contacting no object store at all.
#[test]
fn verifies_a_directory_against_a_repository_reference() {
    let registry = Registry::new("verify");
    let job = registry.job("job");
    let url = registry.url();
    run(&job, &["fetch", "--repo", &url, "models", "-o", "out"]);

    assert_eq!(
        run(
            &job,
            &[
                "verify",
                "--repo",
                &url,
                "models",
                "-o",
                "out",
                "--porcelain"
            ]
        ),
        "ok\t3\tmodels/bert/tokenizer.json\n\
         ok\t13\tmodels/bert/weights.bin\n\
         ok\t12\tmodels/gpt/weights.bin\n"
    );

    // Deleting the object store proves no transfer is involved.
    fs::remove_dir_all(registry.store()).unwrap();
    write(&job.join("out/models/gpt/weights.bin"), "tampered\n");
    let failed = avc(
        &job,
        &[
            "verify",
            "--repo",
            &url,
            "models",
            "-o",
            "out",
            "--porcelain",
        ],
    );
    assert_eq!(failed.status.code(), Some(1));
    let report = String::from_utf8_lossy(&failed.stdout);
    assert!(
        report.contains("modified\t9\tmodels/gpt/weights.bin"),
        "{report}"
    );
}

/// A dry run reports the same plan the real run executes, and writes nothing.
#[test]
fn dry_run_reports_the_transfer_without_making_it() {
    let registry = Registry::new("dry");
    let job = registry.job("job");
    let arguments = [
        "fetch",
        "--repo",
        &registry.url(),
        "data",
        "-o",
        "out",
        "--porcelain",
    ];

    let mut dry = arguments.to_vec();
    dry.push("--dry-run");
    let planned = run(&job, &dry);
    assert_eq!(
        planned, "would-fetch\t2\t11\tdata/\n",
        "duplicate files inside a directory must be counted once"
    );
    assert!(!job.join("out").exists(), "a dry run must write nothing");

    let performed = run(&job, &arguments);
    assert_eq!(
        performed,
        planned.replace("would-fetch", "downloaded"),
        "the real run must move exactly what the dry run predicted"
    );
}

/// The safety rule the rest of AVC follows applies here too, and `--force` is
/// the way a reused runner workspace opts out of it.
#[test]
fn refuses_to_overwrite_a_file_that_differs() {
    let registry = Registry::new("force");
    let job = registry.job("job");
    let fetch = ["fetch", "--repo", &registry.url(), "data", "-o", "out"];
    run(&job, &fetch);

    write(&job.join("out/data/a.bin"), "a local edit\n");
    let refused = avc(&job, &fetch);
    assert_eq!(refused.status.code(), Some(1));
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.contains("data/a.bin"), "{message}");
    assert!(message.contains("--force"), "{message}");
    assert_eq!(
        fs::read_to_string(job.join("out/data/a.bin")).unwrap(),
        "a local edit\n"
    );

    let mut forced = fetch.to_vec();
    forced.push("--force");
    run(&job, &forced);
    assert_eq!(
        fs::read_to_string(job.join("out/data/a.bin")).unwrap(),
        "alpha\n"
    );
}

/// A cache is worth having only if the second job can be served entirely from
/// it. Deleting the object store in between proves nothing went over the wire.
#[test]
fn a_warm_cache_serves_a_fetch_with_the_store_gone() {
    let registry = Registry::new("cache");
    let job = registry.job("job");
    let arguments = [
        "fetch",
        "--repo",
        &registry.url(),
        "-o",
        "out",
        "--cache",
        "cache",
    ];
    run(&job, &arguments);

    fs::remove_dir_all(registry.store()).unwrap();
    fs::remove_dir_all(job.join("out")).unwrap();

    let output = run(&job, &arguments);
    assert!(output.contains("from-cache"), "{output}");
    assert_eq!(
        fs::read_to_string(job.join("out/data/nested/b.bin")).unwrap(),
        "beta\n"
    );
    run(&job, &["verify", "--repo", &registry.url(), "-o", "out"]);
}

/// Path selection, and the errors that keep a typo from passing as an empty
/// selection.
#[test]
fn selects_the_paths_a_pipeline_names() {
    let registry = Registry::new("select");
    let job = registry.job("job");
    let url = registry.url();

    // A pointer path names its artifact, so `git diff --name-only` output can
    // be piped in unchanged.
    let one = run(
        &job,
        &[
            "fetch",
            "--repo",
            &url,
            "models/gpt/weights.bin.avc",
            "-o",
            "out",
            "--porcelain",
        ],
    );
    assert_eq!(one, "downloaded\t1\t12\tmodels/gpt/weights.bin\n");
    // Reported by the path that names it in the repository, written under the
    // name that was asked for.
    assert_eq!(
        fs::read_to_string(job.join("out/weights.bin")).unwrap(),
        "gpt weights\n"
    );
    assert!(!job.join("out/models").exists());

    // A path that names nothing is a typo, not an empty selection.
    let missing = avc(
        &job,
        &["fetch", "--repo", &url, "models/absent", "-o", "out"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("models/absent"));

    // A reference that does not exist is an operational failure, not a data
    // one, so a pipeline can tell "retry this" from "fix your commit".
    let bad_ref = avc(&job, &["fetch", "--repo", &url, "--ref", "no-such-branch"]);
    assert_eq!(bad_ref.status.code(), Some(3));
}

/// Delivering and restoring are two jobs, and naming an output directory is
/// what says which one this is.
///
/// A build agent asking for `models/bert` wants it where it asked, under that
/// name. Someone in a checkout putting an old version back wants it where the
/// pointer says it lives — anywhere else has not restored anything.
#[test]
fn a_checkout_is_restored_in_place_while_an_output_directory_is_delivered_to() {
    let registry = Registry::new("restore");
    let source = registry.source();

    // Restoring: no output directory, so the artifact goes back to its own path.
    fs::remove_file(source.join("models/bert/weights.bin")).unwrap();
    run(&source, &["fetch", "models/bert"]);
    assert_eq!(
        fs::read_to_string(source.join("models/bert/weights.bin")).unwrap(),
        "bert weights\n"
    );

    // Delivering: the same selector, with somewhere to put it.
    run(&source, &["fetch", "models/bert", "-o", "delivered"]);
    assert_eq!(
        fs::read_to_string(source.join("delivered/bert/weights.bin")).unwrap(),
        "bert weights\n"
    );
    assert!(!source.join("delivered/models").exists());

    // `verify` asks the same question, so it looks where `fetch` wrote.
    run(&source, &["verify", "models/bert"]);
    run(&source, &["verify", "models/bert", "-o", "delivered"]);
}

/// Checking part of a tracked directory compares against the manifest, since
/// the pointer only describes the directory as a whole.
#[test]
fn verifies_one_file_out_of_a_tracked_directory() {
    let registry = Registry::new("verify-inside");
    let source = registry.source();

    assert_eq!(
        run(&source, &["verify", "data/nested/b.bin", "--porcelain"]),
        "ok\t5\tdata/nested/b.bin\n"
    );
    assert_eq!(
        run(&source, &["verify", "data/nested", "--porcelain"]),
        "ok\t11\tdata/nested\n"
    );

    // An edit inside the directory is caught for the file that changed, and not
    // blamed on the one that did not.
    write(&source.join("data/nested/b.bin"), "tampered\n");
    let failed = avc(&source, &["verify", "data/nested/b.bin", "--porcelain"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&failed.stdout).contains("modified\t9\tdata/nested/b.bin"),
        "{}",
        String::from_utf8_lossy(&failed.stdout)
    );
    run(&source, &["verify", "data/nested/dup.bin"]);
}

/// The commands that maintain a repository work on whole artifacts, and say so
/// rather than half-doing something.
#[test]
fn repository_commands_refuse_part_of_a_tracked_directory() {
    let registry = Registry::new("partial");
    let source = registry.source();

    for command in ["push", "pull", "checkout"] {
        let refused = avc(&source, &[command, "data/nested/b.bin"]);
        assert_eq!(refused.status.code(), Some(1), "{command}");
        let message = String::from_utf8_lossy(&refused.stderr);
        assert!(message.contains("whole artifacts"), "{command}: {message}");
        assert!(message.contains("avc fetch"), "{command}: {message}");
    }
}

/// The same commands still work in a checkout, where the pointers are already
/// on disk and paths mean what they do everywhere else in AVC.
#[test]
fn works_inside_a_checkout_without_a_repo_url() {
    let registry = Registry::new("local");
    let source = registry.source();

    // The artifacts are still present from publishing, so ask for the state of
    // one project rather than a transfer.
    assert_eq!(
        run(&source, &["verify", "models/bert", "--porcelain"]),
        "ok\t3\tmodels/bert/tokenizer.json\nok\t13\tmodels/bert/weights.bin\n"
    );

    // Prefix selection reaches the repository commands too.
    let listed = run(&source, &["list", "models", "--porcelain"]);
    assert_eq!(listed.lines().count(), 3, "{listed}");
    assert!(run(&source, &["push", "models/bert"]).contains("up-to-date"));

    // Run from a subdirectory, paths still resolve against the repository root
    // rather than the current directory.
    assert_eq!(
        run(
            &source.join("models"),
            &["verify", "models/gpt", "--porcelain"]
        ),
        "ok\t12\tmodels/gpt/weights.bin\n"
    );
}

/// Publish a second version of one artifact, leaving the first tagged
/// `v1.0.0`, and answer the commit the first version lives at.
fn publish_two_versions(source: &Path) -> String {
    git(source, &["tag", "v1.0.0"]);
    let first = git_output(source, &["rev-parse", "HEAD"]);
    write(
        &source.join("models/gpt/weights.bin"),
        "gpt weights, retrained\n",
    );
    run(source, &["commit", "models/gpt/weights.bin"]);
    run(source, &["push"]);
    commit(source, "Retrain gpt");
    first
}

/// A revision is whatever names a commit, and all the spellings of one commit
/// have to reach the same artifacts.
#[test]
fn a_registry_can_be_read_at_any_revision() {
    let registry = Registry::new("revisions");
    let source = registry.source();
    let first = publish_two_versions(&source);
    let job = registry.job("job");

    let at = |revision: &str| {
        run(
            &job,
            &[
                "list",
                "--repo",
                &registry.url(),
                "--ref",
                revision,
                "--porcelain",
            ],
        )
    };

    // A tag, a whole commit id, an abbreviated one — which no server can look
    // up, so it costs a search of the history — and a fully qualified name,
    // for a repository where a branch and a tag share one.
    for revision in ["v1.0.0", &first, &first[..8], "refs/tags/v1.0.0"] {
        let listed = at(revision);
        assert!(
            listed.contains("models/gpt/weights.bin\t12\t"),
            "{revision}: {listed}"
        );
    }
    // A branch, and the default branch by way of `HEAD`, name the newer one.
    for revision in ["main", "HEAD", "refs/heads/main"] {
        let listed = at(revision);
        assert!(
            listed.contains("models/gpt/weights.bin\t23\t"),
            "{revision}: {listed}"
        );
    }

    // Fetching at a revision brings back that version's bytes, not the tip's.
    run(
        &job,
        &[
            "fetch",
            "models/gpt",
            "--repo",
            &registry.url(),
            "--ref",
            "v1.0.0",
            "-o",
            ".",
        ],
    );
    assert_eq!(
        fs::read_to_string(job.join("gpt/weights.bin")).unwrap(),
        "gpt weights\n"
    );
}

/// In a checkout, a revision reads what Git holds rather than what is on disk.
///
/// Accepting `--ref` and ignoring it would make `avc verify --ref` a gate that
/// passes whatever the worktree contains, which is worse than not offering one.
#[test]
fn a_revision_in_a_checkout_reads_git_rather_than_the_working_tree() {
    let registry = Registry::new("revision-local");
    let source = registry.source();
    publish_two_versions(&source);

    // What is on disk is the retrained model, and that is what a command with
    // no revision sees — including a pointer written but not yet committed.
    let disk = run(&source, &["list", "--porcelain"]);
    assert!(disk.contains("models/gpt/weights.bin\t23\t"), "{disk}");
    let tagged = run(&source, &["list", "--ref", "v1.0.0", "--porcelain"]);
    assert!(tagged.contains("models/gpt/weights.bin\t12\t"), "{tagged}");

    // The worktree matches the commit it was built from, and does not match the
    // older tag — which is the whole point of being able to name one.
    run(&source, &["verify"]);
    let against_tag = avc(&source, &["verify", "--ref", "v1.0.0"]);
    assert_eq!(against_tag.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&against_tag.stdout).contains("models/gpt/weights.bin"),
        "{}",
        String::from_utf8_lossy(&against_tag.stdout)
    );

    // Artifacts still belong to the worktree rather than to the temporary
    // checkout the pointers were read out of, so restoring a version puts it
    // back where it was — and refuses to overwrite until told to.
    let refused = avc(&source, &["fetch", "models/gpt", "--ref", "v1.0.0"]);
    assert_eq!(refused.status.code(), Some(1));
    run(
        &source,
        &["fetch", "models/gpt", "--ref", "v1.0.0", "--force"],
    );
    assert_eq!(
        fs::read_to_string(source.join("models/gpt/weights.bin")).unwrap(),
        "gpt weights\n"
    );
}

#[test]
fn a_revision_that_names_nothing_says_so_plainly() {
    let registry = Registry::new("revision-missing");
    let job = registry.job("job");

    let missing = avc(
        &job,
        &[
            "fetch",
            "--repo",
            &registry.url(),
            "--ref",
            "no-such-thing",
            "-o",
            ".",
        ],
    );
    // A provider failure, which `SPEC.md` reserves exit code 3 for.
    assert_eq!(missing.status.code(), Some(3));
    let message = String::from_utf8_lossy(&missing.stderr);
    assert!(
        message.contains("no branch, tag, or commit named `no-such-thing`"),
        "{message}"
    );

    // A name that could have been a commit id is answered in those terms,
    // because that is the search that actually failed.
    let hex = avc(
        &job,
        &[
            "fetch",
            "--repo",
            &registry.url(),
            "--ref",
            "deadbeef",
            "-o",
            ".",
        ],
    );
    let message = String::from_utf8_lossy(&hex.stderr);
    assert!(message.contains("no commit in"), "{message}");
}

/// Every regular file beneath `root`.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}
