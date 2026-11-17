//! Uploading: the providers that send a capture to a host, the consent that must come first, the
//! secrets they use and the one HTTP client they share.
//!
//! A host is a [`Provider`]: it describes its settings form ([`schema`]), validates a
//! configuration, uploads a file with progress and cancellation, deletes what it uploaded and
//! tests its configuration. [`Uploader`](engine::Uploader) is the only way to run one.
//!
//! # Rules every provider follows
//!
//! - **Consent first.** A provider makes a network request only through a [`http::Connection`],
//!   and only [`engine::Uploader`] makes one, after it checked that the user confirmed this
//!   provider and this destination (`consent`). A connection reaches only the hosts of that
//!   destination; any other host, a redirect included, is refused before a socket opens.
//! - **Nothing in the background.** Uploads, tests and deletions start from a user's action. No
//!   provider is the default until the user picks one, and nothing is retried on its own: an
//!   upload that failed is reported with the host's reason and stays failed.
//! - **Secrets stay secret.** API keys, access keys and header values live in the system keychain
//!   (`secrets`). They reach a provider as [`Secret`] values inside [`ProviderConfig`], are never
//!   serialised, and are cut out of every error message before it leaves the engine.
//! - **Blocking.** Every network method blocks until the host answers, the time-outs of
//!   [`http::HttpClient`] run out or the upload is cancelled. Call them on a worker thread.

pub mod consent;
pub mod engine;
mod hosts;
pub mod http;
pub mod schema;
pub mod secrets;
pub mod sigv4;
pub mod sxcu;
pub mod syntax;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use engine::Uploader;
pub use schema::*;

pub type Result<T, E = UploadError> = std::result::Result<T, E>;

