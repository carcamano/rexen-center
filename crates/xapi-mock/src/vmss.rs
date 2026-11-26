//! Scheduled snapshots (`VMSS`, F12b) in the mock, and its clock.
//!
//! **Clock.** The mock has its own clock: it starts at
//! 2026-11-25 12:00:00 UTC (a Wednesday) and only moves when a test calls
//! [`crate::MockServer::advance_clock`]. `host.get_servertime` answers it
//! in UTC and `host.get_server_localtime` in the server's time zone
//! ([`crate::MockServer::set_server_utc_offset`], UTC by default), as
//! `YYYYMMDDTHH:MM:SSZ` and `YYYYMMDDTHH:MM:SS`.
//!
//! **Policies.** `VMSS.create(record)` makes a policy from the record's
//! `name_label`, `name_description`, `enabled`, `type`,
//! `retained_snapshots`, `frequency` and `schedule`, as XAPI checks them:
//! `retained_snapshots` 1–10, and the schedule's keys for the frequency
//! (`min` in 0/15/30/45; `hour` 0–23 for daily and weekly; `days`, a
//! comma-separated list of English day names, for weekly), else
//! `INVALID_VALUE [key, value]`. `VMSS.destroy` refuses a policy that still
//! has VMs (`VMSS_HAS_VM`). `VM.set_snapshot_schedule(vm, policy or
//! OpaqueRef:NULL)` moves a VM in or out (a template or snapshot:
//! `VM_IS_TEMPLATE` / `VM_IS_SNAPSHOT`) and keeps the policies' `VMs` in
//! step. The setters are XAPI's: `set_frequency` clears the schedule (it
//! no longer fits), `set_schedule` and `set_retained_snapshots` check the
//! value, the others just set it.
//!
//! **Snapshots.** `VMSS.snapshot_now(policy)` (and a scheduled run) takes a
//! snapshot of each of the policy's VMs: `VM.snapshot` (type `snapshot` or
//! `snapshot_with_quiesce`) or `VM.checkpoint` (type `checkpoint`), named
//! `<vm>-<policy>-<YYYYMMDDTHHMM>` (server local time) and marked
//! `is_vmss_snapshot`; then the VM's oldest scheduled snapshots beyond
//! `retained_snapshots` are destroyed with their disks (manual snapshots
//! are never touched). `last_run_time` is set, and a message is created
//! for the policy (class `VMSS`): `VMSS_SNAPSHOT_SUCCEEDED` (priority 5)
//! when every VM was snapshotted, else `VMSS_SNAPSHOT_FAILED` (priority 2)
//! with one body line per failed VM, as XAPI writes them:
//! `VM:<name>UUID:<uuid>Error:['<CODE>', '<param>']`. A VM made to fail
//! ([`crate::MockServer::fail_vmss_snapshot`]) or one that does not allow
//! the operation fails.
//!
//! **Schedule.** Advancing the clock runs every **enabled** policy once for
//! each of its scheduled times passed (minute by minute in server local
//! time: hourly at `min`, daily at `hour:min`, weekly at `hour:min` on the
//! listed days), oldest first. A disabled policy never runs by itself.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::provisioning::int;
use crate::{Db, NULL_REF, Reply, State, api_error, str_param};

/// The clock's start: 2026-11-25T12:00:00Z (a Wednesday).
pub const CLOCK_START: i64 = 1_795_608_000;

/// Scheduled-snapshot knobs and the clock of one mock.
#[derive(Default)]
pub(crate) struct World {
    /// Seconds the clock moved since [`CLOCK_START`].
    pub(crate) advanced: i64,
    /// The server's time zone: seconds east of UTC.
    pub(crate) utc_offset: i64,
    /// VMs (references) whose scheduled snapshot fails, with the error.
    pub(crate) failing: Vec<(String, String)>,
    /// Policy runs so far: `(policy, scheduled)`; `scheduled` false for
    /// `snapshot_now`.
    pub(crate) runs: Vec<(String, bool)>,
}

