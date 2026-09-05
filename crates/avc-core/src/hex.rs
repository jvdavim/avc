//! Lower-case hexadecimal encoding of digest output.
//!
//! Every hash this crate produces is eventually written as text — into a
//! pointer file, into an S3 signature — and the digest crates hand back plain
//! bytes rather than something that formats itself.

pub fn encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}
