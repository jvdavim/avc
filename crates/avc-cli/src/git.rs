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
fn could_be_commit_id(revision: &str) -> bool {
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
