//! Asynchronous backend bridging the UI and XAPI: the connection manager.
//!
//! The UI thread never awaits anything. It sends [`Command`]s through an
//! unbounded mpsc channel (non-blocking), reads the latest immutable
//! [`AppState`] snapshot once per frame ([`BackendHandle::state`], an `Arc`
//! clone), and drains discrete [`Notification`]s with
//! [`BackendHandle::try_recv`]. After publishing a new snapshot or
//! notification the backend calls the repaint callback (in the app,
//! `egui::Context::request_repaint`).
//!
//! Several servers can be connected at once; each connection is an actor
//! (see [`crate::connection`]) that logs in (asking the UI about unknown or
//! changed certificates, following `HOST_IS_SLAVE` to the pool coordinator),
//! loads the cache, follows `event.from`, runs a heartbeat and reconnects with
//! backoff when the server goes away. Connecting twice to the same server or
//! pool is refused. Servers that connect are saved (with their password in
//! the OS keyring when the user asks for it) and reconnected on the next
//! start. Operations started here, and connection events, are kept in a
//! history for the Events panel (see [`crate::operations`]).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use xapi::{ClientOptions, Endpoint, Fingerprint, Host, JsonRpcClient, Ref, Session, Sr, Task, Vm};

use crate::actions::{ActionRequest, HostAction, VmAction};
use crate::connection::{self, ConnectionSpec, Secret};
use crate::model::ConnectionModel;
use crate::operations::{
    self, Operation, OperationFailure, OperationKind, OperationStatus, Precheck, PrecheckResult,
};
use crate::rbac::{self, RbacCheck, SessionRbac};
use crate::settings::{ConfigPaths, Preferences, Settings};
use crate::store::Store;

pub use crate::connection::initial_load;

/// Callback asking the UI to redraw. Must be cheap and callable from any thread.
pub type RepaintFn = Arc<dyn Fn() + Send + Sync>;

/// Identifies one connection for its lifetime (never reused).
pub type ConnectionId = u64;

/// Identifies one entry of the operation history.
pub type OperationId = u64;

/// A password. Its `Debug` output is redacted so it cannot leak into logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(password: impl Into<String>) -> Self {
        Self(password.into())
    }

    /// The password itself. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(***)")
    }
}

