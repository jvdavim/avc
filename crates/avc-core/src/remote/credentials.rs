//! Credential and endpoint resolution for S3-compatible remotes.
//!
//! Precedence, highest first:
//!
//! 1. Provider-standard environment variables.
//! 2. `.avc/config.local.toml`, supplied by the caller as [`LocalRemoteOverride`].
//! 3. The tracked `.avc/config.toml`, supplied as [`RemoteConfig`].
//! 4. `~/.aws/credentials` and `~/.aws/config`, for the active profile.
//!
//! Provider chains come first so AVC does not become another place a secret
//! leaks from: a repository-local file can be overridden, never the reverse.
//! The tracked configuration sits below the machine-local one for the same
//! reason a shared default should never beat a deliberate local choice — and
//! it holds names only, never a secret.

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, RemoteConfig, Result};

/// Static credentials for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Present for temporary credentials; signed as `x-amz-security-token`.
    pub session_token: Option<String>,
}

/// Machine-local settings for a single named remote, read from the ignored
/// `.avc/config.local.toml`. Never committed, never merged into the tracked
/// configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LocalRemoteOverride {
    pub name: String,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    /// Force `endpoint/bucket/key` addressing. Defaults to true whenever a
    /// custom endpoint is set, which is what every S3-compatible server wants.
    #[serde(default)]
    pub force_path_style: Option<bool>,
}

/// Resolve credentials for `remote`, consulting the chain above.
pub fn resolve(remote: &RemoteConfig, local: Option<&LocalRemoteOverride>) -> Result<Credentials> {
    if let (Ok(access_key_id), Ok(secret_access_key)) = (
        env::var("AWS_ACCESS_KEY_ID"),
        env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        if !access_key_id.is_empty() && !secret_access_key.is_empty() {
            return Ok(Credentials {
                access_key_id,
                secret_access_key,
                session_token: non_empty_env("AWS_SESSION_TOKEN"),
            });
        }
    }

    if let Some(local) = local {
        if let (Some(access_key_id), Some(secret_access_key)) =
            (local.access_key_id.clone(), local.secret_access_key.clone())
        {
            return Ok(Credentials {
                access_key_id,
                secret_access_key,
                session_token: local.session_token.clone(),
            });
        }
    }

    let profile = resolve_profile(remote, local);

    if let Some(credentials) = from_shared_file(&profile)? {
        return Ok(credentials);
    }

    Err(Error::MissingCredentials(profile))
}

/// Resolve the profile whose section of the shared AWS files to read.
///
/// A profile is a name, not a secret, so the tracked configuration may carry
/// one: a team that keeps its artifacts behind a named profile can say so once
/// in `.avc/config.toml` instead of every member exporting `AWS_PROFILE`.
pub fn resolve_profile(remote: &RemoteConfig, local: Option<&LocalRemoteOverride>) -> String {
    local
        .and_then(|local| local.profile.clone())
        .filter(|profile| !profile.is_empty())
        .or_else(|| non_empty_env("AWS_PROFILE"))
        .or_else(|| remote.profile.clone().filter(|profile| !profile.is_empty()))
        .unwrap_or_else(|| "default".into())
}

/// Resolve the region, which SigV4 needs even when the server ignores it.
pub fn resolve_region(remote: &RemoteConfig, local: Option<&LocalRemoteOverride>) -> String {
    non_empty_env("AWS_REGION")
        .or_else(|| non_empty_env("AWS_DEFAULT_REGION"))
        .or_else(|| local.and_then(|local| local.region.clone()))
        .or_else(|| remote.region.clone())
        .filter(|region| !region.is_empty())
        .or_else(|| {
            let profile = resolve_profile(remote, local);
            shared_config_file("config")
                .and_then(|path| std::fs::read_to_string(path).ok())
                // `~/.aws/config` names the default profile plainly and every
                // other one as `profile <name>`.
                .and_then(|text| {
                    let section = if profile == "default" {
                        "default".to_string()
                    } else {
                        format!("profile {profile}")
                    };
                    profile_field(&text, &section, "region")
                })
        })
        // S3-compatible servers generally ignore the region but still require
        // the signature to commit to one; us-east-1 is the universal default.
        .unwrap_or_else(|| "us-east-1".into())
}

/// Resolve the endpoint override, if any.
pub fn resolve_endpoint(local: Option<&LocalRemoteOverride>) -> Option<String> {
    non_empty_env("AWS_ENDPOINT_URL_S3")
        .or_else(|| non_empty_env("AWS_ENDPOINT_URL"))
        .or_else(|| local.and_then(|local| local.endpoint_url.clone()))
}

