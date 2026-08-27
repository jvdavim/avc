use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{ObjectId, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashResult {
    pub object: ObjectId,
    pub size: u64,
}

/// Incremental hasher for bytes that arrive from somewhere other than a file.
///
/// A download is written and hashed in the same pass, so verifying a 40 GB
/// artifact costs one read of the network stream and no second read of the disk.
#[derive(Default)]
pub struct StreamHasher {
    hasher: Sha256,
    size: u64,
}

impl StreamHasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.size = self.size.saturating_add(bytes.len() as u64);
    }

    /// Consume the hasher and report what it saw.
    pub fn finish(self) -> Result<HashResult> {
        Ok(HashResult {
            object: ObjectId::new(format!("{:x}", self.hasher.finalize()))?,
            size: self.size,
        })
    }
}

pub fn hash_reader(reader: &mut impl Read) -> Result<HashResult> {
    let mut hasher = StreamHasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        // `StreamHasher` saturates rather than failing; a file large enough to
        // overflow u64 is a bug worth reporting, not silently clamping.
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("file size overflow"))?;
    }
    hasher.finish()
}

pub fn hash_file(path: impl AsRef<Path>) -> Result<HashResult> {
    hash_reader(&mut File::open(path)?)
}