impl From<String> for Password {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Password {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Requests from the UI.
#[derive(Debug)]
pub enum Command {
    /// Connect to `server` (see [`xapi::Endpoint::parse`]). With no
    /// `password`, the one saved for this server is used. With
    /// `remember_password` the password is saved in the OS keyring once the
    /// connection works (and any saved one is deleted otherwise).
    Connect {
        server: String,
        username: String,
        password: Option<Password>,
        remember_password: bool,
    },
    /// Log out and drop a connection; it is not restored on the next start.
    Disconnect { connection: ConnectionId },
    /// Try a reconnecting connection again now instead of waiting.
    RetryNow { connection: ConnectionId },
    /// Remove a server from the saved list and delete its saved password
    /// (disconnecting it first).
    Forget { address: String },
    /// Run a VM operation.
    Vm {
        connection: ConnectionId,
        vm: Ref<Vm>,
        action: VmAction,
    },
    /// Run a host operation.
    Host {
        connection: ConnectionId,
        host: Ref<Host>,
        action: HostAction,
    },
    /// Run any operation (VM, host, dismissing alerts, saving performance
    /// alerts).
    Action {
        connection: ConnectionId,
        request: ActionRequest,
    },
    /// Run an operation as another user (role elevation): log in with
    /// `username`/`password`, check that this user may run it, and run it
    /// with that session (logged out afterwards). The outcome of the check
    /// comes back as [`Notification::Elevation`] with `request_id`.
    RunElevated {
        connection: ConnectionId,
        request: ActionRequest,
        username: String,
        password: Password,
        request_id: u64,
    },
    /// Ask the server a read-only question before an operation; the answer
    /// comes back as [`Notification::Precheck`] with `id`.
    Precheck {
        connection: ConnectionId,
        id: u64,
        check: Precheck,
    },
    /// Cancel an operation of the history that runs as a XAPI task.
    CancelOperation { operation: OperationId },
    /// Cancel a XAPI task (one started by any client).
    CancelTask {
        connection: ConnectionId,
        task: Ref<Task>,
    },
    /// Open a VM's or host's console. It runs (reconnecting as needed)
    /// until the [`ConsoleHandle`](crate::console::ConsoleHandle) paired
    /// with `link` is dropped or the connection is removed.
    OpenConsole {
        connection: ConnectionId,
        target: crate::console::ConsoleTarget,
        link: crate::console::ConsoleLink,
    },
    /// Follow the performance data of a host or VM until the
    /// [`PerfHandle`](crate::performance::PerfHandle) paired with `link` is
    /// dropped or the connection is removed.
    OpenPerformance {
        connection: ConnectionId,
        target: crate::performance::PerfTarget,
        link: crate::performance::PerfLink,
    },
    /// Drop finished entries from the operation history.
    ClearFinishedOperations,
    /// Change and save the preferences.
    SetPreferences(Preferences),
    /// Log out of everything and stop; `done` is signalled when finished.
    Shutdown { done: std::sync::mpsc::Sender<()> },
}

/// Why a connection failed or ended.
#[derive(Debug)]
pub enum ConnectionError {
    /// The user declined the server certificate.
    CertificateRejected,
    /// No password was given and none is saved for this server.
    PasswordRequired,
    /// The saved password could not be read from the OS keyring.
    PasswordUnavailable(crate::secrets::SecretError),
    /// Connecting or logging in failed.
    Connect(xapi::Error),
    /// The server's API is too old.
    Unsupported(xapi::Error),
    /// This server or pool is already connected (as `existing`).
    AlreadyConnected { existing: String },
    /// `HOST_IS_SLAVE` redirects did not lead to a coordinator.
    TooManyRedirects,
    /// The connection worked and was then lost.
    Lost(xapi::Error),
}

impl ConnectionError {
    /// Whether retrying cannot help (or needs the user): wrong password,
    /// refused certificate, unsupported server, duplicate…
    ///
    /// A certificate that stopped matching while connected is not fatal: the
    /// reconnection's login shows the "certificate changed" question.
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Connect(e) | Self::Lost(e) => {
                e.is_api(xapi::error::codes::SESSION_AUTHENTICATION_FAILED)
                    || matches!(
                        e,
                        xapi::Error::InvalidEndpoint(_) | xapi::Error::TlsSetup(_)
                    )
            }
            _ => true,
        }
    }

    /// The underlying XAPI error, if any.
    pub fn xapi_error(&self) -> Option<&xapi::Error> {
        match self {
            Self::Connect(e) | Self::Unsupported(e) | Self::Lost(e) => Some(e),
            _ => None,
        }
    }
}

/// State of one connection.
#[derive(Debug, Clone)]
pub enum ConnectionStatus {
    /// Logging in and loading the cache for the first time.
    Connecting,
    Connected {
        api_version: (i64, i64),
    },
    /// The connection was lost; the cache shown is the last one received.
    Reconnecting {
        /// Failed attempts so far.
        attempt: u32,
        error: Arc<ConnectionError>,
        /// When the next attempt starts; `None` while one is running.
        retry_at: Option<std::time::Instant>,
    },
    /// Gave up (the error needs the user); the entry stays until dismissed.
    Failed {
        error: Arc<ConnectionError>,
    },
}

impl ConnectionStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// One connection as the UI sees it.
#[derive(Debug, Clone)]
pub struct ConnectionView {
    pub id: ConnectionId,
    /// The server in use, in [`Endpoint`] display form. After a
    /// `HOST_IS_SLAVE` redirect it is the pool coordinator. It is the key of
    /// the saved server list.
    pub server: String,
    pub username: String,
    pub status: ConnectionStatus,
    /// Last cache received (kept while reconnecting).
    pub model: Arc<ConnectionModel>,
    /// VMs with an operation started by this client still in flight.
    pub busy_vms: Arc<HashSet<Ref<Vm>>>,
    /// Hosts with an operation started by this client still in flight.
    pub busy_hosts: Arc<HashSet<Ref<Host>>>,
    /// SRs with an operation started by this client still in flight.
    pub busy_srs: Arc<HashSet<Ref<Sr>>>,
    /// What the session may do (read after login; `None` until then or
    /// when the server could not tell).
    pub rbac: Option<Arc<SessionRbac>>,
    /// UUID of the pool, once loaded (duplicate detection).
    pub pool_uuid: Option<String>,
    /// `host:port` in use (duplicate detection).
    pub authority: String,
}

