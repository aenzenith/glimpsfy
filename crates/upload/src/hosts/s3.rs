//! S3-compatible buckets (AWS S3, Cloudflare R2, Backblaze B2, MinIO), signed with SigV4.
//!
//! Every object key gets a random folder of its own (`<prefix><random>/<file name>`), because a
//! PUT replaces an object of the same key without a word, and two captures named in the same
//! second must not overwrite each other. The body's SHA-256 is computed before the upload and
//! signed, which every S3-compatible service accepts, over `http` as well as `https`.

use std::io::Read;
use std::time::Instant;

use ureq::http::Method;
use url::Url;

use crate::http::{parse_http_url, Body, Connection, Origin, Request, Response};
use crate::sigv4::{self, SignableRequest, SigningParams};
use crate::{
    AccountNeed, CancelToken, DeleteOutcome, DeletionHandle, DeletionKind, Destination, FieldOption, FieldRule,
    FormShape, Issue, Provider, ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result,
    SettingField, UploadData, UploadError, UploadRequest, UploadResult,
};

pub(crate) struct S3;

/// A resolved bucket configuration.
struct Bucket {
    /// The endpoint as the settings hold it, trimmed, which a deletion handle remembers.
    endpoint_text: String,
    endpoint: Url,
    region: String,
    bucket: String,
    virtual_host: bool,
}

impl Bucket {
    fn read(provider: &ProviderId, config: &ProviderConfig) -> Result<Bucket> {
        let endpoint_text = config.text("endpoint").unwrap_or("").trim_end_matches('/').to_string();
        let endpoint = parse_http_url(&endpoint_text).filter(|u| u.query().is_none()).ok_or_else(|| {
            UploadError::invalid(provider, "endpoint", "the endpoint is not an http or https address")
        })?;
        let bucket = config.text("bucket").unwrap_or("").to_string();
        if bucket.is_empty() || bucket.contains(['/', ' ', '\t', '\n']) {
            return Err(UploadError::invalid(
                provider,
                "bucket",
                "the bucket name is empty or holds a slash or a space",
            ));
        }
        let region = config.text("region").unwrap_or("us-east-1").to_string();
        let host = endpoint.host_str().unwrap_or("").to_ascii_lowercase();
        let virtual_host = match config.text("addressing").unwrap_or("auto") {
            "virtualHost" => true,
            "path" => false,
            _ => host.ends_with(".amazonaws.com") && !bucket.contains('.'),
        };
        Ok(Bucket { endpoint_text, endpoint, region, bucket, virtual_host })
    }

    /// The URL of an object, and the path that is signed (not yet percent-encoded).
    fn object(&self, key: &str) -> Result<(Url, String)> {
        let base_path = self.endpoint.path().trim_end_matches('/');
        let (mut url, path) = if self.virtual_host {
            let mut url = self.endpoint.clone();
            let host = format!("{}.{}", self.bucket, self.endpoint.host_str().unwrap_or(""));
            url.set_host(Some(&host))
                .map_err(|_| UploadError::Network("the bucket's address could not be formed".into()))?;
            (url, format!("{base_path}/{key}"))
        } else {
            (self.endpoint.clone(), format!("{base_path}/{}/{key}", self.bucket))
        };
        url.set_path(&sigv4::encode_path(&path));
        Ok((url, path))
    }

    fn origin(&self) -> Result<Origin> {
        let (url, _) = self.object("")?;
        Origin::of(&url).ok_or_else(|| UploadError::Network("the bucket's address is not http or https".into()))
    }
}

