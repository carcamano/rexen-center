//! A mock XAPI JSON-RPC server for tests.
//!
//! It serves `POST /jsonrpc` over plain HTTP or HTTPS (with the self-signed
//! certificate in `tests/fixtures/`) and keeps an in-memory object database
//! loaded from `tests/fixtures/records.json`. It implements just enough of
//! XAPI for the phase F1 flow:
//!
//! * `session.login_with_password` / `session.logout` (user [`USERNAME`],
//!   password [`PASSWORD`]; anything else fails with
//!   `SESSION_AUTHENTICATION_FAILED`);
//! * `pool.get_all_records`, `host.get_all_records`, `host.get_record`,
//!   `VM.get_all_records`;
//! * `VM.start` / `VM.clean_shutdown` with power-state checks;
//! * `event.from` with tokens and long-polling.
//!
//! Tests can also change objects behind the client's back
//! ([`MockServer::set_vm_power_state`]) to prove that updates arrive through
//! `event.from` alone.

use std::collections::{BTreeMap, HashSet};
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
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Login user accepted by the mock.
pub const USERNAME: &str = "root";
/// Login password accepted by the mock (test-only, not a real credential).
pub const PASSWORD: &str = "mock-password";

const RECORDS_JSON: &str = include_str!("../../../tests/fixtures/records.json");
const CERT_PEM: &str = include_str!("../../../tests/fixtures/mock-server.cert.pem");
const KEY_PEM: &str = include_str!("../../../tests/fixtures/mock-server.key.pem");

const NULL_REF: &str = "OpaqueRef:NULL";
const ALLOWED_HALTED: &[&str] = &[
    "start",
    "start_on",
    "clone",
    "copy",
    "export",
    "destroy",
    "snapshot",
    "make_into_template",
];
const ALLOWED_RUNNING: &[&str] = &[
    "clean_shutdown",
    "clean_reboot",
    "hard_shutdown",
    "hard_reboot",
    "suspend",
    "pause",
    "snapshot",
    "pool_migrate",
];

/// DER bytes of the certificate the HTTPS mock presents.
pub fn certificate_der() -> Vec<u8> {
    CertificateDer::from_pem_slice(CERT_PEM.as_bytes())
        .expect("fixture certificate")
        .as_ref()
        .to_vec()
}

/// The fixture database (`tests/fixtures/records.json`).
pub fn fixture_records() -> Value {
    serde_json::from_str(RECORDS_JSON).expect("fixture records")
}

/// A running mock server. Stops when dropped.
pub struct MockServer {
    addr: SocketAddr,
    tls: bool,
    state: Arc<State>,
    accept_task: JoinHandle<()>,
}

impl MockServer {
    /// Starts a mock with the fixture database on an ephemeral localhost port.
    pub async fn start(tls: bool) -> Self {
        Self::start_with_records(tls, fixture_records()).await
    }

    /// Starts a mock with a custom database (`{"pool": {ref: record}, "host": …, "vm": …}`).
    pub async fn start_with_records(tls: bool, records: Value) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(State::new(records));
        let acceptor = tls.then(tls_acceptor);
        let accept_state = state.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let state = accept_state.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(state.clone(), req));
                    match acceptor {
                        Some(acceptor) => {
                            if let Ok(tls) = acceptor.accept(stream).await {
                                let _ = http1::Builder::new()
                                    .serve_connection(TokioIo::new(tls), service)
                                    .await;
                            }
                        }
                        None => {
                            let _ = http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service)
                                .await;
                        }
                    }
                });
            }
        });
        Self {
            addr,
            tls,
            state,
            accept_task,
        }
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

    /// Methods called so far, in order (parameters are not recorded).
    pub fn calls(&self) -> Vec<String> {
        self.state.db().calls.clone()
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
            .get(&(class.to_owned(), opaque_ref.to_owned()))
            .cloned()
    }

    /// Changes a VM's power state as if someone else did it (emits events).
    pub fn set_vm_power_state(&self, vm: &str, power_state: &str) {
        let mut db = self.state.db();
        db.set_power_state(vm, power_state);
        drop(db);
        self.state.changed.notify_waiters();
    }

    /// Renames an object as if someone else did it (emits a `mod` event).
    pub fn set_name_label(&self, class: &str, opaque_ref: &str, name: &str) {
        let mut db = self.state.db();
        db.modify(class, opaque_ref, |rec| {
            rec.insert("name_label".into(), json!(name));
        });
        drop(db);
        self.state.changed.notify_waiters();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let certs =
        vec![CertificateDer::from_pem_slice(CERT_PEM.as_bytes()).expect("fixture certificate")];
    let key = PrivateKeyDer::from_pem_slice(KEY_PEM.as_bytes()).expect("fixture key");
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
    changed: Notify,
}

