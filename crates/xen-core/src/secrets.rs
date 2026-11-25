//! Password storage.
//!
//! Saved passwords live in the operating system's credential store through
//! the `keyring` crate: the Keychain on macOS, the Credential Manager on
//! Windows and the Secret Service (GNOME Keyring, KWallet) on Linux. They are
//! never written to the settings files or the log.
//!
//! [`SecretStore`] abstracts the store so tests use [`MemorySecretStore`] and
//! never touch the real keyring.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

/// A failure of the credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

/// A store of secrets keyed by account name.
pub trait SecretStore: Send + Sync {
    /// The secret saved for `account`, if any.
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    /// Saves (or replaces) the secret for `account`.
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    /// Deletes the secret for `account`; deleting a missing one is fine.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The account name a server's password is saved under:
/// `user@https://host:port`.
pub fn account_for(username: &str, endpoint: &xapi::Endpoint) -> String {
    format!("{username}@{}", endpoint.base_url())
}

/// The OS credential store. Every entry belongs to one `service` name.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(describe)
    }
}

fn describe(e: keyring::Error) -> SecretError {
    SecretError(e.to_string())
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.entry(account)?.set_password(secret).map_err(describe)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(describe(e)),
        }
    }
}

/// An in-memory store for tests (and when no keyring is wanted).
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    secrets: Mutex<HashMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accounts currently stored, sorted.
    pub fn accounts(&self) -> Vec<String> {
        let mut all: Vec<String> = self.lock().keys().cloned().collect();
        all.sort();
        all
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.secrets.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self.lock().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.lock().insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.lock().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store() {
        let store = MemorySecretStore::new();
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "1").unwrap();
        store.set("a", "2").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("2"));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert!(store.accounts().is_empty());
    }

    #[test]
    fn account_names_include_scheme_and_port() {
        let e = xapi::Endpoint::parse("10.0.0.1").unwrap();
        assert_eq!(account_for("root", &e), "root@https://10.0.0.1:443");
    }

    /// Round trip through the real OS keyring under a clearly named test
    /// service; the entry is removed again. Ignored by default so that
    /// `cargo test` never touches the user's keyring:
    /// `cargo test -p xen-core os_keyring -- --ignored`.
    #[test]
    #[ignore = "touches the OS keyring"]
    fn os_keyring_round_trip() {
        let store = KeyringSecretStore::new("xenadmin-rs-test (safe to delete)");
        let account = format!("test-{}@https://192.0.2.1:443", std::process::id());
        store.set(&account, "not-a-real-password").unwrap();
        assert_eq!(
            store.get(&account).unwrap().as_deref(),
            Some("not-a-real-password")
        );
        store.delete(&account).unwrap();
        assert_eq!(store.get(&account).unwrap(), None);
    }
}
