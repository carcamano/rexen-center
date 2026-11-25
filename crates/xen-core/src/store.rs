//! The backend's persistent state: settings, pinned certificates and saved
//! passwords, behind one handle.
//!
//! The backend is the only writer: it records servers when they connect,
//! pins certificates the user trusts and applies preference changes the UI
//! sends. A [`Store`] without [`ConfigPaths`] keeps everything in memory
//! (tests, or when no configuration directory exists).

use std::sync::{Arc, Mutex, PoisonError};

use xapi::{Endpoint, Fingerprint, TrustStore};

use crate::secrets::{MemorySecretStore, SecretError, SecretStore, account_for};
use crate::settings::{self, ConfigPaths, Preferences, SavedServer, Settings};

/// Settings, known servers and passwords.
pub struct Store {
    paths: Option<ConfigPaths>,
    settings: Mutex<Settings>,
    trust: TrustStore,
    secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::in_memory(Arc::new(MemorySecretStore::new()))
    }
}

impl Store {
    /// Loads `settings.toml` and `known_servers.toml` from `paths`. A file
    /// that cannot be read is reported in the log and replaced by defaults;
    /// a malformed `settings.toml` is first renamed to `settings.toml.bad`
    /// so the next save does not destroy it.
    pub fn open(paths: ConfigPaths, secrets: Arc<dyn SecretStore>) -> Self {
        let settings = match settings::load_settings(&paths) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the settings; using defaults");
                if matches!(e, settings::SettingsError::Parse { .. }) {
                    let file = paths.settings_file();
                    let _ = std::fs::rename(&file, file.with_extension("toml.bad"));
                }
                Settings::default()
            }
        };
        let trust = TrustStore::new();
        match settings::load_known_servers(&paths, &trust) {
            Ok(n) => tracing::info!(pinned = n, dir = %paths.dir.display(), "settings loaded"),
            Err(e) => tracing::warn!(error = %e, "could not read the known servers"),
        }
        Self {
            paths: Some(paths),
            settings: Mutex::new(settings),
            trust,
            secrets,
        }
    }

    /// A store that writes nothing to disk.
    pub fn in_memory(secrets: Arc<dyn SecretStore>) -> Self {
        Self::in_memory_with(Settings::default(), secrets)
    }

    /// An in-memory store starting from `settings`.
    pub fn in_memory_with(settings: Settings, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            paths: None,
            settings: Mutex::new(settings),
            trust: TrustStore::new(),
            secrets,
        }
    }

    /// Where the files live (`None` for an in-memory store).
    pub fn paths(&self) -> Option<&ConfigPaths> {
        self.paths.as_ref()
    }

    /// Pinned certificates (shared with every client the backend builds).
    pub fn trust(&self) -> &TrustStore {
        &self.trust
    }

    /// A copy of the current settings.
    pub fn settings(&self) -> Settings {
        self.lock().clone()
    }

    /// Changes the settings and saves them.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let mut settings = self.lock();
        f(&mut settings);
        if let Some(paths) = &self.paths
            && let Err(e) = settings::save_settings(paths, &settings)
        {
            tracing::warn!(error = %e, "could not save the settings");
        }
        settings.clone()
    }

    /// Replaces the preferences.
    pub fn set_preferences(&self, preferences: Preferences) -> Settings {
        self.update_settings(|s| s.preferences = preferences)
    }

    /// Pins `fingerprint` for `authority` and saves the known servers.
    pub fn trust_certificate(&self, authority: &str, fingerprint: Fingerprint) {
        self.trust.trust(authority, fingerprint);
        if let Some(paths) = &self.paths
            && let Err(e) = settings::save_known_servers(paths, &self.trust)
        {
            tracing::warn!(error = %e, "could not save the known servers");
        }
    }

    /// The saved password for `username` on `endpoint`.
    pub fn password(
        &self,
        username: &str,
        endpoint: &Endpoint,
    ) -> Result<Option<String>, SecretError> {
        self.secrets.get(&account_for(username, endpoint))
    }

    /// Saves the password for `username` on `endpoint`.
    pub fn save_password(
        &self,
        username: &str,
        endpoint: &Endpoint,
        password: &str,
    ) -> Result<(), SecretError> {
        self.secrets.set(&account_for(username, endpoint), password)
    }

    /// Deletes the saved password for `username` on `endpoint`.
    pub fn delete_password(&self, username: &str, endpoint: &Endpoint) -> Result<(), SecretError> {
        self.secrets.delete(&account_for(username, endpoint))
    }

    /// Records a server that connected: `address` (the saved-list key, the
    /// endpoint now in use) replaces `previous` (the address the user typed,
    /// if a redirect changed it) and is marked to reconnect on start.
    pub fn remember_server(
        &self,
        previous: Option<&str>,
        address: &str,
        username: &str,
    ) -> Settings {
        self.update_settings(|s| {
            if let Some(previous) = previous.filter(|p| *p != address) {
                s.remove_server(previous);
            }
            s.upsert_server(SavedServer {
                address: address.to_owned(),
                username: username.to_owned(),
                reconnect: true,
            });
        })
    }

    /// Sets whether `address` reconnects on start (after a user disconnect,
    /// it does not).
    pub fn set_reconnect(&self, address: &str, reconnect: bool) -> Settings {
        self.update_settings(|s| {
            if let Some(server) = s.servers.iter_mut().find(|x| x.address == address) {
                server.reconnect = reconnect;
            }
        })
    }

    /// Removes `address` from the saved list and deletes its password.
    pub fn forget_server(&self, address: &str) -> Settings {
        let mut removed = None;
        let settings = self.update_settings(|s| removed = s.remove_server(address));
        if let Some(server) = removed
            && let Ok(endpoint) = Endpoint::parse(&server.address)
            && let Err(e) = self.delete_password(&server.username, &endpoint)
        {
            tracing::warn!(error = %e, "could not delete a saved password");
        }
        settings
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Settings> {
        self.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
