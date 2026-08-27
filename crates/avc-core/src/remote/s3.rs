//! S3 and S3-compatible transport.
//!
//! Speaks plain REST against any server implementing the S3 API — Amazon S3,
//! MinIO, Cloudflare R2, Ceph, Backblaze B2 — using SigV4 header
//! authentication. There is no service-specific behaviour anywhere below:
//! addressing style and endpoint come from configuration, never from sniffing
//! a hostname.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

use super::credentials::{self, Credentials, LocalRemoteOverride};
use super::sigv4::{format_amz_date, CanonicalRequest, SigningContext, EMPTY_PAYLOAD_SHA256};
use super::{object_from_key, object_key, xml, ObjectStore, RemoteObject};
use crate::{Error, ObjectId, RemoteConfig, Result};

/// One page of `ListObjectsV2`. The protocol maximum; fewer round trips for a
/// large bucket, and irrelevant for a small one.
const LIST_PAGE_SIZE: &str = "1000";

/// Everything the transport needs, with every override already applied.
pub struct S3Settings {
    pub bucket: String,
    pub prefix: String,
    pub region: String,
    /// Scheme and authority only, no trailing slash.
    pub endpoint: String,
    pub force_path_style: bool,
    pub credentials: Credentials,
}

impl S3Settings {
    /// Merge tracked configuration with machine-local overrides and the
    /// environment.
    ///
    /// Precedence, highest first: environment variables,
    /// `.avc/config.local.toml`, then `~/.aws/credentials`.
    pub fn resolve(remote: &RemoteConfig, local: Option<&LocalRemoteOverride>) -> Result<Self> {
        let region = credentials::resolve_region(local);
        let override_endpoint = credentials::resolve_endpoint(local);
        // A custom endpoint is an S3-compatible server until told otherwise,
        // and those overwhelmingly serve path-style addressing only.
        let custom_endpoint = override_endpoint.is_some() || remote.endpoint_url.is_some();
        let endpoint = override_endpoint
            .or_else(|| remote.endpoint_url.clone())
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let endpoint = endpoint.trim_end_matches('/').to_owned();
        let force_path_style = local
            .and_then(|local| local.force_path_style)
            .unwrap_or(custom_endpoint);
        Ok(Self {
            bucket: remote.bucket_or_container.clone(),
            prefix: remote.prefix.clone(),
            region,
            endpoint,
            force_path_style,
            credentials: credentials::resolve(local)?,
        })
    }
}

pub struct S3Store {
    settings: S3Settings,
    /// Scheme and host of every request, after addressing style is applied.
    base: Url,
    agent: ureq::Agent,
}

