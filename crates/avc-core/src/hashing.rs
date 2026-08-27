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

pub fn hash_reader(reader: &mut impl Read) -> Result<HashResult> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("file size overflow"))?;
    }
    let hash = format!("{:x}", hasher.finalize());
    Ok(HashResult {
        object: ObjectId::new(hash)?,
        size,
    })
}

pub fn hash_file(path: impl AsRef<Path>) -> Result<HashResult> {
    hash_reader(&mut File::open(path)?)
}
