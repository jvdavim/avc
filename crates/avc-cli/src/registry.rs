//! The repository artifacts are named in, wherever its pointers come from.
//!
//! An AVC repository is an artifact registry: one Git repository can hold
//! thousands of artifacts belonging to a dozen projects, and a consumer wants
//! one path out of it. This module is what makes that possible — it presents a
//! Git URL and a directory on disk as the same thing, a set of pointers plus
//! the object-store configuration that came with them, so `fetch`, `verify`,
//! and `list` never have to care which they were given.
//!
//! The object store is deliberately not a parameter here. It is configured once
//! in the repository's tracked `.avc/config.toml`, by whoever set the
//! repository up, and read back out of whatever reference is being consumed.
//! A consumer names the repository and the path; the bucket is not their
//! business.

use std::path::{Path, PathBuf};

use crate::{git, Failure, Repo};

/// A repository's pointers, and the configuration that says where their bytes
/// live.
pub(crate) struct Registry {
    repo: Repo,
    /// How this registry is named in output — a URL and commit, or a path.
    description: String,
    /// The worktree these artifacts belong in, when there is one.
    ///
    /// Distinct from `repo.root`, which is wherever the *pointers* were read
    /// from. Those are the same directory for a plain local repository and
    /// different for a revision of one: reading `v1.0.0` in a checkout puts the
    /// pointers in a temporary directory, but the artifacts they name still
    /// belong in the worktree the caller is standing in. A registry named by
    /// URL has no worktree at all.
    worktree: Option<PathBuf>,
    /// Held only to keep a temporary checkout alive for this registry's
    /// lifetime; dropping it deletes the directory `repo.root` points into.
    _checkout: Option<git::Checkout>,
}

impl Registry {
    /// Open a repository from a Git URL, reading its pointers at `revision`.
    ///
    /// Nothing but pointers and configuration is read: artifacts are gitignored,
    /// so the checkout is text, and the bytes come later from the object store.
    pub(crate) fn from_git(url: &str, revision: &str) -> Result<Self, Failure> {
        let checkout = git::Checkout::at(url, revision)?;
        let description = format!("{}@{} ({revision})", git::redact(url), checkout.commit());
        let repo = Repo::at(checkout.path().to_path_buf())?;
        Ok(Self {
            repo,
            description,
            worktree: None,
            _checkout: Some(checkout),
        })
    }

    /// Open a repository already on disk, reading the pointers in it.
    ///
    /// The Git worktree root is preferred over the current directory, because a
    /// pointer's `path` is relative to the repository root and resolving it
    /// against a subdirectory would put the artifact in the wrong place.
    pub(crate) fn from_directory(root: Option<PathBuf>) -> Result<Self, Failure> {
        let root = match root {
            Some(root) => root,
            None => crate::find_root().unwrap_or_else(|_| PathBuf::from(".")),
        };
        Ok(Self {
            description: root.display().to_string(),
            worktree: Some(root.clone()),
            repo: Repo::at(root)?,
            _checkout: None,
        })
    }

    /// Open one revision of a repository on disk.
    ///
    /// The pointers come out of Git rather than off the working tree, which is
    /// the difference between "what this artifact is" and "what this artifact
    /// was at `v1.0.0`". The artifacts themselves still belong to the worktree
    /// the caller is standing in, so `avc fetch --ref v1.0.0` restores that
    /// version into place and `avc verify --ref v1.0.0` checks what is on disk
    /// against it.
    pub(crate) fn from_revision(root: PathBuf, revision: &str) -> Result<Self, Failure> {
        // Read the way any other repository is read, with the worktree itself
        // as the URL. Git clones a path as readily as it clones a URL, and one
        // code path means a revision means the same thing wherever it is named.
        let checkout = git::Checkout::at(&root.display().to_string(), revision)?;
        let description = format!("{}@{} ({revision})", root.display(), checkout.commit());
        let repo = Repo::at(checkout.path().to_path_buf())?;
        Ok(Self {
            repo,
            description,
            worktree: Some(root),
            _checkout: Some(checkout),
        })
    }

