//! Performance data of the mock: `GET /rrd_updates`, `/host_rrds` and
//! `/vm_rrds`, plus `host.get_data_sources` / `VM.get_data_sources`.
//!
//! Values are made up from the time and the data source name (smooth
//! waves), so graphs move; a VM has data only while it runs, as on XAPI
//! (`rrd_updates` for a halted VM fails with HTTP 500, `vm_rrds` still
//! serves its archive). Answers are JSON when the query has `json=true`
//! and JSON is enabled ([`crate::MockServer::set_rrd_json`]), XML
//! otherwise. The clock can be moved forward
//! ([`crate::MockServer::advance_rrd_clock`]) so tests see new rows
//! without waiting.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::Db;

/// `(name, units)` of an object's data sources.
pub(crate) type Sources = Vec<(String, &'static str)>;

/// The archives XAPI keeps: (pdp per row, rows, consolidation functions).
const ARCHIVES: [(i64, usize, &[&str]); 4] = [
    (1, 120, &["AVERAGE"]),
    (12, 120, &["AVERAGE", "MIN", "MAX"]),
    (720, 168, &["AVERAGE", "MIN", "MAX"]),
    (17280, 366, &["AVERAGE", "MIN", "MAX"]),
];

impl Db {
    /// The mock's current time (Unix seconds).
    pub(crate) fn rrd_now(&self) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now + self.rrd_clock_offset
    }

    fn refs_by_uuid(&self, class: &str, uuid: &str) -> Option<String> {
        self.objects.iter().find_map(|((c, r), rec)| {
            (c == class && rec.get("uuid").and_then(Value::as_str) == Some(uuid)).then(|| r.clone())
        })
    }

    fn field_refs(&self, class: &str, r: &str, field: &str) -> Vec<String> {
        self.objects
            .get(&(class.to_owned(), r.to_owned()))
            .and_then(|rec| rec.get(field))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn field(&self, class: &str, r: &str, field: &str) -> Option<&Value> {
        self.objects
            .get(&(class.to_owned(), r.to_owned()))
            .and_then(|rec| rec.get(field))
    }

    /// A host's data sources.
    pub(crate) fn host_sources(&self, host: &str) -> Sources {
        let mut out: Sources = Vec::new();
        let mut cpus: Vec<i64> = self
            .field_refs("host", host, "host_CPUs")
            .iter()
            .filter_map(|c| self.field("host_cpu", c, "number").and_then(int))
            .collect();
        cpus.sort_unstable();
        out.extend(cpus.iter().map(|n| (format!("cpu{n}"), "(fraction)")));
        out.push(("cpu_avg".into(), "(fraction)"));
        out.push(("memory_total_kib".into(), "KiB"));
        out.push(("memory_free_kib".into(), "KiB"));
        let mut pifs: Vec<String> = self
            .field_refs("host", host, "PIFs")
            .iter()
            .filter_map(|p| self.field("pif", p, "device").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        pifs.sort();
        pifs.dedup();
        for d in pifs {
            out.push((format!("pif_{d}_rx"), "B/s"));
            out.push((format!("pif_{d}_tx"), "B/s"));
        }
        out.push(("loadavg".into(), "(fraction)"));
        out
    }

    /// A VM's data sources (none unless it runs).
    pub(crate) fn vm_sources(&self, vm: &str) -> Sources {
        let mut out: Sources = Vec::new();
        let vcpus = self
            .field("vm", vm, "VCPUs_at_startup")
            .and_then(int)
            .unwrap_or(1);
        out.extend((0..vcpus).map(|n| (format!("cpu{n}"), "(fraction)")));
        out.push(("cpu_usage".into(), "(fraction)"));
        out.push(("memory".into(), "B"));
        out.push(("memory_target".into(), "B"));
        out.push(("memory_internal_free".into(), "KiB"));
        for vif in self.field_refs("vm", vm, "VIFs") {
            if let Some(d) = self.field("vif", &vif, "device").and_then(Value::as_str) {
                out.push((format!("vif_{d}_rx"), "B/s"));
                out.push((format!("vif_{d}_tx"), "B/s"));
            }
        }
        for vbd in self.field_refs("vm", vm, "VBDs") {
            if let Some(d) = self
                .field("vbd", &vbd, "device")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                out.push((format!("vbd_{d}_read"), "B/s"));
                out.push((format!("vbd_{d}_write"), "B/s"));
            }
        }
        out
    }

    fn vm_running(&self, vm: &str) -> bool {
        self.get_str("vm", vm, "power_state").as_deref() == Some("Running")
    }

    /// Host memory in KiB (from its metrics).
    fn host_memory_kib(&self, host: &str) -> f64 {
        self.field("host", host, "metrics")
            .and_then(Value::as_str)
            .and_then(|m| self.field("host_metrics", m, "memory_total"))
            .and_then(num)
            .map(|b| b / 1024.0)
            .unwrap_or(16.0 * 1024.0 * 1024.0)
    }

    /// VM memory in bytes.
    fn vm_memory(&self, vm: &str) -> f64 {
        self.field("vm", vm, "metrics")
            .and_then(Value::as_str)
            .and_then(|m| self.field("vm_metrics", m, "memory_actual"))
            .and_then(num)
            .filter(|m| *m > 0.0)
            .or_else(|| self.field("vm", vm, "memory_dynamic_max").and_then(num))
            .unwrap_or(1024.0 * 1024.0 * 1024.0)
    }

    /// The value of data source `name` of an object at time `t`.
    fn value(&self, class: &str, r: &str, name: &str, t: i64) -> f64 {
        let seed = name
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
        let phase = f64::from(seed % 628) / 100.0;
        let wave = |period: f64| (t as f64 / period + phase).sin();
        match name {
            n if n.starts_with("cpu") => (0.25 + 0.2 * wave(40.0)).clamp(0.0, 1.0),
            "loadavg" => 0.5 + 0.3 * wave(90.0),
            "memory_total_kib" => self.host_memory_kib(r),
            "memory_free_kib" => self.host_memory_kib(r) * (0.55 + 0.05 * wave(300.0)),
            "memory" | "memory_target" => self.vm_memory(r),
            "memory_internal_free" => self.vm_memory(r) / 1024.0 * (0.45 + 0.05 * wave(200.0)),
            n if n.starts_with("pif_") => 40_000.0 + 30_000.0 * wave(30.0),
            n if n.starts_with("vif_") => 8_000.0 + 6_000.0 * wave(25.0),
            n if n.starts_with("vbd_") => 60_000.0 + 50_000.0 * wave(35.0),
            _ if class == "vm" => 0.0,
            _ => 1.0,
        }
    }

    /// `rrd_updates` columns: `(legend entry, class, ref, name)`.
    fn update_columns(
        &self,
        host: Option<&str>,
        vm: Option<&str>,
    ) -> Vec<(String, String, String, String)> {
        let mut out = Vec::new();
        if let Some(h) = host {
            let uuid = self.get_str("host", h, "uuid").unwrap_or_default();
            for (name, _) in self.host_sources(h) {
                out.push((
                    format!("AVERAGE:host:{uuid}:{name}"),
                    "host".into(),
                    h.to_owned(),
                    name,
                ));
            }
            // XAPI adds the VMs running there.
            let vms: Vec<String> = self
                .objects
                .iter()
                .filter(|((c, _), rec)| {
                    c == "vm" && rec.get("resident_on").and_then(Value::as_str) == Some(h)
                })
                .map(|((_, r), _)| r.clone())
                .collect();
            for v in vms.iter().filter(|v| self.vm_running(v)) {
                out.extend(self.vm_columns(v));
            }
        }
        if let Some(v) = vm {
            out.extend(self.vm_columns(v));
        }
        out
    }

    fn vm_columns(&self, vm: &str) -> Vec<(String, String, String, String)> {
        let uuid = self.get_str("vm", vm, "uuid").unwrap_or_default();
        self.vm_sources(vm)
            .into_iter()
            .map(|(name, _)| {
                (
                    format!("AVERAGE:vm:{uuid}:{name}"),
                    "vm".into(),
                    vm.to_owned(),
                    name,
                )
            })
            .collect()
    }

    /// `GET /rrd_updates`: `(status, content type, body)`.
    pub(crate) fn rrd_updates(&self, query: &Query) -> (u16, &'static str, String) {
        let interval = query.int("interval").unwrap_or(5).max(1);
        let start = query.int("start").unwrap_or(0);
        let host = (query.get("host") == Some("true"))
            .then(|| self.coordinator_host())
            .flatten();
        let vm = match query.get("vm_uuid") {
            Some(uuid) => match self.refs_by_uuid("vm", uuid) {
                Some(v) if self.vm_running(&v) => Some(v),
                // XAPI: `Rrd.No_RRA_Available`.
                _ => return (500, "text/html", "<html><body><h1>HTTP 500 internal server error</h1>Rrd.No_RRA_Available</body></html>".into()),
            },
            None => None,
        };
        let rows = ARCHIVES
            .iter()
            .find(|(pdp, ..)| pdp * 5 == interval)
            .map(|(_, rows, _)| *rows)
            .unwrap_or(120) as i64;
        let now = self.rrd_now();
        let end = now - now.rem_euclid(interval);
        let first =
            (end - (rows - 1) * interval).max(start - start.rem_euclid(interval) + interval);
        let times: Vec<i64> = (0..)
            .map(|i| end - i * interval)
            .take_while(|t| *t >= first)
            .collect();
        let columns = self.update_columns(host.as_deref(), vm.as_deref());
        let row_values = |t: i64| -> Vec<f64> {
            columns
                .iter()
                .map(|(_, class, r, name)| self.value(class, r, name, t))
                .collect()
        };
        let json = query.get("json") == Some("true") && self.rrd_json;
        let begin = times.last().copied().unwrap_or(end) - interval;
        let body = if json {
            json!({
                "meta": {
                    "start": begin.to_string(), "step": interval.to_string(),
                    "end": end.to_string(), "rows": times.len().to_string(),
                    "columns": columns.len().to_string(),
                    "legend": columns.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
                },
                "data": times.iter().map(|t| json!({
                    "t": t.to_string(),
                    "values": row_values(*t).iter().map(|v| number(*v)).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
            .to_string()
        } else {
            let mut x = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xport><meta><start>{begin}</start><step>{interval}</step><end>{end}</end><rows>{}</rows><columns>{}</columns><legend>",
                times.len(),
                columns.len()
            );
            for c in &columns {
                x.push_str(&format!("<entry>{}</entry>", c.0));
            }
            x.push_str("</legend></meta><data>");
            for t in &times {
                x.push_str(&format!("<row><t>{t}</t>"));
                for v in row_values(*t) {
                    x.push_str(&format!("<v>{}</v>", number(v)));
                }
                x.push_str("</row>");
            }
            x.push_str("</data></xport>");
            x
        };
        (
            200,
            if json { "application/json" } else { "text/xml" },
            body,
        )
    }

    fn coordinator_host(&self) -> Option<String> {
        self.objects
            .iter()
            .find(|((c, _), _)| c == "pool")
            .and_then(|(_, p)| p.get("master").and_then(Value::as_str))
            .map(str::to_owned)
    }

    /// `GET /host_rrds` (the coordinator's) or `GET /vm_rrds?uuid=`.
    pub(crate) fn full_rrd(
        &self,
        vm_uuid: Option<&str>,
        json: bool,
    ) -> (u16, &'static str, String) {
        let (class, r, sources) = match vm_uuid {
            // XAPI keeps the archive of a VM that stopped.
            Some(uuid) => match self.refs_by_uuid("vm", uuid) {
                Some(v) => {
                    let s = self.vm_sources(&v);
                    ("vm", v, s)
                }
                None => return (404, "text/plain", "no such RRD".into()),
            },
            None => match self.coordinator_host() {
                Some(h) => {
                    let s = self.host_sources(&h);
                    ("host", h, s)
                }
                None => return (404, "text/plain", "no host".into()),
            },
        };
        let now = self.rrd_now();
        let json = json && self.rrd_json;
        let mut rras = Vec::new();
        for (pdp, rows, cfs) in ARCHIVES {
            let step = pdp * 5;
            let last = now - now.rem_euclid(step);
            for cf in cfs {
                let data: Vec<Vec<f64>> = (0..rows as i64)
                    .map(|i| last - (rows as i64 - 1 - i) * step)
                    .map(|t| {
                        sources
                            .iter()
                            .map(|(n, _)| self.value(class, &r, n, t))
                            .collect()
                    })
                    .collect();
                rras.push((cf, pdp, data));
            }
        }
        let body = if json {
            json!({
                "version": "0003", "step": "5", "lastupdate": now.to_string(),
                "ds": sources.iter().map(|(n, _)| json!({"name": n, "type": "GAUGE",
                    "minimal_heartbeat": "300", "min": "0.0", "max": "Infinity"})).collect::<Vec<_>>(),
                "rra": rras.iter().map(|(cf, pdp, data)| json!({
                    "cf": cf, "pdp_per_row": pdp.to_string(), "params": {"xff": "0.5"},
                    "cdp_prep": {"ds": []},
                    "database": data.iter().map(|row| row.iter().map(|v| number(*v)).collect::<Vec<_>>()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
            .to_string()
        } else {
            let mut x = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rrd><version>0003</version><step>5</step><lastupdate>{now}</lastupdate>"
            );
            for (n, _) in &sources {
                x.push_str(&format!("<ds><name>{n}</name><type>GAUGE</type><minimal_heartbeat>300</minimal_heartbeat><min>0.0</min><max>Infinity</max></ds>"));
            }
            for (cf, pdp, data) in &rras {
                x.push_str(&format!("<rra><cf>{cf}</cf><pdp_per_row>{pdp}</pdp_per_row><params><xff>0.5</xff></params><cdp_prep></cdp_prep><database>"));
                for row in data {
                    x.push_str("<row>");
                    for v in row {
                        x.push_str(&format!("<v>{}</v>", number(*v)));
                    }
                    x.push_str("</row>");
                }
                x.push_str("</database></rra>");
            }
            x.push_str("</rrd>");
            x
        };
        (
            200,
            if json { "application/json" } else { "text/xml" },
            body,
        )
    }

    /// `host.get_data_sources` / `VM.get_data_sources`.
    pub(crate) fn data_sources(&self, class: &str, r: &str) -> Option<Value> {
        if !self.objects.contains_key(&(class.to_owned(), r.to_owned())) {
            return None;
        }
        let now = self.rrd_now();
        let sources = if class == "host" {
            self.host_sources(r)
        } else if self.vm_running(r) {
            self.vm_sources(r)
        } else {
            Vec::new()
        };
        Some(Value::Array(
            sources
                .iter()
                .map(|(n, units)| {
                    json!({
                        "name_label": n, "name_description": "", "enabled": true,
                        "standard": true, "units": if self.rrd_units { *units } else { "" }, "min": 0.0,
                        "max": if units == &"(fraction)" { json!(1.0) } else { json!(f64::MAX) },
                        "value": self.value(class, r, n, now),
                    })
                })
                .collect(),
        ))
    }
}

fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else {
        format!("{v}")
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// A parsed query string.
pub(crate) struct Query(Vec<(String, String)>);

impl Query {
    pub(crate) fn parse(query: &str) -> Self {
        Self(
            query
                .split('&')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (decode(k), decode(v))
                })
                .collect(),
        )
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.parse().ok())
    }
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
