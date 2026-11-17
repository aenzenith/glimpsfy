//! A `.sxcu` file comes from someone else. It may only describe one request to an `http` or
//! `https` host whose name it states, and its response syntax may only read the answer of that
//! request: no file, no prompt, no second request, and no value of the answer that gets expanded
//! again as if it were part of the template.

mod support;

use glimpsfy_upload::engine::Stored;
use glimpsfy_upload::http::HttpClient;
use glimpsfy_upload::secrets::SecretStore;
use glimpsfy_upload::{providers, sxcu, ProviderId, UploadData, UploadError, Uploader};
use serde_json::{json, Value};
use support::{Reply, TestServer};

fn file(request_url: &str, url_template: &str) -> String {
    json!({
        "Version": "17.0.0", "Name": "Example", "DestinationType": "ImageUploader",
        "RequestMethod": "POST", "RequestURL": request_url, "Body": "MultipartFormData",
        "FileFormName": "file", "URL": url_template
    })
    .to_string()
}

#[test]
fn a_file_that_reaches_beyond_one_http_request_is_refused() {
    let refused_urls = [
        "file:///etc/passwd",
        "ftp://files.example/upload",
        "javascript:alert(1)",
        "data:text/plain,hello",
        "smb://server/share",
        "/upload",
        "files.example/upload",
        "",
        // The host itself must be written out, not computed or hidden behind a user name.
        "https://{json:host}/upload",
        "https://{filename}/upload",
        "https://$json:host$/upload",
        "https://user@files.example/upload",
        "https:///upload",
    ];
    for url in refused_urls {
        assert!(sxcu::read(&file(url, "{json:url}")).is_err(), "the request URL {url:?} was accepted");
    }

    // Response syntax that would ask the user, read another input or parse XML is refused,
    // in every template of the file and in the values it sends.
    let refused_templates = [
        "{select:a|b}",
        "{inputbox:Link}",
        "{outputbox:Link}",
        "{prompt:Link}",
        "{input}",
        "{xml:/a/b}",
        "{json:{response}|url}",
        "{regex:(.*)|1|2}",
        "{unknown:x}",
    ];
    for template in refused_templates {
        assert!(sxcu::read(&file("https://files.example/upload", template)).is_err(), "URL {template:?} was accepted");
        let in_header = json!({
            "RequestURL": "https://files.example/upload", "FileFormName": "file",
            "Headers": { "X-Value": template }, "URL": "{json:url}"
        });
        assert!(sxcu::read(&in_header.to_string()).is_err(), "header {template:?} was accepted");
        let in_deletion = json!({
            "RequestURL": "https://files.example/upload", "FileFormName": "file",
            "URL": "{json:url}", "DeletionURL": template
        });
        assert!(sxcu::read(&in_deletion.to_string()).is_err(), "deletion URL {template:?} was accepted");
    }

    // A file without a way to carry the image, or with a header the client must set, is refused.
    for bad in [
        json!({ "RequestURL": "https://files.example/upload", "Body": "JSON", "Data": "{}" }),
        json!({ "RequestURL": "https://files.example/upload", "RequestMethod": "GET", "FileFormName": "file" }),
        json!({ "RequestURL": "https://files.example/upload", "FileFormName": "file", "Headers": { "Host": "other.example" } }),
    ] {
        assert!(sxcu::read(&bad.to_string()).is_err(), "{bad} was accepted");
    }

    // The same shapes with allowed syntax pass, so each refusal above is the syntax's.
    assert!(sxcu::read(&file("https://files.example/upload?key={random:a|b}", "{json:url}")).is_ok());
    let allowed = json!({
        "RequestURL": "https://files.example/upload", "FileFormName": "file",
        "Headers": { "X-Value": "{filename}" }, "URL": "{json:url}", "DeletionURL": "{regex:del=(\\w+)|1}"
    });
    assert!(sxcu::read(&allowed.to_string()).is_ok());
}

#[test]
fn the_answer_is_read_once_and_never_followed() {
    let canary = TestServer::canary();
    let elsewhere = canary.origin();
    let server = TestServer::start(move |request| match request.target.as_str() {
        // A link that is not a web address, a link built from template text, and a deletion link
        // elsewhere: none of them may lead to a file read or a request.
        "/file" => Reply::json(200, json!({ "url": "file:///etc/passwd" })),
        "/script" => Reply::json(200, json!({ "url": "javascript:fetch('https://evil.example')" })),
        _ => Reply::json(
            200,
            json!({ "url": format!("{elsewhere}/i/{{json:next}}{{header:server}}{{response}}"),
                    "next": "EXPANDED", "delete": format!("{elsewhere}/delete") }),
        )
        .header("server", "EXPANDED"),
    });
    let uploader = Uploader::new(
        glimpsfy_upload::builtin_providers(),
        SecretStore::with_store(keyring_core::mock::Store::new().expect("the mock keychain")),
        HttpClient::for_tests(),
    );
    let id = ProviderId::new(providers::CUSTOM);
    let upload = |path: &str| {
        let text = json!({
            "RequestURL": format!("{}{path}", server.origin()), "FileFormName": "file",
            "URL": "{json:url}", "DeletionURL": "{json:delete}"
        });
        let imported = uploader.import_sxcu(&text.to_string(), None).expect("the file imports");
        let values: Value = serde_json::to_value(&imported.values).expect("values serialize");
        let confirmed = [uploader.consent_key(&id, Some(&values)).expect("a consent key")];
        let stored = Stored { values: Some(&values), confirmed: &confirmed };
        let pending = uploader
            .begin(&id, stored, UploadData::Bytes(b"png".to_vec().into()), "shot.png", "image/png")
            .expect("an admitted upload");
        uploader.run(pending, &|_| {})
    };

    for path in ["/file", "/script"] {
        let result = upload(path);
        assert!(matches!(result, Err(UploadError::Provider { .. })), "{path} gave a link: {result:?}");
    }

    let result = upload("/link").expect("the answer has a web link");
    assert!(!result.url.contains("EXPANDED"), "a value of the answer was expanded again: {}", result.url);
    assert!(result.url.starts_with(&canary.origin()));
    assert!(canary.requests().is_empty(), "the link or the deletion link was requested");
    assert_eq!(server.requests().len(), 3, "the upload made more than one request each");
}
