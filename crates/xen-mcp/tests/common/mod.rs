//! A scripted MCP client talking to the server over in-memory pipes (the
//! same line protocol as stdio), with the server connected to the mock.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use xapi::{Endpoint, Fingerprint};
use xapi_mock::MockServer;
use xen_core::{BackendOptions, MemorySecretStore, SavedServer, SecretStore, Settings, Store};
use xen_mcp::{AuditLog, McpConfig, McpServer};

pub const WAIT: Duration = Duration::from_secs(15);

/// Calls a connection makes by itself (event loop, heartbeat).
pub const BACKGROUND: &[&str] = &["event.from", "host.get_servertime"];

pub struct Client {
    writer: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next_id: i64,
    /// Every line the server sent.
    pub received: Vec<String>,
    pub server: Arc<McpServer>,
    pub audit_dir: PathBuf,
}

pub fn options(store: Arc<Store>) -> BackendOptions {
    BackendOptions {
        originator: "xen-mcp-tests".into(),
        store,
        event_timeout: 2.0,
        heartbeat_interval: Duration::from_millis(500),
        heartbeat_timeout: Duration::from_millis(1000),
        reconnect_delay: Duration::from_millis(100),
        max_reconnect_delay: Duration::from_millis(400),
        task_poll_interval: Duration::from_millis(50),
        perf_interval: Duration::from_millis(150),
        ..BackendOptions::default()
    }
}

/// A unique scratch folder.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "xen-mcp-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A store with `mock` saved (password in a memory keyring, certificate
/// pinned when `pin`).
pub fn store_for(mock: &MockServer, user: &str, password: &str, pin: bool) -> Arc<Store> {
    let endpoint = Endpoint::parse(&mock.address()).unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    secrets
        .set(&xen_core::secrets::account_for(user, &endpoint), password)
        .unwrap();
    let settings = Settings {
        servers: vec![SavedServer {
            address: endpoint.to_string(),
            username: user.to_owned(),
            reconnect: false,
        }],
        ..Settings::default()
    };
    let store = Arc::new(Store::in_memory_with(settings, secrets));
    if pin {
        store.trust().trust(
            &mock.authority(),
            Fingerprint::of_der(&xapi_mock::certificate_der()),
        );
    }
    store
}

/// Short timings for tests.
pub fn config(text: &str) -> McpConfig {
    let mut config = McpConfig::parse(text).unwrap();
    config.connect_wait_seconds = 10;
    config.operation_timeout_seconds = 30;
    config.performance_wait_seconds = 10;
    config
}

impl Client {
    pub async fn start(store: Arc<Store>, config: McpConfig, name: &str) -> Self {
        let audit_dir = scratch(name).join("logs");
        let audit = AuditLog::in_dir(&audit_dir).unwrap();
        let server = McpServer::start(options(store), config, audit).await;
        let (client_end, server_end) = tokio::io::duplex(1 << 22);
        let (server_read, server_write) = tokio::io::split(server_end);
        tokio::spawn(xen_mcp::protocol::serve(
            server_read,
            server_write,
            server.clone(),
        ));
        let (read, writer) = tokio::io::split(client_end);
        let mut client = Self {
            writer,
            lines: BufReader::new(read).lines(),
            next_id: 1,
            received: Vec::new(),
            server,
            audit_dir,
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "xen-mcp-tests", "version": "1"}
                }),
            )
            .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        client.notify("notifications/initialized").await;
        client
    }

    pub async fn send_line(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.write_all(b"\n").await.unwrap();
        self.writer.flush().await.unwrap();
    }

    pub async fn read_message(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("timed out waiting for the server")
            .unwrap()
            .expect("server closed the stream");
        self.received.push(line.clone());
        serde_json::from_str(&line).unwrap()
    }

    pub async fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        self.send_line(&line).await;
    }

    /// Sends a request and returns the whole response message.
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        self.send_line(&line).await;
        let response = self.read_message().await;
        assert_eq!(response["id"], id, "{response}");
        response
    }

    /// Calls a tool; returns `(is_error, structured or text)`.
    pub async fn call(&mut self, tool: &str, arguments: Value) -> (bool, Value) {
        let r = self
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await;
        let result = &r["result"];
        assert!(result.is_object(), "{tool}: {r}");
        let is_error = result["isError"].as_bool().unwrap();
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        if is_error {
            (true, Value::String(text))
        } else {
            let parsed: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(result["structuredContent"], parsed, "{tool}");
            (false, parsed)
        }
    }

    /// A tool that must succeed.
    pub async fn ok(&mut self, tool: &str, arguments: Value) -> Value {
        let (error, value) = self.call(tool, arguments).await;
        assert!(!error, "{tool} failed: {value}");
        value
    }

    /// A tool that must fail; returns its message.
    pub async fn err(&mut self, tool: &str, arguments: Value) -> String {
        let (error, value) = self.call(tool, arguments).await;
        assert!(error, "{tool} should have failed: {value}");
        value.as_str().unwrap().to_owned()
    }

    pub async fn read_resource(&mut self, uri: &str) -> Value {
        let r = self.request("resources/read", json!({"uri": uri})).await;
        let text = r["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{uri}: {r}"));
        serde_json::from_str(text).unwrap()
    }

    /// The audit log's lines.
    pub fn audit_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.audit_dir.join(xen_mcp::audit::AUDIT_FILE))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    pub fn audit_text(&self) -> String {
        std::fs::read_to_string(self.audit_dir.join(xen_mcp::audit::AUDIT_FILE)).unwrap_or_default()
    }

    /// Waits until the server is connected.
    pub async fn wait_connected(&mut self) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let servers = self.ok("list_servers", json!({})).await;
            if servers["servers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["status"] == "connected")
            {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{servers}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// The calls made after `before` calls, without the background ones.
pub fn new_calls(mock: &MockServer, before: usize) -> Vec<String> {
    mock.calls()
        .into_iter()
        .skip(before)
        .filter(|c| !BACKGROUND.contains(&c.as_str()))
        .collect()
}

pub async fn wait_until(what: &str, pred: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !pred() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
