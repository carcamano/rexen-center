//! The tools: their definitions (`tools/list`) and the read tools.
//!
//! Read tools answer from the live cache each connection keeps through
//! `event.from`, so they make no call to the server (the performance tool
//! reads the server's RRDs, as the Performance tab does). Write tools are
//! in [`crate::writes`].

use std::sync::Arc;

use serde_json::{Value, json};
use xen_core::ConnectionView;
use xen_core::alerts;
use xen_core::objects::{self, VmKind};

use crate::server::{Args, Call, McpServer, ToolError, ToolResult, resolve};
use crate::views;

/// One tool's definition.
pub(crate) fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    write: bool,
) -> Value {
    let mut props = json!({
        "server": {
            "type": "string",
            "description": "Saved server address, pool name or pool UUID. Optional when one server is configured."
        }
    });
    if let (Some(all), Some(extra)) = (props.as_object_mut(), properties.as_object()) {
        all.extend(extra.clone());
    }
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        },
        "annotations": {
            "title": title,
            "readOnlyHint": !write,
            "destructiveHint": write,
            "idempotentHint": !write,
            "openWorldHint": false,
        },
    })
}

/// Names of the read tools.
pub const READ_TOOLS: &[&str] = &[
    "list_servers",
    "get_pool",
    "list_hosts",
    "get_host",
    "list_vms",
    "get_vm",
    "list_srs",
    "get_sr",
    "list_networks",
    "get_network",
    "list_alerts",
    "get_performance",
];

/// Names of the write tools.
pub const WRITE_TOOLS: &[&str] = &[
    "vm_start",
    "vm_shutdown",
    "vm_reboot",
    "vm_suspend",
    "vm_resume",
    "vm_snapshot",
    "vm_snapshot_revert",
    "vm_snapshot_delete",
];

/// The tool list: the read tools, and the write tools when writes are on.
pub fn definitions(writes: bool) -> Vec<Value> {
    let name_filter =
        json!({"type": "string", "description": "Only names containing this text (any case)."});
    let mut out = vec![
        tool(
            "list_servers",
            "List servers",
            "The saved servers this MCP server uses: connection status, pool, API version, and whether write tools are enabled with the VMs they may touch.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "get_pool",
            "Pool details",
            "The pool of a server: coordinator, hosts, VM counts, default SR and HA state. Without 'server', every configured server's pool.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "list_hosts",
            "List hosts",
            "The hosts (servers) of the pool: address, enabled, live, maintenance mode, memory, CPUs, running VMs, version.",
            json!({"name": name_filter}),
            &[],
            false,
        ),
        tool(
            "get_host",
            "Host details",
            "One host: CPU, software versions, memory breakdown, NICs, storage and the VMs running on it.",
            json!({"host": {"type": "string", "description": "Host UUID or name."}}),
            &["host"],
            false,
        ),
        tool(
            "list_vms",
            "List VMs",
            "Virtual machines with power state, host, vCPUs, memory, OS and IP addresses. Templates, snapshots and control domains are left out unless asked for.",
            json!({
                "name": name_filter,
                "power_state": {"type": "string", "enum": ["Running", "Halted", "Suspended", "Paused"]},
                "host": {"type": "string", "description": "Only VMs running on this host (UUID or name)."},
                "tag": {"type": "string", "description": "Only VMs with this tag."},
                "include_templates": {"type": "boolean"},
                "include_snapshots": {"type": "boolean"},
                "include_control_domains": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 1, "description": "At most this many (default 500)."}
            }),
            &[],
            false,
        ),
        tool(
            "get_vm",
            "VM details",
            "One VM, template or snapshot: memory, vCPUs, boot mode, disks, network interfaces, snapshots, guest tools, allowed operations.",
            json!({"vm": {"type": "string", "description": "VM UUID or name."}}),
            &["vm"],
            false,
        ),
        tool(
            "list_srs",
            "List storage repositories",
            "Storage repositories: type, shared, size, utilisation, the hosts they are attached to.",
            json!({
                "name": name_filter,
                "type": {"type": "string", "description": "Only SRs of this type (lvm, ext, nfs, iso…)."}
            }),
            &[],
            false,
        ),
        tool(
            "get_sr",
            "Storage repository details",
            "One storage repository with its virtual disks and the VMs using them.",
            json!({"sr": {"type": "string", "description": "SR UUID or name."}}),
            &["sr"],
            false,
        ),
        tool(
            "list_networks",
            "List networks",
            "Networks: bridge, MTU, VLAN, NIC and VM interface counts.",
            json!({"name": name_filter}),
            &[],
            false,
        ),
        tool(
            "get_network",
            "Network details",
            "One network with its host NICs and VM interfaces.",
            json!({"network": {"type": "string", "description": "Network UUID or name."}}),
            &["network"],
            false,
        ),
        tool(
            "list_alerts",
            "List alerts",
            "The pool's alerts (XAPI messages), newest first, with priority, object and text.",
            json!({
                "min_priority": {"type": "integer", "minimum": 1, "maximum": 5, "description": "1 critical … 5 information: only alerts this important or more (default 5, all)."},
                "object": {"type": "string", "description": "Only alerts about this object (name or UUID)."},
                "limit": {"type": "integer", "minimum": 1, "description": "At most this many (default 100)."}
            }),
            &[],
            false,
        ),
        tool(
            "get_performance",
            "Performance data",
            "Recent performance data of a host or VM from the server's RRDs (CPU, memory, network, disk), as the Performance tab shows it. Percent for CPU, bytes for memory, bytes per second for throughput.",
            json!({
                "host": {"type": "string", "description": "Host UUID or name (give host or vm)."},
                "vm": {"type": "string", "description": "VM UUID or name (give host or vm)."},
                "interval": {"type": "string", "enum": ["5s", "1m", "1h", "1d"], "description": "Resolution (default 1m)."},
                "points": {"type": "integer", "minimum": 1, "maximum": 400, "description": "Newest points per data source (default 12)."},
                "sources": {"type": "array", "items": {"type": "string"}, "description": "Data source names (default: those of the default graphs). An empty list returns the names available."}
            }),
            &[],
            false,
        ),
    ];
    if writes {
        out.extend(crate::writes::definitions());
    }
    out
}

