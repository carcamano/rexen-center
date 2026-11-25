//! UI-independent core of the management console.
//!
//! * [`model`]: the live object cache of one connection ([`ConnectionModel`],
//!   one map per XenAPI class the event loop delivers) and the infrastructure
//!   tree derived from it.
//! * [`objects`]: facts derived from cached objects (VM home server, SR
//!   broken, host memory, NIC and network names…) and [`format`](mod@format): sizes and
//!   durations as the original console shows them.
//! * [`backend`]: the asynchronous connection manager. It runs on a tokio
//!   runtime outside the UI thread, takes [`Command`]s over an mpsc channel,
//!   publishes an immutable [`AppState`] snapshot (an `Arc`, swapped
//!   atomically) after each batch of changes and calls a repaint callback so
//!   the UI redraws. Each connection is an actor ([`connection`]) with
//!   reconnection, heartbeat and coordinator redirect.
//! * [`actions`]: the VM and host operations and when each is offered;
//!   [`rbac`]: what the logged-in user may do.
//! * [`operations`]: the operation history behind the Events panel, and the
//!   operations themselves (run as XAPI tasks).
//! * [`settings`], [`secrets`] and [`store`]: preferences and the saved
//!   server list (TOML, with portable mode), pinned certificates, and
//!   passwords in the OS keyring.
//!
//! This crate does not depend on any UI toolkit.

pub mod actions;
pub mod backend;
pub mod connection;
pub mod format;
pub mod model;
pub mod objects;
pub mod operations;
pub mod rbac;
pub mod secrets;
pub mod settings;
pub mod store;

pub use actions::{ActionRequest, HostAction, HostMenu, VmAction};
pub use backend::{
    AppState, BackendHandle, BackendOptions, CertificatePrompt, Command, ConnectionError,
    ConnectionId, ConnectionStatus, ConnectionView, ElevationOutcome, Notification, OperationId,
    Password, RepaintFn,
};
pub use model::{
    ClassMap, ConnectionModel, ModelClass, TreeHost, TreeOptions, TreePool, TreeSr, TreeVm,
};
pub use operations::{
    Operation, OperationFailure, OperationKind, OperationObject, OperationStatus, OperationStep,
    Precheck, PrecheckResult,
};
pub use rbac::{RbacCheck, SessionRbac};
pub use secrets::{KeyringSecretStore, MemorySecretStore, SecretStore};
pub use settings::{ConfigLocation, ConfigPaths, Preferences, SavedServer, Settings};
pub use store::Store;
