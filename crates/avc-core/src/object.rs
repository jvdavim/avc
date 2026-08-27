use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const ALGORITHM: &str = "sha256";

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ObjectId {
    algorithm: String,
    hash: String,
}

impl ObjectId {
    pub fn new(hash: impl Into<String>) -> Result<Self> {
        let hash = hash.into();
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InvalidObjectId(hash));
        }
        Ok(Self {
            algorithm: ALGORITHM.into(),
            hash: hash.to_ascii_lowercase(),
        })
    }

    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn cache_key(&self) -> String {
        format!(
            "objects/{}/{}/{}",
            self.algorithm,
            &self.hash[..2],
            self.hash
        )
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.algorithm, self.hash)
    }
}