/// Runs read tool `name`; `None` if it is not one.
pub async fn call_read(
    server: &McpServer,
    call: &mut Call,
    name: &str,
    args: &Args<'_>,
) -> Option<ToolResult> {
    Some(match name {
        "list_servers" => list_servers(server, args),
        "get_pool" => get_pool(server, call, args).await,
        "list_hosts" => list_hosts(server, call, args).await,
        "get_host" => get_one(server, call, args, "host").await,
        "list_vms" => list_vms(server, call, args).await,
        "get_vm" => get_one(server, call, args, "vm").await,
        "list_srs" => list_srs(server, call, args).await,
        "get_sr" => get_one(server, call, args, "sr").await,
        "list_networks" => list_networks(server, call, args).await,
        "get_network" => get_one(server, call, args, "network").await,
        "list_alerts" => list_alerts(server, call, args).await,
        "get_performance" => crate::perf::get_performance(server, call, args).await,
        _ => return None,
    })
}

fn list_servers(server: &McpServer, args: &Args<'_>) -> ToolResult {
    args.only(&[])?;
    let servers: Vec<Value> = server.slots.iter().map(|s| server.slot_status(s)).collect();
    let mut out = json!({
        "servers": servers,
        "writes_enabled": server.writes_enabled(),
    });
    if !server.unknown.is_empty() {
        out["not_saved"] = json!(server.unknown);
    }
    if server.slots.is_empty() {
        out["hint"] = json!(
            "No saved server is configured: save one with `xenadmin --add-server <address> --password-stdin` or in the window with Remember password."
        );
    }
    Ok(out)
}

/// The connections a list tool reads: the one named, or every configured
/// server (each must be connected).
async fn connections(
    server: &McpServer,
    call: &mut Call,
    args: &Args<'_>,
) -> Result<Vec<Arc<ConnectionView>>, ToolError> {
    match args.str("server")? {
        Some(name) => Ok(vec![server.connection(call, Some(name)).await?.1]),
        None if server.slots.len() == 1 => Ok(vec![server.connection(call, None).await?.1]),
        None => {
            if server.slots.is_empty() {
                return Err(server.slot(None).unwrap_err());
            }
            let mut out = Vec::new();
            for slot in &server.slots {
                out.push(server.connected(slot).await?);
            }
            Ok(out)
        }
    }
}

fn contains(haystack: &str, needle: Option<&str>) -> bool {
    needle.is_none_or(|n| haystack.to_lowercase().contains(&n.to_lowercase()))
}

/// Tags each item with its server when several are listed.
fn tagged(items: Vec<Value>, conn: &ConnectionView, several: bool) -> Vec<Value> {
    if !several {
        return items;
    }
    items
        .into_iter()
        .map(|mut v| {
            v["server"] = json!(conn.server);
            v
        })
        .collect()
}

