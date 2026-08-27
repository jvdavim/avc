//! End-to-end coverage of tracking a directory with one `avc add`.
//!
//! These drive the real binary, because the interesting behaviour of a
//! directory artifact — a manifest object, a round trip through a remote, and
//! a checkout that refuses to clobber — lives in the CLI rather than in
//! `avc-core`.

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

fn avc(worktree: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(arguments)
        .current_dir(worktree)
        .output()
        .expect("the avc binary should run")
}

fn run(worktree: &Path, arguments: &[&str]) -> String {
    let output = avc(worktree, arguments);
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

/// A repository AVC will recognize. Only the `.git` entry matters here; no Git
/// binary is required to exercise AVC itself.
fn repository(directory: &Path) -> PathBuf {
    let worktree = directory.join("worktree");
    fs::create_dir_all(worktree.join(".git")).unwrap();
    run(&worktree, &["init"]);
    worktree
}

fn sample_tree(worktree: &Path) {
    write(&worktree.join("data/a.bin"), "alpha\n");
    write(&worktree.join("data/nested/b.bin"), "beta\n");
    // Identical content twice: one object must serve both paths.
    write(&worktree.join("data/nested/dup.bin"), "alpha\n");
}

#[test]
fn tracks_a_directory_as_one_artifact() {
    let directory = TempDir::new("add");
    let worktree = repository(&directory.0);
    sample_tree(&worktree);

    // The trailing slash a shell completes must name the same artifact.
    let output = run(&worktree, &["add", "data/"]);
    assert!(output.contains("3 file(s)"), "{output}");

    let pointer = fs::read_to_string(worktree.join("data.avc")).unwrap();
    assert!(pointer.contains("path: data\n"), "{pointer}");
    assert!(pointer.contains("kind: directory\n"), "{pointer}");

    // Three files, two distinct objects, plus the manifest that names them.
    let objects = fs::read_dir(worktree.join(".avc/cache/objects/sha256"))
        .unwrap()
        .flat_map(|shard| fs::read_dir(shard.unwrap().path()).unwrap())
        .count();
    assert_eq!(objects, 3, "identical files must share one object");

    assert!(fs::read_to_string(worktree.join(".gitignore"))
        .unwrap()
        .lines()
        .any(|line| line == "data/"));

    assert_eq!(run(&worktree, &["status"]), "ok\tcached\tdata/\n");
}

/// A change anywhere beneath the directory changes the directory's identity.
#[test]
fn reports_any_change_beneath_the_directory() {
    let directory = TempDir::new("status");
    let worktree = repository(&directory.0);
    sample_tree(&worktree);
    run(&worktree, &["add", "data"]);

    write(&worktree.join("data/nested/b.bin"), "changed\n");
    assert!(run(&worktree, &["status"]).starts_with("modified"));

    run(&worktree, &["commit", "data"]);
    assert!(run(&worktree, &["status"]).starts_with("ok"));

    // A new file is a change too, not just an edited one.
    write(&worktree.join("data/c.bin"), "gamma\n");
    assert!(run(&worktree, &["status"]).starts_with("modified"));

    // So is a removed one.
    fs::remove_file(worktree.join("data/c.bin")).unwrap();
    assert!(run(&worktree, &["status"]).starts_with("ok"));

    fs::remove_dir_all(worktree.join("data")).unwrap();
    assert!(run(&worktree, &["status"]).starts_with("missing"));
}

/// The workflow that matters: push, clone, pull, and get the exact tree back.
#[test]
fn survives_a_round_trip_through_a_remote() {
    let directory = TempDir::new("roundtrip");
    let worktree = repository(&directory.0);
    let remote = directory.0.join("remote");
    fs::create_dir_all(&remote).unwrap();
    sample_tree(&worktree);
    run(&worktree, &["add", "data"]);
    run(&worktree, &["remote", "add", "origin", &file_url(&remote)]);
    run(&worktree, &["push"]);
    // Content-addressed objects are immutable, so a second push moves nothing.
    assert!(run(&worktree, &["push"]).contains("pushed 0 object(s)"));

    // A fresh clone has the pointer and the config, and nothing else.
    let clone = directory.0.join("clone");
    fs::create_dir_all(clone.join(".git")).unwrap();
    fs::create_dir_all(clone.join(".avc")).unwrap();
    fs::copy(
        worktree.join(".avc/config.toml"),
        clone.join(".avc/config.toml"),
    )
    .unwrap();
    fs::copy(worktree.join("data.avc"), clone.join("data.avc")).unwrap();

    // `list` learns the directory's size and availability from the manifest on
    // the remote, without downloading any artifact bytes.
    let listed = run(&clone, &["list"]);
    assert!(listed.contains("data/\t17\t"), "{listed}");
    assert!(listed.trim_end().ends_with("available"), "{listed}");

    run(&clone, &["pull"]);
    for (path, contents) in [
        ("data/a.bin", "alpha\n"),
        ("data/nested/b.bin", "beta\n"),
        ("data/nested/dup.bin", "alpha\n"),
    ] {
        assert_eq!(fs::read_to_string(clone.join(path)).unwrap(), contents);
    }
    assert_eq!(run(&clone, &["status"]), "ok\tcached\tdata/\n");
    run(&clone, &["doctor"]);
}

#[test]
fn checkout_refuses_to_discard_an_edit_inside_the_directory() {
    let directory = TempDir::new("checkout");
    let worktree = repository(&directory.0);
    sample_tree(&worktree);
    run(&worktree, &["add", "data"]);

    write(&worktree.join("data/a.bin"), "local edit\n");
    let refused = avc(&worktree, &["checkout"]);
    assert_eq!(refused.status.code(), Some(1));
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.contains("data/a.bin"), "{message}");
    assert_eq!(
        fs::read_to_string(worktree.join("data/a.bin")).unwrap(),
        "local edit\n"
    );

    run(&worktree, &["checkout", "--force"]);
    assert_eq!(
        fs::read_to_string(worktree.join("data/a.bin")).unwrap(),
        "alpha\n"
    );
}

