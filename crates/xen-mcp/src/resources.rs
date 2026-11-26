//! The resources: the same views as the read tools, addressed by URI.
//!
//! * `rexencenter://servers`: the configured servers (as `list_servers`);
//! * `rexencenter://server/{server}/{list}`, where `{list}` is `pool`,
//!   `hosts`, `vms`, `srs`, `networks` or `alerts`;
//! * `rexencenter://server/{server}/{kind}/{uuid}`, `{kind}` one of `pool`,
//!   `host`, `vm`, `sr`, `network`: one object's details;
//! * `rexencenter://server/{server}/{host|vm}/{uuid}/performance`: the last
//!   hour of the default graphs' data, a point a minute.
//!
//! `{server}` is the saved address, percent-encoded (`10.0.0.1`,
//! `http%3A%2F%2F127.0.0.1%3A8080`).

use serde_json::{Map, Value, json};

use crate::server::{Args, Call, McpServer, ToolError, ToolResult};

pub const LISTS: &[(&str, &str, &str)] = &[
    (
        "pool",
        "get_pool",
        "The pool: coordinator, hosts, VM counts, HA",
    ),
    ("hosts", "list_hosts", "Hosts of the pool"),
    ("vms", "list_vms", "Virtual machines"),
    ("srs", "list_srs", "Storage repositories"),
    ("networks", "list_networks", "Networks"),
    ("alerts", "list_alerts", "Alerts, newest first"),
];

/// Percent-encodes everything but unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` sequences.
pub fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `resources/list`: the server list, and each configured server's lists.
pub fn list(server: &McpServer) -> Value {
    let mut resources = vec![json!({
        "uri": "rexencenter://servers",
        "name": "servers",
        "title": "Servers",
        "description": "The saved servers this MCP server uses and their status",
        "mimeType": "application/json",
    })];
    for slot in &server.slots {
        for (list, _, description) in LISTS {
            resources.push(json!({
                "uri": format!("rexencenter://server/{}/{list}", encode(&slot.address)),
                "name": format!("{} {list}", slot.address),
                "title": format!("{description} ({})", slot.address),
                "mimeType": "application/json",
            }));
        }
    }
    json!({"resources": resources})
}

/// `resources/templates/list`.
pub fn templates() -> Value {
    json!({"resourceTemplates": [
        {
            "uriTemplate": "rexencenter://server/{server}/{kind}/{uuid}",
            "name": "object",
            "title": "One object's details",
            "description": "kind: pool, host, vm, sr or network; server: the saved address, percent-encoded",
            "mimeType": "application/json",
        },
        {
            "uriTemplate": "rexencenter://server/{server}/{kind}/{uuid}/performance",
            "name": "performance",
            "title": "Performance of a host or VM",
            "description": "kind: host or vm. The last hour of the default graphs' data, one point a minute",
            "mimeType": "application/json",
        },
    ]})
}

/// `resources/read` of `uri`.
pub async fn read(server: &McpServer, call: &mut Call, uri: &str) -> ToolResult {
    let invalid = || ToolError::Invalid(format!("unknown resource {uri}"));
    if uri == "rexencenter://servers" {
        let empty = Map::new();
        return crate::tools::call_read(server, call, "list_servers", &Args(&empty))
            .await
            .unwrap_or_else(|| Err(invalid()));
    }
    let rest = uri
        .strip_prefix("rexencenter://server/")
        .ok_or_else(invalid)?;
    let parts: Vec<&str> = rest.split('/').collect();
    let address = decode(parts.first().copied().unwrap_or_default()).ok_or_else(invalid)?;
    let mut args = Map::new();
    args.insert("server".into(), json!(address));
    match parts.as_slice() {
        [_, list] => {
            let (_, tool, _) = LISTS
                .iter()
                .find(|(l, _, _)| l == list)
                .ok_or_else(invalid)?;
            crate::tools::call_read(server, call, tool, &Args(&args))
                .await
                .unwrap_or_else(|| Err(invalid()))
        }
        [_, kind, uuid] => {
            let (_, conn) = server.connection(call, Some(&address)).await?;
            let uuid = decode(uuid).ok_or_else(invalid)?;
            let detail = crate::tools::detail(&conn, kind, &uuid)?;
            call.target =
                Some(json!({"kind": kind, "uuid": detail["uuid"], "name": detail["name"]}));
            Ok(detail)
        }
        [_, kind @ ("host" | "vm"), uuid, "performance"] => {
            let uuid = decode(uuid).ok_or_else(invalid)?;
            args.insert((*kind).into(), json!(uuid));
            args.insert("interval".into(), json!("1m"));
            args.insert("points".into(), json!(60));
            crate::perf::get_performance(server, call, &Args(&args)).await
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_round_trip() {
        for s in ["10.0.0.1", "http://127.0.0.1:8080", "host:8443", "ü"] {
            assert_eq!(decode(&encode(s)).as_deref(), Some(s));
        }
        assert_eq!(encode("http://a:1"), "http%3A%2F%2Fa%3A1");
        assert_eq!(decode("%zz"), None);
    }
}
