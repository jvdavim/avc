//! Provider-neutral object transfer.
//!
//! Every remote is reached through [`ObjectStore`], which speaks only in terms
//! of [`ObjectId`] values. Backends never learn a repository path, and callers
//! never learn a URL scheme.

mod credentials;
mod file;
mod s3;
mod sigv4;
pub mod tls;
mod xml;

use std::io::Read;

pub use credentials::{Credentials, LocalRemoteOverride};
pub use file::FileStore;
pub use s3::{S3Settings, S3Store};
pub use tls::TrustRoots;

use crate::{Algorithm, ObjectId, Provider, RemoteConfig, Result};

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

    /// Have the server copy `source` into this object's key, without the bytes
    /// travelling through this process.
    ///
    /// `Ok(false)` means it could not be done — a different service at the far
    /// end, or an object past what a single-request copy will carry — and the
    /// caller should fall back to streaming it. Implementations that cannot do
    /// it at all say so by inheriting this default.
    fn put_copy(&self, _source: &CopySource, _object: &ObjectId, _size: u64) -> Result<bool> {
        Ok(false)
    }
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
///
/// The algorithm comes out of the key rather than being assumed, so a store
/// holding both SHA-256 objects and objects preserved from a migration lists
/// as one set.
pub(crate) fn object_from_key(key: &str) -> Option<ObjectId> {
    let mut segments = key.rsplit('/');
    let hash = segments.next()?;
    let fanout = segments.next()?;
    let algorithm: Algorithm = segments.next()?.parse().ok()?;
    if fanout != hash.get(..2)? {
        return None;
    }
    ObjectId::new(algorithm, hash).ok()
}

/// The directory every object key of every algorithm sits beneath.
///
/// One listing therefore sees the whole store, which is what keeps `avc list`
/// and `avc push` to a single round trip however many algorithms are in play.
pub(crate) fn objects_prefix(prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        "objects/".to_owned()
    } else {
        format!("{prefix}/objects/")
    }
}

/// Where a server should copy an object from, when it can do that itself.
///
/// Migrating a large object store is otherwise a full download followed by a
/// full upload. When both ends are the same S3 service the bytes never need to
/// leave it, and this is how the destination is told where to look.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CopySource {
    S3 {
        /// Scheme and authority, compared against the destination's own: a
        /// server can only copy from storage it is itself serving.
        endpoint: String,
        bucket: String,
        key: String,
    },
    File {
        path: std::path::PathBuf,
    },
}

/// Read access to a store by literal key.
///
/// [`ObjectStore`] deliberately speaks only in [`ObjectId`]s, because that is
/// what keeps repository paths out of a bucket. Reading a *foreign* layout —
/// another tool's cache, laid out by another tool's rules — needs the opposite,
/// so it is a separate trait that only import paths ever hold.
pub trait KeyStore {
    fn describe(&self) -> String;

    /// Open a stream of the bytes at `key`.
    fn get_key(&self, key: &str) -> Result<Box<dyn Read>>;

    /// Every key beneath `prefix`, with its size.
    fn list_keys(&self, prefix: &str) -> Result<Vec<(String, u64)>>;

    /// How a destination store should refer to `key` in a server-side copy.
    fn copy_source(&self, key: &str) -> CopySource;
}

/// Build a raw-key reader for `remote`.
///
/// Separate from [`open`] because the two are used together and for different
/// halves of an import: the source is read by key, the destination is written
/// by object identity.
pub fn open_source(
    remote: &RemoteConfig,
    local: Option<&LocalRemoteOverride>,
) -> Result<Box<dyn KeyStore>> {
    match remote.provider {
        Provider::File => Ok(Box::new(FileStore::new(remote))),
        Provider::S3 => Ok(Box::new(S3Store::new(S3Settings::resolve(remote, local)?)?)),
        Provider::Gcs => Err(crate::Error::UnsupportedProvider("gcs")),
        Provider::Azure => Err(crate::Error::UnsupportedProvider("azure")),
    }
}
