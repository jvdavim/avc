//! Reading a repository's pointers out of Git.
//!
//! An AVC repository is two halves that live in different places: Git holds the
//! pointers and `.avc/config.toml`, and an object store holds the bytes. A
//! consumer only ever needs to name the first — the pointer says which object to
//! fetch, and the configuration says which store to fetch it from. That is why
//! `avc fetch` takes a Git URL and a path rather than a bucket: the bucket is
//! the repository's business, set up once by whoever runs `avc remote add`.
//!
//! What lands in the checkout below is text. Artifacts are gitignored, so a
//! shallow checkout of an artifact registry is its pointer files and its
//! configuration — kilobytes — and the bytes are fetched afterwards, from the
//! object store, only for the paths that were asked for.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Failure;

/// A temporary checkout of one commit, removed when it goes out of scope.
pub(crate) struct Checkout {
    path: PathBuf,
    /// The commit that was actually checked out, so a log records what a
    /// moving revision resolved to on this run.
    commit: String,
}

impl Checkout {
    /// Fetch `revision` from `url` and check it out.
    ///
    /// A revision is anything that names one commit on the far side: a branch,
    /// a tag, `HEAD` for the default branch, a fully qualified `refs/…` name
    /// when a branch and a tag share one, or a commit id. Whichever it is, what
    /// lands here is a detached checkout of exactly one commit.
    ///
    /// Nearly always that costs a single depth-1 fetch. The exception is an
    /// abbreviated commit id, which no server can look up — a prefix is not a
    /// name, and resolving one means having the objects to search. That case
    /// falls back to [`fetch_history`], which is why a pipeline should name a
    /// branch, a tag, or a full commit id rather than a short one.
    pub(crate) fn at(url: &str, revision: &str) -> Result<Self, Failure> {
        let path = temporary_path();
        fs::create_dir_all(&path).map_err(crate::io_error)?;
        // Built before the first Git call, so a failure part-way through still
        // removes the directory on the way out.
        let mut checkout = Self {
            path,
            commit: String::new(),
        };

        git(&checkout.path, &["init", "--quiet"])?;
        git(&checkout.path, &["remote", "add", "origin", url])?;

        let target = match fetch_one(&checkout.path, revision) {
            Ok(()) => "FETCH_HEAD".to_owned(),
            // The server did not recognize the name. If it could be a commit
            // id, that is expected rather than an error: a prefix is never
            // advertised, and a full id is only fetchable directly on a server
            // configured to allow it. Anything else — unreachable, unauthorized,
            // no `git` — is reported as it happened.
            Err(error) if could_be_commit_id(revision) && is_unknown_ref(&error.to_string()) => {
                fetch_history(&checkout.path, url, revision)?;
                resolve_commit(&checkout.path, url, revision)?
            }
            Err(error) => return Err(explain(error, url, revision)),
        };

        git(
            &checkout.path,
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                &target,
            ],
        )?;

        checkout.commit = git(&checkout.path, &["rev-parse", "HEAD"])?
            .trim()
            .to_owned();
        Ok(checkout)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The commit this checkout resolved to, abbreviated.
    pub(crate) fn commit(&self) -> String {
        self.commit.chars().take(12).collect()
    }
}

/// One commit, no tags, no history: everything needed to read a pointer and
/// nothing else.
fn fetch_one(directory: &Path, revision: &str) -> Result<(), Failure> {
    git(
        directory,
        &[
            "fetch",
            "--depth",
            "1",
            "--no-tags",
            "--quiet",
            "origin",
            revision,
        ],
    )
    .map(|_| ())
}

