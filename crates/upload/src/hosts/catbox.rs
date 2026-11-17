//! Catbox (catbox.moe): anonymous uploads through its public API, kept until two years pass
//! without a visit. Anonymous uploads cannot be deleted. See `docs/PROVIDERS.md`.

use ureq::http::Method;
use url::Url;

use super::{endpoint, fixed_destination, reachability, text_link, CHECKED_ON};
use crate::http::{Body, Connection, Multipart, Request};
use crate::{
    AccountNeed, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FormShape, HostFacts, Issue, Provider,
    ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, Retention, UploadError, UploadRequest,
    UploadResult, Visibility,
};

const DOMAIN: &str = "catbox.moe";

pub(crate) struct Catbox {
    base: Url,
}

impl Catbox {
    pub(crate) fn new() -> Catbox {
        Catbox::at(Url::parse("https://catbox.moe/").unwrap_or_else(|_| unreachable!("a fixed, valid URL")))
    }

    pub(crate) fn at(base: Url) -> Catbox {
        Catbox { base }
    }
}

impl Provider for Catbox {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::CATBOX)
    }

    fn describe(&self) -> ProviderDescription {
        let max = 200_000_000;
        ProviderDescription {
            id: self.id(),
            name: "Catbox".into(),
            monogram: "Cb".into(),
            shape: FormShape::Anonymous,
            account: AccountNeed::None,
            domain: Some(DOMAIN.into()),
            operator: Some("upload.host.catbox.operator".into()),
            website: Some("https://catbox.moe/".into()),
            key_url: None,
            facts: Some(HostFacts {
                checked_on: CHECKED_ON.into(),
                sources: vec![
                    "https://catbox.moe/".into(),
                    "https://catbox.moe/tools.php".into(),
                    "https://catbox.moe/faq.php".into(),
                    "https://catbox.moe/legal.php".into(),
                ],
                visibility: Visibility::AnyoneWithLink,
                retention: Retention::Inactivity { days: 730 },
                max_file_bytes: Some(max),
                terms_url: Some("https://catbox.moe/legal.php".into()),
            }),
            max_file_bytes: Some(max),
            links_expire: true,
            supports_delete: false,
            deletion: DeletionKind::None,
            testable: false,
            settings: Vec::new(),
        }
    }

    fn validate(&self, _config: &ProviderConfig) -> Vec<Issue> {
        Vec::new()
    }

    fn destination(&self, _config: &ProviderConfig) -> Result<Destination> {
        fixed_destination(DOMAIN, &self.base)
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let form = Multipart::new().text("reqtype", "fileupload").file(
            "fileToUpload",
            &request.file_name,
            &request.content_type,
            &request.data,
        );
        let content_type = form.content_type();
        let (reader, length) = form.into_reader()?;
        let response = connection.send(
            Request::new(Method::POST, endpoint(&self.base, "user/api.php")?)
                .header("content-type", content_type)
                .body(Body::Stream { reader, length, progress: Some(progress) }),
        )?;
        let link = text_link(&self.id(), &response)?;
        Ok(UploadResult {
            upload_id: request.id,
            provider: self.id(),
            url: link.to_string(),
            deletion: None,
            uploaded_at_ms: crate::now_ms(),
            expires_at_ms: None,
        })
    }

    fn delete(&self, _: &Connection, _: &DeletionHandle, _: &ProviderConfig) -> Result<DeleteOutcome> {
        Err(UploadError::refused(&self.id(), None, "Catbox does not delete anonymous uploads"))
    }

    fn test(&self, connection: &Connection, _config: &ProviderConfig) -> Result<ProviderTestResult> {
        reachability(&self.id(), connection, &self.base)
    }
}