async fn get_pool(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server"])?;
    let conns = connections(server, call, args).await?;
    let pools: Vec<Value> = conns
        .iter()
        .filter_map(|c| {
            let (r, p) = c.model.pool()?;
            let mut v = views::pool_detail(&c.model, r, p);
            v["server"] = json!(c.server);
            Some(v)
        })
        .collect();
    if let [one] = pools.as_slice() {
        call.target = Some(json!({"kind": "pool", "uuid": one["uuid"], "name": one["name"]}));
        return Ok(one.clone());
    }
    Ok(json!({"pools": pools}))
}

async fn list_hosts(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server", "name"])?;
    let name = args.str("name")?;
    let conns = connections(server, call, args).await?;
    let several = conns.len() > 1;
    let mut hosts = Vec::new();
    for c in &conns {
        let items = objects::sorted_hosts(&c.model)
            .into_iter()
            .filter(|(_, h)| contains(&h.name_label, name))
            .map(|(r, h)| views::host_summary(&c.model, r, h))
            .collect();
        hosts.extend(tagged(items, c, several));
    }
    Ok(json!({"count": hosts.len(), "hosts": hosts}))
}

async fn list_vms(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&[
        "server",
        "name",
        "power_state",
        "host",
        "tag",
        "include_templates",
        "include_snapshots",
        "include_control_domains",
        "limit",
    ])?;
    let name = args.str("name")?;
    let power = match args.str("power_state")? {
        None => None,
        Some(p) => Some(
            ["Running", "Halted", "Suspended", "Paused"]
                .into_iter()
                .find(|s| s.eq_ignore_ascii_case(p))
                .ok_or_else(|| {
                    ToolError::Invalid(format!(
                        "power_state '{p}' is not one of Running, Halted, Suspended, Paused"
                    ))
                })?,
        ),
    };
    let host = args.str("host")?;
    let tag = args.str("tag")?;
    let templates = args.bool("include_templates")?.unwrap_or(false);
    let snapshots = args.bool("include_snapshots")?.unwrap_or(false);
    let dom0 = args.bool("include_control_domains")?.unwrap_or(false);
    let limit = args.u64("limit")?.unwrap_or(500) as usize;
    let conns = connections(server, call, args).await?;
    let several = conns.len() > 1;
    let mut vms = Vec::new();
    for c in &conns {
        let model = &c.model;
        let host_ref = match host {
            Some(h) => Some(resolve(
                model
                    .hosts
                    .iter()
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                h,
                "host",
            )?),
            None => None,
        };
        let mut list: Vec<_> = model
            .vms
            .iter()
            .filter(|(_, v)| match objects::vm_kind(v) {
                VmKind::Guest => objects::vm_is_shown(v, false),
                VmKind::ControlDomain => dom0,
                VmKind::Snapshot => snapshots,
                VmKind::UserTemplate => templates && objects::vm_is_shown(v, false),
                VmKind::DefaultTemplate => templates,
            })
            .filter(|(_, v)| contains(&v.name_label, name))
            .filter(|(_, v)| power.is_none_or(|p| v.power_state.as_str() == p))
            .filter(|(_, v)| host_ref.is_none_or(|h| v.resident_on == *h))
            .filter(|(_, v)| tag.is_none_or(|t| v.tags.iter().any(|x| x == t)))
            .collect();
        list.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
        let items = list
            .into_iter()
            .map(|(_, v)| views::vm_summary(model, v))
            .collect();
        vms.extend(tagged(items, c, several));
    }
    let total = vms.len();
    vms.truncate(limit);
    let mut out = json!({"count": vms.len(), "vms": vms});
    if total > limit {
        out["truncated_from"] = json!(total);
    }
    Ok(out)
}

async fn list_srs(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server", "name", "type"])?;
    let name = args.str("name")?;
    let kind = args.str("type")?;
    let conns = connections(server, call, args).await?;
    let several = conns.len() > 1;
    let mut srs = Vec::new();
    for c in &conns {
        let mut list: Vec<_> = c
            .model
            .srs
            .iter()
            .filter(|(_, s)| objects::sr_is_shown(s, false))
            .filter(|(_, s)| contains(&s.name_label, name))
            .filter(|(_, s)| kind.is_none_or(|k| s.r#type.eq_ignore_ascii_case(k)))
            .collect();
        list.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label));
        let items = list
            .into_iter()
            .map(|(r, s)| views::sr_summary(&c.model, r, s))
            .collect();
        srs.extend(tagged(items, c, several));
    }
    Ok(json!({"count": srs.len(), "srs": srs}))
}

