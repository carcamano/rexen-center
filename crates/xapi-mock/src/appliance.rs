//! vApps (`VM_appliance`) in the mock.
//!
//! **Records.** `VM_appliance.create(record)` makes a vApp (name and
//! description from the record, no VMs); `destroy` removes it, its VMs
//! leaving it. `VM.set_appliance(vm, vApp or OpaqueRef:NULL)` moves a VM in
//! or out (`HANDLE_INVALID` for an unknown VM or vApp), and `VM.create`
//! accepts `appliance`, `order`, `start_delay` and `shutdown_delay`. A
//! vApp's `VMs` and `allowed_operations` follow its members, as XAPI's:
//! `start` while a member is halted; `clean_shutdown`, `hard_shutdown` and
//! `shutdown` while one runs; nothing without members. `VM.set_order`,
//! `set_start_delay` (see [`crate::ha`]) and `set_shutdown_delay` set the
//! startup sequence.
//!
//! **Start and shut down.** `VM_appliance.start(vApp, paused)` starts the
//! halted members group by group, by `order` ascending, and waits the
//! group's longest `start_delay` before the next group;
//! `shutdown` / `clean_shutdown` / `hard_shutdown` stop the running members
//! by `order` descending with `shutdown_delay` (`shutdown` shuts a VM down
//! cleanly when it can, forcibly otherwise). A second of delay lasts
//! [`crate::MockServer::set_appliance_delay_unit`] (none by default). An
//! operation the vApp does not offer fails with `OPERATION_NOT_ALLOWED`, a
//! member that cannot start or stop fails the call with its error. Every
//! step is recorded ([`crate::MockServer::appliance_log`]): `("start", vm)`
//! and `("shutdown", vm)` per member, `("delay", seconds)` between groups
//! (only a delay above zero). The `Async.*` forms are tasks whose progress
//! follows the groups; cancelling one stops it between groups (the members
//! already started stay running).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::provisioning::int;
use crate::{Db, NULL_REF, Reply, State, api_error, str_param};

type Error = (String, Vec<String>);

/// vApp knobs and records of one mock.
#[derive(Default)]
pub(crate) struct World {
    /// How long one second of a start or shutdown delay lasts.
    pub(crate) delay_unit: Duration,
    /// What the vApp operations did, in order.
    pub(crate) log: Vec<(String, String)>,
}

/// The record calls handled through [`crate::provisioning`] (their `Async`
/// forms are plain tasks).
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        ("VM_appliance", "create" | "destroy") | ("VM", "set_appliance" | "set_shutdown_delay")
    )
}

/// The calls that start or stop members ([`run`]).
pub(crate) fn runs(class: &str, call: &str) -> bool {
    class == "VM_appliance"
        && matches!(
            call,
            "start" | "shutdown" | "clean_shutdown" | "hard_shutdown"
        )
}

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

