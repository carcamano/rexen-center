//! A mock XAPI JSON-RPC server for tests.
//!
//! It serves `POST /jsonrpc` over plain HTTP or HTTPS (with the self-signed
//! certificate in `tests/fixtures/`) and keeps an in-memory object database
//! loaded from a fixture: [`fixture_records`] (`tests/fixtures/records.json`,
//! a small hand-written lab pool) or [`capture_records`]
//! (`tests/fixtures/xcp-ng-8.3-capture.json`, a sanitized capture of a real
//! XCP-ng 8.3 host with every class it delivers through events). It
//! implements:
//!
//! * `session.login_with_password` / `session.logout` (user [`USERNAME`],
//!   password [`PASSWORD`]; anything else fails with
//!   `SESSION_AUTHENTICATION_FAILED`);
//! * for every class: `get_all_records`, `get_all`, `get_record`,
//!   `get_by_uuid` and the field getters `get_<field>` (a class absent from
//!   the database simply has no objects);
//! * the VM and host operations of the console and RBAC users (see
//!   [`ops`]); their `Async.*` forms return a `task` that advances its
//!   `progress` in steps ([`MockServer::set_task_timing`]) and can be
//!   cancelled (`task.cancel`) and destroyed (`task.destroy`);
//! * `session.get_is_local_superuser` and `session.get_rbac_permissions`;
//! * `host.get_servertime` (the client heartbeat);
//! * `event.inject` and `event.from` (tokens, `"*"`, long-polling);
//! * the console tunnel: `CONNECT /console?uuid=…` (or `?ref=`) with the
//!   session cookie opens a fake VNC console of the VM
//!   ([`MockServer::console`], an [`xen_vnc::testing::MockConsole`]); the
//!   console records' `location` points at the mock, and a VM's console
//!   sessions drop when it reboots, stops, suspends or migrates, as QEMU's
//!   do.
//!
//! * performance data: `GET /rrd_updates`, `/host_rrds` and `/vm_rrds`
//!   (JSON with `json=true`, else XML) and `host.get_data_sources` /
//!   `VM.get_data_sources`, made up from the time (see [`rrd`]);
//! * VM creation, templates, snapshots and disks (see [`provisioning`]);
//! * messages: `message.create` and `message.destroy` (with their events),
//!   and `host.call_plugin` for perfmon's `refresh` / `start`.
//!
//! Tests can also change objects behind the client's back
//! ([`MockServer::set_vm_power_state`], [`MockServer::set_field`],
//! [`MockServer::add_object`],
//! [`MockServer::start_task`], …) to prove that updates arrive through
//! `event.from` alone, and inject failures:
//! [`MockServer::lose_events`] (`EVENTS_LOST`),
//! [`MockServer::invalidate_sessions`] (`SESSION_INVALID`),
//! [`MockServer::set_unknown_classes`] (an older server without some classes),
//! [`MockServer::stop`] / [`MockServer::restart`] (the server goes away and
//! comes back on the same port with every session forgotten, like a toolstack
//! restart), [`MockServer::set_certificate`] (a renewed certificate),
//! [`MockServer::redirect_logins_to`] (a pool member answering
//! `HOST_IS_SLAVE`), [`MockServer::set_unresponsive`] (requests hang, as
//! behind a dead network link) and [`MockServer::fail_method`] (a call or
//! its task fails with a given XAPI error).

pub mod ops;
pub mod provisioning;
pub mod rrd;
pub mod storage;

pub use ops::RBAC_USERS;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};
use tokio::task::{AbortHandle, JoinHandle};
use xen_vnc::testing::MockConsole;

/// Login user accepted by the mock.
pub const USERNAME: &str = "root";
/// Login password accepted by the mock (test-only, not a real credential).
pub const PASSWORD: &str = "mock-password";

const RECORDS_JSON: &str = include_str!("../../../tests/fixtures/records.json");
const CAPTURE_JSON: &str = include_str!("../../../tests/fixtures/xcp-ng-8.3-capture.json");
const CERT_PEM: &str = include_str!("../../../tests/fixtures/mock-server.cert.pem");
const KEY_PEM: &str = include_str!("../../../tests/fixtures/mock-server.key.pem");
const RENEWED_CERT_PEM: &str = include_str!("../../../tests/fixtures/mock-server-renewed.cert.pem");
const RENEWED_KEY_PEM: &str = include_str!("../../../tests/fixtures/mock-server-renewed.key.pem");

const NULL_REF: &str = "OpaqueRef:NULL";

/// The certificates the HTTPS mock can present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockCertificate {
    /// `tests/fixtures/mock-server.cert.pem` (the default).
    Original,
    /// `tests/fixtures/mock-server-renewed.cert.pem`: another self-signed
    /// certificate, as after a host reinstall or certificate renewal.
    Renewed,
}

impl MockCertificate {
    fn pem(self) -> (&'static str, &'static str) {
        match self {
            Self::Original => (CERT_PEM, KEY_PEM),
            Self::Renewed => (RENEWED_CERT_PEM, RENEWED_KEY_PEM),
        }
    }

    /// DER bytes of this certificate.
    pub fn der(self) -> Vec<u8> {
        CertificateDer::from_pem_slice(self.pem().0.as_bytes())
            .expect("fixture certificate")
            .as_ref()
            .to_vec()
    }
}

/// DER bytes of the certificate the HTTPS mock presents by default.
pub fn certificate_der() -> Vec<u8> {
    MockCertificate::Original.der()
}

/// The fixture database (`tests/fixtures/records.json`).
pub fn fixture_records() -> Value {
    serde_json::from_str(RECORDS_JSON).expect("fixture records")
}

/// The sanitized real-host database (`tests/fixtures/xcp-ng-8.3-capture.json`):
/// one XCP-ng 8.3 host with its pool, VMs, storage, networks, metrics,
/// consoles, PCI devices, certificates, roles and messages.
pub fn capture_records() -> Value {
    serde_json::from_str(CAPTURE_JSON).expect("capture fixture")
}

/// A running mock server. Stops when dropped.
pub struct MockServer {
    addr: SocketAddr,
    tls: bool,
    state: Arc<State>,
    running: Mutex<Option<Running>>,
}