/// Immutable snapshot published to the UI.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Connections, in the order they were made.
    pub connections: Vec<Arc<ConnectionView>>,
    /// Operation history, oldest first.
    pub operations: Arc<Vec<Arc<Operation>>>,
    /// Preferences and saved servers.
    pub settings: Arc<Settings>,
    /// Where the settings live (`None` when they are kept in memory only).
    pub config: Option<ConfigPaths>,
}

impl AppState {
    /// The connection `id`.
    pub fn connection(&self, id: ConnectionId) -> Option<&Arc<ConnectionView>> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// The connection currently using saved address `server`.
    pub fn connection_to(&self, server: &str) -> Option<&Arc<ConnectionView>> {
        self.connections.iter().find(|c| c.server == server)
    }

    /// The operation `id`.
    pub fn operation(&self, id: OperationId) -> Option<&Arc<Operation>> {
        self.operations.iter().find(|o| o.id == id)
    }
}

/// Discrete events for the UI, drained with [`BackendHandle::try_recv`].
#[derive(Debug)]
pub enum Notification {
    /// The server presented an untrusted certificate; the UI must ask the user.
    CertificatePrompt(CertificatePrompt),
    /// A VM or host operation failed.
    ActionFailed {
        connection: ConnectionId,
        operation: OperationId,
        kind: OperationKind,
        error: Arc<xapi::Error>,
    },
    /// The outcome of the credentials given for [`Command::RunElevated`].
    Elevation {
        request_id: u64,
        outcome: ElevationOutcome,
    },
    /// The answer to [`Command::Precheck`].
    Precheck {
        connection: ConnectionId,
        id: u64,
        result: PrecheckResult,
    },
    /// A connection could not even start (bad address, already connected…).
    ConnectionRefused {
        server: String,
        error: Arc<ConnectionError>,
    },
}

/// What became of the credentials given to run an operation as another user.
#[derive(Debug, Clone)]
pub enum ElevationOutcome {
    /// The user may run it; it has started (as `operation`).
    Authorized { operation: OperationId },
    /// The login worked but that user's roles do not allow the operation.
    NotAuthorized,
    /// The login failed (wrong password, unknown user…).
    LoginFailed(Arc<xapi::Error>),
    /// The server is not connected.
    NotConnected,
}

/// A pending "trust this certificate?" question.
#[derive(Debug)]
pub struct CertificatePrompt {
    pub connection: ConnectionId,
    pub server: String,
    pub fingerprint: Fingerprint,
    /// Fingerprint trusted before for this server, if the certificate changed.
    pub previous: Option<Fingerprint>,
    pub(crate) reply: oneshot::Sender<bool>,
}

impl CertificatePrompt {
    /// Trust the certificate (it is saved) and continue connecting.
    pub fn accept(self) {
        let _ = self.reply.send(true);
    }

    /// Refuse the certificate and abandon the connection.
    pub fn reject(self) {
        let _ = self.reply.send(false);
    }

    /// The connection that asked has gone away; nobody waits for an answer.
    pub fn is_stale(&self) -> bool {
        self.reply.is_closed()
    }
}