impl Db {
    /// The calls of [`handles`].
    pub(crate) fn appliance_call(
        &mut self,
        class: &str,
        call: &str,
        params: &[Value],
    ) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i).to_owned();
        Some(match (class, call) {
            ("VM_appliance", "create") => {
                let given = params
                    .get(1)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let (r, uuid) = self.new_ref("vm_appliance");
                self.insert(
                    "vm_appliance",
                    &r,
                    json!({
                        "uuid": uuid,
                        "name_label": text(&given, "name_label"),
                        "name_description": text(&given, "name_description"),
                        "allowed_operations": [],
                        "current_operations": {},
                        "VMs": [],
                    }),
                );
                Ok(json!(r))
            }
            ("VM_appliance", "destroy") => {
                let r = p(1);
                let Some(rec) = self.record("vm_appliance", &r).cloned() else {
                    return Some(api_error("HANDLE_INVALID", &["VM_appliance", &r]));
                };
                for vm in refs(&rec, "VMs") {
                    self.modify("vm", &vm, |v| {
                        v.insert("appliance".into(), json!(NULL_REF));
                    });
                }
                self.remove("vm_appliance", &r);
                Ok(json!(""))
            }
            ("VM", "set_appliance") => {
                let (vm, app) = (p(1), p(2));
                let app = if app.is_empty() {
                    NULL_REF.to_owned()
                } else {
                    app
                };
                if !self.exists("vm", &vm) {
                    return Some(api_error("HANDLE_INVALID", &["VM", &vm]));
                }
                if app != NULL_REF && !self.exists("vm_appliance", &app) {
                    return Some(api_error("HANDLE_INVALID", &["VM_appliance", &app]));
                }
                self.assign_appliance(&vm, &app);
                Ok(json!(""))
            }
            _ => {
                // `VM.set_shutdown_delay`.
                let vm = p(1);
                if !self.exists("vm", &vm) {
                    return Some(api_error("HANDLE_INVALID", &["VM", &vm]));
                }
                let value = crate::provisioning::int_param(params, 2).unwrap_or(0);
                self.modify("vm", &vm, |rec| {
                    rec.insert("shutdown_delay".into(), json!(value));
                });
                Ok(json!(""))
            }
        })
    }

    /// Puts `vm` in vApp `app` (`OpaqueRef:NULL`: in none).
    pub(crate) fn assign_appliance(&mut self, vm: &str, app: &str) {
        let old = self.get_str("vm", vm, "appliance").unwrap_or_default();
        if !old.is_empty() && old != NULL_REF && old != app {
            self.drop_ref("vm_appliance", &old, "VMs", vm);
            self.refresh_appliance(&old);
        }
        self.modify("vm", vm, |v| {
            v.insert("appliance".into(), json!(app));
        });
        if app != NULL_REF {
            self.push_ref("vm_appliance", app, "VMs", vm);
            self.refresh_appliance(app);
        }
    }

    /// Recomputes what vApp `app` allows from its members' power states.
    pub(crate) fn refresh_appliance(&mut self, app: &str) {
        let Some(rec) = self.record("vm_appliance", app) else {
            return;
        };
        let states: Vec<String> = refs(rec, "VMs")
            .iter()
            .map(|vm| self.vm_power(vm))
            .collect();
        let mut ops: Vec<&str> = Vec::new();
        if states.iter().any(|s| s == "Halted") {
            ops.push("start");
        }
        if states.iter().any(|s| s == "Running") {
            ops.extend(["clean_shutdown", "hard_shutdown", "shutdown"]);
        }
        let current = rec.get("allowed_operations").cloned();
        if current != Some(json!(ops)) {
            self.modify("vm_appliance", app, |r| {
                r.insert("allowed_operations".into(), json!(ops));
            });
        }
    }

    /// Refreshes the vApp of `vm`, if it is in one.
    pub(crate) fn refresh_vm_appliance(&mut self, vm: &str) {
        if let Some(app) = self.get_str("vm", vm, "appliance")
            && app != NULL_REF
        {
            self.refresh_appliance(&app);
        }
    }

    /// The members of `app` an operation acts on, by group: `(members,
    /// delay after the group)`.
    fn appliance_groups(&self, app: &str, start: bool) -> Vec<(Vec<String>, i64)> {
        let members = self
            .record("vm_appliance", app)
            .map(|r| refs(r, "VMs"))
            .unwrap_or_default();
        let mut groups: BTreeMap<i64, (Vec<String>, i64)> = BTreeMap::new();
        for vm in members {
            let Some(rec) = self.record("vm", &vm) else {
                continue;
            };
            let power = text(rec, "power_state");
            let wanted = if start {
                power == "Halted"
            } else {
                power == "Running" || power == "Paused"
            };
            if !wanted {
                continue;
            }
            let delay = int(
                rec,
                if start {
                    "start_delay"
                } else {
                    "shutdown_delay"
                },
            );
            let group = groups.entry(int(rec, "order")).or_default();
            group.0.push(vm);
            group.1 = group.1.max(delay);
        }
        let mut out: Vec<(Vec<String>, i64)> = groups.into_values().collect();
        if !start {
            out.reverse();
        }
        out
    }

    /// Starts or stops one member; returns its power state before.
    fn appliance_step(&mut self, vm: &str, call: &str, paused: bool) -> Result<String, Error> {
        let before = self.vm_power(vm);
        let op = match call {
            "start" => "start",
            "clean_shutdown" => "clean_shutdown",
            "hard_shutdown" => "hard_shutdown",
            // `shutdown`: cleanly when the VM can.
            _ => {
                let clean = self
                    .record("vm", vm)
                    .and_then(|r| r.get("allowed_operations"))
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|o| o.as_str() == Some("clean_shutdown")));
                if clean {
                    "clean_shutdown"
                } else {
                    "hard_shutdown"
                }
            }
        };
        let work = self.check_vm_op(op, &[json!(""), json!(vm)])?;
        self.apply(&work);
        if op == "start" && paused {
            self.move_vm(vm, "Paused", None);
        }
        let entry = if op == "start" { "start" } else { "shutdown" };
        self.appliance.log.push((entry.to_owned(), vm.to_owned()));
        Ok(before)
    }
}

