//! Error type shared by every XAPI call.

use crate::tls::Fingerprint;

/// Result alias for XAPI operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// XAPI error codes the client treats specially.
pub mod codes {
    pub const SESSION_AUTHENTICATION_FAILED: &str = "SESSION_AUTHENTICATION_FAILED";
    pub const SESSION_INVALID: &str = "SESSION_INVALID";
    pub const HOST_IS_SLAVE: &str = "HOST_IS_SLAVE";
    pub const HOST_STILL_BOOTING: &str = "HOST_STILL_BOOTING";
    pub const RBAC_PERMISSION_DENIED: &str = "RBAC_PERMISSION_DENIED";
    pub const VM_BAD_POWER_STATE: &str = "VM_BAD_POWER_STATE";
    pub const EVENTS_LOST: &str = "EVENTS_LOST";
    pub const MESSAGE_METHOD_UNKNOWN: &str = "MESSAGE_METHOD_UNKNOWN";
    pub const HOST_UNKNOWN_TO_MASTER: &str = "HOST_UNKNOWN_TO_MASTER";
    pub const TASK_CANCELLED: &str = "TASK_CANCELLED";
    pub const HANDLE_INVALID: &str = "HANDLE_INVALID";
}

/// Everything that can go wrong talking to a XAPI server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered with a XAPI failure (`error.message` = code,
    /// `error.data` = parameters).
    #[error("{code}{}", format_params(.params))]
    Api { code: String, params: Vec<String> },

    /// The server presented a TLS certificate that is not trusted yet.
    /// The caller may show `fingerprint` to the user and retry after trusting it.
    #[error("the certificate presented by {host} is not trusted (SHA-256 {fingerprint})")]
    UntrustedCertificate {
        host: String,
        fingerprint: Fingerprint,
    },

    /// Network/HTTP failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The response could not be decoded into the expected type.
    #[error("could not decode the response of {method}: {source}")]
    Decode {
        method: String,
        #[source]
        source: serde_json::Error,
    },

    /// The response did not follow the JSON-RPC protocol.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The connected server is older than [`crate::MIN_API_VERSION`].
    #[error("server API version {major}.{minor} is not supported (requires {}.{} or later)", crate::MIN_API_VERSION.0, crate::MIN_API_VERSION.1)]
    UnsupportedApiVersion { major: i64, minor: i64 },

    /// The endpoint string could not be parsed.
    #[error("invalid server address: {0}")]
    InvalidEndpoint(String),

    /// TLS configuration could not be built.
    #[error("TLS setup failed: {0}")]
    TlsSetup(#[from] rustls::Error),

    /// A local file of a transfer could not be read or written.
    #[error("file error: {0}")]
    Io(#[source] std::io::Error),

    /// The user cancelled a transfer.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// The XAPI error code, if this is an API failure.
    pub fn api_code(&self) -> Option<&str> {
        match self {
            Error::Api { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Whether this is the API failure `code`.
    pub fn is_api(&self, code: &str) -> bool {
        self.api_code() == Some(code)
    }

    /// Whether the server could not be reached or stopped answering
    /// (connection refused or reset, timeout, garbled HTTP): the kind of
    /// failure a later retry can cure. API failures, certificate problems and
    /// bad addresses are not.
    pub fn is_unreachable(&self) -> bool {
        match self {
            Error::Transport(_) | Error::Protocol(_) => true,
            Error::Api { code, .. } => code == codes::HOST_STILL_BOOTING,
            _ => false,
        }
    }
}

fn format_params(params: &[String]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!(" ({})", params.join(", "))
    }
}
