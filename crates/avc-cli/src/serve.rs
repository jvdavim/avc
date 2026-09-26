//! A browsable catalog of a repository's artifacts, served over HTTP.
//!
//! `avc list` answers "what is in this registry" for someone at a terminal.
//! `avc serve` answers it for everyone else: it puts the same listing behind a
//! small web page, where a teammate can walk the repository's paths, look inside
//! a tracked directory, and download the one file or directory they need,
//! without installing AVC or holding the bucket's credentials themselves.
//!
//! The server is deliberately small. It speaks just enough HTTP/1.1 over
//! `std::net` to serve a page, two JSON documents, and downloads — one request
//! per connection, a thread per connection, no keep-alive — because pulling in
//! an async runtime and a web framework for that would triple the dependency
//! tree of a tool whose README promises it stays small.
//!
//! What it serves follows the rules the rest of AVC keeps:
//!
//! - **Only what a pointer names.** A download is resolved through the
//!   registry's own selection, so a path either names an artifact, a prefix of
//!   artifacts, or something inside a tracked directory — never a file on disk.
//! - **Verified bytes only.** Every object is hashed as it streams, and the
//!   last chunk is held back until the digest checks out. A corrupt object ends
//!   the response short of its `Content-Length`, which every HTTP client
//!   reports as a failed download rather than a successful one with the wrong
//!   bytes.
//! - **What was named, under its own name.** A download of `models/bert` is a
//!   tar archive holding `bert/…`, exactly as `avc fetch models/bert -o .` would
//!   have laid it out.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Args;

use crate::git;
use crate::registry::{self, Registry};
use crate::ui::{self, Style};
use crate::Failure;

/// The page itself. Everything it needs is inline, so the server has exactly
/// one static asset and the page works without reaching the internet.
const PAGE: &str = include_str!("serve/index.html");

/// Longest request head accepted. A catalog request is a request line and a
/// handful of headers; anything longer is not a browser talking to us.
const MAX_HEAD: usize = 16 * 1024;

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Git URL of the repository to serve. Defaults to the checkout the
    /// command is run in, whose working tree is re-read on every request so
    /// new pointers show up.
    #[arg(long, value_name = "URL", env = "AVC_REPO")]
    pub repo: Option<String>,

    /// Revision shown when a visitor has not picked one: a branch, a tag, a
    /// commit, or a fully qualified `refs/...` name. Visitors can switch to any
    /// other branch, tag, or commit from the page.
    #[arg(long = "ref", value_name = "REV", env = "AVC_REF")]
    pub reference: Option<String>,

    /// Named object store, when the repository configures more than one.
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,

    /// Object store URL, overriding the one the repository configures.
    #[arg(long, value_name = "URL")]
    pub remote_url: Option<String>,

    /// Address to listen on. The default is reachable from this machine only;
    /// pass `0.0.0.0` to share the catalog with your network.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1")]
    pub bind: IpAddr,

    /// Port to listen on. `0` picks a free one.
    #[arg(long, short, value_name = "PORT", default_value_t = 8080)]
    pub port: u16,
}

/// Serve the catalog until the process is stopped.
pub fn serve(args: &ServeArgs) -> Result<(), Failure> {
    let (url, root, mirror) = match &args.repo {
        Some(url) => {
            ui::line(
                &format!("reading the history of {}", git::redact(url)),
                Style::Dim,
            );
            (url.clone(), None, Some(git::Mirror::clone(url)?))
        }
        None => {
            let root = crate::find_root()?;
            (root.display().to_string(), Some(root), None)
        }
    };
    // A checkout with no `--ref` is served as it stands on disk, which is what
    // every other command in a checkout does; anything else names a revision.
    let (default, worktree) = match (&root, &args.reference) {
        (Some(root), None) => (
            String::new(),
            Some(Arc::new(Registry::from_directory(Some(root.clone()))?)),
        ),
        (_, reference) => (reference.clone().unwrap_or_else(|| "HEAD".to_owned()), None),
    };
    let catalog = Arc::new(Catalog {
        url,
        root,
        mirror,
        default,
        worktree,
        remote: args.remote.clone(),
        remote_url: args.remote_url.clone(),
        trees: Mutex::new(HashMap::new()),
        refs: Mutex::new(None),
        versions: Mutex::new(VecDeque::new()),
    });
    // Opened once up front so a bad revision or a misconfigured remote fails
    // here, at the terminal, rather than as an error page somebody else finds.
    let version = catalog.version("")?;

    let listener = TcpListener::bind(SocketAddr::new(args.bind, args.port)).map_err(|error| {
        Failure::from(format!(
            "cannot listen on {}: {error}",
            SocketAddr::new(args.bind, args.port)
        ))
    })?;
    let address = listener.local_addr().map_err(crate::io_error)?;
    let loopback = address.ip().is_loopback();

    ui::heading(&format!("serving {}", version.registry.describe()));
    ui::field(
        "objects",
        &version.store.as_ref().map_or_else(
            || "local cache only (no object store configured)".to_owned(),
            |store| store.describe(),
        ),
    );
    ui::field("url", &format!("http://{}/", display_address(address)));
    if !loopback {
        ui::line(
            "warning: the catalog is reachable from the network, and it serves every \
             artifact with this machine's credentials",
            Style::Warn,
        );
    }
    ui::note("press Ctrl-C to stop");
    println!();
    drop(version);

    for connection in listener.incoming() {
        let Ok(stream) = connection else { continue };
        let catalog = Arc::clone(&catalog);
        std::thread::spawn(move || handle(&catalog, stream, loopback));
    }
    Ok(())
}

