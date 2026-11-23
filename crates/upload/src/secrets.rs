//! Secrets of the providers in the system keychain, through `keyring-core`.
//!
//! Everywhere an item's service is `com.aenzenith.glimpsfy.upload.<provider>` and its account is
//! the field's key (`apiKey`, `headers:Authorization`):
//!
//! - On macOS the store is `apple-native-keyring-store`'s keychain module: a generic password in
//!   the login keychain, which Keychain Access lists under that service.
//! - On Windows it is `windows-native-keyring-store`: a generic credential of the Credential
//!   Manager, listed as `<account>.<service>` with the account as its user name.
//! - On Linux it is `zbus-secret-service-keyring-store`: an item in the default collection of the
//!   session's Secret Service (GNOME Keyring, KWallet), with the attributes `service` and
//!   `username`. Seahorse and KWalletManager list it as `<service> (<account>)`.
//!
//! Tests use `keyring_core::mock::Store`, which keeps everything in memory.
//!
//! A secret is written here and read here only, and there is no second place: when the system
//! has no keyring (a session without a Secret Service), every call fails and nothing is written
//! to a file instead. Nothing in this module returns a secret to a caller outside the crate
//! except [`SecretStore::get`], which the engine uses to build one request's configuration; the
//! status query only says whether a value exists, and it reads the item's attributes without
//! handing the value to anyone.

use std::collections::HashMap;
use std::sync::Arc;

use keyring_core::{CredentialPersistence, CredentialStore, Entry, Error as KeyringError};

use crate::{ProviderId, Result, Secret, UploadError};

/// The service prefix of every keychain item Glimpsfy's uploads keep.
pub const SERVICE_PREFIX: &str = "com.aenzenith.glimpsfy.upload";

/// The Credential Manager keeps at most 2560 bytes for one credential and Windows stores text as
/// UTF-16, which leaves room for 1280 UTF-16 units. A longer value (the bearer token of a custom
/// uploader can be) is refused whole instead of being cut.
#[cfg(windows)]
const CREDENTIAL_MANAGER_MAX_UTF16_UNITS: usize = 1280;

/// The keychain, or whichever store `keyring-core` was given.
#[derive(Clone)]
pub struct SecretStore {
    store: Arc<CredentialStore>,
    /// The service prefix, [`SERVICE_PREFIX`] unless a dev run keeps its secrets apart.
    prefix: Arc<str>,
    /// What the store's search calls the account (`user` on the keychain, `username` on the
    /// Secret Service). `None` is a store that cannot search by service and account (the
    /// Credential Manager), which is asked for the item's attributes instead.
    search_account: Option<&'static str>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore").field("vendor", &self.store.vendor()).finish()
    }
}

impl SecretStore {
    /// The system keychain: the login keychain on macOS, the Credential Manager on Windows and
    /// the session's Secret Service on Linux. The Secret Service is not contacted here but by the
    /// first call that needs it, so a session that has none yet fails that call (with an error
    /// that says so) and works once a keyring runs, without the app starting again.
    pub fn system() -> Result<SecretStore> {
        #[cfg(target_os = "macos")]
        {
            let store = apple_native_keyring_store::keychain::Store::new().map_err(keychain_error)?;
            Ok(SecretStore { store, prefix: SERVICE_PREFIX.into(), search_account: Some("user") })
        }
        #[cfg(windows)]
        {
            let store = windows_native_keyring_store::Store::new().map_err(keychain_error)?;
            Ok(SecretStore { store, prefix: SERVICE_PREFIX.into(), search_account: None })
        }
        #[cfg(target_os = "linux")]
        {
            let store = Arc::new(secret_service::Lazy::default());
            Ok(SecretStore { store, prefix: SERVICE_PREFIX.into(), search_account: Some("username") })
        }
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        {
            Err(UploadError::NotImplemented("the keychain of this platform"))
        }
    }

    /// Any `keyring-core` store, such as `keyring_core::mock::Store` in tests.
    pub fn with_store(store: Arc<CredentialStore>) -> SecretStore {
        SecretStore { store, prefix: SERVICE_PREFIX.into(), search_account: Some("user") }
    }