/// Fetch enough history to resolve a commit id locally.
///
/// Every branch and tag, with their commits and trees but not their file
/// contents — an artifact registry's blobs are pointer files of a few hundred
/// bytes each, but its history can be long, and none of those blobs is needed
/// to find a commit. A server that does not support filtering says so and sends
/// them anyway, which is a slower success rather than a failure.
fn fetch_history(directory: &Path, url: &str, revision: &str) -> Result<(), Failure> {
    git(
        directory,
        &[
            "fetch",
            "--filter=blob:none",
            "--tags",
            "--quiet",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )
    .map(|_| ())
    .map_err(|error| explain(error, url, revision))
}

/// Turn a commit id — abbreviated or whole — into the commit it names.
fn resolve_commit(directory: &Path, url: &str, revision: &str) -> Result<String, Failure> {
    // `^{commit}` is what makes this reject a prefix that happens to match a
    // tree or a blob, rather than checking out something that is not a commit.
    git(
        directory,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .map(|commit| commit.trim().to_owned())
    .map_err(|_| {
        let url = redact(url);
        Failure::provider(format!(
            "no commit in {url} matches `{revision}`; an abbreviated id has to \
             be unambiguous and on a branch or tag, so name more of it, or name \
             the branch or tag instead"
        ))
    })
}

/// Whether `revision` could be a commit id, whole or abbreviated.
///
/// Git's own lower bound is four characters, below which a prefix would match
/// most of any repository. A name made only of hex characters — `dad`, `beef`,
/// a branch called `abc123` — is ambiguous by construction, and is treated as a
/// ref first: this is only ever consulted after the server has said it has no
/// such ref.
pub(crate) fn could_be_commit_id(revision: &str) -> bool {
    (4..=40).contains(&revision.len()) && revision.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether a failed fetch means "no such name here", as opposed to a transport
/// or credential failure that deserves to be reported exactly as it happened.
fn is_unknown_ref(message: &str) -> bool {
    // The first is what a server says about a name it does not advertise; the
    // other two are what it says about a commit id it will not serve directly,
    // which is the default for anything but the major hosts.
    [
        "couldn't find remote ref",
        "unadvertised object",
        "not our ref",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// A bare copy of a remote repository's branches, tags, and history.
///
/// A pointer registry is text, so a full copy of it is small, and holding one
/// turns every question a history browser asks — which commits exist, what
/// their parents are, what a given commit's pointers said — into a local one.
/// Removed when it goes out of scope.
pub(crate) struct Mirror {
    path: PathBuf,
    url: String,
}

impl Mirror {
    /// Copy `url`'s branches and tags into a temporary bare repository.
    pub(crate) fn clone(url: &str) -> Result<Self, Failure> {
        let mirror = Self {
            path: temporary_path(),
            url: url.to_owned(),
        };
        let target = mirror.path.display().to_string();
        git(
            &std::env::temp_dir(),
            &["clone", "--bare", "--quiet", "--", url, &target],
        )
        .map_err(|error| explain(error, url, "its history"))?;
        Ok(mirror)
    }

    /// Bring the copy up to date: new commits, moved branches, new tags, and
    /// branches or tags since deleted.
    pub(crate) fn update(&self) -> Result<(), Failure> {
        git(
            &self.path,
            &[
                "fetch",
                "--quiet",
                "--prune",
                "origin",
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ],
        )
        .map(|_| ())
        .map_err(|error| explain(error, &self.url, "its history"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// One commit, as a history browser draws it.
#[derive(Debug)]
pub(crate) struct Commit {
    pub(crate) hash: String,
    pub(crate) parents: Vec<String>,
    pub(crate) author: String,
    /// Seconds since the Unix epoch.
    pub(crate) time: i64,
    pub(crate) subject: String,
}

/// Up to `limit` commits reachable from any branch or tag of the repository in
/// `directory` — and from `HEAD`, which in a checkout may be detached — newest
/// first, every child ahead of its parents.
pub(crate) fn history(directory: &Path, limit: usize) -> Result<Vec<Commit>, Failure> {
    let limit = limit.to_string();
    let mut arguments = vec![
        "log",
        "--branches",
        "--tags",
        "--topo-order",
        "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s%x1e",
        "-n",
        &limit,
    ];
    // A repository with no commits has no `HEAD` to name, and saying so is
    // an error to `git log` rather than an empty history.
    if head_commit(directory).is_some() {
        arguments.push("HEAD");
    }
    let output = git(directory, &arguments)?;
    Ok(parse_history(&output))
}

fn parse_history(output: &str) -> Vec<Commit> {
    output
        .split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\x1f');
            let hash = fields.next().filter(|hash| !hash.is_empty())?.to_owned();
            let parents = fields
                .next()?
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let author = fields.next()?.to_owned();
            let time = fields.next()?.parse().unwrap_or(0);
            let subject = fields.next().unwrap_or("").to_owned();
            Some(Commit {
                hash,
                parents,
                author,
                time,
                subject,
            })
        })
        .collect()
}

/// The commit `HEAD` names in `directory`, if it names one.
pub(crate) fn head_commit(directory: &Path) -> Option<String> {
    git(directory, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .ok()
        .map(|commit| commit.trim().to_owned())
        .filter(|commit| !commit.is_empty())
}

/// The branches and tags a repository advertises, and its default branch.
#[derive(Debug, Default)]
pub(crate) struct Refs {
    /// The branch `HEAD` points at, when the repository says.
    pub(crate) head: Option<String>,
    /// `(name, commit)`, by short name: `main`, not `refs/heads/main`.
    pub(crate) branches: Vec<(String, String)>,
    /// `(name, commit)`. An annotated tag is listed with the commit it tags,
    /// not the tag object, so it compares equal to a branch at the same commit.
    pub(crate) tags: Vec<(String, String)>,
}

impl Refs {
    /// What to fetch for `name`, and the commit it names right now.
    ///
    /// A short name that is both a branch and a tag is ambiguous on the wire,
    /// so it is answered with the fully qualified name of the tag — Git's own
    /// preference when it resolves one locally.
    pub(crate) fn resolve(&self, name: &str) -> Option<(String, String)> {
        let find = |list: &[(String, String)], wanted: &str| {
            list.iter()
                .find(|(candidate, _)| candidate == wanted)
                .map(|(_, commit)| commit.clone())
        };
        if let Some(branch) = name.strip_prefix("refs/heads/") {
            return find(&self.branches, branch).map(|commit| (name.to_owned(), commit));
        }
        if let Some(tag) = name.strip_prefix("refs/tags/") {
            return find(&self.tags, tag).map(|commit| (name.to_owned(), commit));
        }
        if name == "HEAD" {
            let head = self.head.as_deref()?;
            return find(&self.branches, head).map(|commit| ("HEAD".to_owned(), commit));
        }
        match (find(&self.tags, name), find(&self.branches, name)) {
            (Some(commit), Some(_)) => Some((format!("refs/tags/{name}"), commit)),
            (Some(commit), None) | (None, Some(commit)) => Some((name.to_owned(), commit)),
            (None, None) => None,
        }
    }
}

/// List the branches and tags at `url`, without fetching anything else.
pub(crate) fn list_refs(url: &str) -> Result<Refs, Failure> {
    let directory = std::env::temp_dir();
    let listing = git(&directory, &["ls-remote", "--heads", "--tags", url])
        .map_err(|error| Failure::provider(format!("{error}\n  while listing {}", redact(url))))?;
    let mut refs = parse_refs(&listing);
    // Only asked for separately because `--heads --tags` filters `HEAD` out.
    // A repository with no default branch is not an error, just unlabelled.
    if let Ok(symref) = git(&directory, &["ls-remote", "--symref", url, "HEAD"]) {
        refs.head = symref.lines().find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .split('\t')
                .next()
                .map(str::to_owned)
        });
    }
    Ok(refs)
}

/// Read `git ls-remote` output into branches and tags.
fn parse_refs(listing: &str) -> Refs {
    let mut refs = Refs::default();
    let mut peeled = std::collections::HashMap::new();
    for line in listing.lines() {
        let Some((commit, name)) = line.split_once('\t') else {
            continue;
        };
        if let Some(branch) = name.strip_prefix("refs/heads/") {
            refs.branches.push((branch.to_owned(), commit.to_owned()));
        } else if let Some(tag) = name.strip_prefix("refs/tags/") {
            match tag.strip_suffix("^{}") {
                Some(tag) => {
                    peeled.insert(tag.to_owned(), commit.to_owned());
                }
                None => refs.tags.push((tag.to_owned(), commit.to_owned())),
            }
        }
    }
    for (tag, commit) in &mut refs.tags {
        if let Some(target) = peeled.remove(tag.as_str()) {
            *commit = target;
        }
    }
    refs
}

/// Say what was being looked for and where, and translate Git's own words for
/// a name it could not find into ours.
fn explain(error: Failure, url: &str, revision: &str) -> Failure {
    let message = error.to_string();
    if is_unknown_ref(&message) {
        return Failure::provider(format!(
            "no branch, tag, or commit named `{revision}` in {}",
            redact(url)
        ));
    }
    Failure::provider(format!(
        "{message}\n  while reading {revision} from {}",
        redact(url)
    ))
}

impl Drop for Checkout {
    fn drop(&mut self) {
        // A failed cleanup is not worth failing a command over; the directory
        // is under the system temporary root either way.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Run one Git command, returning its standard output.
///
/// Failures are provider failures — `SPEC.md`'s exit code 3 — because they are
/// almost always operational: an unreachable host, a missing credential, a
/// reference that does not exist on the server.
fn git(directory: &Path, arguments: &[&str]) -> Result<String, Failure> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        // Without this, Git in a pipeline with no credentials waits forever on
        // a password prompt nobody will ever type into.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| {
            Failure::provider(format!(
                "could not run git: {error}; \
                 reading pointers from a Git URL requires the git command"
            ))
        })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        let message = if message.is_empty() {
            "no output".to_owned()
        } else {
            redact(message)
        };
        return Err(Failure::provider(format!(
            "git {} failed: {message}",
            arguments.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A private, self-cleaning directory for one checkout.
fn temporary_path() -> PathBuf {
    let unique = format!(
        "avc-registry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    );
    std::env::temp_dir().join(unique)
}

/// Remove any `user:password@` from a URL before it reaches a log.
///
/// A token pasted into a clone URL is a common way to authenticate in CI, and
/// an error message is not a good place for it to end up.
pub(crate) fn redact(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("://") {
        let (before, after) = rest.split_at(start + 3);
        output.push_str(before);
        // Userinfo, when present, runs to the first `@` and cannot contain a
        // `/`, so anything after a slash is already the path.
        let authority_end = after.find('/').unwrap_or(after.len());
        match after[..authority_end].find('@') {
            Some(at) => {
                output.push_str("***@");
                rest = &after[at + 1..];
            }
            None => {
                output.push_str(&after[..authority_end]);
                rest = &after[authority_end..];
            }
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_id_is_told_apart_from_a_name_conservatively() {
        assert!(could_be_commit_id("9f2c661b"));
        assert!(could_be_commit_id(
            "9f2c661b30c2bb9a00dfa53556c84e1c13ea69a3"
        ));
        // Shorter than Git will resolve, and longer than a commit id gets.
        assert!(!could_be_commit_id("9f2"));
        assert!(!could_be_commit_id(
            "9f2c661b30c2bb9a00dfa53556c84e1c13ea69a3a"
        ));
        assert!(!could_be_commit_id("main"));
        assert!(!could_be_commit_id("v1.0.0"));
        assert!(!could_be_commit_id("refs/tags/v1.0.0"));
        // A branch may well be named in hex. That costs nothing: this is only
        // consulted once the server has said it has no ref by that name.
        assert!(could_be_commit_id("deadbeef"));
    }

    #[test]
    fn only_an_unrecognized_name_falls_back_to_searching_history() {
        assert!(is_unknown_ref("fatal: couldn't find remote ref 9f2c661b"));
        assert!(is_unknown_ref(
            "error: Server does not allow request for unadvertised object 9f2c661b"
        ));
        // A transport or credential failure means the revision was never
        // looked up at all, so retrying with more of the repository would only
        // fail again, more slowly, with a worse message.
        assert!(!is_unknown_ref(
            "fatal: could not read Username for 'https://host'"
        ));
        assert!(!is_unknown_ref(
            "fatal: unable to access 'https://host/': Could not resolve host"
        ));
    }

    #[test]
    fn history_records_survive_any_subject() {
        let commits = parse_history(
            "aaaa\x1fbbbb cccc\x1fAda\x1f1700000000\x1fMerge: a, b; c\x1e\n\
             bbbb\x1f\x1fAda\x1f1690000000\x1f\x1e\n",
        );
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, ["bbbb", "cccc"]);
        assert_eq!(commits[0].subject, "Merge: a, b; c");
        assert_eq!(commits[0].time, 1_700_000_000);
        // A root commit has no parents, and a commit may have no subject.
        assert!(commits[1].parents.is_empty());
        assert_eq!(commits[1].subject, "");
    }

    #[test]
    fn refs_resolve_the_way_git_would() {
        let refs = parse_refs(
            "aaaa\trefs/heads/main\n\
             bbbb\trefs/heads/v2\n\
             cccc\trefs/tags/v1\n\
             dddd\trefs/tags/v2\n\
             eeee\trefs/tags/v2^{}\n",
        );
        // An annotated tag names the commit it tags, not the tag object.
        assert_eq!(
            refs.tags,
            [("v1".into(), "cccc".into()), ("v2".into(), "eeee".into())]
        );
        assert_eq!(refs.resolve("main"), Some(("main".into(), "aaaa".into())));
        assert_eq!(refs.resolve("v1"), Some(("v1".into(), "cccc".into())));
        // Ambiguous short names go to the tag, spelled so the server agrees.
        assert_eq!(
            refs.resolve("v2"),
            Some(("refs/tags/v2".into(), "eeee".into()))
        );
        assert_eq!(
            refs.resolve("refs/heads/v2"),
            Some(("refs/heads/v2".into(), "bbbb".into()))
        );
        assert_eq!(refs.resolve("missing"), None);
        assert_eq!(refs.resolve("HEAD"), None);
    }

    #[test]
    fn credentials_never_survive_into_a_message() {
        assert_eq!(
            redact("https://user:ghp_secret@github.com/org/repo.git"),
            "https://***@github.com/org/repo.git"
        );
        assert_eq!(
            redact("fatal: could not read https://x-access-token:abc@host/a/b"),
            "fatal: could not read https://***@host/a/b"
        );
        // Nothing to hide, nothing changed.
        assert_eq!(
            redact("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        assert_eq!(
            redact("https://github.com/org/repo"),
            "https://github.com/org/repo"
        );
    }
}
