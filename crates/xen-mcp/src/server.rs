//! The MCP server's state: the backend it drives, the servers it connects
//! to, the configuration and the audit log; and what every tool shares
//! (arguments, finding a server and an object, the audit entry).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use xapi::{Endpoint, Ref};
use xen_core::branding;
use xen_core::{
    BackendHandle, BackendOptions, Command, ConnectionError, ConnectionId, ConnectionStatus,
    ConnectionView, Notification,
};

use crate::audit::{AuditEntry, AuditLog, Outcome, redact, rfc3339};
use crate::config::{McpConfig, same_server};

/// Why a tool did not do what was asked. The text goes back to the client
/// and into the audit log, so it never holds a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// A rule refused it before any call reached a server.
    Refused(String),
    /// Bad arguments or an unknown object.
    Invalid(String),
    /// The server is unreachable or the operation failed.
    Failed(String),
}

impl ToolError {
    pub fn message(&self) -> String {
        match self {
            Self::Refused(m) => format!("refused: {m}"),
            Self::Invalid(m) | Self::Failed(m) => m.clone(),
        }
    }

    fn outcome(&self) -> Outcome {
        match self {
            Self::Refused(_) => Outcome::Refused,
            Self::Invalid(_) | Self::Failed(_) => Outcome::Error,
        }
    }
}

pub type ToolResult = Result<Value, ToolError>;

/// What a call resolved to, for the audit log.
#[derive(Debug, Default)]
pub struct Call {
    pub server: Option<String>,
    pub target: Option<Value>,
    /// The operation's end, for write tools.
    pub detail: Option<String>,
}

/// A tool's arguments, with typed accessors that refuse wrong types and
/// unknown names.
pub struct Args<'a>(pub &'a Map<String, Value>);

impl<'a> Args<'a> {
    /// Refuses arguments not in `known`.
    pub fn only(&self, known: &[&str]) -> Result<(), ToolError> {
        match self.0.keys().find(|k| !known.contains(&k.as_str())) {
            Some(k) => Err(ToolError::Invalid(format!(
                "unknown argument '{k}' (expected: {})",
                known.join(", ")
            ))),
            None => Ok(()),
        }
    }

    pub fn str(&self, key: &str) -> Result<Option<&'a str>, ToolError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(ToolError::Invalid(format!("'{key}' must be a string"))),
        }
    }

    pub fn req_str(&self, key: &str) -> Result<&'a str, ToolError> {
        self.str(key)?
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::Invalid(format!("'{key}' is required")))
    }

    pub fn bool(&self, key: &str) -> Result<Option<bool>, ToolError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(ToolError::Invalid(format!("'{key}' must be true or false"))),
        }
    }

    pub fn u64(&self, key: &str) -> Result<Option<u64>, ToolError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| {
                ToolError::Invalid(format!("'{key}' must be a non-negative integer"))
            }),
        }
    }

    pub fn str_list(&self, key: &str) -> Result<Option<Vec<&'a str>>, ToolError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str().ok_or_else(|| {
                        ToolError::Invalid(format!("'{key}' must be a list of strings"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(ToolError::Invalid(format!(
                "'{key}' must be a list of strings"
            ))),
        }
    }
}

/// A saved server the MCP server connects to.
#[derive(Debug)]
pub struct Slot {
    /// The saved address (the server list's key).
    pub address: String,
    pub username: String,
    id: Mutex<Option<ConnectionId>>,
}