/// How the listening address is written in a URL: an IPv6 address needs
/// brackets, and an unspecified one is reached through loopback.
fn display_address(address: SocketAddr) -> String {
    let ip = if address.ip().is_unspecified() {
        match address.ip() {
            IpAddr::V4(_) => IpAddr::from([127, 0, 0, 1]),
            IpAddr::V6(_) => IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
        }
    } else {
        address.ip()
    };
    SocketAddr::new(ip, address.port()).to_string()
}

/// How long a listing of branches and tags is trusted before it is asked for
/// again. Long enough that browsing does not run `git ls-remote` on every
/// click; short enough that a new tag shows up while someone is looking.
const REFS_TTL: Duration = Duration::from_secs(20);

/// How many commits the history graph shows. Enough for any registry's recent
/// past; older versions stay reachable by commit id.
const MAX_HISTORY: usize = 500;

/// How many revisions' pointers are kept checked out at once. Each is a
/// directory of pointer files, so this bounds disk use, not correctness.
const MAX_VERSIONS: usize = 16;

/// The repository being served, and what has been learned about it.
struct Catalog {
    /// The repository as it was named: the Git URL, or the checkout's path.
    url: String,
    /// The checkout being served, when it is one.
    root: Option<PathBuf>,
    /// A local copy of the repository named by URL, which history is read
    /// from and revisions are checked out of.
    mirror: Option<git::Mirror>,
    /// The revision a request that names none gets; empty for the working tree.
    default: String,
    /// The working tree, when that is the default.
    worktree: Option<Arc<Registry>>,
    remote: Option<String>,
    remote_url: Option<String>,
    /// Manifests already read, by digest.
    ///
    /// A manifest is immutable — its digest is its name — so one read is good
    /// for the life of the server and for every revision that names it.
    trees: Mutex<HashMap<String, avc_core::Tree>>,
    /// The latest branch and tag listing, and when it was taken.
    refs: Mutex<Option<(Instant, Arc<git::Refs>)>>,
    /// Revisions already checked out, most recently used last.
    ///
    /// Keyed by the name fetched *and* the commit it named, so a branch that
    /// has moved since is checked out afresh rather than served stale.
    versions: Mutex<VecDeque<(String, Arc<Registry>)>>,
}

/// One revision of the repository, ready to answer a request.
struct Version<'a> {
    catalog: &'a Catalog,
    registry: Arc<Registry>,
    store: Option<Box<dyn avc_core::ObjectStore>>,
}

/// One object a download will send, and the name it is sent under.
struct Item {
    name: String,
    object: avc_core::ObjectId,
    size: u64,
}

impl Catalog {
    /// The branches and tags, listed at most once per [`REFS_TTL`].
    fn refs(&self) -> Result<Arc<git::Refs>, Failure> {
        if let Some((taken, refs)) = &*self.refs.lock().unwrap() {
            if taken.elapsed() < REFS_TTL {
                return Ok(Arc::clone(refs));
            }
        }
        if let Some(mirror) = &self.mirror {
            mirror.update()?;
        }
        let refs = Arc::new(git::list_refs(&self.source().display().to_string())?);
        *self.refs.lock().unwrap() = Some((Instant::now(), Arc::clone(&refs)));
        Ok(refs)
    }

    /// The local Git repository that history and revisions are read from.
    fn source(&self) -> &Path {
        match (&self.mirror, &self.root) {
            (Some(mirror), _) => mirror.path(),
            (None, Some(root)) => root,
            (None, None) => unreachable!("a catalog is either a checkout or a mirror"),
        }
    }

