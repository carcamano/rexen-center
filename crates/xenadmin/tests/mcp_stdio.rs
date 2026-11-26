//! `rexencenter --mcp` over real standard input and output: stdout carries
//! only protocol lines, the configuration folder is only read, the session
//! ends when stdin closes, and a broken `mcp.toml` stops the start.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rexencenter-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn mcp(dir: &std::path::Path, extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rexencenter"));
    cmd.arg("--mcp")
        .args(extra)
        .env("REXENCENTER_CONFIG_DIR", dir)
        .env_remove("XENADMIN_CONFIG_DIR")
        // A service name nobody uses: nothing is saved under it.
        .env(
            "REXENCENTER_KEYRING_SERVICE",
            format!("rexen-center-test-mcp-{}", std::process::id()),
        )
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

#[test]
fn serves_mcp_on_stdio_and_only_reads_the_configuration() {
    let dir = scratch("stdio");
    // A saved server that will not answer, and no saved password.
    let settings =
        "[[server]]\naddress = \"http://127.0.0.1:9\"\nusername = \"root\"\nreconnect = true\n";
    std::fs::write(dir.join("settings.toml"), settings).unwrap();
    std::fs::write(dir.join("mcp.toml"), "[writes]\nenabled = false\n").unwrap();

    let mut child = mcp(&dir, &[]).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |id: i64, method: &str, params: Value| -> Value {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut answer = String::new();
        stdout.read_line(&mut answer).unwrap();
        let answer: Value = serde_json::from_str(&answer).expect("stdout carries JSON-RPC only");
        assert_eq!(answer["id"], id);
        answer
    };
    let init = ask(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "rexen-center");
    assert_eq!(
        init["result"]["serverInfo"]["title"],
        "ReXen Center MCP server"
    );
    let tools = ask(2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"list_vms"));
    assert!(
        !names.iter().any(|n| n.starts_with("vm_")),
        "read only: {names:?}"
    );
    let servers = ask(
        3,
        "tools/call",
        json!({"name": "list_servers", "arguments": {}}),
    );
    let server = &servers["result"]["structuredContent"]["servers"][0];
    assert_eq!(server["address"], "http://127.0.0.1:9");
    let refused = ask(
        4,
        "tools/call",
        json!({"name": "vm_start", "arguments": {"vm": "x", "confirm": true}}),
    );
    assert_eq!(refused["result"]["isError"], true);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("MCP server starting"), "{stderr}");
    assert!(
        !stderr.contains('\u{1b}'),
        "no colour codes when stderr is not a terminal"
    );

    // Read only: the saved list is as it was, nothing was pinned.
    assert_eq!(
        std::fs::read_to_string(dir.join("settings.toml")).unwrap(),
        settings
    );
    assert!(!dir.join("known_servers.toml").exists());
    let audit = std::fs::read_to_string(dir.join("logs").join("mcp-audit.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 2, "{audit}");
    assert!(audit.contains("\"outcome\":\"refused\""));
    // The MCP server's own log is named after the product too.
    let logs: Vec<String> = std::fs::read_dir(dir.join("logs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        logs.iter().any(|f| f.starts_with("rexen-center-mcp.")),
        "{logs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_configuration_stops_the_start() {
    let dir = scratch("broken");
    let bad = dir.join("custom.toml");
    std::fs::write(&bad, "[writes]\nenable = true\n").unwrap();
    let out = mcp(&dir, &["--mcp-config", bad.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("enable"));
    assert!(out.stdout.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