impl S3Store {
    pub fn new(settings: S3Settings) -> Result<Self> {
        let endpoint = Url::parse(&settings.endpoint)
            .map_err(|_| Error::InvalidRemote(settings.endpoint.clone()))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(Error::InvalidRemote(settings.endpoint.clone()));
        }
        let base = if settings.force_path_style {
            endpoint
        } else {
            // Virtual-hosted-style: the bucket becomes a host label. Path-style
            // is deprecated on Amazon S3, so this is the default there.
            let host = endpoint
                .host_str()
                .ok_or_else(|| Error::InvalidRemote(settings.endpoint.clone()))?;
            let mut virtual_host = endpoint.clone();
            virtual_host
                .set_host(Some(&format!("{}.{host}", settings.bucket)))
                .map_err(|_| Error::InvalidRemote(settings.bucket.clone()))?;
            virtual_host
        };
        Ok(Self {
            settings,
            base,
            // Let 4xx and 5xx arrive as responses, not errors: S3 explains
            // itself in the XML body, and a 404 is an ordinary answer to
            // `exists`.
            agent: ureq::Agent::new_with_config(
                ureq::config::Config::builder()
                    .http_status_as_error(false)
                    .build(),
            ),
        })
    }

    /// Absolute request path for `key`, including the bucket when addressing is
    /// path-style. Unencoded — the signer and the URL builder each encode once.
    fn request_path(&self, key: &str) -> String {
        if self.settings.force_path_style {
            format!("/{}/{key}", self.settings.bucket)
        } else {
            format!("/{key}")
        }
    }

    fn url(&self, path: &str, query: &[(String, String)]) -> String {
        let mut url = format!(
            "{}://{}{}{}",
            self.base.scheme(),
            self.base.host_str().unwrap_or_default(),
            self.base
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default(),
            super::sigv4::encode_path(path),
        );
        if !query.is_empty() {
            let mut pairs: Vec<String> = query
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        super::sigv4::encode_strict(key),
                        super::sigv4::encode_strict(value)
                    )
                })
                .collect();
            pairs.sort();
            url.push('?');
            url.push_str(&pairs.join("&"));
        }
        url
    }

    /// Host header value, which SigV4 signs and must match what is sent.
    fn host_header(&self) -> String {
        match self.base.port() {
            Some(port) => format!("{}:{port}", self.base.host_str().unwrap_or_default()),
            None => self.base.host_str().unwrap_or_default().to_owned(),
        }
    }

    /// Build the full header set for one request, `authorization` included.
    ///
    /// Every header in `extra` is signed along with the mandatory ones, so a
    /// proxy that rewrites one of them is caught rather than silently trusted.
    fn signed_headers(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        payload_hash: &str,
        extra: &[(String, String)],
    ) -> Vec<(String, String)> {
        let timestamp = format_amz_date(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                // A clock before 1970 yields a signature the server rejects
                // with a clear skew error, which beats panicking here.
                .unwrap_or_default(),
        );
        let mut headers = vec![
            ("host".to_string(), self.host_header()),
            ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
            ("x-amz-date".to_string(), timestamp.clone()),
        ];
        if let Some(token) = &self.settings.credentials.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        headers.extend(extra.iter().cloned());
        let authorization = CanonicalRequest {
            method,
            path,
            query,
            headers: headers.clone(),
            payload_hash,
        }
        .authorization(&SigningContext {
            access_key_id: &self.settings.credentials.access_key_id,
            secret_access_key: &self.settings.credentials.secret_access_key,
            region: &self.settings.region,
            timestamp: &timestamp,
        });
        headers.push(("authorization".to_string(), authorization));
        headers
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        payload_hash: &str,
        extra: &[(String, String)],
        body: Option<(&mut dyn Read, u64)>,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(self.url(path, query));
        for (name, value) in self.signed_headers(method, path, query, payload_hash, extra) {
            builder = builder.header(name, value);
        }
        let response = match body {
            Some((reader, size)) => {
                // An explicit content-length keeps ureq out of chunked
                // encoding, which S3 rejects without an aws-chunked signature.
                let request = builder
                    .header("content-length", size.to_string())
                    .body(ureq::SendBody::from_reader(reader))
                    .map_err(|error| Error::Provider(error.to_string()))?;
                self.agent.run(request)
            }
            None => {
                let request = builder
                    .body(ureq::SendBody::none())
                    .map_err(|error| Error::Provider(error.to_string()))?;
                self.agent.run(request)
            }
        };
        match response {
            Ok(response) => Ok(response),
            Err(ureq::Error::StatusCode(status)) => Err(Error::Provider(format!(
                "{method} {} failed with HTTP {status}",
                self.describe_key(path)
            ))),
            Err(error) => Err(Error::Provider(format!(
                "{method} {} failed: {error}",
                self.describe_key(path)
            ))),
        }
    }

    fn describe_key(&self, path: &str) -> String {
        format!("{}{path}", self.settings.endpoint)
    }
}

/// Turn a non-2xx response into an error carrying S3's own explanation.
///
/// The XML body is far more actionable than the status line: `SignatureDoesNotMatch`
/// and `InvalidAccessKeyId` are both 403.
fn provider_error(context: &str, mut response: ureq::http::Response<ureq::Body>) -> Error {
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let code = xml::element(&body, "Code").map(xml::decode);
    let message = xml::element(&body, "Message").map(xml::decode);
    match (code, message) {
        (Some(code), Some(message)) => {
            Error::Provider(format!("{context}: HTTP {status} {code}: {message}"))
        }
        (Some(code), None) => Error::Provider(format!("{context}: HTTP {status} {code}")),
        _ => Error::Provider(format!("{context}: HTTP {status}")),
    }
}

impl ObjectStore for S3Store {
    fn describe(&self) -> String {
        format!("{}/{}", self.settings.endpoint, self.settings.bucket)
    }

    fn put(&self, object: &ObjectId, size: u64, body: &mut dyn Read) -> Result<()> {
        let key = object_key(&self.settings.prefix, object);
        let path = self.request_path(&key);
        // Content addressing pays off here: the payload hash SigV4 demands is
        // the object's own hash, so the bytes are read exactly once.
        let response = self.request(
            "PUT",
            &path,
            &[],
            object.hash(),
            &[(
                "content-type".to_string(),
                "application/octet-stream".to_string(),
            )],
            Some((body, size)),
        )?;
        if !response.status().is_success() {
            return Err(provider_error(&format!("upload {key}"), response));
        }
        Ok(())
    }

    fn get(&self, object: &ObjectId) -> Result<Box<dyn Read>> {
        let key = object_key(&self.settings.prefix, object);
        let path = self.request_path(&key);
        let response = self.request("GET", &path, &[], EMPTY_PAYLOAD_SHA256, &[], None)?;
        let status = response.status();
        if status == 404 {
            return Err(Error::ObjectNotFound(object.hash().to_owned()));
        }
        if !status.is_success() {
            return Err(provider_error(&format!("download {key}"), response));
        }
        Ok(Box::new(response.into_body().into_reader()))
    }

