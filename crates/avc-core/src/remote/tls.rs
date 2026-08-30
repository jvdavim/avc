//! Which certificate authorities an HTTPS connection is allowed to trust.
//!
//! This module exists for one situation: a network that terminates TLS at a
//! proxy and re-signs it with a private certificate authority. The certificate
//! the proxy presents is perfectly valid — but it is issued by a CA that only
//! that organization knows about, so a client carrying the public root set
//! rejects it, and every transfer fails with a certificate error that says
//! nothing about how to fix it.
//!
//! Three sources of trust are available, and exactly one is in effect:
//!
//! * [`TrustRoots::Builtin`] — the Mozilla root set compiled into the binary.
//!   The default, and the right answer on an ordinary network.
//! * [`TrustRoots::Bundle`] — a PEM file. This *replaces* the built-in roots,
//!   which is what `AWS_CA_BUNDLE`, `SSL_CERT_FILE`, and `curl --cacert` all
//!   mean, so a bundle must contain every CA the run needs and not only the
//!   private one.
//! * [`TrustRoots::System`] — the trust store the operating system maintains.
//!   On a machine an IT department manages, the private CA is already installed
//!   there alongside the public roots, so this needs no path and no file.
//!
//! Nothing here can be used to *skip* verification. There is no flag for it and
//! no environment variable that enables it: a run either trusts a CA that signed
//! what it received, or it fails.

use std::env;
use std::path::{Path, PathBuf};

use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig};

use super::credentials::LocalRemoteOverride;
use crate::{Error, Result};

/// The certificate authorities a transfer may verify a server against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustRoots {
    /// The Mozilla root set compiled into this binary.
    Builtin,
    /// The operating system's own trust store.
    System,
    /// A PEM bundle, replacing the built-in roots.
    Bundle(PathBuf),
}

impl TrustRoots {
    /// How this is spelled in output, for `avc doctor` and error messages.
    pub fn describe(&self) -> String {
        match self {
            TrustRoots::Builtin => "built-in Mozilla roots".to_owned(),
            TrustRoots::System => "the system trust store".to_owned(),
            TrustRoots::Bundle(path) => format!("CA bundle {}", path.display()),
        }
    }
}

/// Resolve which roots to trust, first match winning:
///
/// 1. `AVC_CA_BUNDLE`
/// 2. `AVC_SYSTEM_CERTS`
/// 3. `AWS_CA_BUNDLE`, then `SSL_CERT_FILE` — already set on many managed
///    machines, and honoured so AVC needs no separate ceremony there.
/// 4. `ca_bundle`, then `use_system_certs`, in `.avc/config.local.toml`
/// 5. The built-in roots.
///
/// Machine-local, never tracked: the path to an organization's bundle is a
/// property of the machine the command runs on, not of the repository, and a
/// clone taken home from the office must not inherit it.
pub fn resolve(local: Option<&LocalRemoteOverride>) -> TrustRoots {
    if let Some(path) = non_empty_env("AVC_CA_BUNDLE") {
        return TrustRoots::Bundle(PathBuf::from(path));
    }
    if env_flag("AVC_SYSTEM_CERTS") {
        return TrustRoots::System;
    }
    for name in ["AWS_CA_BUNDLE", "SSL_CERT_FILE"] {
        if let Some(path) = non_empty_env(name) {
            return TrustRoots::Bundle(PathBuf::from(path));
        }
    }
    if let Some(local) = local {
        if let Some(path) = local.ca_bundle.as_ref().filter(|path| !path.is_empty()) {
            return TrustRoots::Bundle(PathBuf::from(path));
        }
        if local.use_system_certs.unwrap_or(false) {
            return TrustRoots::System;
        }
    }
    TrustRoots::Builtin
}

/// Build the TLS configuration for `roots`, or `None` to keep the default.
///
/// The bundle is read and parsed here rather than at the first request, so a
/// path that does not exist is reported as itself instead of as a connection
/// that mysteriously failed.
pub fn configure(roots: &TrustRoots) -> Result<Option<TlsConfig>> {
    match roots {
        TrustRoots::Builtin => Ok(None),
        TrustRoots::System => Ok(Some(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )),
        TrustRoots::Bundle(path) => Ok(Some(
            TlsConfig::builder()
                .root_certs(RootCerts::new_with_certs(&read_bundle(path)?))
                .build(),
        )),
    }
}

