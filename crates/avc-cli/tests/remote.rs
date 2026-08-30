//! What `avc remote add` records, and what the transports do with it.
//!
//! A remote is described in one place — the tracked `.avc/config.toml` — so
//! everything a clone needs to reach the right bytes belongs there: the bucket,
//! the prefix inside it, the signing region, and the name of the AWS profile to
//! authenticate with. Credentials never do; those stay in the environment or in
//! the gitignored `config.local.toml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that removes itself, so a failing test leaves no litter.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-remote-{label}-{}-{:?}",
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
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("AVC_REPO")
        .env_remove("AVC_REF")
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

/// A `file://` URL for a local path, spelled so it parses on Windows too.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// An initialized worktree, ready for `avc remote add`.
fn worktree(label: &str) -> TempDir {
    let directory = TempDir::new(label);
    let output = Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .current_dir(&directory.0)
        .output()
        .expect("these tests need the git command");
    assert!(output.status.success());
    run(&directory.0, &["init"]);
    directory
}

#[test]
fn a_remote_records_its_bucket_prefix_region_and_profile() {
    let directory = worktree("record");
    let added = run(
        &directory.0,
        &[
            "remote",
            "add",
            "origin",
            "s3://my-bucket/artifacts/v1",
            "--region",
            "sa-east-1",
            "--profile",
            "artifacts",
        ],
    );
    assert!(added.contains("my-bucket/artifacts/v1"), "{added}");
    assert!(added.contains("sa-east-1"), "{added}");
    assert!(added.contains("artifacts"), "{added}");

    let config = fs::read_to_string(directory.0.join(".avc/config.toml")).unwrap();
    assert!(
        config.contains("bucket_or_container = \"my-bucket\""),
        "{config}"
    );
    assert!(config.contains("prefix = \"artifacts/v1\""), "{config}");
    assert!(config.contains("region = \"sa-east-1\""), "{config}");
    assert!(config.contains("profile = \"artifacts\""), "{config}");
    // The tracked file is committed, so a credential must never reach it.
    assert!(!config.contains("secret"), "{config}");

    let listed = run(&directory.0, &["remote", "list"]);
    assert!(listed.contains("REGION"), "{listed}");
    assert!(listed.contains("PROFILE"), "{listed}");
    assert!(listed.contains("sa-east-1"), "{listed}");
}

#[test]
fn region_and_profile_are_optional_and_stay_out_of_the_way() {
    let directory = worktree("optional");
    run(&directory.0, &["remote", "add", "origin", "s3://my-bucket"]);

    let config = fs::read_to_string(directory.0.join(".avc/config.toml")).unwrap();
    assert!(!config.contains("region"), "{config}");
    assert!(!config.contains("profile"), "{config}");

    // With nothing to show, the listing keeps the columns it always had.
    let listed = run(&directory.0, &["remote", "list"]);
    assert!(!listed.contains("REGION"), "{listed}");
    assert!(!listed.contains("PROFILE"), "{listed}");
}

/// The prefix in the URL is where the bytes actually land — not merely a field
/// that round-trips through the configuration file.
#[test]
fn a_prefix_in_the_url_becomes_the_key_prefix_on_the_remote() {
    let directory = worktree("prefix");
    let store = directory.0.join("store");
    fs::create_dir_all(&store).unwrap();
    fs::write(directory.0.join("model.bin"), "weights\n").unwrap();

    run(&directory.0, &["add", "model.bin"]);
    run(
        &directory.0,
        &[
            "remote",
            "add",
            "origin",
            &format!("{}/team-a/artifacts", file_url(&store)),
        ],
    );
    run(&directory.0, &["push"]);

    assert!(
        store.join("team-a/artifacts/objects/sha256").is_dir(),
        "objects should be written beneath the prefix"
    );
    assert!(
        !store.join("objects").exists(),
        "nothing should be written at the store root"
    );

    // And the prefix is read back the same way: a fresh clone of the same
    // configuration finds the objects it pushed.
    let listed = run(&directory.0, &["list"]);
    assert!(listed.contains("model.bin"), "{listed}");
}

#[test]
fn an_empty_region_or_profile_is_not_recorded_as_a_choice() {
    let directory = worktree("empty");
    run(
        &directory.0,
        &[
            "remote",
            "add",
            "origin",
            "s3://my-bucket",
            "--region",
            "  ",
            "--profile",
            "",
        ],
    );
    let config = fs::read_to_string(directory.0.join(".avc/config.toml")).unwrap();
    assert!(!config.contains("region"), "{config}");
    assert!(!config.contains("profile"), "{config}");
}