/// The accept loop and the live client connections of a started server.
struct Running {
    accept_task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Running {
    /// Stops accepting and drops every connection; the returned handle
    /// completes once the listening socket is closed.
    fn stop(self) -> JoinHandle<()> {
        self.accept_task.abort();
        for conn in lock(&self.connections).drain(..) {
            conn.abort();
        }
        self.accept_task
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MockServer {
    /// Starts a mock with the fixture database on an ephemeral localhost port.
    pub async fn start(tls: bool) -> Self {
        Self::start_with_records(tls, fixture_records()).await
    }

    /// Starts a mock with a custom database (`{"pool": {ref: record}, "host": …, "vm": …}`).
    pub async fn start_with_records(tls: bool, records: Value) -> Self {
        Self::start_on(0, tls, records).await
    }

    /// Like [`MockServer::start_with_records`] on a given localhost port
    /// (`0` picks a free one).
    pub async fn start_on(port: u16, tls: bool, records: Value) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(State::new(records));
        let scheme = if tls { "https" } else { "http" };
        state.db().point_consoles_at(&format!("{scheme}://{addr}"));
        let running = serve(listener, tls, state.clone());
        Self {
            addr,
            tls,
            state,
            running: Mutex::new(Some(running)),
        }
    }

    /// Stops listening and drops every client connection (requests in flight,
    /// such as an `event.from` long-poll, fail). The database is kept.
    pub fn stop(&self) {
        if let Some(running) = lock(&self.running).take() {
            running.stop();
        }
        for console in lock(&self.state.consoles).values() {
            console.disconnect_all();
        }
    }

    /// Starts again on the same port after [`MockServer::stop`] (or stops
    /// first if running). Like a toolstack restart, every session is
    /// forgotten; objects are kept.
    pub async fn restart(&self) {
        let running = lock(&self.running).take();
        if let Some(running) = running {
            let _ = running.stop().await;
        }
        for console in lock(&self.state.consoles).values() {
            console.disconnect_all();
        }
        self.state.db().sessions.clear();
        let mut tries = 0;
        let listener = loop {
            match TcpListener::bind(self.addr).await {
                Ok(l) => break l,
                Err(e) if tries < 100 => {
                    tries += 1;
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("rebind mock server: {e}"),
            }
        };
        *lock(&self.running) = Some(serve(listener, self.tls, self.state.clone()));
        self.state.changed.notify_waiters();
    }

    /// The fake VNC console of `vm` (made on first use: 640×480, RFB 3.3
    /// with the QEMU Extended Key Event, or 640×384 without it for a control
    /// domain, like dom0's text console).
    pub fn console(&self, vm: &str) -> MockConsole {
        self.state.console(vm)
    }

    /// Whether the server is listening.
    pub fn is_running(&self) -> bool {
        lock(&self.running).is_some()
    }

    /// The certificate presented on new TLS connections. Existing connections
    /// keep the old one; [`MockServer::restart`] drops them.
    pub fn set_certificate(&self, certificate: MockCertificate) {
        *lock(&self.state.certificate) = certificate;
    }

    /// Makes this server act as a pool member: logins fail with
    /// `HOST_IS_SLAVE` naming `coordinator` (an address such as another mock's
    /// [`MockServer::authority`]). `None` makes it a coordinator again.
    pub fn redirect_logins_to(&self, coordinator: Option<&str>) {
        self.state.db().coordinator = coordinator.map(str::to_owned);
    }

    /// While set, requests are accepted but never answered (a dead link or a
    /// hung toolstack). Clearing it releases the held requests.
    pub fn set_unresponsive(&self, unresponsive: bool) {
        self.state.unresponsive.send_replace(unresponsive);
    }

    /// How `Async.*` tasks advance: `steps` progress updates, `step` apart.
    /// The default is 4 steps of 100 ms.
    pub fn set_task_timing(&self, steps: u32, step: Duration) {
        let mut db = self.state.db();
        db.task_steps = steps.max(1);
        db.task_step = step;
    }

    /// Starts a server task as if another client did it (`name_label`
    /// `name`). It advances like `Async.*` tasks and then succeeds, unless
    /// cancelled. Returns its reference.
    pub fn start_task(&self, name: &str) -> String {
        spawn_task(&self.state, name, ops::Work::Nothing)
    }

    /// Makes the PBDs of `host` fail to plug (`SR_BACKEND_FAILURE_47`), also
    /// the ones `SR.create` makes, or plug again.
    pub fn fail_pbd_plug(&self, host: &str, fail: bool) {
        let mut db = self.state.db();
        db.storage.plug_failures.retain(|h| h != host);
        if fail {
            db.storage.plug_failures.push(host.to_owned());
        }
    }

    /// Puts an ISO file on an ISO share of the simulated storage
    /// (`"nfs.lab:/exports/iso"`, `"//smb.lab/iso"`); a scan finds it.
    pub fn add_iso_file(&self, share: &str, name: &str) {
        self.state.db().storage.add_iso(share, name);
    }

    /// Makes `method` (e.g. `"VM.clean_reboot"`; its `Async.*` form too)
    /// fail with XAPI error `code` and `params` until cleared with `None`.
    /// An `Async.*` call returns a task that fails with it.
    pub fn fail_method(&self, method: &str, error: Option<(&str, &[&str])>) {
        let mut db = self.state.db();
        match error {
            Some((code, params)) => {
                db.failures.insert(
                    method.to_owned(),
                    (
                        code.to_owned(),
                        params.iter().map(|p| (*p).to_owned()).collect(),
                    ),
                );
            }
            None => {
                db.failures.remove(method);
            }
        }
    }

    /// Current `power_state` of a VM.
    pub fn vm_power_state(&self, vm: &str) -> String {
        self.state
            .db()
            .get_str("vm", vm, "power_state")
            .unwrap_or_default()
    }

    /// Whether a host is live (`host_metrics.live`).
    pub fn host_live(&self, host: &str) -> bool {
        self.state.db().host_live(host)
    }

    /// The references of every `task` object.
    pub fn tasks(&self) -> Vec<String> {
        self.state
            .db()
            .objects
            .keys()
            .filter(|(c, _)| c == "task")
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Address to type in a connect dialog, e.g. `https://127.0.0.1:41234`.
    pub fn address(&self) -> String {
        format!(
            "{}://{}",
            if self.tls { "https" } else { "http" },
            self.addr
        )
    }

    /// `host:port` (the certificate pinning key).
    pub fn authority(&self) -> String {
        self.addr.to_string()
    }

    /// Answer RRD requests in XML even when JSON is asked for, like a
    /// server without JSON support.
    pub fn set_rrd_json(&self, json: bool) {
        self.state.db().rrd_json = json;
    }

    /// Whether `get_data_sources` lists units (off: empty units, as a VM
    /// that just started may report).
    pub fn set_data_source_units(&self, units: bool) {
        self.state.db().rrd_units = units;
    }

    /// Moves the clock of the performance data `seconds` forward, so new
    /// rows show up without waiting.
    pub fn advance_rrd_clock(&self, seconds: i64) {
        self.state.db().rrd_clock_offset += seconds;
    }

    /// The `host.call_plugin` calls so far, `(plugin, function)`.
    pub fn plugin_calls(&self) -> Vec<(String, String)> {
        self.state.db().plugin_calls.clone()
    }

    /// Creates a message as XAPI would (e.g. a perfmon `ALARM`); returns
    /// its reference.
    pub fn create_message(
        &self,
        name: &str,
        priority: i64,
        cls: &str,
        obj_uuid: &str,
        body: &str,
    ) -> String {
        let reply = {
            let mut db = self.state.db();
            let n = db.next_message;
            db.next_message += 1;
            let r = format!("OpaqueRef:mock-message-{n}");
            let record = json!({
                "uuid": format!("3e55a9e0-0000-4000-8000-{n:012}"), "name": name,
                "priority": priority, "cls": cls, "obj_uuid": obj_uuid,
                "timestamp": "20261125T12:00:00Z", "body": body,
            });
            db.objects
                .insert(("message".to_owned(), r.clone()), record.clone());
            db.push_event("message", "add", &r, Some(record));
            r
        };
        self.state.changed.notify_waiters();
        reply
    }

    /// A field of an object, if it exists.
    pub fn field(&self, class: &str, opaque_ref: &str, field: &str) -> Option<Value> {
        self.state
            .db()
            .objects
            .get(&(class.to_lowercase(), opaque_ref.to_owned()))
            .and_then(|r| r.get(field))
            .cloned()
    }

    /// The references of every object of `class`.
    pub fn refs(&self, class: &str) -> Vec<String> {
        self.state
            .db()
            .objects
            .keys()
            .filter(|(c, _)| *c == class.to_lowercase())
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Methods called so far, in order (parameters are not recorded).
    pub fn calls(&self) -> Vec<String> {
        self.state.db().calls.clone()
    }

    /// How many times `method` was called.
    pub fn call_count(&self, method: &str) -> usize {
        self.state
            .db()
            .calls
            .iter()
            .filter(|c| *c == method)
            .count()
    }

    /// Makes the next `event.from` with a token fail with `EVENTS_LOST`, as a
    /// server does when the client fell too far behind.
    pub fn lose_events(&self) {
        self.state.db().lose_events = true;
        self.state.changed.notify_waiters();
    }

    /// Forgets every session, as a toolstack restart does: the next call on
    /// any session fails with `SESSION_INVALID`.
    pub fn invalidate_sessions(&self) {
        self.state.db().sessions.clear();
        self.state.changed.notify_waiters();
    }

    /// Classes whose calls fail with `MESSAGE_METHOD_UNKNOWN`, like an older
    /// server that predates them (names as in the API, e.g. `"VM_group"`).
    pub fn set_unknown_classes(&self, classes: &[&str]) {
        self.state.db().unknown_classes = classes.iter().map(|c| c.to_lowercase()).collect();
    }

    /// Creates an object as if someone else did it (emits an `add` event).
    pub fn add_object(&self, class: &str, opaque_ref: &str, record: Value) {
        let mut db = self.state.db();
        let class = class.to_lowercase();
        db.objects
            .insert((class.clone(), opaque_ref.to_owned()), record.clone());
        db.push_event(&class, "add", opaque_ref, Some(record));
        drop(db);
        self.state.changed.notify_waiters();
    }

    /// Destroys an object as if someone else did it (emits a `del` event).
    pub fn remove_object(&self, class: &str, opaque_ref: &str) {
        let mut db = self.state.db();
        let class = class.to_lowercase();
        if db
            .objects
            .remove(&(class.clone(), opaque_ref.to_owned()))
            .is_some()
        {
            db.push_event(&class, "del", opaque_ref, None);
        }
        drop(db);
        self.state.changed.notify_waiters();
    }

    /// Number of sessions currently logged in.
    pub fn active_sessions(&self) -> usize {
        self.state.db().sessions.len()
    }

    /// Current record of an object, e.g. `("vm", "OpaqueRef:vm-web")`.
    pub fn record(&self, class: &str, opaque_ref: &str) -> Option<Value> {
        self.state
            .db()
            .objects
            .get(&(class.to_lowercase(), opaque_ref.to_owned()))
            .cloned()
    }

    /// Changes a VM's power state as if someone else did it (emits events).
    pub fn set_vm_power_state(&self, vm: &str, power_state: &str) {
        let mut db = self.state.db();
        let before = db.get_str("vm", vm, "power_state").unwrap_or_default();
        db.move_vm(vm, power_state, None);
        drop(db);
        self.state.qemu_restarted(vm, &before, power_state);
        self.state.changed.notify_waiters();
    }

    /// Renames an object as if someone else did it (emits a `mod` event).
    pub fn set_name_label(&self, class: &str, opaque_ref: &str, name: &str) {
        let mut db = self.state.db();
        db.modify(&class.to_lowercase(), opaque_ref, |rec| {
            rec.insert("name_label".into(), json!(name));
        });
        drop(db);
        self.state.changed.notify_waiters();
    }

    /// Sets any field of an object as if someone else did it (emits a `mod`
    /// event), e.g. `("host", "OpaqueRef:host-0002", "enabled", json!(false))`.
    pub fn set_field(&self, class: &str, opaque_ref: &str, field: &str, value: Value) {
        let mut db = self.state.db();
        db.modify(&class.to_lowercase(), opaque_ref, |rec| {
            rec.insert(field.to_owned(), value);
        });
        drop(db);
        self.state.changed.notify_waiters();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(listener: TcpListener, tls: bool, state: Arc<State>) -> Running {
    let connections: Arc<Mutex<Vec<AbortHandle>>> = Arc::default();
    let conns = connections.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let state = state.clone();
            let acceptor = tls.then(|| tls_acceptor(*lock(&state.certificate)));
            let task = tokio::spawn(async move {
                let service = service_fn(move |req| handle(state.clone(), req));
                match acceptor {
                    Some(acceptor) => {
                        if let Ok(tls) = acceptor.accept(stream).await {
                            let _ = http1::Builder::new()
                                .serve_connection(TokioIo::new(tls), service)
                                .with_upgrades()
                                .await;
                        }
                    }
                    None => {
                        let _ = http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .with_upgrades()
                            .await;
                    }
                }
            });
            let mut list = lock(&conns);
            list.retain(|c| !c.is_finished());
            list.push(task.abort_handle());
        }
    });
    Running {
        accept_task,
        connections,
    }
}

fn tls_acceptor(certificate: MockCertificate) -> tokio_rustls::TlsAcceptor {
    let (cert_pem, key_pem) = certificate.pem();
    let certs =
        vec![CertificateDer::from_pem_slice(cert_pem.as_bytes()).expect("fixture certificate")];
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).expect("fixture key");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("server config");
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

struct State {
    db: Mutex<Db>,
    /// Fake VNC consoles by VM reference.
    consoles: Mutex<HashMap<String, MockConsole>>,
    changed: Notify,
    certificate: Mutex<MockCertificate>,
    unresponsive: watch::Sender<bool>,
    /// Runs background task progress (tests may call in from plain threads).
    runtime: tokio::runtime::Handle,
}

impl State {
    fn new(records: Value) -> Self {
        let mut objects = BTreeMap::new();
        let mut has_roles = false;
        if let Value::Object(classes) = records {
            for (class, objs) in classes {
                // Keys starting with `_` are comments.
                let Value::Object(objs) = objs else { continue };
                if class.starts_with('_') {
                    continue;
                }
                has_roles |= class.eq_ignore_ascii_case("role") && !objs.is_empty();
                for (opaque_ref, record) in objs {
                    objects.insert((class.to_lowercase(), opaque_ref), record);
                }
            }
        }
        if !has_roles {
            for (r, record) in ops::role_objects() {
                objects.insert(("role".to_owned(), r), record);
            }
        }
        let mut db = Db {
            objects,
            events: Vec::new(),
            seq: 0,
            sessions: HashMap::new(),
            calls: Vec::new(),
            next_session: 1,
            lose_events: false,
            unknown_classes: HashSet::new(),
            coordinator: None,
            task_steps: 4,
            task_step: Duration::from_millis(100),
            next_task: 1,
            failures: HashMap::new(),
            console_base: String::new(),
            next_console: 1,
            rrd_json: true,
            rrd_units: true,
            rrd_clock_offset: 0,
            next_message: 1,
            plugin_calls: Vec::new(),
            next_object: 1,
            storage: storage::World::default(),
        };
        db.init_sr_ops();
        // Drives without `allowed_operations` get them from their VM.
        let bare: Vec<String> = db
            .objects
            .iter()
            .filter(|((c, _), rec)| c == "vbd" && rec.get("allowed_operations").is_none())
            .map(|((_, r), _)| r.clone())
            .collect();
        for vbd in bare {
            db.refresh_vbd_ops(&vbd);
        }
        db.events.clear();
        Self {
            db: Mutex::new(db),
            consoles: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            certificate: Mutex::new(MockCertificate::Original),
            unresponsive: watch::Sender::new(false),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn console(&self, vm: &str) -> MockConsole {
        let dom0 = self
            .db()
            .objects
            .get(&("vm".to_owned(), vm.to_owned()))
            .and_then(|r| r.get("is_control_domain"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        lock(&self.consoles)
            .entry(vm.to_owned())
            .or_insert_with(|| {
                if dom0 {
                    let c = MockConsole::new(640, 384);
                    c.set_ext_key(false);
                    c
                } else {
                    MockConsole::new(640, 480)
                }
            })
            .clone()
    }

    /// A VM went from `before` to `after`: its QEMU (and so its console
    /// sessions) went away unless it was paused or unpaused, or started.
    fn qemu_restarted(&self, vm: &str, before: &str, after: &str) {
        let running = |s: &str| s == "Running" || s == "Paused";
        let kept = running(before) && running(after) && before != after;
        if running(before)
            && !kept
            && let Some(console) = lock(&self.consoles).get(vm)
        {
            console.disconnect_all();
        }
    }
}

struct StoredEvent {
    seq: u64,
    class: String,
    operation: &'static str,
    opaque_ref: String,
    snapshot: Option<Value>,
}

struct Db {
    objects: BTreeMap<(String, String), Value>,
    events: Vec<StoredEvent>,
    seq: u64,
    /// Session → its role (`None`: a local superuser).
    sessions: HashMap<String, Option<&'static str>>,
    calls: Vec<String>,
    next_session: u64,
    lose_events: bool,
    unknown_classes: HashSet<String>,
    /// Set when acting as a pool member: where logins are redirected.
    coordinator: Option<String>,
    task_steps: u32,
    task_step: Duration,
    next_task: u64,
    /// Calls made to fail ([`MockServer::fail_method`]).
    failures: HashMap<String, (String, Vec<String>)>,
    /// `scheme://host:port` of this server, for new console records.
    console_base: String,
    next_console: u64,
    /// RRD answers in JSON when asked (else always XML).
    rrd_json: bool,
    /// Data sources list their units (a VM that just started may not).
    rrd_units: bool,
    /// Seconds added to the clock of the RRD data.
    rrd_clock_offset: i64,
    next_message: u64,
    /// `host.call_plugin` calls: `(plugin, function)`.
    plugin_calls: Vec<(String, String)>,
    /// Counter for the references of objects the mock creates.
    next_object: u64,
    /// The simulated storage behind the SRs.
    storage: storage::World,
}

impl Db {
    /// Makes every console's `location` point at this server.
    fn point_consoles_at(&mut self, base: &str) {
        self.console_base = base.to_owned();
        for ((class, _), record) in self.objects.iter_mut() {
            if class != "console" {
                continue;
            }
            let Some(location) = record.get("location").and_then(Value::as_str) else {
                continue;
            };
            let path = location
                .split_once("://")
                .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
                .unwrap_or_else(|| "/console".to_owned());
            record["location"] = json!(format!("{base}{path}"));
        }
    }

    fn push_event(
        &mut self,
        class: &str,
        operation: &'static str,
        opaque_ref: &str,
        snapshot: Option<Value>,
    ) {
        self.seq += 1;
        self.events.push(StoredEvent {
            seq: self.seq,
            class: class.to_owned(),
            operation,
            opaque_ref: opaque_ref.to_owned(),
            snapshot,
        });
    }

    fn modify(&mut self, class: &str, opaque_ref: &str, f: impl FnOnce(&mut Map<String, Value>)) {
        let key = (class.to_owned(), opaque_ref.to_owned());
        let Some(Value::Object(rec)) = self.objects.get_mut(&key) else {
            return;
        };
        f(rec);
        let snapshot = Value::Object(rec.clone());
        self.push_event(class, "mod", opaque_ref, Some(snapshot));
    }

    fn get_str(&self, class: &str, opaque_ref: &str, field: &str) -> Option<String> {
        self.objects
            .get(&(class.to_owned(), opaque_ref.to_owned()))?
            .get(field)?
            .as_str()
            .map(str::to_owned)
    }

    /// Puts a VM in `power_state`, on `host` if given (a running VM stays
    /// where it is; one that starts goes to its home server or the first
    /// host).
    fn move_vm(&mut self, vm: &str, power_state: &str, host: Option<String>) {
        let old_host = self
            .get_str("vm", vm, "resident_on")
            .unwrap_or_else(|| NULL_REF.into());
        let new_host = if power_state == "Running" || power_state == "Paused" {
            let affinity = self
                .get_str("vm", vm, "affinity")
                .unwrap_or_else(|| NULL_REF.into());
            if let Some(host) = host {
                host
            } else if old_host != NULL_REF {
                old_host.clone()
            } else if affinity != NULL_REF
                && self
                    .objects
                    .contains_key(&("host".into(), affinity.clone()))
            {
                affinity
            } else {
                self.objects
                    .keys()
                    .find(|(c, _)| c == "host")
                    .map(|(_, r)| r.clone())
                    .unwrap_or_else(|| NULL_REF.into())
            }
        } else {
            NULL_REF.to_owned()
        };
        self.modify("vm", vm, |rec| {
            rec.insert("power_state".into(), json!(power_state));
            rec.insert("resident_on".into(), json!(new_host));
            let allowed = provisioning::vm_allowed(rec);
            rec.insert("allowed_operations".into(), json!(allowed));
            rec.insert("current_operations".into(), json!({}));
        });
        self.sync_consoles(vm, power_state);
        self.sync_vbds(vm);
        if old_host != new_host {
            if old_host != NULL_REF {
                self.modify("host", &old_host, |rec| {
                    if let Some(Value::Array(list)) = rec.get_mut("resident_VMs") {
                        list.retain(|v| v.as_str() != Some(vm));
                    }
                });
            }
            if new_host != NULL_REF {
                self.modify("host", &new_host, |rec| {
                    if let Some(Value::Array(list)) = rec.get_mut("resident_VMs") {
                        list.push(json!(vm));
                    }
                });
            }
        }
    }

    /// Like XAPI: a VM that starts gets a VNC console record, and a VM that
    /// stops (halted or suspended) loses its console records.
    fn sync_consoles(&mut self, vm: &str, power_state: &str) {
        let running = power_state == "Running" || power_state == "Paused";
        let current: Vec<String> = self
            .objects
            .get(&("vm".to_owned(), vm.to_owned()))
            .and_then(|r| r.get("consoles"))
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if running && current.is_empty() && !self.console_base.is_empty() {
            let n = self.next_console;
            self.next_console += 1;
            let r = format!("OpaqueRef:mock-console-{n}");
            let uuid = format!("c0c0c0c0-0000-4000-8000-{n:012}");
            let record = json!({
                "uuid": uuid, "protocol": "rfb",
                "location": format!("{}/console?uuid={uuid}", self.console_base),
                "VM": vm, "other_config": {}
            });
            self.objects
                .insert(("console".to_owned(), r.clone()), record.clone());
            self.push_event("console", "add", &r, Some(record));
            self.modify("vm", vm, |rec| {
                rec.insert("consoles".into(), json!([r]));
            });
        } else if !running && !current.is_empty() {
            for r in &current {
                if self
                    .objects
                    .remove(&("console".to_owned(), r.clone()))
                    .is_some()
                {
                    self.push_event("console", "del", r, None);
                }
            }
            self.modify("vm", vm, |rec| {
                rec.insert("consoles".into(), json!([]));
            });
        }
    }

    /// Ends a pending task.
    fn finish_task(&mut self, task: &str, status: &str, result: &str, error_info: &[&str]) {
        let progress = if status == "success" { 1.0 } else { -1.0 };
        self.modify("task", task, |rec| {
            rec.insert("status".into(), json!(status));
            if progress >= 0.0 {
                rec.insert("progress".into(), json!(progress));
            }
            rec.insert("result".into(), json!(result));
            rec.insert("error_info".into(), json!(error_info));
            rec.insert("allowed_operations".into(), json!([]));
            rec.insert("finished".into(), json!("20261125T12:00:01Z"));
        });
    }

    fn refs_of(&self, class: &str) -> Value {
        Value::Array(
            self.objects
                .keys()
                .filter(|(c, _)| c == class)
                .map(|(_, r)| json!(r))
                .collect(),
        )
    }

    /// Generic per-class calls: `get_all_records`, `get_all`, `get_record`,
    /// `get_by_uuid` and field getters. `None` if `method` is none of these.
    fn class_call(&self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        let key = |r: &str| (class.to_owned(), r.to_owned());
        Some(match call {
            "get_all_records" => Ok(self.all_records(class)),
            "get_all" => Ok(self.refs_of(class)),
            "get_record" => {
                let r = str_param(params, 1);
                match self.objects.get(&key(r)) {
                    Some(rec) => Ok(rec.clone()),
                    None => api_error("HANDLE_INVALID", &[class, r]),
                }
            }
            "get_by_uuid" => {
                let uuid = str_param(params, 1);
                match self
                    .objects
                    .iter()
                    .find(|((c, _), rec)| c == class && rec.get("uuid") == Some(&json!(uuid)))
                {
                    Some(((_, r), _)) => Ok(json!(r)),
                    None => api_error("UUID_INVALID", &[class, uuid]),
                }
            }
            _ => {
                let field = call.strip_prefix("get_")?;
                let r = str_param(params, 1);
                match self.objects.get(&key(r)) {
                    Some(rec) => Ok(rec.get(field)?.clone()),
                    None => api_error("HANDLE_INVALID", &[class, r]),
                }
            }
        })
    }

    fn all_records(&self, class: &str) -> Value {
        let map: Map<String, Value> = self
            .objects
            .iter()
            .filter(|((c, _), _)| c == class)
            .map(|((_, r), rec)| (r.clone(), rec.clone()))
            .collect();
        Value::Object(map)
    }
}

type Reply = Result<Value, (String, Vec<String>)>;

fn api_error(code: &str, params: &[&str]) -> Reply {
    Err((
        code.to_owned(),
        params.iter().map(|s| (*s).to_owned()).collect(),
    ))
}

async fn handle(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() == Method::CONNECT {
        return Ok(console_connect(&state, req));
    }
    if req.method() == Method::GET
        && matches!(req.uri().path(), "/rrd_updates" | "/host_rrds" | "/vm_rrds")
    {
        return Ok(rrd_get(&state, &req));
    }
    if req.method() != Method::POST || req.uri().path() != "/jsonrpc" {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from_static(b"not found")))
            .expect("response"));
    }
    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => Bytes::new(),
    };
    // A hung server: hold the request until released.
    let mut unresponsive = state.unresponsive.subscribe();
    let _ = unresponsive.wait_for(|hung| !hung).await;
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params = match request.get("params") {
        Some(Value::Array(p)) => p.clone(),
        _ => Vec::new(),
    };
    let reply = dispatch(&state, &method, &params).await;
    let response = match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, data)) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": 1, "message": code, "data": data}
        }),
    };
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(response.to_string())))
        .expect("response"))
}