/// Every certificate in a PEM bundle.
///
/// A bundle holds a chain, or an entire root set, so all of them are taken and
/// not merely the first. Private keys in the same file are ignored: a trust
/// store is made of certificates, and a key that lands in one by accident is
/// not something to fail on.
fn read_bundle(path: &Path) -> Result<Vec<Certificate<'static>>> {
    let pem = std::fs::read(path).map_err(|error| {
        Error::Tls(format!(
            "cannot read the CA bundle at {}: {error}",
            path.display()
        ))
    })?;
    let mut certificates = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        match item {
            Ok(PemItem::Certificate(certificate)) => certificates.push(certificate),
            Ok(_) => continue,
            Err(error) => {
                return Err(Error::Tls(format!(
                    "the CA bundle at {} is not valid PEM: {error}",
                    path.display()
                )))
            }
        }
    }
    if certificates.is_empty() {
        return Err(Error::Tls(format!(
            "the CA bundle at {} contains no certificates; \
             it must be PEM, not DER — convert one with \
             `openssl x509 -inform der -in ca.crt -out ca.pem`",
            path.display()
        )));
    }
    Ok(certificates)
}

/// Whether a transport failure looks like a rejected certificate, and if so
/// what to tell the user.
///
/// A certificate error is the one network failure whose fix is neither obvious
/// nor guessable, so it is the one that gets a sentence pointing at the
/// setting that fixes it.
pub(crate) fn failure_hint(message: &str, roots: &TrustRoots) -> Option<String> {
    const MARKERS: [&str; 6] = [
        "certificate",
        "CaUsedAsEndEntity",
        "UnknownIssuer",
        "BadSignature",
        "NotValidForName",
        "invalid peer",
    ];
    let lowered = message.to_lowercase();
    if !MARKERS
        .iter()
        .any(|marker| lowered.contains(&marker.to_lowercase()))
    {
        return None;
    }
    Some(format!(
        "the server's certificate was not signed by a CA this run trusts ({}). \
         If this network inspects TLS through a proxy, set AVC_SYSTEM_CERTS=1 to \
         use the machine's own trust store, or point AVC_CA_BUNDLE at your \
         organization's PEM bundle. See docs/configuration.md",
        roots.describe()
    ))
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// An environment variable read as a switch, spelled however the caller spells
/// switches: `1`, `true`, and `yes` are on; `0`, `false`, `no`, and empty are
/// off.
fn env_flag(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real self-signed certificate, generated once with `openssl req -x509
    /// -newkey ed25519`. Nothing in these tests connects to anything; what
    /// matters is that a genuine PEM parses as one.
    const PEM: &str = "\
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

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(label: &str, contents: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "avc-tls-{label}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_bundle_yields_every_certificate_in_it() {
        let single = TempFile::new("single", PEM);
        assert_eq!(read_bundle(&single.0).unwrap().len(), 1);

        // A bundle is normally a whole root set, not one certificate.
        let many = TempFile::new("many", &PEM.repeat(3));
        assert_eq!(read_bundle(&many.0).unwrap().len(), 3);
        assert!(configure(&TrustRoots::Bundle(many.0.clone()))
            .unwrap()
            .is_some());
    }

    /// Every one of these is a real thing someone does, and each has to say
    /// what went wrong rather than surface later as a failed connection.
    #[test]
    fn an_unusable_bundle_is_reported_before_anything_connects() {
        let missing = std::env::temp_dir().join("avc-tls-does-not-exist.pem");
        let error = configure(&TrustRoots::Bundle(missing)).unwrap_err();
        assert!(error.to_string().contains("cannot read"), "{error}");
        assert!(error.is_provider_failure());

        // A DER file named as if it were PEM: parses to nothing.
        let der = TempFile::new("der", "\u{0}\u{1}not pem at all\u{2}");
        let error = configure(&TrustRoots::Bundle(der.0.clone())).unwrap_err();
        assert!(error.to_string().contains("no certificates"), "{error}");
        assert!(error.to_string().contains("openssl"), "{error}");
    }

    #[test]
    fn the_default_is_the_built_in_roots_and_needs_no_configuration() {
        assert!(configure(&TrustRoots::Builtin).unwrap().is_none());
        assert!(configure(&TrustRoots::System).unwrap().is_some());
    }

    #[test]
    fn a_certificate_failure_names_the_setting_that_fixes_it() {
        let roots = TrustRoots::Builtin;
        let hint = failure_hint("invalid peer certificate: UnknownIssuer", &roots)
            .expect("a certificate rejection should be recognized");
        assert!(hint.contains("AVC_CA_BUNDLE"), "{hint}");
        assert!(hint.contains("AVC_SYSTEM_CERTS"), "{hint}");
        assert!(hint.contains("built-in Mozilla roots"), "{hint}");

        // An ordinary failure is left alone; every error would otherwise end
        // with advice about certificates.
        assert!(failure_hint("connection refused", &roots).is_none());
        assert!(failure_hint("HTTP 403 AccessDenied", &roots).is_none());
    }
}
