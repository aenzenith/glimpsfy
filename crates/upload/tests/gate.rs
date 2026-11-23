//! What leaves the machine: one rule, walked over every built-in provider.
//!
//! Before the user confirms a provider's destination, no command makes a request: not an
//! upload, not a test, not a deletion. After the confirmation, requests go to that destination's
//! host and to no other, even when the host's answer names another one (a link, a deletion link,
//! a redirect). Each public host is moved to a local test server; a second local server, the
//! canary, stands for every other host on the Internet and must never hear a thing.
//!
//! The app's switches win over a confirmation: with Allow uploads off or Work offline on, nothing
//! is sent to any host, and a switch turned off during an upload stops it.

mod support;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use glimpsfy_upload::engine::Stored;
use glimpsfy_upload::http::{Gate, HttpClient};
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{
    providers, DeleteOutcome, DeletionHandle, ProviderId, SwitchedOff, UploadData, UploadError, Uploader,
};
use serde_json::{json, Value};
use support::{Reply, TestServer};

struct Fixture {
    server: TestServer,
    canary: TestServer,
    uploader: Uploader,
}

fn fixture() -> Fixture {
    fixture_with(HttpClient::for_tests())
}

fn fixture_with(client: HttpClient) -> Fixture {
    let canary = TestServer::canary();
    let elsewhere = canary.origin();
    let server = TestServer::start(move |request| {
        let path = request.target.split('?').next().unwrap_or("");
        match (request.method.as_str(), path) {
            ("POST", "/user/api.php") => Reply::text(200, &format!("{elsewhere}/catbox.png")),
            ("POST", "/resources/internals/api.php") => Reply::text(200, &format!("{elsewhere}/litterbox.png")),
            ("POST", "/") => Reply::text(200, &format!("{elsewhere}/zero.png\n")).header("x-token", "zero-token"),
            ("POST", "/1/upload") => Reply::json(
                200,
                json!({ "success": true, "status": 200,
                        "data": { "url": format!("{elsewhere}/imgbb.png"), "delete_url": format!("{elsewhere}/imgbb-delete") } }),
            ),
            ("PUT", p) if p.starts_with("/bucket/") => Reply::text(200, ""),
            ("DELETE", p) if p.starts_with("/bucket/") => Reply::text(204, ""),
            ("POST", "/custom") => Reply::json(
                200,
                json!({ "url": format!("{elsewhere}/custom.png"), "del": format!("{elsewhere}/custom-delete") }),
            ),
            ("POST", "/redirect") => Reply::text(302, "").header("location", &format!("{elsewhere}/moved")),
            ("GET", "/") => Reply::text(200, "front page"),
            _ => Reply::text(404, "no such path"),
        }
    });
    let store = keyring_core::mock::Store::new().expect("the mock keychain");
    let uploader =
        Uploader::new(glimpsfy_upload::builtin_providers_at(&server.url), SecretStore::with_store(store), client);
    uploader.set_secret(&id(providers::IMGBB), "apiKey", "imgbb-key-0001").expect("a stored key");
    uploader.set_secret(&id(providers::S3), "secretAccessKey", "s3-secret-0001").expect("a stored secret");
    uploader
        .set_secret(&id(providers::CUSTOM), "headers:Authorization", "Bearer custom-0001")
        .expect("a stored header");
    Fixture { server, canary, uploader }
}

fn id(name: &str) -> ProviderId {
    ProviderId::new(name)
}

/// Each provider's settings, pointed at the test server.
fn values(server: &TestServer, provider: &str) -> Value {
    let origin = server.origin();
    match provider {
        providers::LITTERBOX => json!({ "time": "1h" }),
        providers::ZERO_X_ZERO => json!({ "secret": true }),
        providers::IMGBB => json!({ "expiration": "0" }),
        providers::S3 => json!({
            "endpoint": origin, "region": "us-east-1", "bucket": "bucket",
            "addressing": "path", "accessKeyId": "AKIDTEST"
        }),
        providers::CUSTOM => json!({
            "requestUrl": format!("{origin}/custom"), "method": "POST", "body": "multipart",
            "fileFormName": "file", "headers": ["Authorization"],
            "urlTemplate": "{json:url}", "deletionUrlTemplate": "{json:del}"
        }),
        _ => json!({}),
    }
}

/// A deletion handle of the kind each provider hands out.
fn handle(server: &TestServer, provider: &str) -> DeletionHandle {
    match provider {
        providers::ZERO_X_ZERO => {
            DeletionHandle::Token { token: "zero-token".into(), url: Some(format!("{}/zero.png", server.origin())) }
        }
        providers::S3 => {
            DeletionHandle::ObjectKey { key: "a/b.png".into(), endpoint: server.origin(), bucket: "bucket".into() }
        }
        _ => DeletionHandle::Url { url: format!("{}/delete", server.origin()) },
    }
}

fn png() -> UploadData {
    UploadData::Bytes(Arc::from(&b"\x89PNG\r\n\x1a\nnot really an image, but bytes all the same"[..]))
}