/// Runs `VM_appliance.start` / `shutdown` / `clean_shutdown` /
/// `hard_shutdown` (`failure`: the error [`crate::MockServer::fail_method`]
/// set for it).
pub(crate) async fn run(
    state: &Arc<State>,
    method: &str,
    params: &[Value],
    failure: Option<Error>,
) -> Reply {
    let is_async = method.starts_with("Async.");
    let call = method
        .trim_start_matches("Async.")
        .trim_start_matches("VM_appliance.")
        .to_owned();
    let app = str_param(params, 1).to_owned();
    let paused = call == "start" && params.get(2).and_then(Value::as_bool) == Some(true);
    let checked = match failure {
        Some(e) => Err(e),
        None => {
            let db = state.db();
            match db.record("vm_appliance", &app) {
                None => Err((
                    "HANDLE_INVALID".to_owned(),
                    vec!["VM_appliance".to_owned(), app.clone()],
                )),
                Some(rec) => {
                    let allowed = rec
                        .get("allowed_operations")
                        .and_then(Value::as_array)
                        .is_some_and(|a| a.iter().any(|o| o.as_str() == Some(call.as_str())));
                    if allowed {
                        Ok(())
                    } else {
                        Err(("OPERATION_NOT_ALLOWED".to_owned(), vec![call.clone()]))
                    }
                }
            }
        }
    };
    if !is_async {
        let reply = match checked {
            Err(e) => Err(e),
            Ok(()) => run_groups(state, &app, &call, paused, None)
                .await
                .map(|()| json!("")),
        };
        state.changed.notify_waiters();
        return reply;
    }
    let task = crate::create_task(state, method);
    let state = state.clone();
    let task_ref = task.clone();
    let runtime = state.runtime.clone();
    runtime.spawn(async move {
        let step = state.db().task_step;
        tokio::time::sleep(step).await;
        let outcome = match checked {
            Err(e) => Err(e),
            Ok(()) => run_groups(&state, &app, &call, paused, Some(&task_ref)).await,
        };
        let mut db = state.db();
        if db.get_str("task", &task_ref, "status").as_deref() == Some("pending") {
            match outcome {
                Ok(()) => db.finish_task(&task_ref, "success", "", &[]),
                Err((code, params)) => {
                    let mut info = vec![code.as_str()];
                    info.extend(params.iter().map(String::as_str));
                    db.finish_task(&task_ref, "failure", "", &info);
                }
            }
        }
        drop(db);
        state.changed.notify_waiters();
    });
    Ok(json!(task))
}