fn from_shared_file(profile: &str) -> Result<Option<Credentials>> {
    let Some(path) = shared_config_file("credentials") else {
        return Ok(None);
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let Some(access_key_id) = profile_field(&text, profile, "aws_access_key_id") else {
        return Ok(None);
    };
    let Some(secret_access_key) = profile_field(&text, profile, "aws_secret_access_key") else {
        return Err(Error::MissingCredentials(format!(
            "{profile} (aws_access_key_id without aws_secret_access_key)"
        )));
    };
    Ok(Some(Credentials {
        access_key_id,
        secret_access_key,
        session_token: profile_field(&text, profile, "aws_session_token"),
    }))
}

fn shared_config_file(name: &str) -> Option<PathBuf> {
    let explicit = if name == "credentials" {
        non_empty_env("AWS_SHARED_CREDENTIALS_FILE")
    } else {
        non_empty_env("AWS_CONFIG_FILE")
    };
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }
    let home = non_empty_env("HOME").or_else(|| non_empty_env("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".aws").join(name))
}

/// Read `key` from `[section]` of an AWS-style INI file.
///
/// Deliberately minimal: no nested sub-sections, no continuation lines, no
/// interpolation. Anything more belongs in an environment variable.
fn profile_field(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = header.trim() == section;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "\
# a comment
[default]
aws_access_key_id = AKIADEFAULT
aws_secret_access_key = defaultsecret

[profile ci]
region = eu-west-1

[ci]
aws_access_key_id=AKIACI
aws_secret_access_key=cisecret
aws_session_token = tok
";

    #[test]
    fn reads_the_requested_profile_only() {
        assert_eq!(
            profile_field(INI, "default", "aws_access_key_id").as_deref(),
            Some("AKIADEFAULT")
        );
        assert_eq!(
            profile_field(INI, "ci", "aws_session_token").as_deref(),
            Some("tok")
        );
        // `[profile ci]` in config and `[ci]` in credentials are distinct sections.
        assert_eq!(
            profile_field(INI, "profile ci", "region").as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(profile_field(INI, "ci", "region"), None);
        assert_eq!(profile_field(INI, "absent", "aws_access_key_id"), None);
    }

    fn remote(region: Option<&str>, profile: Option<&str>) -> RemoteConfig {
        RemoteConfig {
            name: "origin".into(),
            provider: crate::Provider::S3,
            bucket_or_container: "my-bucket".into(),
            prefix: String::new(),
            endpoint_url: None,
            region: region.map(str::to_owned),
            profile: profile.map(str::to_owned),
        }
    }

    /// Restores every variable it touches, so the rest of the suite sees the
    /// environment it started with.
    struct EnvGuard(Vec<(&'static str, Option<String>)>);

    impl EnvGuard {
        fn clearing(names: &[&'static str]) -> Self {
            let saved = names
                .iter()
                .map(|name| (*name, env::var(name).ok()))
                .collect();
            for name in names {
                env::remove_var(name);
            }
            Self(saved)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }

    /// One test rather than several: these all move the same process-wide
    /// environment, and splitting them would let them race each other.
    #[test]
    fn region_and_profile_resolve_in_the_documented_order() {
        let _guard = EnvGuard::clearing(&[
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_PROFILE",
            "AWS_CONFIG_FILE",
        ]);
        let shared = std::env::temp_dir().join(format!("avc-aws-config-{}", std::process::id()));
        std::fs::write(&shared, "[default]\nregion = ap-south-1\n").unwrap();
        env::set_var("AWS_CONFIG_FILE", &shared);

        let local = |region: Option<&str>, profile: Option<&str>| LocalRemoteOverride {
            name: "origin".into(),
            region: region.map(str::to_owned),
            profile: profile.map(str::to_owned),
            ..Default::default()
        };

        // The repository may name both, which is the point of putting them in
        // the tracked configuration: a clone needs no local setup to reach the
        // right bucket in the right region.
        assert_eq!(
            resolve_region(&remote(Some("sa-east-1"), None), None),
            "sa-east-1"
        );
        assert_eq!(
            resolve_profile(&remote(None, Some("artifacts")), None),
            "artifacts"
        );

        // A tracked region beats `~/.aws/config`; with none, that file answers.
        assert_eq!(resolve_region(&remote(None, None), None), "ap-south-1");

        // `config.local.toml` overrides the tracked value on this machine.
        let overridden = local(Some("eu-west-1"), Some("minio-dev"));
        assert_eq!(
            resolve_region(&remote(Some("sa-east-1"), None), Some(&overridden)),
            "eu-west-1"
        );
        assert_eq!(
            resolve_profile(&remote(None, Some("artifacts")), Some(&overridden)),
            "minio-dev"
        );

        // And the environment overrides everything below it.
        env::set_var("AWS_REGION", "us-west-2");
        env::set_var("AWS_PROFILE", "from-env");
        assert_eq!(
            resolve_region(
                &remote(Some("sa-east-1"), None),
                Some(&local(Some("eu-west-1"), None))
            ),
            "us-west-2"
        );
        assert_eq!(
            resolve_profile(&remote(None, Some("artifacts")), None),
            "from-env"
        );
        // A machine-local profile is the one exception, matching the behaviour
        // that shipped before the tracked field existed: someone who wrote a
        // profile into `config.local.toml` meant that repository, not whatever
        // `AWS_PROFILE` happens to hold in this shell.
        assert_eq!(
            resolve_profile(
                &remote(None, Some("artifacts")),
                Some(&local(None, Some("minio-dev")))
            ),
            "minio-dev"
        );

        // Nothing configured anywhere still signs, because SigV4 requires a
        // region even where the server ignores it.
        env::remove_var("AWS_REGION");
        env::remove_var("AWS_CONFIG_FILE");
        assert_eq!(resolve_region(&remote(None, None), None), "us-east-1");
        assert_eq!(resolve_profile(&remote(None, None), None), "from-env");

        let _ = std::fs::remove_file(&shared);
    }
}
