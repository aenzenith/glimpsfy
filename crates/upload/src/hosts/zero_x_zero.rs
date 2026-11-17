//! 0x0.st: a public file host run by one person, which keeps a file for 30 days to a year
//! depending on its size and hands out a token that deletes it. Its page asks client software for
//! the user's consent, a user agent of its own, a word about what the host is, and a way to use
//! another instance; Glimpsfy does all four. See `docs/PROVIDERS.md`.

use ureq::http::Method;
use url::Url;

use super::{fixed_destination, reachability, text_link, CHECKED_ON};
use crate::http::{parse_http_url, Body, Connection, Multipart, Origin, Request};
use crate::{
    AccountNeed, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FieldInput, FormShape, HostFacts, Issue,
    Provider, ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, Retention, SettingField,
    UploadError, UploadRequest, UploadResult, Visibility,
};

const DOMAIN: &str = "0x0.st";
const DEFAULT_INSTANCE: &str = "https://0x0.st/";
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MIN_DAYS: f64 = 30.0;
const MAX_DAYS: f64 = 365.0;

pub(crate) struct ZeroXZero {
    base: Url,
}

impl ZeroXZero {
    pub(crate) fn new() -> ZeroXZero {
        ZeroXZero::at(Url::parse(DEFAULT_INSTANCE).unwrap_or_else(|_| unreachable!("a fixed, valid URL")))
    }

    pub(crate) fn at(base: Url) -> ZeroXZero {
        ZeroXZero { base }
    }

    /// The instance the settings name, or the built-in one.
    fn instance(&self, config: &ProviderConfig) -> Option<Url> {
        match config.text("instance") {
            Some(text) if text.trim_end_matches('/') != DEFAULT_INSTANCE.trim_end_matches('/') => {
                let mut url = parse_http_url(text)?;
                if !url.path().ends_with('/') {
                    url.set_path(&format!("{}/", url.path()));
                }
                Some(url)
            }
            _ => Some(self.base.clone()),
        }
    }

    fn is_default(&self, config: &ProviderConfig) -> bool {
        self.instance(config).is_some_and(|url| url == self.base)
    }
}

/// The expiry 0x0.st's page states for a file of `size` bytes: `min_age + (min_age - max_age) *
/// pow((file_size / max_size - 1), 3)` days.
fn expiry_days(size: u64) -> f64 {
    let ratio = (size as f64 / MAX_BYTES as f64).min(1.0);
    MIN_DAYS + (MIN_DAYS - MAX_DAYS) * (ratio - 1.0).powi(3)
}

