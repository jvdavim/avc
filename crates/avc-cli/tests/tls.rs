//! Trusting a private certificate authority.
//!
//! On a network that inspects TLS, every HTTPS server AVC talks to presents a
//! certificate signed by a CA that only that organization knows about. These
//! tests assert the two halves of the answer: the trust configuration is read
//! and validated *before* a connection is attempted, so a mistake in it is
//! reported as itself, and a usable bundle is accepted and gets out of the way.
//!
//! The half that cannot be tested without a TLS server — that a certificate
//! signed by a bundled CA actually verifies — is rustls's own contract, not
//! this crate's.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A real self-signed certificate, generated once with
/// `openssl req -x509 -newkey ed25519`. Nothing here connects to anything.
const CERTIFICATE: &str = "\
-----BEGIN CERTIFICATE-----
MIIBPDCB76ADAgECAhQYbcSWaslOPY3cEGIJcUT4WhBNnjAFBgMrZXAwFDESMBAG
A1UEAwwJYXZjLXRlc3RzMB4XDTI2MDgzMDEzNTgyOVoXDTI2MDgzMTEzNTgyOVow
FDESMBAGA1UEAwwJYXZjLXRlc3RzMCowBQYDK2VwAyEAh/ZWl1JtQu8XqGP31opF
8o5wXUaoXOcktb1WMC9cy8OjUzBRMB0GA1UdDgQWBBTYBvGeHsQYquI0H/kchzms
AHr2MzAfBgNVHSMEGDAWgBTYBvGeHsQYquI0H/kchzmsAHr2MzAPBgNVHRMBAf8E
BTADAQH/MAUGAytlcANBAC3GfVRmtUyfnixtM8zuIm4miFzL7m4wEZYP6+WRQzBk
A0ftXvclmtDqlIIkZkf7tyI6VMsxQbu4FhSjtUfVPAA=
-----END CERTIFICATE-----
";

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = format!(
            "avc-tls-{label}-{}-{:?}",
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

/// Run `avc` with an environment that decides nothing on its own: a developer
/// who has `AWS_CA_BUNDLE` exported must not change what these tests see.
fn avc(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(arguments)
        .env("NO_COLOR", "1")
        .env("AWS_ACCESS_KEY_ID", "key")
        .env("AWS_SECRET_ACCESS_KEY", "secret")
        .env_remove("AVC_CA_BUNDLE")
        .env_remove("AVC_SYSTEM_CERTS")
        .env_remove("AWS_CA_BUNDLE")
        .env_remove("SSL_CERT_FILE")
        .current_dir(directory)
        .output()
        .expect("the avc binary should run")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// An initialized repository whose remote is an HTTPS endpoint nothing listens
/// on: any message about a certificate must come from configuration, not from
/// a handshake.
fn repository(label: &str) -> TempDir {
    let directory = TempDir::new(label);
    let git = Command::new("git")
        .args(["init", "--quiet", "-b", "main"])
        .current_dir(&directory.0)
        .output()
        .expect("these tests need the git command");
    assert!(git.status.success());
    assert!(avc(&directory.0, &["init"]).status.success());
    // Something to push, so a run that gets past the trust configuration goes
    // on to attempt a connection rather than finding nothing to do.
    fs::write(directory.0.join("model.bin"), "weights\n").unwrap();
    assert!(avc(&directory.0, &["add", "model.bin"]).status.success());
    assert!(avc(
        &directory.0,
        &[
            "remote",
            "add",
            "origin",
            "s3+https://127.0.0.1:1/my-bucket"
        ]
    )
    .status
    .success());
    directory
}

#[test]
fn a_ca_bundle_that_cannot_be_read_is_reported_as_itself() {
    let repository = repository("missing");
    let absent = repository.0.join("corporate-root.pem");
    let output = Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(["push"])
        .env("NO_COLOR", "1")
        .env("AWS_ACCESS_KEY_ID", "key")
        .env("AWS_SECRET_ACCESS_KEY", "secret")
        .env("AVC_CA_BUNDLE", &absent)
        .current_dir(&repository.0)
        .output()
        .unwrap();

    let message = stderr(&output);
    assert!(message.contains("cannot read the CA bundle"), "{message}");
    assert!(message.contains("corporate-root.pem"), "{message}");
    // Operational, so `SPEC.md`'s exit code 3 applies.
    assert_eq!(output.status.code(), Some(3), "{message}");
}

#[test]
fn a_file_that_is_not_pem_says_so_rather_than_failing_to_connect() {
    let repository = repository("notpem");
    let bundle = repository.0.join("der.crt");
    fs::write(&bundle, [0x30, 0x82, 0x01, 0x00]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(["push"])
        .env("NO_COLOR", "1")
        .env("AWS_ACCESS_KEY_ID", "key")
        .env("AWS_SECRET_ACCESS_KEY", "secret")
        .env("AVC_CA_BUNDLE", &bundle)
        .current_dir(&repository.0)
        .output()
        .unwrap();

    let message = stderr(&output);
    assert!(message.contains("no certificates"), "{message}");
    assert!(message.contains("openssl"), "{message}");
}

/// A usable bundle is accepted and then gets out of the way: what fails is the
/// connection to a port nothing listens on, which is the next thing to fail.
#[test]
fn a_usable_bundle_is_accepted_from_every_place_it_can_be_named() {
    let repository = repository("accepted");
    let bundle = repository.0.join("corporate-root.pem");
    fs::write(&bundle, CERTIFICATE).unwrap();

    let from_env = |name: &str| {
        Command::new(env!("CARGO_BIN_EXE_avc"))
            .args(["push"])
            .env("NO_COLOR", "1")
            .env("AWS_ACCESS_KEY_ID", "key")
            .env("AWS_SECRET_ACCESS_KEY", "secret")
            .env(name, &bundle)
            .current_dir(&repository.0)
            .output()
            .unwrap()
    };

    // `AWS_CA_BUNDLE` and `SSL_CERT_FILE` are honoured because a managed
    // machine usually has one of them set already.
    for name in ["AVC_CA_BUNDLE", "AWS_CA_BUNDLE", "SSL_CERT_FILE"] {
        let message = stderr(&from_env(name));
        assert!(!message.contains("CA bundle"), "{name}: {message}");
        assert!(message.contains("failed"), "{name}: {message}");
    }

    // And the same path in the gitignored machine-local configuration.
    fs::write(
        repository.0.join(".avc/config.local.toml"),
        format!(
            "[[remotes]]\nname = \"origin\"\nca_bundle = {:?}\n",
            bundle.display().to_string()
        ),
    )
    .unwrap();
    let message = stderr(&avc(&repository.0, &["push"]));
    assert!(!message.contains("CA bundle"), "{message}");
}

/// The system trust store needs no file, which is the point of it: on a managed
/// machine the private CA is already installed there.
#[test]
fn the_system_trust_store_can_be_asked_for_without_a_path() {
    let repository = repository("system");
    let output = Command::new(env!("CARGO_BIN_EXE_avc"))
        .args(["push"])
        .env("NO_COLOR", "1")
        .env("AWS_ACCESS_KEY_ID", "key")
        .env("AWS_SECRET_ACCESS_KEY", "secret")
        .env("AVC_SYSTEM_CERTS", "1")
        .current_dir(&repository.0)
        .output()
        .unwrap();

    let message = stderr(&output);
    assert!(!message.contains("CA bundle"), "{message}");
    assert!(message.contains("failed"), "{message}");
}
