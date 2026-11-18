//! The engine: the only code that runs a provider's network methods, after the checks that must
//! come first.
//!
//! Every upload, test and deletion goes through [`Uploader`], in this order: the provider's
//! configuration is built from the settings and the keychain, its destination is worked out, the
//! consent recorded in the settings must cover that destination, the configuration must have no
//! errors, and only then is a [`Connection`] opened, bound to the destination's origins. An error
//! leaves the engine with every secret of the configuration cut out of its text.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::consent::{self, ConsentInfo, RetentionNow};
use crate::http::{Connection, HttpClient};
use crate::secrets::SecretStore;
use crate::sxcu;
use crate::{
    pair_secret_key, CancelToken, DeleteOutcome, DeletionHandle, Destination, FieldInput, FieldValue, Issue, Provider,
    ProviderConfig, ProviderDescription, ProviderId, ProviderTestResult, Result, Secret, UploadData, UploadError,
    UploadId, UploadProgress, UploadRequest, UploadResult, Visibility,
};

/// What the engine reads from the settings for one call: the provider's own values
/// (`settings.upload.providers[<id>]`) and the consent keys (`settings.upload.confirmedProviders`).
#[derive(Clone, Copy, Debug)]
pub struct Stored<'a> {
    pub values: Option<&'a Value>,
    pub confirmed: &'a [String],
}

/// Runs the providers.
pub struct Uploader {
    providers: Vec<Arc<dyn Provider>>,
    secrets: SecretStore,
    client: HttpClient,
    running: Mutex<HashMap<UploadId, CancelToken>>,
}

/// An upload that passed every check and waits for its worker thread.
pub struct PendingUpload {
    pub id: UploadId,
    pub provider: ProviderId,
    pub total_bytes: u64,
    pub file_name: String,
    destination: Destination,
    request: UploadRequest,
    implementation: Arc<dyn Provider>,
}

impl std::fmt::Debug for PendingUpload {
    /// Names the upload only; the request carries the configuration and its secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingUpload")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}

/// What an imported `.sxcu` file gave the custom provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SxcuImported {
    /// The provider's new values, to save as `settings.upload.providers.custom`.
    pub values: BTreeMap<String, FieldValue>,
    /// The keychain keys that now hold the file's header, parameter and argument values.
    pub secret_keys: Vec<String>,
    /// What the user should know, such as a plain `http` address.
    pub warnings: Vec<Issue>,
}

impl Uploader {
    pub fn new(providers: Vec<Arc<dyn Provider>>, secrets: SecretStore, client: HttpClient) -> Uploader {
        Uploader { providers, secrets, client, running: Mutex::new(HashMap::new()) }
    }

    pub fn describe_all(&self) -> Vec<ProviderDescription> {
        self.providers.iter().map(|p| p.describe()).collect()
    }

    pub fn provider(&self, id: &ProviderId) -> Result<Arc<dyn Provider>> {
        self.providers.iter().find(|p| p.id() == *id).cloned().ok_or_else(|| UploadError::UnknownProvider(id.clone()))
    }

    // ------------------------------------------------------------------------------- config

