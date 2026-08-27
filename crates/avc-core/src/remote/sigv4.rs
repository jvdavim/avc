//! AWS Signature Version 4 for S3.
//!
//! Implemented directly rather than pulled in, because the whole signature is
//! four hashes over strings we already have. Content addressing makes the
//! expensive part free: `x-amz-content-sha256` for an upload is the object's
//! own hash, so a push never reads the payload twice.
//!
//! Reference: <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const SERVICE: &str = "s3";
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// A request being prepared for signing.
pub struct CanonicalRequest<'a> {
    pub method: &'a str,
    /// Absolute path, already split into unencoded segments.
    pub path: &'a str,
    /// Query parameters in any order; sorted during canonicalization.
    pub query: &'a [(String, String)],
    /// Lowercase header names with their values. `host` must be present.
    pub headers: Vec<(String, String)>,
    /// Hex SHA-256 of the request payload.
    pub payload_hash: &'a str,
}

/// Everything needed to sign, resolved from configuration and the environment.
pub struct SigningContext<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    /// `YYYYMMDDTHHMMSSZ`, UTC.
    pub timestamp: &'a str,
}

impl<'a> CanonicalRequest<'a> {
    /// Produce the `Authorization` header value for this request.
    ///
    /// `headers` must already contain `x-amz-date`, `x-amz-content-sha256`, and
    /// `x-amz-security-token` when a session token is in play; every header
    /// present here is signed, so adding one afterwards invalidates the result.
    pub fn authorization(mut self, context: &SigningContext<'_>) -> String {
        self.headers.sort_by(|left, right| left.0.cmp(&right.0));

        let signed_headers = self
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let canonical_headers = self
            .headers
            .iter()
            .map(|(name, value)| format!("{name}:{}\n", value.trim()))
            .collect::<String>();

        let mut query: Vec<String> = self
            .query
            .iter()
            .map(|(key, value)| format!("{}={}", encode_strict(key), encode_strict(value)))
            .collect();
        query.sort();

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.method,
            encode_path(self.path),
            query.join("&"),
            canonical_headers,
            signed_headers,
            self.payload_hash,
        );

        let date = &context.timestamp[..8];
        let scope = format!("{date}/{}/{SERVICE}/aws4_request", context.region);
        let string_to_sign = format!(
            "{ALGORITHM}\n{}\n{scope}\n{}",
            context.timestamp,
            hex(Sha256::digest(canonical_request.as_bytes()).as_slice()),
        );

        let signature = hex(&sign(
            &signing_key(context.secret_access_key, date, context.region),
            string_to_sign.as_bytes(),
        ));

        format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            context.access_key_id,
        )
    }
}

fn signing_key(secret: &str, date: &str, region: &str) -> Vec<u8> {
    let initial = sign(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let regional = sign(&initial, region.as_bytes());
    let service = sign(&regional, SERVICE.as_bytes());
    sign(&service, b"aws4_request")
}

fn sign(key: &[u8], message: &[u8]) -> Vec<u8> {
    // `new_from_slice` only rejects keys for fixed-key MACs; HMAC accepts any length.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// Percent-encode a path, preserving `/` as a segment separator.
///
/// S3 signs the path encoded exactly once; `/` must survive as a delimiter or
/// the canonical URI stops matching what the server reconstructs.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_strict)
        .collect::<Vec<_>>()
        .join("/")
}

/// RFC 3986 unreserved-only encoding, as SigV4 requires.
pub fn encode_strict(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(*byte as char)
            }
            other => output.push_str(&format!("%{other:02X}")),
        }
    }
    output
}

/// Format a Unix timestamp as `YYYYMMDDTHHMMSSZ`.
///
/// Hand-rolled to avoid a date dependency for the one format S3 accepts.
pub fn format_amz_date(unix_seconds: u64) -> String {
    let days = unix_seconds / 86_400;
    let seconds = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60,
    )
}