fn plain(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(text.as_bytes())))
        .expect("response")
}

/// `CONNECT /console?uuid=…|ref=…` with `Cookie: session_id=…`: the
/// console tunnel. On success the connection becomes the VM's fake VNC
/// console.
fn console_connect(state: &Arc<State>, mut req: Request<Incoming>) -> Response<Full<Bytes>> {
    state.db().calls.push("http/connect_console".to_owned());
    if req.uri().path() != "/console" {
        return plain(StatusCode::NOT_FOUND, "not found");
    }
    let session = req
        .headers()
        .get_all(hyper::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix("session_id=").map(str::to_owned));
    let query = req.uri().query().unwrap_or_default().to_owned();
    let wanted = |key: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(str::to_owned)
    };
    let vm = {
        let db = state.db();
        if !session.is_some_and(|s| db.sessions.contains_key(&s)) {
            return plain(StatusCode::UNAUTHORIZED, "session invalid");
        }
        let console = match (wanted("ref"), wanted("uuid")) {
            (Some(r), _) => db.objects.get(&("console".to_owned(), r)),
            (None, Some(uuid)) => db.objects.iter().find_map(|((c, _), rec)| {
                (c == "console" && rec.get("uuid").and_then(Value::as_str) == Some(&uuid))
                    .then_some(rec)
            }),
            _ => None,
        };
        let Some(vm) = console
            .and_then(|c| c.get("VM"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return plain(StatusCode::NOT_FOUND, "no such console");
        };
        let power = db.get_str("vm", &vm, "power_state").unwrap_or_default();
        if power != "Running" && power != "Paused" {
            return plain(StatusCode::NOT_FOUND, "the VM is not running");
        }
        vm
    };
    let console = state.console(&vm);
    let upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        if let Ok(upgraded) = upgrade.await {
            let _ = console.serve(TokioIo::new(upgraded)).await;
        }
    });
    Response::new(Full::new(Bytes::new()))
}

