//! Rewriting a Git history, one commit at a time.
//!
//! The migration does not copy a DVC repository's tip; it replays the whole
//! graph, so every branch, every tag, and every merge that existed in the DVC
//! project exists in the AVC one, with the same authors, the same dates, and
//! the same messages. What changes inside each commit is only this: `.dvc`
//! files become `.avc` pointers, DVC's own files are dropped, and the tracked
//! configuration naming the object store is added.
//!
//! It is done with Git's plumbing rather than by checking commits out. A
//! checkout would write every file of every commit to disk — for a repository
//! with a long history, that is hours of I/O to produce trees that are then
//! immediately discarded. Instead each commit's tree is assembled in a
//! temporary index, which touches only the entries that actually change.
//!
//! Two things cannot survive a rewrite and are not pretended to: a GPG
//! signature, which signs content that no longer exists, and an annotated tag
//! object, whose target commit has a new identity. Signatures are dropped and
//! annotated tags become lightweight ones at the rewritten commit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::git::redact;
use crate::Failure;

/// Where the source repository's refs are parked while the migration runs.
///
/// Outside `refs/heads` and `refs/tags`, so a half-finished migration never
/// leaves something that looks like a branch of the destination repository, and
/// so the originals cannot collide with the rewritten refs.
pub(crate) const SOURCE_NAMESPACE: &str = "refs/avc-migrate";

/// Who made a commit, and when, exactly as Git recorded it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) name: String,
    pub(crate) email: String,
    /// Git's raw date format: seconds since the epoch, then a zone offset.
    /// Kept verbatim so a rewritten commit carries the original instant *and*
    /// the original zone, which a normalized timestamp would lose.
    pub(crate) date: String,
}

/// One commit of the source history.
#[derive(Clone, Debug)]
pub(crate) struct Commit {
    pub(crate) parents: Vec<String>,
    pub(crate) author: Identity,
    pub(crate) committer: Identity,
    /// Bytes, not text: a commit message is whatever the author's editor wrote,
    /// and re-encoding one would change history that is being preserved.
    pub(crate) message: Vec<u8>,
}

/// One file in a commit's tree.
#[derive(Clone, Debug)]
pub(crate) struct TreeFile {
    /// Git's mode, which decides whether the entry is a file, an executable,
    /// or a symlink; carried through untouched.
    pub(crate) mode: String,
    pub(crate) id: String,
    pub(crate) path: String,
}

impl TreeFile {
    pub(crate) fn is_blob(&self) -> bool {
        self.mode != "160000" && self.mode != "040000"
    }
}

/// An edit to make to a commit's tree.
pub(crate) enum Change {
    Add {
        path: String,
        mode: String,
        id: String,
    },
    Remove {
        path: String,
    },
}

/// A Git repository, driven through its plumbing.
pub(crate) struct Git {
    root: PathBuf,
}

impl Git {
    pub(crate) fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create the repository if there is not one here already.
    pub(crate) fn init(&self) -> Result<(), Failure> {
        if self.root.join(".git").exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root).map_err(crate::io_error)?;
        self.run(&["init", "--quiet"])?;
        Ok(())
    }

    pub(crate) fn run(&self, arguments: &[&str]) -> Result<String, Failure> {
        let output = self.output(arguments, None, &[])?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    pub(crate) fn run_bytes(&self, arguments: &[&str]) -> Result<Vec<u8>, Failure> {
        self.output(arguments, None, &[])
    }

    /// Run a command that reads standard input, with extra environment.
    pub(crate) fn run_input(
        &self,
        arguments: &[&str],
        input: &[u8],
        environment: &[(&str, &str)],
    ) -> Result<String, Failure> {
        let output = self.output(arguments, Some(input), environment)?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    fn output(
        &self,
        arguments: &[&str],
        input: Option<&[u8]>,
        environment: &[(&str, &str)],
    ) -> Result<Vec<u8>, Failure> {
        let mut command = Command::new("git");
        command
            .args(arguments)
            .current_dir(&self.root)
            // Without this, a fetch with no credentials waits forever on a
            // password prompt nobody will ever type into.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|error| {
            Failure::provider(format!(
                "could not run git: {error}; migrating a repository requires the git command"
            ))
        })?;
        if let Some(input) = input {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("stdin was piped");
            // A write failure here is almost always the child having exited
            // already, and its own stderr says why; that is the better error,
            // so this one is deliberately swallowed.
            let _ = stdin.write_all(input);
            drop(stdin);
        }
        let output = child.wait_with_output().map_err(crate::io_error)?;
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
        Ok(output.stdout)
    }

    /// Whether this repository already has history of its own.
    ///
    /// The test for "is this a brand-new project?", which decides whether the
    /// migrated branches keep their own names or are given a prefix.
    ///
    /// Branches and tags only — deliberately not `--all`, which would also see
    /// the source refs this migration parks in its own namespace and would
    /// therefore report a half-finished migration's own scratch space as
    /// somebody else's history.
    pub(crate) fn has_commits(&self) -> bool {
        self.run(&["rev-list", "-n", "1", "--branches", "--tags"])
            .is_ok_and(|output| !output.trim().is_empty())
    }
}