    /// The repository at `reference`, or at the default when it is empty.
    ///
    /// Only a name the repository advertises, the default the server was
    /// started with, or something shaped like a commit id is accepted. That is
    /// what keeps a request from handing Git an argument of its own choosing —
    /// a "revision" of `--upload-pack=…` is a command to run.
    fn version(&self, reference: &str) -> Result<Version<'_>, Failure> {
        let reference = if reference.is_empty() {
            self.default.as_str()
        } else {
            reference
        };
        let registry = match (reference, &self.worktree) {
            ("", Some(worktree)) => Arc::clone(worktree),
            _ => self.checkout(reference)?,
        };
        let store = if self.remote_url.is_none() && registry.repo().config.remotes.is_empty() {
            // Nothing to fetch from; the local cache is all there is.
            None
        } else {
            Some(registry.store(self.remote_url.as_deref(), self.remote.as_deref())?)
        };
        Ok(Version {
            catalog: self,
            registry,
            store,
        })
    }

    /// The pointers at a named revision, checked out once and then reused.
    fn checkout(&self, reference: &str) -> Result<Arc<Registry>, Failure> {
        let listed = match self.refs() {
            Ok(refs) => refs.resolve(reference),
            // An unreachable remote should not stop the default from loading;
            // opening it below reports the real problem if there is one.
            Err(_) if reference == self.default => None,
            Err(error) => return Err(error),
        };
        let (fetch, key) = match listed {
            Some((fetch, commit)) => {
                let key = format!("{fetch}@{commit}");
                (fetch, key)
            }
            None if reference == self.default || git::could_be_commit_id(reference) => {
                (reference.to_owned(), reference.to_owned())
            }
            None => return Err(format!("no branch, tag, or commit named `{reference}`").into()),
        };
        if fetch.starts_with('-') {
            return Err(format!("no branch, tag, or commit named `{reference}`").into());
        }

        {
            let mut versions = self.versions.lock().unwrap();
            if let Some(index) = versions.iter().position(|(name, _)| *name == key) {
                let entry = versions.remove(index).expect("the index was just found");
                let registry = Arc::clone(&entry.1);
                versions.push_back(entry);
                return Ok(registry);
            }
        }
        // Checked out without the lock held: a fetch can take seconds, and
        // other versions should not wait on it. Two requests racing for the
        // same revision both check it out, and one copy is kept.
        let registry = Arc::new(match &self.root {
            Some(root) => Registry::from_revision(root.clone(), &fetch)?,
            None => {
                Registry::from_git_via(&self.source().display().to_string(), &self.url, &fetch)?
            }
        });
        let mut versions = self.versions.lock().unwrap();
        if !versions.iter().any(|(name, _)| *name == key) {
            versions.push_back((key, Arc::clone(&registry)));
            // Dropping a registry deletes its checkout, but only once the last
            // request still reading from it has finished.
            while versions.len() > MAX_VERSIONS {
                versions.pop_front();
            }
        }
        Ok(registry)
    }

    /// The commit graph a visitor picks a version from: every commit reachable
    /// from a branch or tag, newest first, with the branches and tags that
    /// point at each.
    fn history_json(&self) -> Result<String, Failure> {
        let refs = self.refs()?;
        let commits = git::history(self.source(), MAX_HISTORY)?;
        let mut labels: HashMap<&str, (Vec<&str>, Vec<&str>)> = HashMap::new();
        for (name, commit) in &refs.branches {
            labels.entry(commit).or_default().0.push(name);
        }
        for (name, commit) in &refs.tags {
            labels.entry(commit).or_default().1.push(name);
        }
        let names = |list: &[&str]| {
            list.iter()
                .map(|name| json(name))
                .collect::<Vec<_>>()
                .join(",")
        };
        let rows: Vec<String> = commits
            .iter()
            .map(|commit| {
                let (branches, tags) = labels
                    .get(commit.hash.as_str())
                    .cloned()
                    .unwrap_or_default();
                format!(
                    "{{\"hash\":{},\"parents\":[{}],\"author\":{},\"time\":{},\"subject\":{},\"branches\":[{}],\"tags\":[{}]}}",
                    json(&commit.hash),
                    commit.parents.iter().map(|parent| json(parent)).collect::<Vec<_>>().join(","),
                    json(&commit.author),
                    commit.time,
                    json(&commit.subject),
                    names(&branches),
                    names(&tags),
                )
            })
            .collect();
        // The working tree sits on top of whatever the checkout has checked
        // out, which is where a visitor expects to find it.
        let worktree = match (&self.worktree, &self.root) {
            (Some(_), Some(root)) => format!(
                "{{\"head\":{}}}",
                git::head_commit(root).map_or("null".to_owned(), |commit| json(&commit))
            ),
            _ => "null".to_owned(),
        };
        Ok(format!(
            "{{\"default\":{},\"worktree\":{worktree},\"head\":{},\"truncated\":{},\"commits\":[{}]}}",
            json(&self.default),
            refs.head.as_deref().map_or("null".to_owned(), json),
            commits.len() >= MAX_HISTORY,
            rows.join(","),
        ))
    }

    /// The branches and tags a visitor can choose between.
    fn refs_json(&self) -> Result<String, Failure> {
        let refs = self.refs()?;
        let list = |entries: &[(String, String)]| {
            entries
                .iter()
                .map(|(name, commit)| {
                    format!(
                        "{{\"name\":{},\"commit\":{}}}",
                        json(name),
                        json(&commit.chars().take(12).collect::<String>())
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        Ok(format!(
            "{{\"default\":{},\"worktree\":{},\"head\":{},\"branches\":[{}],\"tags\":[{}]}}",
            json(&self.default),
            self.worktree.is_some(),
            refs.head.as_deref().map_or("null".to_owned(), json),
            list(&refs.branches),
            list(&refs.tags),
        ))
    }
}

impl Version<'_> {
    fn store(&self) -> Option<&dyn avc_core::ObjectStore> {
        self.store.as_deref()
    }

    /// Where `object` would be in the local cache.
    ///
    /// A revision of a checkout reads its pointers from a temporary directory,
    /// but its bytes are still in the checkout's cache, so that is where to
    /// look.
    fn cache_file(&self, object: &avc_core::ObjectId) -> PathBuf {
        self.registry
            .worktree()
            .unwrap_or(&self.registry.repo().root)
            .join(".avc/cache")
            .join(object.cache_key())
    }

    /// Whether the local cache holds `object`.
    fn cached(&self, object: &avc_core::ObjectId) -> bool {
        self.cache_file(object).is_file()
    }

    /// Every object hash the store holds, in one listing.
    fn present(&self) -> Result<HashSet<String>, Failure> {
        Ok(match self.store() {
            Some(store) => store
                .list()?
                .into_iter()
                .map(|found| found.object.hash().to_owned())
                .collect(),
            None => HashSet::new(),
        })
    }

    /// A stream of `object`'s bytes, from the cache if it is there and the
    /// store otherwise. Unverified; see [`copy_verified`].
    fn open(&self, object: &avc_core::ObjectId, label: &str) -> Result<Box<dyn Read>, Failure> {
        let cached = self.cache_file(object);
        if cached.is_file() {
            return Ok(Box::new(
                std::fs::File::open(cached).map_err(crate::io_error)?,
            ));
        }
        match self.store() {
            Some(store) => Ok(store.get(object)?),
            None => Err(format!(
                "{label} is not in the local cache, and no object store is configured"
            )
            .into()),
        }
    }

    /// A directory's manifest, read and verified once, then remembered.
    fn tree(&self, pointer: &avc_core::Pointer) -> Result<avc_core::Tree, Failure> {
        let object = pointer.object_id()?;
        if let Some(tree) = self.catalog.trees.lock().unwrap().get(object.hash()) {
            return Ok(tree.clone());
        }
        let mut bytes = Vec::new();
        self.open(&object, &pointer.path)?
            .read_to_end(&mut bytes)
            .map_err(crate::io_error)?;
        let actual = avc_core::hash_reader(&mut bytes.as_slice(), pointer.algorithm())?;
        if actual.size != pointer.object.size || actual.object != object {
            return Err(format!(
                "the manifest for {} does not match its pointer",
                pointer.path
            )
            .into());
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("directory manifest for {} is not UTF-8", pointer.path))?;
        let tree = avc_core::Tree::parse(&text)?;
        self.catalog
            .trees
            .lock()
            .unwrap()
            .insert(object.hash().to_owned(), tree.clone());
        Ok(tree)
    }

    /// Every artifact, with its size and whether it can be downloaded.
    fn catalog_json(&self) -> Result<String, Failure> {
        let artifacts = self.registry.artifacts()?;
        // One listing answers every artifact, exactly as it does for `avc list`.
        let present = self.present()?;
        let reachable =
            |object: &avc_core::ObjectId| present.contains(object.hash()) || self.cached(object);

        let mut rows = Vec::with_capacity(artifacts.len());
        for pointer in &artifacts {
            let object = pointer.object_id()?;
            let (size, files, available) = if pointer.is_directory() {
                // A manifest nobody holds leaves the directory's contents
                // unknowable; that is a missing artifact, not a broken page.
                match reachable(&object).then(|| self.tree(pointer)) {
                    Some(Ok(tree)) => {
                        let complete = tree
                            .entries
                            .iter()
                            .all(|entry| entry.object_id().is_ok_and(|object| reachable(&object)));
                        (Some(tree.total_size()), Some(tree.entries.len()), complete)
                    }
                    _ => (None, None, false),
                }
            } else {
                (Some(pointer.object.size), None, reachable(&object))
            };
            rows.push(format!(
                "{{\"path\":{},\"kind\":\"{}\",\"size\":{},\"files\":{},\"object\":{},\"available\":{available}}}",
                json(&pointer.path),
                if pointer.is_directory() { "directory" } else { "file" },
                optional(size),
                optional(files),
                json(&object.to_string()),
            ));
        }
        Ok(format!(
            "{{\"repository\":{},\"commit\":{},\"store\":{},\"artifacts\":[{}]}}",
            json(self.registry.describe()),
            self.registry
                .commit()
                .map_or("null".to_owned(), |commit| json(&commit)),
            self.store()
                .map_or("null".to_owned(), |store| json(&store.describe())),
            rows.join(",")
        ))
    }

    /// The files inside one tracked directory.
    fn tree_json(&self, path: &str) -> Result<String, Failure> {
        let wanted = registry::normalize_selector(path)?;
        let pointer = self
            .registry
            .artifacts()?
            .into_iter()
            .find(|pointer| pointer.path == wanted && pointer.is_directory())
            .ok_or_else(|| Failure::from(format!("no tracked directory at {wanted}")))?;
        let present = self.present()?;
        let mut rows = Vec::new();
        for entry in self.tree(&pointer)?.entries {
            let object = entry.object_id()?;
            let available = present.contains(object.hash()) || self.cached(&object);
            rows.push(format!(
                "{{\"path\":{},\"size\":{},\"object\":{},\"available\":{available}}}",
                json(&entry.path),
                entry.size,
                json(&object.to_string()),
            ));
        }
        Ok(format!(
            "{{\"path\":{},\"entries\":[{}]}}",
            json(&pointer.path),
            rows.join(",")
        ))
    }

    /// Resolve a download to the objects it sends.
    ///
    /// Returns the items and whether they are one file, sent as itself, rather
    /// than a set to be archived.
    fn plan(&self, path: &str) -> Result<(Vec<Item>, bool), Failure> {
        let selected = self.registry.select(&[path.to_owned()])?;
        let mut items = Vec::new();
        let mut single = selected.len() == 1;
        for selection in &selected {
            let pointer = &selection.pointer;
            if !pointer.is_directory() {
                items.push(Item {
                    name: selection.destination(&pointer.path),
                    object: pointer.object_id()?,
                    size: pointer.object.size,
                });
                continue;
            }
            let before = items.len();
            for entry in self.tree(pointer)?.entries {
                if !selection.includes(&entry.path) {
                    continue;
                }
                let repository_path = format!("{}/{}", pointer.path, entry.path);
                // One file named inside a directory is sent as that file.
                single &= selection.inside.as_deref() == Some(entry.path.as_str());
                items.push(Item {
                    name: selection.destination(&repository_path),
                    object: entry.object_id()?,
                    size: entry.size,
                });
            }
            if items.len() == before {
                return Err(registry::missing_inside(selection).into());
            }
        }
        let single = single && items.len() == 1;
        Ok((items, single))
    }
}

/// Answer one connection, logging a failure rather than letting it end the
/// server.
fn handle(catalog: &Catalog, mut stream: TcpStream, loopback: bool) {
    // A client that opens a connection and says nothing should not hold a
    // thread forever. Downloads are exempt: the timeout is on reading.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(message) => {
            let _ = respond_error(&mut stream, 400, &message);
            return;
        }
    };
    if loopback && !request.host_is_local() {
        // A page on some other site can make a browser send requests here; if
        // it also controls a DNS name that resolves to 127.0.0.1, it could read
        // the responses. Refusing any Host but our own closes that door.
        let _ = respond_error(&mut stream, 403, "unexpected Host header");
        return;
    }
    if request.method != "GET" {
        let _ = respond_error(&mut stream, 405, "only GET is supported");
        return;
    }
    // Set once a download's headers are out: from then on an error can only
    // be signalled by cutting the response short, never by a second response
    // whose bytes the client would read as the end of the file.
    let mut started = false;
    // Which revision to answer from; empty means the server's default.
    let reference = request.query("ref").unwrap_or_default();
    let outcome = match request.path.as_str() {
        "/" | "/index.html" => respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        )
        .map_err(|error| Failure::from(crate::io_error(error))),
        "/api/history" => catalog
            .history_json()
            .and_then(|body| send_json(&mut stream, &body)),
        "/api/refs" => catalog
            .refs_json()
            .and_then(|body| send_json(&mut stream, &body)),
        "/api/catalog" => catalog
            .version(&reference)
            .and_then(|version| version.catalog_json())
            .and_then(|body| send_json(&mut stream, &body)),
        "/api/tree" => match request.query("path") {
            Some(path) => catalog
                .version(&reference)
                .and_then(|version| version.tree_json(&path))
                .and_then(|body| send_json(&mut stream, &body)),
            None => Err("missing ?path=".into()),
        },
        "/download" => match request.query("path") {
            Some(path) => catalog
                .version(&reference)
                .and_then(|version| download(&version, &mut stream, &path, &mut started)),
            None => Err("missing ?path=".into()),
        },
        _ => {
            let _ = respond_error(&mut stream, 404, "not found");
            return;
        }
    };
    if let Err(failure) = outcome {
        eprintln!(
            "{} {} {}: {}",
            ui::paint_err("avc:", Style::Error),
            request.method,
            request.target,
            failure.message
        );
        if started {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        let status = if failure.code == crate::EXIT_PROVIDER_ERROR {
            502
        } else if failure.message.starts_with("no ") {
            404
        } else {
            400
        };
        let _ = respond_error(&mut stream, status, &failure.message);
    }
}

