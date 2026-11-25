//! UI-independent core of the management console.
//!
//! * [`model`]: the live object cache of one connection ([`ConnectionModel`],
//!   one map per XenAPI class the event loop delivers) and the infrastructure
//!   tree derived from it.
//! * [`backend`]: the asynchronous connection manager. It runs on a tokio
//!   runtime outside the UI thread, takes [`Command`]s over an mpsc channel,
//!   publishes an immutable [`AppState`] snapshot (an `Arc`, swapped
//!   atomically) after each batch of changes and calls a repaint callback so
//!   the UI redraws. Each connection is an actor ([`connection`]) with
//!   reconnection, heartbeat and coordinator redirect.
//! * [`operations`]: the operation history behind the Events panel.
//! * [`settings`], [`secrets`] and [`store`]: preferences and the saved
//!   server list (TOML, with portable mode), pinned certificates, and
//!   passwords in the OS keyring.
//!
//! This crate does not depend on any UI toolkit.

pub mod backend;
pub mod connection;
pub mod model;
pub mod operations;
pub mod secrets;
pub mod settings;
pub mod store;

pub use backend::{
    AppState, BackendHandle, BackendOptions, CertificatePrompt, Command, ConnectionError,
    ConnectionId, ConnectionStatus, ConnectionView, Notification, OperationId, Password, RepaintFn,
    VmAction,
};
pub use model::{ClassMap, ConnectionModel, ModelClass, TreeHost, TreePool, TreeVm};
pub use operations::{Operation, OperationFailure, OperationKind, OperationStatus};
pub use secrets::{KeyringSecretStore, MemorySecretStore, SecretStore};
pub use settings::{ConfigLocation, ConfigPaths, Preferences, SavedServer, Settings};
pub use store::Store;
