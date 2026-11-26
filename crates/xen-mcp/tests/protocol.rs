//! The MCP protocol against the mock: the handshake, the tool and resource
//! lists, every read tool and resource, and certificate pinning.

mod common;

use common::{Client, config, new_calls, store_for};
use serde_json::{Value, json};
use xapi_mock::MockServer;

/// The address as the server list saves it.
fn saved_address(mock: &MockServer) -> String {
    xapi::Endpoint::parse(&mock.address()).unwrap().to_string()
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|t| t[key].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_and_the_lists() {
    let mock = MockServer::start(true).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, true);
    let mut c = Client::start(store, config(""), "handshake").await;

    // The handshake: capabilities, server info, instructions.
    let init = c
        .request(
            "initialize",
            json!({"protocolVersion": "1999-01-01", "capabilities": {}, "clientInfo": {"name": "x", "version": "1"}}),
        )
        .await;
    let result = &init["result"];
    assert_eq!(
        result["protocolVersion"], "2025-11-25",
        "unknown revision: our newest"
    );
    assert_eq!(result["serverInfo"]["name"], "rexen-center");
    assert_eq!(result["serverInfo"]["title"], "ReXen Center MCP server");
    assert_eq!(
        result["serverInfo"]["description"],
        "ReXen Center — XCP-ng Center, born again in Rust"
    );
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("the ReXen Center console's saved servers")
    );
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("confirm: true")
    );
    let old = c
        .request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
        )
        .await;
    assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
    // 2024-11-05 has no structured results.
    let r = c
        .request(
            "tools/call",
            json!({"name": "list_servers", "arguments": {}}),
        )
        .await;
    assert!(r["result"].get("structuredContent").is_none(), "{r}");
    c.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    )
    .await;

    assert_eq!(c.request("ping", json!({})).await["result"], json!({}));

    // Read only by default: no write tool is listed.
    let tools = c.request("tools/list", json!({})).await;
    let tools = &tools["result"]["tools"];
    let listed = names(tools, "name");
    assert_eq!(listed, xen_mcp::tools::READ_TOOLS);
    for t in tools.as_array().unwrap() {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{t}");
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
        assert!(t["inputSchema"]["properties"]["server"].is_object());
    }

    // Resources: the server list and six lists per server; two templates.
    let resources = c.request("resources/list", json!({})).await;
    let uris = names(&resources["result"]["resources"], "uri");
    let server = xen_mcp::resources::encode(&saved_address(&mock));
    assert_eq!(uris.len(), 7, "{uris:?}");
    assert_eq!(uris[0], "rexencenter://servers");
    for list in ["pool", "hosts", "vms", "srs", "networks", "alerts"] {
        assert!(
            uris.contains(&format!("rexencenter://server/{server}/{list}")),
            "{list}"
        );
    }
    let templates = c.request("resources/templates/list", json!({})).await;
    assert_eq!(
        templates["result"]["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Protocol errors.
    let e = c.request("no/such", json!({})).await;
    assert_eq!(e["error"]["code"], -32601);
    let e = c
        .request(
            "tools/call",
            json!({"name": "no_such_tool", "arguments": {}}),
        )
        .await;
    assert_eq!(e["error"]["code"], -32602);
    let e = c
        .request("resources/read", json!({"uri": "rexencenter://nothing"}))
        .await;
    assert_eq!(e["error"]["code"], -32002);
    c.send_line("{not json").await;
    let e = c.read_message().await;
    assert_eq!(e["error"]["code"], -32700);
    c.send_line("[1, 2]").await;
    let e = c.read_message().await;
    assert_eq!(e["error"]["code"], -32600);
    // A bad argument is a tool error the model can read.
    let msg = c.err("list_vms", json!({"limit": "ten"})).await;
    assert!(msg.contains("limit"), "{msg}");
    let msg = c.err("list_vms", json!({"colour": "red"})).await;
    assert!(msg.contains("unknown argument 'colour'"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_listed_only_when_enabled() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let mut c = Client::start(store, config("[writes]\nenabled = true\n"), "listed").await;
    let tools = c.request("tools/list", json!({})).await;
    let tools = &tools["result"]["tools"];
    let listed = names(tools, "name");
    let mut expected: Vec<&str> = xen_mcp::tools::READ_TOOLS.to_vec();
    expected.extend(xen_mcp::tools::WRITE_TOOLS);
    assert_eq!(listed, expected);
    for t in tools
        .as_array()
        .unwrap()
        .iter()
        .skip(xen_mcp::tools::READ_TOOLS.len())
    {
        assert_eq!(t["annotations"]["destructiveHint"], true, "{t}");
        let required = t["inputSchema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "confirm"), "{t}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_tools_answer_from_the_cache() {
    let mock = MockServer::start(true).await;
    mock.create_message(
        "ALARM",
        3,
        "VM",
        "0b0b0b0b-0000-4000-8000-000000000001",
        "value: 0.950000\nconfig:\n<variable>\n\t<name value=\"cpu_usage\"/>\n\t<alarm_trigger_level value=\"0.9\"/>\n\t<alarm_trigger_period value=\"60\"/>\n</variable>\n",
    );
    mock.create_message(
        "HA_STATEFILE_LOST",
        1,
        "Pool",
        "5a0e3b3c-0000-4000-8000-000000000001",
        "The pool lost its HA state file",
    );
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, true);
    let mut c = Client::start(store, config(""), "reads").await;
    c.wait_connected().await;
    let before = mock.calls().len();

    let servers = c.ok("list_servers", json!({})).await;
    let s = &servers["servers"][0];
    assert_eq!(s["status"], "connected");
    assert_eq!(s["pool"]["name"], "Lab Pool");
    assert_eq!(s["api_version"], "2.21");
    assert_eq!(s["writes"]["enabled"], false);
    assert_eq!(servers["writes_enabled"], false);

    let pool = c.ok("get_pool", json!({})).await;
    assert_eq!(pool["name"], "Lab Pool");
    assert_eq!(pool["hosts"], 2);
    assert_eq!(pool["coordinator"]["name"], "xcp-host-01");
    assert_eq!(pool["host_list"].as_array().unwrap().len(), 2);
    // The server can be named by pool name or address.
    let by_pool = c.ok("get_pool", json!({"server": "Lab Pool"})).await;
    assert_eq!(by_pool["uuid"], pool["uuid"]);
    let msg = c.err("get_pool", json!({"server": "10.9.9.9"})).await;
    assert!(msg.contains("not a configured server"), "{msg}");

    let hosts = c.ok("list_hosts", json!({})).await;
    assert_eq!(hosts["count"], 2);
    assert_eq!(hosts["hosts"][0]["name"], "xcp-host-01");
    assert_eq!(hosts["hosts"][0]["coordinator"], true);
    let filtered = c.ok("list_hosts", json!({"name": "HOST-02"})).await;
    assert_eq!(filtered["count"], 1);
    let host = c.ok("get_host", json!({"host": "xcp-host-01"})).await;
    assert_eq!(host["uuid"], "1c0e3b3c-0000-4000-8000-000000000001");
    assert!(
        host["nics"].as_array().is_some_and(|n| !n.is_empty()),
        "{host}"
    );
    assert!(
        host["vms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "web-01"),
        "{host}"
    );
    let by_uuid = c
        .ok(
            "get_host",
            json!({"host": "1C0E3B3C-0000-4000-8000-000000000002"}),
        )
        .await;
    assert_eq!(by_uuid["name"], "xcp-host-02");

    let vms = c.ok("list_vms", json!({})).await;
    let vm_names = names(&vms["vms"], "name");
    assert_eq!(vm_names, ["build-runner", "db-01", "legacy-app", "web-01"]);
    let running = c.ok("list_vms", json!({"power_state": "running"})).await;
    assert_eq!(names(&running["vms"], "name"), ["web-01"]);
    let all = c
        .ok("list_vms", json!({"include_snapshots": true, "include_templates": true, "include_control_domains": true}))
        .await;
    assert!(all["count"].as_u64().unwrap() >= 9, "{all}");
    let limited = c.ok("list_vms", json!({"limit": 2})).await;
    assert_eq!(limited["count"], 2);
    assert_eq!(limited["truncated_from"], 4);
    let vm = c.ok("get_vm", json!({"vm": "web-01"})).await;
    assert_eq!(vm["power_state"], "Running");
    assert_eq!(vm["kind"], "vm");
    assert_eq!(vm["snapshots"][0]["name"], "web-01 before upgrade");
    assert!(
        vm["disks"].as_array().is_some_and(|d| !d.is_empty()),
        "{vm}"
    );
    assert!(
        vm["allowed_operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "clean_shutdown")
    );
    let snap = c.ok("get_vm", json!({"vm": "web-01 before upgrade"})).await;
    assert_eq!(snap["kind"], "snapshot");
    assert_eq!(snap["snapshot_of"]["name"], "web-01");
    let msg = c.err("get_vm", json!({"vm": "nope"})).await;
    assert!(msg.contains("no VM has the UUID or name 'nope'"), "{msg}");

    let srs = c.ok("list_srs", json!({})).await;
    assert!(srs["count"].as_u64().unwrap() >= 3, "{srs}");
    let nfs = c.ok("list_srs", json!({"type": "NFS"})).await;
    assert_eq!(nfs["count"], 1);
    assert_eq!(nfs["srs"][0]["name"], "NFS VM storage");
    let msg = c.err("get_sr", json!({"sr": "Local storage"})).await;
    assert!(msg.contains("several SRs are named"), "{msg}");
    let sr = c.ok("get_sr", json!({"sr": "NFS VM storage"})).await;
    assert_eq!(sr["shared"], true);
    assert!(sr.get("device_config").is_none());
    assert!(!sr.to_string().contains("device_config"));

    let nets = c.ok("list_networks", json!({})).await;
    assert!(
        names(&nets["networks"], "name").contains(&"Guests VLAN 20".to_owned()),
        "{nets}"
    );
    let net = c
        .ok("get_network", json!({"network": "Guests VLAN 20"}))
        .await;
    assert_eq!(net["vlan"], 20);

    let alerts = c.ok("list_alerts", json!({})).await;
    assert_eq!(alerts["count"], 2, "{alerts}");
    let critical = c.ok("list_alerts", json!({"min_priority": 1})).await;
    assert_eq!(critical["count"], 1);
    assert_eq!(critical["alerts"][0]["name"], "HA_STATEFILE_LOST");
    assert_eq!(critical["alerts"][0]["object"]["kind"], "pool");
    let about = c.ok("list_alerts", json!({"object": "web-01"})).await;
    assert_eq!(about["count"], 1);
    assert_eq!(about["alerts"][0]["alarm"]["variable"], "cpu_usage");
    assert_eq!(about["alerts"][0]["object"]["kind"], "vm");

    // Performance from the RRDs (the mock serves them).
    let perf = c
        .ok(
            "get_performance",
            json!({"host": "xcp-host-01", "interval": "5s", "points": 3}),
        )
        .await;
    assert_eq!(perf["object"]["name"], "xcp-host-01");
    let series = perf["series"].as_array().unwrap();
    assert!(!series.is_empty(), "{perf}");
    assert!(
        series
            .iter()
            .all(|s| s["points"].as_array().unwrap().len() <= 3)
    );
    let cpu = series.iter().find(|s| s["name"] == "cpu0").expect("cpu0");
    assert_eq!(cpu["unit"], "percent");
    let perf = c
        .ok(
            "get_performance",
            json!({"vm": "web-01", "sources": ["memory", "nonsense"]}),
        )
        .await;
    assert_eq!(perf["series"][0]["name"], "memory");
    assert_eq!(perf["series"][0]["unit"], "bytes");
    assert_eq!(perf["unknown_sources"], json!(["nonsense"]));
    let available = c
        .ok("get_performance", json!({"vm": "web-01", "sources": []}))
        .await;
    assert!(
        available["available_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "memory")
    );
    let msg = c
        .err("get_performance", json!({"host": "a", "vm": "b"}))
        .await;
    assert!(msg.contains("either"), "{msg}");

    // Read tools read the cache: nothing was asked but the performance
    // tool's data sources and RRDs.
    let calls = new_calls(&mock, before);
    assert!(
        calls.iter().all(
            |c| c.ends_with(".get_data_sources") || c.starts_with("http/") && c.contains("rrd")
        ),
        "unexpected calls: {calls:?}"
    );
    assert!(calls.iter().any(|c| c == "http/host_rrds"), "{calls:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resources_read_the_same_views() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let mut c = Client::start(store, config(""), "resources").await;
    c.wait_connected().await;
    let server = xen_mcp::resources::encode(&saved_address(&mock));

    let servers = c.read_resource("rexencenter://servers").await;
    assert_eq!(servers["servers"][0]["status"], "connected");
    let vms = c
        .read_resource(&format!("rexencenter://server/{server}/vms"))
        .await;
    assert_eq!(vms, c.ok("list_vms", json!({})).await);
    for (list, key) in [
        ("hosts", "hosts"),
        ("srs", "srs"),
        ("networks", "networks"),
        ("alerts", "alerts"),
    ] {
        let v = c
            .read_resource(&format!("rexencenter://server/{server}/{list}"))
            .await;
        assert!(v[key].is_array(), "{list}: {v}");
    }
    let pool = c
        .read_resource(&format!("rexencenter://server/{server}/pool"))
        .await;
    assert_eq!(pool["name"], "Lab Pool");
    let vm = c
        .read_resource(&format!(
            "rexencenter://server/{server}/vm/0b0b0b0b-0000-4000-8000-000000000001"
        ))
        .await;
    assert_eq!(vm["name"], "web-01");
    let host = c
        .read_resource(&format!(
            "rexencenter://server/{server}/host/1c0e3b3c-0000-4000-8000-000000000001"
        ))
        .await;
    assert_eq!(host["name"], "xcp-host-01");
    let p = c
        .read_resource(&format!(
            "rexencenter://server/{server}/pool/5a0e3b3c-0000-4000-8000-000000000001"
        ))
        .await;
    assert_eq!(p["name"], "Lab Pool");
    let perf = c
        .read_resource(&format!(
            "rexencenter://server/{server}/vm/0b0b0b0b-0000-4000-8000-000000000001/performance"
        ))
        .await;
    assert_eq!(perf["interval"], "1m");
    assert_eq!(perf["object"]["name"], "web-01");
    // Each read is audited.
    let audited: Vec<Value> = c
        .audit_lines()
        .into_iter()
        .filter(|l| l["tool"] == "resources/read")
        .collect();
    assert_eq!(audited.len(), 11);
    assert!(audited.iter().all(|l| l["outcome"] == "ok"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpinned_certificate_is_refused_before_login() {
    let mock = MockServer::start(true).await;
    let store = store_for(&mock, xapi_mock::USERNAME, xapi_mock::PASSWORD, false);
    let mut c = Client::start(store.clone(), config(""), "tofu").await;
    let msg = c.err("list_vms", json!({})).await;
    assert!(msg.contains("not connected"), "{msg}");
    assert!(msg.contains("not trusted"), "{msg}");
    let servers = c.ok("list_servers", json!({})).await;
    assert!(
        servers["servers"][0]["problem"]
            .as_str()
            .unwrap()
            .contains("ReXen Center window"),
        "{servers}"
    );
    assert!(
        !mock
            .calls()
            .iter()
            .any(|m| m == "session.login_with_password"),
        "no credentials sent"
    );
    assert!(
        store.trust().get(&mock.authority()).is_none(),
        "nothing pinned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_a_saved_password_says_so() {
    let mock = MockServer::start(false).await;
    let store = store_for(&mock, "someone-else", "x", false);
    // The password is saved for another account name.
    let endpoint = xapi::Endpoint::parse(&mock.address()).unwrap();
    store.delete_password("someone-else", &endpoint).unwrap();
    let mut c = Client::start(store, config(""), "nopw").await;
    let msg = c.err("list_hosts", json!({})).await;
    assert!(msg.contains("no saved password"), "{msg}");
}
