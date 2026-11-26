//! The write tools against the mock: refused when disabled, without
//! confirmation, outside the allow-list, when not offered, when the roles
//! do not allow them (each time with no call reaching the server); and,
//! for an allowed VM, run through the window's operations.

mod common;

use common::{Client, config, new_calls, store_for};
use serde_json::{Value, json};
use xapi_mock::MockServer;

/// One call of every write tool, aimed at `vm` / `snapshot`.
fn every_write(vm: &str, snapshot: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("vm_start", json!({"vm": vm, "confirm": true})),
        ("vm_shutdown", json!({"vm": vm, "confirm": true})),
        (
            "vm_shutdown",
            json!({"vm": vm, "force": true, "confirm": true}),
        ),
        ("vm_reboot", json!({"vm": vm, "confirm": true})),
        (
            "vm_reboot",
            json!({"vm": vm, "force": true, "confirm": true}),
        ),
        ("vm_suspend", json!({"vm": vm, "confirm": true})),
        ("vm_resume", json!({"vm": vm, "confirm": true})),
        (
            "vm_snapshot",
            json!({"vm": vm, "name": "s", "confirm": true}),
        ),
        (
            "vm_snapshot_revert",
            json!({"snapshot": snapshot, "confirm": true}),
        ),
        (
            "vm_snapshot_delete",
            json!({"snapshot": snapshot, "confirm": true}),
        ),
    ]
}

