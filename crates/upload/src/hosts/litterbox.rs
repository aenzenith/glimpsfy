//! Litterbox (litterbox.catbox.moe): Catbox's temporary host, which deletes an upload 1, 12, 24
//! or 72 hours after it arrives. It offers no way to delete earlier. See `docs/PROVIDERS.md`.

use ureq::http::Method;
use url::Url;

use super::{endpoint, fixed_destination, reachability, text_link, CHECKED_ON};
use crate::http::{Body, Connection, Multipart, Request};
use crate::{
    AccountNeed, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FieldOption, FormShape, HostFacts, Issue,
    Provider, ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, Retention, SettingField,
    UploadError, UploadRequest, UploadResult, Visibility,
};

const DOMAIN: &str = "litterbox.catbox.moe";
/// The periods the API takes, in hours; the API spells them `1h`, `12h`, `24h` and `72h`.
const HOURS: [u32; 4] = [1, 12, 24, 72];

pub(crate) struct Litterbox {
    base: Url,
}

impl Litterbox {
    pub(crate) fn new() -> Litterbox {
        Litterbox::at(
            Url::parse("https://litterbox.catbox.moe/").unwrap_or_else(|_| unreachable!("a fixed, valid URL")),
        )
    }

    pub(crate) fn at(base: Url) -> Litterbox {
        Litterbox { base }
    }

    fn hours(config: &ProviderConfig) -> u32 {
        config
            .text("time")
            .and_then(|t| t.trim_end_matches('h').parse().ok())
            .filter(|h| HOURS.contains(h))
            .unwrap_or(72)
    }
}

impl Provider for Litterbox {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::LITTERBOX)
    }

    fn describe(&self) -> ProviderDescription {
        let max = 1_000_000_000;
        let options =
            HOURS.iter().map(|h| FieldOption::new(&format!("{h}h"), &format!("upload.option.hours{h}"))).collect();
        ProviderDescription {
            id: self.id(),
            name: "Litterbox".into(),
            monogram: "Lb".into(),
            shape: FormShape::Anonymous,
            account: AccountNeed::None,
            domain: Some(DOMAIN.into()),
            operator: Some("upload.host.litterbox.operator".into()),
            website: Some("https://litterbox.catbox.moe/".into()),
            key_url: None,
            facts: Some(HostFacts {
                checked_on: CHECKED_ON.into(),
                sources: vec![
                    "https://litterbox.catbox.moe/".into(),
                    "https://litterbox.catbox.moe/tools.php".into(),
                    "https://litterbox.catbox.moe/faq.php".into(),
                    "https://catbox.moe/legal.php".into(),
                ],
                visibility: Visibility::NotStated,
                retention: Retention::Chosen { setting: "time".into(), hours: HOURS.to_vec() },
                max_file_bytes: Some(max),
                terms_url: Some("https://catbox.moe/legal.php".into()),
            }),
            max_file_bytes: Some(max),
            links_expire: true,
            supports_delete: false,
            deletion: DeletionKind::None,
            testable: false,
            settings: vec![SettingField::select("time", "upload.field.litterboxTime", options, "72h")
                .section("upload.section.retention")],
        }
    }

    fn validate(&self, _config: &ProviderConfig) -> Vec<Issue> {
        Vec::new()
    }

    fn destination(&self, _config: &ProviderConfig) -> Result<Destination> {
        fixed_destination(DOMAIN, &self.base)
    }

    fn chosen_hours(&self, config: &ProviderConfig) -> Option<u32> {
        Some(Litterbox::hours(config))
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let hours = Litterbox::hours(&request.config);
        let form = Multipart::new().text("reqtype", "fileupload").text("time", &format!("{hours}h")).file(
            "fileToUpload",
            &request.file_name,
            &request.content_type,
            &request.data,
        );
        let content_type = form.content_type();
        let (reader, length) = form.into_reader()?;
        let response = connection.send(
            Request::new(Method::POST, endpoint(&self.base, "resources/internals/api.php")?)
                .header("content-type", content_type)
                .body(Body::Stream { reader, length, progress: Some(progress) }),
        )?;
        let link = text_link(&self.id(), &response)?;
        let now = crate::now_ms();
        Ok(UploadResult {
            upload_id: request.id,
            provider: self.id(),
            url: link.to_string(),
            deletion: None,
            uploaded_at_ms: now,
            expires_at_ms: Some(now + i64::from(hours) * 3_600_000),
        })
    }

    fn delete(&self, _: &Connection, _: &DeletionHandle, _: &ProviderConfig) -> Result<DeleteOutcome> {
        Err(UploadError::refused(&self.id(), None, "Litterbox does not delete uploads before they expire"))
    }

    fn test(&self, connection: &Connection, _config: &ProviderConfig) -> Result<ProviderTestResult> {
        reachability(&self.id(), connection, &self.base)
    }
}
