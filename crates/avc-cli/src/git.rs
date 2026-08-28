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

/// A shallow, temporary checkout of one commit, removed when it goes out of
/// scope.
pub(crate) struct Checkout {
    path: PathBuf,
    /// The commit that was actually checked out, so a log records what a
    /// moving reference resolved to on this run.
    commit: String,
}

impl Checkout {
    /// Fetch `reference` from `url` one commit deep and check it out.
    ///
    /// `reference` may be a branch, a tag, or `HEAD` for the default branch. A
    /// full commit SHA works against servers that allow fetching one directly,
    /// which the major hosts do.
    pub(crate) fn shallow(url: &str, reference: &str) -> Result<Self, Failure> {
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
        // One commit, no tags, no history: everything needed to read a pointer
        // and nothing else.
        git(
            &checkout.path,
            &[
                "fetch",
                "--depth",
                "1",
                "--no-tags",
                "--quiet",
                "origin",
                reference,
            ],
        )
        .map_err(|error| {
            Failure::provider(format!(
                "{error}\n  while fetching {reference} from {}",
                redact(url)
            ))
        })?;
        git(
            &checkout.path,
            &[
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                "FETCH_HEAD",
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