impl World {
    /// UTC now, in Unix seconds.
    pub(crate) fn now(&self) -> i64 {
        CLOCK_START + self.advanced
    }
}

/// The record calls handled through [`crate::provisioning`].
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        (
            "VMSS",
            "create"
                | "destroy"
                | "snapshot_now"
                | "set_frequency"
                | "set_schedule"
                | "set_retained_snapshots"
                | "add_to_schedule"
                | "remove_from_schedule"
        ) | ("VM", "set_snapshot_schedule")
            | ("host", "get_server_localtime")
    )
}

// ---- dates ------------------------------------------------------------------------------

/// (year, month, day) of a day count since 1970-01-01.
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `YYYYMMDDTHH:MM:SS` of `secs`.
pub(crate) fn stamp(secs: i64) -> String {
    let (y, mo, d) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}{mo:02}{d:02}T{:02}:{:02}:{:02}",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

/// Day of the week of `secs` (0 = Sunday).
fn weekday(secs: i64) -> i64 {
    // 1970-01-01 was a Thursday.
    (secs.div_euclid(86_400) + 4).rem_euclid(7)
}

const DAYS: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

fn text(rec: &Map<String, Value>, field: &str) -> String {
    rec.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn refs(rec: &Map<String, Value>, field: &str) -> Vec<String> {
    rec.get(field)
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn schedule_of(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

/// XAPI's schedule check for `frequency`.
fn check_schedule(
    frequency: &str,
    schedule: &Map<String, Value>,
) -> Result<(), (String, Vec<String>)> {
    let bad =
        |k: &str, v: &str| Err(("INVALID_VALUE".to_owned(), vec![k.to_owned(), v.to_owned()]));
    let get = |k: &str| schedule.get(k).and_then(Value::as_str).unwrap_or_default();
    let min = get("min");
    if !matches!(min, "0" | "15" | "30" | "45") {
        return bad("min", min);
    }
    if frequency == "hourly" {
        return Ok(());
    }
    let hour = get("hour");
    if !hour.parse::<u32>().is_ok_and(|h| h < 24) {
        return bad("hour", hour);
    }
    if frequency == "daily" {
        return Ok(());
    }
    if frequency != "weekly" {
        return bad("frequency", frequency);
    }
    let days = get("days");
    let list: Vec<&str> = days
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .collect();
    if list.is_empty()
        || list
            .iter()
            .any(|d| !DAYS.contains(&d.to_lowercase().as_str()))
    {
        return bad("days", days);
    }
    Ok(())
}

fn check_retention(value: i64) -> Result<(), (String, Vec<String>)> {
    if (1..=10).contains(&value) {
        Ok(())
    } else {
        Err((
            "INVALID_VALUE".to_owned(),
            vec!["retained_snapshots".to_owned(), value.to_string()],
        ))
    }
}

/// Whether a policy with `frequency` and `schedule` runs at server local
/// time `local` (a whole minute).
fn due(frequency: &str, schedule: &Map<String, Value>, local: i64) -> bool {
    let num = |k: &str| {
        schedule
            .get(k)
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<i64>().ok())
    };
    let t = local.rem_euclid(86_400);
    let (hour, min) = (t / 3600, (t % 3600) / 60);
    if num("min") != Some(min) {
        return false;
    }
    match frequency {
        "hourly" => true,
        "daily" => num("hour") == Some(hour),
        "weekly" => {
            let today = DAYS[weekday(local) as usize];
            num("hour") == Some(hour)
                && schedule
                    .get("days")
                    .and_then(Value::as_str)
                    .is_some_and(|d| d.split(',').any(|x| x.trim().eq_ignore_ascii_case(today)))
        }
        _ => false,
    }
}

impl Db {
    /// The calls of [`handles`].
    pub(crate) fn vmss_call(&mut self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i).to_owned();
        Some(match (class, call) {
            ("host", "get_server_localtime") => {
                let host = p(1);
                if !self.exists("host", &host) {
                    return Some(api_error("HANDLE_INVALID", &["host", &host]));
                }
                Ok(json!(stamp(self.vmss.now() + self.vmss.utc_offset)))
            }
            ("VMSS", "create") => self.vmss_create(params),
            ("VMSS", "destroy") => {
                let r = p(1);
                let Some(rec) = self.record("vmss", &r) else {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                };
                if !refs(rec, "VMs").is_empty() {
                    return Some(api_error("VMSS_HAS_VM", &[]));
                }
                self.remove("vmss", &r);
                Ok(json!(""))
            }
            ("VMSS", "snapshot_now") => {
                let r = p(1);
                if !self.exists("vmss", &r) {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                }
                self.run_policy(&r, false);
                Ok(json!(""))
            }
            ("VMSS", "set_frequency") => {
                let (r, value) = (p(1), p(2));
                if !self.exists("vmss", &r) {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                }
                if !matches!(value.as_str(), "hourly" | "daily" | "weekly") {
                    return Some(api_error("INVALID_VALUE", &["frequency", &value]));
                }
                self.modify("vmss", &r, |rec| {
                    rec.insert("frequency".into(), json!(value));
                    rec.insert("schedule".into(), json!({}));
                });
                Ok(json!(""))
            }
            ("VMSS", "set_schedule") => {
                let r = p(1);
                let Some(rec) = self.record("vmss", &r) else {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                };
                let schedule = schedule_of(params.get(2));
                if let Err((code, ps)) = check_schedule(&text(rec, "frequency"), &schedule) {
                    let ps: Vec<&str> = ps.iter().map(String::as_str).collect();
                    return Some(api_error(&code, &ps));
                }
                self.modify("vmss", &r, |rec| {
                    rec.insert("schedule".into(), Value::Object(schedule));
                });
                Ok(json!(""))
            }
            ("VMSS", "add_to_schedule" | "remove_from_schedule") => {
                let (r, key) = (p(1), p(2));
                if !self.exists("vmss", &r) {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                }
                let value = p(3);
                let add = call == "add_to_schedule";
                self.modify("vmss", &r, |rec| {
                    let entry = rec.entry("schedule").or_insert_with(|| json!({}));
                    if let Value::Object(m) = entry {
                        if add {
                            m.insert(key, json!(value));
                        } else {
                            m.remove(&key);
                        }
                    }
                });
                Ok(json!(""))
            }
            ("VMSS", "set_retained_snapshots") => {
                let r = p(1);
                if !self.exists("vmss", &r) {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &r]));
                }
                let value = crate::provisioning::int_param(params, 2).unwrap_or(0);
                if let Err((code, ps)) = check_retention(value) {
                    let ps: Vec<&str> = ps.iter().map(String::as_str).collect();
                    return Some(api_error(&code, &ps));
                }
                self.modify("vmss", &r, |rec| {
                    rec.insert("retained_snapshots".into(), json!(value));
                });
                Ok(json!(""))
            }
            _ => {
                // `VM.set_snapshot_schedule`.
                let (vm, policy) = (p(1), p(2));
                let policy = if policy.is_empty() {
                    NULL_REF.to_owned()
                } else {
                    policy
                };
                let Some(rec) = self.record("vm", &vm) else {
                    return Some(api_error("HANDLE_INVALID", &["VM", &vm]));
                };
                if rec.get("is_a_template").and_then(Value::as_bool) == Some(true) {
                    let code = if rec.get("is_a_snapshot").and_then(Value::as_bool) == Some(true) {
                        "VM_IS_SNAPSHOT"
                    } else {
                        "VM_IS_TEMPLATE"
                    };
                    return Some(api_error(code, &[&vm]));
                }
                if policy != NULL_REF && !self.exists("vmss", &policy) {
                    return Some(api_error("HANDLE_INVALID", &["VMSS", &policy]));
                }
                let old = self
                    .get_str("vm", &vm, "snapshot_schedule")
                    .unwrap_or_else(|| NULL_REF.to_owned());
                if old != NULL_REF && old != policy {
                    self.drop_ref("vmss", &old, "VMs", &vm);
                }
                self.modify("vm", &vm, |rec| {
                    rec.insert("snapshot_schedule".into(), json!(policy));
                });
                if policy != NULL_REF {
                    self.push_ref("vmss", &policy, "VMs", &vm);
                }
                Ok(json!(""))
            }
        })
    }

    fn vmss_create(&mut self, params: &[Value]) -> Reply {
        let given = params
            .get(1)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let frequency = text(&given, "frequency");
        let schedule = schedule_of(given.get("schedule"));
        if let Err((code, ps)) = check_schedule(&frequency, &schedule) {
            let ps: Vec<&str> = ps.iter().map(String::as_str).collect();
            return api_error(&code, &ps);
        }
        let retained = int(&given, "retained_snapshots");
        if let Err((code, ps)) = check_retention(retained) {
            let ps: Vec<&str> = ps.iter().map(String::as_str).collect();
            return api_error(&code, &ps);
        }
        let kind = match text(&given, "type").as_str() {
            "" => "snapshot".to_owned(),
            k @ ("snapshot" | "checkpoint" | "snapshot_with_quiesce") => k.to_owned(),
            other => return api_error("INVALID_VALUE", &["type", other]),
        };
        let (r, uuid) = self.new_ref("vmss");
        self.insert(
            "vmss",
            &r,
            json!({
                "uuid": uuid,
                "name_label": text(&given, "name_label"),
                "name_description": text(&given, "name_description"),
                "enabled": given.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                "type": kind,
                "retained_snapshots": retained,
                "frequency": frequency,
                "schedule": schedule,
                "last_run_time": "19700101T00:00:00Z",
                "VMs": [],
            }),
        );
        Ok(json!(r))
    }

    /// One run of `policy` (`scheduled`: by the clock).
    pub(crate) fn run_policy(&mut self, policy: &str, scheduled: bool) {
        let Some(rec) = self.record("vmss", policy).cloned() else {
            return;
        };
        self.vmss.runs.push((policy.to_owned(), scheduled));
        let name = text(&rec, "name_label");
        let kind = text(&rec, "type");
        let retained = int(&rec, "retained_snapshots").max(1);
        let local = self.vmss.now() + self.vmss.utc_offset;
        let when = stamp(local);
        let label = format!("{}{}", &when[..11], when[12..14].to_owned() + &when[15..17]);
        let mut failures = Vec::new();
        for vm in refs(&rec, "VMs") {
            let Some(vm_rec) = self.record("vm", &vm).cloned() else {
                continue;
            };
            let vm_name = text(&vm_rec, "name_label");
            let snapshot_name = format!("{vm_name}-{name}-{label}");
            let failing = self
                .vmss
                .failing
                .iter()
                .find(|(v, _)| *v == vm)
                .map(|(_, e)| e.clone());
            let result = match failing {
                Some(code) => Err((code, vec![vm.clone()])),
                None => {
                    let call = if kind == "checkpoint" {
                        "checkpoint"
                    } else {
                        "snapshot"
                    };
                    self.provisioning_call(
                        "VM",
                        call,
                        &[json!(""), json!(vm), json!(snapshot_name)],
                    )
                    .unwrap_or_else(|| api_error("MESSAGE_METHOD_UNKNOWN", &[call]))
                }
            };
            match result {
                Ok(snap) => {
                    let snap = snap.as_str().unwrap_or_default().to_owned();
                    let now = stamp(self.vmss.now());
                    self.modify("vm", &snap, |r| {
                        r.insert("is_vmss_snapshot".into(), json!(true));
                        r.insert("snapshot_time".into(), json!(format!("{now}Z")));
                    });
                    self.apply_retention(&vm, retained);
                }
                Err((code, ps)) => {
                    let mut items = vec![format!("'{code}'")];
                    items.extend(ps.iter().map(|p| format!("'{p}'")));
                    failures.push(format!(
                        "VM:{vm_name}UUID:{}Error:[{}]",
                        text(&vm_rec, "uuid"),
                        items.join(", ")
                    ));
                }
            }
        }
        let now = stamp(self.vmss.now());
        self.modify("vmss", policy, |r| {
            r.insert("last_run_time".into(), json!(format!("{now}Z")));
        });
        let uuid = text(&rec, "uuid");
        if failures.is_empty() {
            self.vmss_message("VMSS_SNAPSHOT_SUCCEEDED", 5, &uuid, "");
        } else {
            self.vmss_message("VMSS_SNAPSHOT_FAILED", 2, &uuid, &failures.join("\n"));
        }
    }

    /// Destroys the oldest scheduled snapshots of `vm` beyond `keep`.
    fn apply_retention(&mut self, vm: &str, keep: i64) {
        let mut scheduled: Vec<(String, String, u64)> = self
            .objects
            .iter()
            .filter(|((c, _), rec)| {
                c == "vm"
                    && rec.get("snapshot_of").and_then(Value::as_str) == Some(vm)
                    && rec.get("is_vmss_snapshot").and_then(Value::as_bool) == Some(true)
            })
            .map(|((_, r), rec)| {
                let time = rec
                    .get("snapshot_time")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let serial = r
                    .rsplit('-')
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                (r.clone(), time, serial)
            })
            .collect();
        scheduled.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
        let excess = scheduled.len().saturating_sub(keep.max(0) as usize);
        for (snap, _, _) in scheduled.into_iter().take(excess) {
            self.destroy_snapshot(&snap);
        }
    }

    /// Destroys a snapshot with its disks (`VM.destroy` keeps disks; a
    /// policy's clean-up does not).
    fn destroy_snapshot(&mut self, snap: &str) {
        let Some(rec) = self.record("vm", snap).cloned() else {
            return;
        };
        let disks: Vec<String> = refs(&rec, "VBDs")
            .iter()
            .filter_map(|vbd| self.record("vbd", vbd))
            .filter(|v| v.get("type").and_then(Value::as_str) != Some("CD"))
            .filter_map(|v| v.get("VDI").and_then(Value::as_str))
            .filter(|v| *v != NULL_REF)
            .map(str::to_owned)
            .collect();
        let _ = self.provisioning_call("VM", "destroy", &[json!(""), json!(snap)]);
        for vdi in disks {
            let _ = self.provisioning_call("VDI", "destroy", &[json!(""), json!(vdi)]);
        }
    }

    fn vmss_message(&mut self, name: &str, priority: i64, uuid: &str, body: &str) {
        let n = self.next_message;
        self.next_message += 1;
        let r = format!("OpaqueRef:mock-message-{n}");
        let record = json!({
            "uuid": format!("3e55a9e0-0000-4000-8000-{n:012}"), "name": name,
            "priority": priority, "cls": "VMSS", "obj_uuid": uuid,
            "timestamp": format!("{}Z", stamp(self.vmss.now())), "body": body,
        });
        self.objects
            .insert(("message".to_owned(), r.clone()), record.clone());
        self.push_event("message", "add", &r, Some(record));
    }

    /// Moves the clock `seconds` forward, running the enabled policies
    /// whose scheduled minutes it passes.
    pub(crate) fn advance_clock(&mut self, seconds: i64) {
        let start = self.vmss.now();
        let end = start + seconds.max(0);
        // Each whole minute after `start` up to `end`, in local time.
        let mut minute = (start.div_euclid(60) + 1) * 60;
        while minute <= end {
            self.vmss.advanced = minute - CLOCK_START;
            let local = minute + self.vmss.utc_offset;
            let due_now: Vec<String> = self
                .objects
                .iter()
                .filter(|((c, _), rec)| {
                    c == "vmss"
                        && rec.get("enabled").and_then(Value::as_bool) == Some(true)
                        && due(
                            rec.get("frequency")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            &schedule_of(rec.get("schedule")),
                            local,
                        )
                })
                .map(|((_, r), _)| r.clone())
                .collect();
            for policy in due_now {
                self.run_policy(&policy, true);
            }
            minute += 60;
        }
        self.vmss.advanced = end - CLOCK_START;
    }
}

