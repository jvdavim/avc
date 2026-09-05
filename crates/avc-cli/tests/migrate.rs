//! End-to-end coverage of migrating a DVC project.
//!
//! These build a real DVC repository — real Git history, real `.dvc` files,
//! real objects laid out the way a DVC remote lays them out — and migrate it
//! with the real binary. The point of testing at this level is that almost
//! every interesting failure of a migration is an interaction: a pointer that
//! parses but names an object at the wrong key, a history that replays but
//! loses a merge, a resume that produces a different answer from the run it
//! resumed. None of those are visible to a unit test of any one piece.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-migrate-{label}-{}-{:?}",
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

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
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
        .args(["--color", "never", "--progress", "never"])
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

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0530")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0530")
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// MD5, computed the long way so the test does not depend on the same code the
/// migration uses to decide an object's name.
fn md5_hex(bytes: &[u8]) -> String {
    let output = Command::new("md5sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin was piped")
                .write_all(bytes)?;
            child.wait_with_output()
        })
        .expect("md5sum should run");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("md5sum prints a digest")
        .to_owned()
}

/// A DVC project under construction: a Git repository and the remote beside it.
struct DvcProject {
    repo: PathBuf,
    store: PathBuf,
}

impl DvcProject {
    fn new(root: &TempDir) -> Self {
        let repo = root.join("dvcrepo");
        let store = root.join("dvcstore");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&store).unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]);
        // DVC's own configuration directory, which the migration drops.
        fs::create_dir_all(repo.join(".dvc")).unwrap();
        fs::write(repo.join(".dvc/config"), "[core]\n    remote = storage\n").unwrap();
        Self { repo, store }
    }

    /// Put bytes on the DVC remote under DVC 3's key layout.
    fn store_object(&self, bytes: &[u8], directory: bool) -> String {
        let md5 = md5_hex(bytes);
        let suffix = if directory { ".dir" } else { "" };
        let path = self
            .store
            .join("files/md5")
            .join(&md5[..2])
            .join(format!("{}{suffix}", &md5[2..]));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        md5
    }

    /// Track a file the way `dvc add` would: object on the remote, `.dvc` file
    /// in Git, artifact itself ignored.
    fn add_file(&self, path: &str, contents: &str) {
        let md5 = self.store_object(contents.as_bytes(), false);
        fs::create_dir_all(self.repo.join(path).parent().unwrap()).unwrap();
        fs::write(
            self.repo.join(format!("{path}.dvc")),
            format!(
                "outs:\n- md5: {md5}\n  size: {}\n  hash: md5\n  path: {}\n",
                contents.len(),
                Path::new(path).file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
    }

    /// Track a directory the way `dvc add` would, manifest object and all.
    fn add_directory(&self, path: &str, files: &[(&str, &str)]) {
        let mut entries = Vec::new();
        let mut total = 0;
        for (relative, contents) in files {
            let md5 = self.store_object(contents.as_bytes(), false);
            total += contents.len();
            entries.push(format!(
                "{{\"md5\": \"{md5}\", \"relpath\": \"{relative}\"}}"
            ));
        }
        let manifest = format!("[{}]", entries.join(", "));
        let md5 = self.store_object(manifest.as_bytes(), true);
        fs::write(
            self.repo.join(format!("{path}.dvc")),
            format!(
                "outs:\n- md5: {md5}.dir\n  size: {total}\n  nfiles: {}\n  hash: md5\n  path: {path}\n",
                files.len()
            ),
        )
        .unwrap();
    }

    fn commit(&self, message: &str) {
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", message]);
    }
}

/// A project with two commits, a branch, a tag, a directory artifact, and a
/// pipeline output — the shapes a real migration has to carry.
fn project(root: &TempDir) -> DvcProject {
    let dvc = DvcProject::new(root);
    dvc.add_file("model.bin", "weights version one\n");
    fs::write(dvc.repo.join(".gitignore"), "/model.bin\n").unwrap();
    dvc.commit("track the model");

    dvc.add_directory(
        "raw",
        &[("a.csv", "alpha\n"), ("nested/b.csv", "beta beta\n")],
    );
    let output = "model output\n";
    let md5 = dvc.store_object(output.as_bytes(), false);
    fs::write(
        dvc.repo.join("dvc.lock"),
        format!(
            "schema: '2.0'\nstages:\n  train:\n    cmd: python train.py\n    outs:\n    \
             - path: model.pkl\n      hash: md5\n      md5: {md5}\n      size: {}\n",
            output.len()
        ),
    )
    .unwrap();
    fs::write(
        dvc.repo.join("dvc.yaml"),
        "stages:\n  train:\n    cmd: python train.py\n",
    )
    .unwrap();
    dvc.commit("a dataset directory and a pipeline output");
    git(&dvc.repo, &["tag", "v1.0"]);

    git(&dvc.repo, &["checkout", "--quiet", "-b", "dev"]);
    dvc.add_file("model.bin", "weights version two, longer\n");
    dvc.commit("retrain");
    git(&dvc.repo, &["checkout", "--quiet", "main"]);
    dvc
}

fn migrate(dvc: &DvcProject, into: &Path, store: &Path, extra: &[&str]) -> String {
    let mut arguments = vec![
        "migrate".to_owned(),
        "dvc".to_owned(),
        dvc.repo.to_string_lossy().into_owned(),
        file_url(&dvc.store),
        "--into".to_owned(),
        into.to_string_lossy().into_owned(),
        "--to".to_owned(),
        file_url(store),
    ];
    arguments.extend(extra.iter().map(|value| value.to_string()));
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run(into.parent().unwrap(), &borrowed)
}

#[test]
fn a_migrated_repository_is_a_working_avc_repository() {
    let root = TempDir::new("roundtrip");
    let dvc = project(&root);
    let into = root.join("avcrepo");
    let store = root.join("avcstore");
    migrate(&dvc, &into, &store, &[]);

    // Every kind of tracked thing came across: a hand-tracked file, a tracked
    // directory, and a pipeline output from dvc.lock.
    let tracked = git(&into, &["ls-files"]);
    assert!(tracked.contains("model.bin.avc"), "{tracked}");
    assert!(tracked.contains("raw.avc"), "{tracked}");
    assert!(tracked.contains("model.pkl.avc"), "{tracked}");
    // DVC's own files are gone; the pipeline definition is not DVC's to delete.
    assert!(!tracked.contains(".dvc/config"), "{tracked}");
    assert!(!tracked.contains("dvc.lock"), "{tracked}");
    assert!(tracked.contains("dvc.yaml"), "{tracked}");

    // And the bytes are really there, byte for byte.
    run(&into, &["pull"]);
    assert_eq!(
        fs::read_to_string(into.join("model.bin")).unwrap(),
        "weights version one\n"
    );
    assert_eq!(
        fs::read_to_string(into.join("raw/nested/b.csv")).unwrap(),
        "beta beta\n"
    );
    assert_eq!(
        fs::read_to_string(into.join("model.pkl")).unwrap(),
        "model output\n"
    );
    let status = run(&into, &["status"]);
    assert!(status.contains("3 ok, 0 modified, 0 missing"), "{status}");
    run(&into, &["doctor"]);
}

#[test]
fn an_objects_identity_survives_so_the_move_costs_no_bytes() {
    let root = TempDir::new("identity");
    let dvc = project(&root);
    let into = root.join("avcrepo");
    let store = root.join("avcstore");
    let report = migrate(&dvc, &into, &store, &[]);

    // The headline promise: nothing was read to work out where it goes.
    assert!(
        report.contains("no bytes over the network"),
        "the migration should move objects rather than stream them: {report}"
    );

    let pointer = fs::read_to_string(into.join("model.bin.avc")).unwrap();
    assert!(pointer.contains("algorithm: md5"), "{pointer}");
    // The object is at the key its preserved MD5 names, and its digest is the
    // one DVC gave it.
    let md5 = md5_hex(b"weights version one\n");
    assert!(pointer.contains(&md5), "{pointer}");
    assert!(store
        .join("objects/md5")
        .join(&md5[..2])
        .join(&md5)
        .is_file());

    // `--rehash` buys SHA-256 instead, at the price of reading everything.
    let sha_into = root.join("sha");
    let sha_store = root.join("shastore");
    let report = migrate(&dvc, &sha_into, &sha_store, &["--rehash"]);
    assert!(!report.contains("no bytes over the network"), "{report}");
    let pointer = fs::read_to_string(sha_into.join("model.bin.avc")).unwrap();
    assert!(pointer.contains("algorithm: sha256"), "{pointer}");
    run(&sha_into, &["pull"]);
    assert_eq!(
        fs::read_to_string(sha_into.join("model.bin")).unwrap(),
        "weights version one\n"
    );
}

#[test]
fn the_whole_history_is_replayed_not_just_the_tip() {
    let root = TempDir::new("history");
    let dvc = project(&root);
    let into = root.join("avcrepo");
    migrate(&dvc, &into, &root.join("avcstore"), &[]);

    // Same commits, same branches, same tag.
    assert_eq!(
        git(&dvc.repo, &["rev-list", "--count", "--all"]).trim(),
        git(&into, &["rev-list", "--count", "--all"]).trim()
    );
    let branches = git(&into, &["branch", "--format=%(refname:short)"]);
    assert!(
        branches.contains("main") && branches.contains("dev"),
        "{branches}"
    );
    assert_eq!(git(&into, &["tag"]).trim(), "v1.0");

    // Same authorship and the same instant, zone included: a rewritten history
    // that renamed its authors or shifted its dates would not be the history.
    let format = "--format=%an <%ae> %ad %s";
    assert_eq!(
        git(
            &dvc.repo,
            &["log", "--reverse", format, "--date=raw", "main"]
        ),
        git(&into, &["log", "--reverse", format, "--date=raw", "main"])
    );

    // An older revision still resolves, which is the whole reason for
    // replaying history rather than migrating a checkout.
    let older = run(
        &into,
        &[
            "list",
            "--repo",
            &into.to_string_lossy(),
            "--ref",
            "v1.0",
            "--porcelain",
        ],
    );
    assert!(older.contains("model.bin"), "{older}");
    // The dev branch has its own version of the artifact, not main's.
    let dev = git(&into, &["show", "dev:model.bin.avc"]);
    assert!(
        dev.contains(&md5_hex(b"weights version two, longer\n")),
        "{dev}"
    );
}

#[test]
fn migrating_into_a_repository_that_already_has_history_leaves_it_alone() {
    let root = TempDir::new("existing");
    let dvc = project(&root);
    let into = root.join("mine");
    fs::create_dir_all(&into).unwrap();
    git(&into, &["init", "--quiet", "-b", "main"]);
    fs::write(into.join("README.md"), "our own work\n").unwrap();
    git(&into, &["add", "-A"]);
    git(&into, &["commit", "--quiet", "-m", "existing project"]);
    let before = git(&into, &["rev-parse", "main"]);

    migrate(&dvc, &into, &root.join("avcstore"), &[]);

    // The migrated refs are prefixed, so nothing that was here is shadowed.
    let branches = git(&into, &["branch", "--format=%(refname:short)"]);
    assert!(branches.contains("dvc-main"), "{branches}");
    assert!(branches.contains("dvc-dev"), "{branches}");
    assert_eq!(git(&into, &["tag"]).trim(), "dvc-v1.0");
    assert_eq!(before, git(&into, &["rev-parse", "main"]), "main moved");
    // Including the working tree: this runs in somebody's checkout.
    assert_eq!(git(&into, &["status", "--porcelain"]).trim(), "");
}

#[test]
fn an_interrupted_migration_resumes_where_it_stopped() {
    let root = TempDir::new("resume");
    let dvc = project(&root);
    let into = root.join("avcrepo");
    let store = root.join("avcstore");

    // A destination that cannot be written to, so the transfer phase fails
    // after the phases before it have succeeded. A regular file where the
    // object directory belongs is the portable way to arrange that: every OS
    // refuses to create a directory underneath a file, whereas a read-only
    // directory stops nothing on Windows.
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("objects"), "not a directory\n").unwrap();

    let failed = avc(
        &root.0,
        &[
            "migrate",
            "dvc",
            &dvc.repo.to_string_lossy(),
            &file_url(&dvc.store),
            "--into",
            &into.to_string_lossy(),
            "--to",
            &file_url(&store),
        ],
    );
    assert!(!failed.status.success(), "the transfer should have failed");
    // Exit code 3 is what SPEC.md reserves for an operational failure, and is
    // what tells a pipeline this is worth retrying.
    assert_eq!(failed.status.code(), Some(3));
    // The failure says which object it was on, not just that something failed.
    let message = String::from_utf8_lossy(&failed.stderr);
    assert!(message.contains("while moving DVC object"), "{message}");

    fs::remove_file(store.join("objects")).unwrap();
    let report = migrate(&dvc, &into, &store, &[]);
    // The expensive phases are not repeated.
    assert!(report.contains("already taken"), "{report}");
    // And the resumed run reaches the same answer an uninterrupted one does —
    // in particular it still treats the destination as a new repository, even
    // though the failed run left refs in it.
    assert!(
        report.contains("branches and tags keep their names"),
        "{report}"
    );
    let branches = git(&into, &["branch", "--format=%(refname:short)"]);
    assert!(
        branches.contains("main") && !branches.contains("dvc-main"),
        "{branches}"
    );
    run(&into, &["pull"]);
    run(&into, &["doctor"]);
}

#[test]
fn a_journal_from_a_different_migration_is_not_resumed() {
    let root = TempDir::new("fingerprint");
    let dvc = project(&root);
    let into = root.join("avcrepo");
    migrate(&dvc, &into, &root.join("store-one"), &[]);
    // A completed migration leaves no journal behind, so re-running with
    // different arguments has nothing stale to trip over.
    assert!(!into.join(".avc/state/migrate").exists());

    // But a migration in progress does, and resuming it with a different
    // destination would mix two migrations into one repository.
    let other = root.join("second");
    migrate(&dvc, &other, &root.join("store-two"), &[]);
    fs::create_dir_all(other.join(".avc/state/migrate")).unwrap();
    fs::write(
        other.join(".avc/state/migrate/migration"),
        "1\nsomething else\n",
    )
    .unwrap();
    let refused = avc(
        &root.0,
        &[
            "migrate",
            "dvc",
            &dvc.repo.to_string_lossy(),
            &file_url(&dvc.store),
            "--into",
            &other.to_string_lossy(),
            "--to",
            &file_url(&root.join("store-two")),
        ],
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(message.contains("--restart"), "{message}");
}
