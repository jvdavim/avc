//! End-to-end coverage of `avc serve`.
//!
//! The server is started on a free port against a published registry read by
//! Git URL — so no cache is involved and every byte comes from the object
//! store — and spoken to over a plain socket, the way any HTTP client would.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// A scratch directory that removes itself, so a failing test leaves no litter.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-serve-{label}-{}-{:?}",
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

fn run(directory: &Path, arguments: &[&str]) {
    let output = avc(directory, arguments);
    assert!(
        output.status.success(),
        "avc {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(directory: &Path, arguments: &[&str]) {
    // An identity of its own, because an annotated tag needs a committer and
    // a CI runner has none configured.
    let output = Command::new("git")
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .current_dir(directory)
        .output()
        .expect("these tests need the git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
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

/// A committed registry with a remote, returning its Git URL.
fn publish(root: &Path) -> String {
    let source = root.join("registry");
    fs::create_dir_all(root.join("store")).unwrap();
    fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "--quiet", "-b", "main"]);
    write(&source.join("models/bert/weights.bin"), "bert weights\n");
    write(&source.join("models/bert/tokenizer.json"), "{}\n");
    write(&source.join("data/a.bin"), "alpha\n");
    write(&source.join("data/nested/b.bin"), "beta\n");
    run(&source, &["init"]);
    run(
        &source,
        &[
            "add",
            "models/bert/weights.bin",
            "models/bert/tokenizer.json",
            "data",
        ],
    );
    run(
        &source,
        &["remote", "add", "origin", &file_url(&root.join("store"))],
    );
    run(&source, &["push"]);
    git(&source, &["add", "-A"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "Publish",
        ],
    );
    file_url(&source)
}

/// A running server, stopped when dropped.
struct Server {
    child: Child,
    address: String,
}

impl Server {
    fn start(directory: &Path, repo: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_avc"))
            .args(["serve", "--repo", repo, "--port", "0"])
            .env("NO_COLOR", "1")
            .current_dir(directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the avc binary should run");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut address = None;
        for line in lines.by_ref() {
            let line = line.unwrap();
            if let Some(url) = line.split_whitespace().find(|w| w.starts_with("http://")) {
                address = Some(
                    url.trim_start_matches("http://")
                        .trim_end_matches('/')
                        .to_owned(),
                );
                break;
            }
        }
        // Keep reading what the server logs, as a terminal would. Closing the
        // pipe instead would test a server whose log nobody reads — which is
        // `a_closed_log_does_not_stop_the_server`'s job, not every test's.
        std::thread::spawn(move || lines.for_each(drop));
        Self {
            child,
            address: address.expect("serve should print the URL it listens on"),
        }
    }

    /// Send a GET and return the status, headers, and body as received — which
    /// may be shorter than `Content-Length` promised.
    fn get(&self, target: &str) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.address
        )
        .unwrap();
        let mut response = Vec::new();
        // A connection the server resets mid-body is an outcome under test,
        // not a test failure.
        let _ = stream.read_to_end(&mut response);
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a complete response head");
        let head = String::from_utf8(response[..split].to_vec()).unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head, response[split + 4..].to_vec())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap()
}

/// File names in a tar archive, read from its headers.
fn tar_names(archive: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut offset = 0;
    while offset + 512 <= archive.len() && archive[offset] != 0 {
        let header = &archive[offset..offset + 512];
        let name = String::from_utf8_lossy(&header[..100])
            .trim_end_matches('\0')
            .to_owned();
        let size = u64::from_str_radix(
            String::from_utf8_lossy(&header[124..135]).trim_matches('\0'),
            8,
        )
        .unwrap() as usize;
        names.push(name);
        offset += 512 + size.div_ceil(512) * 512;
    }
    names
}