    fn exists(&self, object: &ObjectId) -> Result<bool> {
        let key = object_key(&self.settings.prefix, object);
        let path = self.request_path(&key);
        let response = self.request("HEAD", &path, &[], EMPTY_PAYLOAD_SHA256, &[], None)?;
        let status = response.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == 404 {
            return Ok(false);
        }
        Err(provider_error(&format!("stat {key}"), response))
    }

    fn list(&self) -> Result<Vec<RemoteObject>> {
        // Every object we write lives under this one directory, so a single
        // prefixed listing sees all of them and nothing else.
        let search_prefix = {
            let prefix = self.settings.prefix.trim_matches('/');
            if prefix.is_empty() {
                format!("objects/{}/", crate::ALGORITHM)
            } else {
                format!("{prefix}/objects/{}/", crate::ALGORITHM)
            }
        };
        let path = self.request_path("");
        let mut found = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type".to_string(), "2".to_string()),
                ("max-keys".to_string(), LIST_PAGE_SIZE.to_string()),
                ("prefix".to_string(), search_prefix.clone()),
            ];
            if let Some(token) = &continuation {
                query.push(("continuation-token".to_string(), token.clone()));
            }
            let mut response =
                self.request("GET", &path, &query, EMPTY_PAYLOAD_SHA256, &[], None)?;
            if !response.status().is_success() {
                return Err(provider_error("list objects", response));
            }
            let body = response
                .body_mut()
                .read_to_string()
                .map_err(|error| Error::Provider(error.to_string()))?;
            for contents in xml::elements(&body, "Contents") {
                let Some(key) = xml::element(contents, "Key").map(xml::decode) else {
                    continue;
                };
                // Keys we did not write share the bucket; skip them silently
                // rather than failing a listing someone else's data broke.
                let Some(object) = object_from_key(&key) else {
                    continue;
                };
                let size = xml::element(contents, "Size")
                    .and_then(|size| size.trim().parse().ok())
                    .unwrap_or(0);
                found.push(RemoteObject { object, size });
            }
            if xml::element(&body, "IsTruncated").map(str::trim) != Some("true") {
                break;
            }
            let Some(token) = xml::element(&body, "NextContinuationToken").map(xml::decode) else {
                break;
            };
            continuation = Some(token);
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;

    fn settings(endpoint: &str, path_style: bool) -> S3Settings {
        S3Settings {
            bucket: "my-bucket".into(),
            prefix: "artifacts".into(),
            region: "us-east-1".into(),
            endpoint: endpoint.into(),
            force_path_style: path_style,
            credentials: Credentials {
                access_key_id: "key".into(),
                secret_access_key: "secret".into(),
                session_token: None,
            },
        }
    }

    fn object() -> ObjectId {
        ObjectId::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").unwrap()
    }

    #[test]
    fn path_style_addressing_keeps_the_bucket_in_the_path() {
        let store = S3Store::new(settings("http://localhost:9000", true)).unwrap();
        let key = object_key(&store.settings.prefix, &object());
        assert_eq!(
            store.url(&store.request_path(&key), &[]),
            "http://localhost:9000/my-bucket/artifacts/objects/sha256/01/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(store.host_header(), "localhost:9000");
    }

    #[test]
    fn virtual_host_addressing_moves_the_bucket_into_the_host() {
        let store = S3Store::new(settings("https://s3.us-east-1.amazonaws.com", false)).unwrap();
        let key = object_key(&store.settings.prefix, &object());
        assert_eq!(
            store.url(&store.request_path(&key), &[]),
            "https://my-bucket.s3.us-east-1.amazonaws.com/artifacts/objects/sha256/01/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(store.host_header(), "my-bucket.s3.us-east-1.amazonaws.com");
    }

    #[test]
    fn query_parameters_are_sorted_and_encoded() {
        let store = S3Store::new(settings("http://localhost:9000", true)).unwrap();
        let url = store.url(
            "/my-bucket",
            &[
                ("prefix".into(), "artifacts/objects/".into()),
                ("list-type".into(), "2".into()),
            ],
        );
        assert_eq!(
            url,
            "http://localhost:9000/my-bucket?list-type=2&prefix=artifacts%2Fobjects%2F"
        );
    }

    #[test]
    fn a_custom_endpoint_defaults_to_path_style() {
        let remote =
            RemoteConfig::from_url("minio", "s3+https://storage.example/my-bucket/artifacts")
                .unwrap();
        assert_eq!(remote.provider, Provider::S3);
        let local = LocalRemoteOverride {
            name: "minio".into(),
            access_key_id: Some("key".into()),
            secret_access_key: Some("secret".into()),
            ..Default::default()
        };
        let resolved = S3Settings::resolve(&remote, Some(&local)).unwrap();
        assert!(resolved.force_path_style);
        assert_eq!(resolved.endpoint, "https://storage.example");
    }

    #[test]
    fn rejects_a_non_http_endpoint() {
        assert!(S3Store::new(settings("ftp://storage.example", true)).is_err());
    }
}