    /// `keyring-core`'s mock store, which keeps everything in this process's memory, for the
    /// tests of crates that do not depend on `keyring-core` themselves.
    #[doc(hidden)]
    pub fn in_memory() -> Result<SecretStore> {
        Ok(SecretStore::with_store(keyring_core::mock::Store::new().map_err(keychain_error)?))
    }

    /// The same store under another service prefix, so a dev run's secrets never meet the
    /// user's own (`GLIMPSFY_HOME`).
    pub fn with_service_prefix(mut self, prefix: &str) -> SecretStore {
        self.prefix = prefix.into();
        self
    }

    fn entry(&self, provider: &ProviderId, key: &str) -> Result<Entry> {
        if key.is_empty() {
            return Err(UploadError::Keychain("a secret needs a key".into()));
        }
        self.store.build(&self.service(provider), key, None).map_err(|error| self.failed(error))
    }

    /// Stores `value` under the provider's `key`, replacing what was there.
    pub fn set(&self, provider: &ProviderId, key: &str, value: &str) -> Result<()> {
        #[cfg(windows)]
        if value.encode_utf16().count() > CREDENTIAL_MANAGER_MAX_UTF16_UNITS {
            return Err(UploadError::Keychain(format!(
                "the value is longer than the {CREDENTIAL_MANAGER_MAX_UTF16_UNITS} characters the Windows Credential Manager keeps for one secret"
            )));
        }
        self.entry(provider, key)?.set_password(value).map_err(|error| self.failed(error))
    }

    /// Removes the provider's `key`. Removing a key that is not stored is not an error.
    pub fn remove(&self, provider: &ProviderId, key: &str) -> Result<()> {
        match self.entry(provider, key)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(self.failed(error)),
        }
    }

    /// Whether a value is stored under the provider's `key`, without returning it. The keychain
    /// answers from the item's attributes, so the value is not decrypted and macOS does not ask
    /// for permission; the Secret Service answers a search by attributes the same way, with the
    /// items' paths. The Credential Manager has no read without the value, so there the store
    /// crate reads the credential, keeps its attributes and wipes the value before it frees it.
    /// A store that cannot search, or one that lives in memory (the mock keeps an entry it has
    /// deleted, and matches parts of names), is asked for the credential itself, which reads
    /// nothing out of the process.
    pub fn exists(&self, provider: &ProviderId, key: &str) -> Result<bool> {
        let service = self.service(provider);
        let Some(account) = self.search_account else {
            return match self.entry(provider, key)?.get_attributes() {
                Ok(_) => Ok(true),
                Err(KeyringError::NoEntry) => Ok(false),
                Err(error) => Err(self.failed(error)),
            };
        };
        let in_memory =
            matches!(self.store.persistence(), CredentialPersistence::EntryOnly | CredentialPersistence::ProcessOnly);
        let spec = HashMap::from([("service", service.as_str()), (account, key)]);
        let found = match self.store.search(&spec) {
            Ok(found) => found,
            Err(KeyringError::NotSupportedByStore(_)) => vec![self.entry(provider, key)?],
            Err(error) => return Err(self.failed(error)),
        };
        for entry in found {
            if entry.get_specifiers().is_some_and(|(s, k)| s != service || k != key) {
                continue;
            }
            if !in_memory {
                return Ok(true);
            }
            match entry.get_credential() {
                Ok(_) => return Ok(true),
                Err(KeyringError::NoEntry) => continue,
                Err(error) => return Err(self.failed(error)),
            }
        }
        Ok(false)
    }

    fn service(&self, provider: &ProviderId) -> String {
        format!("{}.{provider}", self.prefix)
    }

    /// The value stored under the provider's `key`, for one request's configuration.
    pub(crate) fn get(&self, provider: &ProviderId, key: &str) -> Result<Option<Secret>> {
        match self.entry(provider, key)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(self.failed(error)),
        }
    }

    /// The error of a failed call. On Linux a platform failure also drops the connection: a
    /// Secret Service that started again has forgotten the session the connection held, so the
    /// next call must open a new one.
    fn failed(&self, error: KeyringError) -> UploadError {
        #[cfg(target_os = "linux")]
        if matches!(error, KeyringError::PlatformFailure(_)) {
            if let Some(lazy) = self.store.as_any().downcast_ref::<secret_service::Lazy>() {
                lazy.forget();
            }
        }
        keychain_error(error)
    }
}

