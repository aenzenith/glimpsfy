//! What a provider tells the UI about itself: the settings form the settings window renders, the
//! facts about its host that `docs/PROVIDERS.md` records, and the problems a configuration has.
//!
//! This is the contract with the settings window. Every label, help and message is a key of
//! `src/locales/*/upload.json`, so the form follows the UI language; product names and domains are
//! never translated. The TypeScript mirror is `src/lib/api/upload.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ProviderId;

/// The four shapes of provider form in Settings › Upload (artboard 6 of the settings prototype).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormShape {
    /// A public host with nothing to set up: its terms in short and, at most, a retention choice.
    Anonymous,
    /// A host that works with the user's own key: the key, the Keychain line and a test.
    Key,
    /// An S3-compatible bucket: bucket, credentials and links.
    Bucket,
    /// A custom HTTP endpoint, typed in or read from a ShareX `.sxcu` file: request and response.
    Custom,
}

/// What a user needs before a provider works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountNeed {
    /// The provider works without an account.
    None,
    /// The user registers their own client ID, because one shipped in an open source binary
    /// could be abused by anyone.
    ClientId,
    ApiKey,
    /// An access key ID and a secret access key, as S3 uses.
    AccessKeys,
    /// Whatever the custom endpoint asks for.
    Custom,
}

/// How an upload can be removed from its host later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeletionKind {
    /// The host offers no way to delete an upload.
    None,
    /// Glimpsfy deletes the upload itself with a request to the host.
    Request,
    /// The host hands out a deletion page, which Glimpsfy opens in the browser.
    Page,
}

/// Who can open an uploaded file, as the host states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Visibility {
    /// Anyone who has the link can open the file, and the host does not list it publicly.
    AnyoneWithLink,
    /// The bucket's own policy decides (S3).
    BucketPolicy,
    /// The server's owner decides (custom HTTP).
    ServerOwner,
    /// The host's documentation does not say.
    NotStated,
}

/// How long a host keeps an upload, as its documentation states it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Retention {
    /// The documentation states no expiry: the file stays until the host removes it.
    NoExpiry,
    /// The host removes a file after this many days without a visit.
    Inactivity { days: u32 },
    /// The user picks one of these periods per upload, in the provider's `setting`.
    Chosen { setting: String, hours: Vec<u32> },
    /// The period follows from the file size, between these bounds, by the host's formula.
    BySize { min_days: u32, max_days: u32, max_bytes: u64 },
    /// The user may set an expiry in the provider's `setting`; without one the documentation does
    /// not say how long the file stays.
    Optional { setting: String },
    /// The user's own storage keeps the file as long as its owner does (S3, custom HTTP).
    OwnStorage,
    /// The documentation does not say.
    NotStated,
}

/// What `docs/PROVIDERS.md` records about a public host, with the date it was read. The app shows
/// nothing about a host that is not here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFacts {
    /// The day the host's documentation was read, as `YYYY-MM-DD`.
    pub checked_on: String,
    /// The pages that were read.
    pub sources: Vec<String>,
    pub visibility: Visibility,
    pub retention: Retention,
    /// The largest file the host accepts, when its documentation states one.
    pub max_file_bytes: Option<u64>,
    /// The page with the host's terms.
    pub terms_url: Option<String>,
}

/// How a provider presents itself and the settings it needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescription {
    pub id: ProviderId,
    /// The host's product name, which is never translated ("Catbox").
    pub name: String,
    /// The two-letter monogram of the host list.
    pub monogram: String,
    pub shape: FormShape,
    pub account: AccountNeed,
    /// The domain files go to, for a host whose address is fixed (`litterbox.catbox.moe`). S3 and
    /// custom endpoints name theirs from their settings, through `upload_preflight`.
    pub domain: Option<String>,
    /// The key of the sentence that says who runs the host, as the host names itself
    /// (`upload.host.catbox.operator`).
    pub operator: Option<String>,
    pub website: Option<String>,
    /// Where the user gets their own key ("Register an app", "Get a key").
    pub key_url: Option<String>,
    /// What the host's documentation says, with the date it was read; `None` for S3 and custom
    /// endpoints, whose rules are the user's own.
    pub facts: Option<HostFacts>,
    /// The largest file the host accepts, when its documentation states one.
    pub max_file_bytes: Option<u64>,
    /// Whether links of this host expire on their own.
    pub links_expire: bool,
    /// Whether an upload can be deleted from the host later.
    pub supports_delete: bool,
    pub deletion: DeletionKind,
    /// Whether "Test connection" makes sense for this provider.
    pub testable: bool,
    /// The settings form, in the order the settings window shows it.
    pub settings: Vec<SettingField>,
}