fn send_json(stream: &mut TcpStream, body: &str) -> Result<(), Failure> {
    respond(stream, 200, "application/json", body.as_bytes()).map_err(|e| crate::io_error(e).into())
}

/// Send the objects a path names: one file as itself, anything else as a tar
/// archive laid out the way `avc fetch` would lay it out.
fn download(
    version: &Version<'_>,
    stream: &mut TcpStream,
    path: &str,
    started: &mut bool,
) -> Result<(), Failure> {
    let (items, single) = version.plan(path)?;
    let wanted = registry::normalize_selector(path)?;
    let base = wanted.rsplit('/').next().unwrap_or("artifacts").to_owned();

    if single {
        let item = &items[0];
        let mut body = version.open(&item.object, &item.name)?;
        *started = true;
        write_head(
            stream,
            item.size,
            "application/octet-stream",
            &attachment(&base),
        )
        .map_err(crate::io_error)?;
        copy_verified(&mut body, stream, &item.object, item.size, &wanted)?;
    } else {
        let archive = format!("{base}.tar");
        let length = items
            .iter()
            .map(|item| tar::entry_length(&item.name, item.size))
            .sum::<u64>()
            + tar::TRAILER;
        *started = true;
        write_head(stream, length, "application/x-tar", &attachment(&archive))
            .map_err(crate::io_error)?;
        for item in &items {
            // Opened before its header is written, so a missing object fails
            // before any of its bytes are promised.
            let mut body = version.open(&item.object, &item.name)?;
            stream
                .write_all(&tar::header(&item.name, item.size))
                .map_err(crate::io_error)?;
            copy_verified(&mut body, stream, &item.object, item.size, &item.name)?;
            stream
                .write_all(&tar::padding(item.size))
                .map_err(crate::io_error)?;
        }
        stream
            .write_all(&[0; tar::TRAILER as usize])
            .map_err(crate::io_error)?;
    }
    let total: u64 = items.iter().map(|item| item.size).sum();
    let at = version.registry.commit().map_or_else(
        || "working tree".to_owned(),
        |commit| format!("at {commit}"),
    );
    ui::action(
        "served",
        Style::Ok,
        &wanted,
        Some(&format!(
            "{}, {}, {at}",
            ui::plural(items.len(), "file"),
            ui::size(total)
        )),
    );
    Ok(())
}