/// `GET /rrd_updates`, `/host_rrds`, `/vm_rrds` with `session_id=`.
fn rrd_get(state: &Arc<State>, req: &Request<Incoming>) -> Response<Full<Bytes>> {
    let path = req.uri().path().to_owned();
    let query = rrd::Query::parse(req.uri().query().unwrap_or_default());
    let mut db = state.db();
    db.calls.push(format!("http{path}"));
    if !query
        .get("session_id")
        .is_some_and(|s| db.sessions.contains_key(s))
    {
        return plain(StatusCode::UNAUTHORIZED, "session invalid");
    }
    let json = query.get("json") == Some("true");
    let (status, content_type, body) = match path.as_str() {
        "/rrd_updates" => db.rrd_updates(&query),
        "/host_rrds" => db.full_rrd(None, json),
        _ => match query.get("uuid") {
            Some(uuid) => db.full_rrd(Some(uuid), json),
            None => (400, "text/plain", "uuid missing".to_owned()),
        },
    };
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)))
        .expect("response")
}

fn str_param(params: &[Value], i: usize) -> &str {
    params.get(i).and_then(Value::as_str).unwrap_or_default()
}

async fn dispatch(state: &Arc<State>, method: &str, params: &[Value]) -> Reply {
    state.db().calls.push(method.to_owned());
    match method {
        "session.login_with_password" => {
            let Some(role) = ops::login(str_param(params, 0), str_param(params, 1)) else {
                return api_error(
                    "SESSION_AUTHENTICATION_FAILED",
                    &[str_param(params, 0), "Authentication failure"],
                );
            };
            let mut db = state.db();
            if let Some(coordinator) = db.coordinator.clone() {
                return api_error("HOST_IS_SLAVE", &[&coordinator]);
            }
            let session = format!("OpaqueRef:mock-session-{}", db.next_session);
            db.next_session += 1;
            db.sessions.insert(session.clone(), role);
            return Ok(json!(session));
        }
        "event.from" => return event_from(state, params).await,
        _ => {}
    }

    let session = str_param(params, 0).to_owned();
    let mut db = state.db();
    let (class, call) = method.split_once('.').unwrap_or((method, ""));
    let class = class.to_lowercase();
    if db.unknown_classes.contains(&class) {
        return api_error("MESSAGE_METHOD_UNKNOWN", &[method]);
    }
    let Some(role) = db.sessions.get(&session).copied() else {
        return api_error("SESSION_INVALID", &[&session]);
    };
    if let Some(permission) = ops::required_permission(method)
        && !ops::may(role, permission)
    {
        return api_error(
            "RBAC_PERMISSION_DENIED",
            &[permission, "No permission in user session"],
        );
    }
    let sync = method.trim_start_matches("Async.");
    let failure = db.failures.get(sync).cloned();
    if let Some((code, params)) = failure.clone()
        && !method.starts_with("Async.")
    {
        let params: Vec<&str> = params.iter().map(String::as_str).collect();
        return api_error(&code, &params);
    }
    let (sync_class, sync_call) = sync.split_once('.').unwrap_or((sync, ""));
    let reply = match method {
        "session.logout" => {
            db.sessions.remove(&session);
            Ok(json!(""))
        }
        "event.inject" => {
            let (class, r) = (str_param(params, 1).to_lowercase(), str_param(params, 2));
            match db.objects.get(&(class.clone(), r.to_owned())).cloned() {
                Some(rec) => {
                    // The token precedes the injected event, so the first
                    // event.from from it returns that event.
                    let token = db.seq.to_string();
                    db.push_event(&class, "mod", r, Some(rec));
                    Ok(json!(token))
                }
                None => api_error("HANDLE_INVALID", &[&class, r]),
            }
        }
        "session.get_is_local_superuser" => Ok(json!(role.is_none())),
        "session.get_rbac_permissions" => Ok(json!(ops::permissions_of(role))),
        "VM.assert_can_boot_here" => db.assert_can_boot_here(params),
        "host.get_vms_which_prevent_evacuation" => {
            let host = str_param(params, 1);
            if db
                .objects
                .contains_key(&("host".to_owned(), host.to_owned()))
            {
                Ok(Value::Object(db.evacuation_blockers(host)))
            } else {
                api_error("HANDLE_INVALID", &["host", host])
            }
        }
        _ if sync_call == "add_to_other_config" || sync_call == "remove_from_other_config" => {
            db.other_config(&sync_class.to_lowercase(), sync_call, params)
        }
        _ if provisioning::handles(sync_class, sync_call) => {
            if method.starts_with("Async.") {
                let work = match failure {
                    Some(error) => ops::Work::Fail(error),
                    None => ops::Work::Call {
                        class: sync_class.to_owned(),
                        call: sync_call.to_owned(),
                        params: params.to_vec(),
                    },
                };
                drop(db);
                let task = spawn_task(state, method, work);
                state.changed.notify_waiters();
                return Ok(json!(task));
            }
            db.provisioning_call(sync_class, sync_call, params)
                .expect("handled call")
        }
        _ if (sync_class == "VM" || sync_class == "host")
            && ops::required_permission(method).is_some()
            && !matches!(
                sync_call,
                "assert_can_boot_here" | "get_vms_which_prevent_evacuation" | "call_plugin"
            ) =>
        {
            let checked = if sync_class == "VM" {
                db.check_vm_op(sync_call, params)
            } else {
                db.check_host_op(sync_call, params)
            };
            if method.starts_with("Async.") {
                let work = match (failure, checked) {
                    (Some(error), _) | (None, Err(error)) => ops::Work::Fail(error),
                    (None, Ok(work)) => work,
                };
                drop(db);
                let task = spawn_task(state, method, work);
                if sync_class == "VM" {
                    state
                        .db()
                        .begin_vm_op(str_param(params, 1), sync_call, &task);
                }
                state.changed.notify_waiters();
                return Ok(json!(task));
            }
            checked.map(|work| {
                let before = vm_state_before(&db, &work);
                if db.apply(&work) {
                    db.sessions.clear();
                }
                if let (Some((vm, before)), ops::Work::Vm { power_state, .. }) = (before, &work) {
                    state.qemu_restarted(&vm, &before, power_state);
                }
                json!("")
            })
        }
        "host.get_data_sources" | "VM.get_data_sources" => {
            let r = str_param(params, 1);
            db.data_sources(&class, r)
                .ok_or(())
                .or_else(|()| api_error("HANDLE_INVALID", &[&class, r]))
        }
        "message.create" => {
            let n = db.next_message;
            db.next_message += 1;
            let r = format!("OpaqueRef:mock-message-{n}");
            let record = json!({
                "uuid": format!("3e55a9e0-0000-4000-8000-{n:012}"),
                "name": str_param(params, 1),
                "priority": params.get(2).cloned().unwrap_or(json!(5)),
                "cls": str_param(params, 3),
                "obj_uuid": str_param(params, 4),
                "timestamp": "20261125T12:00:00Z",
                "body": str_param(params, 5),
            });
            db.objects
                .insert(("message".to_owned(), r.clone()), record.clone());
            db.push_event("message", "add", &r, Some(record));
            Ok(json!(r))
        }
        "message.destroy" => {
            let r = str_param(params, 1).to_owned();
            if db
                .objects
                .remove(&("message".to_owned(), r.clone()))
                .is_some()
            {
                db.push_event("message", "del", &r, None);
                Ok(json!(""))
            } else {
                api_error("HANDLE_INVALID", &["message", &r])
            }
        }
        "host.call_plugin" => {
            let (plugin, function) = (str_param(params, 2), str_param(params, 3));
            db.plugin_calls
                .push((plugin.to_owned(), function.to_owned()));
            match (plugin, function) {
                ("perfmon", "refresh" | "start" | "stop") => Ok(json!("")),
                ("trim", "do_trim") => db.trim(params),
                _ => api_error("XENAPI_MISSING_PLUGIN", &[plugin]),
            }
        }
        "host.get_servertime" => {
            let host = str_param(params, 1);
            if db
                .objects
                .contains_key(&("host".to_owned(), host.to_owned()))
            {
                Ok(json!("20261125T12:00:00Z"))
            } else {
                api_error("HANDLE_INVALID", &["host", host])
            }
        }
        "task.cancel" => {
            let task = str_param(params, 1).to_owned();
            match db.get_str("task", &task, "status").as_deref() {
                None => api_error("HANDLE_INVALID", &["task", &task]),
                Some("pending") => {
                    db.finish_task(&task, "cancelled", "", &["TASK_CANCELLED", &task]);
                    Ok(json!(""))
                }
                Some(_) => api_error("OPERATION_NOT_ALLOWED", &["task is not pending"]),
            }
        }
        "task.destroy" => {
            let task = str_param(params, 1).to_owned();
            if db
                .objects
                .remove(&("task".to_owned(), task.clone()))
                .is_some()
            {
                db.push_event("task", "del", &task, None);
                Ok(json!(""))
            } else {
                api_error("HANDLE_INVALID", &["task", &task])
            }
        }
        _ => db
            .class_call(&class, call, params)
            .unwrap_or_else(|| api_error("MESSAGE_METHOD_UNKNOWN", &[method])),
    };
    drop(db);
    state.changed.notify_waiters();
    reply
}