    /// Open whichever source the arguments describe.
    ///
    /// A revision is optional, and its absence is not the same as `HEAD`. With
    /// no revision, a local repository is read off the working tree — so a
    /// pointer that has been written but not committed still counts, which is
    /// what every other command does. Naming a revision, `HEAD` included, reads
    /// the pointers Git holds at that commit instead.
    pub(crate) fn open(url: Option<&str>, revision: Option<&str>) -> Result<Self, Failure> {
        match (url, revision) {
            (Some(url), revision) => Self::from_git(url, revision.unwrap_or("HEAD")),
            (None, Some(revision)) => Self::from_revision(crate::find_root()?, revision),
            (None, None) => Self::from_directory(None),
        }
    }

    /// The worktree these artifacts belong in, if any. See [`Registry`].
    ///
    /// This, and not `repo.root`, is where a pointer's path is resolved
    /// against: the two differ exactly when a revision was named.
    pub(crate) fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }

    /// The underlying repository, for the helpers that read its cache.
    ///
    /// A registry read from a Git URL has no cache directory, so those helpers
    /// simply find nothing there and go to the object store instead.
    pub(crate) fn repo(&self) -> &Repo {
        &self.repo
    }

    pub(crate) fn describe(&self) -> &str {
        &self.description
    }

    /// Every artifact this repository tracks, sorted by path.
    pub(crate) fn artifacts(&self) -> Result<Vec<avc_core::Pointer>, Failure> {
        let mut pointers = Vec::new();
        for relative in crate::pointer_files(&self.repo.root)? {
            let path = self.repo.root.join(&relative);
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", relative.display()))?;
            let pointer = avc_core::Pointer::parse(&text)
                .map_err(|error| format!("{}: {error}", relative.display()))?;
            pointers.push(pointer);
        }
        pointers.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(pointers)
    }

    /// The artifacts `paths` names, or all of them when nothing is named.
    pub(crate) fn select(&self, paths: &[String]) -> Result<Vec<Selection>, Failure> {
        select(self.artifacts()?, paths)
    }

    /// The object store this repository's artifacts live in.
    ///
    /// `url` overrides the tracked configuration, which is an escape hatch for
    /// a mirror or a repository whose configuration predates the remote; the
    /// ordinary path is that the repository already knows.
    pub(crate) fn store(
        &self,
        url: Option<&str>,
        name: Option<&str>,
    ) -> Result<Box<dyn avc_core::ObjectStore>, Failure> {
        if let Some(url) = url {
            let config = avc_core::RemoteConfig::from_url("--remote-url", url)?;
            return Ok(avc_core::remote::open(&config, None)?);
        }
        if self.repo.config.remotes.is_empty() {
            return Err(format!(
                "{} configures no object store; \
                 run `avc remote add` in the repository, or name one with --remote-url",
                self.description
            )
            .into());
        }
        crate::open_store(&self.repo, name)
    }
}

/// One artifact a selector matched, and how that selector narrowed it.
///
/// A selection is not just "which artifact": it also carries *how it was
/// named*, because that decides two things a bare pointer cannot. Which files
/// of a tracked directory are wanted, and where they belong under an output
/// root.
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub(crate) pointer: avc_core::Pointer,
    /// The part of the selector that continued past a tracked directory:
    /// `Some("weights.bin")` for one file inside it, `Some("sub")` for a
    /// subdirectory of it, `None` for the whole artifact.
    pub(crate) inside: Option<String>,
    /// Whether the selector named this artifact itself rather than reaching it
    /// as a prefix. `avc list data` looks *inside* the directory it names;
    /// `avc list models` lists the artifacts beneath it.
    pub(crate) exact: bool,
    /// Leading repository path dropped when placing this beneath an output
    /// root: everything above the last segment the selector named.
    strip: String,
}

impl Selection {
    /// The path the user named, which is what output should call this.
    pub(crate) fn label(&self) -> String {
        match &self.inside {
            Some(inner) => format!("{}/{inner}", self.pointer.path),
            None => crate::display_path(&self.pointer),
        }
    }