impl Provider for ZeroXZero {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::ZERO_X_ZERO)
    }

    fn describe(&self) -> ProviderDescription {
        ProviderDescription {
            id: self.id(),
            name: "0x0.st".into(),
            monogram: "0x".into(),
            shape: FormShape::Anonymous,
            account: AccountNeed::None,
            domain: Some(DOMAIN.into()),
            operator: Some("upload.host.0x0.operator".into()),
            website: Some(DEFAULT_INSTANCE.into()),
            key_url: None,
            facts: Some(HostFacts {
                checked_on: CHECKED_ON.into(),
                sources: vec!["https://web.archive.org/web/20261103083434/https://0x0.st/".into()],
                visibility: Visibility::AnyoneWithLink,
                retention: Retention::BySize { min_days: 30, max_days: 365, max_bytes: MAX_BYTES },
                max_file_bytes: Some(MAX_BYTES),
                terms_url: Some(DEFAULT_INSTANCE.into()),
            }),
            max_file_bytes: Some(MAX_BYTES),
            links_expire: true,
            supports_delete: true,
            deletion: DeletionKind::Request,
            testable: false,
            settings: vec![
                SettingField {
                    input: FieldInput::Toggle { default: true },
                    ..SettingField::text("secret", "upload.field.hardToGuess")
                }
                .help("upload.help.0x0.secret"),
                SettingField::url("instance", "upload.field.instance", DEFAULT_INSTANCE)
                    .default_text(DEFAULT_INSTANCE)
                    .help("upload.help.0x0.instance"),
            ],
        }
    }

    fn validate(&self, config: &ProviderConfig) -> Vec<Issue> {
        let mut issues = Vec::new();
        match self.instance(config) {
            None => issues.push(Issue::error("instance", "upload.issue.httpOnly")),
            Some(url) => {
                if Origin::of(&url).is_some_and(|o| o.scheme == "http" && !o.is_local()) {
                    issues.push(Issue::warning("instance", "upload.issue.plainHttp"));
                }
            }
        }
        issues
    }

    fn destination(&self, config: &ProviderConfig) -> Result<Destination> {
        let instance = self.instance(config).ok_or_else(|| {
            UploadError::invalid(&self.id(), "instance", "the instance is not an http or https address")
        })?;
        if self.is_default(config) {
            return fixed_destination(DOMAIN, &instance);
        }
        let origin = Origin::of(&instance).ok_or_else(|| {
            UploadError::invalid(&self.id(), "instance", "the instance is not an http or https address")
        })?;
        Ok(Destination { domain: origin.display(), bucket: None, origins: vec![origin], fixed: false })
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let instance = self.instance(&request.config).ok_or_else(|| {
            UploadError::invalid(&self.id(), "instance", "the instance is not an http or https address")
        })?;
        let mut form = Multipart::new().file("file", &request.file_name, &request.content_type, &request.data);
        if request.config.toggle("secret").unwrap_or(true) {
            form = form.text("secret", "");
        }
        let size = request.data.len()?;
        let content_type = form.content_type();
        let (reader, length) = form.into_reader()?;
        let response = connection.send(
            Request::new(Method::POST, instance).header("content-type", content_type).body(Body::Stream {
                reader,
                length,
                progress: Some(progress),
            }),
        )?;
        let link = text_link(&self.id(), &response)?;
        let now = crate::now_ms();
        // `X-Expires` (milliseconds) is not on the host's page, so it is used only when it is a
        // plausible time; the page's formula covers the built-in instance otherwise.
        let header_expiry = response
            .header("x-expires")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|ms| *ms > now && *ms < now + 400 * 86_400_000);
        let expires_at_ms = header_expiry
            .or_else(|| self.is_default(&request.config).then(|| now + (expiry_days(size) * 86_400_000.0) as i64));
        let deletion = response
            .header("x-token")
            .map(|token| DeletionHandle::Token { token: token.trim().to_string(), url: Some(link.to_string()) });
        Ok(UploadResult {
            upload_id: request.id,
            provider: self.id(),
            url: link.to_string(),
            deletion,
            uploaded_at_ms: now,
            expires_at_ms,
        })
    }

    fn delete(
        &self,
        connection: &Connection,
        deletion: &DeletionHandle,
        _config: &ProviderConfig,
    ) -> Result<DeleteOutcome> {
        let DeletionHandle::Token { token, url: Some(url) } = deletion else {
            return Err(UploadError::invalid(&self.id(), "deletion", "0x0.st needs the file's link and its token"));
        };
        let url = parse_http_url(url).ok_or_else(|| {
            UploadError::invalid(&self.id(), "deletion", "the file's link is not an http or https address")
        })?;
        let form = Multipart::new().text("token", token).text("delete", "");
        let content_type = form.content_type();
        let (reader, length) = form.into_reader()?;
        let response = connection.send(
            Request::new(Method::POST, url).header("content-type", content_type).body(Body::Stream {
                reader,
                length,
                progress: None,
            }),
        )?;
        if response.is_success() {
            Ok(DeleteOutcome::Deleted)
        } else {
            Err(super::refusal(&self.id(), &response))
        }
    }

    fn test(&self, connection: &Connection, config: &ProviderConfig) -> Result<ProviderTestResult> {
        let instance = self.instance(config).unwrap_or_else(|| self.base.clone());
        reachability(&self.id(), connection, &instance)
    }
}
