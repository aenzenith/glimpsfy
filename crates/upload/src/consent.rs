//! The first-upload confirmation, as data: what the user confirmed and what the dialog shows.
//!
//! Consent is kept in the settings as a list of keys (`settings.upload.confirmedProviders`). A
//! host whose address is fixed is confirmed under its provider id (`litterbox`); a destination
//! the user configures is confirmed with its address (`custom@share.example.dev`,
//! `s3@s3.eu-central-1.amazonaws.com/team-captures`, and `custom@http://share.example.dev` for
//! plain `http`), so pointing the provider somewhere new, or dropping to `http`, asks again. Only
//! the `upload_confirm_provider` command adds a key, after the user pressed Upload in the dialog;
//! `engine` refuses every request whose key is missing.

use serde::{Deserialize, Serialize};

use crate::{Destination, ProviderId, Retention, Visibility};

/// The key under which consent to `destination` through `provider` is recorded.
pub fn consent_key(provider: &ProviderId, destination: &Destination) -> String {
    if destination.fixed {
        return provider.to_string();
    }
    // The dialog warns about plain `http` only for a destination that uses it, so confirming
    // the `https` address must not cover the `http` one. An `https` key keeps the bare address,
    // so the consents recorded before the scheme was part of the key stay valid.
    let scheme = if destination.origins.iter().any(|origin| origin.scheme == "http") { "http://" } else { "" };
    match &destination.bucket {
        Some(bucket) => format!("{provider}@{scheme}{}/{bucket}", destination.domain),
        None => format!("{provider}@{scheme}{}", destination.domain),
    }
}

/// Whether the recorded keys cover this destination.
pub fn is_confirmed(confirmed: &[String], provider: &ProviderId, destination: &Destination) -> bool {
    let key = consent_key(provider, destination);
    confirmed.contains(&key)
}

/// What the first-upload dialog says about a destination, from the provider's facts and the
/// current settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentInfo {
    pub provider: ProviderId,
    /// The host's product name, never translated.
    pub name: String,
    /// The domain the image goes to.
    pub domain: String,
    /// The bucket, for an S3-compatible destination.
    pub bucket: Option<String>,
    /// The key of the sentence that names who runs the host, when the provider knows it.
    pub operator: Option<String>,
    pub visibility: Visibility,
    /// How long the host keeps the file with the current settings.
    pub retention: RetentionNow,
    /// The day `docs/PROVIDERS.md` read the host's documentation, for a public host.
    pub checked_on: Option<String>,
    pub terms_url: Option<String>,
    pub max_file_bytes: Option<u64>,
    /// Whether the request would travel unencrypted to another machine.
    pub plain_http: bool,
    /// Whether the user has confirmed this destination already.
    pub confirmed: bool,
    /// The key `upload_confirm_provider` records.
    pub consent_key: String,
}

/// How long an upload made now stays on the host, as far as the host says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RetentionNow {
    /// The host deletes the file this many hours after the upload.
    Hours { hours: u32 },
    /// The host keeps the file between these many days, depending on its size.
    BySize { min_days: u32, max_days: u32 },
    /// The host keeps the file until it goes unvisited for this many days.
    UntilUnvisited { days: u32 },
    /// The host keeps the file and states no expiry.
    NoExpiry,
    /// The user's own storage keeps the file as long as its owner does.
    OwnStorage,
    /// The host's documentation does not say.
    NotStated,
}

impl RetentionNow {
    /// The retention a host states, with the choice the settings made where the host offers one.
    pub fn from(retention: &Retention, chosen_hours: Option<u32>) -> RetentionNow {
        match retention {
            Retention::NoExpiry => RetentionNow::NoExpiry,
            Retention::Inactivity { days } => RetentionNow::UntilUnvisited { days: *days },
            Retention::Chosen { hours, .. } => {
                let fallback = hours.last().copied().unwrap_or(0);
                RetentionNow::Hours { hours: chosen_hours.filter(|h| hours.contains(h)).unwrap_or(fallback) }
            }
            Retention::BySize { min_days, max_days, .. } => {
                RetentionNow::BySize { min_days: *min_days, max_days: *max_days }
            }
            Retention::Optional { .. } => match chosen_hours {
                Some(hours) if hours > 0 => RetentionNow::Hours { hours },
                _ => RetentionNow::NotStated,
            },
            Retention::OwnStorage => RetentionNow::OwnStorage,
            Retention::NotStated => RetentionNow::NotStated,
        }
    }
}