/// `MockServer::advance_clock`.
pub(crate) fn advance(state: &Arc<State>, seconds: i64) {
    state.db().advance_clock(seconds);
    state.changed.notify_waiters();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::MockServer;
    use crate::migrate::tests::{call, login};

    #[test]
    fn schedule_checks_and_due_times() {
        let s = |v: Value| v.as_object().cloned().unwrap();
        assert!(check_schedule("hourly", &s(json!({"min": "15"}))).is_ok());
        assert!(check_schedule("hourly", &s(json!({"min": "10"}))).is_err());
        assert!(check_schedule("daily", &s(json!({"min": "0"}))).is_err());
        assert!(
            check_schedule(
                "weekly",
                &s(json!({"min": "0", "hour": "3", "days": "monday,Friday"}))
            )
            .is_ok()
        );
        assert!(
            check_schedule(
                "weekly",
                &s(json!({"min": "0", "hour": "3", "days": "funday"}))
            )
            .is_err()
        );
        assert_eq!(weekday(CLOCK_START), 3);
        assert_eq!(stamp(CLOCK_START), "20261125T12:00:00");
        let weekly = s(json!({"min": "30", "hour": "12", "days": "wednesday"}));
        assert!(due("weekly", &weekly, CLOCK_START + 30 * 60));
        assert!(!due("weekly", &weekly, CLOCK_START + 30 * 60 + 86_400));
    }

    #[tokio::test]
    async fn policies_snapshot_on_schedule_and_keep_the_retention() {
        let m = MockServer::start(false).await;
        let s = login(&m).await;
        let vm = "OpaqueRef:vm-web";
        let r = call(
            &m,
            &s,
            "VMSS.create",
            &[
                json!({"name_label": "p", "enabled": true, "type": "snapshot",
                     "retained_snapshots": 11, "frequency": "hourly", "schedule": {"min": "15"}}),
            ],
        )
        .await;
        assert_eq!(r.unwrap_err().0, "INVALID_VALUE");
        let p = call(
            &m,
            &s,
            "VMSS.create",
            &[
                json!({"name_label": "p", "enabled": true, "type": "snapshot",
                     "retained_snapshots": 2, "frequency": "hourly", "schedule": {"min": "15"}}),
            ],
        )
        .await
        .unwrap();
        let p = p.as_str().unwrap().to_owned();
        call(&m, &s, "VM.set_snapshot_schedule", &[json!(vm), json!(p)])
            .await
            .unwrap();
        assert_eq!(m.field("vmss", &p, "VMs").unwrap(), json!([vm]));
        let r = call(&m, &s, "VMSS.destroy", &[json!(p)]).await;
        assert_eq!(r.unwrap_err().0, "VMSS_HAS_VM");
        // Three hours: three runs, two snapshots kept.
        m.advance_clock(3 * 3600);
        assert_eq!(m.vmss_runs().len(), 3);
        let snaps: Vec<String> = m
            .refs("vm")
            .into_iter()
            .filter(|r| m.field("vm", r, "snapshot_of") == Some(json!(vm)))
            .filter(|r| m.field("vm", r, "is_vmss_snapshot") == Some(json!(true)))
            .collect();
        assert_eq!(snaps.len(), 2);
        // Disabled: no more runs.
        call(&m, &s, "VMSS.set_enabled", &[json!(p), json!(false)])
            .await
            .unwrap();
        m.advance_clock(3 * 3600);
        assert_eq!(m.vmss_runs().len(), 3);
        call(&m, &s, "VMSS.set_frequency", &[json!(p), json!("daily")])
            .await
            .unwrap();
        assert_eq!(m.field("vmss", &p, "schedule").unwrap(), json!({}));
        call(
            &m,
            &s,
            "VM.set_snapshot_schedule",
            &[json!(vm), json!(crate::NULL_REF)],
        )
        .await
        .unwrap();
        call(&m, &s, "VMSS.destroy", &[json!(p)]).await.unwrap();
        let ok = m
            .refs("message")
            .into_iter()
            .filter(|r| m.field("message", r, "name") == Some(json!("VMSS_SNAPSHOT_SUCCEEDED")))
            .count();
        assert_eq!(ok, 3);
    }
}
