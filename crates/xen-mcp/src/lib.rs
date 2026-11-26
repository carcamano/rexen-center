//! A Model Context Protocol (MCP) server for the management console,
//! started with `rexencenter --mcp`: an MCP client (an AI assistant) can read
//! the pools, hosts, VMs, storage, networks, alerts and performance data of
//! the saved servers and, when the configuration allows it, start, stop and
//! snapshot the VMs it names.
//!
//! * It reuses the console's saved servers, pinned certificates and saved
//!   passwords ([`xen_core::Store`], opened read only: it never changes the
//!   server list, the pins or the keyring) and its backend: each server is
//!   a connection that keeps a live cache through `event.from`, so read
//!   tools answer from the cache ([`tools`]).
//! * Write tools are off by default; when on ([`config`]), they touch only
//!   the VMs of a per-server allow-list, need `confirm: true`, and go
//!   through the window's operations, rules and RBAC check ([`writes`]).
//! * Every tool call and resource read goes to an audit log ([`audit`]).
//! * Nothing hands out a password, a session reference or the keyring: the
//!   backend holds them and no view includes them ([`views`]).
//! * The transport is stdio ([`protocol`]): one JSON-RPC message per line.
//!   An HTTP transport (MCP "Streamable HTTP") would be a second front end
//!   on the same [`McpServer`]: a local `hyper` server (already a workspace
//!   dependency) taking `POST /mcp` bodies to the same request handler,
//!   with a bearer token and the `Origin` check the specification asks for.
//!   It is not built in this phase.

pub mod audit;
pub mod config;
pub mod perf;
pub mod protocol;
pub mod resources;
pub mod server;
pub mod tools;
pub mod views;
pub mod writes;

use std::sync::Arc;

use xen_core::secrets::{SecretError, SecretStore};
use xen_core::{BackendOptions, Store};

pub use audit::AuditLog;
pub use config::McpConfig;
pub use server::McpServer;

/// A secret store that reads through `inner` and refuses to change it, so
/// the MCP server never writes to (or deletes from) the keyring.
pub struct ReadOnlySecrets<S>(pub S);

impl<S: SecretStore> SecretStore for ReadOnlySecrets<S> {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        self.0.get(account)
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<(), SecretError> {
        Err(SecretError(
            "the MCP server does not change saved passwords".into(),
        ))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Err(SecretError(
            "the MCP server does not change saved passwords".into(),
        ))
    }
}

/// Runs the MCP server on standard input and output until the client
/// closes standard input or `stop` resolves (a termination signal), then
/// logs out of every server.
pub async fn run_stdio(
    store: Arc<Store>,
    originator: String,
    config: McpConfig,
    audit: AuditLog,
    stop: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    let options = BackendOptions {
        originator,
        store,
        ..BackendOptions::default()
    };
    let server = McpServer::start(options, config, audit).await;
    let result = tokio::select! {
        r = protocol::serve(tokio::io::stdin(), tokio::io::stdout(), server.clone()) => r,
        () = stop => {
            tracing::info!("termination signal; stopping the MCP server");
            Ok(())
        }
    };
    server.shutdown().await;
    result
}