/// Copy every branch and tag of the source repository into this one, parked in
/// the migration's own ref namespace.
///
/// A second run re-fetches, which is incremental: the objects already here are
/// not sent again, so an interrupted migration does not pay for the clone twice.
pub(crate) fn fetch_source(git: &Git, url: &str) -> Result<(), Failure> {
    git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        url,
        &format!("+refs/heads/*:{SOURCE_NAMESPACE}/heads/*"),
        &format!("+refs/tags/*:{SOURCE_NAMESPACE}/tags/*"),
    ])
    .map_err(|error| {
        Failure::provider(format!(
            "{error}\n  while reading the DVC repository at {}",
            redact(url)
        ))
    })?;
    Ok(())
}

/// The branch the source repository's `HEAD` points at.
pub(crate) fn source_default_branch(git: &Git, url: &str) -> Option<String> {
    let output = git.run(&["ls-remote", "--symref", url, "HEAD"]).ok()?;
    output.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    })
}

/// The source branches and tags, as (short name, commit or tag object).
pub(crate) fn source_refs(git: &Git) -> Result<(Vec<Reference>, Vec<Reference>), Failure> {
    let read = |kind: &str| -> Result<Vec<Reference>, Failure> {
        let pattern = format!("{SOURCE_NAMESPACE}/{kind}/");
        let output = git.run(&[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            &pattern,
        ])?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let (refname, id) = line.split_once('\t')?;
                Some(Reference {
                    name: refname.strip_prefix(&pattern)?.to_owned(),
                    id: id.to_owned(),
                })
            })
            .collect())
    };
    Ok((read("heads")?, read("tags")?))
}

pub(crate) struct Reference {
    pub(crate) name: String,
    pub(crate) id: String,
}