/// One row of a provider's settings form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingField {
    /// The key the value is stored under, unique inside the provider. A secret is stored in the
    /// Keychain under this key; every other value under `settings.upload.providers[<id>][<key>]`.
    pub key: String,
    /// The key of the row's label in `src/locales/*/upload.json`.
    pub label: String,
    /// The key of the help sentence under the row, when the row has one.
    pub help: Option<String>,
    /// The key of the heading of the group the row belongs to ("Bucket", "Credentials"…). Rows
    /// with the same section follow each other.
    pub section: Option<String>,
    pub required: bool,
    /// Whether the value (or, for `pairs`, each value of the list) lives in the Keychain. The UI
    /// sends it once with `upload_set_secret`, never writes it into the settings, and only learns
    /// whether one is stored (`upload_secret_status`).
    pub secret: bool,
    /// Checks the UI runs while the user types. The core runs the same checks, and more, in
    /// `upload_validate`, which is the one to trust.
    pub rules: Vec<FieldRule>,
    /// The kind of input and its own options. In JSON it is flattened into the row, as
    /// `{ "key": "…", "kind": "select", "options": […], … }`.
    #[serde(flatten)]
    pub input: FieldInput,
}

/// The input of a settings row, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FieldInput {
    /// A line of text. `tokens` lists the `{token}`s it understands, for the token buttons;
    /// `monospace` asks for the mono field style (keys, paths, templates). The placeholder of a
    /// text, URL or pairs row is a literal example (`eu-central-1`, `{json:url}`), shown as it is.
    Text {
        default: Option<String>,
        placeholder: Option<String>,
        monospace: bool,
        tokens: Vec<String>,
    },
    /// A value that goes to the Keychain. Its stored value is never sent to the UI. Its
    /// placeholder is a key of `src/locales/*/upload.json` ("Paste your API key").
    Secret {
        placeholder: Option<String>,
    },
    /// An `http` or `https` URL; anything else is refused, and plain `http` to a host other than
    /// this Mac gets a warning.
    Url {
        default: Option<String>,
        placeholder: Option<String>,
    },
    Select {
        options: Vec<FieldOption>,
        default: Option<String>,
    },
    Number {
        default: Option<f64>,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Toggle {
        default: bool,
    },
    /// A list of name and value pairs, such as the headers of a custom endpoint. With
    /// `secretValues`, the settings keep only the names (a list of strings) and each value lives
    /// in the Keychain under [`pair_secret_key`]`(field, name)`.
    Pairs {
        secret_values: bool,
        name_placeholder: Option<String>,
        value_placeholder: Option<String>,
    },
}

/// One choice of a `select` row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldOption {
    pub value: String,
    /// The key of the choice's label in `src/locales/*/upload.json`. A choice whose label is a
    /// name (an HTTP method) has `literal` set and is shown as it is.
    pub label: String,
    pub literal: bool,
}

/// A check the UI can run on a row while the user types. Each rule has its own message key,
/// `upload.rule.<rule>`, which takes the rule's numbers as parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FieldRule {
    MaxLength {
        max: usize,
    },
    /// Only `http://` and `https://` addresses are accepted.
    HttpUrl,
    /// Only these `{token}`s may appear in the text.
    Tokens {
        allowed: Vec<String>,
    },
    /// The value must not contain whitespace.
    NoWhitespace,
}

/// The key under which one value of a secret `pairs` row lives in the Keychain.
pub fn pair_secret_key(field: &str, name: &str) -> String {
    format!("{field}:{name}")
}

/// A stored value of a non-secret settings row. In JSON it is the plain value: a string, a
/// number, a boolean, a list of pairs or, for a `pairs` row with secret values, a list of names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    Toggle(bool),
    Number(f64),
    Text(String),
    Pairs(Vec<Pair>),
    Names(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pair {
    pub name: String,
    pub value: String,
}

/// How serious a problem of a configuration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// The provider cannot upload until it is fixed.
    Error,
    /// The provider works, and the user should know something (plain `http`, a bucket name that
    /// does not fit virtual-host addressing).
    Warning,
}

