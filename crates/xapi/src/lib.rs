//! XenAPI (XAPI) client.
//!
//! The crate is split in two layers so the F2 code generator can replace the
//! API surface without touching the transport:
//!
//! * **Core (hand-written, stable):** [`endpoint`], [`tls`] (trust-on-first-use
//!   certificate pinning), [`transport`] (JSON-RPC 2.0 over HTTPS to
//!   `/jsonrpc`), [`session`], [`event`] (`event.from`), [`error`], [`refs`]
//!   (typed `OpaqueRef`s) and the [`xapi_enum!`] helper.
//! * **API surface ([`generated`]):** record structs, enums and per-class call
//!   wrappers. In phase F1 this module is hand-written and covers only what the
//!   base app needs; in phase F2 it is generated from `xenapi.json`.
//!
//! This crate has no UI dependency.

#[macro_use]
mod macros;

pub mod endpoint;
pub mod error;
pub mod event;
pub mod generated;
pub mod refs;
pub mod session;
pub mod tls;
pub mod transport;

pub use endpoint::Endpoint;
pub use error::{Error, Result};
pub use event::{Event, EventBatch, EventOperation};
pub use generated::*;
pub use refs::Ref;
pub use session::Session;
pub use tls::{Fingerprint, TrustStore};
pub use transport::{ClientOptions, JsonRpcClient};

/// XenAPI version this client speaks (sent on login). 2.21 = XCP-ng 8.3.
pub const API_VERSION: &str = "2.21";

/// Oldest supported XAPI API version (major, minor). 2.16 = XCP-ng 8.2.
pub const MIN_API_VERSION: (i64, i64) = (2, 16);
