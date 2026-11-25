//! Asynchronous backend bridging the UI and XAPI.
//!
//! The UI thread never awaits anything. It sends [`Command`]s through an
//! unbounded mpsc channel (non-blocking), reads the latest immutable
//! [`AppState`] snapshot once per frame ([`BackendHandle::state`], an `Arc`
//! clone), and drains discrete [`Notification`]s with
//! [`BackendHandle::try_recv`]. After publishing a new snapshot or
//! notification the backend calls the repaint callback (in the app,
//! `egui::Context::request_repaint`).
//!
//! Phase F1 manages a single connection; connecting again replaces it.
//! Automatic reconnection, multiple connections and persisted settings belong
//! to phase F2.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use xapi::{ClientOptions, Endpoint, Fingerprint, JsonRpcClient, Ref, Session, TrustStore, Vm};

use crate::model::{ConnectionModel, EVENT_CLASSES};

/// Callback asking the UI to redraw. Must be cheap and callable from any thread.
pub type RepaintFn = Arc<dyn Fn() + Send + Sync>;

/// Requests from the UI.
#[derive(Debug)]
pub enum Command {
    /// Connect to `server` (see [`xapi::Endpoint::parse`]), replacing any
    /// current connection.
    Connect {
        server: String,
        username: String,
        password: String,
    },
    /// Log out and drop the current connection.
    Disconnect,
    /// Run a VM power operation.
    Vm { vm: Ref<Vm>, action: VmAction },
    /// Log out and stop; `done` is signalled when finished.
    Shutdown { done: std::sync::mpsc::Sender<()> },
}

/// VM operations available in phase F1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmAction {
    Start,
    CleanShutdown,
}

/// Why a connection failed or ended.
#[derive(Debug)]
pub enum ConnectionError {
    /// The user declined the server certificate.
    CertificateRejected,
    /// Connecting or logging in failed.
    Connect(xapi::Error),
    /// The server's API is too old.
    Unsupported(xapi::Error),
    /// The connection worked and was then lost.
    Lost(xapi::Error),
}

/// State of the (single) connection.
#[derive(Debug, Clone, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting {
        server: String,
    },
    Connected {
        server: String,
        api_version: (i64, i64),
    },
    Failed {
        server: String,
        error: Arc<ConnectionError>,
    },
}

impl ConnectionStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Immutable snapshot published to the UI.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub model: Arc<ConnectionModel>,
    /// VMs with an operation started by this client still in flight.
    pub busy_vms: Arc<HashSet<Ref<Vm>>>,
}

/// Discrete events for the UI, drained with [`BackendHandle::try_recv`].
#[derive(Debug)]
pub enum Notification {
    /// The server presented an untrusted certificate; the UI must ask the user.
    CertificatePrompt(CertificatePrompt),
    /// A VM operation failed.
    VmActionFailed {
        vm_name: String,
        action: VmAction,
        error: xapi::Error,
    },
}

/// A pending "trust this certificate?" question.
#[derive(Debug)]
pub struct CertificatePrompt {
    pub server: String,
    pub fingerprint: Fingerprint,
    /// Fingerprint trusted before for this server, if the certificate changed.
    pub previous: Option<Fingerprint>,
    reply: oneshot::Sender<bool>,
}

impl CertificatePrompt {
    /// Trust the certificate and continue connecting.
    pub fn accept(self) {
        let _ = self.reply.send(true);
    }

    /// Refuse the certificate and abandon the connection.
    pub fn reject(self) {
        let _ = self.reply.send(false);
    }
}

/// Backend configuration.
#[derive(Clone)]
pub struct BackendOptions {
    /// Originator string sent on login (shows up in the server's logs).
    pub originator: String,
    /// Pinned certificates.
    pub trust: TrustStore,
    /// Server-side `event.from` timeout, seconds.
    pub event_timeout: f64,
    /// Minimum interval between two published snapshots (coalescing).
    pub publish_interval: Duration,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            originator: "xen-core".to_owned(),
            trust: TrustStore::new(),
            event_timeout: 30.0,
            publish_interval: Duration::from_millis(100),
        }
    }
}

/// The UI's end of the backend.
pub struct BackendHandle {
    commands: mpsc::UnboundedSender<Command>,
    notifications: mpsc::UnboundedReceiver<Notification>,
    shared: Arc<Shared>,
}