/// Sends one signed request for an object.
#[allow(clippy::too_many_arguments)]
fn signed(
    connection: &Connection,
    config: &ProviderConfig,
    bucket: &Bucket,
    method: Method,
    key: &str,
    payload_hash: &str,
    extra: Vec<(String, String)>,
    body: Body<'_>,
) -> Result<Response> {
    let provider = connection.provider().clone();
    let access_key_id = config
        .text("accessKeyId")
        .ok_or_else(|| UploadError::invalid(&provider, "accessKeyId", "the access key ID is missing"))?;
    let secret = config.secret(&provider, "secretAccessKey")?;
    let (url, path) = bucket.object(key)?;
    let host = bucket.origin()?.display();
    let time = chrono::Utc::now();
    let amz_date = time.format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers = vec![
        ("host".to_string(), host),
        ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
        ("x-amz-date".to_string(), amz_date),
    ];
    let signature = sigv4::sign(
        &SignableRequest { method: method.as_str(), path: &path, query: &[], headers: &headers, payload_hash },
        &SigningParams {
            access_key_id,
            secret_access_key: secret.expose(),
            region: &bucket.region,
            service: "s3",
            time,
            normalize_path: false,
        },
    );
    // The client sets `Host` from the URL itself, to the value that was signed.
    headers.remove(0);
    let mut request = Request::new(method, url).header("authorization", signature.authorization);
    for (name, value) in headers.into_iter().chain(extra) {
        request = request.header(&name, value);
    }
    connection.send(request.body(body))
}

/// The host's code and message from an S3 error document (`<Code>AccessDenied</Code>`).
fn refusal(provider: &ProviderId, response: &Response) -> UploadError {
    let text = response.text();
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&format!("</{name}>"))? + start;
        Some(text[start..end].trim().to_string())
    };
    match (tag("Code"), tag("Message")) {
        (Some(code), Some(message)) => {
            UploadError::refused(provider, Some(response.status), format!("{code}: {message}"))
        }
        (Some(code), None) => UploadError::refused(provider, Some(response.status), code),
        _ => super::refusal(provider, response),
    }
}

/// The hex SHA-256 of the data, read once before the upload.
fn payload_hash(data: &UploadData, cancel: &CancelToken) -> Result<String> {
    if let UploadData::Bytes(bytes) = data {
        return Ok(sigv4::sha256_hex(bytes));
    }
    let mut reader = data.open()?;
    let mut hash = sigv4::Sha256Stream::default();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finish_hex())
}

/// Fills the path prefix's date tokens.
fn prefix(config: &ProviderConfig) -> String {
    let now = chrono::Local::now();
    let text = config.text("pathPrefix").unwrap_or("");
    let text = text
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{year}", &now.format("%Y").to_string())
        .replace("{month}", &now.format("%m").to_string())
        .replace("{day}", &now.format("%d").to_string());
    let text = text.trim_start_matches('/').to_string();
    if text.is_empty() || text.ends_with('/') {
        text
    } else {
        format!("{text}/")
    }
}

/// The public link of an object: the template of the settings, or the object's own address.
fn public_url(provider: &ProviderId, config: &ProviderConfig, bucket: &Bucket, key: &str) -> Result<String> {
    match config.text("publicUrl") {
        Some(template) => {
            let filled = template.replace("{key}", &sigv4::encode_path(key)).replace("{bucket}", &bucket.bucket);
            parse_http_url(&filled).map(|u| u.to_string()).ok_or_else(|| {
                UploadError::invalid(provider, "publicUrl", "the public URL is not an http or https address")
            })
        }
        None => Ok(bucket.object(key)?.0.to_string()),
    }
}

impl Provider for S3 {
    fn id(&self) -> ProviderId {
        ProviderId::new(crate::providers::S3)
    }

