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
    /// Held only to keep a temporary checkout alive for this registry's
    /// lifetime; dropping it deletes the directory `repo.root` points into.
    _checkout: Option<git::Checkout>,
}

impl Registry {
    /// Open a repository from a Git URL, reading its pointers at `reference`.
    ///
    /// Nothing but pointers and configuration is read: artifacts are gitignored,
    /// so the checkout is text, and the bytes come later from the object store.
    pub(crate) fn from_git(url: &str, reference: &str) -> Result<Self, Failure> {
        let checkout = git::Checkout::shallow(url, reference)?;
        let description = format!("{}@{} ({reference})", git::redact(url), checkout.commit());
        let repo = Repo::at(checkout.path().to_path_buf())?;
        Ok(Self {
            repo,
            description,
            _checkout: Some(checkout),
        })
    }

    /// Open a repository already on disk.
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
            repo: Repo::at(root)?,
            _checkout: None,
        })
    }

    /// Open whichever source the arguments describe.
    pub(crate) fn open(url: Option<&str>, reference: &str) -> Result<Self, Failure> {
        match url {
            Some(url) => Self::from_git(url, reference),
            None => Self::from_directory(None),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.repo.root
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

    /// Whether this registry is a directory the caller can write into.
    ///
    /// A registry read from a Git URL lives in a temporary checkout that is
    /// deleted when the command ends, so its root is not somewhere artifacts
    /// may be materialized.
    pub(crate) fn is_local(&self) -> bool {
        self._checkout.is_none()
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
    pub(crate) fn select(&self, paths: &[String]) -> Result<Vec<avc_core::Pointer>, Failure> {
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

/// Resolve path selectors against a set of artifacts.
///
/// A selector is a path inside the repository and matches in one of two ways:
/// exactly, naming one artifact, or as a directory prefix, naming every
/// artifact beneath it. That is what makes a shared registry usable — `avc
/// fetch models/bert` pulls one project's artifacts out of a repository holding
/// a hundred, without the caller knowing what is in it.
///
/// A trailing `/` is optional, and a trailing `.avc` is accepted and stripped,
/// so a pointer path piped from `git diff --name-only` names its artifact
/// without being rewritten first.
pub(crate) fn select(
    artifacts: Vec<avc_core::Pointer>,
    paths: &[String],
) -> Result<Vec<avc_core::Pointer>, Failure> {
    if paths.is_empty() {
        return Ok(artifacts);
    }
    let mut selected: Vec<avc_core::Pointer> = Vec::new();
    for value in paths {
        let wanted = normalize_selector(value)?;
        let prefix = format!("{wanted}/");
        let matched: Vec<&avc_core::Pointer> = artifacts
            .iter()
            // An exact match wins outright: a tracked directory named `data`
            // is one artifact, not a prefix over the artifacts beneath it.
            .filter(|pointer| pointer.path == wanted)
            .chain(
                artifacts
                    .iter()
                    .filter(|pointer| pointer.path.starts_with(&prefix)),
            )
            .collect();
        if matched.is_empty() {
            // A path that names nothing is a typo, and a typo in a pipeline
            // should fail rather than quietly select nothing.
            return Err(format!("no artifact at {wanted}").into());
        }
        for pointer in matched {
            if !selected.iter().any(|kept| kept.path == pointer.path) {
                selected.push(pointer.clone());
            }
        }
    }
    selected.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(selected)
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
        let object = avc_core::ObjectId::new(
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

    fn paths(selected: &[avc_core::Pointer]) -> Vec<&str> {
        selected.iter().map(|p| p.path.as_str()).collect()
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
    }
}
