//! ImgBB: image uploads with the user's own API key, an optional expiry, and a deletion page
//! the browser opens. ImgBB documents no way to check a key without an upload, so the connection
//! test uploads a 1 × 1 image that ImgBB deletes after 60 seconds. See `docs/PROVIDERS.md`.

use std::time::Instant;

use serde_json::Value;
use ureq::http::Method;
use url::Url;

use super::{endpoint, fixed_destination, refusal, CHECKED_ON, TEST_PNG};
use crate::http::{parse_http_url, Body, Connection, Multipart, Request, Response};
use crate::{
    AccountNeed, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FieldOption, FieldRule, FormShape,
    HostFacts, Issue, Provider, ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, Retention,
    SettingField, UploadData, UploadError, UploadRequest, UploadResult, Visibility,
};

const DOMAIN: &str = "imgbb.com";
const MAX_BYTES: u64 = 32_000_000;
/// The expiries the form offers, in seconds; 0 sets none. The API takes 60 to 15552000.
const EXPIRATIONS: [(u32, &str); 5] = [
    (0, "upload.option.never"),
    (86_400, "upload.option.days1"),
    (604_800, "upload.option.days7"),
    (2_592_000, "upload.option.days30"),
    (15_552_000, "upload.option.days180"),
];

pub(crate) struct ImgBb {
    base: Url,
}

impl ImgBb {
    pub(crate) fn new() -> ImgBb {
        ImgBb::at(Url::parse("https://api.imgbb.com/").unwrap_or_else(|_| unreachable!("a fixed, valid URL")))
    }

    pub(crate) fn at(base: Url) -> ImgBb {
        ImgBb { base }
    }

    fn expiration(config: &ProviderConfig) -> u32 {
        config.number("expiration").map(|n| n as u32).filter(|s| EXPIRATIONS.iter().any(|(e, _)| e == s)).unwrap_or(0)
    }

    fn send(
        &self,
        connection: &Connection,
        config: &ProviderConfig,
        (data, file_name, content_type): (&UploadData, &str, &str),
        expiration: u32,
        progress: Option<&dyn Fn(u64)>,
    ) -> Result<(Response, Value)> {
        let key = config.secret(&self.id(), "apiKey")?;
        let stem = file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(file_name);
        // The key travels in the form body, not in the URL, so it never sits in an address.
        let mut form = Multipart::new().text("key", key.expose()).text("name", stem);
        if expiration > 0 {
            form = form.text("expiration", &expiration.to_string());
        }
        let form = form.file("image", file_name, content_type, data);
        let header = form.content_type();
        let (reader, length) = form.into_reader()?;
        let response = connection.send(
            Request::new(Method::POST, endpoint(&self.base, "1/upload")?)
                .header("content-type", header)
                .body(Body::Stream { reader, length, progress }),
        )?;
        let json: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        if !response.is_success() || json.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(refusal(&self.id(), &response));
        }
        Ok((response, json))
    }
}

impl Provider for ImgBb {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::IMGBB)
    }

    fn describe(&self) -> ProviderDescription {
        let options = EXPIRATIONS.iter().map(|(s, label)| FieldOption::new(&s.to_string(), label)).collect();
        ProviderDescription {
            id: self.id(),
            name: "ImgBB".into(),
            monogram: "BB".into(),
            shape: FormShape::Key,
            account: AccountNeed::ApiKey,
            domain: Some(DOMAIN.into()),
            operator: Some("upload.host.imgbb.operator".into()),
            website: Some("https://imgbb.com/".into()),
            key_url: Some("https://api.imgbb.com/".into()),
            facts: Some(HostFacts {
                checked_on: CHECKED_ON.into(),
                sources: vec![
                    "https://api.imgbb.com/".into(),
                    "https://imgbb.com/".into(),
                    "https://imgbb.com/tos".into(),
                ],
                visibility: Visibility::NotStated,
                retention: Retention::Optional { setting: "expiration".into() },
                max_file_bytes: Some(MAX_BYTES),
                terms_url: Some("https://imgbb.com/tos".into()),
            }),
            max_file_bytes: Some(MAX_BYTES),
            links_expire: false,
            supports_delete: true,
            deletion: DeletionKind::Page,
            testable: true,
            settings: vec![
                SettingField::secret("apiKey", "upload.field.apiKey")
                    .required()
                    .placeholder("upload.placeholder.apiKey")
                    .rule(FieldRule::NoWhitespace)
                    .help("upload.help.keychain")
                    .section("upload.section.account"),
                SettingField::select("expiration", "upload.field.imgbbExpiration", options, "0")
                    .section("upload.section.retention"),
            ],
        }
    }

    fn validate(&self, config: &ProviderConfig) -> Vec<Issue> {
        let mut issues = Vec::new();
        if let Some(key) = config.secrets.get("apiKey") {
            if key.expose().chars().any(char::is_whitespace) {
                issues.push(Issue::error("apiKey", "upload.rule.noWhitespace"));
            }
        }
        issues
    }

    fn destination(&self, _config: &ProviderConfig) -> Result<Destination> {
        fixed_destination(DOMAIN, &self.base)
    }

    fn chosen_hours(&self, config: &ProviderConfig) -> Option<u32> {
        Some(ImgBb::expiration(config) / 3600)
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let expiration = ImgBb::expiration(&request.config);
        let file = (&request.data, request.file_name.as_str(), request.content_type.as_str());
        let (response, json) = self.send(connection, &request.config, file, expiration, Some(progress))?;
        let link =
            json.pointer("/data/url").and_then(Value::as_str).and_then(parse_http_url).ok_or_else(|| {
                UploadError::refused(&self.id(), Some(response.status), "ImgBB answered without a link")
            })?;
        let deletion = json
            .pointer("/data/delete_url")
            .and_then(Value::as_str)
            .and_then(parse_http_url)
            .map(|url| DeletionHandle::Url { url: url.to_string() });
        let now = crate::now_ms();
        Ok(UploadResult {
            upload_id: request.id,
            provider: self.id(),
            url: link.to_string(),
            deletion,
            uploaded_at_ms: now,
            expires_at_ms: (expiration > 0).then(|| now + i64::from(expiration) * 1000),
        })
    }

    fn delete(&self, _: &Connection, deletion: &DeletionHandle, _: &ProviderConfig) -> Result<DeleteOutcome> {
        match deletion {
            DeletionHandle::Url { url } => Ok(DeleteOutcome::OpenPage { url: url.clone() }),
            _ => Err(UploadError::invalid(&self.id(), "deletion", "ImgBB deletes through its deletion page only")),
        }
    }

    fn test(&self, connection: &Connection, config: &ProviderConfig) -> Result<ProviderTestResult> {
        let started = Instant::now();
        let data = UploadData::Bytes(TEST_PNG.into());
        let (_, json) = self.send(connection, config, (&data, "glimpsfy-test.png", "image/png"), 60, None)?;
        Ok(ProviderTestResult {
            provider: self.id(),
            checked_at_ms: crate::now_ms(),
            latency_ms: started.elapsed().as_millis() as u64,
            url: json.pointer("/data/url").and_then(Value::as_str).map(str::to_string),
            warnings: Vec::new(),
        })
    }
}
