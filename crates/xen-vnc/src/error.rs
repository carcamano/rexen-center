//! Errors of the console client.

/// Result alias of this crate.
pub type Result<T, E = VncError> = std::result::Result<T, E>;

/// Why a console connection could not be made or ended.
#[derive(Debug, thiserror::Error)]
pub enum VncError {
    /// Network failure, or the server closed the stream.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// `console.location` is not a URL this client understands.
    #[error("invalid console location: {0}")]
    Location(String),
    /// TLS handshake or configuration failure (including a certificate
    /// that is not trusted).
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),
    /// The server refused the `CONNECT` (for example 404 when the console is
    /// gone, 403 when the session may not open it).
    #[error("the server refused the console: HTTP {status} {reason}")]
    Refused { status: u16, reason: String },
    /// The server broke the RFB protocol (or speaks a part of it this client
    /// does not).
    #[error("VNC protocol error: {0}")]
    Protocol(String),
    /// The server ended the session before it started (RFB security failure).
    #[error("the console server refused the session: {0}")]
    Rejected(String),
    /// A step took too long.
    #[error("timed out")]
    Timeout,
}

impl VncError {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }
}
