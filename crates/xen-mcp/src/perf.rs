//! The performance tool: the recent RRD data of a host or VM, read the way
//! the Performance tab reads it ([`xen_core::performance`]: the data
//! sources, the whole RRD, then updates), for as long as the call lasts.

use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{Value, json};
use xen_core::performance::{self, PerfStatus, PerfTarget, Unit};
use xen_core::{Command, ConnectionView};
use xen_rrd::Interval;

use crate::server::{Args, Call, McpServer, ToolError, ToolResult, resolve};

fn interval(text: Option<&str>) -> Result<Interval, ToolError> {
    match text.unwrap_or("1m") {
        "5s" => Ok(Interval::FiveSeconds),
        "1m" => Ok(Interval::OneMinute),
        "1h" => Ok(Interval::OneHour),
        "1d" => Ok(Interval::OneDay),
        other => Err(ToolError::Invalid(format!(
            "interval '{other}' is not one of 5s, 1m, 1h, 1d"
        ))),
    }
}

fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Percent => "percent",
        Unit::Bytes => "bytes",
        Unit::BytesPerSecond => "bytes_per_second",
        Unit::Plain => "number",
    }
}

/// The object `args` names (`host` or `vm`) on `conn`.
pub(crate) fn target(
    conn: &ConnectionView,
    host: Option<&str>,
    vm: Option<&str>,
) -> Result<(PerfTarget, Value), ToolError> {
    let model = &conn.model;
    match (host, vm) {
        (Some(h), None) => {
            let r = resolve(
                model
                    .hosts
                    .iter()
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                h,
                "host",
            )?;
            let rec = &model.hosts[r];
            Ok((
                PerfTarget::Host(r.clone()),
                json!({"kind": "host", "uuid": rec.uuid, "name": rec.name_label}),
            ))
        }
        (None, Some(v)) => {
            let r = resolve(
                model
                    .vms
                    .iter()
                    .filter(|(_, x)| !x.is_a_template && !x.is_a_snapshot)
                    .map(|(r, x)| (r, x.uuid.as_str(), x.name_label.as_str())),
                v,
                "VM",
            )?;
            let rec = &model.vms[r];
            Ok((
                PerfTarget::Vm(r.clone()),
                json!({"kind": "vm", "uuid": rec.uuid, "name": rec.name_label}),
            ))
        }
        _ => Err(ToolError::Invalid("give either 'host' or 'vm'".into())),
    }
}

pub async fn get_performance(server: &McpServer, call: &mut Call, args: &Args<'_>) -> ToolResult {
    args.only(&["server", "host", "vm", "interval", "points", "sources"])?;
    let interval = interval(args.str("interval")?)?;
    let points = args.u64("points")?.unwrap_or(12).clamp(1, 400) as usize;
    let wanted = args.str_list("sources")?;
    let (host, vm) = (args.str("host")?, args.str("vm")?);
    let (_, conn) = server.connection(call, args.str("server")?).await?;
    let (target, described) = target(&conn, host, vm)?;
    call.target = Some(described.clone());
    query(server, &conn, target, described, interval, points, wanted).await
}

/// Opens the performance data of `target`, waits for it to load and
/// returns the newest `points` of each wanted source.
pub(crate) async fn query(
    server: &McpServer,
    conn: &ConnectionView,
    target: PerfTarget,
    described: Value,
    interval: Interval,
    points: usize,
    wanted: Option<Vec<&str>>,
) -> ToolResult {
    let (handle, link) = performance::pair();
    server.backend.send(Command::OpenPerformance {
        connection: conn.id,
        target: target.clone(),
        link,
    });
    let deadline = Instant::now() + server.timings.performance_wait;
    let data = loop {
        let data = handle.data();
        match &data.status {
            PerfStatus::Live if !data.archives.is_empty() => break data,
            PerfStatus::NotRunning => break data,
            PerfStatus::Gone => {
                return Err(ToolError::Invalid("the object is no longer there".into()));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let why = match &data.status {
                PerfStatus::Failed(e) => format!(": {e}"),
                PerfStatus::Disconnected => ": the server is not connected".into(),
                _ => String::new(),
            };
            if data.archives.is_empty() {
                return Err(ToolError::Failed(format!(
                    "the performance data did not load in time{why}"
                )));
            }
            break data;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    drop(handle);

    let available: BTreeSet<&str> = data.archives.names();
    let names: Vec<String> = match wanted {
        Some(list) if list.is_empty() => {
            return Ok(json!({
                "object": described,
                "available_sources": available,
            }));
        }
        Some(list) => list.into_iter().map(str::to_owned).collect(),
        None => {
            let mut names: Vec<String> = performance::default_graphs(&conn.model, &target)
                .into_iter()
                .flat_map(|g| g.sources)
                .collect();
            names.dedup();
            names
        }
    };
    let mut series = Vec::new();
    let mut missing = Vec::new();
    for name in &names {
        let raw = data.archives.series(interval, name);
        if raw.is_empty() && !available.contains(name.as_str()) {
            missing.push(name.clone());
            continue;
        }
        let (unit, factor) = performance::unit_of(name, &data.sources);
        let start = raw.len().saturating_sub(points);
        let pts: Vec<Value> = raw[start..]
            .iter()
            .map(|(t, v)| {
                let v = v * factor;
                json!([
                    crate::audit::unix_rfc3339(*t),
                    if v.is_finite() { json!(v) } else { Value::Null }
                ])
            })
            .collect();
        let latest = raw[start..]
            .iter()
            .rev()
            .map(|(_, v)| v * factor)
            .find(|v| v.is_finite());
        series.push(json!({
            "name": name,
            "unit": unit_name(unit),
            "latest": latest,
            "points": pts,
        }));
    }
    let mut out = json!({
        "object": described,
        "status": match data.status {
            PerfStatus::NotRunning => "not running (the data shown is what was recorded before it stopped)",
            PerfStatus::Live => "live",
            _ => "partial",
        },
        "interval": match interval {
            Interval::FiveSeconds => "5s",
            Interval::OneMinute => "1m",
            Interval::OneHour => "1h",
            Interval::OneDay => "1d",
        },
        "series": series,
    });
    if !missing.is_empty() {
        out["unknown_sources"] = json!(missing);
        out["available_sources"] = json!(available);
    }
    Ok(out)
}
