//! The audit log records every tool call and resource read, and no
//! password or session reference reaches the audit log, a tool result, a
//! resource, an error or the trace log (captured here at TRACE level with
//! every dependency's logging on).

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{Client, config, store_for};
use serde_json::{Value, json};
use xapi_mock::MockServer;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_call_is_audited_and_no_secret_leaks() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .init();

    let mock = MockServer::start(true).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, true);
    let address = xapi::Endpoint::parse(&mock.address()).unwrap().to_string();
    let text =
        format!("[writes]\nenabled = true\n[writes.allow]\n\"{address}\" = [\"build-runner\"]\n");
    let mut c = Client::start(store, config(&text), "audit").await;
    c.wait_connected().await;
    let waited = c.audit_lines().len();

    let calls: Vec<(&str, Value)> = vec![
        ("list_servers", json!({})),
        ("get_pool", json!({})),
        ("list_hosts", json!({})),
        ("get_host", json!({"host": "xcp-host-01"})),
        ("list_vms", json!({})),
        ("get_vm", json!({"vm": "web-01"})),
        ("list_srs", json!({})),
        ("get_sr", json!({"sr": "NFS VM storage"})),
        ("list_networks", json!({})),
        ("get_network", json!({"network": "Guests VLAN 20"})),
        ("list_alerts", json!({})),
        ("get_performance", json!({"vm": "web-01", "interval": "5s"})),
        ("vm_start", json!({"vm": "build-runner", "confirm": true})),
        (
            "vm_shutdown",
            json!({"vm": "build-runner", "force": true, "confirm": true}),
        ),
        ("vm_start", json!({"vm": "web-01", "confirm": true})),
        ("vm_start", json!({"vm": "build-runner"})),
        // A client passing secrets as arguments: refused, and redacted.
        (
            "list_vms",
            json!({"password": xapi_mock::PASSWORD, "session_id": "OpaqueRef:mock-session-1"}),
        ),
        ("get_vm", json!({"vm": "missing"})),
    ];
    for (tool, args) in &calls {
        c.call(tool, args.clone()).await;
    }
    let uri = format!(
        "rexencenter://server/{}/vms",
        xen_mcp::resources::encode(&address)
    );
    c.read_resource(&uri).await;
    // An unknown tool is a protocol error, and audited.
    let e = c
        .request(
            "tools/call",
            json!({"name": "host_reboot", "arguments": {"host": "xcp-host-01"}}),
        )
        .await;
    assert_eq!(e["error"]["code"], -32602);

    let audit = c.audit_lines();
    let mine = &audit[waited..];
    assert_eq!(mine.len(), calls.len() + 2, "one line per call: {mine:#?}");
    for (line, (tool, _)) in mine.iter().zip(&calls) {
        assert_eq!(line["tool"], *tool, "{line}");
        assert!(line["time"].as_str().unwrap().ends_with('Z'));
        assert!(line["duration_ms"].is_u64());
        assert!(["ok", "refused", "error"].contains(&line["outcome"].as_str().unwrap()));
        assert!(line["arguments"].is_object());
    }
    let outcomes: Vec<&str> = mine
        .iter()
        .map(|l| l["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(
        outcomes,
        [
            "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", // reads
            "ok", "ok", // the allowed VM
            "refused", "refused", // outside the allow-list; no confirmation
            "error", "error", // bad argument; unknown VM
            "ok",    // resource
            "error", // unknown tool
        ]
    );
    assert_eq!(mine[3]["target"]["name"], "xcp-host-01");
    assert_eq!(
        mine[12]["target"]["uuid"],
        "0b0b0b0b-0000-4000-8000-000000000003"
    );
    assert!(mine[12]["detail"].as_str().unwrap().contains("succeeded"));
    assert_eq!(mine[16]["arguments"]["password"], "[redacted]");
    assert_eq!(mine[16]["arguments"]["session_id"], "[redacted]");
    assert_eq!(mine[18]["tool"], "resources/read");
    assert_eq!(mine[19]["tool"], "host_reboot");

    // No secret anywhere.
    c.server.shutdown().await;
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let responses = c.received.join("\n");
    let audit_text = c.audit_text();
    assert!(logs.contains("xapi call"), "the trace log was captured");
    assert!(responses.len() > 10_000, "responses were captured");
    for (what, text) in [
        ("trace log", &logs),
        ("responses", &responses),
        ("audit log", &audit_text),
    ] {
        assert!(
            !text.contains(xapi_mock::PASSWORD),
            "password in the {what}"
        );
        assert!(
            !text.contains("mock-session"),
            "session reference in the {what}"
        );
    }
}