#[test]
fn browses_and_downloads_a_registry() {
    let root = TempDir::new("browse");
    let repo = publish(&root.0);
    let server = Server::start(&root.0, &repo);

    let (status, head, page) = server.get("/");
    assert_eq!(status, 200);
    assert!(head.contains("text/html"));
    assert!(String::from_utf8(page).unwrap().contains("AVC catalog"));

    let (status, _, catalog) = server.get("/api/catalog");
    assert_eq!(status, 200);
    let catalog = String::from_utf8(catalog).unwrap();
    for expected in [
        r#""path":"data","kind":"directory","size":11,"files":2"#,
        r#""path":"models/bert/weights.bin","kind":"file","size":13"#,
    ] {
        assert!(catalog.contains(expected), "{catalog}");
    }
    assert!(!catalog.contains(r#""available":false"#), "{catalog}");

    let (_, _, tree) = server.get("/api/tree?path=data");
    let tree = String::from_utf8(tree).unwrap();
    assert!(tree.contains(r#""path":"nested/b.bin","size":5"#), "{tree}");

    // One file comes down as itself, under its own name.
    let (status, head, body) = server.get("/download?path=models%2Fbert%2Fweights.bin");
    assert_eq!(status, 200);
    assert!(head.contains(r#"filename="weights.bin""#), "{head}");
    assert_eq!(body, b"bert weights\n");

    // So does one file inside a tracked directory.
    let (_, _, body) = server.get("/download?path=data/nested/b.bin");
    assert_eq!(body, b"beta\n");

    // Anything larger is an archive laid out as `avc fetch` would lay it out.
    let (status, head, body) = server.get("/download?path=models/bert");
    assert_eq!(status, 200);
    assert!(head.contains(r#"filename="bert.tar""#), "{head}");
    assert_eq!(body.len(), content_length(&head));
    assert_eq!(
        tar_names(&body),
        ["bert/tokenizer.json", "bert/weights.bin"]
    );
    let (_, _, body) = server.get("/download?path=data");
    assert_eq!(tar_names(&body), ["data/a.bin", "data/nested/b.bin"]);

    let (status, _, body) = server.get("/download?path=models/absent");
    assert_eq!(status, 404);
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("no artifact at models/absent"));
}

/// The server must never hand out bytes that do not match their pointer, and
/// the only way to say so once a download has begun is to end it short.
#[test]
fn a_corrupt_object_is_never_delivered_whole() {
    let root = TempDir::new("corrupt");
    let repo = publish(&root.0);
    let object = walk(&root.0.join("store"))
        .into_iter()
        .find(|path| fs::read(path).is_ok_and(|bytes| bytes == b"bert weights\n"))
        .unwrap();
    // Same length, different bytes: only the digest can tell.
    fs::write(&object, "BERT WEIGHTS\n").unwrap();

    let server = Server::start(&root.0, &repo);
    let (status, head, body) = server.get("/download?path=models/bert/weights.bin");
    assert_eq!(status, 200);
    assert!(
        body.len() < content_length(&head),
        "a corrupt object was sent whole"
    );
    assert!(!body.windows(4).any(|window| window == b"BERT"));

    // The rest of the registry is unaffected.
    let (_, _, body) = server.get("/download?path=models/bert/tokenizer.json");
    assert_eq!(body, b"{}\n");
}

/// Requests naming another host are refused while serving on loopback, which
/// is what stops a DNS-rebinding page from reading the catalog.
#[test]
fn refuses_a_foreign_host_header() {
    let root = TempDir::new("host");
    let repo = publish(&root.0);
    let server = Server::start(&root.0, &repo);
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .write_all(b"GET /api/catalog HTTP/1.1\r\nHost: attacker.example\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
}

fn commit(directory: &Path, message: &str) {
    git(directory, &["add", "-A"]);
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

/// Every version a visitor can pick serves the bytes that version's pointers
/// name, read from the object store.
#[test]
fn serves_the_version_a_visitor_picks() {
    let root = TempDir::new("versions");
    let repo = publish(&root.0);
    let source = root.0.join("registry");
    git(&source, &["tag", "v1.0.0"]);
    git(&source, &["branch", "legacy"]);
    let first = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    write(&source.join("models/bert/weights.bin"), "bert weights v2\n");
    write(&source.join("data/a.bin"), "alpha v2\n");
    run(&source, &["commit", "models/bert/weights.bin", "data"]);
    run(&source, &["push"]);
    commit(&source, "Second version");
    git(&source, &["tag", "-a", "v2.0.0", "-m", "Second"]);

    let server = Server::start(&root.0, &repo);
    let (status, _, refs) = server.get("/api/refs");
    assert_eq!(status, 200);
    let refs = String::from_utf8(refs).unwrap();
    for expected in [
        r#""head":"main""#,
        r#""name":"legacy""#,
        r#""name":"v1.0.0""#,
        r#""name":"v2.0.0""#,
    ] {
        assert!(refs.contains(expected), "{refs}");
    }

    let weights = |reference: &str| {
        let (status, _, body) = server.get(&format!(
            "/download?path=models/bert/weights.bin&ref={reference}"
        ));
        assert_eq!(status, 200, "{reference}");
        String::from_utf8(body).unwrap()
    };
    // The default is the repository's default branch.
    assert_eq!(weights(""), "bert weights v2\n");
    assert_eq!(weights("main"), "bert weights v2\n");
    assert_eq!(weights("v2.0.0"), "bert weights v2\n");
    assert_eq!(weights("v1.0.0"), "bert weights\n");
    assert_eq!(weights("legacy"), "bert weights\n");
    assert_eq!(weights(&first.trim()[..12]), "bert weights\n");
    // A full commit id is what clicking a commit in the history graph sends.
    assert_eq!(weights(first.trim()), "bert weights\n");

    // The graph: every commit, newest first, labelled with what points at it.
    let (status, _, history) = server.get("/api/history");
    assert_eq!(status, 200);
    let history = String::from_utf8(history).unwrap();
    let newest = history
        .find(r#""subject":"Second version""#)
        .expect(&history);
    let oldest = history.find(r#""subject":"Publish""#).expect(&history);
    assert!(newest < oldest, "{history}");
    assert!(history.contains(&format!(
        r#""hash":"{}","parents":[],"author":"Test""#,
        first.trim()
    )));
    assert!(
        history.contains(r#""branches":["main"],"tags":["v2.0.0"]"#),
        "{history}"
    );
    assert!(
        history.contains(r#""branches":["legacy"],"tags":["v1.0.0"]"#),
        "{history}"
    );
    // Served by URL, there is no working tree to offer.
    assert!(history.contains(r#""worktree":null"#), "{history}");

    // A file inside a tracked directory follows the version too.
    let (_, _, body) = server.get("/download?path=data/a.bin&ref=v1.0.0");
    assert_eq!(body, b"alpha\n");

    let (_, _, catalog) = server.get("/api/catalog?ref=v1.0.0");
    let catalog = String::from_utf8(catalog).unwrap();
    assert!(
        catalog.contains(&format!(r#""commit":"{}""#, &first.trim()[..12])),
        "{catalog}"
    );
}

/// A revision is handed to Git, so only names the repository advertises, or
/// commit ids, may reach it — never an option of the requester's choosing.
#[test]
fn refuses_a_version_the_repository_does_not_have() {
    let root = TempDir::new("badref");
    let repo = publish(&root.0);
    let server = Server::start(&root.0, &repo);
    let marker = root.0.join("injected");
    for reference in [
        "nosuch".to_owned(),
        format!("--upload-pack=touch%20{}", marker.display()),
    ] {
        let (status, _, body) = server.get(&format!("/api/catalog?ref={reference}"));
        assert_eq!(status, 404, "{}", String::from_utf8_lossy(&body));
    }
    assert!(!marker.exists());
}

/// A server whose log is piped somewhere that stops reading — `avc serve |
/// head`, or a supervisor that went away — must keep serving.
#[test]
fn a_closed_log_does_not_stop_the_server() {
    let root = TempDir::new("closed-log");
    let repo = publish(&root.0);
    let mut child = Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(["serve", "--repo", &repo, "--port", "0"])
        .env("NO_COLOR", "1")
        .current_dir(&root.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the avc binary should run");
    let stderr = child.stderr.take().unwrap();
    // Read only the line naming the address, then hang up, before the server
    // has printed the rest of its banner or logged a single download.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut address = None;
    let mut line = String::new();
    while address.is_none() && stdout.read_line(&mut line).unwrap() > 0 {
        address = line
            .split_whitespace()
            .find(|word| word.starts_with("http://"))
            .map(|url| {
                url.trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_owned()
            });
        line.clear();
    }
    drop(stdout);
    let server = Server {
        child,
        address: address.expect("serve should print the URL it listens on"),
    };
    // Twice: the first download's log line is a write to the dead pipe, and
    // the second shows the server survived it.
    for _ in 0..2 {
        let (status, _, body) = server.get("/download?path=models/bert/weights.bin");
        assert_eq!(status, 200);
        assert_eq!(body, b"bert weights\n");
    }
    // A write to a closed pipe that panicked would say so here, even when it
    // happened after the response was sent.
    drop(server);
    let mut errors = String::new();
    BufReader::new(stderr).read_to_string(&mut errors).unwrap();
    assert!(!errors.contains("panicked"), "{errors}");
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