/// Backend configuration.
#[derive(Clone)]
pub struct BackendOptions {
    /// Originator string sent on login (shows up in the server's logs).
    pub originator: String,
    /// Settings, pinned certificates and saved passwords.
    pub store: Arc<Store>,
    /// Server-side `event.from` timeout, seconds. The HTTP timeout of the
    /// event client is always longer (see [`event_client_timeout`]).
    pub event_timeout: f64,
    /// Timeout of ordinary calls (login, loading, operations).
    pub call_timeout: Duration,
    /// Minimum interval between two published snapshots (coalescing).
    pub publish_interval: Duration,
    /// Interval between heartbeats (`host.get_servertime`).
    pub heartbeat_interval: Duration,
    /// Timeout of one heartbeat. Two failed heartbeats in a row mean the
    /// connection is lost.
    pub heartbeat_timeout: Duration,
    /// Delay before the first reconnection attempt; it doubles after each
    /// failed attempt up to `max_reconnect_delay`.
    pub reconnect_delay: Duration,
    pub max_reconnect_delay: Duration,
    /// Interval between `task.get_record` polls of a running operation.
    pub task_poll_interval: Duration,
    /// Reconnect the saved servers marked to reconnect when the backend
    /// starts (and the preference allows it).
    pub restore_connections: bool,
    /// Interval between two `rrd_updates` requests of an open Performance
    /// tab (the original's 5 s).
    pub perf_interval: Duration,
    /// After saving performance alerts, ask perfmon on the hosts concerned
    /// to read its configuration again (`host.call_plugin perfmon
    /// refresh`), as the original does.
    pub perfmon_refresh: bool,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            originator: "xen-core".to_owned(),
            store: Arc::new(Store::default()),
            event_timeout: 60.0,
            call_timeout: Duration::from_secs(60),
            publish_interval: Duration::from_millis(100),
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(15),
            reconnect_delay: Duration::from_secs(2),
            max_reconnect_delay: Duration::from_secs(60),
            task_poll_interval: Duration::from_millis(500),
            restore_connections: true,
            perf_interval: Duration::from_secs(5),
            perfmon_refresh: true,
        }
    }
}

/// HTTP timeout for `event.from` long-polls with a server-side timeout of
/// `server_timeout` seconds: always longer, so the server answers (with an
/// empty batch) before the client gives up.
pub fn event_client_timeout(server_timeout: f64) -> Duration {
    Duration::from_secs_f64(server_timeout.max(0.0) * 1.1 + 30.0)
}

/// The UI's end of the backend.
pub struct BackendHandle {
    commands: mpsc::UnboundedSender<Command>,
    notifications: mpsc::UnboundedReceiver<Notification>,
    shared: Arc<Shared>,
}

impl BackendHandle {
    /// Starts the backend on `runtime`, and reconnects the saved servers
    /// marked to reconnect if the preferences ask for it.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        options: BackendOptions,
        repaint: RepaintFn,
    ) -> Self {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (notify_tx, notifications) = mpsc::unbounded_channel();
        let settings = options.store.settings();
        let initial = AppState {
            settings: Arc::new(settings.clone()),
            config: options.store.paths().cloned(),
            ..AppState::default()
        };
        let restore = options.restore_connections && settings.preferences.restore_connections;
        let shared = Arc::new(Shared {
            state: ArcSwap::from_pointee(initial),
            update_lock: Mutex::new(()),
            next_id: AtomicU64::new(1),
            repaint,
            notify_tx,
            options,
        });
        runtime.spawn(run(shared.clone(), command_rx));
        let handle = Self {
            commands,
            notifications,
            shared,
        };
        if restore {
            for server in settings.servers.iter().filter(|s| s.reconnect) {
                tracing::info!(server = %server.address, "restoring connection");
                handle.send(Command::Connect {
                    server: server.address.clone(),
                    username: server.username.clone(),
                    password: None,
                    remember_password: true,
                });
            }
        }
        handle
    }

    /// Sends a command (never blocks).
    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::warn!("backend stopped; command dropped");
        }
    }

    /// The latest published snapshot.
    pub fn state(&self) -> Arc<AppState> {
        self.shared.state.load_full()
    }

    /// Next pending notification, if any (never blocks).
    pub fn try_recv(&mut self) -> Option<Notification> {
        self.notifications.try_recv().ok()
    }

    /// Waits for the next notification (for async callers such as tests).
    pub async fn recv(&mut self) -> Option<Notification> {
        self.notifications.recv().await
    }

    /// Logs out and stops the backend, waiting at most `timeout`. Blocking;
    /// call from a thread that is not running the tokio runtime.
    pub fn shutdown_blocking(&self, timeout: Duration) {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Command::Shutdown { done });
        let _ = wait.recv_timeout(timeout);
    }
}