/// Howard Hinnant's `civil_from_days`, shifted to a March-based year so leap
/// days land at the end of the cycle and need no special case.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const TIMESTAMP: &str = "20130524T000000Z";
    const HASH: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    /// Signatures below were produced by botocore's `SigV4Auth` with the clock
    /// pinned to `TIMESTAMP`. They are an independent implementation's answer,
    /// not this module's own output recorded back — if the canonical request,
    /// the scope, or the key derivation drifts, these stop matching.
    fn signature(
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        payload_hash: &str,
        host: &str,
        session_token: Option<&str>,
    ) -> String {
        let mut headers = vec![
            ("host".to_string(), host.to_string()),
            ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
            ("x-amz-date".to_string(), TIMESTAMP.to_string()),
        ];
        if let Some(token) = session_token {
            headers.push(("x-amz-security-token".to_string(), token.to_string()));
        }
        headers.extend(
            extra
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string())),
        );
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        CanonicalRequest {
            method,
            path,
            query: &query,
            headers,
            payload_hash,
        }
        .authorization(&SigningContext {
            access_key_id: KEY,
            secret_access_key: SECRET,
            region: "us-east-1",
            timestamp: TIMESTAMP,
        })
    }

    fn expected(signed_headers: &str, signature: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={KEY}/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders={signed_headers}, Signature={signature}"
        )
    }

    #[test]
    fn signs_a_virtual_host_style_get() {
        assert_eq!(
            signature(
                "GET",
                "/",
                &[],
                &[],
                EMPTY_PAYLOAD_SHA256,
                "examplebucket.s3.amazonaws.com",
                None,
            ),
            expected(
                "host;x-amz-content-sha256;x-amz-date",
                "fc17940bd195def017f1e7139d4d9b4005f13a9170c574057f4e3a05d4021e45",
            )
        );
    }

    /// Path-style against a host with a port — the MinIO shape. The port must
    /// be part of the signed `host` header or the signature will not verify.
    #[test]
    fn signs_a_path_style_get_against_a_host_with_a_port() {
        assert_eq!(
            signature(
                "GET",
                &format!("/my-bucket/artifacts/objects/sha256/01/{}", "01".repeat(32)),
                &[],
                &[],
                EMPTY_PAYLOAD_SHA256,
                "localhost:9000",
                None,
            ),
            expected(
                "host;x-amz-content-sha256;x-amz-date",
                "b8d3ce718b22822c8b0b1965e98c2678e6be495cac2615f89802d541b5c17482",
            )
        );
    }

    /// An upload signs a real payload hash plus an extra header, and the
    /// signed-header list must stay sorted with `content-type` first.
    #[test]
    fn signs_a_put_with_a_payload_hash_and_extra_header() {
        assert_eq!(
            signature(
                "PUT",
                &format!("/my-bucket/objects/sha256/ab/{}", "ab".repeat(32)),
                &[],
                &[("content-type", "application/octet-stream")],
                HASH,
                "localhost:9000",
                None,
            ),
            expected(
                "content-type;host;x-amz-content-sha256;x-amz-date",
                "6381aca2ba9dc3b29cf2629e522854a9a5c7af4927a9281d29a8711f25ae42a1",
            )
        );
    }

    /// Query parameters are canonicalized sorted and individually encoded;
    /// the `/` inside `prefix` must become `%2F`.
    #[test]
    fn signs_a_list_request_with_query_parameters() {
        assert_eq!(
            signature(
                "GET",
                "/my-bucket",
                &[
                    ("prefix", "artifacts/objects/sha256/"),
                    ("list-type", "2"),
                    ("max-keys", "1000"),
                ],
                &[],
                EMPTY_PAYLOAD_SHA256,
                "localhost:9000",
                None,
            ),
            expected(
                "host;x-amz-content-sha256;x-amz-date",
                "a17e9b254ba09cafcb72c1c3e5ba860185c6639b1751c5c7ccba38a66122f5f3",
            )
        );
    }

    #[test]
    fn signs_temporary_credentials_including_the_session_token() {
        assert_eq!(
            signature(
                "GET",
                "/my-bucket/key",
                &[],
                &[],
                EMPTY_PAYLOAD_SHA256,
                "localhost:9000",
                Some("SESSIONTOKEN123"),
            ),
            expected(
                "host;x-amz-content-sha256;x-amz-date;x-amz-security-token",
                "24b8c8de66b6c50e4639a285f5625dd5dbd88adcac35aacf75b52dc6c8716f0f",
            )
        );
    }

    #[test]
    fn encodes_paths_and_queries_the_way_sigv4_expects() {
        assert_eq!(encode_path("/a b/c~d"), "/a%20b/c~d");
        assert_eq!(encode_strict("a/b"), "a%2Fb");
        assert_eq!(encode_strict("-_.~"), "-_.~");
    }

    #[test]
    fn formats_dates_across_leap_years_and_epochs() {
        assert_eq!(format_amz_date(0), "19700101T000000Z");
        assert_eq!(format_amz_date(1_369_353_600), "20130524T000000Z");
        // 2024-02-29, the case a naive date routine gets wrong.
        assert_eq!(format_amz_date(1_709_208_296), "20240229T120456Z");
    }
}