impl Slot {
    fn id(&self) -> Option<ConnectionId> {
        *self.id.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Timings, from the configuration (tests make them short).
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub connect_wait: Duration,
    pub operation_timeout: Duration,
    pub performance_wait: Duration,
    /// How long a finished operation waits for the cache to show its
    /// effect before answering.
    pub settle_wait: Duration,
}

impl Timings {
    fn of(config: &McpConfig) -> Self {
        Self {
            connect_wait: Duration::from_secs(config.connect_wait_seconds),
            operation_timeout: Duration::from_secs(config.operation_timeout_seconds),
            performance_wait: Duration::from_secs(config.performance_wait_seconds),
            settle_wait: Duration::from_secs(30),
        }
    }
}

/// The MCP server: one backend, its connections, the rules and the log.
pub struct McpServer {
    pub(crate) backend: BackendHandle,
    pub(crate) config: McpConfig,
    pub(crate) slots: Vec<Slot>,
    /// Saved servers named in `servers` that are not in the saved list.
    pub(crate) unknown: Vec<String>,
    /// Why a server could not connect, by server address (certificate not
    /// trusted, refused before connecting).
    problems: Mutex<HashMap<String, String>>,
    pub(crate) audit: AuditLog,
    pub(crate) timings: Timings,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

impl McpServer {
    /// Starts the backend on the current tokio runtime and connects the
    /// saved servers `config` selects, with their saved passwords. Only
    /// pinned certificates are accepted: an unknown or changed one is
    /// refused (trust it once in the window first).
    pub async fn start(
        mut options: BackendOptions,
        config: McpConfig,
        audit: AuditLog,
    ) -> Arc<Self> {
        options.restore_connections = false;
        let settings = options.store.settings();
        let mut backend =
            BackendHandle::spawn(&tokio::runtime::Handle::current(), options, Arc::new(|| {}));
        let notifications = backend.take_notifications();
        let unknown = config
            .servers
            .iter()
            .filter(|s| !settings.servers.iter().any(|x| same_server(s, &x.address)))
            .cloned()
            .collect();
        let slots: Vec<Slot> = settings
            .servers
            .iter()
            .filter(|s| config.selects(&s.address))
            .map(|s| Slot {
                address: s.address.clone(),
                username: s.username.clone(),
                id: Mutex::new(None),
            })
            .collect();
        let timings = Timings::of(&config);
        let server = Arc::new(Self {
            backend,
            config,
            slots,
            unknown,
            problems: Mutex::new(HashMap::new()),
            audit,
            timings,
        });
        tokio::spawn(pump(Arc::downgrade(&server), notifications));
        for slot in &server.slots {
            tracing::info!(server = %slot.address, "connecting");
            server.backend.send(Command::Connect {
                server: slot.address.clone(),
                username: slot.username.clone(),
                password: None,
                remember_password: true,
            });
        }
        server.bind_slots().await;
        server
    }

