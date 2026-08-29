//! End-to-end coverage of transfer progress.
//!
//! The properties worth pinning down are about where progress goes rather than
//! what it says. A bar belongs on stderr and nowhere else, so a script reading
//! `avc push` gets the same bytes whether or not a person was watching. A build
//! agent must never be handed an animation to store, however its runner has set
//! the environment up. And `--porcelain` is a contract that a progress line
//! would corrupt.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that removes itself, so a failing test leaves no litter.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-cli-{label}-{}-{:?}",
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

/// Run `avc` with a fixed environment.
///
/// Every variable the progress decision consults is cleared, because the suite
/// itself usually runs under `CI` and a test that inherited it would assert the
/// opposite of what it says.
fn avc(worktree: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_avc"));
    command.args(arguments).current_dir(worktree);
    for name in [
        "CI",
        "CONTINUOUS_INTEGRATION",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "JENKINS_URL",
        "TEAMCITY_VERSION",
        "TF_BUILD",
        "AVC_PROGRESS",
    ] {
        command.env_remove(name);
    }
    command.output().expect("the avc binary should run")
}

fn run(worktree: &Path, arguments: &[&str]) -> Output {
    let output = avc(worktree, arguments);
    assert!(
        output.status.success(),
        "avc {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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

/// A repository with one directory artifact and a `file://` remote to push it
/// to. Only the `.git` entry matters; no Git binary is required.
fn repository(directory: &Path) -> PathBuf {
    let worktree = directory.join("worktree");
    fs::create_dir_all(worktree.join(".git")).unwrap();
    run(&worktree, &["init"]);
    let remote = directory.join("remote");
    fs::create_dir_all(&remote).unwrap();
    run(&worktree, &["remote", "add", "origin", &file_url(&remote)]);
    for (path, contents) in [("data/a.bin", "alpha\n"), ("data/nested/b.bin", "beta\n")] {
        let file = worktree.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
    run(&worktree, &["add", "data"]);
    worktree
}

#[test]
fn a_bar_stays_on_stderr_and_leaves_stdout_byte_identical() {
    let directory = TempDir::new("progress-streams");
    let worktree = repository(&directory.0);

    let watched = run(
        &worktree,
        &["--progress", "always", "--color", "never", "push"],
    );
    assert!(stderr(&watched).contains('\r'), "{}", stderr(&watched));
    assert!(!stdout(&watched).contains('\r'), "{}", stdout(&watched));

    // The same push, unwatched. What a script reads must not depend on whether
    // a person was watching the transfer it read from.
    let remote = directory.0.join("remote");
    fs::remove_dir_all(&remote).unwrap();
    fs::create_dir_all(&remote).unwrap();
    let quiet = run(
        &worktree,
        &["--progress", "never", "--color", "never", "push"],
    );
    assert_eq!(stdout(&watched), stdout(&quiet));
    assert!(!stderr(&quiet).contains('\r'), "{}", stderr(&quiet));
}

#[test]
fn a_pipeline_is_never_handed_an_animation() {
    let directory = TempDir::new("progress-ci");
    let worktree = repository(&directory.0);

    let mut command = Command::new(env!("CARGO_BIN_EXE_avc"));
    let logged = command
        .args(["--color", "never", "push"])
        .current_dir(&worktree)
        // Set the way a runner that allocates a pseudo-terminal would: the bar
        // must stay off on the strength of `CI` alone.
        .env("CI", "true")
        .env("AVC_PROGRESS", "auto")
        .output()
        .expect("the avc binary should run");
    assert!(logged.status.success(), "{}", stderr(&logged));
    assert!(!stdout(&logged).contains('\r'), "{}", stdout(&logged));
    assert!(!stderr(&logged).contains('\r'), "{}", stderr(&logged));
}

#[test]
fn porcelain_carries_records_and_nothing_else() {
    let directory = TempDir::new("progress-porcelain");
    let worktree = repository(&directory.0);
    run(&worktree, &["push"]);
    fs::remove_dir_all(worktree.join("data")).unwrap();

    let fetched = run(&worktree, &["--progress", "always", "fetch", "--porcelain"]);
    for line in stdout(&fetched).lines() {
        assert_eq!(line.split('\t').count(), 4, "{line}");
    }
    assert!(!stdout(&fetched).contains('\r'), "{}", stdout(&fetched));
    assert!(!stderr(&fetched).contains('\r'), "{}", stderr(&fetched));
}