/// Starts or stops the members group by group, waiting between groups.
/// With `task`, stops (successfully) once it is no longer pending.
async fn run_groups(
    state: &Arc<State>,
    app: &str,
    call: &str,
    paused: bool,
    task: Option<&str>,
) -> Result<(), Error> {
    let start = call == "start";
    let groups = state.db().appliance_groups(app, start);
    let count = groups.len().max(1);
    for (i, (members, delay)) in groups.into_iter().enumerate() {
        let mut changed = Vec::new();
        {
            let mut db = state.db();
            if let Some(t) = task
                && db.get_str("task", t, "status").as_deref() != Some("pending")
            {
                return Ok(());
            }
            for vm in &members {
                let before = db.appliance_step(vm, call, paused)?;
                let after = db.vm_power(vm);
                changed.push((vm.clone(), before, after));
            }
            if let Some(t) = task {
                let progress = (i + 1) as f64 / count as f64;
                db.modify("task", t, |rec| {
                    rec.insert("progress".into(), json!(progress.min(0.99)));
                });
            }
            if i + 1 < count && delay > 0 {
                db.appliance
                    .log
                    .push(("delay".to_owned(), delay.to_string()));
            }
        }
        for (vm, before, after) in changed {
            state.qemu_restarted(&vm, &before, &after);
        }
        state.changed.notify_waiters();
        if i + 1 < count && delay > 0 {
            let unit = state.db().appliance.delay_unit;
            tokio::time::sleep(unit * u32::try_from(delay).unwrap_or(u32::MAX)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use crate::MockServer;
    use crate::migrate::tests::{call, login, wait_task};

    fn ops(m: &MockServer, app: &str) -> Vec<String> {
        m.field("vm_appliance", app, "allowed_operations")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    #[tokio::test]
    async fn vapps_start_and_stop_in_order() {
        let m = MockServer::start(false).await;
        m.set_task_timing(2, Duration::from_millis(10));
        let s = login(&m).await;
        let app = call(
            &m,
            &s,
            "VM_appliance.create",
            &[json!({"name_label": "stack", "name_description": "d"})],
        )
        .await
        .unwrap();
        let app = app.as_str().unwrap().to_owned();
        assert!(ops(&m, &app).is_empty());
        let r = call(&m, &s, "VM_appliance.start", &[json!(app), json!(false)]).await;
        assert_eq!(r.unwrap_err().0, "OPERATION_NOT_ALLOWED");
        // db first (order 0, waits 3 s), then build (order 1).
        let (db, build) = ("OpaqueRef:vm-db", "OpaqueRef:vm-build");
        for (vm, order, delay) in [(build, 1, 0), (db, 0, 3)] {
            call(&m, &s, "VM.set_appliance", &[json!(vm), json!(app)])
                .await
                .unwrap();
            call(&m, &s, "VM.set_order", &[json!(vm), json!(order)])
                .await
                .unwrap();
            call(&m, &s, "VM.set_start_delay", &[json!(vm), json!(delay)])
                .await
                .unwrap();
            call(
                &m,
                &s,
                "VM.set_shutdown_delay",
                &[json!(vm), json!(order * 2)],
            )
            .await
            .unwrap();
        }
        assert_eq!(m.field("vm", db, "appliance").unwrap(), json!(app));
        assert_eq!(
            m.field("vm_appliance", &app, "VMs")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(ops(&m, &app), vec!["start"]);
        m.set_appliance_delay_unit(Duration::from_millis(10));
        let task = call(
            &m,
            &s,
            "Async.VM_appliance.start",
            &[json!(app), json!(false)],
        )
        .await
        .unwrap();
        let (status, _) = wait_task(&m, task.as_str().unwrap()).await;
        assert_eq!(status, "success");
        let log: Vec<(String, String)> = m.appliance_log();
        let want: Vec<(String, String)> = [("start", db), ("delay", "3"), ("start", build)]
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
        assert_eq!(log, want);
        assert_eq!(m.vm_power_state(db), "Running");
        assert_eq!(
            ops(&m, &app),
            vec!["clean_shutdown", "hard_shutdown", "shutdown"]
        );
        call(&m, &s, "VM_appliance.shutdown", &[json!(app)])
            .await
            .unwrap();
        let log = m.appliance_log();
        let tail: Vec<(&str, &str)> = log[3..]
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            tail,
            vec![("shutdown", build), ("delay", "2"), ("shutdown", db)]
        );
        assert_eq!(m.vm_power_state(build), "Halted");
        assert_eq!(ops(&m, &app), vec!["start"]);
        // Out of the vApp, then the vApp goes.
        call(
            &m,
            &s,
            "VM.set_appliance",
            &[json!(build), json!(crate::NULL_REF)],
        )
        .await
        .unwrap();
        assert_eq!(m.field("vm_appliance", &app, "VMs").unwrap(), json!([db]));
        call(&m, &s, "VM_appliance.destroy", &[json!(app)])
            .await
            .unwrap();
        assert!(m.record("vm_appliance", &app).is_none());
        assert_eq!(
            m.field("vm", db, "appliance").unwrap(),
            json!(crate::NULL_REF)
        );
        // VM.create puts a VM in a vApp.
        let app2 = call(
            &m,
            &s,
            "VM_appliance.create",
            &[json!({"name_label": "imported"})],
        )
        .await
        .unwrap();
        let vm = call(
            &m,
            &s,
            "VM.create",
            &[json!({
                "name_label": "x", "memory_static_max": 1024, "memory_dynamic_max": 1024,
                "memory_dynamic_min": 1024, "memory_static_min": 1024, "VCPUs_max": 1,
                "VCPUs_at_startup": 1, "appliance": app2, "order": 2, "start_delay": 5,
            })],
        )
        .await
        .unwrap();
        assert_eq!(
            m.field("vm", vm.as_str().unwrap(), "order").unwrap(),
            json!(2)
        );
        assert_eq!(
            m.field("vm_appliance", app2.as_str().unwrap(), "VMs")
                .unwrap(),
            json!([vm])
        );
    }
}