/// Most entries the operation history keeps (finished ones go first).
const MAX_HISTORY: usize = 500;

pub(crate) struct Shared {
    state: ArcSwap<AppState>,
    update_lock: Mutex<()>,
    next_id: AtomicU64,
    repaint: RepaintFn,
    notify_tx: mpsc::UnboundedSender<Notification>,
    pub(crate) options: BackendOptions,
}

impl Shared {
    pub(crate) fn store(&self) -> &Store {
        &self.options.store
    }

    pub(crate) fn state(&self) -> Arc<AppState> {
        self.state.load_full()
    }

    pub(crate) fn repaint_fn(&self) -> RepaintFn {
        self.repaint.clone()
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Publishes a modified copy of the state.
    pub(crate) fn update<R>(&self, f: impl FnOnce(&mut AppState) -> R) -> R {
        let result = {
            let _guard = self
                .update_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let mut next = AppState::clone(&self.state.load());
            let result = f(&mut next);
            self.state.store(Arc::new(next));
            result
        };
        (self.repaint)();
        result
    }

    /// Modifies connection `id`; `false` (and nothing published) if it is
    /// gone, so work of a closed connection never shows up.
    pub(crate) fn update_connection(
        &self,
        id: ConnectionId,
        f: impl FnOnce(&mut ConnectionView),
    ) -> bool {
        self.update(|s| match s.connections.iter_mut().find(|c| c.id == id) {
            Some(view) => {
                f(Arc::make_mut(view));
                true
            }
            None => false,
        })
    }

    /// Moves connection `id` to `endpoint`, unless another live connection
    /// already uses it; then returns that connection's server.
    pub(crate) fn claim_endpoint(
        &self,
        id: ConnectionId,
        endpoint: &Endpoint,
    ) -> Result<(), String> {
        let authority = endpoint.authority();
        self.update(|s| {
            if let Some(other) = s
                .connections
                .iter()
                .find(|c| c.id != id && !c.status.is_failed() && c.authority == authority)
            {
                return Err(other.server.clone());
            }
            if let Some(view) = s.connections.iter_mut().find(|c| c.id == id) {
                let view = Arc::make_mut(view);
                view.server = endpoint.to_string();
                view.authority = authority;
            }
            Ok(())
        })
    }

    /// Records that connection `id` reached pool `uuid`, unless another live
    /// connection already has it; then returns that connection's server.
    pub(crate) fn claim_pool(&self, id: ConnectionId, uuid: &str) -> Result<(), String> {
        self.update(|s| {
            if let Some(other) = s.connections.iter().find(|c| {
                c.id != id && !c.status.is_failed() && c.pool_uuid.as_deref() == Some(uuid)
            }) {
                return Err(other.server.clone());
            }
            if let Some(view) = s.connections.iter_mut().find(|c| c.id == id) {
                Arc::make_mut(view).pool_uuid = Some(uuid.to_owned());
            }
            Ok(())
        })
    }

    pub(crate) fn remove_connection(&self, id: ConnectionId) {
        self.update(|s| s.connections.retain(|c| c.id != id));
    }

    pub(crate) fn publish_settings(&self, settings: Settings) {
        self.update(|s| s.settings = Arc::new(settings));
    }

    pub(crate) fn notify(&self, notification: Notification) {
        let _ = self.notify_tx.send(notification);
        (self.repaint)();
    }

    /// Adds a running entry to the operation history.
    pub(crate) fn start_operation(
        &self,
        connection: Option<ConnectionId>,
        server: &str,
        kind: OperationKind,
    ) -> OperationId {
        let id = self.next_id();
        let operation = Operation {
            id,
            connection,
            server: server.to_owned(),
            kind,
            status: OperationStatus::Running,
            progress: None,
            task: None,
            object: None,
            step: None,
            elevated_as: None,
            created: None,
            notice: None,
            started: SystemTime::now(),
            finished: None,
        };
        self.update(|s| {
            let ops = Arc::make_mut(&mut s.operations);
            ops.push(Arc::new(operation));
            if ops.len() > MAX_HISTORY {
                // Drop the oldest finished entries first.
                let excess = ops.len() - MAX_HISTORY;
                let mut dropped = 0;
                ops.retain(|o| {
                    if dropped < excess && !o.is_running() {
                        dropped += 1;
                        false
                    } else {
                        true
                    }
                });
            }
        });
        id
    }

    pub(crate) fn update_operation(&self, id: OperationId, f: impl FnOnce(&mut Operation)) {
        self.update(|s| {
            let ops = Arc::make_mut(&mut s.operations);
            if let Some(op) = ops.iter_mut().find(|o| o.id == id) {
                f(Arc::make_mut(op));
            }
        });
    }

    /// Ends a running operation; one that already ended keeps its outcome
    /// (e.g. an operation failed by a disconnect whose task then errors).
    pub(crate) fn finish_operation(&self, id: OperationId, status: OperationStatus) {
        self.update_operation(id, |op| {
            if !op.is_running() {
                return;
            }
            if matches!(status, OperationStatus::Succeeded) {
                op.progress = op.progress.map(|_| 1.0);
            }
            op.status = status;
            op.finished = Some(SystemTime::now());
        });
    }
}

pub(crate) type SessionSlot = Arc<Mutex<Option<Session>>>;

pub(crate) fn slot_get(slot: &SessionSlot) -> Option<Session> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub(crate) fn slot_set(slot: &SessionSlot, session: Option<Session>) -> Option<Session> {
    std::mem::replace(
        &mut *slot.lock().unwrap_or_else(PoisonError::into_inner),
        session,
    )
}

/// Logs `session` out in the background (best effort, 5 s at most).
pub(crate) fn logout_later(session: Session) -> JoinHandle<()> {
    tokio::spawn(async move {
        let server = session.client().endpoint().to_string();
        match tokio::time::timeout(Duration::from_secs(5), session.logout()).await {
            Ok(Err(e)) => tracing::debug!(%server, error = %e, "logout failed"),
            Err(_) => tracing::debug!(%server, "logout timed out"),
            Ok(Ok(())) => tracing::info!(%server, "logged out"),
        }
    })
}

struct ActiveConnection {
    task: JoinHandle<()>,
    session: SessionSlot,
    wake: Arc<Notify>,
}

impl ActiveConnection {
    /// Stops the connection task and logs out in the background.
    fn close(self) -> Option<JoinHandle<()>> {
        self.task.abort();
        slot_set(&self.session, None).map(logout_later)
    }
}

async fn run(shared: Arc<Shared>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut active: HashMap<ConnectionId, ActiveConnection> = HashMap::new();
    while let Some(command) = commands.recv().await {
        active.retain(|_, c| !c.task.is_finished());
        match command {
            Command::Connect {
                server,
                username,
                password,
                remember_password,
            } => connect(
                &shared,
                &mut active,
                server,
                username,
                password,
                remember_password,
            ),
            Command::Disconnect { connection } => {
                disconnect(&shared, &mut active, connection);
            }
            Command::RetryNow { connection } => {
                if let Some(c) = active.get(&connection) {
                    c.wake.notify_one();
                }
            }
            Command::Forget { address } => {
                let state = shared.state();
                if let Some(view) = state.connection_to(&address) {
                    disconnect(&shared, &mut active, view.id);
                }
                // The keyring may block (or ask the user): off the loop.
                let shared = shared.clone();
                tokio::spawn(async move {
                    let store = shared.options.store.clone();
                    if let Ok(settings) =
                        tokio::task::spawn_blocking(move || store.forget_server(&address)).await
                    {
                        shared.publish_settings(settings);
                    }
                });
            }
            Command::Vm {
                connection,
                vm,
                action,
            } => {
                let session = active.get(&connection).and_then(|c| slot_get(&c.session));
                let request = ActionRequest::Vm { vm, action };
                operations::start_action(&shared, connection, session, request, None);
            }
            Command::Host {
                connection,
                host,
                action,
            } => {
                let session = active.get(&connection).and_then(|c| slot_get(&c.session));
                let request = ActionRequest::Host { host, action };
                operations::start_action(&shared, connection, session, request, None);
            }
            Command::Action {
                connection,
                request,
            } => {
                let session = active.get(&connection).and_then(|c| slot_get(&c.session));
                operations::start_action(&shared, connection, session, request, None);
            }
            Command::RunElevated {
                connection,
                request,
                username,
                password,
                request_id,
            } => {
                let session = active.get(&connection).and_then(|c| slot_get(&c.session));
                run_elevated(
                    &shared, connection, session, request, username, password, request_id,
                );
            }
            Command::Precheck {
                connection,
                id,
                check,
            } => match active.get(&connection).and_then(|c| slot_get(&c.session)) {
                Some(session) => operations::run_precheck(&shared, session, connection, id, check),
                None => tracing::debug!("precheck ignored: not connected"),
            },
            Command::CancelOperation { operation } => {
                let state = shared.state();
                let target = state
                    .operation(operation)
                    .and_then(|op| Some((op.connection?, op.task.clone()?)));
                if let Some((connection, task)) = target
                    && let Some(session) =
                        active.get(&connection).and_then(|c| slot_get(&c.session))
                {
                    operations::cancel_task(&shared, connection, session, task, false);
                }
            }
            Command::CancelTask { connection, task } => {
                match active.get(&connection).and_then(|c| slot_get(&c.session)) {
                    Some(session) => {
                        operations::cancel_task(&shared, connection, session, task, true);
                    }
                    None => tracing::debug!("task cancel ignored: not connected"),
                }
            }
            Command::OpenConsole {
                connection,
                target,
                link,
            } => match active.get(&connection) {
                Some(c) => {
                    let env = crate::console::ConsoleEnv {
                        shared: shared.clone(),
                        connection,
                        session: c.session.clone(),
                    };
                    tokio::spawn(crate::console::run(env, target, link));
                }
                None => tracing::debug!("console ignored: no such connection"),
            },
            Command::OpenPerformance {
                connection,
                target,
                link,
            } => match active.get(&connection) {
                Some(c) => {
                    let env = crate::performance::PerfEnv {
                        shared: shared.clone(),
                        connection,
                        session: c.session.clone(),
                    };
                    tokio::spawn(crate::performance::run(env, target, link));
                }
                None => tracing::debug!("performance ignored: no such connection"),
            },
            Command::ClearFinishedOperations => shared.update(|s| {
                Arc::make_mut(&mut s.operations).retain(|o| o.is_running());
            }),
            Command::SetPreferences(preferences) => {
                let settings = shared.store().set_preferences(preferences);
                shared.publish_settings(settings);
            }
            Command::Shutdown { done } => {
                close_all(&mut active).await;
                let _ = done.send(());
                return;
            }
        }
    }
    close_all(&mut active).await;
}

async fn close_all(active: &mut HashMap<ConnectionId, ActiveConnection>) {
    let logouts: Vec<_> = active.drain().filter_map(|(_, c)| c.close()).collect();
    for logout in logouts {
        let _ = logout.await;
    }
}

fn connect(
    shared: &Arc<Shared>,
    active: &mut HashMap<ConnectionId, ActiveConnection>,
    server: String,
    username: String,
    password: Option<Password>,
    remember_password: bool,
) {
    let refuse = |label: String, error: ConnectionError| {
        let error = Arc::new(error);
        let op = shared.start_operation(None, &label, OperationKind::Connect);
        shared.finish_operation(
            op,
            OperationStatus::Failed(OperationFailure::Connection(error.clone())),
        );
        shared.notify(Notification::ConnectionRefused {
            server: label,
            error,
        });
    };
    let endpoint = match Endpoint::parse(&server) {
        Ok(e) => e,
        Err(e) => return refuse(server, ConnectionError::Connect(e)),
    };
    let label = endpoint.to_string();
    let authority = endpoint.authority();

    // A failed entry for the same server is replaced; a live one is kept.
    let state = shared.state();
    if let Some(existing) = state.connections.iter().find(|c| c.authority == authority) {
        if existing.status.is_failed() {
            if let Some(old) = active.remove(&existing.id) {
                old.close();
            }
            shared.remove_connection(existing.id);
        } else {
            let existing = existing.server.clone();
            return refuse(label, ConnectionError::AlreadyConnected { existing });
        }
    }

    let id = shared.next_id();
    shared.update(|s| {
        s.connections.push(Arc::new(ConnectionView {
            id,
            server: label.clone(),
            username: username.clone(),
            status: ConnectionStatus::Connecting,
            model: Arc::default(),
            busy_vms: Arc::default(),
            busy_hosts: Arc::default(),
            busy_srs: Arc::default(),
            rbac: None,
            pool_uuid: None,
            authority,
        }));
    });
    let session: SessionSlot = Arc::default();
    let wake = Arc::new(Notify::new());
    let spec = ConnectionSpec {
        id,
        endpoint,
        typed_address: label,
        username,
        secret: match password {
            Some(p) => Secret::Given(p),
            None => Secret::Saved,
        },
        remember_password,
    };
    let task = tokio::spawn(connection::run(
        shared.clone(),
        spec,
        session.clone(),
        wake.clone(),
    ));
    active.insert(
        id,
        ActiveConnection {
            task,
            session,
            wake,
        },
    );
}

/// Logs in as another user on the same server, checks that user may run
/// `request`, then runs it with that session.
fn run_elevated(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    session: Option<Session>,
    request: ActionRequest,
    username: String,
    password: Password,
    request_id: u64,
) {
    let reply = move |shared: &Shared, outcome| {
        shared.notify(Notification::Elevation {
            request_id,
            outcome,
        });
    };
    let Some(session) = session else {
        return reply(shared, ElevationOutcome::NotConnected);
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        let client = session.client();
        let options = ClientOptions {
            timeout: shared.options.call_timeout,
            ..ClientOptions::default()
        };
        let login = async {
            let client = JsonRpcClient::new(client.endpoint(), shared.store().trust(), &options)?;
            Session::login_with_password(
                Arc::new(client),
                &username,
                password.expose(),
                &shared.options.originator,
            )
            .await
        };
        let elevated = match login.await {
            Ok(s) => s,
            Err(e) => {
                tracing::info!(error = %e, "role elevation: login failed");
                return reply(&shared, ElevationOutcome::LoginFailed(Arc::new(e)));
            }
        };
        let rights = match rbac::fetch(&elevated).await {
            Ok(r) => r,
            Err(e) => {
                logout_later(elevated);
                return reply(&shared, ElevationOutcome::LoginFailed(Arc::new(e)));
            }
        };
        let model = shared
            .state()
            .connection(connection)
            .map(|v| v.model.clone())
            .unwrap_or_default();
        if let RbacCheck::Denied { .. } =
            rbac::check(&model, Some(&rights), &request.rbac_methods())
        {
            tracing::info!("role elevation: that user is not authorized either");
            logout_later(elevated);
            return reply(&shared, ElevationOutcome::NotAuthorized);
        }
        let operation =
            operations::start_action(&shared, connection, Some(elevated), request, Some(username));
        reply(&shared, ElevationOutcome::Authorized { operation });
    });
}

fn disconnect(
    shared: &Arc<Shared>,
    active: &mut HashMap<ConnectionId, ActiveConnection>,
    id: ConnectionId,
) {
    if let Some(old) = active.remove(&id) {
        old.close();
    }
    let state = shared.state();
    let Some(view) = state.connection(id) else {
        return;
    };
    // Anything still running for this connection ends with it.
    let running: Vec<OperationId> = state
        .operations
        .iter()
        .filter(|o| o.connection == Some(id) && o.is_running())
        .map(|o| o.id)
        .collect();
    for op in running {
        shared.finish_operation(op, OperationStatus::Failed(OperationFailure::Disconnected));
    }
    shared.remove_connection(id);
    if shared.state().settings.server(&view.server).is_some() {
        let settings = shared.store().set_reconnect(&view.server, false);
        shared.publish_settings(settings);
    }
    tracing::info!(server = %view.server, "disconnected");
}
