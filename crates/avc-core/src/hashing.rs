use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use md5::Md5;
use sha2::{Digest, Sha256};

use crate::hex;
use crate::{Algorithm, ObjectId, Result};

/// Bytes moved per read.
///
/// Large enough that a multi-gigabyte artifact is not paced by syscall
/// overhead, small enough to stay well inside a CPU cache alongside the digest
/// state. Heap-allocated: a buffer this size does not belong on the stack.
const CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashResult {
    pub object: ObjectId,
    pub size: u64,
}

/// The digest state for whichever algorithm is in use.
///
/// An enum rather than a boxed trait object: there are two of these, the choice
/// is made once per artifact, and the inner loop is hot enough that a virtual
/// call per megabyte is not worth paying for the polymorphism.
enum Digester {
    Sha256(Box<Sha256>),
    Md5(Box<Md5>),
}

/// Incremental hasher for bytes that arrive from somewhere other than a file.
///
/// A download is written and hashed in the same pass, so verifying a 40 GB
/// artifact costs one read of the network stream and no second read of the disk.
pub struct StreamHasher {
    digester: Digester,
    size: u64,
}

impl Default for StreamHasher {
    fn default() -> Self {
        Self::new(Algorithm::default())
    }
}

impl StreamHasher {
    pub fn new(algorithm: Algorithm) -> Self {
        Self {
            digester: match algorithm {
                Algorithm::Sha256 => Digester::Sha256(Box::default()),
                Algorithm::Md5 => Digester::Md5(Box::default()),
            },
            size: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match &mut self.digester {
            Digester::Sha256(hasher) => hasher.update(bytes),
            Digester::Md5(hasher) => hasher.update(bytes),
        }
        self.size = self.size.saturating_add(bytes.len() as u64);
    }

    /// Consume the hasher and report what it saw.
    pub fn finish(self) -> Result<HashResult> {
        let (algorithm, hash) = match self.digester {
            Digester::Sha256(hasher) => (Algorithm::Sha256, hex::encode(&hasher.finalize())),
            Digester::Md5(hasher) => (Algorithm::Md5, hex::encode(&hasher.finalize())),
        };
        Ok(HashResult {
            object: ObjectId::new(algorithm, hash)?,
            size: self.size,
        })
    }
}

pub fn hash_reader(reader: &mut impl Read, algorithm: Algorithm) -> Result<HashResult> {
    hash_copy(reader, &mut io::sink(), algorithm)
}

pub fn hash_file(path: impl AsRef<Path>, algorithm: Algorithm) -> Result<HashResult> {
    hash_reader(&mut File::open(path)?, algorithm)
}

/// Copy every byte of `reader` into `writer`, hashing as it goes.
///
/// One pass over the bytes for both jobs. Storing an artifact needs its content
/// address *and* a copy in the cache, and doing those separately reads a
/// 40 GB file twice — the second time from a page cache the first read has
/// already evicted. The address is only known at the end, so a caller writes to
/// a temporary file and names it once this returns.
pub fn hash_copy(
    reader: &mut impl Read,
    writer: &mut impl Write,
    algorithm: Algorithm,
) -> Result<HashResult> {
    let mut hasher = StreamHasher::new(algorithm);
    let mut buffer = vec![0_u8; CHUNK];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        // `StreamHasher` saturates rather than failing; a file large enough to
        // overflow u64 is a bug worth reporting, not silently clamping.
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("file size overflow"))?;
    }
    hasher.finish()
}
