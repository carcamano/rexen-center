//! UI-independent core of the management console.
//!
//! * [`model`]: the live object cache of one connection ([`ConnectionModel`],
//!   one map per XenAPI class the event loop delivers) and the infrastructure
//!   tree derived from it.
//! * [`backend`]: the asynchronous backend. It runs on a tokio runtime outside
//!   the UI thread, takes [`Command`]s over an mpsc channel, publishes an
//!   immutable [`AppState`] snapshot (an `Arc`, swapped atomically) after each
//!   batch of changes and calls a repaint callback so the UI redraws.
//!
//! This crate does not depend on any UI toolkit.

pub mod backend;
pub mod model;

pub use backend::{
    AppState, BackendHandle, BackendOptions, CertificatePrompt, Command, ConnectionError,
    ConnectionStatus, Notification, RepaintFn, VmAction,
};
pub use model::{ClassMap, ConnectionModel, ModelClass, TreeHost, TreePool, TreeVm};