    /// Where a repository path lands beneath an output root.
    ///
    /// What was *named* is what appears in the output directory, not the
    /// repository's path to it: `avc fetch models/bert -o .` writes `./bert`,
    /// and `avc fetch models/bert/weights.bin -o .` writes `./weights.bin`.
    /// The parent directories a selector walked through are scaffolding for
    /// finding the artifact, not part of the artifact's identity, and
    /// reproducing them in a pipeline's workspace only makes every consumer
    /// write a `mv` afterwards.
    ///
    /// Naming nothing at all keeps every path in full, since there is no
    /// selector to take a parent from — and collapsing a whole repository into
    /// one directory would be a name collision waiting to happen.
    pub(crate) fn destination(&self, repository_path: &str) -> String {
        repository_path
            .strip_prefix(&self.strip)
            .unwrap_or(repository_path)
            .to_owned()
    }

    /// Whether a manifest entry belongs to this selection.
    ///
    /// Entry paths are relative to the tracked directory, and so is `inside`.
    pub(crate) fn includes(&self, entry: &str) -> bool {
        match &self.inside {
            None => true,
            // An exact file, or everything beneath a subdirectory of the
            // artifact. `sub` must not match `submarine.bin`.
            Some(inner) => entry == inner || entry.starts_with(&format!("{inner}/")),
        }
    }
}

/// Resolve path selectors against a set of artifacts.
///
/// A selector is a path inside the repository and matches in one of three ways:
/// exactly, naming one artifact; as a directory prefix, naming every artifact
/// beneath it; or by continuing *past* a tracked directory, naming a file or a
/// subdirectory inside one. That is what makes a shared registry usable — `avc
/// fetch models/bert` pulls one project's artifacts out of a repository holding
/// a hundred, and `avc fetch models/bert/weights.bin` pulls one file out of a
/// tracked directory, without the caller knowing how the publisher chose to
/// group things.
///
/// The three are tried in that order, so a real artifact always wins over a
/// path that merely looks like one from inside a directory's manifest.
///
/// A trailing `/` is optional, and a trailing `.avc` is accepted and stripped,
/// so a pointer path piped from `git diff --name-only` names its artifact
/// without being rewritten first.
pub(crate) fn select(
    artifacts: Vec<avc_core::Pointer>,
    paths: &[String],
) -> Result<Vec<Selection>, Failure> {
    if paths.is_empty() {
        return Ok(artifacts
            .into_iter()
            .map(|pointer| Selection {
                pointer,
                inside: None,
                exact: false,
                strip: String::new(),
            })
            .collect());
    }
    let mut selected: Vec<Selection> = Vec::new();
    for value in paths {
        let wanted = normalize_selector(value)?;
        let strip = parent_of(&wanted);
        let prefix = format!("{wanted}/");
        let mut matched: Vec<Selection> = Vec::new();

        // An exact match wins outright: a tracked directory named `data` is one
        // artifact, not a prefix over the artifacts beneath it.
        for pointer in artifacts.iter().filter(|pointer| pointer.path == wanted) {
            matched.push(Selection {
                pointer: pointer.clone(),
                inside: None,
                exact: true,
                strip: strip.clone(),
            });
        }
        for pointer in artifacts
            .iter()
            .filter(|pointer| pointer.path.starts_with(&prefix))
        {
            matched.push(Selection {
                pointer: pointer.clone(),
                inside: None,
                exact: false,
                strip: strip.clone(),
            });
        }

        if matched.is_empty() {
            // Nothing is tracked at this path, but a tracked directory above it
            // may hold it. Its manifest lists every file it contains, each
            // stored as an ordinary object, so one of them can be named on its
            // own — the grouping the publisher chose is not a wall.
            if let Some(pointer) = artifacts
                .iter()
                .filter(|pointer| pointer.is_directory())
                .filter(|pointer| wanted.starts_with(&format!("{}/", pointer.path)))
                // The innermost tracked directory owns the path, if directories
                // are ever allowed to nest.
                .max_by_key(|pointer| pointer.path.len())
            {
                matched.push(Selection {
                    inside: Some(wanted[pointer.path.len() + 1..].to_owned()),
                    pointer: pointer.clone(),
                    exact: true,
                    strip,
                });
            }
        }

        if matched.is_empty() {
            // A path that names nothing is a typo, and a typo in a pipeline
            // should fail rather than quietly select nothing.
            return Err(format!("no artifact at {wanted}").into());
        }
        for selection in matched {
            if !selected.iter().any(|kept| {
                kept.pointer.path == selection.pointer.path && kept.inside == selection.inside
            }) {
                selected.push(selection);
            }
        }
    }
    selected.sort_by(|left, right| {
        (&left.pointer.path, &left.inside).cmp(&(&right.pointer.path, &right.inside))
    });
    Ok(selected)
}

