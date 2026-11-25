//! The VM and host (dom0) consoles: finding a VM's VNC console in the
//! cache, opening its tunnel, running the RFB session, and reconnecting
//! while the VM runs (after a reboot, a toolstack restart or a network
//! glitch), as the original's `XSVNCScreen` does.
//!
//! The UI makes a [`ConsoleHandle`] / [`ConsoleLink`] pair with [`pair`],
//! keeps the handle (screen, status, input) and gives the link to the
//! backend in [`Command::OpenConsole`](crate::Command::OpenConsole).
//! Dropping the handle closes the console.
//!
//! Like the original (`XenConnection.DuplicateSession`), the tunnel
//! authenticates with the connection's current session (`Cookie:
//! session_id=`), re-read at every attempt so a reconnected connection's
//! new session is used. The TLS certificate of the host running the VM must
//! be the one pinned for that address or, for another pool member, one the
//! pool lists for that host (`Certificate` records of type `host`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use xapi::{
    CertificateType, ConsoleProtocol, Fingerprint, Host, Ref, Session, TrustStore, Vm, VmPowerState,
};
use xen_vnc::keymap::CTRL_ALT_DEL;
use xen_vnc::tunnel::{self, ConsoleLocation, ConsoleStream};
use xen_vnc::{Input, Screen, ServerEvent, VncError};

use crate::backend::{ConnectionId, RepaintFn, SessionSlot, Shared, slot_get};
use crate::model::ConnectionModel;
use crate::objects;

/// What a console shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConsoleTarget {
    /// A VM's console.
    Vm(Ref<Vm>),
    /// A host's own console: its control domain's text console.
    Host(Ref<Host>),
}

/// Where a console stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleStatus {
    /// Looking for the console or opening it.
    Connecting,
    /// Showing the guest's screen. `scancodes`: keys go as scancodes
    /// (QEMU Extended Key Event acknowledged), so the guest's layout applies.
    Connected { scancodes: bool },
    /// The VM is not running (its power state); the console reconnects by
    /// itself once it runs again.
    NotRunning(VmPowerState),
    /// The host is not live.
    HostDown,
    /// The connection to the server is down; waiting for it.
    Disconnected,
    /// The last attempt failed; retrying.
    Retrying { error: String },
    /// The VM or host is gone from the cache.
    Gone,
}

/// Discrete console events for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// Clipboard text from the guest.
    CutText(String),
    Bell,
}

/// State shared by a console's handle and its backend task.
#[derive(Debug)]
struct ConsoleShared {
    screen: Arc<Screen>,
    status: Mutex<ConsoleStatus>,
    events: Mutex<VecDeque<ConsoleEvent>>,
    /// Sessions that reached the RFB `ServerInit`.
    sessions: std::sync::atomic::AtomicU64,
    /// Inputs handed to the console (keys, pointer, clipboard).
    inputs_sent: std::sync::atomic::AtomicU64,
    closed: AtomicBool,
    close: Notify,
}

