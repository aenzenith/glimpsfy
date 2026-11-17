//! Secrets go to the keychain and nowhere else: not into the values the settings file keeps, not
//! into an error (even when the host quotes the key back), not into a request to anyone but the
//! provider that needs it. The keychain here is keyring-core's mock store.

mod support;

use glimpsfy_upload::engine::Stored;
use glimpsfy_upload::http::HttpClient;
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{providers, ProviderId, UploadData, UploadError, Uploader};
use serde_json::{json, Value};
use support::{Reply, TestServer};

const IMGBB_KEY: &str = "imgbb-key-4f1c9e";
const S3_SECRET: &str = "s3/secret+key=7a2b";
const TOKEN: &str = "Bearer sxcu-token-9d81";
const ARGUMENT: &str = "argument-secret-55aa";

/// Every host of this test quotes the whole request back in its refusal, as a careless server
/// might.
fn echoing_server() -> TestServer {
    TestServer::start(|request| {
        let mut echo = format!("{} {}\n", request.method, request.target);
        for (name, value) in &request.headers {
            echo.push_str(&format!("{name}: {value}\n"));
        }
        echo.push_str(&String::from_utf8_lossy(&request.body));
        Reply::json(401, json!({ "error": { "message": echo }, "message": echo }))
    })
}

fn secrets_in(text: &str) -> Vec<&'static str> {
    [IMGBB_KEY, S3_SECRET, TOKEN, ARGUMENT]
        .into_iter()
        .filter(|secret| text.contains(secret) || text.contains(&secret.replace('/', "%2F").replace('+', "%2B")))
        .collect()
}

#[test]
fn secrets_stay_in_the_keychain_and_out_of_settings_errors_and_other_requests() {
    let server = echoing_server();
    let uploader = Uploader::new(
        glimpsfy_upload::builtin_providers_at(&server.url),
        SecretStore::with_store(keyring_core::mock::Store::new().expect("the mock keychain")),
        HttpClient::for_tests(),
    );
    let imgbb = ProviderId::new(providers::IMGBB);
    let s3 = ProviderId::new(providers::S3);
    let custom = ProviderId::new(providers::CUSTOM);

    uploader.set_secret(&imgbb, "apiKey", IMGBB_KEY).expect("the key is stored");
    uploader.set_secret(&s3, "secretAccessKey", S3_SECRET).expect("the secret is stored");
    // A ShareX file brings its token and argument: both go to the keychain on import.
    let sxcu = json!({
        "Version": "17.0.0", "DestinationType": "ImageUploader", "RequestMethod": "POST",
        "RequestURL": format!("{}/custom", server.origin()), "Body": "MultipartFormData",
        "FileFormName": "file", "Headers": { "Authorization": TOKEN }, "Arguments": { "key": ARGUMENT },
        "URL": "{json:url}"
    });
    let imported = uploader.import_sxcu(&sxcu.to_string(), None).expect("the file imports");
    let custom_values = serde_json::to_value(&imported.values).expect("values serialize");

    // What the settings file would keep holds names only.
    let settings = json!({ "providers": {
        "imgbb": {}, "custom": custom_values,
        "s3": { "endpoint": server.origin(), "region": "us-east-1", "bucket": "bucket",
                "addressing": "path", "accessKeyId": "AKIDTEST" }
    }});
    assert!(secrets_in(&settings.to_string()).is_empty(), "a secret reached the settings: {settings}");
    for (provider, key) in
        [(&imgbb, "apiKey"), (&s3, "secretAccessKey"), (&custom, "headers:Authorization"), (&custom, "arguments:key")]
    {
        assert!(uploader.secret_status(provider, key).expect("a status"), "{provider} {key} is not in the keychain");
    }

    // Each upload is refused by a host that quotes the request; the error must not quote a secret.
    for provider in [&imgbb, &s3, &custom] {
        let values: &Value = &settings["providers"][provider.as_str()];
        let confirmed = [uploader.consent_key(provider, Some(values)).expect("a consent key")];
        let stored = Stored { values: Some(values), confirmed: &confirmed };
        let pending = uploader
            .begin(provider, stored, UploadData::Bytes(b"png bytes".to_vec().into()), "shot.png", "image/png")
            .expect("an admitted upload");
        let error = uploader.run(pending, &|_| {}).expect_err("the host refused");
        assert!(matches!(error, UploadError::Provider { .. }), "{provider}: {error:?}");
        let text = format!("{error} {error:?}");
        assert!(secrets_in(&text).is_empty(), "{provider}'s error carries a secret: {text}");
        let tested = uploader.test(provider, stored).expect_err("the host refused the test");
        let text = format!("{tested} {tested:?}");
        assert!(secrets_in(&text).is_empty(), "{provider}'s test error carries a secret: {text}");
    }

    let requests = server.requests();
    for request in &requests {
        // A key never travels in an address, where proxies and server logs keep it.
        assert!(secrets_in(&request.target).is_empty(), "a secret in the address {}", request.target);
        // The S3 secret signs requests; it is never sent at all.
        let raw = format!("{:?} {}", request.headers, String::from_utf8_lossy(&request.body));
        assert!(!raw.contains(S3_SECRET), "the S3 secret left the machine");
    }

    // Removing a secret removes it: the next upload stops before any request.
    let before = requests.len();
    uploader.remove_secret(&imgbb, "apiKey").expect("the key is removed");
    assert!(!uploader.secret_status(&imgbb, "apiKey").expect("a status"));
    let confirmed = [providers::IMGBB.to_string()];
    let empty = json!({});
    let stored = Stored { values: Some(&empty), confirmed: &confirmed };
    let begun = uploader.begin(&imgbb, stored, UploadData::Bytes(b"png".to_vec().into()), "a.png", "image/png");
    assert!(matches!(begun, Err(UploadError::MissingSecret { .. })), "{begun:?}");
    assert_eq!(server.requests().len(), before);
}
