//! A custom HTTP endpoint, typed in or read from a ShareX `.sxcu` file (`sxcu`).
//!
//! The request goes to the request URL and nowhere else: a redirect comes back as it is (its
//! `Location` is what `{responseurl}` and `{header:location}` read), and the deletion link the
//! answer names is opened in the browser, never requested by Glimpsfy.

use std::time::Instant;

use ureq::http::Method;

use super::TEST_PNG;
use crate::http::{parse_http_url, Body, Connection, Multipart, Origin, Request, Response};
use crate::syntax::{Answer, Place, Template};
use crate::{
    sxcu, AccountNeed, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FieldOption, FieldValue, FormShape,
    Issue, Provider, ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, SettingField,
    UploadData, UploadError, UploadRequest, UploadResult,
};

pub(crate) struct Custom;

const RESPONSE_TOKENS: [&str; 6] = ["response", "responseurl", "header:", "json:", "regex:", "filename"];

impl Custom {
    fn template(
        config: &ProviderConfig,
        key: &str,
        place: Place,
    ) -> Option<std::result::Result<Template, &'static str>> {
        let text = config.text(key)?;
        Some(Template::parse(text, place).map_err(|e| e.message))
    }

    /// Sends the file as the settings describe and returns the answer.
    fn send(
        &self,
        connection: &Connection,
        config: &ProviderConfig,
        data: &UploadData,
        file_name: &str,
        content_type: &str,
        progress: Option<&dyn Fn(u64)>,
    ) -> Result<Response> {
        let id = self.id();
        let invalid = |field: &str, reason: &str| UploadError::invalid(&id, field, reason);
        let url_template = Custom::template(config, "requestUrl", Place::Request)
            .ok_or_else(|| invalid("requestUrl", "the request URL is missing"))?
            .map_err(|m| invalid("requestUrl", m))?;
        // A file name keeps `#`, `?`, `%`, `&` and spaces, which would cut the path or start a
        // query, so it enters the address percent-encoded; headers and the form get it as it is.
        let url_name = crate::sigv4::encode(file_name, true);
        let mut url = parse_http_url(&url_template.fill(&url_name))
            .ok_or_else(|| invalid("requestUrl", "the request URL is not an http or https address"))?;
        let fill = |value: &str| -> Result<String> {
            Ok(Template::parse(value, Place::Request).map_err(|e| invalid("headers", e.message))?.fill(file_name))
        };
        {
            let parameters = config.secret_pairs("parameters");
            if !parameters.is_empty() {
                let mut query = url.query_pairs_mut();
                for (name, value) in &parameters {
                    query.append_pair(name, &fill(value.expose())?);
                }
            }
        }
        let method = match config.text("method").unwrap_or("POST") {
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            _ => Method::POST,
        };
        let mut request = Request::new(method, url);
        for (name, value) in config.secret_pairs("headers") {
            request = request.header(&name, fill(value.expose())?);
        }
        let request = if config.text("body") == Some("binary") {
            let length = data.len()?;
            request.header("content-type", content_type).body(Body::Stream { reader: data.open()?, length, progress })
        } else {
            let mut form = Multipart::new();
            for (name, value) in config.secret_pairs("arguments") {
                form = form.text(&name, &fill(value.expose())?);
            }
            let form = form.file(config.text("fileFormName").unwrap_or("file"), file_name, content_type, data);
            let header = form.content_type();
            let (reader, length) = form.into_reader()?;
            request.header("content-type", header).body(Body::Stream { reader, length, progress })
        };
        connection.send(request)
    }

    /// Reads the link, and the deletion link, from the answer.
    fn read_answer(
        &self,
        config: &ProviderConfig,
        response: &Response,
        file_name: &str,
    ) -> Result<(String, Option<String>)> {
        let id = self.id();
        let body = response.text();
        let location = response.header("location").map(str::to_string);
        let answer = Answer::new(&body, &response.headers, location.as_deref().unwrap_or(&response.url), file_name);

        if !(200..400).contains(&response.status) {
            let reason = Custom::template(config, "errorTemplate", Place::Response)
                .and_then(|t| t.ok())
                .and_then(|t| t.evaluate(&answer).ok())
                .filter(|r| !r.trim().is_empty());
            return Err(match reason {
                Some(reason) => UploadError::refused(&id, Some(response.status), reason),
                None => super::refusal(&id, response),
            });
        }

        let template = Custom::template(config, "urlTemplate", Place::Response)
            .unwrap_or_else(|| Template::parse("{response}", Place::Response).map_err(|e| e.message))
            .map_err(|m| UploadError::invalid(&id, "urlTemplate", m))?;
        let link = template
            .evaluate(&answer)
            .map_err(|e| UploadError::refused(&id, Some(response.status), format!("the answer has no link: {e}")))?;
        let link = parse_http_url(&link).ok_or_else(|| {
            UploadError::refused(
                &id,
                Some(response.status),
                format!("the link the answer gave is not an http or https address: {}", crate::shorten(&link)),
            )
        })?;
        let deletion = Custom::template(config, "deletionUrlTemplate", Place::Response)
            .and_then(|t| t.ok())
            .and_then(|t| t.evaluate(&answer).ok())
            .and_then(|text| parse_http_url(&text))
            .map(|u| u.to_string());
        Ok((link.to_string(), deletion))
    }
}