/// One problem of a configuration, for the row it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// The row's key, or `None` for the provider as a whole.
    pub field: Option<String>,
    pub severity: Severity,
    /// The message's key in `src/locales/*/upload.json`, a complete sentence that says what is
    /// wrong and what to do.
    pub message: String,
    /// The message's `{placeholders}`.
    pub params: BTreeMap<String, String>,
}

impl Issue {
    pub fn error(field: &str, message: &str) -> Issue {
        Issue { field: Some(field.into()), severity: Severity::Error, message: message.into(), params: BTreeMap::new() }
    }

    pub fn warning(field: &str, message: &str) -> Issue {
        Issue {
            field: Some(field.into()),
            severity: Severity::Warning,
            message: message.into(),
            params: BTreeMap::new(),
        }
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Issue {
        self.params.insert(name.into(), value.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

// ---------------------------------------------------------------------- builders for providers

impl SettingField {
    fn new(key: &str, label: &str, input: FieldInput) -> SettingField {
        SettingField {
            key: key.into(),
            label: label.into(),
            help: None,
            section: None,
            required: false,
            secret: matches!(input, FieldInput::Secret { .. } | FieldInput::Pairs { secret_values: true, .. }),
            rules: Vec::new(),
            input,
        }
    }

    pub(crate) fn text(key: &str, label: &str) -> SettingField {
        SettingField::new(
            key,
            label,
            FieldInput::Text { default: None, placeholder: None, monospace: false, tokens: vec![] },
        )
    }

    pub(crate) fn secret(key: &str, label: &str) -> SettingField {
        SettingField::new(key, label, FieldInput::Secret { placeholder: None })
    }

    pub(crate) fn url(key: &str, label: &str, placeholder: &str) -> SettingField {
        let mut field =
            SettingField::new(key, label, FieldInput::Url { default: None, placeholder: Some(placeholder.into()) });
        field.rules.push(FieldRule::HttpUrl);
        field
    }

    pub(crate) fn select(key: &str, label: &str, options: Vec<FieldOption>, default: &str) -> SettingField {
        SettingField::new(key, label, FieldInput::Select { options, default: Some(default.into()) })
    }

    pub(crate) fn pairs(key: &str, label: &str, secret_values: bool) -> SettingField {
        SettingField::new(
            key,
            label,
            FieldInput::Pairs { secret_values, name_placeholder: None, value_placeholder: None },
        )
    }

    pub(crate) fn help(mut self, help: &str) -> Self {
        self.help = Some(help.into());
        self
    }

    pub(crate) fn section(mut self, section: &str) -> Self {
        self.section = Some(section.into());
        self
    }

    pub(crate) fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub(crate) fn rule(mut self, rule: FieldRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Sets the placeholder of a text, URL or secret row.
    pub(crate) fn placeholder(mut self, text: &str) -> Self {
        match &mut self.input {
            FieldInput::Text { placeholder, .. }
            | FieldInput::Url { placeholder, .. }
            | FieldInput::Secret { placeholder } => *placeholder = Some(text.into()),
            FieldInput::Pairs { name_placeholder, .. } => *name_placeholder = Some(text.into()),
            _ => {}
        }
        self
    }

    /// Sets the default of a text or URL row.
    pub(crate) fn default_text(mut self, text: &str) -> Self {
        if let FieldInput::Text { default, .. } | FieldInput::Url { default, .. } = &mut self.input {
            *default = Some(text.into());
        }
        self
    }

    pub(crate) fn mono(mut self) -> Self {
        if let FieldInput::Text { monospace, .. } = &mut self.input {
            *monospace = true;
        }
        self
    }

    pub(crate) fn tokens(mut self, list: &[&str]) -> Self {
        if let FieldInput::Text { tokens, .. } = &mut self.input {
            *tokens = list.iter().map(|t| t.to_string()).collect();
        }
        self.rules.push(FieldRule::Tokens { allowed: list.iter().map(|t| t.to_string()).collect() });
        self
    }
}

impl FieldOption {
    pub(crate) fn new(value: &str, label: &str) -> FieldOption {
        FieldOption { value: value.into(), label: label.into(), literal: false }
    }

    pub(crate) fn literal(value: &str) -> FieldOption {
        FieldOption { value: value.into(), label: value.into(), literal: true }
    }
}
