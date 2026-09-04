//! Behaviour every `ObjectStore` backend must share.
//!
//! The `file://` backend runs everywhere, so the contract is exercised on every
//! CI run. The S3 backend runs the *same* assertions against a real server when
//! `AVC_TEST_S3_ENDPOINT` is set — point it at a MinIO instance and the two
//! backends are held to one standard rather than two.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use avc_core::remote::{self, LocalRemoteOverride, ObjectStore};
use avc_core::{ObjectId, Provider, RemoteConfig};

/// A scratch directory that removes itself, so a failing test leaves no litter.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn object_for(bytes: &[u8]) -> (ObjectId, Vec<u8>) {
    let result = avc_core::hash_reader(
        &mut Cursor::new(bytes.to_vec()),
        avc_core::Algorithm::Sha256,
    )
    .unwrap();
    (result.object, bytes.to_vec())
}

/// The contract: an absent object is absent, a stored object round-trips
/// byte-for-byte, storing is idempotent, and listing reports what was stored.
fn assert_store_contract(store: &dyn ObjectStore, marker: &[u8]) {
    let (object, bytes) = object_for(marker);

    assert!(
        !store.exists(&object).unwrap(),
        "a never-stored object must not be reported as present"
    );
    assert!(
        matches!(store.get(&object), Err(avc_core::Error::ObjectNotFound(_))),
        "reading an absent object must say so specifically, not fail generically"
    );

    store
        .put(&object, bytes.len() as u64, &mut Cursor::new(bytes.clone()))
        .unwrap();

    assert!(store.exists(&object).unwrap());

    let mut downloaded = Vec::new();
    std::io::copy(&mut store.get(&object).unwrap(), &mut downloaded).unwrap();
    assert_eq!(downloaded, bytes, "downloaded bytes must be exact");

    // Objects are immutable and keyed by content, so re-storing is defined to
    // be harmless rather than an error.
    store
        .put(&object, bytes.len() as u64, &mut Cursor::new(bytes.clone()))
        .unwrap();

    let listed = store.list().unwrap();
    let found = listed
        .iter()
        .find(|candidate| candidate.object == object)
        .expect("a stored object must appear in the listing");
    assert_eq!(found.size, bytes.len() as u64);
}

#[test]
fn file_backend_satisfies_the_object_store_contract() {
    let directory = TempDir::new("file");
    let remote = RemoteConfig {
        name: "local".into(),
        provider: Provider::File,
        bucket_or_container: directory.0.to_string_lossy().into_owned(),
        prefix: "artifacts".into(),
        endpoint_url: None,
        region: None,
        profile: None,
    };
    let store = remote::open(&remote, None).unwrap();
    assert_store_contract(store.as_ref(), b"file backend contract");
}

#[test]
fn file_backend_lists_nothing_before_anything_is_pushed() {
    let directory = TempDir::new("empty");
    let remote = RemoteConfig {
        name: "local".into(),
        provider: Provider::File,
        bucket_or_container: directory.0.to_string_lossy().into_owned(),
        prefix: String::new(),
        endpoint_url: None,
        region: None,
        profile: None,
    };
    let store = remote::open(&remote, None).unwrap();
    assert!(store.list().unwrap().is_empty());
}

/// A prefix keeps two repositories from seeing each other's objects even when
/// they share a bucket.
#[test]
fn prefixes_isolate_two_remotes_in_one_location() {
    let directory = TempDir::new("prefix");
    let build = |prefix: &str| RemoteConfig {
        name: "local".into(),
        provider: Provider::File,
        bucket_or_container: directory.0.to_string_lossy().into_owned(),
        prefix: prefix.into(),
        endpoint_url: None,
        region: None,
        profile: None,
    };
    let first = remote::open(&build("team-a"), None).unwrap();
    let second = remote::open(&build("team-b"), None).unwrap();

    let (object, bytes) = object_for(b"only team a has this");
    first
        .put(&object, bytes.len() as u64, &mut Cursor::new(bytes))
        .unwrap();

    assert!(first.exists(&object).unwrap());
    assert!(!second.exists(&object).unwrap());
    assert!(second.list().unwrap().is_empty());
}

#[test]
fn unimplemented_providers_report_themselves_clearly() {
    for (provider, name) in [(Provider::Gcs, "gcs"), (Provider::Azure, "azure")] {
        let remote = RemoteConfig {
            name: "origin".into(),
            provider,
            bucket_or_container: "bucket".into(),
            prefix: String::new(),
            endpoint_url: None,
            region: None,
            profile: None,
        };
        let error = match remote::open(&remote, None) {
            Err(error) => error,
            Ok(_) => panic!("{name} has no adapter yet and must not open a store"),
        };
        assert!(matches!(error, avc_core::Error::UnsupportedProvider(_)));
        assert!(error.to_string().contains(name));
        // Reported as an operational failure, so `SPEC.md`'s exit code 3
        // applies rather than the user-error code.
        assert!(error.is_provider_failure());
    }
}

/// Live test against a real S3-compatible server. Skipped unless configured.
///
/// ```sh
/// export AVC_TEST_S3_ENDPOINT=http://127.0.0.1:9000
/// export AVC_TEST_S3_BUCKET=avc-test
/// export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin
/// cargo test -p avc-core --test object_store -- --ignored
/// ```
#[test]
#[ignore = "requires a running S3-compatible server; see the doc comment"]
fn s3_backend_satisfies_the_object_store_contract() {
    let Ok(endpoint) = std::env::var("AVC_TEST_S3_ENDPOINT") else {
        panic!("set AVC_TEST_S3_ENDPOINT to run this test");
    };
    let bucket = std::env::var("AVC_TEST_S3_BUCKET").unwrap_or_else(|_| "avc-test".into());
    let remote = RemoteConfig {
        name: "minio".into(),
        provider: Provider::S3,
        bucket_or_container: bucket,
        // A unique prefix keeps repeated runs from colliding, and keeps the
        // "absent object" assertions meaningful.
        prefix: format!("it-{}", std::process::id()),
        endpoint_url: Some(endpoint),
        region: None,
        profile: None,
    };
    let local = LocalRemoteOverride {
        name: "minio".into(),
        force_path_style: Some(true),
        ..Default::default()
    };
    let store = remote::open(&remote, Some(&local)).unwrap();
    assert_store_contract(
        store.as_ref(),
        format!("s3 backend contract {}", std::process::id()).as_bytes(),
    );
}