impl State {
    fn new(records: Value) -> Self {
        let mut objects = BTreeMap::new();
        if let Value::Object(classes) = records {
            for (class, objs) in classes {
                let Value::Object(objs) = objs else { continue };
                for (opaque_ref, record) in objs {
                    objects.insert((class.to_lowercase(), opaque_ref), record);
                }
            }
        }
        Self {
            db: Mutex::new(Db {
                objects,
                events: Vec::new(),
                seq: 0,
                sessions: HashSet::new(),
                calls: Vec::new(),
                next_session: 1,
            }),
            changed: Notify::new(),
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
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
    sessions: HashSet<String>,
    calls: Vec<String>,
    next_session: u64,
}

impl Db {
    fn modify(&mut self, class: &str, opaque_ref: &str, f: impl FnOnce(&mut Map<String, Value>)) {
        let key = (class.to_owned(), opaque_ref.to_owned());
        let Some(Value::Object(rec)) = self.objects.get_mut(&key) else {
            return;
        };
        f(rec);
        let snapshot = Value::Object(rec.clone());
        self.seq += 1;
        self.events.push(StoredEvent {
            seq: self.seq,
            class: class.to_owned(),
            operation: "mod",
            opaque_ref: opaque_ref.to_owned(),
            snapshot: Some(snapshot),
        });
    }

    fn get_str(&self, class: &str, opaque_ref: &str, field: &str) -> Option<String> {
        self.objects
            .get(&(class.to_owned(), opaque_ref.to_owned()))?
            .get(field)?
            .as_str()
            .map(str::to_owned)
    }

    fn set_power_state(&mut self, vm: &str, power_state: &str) {
        let old_host = self
            .get_str("vm", vm, "resident_on")
            .unwrap_or_else(|| NULL_REF.into());
        let new_host = if power_state == "Running" || power_state == "Paused" {
            let affinity = self
                .get_str("vm", vm, "affinity")
                .unwrap_or_else(|| NULL_REF.into());
            if affinity != NULL_REF
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
        let allowed: &[&str] = match power_state {
            "Running" => ALLOWED_RUNNING,
            "Halted" => ALLOWED_HALTED,
            _ => &["hard_shutdown"],
        };
        self.modify("vm", vm, |rec| {
            rec.insert("power_state".into(), json!(power_state));
            rec.insert("resident_on".into(), json!(new_host));
            rec.insert("allowed_operations".into(), json!(allowed));
            rec.insert("current_operations".into(), json!({}));
        });
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

fn str_param(params: &[Value], i: usize) -> &str {
    params.get(i).and_then(Value::as_str).unwrap_or_default()
}

async fn dispatch(state: &State, method: &str, params: &[Value]) -> Reply {
    state.db().calls.push(method.to_owned());
    match method {
        "session.login_with_password" => {
            if str_param(params, 0) != USERNAME || str_param(params, 1) != PASSWORD {
                return api_error(
                    "SESSION_AUTHENTICATION_FAILED",
                    &[str_param(params, 0), "Authentication failure"],
                );
            }
            let mut db = state.db();
            let session = format!("OpaqueRef:mock-session-{}", db.next_session);
            db.next_session += 1;
            db.sessions.insert(session.clone());
            return Ok(json!(session));
        }
        "event.from" => return event_from(state, params).await,
        _ => {}
    }

    let session = str_param(params, 0).to_owned();
    let mut db = state.db();
    if !db.sessions.contains(&session) {
        return api_error("SESSION_INVALID", &[&session]);
    }
    let reply = match method {
        "session.logout" => {
            db.sessions.remove(&session);
            Ok(json!(""))
        }
        "pool.get_all_records" => Ok(db.all_records("pool")),
        "host.get_all_records" => Ok(db.all_records("host")),
        "VM.get_all_records" => Ok(db.all_records("vm")),
        "host.get_record" => {
            let host = str_param(params, 1);
            match db.objects.get(&("host".into(), host.to_owned())) {
                Some(rec) => Ok(rec.clone()),
                None => api_error("HANDLE_INVALID", &["host", host]),
            }
        }
        "VM.start" | "VM.clean_shutdown" => {
            let vm = str_param(params, 1).to_owned();
            let (required, op, target) = if method == "VM.start" {
                ("Halted", "start", "Running")
            } else {
                ("Running", "clean_shutdown", "Halted")
            };
            match db.objects.get(&("vm".into(), vm.clone())) {
                None => api_error("HANDLE_INVALID", &["VM", &vm]),
                Some(rec) => {
                    let current = rec
                        .get("power_state")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let is_guest = !rec
                        .get("is_a_template")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        && !rec
                            .get("is_control_domain")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    if !is_guest {
                        api_error("OPERATION_NOT_ALLOWED", &[op])
                    } else if current != required {
                        api_error(
                            "VM_BAD_POWER_STATE",
                            &[&vm, &required.to_lowercase(), &current.to_lowercase()],
                        )
                    } else {
                        db.set_power_state(&vm, target);
                        Ok(json!(""))
                    }
                }
            }
        }
        _ => api_error("MESSAGE_METHOD_UNKNOWN", &[method]),
    };
    drop(db);
    state.changed.notify_waiters();
    reply
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
        if !db.sessions.contains(&session) {
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
            let db = state.db();
            if !db.sessions.contains(&session) {
                return api_error("SESSION_INVALID", &[&session]);
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
