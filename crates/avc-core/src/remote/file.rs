//! Offline `file://` backend.
//!
//! Kept behind the same trait as the cloud adapters so that a bug found with a
//! local directory is a bug found everywhere.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{object_from_key, object_key, ObjectStore, RemoteObject};
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

    fn list(&self) -> Result<Vec<RemoteObject>> {
        let mut found = Vec::new();
        let base = if self.prefix.trim_matches('/').is_empty() {
            self.root.clone()
        } else {
            self.root.join(self.prefix.trim_matches('/'))
        };
        let base = base.join("objects").join(crate::ALGORITHM);
        if !base.is_dir() {
            return Ok(found);
        }
        for fanout in read_dir_sorted(&base)? {
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
                let Some(object) =
                    object_from_key(&format!("{}/{bucket}/{hash}", crate::ALGORITHM))
                else {
                    continue;
                };
                found.push(RemoteObject {
                    object,
                    size: entry.metadata()?.len(),
                });
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