/// `gc` must keep every object a manifest still names, and never guess when it
/// cannot read one.
#[test]
fn gc_keeps_objects_a_manifest_still_names() {
    let directory = TempDir::new("gc");
    let worktree = repository(&directory.0);
    sample_tree(&worktree);
    run(&worktree, &["add", "data"]);
    write(&worktree.join("data/nested/b.bin"), "changed\n");
    run(&worktree, &["commit", "data"]);

    // Only the superseded manifest and the replaced file are unreachable; the
    // two files that survived the edit are still named by the new manifest.
    let planned = run(&worktree, &["gc", "--dry-run"]);
    assert_eq!(planned.lines().count(), 2, "{planned}");
    run(&worktree, &["gc"]);
    assert_eq!(run(&worktree, &["status"]), "ok\tcached\tdata/\n");
    run(&worktree, &["doctor"]);

    // With the manifest gone, reachability is unknowable and deleting is not
    // safe, so `gc` stops instead of collecting.
    let pointer = fs::read_to_string(worktree.join("data.avc")).unwrap();
    let hash = pointer
        .lines()
        .find_map(|line| line.trim().strip_prefix("hash: "))
        .unwrap()
        .to_owned();
    fs::remove_file(
        worktree
            .join(".avc/cache/objects/sha256")
            .join(&hash[..2])
            .join(&hash),
    )
    .unwrap();
    let stopped = avc(&worktree, &["gc"]);
    assert_eq!(stopped.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stopped.stderr).contains("refusing to delete"));
}

#[test]
fn rejects_directories_it_cannot_track_faithfully() {
    let directory = TempDir::new("reject");
    let worktree = repository(&directory.0);

    fs::create_dir_all(worktree.join("empty")).unwrap();
    let empty = avc(&worktree, &["add", "empty"]);
    assert_eq!(empty.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no files to track"));

    // A pointer file inside a tracked directory would be read as a pointer by
    // worktree discovery and as content by the manifest.
    write(&worktree.join("mixed/model.bin"), "bytes\n");
    write(&worktree.join("mixed/model.bin.avc"), "version: 1\n");
    let mixed = avc(&worktree, &["add", "mixed"]);
    assert_eq!(mixed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("mixed/model.bin.avc"));
}
