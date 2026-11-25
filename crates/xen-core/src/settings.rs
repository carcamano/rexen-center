//! Persistent settings: preferences, the saved server list and the pinned
//! certificates (trust on first use), as TOML files in the configuration
//! directory.
//!
//! * `settings.toml`: [`Settings`] (preferences and saved servers). Passwords
//!   are never written here; they live in the OS keyring (see
//!   [`crate::secrets`]).
//! * `known_servers.toml`: the SHA-256 fingerprint trusted for each server
//!   (`host:port`), loaded into the [`TrustStore`] on start and rewritten
//!   whenever the user trusts a certificate.
//!
//! [`ConfigPaths::resolve`] picks the directory: `XENADMIN_CONFIG_DIR` if set,
//! otherwise a `settings` directory next to the executable when a `portable`
//! marker file sits beside it (portable mode, as in the original), otherwise
//! the platform's configuration directory
//! (`~/Library/Application Support/<app>` on macOS, `%APPDATA%\<app>` on
//! Windows, `$XDG_CONFIG_HOME/<app>` on Linux). Log files go to its `logs`
//! subdirectory.
//!
//! Files are written atomically (temporary file, then rename).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xapi::{Fingerprint, TrustStore};

/// Environment variable that overrides the configuration directory.
pub const CONFIG_DIR_ENV: &str = "XENADMIN_CONFIG_DIR";

/// Name of the marker file that turns on portable mode when it sits next to
/// the executable.
pub const PORTABLE_MARKER: &str = "portable";

const SETTINGS_FILE: &str = "settings.toml";
const KNOWN_SERVERS_FILE: &str = "known_servers.toml";
const LOG_DIR: &str = "logs";

/// How the configuration directory was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLocation {
    /// The platform's per-user configuration directory.
    User,
    /// `settings/` next to the executable (a `portable` marker is present).
    Portable,
    /// Set through [`CONFIG_DIR_ENV`] (or given explicitly).
    Override,
}

/// Where the settings, known servers and logs live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub dir: PathBuf,
    pub location: ConfigLocation,
}

impl ConfigPaths {
    /// An explicit directory.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            location: ConfigLocation::Override,
        }
    }

    /// Resolves the directory for application `app_name` (see the module
    /// docs for the rules). `None` only if the platform has no home
    /// directory.
    pub fn resolve(app_name: &str) -> Option<Self> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        Self::resolve_with(
            app_name,
            std::env::var_os(CONFIG_DIR_ENV).map(PathBuf::from),
            exe_dir.as_deref(),
        )
    }

    /// [`ConfigPaths::resolve`] with its inputs made explicit (for tests).
    pub fn resolve_with(
        app_name: &str,
        override_dir: Option<PathBuf>,
        exe_dir: Option<&Path>,
    ) -> Option<Self> {
        if let Some(dir) = override_dir.filter(|d| !d.as_os_str().is_empty()) {
            return Some(Self::at(dir));
        }
        if let Some(exe_dir) = exe_dir
            && exe_dir.join(PORTABLE_MARKER).is_file()
        {
            return Some(Self {
                dir: exe_dir.join("settings"),
                location: ConfigLocation::Portable,
            });
        }
        let dirs = directories::ProjectDirs::from("", "", app_name)?;
        Some(Self {
            dir: dirs.config_dir().to_path_buf(),
            location: ConfigLocation::User,
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    pub fn known_servers_file(&self) -> PathBuf {
        self.dir.join(KNOWN_SERVERS_FILE)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.dir.join(LOG_DIR)
    }
}

/// User preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Remember passwords in the OS keyring (the connect dialog's default).
    pub save_passwords: bool,
    /// On start, reconnect to the servers that were connected when the
    /// application last closed.
    pub restore_connections: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            save_passwords: true,
            restore_connections: true,
        }
    }
}

/// A server in the saved list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    /// Address as typed or redirected to, in [`xapi::Endpoint`] display form
    /// (`10.0.0.1`, `host:8443`, `http://127.0.0.1:8080`). The list key.
    pub address: String,
    pub username: String,
    /// It was connected when last seen: reconnect on start.
    #[serde(default)]
    pub reconnect: bool,
}

/// Contents of `settings.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub preferences: Preferences,
    #[serde(rename = "server")]
    pub servers: Vec<SavedServer>,
}

impl Settings {
    /// The saved entry for `address`.
    pub fn server(&self, address: &str) -> Option<&SavedServer> {
        self.servers.iter().find(|s| s.address == address)
    }

    /// Adds or updates the entry for `address`.
    pub fn upsert_server(&mut self, server: SavedServer) {
        match self
            .servers
            .iter_mut()
            .find(|s| s.address == server.address)
        {
            Some(existing) => *existing = server,
            None => self.servers.push(server),
        }
    }

    /// Removes the entry for `address`; returns it.
    pub fn remove_server(&mut self, address: &str) -> Option<SavedServer> {
        let i = self.servers.iter().position(|s| s.address == address)?;
        Some(self.servers.remove(i))
    }
}