impl ConsoleShared {
    fn set_status(&self, status: ConsoleStatus) -> bool {
        let mut current = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == status {
            return false;
        }
        *current = status;
        true
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// The UI's side of a console. Dropping it closes the console.
#[derive(Debug)]
pub struct ConsoleHandle {
    shared: Arc<ConsoleShared>,
    inputs: mpsc::UnboundedSender<Input>,
}

/// The backend's side of a console, given in
/// [`Command::OpenConsole`](crate::Command::OpenConsole).
#[derive(Debug)]
pub struct ConsoleLink {
    shared: Arc<ConsoleShared>,
    inputs: mpsc::UnboundedReceiver<Input>,
}

/// A new console handle and the link for the backend.
pub fn pair() -> (ConsoleHandle, ConsoleLink) {
    let shared = Arc::new(ConsoleShared {
        screen: Screen::new(),
        status: Mutex::new(ConsoleStatus::Connecting),
        events: Mutex::default(),
        sessions: Default::default(),
        inputs_sent: Default::default(),
        closed: AtomicBool::new(false),
        close: Notify::new(),
    });
    let (tx, rx) = mpsc::unbounded_channel();
    (
        ConsoleHandle {
            shared: shared.clone(),
            inputs: tx,
        },
        ConsoleLink { shared, inputs: rx },
    )
}

impl ConsoleHandle {
    /// The remote screen (framebuffer and cursor).
    pub fn screen(&self) -> &Arc<Screen> {
        &self.shared.screen
    }

    pub fn status(&self) -> ConsoleStatus {
        self.shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// RFB sessions started so far (1 after the first connection; more
    /// after reconnections).
    pub fn sessions(&self) -> u64 {
        self.shared.sessions.load(Ordering::Acquire)
    }

    /// Sends input to the guest (dropped while not connected).
    pub fn send(&self, input: Input) {
        self.shared.inputs_sent.fetch_add(1, Ordering::AcqRel);
        let _ = self.inputs.send(input);
    }

    /// Inputs (keys, pointer events, clipboard texts, refreshes) given to
    /// [`ConsoleHandle::send`] so far: 0 for a console only looked at.
    pub fn inputs_sent(&self) -> u64 {
        self.shared.inputs_sent.load(Ordering::Acquire)
    }

    /// Ctrl+Alt+Del, as the original's `SendCAD`.
    pub fn send_ctrl_alt_del(&self) {
        for (key, down) in CTRL_ALT_DEL {
            self.send(Input::Key {
                down,
                scancode: key.scancode(),
                keysym: key.keysym(false),
            });
        }
    }

    /// Takes the clipboard texts and bells received since the last call.
    pub fn take_events(&self) -> Vec<ConsoleEvent> {
        self.shared
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

impl Drop for ConsoleHandle {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.close.notify_waiters();
        self.shared.close.notify_one();
    }
}

/// Tunnel timeout (TCP, TLS and the `CONNECT` reply).
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The RFB handshake must finish in this time once the tunnel is open.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a waiting console looks at the cache again.
const POLL: Duration = Duration::from_millis(250);
/// Retry delays of the original (`XSVNCScreen`): 10 quick retries 100 ms
/// apart, then every 5 s.
const SHORT_RETRY_COUNT: u32 = 10;
const SHORT_RETRY: Duration = Duration::from_millis(100);
const LONG_RETRY: Duration = Duration::from_secs(5);

/// What a console needs from the backend, abstracted so it can be tested.
pub(crate) struct ConsoleEnv {
    pub(crate) shared: Arc<Shared>,
    pub(crate) connection: ConnectionId,
    pub(crate) session: SessionSlot,
}

impl ConsoleEnv {
    fn repaint(&self) -> RepaintFn {
        self.shared.repaint_fn()
    }

    /// The connection's model if it is connected; `Err(true)` if the
    /// connection is gone, `Err(false)` if it is down for now.
    fn model(&self) -> Result<Arc<ConnectionModel>, bool> {
        let state = self.shared.state();
        match state.connection(self.connection) {
            None => Err(true),
            Some(view) if view.status.is_connected() => Ok(view.model.clone()),
            Some(_) => Err(false),
        }
    }
}

/// Runs a console until its handle is dropped or the connection goes away.
pub(crate) async fn run(env: ConsoleEnv, target: ConsoleTarget, link: ConsoleLink) {
    let ConsoleLink { shared, mut inputs } = link;
    let repaint = env.repaint();
    let set_status = |status: ConsoleStatus| {
        if shared.set_status(status) {
            repaint();
        }
    };
    let mut retries: u32 = 0;
    while !shared.is_closed() {
        // Inputs typed while not connected are dropped.
        while inputs.try_recv().is_ok() {}

        let model = match env.model() {
            Ok(m) => m,
            Err(true) => break,
            Err(false) => {
                set_status(ConsoleStatus::Disconnected);
                wait(&shared, POLL).await;
                continue;
            }
        };
        let vm = match resolve(&model, &target) {
            Ok(vm) => vm,
            Err(status) => {
                set_status(status);
                retries = 0;
                wait(&shared, POLL).await;
                continue;
            }
        };
        let Some(session) = slot_get(&env.session) else {
            set_status(ConsoleStatus::Disconnected);
            wait(&shared, POLL).await;
            continue;
        };
        if retries == 0 {
            set_status(ConsoleStatus::Connecting);
        }

        let attempt = async {
            let stream = open_console(&model, &vm, &session, env.shared.store().trust()).await?;
            let on_event = |event: ServerEvent| match event {
                ServerEvent::Connected(_) => {
                    shared.sessions.fetch_add(1, Ordering::AcqRel);
                    let scancodes = shared.screen.lock().scancodes;
                    shared.set_status(ConsoleStatus::Connected { scancodes });
                    repaint();
                }
                ServerEvent::Updated | ServerEvent::Resized { .. } => {
                    let scancodes = shared.screen.lock().scancodes;
                    shared.set_status(ConsoleStatus::Connected { scancodes });
                    repaint();
                }
                ServerEvent::Bell => push_event(&shared, ConsoleEvent::Bell, &repaint),
                ServerEvent::CutText(text) => {
                    push_event(&shared, ConsoleEvent::CutText(text), &repaint);
                }
            };
            run_session(stream, &shared.screen, &mut inputs, &on_event).await
        };
        let result = tokio::select! {
            r = attempt => r,
            _ = closed(&shared) => break,
            _ = connection_removed(&env) => break,
        };
        match result {
            // The handle was dropped (its input channel closed).
            Ok(()) => break,
            Err(e) => {
                let connected_before = matches!(
                    *shared.status.lock().unwrap_or_else(PoisonError::into_inner),
                    ConsoleStatus::Connected { .. }
                );
                if connected_before {
                    retries = 0;
                    tracing::info!(error = %e, "console closed; reconnecting");
                } else {
                    tracing::debug!(error = %e, "console attempt failed");
                }
                retries += 1;
                // A stopped VM shows as such, not as an error.
                let still_running = env
                    .model()
                    .ok()
                    .is_some_and(|m| resolve(&m, &target).is_ok());
                if still_running {
                    set_status(ConsoleStatus::Retrying {
                        error: e.to_string(),
                    });
                }
                let delay = if retries < SHORT_RETRY_COUNT {
                    SHORT_RETRY
                } else {
                    LONG_RETRY
                };
                wait(&shared, delay).await;
            }
        }
    }
    tracing::debug!("console closed");
}

/// Resolves once the connection is removed (disconnected by the user).
async fn connection_removed(env: &ConsoleEnv) {
    while !matches!(env.model(), Err(true)) {
        tokio::time::sleep(POLL).await;
    }
}

fn push_event(shared: &ConsoleShared, event: ConsoleEvent, repaint: &RepaintFn) {
    let mut events = shared.events.lock().unwrap_or_else(PoisonError::into_inner);
    events.push_back(event);
    // Nobody reading: keep only the latest few.
    while events.len() > 32 {
        events.pop_front();
    }
    drop(events);
    repaint();
}

async fn closed(shared: &ConsoleShared) {
    loop {
        let notified = shared.close.notified();
        if shared.is_closed() {
            return;
        }
        notified.await;
    }
}

/// Sleeps `delay`, or less if the console is closed meanwhile.
async fn wait(shared: &ConsoleShared, delay: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = closed(shared) => {}
    }
}

async fn run_session(
    stream: Box<dyn ConsoleStream>,
    screen: &Arc<Screen>,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    on_event: &(dyn Fn(ServerEvent) + Send + Sync),
) -> Result<(), VncError> {
    // The handshake must not hang on a server that accepted the tunnel but
    // never speaks.
    let connected = Arc::new(AtomicBool::new(false));
    let watch = {
        let connected = connected.clone();
        move |event: ServerEvent| {
            if matches!(event, ServerEvent::Connected(_)) {
                connected.store(true, Ordering::Release);
            }
            on_event(event);
        }
    };
    let session = xen_vnc::client::run(stream, screen, inputs, &watch);
    tokio::pin!(session);
    tokio::select! {
        r = &mut session => r,
        _ = tokio::time::sleep(HANDSHAKE_TIMEOUT), if !connected.load(Ordering::Acquire) => {
            if connected.load(Ordering::Acquire) {
                session.await
            } else {
                Err(VncError::Timeout)
            }
        }
    }
}

/// The VM whose console `target` shows, if it can have one now; otherwise
/// the status to show.
pub fn resolve(model: &ConnectionModel, target: &ConsoleTarget) -> Result<Ref<Vm>, ConsoleStatus> {
    match target {
        ConsoleTarget::Vm(r) => {
            let vm = model.get(r).ok_or(ConsoleStatus::Gone)?;
            if vm.power_state != VmPowerState::Running {
                return Err(ConsoleStatus::NotRunning(vm.power_state.clone()));
            }
            Ok(r.clone())
        }
        ConsoleTarget::Host(h) => {
            let host = model.get(h).ok_or(ConsoleStatus::Gone)?;
            if !objects::host_is_live(model, host) {
                return Err(ConsoleStatus::HostDown);
            }
            let dom0 = model
                .all::<Vm>()
                .iter()
                .find(|(_, vm)| vm.is_control_domain && &vm.resident_on == h && vm.domid == 0)
                .map(|(r, _)| r.clone())
                .or_else(|| {
                    model
                        .get(&host.control_domain)
                        .map(|_| host.control_domain.clone())
                })
                .ok_or(ConsoleStatus::Gone)?;
            Ok(dom0)
        }
    }
}

/// The VNC (`rfb`) console records of `vm`, as `(location, host the VM runs
/// on)`.
pub fn rfb_consoles(model: &ConnectionModel, vm: &Ref<Vm>) -> Vec<String> {
    let Some(record) = model.get(vm) else {
        return Vec::new();
    };
    let mut out: Vec<String> = record
        .consoles
        .iter()
        .filter_map(|c| model.get(c))
        .filter(|c| c.protocol == ConsoleProtocol::Rfb)
        .map(|c| c.location.clone())
        .collect();
    if out.is_empty() {
        // Consoles whose VM field points here but are missing from the
        // VM's list (an event not seen yet).
        out = model
            .all::<xapi::Console>()
            .values()
            .filter(|c| &c.vm == vm && c.protocol == ConsoleProtocol::Rfb)
            .map(|c| c.location.clone())
            .collect();
    }
    out.sort();
    out
}

/// The certificate fingerprints accepted for the console of `vm` at
/// `location`: the one pinned for that address, and the `host`
/// certificates the pool lists for the host running the VM.
pub fn accepted_fingerprints(
    model: &ConnectionModel,
    vm: &Ref<Vm>,
    location: &ConsoleLocation,
    trust: &TrustStore,
) -> Vec<Fingerprint> {
    let mut accepted: Vec<Fingerprint> = trust.get(&location.authority()).into_iter().collect();
    if let Some(record) = model.get(vm) {
        for cert in model.all::<xapi::Certificate>().values() {
            if cert.host == record.resident_on
                && cert.r#type == CertificateType::Host
                && let Some(fp) = Fingerprint::parse(&cert.fingerprint_sha256)
                && !accepted.contains(&fp)
            {
                accepted.push(fp);
            }
        }
    }
    accepted
}

/// Opens the tunnel to the first of `vm`'s VNC consoles that answers.
pub async fn open_console(
    model: &ConnectionModel,
    vm: &Ref<Vm>,
    session: &Session,
    trust: &TrustStore,
) -> Result<Box<dyn ConsoleStream>, VncError> {
    let locations = rfb_consoles(model, vm);
    let mut last = VncError::Protocol("the VM has no VNC console".to_owned());
    for location in locations {
        let location = match ConsoleLocation::parse(&location) {
            Ok(l) => l,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let tls = if location.tls {
            let accepted = accepted_fingerprints(model, vm, &location, trust);
            Some(Arc::new(
                xapi::pinned_client_config(accepted).map_err(|e| VncError::Tls(rustls_error(e)))?,
            ))
        } else {
            None
        };
        match tunnel::open(&location, session.session_ref().as_str(), tls, OPEN_TIMEOUT).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn rustls_error(e: xapi::Error) -> rustls::Error {
    match e {
        xapi::Error::TlsSetup(inner) => inner,
        other => rustls::Error::General(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> ConnectionModel {
        let db: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/xcp-ng-8.3-capture.json"
        ))
        .unwrap();
        ConnectionModel::from_database(&db)
    }

    #[test]
    fn host_consoles_are_the_control_domain_and_vm_consoles_need_a_running_vm() {
        let model = capture();
        let (host_ref, host) = model.hosts.iter().next().unwrap();
        let dom0 = resolve(&model, &ConsoleTarget::Host(host_ref.clone())).unwrap();
        let record = model.get(&dom0).unwrap();
        assert!(record.is_control_domain);
        assert_eq!(&record.resident_on, host_ref);
        assert!(!rfb_consoles(&model, &dom0).is_empty());
        let _ = host;

        let running = model
            .vms
            .iter()
            .find(|(_, v)| !v.is_control_domain && v.power_state == VmPowerState::Running)
            .map(|(r, _)| r.clone())
            .unwrap();
        assert_eq!(
            resolve(&model, &ConsoleTarget::Vm(running.clone())),
            Ok(running.clone())
        );
        assert!(!rfb_consoles(&model, &running).is_empty());
        let halted = model
            .vms
            .iter()
            .find(|(_, v)| !v.is_a_template && v.power_state == VmPowerState::Halted)
            .map(|(r, _)| r.clone())
            .unwrap();
        assert_eq!(
            resolve(&model, &ConsoleTarget::Vm(halted)),
            Err(ConsoleStatus::NotRunning(VmPowerState::Halted))
        );
        assert_eq!(
            resolve(&model, &ConsoleTarget::Vm(Ref::new("OpaqueRef:nope"))),
            Err(ConsoleStatus::Gone)
        );
    }

    #[test]
    fn pool_member_certificates_are_accepted_for_their_consoles() {
        let model = capture();
        let running = model
            .vms
            .iter()
            .find(|(_, v)| !v.is_control_domain && v.power_state == VmPowerState::Running)
            .map(|(r, _)| r.clone())
            .unwrap();
        let location = ConsoleLocation::parse(&rfb_consoles(&model, &running)[0]).unwrap();
        let trust = TrustStore::new();
        let listed = accepted_fingerprints(&model, &running, &location, &trust);
        // The capture lists one `host` certificate (the `host_internal` one
        // is not accepted).
        assert_eq!(listed.len(), 1);
        let pinned = Fingerprint([9; 32]);
        trust.trust(&location.authority(), pinned);
        let both = accepted_fingerprints(&model, &running, &location, &trust);
        assert_eq!(both[0], pinned);
        assert_eq!(both.len(), 2);
    }
}
