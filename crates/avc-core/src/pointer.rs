use serde::{Deserialize, Serialize};

use crate::{normalize_repo_path, Error, ObjectId, Result, ALGORITHM};

pub const POINTER_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pointer {
    pub version: u32,
    pub path: String,
    pub object: ObjectMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectMetadata {
    pub algorithm: String,
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
            object: ObjectMetadata {
                algorithm: ALGORITHM.into(),
                hash: object.hash().into(),
                size,
                media_type,
            },
        })
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
        if self.object.algorithm != ALGORITHM {
            return Err(Error::InvalidObjectId(self.object.algorithm.clone()));
        }
        ObjectId::new(self.object.hash.clone())?;
        Ok(())
    }

    pub fn object_id(&self) -> Result<ObjectId> {
        ObjectId::new(self.object.hash.clone())
    }
}