/// Every commit reachable from the source refs, parents before children.
///
/// Topological order is what makes a single pass possible: a commit's parents
/// have always been rewritten by the time it is reached, so its new parent ids
/// are already known.
pub(crate) fn commits_in_order(git: &Git) -> Result<Vec<String>, Failure> {
    let output = git.run(&[
        "rev-list",
        "--topo-order",
        "--reverse",
        &format!("--glob={SOURCE_NAMESPACE}/heads"),
        &format!("--glob={SOURCE_NAMESPACE}/tags"),
    ])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Read a commit object: its parents, who wrote it, and what it says.
pub(crate) fn read_commit(git: &Git, id: &str) -> Result<Commit, Failure> {
    let raw = git.run_bytes(&["cat-file", "commit", id])?;
    // The header ends at the first blank line; everything after it is the
    // message, byte for byte.
    let split = find_blank_line(&raw).unwrap_or(raw.len());
    let header = String::from_utf8_lossy(&raw[..split]).into_owned();
    let message = raw
        .get(split.saturating_add(2)..)
        .unwrap_or_default()
        .to_vec();

    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;
    for line in header.lines() {
        if let Some(parent) = line.strip_prefix("parent ") {
            parents.push(parent.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("author ") {
            author = parse_identity(value);
        } else if let Some(value) = line.strip_prefix("committer ") {
            committer = parse_identity(value);
        }
    }
    let author =
        author.ok_or_else(|| Failure::from(format!("commit {id} has no author to preserve")))?;
    let committer = committer.unwrap_or_else(|| author.clone());
    Ok(Commit {
        parents,
        author,
        committer,
        message,
    })
}

/// Split `Name <email> 1700000000 +0100` into its three parts.
fn parse_identity(value: &str) -> Option<Identity> {
    let open = value.rfind(" <")?;
    let close = value[open..].find('>')? + open;
    Some(Identity {
        name: value[..open].to_owned(),
        email: value[open + 2..close].to_owned(),
        date: value[close + 1..].trim().to_owned(),
    })
}

fn find_blank_line(raw: &[u8]) -> Option<usize> {
    raw.windows(2).position(|pair| pair == b"\n\n")
}

/// Every file in a commit's tree, at its full path.
pub(crate) fn list_tree(git: &Git, commit: &str) -> Result<Vec<TreeFile>, Failure> {
    // NUL-terminated so a path containing a newline or a quote is read as
    // itself rather than as Git's quoted rendering of itself.
    let raw = git.run_bytes(&["ls-tree", "-r", "-z", "--full-tree", commit])?;
    let mut files = Vec::new();
    for record in raw.split(|byte| *byte == 0).filter(|part| !part.is_empty()) {
        let text = String::from_utf8_lossy(record);
        let Some((meta, path)) = text.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split_whitespace();
        let (Some(mode), Some(_kind), Some(id)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        files.push(TreeFile {
            mode: mode.to_owned(),
            id: id.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(files)
}

pub(crate) fn read_blob(git: &Git, id: &str) -> Result<Vec<u8>, Failure> {
    git.run_bytes(&["cat-file", "blob", id])
}

/// Store bytes as a blob and report its id.
pub(crate) fn write_blob(git: &Git, bytes: &[u8]) -> Result<String, Failure> {
    let id = git.run_input(&["hash-object", "-w", "--stdin"], bytes, &[])?;
    Ok(id.trim().to_owned())
}

/// Build a tree from `base`'s tree with `changes` applied.
///
/// The index lives in a file of its own rather than the repository's, so a
/// migration never disturbs a working tree someone may have staged work in.
pub(crate) fn build_tree(
    git: &Git,
    index: &Path,
    base: &str,
    changes: &[Change],
) -> Result<String, Failure> {
    let index = index.to_string_lossy().into_owned();
    let environment = [("GIT_INDEX_FILE", index.as_str())];
    git.run_input(&["read-tree", base], &[], &environment)?;

    if !changes.is_empty() {
        let mut script = Vec::new();
        for change in changes {
            let path = match change {
                Change::Add { path, mode, id } => {
                    script.extend_from_slice(format!("{mode} {id}\t").as_bytes());
                    path
                }
                Change::Remove { path } => {
                    // Mode zero with the null object is how `--index-info`
                    // spells a removal.
                    script.extend_from_slice(b"0 0000000000000000000000000000000000000000\t");
                    path
                }
            };
            // The record format ends a path at the newline, so a path
            // containing one cannot be expressed. Refusing is the only honest
            // answer: the alternative is a tree that silently omits a file.
            if path.contains('\n') {
                return Err(format!(
                    "cannot migrate a commit containing a path with a newline in it: {path:?}"
                )
                .into());
            }
            script.extend_from_slice(path.as_bytes());
            script.push(b'\n');
        }
        git.run_input(&["update-index", "--index-info"], &script, &environment)?;
    }
    let tree = git.run_input(&["write-tree"], &[], &environment)?;
    Ok(tree.trim().to_owned())
}

/// Commit `tree` with `parents`, preserving everything about `original` that a
/// rewrite can preserve.
pub(crate) fn commit_tree(
    git: &Git,
    tree: &str,
    parents: &[String],
    original: &Commit,
) -> Result<String, Failure> {
    // A rewritten commit cannot carry the original's signature, and signing it
    // with the operator's own key would assert something nobody asked for --
    // besides stopping a long migration dead on a passphrase prompt for every
    // commit, on any machine with `commit.gpgsign` set.
    let mut arguments = vec![
        "-c".to_owned(),
        "commit.gpgsign=false".to_owned(),
        "commit-tree".to_owned(),
        tree.to_owned(),
    ];
    for parent in parents {
        arguments.push("-p".to_owned());
        arguments.push(parent.clone());
    }
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let id = git.run_input(
        &borrowed,
        &original.message,
        &[
            ("GIT_AUTHOR_NAME", original.author.name.as_str()),
            ("GIT_AUTHOR_EMAIL", original.author.email.as_str()),
            ("GIT_AUTHOR_DATE", original.author.date.as_str()),
            ("GIT_COMMITTER_NAME", original.committer.name.as_str()),
            ("GIT_COMMITTER_EMAIL", original.committer.email.as_str()),
            ("GIT_COMMITTER_DATE", original.committer.date.as_str()),
        ],
    )?;
    Ok(id.trim().to_owned())
}

pub(crate) fn set_ref(git: &Git, name: &str, id: &str) -> Result<(), Failure> {
    git.run(&["update-ref", name, id])?;
    Ok(())
}

/// Remove the parked source refs, so the destination is left with only its own.
pub(crate) fn clear_source_refs(git: &Git) -> Result<(), Failure> {
    let output = git.run(&["for-each-ref", "--format=%(refname)", SOURCE_NAMESPACE])?;
    for refname in output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        // Best effort: a leftover private ref is untidy, not harmful, and is
        // not worth failing a completed migration over.
        let _ = git.run(&["update-ref", "-d", refname]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_keeps_its_instant_and_its_zone() {
        let identity = parse_identity("Ada Lovelace <ada@example.com> 1700000000 +0530").unwrap();
        assert_eq!(identity.name, "Ada Lovelace");
        assert_eq!(identity.email, "ada@example.com");
        // Not normalized to UTC: the zone is part of what was recorded.
        assert_eq!(identity.date, "1700000000 +0530");

        // A name containing an angle bracket is unusual and legal.
        let odd = parse_identity("A <B> C <c@example.com> 1 +0000").unwrap();
        assert_eq!(odd.name, "A <B> C");
        assert_eq!(odd.email, "c@example.com");
    }

    #[test]
    fn submodules_and_subtrees_are_told_apart_from_files() {
        let entry = |mode: &str| TreeFile {
            mode: mode.into(),
            id: "x".into(),
            path: "p".into(),
        };
        assert!(entry("100644").is_blob());
        assert!(entry("100755").is_blob());
        assert!(entry("120000").is_blob());
        // A submodule has no blob to read, and reading one as a `.dvc` file
        // would fail in a confusing place.
        assert!(!entry("160000").is_blob());
    }
}
