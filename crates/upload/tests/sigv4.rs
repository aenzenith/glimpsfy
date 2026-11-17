//! The SigV4 signer against the AWS Signature Version 4 test suite, copied verbatim into
//! `fixtures/sigv4_suite.json` from awslabs/aws-c-auth (`tests/aws-signing-test-suite/v4`).
//! A wrong signature would send a user's keys to a bucket that refuses every upload, or, worse,
//! one that signs a request other than the one sent.

use chrono::{DateTime, Utc};
use glimpsfy_upload::sigv4::{self, SignableRequest, SigningParams};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suite {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    normalize: bool,
    sign_body: bool,
    region: String,
    service: String,
    timestamp: String,
    access_key_id: String,
    secret_access_key: String,
    request: String,
    canonical_request: String,
    string_to_sign: String,
    signature: String,
}

type Pairs = Vec<(String, String)>;

/// Splits a `request.txt` of the suite into method, path, query, headers and body. Folded header
/// lines (starting with whitespace) continue the previous value.
fn parse(request: &str) -> (String, String, Pairs, Pairs, Vec<u8>) {
    let (head, body) = request.split_once("\n\n").unwrap_or((request.trim_end_matches('\n'), ""));
    let mut lines = head.lines();
    let first = lines.next().expect("a request line");
    let method = first[..first.find(' ').expect("a method")].to_string();
    let target = first[method.len() + 1..first.rfind(' ').expect("a protocol")].to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    let mut headers: Pairs = Vec::new();
    for line in lines {
        if line.starts_with([' ', '\t']) {
            let last = headers.last_mut().expect("a header before its continuation");
            last.1.push(' ');
            last.1.push_str(line.trim());
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.to_string(), value.to_string()));
        }
    }
    (method, path.to_string(), query, headers, body.as_bytes().to_vec())
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).expect("UTF-8")
}

#[test]
fn signatures_match_the_published_aws_test_suite() {
    let suite: Suite = serde_json::from_str(include_str!("fixtures/sigv4_suite.json")).expect("the suite parses");
    assert!(suite.cases.len() >= 30, "the suite lost its cases");
    for case in &suite.cases {
        let (method, path, query, mut headers, body) = parse(&case.request);
        let time: DateTime<Utc> = case.timestamp.parse().expect("an RFC 3339 time");
        headers.push(("X-Amz-Date".into(), time.format("%Y%m%dT%H%M%SZ").to_string()));
        let payload_hash = sigv4::sha256_hex(&body);
        if case.sign_body {
            headers.push(("x-amz-content-sha256".into(), payload_hash.clone()));
        }
        let signed = sigv4::sign(
            &SignableRequest {
                method: &method,
                path: &path,
                query: &query,
                headers: &headers,
                payload_hash: &payload_hash,
            },
            &SigningParams {
                access_key_id: &case.access_key_id,
                secret_access_key: &case.secret_access_key,
                region: &case.region,
                service: &case.service,
                time,
                normalize_path: case.normalize,
            },
        );
        assert_eq!(signed.canonical_request, case.canonical_request, "canonical request of {}", case.name);
        assert_eq!(signed.string_to_sign, case.string_to_sign, "string to sign of {}", case.name);
        assert_eq!(signed.signature, case.signature.trim(), "signature of {}", case.name);
        assert!(signed.authorization.ends_with(&format!("Signature={}", case.signature.trim())));
    }
}
