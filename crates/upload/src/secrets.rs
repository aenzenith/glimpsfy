//! Secrets of the providers in the system keychain, through `keyring-core`.
//!
//! On macOS the store is `apple-native-keyring-store`'s keychain module: a generic password in
//! the login keychain, whose service is `com.aenzenith.glimpsfy.upload.<provider>` and whose
//! account is the field's key (`apiKey`, `headers:Authorization`). Keychain Access lists them
//! under that service. Tests use `keyring_core::mock::Store`, which keeps everything in memory.
//!
//! A secret is written here and read here only. Nothing in this module returns a secret to a
//! caller outside the crate except [`SecretStore::get`], which the engine uses to build one
//! request's configuration; the status query only says whether a value exists, and on the
//! keychain it reads the item's attributes without decrypting the value.

use std::collections::HashMap;
use std::sync::Arc;

use keyring_core::{CredentialPersistence, CredentialStore, Entry, Error as KeyringError};

use crate::{ProviderId, Result, Secret, UploadError};

/// The service prefix of every keychain item Glimpsfy's uploads keep.
pub const SERVICE_PREFIX: &str = "com.aenzenith.glimpsfy.upload";

/// The keychain, or whichever store `keyring-core` was given.
#[derive(Clone)]
pub struct SecretStore {
    store: Arc<CredentialStore>,
    /// The service prefix, [`SERVICE_PREFIX`] unless a dev run keeps its secrets apart.
    prefix: Arc<str>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore").field("vendor", &self.store.vendor()).finish()
    }
}

impl SecretStore {
    /// The system keychain. Only macOS has one wired in yet.
    pub fn system() -> Result<SecretStore> {
        #[cfg(target_os = "macos")]
        {
            let store = apple_native_keyring_store::keychain::Store::new().map_err(keychain_error)?;
            Ok(SecretStore { store, prefix: SERVICE_PREFIX.into() })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(UploadError::NotImplemented("the keychain of this platform"))
        }
    }

    /// Any `keyring-core` store, such as `keyring_core::mock::Store` in tests.
    pub fn with_store(store: Arc<CredentialStore>) -> SecretStore {
        SecretStore { store, prefix: SERVICE_PREFIX.into() }
    }

    /// `keyring-core`'s mock store, which keeps everything in this process's memory, for the
    /// tests of crates that do not depend on `keyring-core` themselves.
    #[doc(hidden)]
    pub fn in_memory() -> Result<SecretStore> {
        Ok(SecretStore {
            store: keyring_core::mock::Store::new().map_err(keychain_error)?,
            prefix: SERVICE_PREFIX.into(),
        })
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
        self.store.build(&self.service(provider), key, None).map_err(keychain_error)
    }

    /// Stores `value` under the provider's `key`, replacing what was there.
    pub fn set(&self, provider: &ProviderId, key: &str, value: &str) -> Result<()> {
        self.entry(provider, key)?.set_password(value).map_err(keychain_error)
    }

    /// Removes the provider's `key`. Removing a key that is not stored is not an error.
    pub fn remove(&self, provider: &ProviderId, key: &str) -> Result<()> {
        match self.entry(provider, key)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        }
    }

    /// Whether a value is stored under the provider's `key`, without returning it. The keychain
    /// answers from the item's attributes, so the value is not decrypted and macOS does not ask
    /// for permission. A store that cannot search, or one that lives in memory (the mock keeps an
    /// entry it has deleted, and matches parts of names), is asked for the credential itself,
    /// which reads nothing out of the process.
    pub fn exists(&self, provider: &ProviderId, key: &str) -> Result<bool> {
        let service = self.service(provider);
        let in_memory =
            matches!(self.store.persistence(), CredentialPersistence::EntryOnly | CredentialPersistence::ProcessOnly);
        let spec = HashMap::from([("service", service.as_str()), ("user", key)]);
        let found = match self.store.search(&spec) {
            Ok(found) => found,
            Err(KeyringError::NotSupportedByStore(_)) => vec![self.entry(provider, key)?],
            Err(error) => return Err(keychain_error(error)),
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
                Err(error) => return Err(keychain_error(error)),
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
            Err(error) => Err(keychain_error(error)),
        }
    }
}

/// The keychain's own reason, which never holds the secret.
fn keychain_error(error: KeyringError) -> UploadError {
    let reason = match error {
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