impl Provider for Custom {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::CUSTOM)
    }

    fn describe(&self) -> ProviderDescription {
        let methods = ["POST", "PUT", "PATCH"].map(FieldOption::literal).to_vec();
        let bodies = vec![
            FieldOption::new("multipart", "upload.option.bodyMultipart"),
            FieldOption::new("binary", "upload.option.bodyBinary"),
        ];
        let request = "upload.section.request";
        let response = "upload.section.response";
        ProviderDescription {
            id: self.id(),
            name: "Custom HTTP".into(),
            monogram: "{ }".into(),
            shape: FormShape::Custom,
            account: AccountNeed::Custom,
            domain: None,
            operator: None,
            website: None,
            key_url: None,
            facts: None,
            max_file_bytes: None,
            links_expire: false,
            supports_delete: true,
            deletion: DeletionKind::Page,
            testable: true,
            settings: vec![
                SettingField::text("name", "upload.field.name").section(request),
                SettingField::url("requestUrl", "upload.field.requestUrl", "https://share.example.dev/api/upload")
                    .required()
                    .help("upload.help.custom.requestUrl")
                    .section(request),
                SettingField::select("method", "upload.field.method", methods, "POST").section(request),
                SettingField::select("body", "upload.field.body", bodies, "multipart").section(request),
                SettingField::text("fileFormName", "upload.field.fileFormName")
                    .mono()
                    .default_text("file")
                    .help("upload.help.custom.fileFormName")
                    .section(request),
                SettingField::pairs("parameters", "upload.field.parameters", true)
                    .help("upload.help.custom.pairs")
                    .section(request),
                SettingField::pairs("headers", "upload.field.headers", true)
                    .help("upload.help.custom.pairs")
                    .section(request),
                SettingField::pairs("arguments", "upload.field.arguments", true)
                    .help("upload.help.custom.pairs")
                    .section(request),
                SettingField::text("urlTemplate", "upload.field.urlTemplate")
                    .required()
                    .mono()
                    .default_text("{response}")
                    .placeholder("{json:files[0].url}")
                    .help("upload.help.custom.urlTemplate")
                    .section(response),
                SettingField::text("deletionUrlTemplate", "upload.field.deletionUrlTemplate")
                    .mono()
                    .placeholder("{json:deletionUrl}")
                    .help("upload.help.custom.deletionUrlTemplate")
                    .section(response),
                SettingField::text("errorTemplate", "upload.field.errorTemplate")
                    .mono()
                    .placeholder("{json:error.message}")
                    .help("upload.help.custom.errorTemplate")
                    .section(response),
            ],
        }
    }

    fn validate(&self, config: &ProviderConfig) -> Vec<Issue> {
        let mut issues = Vec::new();
        if let Some(url) = config.text("requestUrl") {
            match sxcu::check_request_url(url) {
                Err(message) => issues.push(Issue::error("requestUrl", message)),
                Ok(()) => {
                    let filled = Template::parse(url, Place::Request).map(|t| t.fill("file.png")).unwrap_or_default();
                    if parse_http_url(&filled)
                        .and_then(|u| Origin::of(&u))
                        .is_some_and(|o| o.scheme == "http" && !o.is_local())
                    {
                        issues.push(Issue::warning("requestUrl", "upload.issue.plainHttp"));
                    }
                }
            }
        }
        if let Some(method) = config.text("method") {
            if !["POST", "PUT", "PATCH"].contains(&method) {
                issues.push(Issue::error("method", "upload.sxcu.method"));
            }
        }
        if let Some(body) = config.text("body") {
            if body != "multipart" && body != "binary" {
                issues.push(Issue::error("body", "upload.sxcu.body"));
            }
        }
        // A field never typed in sends as "file", the default the form shows; only one the user
        // emptied is an error.
        let emptied = matches!(config.values.get("fileFormName"), Some(FieldValue::Text(t)) if t.trim().is_empty());
        if config.text("body") != Some("binary") && emptied {
            issues.push(Issue::error("fileFormName", "upload.sxcu.fileFormName"));
        }
        for field in ["urlTemplate", "deletionUrlTemplate", "errorTemplate"] {
            if let Some(text) = config.text(field) {
                if let Err(error) = Template::parse(text, Place::Response) {
                    issues
                        .push(Issue::error(field, error.message).with("function", error.function.unwrap_or_default()));
                } else if field == "urlTemplate" && !RESPONSE_TOKENS.iter().any(|t| text.contains(t)) {
                    // A link that never reads the answer is the same for every upload.
                    issues.push(Issue::warning(field, "upload.issue.staticLink"));
                }
            }
        }
        for field in sxcu::SECRET_PAIR_FIELDS {
            for (name, value) in config.secret_pairs(field) {
                if let Err(error) = Template::parse(value.expose(), Place::Request) {
                    issues.push(Issue::error(field, error.message).with("name", name));
                }
            }
            if field == "headers" {
                for name in config.names(field) {
                    let valid = !name.is_empty()
                        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
                    if !valid {
                        issues.push(Issue::error(field, "upload.issue.headerName").with("name", name));
                    }
                }
            }
        }
        issues
    }

    fn destination(&self, config: &ProviderConfig) -> Result<Destination> {
        let id = self.id();
        let text = config
            .text("requestUrl")
            .ok_or_else(|| UploadError::invalid(&id, "requestUrl", "the request URL is missing"))?;
        sxcu::check_request_url(text).map_err(|m| UploadError::invalid(&id, "requestUrl", m))?;
        let filled = Template::parse(text, Place::Request)
            .map_err(|e| UploadError::invalid(&id, "requestUrl", e.message))?
            .fill("file.png");
        let origin = parse_http_url(&filled).and_then(|u| Origin::of(&u)).ok_or_else(|| {
            UploadError::invalid(&id, "requestUrl", "the request URL is not an http or https address")
        })?;
        Ok(Destination { domain: origin.display(), bucket: None, origins: vec![origin], fixed: false })
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let response = self.send(
            connection,
            &request.config,
            &request.data,
            &request.file_name,
            &request.content_type,
            Some(progress),
        )?;
        let (url, deletion) = self.read_answer(&request.config, &response, &request.file_name)?;
        Ok(UploadResult {
            upload_id: request.id,
            provider: self.id(),
            url,
            deletion: deletion.map(|url| DeletionHandle::Url { url }),
            uploaded_at_ms: crate::now_ms(),
            expires_at_ms: None,
        })
    }

    fn delete(&self, _: &Connection, deletion: &DeletionHandle, _: &ProviderConfig) -> Result<DeleteOutcome> {
        match deletion {
            DeletionHandle::Url { url } => Ok(DeleteOutcome::OpenPage { url: url.clone() }),
            _ => {
                Err(UploadError::invalid(&self.id(), "deletion", "a custom endpoint deletes through its deletion link"))
            }
        }
    }

    fn test(&self, connection: &Connection, config: &ProviderConfig) -> Result<ProviderTestResult> {
        let started = Instant::now();
        let data = UploadData::Bytes(TEST_PNG.into());
        let response = self.send(connection, config, &data, "glimpsfy-test.png", "image/png", None)?;
        let (url, _) = self.read_answer(config, &response, "glimpsfy-test.png")?;
        Ok(ProviderTestResult {
            provider: self.id(),
            checked_at_ms: crate::now_ms(),
            latency_ms: started.elapsed().as_millis() as u64,
            url: Some(url),
            warnings: Vec::new(),
        })
    }
}