/// Stream an object to `output`, verifying it as it goes.
///
/// The most recent chunk is always held back, and only written once the whole
/// object has hashed to what the pointer says. A corrupt or truncated object
/// therefore never delivers its final bytes, the response falls short of the
/// length its headers promised, and the client reports a failed download
/// instead of saving the wrong file.
fn copy_verified(
    body: &mut dyn Read,
    output: &mut dyn Write,
    expected: &avc_core::ObjectId,
    size: u64,
    label: &str,
) -> Result<(), Failure> {
    let mut hasher = avc_core::StreamHasher::new(expected.algorithm());
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut held: Vec<u8> = Vec::with_capacity(buffer.len());
    let mut read_total = 0_u64;
    loop {
        let read = body.read(&mut buffer).map_err(crate::io_error)?;
        if read == 0 {
            break;
        }
        read_total += read as u64;
        // More bytes than promised would overrun `Content-Length` and corrupt
        // everything after it in an archive; stop before sending any of them.
        if read_total > size {
            return Err(format!("object for {label} is larger than its pointer says").into());
        }
        hasher.update(&buffer[..read]);
        output.write_all(&held).map_err(crate::io_error)?;
        held.clear();
        held.extend_from_slice(&buffer[..read]);
    }
    let actual = hasher.finish()?;
    if actual.size != size || actual.object != *expected {
        return Err(format!("object for {label} does not match its pointer").into());
    }
    output.write_all(&held).map_err(crate::io_error)?;
    Ok(())
}

