//! Offline `file://` backend.
//!
//! Kept behind the same trait as the cloud adapters so that a bug found with a
//! local directory is a bug found everywhere.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{object_from_key, object_key, CopySource, KeyStore, ObjectStore, RemoteObject};
use crate::{Error, ObjectId, RemoteConfig, Result};

pub struct FileStore {
    root: PathBuf,
    prefix: String,
}

impl FileStore {
    pub fn new(remote: &RemoteConfig) -> Self {
        Self {
            root: PathBuf::from(&remote.bucket_or_container),
            prefix: remote.prefix.clone(),
        }
    }

    fn path(&self, object: &ObjectId) -> PathBuf {
        self.root.join(object_key(&self.prefix, object))
    }

    /// Where a literal key lands beneath this store's root.
    fn key_path(&self, key: &str) -> PathBuf {
        let prefix = self.prefix.trim_matches('/');
        let mut path = self.root.clone();
        if !prefix.is_empty() {
            path.push(prefix);
        }
        for segment in key.split('/').filter(|segment| !segment.is_empty()) {
            path.push(segment);
        }
        path
    }
}

impl KeyStore for FileStore {
    fn describe(&self) -> String {
        format!("file://{}", self.root.display())
    }

    fn get_key(&self, key: &str) -> Result<Box<dyn Read>> {
        let path = self.key_path(key);
        match File::open(&path) {
            Ok(file) => Ok(Box::new(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::ObjectNotFound(key.to_owned()))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        let base = self.key_path(prefix);
        let mut found = Vec::new();
        // A prefix names a directory here rather than a string to match on,
        // which is the one place the filesystem and an object store differ in
        // a way callers can see.
        if base.is_dir() {
            walk(&base, &mut String::new(), &mut found)?;
            let prefix = prefix.trim_matches('/');
            if !prefix.is_empty() {
                for (key, _) in &mut found {
                    *key = format!("{prefix}/{key}");
                }
            }
        }
        found.sort();
        Ok(found)
    }

    fn copy_source(&self, key: &str) -> CopySource {
        CopySource::File {
            path: self.key_path(key),
        }
    }
}

/// Collect every file beneath `directory`, keyed by its path relative to the
/// directory the walk started at.
fn walk(directory: &Path, relative: &mut String, output: &mut Vec<(String, u64)>) -> Result<()> {
    for path in read_dir_sorted(directory)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let length = relative.len();
        if !relative.is_empty() {
            relative.push('/');
        }
        relative.push_str(name);
        if path.is_dir() {
            walk(&path, relative, output)?;
        } else if let Ok(metadata) = path.metadata() {
            output.push((relative.clone(), metadata.len()));
        }
        relative.truncate(length);
    }
    Ok(())
}

impl ObjectStore for FileStore {
    fn describe(&self) -> String {
        format!("file://{}", self.root.display())
    }

    fn put(&self, object: &ObjectId, _size: u64, body: &mut dyn Read) -> Result<()> {
        let destination = self.path(object);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write beside the destination so the rename cannot cross a filesystem
        // boundary, then publish atomically. A reader never sees a partial object.
        let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
        let mut output = File::create(&temporary)?;
        let copied = std::io::copy(body, &mut output);
        let result = copied.and_then(|_| output.sync_all());
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        drop(output);
        fs::rename(&temporary, &destination)?;
        Ok(())
    }

    fn get(&self, object: &ObjectId) -> Result<Box<dyn Read>> {
        let path = self.path(object);
        match File::open(&path) {
            Ok(file) => Ok(Box::new(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::ObjectNotFound(object.hash().to_owned()))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn exists(&self, object: &ObjectId) -> Result<bool> {
        Ok(self.path(object).is_file())
    }

    fn put_copy(&self, source: &CopySource, object: &ObjectId, _size: u64) -> Result<bool> {
        // Only a file source: there is nothing this process could ask a remote
        // service to do that would land bytes on this disk without them
        // passing through it.
        let CopySource::File { path } = source else {
            return Ok(false);
        };
        let destination = self.path(object);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_extension(format!("tmp-{}", std::process::id()));
        fs::copy(path, &temporary)?;
        fs::rename(&temporary, &destination)?;
        Ok(true)
    }

    fn list(&self) -> Result<Vec<RemoteObject>> {
        let mut found = Vec::new();
        let base = if self.prefix.trim_matches('/').is_empty() {
            self.root.clone()
        } else {
            self.root.join(self.prefix.trim_matches('/'))
        };
        let base = base.join("objects");
        if !base.is_dir() {
            return Ok(found);
        }
        // One level deeper than it used to be: `objects/` now holds a
        // directory per algorithm, and a store may hold more than one.
        for algorithm in read_dir_sorted(&base)? {
            let Some(name) = algorithm.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !algorithm.is_dir() {
                continue;
            }
            for fanout in read_dir_sorted(&algorithm)? {
                if !fanout.is_dir() {
                    continue;
                }
                let Some(bucket) = fanout.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                for entry in read_dir_sorted(&fanout)? {
                    let Some(hash) = entry.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    let Some(object) = object_from_key(&format!("{name}/{bucket}/{hash}")) else {
                        continue;
                    };
                    found.push(RemoteObject {
                        object,
                        size: entry.metadata()?.len(),
                    });
                }
            }
        }
        Ok(found)
    }
}

fn read_dir_sorted(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        paths.push(entry?.path());
    }
    paths.sort();
    Ok(paths)
}