#[test]
fn no_request_leaves_before_the_user_confirms_the_destination() {
    let f = fixture();
    for provider in providers::BUILTIN {
        let values = values(&f.server, provider);
        let pid = id(provider);
        // Nothing confirmed; then consent for the same provider at another address, which must
        // not count for this one.
        let other = [format!("{provider}@elsewhere.example"), format!("{provider}@127.0.0.1:1/bucket")];
        for confirmed in [&[][..], &other[..]] {
            let stored = Stored { values: Some(&values), confirmed };
            let begun = f.uploader.begin(&pid, stored, png(), "shot.png", "image/png");
            assert!(matches!(begun, Err(UploadError::NotConfirmed { .. })), "{provider} began an upload: {begun:?}");
            let tested = f.uploader.test(&pid, stored);
            assert!(matches!(tested, Err(UploadError::NotConfirmed { .. })), "{provider} ran a test: {tested:?}");
            match f.uploader.delete(&pid, stored, &handle(&f.server, provider)) {
                Err(UploadError::NotConfirmed { .. }) => {}
                // A deletion page is handed to the browser; Glimpsfy itself requests nothing.
                Ok(DeleteOutcome::OpenPage { .. }) => {}
                other => panic!("{provider} deleted without consent: {other:?}"),
            }
        }
    }
    assert!(f.server.requests().is_empty(), "the host heard {:?}", f.server.requests());
    assert!(f.canary.requests().is_empty(), "another host heard {:?}", f.canary.requests());
}

/// Consent to a host over `https` does not cover the same host over plain `http`: the dialog
/// that warns about unencrypted requests (with keychain headers in them) must show again for
/// every provider whose address the user sets. A consent recorded for `https` keeps its key.
#[test]
fn consent_to_https_does_not_cover_the_same_host_over_plain_http() {
    let uploader = Uploader::new(
        glimpsfy_upload::builtin_providers(),
        SecretStore::with_store(keyring_core::mock::Store::new().expect("the mock keychain")),
        HttpClient::for_tests(),
    );
    let values = |provider: &str, scheme: &str| match provider {
        providers::S3 => json!({
            "endpoint": format!("{scheme}://minio.example.dev:9000"), "region": "us-east-1",
            "bucket": "team", "addressing": "path", "accessKeyId": "AKIDTEST"
        }),
        providers::ZERO_X_ZERO => json!({ "instance": format!("{scheme}://0x0.example.dev/") }),
        _ => json!({ "requestUrl": format!("{scheme}://share.example.dev/up"), "urlTemplate": "{json:url}" }),
    };
    for (provider, https_key) in [
        (providers::CUSTOM, "custom@share.example.dev"),
        (providers::S3, "s3@minio.example.dev:9000/team"),
        (providers::ZERO_X_ZERO, "0x0@0x0.example.dev"),
    ] {
        let pid = id(provider);
        let (secure, plain) = (values(provider, "https"), values(provider, "http"));
        let secure_key = uploader.consent_key(&pid, Some(&secure)).expect("a consent key");
        let plain_key = uploader.consent_key(&pid, Some(&plain)).expect("a consent key");
        assert_eq!(secure_key, https_key, "{provider}'s https consents were invalidated");
        assert_ne!(secure_key, plain_key, "{provider} shares a consent between http and https");

        let confirmed = [secure_key];
        let stored = Stored { values: Some(&plain), confirmed: &confirmed };
        let begun = uploader.begin(&pid, stored, png(), "shot.png", "image/png");
        assert!(matches!(begun, Err(UploadError::NotConfirmed { .. })), "{provider} began over http: {begun:?}");
        let info = uploader.consent_info(&pid, stored).expect("the dialog's facts");
        assert!(!info.confirmed && info.plain_http, "{provider} would skip the plain http warning");
    }
}

#[test]
fn confirmed_requests_reach_the_chosen_host_and_no_other() {
    let f = fixture();
    let host =
        f.server.url.host_str().map(|h| format!("{h}:{}", f.server.url.port().unwrap_or(80))).unwrap_or_default();
    for provider in providers::BUILTIN {
        let values = values(&f.server, provider);
        let pid = id(provider);
        let key = f.uploader.consent_key(&pid, Some(&values)).expect("a consent key");
        let confirmed = [key];
        let stored = Stored { values: Some(&values), confirmed: &confirmed };
        let before = f.server.requests().len();

        let pending = f.uploader.begin(&pid, stored, png(), "shot.png", "image/png").expect("an admitted upload");
        let result = f.uploader.run(pending, &|_| {}).unwrap_or_else(|e| panic!("{provider} failed: {e}"));
        assert!(f.server.requests().len() > before, "{provider} uploaded nowhere");

        f.uploader.test(&pid, stored).unwrap_or_else(|e| panic!("{provider} test failed: {e}"));

        // The link and the deletion link point at the canary; deleting must not follow them there.
        if let Some(deletion) = &result.deletion {
            match f.uploader.delete(&pid, stored, deletion) {
                Ok(_) | Err(UploadError::HostNotAllowed { .. }) => {}
                Err(other) => panic!("{provider} delete failed oddly: {other}"),
            }
        }
    }

    // A redirect is not followed either.
    let values = json!({
        "requestUrl": format!("{}/redirect", f.server.origin()), "method": "POST", "body": "binary",
        "urlTemplate": "{json:url}"
    });
    let pid = id(providers::CUSTOM);
    let confirmed = [f.uploader.consent_key(&pid, Some(&values)).expect("a consent key")];
    let stored = Stored { values: Some(&values), confirmed: &confirmed };
    let pending = f.uploader.begin(&pid, stored, png(), "shot.png", "image/png").expect("an admitted upload");
    assert!(f.uploader.run(pending, &|_| {}).is_err(), "a redirect without a link counted as an upload");

    let requests = f.server.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        assert_eq!(
            request.header("host"),
            Some(host.as_str()),
            "{} {} went to another host",
            request.method,
            request.target
        );
        let agent = request.header("user-agent").unwrap_or("");
        assert!(agent.starts_with("Glimpsfy/") && !agent.contains(' '), "the user agent says more: {agent}");
        assert!(request.header("cookie").is_none() && request.header("referer").is_none());
    }
    assert!(f.canary.requests().is_empty(), "another host heard {:?}", f.canary.requests());
}