impl BackendHandle {
    /// Starts the backend on `runtime`.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        options: BackendOptions,
        repaint: RepaintFn,
    ) -> Self {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (notify_tx, notifications) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            state: ArcSwap::from_pointee(AppState::default()),
            update_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
            repaint,
            notify_tx,
            options,
        });
        runtime.spawn(run(shared.clone(), command_rx));
        Self {
            commands,
            notifications,
            shared,
        }
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

struct Shared {
    state: ArcSwap<AppState>,
    update_lock: Mutex<()>,
    /// Incremented whenever the current connection is replaced; work from an
    /// older generation must not publish.
    generation: AtomicU64,
    repaint: RepaintFn,
    notify_tx: mpsc::UnboundedSender<Notification>,
    options: BackendOptions,
}

impl Shared {
    /// Publishes a modified copy of the state if `generation` is current.
    fn update(&self, generation: u64, f: impl FnOnce(&mut AppState)) {
        {
            let _guard = self
                .update_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let mut next = AppState::clone(&self.state.load());
            f(&mut next);
            self.state.store(Arc::new(next));
        }
        (self.repaint)();
    }

    fn notify(&self, notification: Notification) {
        let _ = self.notify_tx.send(notification);
        (self.repaint)();
    }

    fn next_generation(&self) -> u64 {
        let _guard = self
            .update_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }
}

type SessionSlot = Arc<Mutex<Option<Session>>>;

struct ActiveConnection {
    task: JoinHandle<()>,
    session: SessionSlot,
}

impl ActiveConnection {
    fn session(&self) -> Option<Session> {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops the connection task and logs out in the background.
    fn close(self) -> Option<JoinHandle<()>> {
        self.task.abort();
        let session = self
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        Some(tokio::spawn(async move {
            if let Err(e) = tokio::time::timeout(Duration::from_secs(5), session.logout())
                .await
                .unwrap_or(Ok(()))
            {
                tracing::debug!(error = %e, "logout failed");
            }
        }))
    }
}

async fn run(shared: Arc<Shared>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut current: Option<ActiveConnection> = None;
    while let Some(command) = commands.recv().await {
        match command {
            Command::Connect {
                server,
                username,
                password,
            } => {
                if let Some(old) = current.take() {
                    old.close();
                }
                let generation = shared.next_generation();
                let session: SessionSlot = Arc::default();
                let task = tokio::spawn(connection_task(
                    shared.clone(),
                    generation,
                    server,
                    username,
                    password,
                    session.clone(),
                ));
                current = Some(ActiveConnection { task, session });
            }
            Command::Disconnect => {
                if let Some(old) = current.take() {
                    old.close();
                }
                let generation = shared.next_generation();
                shared.update(generation, |s| *s = AppState::default());
            }
            Command::Vm { vm, action } => {
                match current.as_ref().and_then(ActiveConnection::session) {
                    Some(session) => {
                        let generation = shared.generation.load(Ordering::SeqCst);
                        tokio::spawn(vm_action(shared.clone(), generation, session, vm, action));
                    }
                    None => tracing::debug!(?action, "VM action ignored: not connected"),
                }
            }
            Command::Shutdown { done } => {
                if let Some(old) = current.take()
                    && let Some(logout) = old.close()
                {
                    let _ = logout.await;
                }
                let _ = done.send(());
                return;
            }
        }
    }
    if let Some(old) = current.take()
        && let Some(logout) = old.close()
    {
        let _ = logout.await;
    }
}

async fn connection_task(
    shared: Arc<Shared>,
    generation: u64,
    server: String,
    username: String,
    password: String,
    slot: SessionSlot,
) {
    let fail = |label: String, error: ConnectionError| {
        shared.update(generation, |s| {
            *s = AppState {
                status: ConnectionStatus::Failed {
                    server: label,
                    error: Arc::new(error),
                },
                ..AppState::default()
            };
        });
    };

    let endpoint = match Endpoint::parse(&server) {
        Ok(e) => e,
        Err(e) => return fail(server, ConnectionError::Connect(e)),
    };
    let label = endpoint.to_string();
    shared.update(generation, |s| {
        *s = AppState {
            status: ConnectionStatus::Connecting {
                server: label.clone(),
            },
            ..AppState::default()
        };
    });

    let trust = &shared.options.trust;
    let call_options = ClientOptions {
        timeout: Duration::from_secs(60),
        ..ClientOptions::default()
    };
    let session = loop {
        let client = match JsonRpcClient::new(&endpoint, trust, &call_options) {
            Ok(c) => Arc::new(c),
            Err(e) => return fail(label, ConnectionError::Connect(e)),
        };
        match Session::login_with_password(client, &username, &password, &shared.options.originator)
            .await
        {
            Ok(session) => break session,
            Err(xapi::Error::UntrustedCertificate { host, fingerprint }) => {
                let (reply, answer) = oneshot::channel();
                shared.notify(Notification::CertificatePrompt(CertificatePrompt {
                    server: label.clone(),
                    fingerprint,
                    previous: trust.get(&host),
                    reply,
                }));
                if answer.await == Ok(true) {
                    trust.trust(&host, fingerprint);
                } else {
                    return fail(label, ConnectionError::CertificateRejected);
                }
            }
            Err(e) => return fail(label, ConnectionError::Connect(e)),
        }
    };
    drop(password);
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());