async fn list_networks(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server", "name"])?;
    let name = args.str("name")?;
    let conns = connections(server, call, args).await?;
    let several = conns.len() > 1;
    let mut nets = Vec::new();
    for c in &conns {
        let model = &c.model;
        let mut list: Vec<_> = model
            .networks
            .values()
            .filter(|n| !objects::is_guest_installer_network(n))
            .filter(|n| contains(&objects::network_name(model, n), name))
            .collect();
        list.sort_by(|a, b| {
            objects::natural_cmp(
                &objects::network_name(model, a),
                &objects::network_name(model, b),
            )
        });
        let items = list
            .into_iter()
            .map(|n| views::network_summary(model, n))
            .collect();
        nets.extend(tagged(items, c, several));
    }
    Ok(json!({"count": nets.len(), "networks": nets}))
}

async fn list_alerts(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server", "min_priority", "object", "limit"])?;
    let min = args.u64("min_priority")?.unwrap_or(5);
    if !(1..=5).contains(&min) {
        return Err(ToolError::Invalid("min_priority must be 1 to 5".into()));
    }
    let object = args.str("object")?;
    let limit = args.u64("limit")?.unwrap_or(100) as usize;
    let conns = connections(server, call, args).await?;
    let several = conns.len() > 1;
    let mut out = Vec::new();
    for c in &conns {
        let items = alerts::alerts(&c.model)
            .iter()
            .filter(|a| u64::from(views::priority_rank(a.priority)) <= min)
            .map(|a| views::alert(&c.model, a))
            .filter(|v| {
                object.is_none_or(|o| {
                    v["object"]["name"] == o
                        || v["object"]["uuid"]
                            .as_str()
                            .is_some_and(|u| u.eq_ignore_ascii_case(o))
                })
            })
            .collect();
        out.extend(tagged(items, c, several));
    }
    out.sort_by(|a, b| b["time"].as_str().cmp(&a["time"].as_str()));
    let total = out.len();
    out.truncate(limit);
    let mut result = json!({"count": out.len(), "alerts": out});
    if total > limit {
        result["truncated_from"] = json!(total);
    }
    Ok(result)
}

/// `get_host`, `get_vm`, `get_sr`, `get_network`.
async fn get_one(server: &McpServer, call: &mut Call, args: &Args<'_>, kind: &str) -> ToolResult {
    args.only(&["server", kind])?;
    let query = args.req_str(kind)?;
    let (_, conn) = server.connection(call, args.str("server")?).await?;
    let detail = detail(&conn, kind, query)?;
    call.target = Some(json!({"kind": kind, "uuid": detail["uuid"], "name": detail["name"]}));
    Ok(detail)
}

/// The detail view of object `query` of `kind` (`pool`, `host`, `vm`, `sr`,
/// `network`) on `conn`.
pub(crate) fn detail(conn: &ConnectionView, kind: &str, query: &str) -> ToolResult {
    let model = &conn.model;
    match kind {
        "pool" => {
            let (r, p) = model
                .pool()
                .filter(|(r, p)| r.as_str() == query || p.uuid.eq_ignore_ascii_case(query))
                .ok_or_else(|| ToolError::Invalid(format!("no pool has the UUID '{query}'")))?;
            Ok(views::pool_detail(model, r, p))
        }
        "host" => {
            let r = resolve(
                model
                    .hosts
                    .iter()
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                query,
                "host",
            )?;
            Ok(views::host_detail(model, r, &model.hosts[r]))
        }
        "vm" => {
            let r = resolve(
                model
                    .vms
                    .iter()
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                query,
                "VM",
            )?;
            Ok(views::vm_detail(model, &model.vms[r]))
        }
        "sr" => {
            let r = resolve(
                model
                    .srs
                    .iter()
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                query,
                "SR",
            )?;
            Ok(views::sr_detail(model, r, &model.srs[r]))
        }
        "network" => {
            let names: Vec<(xapi::Ref<xapi::Network>, String, String)> = model
                .networks
                .iter()
                .map(|(r, n)| (r.clone(), n.uuid.clone(), objects::network_name(model, n)))
                .collect();
            let r = resolve(
                names.iter().map(|(r, u, n)| (r, u.as_str(), n.as_str())),
                query,
                "network",
            )?;
            Ok(views::network_detail(model, r, &model.networks[r]))
        }
        other => Err(ToolError::Invalid(format!("unknown object kind '{other}'"))),
    }
}