    /// The configuration of one call: the non-secret values from the settings and the secrets
    /// from the keychain. A value the settings hold under a secret row's key is ignored, so a
    /// secret that slipped into the settings file is never used.
    pub fn config(&self, provider: &dyn Provider, values: Option<&Value>) -> Result<ProviderConfig> {
        let id = provider.id();
        let description = provider.describe();
        let stored: BTreeMap<String, FieldValue> = match values {
            Some(Value::Object(map)) => map
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), serde_json::from_value::<FieldValue>(v.clone()).ok()?)))
                .collect(),
            _ => BTreeMap::new(),
        };
        let mut config = ProviderConfig::default();
        for field in &description.settings {
            match &field.input {
                FieldInput::Secret { .. } => {
                    if let Some(secret) = self.secrets.get(&id, &field.key)? {
                        config.secrets.insert(field.key.clone(), secret);
                    }
                }
                FieldInput::Pairs { secret_values: true, .. } => {
                    let Some(value) = stored.get(&field.key) else { continue };
                    let names = match value {
                        FieldValue::Names(names) => names.clone(),
                        FieldValue::Pairs(pairs) => pairs.iter().map(|p| p.name.clone()).collect(),
                        _ => continue,
                    };
                    for name in &names {
                        let key = pair_secret_key(&field.key, name);
                        if let Some(secret) = self.secrets.get(&id, &key)? {
                            config.secrets.insert(key, secret);
                        }
                    }
                    config.values.insert(field.key.clone(), FieldValue::Names(names));
                }
                _ => {
                    if let Some(value) = stored.get(&field.key) {
                        config.values.insert(field.key.clone(), value.clone());
                    }
                }
            }
        }
        Ok(config)
    }

    /// The problems of the stored configuration, the missing secrets included.
    pub fn validate(&self, id: &ProviderId, values: Option<&Value>) -> Result<Vec<Issue>> {
        let provider = self.provider(id)?;
        let config = self.config(provider.as_ref(), values)?;
        Ok(full_validation(provider.as_ref(), &config))
    }

    // ------------------------------------------------------------------------------- consent

    /// What the first-upload dialog shows for the provider's current destination.
    pub fn consent_info(&self, id: &ProviderId, stored: Stored) -> Result<ConsentInfo> {
        let provider = self.provider(id)?;
        let config = self.config(provider.as_ref(), stored.values)?;
        let destination = provider.destination(&config).map_err(|e| scrub(e, &config))?;
        let description = provider.describe();
        let facts = description.facts.as_ref();
        let retention = match facts {
            Some(facts) => RetentionNow::from(&facts.retention, provider.chosen_hours(&config)),
            None => RetentionNow::OwnStorage,
        };
        let visibility = match (facts, description.shape) {
            (Some(facts), _) => facts.visibility,
            (None, crate::FormShape::Bucket) => Visibility::BucketPolicy,
            (None, _) => Visibility::ServerOwner,
        };
        Ok(ConsentInfo {
            provider: id.clone(),
            name: description.name.clone(),
            domain: destination.domain.clone(),
            bucket: destination.bucket.clone(),
            operator: description.operator.clone(),
            visibility,
            retention,
            checked_on: facts.map(|f| f.checked_on.clone()),
            terms_url: facts.and_then(|f| f.terms_url.clone()),
            max_file_bytes: description.max_file_bytes,
            plain_http: destination.plain_http(),
            confirmed: consent::is_confirmed(stored.confirmed, id, &destination),
            consent_key: consent::consent_key(id, &destination),
        })
    }

    /// The consent key to record when the user confirms the provider's current destination.
    pub fn consent_key(&self, id: &ProviderId, values: Option<&Value>) -> Result<String> {
        let provider = self.provider(id)?;
        let config = self.config(provider.as_ref(), values)?;
        let destination = provider.destination(&config).map_err(|e| scrub(e, &config))?;
        Ok(consent::consent_key(id, &destination))
    }

    /// The checks every network call passes: a destination, consent to it, and no error in the
    /// configuration. Returns what the call needs to open its connection.
    fn admit(&self, id: &ProviderId, stored: Stored) -> Result<(Arc<dyn Provider>, ProviderConfig, Destination)> {
        let provider = self.provider(id)?;
        let config = self.config(provider.as_ref(), stored.values)?;
        let destination = provider.destination(&config).map_err(|e| scrub(e, &config))?;
        if !consent::is_confirmed(stored.confirmed, id, &destination) {
            return Err(UploadError::NotConfirmed { provider: id.clone(), domain: destination.domain.clone() });
        }
        if let Some(issue) = full_validation(provider.as_ref(), &config).into_iter().find(Issue::is_error) {
            let field = issue.field.clone().unwrap_or_default();
            let secret_row = provider.describe().settings.iter().any(|f| f.key == field && f.secret);
            return Err(if secret_row && issue.message == "upload.issue.required" {
                UploadError::MissingSecret { provider: id.clone(), key: field }
            } else {
                UploadError::invalid(id, &field, issue.message)
            });
        }
        Ok((provider, config, destination))
    }

    // ------------------------------------------------------------------------------- uploads

    /// Checks everything an upload needs and registers it, without sending anything. The id it
    /// returns is the one `cancel` takes; the worker thread then calls [`Uploader::run`].
    pub fn begin(
        &self,
        id: &ProviderId,
        stored: Stored,
        data: UploadData,
        file_name: &str,
        content_type: &str,
    ) -> Result<PendingUpload> {
        let (provider, config, destination) = self.admit(id, stored)?;
        let total_bytes = data.len()?;
        if let Some(limit) = provider.describe().max_file_bytes {
            if total_bytes > limit {
                return Err(UploadError::FileTooLarge { provider: id.clone(), size: total_bytes, limit });
            }
        }
        let upload_id = UploadId(format!("u{}", random_token(12)));
        let cancel = CancelToken::new();
        self.running_uploads().insert(upload_id.clone(), cancel.clone());
        Ok(PendingUpload {
            id: upload_id.clone(),
            provider: id.clone(),
            total_bytes,
            file_name: file_name.to_string(),
            destination,
            request: UploadRequest {
                id: upload_id,
                data,
                file_name: file_name.to_string(),
                content_type: content_type.to_string(),
                config,
                cancel,
            },
            implementation: provider,
        })
    }

    /// Sends a pending upload and returns its link. `progress` hears the bytes sent so far.
    pub fn run(&self, pending: PendingUpload, progress: &dyn Fn(UploadProgress)) -> Result<UploadResult> {
        let PendingUpload { id, provider, total_bytes, destination, request, implementation, .. } = pending;
        let config = request.config.clone();
        let connection = Connection::open(&self.client, provider.clone(), destination.origins, request.cancel.clone());
        let report = |sent: u64| {
            progress(UploadProgress {
                upload_id: id.clone(),
                provider: provider.clone(),
                sent_bytes: sent.min(total_bytes),
                total_bytes,
            })
        };
        let result = implementation.upload(&connection, request, &report);
        self.running_uploads().remove(&id);
        result.map_err(|e| scrub(e, &config))
    }

    /// Cancels a running upload. Returns whether an upload with this id was running.
    pub fn cancel(&self, id: &UploadId) -> bool {
        match self.running_uploads().get(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Forgets an upload that never reached `run` (its preparation failed after `begin`).
    pub fn abandon(&self, id: &UploadId) {
        self.running_uploads().remove(id);
    }

    fn running_uploads(&self) -> std::sync::MutexGuard<'_, HashMap<UploadId, CancelToken>> {
        self.running.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---------------------------------------------------------------------- test and delete

    /// Checks the configuration against the host.
    pub fn test(&self, id: &ProviderId, stored: Stored) -> Result<ProviderTestResult> {
        let (provider, config, destination) = self.admit(id, stored)?;
        let connection = Connection::open(&self.client, id.clone(), destination.origins, CancelToken::new());
        provider.test(&connection, &config).map_err(|e| scrub(e, &config))
    }

    /// Deletes an upload from its host. A deletion page is never requested here: its URL comes
    /// back as [`DeleteOutcome::OpenPage`], for the browser.
    pub fn delete(&self, id: &ProviderId, stored: Stored, deletion: &DeletionHandle) -> Result<DeleteOutcome> {
        if let DeletionHandle::Url { url } = deletion {
            return match crate::http::parse_http_url(url) {
                Some(url) => Ok(DeleteOutcome::OpenPage { url: url.to_string() }),
                None => Err(UploadError::invalid(id, "deletion", "the deletion link is not an http or https address")),
            };
        }
        let (provider, config, destination) = self.admit(id, stored)?;
        let connection = Connection::open(&self.client, id.clone(), destination.origins, CancelToken::new());
        provider.delete(&connection, deletion, &config).map_err(|e| scrub(e, &config))
    }

    // ------------------------------------------------------------------------------- secrets

    /// Stores a provider's secret. `key` must name a secret row of the provider, or one value of a
    /// secret `pairs` row (`headers:Authorization`).
    pub fn set_secret(&self, id: &ProviderId, key: &str, value: &str) -> Result<()> {
        self.check_secret_key(id, key)?;
        let value = value.trim();
        if value.is_empty() {
            return Err(UploadError::invalid(id, key, "the value is empty"));
        }
        self.secrets.set(id, key, value)
    }

    pub fn remove_secret(&self, id: &ProviderId, key: &str) -> Result<()> {
        self.check_secret_key(id, key)?;
        self.secrets.remove(id, key)
    }

    /// Whether a secret is stored, without reading it out.
    pub fn secret_status(&self, id: &ProviderId, key: &str) -> Result<bool> {
        self.check_secret_key(id, key)?;
        self.secrets.exists(id, key)
    }

    fn check_secret_key(&self, id: &ProviderId, key: &str) -> Result<()> {
        let description = self.provider(id)?.describe();
        let known = description.settings.iter().any(|field| match &field.input {
            FieldInput::Secret { .. } => field.key == key,
            FieldInput::Pairs { secret_values: true, .. } => {
                key.strip_prefix(&format!("{}:", field.key)).is_some_and(|name| !name.trim().is_empty())
            }
            _ => false,
        });
        if known {
            Ok(())
        } else {
            Err(UploadError::invalid(id, key, "this is not a secret of the provider"))
        }
    }

    // ---------------------------------------------------------------------------------- .sxcu

    /// Reads a ShareX `.sxcu` file into the custom provider: the secret values go to the keychain
    /// at once (replacing the ones `old` names), and the rest comes back for the settings.
    pub fn import_sxcu(&self, text: &str, old: Option<&Value>) -> Result<SxcuImported> {
        let id = ProviderId::new(crate::providers::CUSTOM);
        let imported = sxcu::read(text).map_err(|e| UploadError::invalid(&id, e.field, e.message))?;
        let provider = self.provider(&id)?;
        let previous = self.config(provider.as_ref(), old)?;
        for field in sxcu::SECRET_PAIR_FIELDS {
            for name in previous.names(field) {
                self.secrets.remove(&id, &pair_secret_key(field, &name))?;
            }
        }
        let mut secret_keys = Vec::new();
        for (key, value) in &imported.secrets {
            self.secrets.set(&id, key, value)?;
            secret_keys.push(key.clone());
        }
        let mut config = ProviderConfig { values: imported.values.clone(), ..ProviderConfig::default() };
        for (key, value) in &imported.secrets {
            config.secrets.insert(key.clone(), Secret::new(value.clone()));
        }
        let warnings = provider.validate(&config).into_iter().filter(|i| !i.is_error()).collect();
        Ok(SxcuImported { values: imported.values, secret_keys, warnings })
    }
}

/// The provider's own checks plus the required rows of its form.
fn full_validation(provider: &dyn Provider, config: &ProviderConfig) -> Vec<Issue> {
    let mut issues = Vec::new();
    for field in provider.describe().settings.iter().filter(|f| f.required) {
        let present = match &field.input {
            FieldInput::Secret { .. } => config.secrets.contains_key(&field.key),
            FieldInput::Toggle { .. } | FieldInput::Select { .. } | FieldInput::Number { .. } => true,
            _ => match config.value(&field.key) {
                Some(FieldValue::Text(text)) => !text.trim().is_empty(),
                Some(_) => true,
                // A row never typed in holds the default the form shows, and the provider sends
                // that default; only a row the user emptied is missing.
                None => matches!(
                    &field.input,
                    FieldInput::Text { default: Some(default), .. } | FieldInput::Url { default: Some(default), .. }
                        if !default.trim().is_empty()
                ),
            },
        };
        if !present {
            issues.push(Issue::error(&field.key, "upload.issue.required"));
        }
    }
    for issue in provider.validate(config) {
        if !issues.iter().any(|i| i.field == issue.field && i.is_error()) {
            issues.push(issue);
        }
    }
    issues
}

/// Cuts every secret of `config` out of an error's text, as it is and percent-encoded, so a host
/// that echoes a key back cannot carry it into a toast, a log or an event.
pub fn scrub(error: UploadError, config: &ProviderConfig) -> UploadError {
    let clean = |text: String| -> String {
        let mut text = text;
        for secret in config.secrets.values() {
            let raw = secret.expose();
            if raw.len() < 4 {
                continue;
            }
            for form in [raw.to_string(), crate::sigv4::encode(raw, true)] {
                text = text.replace(&form, "•••");
            }
        }
        text
    };
    match error {
        UploadError::Provider { provider, status, reason } => {
            UploadError::Provider { provider, status, reason: clean(reason) }
        }
        UploadError::InvalidConfig { provider, field, reason } => {
            UploadError::InvalidConfig { provider, field: clean(field), reason: clean(reason) }
        }
        UploadError::Network(reason) => UploadError::Network(clean(reason)),
        UploadError::HostNotAllowed { provider, host } => UploadError::HostNotAllowed { provider, host: clean(host) },
        UploadError::Keychain(reason) => UploadError::Keychain(clean(reason)),
        UploadError::Io(io) => UploadError::Io(std::io::Error::new(io.kind(), clean(io.to_string()))),
        other => other,
    }
}

// ----------------------------------------------------------------------------------- random

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A random 64-bit number. The standard library seeds its hasher keys from the system's
/// randomness, which is all an upload id, a multipart boundary or an object key suffix needs.
fn random_u64() -> u64 {
    RandomState::new().hash_one((COUNTER.fetch_add(1, Ordering::Relaxed), Instant::now()))
}

/// `len` random characters from `a–z` and `0–9`.
pub(crate) fn random_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let mut n = random_u64();
        for _ in 0..12 {
            out.push(ALPHABET[(n % 36) as usize] as char);
            n /= 36;
        }
    }
    out.truncate(len);
    out
}

/// A random index below `len` (0 when `len` is 0).
pub(crate) fn random_index(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (random_u64() % len as u64) as usize
    }
}