    let abandon = |error: ConnectionError| {
        slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        let session = session.clone();
        tokio::spawn(async move { session.logout().await });
        fail(label.clone(), error);
    };

    // Initial load.
    let loaded = tokio::try_join!(
        xapi::pool::get_all_records(&session),
        xapi::host::get_all_records(&session),
        xapi::vm::get_all_records(&session),
    );
    let mut model = match loaded {
        Ok((pools, hosts, vms)) => ConnectionModel::from_records(pools, hosts, vms),
        Err(e) => return abandon(ConnectionError::Connect(e)),
    };
    let api_version = model
        .coordinator()
        .map_or((0, 0), |h| (h.api_version_major, h.api_version_minor));
    if api_version < xapi::MIN_API_VERSION {
        let (major, minor) = api_version;
        return abandon(ConnectionError::Unsupported(
            xapi::Error::UnsupportedApiVersion { major, minor },
        ));
    }
    shared.update(generation, |s| {
        s.status = ConnectionStatus::Connected {
            server: label.clone(),
            api_version,
        };
        s.model = Arc::new(model.clone());
    });

    // Live updates. event.from long-polls on its own HTTP client so it never
    // delays regular calls; its timeout must exceed the server-side one.
    let event_options = ClientOptions {
        timeout: Duration::from_secs_f64(shared.options.event_timeout + 90.0),
        ..ClientOptions::default()
    };
    let events = match JsonRpcClient::new(&endpoint, trust, &event_options) {
        Ok(c) => session.with_client(Arc::new(c)),
        Err(e) => return abandon(ConnectionError::Lost(e)),
    };
    // The first call (empty token) returns every object again; applying it is
    // idempotent and closes the gap between the initial load and the token.
    let mut token = String::new();
    loop {
        match events
            .event_from(EVENT_CLASSES, &token, shared.options.event_timeout)
            .await
        {
            Ok(batch) => {
                token = batch.token;
                if model.apply_events(&batch.events) {
                    let snapshot = Arc::new(model.clone());
                    shared.update(generation, |s| s.model = snapshot);
                    // Coalesce event storms: at most one snapshot per interval.
                    tokio::time::sleep_until(Instant::now() + shared.options.publish_interval)
                        .await;
                }
            }
            Err(e) => return abandon(ConnectionError::Lost(e)),
        }
    }
}

async fn vm_action(
    shared: Arc<Shared>,
    generation: u64,
    session: Session,
    vm: Ref<Vm>,
    action: VmAction,
) {
    let set_busy = |busy: bool| {
        shared.update(generation, |s| {
            let mut set = HashSet::clone(&s.busy_vms);
            if busy {
                set.insert(vm.clone());
            } else {
                set.remove(&vm);
            }
            s.busy_vms = Arc::new(set);
        });
    };
    set_busy(true);
    let result = match action {
        VmAction::Start => xapi::vm::start(&session, &vm, false, false).await,
        VmAction::CleanShutdown => xapi::vm::clean_shutdown(&session, &vm).await,
    };
    set_busy(false);
    if let Err(error) = result
        && shared.generation.load(Ordering::SeqCst) == generation
    {
        let vm_name = shared
            .state
            .load()
            .model
            .vms
            .get(&vm)
            .map(|r| r.name_label.clone())
            .unwrap_or_else(|| vm.to_string());
        shared.notify(Notification::VmActionFailed {
            vm_name,
            action,
            error,
        });
    }
}
