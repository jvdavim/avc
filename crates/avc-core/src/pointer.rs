use serde::{Deserialize, Serialize};

use crate::{normalize_repo_path, Algorithm, Error, ObjectId, Result, TREE_MEDIA_TYPE};

pub const POINTER_VERSION: u32 = 1;

/// What a pointer's `path` names.
///
/// A file pointer references the artifact's bytes directly; a directory
/// pointer references a manifest object that names every file beneath it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    #[default]
    File,
    Directory,
}

impl ArtifactKind {
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub version: u32,
    pub path: String,
    /// Absent means `file`, so pointers written before directories existed
    /// still parse and every file pointer keeps its original bytes.
    #[serde(default, skip_serializing_if = "ArtifactKind::is_file")]
    pub kind: ArtifactKind,
    pub object: ObjectMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectMetadata {
    pub algorithm: Algorithm,
    pub hash: String,
    pub size: u64,
    #[serde(default)]
    pub media_type: Option<String>,
}

pub type Artifact = Pointer;

impl Pointer {
    pub fn new(
        path: impl AsRef<std::path::Path>,
        object: ObjectId,
        size: u64,
        media_type: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            version: POINTER_VERSION,
            path: normalize_repo_path(path)?,
            kind: ArtifactKind::File,
            object: ObjectMetadata {
                // The algorithm belongs to the object, not to this build of
                // AVC: a pointer written for a migrated artifact records the
                // algorithm that artifact is addressed with.
                algorithm: object.algorithm(),
                hash: object.hash().into(),
                size,
                media_type,
            },
        })
    }

    /// A pointer to a directory, identified by its manifest object.
    ///
    /// `manifest` and `size` describe the manifest itself, not the files it
    /// names, because that is the object a transfer must fetch and verify
    /// before anything else about the directory is known.
    pub fn new_directory(
        path: impl AsRef<std::path::Path>,
        manifest: ObjectId,
        size: u64,
    ) -> Result<Self> {
        Ok(Self {
            kind: ArtifactKind::Directory,
            ..Self::new(path, manifest, size, Some(TREE_MEDIA_TYPE.into()))?
        })
    }

    pub fn is_directory(&self) -> bool {
        self.kind == ArtifactKind::Directory
    }

    pub fn parse(input: &str) -> Result<Self> {
        let pointer: Self = serde_yaml::from_str(input)?;
        pointer.validate()?;
        Ok(pointer)
    }

    pub fn serialize_canonical(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != POINTER_VERSION {
            return Err(Error::UnsupportedPointerVersion(self.version));
        }
        normalize_repo_path(&self.path)?;
        // Width is checked against the named algorithm, so a pointer
        // claiming `md5` with a 64-character digest is rejected rather than
        // silently addressing an object that does not exist.
        ObjectId::new(self.object.algorithm, self.object.hash.clone())?;
        Ok(())
    }

    pub fn object_id(&self) -> Result<ObjectId> {
        ObjectId::new(self.object.algorithm, self.object.hash.clone())
    }

    /// The algorithm this artifact is addressed with, which is what any
    /// re-hashing of it on disk has to use.
    pub fn algorithm(&self) -> Algorithm {
        self.object.algorithm
    }
}