fn saved_address(mock: &MockServer) -> String {
    xapi::Endpoint::parse(&mock.address()).unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_write_tool_is_refused_when_writes_are_disabled() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    // Even with the VM in the allow-list: the switch comes first.
    let text = format!(
        "[writes.allow]\n\"{}\" = [\"web-01\"]\n",
        saved_address(&mock)
    );
    let mut c = Client::start(store, config(&text), "disabled").await;
    c.wait_connected().await;
    let before = mock.calls().len();
    for (tool, args) in every_write("web-01", "web-01 before upgrade") {
        let msg = c.err(tool, args).await;
        assert!(
            msg.starts_with("refused: write tools are disabled"),
            "{tool}: {msg}"
        );
    }
    assert_eq!(new_calls(&mock, before), Vec::<String>::new());
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-web"), "Running");
    let audit = c.audit_lines();
    let refused: Vec<&Value> = audit.iter().filter(|l| l["outcome"] == "refused").collect();
    assert_eq!(refused.len(), 10);
    assert!(
        refused.iter().all(|l| l.get("server").is_none()),
        "refused before the server was looked at"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_with_writes_enabled_make_no_call() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let text = format!(
        "[writes]\nenabled = true\n[writes.allow]\n\"{}\" = [\"build-runner\", \"db-01\", \"no-such-vm\"]\n",
        saved_address(&mock)
    );
    let mut c = Client::start(store, config(&text), "refusals").await;
    c.wait_connected().await;
    let before = mock.calls().len();

    // No confirmation.
    let msg = c.err("vm_start", json!({"vm": "build-runner"})).await;
    assert!(msg.contains("confirm: true"), "{msg}");
    let msg = c
        .err("vm_start", json!({"vm": "build-runner", "confirm": false}))
        .await;
    assert!(msg.starts_with("refused:"), "{msg}");
    // Outside the allow-list: every tool, on a running VM and its snapshot.
    for (tool, args) in every_write("web-01", "web-01 before upgrade") {
        let msg = c.err(tool, args).await;
        assert!(
            msg.contains(
                "VM 'web-01' (0b0b0b0b-0000-4000-8000-000000000001) is not in the write allow-list"
            ),
            "{tool}: {msg}"
        );
    }
    // By UUID too.
    let msg = c
        .err(
            "vm_shutdown",
            json!({"vm": "0b0b0b0b-0000-4000-8000-000000000004", "force": true, "confirm": true}),
        )
        .await;
    assert!(msg.contains("'legacy-app'"), "{msg}");
    // Templates, snapshots and control domains are not VMs for these tools.
    let msg = c
        .err(
            "vm_start",
            json!({"vm": "Debian Bookworm 12", "confirm": true}),
        )
        .await;
    assert!(msg.contains("no VM has"), "{msg}");
    let msg = c
        .err(
            "vm_snapshot_delete",
            json!({"snapshot": "web-01", "confirm": true}),
        )
        .await;
    assert!(msg.contains("no snapshot has"), "{msg}");
    // Allowed, but not offered now: the window's rules.
    let msg = c
        .err("vm_suspend", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert!(
        msg.contains("suspend is not possible for 'build-runner' now (power state Halted"),
        "{msg}"
    );
    let msg = c
        .err("vm_reboot", json!({"vm": "db-01", "confirm": true}))
        .await;
    assert!(
        msg.contains("clean reboot is not possible for 'db-01'"),
        "{msg}"
    );
    let msg = c
        .err(
            "vm_snapshot",
            json!({"vm": "build-runner", "name": "x", "memory": true, "confirm": true}),
        )
        .await;
    assert!(msg.contains("needs a running VM"), "{msg}");
    // A server that is not configured.
    let msg = c
        .err(
            "vm_start",
            json!({"server": "10.9.9.9", "vm": "build-runner", "confirm": true}),
        )
        .await;
    assert!(msg.contains("not a configured server"), "{msg}");
    // Unknown arguments are not ignored.
    let msg = c
        .err(
            "vm_start",
            json!({"vm": "build-runner", "confirm": true, "force": true}),
        )
        .await;
    assert!(msg.contains("unknown argument 'force'"), "{msg}");

    assert_eq!(
        new_calls(&mock, before),
        Vec::<String>::new(),
        "no call reached the server"
    );
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-web"), "Running");
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-build"), "Halted");
    let audit = c.audit_lines();
    assert!(
        audit
            .iter()
            .filter(|l| l["tool"].as_str().unwrap().starts_with("vm_"))
            .all(|l| l["outcome"] != "ok"),
        "{audit:?}"
    );
    let target = audit
        .iter()
        .find(|l| l["tool"] == "vm_snapshot_revert")
        .unwrap();
    assert_eq!(target["target"]["kind"], "snapshot");
    assert_eq!(target["target"]["vm"]["name"], "web-01");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_an_allow_list_and_a_shared_name_are_refused() {
    let mock = MockServer::start(false).await;
    // A second VM named like an allowed one.
    let mut twin = mock.record("vm", "OpaqueRef:vm-build").unwrap();
    twin["uuid"] = json!("0b0b0b0b-0000-4000-8000-0000000000ff");
    mock.add_object("vm", "OpaqueRef:vm-build-twin", twin);
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let text = format!(
        "[writes]\nenabled = true\n[writes.allow]\n\"{}\" = [\"build-runner\"]\n",
        saved_address(&mock)
    );
    let mut c = Client::start(store.clone(), config(&text), "shared-name").await;
    c.wait_connected().await;
    let before = mock.calls().len();
    let msg = c
        .err(
            "vm_start",
            json!({"vm": "0b0b0b0b-0000-4000-8000-000000000003", "confirm": true}),
        )
        .await;
    assert!(msg.contains("several VMs share it"), "{msg}");
    assert_eq!(new_calls(&mock, before), Vec::<String>::new());

    // An allow-list naming another server only.
    let mock2 = MockServer::start(false).await;
    let store2 = store_for(&mock2, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let mut c2 = Client::start(
        store2,
        config("[writes]\nenabled = true\n[writes.allow]\n\"10.9.9.9\" = [\"build-runner\"]\n"),
        "other-server",
    )
    .await;
    c2.wait_connected().await;
    let before = mock2.calls().len();
    let msg = c2
        .err("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert!(msg.contains("has no write allow-list"), "{msg}");
    assert_eq!(new_calls(&mock2, before), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roles_and_ha_are_checked_before_running() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, "read-only-user", "read-only-password", false);
    let text = format!(
        "[writes]\nenabled = true\n[writes.allow]\n\"{}\" = [\"build-runner\"]\n",
        saved_address(&mock)
    );
    let mut c = Client::start(store, config(&text), "rbac").await;
    c.wait_connected().await;
    let before = mock.calls().len();
    let msg = c
        .err("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert!(
        msg.contains("the user read-only-user is not allowed to do this"),
        "{msg}"
    );
    assert_eq!(new_calls(&mock, before), Vec::<String>::new());
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-build"), "Halted");

    // HA: a protected VM in an HA pool starts from the window only.
    let mock = MockServer::start(false).await;
    mock.set_field("pool", "OpaqueRef:pool-0001", "ha_enabled", json!(true));
    mock.set_field(
        "vm",
        "OpaqueRef:vm-build",
        "ha_restart_priority",
        json!("restart"),
    );
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let text = format!(
        "[writes]\nenabled = true\n[writes.allow]\n\"{}\" = [\"build-runner\"]\n",
        saved_address(&mock)
    );
    let mut c = Client::start(store, config(&text), "ha").await;
    c.wait_connected().await;
    let before = mock.calls().len();
    let msg = c
        .err("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert!(msg.contains("HA protects this VM"), "{msg}");
    assert_eq!(new_calls(&mock, before), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_vm_goes_through_its_lifecycle_and_snapshots() {
    let mock = MockServer::start(true).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, true);
    let text = format!(
        "[writes]\nenabled = true\n[writes.allow]\n\"{}\" = [\"0b0b0b0b-0000-4000-8000-000000000003\"]\n",
        mock.address()
    );
    let mut c = Client::start(store, config(&text), "lifecycle").await;
    c.wait_connected().await;

    let servers = c.ok("list_servers", json!({})).await;
    assert_eq!(
        servers["servers"][0]["writes"]["allow"],
        json!(["0b0b0b0b-0000-4000-8000-000000000003"])
    );

    let before = mock.calls().len();
    let r = c
        .ok("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert_eq!(r["status"], "succeeded");
    assert_eq!(r["vm"]["power_state"], "Running");
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-build"), "Running");
    let calls = new_calls(&mock, before);
    assert!(calls.iter().any(|m| m == "Async.VM.start"), "{calls:?}");
    // Only the allowed VM was touched.
    assert!(
        calls
            .iter()
            .all(|m| !m.starts_with("Async.VM") || m == "Async.VM.start"),
        "{calls:?}"
    );
    assert_eq!(mock.vm_power_state("OpaqueRef:vm-db"), "Halted");

    let r = c
        .ok("vm_reboot", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert_eq!(r["vm"]["power_state"], "Running");
    let r = c
        .ok("vm_suspend", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert_eq!(r["vm"]["power_state"], "Suspended");
    let r = c
        .ok("vm_resume", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert_eq!(r["vm"]["power_state"], "Running");
    let r = c
        .ok(
            "vm_reboot",
            json!({"vm": "build-runner", "force": true, "confirm": true}),
        )
        .await;
    assert_eq!(r["vm"]["power_state"], "Running");
    let r = c
        .ok(
            "vm_shutdown",
            json!({"vm": "build-runner", "confirm": true}),
        )
        .await;
    assert_eq!(r["vm"]["power_state"], "Halted");
    c.ok("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    let r = c
        .ok(
            "vm_shutdown",
            json!({"vm": "build-runner", "force": true, "confirm": true}),
        )
        .await;
    assert_eq!(r["vm"]["power_state"], "Halted");
    for method in [
        "Async.VM.clean_reboot",
        "Async.VM.suspend",
        "Async.VM.resume",
        "Async.VM.hard_reboot",
        "Async.VM.clean_shutdown",
        "Async.VM.hard_shutdown",
    ] {
        assert!(mock.calls().iter().any(|m| m == method), "{method}");
    }

    // Snapshots: take, revert, delete.
    let r = c
        .ok("vm_snapshot", json!({"vm": "build-runner", "name": "mcp-test", "description": "by MCP", "confirm": true}))
        .await;
    let snap = r["snapshot"]["uuid"]
        .as_str()
        .expect("snapshot uuid")
        .to_owned();
    assert_eq!(r["snapshot"]["name"], "mcp-test");
    let vm = c.ok("get_vm", json!({"vm": "build-runner"})).await;
    assert_eq!(vm["snapshots"][0]["uuid"], snap.as_str());
    let r = c
        .ok(
            "vm_snapshot_revert",
            json!({"snapshot": snap, "confirm": true}),
        )
        .await;
    assert_eq!(r["status"], "succeeded");
    let r = c
        .ok(
            "vm_snapshot_delete",
            json!({"snapshot": "mcp-test", "confirm": true}),
        )
        .await;
    assert_eq!(r["deleted"], true);
    let vm = c.ok("get_vm", json!({"vm": "build-runner"})).await;
    assert_eq!(vm["snapshots"], json!([]));
    for method in ["Async.VM.snapshot", "Async.VM.revert", "Async.VM.destroy"] {
        assert!(mock.calls().iter().any(|m| m == method), "{method}");
    }
    // A failure of the server comes back as an error.
    mock.fail_method(
        "VM.start",
        Some(("VM_BAD_POWER_STATE", &["x", "halted", "running"])),
    );
    let msg = c
        .err("vm_start", json!({"vm": "build-runner", "confirm": true}))
        .await;
    assert!(
        msg.contains("the operation failed: VM_BAD_POWER_STATE"),
        "{msg}"
    );

    // The audit log has every call, with its operation's end.
    let audit = c.audit_lines();
    let writes: Vec<&Value> = audit
        .iter()
        .filter(|l| l["tool"].as_str().unwrap().starts_with("vm_"))
        .collect();
    assert_eq!(writes.len(), 12);
    assert!(
        writes[..11]
            .iter()
            .all(|l| l["outcome"] == "ok" && l["detail"].as_str().unwrap().ends_with("succeeded"))
    );
    assert_eq!(writes[11]["outcome"], "error");
    assert!(
        writes
            .iter()
            .all(|l| l["target"]["uuid"].is_string() && l["server"].is_string())
    );
}
