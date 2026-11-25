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
//! * [`console`]: the VM and host consoles (VNC through the XAPI tunnel,
//!   reconnecting while the VM runs).
//! * [`performance`]: the performance data of a host or VM (XAPI's RRDs,
//!   through [`xen_rrd`]) and the default graphs; [`alerts`]: the XAPI
//!   messages shown as alerts; [`perfmon`]: the performance alert
//!   configuration (`other_config["perfmon"]`).
//! * [`operations`]: the operation history behind the Events panel, and the
//!   operations themselves (run as XAPI tasks).
//! * [`provisioning`]: VM creation, templates, snapshots and disks (the
//!   New VM wizard's request, when each operation is offered, the ISO and
//!   disk lists); [`provision_run`]: how each runs, call for call as the
//!   original's actions.
//! * [`storage`]: storage repositories (the New SR wizard's request, when
//!   each SR operation is offered, the probe answers); [`storage_run`]: how
//!   each runs, call for call as the original's actions.
//! * [`settings`], [`secrets`] and [`store`]: preferences and the saved
//!   server list (TOML, with portable mode), pinned certificates, and
//!   passwords in the OS keyring.
//!
//! This crate does not depend on any UI toolkit.

pub mod actions;
pub mod alerts;
pub mod backend;
pub mod connection;
pub mod console;
pub mod format;
pub mod model;
pub mod network;
pub mod network_run;
pub mod objects;
pub mod operations;
pub mod perfmon;
pub mod performance;
pub mod pool;
pub mod pool_run;
pub mod properties;
pub mod provision_run;
pub mod provisioning;
pub mod rbac;
pub mod secrets;
pub mod settings;
pub mod storage;
pub mod storage_run;
pub mod store;

pub use actions::{ActionRequest, HostAction, HostMenu, PerfmonObjectRef, VmAction};
pub use backend::{
    AppState, BackendHandle, BackendOptions, CertificatePrompt, Command, ConnectionError,
    ConnectionId, ConnectionStatus, ConnectionView, ElevationOutcome, Notification, OperationId,
    Password, RepaintFn,
};
pub use console::{ConsoleEvent, ConsoleHandle, ConsoleLink, ConsoleStatus, ConsoleTarget};
pub use model::{
    ClassMap, ConnectionModel, ModelClass, TreeHost, TreeOptions, TreePool, TreeSr, TreeVm,
};
pub use operations::{
    Operation, OperationFailure, OperationKind, OperationObject, OperationStatus, OperationStep,
    Precheck, PrecheckResult,
};
pub use performance::{PerfData, PerfHandle, PerfLink, PerfStatus, PerfTarget};
pub use rbac::{RbacCheck, SessionRbac};
pub use secrets::{KeyringSecretStore, MemorySecretStore, SecretStore};
pub use settings::{ConfigLocation, ConfigPaths, Preferences, SavedServer, Settings};
pub use store::Store;