/// A `Content-Disposition` value that survives any file name: an ASCII
/// fallback for old clients, and the exact UTF-8 name for everyone else.
fn attachment(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        percent_encode(name)
    )
}

/// The parts of a request this server looks at.
struct Request {
    method: String,
    /// The request target as sent, for log lines.
    target: String,
    path: String,
    query: Vec<(String, String)>,
    host: Option<String>,
}

impl Request {
    fn query(&self, key: &str) -> Option<String> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    }

    /// Whether the `Host` header names this machine's loopback interface.
    fn host_is_local(&self) -> bool {
        let Some(host) = &self.host else {
            // HTTP/1.0 clients may omit it; a browser never does.
            return true;
        };
        let name = if let Some(rest) = host.strip_prefix('[') {
            rest.split(']').next().unwrap_or("")
        } else {
            host.rsplit_once(':')
                .map_or(host.as_str(), |(name, _)| name)
        };
        name.eq_ignore_ascii_case("localhost")
            || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.take(MAX_HEAD as u64));
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err("malformed request line".into());
    };
    let (method, target) = (method.to_owned(), target.to_owned());

    let mut host = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).map_err(|e| e.to_string())? == 0 {
            return Err("request head too long or incomplete".into());
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("host") {
                host = Some(value.trim().to_owned());
            }
        }
    }

    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((percent_decode(name)?, percent_decode(value)?))
        })
        .collect::<Result<_, String>>()?;
    Ok(Request {
        method,
        path: percent_decode(path)?,
        target,
        query,
        host,
    })
}

fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = text
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    .ok_or("malformed percent-encoding")?;
                decoded.push(hex);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|_| "request is not UTF-8".to_owned())
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        502 => "Bad Gateway",
        _ => "Error",
    }
}

fn write_head(
    stream: &mut TcpStream,
    length: u64,
    content_type: &str,
    disposition: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\n\
         Content-Disposition: {disposition}\r\nX-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    )
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; \
         style-src 'unsafe-inline'; connect-src 'self'; img-src data:\r\n\
         Connection: close\r\n\r\n",
        status_text(status),
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_error(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    let body = format!("{{\"error\":{}}}", json(message));
    respond(stream, status, "application/json", body.as_bytes())
}

/// A JSON string literal.
fn json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or("null".to_owned(), |value| value.to_string())
}

/// Just enough of the tar format to archive regular files.
///
/// Each file is a 512-byte ustar header followed by its bytes padded to a
/// multiple of 512. A path too long for the header's 100-byte name field, or a
/// file too large for its 11-digit octal size, gets a PAX extended header
/// carrying the real value first — which every tar in use today understands.
/// The archive's length is known before a byte is sent, so a download can
/// promise a `Content-Length` and a browser can show real progress.
mod tar {
    const BLOCK: u64 = 512;
    /// Two zero blocks end an archive.
    pub const TRAILER: u64 = 2 * BLOCK;
    /// The largest size the ustar header's octal field can hold.
    const MAX_USTAR_SIZE: u64 = 0o77_777_777_777;

    fn padded(size: u64) -> u64 {
        size.div_ceil(BLOCK) * BLOCK
    }

    /// PAX records needed for this entry, if any.
    fn extended(name: &str, size: u64) -> Option<Vec<u8>> {
        let mut records = Vec::new();
        if name.len() > 100 {
            records.extend(record("path", name));
        }
        if size > MAX_USTAR_SIZE {
            records.extend(record("size", &size.to_string()));
        }
        (!records.is_empty()).then_some(records)
    }

    /// One `<length> <key>=<value>\n` record, whose length counts itself.
    fn record(key: &str, value: &str) -> Vec<u8> {
        let body = format!(" {key}={value}\n");
        let mut length = body.len() + 1;
        while length.to_string().len() + body.len() != length {
            length += 1;
        }
        format!("{length}{body}").into_bytes()
    }

    /// Bytes this entry occupies in the archive.
    pub fn entry_length(name: &str, size: u64) -> u64 {
        let pax = extended(name, size).map_or(0, |records| BLOCK + padded(records.len() as u64));
        pax + BLOCK + padded(size)
    }