/// Creates a pending `task` object named `name` and advances it in the
/// background; returns its reference.
fn spawn_task(state: &Arc<State>, name: &str, work: ops::Work) -> String {
    let (task, steps, step) = {
        let mut db = state.db();
        let task = format!("OpaqueRef:mock-task-{}", db.next_task);
        let uuid = format!("7a5c0000-0000-4000-8000-{:012}", db.next_task);
        db.next_task += 1;
        let resident_on = db
            .objects
            .keys()
            .find(|(c, _)| c == "host")
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| NULL_REF.into());
        let record = json!({
            "uuid": uuid, "name_label": name, "name_description": "",
            "allowed_operations": ["cancel"], "current_operations": {},
            "created": "20261125T12:00:00Z", "finished": "19700101T00:00:00Z",
            "status": "pending", "resident_on": resident_on, "progress": 0.0,
            "type": "", "result": "", "error_info": [], "other_config": {},
            "subtask_of": NULL_REF, "subtasks": [], "backtrace": "()"
        });
        db.objects
            .insert(("task".to_owned(), task.clone()), record.clone());
        db.push_event("task", "add", &task, Some(record));
        (task, db.task_steps, db.task_step)
    };
    state.changed.notify_waiters();
    let state = state.clone();
    let task_ref = task.clone();
    let runtime = state.runtime.clone();
    runtime.spawn(async move {
        for i in 1..=steps {
            tokio::time::sleep(step).await;
            let mut db = state.db();
            if db.get_str("task", &task_ref, "status").as_deref() != Some("pending") {
                // Cancelled or destroyed: the VM is free again.
                if let ops::Work::Vm { vm, .. } = &work {
                    db.settle_vm(vm);
                }
                drop(db);
                state.changed.notify_waiters();
                return;
            }
            if i < steps {
                let progress = f64::from(i) / f64::from(steps);
                db.modify("task", &task_ref, |rec| {
                    rec.insert("progress".into(), json!(progress));
                });
            } else {
                match &work {
                    ops::Work::Call {
                        class,
                        call,
                        params,
                    } => match db.provisioning_call(class, call, params) {
                        Some(Ok(value)) => {
                            let result = match value.as_str() {
                                Some(r) if !r.is_empty() => format!(
                                    "<value>{}</value>",
                                    r.replace('&', "&amp;")
                                        .replace('<', "&lt;")
                                        .replace('>', "&gt;")
                                ),
                                _ => String::new(),
                            };
                            db.finish_task(&task_ref, "success", &result, &[]);
                        }
                        Some(Err((code, params))) => {
                            let mut info = vec![code.as_str()];
                            info.extend(params.iter().map(String::as_str));
                            db.finish_task(&task_ref, "failure", "", &info);
                        }
                        None => {
                            let name = format!("{class}.{call}");
                            db.finish_task(
                                &task_ref,
                                "failure",
                                "",
                                &["MESSAGE_METHOD_UNKNOWN", &name],
                            );
                        }
                    },
                    ops::Work::Fail((code, params)) => {
                        let mut info = vec![code.as_str()];
                        info.extend(params.iter().map(String::as_str));
                        db.finish_task(&task_ref, "failure", "", &info);
                        // A VM whose operation failed is free again.
                        if let Some(vm) = db
                            .get_str("task", &task_ref, "name_label")
                            .filter(|n| n.starts_with("Async.VM."))
                            .and_then(|_| vm_of_task(&db, &task_ref))
                        {
                            db.settle_vm(&vm);
                        }
                    }
                    work => {
                        let before = vm_state_before(&db, work);
                        let restart = db.apply(work);
                        db.finish_task(&task_ref, "success", "", &[]);
                        if let (Some((vm, before)), ops::Work::Vm { power_state, .. }) =
                            (before, work)
                        {
                            state.qemu_restarted(&vm, &before, power_state);
                        }
                        if restart {
                            // The toolstack restarted: every session is gone.
                            db.sessions.clear();
                        }
                    }
                }
            }
            drop(db);
            state.changed.notify_waiters();
        }
    });
    task
}

