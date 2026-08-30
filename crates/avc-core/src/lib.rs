//! Domain primitives for Artifact Version Control.

mod config;
mod hashing;
mod object;
mod path;
mod pointer;
pub mod remote;
mod tree;

pub use config::{Provider, RemoteConfig};
pub use hashing::{hash_file, hash_reader, HashResult, StreamHasher};
pub use object::{ObjectId, ALGORITHM};
pub use path::{normalize_repo_path, pointer_path, validate_repo_path};
pub use pointer::{Artifact, ArtifactKind, ObjectMetadata, Pointer, POINTER_VERSION};
pub use remote::{Credentials, LocalRemoteOverride, ObjectStore, RemoteObject, TrustRoots};
pub use tree::{Tree, TreeEntry, TREE_MEDIA_TYPE, TREE_VERSION};

/// Errors returned by core validation and serialization operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid object ID: {0}")]
    InvalidObjectId(String),
    #[error("invalid repository path: {0}")]
    InvalidPath(String),
    #[error("invalid remote URL: {0}")]
    InvalidRemote(String),
    #[error("unsupported pointer version: {0}")]
    UnsupportedPointerVersion(u32),
    #[error("unsupported directory manifest version: {0}")]
    UnsupportedTreeVersion(u32),
    #[error("invalid directory manifest: {0}")]
    InvalidTree(String),
    #[error("pointer serialization failed: {0}")]
    PointerSerialization(#[from] serde_yaml::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote object not found: {0}")]
    ObjectNotFound(String),
    #[error("no credentials found for profile '{0}'; set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or add them to .avc/config.local.toml")]
    MissingCredentials(String),
    #[error("provider adapter not implemented: {0}")]
    UnsupportedProvider(&'static str),
    #[error("{0}")]
    Tls(String),
    #[error("{0}")]
    Provider(String),
}

impl Error {
    /// Whether this is a provider or operational failure rather than a user,
    /// data, or state error. `SPEC.md` reserves exit code 3 for these.
    pub fn is_provider_failure(&self) -> bool {
        matches!(
            self,
            Error::Provider(_)
                | Error::MissingCredentials(_)
                | Error::UnsupportedProvider(_)
                | Error::Tls(_)
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn hashes_stream_without_loading_all_bytes() {
        let mut input = Cursor::new(vec![b'a'; 128 * 1024]);
        let result = hash_reader(&mut input).unwrap();
        assert_eq!(result.size, 128 * 1024);
        assert_eq!(
            result.object.hash(),
            "b44ffb72fcc259676bd80495fef1b44b808ca8f1ffe1b1706a4d7911b0e31f11"
        );
    }

    #[test]
    fn pointer_serialization_is_stable_and_round_trips() {
        let object =
            ObjectId::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let pointer = Pointer::new(
            "data/model.bin",
            object,
            42,
            Some("application/octet-stream".into()),
        )
        .unwrap();
        let yaml = pointer.serialize_canonical().unwrap();
        assert_eq!(yaml, "version: 1\npath: data/model.bin\nobject:\n  algorithm: sha256\n  hash: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  size: 42\n  media_type: application/octet-stream\n");
        assert_eq!(Pointer::parse(&yaml).unwrap(), pointer);
    }

    #[test]
    fn rejects_invalid_pointer_data() {
        let invalid = "version: 1\npath: ../secret\nobject:\n  algorithm: sha256\n  hash: bad\n  size: 1\nextra: true\n";
        assert!(Pointer::parse(invalid).is_err());
        assert!(ObjectId::new("not-a-hash").is_err());
        assert!(validate_repo_path("../secret").is_err());
        assert!(validate_repo_path("/absolute").is_err());
        assert!(validate_repo_path("a\\b").is_err());
    }

    #[test]
    fn supports_unicode_repository_paths() {
        let object =
            ObjectId::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let pointer = Pointer::new("données/模型.bin", object, 1, None).unwrap();
        assert_eq!(pointer.path, "données/模型.bin");
        assert_eq!(
            pointer_path("données/模型.bin").unwrap().to_str().unwrap(),
            "données/模型.bin.avc"
        );
    }

    #[test]
    fn directory_pointer_names_its_manifest_and_files_stay_unchanged() {
        let manifest =
            ObjectId::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let pointer = Pointer::new_directory("data", manifest.clone(), 387).unwrap();
        assert!(pointer.is_directory());
        let yaml = pointer.serialize_canonical().unwrap();
        assert_eq!(
            yaml,
            "version: 1\npath: data\nkind: directory\nobject:\n  algorithm: sha256\n  hash: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  size: 387\n  media_type: application/vnd.avc.tree+yaml\n"
        );
        assert_eq!(Pointer::parse(&yaml).unwrap(), pointer);

        // A file pointer keeps the bytes it had before directories existed,
        // and a pointer written by an older AVC still parses as a file.
        let file = Pointer::new("model.bin", manifest, 42, None).unwrap();
        let yaml = file.serialize_canonical().unwrap();
        assert!(!yaml.contains("kind"));
        assert_eq!(Pointer::parse(&yaml).unwrap().kind, ArtifactKind::File);
    }

    #[test]
    fn manifest_order_is_canonical_so_a_directory_has_one_identity() {
        let object = |byte: char| {
            ObjectId::new(std::iter::repeat(byte).take(64).collect::<String>()).unwrap()
        };
        let entry = |path: &str, byte: char| TreeEntry::new(path, object(byte), 1).unwrap();

        // Discovery order must not change the manifest, or the same directory
        // would hash differently on two machines.
        let one = Tree::new(vec![entry("nested/b.bin", 'b'), entry("a.bin", 'a')]).unwrap();
        let two = Tree::new(vec![entry("a.bin", 'a'), entry("nested/b.bin", 'b')]).unwrap();
        assert_eq!(one, two);
        assert_eq!(one.total_size(), 2);
        let yaml = one.serialize_canonical().unwrap();
        assert_eq!(Tree::parse(&yaml).unwrap(), one);

        // A manifest is untrusted input that decides where checkout writes.
        assert!(Tree::parse(
            "version: 1\nentries:\n- path: ../escape\n  algorithm: sha256\n  hash: aa\n  size: 1\n"
        )
        .is_err());
        assert!(Tree::parse("version: 2\nentries: []\n").is_err());
        assert!(Tree::parse("version: 1\nentries: []\nextra: true\n").is_err());
        // Unsorted or duplicated entries did not come from `Tree::new`.
        let unsorted = yaml.replace("- path: a.bin", "- path: z.bin");
        assert!(Tree::parse(&unsorted).is_err());
    }

    #[test]
    fn a_trailing_slash_names_the_same_artifact() {
        assert_eq!(normalize_repo_path("data/").unwrap(), "data");
        assert_eq!(normalize_repo_path("data/nested//").unwrap(), "data/nested");
        assert_eq!(pointer_path("data/").unwrap().to_str().unwrap(), "data.avc");
        assert!(normalize_repo_path("/").is_err());
    }

    #[test]
    fn parses_explicit_remote_schemes() {
        let s3 = RemoteConfig::from_url("origin", "s3://bucket/path").unwrap();
        assert_eq!(s3.provider, Provider::S3);
        assert_eq!(s3.prefix, "path");
        let compatible =
            RemoteConfig::from_url("local", "s3+https://storage.example/bucket/path").unwrap();
        assert_eq!(compatible.bucket_or_container, "bucket");
        assert_eq!(compatible.prefix, "path");
        assert_eq!(
            compatible.endpoint_url.as_deref(),
            Some("https://storage.example")
        );
        assert!(RemoteConfig::from_url("origin", "https://bucket/path").is_err());
        assert!(RemoteConfig::from_url("origin", "s3://").is_err());
    }
}
