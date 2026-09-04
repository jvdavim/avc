use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The algorithm a new artifact is addressed with.
///
/// Everything AVC creates uses SHA-256. MD5 exists because a repository
/// migrated from another tool arrives with its objects already addressed, and
/// re-addressing them would mean reading every byte of every version back over
/// the network. See [`Algorithm::Md5`].
pub const ALGORITHM: &str = "sha256";

/// A content-addressing function.
///
/// An object's algorithm is part of its identity, not a global setting: it is
/// recorded in the pointer, in the manifest entry, and in the object's key, so
/// two artifacts in one repository may be addressed differently and neither is
/// ambiguous.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Algorithm {
    /// What `avc add` uses, and what a repository should be on.
    ///
    /// Collision-resistant, and — on any CPU built since 2017 — around three
    /// times faster than MD5, because the instruction set implements it.
    #[default]
    Sha256,
    /// Only ever produced by importing from a tool that used it.
    ///
    /// MD5 collisions are generatable in seconds, and in a content-addressed
    /// store that means two different files can claim one address. AVC will not
    /// mint an MD5 object; it only preserves one it was handed, so that a
    /// migration does not have to re-read a multi-terabyte object store. `avc
    /// migrate dvc --rehash` trades that network cost for SHA-256 identities.
    Md5,
}

impl Algorithm {
    /// The name as it appears in a pointer, a manifest, and an object key.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Md5 => "md5",
        }
    }

    /// How many hexadecimal characters a digest of this algorithm has.
    pub fn hex_length(self) -> usize {
        match self {
            Algorithm::Sha256 => 64,
            Algorithm::Md5 => 32,
        }
    }

    /// Whether AVC will create new objects with this algorithm.
    ///
    /// Consulted where content is being addressed for the first time, so that
    /// a weak algorithm can be read and preserved but never chosen.
    pub fn is_minted(self) -> bool {
        matches!(self, Algorithm::Sha256)
    }
}

impl FromStr for Algorithm {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "sha256" => Ok(Algorithm::Sha256),
            "md5" => Ok(Algorithm::Md5),
            other => Err(Error::UnsupportedAlgorithm(other.to_owned())),
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl Serialize for Algorithm {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Algorithm {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ObjectId {
    algorithm: Algorithm,
    hash: String,
}

impl ObjectId {
    /// An object addressed by `algorithm`, whose digest is `hash`.
    ///
    /// The digest is checked against the algorithm's own width, so a 32-digit
    /// value can never be read as a SHA-256 nor a 64-digit one as an MD5.
    pub fn new(algorithm: Algorithm, hash: impl Into<String>) -> Result<Self> {
        let hash = hash.into();
        if hash.len() != algorithm.hex_length()
            || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::InvalidObjectId(hash));
        }
        Ok(Self {
            algorithm,
            hash: hash.to_ascii_lowercase(),
        })
    }

    /// The common case: an object AVC addressed itself.
    pub fn sha256(hash: impl Into<String>) -> Result<Self> {
        Self::new(Algorithm::Sha256, hash)
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn cache_key(&self) -> String {
        format!(
            "objects/{}/{}/{}",
            self.algorithm.name(),
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