/// What to say when a selector named something a tracked directory turned out
/// not to contain.
///
/// Whether the path exists is only knowable once the manifest has been read,
/// which happens well after selection, so this is reported by whoever read it.
pub(crate) fn missing_inside(selection: &Selection) -> String {
    format!(
        "no file at {} inside the tracked directory {}; \
         `avc list {}` shows what is in it",
        selection.label(),
        selection.pointer.path,
        selection.pointer.path
    )
}

/// Everything above the last segment of a selector, including the separator.
///
/// `models/bert` yields `models/`, and `model.bin` yields nothing at all.
fn parent_of(selector: &str) -> String {
    match selector.rfind('/') {
        Some(index) => selector[..=index].to_owned(),
        None => String::new(),
    }
}

/// Normalize one path selector to the form a pointer's `path` field uses.
pub(crate) fn normalize_selector(value: &str) -> Result<String, Failure> {
    let trimmed = value.trim_end_matches('/');
    let stripped = trimmed.strip_suffix(".avc").unwrap_or(trimmed);
    Ok(avc_core::normalize_repo_path(Path::new(stripped))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(path: &str, directory: bool) -> avc_core::Pointer {
        let object = avc_core::ObjectId::sha256(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        if directory {
            avc_core::Pointer::new_directory(path, object, 1).unwrap()
        } else {
            avc_core::Pointer::new(path, object, 1, None).unwrap()
        }
    }

    fn registry() -> Vec<avc_core::Pointer> {
        vec![
            artifact("models/bert/weights.bin", false),
            artifact("models/bert/tokenizer.json", false),
            artifact("models/gpt/weights.bin", false),
            artifact("data", true),
            artifact("data-extra/one.bin", false),
        ]
    }

    fn paths(selected: &[Selection]) -> Vec<&str> {
        selected
            .iter()
            .map(|selection| selection.pointer.path.as_str())
            .collect()
    }

    /// Where each selected artifact would be written beneath `-o`.
    fn destinations(selected: &[Selection]) -> Vec<String> {
        selected
            .iter()
            .map(|selection| selection.destination(&selection.pointer.path))
            .collect()
    }

    #[test]
    fn a_prefix_selects_one_project_out_of_a_shared_registry() {
        let selected = select(registry(), &["models/bert".into()]).unwrap();
        assert_eq!(
            paths(&selected),
            ["models/bert/tokenizer.json", "models/bert/weights.bin"]
        );
        // A trailing slash names the same thing, as it does everywhere else.
        assert_eq!(
            paths(&select(registry(), &["models/bert/".into()]).unwrap()),
            paths(&selected)
        );
    }

    /// What was named lands in the output directory; the path walked to reach
    /// it does not come along.
    #[test]
    fn a_selector_puts_what_it_named_at_the_output_root() {
        assert_eq!(
            destinations(&select(registry(), &["models/bert".into()]).unwrap()),
            ["bert/tokenizer.json", "bert/weights.bin"]
        );
        assert_eq!(
            destinations(&select(registry(), &["models/gpt/weights.bin".into()]).unwrap()),
            ["weights.bin"]
        );
        assert_eq!(
            destinations(&select(registry(), &["data".into()]).unwrap()),
            ["data"]
        );
        // A top-level selector has no parent to drop.
        assert_eq!(
            destinations(&select(registry(), &["models".into()]).unwrap()),
            [
                "models/bert/tokenizer.json",
                "models/bert/weights.bin",
                "models/gpt/weights.bin"
            ]
        );
        // Naming nothing keeps every path in full: there is no selector to
        // take a parent from, and flattening a whole registry would collide.
        assert_eq!(
            destinations(&select(registry(), &[]).unwrap()),
            paths(&select(registry(), &[]).unwrap())
        );
    }

    /// A tracked directory is a grouping, not a wall: each file inside it is an
    /// ordinary object and can be named on its own.
    #[test]
    fn a_path_inside_a_tracked_directory_names_what_is_in_it() {
        let selected = select(registry(), &["data/nested/b.bin".into()]).unwrap();
        assert_eq!(paths(&selected), ["data"]);
        assert_eq!(selected[0].inside.as_deref(), Some("nested/b.bin"));
        assert_eq!(selected[0].label(), "data/nested/b.bin");
        // Only that file, and it lands under its own name.
        assert!(selected[0].includes("nested/b.bin"));
        assert!(!selected[0].includes("nested/other.bin"));
        assert_eq!(
            selected[0].destination("data/nested/b.bin"),
            "b.bin".to_owned()
        );

        // A subdirectory of a tracked directory takes everything beneath it,
        // and keeps its own shape.
        let subdirectory = select(registry(), &["data/nested".into()]).unwrap();
        assert_eq!(subdirectory[0].inside.as_deref(), Some("nested"));
        assert!(subdirectory[0].includes("nested/b.bin"));
        assert!(!subdirectory[0].includes("nested-other/b.bin"));
        assert!(!subdirectory[0].includes("a.bin"));
        assert_eq!(
            subdirectory[0].destination("data/nested/b.bin"),
            "nested/b.bin".to_owned()
        );
    }

    /// A real artifact always wins over a path that only looks like one from
    /// inside a manifest.
    #[test]
    fn an_artifact_of_its_own_beats_a_file_inside_a_directory() {
        let mut artifacts = registry();
        artifacts.push(artifact("data/nested/b.bin", false));
        let selected = select(artifacts, &["data/nested/b.bin".into()]).unwrap();
        assert_eq!(paths(&selected), ["data/nested/b.bin"]);
        assert_eq!(selected[0].inside, None);
    }

    #[test]
    fn an_exact_match_beats_the_prefix_that_shares_its_name() {
        // `data` is one directory artifact; `data-extra/one.bin` merely starts
        // with the same characters, and must not be dragged in with it.
        assert_eq!(
            paths(&select(registry(), &["data".into()]).unwrap()),
            ["data"]
        );
    }

    #[test]
    fn a_pointer_path_names_its_artifact() {
        assert_eq!(
            paths(&select(registry(), &["models/gpt/weights.bin.avc".into()]).unwrap()),
            ["models/gpt/weights.bin"]
        );
    }

    #[test]
    fn selecting_twice_yields_one_copy_and_a_stable_order() {
        let selected = select(registry(), &["models/gpt".into(), "models".into()]).unwrap();
        assert_eq!(
            paths(&selected),
            [
                "models/bert/tokenizer.json",
                "models/bert/weights.bin",
                "models/gpt/weights.bin"
            ]
        );
    }

    #[test]
    fn a_path_that_names_nothing_is_an_error() {
        assert!(select(registry(), &["models/absent".into()]).is_err());
        // Escaping the repository is refused before anything is matched.
        assert!(select(registry(), &["../secrets".into()]).is_err());
        // A path under something that is *not* a tracked directory has no
        // manifest to look in, so it names nothing either.
        assert!(select(registry(), &["models/gpt/weights.bin/more".into()]).is_err());
    }

    #[test]
    fn selecting_the_same_file_inside_a_directory_twice_yields_one_copy() {
        let selected = select(
            registry(),
            &["data/a.bin".into(), "data/a.bin".into(), "data".into()],
        )
        .unwrap();
        assert_eq!(paths(&selected), ["data", "data"]);
        // The whole directory and one file inside it are different requests,
        // and both survive; the repeat does not.
        let inside: Vec<Option<&str>> = selected
            .iter()
            .map(|selection| selection.inside.as_deref())
            .collect();
        assert_eq!(inside, [None, Some("a.bin")]);
    }
}
