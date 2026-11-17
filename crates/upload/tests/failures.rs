//! A failed or cancelled upload ends without a link, frees its id, and says why: the host's own
//! reason with its HTTP status, or the user's cancel. A failure that looked like success would
//! put a dead link on the clipboard and in the history.

mod support;

use std::sync::Arc;

use glimpsfy_upload::engine::Stored;
use glimpsfy_upload::http::HttpClient;
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{providers, ProviderId, UploadData, UploadError, Uploader};
use serde_json::{json, Value};
use support::{Reply, TestServer};

const REASON: &str = "the file is over this account's quota";

fn uploader(server: &TestServer) -> Uploader {
    let uploader = Uploader::new(
        glimpsfy_upload::builtin_providers_at(&server.url),
        SecretStore::with_store(keyring_core::mock::Store::new().expect("the mock keychain")),
        HttpClient::for_tests(),
    );
    uploader.set_secret(&ProviderId::new(providers::IMGBB), "apiKey", "imgbb-key-0001").expect("a key");
    uploader.set_secret(&ProviderId::new(providers::S3), "secretAccessKey", "s3-secret-0001").expect("a secret");
    uploader
}

fn values(server: &TestServer, provider: &str) -> Value {
    match provider {
        providers::S3 => json!({
            "endpoint": server.origin(), "region": "us-east-1", "bucket": "bucket",
            "addressing": "path", "accessKeyId": "AKIDTEST"
        }),
        providers::CUSTOM => json!({
            "requestUrl": format!("{}/custom", server.origin()), "method": "POST", "body": "multipart",
            "fileFormName": "file", "urlTemplate": "{json:url}"
        }),
        _ => json!({}),
    }
}

fn image(bytes: usize) -> UploadData {
    UploadData::Bytes(Arc::from(vec![7u8; bytes]))
}

#[test]
fn a_refusal_carries_the_hosts_reason_and_status_for_every_provider() {
    // Each host refuses in its own dialect: plain text, JSON, S3's XML.
    let server = TestServer::start(|request| {
        let path = request.target.split('?').next().unwrap_or("");
        match path {
            "/1/upload" => Reply::json(400, json!({ "status_code": 400, "error": { "message": REASON } })),
            "/custom" => Reply::json(413, json!({ "message": REASON })),
            p if p.starts_with("/bucket/") => Reply::text(
                403,
                &format!("<?xml version=\"1.0\"?><Error><Code>QuotaExceeded</Code><Message>{REASON}</Message></Error>"),
            ),
            _ => Reply::text(413, &format!("<html><body><h1>413</h1><p>{REASON}</p></body></html>")),
        }
    });
    let uploader = uploader(&server);
    for provider in providers::BUILTIN {
        let id = ProviderId::new(provider);
        let values = values(&server, provider);
        let confirmed = [uploader.consent_key(&id, Some(&values)).expect("a consent key")];
        let stored = Stored { values: Some(&values), confirmed: &confirmed };
        let pending = uploader.begin(&id, stored, image(2048), "shot.png", "image/png").expect("an admitted upload");
        let upload_id = pending.id.clone();
        match uploader.run(pending, &|_| {}) {
            Err(UploadError::Provider { status: Some(status), reason, .. }) => {
                assert!((400..500).contains(&status), "{provider} lost the status: {status}");
                assert!(reason.contains(REASON), "{provider} lost the host's reason: {reason}");
            }
            other => panic!("{provider} did not report the refusal: {other:?}"),
        }
        assert!(!uploader.cancel(&upload_id), "{provider} left its failed upload running");
    }
}

#[test]
fn a_cancelled_upload_stops_sending_and_returns_no_link() {
    let server = TestServer::start(|_| Reply::text(200, "https://files.example/a.png"));
    let uploader = uploader(&server);
    let id = ProviderId::new(providers::CATBOX);
    let confirmed = [providers::CATBOX.to_string()];
    let stored = Stored { values: None, confirmed: &confirmed };
    let pending =
        uploader.begin(&id, stored, image(8 * 1024 * 1024), "shot.png", "image/png").expect("an admitted upload");
    let upload_id = pending.id.clone();

    // The user presses Cancel as soon as the first bytes are out.
    let result = uploader.run(pending, &|progress| {
        if progress.sent_bytes > 0 {
            uploader.cancel(&progress.upload_id);
        }
    });

    assert!(matches!(result, Err(UploadError::Cancelled)), "the upload went on: {result:?}");
    assert!(!uploader.cancel(&upload_id), "the cancelled upload is still registered");
    // The host never got the whole file, so it never answered with a link.
    assert!(server.requests().iter().all(|r| !r.complete), "the whole file reached the host");
}
