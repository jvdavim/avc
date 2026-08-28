//! End-to-end coverage of the commands built for CI/CD.
//!
//! What makes `fetch` and `verify` worth having is what they *do not* need, so
//! that is what these tests assert: every case below runs in a directory with
//! no `.git`, no `.avc`, and no configuration — nothing but pointer files, the
//! same way a deploy job has nothing but what it was handed.

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
        .env_remove("AVC_REMOTE_URL")
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

/// The three things a pipeline is given: a remote holding the bytes, the
/// pointer files that name them, and an empty directory to work in.
struct Fixture {
    directory: TempDir,
}

impl Fixture {
    /// Track a file and a directory in a throwaway repository, push both to a
    /// `file://` remote, then throw the repository away and keep the pointers.
    fn new(label: &str) -> Self {
        let directory = TempDir::new(label);
        let worktree = directory.0.join("author");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        fs::create_dir_all(directory.0.join("remote")).unwrap();
        write(&worktree.join("model.bin"), "a model\n");
        write(&worktree.join("data/a.bin"), "alpha\n");
        write(&worktree.join("data/nested/b.bin"), "beta\n");
        // Identical content twice: one object must serve both paths, even with
        // no cache to deduplicate against.
        write(&worktree.join("data/nested/dup.bin"), "alpha\n");

        run(&worktree, &["init"]);
        run(&worktree, &["add", "model.bin", "data"]);
        run(
            &worktree,
            &[
                "remote",
                "add",
                "origin",
                &file_url(&directory.0.join("remote")),
            ],
        );
        run(&worktree, &["push"]);

        // The job: pointer files and nothing else.
        let job = directory.0.join("job");
        fs::create_dir_all(&job).unwrap();
        for pointer in ["model.bin.avc", "data.avc"] {
            fs::copy(worktree.join(pointer), job.join(pointer)).unwrap();
        }
        fs::remove_dir_all(&worktree).unwrap();
        Self { directory }
    }

    fn job(&self) -> PathBuf {
        self.directory.0.join("job")
    }

    fn remote_url(&self) -> String {
        file_url(&self.directory.0.join("remote"))
    }
}

/// The whole point: artifacts land, verified, with no repository and nothing
/// written except the artifacts themselves.
#[test]
fn fetches_into_a_directory_that_is_not_a_repository() {
    let fixture = Fixture::new("fetch");
    let job = fixture.job();

    let output = run(
        &job,
        &["fetch", "--remote-url", &fixture.remote_url(), "-o", "out"],
    );
    assert!(output.contains("downloaded"), "{output}");

    for (path, contents) in [
        ("out/model.bin", "a model\n"),
        ("out/data/a.bin", "alpha\n"),
        ("out/data/nested/b.bin", "beta\n"),
        ("out/data/nested/dup.bin", "alpha\n"),
    ] {
        assert_eq!(fs::read_to_string(job.join(path)).unwrap(), contents);
    }

    // No cache, and no manifest left lying in the output tree: a directory
    // artifact materializes as its files and nothing else.
    assert!(!job.join(".avc").exists(), "fetch must not create a cache");
    assert_eq!(
        walk_files(&job.join("out")).len(),
        4,
        "the output tree must hold the artifact files and nothing else"
    );

    // Three files, two distinct objects, plus the manifest — but the manifest
    // is metadata, so only the two file objects and `model.bin` are counted.
    let porcelain = run(
        &job,
        &[
            "fetch",
            "--remote-url",
            &fixture.remote_url(),
            "-o",
            "out",
            "--porcelain",
        ],
    );
    assert_eq!(
        porcelain, "up-to-date\t0\t0\tdata/\nup-to-date\t0\t0\tmodel.bin\n",
        "a second fetch into a populated workspace must transfer nothing"
    );
}

/// A dry run reports the same plan the real run executes, and writes nothing.
#[test]
fn dry_run_reports_the_transfer_without_making_it() {
    let fixture = Fixture::new("dry");
    let job = fixture.job();
    let arguments = [
        "fetch",
        "--remote-url",
        &fixture.remote_url(),
        "-o",
        "out",
        "--porcelain",
    ];

    let mut dry = arguments.to_vec();
    dry.push("--dry-run");
    let planned = run(&job, &dry);
    assert_eq!(
        planned, "would-fetch\t2\t11\tdata/\nwould-fetch\t1\t8\tmodel.bin\n",
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
    let fixture = Fixture::new("force");
    let job = fixture.job();
    let fetch = ["fetch", "--remote-url", &fixture.remote_url(), "-o", "out"];
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
/// it. Deleting the remote in between proves nothing went over the wire.
#[test]
fn a_warm_cache_serves_a_fetch_with_the_remote_gone() {
    let fixture = Fixture::new("cache");
    let job = fixture.job();
    let arguments = [
        "fetch",
        "--remote-url",
        &fixture.remote_url(),
        "-o",
        "out",
        "--cache",
        "cache",
    ];
    run(&job, &arguments);

    fs::remove_dir_all(fixture.directory.0.join("remote")).unwrap();
    fs::remove_dir_all(job.join("out")).unwrap();

    let output = run(&job, &arguments);
    assert!(output.contains("from-cache"), "{output}");
    assert_eq!(
        fs::read_to_string(job.join("out/data/nested/b.bin")).unwrap(),
        "beta\n"
    );
    run(&job, &["verify", "-o", "out"]);
}

/// `verify` is a gate, so what matters is the exit code and that it needs
/// nothing but the pointers and the bytes.
#[test]
fn verify_fails_on_anything_that_does_not_match_its_pointer() {
    let fixture = Fixture::new("verify");
    let job = fixture.job();
    run(
        &job,
        &["fetch", "--remote-url", &fixture.remote_url(), "-o", "out"],
    );

    assert_eq!(
        run(&job, &["verify", "-o", "out", "--porcelain"]),
        "ok\t17\tdata/\nok\t8\tmodel.bin\n"
    );

    // A file added inside a tracked directory changes the directory's identity
    // just as an edited one does.
    write(&job.join("out/data/extra.bin"), "gamma\n");
    fs::remove_file(job.join("out/model.bin")).unwrap();

    let failed = avc(&job, &["verify", "-o", "out", "--porcelain"]);
    assert_eq!(failed.status.code(), Some(1));
    let report = String::from_utf8_lossy(&failed.stdout);
    assert!(report.contains("modified\t23\tdata/"), "{report}");
    assert!(report.contains("missing\t0\tmodel.bin"), "{report}");
}

/// Pointer selection: named files, a directory to scan, and stdin.
#[test]
fn selects_the_pointers_a_pipeline_names() {
    let fixture = Fixture::new("select");
    let job = fixture.job();
    let url = fixture.remote_url();

    let one = run(
        &job,
        &[
            "fetch",
            "model.bin.avc",
            "--remote-url",
            &url,
            "-o",
            "out",
            "--porcelain",
        ],
    );
    assert_eq!(one, "downloaded\t1\t8\tmodel.bin\n");
    assert!(!job.join("out/data").exists(), "only the named pointer");

    // A path that names nothing is a typo, not an empty selection.
    let missing = avc(&job, &["fetch", "absent.avc", "--remote-url", &url]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("absent.avc"));

    // Outside a repository and with no remote named, the error has to say how
    // to name one rather than complain about Git.
    let unconfigured = avc(&job, &["fetch"]);
    assert_eq!(unconfigured.status.code(), Some(1));
    let message = String::from_utf8_lossy(&unconfigured.stderr);
    assert!(message.contains("--remote-url"), "{message}");
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
