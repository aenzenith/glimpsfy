//! AWS Signature Version 4 for S3-compatible buckets, in about two hundred lines.
//!
//! **Why not the AWS SDK.** Glimpsfy signs three requests (PUT, DELETE and a test PUT) against
//! whichever S3-compatible service the user points it at. The SDK (`aws-sdk-s3` with
//! `aws-config` and `aws-sigv4`) would bring its own async runtime, HTTP stack and credential
//! chain (environment, profile files, instance metadata) that a desktop app must not consult on
//! its own. The algorithm itself is small, it is specified to the byte, and the published test
//! suite (`tests/fixtures/sigv4_suite.json`, checked in `tests/sigv4.rs`) pins every step.
//!
//! The signer follows the suite's two modes: `normalize` collapses `.` and `..` segments and
//! repeated slashes as most AWS services do; S3 signs the path as it is, so the S3 provider signs
//! with `normalize: false`. Paths are percent-encoded once in both modes, as S3 expects.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// The SHA-256 of an empty body, which a request without a body signs.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// What a signature needs besides the request.
#[derive(Clone)]
pub struct SigningParams<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    pub time: DateTime<Utc>,
    /// Collapse `.`, `..` and repeated slashes in the path before signing (not for S3).
    pub normalize_path: bool,
}

/// The request as the signer reads it.
pub struct SignableRequest<'a> {
    pub method: &'a str,
    /// The path as it is sent, not yet percent-encoded (`/my bucket/a b.png`).
    pub path: &'a str,
    /// The query parameters, not yet percent-encoded.
    pub query: &'a [(String, String)],
    /// Every header to sign, `host` included; names in any case.
    pub headers: &'a [(String, String)],
    /// The hex SHA-256 of the body, or `UNSIGNED-PAYLOAD`.
    pub payload_hash: &'a str,
}

/// The steps of one signature, kept so the tests can compare each of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signature: String,
    /// The value of the `Authorization` header.
    pub authorization: String,
    /// `20150830T123600Z`, the value of `X-Amz-Date`.
    pub amz_date: String,
}

/// Signs a request. The caller sends `X-Amz-Date` (and, for S3, `x-amz-content-sha256`) with
/// the values it signed; both must be in `request.headers`.
pub fn sign(request: &SignableRequest, params: &SigningParams) -> Signature {
    let amz_date = params.time.format("%Y%m%dT%H%M%SZ").to_string();
    let date = params.time.format("%Y%m%d").to_string();

    let path = if params.normalize_path { normalize(request.path) } else { request.path.to_string() };
    let canonical_uri = if path.is_empty() { "/".to_string() } else { encode_path(&path) };

    let mut query: Vec<(String, String)> =
        request.query.iter().map(|(k, v)| (encode(k, true), encode(v, true))).collect();
    query.sort();
    let canonical_query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

    // Header names in lower case, sorted; repeated names joined with commas in their order; the
    // value trimmed with inner runs of whitespace (folded lines included) collapsed to one space.
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in request.headers {
        let name = name.trim().to_ascii_lowercase();
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        match headers.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => {
                existing.push(',');
                existing.push_str(&value);
            }
            None => headers.push((name, value)),
        }
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method, canonical_uri, canonical_query, canonical_headers, signed_headers, request.payload_hash
    );

    let scope = format!("{date}/{}/{}/aws4_request", params.region, params.service);
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex(&Sha256::digest(&canonical_request)));

    let k_date = hmac(format!("AWS4{}", params.secret_access_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, params.region.as_bytes());
    let k_service = hmac(&k_region, params.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        params.access_key_id
    );
    Signature { canonical_request, string_to_sign, signature, authorization, amz_date }
}

/// The hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A running SHA-256, for a body read in pieces.
#[derive(Default)]
pub struct Sha256Stream(Sha256);

impl Sha256Stream {
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish_hex(self) -> String {
        hex(&self.0.finalize())
    }
}

/// HMAC-SHA256 as RFC 2104 defines it, written out so that no call can fail.
fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner_pad: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let outer_pad: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&inner_pad).chain_update(data).finalize();
    Sha256::new().chain_update(&outer_pad).chain_update(inner).finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Percent-encodes everything except the unreserved characters `A–Z a–z 0–9 - . _ ~`, and `/`
/// too unless `slash` is set, as SigV4 asks.
pub fn encode(text: &str, slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            b'/' if !slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Percent-encodes a path segment by segment, keeping its slashes.
pub fn encode_path(path: &str) -> String {
    encode(path, false)
}

/// Removes `.` and `..` segments and empty segments, keeping a trailing slash, as RFC 3986 asks.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let trailing = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    let mut joined = format!("/{}", out.join("/"));
    if trailing && joined != "/" {
        joined.push('/');
    }
    joined
}