/// Why an upload, a test or a deletion did not happen. A message never holds a secret: the
/// engine cuts every secret of the configuration out of it (`engine::scrub`).
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    #[error("there is no upload provider called {0}")]
    UnknownProvider(ProviderId),
    /// No provider was named and none is the default.
    #[error("no upload provider is chosen yet")]
    NoProviderChosen,
    /// The user has not confirmed uploads to this provider and destination yet, so nothing was
    /// sent. The UI shows the first-upload confirmation and calls `upload_confirm_provider`.
    #[error("uploads to {domain} through {provider} are not confirmed yet")]
    NotConfirmed { provider: ProviderId, domain: String },
    /// A request would have gone to a host the confirmed destination does not include.
    #[error("{provider} tried to reach {host}, which is not the host the user confirmed")]
    HostNotAllowed { provider: ProviderId, host: String },
    /// A setting is missing or has a value the provider cannot use.
    #[error("the setting {field} of {provider} is invalid: {reason}")]
    InvalidConfig { provider: ProviderId, field: String, reason: String },
    /// A secret the provider needs is not in the keychain.
    #[error("{provider} needs the secret {key}, and none is stored")]
    MissingSecret { provider: ProviderId, key: String },
    /// The host answered and refused. `reason` is the host's own explanation, shortened, so the
    /// toast can say why; `status` is the HTTP status when there is one.
    #[error("{provider} refused the request ({}): {reason}", status.map(|s| s.to_string()).unwrap_or_else(|| "no status".into()))]
    Provider { provider: ProviderId, status: Option<u16>, reason: String },
    /// The host could not be reached (no connection, DNS, TLS or a time-out).
    #[error("the host could not be reached: {0}")]
    Network(String),
    #[error("the file is {size} bytes, and {provider} accepts at most {limit}")]
    FileTooLarge { provider: ProviderId, size: u64, limit: u64 },
    #[error("the upload was cancelled")]
    Cancelled,
    /// The system keychain refused to read, write or delete a secret.
    #[error("the keychain failed: {0}")]
    Keychain(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl UploadError {
    pub(crate) fn invalid(provider: &ProviderId, field: &str, reason: impl Into<String>) -> UploadError {
        UploadError::InvalidConfig { provider: provider.clone(), field: field.into(), reason: reason.into() }
    }

    pub(crate) fn refused(provider: &ProviderId, status: Option<u16>, reason: impl Into<String>) -> UploadError {
        UploadError::Provider { provider: provider.clone(), status, reason: shorten(&reason.into()) }
    }
}

/// Keeps the first 300 characters of a host's answer on one line, so a toast can quote it.
pub(crate) fn shorten(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(300) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// Identifies a provider in the settings, in the history and in the keychain's service
/// names. The built-in ids are in [`providers`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        ProviderId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies one upload. The command that starts the upload returns it, and every progress event
/// carries it, so the UI can cancel that upload.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UploadId(pub String);

impl fmt::Display for UploadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ids of the built-in providers. `docs/PROVIDERS.md` says why each public host is in the
/// list, and why Imgur is not.
pub mod providers {
    /// Catbox keeps anonymous uploads until two years pass without a visit.
    pub const CATBOX: &str = "catbox";
    /// Litterbox, run by Catbox, keeps uploads for 1, 12, 24 or 72 hours.
    pub const LITTERBOX: &str = "litterbox";
    /// 0x0.st keeps a file for 30 days to a year, depending on its size.
    pub const ZERO_X_ZERO: &str = "0x0";
    /// ImgBB works with the user's own API key.
    pub const IMGBB: &str = "imgbb";
    /// Any S3-compatible bucket works: AWS S3, Cloudflare R2, Backblaze B2 or MinIO.
    pub const S3: &str = "s3";
    /// A custom HTTP endpoint is configured by hand or read from a ShareX `.sxcu` file.
    pub const CUSTOM: &str = "custom";

    /// In the order of the host list in Settings › Upload.
    pub const BUILTIN: [&str; 6] = [LITTERBOX, CATBOX, ZERO_X_ZERO, IMGBB, S3, CUSTOM];
}

/// Returns the built-in providers, in the order of the host list.
pub fn builtin_providers() -> Vec<Arc<dyn Provider>> {
    hosts::builtin()
}

/// The built-in providers with every public host moved to `base`, for tests against a local
/// server; consent and the origin check apply to them as to the real ones.
#[doc(hidden)]
pub use hosts::builtin_at as builtin_providers_at;

/// Holds a secret read from the keychain. It is never serialised, and its `Debug` output hides it,
/// so it cannot leak into a log, the settings file or the history by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The secret's text, for the one request that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

/// A provider's configuration for one call: the values from the settings file and the secrets
/// from the keychain. It is built for the call and dropped after it.
#[derive(Clone, Debug, Default)]
pub struct ProviderConfig {
    pub values: BTreeMap<String, FieldValue>,
    pub secrets: BTreeMap<String, Secret>,
}

impl ProviderConfig {
    pub fn value(&self, key: &str) -> Option<&FieldValue> {
        self.values.get(key)
    }

    /// The trimmed text of a row, or `None` when it is empty or not text.
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(FieldValue::Text(text)) => Some(text.trim()).filter(|t| !t.is_empty()),
            _ => None,
        }
    }

    pub fn toggle(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some(FieldValue::Toggle(on)) => Some(*on),
            _ => None,
        }
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        match self.values.get(key) {
            Some(FieldValue::Number(n)) => Some(*n),
            Some(FieldValue::Text(text)) => text.trim().parse().ok(),
            _ => None,
        }
    }

    /// The names of a `pairs` row with secret values, in their order.
    pub fn names(&self, key: &str) -> Vec<String> {
        match self.values.get(key) {
            Some(FieldValue::Names(names)) => names.clone(),
            Some(FieldValue::Pairs(pairs)) => pairs.iter().map(|p| p.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// The name and value pairs of a `pairs` row whose values are secrets, read from the
    /// keychain. A name without a stored value is left out.
    pub fn secret_pairs(&self, field: &str) -> Vec<(String, Secret)> {
        self.names(field)
            .into_iter()
            .filter_map(|name| {
                let secret = self.secrets.get(&pair_secret_key(field, &name))?.clone();
                Some((name, secret))
            })
            .collect()
    }

    /// Returns the secret stored under `key`, or [`UploadError::MissingSecret`] when none is stored.
    pub fn secret(&self, provider: &ProviderId, key: &str) -> Result<&Secret> {
        self.secrets
            .get(key)
            .ok_or_else(|| UploadError::MissingSecret { provider: provider.clone(), key: key.to_owned() })
    }
}

/// Stops an upload from another thread. The upload's body checks it before each chunk it hands to
/// the socket and stops as soon as it is set.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The bytes to upload: an encoded capture in memory, or a file on disk such as a recording.
#[derive(Clone, Debug)]
pub enum UploadData {
    Bytes(Arc<[u8]>),
    File(PathBuf),
}

impl UploadData {
    /// The number of bytes the upload sends.
    pub fn len(&self) -> std::io::Result<u64> {
        match self {
            UploadData::Bytes(bytes) => Ok(bytes.len() as u64),
            UploadData::File(path) => Ok(std::fs::metadata(path)?.len()),
        }
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// A fresh reader from the first byte.
    pub fn open(&self) -> std::io::Result<Box<dyn Read + Send>> {
        match self {
            UploadData::Bytes(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
            UploadData::File(path) => Ok(Box::new(File::open(path)?)),
        }
    }
}

/// One upload as a provider receives it.
#[derive(Clone, Debug)]
pub struct UploadRequest {
    pub id: UploadId,
    pub data: UploadData,
    /// The file name the host shows and keeps, already made from the user's template.
    pub file_name: String,
    /// The MIME type, such as `image/png` or `video/mp4`.
    pub content_type: String,
    pub config: ProviderConfig,
    pub cancel: CancelToken,
}

/// How far an upload has come. It is also the payload of the `upload://progress` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgress {
    pub upload_id: UploadId,
    pub provider: ProviderId,
    pub sent_bytes: u64,
    pub total_bytes: u64,
}

/// What deleting an upload later needs, as the host hands it out. The history keeps it as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DeletionHandle {
    /// A page that deletes the file when the user opens it, as ImgBB and custom endpoints return.
    /// Glimpsfy opens it in the browser and never requests it itself, because it may be on any
    /// host.
    Url { url: String },
    /// A token the provider sends back to delete the file, such as 0x0.st's `X-Token`, with the
    /// file's own URL when the deletion request goes there.
    Token {
        token: String,
        #[serde(default)]
        url: Option<String>,
    },
    /// An object in an S3-compatible bucket. The endpoint and the bucket are kept, so a deletion
    /// never reaches a bucket the settings point to later.
    ObjectKey {
        key: String,
        #[serde(default)]
        endpoint: String,
        #[serde(default)]
        bucket: String,
    },
}

/// A finished upload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub upload_id: UploadId,
    pub provider: ProviderId,
    /// The public link, which goes to the clipboard as Settings › Upload › After upload says.
    pub url: String,
    pub deletion: Option<DeletionHandle>,
    pub uploaded_at_ms: i64,
    /// When the host removes the file on its own, if it says so.
    pub expires_at_ms: Option<i64>,
}

/// What a deletion did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DeleteOutcome {
    /// The host confirmed the deletion.
    Deleted,
    /// The host's deletion page has to be opened in the browser, where the user finishes it.
    OpenPage { url: String },
}