/// For VM work: the VM and its power state before the work is applied.
fn vm_state_before(db: &Db, work: &ops::Work) -> Option<(String, String)> {
    match work {
        ops::Work::Vm { vm, .. } => Some((
            vm.clone(),
            db.get_str("vm", vm, "power_state").unwrap_or_default(),
        )),
        _ => None,
    }
}

/// The VM whose `current_operations` names `task`.
fn vm_of_task(db: &Db, task: &str) -> Option<String> {
    db.objects
        .iter()
        .find(|((c, _), rec)| {
            c == "vm"
                && rec
                    .get("current_operations")
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.contains_key(task))
        })
        .map(|((_, r), _)| r.clone())
}

async fn event_from(state: &State, params: &[Value]) -> Reply {
    let session = str_param(params, 0).to_owned();
    let classes: Vec<String> = match params.get(1) {
        Some(Value::Array(c)) => c
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_lowercase)
            .collect(),
        _ => Vec::new(),
    };
    let token = str_param(params, 2).to_owned();
    let timeout = params
        .get(3)
        .and_then(Value::as_f64)
        .unwrap_or(30.0)
        .clamp(0.0, 60.0);
    let wanted = |class: &str| classes.iter().any(|c| c == "*" || c == class);

    let since: u64 = if token.is_empty() {
        let db = state.db();
        if !db.sessions.contains_key(&session) {
            return api_error("SESSION_INVALID", &[&session]);
        }
        let events: Vec<Value> = db
            .objects
            .iter()
            .filter(|((c, _), _)| wanted(c))
            .map(|((c, r), rec)| json!({"id": "0", "class": c, "operation": "add", "ref": r, "snapshot": rec}))
            .collect();
        return Ok(json!({"events": events, "valid_ref_counts": {}, "token": db.seq.to_string()}));
    } else {
        match token.parse() {
            Ok(t) => t,
            Err(_) => return api_error("EVENT_FROM_TOKEN_PARSE_FAILURE", &[&token]),
        }
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(timeout);
    loop {
        let notified = state.changed.notified();
        {
            let mut db = state.db();
            if !db.sessions.contains_key(&session) {
                return api_error("SESSION_INVALID", &[&session]);
            }
            if std::mem::take(&mut db.lose_events) {
                return api_error("EVENTS_LOST", &[]);
            }
            let events: Vec<Value> = db
                .events
                .iter()
                .filter(|e| e.seq > since && wanted(&e.class))
                .map(|e| {
                    json!({
                        "id": e.seq.to_string(), "class": e.class, "operation": e.operation,
                        "ref": e.opaque_ref, "snapshot": e.snapshot
                    })
                })
                .collect();
            if !events.is_empty() || tokio::time::Instant::now() >= deadline {
                return Ok(
                    json!({"events": events, "valid_ref_counts": {}, "token": db.seq.to_string()}),
                );
            }
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            // Timed out: loop once more to return an empty batch.
        }
    }
}
