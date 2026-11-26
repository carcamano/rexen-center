//! The MCP server's configuration file, `mcp.toml` in the configuration
//! folder (next to `settings.toml`):
//!
//! ```toml
//! # Saved servers to connect to (addresses as in the server list).
//! # Empty or absent: every saved server.
//! servers = ["10.0.0.1"]
//!
//! [writes]
//! # Write tools are off unless this is true.
//! enabled = false
//!
//! # Per server, the only VMs the write tools may touch: UUIDs or exact
//! # names. A server without an entry allows no writes.
//! [writes.allow]
//! "10.0.0.1" = ["test-vm-01", "0b0b0b0b-0000-4000-8000-000000000003"]
//! ```
//!
//! A missing file means the defaults: every saved server, read only. An
//! unknown key is an error, so a misspelt switch (`enable = true`) does not
//! silently leave the server in another mode than intended.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use xapi::Endpoint;

/// The file name, in the configuration folder.
pub const CONFIG_FILE: &str = "mcp.toml";

/// Contents of `mcp.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Saved servers to connect to; empty means all of them.
    pub servers: Vec<String>,
    pub writes: WriteConfig,
    /// How long a tool waits for a server that is still connecting.
    pub connect_wait_seconds: u64,
    /// How long a write tool waits for its operation to finish.
    pub operation_timeout_seconds: u64,
    /// How long the performance tool waits for the data to load.
    pub performance_wait_seconds: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            writes: WriteConfig::default(),
            connect_wait_seconds: 30,
            operation_timeout_seconds: 600,
            performance_wait_seconds: 60,
        }
    }
}

/// The write switch and the allow-list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WriteConfig {
    pub enabled: bool,
    /// Server address → VM UUIDs or names.
    pub allow: BTreeMap<String, Vec<String>>,
}

/// Why `mcp.toml` could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl McpConfig {
    /// Reads `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConfigError(format!("{}: {e}", path.display()))),
        };
        Self::parse(&text).map_err(|e| ConfigError(format!("{}: {}", path.display(), e.0)))
    }

    /// Parses the file's text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError(e.to_string()))
    }

    /// Whether `address` (a saved server address) is one to connect to.
    pub fn selects(&self, address: &str) -> bool {
        self.servers.is_empty() || self.servers.iter().any(|s| same_server(s, address))
    }

    /// The allow-list entries of server `address`: the entry whose key
    /// names the same server (any address form), if any.
    pub fn allowed_for<'a>(&'a self, addresses: &[&str]) -> Option<&'a [String]> {
        self.writes
            .allow
            .iter()
            .find(|(key, _)| addresses.iter().any(|a| same_server(key, a)))
            .map(|(_, list)| list.as_slice())
    }
}

/// Whether two addresses name the same server (`10.0.0.1`,
/// `https://10.0.0.1:443` and `10.0.0.1:443` do).
pub fn same_server(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(address: &str) -> String {
    Endpoint::parse(address.trim()).map_or_else(|_| address.trim().to_owned(), |e| e.base_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_read_only_and_select_everything() {
        let config = McpConfig::parse("").unwrap();
        assert!(!config.writes.enabled);
        assert!(config.writes.allow.is_empty());
        assert!(config.selects("10.0.0.1"));
        assert_eq!(config, McpConfig::default());
        let missing = McpConfig::load(Path::new("/nonexistent/mcp.toml")).unwrap();
        assert_eq!(missing, McpConfig::default());
    }

    #[test]
    fn parses_the_switch_and_the_allow_list() {
        let config = McpConfig::parse(
            r#"
servers = ["10.0.0.1"]
[writes]
enabled = true
[writes.allow]
"https://10.0.0.1:443" = ["test-vm", "0b0b0b0b-0000-4000-8000-000000000003"]
"#,
        )
        .unwrap();
        assert!(config.writes.enabled);
        assert!(config.selects("10.0.0.1"));
        assert!(config.selects("10.0.0.1:443"));
        assert!(!config.selects("10.0.0.2"));
        assert_eq!(config.allowed_for(&["10.0.0.1"]).unwrap().len(), 2);
        assert!(config.allowed_for(&["10.0.0.2"]).is_none());
    }

    #[test]
    fn a_misspelt_key_is_an_error() {
        let e = McpConfig::parse("[writes]\nenable = true\n").unwrap_err();
        assert!(e.0.contains("enable"), "{e}");
        assert!(McpConfig::parse("write = true\n").is_err());
    }
}