/// A successful check of a provider's configuration ("Test connection" in settings).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResult {
    pub provider: ProviderId,
    pub checked_at_ms: i64,
    /// How long the host took to answer.
    pub latency_ms: u64,
    /// The link of the test file, when the test had to upload one.
    pub url: Option<String>,
    /// What the user should know although the test passed, such as a test file the bucket kept.
    pub warnings: Vec<Issue>,
}

/// Where a configuration sends its requests. The first-upload confirmation names `domain`, the
/// consent recorded for it covers exactly this destination, and a connection reaches only
/// `origins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// The host's domain as the user reads it (`litterbox.catbox.moe`, `minio.local:9000`).
    pub domain: String,
    /// The bucket, for an S3-compatible destination.
    pub bucket: Option<String>,
    /// The scheme, host and port every request goes to.
    pub origins: Vec<http::Origin>,
    /// Whether the address is the provider's own and cannot be changed in the settings. Consent
    /// to a fixed destination is recorded under the provider's id alone; consent to a configured
    /// one names the destination, so a new endpoint asks again.
    pub fixed: bool,
}

impl Destination {
    /// Whether a request would travel unencrypted to another machine.
    pub fn plain_http(&self) -> bool {
        self.origins.iter().any(|origin| origin.scheme == "http" && !origin.is_local())
    }
}

/// Uploads to one host; every provider implements this trait.
///
/// The network methods take a [`http::Connection`], which only [`engine::Uploader`] can open,
/// after the consent check. So no code outside this crate can make a provider send anything.
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// The provider's name, facts, account needs and settings form.
    fn describe(&self) -> ProviderDescription;

    /// The problems of a configuration, without touching the network. An upload refuses to start
    /// while any of them is an error.
    fn validate(&self, config: &ProviderConfig) -> Vec<Issue>;

    /// Where this configuration sends its requests.
    fn destination(&self, config: &ProviderConfig) -> Result<Destination>;

    /// How many hours the host keeps an upload under this configuration, for a host that lets
    /// the user choose (Litterbox's period, ImgBB's expiration).
    fn chosen_hours(&self, _config: &ProviderConfig) -> Option<u32> {
        None
    }

    /// Uploads the request's data and returns the link. `progress` is called on the calling
    /// thread as bytes are sent. Blocks until the host answers or the cancel token is set.
    fn upload(
        &self,
        connection: &http::Connection,
        request: UploadRequest,
        progress: &dyn Fn(u64),
    ) -> Result<UploadResult>;

    /// Deletes an earlier upload from the host. Blocks until the host answers.
    fn delete(
        &self,
        connection: &http::Connection,
        deletion: &DeletionHandle,
        config: &ProviderConfig,
    ) -> Result<DeleteOutcome>;

    /// Checks the configuration with the smallest request the host allows, and uploads nothing
    /// when the host offers another way to check. Blocks until the host answers.
    fn test(&self, connection: &http::Connection, config: &ProviderConfig) -> Result<ProviderTestResult>;
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