    fn describe(&self) -> ProviderDescription {
        let addressing = vec![
            FieldOption::new("auto", "upload.option.addressingAuto"),
            FieldOption::new("path", "upload.option.addressingPath"),
            FieldOption::new("virtualHost", "upload.option.addressingVirtual"),
        ];
        ProviderDescription {
            id: self.id(),
            name: "S3".into(),
            monogram: "S3".into(),
            shape: FormShape::Bucket,
            account: AccountNeed::AccessKeys,
            domain: None,
            operator: None,
            website: None,
            key_url: None,
            facts: None,
            max_file_bytes: None,
            links_expire: false,
            supports_delete: true,
            deletion: DeletionKind::Request,
            testable: true,
            settings: vec![
                SettingField::url("endpoint", "upload.field.endpoint", "https://s3.eu-central-1.amazonaws.com")
                    .required()
                    .help("upload.help.s3.endpoint")
                    .section("upload.section.bucket"),
                SettingField::text("region", "upload.field.region")
                    .required()
                    .mono()
                    .placeholder("eu-central-1")
                    .rule(FieldRule::NoWhitespace)
                    .help("upload.help.s3.region")
                    .section("upload.section.bucket"),
                SettingField::text("bucket", "upload.field.bucket")
                    .required()
                    .mono()
                    .placeholder("team-captures")
                    .rule(FieldRule::NoWhitespace)
                    .rule(FieldRule::MaxLength { max: 255 })
                    .section("upload.section.bucket"),
                SettingField::select("addressing", "upload.field.addressing", addressing, "auto")
                    .help("upload.help.s3.addressing")
                    .section("upload.section.bucket"),
                SettingField::text("accessKeyId", "upload.field.accessKeyId")
                    .required()
                    .mono()
                    .rule(FieldRule::NoWhitespace)
                    .section("upload.section.credentials"),
                SettingField::secret("secretAccessKey", "upload.field.secretAccessKey")
                    .required()
                    .placeholder("upload.placeholder.secret")
                    .help("upload.help.keychain")
                    .section("upload.section.credentials"),
                SettingField::text("pathPrefix", "upload.field.pathPrefix")
                    .mono()
                    .placeholder("glimpsfy/{date}/")
                    .tokens(&["date", "year", "month", "day"])
                    .help("upload.help.s3.pathPrefix")
                    .section("upload.section.links"),
                SettingField::text("publicUrl", "upload.field.publicUrl")
                    .mono()
                    .placeholder("https://captures.example.dev/{key}")
                    .tokens(&["key", "bucket"])
                    .help("upload.help.s3.publicUrl")
                    .section("upload.section.links"),
                SettingField::text("cacheControl", "upload.field.cacheControl")
                    .mono()
                    .placeholder("public, max-age=31536000, immutable")
                    .help("upload.help.s3.cacheControl")
                    .section("upload.section.links"),
            ],
        }
    }

    fn validate(&self, config: &ProviderConfig) -> Vec<Issue> {
        let mut issues = Vec::new();
        if let Some(text) = config.text("endpoint") {
            match parse_http_url(text) {
                None => issues.push(Issue::error("endpoint", "upload.issue.httpOnly")),
                Some(url) if url.query().is_some() => {
                    issues.push(Issue::error("endpoint", "upload.issue.endpointQuery"))
                }
                Some(url) => {
                    if Origin::of(&url).is_some_and(|o| o.scheme == "http" && !o.is_local()) {
                        issues.push(Issue::warning("endpoint", "upload.issue.plainHttp"));
                    }
                }
            }
        }
        if let Some(bucket) = config.text("bucket") {
            if bucket.contains(['/', ' ', '\t']) {
                issues.push(Issue::error("bucket", "upload.issue.bucketName"));
            } else if bucket.contains('.')
                && config.text("addressing") == Some("virtualHost")
                && config.text("endpoint").is_some_and(|e| e.starts_with("https"))
            {
                issues.push(Issue::warning("bucket", "upload.issue.dottedBucket"));
            }
        }
        for key in ["region", "accessKeyId"] {
            if config.text(key).is_some_and(|t| t.chars().any(char::is_whitespace)) {
                issues.push(Issue::error(key, "upload.rule.noWhitespace"));
            }
        }
        if let Some(template) = config.text("publicUrl") {
            let filled = template.replace("{key}", "a").replace("{bucket}", "b");
            if parse_http_url(&filled).is_none() {
                issues.push(Issue::error("publicUrl", "upload.issue.httpOnly"));
            }
        }
        if config.text("cacheControl").is_some_and(|t| t.contains(['\r', '\n'])) {
            issues.push(Issue::error("cacheControl", "upload.issue.oneLine"));
        }
        issues
    }