/// Errors reading or writing a settings file.
#[derive(Debug)]
pub enum SettingsError {
    Io { path: PathBuf, source: io::Error },
    Parse { path: PathBuf, message: String },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Parse { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for SettingsError {}

/// Reads `settings.toml`; a missing file gives the defaults.
pub fn load_settings(paths: &ConfigPaths) -> Result<Settings, SettingsError> {
    read_toml(&paths.settings_file()).map(Option::unwrap_or_default)
}

/// Writes `settings.toml`.
pub fn save_settings(paths: &ConfigPaths, settings: &Settings) -> Result<(), SettingsError> {
    write_toml(&paths.settings_file(), settings)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownServersFile {
    /// `host:port` → colon-separated SHA-256 fingerprint.
    #[serde(default)]
    servers: BTreeMap<String, String>,
}

/// Loads `known_servers.toml` into `trust`; returns how many pins were read.
/// Malformed fingerprints are skipped with a warning.
pub fn load_known_servers(paths: &ConfigPaths, trust: &TrustStore) -> Result<usize, SettingsError> {
    let Some(file) = read_toml::<KnownServersFile>(&paths.known_servers_file())? else {
        return Ok(0);
    };
    let mut count = 0;
    for (authority, text) in file.servers {
        match Fingerprint::parse(&text) {
            Some(fp) => {
                trust.trust(&authority, fp);
                count += 1;
            }
            None => tracing::warn!(%authority, "ignoring a malformed pinned fingerprint"),
        }
    }
    Ok(count)
}

/// Writes every pin of `trust` to `known_servers.toml`.
pub fn save_known_servers(paths: &ConfigPaths, trust: &TrustStore) -> Result<(), SettingsError> {
    let file = KnownServersFile {
        servers: trust
            .entries()
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect(),
    };
    write_toml(&paths.known_servers_file(), &file)
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, SettingsError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| SettingsError::Parse {
            path: path.to_owned(),
            message: e.to_string(),
        })
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), SettingsError> {
    let io_err = |source| SettingsError::Io {
        path: path.to_owned(),
        source,
    };
    let text = toml::to_string_pretty(value).map_err(|e| SettingsError::Parse {
        path: path.to_owned(),
        message: e.to_string(),
    })?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(io_err)?;
    fs::rename(&tmp, path).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xen-core-settings-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let paths = ConfigPaths::at(temp_dir("roundtrip"));
        assert_eq!(load_settings(&paths).unwrap(), Settings::default());
        let mut settings = Settings::default();
        settings.preferences.save_passwords = false;
        settings.upsert_server(SavedServer {
            address: "10.0.0.1".into(),
            username: "root".into(),
            reconnect: true,
        });
        settings.upsert_server(SavedServer {
            address: "10.0.0.1".into(),
            username: "admin".into(),
            reconnect: false,
        });
        save_settings(&paths, &settings).unwrap();
        let text = fs::read_to_string(paths.settings_file()).unwrap();
        assert!(text.contains("[[server]]"), "{text}");
        let loaded = load_settings(&paths).unwrap();
        assert_eq!(loaded, settings);
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(loaded.server("10.0.0.1").unwrap().username, "admin");
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn partial_and_broken_files() {
        let paths = ConfigPaths::at(temp_dir("partial"));
        fs::create_dir_all(&paths.dir).unwrap();
        fs::write(
            paths.settings_file(),
            "[preferences]\nrestore_connections = false\n",
        )
        .unwrap();
        let s = load_settings(&paths).unwrap();
        assert!(!s.preferences.restore_connections);
        assert!(s.preferences.save_passwords, "missing keys take defaults");
        fs::write(paths.settings_file(), "not = [toml").unwrap();
        assert!(matches!(
            load_settings(&paths),
            Err(SettingsError::Parse { .. })
        ));
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn known_servers_round_trip() {
        let paths = ConfigPaths::at(temp_dir("known"));
        let trust = TrustStore::new();
        assert_eq!(load_known_servers(&paths, &trust).unwrap(), 0);
        trust.trust("10.0.0.1:443", Fingerprint([1; 32]));
        trust.trust("[fe80::1]:443", Fingerprint([2; 32]));
        save_known_servers(&paths, &trust).unwrap();
        let loaded = TrustStore::new();
        assert_eq!(load_known_servers(&paths, &loaded).unwrap(), 2);
        assert_eq!(loaded.entries(), trust.entries());
        let _ = fs::remove_dir_all(&paths.dir);
    }

    #[test]
    fn portable_marker_and_override() {
        let exe_dir = temp_dir("portable");
        fs::create_dir_all(&exe_dir).unwrap();
        let user = ConfigPaths::resolve_with("xen-core-test", None, Some(&exe_dir)).unwrap();
        assert_eq!(user.location, ConfigLocation::User);
        fs::write(exe_dir.join(PORTABLE_MARKER), "").unwrap();
        let portable = ConfigPaths::resolve_with("xen-core-test", None, Some(&exe_dir)).unwrap();
        assert_eq!(portable.location, ConfigLocation::Portable);
        assert_eq!(portable.dir, exe_dir.join("settings"));
        let forced =
            ConfigPaths::resolve_with("xen-core-test", Some("/x/y".into()), Some(&exe_dir))
                .unwrap();
        assert_eq!(forced, ConfigPaths::at("/x/y"));
        let _ = fs::remove_dir_all(&exe_dir);
    }
}
