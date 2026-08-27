use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    File,
    S3,
    Gcs,
    Azure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub name: String,
    pub provider: Provider,
    pub bucket_or_container: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub endpoint_url: Option<String>,
}

impl RemoteConfig {
    pub fn from_url(name: impl Into<String>, raw: &str) -> Result<Self> {
        let parsed = Url::parse(raw).map_err(|_| Error::InvalidRemote(raw.into()))?;
        let provider = match parsed.scheme() {
            "file" => Provider::File,
            "s3" | "s3+https" => Provider::S3,
            "gs" => Provider::Gcs,
            "az" => Provider::Azure,
            _ => return Err(Error::InvalidRemote(raw.into())),
        };
        if parsed.scheme() == "file" {
            let path = parsed
                .to_file_path()
                .map_err(|_| Error::InvalidRemote(raw.into()))?;
            return Ok(Self {
                name: name.into(),
                provider,
                bucket_or_container: path.to_string_lossy().into_owned(),
                prefix: String::new(),
                endpoint_url: None,
            });
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| Error::InvalidRemote(raw.into()))?;
        let path = parsed.path().trim_matches('/');
        let (bucket_or_container, prefix, endpoint_url) = if parsed.scheme() == "s3+https" {
            let mut parts = path.splitn(2, '/');
            let bucket = parts
                .next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::InvalidRemote(raw.into()))?;
            (
                bucket.to_string(),
                parts.next().unwrap_or_default().to_string(),
                Some(format!("https://{host}")),
            )
        } else {
            if path.is_empty() && parsed.scheme() != "s3" {
                return Err(Error::InvalidRemote(raw.into()));
            }
            (host.to_string(), path.to_string(), None)
        };
        Ok(Self {
            name: name.into(),
            provider,
            bucket_or_container,
            prefix,
            endpoint_url,
        })
    }
}