    /// Learns the connection id of each slot: the backend creates the
    /// connection entry, under the saved address, as it takes the command.
    async fn bind_slots(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.backend.state();
            let mut missing = false;
            for slot in &self.slots {
                let mut id = slot.id.lock().unwrap_or_else(PoisonError::into_inner);
                if id.is_none() {
                    *id = state.connection_to(&display(&slot.address)).map(|c| c.id);
                    missing |= id.is_none() && self.problem(&slot.address, None).is_none();
                }
            }
            if !missing || Instant::now() > deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn set_problem(&self, server: &str, text: String) {
        self.problems
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(display(server), text);
    }

    fn problem(&self, address: &str, view: Option<&ConnectionView>) -> Option<String> {
        let problems = self.problems.lock().unwrap_or_else(PoisonError::into_inner);
        problems
            .get(&display(address))
            .or_else(|| view.and_then(|v| problems.get(&v.server)))
            .cloned()
    }

    /// Logs out of every server (the backend stops).
    pub async fn shutdown(&self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.backend.send(Command::Shutdown { done });
        let _ =
            tokio::task::spawn_blocking(move || wait.recv_timeout(Duration::from_secs(10))).await;
    }

    /// Whether the write tools are switched on.
    pub fn writes_enabled(&self) -> bool {
        self.config.writes.enabled
    }

    /// The slot's connection as last published.
    pub(crate) fn view(&self, slot: &Slot) -> Option<Arc<ConnectionView>> {
        let id = slot.id()?;
        self.backend.state().connection(id).cloned()
    }

    /// A slot's state, as `list_servers` shows it.
    pub(crate) fn slot_status(&self, slot: &Slot) -> Value {
        let view = self.view(slot);
        let status = match &view {
            Some(v) => status_text(&v.status),
            None => "not connected".to_owned(),
        };
        let mut out = json!({
            "address": slot.address,
            "username": slot.username,
            "status": status,
        });
        if let Some(v) = &view {
            if v.server != display(&slot.address) {
                out["connected_to"] = json!(v.server);
            }
            if let Some((_, pool)) = v.model.pool() {
                out["pool"] = json!({
                    "uuid": pool.uuid,
                    "name": xen_core::objects::pool_name(&v.model, pool),
                });
            }
            if let ConnectionStatus::Connected { api_version } = &v.status {
                out["api_version"] = json!(format!("{}.{}", api_version.0, api_version.1));
            }
        }
        if let Some(p) = self.problem(&slot.address, view.as_deref()) {
            out["problem"] = json!(p);
        }
        let addresses = [slot.address.as_str()];
        out["writes"] = json!({
            "enabled": self.config.writes.enabled,
            "allow": self.config.allowed_for(&addresses).unwrap_or_default(),
        });
        out
    }

    /// The slot `server` names (or the only one), without waiting.
    pub(crate) fn slot(&self, server: Option<&str>) -> Result<&Slot, ToolError> {
        match server {
            None => match self.slots.as_slice() {
                [only] => Ok(only),
                [] => Err(ToolError::Invalid(format!(
                    "no server is configured: save one with `{} --add-server` or in the window",
                    branding::BINARY_NAME
                ))),
                many => Err(ToolError::Invalid(format!(
                    "several servers are configured; pass 'server' (one of: {})",
                    many.iter()
                        .map(|s| s.address.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
            },
            Some(name) => self
                .slots
                .iter()
                .find(|s| {
                    same_server(&s.address, name)
                        || self.view(s).is_some_and(|v| {
                            same_server(&v.server, name)
                                || v.model.pool().is_some_and(|(_, p)| {
                                    p.uuid.eq_ignore_ascii_case(name)
                                        || xen_core::objects::pool_name(&v.model, p) == name
                                })
                        })
                })
                .ok_or_else(|| {
                    ToolError::Invalid(format!(
                        "'{name}' is not a configured server (see list_servers)"
                    ))
                }),
        }
    }

    /// The connection of `server`, once connected (waiting while it
    /// connects). Records the server for the audit log.
    pub(crate) async fn connection(
        &self,
        call: &mut Call,
        server: Option<&str>,
    ) -> Result<(&Slot, Arc<ConnectionView>), ToolError> {
        let slot = self.slot(server)?;
        call.server = Some(slot.address.clone());
        let view = self.connected(slot).await?;
        Ok((slot, view))
    }

    pub(crate) async fn connected(&self, slot: &Slot) -> Result<Arc<ConnectionView>, ToolError> {
        let deadline = Instant::now() + self.timings.connect_wait;
        loop {
            let view = self.view(slot);
            if let Some(v) = &view {
                match &v.status {
                    ConnectionStatus::Connected { .. } => return Ok(v.clone()),
                    ConnectionStatus::Failed { .. } => {
                        return Err(self.not_connected(slot, Some(v)));
                    }
                    _ => {}
                }
            } else if self.problem(&slot.address, None).is_some() {
                return Err(self.not_connected(slot, None));
            }
            if Instant::now() >= deadline {
                return Err(self.not_connected(slot, view.as_deref()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn not_connected(&self, slot: &Slot, view: Option<&ConnectionView>) -> ToolError {
        let status = view.map_or_else(|| "not connected".to_owned(), |v| status_text(&v.status));
        let problem = self
            .problem(&slot.address, view)
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        ToolError::Failed(format!(
            "server {} is not connected: {status}{problem}",
            slot.address
        ))
    }

    /// Writes the audit entry of a finished call.
    pub(crate) fn audit(
        &self,
        tool: &str,
        arguments: &Value,
        call: Call,
        result: &ToolResult,
        started: Instant,
    ) {
        let (outcome, detail) = match result {
            Ok(_) => (Outcome::Ok, call.detail),
            Err(e) => (e.outcome(), Some(e.message())),
        };
        self.audit.record(&AuditEntry {
            time: rfc3339(SystemTime::now()),
            tool: tool.to_owned(),
            arguments: redact(arguments),
            server: call.server,
            target: call.target,
            outcome,
            detail,
            duration_ms: started.elapsed().as_millis(),
        });
    }
}

/// Drains the backend's notifications: refuses certificate questions
/// (only pinned certificates are used here) and remembers why a server
/// could not connect.
async fn pump(server: std::sync::Weak<McpServer>, mut rx: UnboundedReceiver<Notification>) {
    while let Some(n) = rx.recv().await {
        let Some(server) = server.upgrade() else {
            return;
        };
        match n {
            Notification::CertificatePrompt(p) => {
                let text = match &p.previous {
                    Some(_) => format!(
                        "the server's certificate changed (SHA-256 {}); refused. If the change is expected, connect once with the {} window and trust it",
                        p.fingerprint,
                        branding::PRODUCT_NAME
                    ),
                    None => format!(
                        "the server's certificate is not trusted yet (SHA-256 {}); refused. Connect once with the {} window and trust it",
                        p.fingerprint,
                        branding::PRODUCT_NAME
                    ),
                };
                tracing::warn!(server = %p.server, "untrusted certificate refused");
                server.set_problem(&p.server, text);
                p.reject();
            }
            Notification::ConnectionRefused { server: s, error } => {
                server.set_problem(&s, describe_connection_error(&error));
            }
            _ => {}
        }
    }
}

/// `address` in the backend's display form (the connection's `server`).
fn display(address: &str) -> String {
    Endpoint::parse(address).map_or_else(|_| address.to_owned(), |e| e.to_string())
}

pub(crate) fn status_text(status: &ConnectionStatus) -> String {
    match status {
        ConnectionStatus::Connecting => "connecting".into(),
        ConnectionStatus::Connected { .. } => "connected".into(),
        ConnectionStatus::Reconnecting { attempt, error, .. } => format!(
            "reconnecting (attempt {attempt}): {}",
            describe_connection_error(error)
        ),
        ConnectionStatus::Failed { error } => {
            format!("failed: {}", describe_connection_error(error))
        }
    }
}

/// A connection error as text (no secret: XAPI errors carry codes and
/// parameters, never the password, and session references are redacted
/// by the client).
pub(crate) fn describe_connection_error(e: &ConnectionError) -> String {
    match e {
        ConnectionError::CertificateRejected => "certificate not trusted".into(),
        ConnectionError::PasswordRequired => {
            format!(
                "no saved password for this server (save it with `{} --add-server` or Remember password in the window)",
                branding::BINARY_NAME
            )
        }
        ConnectionError::PasswordUnavailable(e) => {
            format!("the saved password could not be read from the keyring: {e}")
        }
        ConnectionError::Connect(e) => format!("could not connect: {e}"),
        ConnectionError::Unsupported(e) => format!("unsupported server: {e}"),
        ConnectionError::AlreadyConnected { existing } => {
            format!("already connected as {existing}")
        }
        ConnectionError::TooManyRedirects => "too many pool coordinator redirects".into(),
        ConnectionError::Lost(e) => format!("connection lost: {e}"),
    }
}

/// Finds one object among `items` (reference, UUID, name) by OpaqueRef,
/// UUID (any case) or exact name; a name shared by several objects is an
/// error listing their UUIDs.
pub(crate) fn resolve<'a, T: 'a>(
    items: impl Iterator<Item = (&'a Ref<T>, &'a str, &'a str)>,
    query: &str,
    what: &str,
) -> Result<&'a Ref<T>, ToolError> {
    let query = query.trim();
    let items: Vec<_> = items.collect();
    if let Some((r, _, _)) = items
        .iter()
        .find(|(r, uuid, _)| r.as_str() == query || uuid.eq_ignore_ascii_case(query))
    {
        return Ok(r);
    }
    let named: Vec<_> = items.iter().filter(|(_, _, name)| *name == query).collect();
    match named.as_slice() {
        [(r, _, _)] => Ok(r),
        [] => Err(ToolError::Invalid(format!(
            "no {what} has the UUID or name '{query}'"
        ))),
        many => Err(ToolError::Invalid(format!(
            "several {what}s are named '{query}'; use a UUID: {}",
            many.iter()
                .map(|(_, uuid, _)| *uuid)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_by_ref_uuid_and_unique_name() {
        let refs = [
            Ref::<xapi::Vm>::new("OpaqueRef:a"),
            Ref::new("OpaqueRef:b"),
            Ref::new("OpaqueRef:c"),
        ];
        let items = || {
            refs.iter()
                .zip([("U-A", "web"), ("u-b", "db"), ("u-c", "db")])
                .map(|(r, (u, n))| (r, u, n))
        };
        assert_eq!(resolve(items(), "web", "VM").unwrap(), &refs[0]);
        assert_eq!(resolve(items(), "u-a", "VM").unwrap(), &refs[0]);
        assert_eq!(resolve(items(), "OpaqueRef:c", "VM").unwrap(), &refs[2]);
        let e = resolve(items(), "db", "VM").unwrap_err();
        assert!(e.message().contains("u-b, u-c"), "{e:?}");
        assert!(resolve(items(), "nope", "VM").is_err());
    }

    #[test]
    fn arguments_are_typed() {
        let map = serde_json::json!({"vm": "x", "force": true, "n": 3, "list": ["a"]});
        let args = Args(map.as_object().unwrap());
        assert_eq!(args.req_str("vm").unwrap(), "x");
        assert_eq!(args.bool("force").unwrap(), Some(true));
        assert_eq!(args.u64("n").unwrap(), Some(3));
        assert_eq!(args.str_list("list").unwrap(), Some(vec!["a"]));
        assert!(args.bool("vm").is_err());
        assert!(args.req_str("missing").is_err());
        assert!(args.only(&["vm", "force", "n"]).is_err());
        assert!(args.only(&["vm", "force", "n", "list"]).is_ok());
    }
}
