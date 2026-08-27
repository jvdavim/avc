//! Provider-neutral object transfer.
//!
//! Every remote is reached through [`ObjectStore`], which speaks only in terms
//! of [`ObjectId`] values. Backends never learn a repository path, and callers
//! never learn a URL scheme.

mod credentials;
mod file;
mod s3;
mod sigv4;
mod xml;

use std::io::Read;

pub use credentials::{Credentials, LocalRemoteOverride};
pub use file::FileStore;
pub use s3::{S3Settings, S3Store};

use crate::{ObjectId, Provider, RemoteConfig, Result};

/// An object as it exists on a remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteObject {
    pub object: ObjectId,
    pub size: u64,
}

/// A content-addressed object store.
///
/// Implementations move exact bytes and nothing else. Verification of those
/// bytes is the caller's responsibility, because only the caller knows whether
/// a partial transfer should be retained.
pub trait ObjectStore {
    /// Human-readable description of where this store points, for messages.
    fn describe(&self) -> String;

    /// Upload `size` bytes read from `body` under `object`'s key.
    ///
    /// `size` must equal the number of bytes `body` yields. Existing objects
    /// are immutable, so an overwrite is a no-op by construction: the key is
    /// derived from the content.
    fn put(&self, object: &ObjectId, size: u64, body: &mut dyn Read) -> Result<()>;

    /// Open a stream of `object`'s bytes.
    fn get(&self, object: &ObjectId) -> Result<Box<dyn Read>>;

    /// Report whether `object` is present, without transferring its bytes.
    fn exists(&self, object: &ObjectId) -> Result<bool>;

    /// Enumerate every object under the configured prefix.
    fn list(&self) -> Result<Vec<RemoteObject>>;
}

/// Build the store for `remote`, applying machine-local overrides.
///
/// The provider comes from configuration and is never inferred here; this
/// function only dispatches on a decision already made by [`RemoteConfig`].
pub fn open(
    remote: &RemoteConfig,
    local: Option<&LocalRemoteOverride>,
) -> Result<Box<dyn ObjectStore>> {
    match remote.provider {
        Provider::File => Ok(Box::new(FileStore::new(remote))),
        Provider::S3 => Ok(Box::new(S3Store::new(S3Settings::resolve(remote, local)?)?)),
        Provider::Gcs => Err(crate::Error::UnsupportedProvider("gcs")),
        Provider::Azure => Err(crate::Error::UnsupportedProvider("azure")),
    }
}

/// The remote key for `object` beneath `prefix`.
///
/// Mirrors the cache layout exactly, so a remote can be rsynced into a cache
/// and remain valid.
pub(crate) fn object_key(prefix: &str, object: &ObjectId) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        object.cache_key()
    } else {
        format!("{prefix}/{}", object.cache_key())
    }
}

/// Recover an [`ObjectId`] from a listing key, ignoring anything that does not
/// look like an object we wrote.
pub(crate) fn object_from_key(key: &str) -> Option<ObjectId> {
    let mut segments = key.rsplit('/');
    let hash = segments.next()?;
    let fanout = segments.next()?;
    let algorithm = segments.next()?;
    if algorithm != crate::ALGORITHM || fanout != hash.get(..2)? {
        return None;
    }
    ObjectId::new(hash).ok()
}
