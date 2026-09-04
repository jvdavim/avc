//! Directory manifests.
//!
//! A tracked directory is not a new kind of storage: it is an ordinary
//! content-addressed object whose bytes list the files beneath it. The
//! directory's identity is therefore the hash of that manifest, which changes
//! whenever any file inside it changes, is added, renamed, or removed.
//!
//! Entry paths are relative to the tracked directory rather than to the
//! repository, so the same content tracked at two different paths — or moved —
//! produces the same manifest and reuses every object already stored.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{normalize_repo_path, validate_repo_path, Algorithm, Error, ObjectId, Result};

pub const TREE_VERSION: u32 = 1;

/// Media type recorded in a directory pointer, so a manifest object is
/// recognizable for what it is without being fetched.
pub const TREE_MEDIA_TYPE: &str = "application/vnd.avc.tree+yaml";

/// The manifest of a tracked directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    pub version: u32,
    pub entries: Vec<TreeEntry>,
}

/// One file inside a tracked directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    /// Path relative to the tracked directory, never to the repository root.
    pub path: String,
    pub algorithm: Algorithm,
    pub hash: String,
    pub size: u64,
}

impl TreeEntry {
    pub fn new(path: impl AsRef<Path>, object: ObjectId, size: u64) -> Result<Self> {
        Ok(Self {
            path: normalize_repo_path(path)?,
            algorithm: object.algorithm(),
            hash: object.hash().into(),
            size,
        })
    }

    pub fn object_id(&self) -> Result<ObjectId> {
        ObjectId::new(self.algorithm, self.hash.clone())
    }

    fn validate(&self) -> Result<()> {
        // A manifest drives filesystem writes on checkout, so its paths are
        // untrusted input held to the same rules as a pointer's own path.
        validate_repo_path(&self.path)?;
        ObjectId::new(self.algorithm, self.hash.clone())?;
        Ok(())
    }
}

impl Tree {
    /// Build a manifest from entries in any order.
    ///
    /// Entries are sorted here, because a directory's identity must not depend
    /// on the order the filesystem happened to enumerate it in.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self> {
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let tree = Self {
            version: TREE_VERSION,
            entries,
        };
        tree.validate()?;
        Ok(tree)
    }

    pub fn parse(input: &str) -> Result<Self> {
        let tree: Self = serde_yaml::from_str(input)?;
        tree.validate()?;
        Ok(tree)
    }

    pub fn serialize_canonical(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != TREE_VERSION {
            return Err(Error::UnsupportedTreeVersion(self.version));
        }
        for (index, entry) in self.entries.iter().enumerate() {
            entry.validate()?;
            // Canonical order is part of the object's identity: a manifest
            // that is unsorted or repeats a path did not come from `new`, and
            // two spellings of one directory must never hash differently.
            if index > 0 {
                let previous = &self.entries[index - 1].path;
                if previous >= &entry.path {
                    return Err(Error::InvalidTree(format!(
                        "entries are not in canonical order: '{}' follows '{previous}'",
                        entry.path
                    )));
                }
            }
        }
        Ok(())
    }

    /// Total bytes of the files described, which is what a user means by the
    /// size of a directory — not the size of this manifest.
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.size))
    }
}