/// The keychain's own reason, which never holds the secret.
fn keychain_error(error: KeyringError) -> UploadError {
    let reason = match error {
        #[cfg(target_os = "linux")]
        KeyringError::NoStorageAccess(reason) if reason.is::<secret_service::Unavailable>() => reason.to_string(),
        KeyringError::NoStorageAccess(_) | KeyringError::PlatformFailure(_) => {
            "the keychain is locked or refused access".to_string()
        }
        KeyringError::TooLong(name, max) => format!("the {name} is longer than the keychain's {max} bytes"),
        KeyringError::Invalid(name, reason) => format!("the {name} is invalid: {reason}"),
        // These two carry the stored bytes, so their own text must never be shown.
        KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(..) => {
            "the stored value is not text Glimpsfy can read".to_string()
        }
        KeyringError::NoEntry => "no value is stored".to_string(),
        KeyringError::Ambiguous(_) => "the keychain holds more than one value for this key".to_string(),
        _ => "the keychain could not be used".to_string(),
    };
    UploadError::Keychain(reason)
}

/// The session's Secret Service, connected on first use.
#[cfg(target_os = "linux")]
mod secret_service {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use keyring_core::api::CredentialStoreApi;
    use keyring_core::{Entry, Error as KeyringError};
    use zbus_secret_service_keyring_store::Store;

    /// A Secret Service store that opens its D-Bus connection when it is first asked for an
    /// item, and again after [`Lazy::forget`]. A session without a Secret Service (a bare window
    /// manager, a headless login) answers every call with [`Unavailable`] until one runs.
    #[derive(Default)]
    pub(super) struct Lazy {
        connected: Mutex<Option<Arc<Store>>>,
    }

    /// No Secret Service answered. The text names no secret: it is the bus's reason.
    #[derive(Debug)]
    pub(super) struct Unavailable(String);

    impl std::fmt::Display for Unavailable {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "no system keyring is available: no Secret Service such as GNOME Keyring or KWallet answers in this session ({})",
                self.0
            )
        }
    }

    impl std::error::Error for Unavailable {}

    impl Lazy {
        fn store(&self) -> keyring_core::Result<Arc<Store>> {
            let mut connected = self.connected.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(store) = connected.as_ref() {
                return Ok(store.clone());
            }
            let store = Store::new().map_err(|error| {
                let reason = match error {
                    KeyringError::PlatformFailure(reason) | KeyringError::NoStorageAccess(reason) => reason.to_string(),
                    other => other.to_string(),
                };
                KeyringError::NoStorageAccess(Box::new(Unavailable(reason)))
            })?;
            *connected = Some(store.clone());
            Ok(store)
        }

        /// Drops the connection, so the next call opens a new one.
        pub(super) fn forget(&self) {
            *self.connected.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    impl std::fmt::Debug for Lazy {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Lazy").finish_non_exhaustive()
        }
    }

    impl CredentialStoreApi for Lazy {
        fn vendor(&self) -> String {
            "Secret Service store, https://crates.io/crates/zbus-secret-service-keyring-store".to_string()
        }

        fn id(&self) -> String {
            "the session's Secret Service".to_string()
        }

        fn build(
            &self,
            service: &str,
            user: &str,
            modifiers: Option<&HashMap<&str, &str>>,
        ) -> keyring_core::Result<Entry> {
            let store = self.store()?;
            if modifiers.is_some() {
                return store.build(service, user, modifiers);
            }
            // The label is what Seahorse and KWalletManager list a new item under.
            let label = format!("{service} ({user})");
            store.build(service, user, Some(&HashMap::from([("label", label.as_str())])))
        }

        fn search(&self, spec: &HashMap<&str, &str>) -> keyring_core::Result<Vec<Entry>> {
            self.store()?.search(spec)
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }
}