    /// Everything that precedes a file's bytes.
    pub fn header(name: &str, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(records) = extended(name, size) {
            out.extend(block(b"PaxHeader", records.len() as u64, b'x'));
            let length = records.len() as u64;
            out.extend(records);
            out.extend(padding(length));
        }
        out.extend(block(name.as_bytes(), size.min(MAX_USTAR_SIZE), b'0'));
        out
    }

    /// Zeroes that bring `size` bytes up to a block boundary.
    pub fn padding(size: u64) -> Vec<u8> {
        vec![0; (padded(size) - size) as usize]
    }

    fn block(name: &[u8], size: u64, kind: u8) -> [u8; BLOCK as usize] {
        let mut header = [0_u8; BLOCK as usize];
        let name = &name[..name.len().min(100)];
        header[..name.len()].copy_from_slice(name);
        octal(&mut header[100..108], 0o644);
        octal(&mut header[108..116], 0);
        octal(&mut header[116..124], 0);
        octal(&mut header[124..136], size);
        octal(&mut header[136..148], 0);
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        // The checksum is computed with its own field read as spaces.
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        octal(&mut header[148..155], u64::from(sum));
        header[155] = b' ';
        header
    }

    /// Zero-padded octal filling all but the last byte, which stays NUL.
    fn octal(field: &mut [u8], value: u64) {
        let digits = field.len() - 1;
        let text = format!("{value:0digits$o}");
        field[..digits].copy_from_slice(&text.as_bytes()[text.len() - digits..]);
        field[digits] = 0;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_pax_record_counts_its_own_length() {
            let record = record("path", "a");
            assert_eq!(record, b"9 path=a\n");
            // Crossing a digit boundary is the case that is easy to get wrong.
            let long = super::record("path", &"x".repeat(90));
            assert_eq!(
                long.len().to_string(),
                String::from_utf8(long.clone())
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
            );
        }

        #[test]
        fn entry_length_matches_what_is_written() {
            for (name, size) in [("a.bin", 0_u64), ("b", 513), (&*"n/".repeat(80), 7)] {
                let written = header(name, size).len() as u64 + padded(size);
                assert_eq!(entry_length(name, size), written, "{name}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_escape_what_json_requires() {
        assert_eq!(json("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
        assert_eq!(json("données/模型.bin"), "\"données/模型.bin\"");
    }

    #[test]
    fn percent_encoding_round_trips_any_path() {
        for path in [
            "models/bert/weights.bin",
            "a b+c",
            "données/模型.bin",
            "100%",
        ] {
            assert_eq!(percent_decode(&percent_encode(path)).unwrap(), path);
        }
        assert!(percent_decode("%zz").is_err());
        assert!(percent_decode("%4").is_err());
    }

    #[test]
    fn only_a_loopback_host_is_local() {
        let request = |host: &str| Request {
            method: "GET".into(),
            target: "/".into(),
            path: "/".into(),
            query: Vec::new(),
            host: Some(host.into()),
        };
        for host in [
            "localhost:8080",
            "127.0.0.1:8080",
            "[::1]:8080",
            "LOCALHOST",
        ] {
            assert!(request(host).host_is_local(), "{host}");
        }
        for host in ["evil.example:8080", "10.0.0.1", "[2001:db8::1]:80"] {
            assert!(!request(host).host_is_local(), "{host}");
        }
    }

    /// A corrupt object must never deliver its last bytes, so the response
    /// falls short of its promised length and the client sees a failure.
    #[test]
    fn a_corrupt_object_withholds_its_final_chunk() {
        let good = vec![b'a'; 200 * 1024];
        let object = avc_core::hash_reader(&mut good.as_slice(), avc_core::Algorithm::Sha256)
            .unwrap()
            .object;

        let mut output = Vec::new();
        copy_verified(
            &mut good.as_slice(),
            &mut output,
            &object,
            good.len() as u64,
            "x",
        )
        .unwrap();
        assert_eq!(output, good);

        let mut bad = good.clone();
        *bad.last_mut().unwrap() = b'b';
        let mut output = Vec::new();
        assert!(copy_verified(
            &mut bad.as_slice(),
            &mut output,
            &object,
            bad.len() as u64,
            "x"
        )
        .is_err());
        assert!(output.len() < bad.len());

        // Longer than promised is refused before it overruns the response.
        let mut longer = good.clone();
        longer.push(b'a');
        let mut output = Vec::new();
        assert!(copy_verified(
            &mut longer.as_slice(),
            &mut output,
            &object,
            good.len() as u64,
            "x"
        )
        .is_err());
        assert!(output.len() <= good.len());
    }
}