/// The switches as the app sets them: 0 open, 1 Allow uploads off, 2 Work offline on.
fn switches() -> (Arc<AtomicU8>, Gate) {
    let switch = Arc::new(AtomicU8::new(0));
    let read = switch.clone();
    let gate: Gate = Arc::new(move || match read.load(Ordering::SeqCst) {
        0 => None,
        1 => Some(SwitchedOff::Uploads),
        _ => Some(SwitchedOff::Offline),
    });
    (switch, gate)
}

#[test]
fn no_request_leaves_while_uploads_or_the_network_are_off() {
    let (switch, gate) = switches();
    let f = fixture_with(HttpClient::for_tests().with_gate(gate));
    let off = |result: &str, expected: SwitchedOff, outcome: Result<(), UploadError>| match outcome {
        Err(UploadError::SwitchedOff(found)) if found == expected => {}
        other => panic!("{result} went ahead with {expected:?}: {other:?}"),
    };
    for provider in providers::BUILTIN {
        let values = values(&f.server, provider);
        let pid = id(provider);
        // Confirmed, so only the switches stand between the call and the host.
        let confirmed = [f.uploader.consent_key(&pid, Some(&values)).expect("a consent key")];
        let stored = Stored { values: Some(&values), confirmed: &confirmed };
        for (position, expected) in [(1, SwitchedOff::Uploads), (2, SwitchedOff::Offline)] {
            switch.store(position, Ordering::SeqCst);
            let begun = f.uploader.begin(&pid, stored, png(), "shot.png", "image/png").map(drop);
            off(&format!("{provider}'s upload"), expected, begun);
            off(&format!("{provider}'s test"), expected, f.uploader.test(&pid, stored).map(drop));
            let deleted = f.uploader.delete(&pid, stored, &handle(&f.server, provider)).map(drop);
            off(&format!("{provider}'s deletion"), expected, deleted);

            // Admitted while the switches allowed it, switched off before its thread sent it.
            switch.store(0, Ordering::SeqCst);
            let pending = f.uploader.begin(&pid, stored, png(), "shot.png", "image/png").expect("an admitted upload");
            switch.store(position, Ordering::SeqCst);
            off(&format!("{provider}'s admitted upload"), expected, f.uploader.run(pending, &|_| {}).map(drop));
        }
    }
    assert!(f.server.requests().is_empty(), "the host heard {:?}", f.server.requests());
    assert!(f.canary.requests().is_empty(), "another host heard {:?}", f.canary.requests());

    // Switched off in the middle of the body: the rest of the file never leaves.
    let big = || UploadData::Bytes(Arc::from(vec![0x42u8; 4 * 1024 * 1024]));
    for provider in providers::BUILTIN {
        let values = values(&f.server, provider);
        let pid = id(provider);
        let confirmed = [f.uploader.consent_key(&pid, Some(&values)).expect("a consent key")];
        let stored = Stored { values: Some(&values), confirmed: &confirmed };
        switch.store(0, Ordering::SeqCst);
        let pending = f.uploader.begin(&pid, stored, big(), "big.png", "image/png").expect("an admitted upload");
        let flip = switch.clone();
        let ran = f.uploader.run(pending, &move |progress| {
            if progress.sent_bytes > 0 {
                flip.store(2, Ordering::SeqCst);
            }
        });
        off(&format!("{provider}'s upload in flight"), SwitchedOff::Offline, ran.map(drop));
    }
    let heard = f.server.requests();
    assert!(!heard.is_empty(), "no upload got as far as its body");
    assert!(
        heard.iter().all(|r| !r.complete),
        "a body went out whole after the switch: {:?}",
        heard.iter().map(|r| (&r.method, &r.target, r.body.len())).collect::<Vec<_>>()
    );
    assert!(f.canary.requests().is_empty(), "another host heard {:?}", f.canary.requests());
}
