//! The Model Context Protocol over a byte stream: JSON-RPC 2.0 messages,
//! one per line (the stdio transport). Requests are handled concurrently
//! (a write tool waiting for its operation does not hold up a list), and
//! responses are written whole, one per line, as they are ready.
//!
//! Handled: `initialize`, `ping`, `tools/list`, `tools/call`,
//! `resources/list`, `resources/templates/list`, `resources/read`;
//! notifications are accepted and ignored. Batches (allowed by the
//! 2025-03-26 revision only) are refused.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::server::{Args, Call, McpServer, ToolError};

/// Revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// MCP's "resource not found".
const RESOURCE_NOT_FOUND: i64 = -32002;
/// JSON-RPC internal error: the server could not answer (not connected…).
const INTERNAL_ERROR: i64 = -32603;

const INSTRUCTIONS: &str = "Manages XCP-ng / XenServer pools through the xenadmin-rs console's saved servers. \
Read tools answer from a live cache and change nothing. Objects are named by UUID or exact name. \
Write tools (VM start, shutdown, reboot, suspend, resume and snapshots) exist only when enabled in mcp.toml, \
touch only the VMs in the server's allow-list, and need confirm: true after the user agreed.";

/// A JSON-RPC error.
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The session's negotiated revision.
#[derive(Debug, Default)]
struct Session {
    version: Mutex<Option<String>>,
}

impl Session {
    /// Whether results may carry `structuredContent` (2025-06-18 on).
    fn structured(&self) -> bool {
        self.version
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_deref()
            .is_none_or(|v| v >= "2025-06-18")
    }
}

/// Serves MCP on `reader`/`writer` until the reader ends; then waits for
/// the requests still running.
pub async fn serve<R, W>(reader: R, writer: W, server: Arc<McpServer>) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(line) = rx.recv().await {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    });
    let session = Arc::new(Session::default());
    let mut lines = BufReader::new(reader).lines();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(error_response(
                    &Value::Null,
                    &RpcError::new(PARSE_ERROR, format!("parse error: {e}")),
                ));
                continue;
            }
        };
        let Some(obj) = message.as_object() else {
            let _ = tx.send(error_response(
                &Value::Null,
                &RpcError::new(
                    INVALID_REQUEST,
                    "batches and non-object messages are not supported",
                ),
            ));
            continue;
        };
        let Some(method) = obj.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // A response to a request of ours: we send none.
            continue;
        };
        let Some(id) = obj.get("id").cloned() else {
            tracing::debug!(%method, "notification");
            continue;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let (server, session, tx) = (server.clone(), session.clone(), tx.clone());
        tasks.spawn(async move {
            let line = match handle(&server, &session, &method, params).await {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                Err(e) => error_response(&id, &e),
            };
            let _ = tx.send(line);
        });
        while tasks.try_join_next().is_some() {}
    }
    while tasks.join_next().await.is_some() {}
    drop(tx);
    writer_task.await.map_err(std::io::Error::other)?
}

fn error_response(id: &Value, e: &RpcError) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}}).to_string()
}

async fn handle(
    server: &McpServer,
    session: &Session,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    let empty = Map::new();
    let params_obj = match &params {
        Value::Object(m) => m,
        Value::Null => &empty,
        _ => return Err(RpcError::new(INVALID_PARAMS, "params must be an object")),
    };
    match method {
        "initialize" => {
            let asked = params_obj.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .and_then(|a| PROTOCOL_VERSIONS.iter().find(|v| **v == a))
                .copied()
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            *session
                .version
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(version.to_owned());
            tracing::info!(protocol = version, client = ?params_obj.get("clientInfo").and_then(|c| c.get("name")), "mcp session");
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": {"listChanged": false},
                    "resources": {"listChanged": false, "subscribe": false},
                },
                "serverInfo": {
                    "name": "xenadmin-rs",
                    "title": "XenAdmin (Rust) MCP server",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": crate::tools::definitions(server.writes_enabled())})),
        "tools/call" => {
            let name = params_obj
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call needs a tool name"))?;
            let arguments = match params_obj.get("arguments") {
                None | Some(Value::Null) => Value::Object(Map::new()),
                Some(v @ Value::Object(_)) => v.clone(),
                Some(_) => {
                    return Err(RpcError::new(INVALID_PARAMS, "arguments must be an object"));
                }
            };
            call_tool(server, session, name, &arguments).await
        }
        "resources/list" => Ok(crate::resources::list(server)),
        "resources/templates/list" => Ok(crate::resources::templates()),
        "resources/read" => {
            let uri = params_obj
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "resources/read needs a uri"))?;
            let started = Instant::now();
            let mut call = Call::default();
            let result = crate::resources::read(server, &mut call, uri).await;
            server.audit(
                "resources/read",
                &json!({"uri": uri}),
                call,
                &result,
                started,
            );
            match result {
                Ok(value) => Ok(json!({"contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": pretty(&value),
                }]})),
                Err(e @ ToolError::Invalid(_)) => {
                    Err(RpcError::new(RESOURCE_NOT_FOUND, e.message()))
                }
                Err(e) => Err(RpcError::new(INTERNAL_ERROR, e.message())),
            }
        }
        other => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

async fn call_tool(
    server: &McpServer,
    session: &Session,
    name: &str,
    arguments: &Value,
) -> Result<Value, RpcError> {
    let started = Instant::now();
    let mut call = Call::default();
    let args = Args(arguments.as_object().expect("arguments are an object"));
    let result = match crate::tools::call_read(server, &mut call, name, &args).await {
        Some(r) => Some(r),
        None => crate::writes::call_write(server, &mut call, name, &args).await,
    };
    let Some(result) = result else {
        let unknown: Result<Value, ToolError> =
            Err(ToolError::Invalid(format!("unknown tool {name}")));
        server.audit(name, arguments, call, &unknown, started);
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("unknown tool: {name}"),
        ));
    };
    server.audit(name, arguments, call, &result, started);
    Ok(match result {
        Ok(value) => {
            let mut out = json!({
                "content": [{"type": "text", "text": pretty(&value)}],
                "isError": false,
            });
            if session.structured() && value.is_object() {
                out["structuredContent"] = value;
            }
            out
        }
        Err(e) => json!({
            "content": [{"type": "text", "text": e.message()}],
            "isError": true,
        }),
    })
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