    fn destination(&self, config: &ProviderConfig) -> Result<Destination> {
        let bucket = Bucket::read(&self.id(), config)?;
        let origin = bucket.origin()?;
        Ok(Destination {
            domain: origin.display(),
            bucket: Some(bucket.bucket.clone()),
            origins: vec![origin],
            fixed: false,
        })
    }

    fn upload(&self, connection: &Connection, request: UploadRequest, progress: &dyn Fn(u64)) -> Result<UploadResult> {
        let id = self.id();
        let bucket = Bucket::read(&id, &request.config)?;
        let key = format!("{}{}/{}", prefix(&request.config), crate::engine::random_token(10), request.file_name);
        let hash = payload_hash(&request.data, &request.cancel)?;
        let length = request.data.len()?;
        let mut extra = vec![("content-type".to_string(), request.content_type.clone())];
        if let Some(cache) = request.config.text("cacheControl") {
            extra.push(("cache-control".to_string(), cache.to_string()));
        }
        let reader = request.data.open()?;
        let response = signed(
            connection,
            &request.config,
            &bucket,
            Method::PUT,
            &key,
            &hash,
            extra,
            Body::Stream { reader: Box::new(reader.take(length)), length, progress: Some(progress) },
        )?;
        if !response.is_success() {
            return Err(refusal(&id, &response));
        }
        Ok(UploadResult {
            upload_id: request.id,
            provider: id.clone(),
            url: public_url(&id, &request.config, &bucket, &key)?,
            deletion: Some(DeletionHandle::ObjectKey {
                key,
                endpoint: bucket.endpoint_text.clone(),
                bucket: bucket.bucket.clone(),
            }),
            uploaded_at_ms: crate::now_ms(),
            expires_at_ms: None,
        })
    }

    fn delete(
        &self,
        connection: &Connection,
        deletion: &DeletionHandle,
        config: &ProviderConfig,
    ) -> Result<DeleteOutcome> {
        let id = self.id();
        let DeletionHandle::ObjectKey { key, endpoint, bucket: bucket_name } = deletion else {
            return Err(UploadError::invalid(&id, "deletion", "an S3 upload is deleted by its object key"));
        };
        let bucket = Bucket::read(&id, config)?;
        // The handle names the bucket the object went to; a key of the same name in a bucket the
        // settings point to now is somebody else's file.
        if endpoint.trim_end_matches('/') != bucket.endpoint_text || *bucket_name != bucket.bucket {
            return Err(UploadError::invalid(
                &id,
                "bucket",
                "the endpoint or the bucket changed since this upload, so delete it in the bucket's own console",
            ));
        }
        let response =
            signed(connection, config, &bucket, Method::DELETE, key, sigv4::EMPTY_SHA256, vec![], Body::Empty)?;
        if response.is_success() {
            Ok(DeleteOutcome::Deleted)
        } else {
            Err(refusal(&id, &response))
        }
    }

    fn test(&self, connection: &Connection, config: &ProviderConfig) -> Result<ProviderTestResult> {
        let id = self.id();
        let bucket = Bucket::read(&id, config)?;
        let key = format!("{}glimpsfy-connection-test-{}.txt", prefix(config), crate::engine::random_token(8));
        let body = b"Glimpsfy checked that it can write to this bucket.".to_vec();
        let started = Instant::now();
        let response = signed(
            connection,
            config,
            &bucket,
            Method::PUT,
            &key,
            &sigv4::sha256_hex(&body),
            vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            Body::Bytes(body),
        )?;
        if !response.is_success() {
            return Err(refusal(&id, &response));
        }
        let latency_ms = started.elapsed().as_millis() as u64;
        let mut warnings = Vec::new();
        let deleted =
            signed(connection, config, &bucket, Method::DELETE, &key, sigv4::EMPTY_SHA256, vec![], Body::Empty);
        if !deleted.as_ref().is_ok_and(Response::is_success) {
            warnings.push(Issue::warning("secretAccessKey", "upload.issue.testFileKept").with("key", key));
        }
        Ok(ProviderTestResult { provider: id, checked_at_ms: crate::now_ms(), latency_ms, url: None, warnings })
    }
}
