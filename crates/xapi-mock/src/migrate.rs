//! VM migration with storage motion, within a pool and between pools
//! (`VM.migrate_send`), as the Migrate, Move and Copy wizards use it.
//!
//! **Calls.**
//!
//! * `host.migrate_receive(host, network, options)` on the destination:
//!   `HANDLE_INVALID` for an unknown host or network; otherwise the
//!   destination map XAPI hands to the source: `session_id` (a new session
//!   on this server), `host`, `network`, `master` (`https://<address>/`,
//!   the address this mock answers at), `SM` and `xenops` (service URLs
//!   carrying the session).
//! * `VM.assert_can_migrate(vm, dest, live, vdi_map, vif_map, options)` on
//!   the source, and the same checks at the start of `VM.migrate_send`:
//!   `HANDLE_INVALID` (VM); `CANNOT_CONTACT_HOST [master]` when no mock
//!   answers at `dest["master"]`; the error set for that destination host
//!   by [`crate::MockServer::refuse_migration_to`] (on either server);
//!   `VM_IS_TEMPLATE [vm]` for a template unless `options["copy"]` is
//!   `"true"`; `VM_BAD_POWER_STATE [vm, "halted", state]` for a VM neither
//!   running, halted nor suspended; `OPERATION_NOT_ALLOWED ["migrate_send"]`
//!   when the VM does not offer it; `SESSION_INVALID [session]` when
//!   `dest["session_id"]` is not a session of the destination;
//!   `HANDLE_INVALID` for an unknown destination host, mapped SR or
//!   network; between pools, `VDI_NOT_IN_MAP [vdi]` / `VIF_NOT_IN_MAP [vif]`
//!   for a disk or interface of the VM left out of the maps (within a pool
//!   unmapped disks stay where they are); `HOST_NOT_ENOUGH_FREE_MEMORY
//!   [needed, available]` when a running VM's `memory_dynamic_max` is more
//!   than the destination host's `host_metrics.memory_free`.
//! * `VM.migrate_send` / `Async.VM.migrate_send` (same arguments) returns the
//!   VM on the destination. The `Async` form is a task that advances like
//!   the others, marks the VM busy while it runs, can be cancelled
//!   (nothing changes) and fails as [`crate::MockServer::fail_method`]
//!   says (the VM is left as it was). At its end:
//!   * **between pools, or a copy** (`options["copy"] = "true"`): the
//!     destination gets the VM (a move keeps its uuid, a copy gets a new
//!     one and is halted), its drives, a new disk on the mapped SR for each
//!     mapped disk (name and sizes kept, the content the mock keeps for it
//!     too), its interfaces on the mapped networks (MAC and device kept),
//!     its metrics, and those of its snapshots whose disks are all mapped.
//!     A running VM runs on the destination host. A move then removes the
//!     VM, its drives, interfaces, moved disks and moved snapshots from the
//!     source; a copy leaves the source as it was;
//!   * **within the pool**: the VM stays; each mapped disk moves to its new
//!     SR (a new disk, the drive pointing at it, the old one destroyed),
//!     each mapped interface to its new network, and a running VM to the
//!     destination host.
//!
//!   The session `host.migrate_receive` made is closed once
//!   `migrate_send` ends. Two databases are never locked at once.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::provisioning::int;
use crate::{Db, NULL_REF, Reply, State, api_error, str_param};

type Error = (String, Vec<String>);

/// Migration knobs of one mock.
#[derive(Default)]
pub(crate) struct World {
    /// Destination host → the error a migration there fails with.
    pub(crate) refusals: HashMap<String, Error>,
}

/// Whether `class.call` (without `Async.`) is handled here.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        ("host", "migrate_receive") | ("VM", "assert_can_migrate") | ("VM", "migrate_send")
    )
}

fn err(code: &str, params: &[&str]) -> Error {
    (
        code.to_owned(),
        params.iter().map(|s| (*s).to_owned()).collect(),
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

/// A `{ref: ref}` map parameter as pairs.
fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Runs a call of [`handles`] (`failure`: the error
/// [`crate::MockServer::fail_method`] set for it).
pub(crate) async fn call(
    state: &Arc<State>,
    method: &str,
    params: &[Value],
    failure: Option<Error>,
) -> Reply {
    let is_async = method.starts_with("Async.");
    let reply = match method.trim_start_matches("Async.") {
        "host.migrate_receive" if !is_async => receive(state, params),
        "VM.assert_can_migrate" if !is_async => plan(state, params, false).map(|_| json!("")),
        "VM.migrate_send" if is_async => start_async(state, method, params, failure),
        "VM.migrate_send" => plan(state, params, false).map(|p| json!(perform(state, &p))),
        _ => api_error("MESSAGE_METHOD_UNKNOWN", &[method]),
    };
    state.changed.notify_waiters();
    reply
}

/// `host.migrate_receive`.
fn receive(state: &Arc<State>, params: &[Value]) -> Reply {
    let (host, network) = (str_param(params, 1), str_param(params, 2));
    let mut db = state.db();
    if !db.exists("host", host) {
        return api_error("HANDLE_INVALID", &["host", host]);
    }
    if !db.exists("network", network) {
        return api_error("HANDLE_INVALID", &["network", network]);
    }
    let session = format!("OpaqueRef:mock-session-{}", db.next_session);
    db.next_session += 1;
    db.sessions
        .insert(session.clone(), crate::ops::MockSession::superuser());
    let address = db.pool.authority.clone();
    Ok(json!({
        "session_id": session,
        "host": host,
        "network": network,
        "master": format!("https://{address}/"),
        "SM": format!("http://{address}/services/SM?session_id={session}"),
        "xenops": format!("http://{address}/services/xenops?session_id={session}"),
    }))
}

/// A checked migration.
struct Plan {
    vm: String,
    /// The destination server (maybe this one).
    target: Arc<State>,
    /// The destination is this server's pool.
    same: bool,
    host: String,
    session: String,
    vdi_map: Vec<(String, String)>,
    vif_map: Vec<(String, String)>,
    copy: bool,
}

/// The checks of `VM.assert_can_migrate`. With `busy_ok` the VM may be
/// busy with this very migration (the end of an `Async` one).
fn plan(state: &Arc<State>, params: &[Value], busy_ok: bool) -> Result<Plan, Error> {
    let vm = str_param(params, 1).to_owned();
    let dest = params
        .get(2)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let vdi_map = pairs(params.get(4));
    let vif_map = pairs(params.get(5));
    let copy = params
        .get(6)
        .and_then(|o| o.get("copy"))
        .and_then(Value::as_str)
        == Some("true");
    let (master, host, session) = (
        text(&dest, "master"),
        text(&dest, "host"),
        text(&dest, "session_id"),
    );

    // The source's side.
    let (power, template, allowed, resident, memory, disks, vifs, refusal) = {
        let db = state.db();
        let Some(rec) = db.record("vm", &vm) else {
            return Err(err("HANDLE_INVALID", &["VM", &vm]));
        };
        let flag = |f: &str| rec.get(f).and_then(Value::as_bool).unwrap_or(false);
        let allowed = rec
            .get("allowed_operations")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|o| o.as_str() == Some("migrate_send")));
        (
            text(rec, "power_state"),
            flag("is_a_template"),
            allowed,
            text(rec, "resident_on"),
            int(rec, "memory_dynamic_max"),
            db.vm_disks(&vm),
            refs(rec, "VIFs"),
            db.migrate.refusals.get(&host).cloned(),
        )
    };
    let address = master.trim().trim_end_matches('/');
    let Some(target) = crate::pool::lookup(address) else {
        return Err(err("CANNOT_CONTACT_HOST", &[&master]));
    };
    let same = Arc::ptr_eq(&target, state);
    if let Some(e) = refusal {
        return Err(e);
    }
    if template && !copy {
        return Err(err("VM_IS_TEMPLATE", &[&vm]));
    }
    if !matches!(power.as_str(), "Running" | "Halted" | "Suspended") {
        return Err(err(
            "VM_BAD_POWER_STATE",
            &[&vm, "halted", &power.to_lowercase()],
        ));
    }
    if !template && !busy_ok && !allowed {
        return Err(err("OPERATION_NOT_ALLOWED", &["migrate_send"]));
    }

    // The destination's side.
    {
        let db = target.db();
        if let Some(e) = db.migrate.refusals.get(&host) {
            return Err(e.clone());
        }
        if !db.sessions.contains_key(&session) {
            return Err(err("SESSION_INVALID", &[&session]));
        }
        if !db.exists("host", &host) {
            return Err(err("HANDLE_INVALID", &["host", &host]));
        }
        for (_, sr) in &vdi_map {
            if !db.exists("sr", sr) {
                return Err(err("HANDLE_INVALID", &["SR", sr]));
            }
        }
        for (_, net) in &vif_map {
            if !db.exists("network", net) {
                return Err(err("HANDLE_INVALID", &["network", net]));
            }
        }
        if !same {
            if let Some(d) = disks.iter().find(|d| !vdi_map.iter().any(|(v, _)| v == *d)) {
                return Err(err("VDI_NOT_IN_MAP", &[d]));
            }
            if let Some(v) = vifs.iter().find(|v| !vif_map.iter().any(|(f, _)| f == *v)) {
                return Err(err("VIF_NOT_IN_MAP", &[v]));
            }
        }
        let moves_host = !(same && resident == host);
        if power == "Running" && !copy && moves_host {
            let free = db
                .get_str("host", &host, "metrics")
                .and_then(|m| db.record("host_metrics", &m).map(|r| int(r, "memory_free")))
                .unwrap_or(0);
            if memory > free {
                return Err(err(
                    "HOST_NOT_ENOUGH_FREE_MEMORY",
                    &[&memory.to_string(), &free.to_string()],
                ));
            }
        }
    }
    Ok(Plan {
        vm,
        target,
        same,
        host,
        session,
        vdi_map,
        vif_map,
        copy,
    })
}

/// Does a checked migration; returns the VM on the destination.
fn perform(state: &Arc<State>, plan: &Plan) -> String {
    let before = state.db().vm_power(&plan.vm);
    let vm = if plan.same && !plan.copy {
        in_place(state, plan)
    } else {
        transfer(state, plan)
    };
    if !plan.same && !plan.copy {
        // The source's QEMU is gone.
        state.qemu_restarted(&plan.vm, &before, "Halted");
    }
    plan.target.db().sessions.remove(&plan.session);
    plan.target.changed.notify_waiters();
    state.changed.notify_waiters();
    vm
}

/// Storage motion within the pool: the VM's mapped disks and interfaces
/// move, and a running VM goes to the destination host.
fn in_place(state: &Arc<State>, plan: &Plan) -> String {
    let mut db = state.db();
    let vm = plan.vm.clone();
    for (vdi, sr) in &plan.vdi_map {
        let on = db.get_str("vdi", vdi, "SR");
        if on.is_some() && on.as_deref() != Some(sr.as_str()) {
            db.relocate_vdi(vdi, sr);
        }
    }
    for (vif, net) in &plan.vif_map {
        let Some(old) = db.get_str("vif", vif, "network") else {
            continue;
        };
        if &old != net {
            db.drop_ref("network", &old, "VIFs", vif);
            db.modify("vif", vif, |r| {
                r.insert("network".into(), json!(net));
            });
            db.push_ref("network", net, "VIFs", vif);
        }
    }
    let power = db.vm_power(&vm);
    let resident = db.get_str("vm", &vm, "resident_on").unwrap_or_default();
    if matches!(power.as_str(), "Running" | "Paused") && resident != plan.host {
        db.move_vm(&vm, &power, Some(plan.host.clone()));
    }
    db.settle_migrated(&vm);
    vm
}

/// One VM to carry over: its record, drives and interfaces.
struct Carried {
    vm: String,
    rec: Map<String, Value>,
    vbds: Vec<Map<String, Value>>,
    vifs: Vec<(String, Map<String, Value>)>,
    metrics: Option<Map<String, Value>>,
}

/// One disk to carry over.
struct CarriedDisk {
    vdi: String,
    rec: Map<String, Value>,
    sr: String,
    content: Option<crate::vhd::Content>,
    digest: Option<String>,
}

/// A copy, or a move to another pool.
fn transfer(state: &Arc<State>, plan: &Plan) -> String {
    let vdi_sr: HashMap<&str, &str> = plan
        .vdi_map
        .iter()
        .map(|(v, s)| (v.as_str(), s.as_str()))
        .collect();
    let vif_net: HashMap<&str, &str> = plan
        .vif_map
        .iter()
        .map(|(v, n)| (v.as_str(), n.as_str()))
        .collect();

    // Gather what goes, under the source's lock.
    let (vms, disks) = {
        let db = state.db();
        // Within the pool, a copy's unmapped disks are copied in their own SR.
        let sr_of = |vdi: &str| -> Option<String> {
            match vdi_sr.get(vdi) {
                Some(sr) => Some((*sr).to_owned()),
                None if plan.same => db.get_str("vdi", vdi, "SR"),
                None => None,
            }
        };
        let carry = |vm: &str| -> Option<Carried> {
            let rec = db.record("vm", vm)?.clone();
            let vbds = refs(&rec, "VBDs")
                .iter()
                .filter_map(|b| db.record("vbd", b).cloned())
                .collect();
            let vifs = refs(&rec, "VIFs")
                .into_iter()
                .filter_map(|f| Some((f.clone(), db.record("vif", &f)?.clone())))
                .collect();
            let metrics = db.record("vm_metrics", &text(&rec, "metrics")).cloned();
            Some(Carried {
                vm: vm.to_owned(),
                rec,
                vbds,
                vifs,
                metrics,
            })
        };
        let mut vms = Vec::new();
        if let Some(main) = carry(&plan.vm) {
            let snapshots = refs(&main.rec, "snapshots");
            vms.push(main);
            for snap in snapshots {
                let all_mapped = db.vm_disks(&snap).iter().all(|d| sr_of(d).is_some());
                if all_mapped && let Some(c) = carry(&snap) {
                    vms.push(c);
                }
            }
        }
        let mut seen = HashSet::new();
        let mut disks = Vec::new();
        for c in &vms {
            for d in db.vm_disks(&c.vm) {
                if !seen.insert(d.clone()) {
                    continue;
                }
                let (Some(sr), Some(rec)) = (sr_of(&d), db.record("vdi", &d).cloned()) else {
                    continue;
                };
                disks.push(CarriedDisk {
                    content: db.xva.contents.get(&d).cloned(),
                    digest: db.xva.digests.get(&d).cloned(),
                    vdi: d,
                    rec,
                    sr,
                });
            }
        }
        (vms, disks)
    };

    // Build it on the destination.
    let new_main = {
        let mut db = plan.target.db();
        let mut vdi_new: HashMap<String, String> = HashMap::new();
        let mut vdi_uuid: HashMap<String, String> = HashMap::new();
        for d in &disks {
            let (r, uuid) = db.new_ref("vdi");
            vdi_new.insert(d.vdi.clone(), r);
            vdi_uuid.insert(d.vdi.clone(), uuid);
        }
        for d in &disks {
            let r = vdi_new[&d.vdi].clone();
            let mut rec = d.rec.clone();
            let uuid = vdi_uuid[&d.vdi].clone();
            rec.insert("uuid".into(), json!(uuid));
            rec.insert("location".into(), json!(uuid));
            rec.insert("SR".into(), json!(d.sr));
            rec.insert("VBDs".into(), json!([]));
            rec.insert("current_operations".into(), json!({}));
            let parent = text(&d.rec, "snapshot_of");
            rec.insert(
                "snapshot_of".into(),
                json!(
                    vdi_new
                        .get(&parent)
                        .cloned()
                        .unwrap_or_else(|| NULL_REF.into())
                ),
            );
            let snaps: Vec<String> = refs(&d.rec, "snapshots")
                .iter()
                .filter_map(|s| vdi_new.get(s).cloned())
                .collect();
            rec.insert("snapshots".into(), json!(snaps));
            let size = int(&rec, "virtual_size");
            db.insert("vdi", &r, Value::Object(rec));
            db.push_ref("sr", &d.sr, "VDIs", &r);
            db.modify("sr", &d.sr, |s| {
                let alloc = int(s, "virtual_allocation") + size;
                s.insert("virtual_allocation".into(), json!(alloc));
            });
            if let Some(c) = &d.content {
                db.xva.contents.insert(r.clone(), c.clone());
            }
            if let Some(g) = &d.digest {
                db.xva.digests.insert(r.clone(), g.clone());
            }
        }
        let mut vm_new: HashMap<String, String> = HashMap::new();
        let mut vm_uuid: HashMap<String, String> = HashMap::new();
        for c in &vms {
            let (r, uuid) = db.new_ref("vm");
            vm_new.insert(c.vm.clone(), r);
            vm_uuid.insert(c.vm.clone(), uuid);
        }
        for c in &vms {
            let r = vm_new[&c.vm].clone();
            let mut rec = c.rec.clone();
            if plan.copy {
                rec.insert("uuid".into(), json!(vm_uuid[&c.vm]));
            }
            let is_main = c.vm == plan.vm;
            let power = text(&rec, "power_state");
            let power = if is_main && plan.copy {
                "Halted".to_owned()
            } else {
                power
            };
            for (k, v) in [
                ("power_state", json!("Halted")),
                ("resident_on", json!(NULL_REF)),
                ("affinity", json!(NULL_REF)),
                ("appliance", json!(NULL_REF)),
                ("guest_metrics", json!(NULL_REF)),
                ("suspend_VDI", json!(NULL_REF)),
                ("suspend_SR", json!(NULL_REF)),
                ("parent", json!(NULL_REF)),
                ("VBDs", json!([])),
                ("VIFs", json!([])),
                ("VTPMs", json!([])),
                ("VUSBs", json!([])),
                ("consoles", json!([])),
                ("crash_dumps", json!([])),
                ("children", json!([])),
                ("current_operations", json!({})),
            ] {
                rec.insert(k.into(), v);
            }
            if !is_main {
                rec.insert("power_state".into(), json!(power));
            }
            let parent = text(&c.rec, "snapshot_of");
            rec.insert(
                "snapshot_of".into(),
                json!(
                    vm_new
                        .get(&parent)
                        .cloned()
                        .unwrap_or_else(|| NULL_REF.into())
                ),
            );
            let snaps: Vec<String> = refs(&c.rec, "snapshots")
                .iter()
                .filter_map(|s| vm_new.get(s).cloned())
                .collect();
            rec.insert("snapshots".into(), json!(snaps));
            let metrics = match &c.metrics {
                Some(m) => {
                    let (mr, muuid) = db.new_ref("vm_metrics");
                    let mut m = m.clone();
                    m.insert("uuid".into(), json!(muuid));
                    db.insert("vm_metrics", &mr, Value::Object(m));
                    mr
                }
                None => NULL_REF.to_owned(),
            };
            rec.insert("metrics".into(), json!(metrics));
            let allowed = crate::provisioning::vm_allowed(&rec);
            rec.insert("allowed_operations".into(), json!(allowed));
            db.insert("vm", &r, Value::Object(rec));
            for b in &c.vbds {
                let old_vdi = text(b, "VDI");
                let vdi = vdi_new.get(&old_vdi).cloned();
                let (br, buuid) = db.new_ref("vbd");
                let mut b = b.clone();
                b.insert("uuid".into(), json!(buuid));
                b.insert("VM".into(), json!(r));
                b.insert(
                    "VDI".into(),
                    json!(vdi.clone().unwrap_or_else(|| NULL_REF.into())),
                );
                b.insert("empty".into(), json!(vdi.is_none()));
                b.insert("currently_attached".into(), json!(false));
                b.insert("device".into(), json!(""));
                b.insert("current_operations".into(), json!({}));
                b.insert("metrics".into(), json!(NULL_REF));
                if vdi.is_none() && text(&b, "type") != "CD" {
                    // A disk left behind: no drive for it.
                    continue;
                }
                db.insert("vbd", &br, Value::Object(b));
                db.push_ref("vm", &r, "VBDs", &br);
                if let Some(vdi) = vdi {
                    db.push_ref("vdi", &vdi, "VBDs", &br);
                }
                db.refresh_vbd_ops(&br);
            }
            for (old_vif, f) in &c.vifs {
                let net = match vif_net.get(old_vif.as_str()) {
                    Some(n) => (*n).to_owned(),
                    // Within the pool an unmapped interface keeps its network.
                    None if plan.same => text(f, "network"),
                    None => continue,
                };
                let (fr, fuuid) = db.new_ref("vif");
                let mut f = f.clone();
                f.insert("uuid".into(), json!(fuuid));
                f.insert("VM".into(), json!(r));
                f.insert("network".into(), json!(net));
                f.insert("currently_attached".into(), json!(false));
                f.insert("current_operations".into(), json!({}));
                f.insert("metrics".into(), json!(NULL_REF));
                db.insert("vif", &fr, Value::Object(f));
                db.push_ref("vm", &r, "VIFs", &fr);
                db.push_ref("network", &net, "VIFs", &fr);
            }
            if is_main && matches!(power.as_str(), "Running" | "Paused" | "Suspended") {
                db.move_vm(&r, &power, Some(plan.host.clone()));
            } else {
                db.sync_vbds(&r);
                db.sync_vifs(&r);
            }
        }
        vm_new[&plan.vm].clone()
    };
    plan.target.changed.notify_waiters();

    // The source: a move takes the VM away; a copy leaves it.
    let mut db = state.db();
    if plan.copy {
        db.settle_migrated(&plan.vm);
    } else {
        for c in &vms {
            db.remove_carried(&c.vm);
        }
        for d in &disks {
            db.xva.contents.remove(&d.vdi);
            db.xva.digests.remove(&d.vdi);
            db.destroy_vdi_record(&d.vdi);
        }
    }
    new_main
}

/// `Async.VM.migrate_send`: checked now, done when the task ends.
fn start_async(
    state: &Arc<State>,
    method: &str,
    params: &[Value],
    failure: Option<Error>,
) -> Reply {
    let vm = str_param(params, 1).to_owned();
    let checked = match failure {
        Some(e) => Err(e),
        None => plan(state, params, false).map(|_| ()),
    };
    let task = crate::create_task(state, method);
    if checked.is_ok() {
        state.db().begin_vm_op(&vm, "migrate_send", &task);
    }
    let (steps, step) = {
        let db = state.db();
        (db.task_steps, db.task_step)
    };
    let state = state.clone();
    let params = params.to_vec();
    let task_ref = task.clone();
    let runtime = state.runtime.clone();
    runtime.spawn(async move {
        let settle = |state: &Arc<State>| {
            let mut db = state.db();
            if db.exists("vm", &vm) {
                db.settle_migrated(&vm);
            }
        };
        for i in 1..steps {
            tokio::time::sleep(step).await;
            let mut db = state.db();
            if db.get_str("task", &task_ref, "status").as_deref() != Some("pending") {
                drop(db);
                settle(&state);
                state.changed.notify_waiters();
                return;
            }
            let progress = f64::from(i) / f64::from(steps);
            db.modify("task", &task_ref, |rec| {
                rec.insert("progress".into(), json!(progress));
            });
            drop(db);
            state.changed.notify_waiters();
        }
        tokio::time::sleep(step).await;
        let pending = state.db().get_str("task", &task_ref, "status").as_deref() == Some("pending");
        if !pending {
            settle(&state);
            state.changed.notify_waiters();
            return;
        }
        let outcome = checked
            .and_then(|()| plan(&state, &params, true))
            .map(|p| perform(&state, &p));
        match outcome {
            Ok(new_vm) => {
                state.db().finish_task(
                    &task_ref,
                    "success",
                    &format!("<value>{new_vm}</value>"),
                    &[],
                );
            }
            Err((code, params)) => {
                settle(&state);
                let mut info = vec![code.as_str()];
                info.extend(params.iter().map(String::as_str));
                state.db().finish_task(&task_ref, "failure", "", &info);
            }
        }
        state.changed.notify_waiters();
    });
    Ok(json!(task))
}

impl Db {
    /// The disks of a VM's drives (CD drives and empty drives left out).
    pub(crate) fn vm_disks(&self, vm: &str) -> Vec<String> {
        let Some(rec) = self.record("vm", vm) else {
            return Vec::new();
        };
        refs(rec, "VBDs")
            .iter()
            .filter_map(|b| self.record("vbd", b))
            .filter(|b| text(b, "type") != "CD")
            .map(|b| text(b, "VDI"))
            .filter(|v| v != NULL_REF && self.exists("vdi", v))
            .collect()
    }

    /// Moves disk `vdi` to `sr`: a new disk there takes its drives and
    /// content; the old one is destroyed.
    pub(crate) fn relocate_vdi(&mut self, vdi: &str, sr: &str) -> Option<String> {
        let mut rec = self.record("vdi", vdi)?.clone();
        let vbds = refs(&rec, "VBDs");
        let (r, uuid) = self.new_ref("vdi");
        rec.insert("uuid".into(), json!(uuid));
        rec.insert("location".into(), json!(uuid));
        rec.insert("SR".into(), json!(sr));
        rec.insert("snapshots".into(), json!([]));
        rec.insert("snapshot_of".into(), json!(NULL_REF));
        let size = int(&rec, "virtual_size");
        self.insert("vdi", &r, Value::Object(rec));
        self.push_ref("sr", sr, "VDIs", &r);
        self.modify("sr", sr, |s| {
            let alloc = int(s, "virtual_allocation") + size;
            s.insert("virtual_allocation".into(), json!(alloc));
        });
        for vbd in &vbds {
            self.modify("vbd", vbd, |b| {
                b.insert("VDI".into(), json!(r));
            });
        }
        self.modify("vdi", vdi, |v| {
            v.insert("VBDs".into(), json!([]));
        });
        if let Some(c) = self.xva.contents.remove(vdi) {
            self.xva.contents.insert(r.clone(), c);
        }
        if let Some(d) = self.xva.digests.remove(vdi) {
            self.xva.digests.insert(r.clone(), d);
        }
        self.destroy_vdi_record(vdi);
        Some(r)
    }

    /// A VM's operations after a migration ended (or did not happen).
    pub(crate) fn settle_migrated(&mut self, vm: &str) {
        let Some(rec) = self.record("vm", vm) else {
            return;
        };
        let ops = crate::provisioning::vm_allowed(rec);
        self.modify("vm", vm, |rec| {
            rec.insert("allowed_operations".into(), json!(ops));
            rec.insert("current_operations".into(), json!({}));
        });
    }

    /// Removes a VM that moved away, with its drives, interfaces, consoles,
    /// metrics and vTPMs (not its disks).
    fn remove_carried(&mut self, vm: &str) {
        let Some(rec) = self.record("vm", vm).cloned() else {
            return;
        };
        for vbd in refs(&rec, "VBDs") {
            if let Some(vdi) = self.get_str("vbd", &vbd, "VDI") {
                self.drop_ref("vdi", &vdi, "VBDs", &vbd);
            }
            self.remove("vbd", &vbd);
        }
        for vif in refs(&rec, "VIFs") {
            if let Some(net) = self.get_str("vif", &vif, "network") {
                self.drop_ref("network", &net, "VIFs", &vif);
            }
            self.remove("vif", &vif);
        }
        for c in refs(&rec, "consoles") {
            self.remove("console", &c);
        }
        for t in refs(&rec, "VTPMs") {
            self.remove("vtpm", &t);
        }
        self.remove("vm_metrics", &text(&rec, "metrics"));
        let host = text(&rec, "resident_on");
        if host != NULL_REF {
            self.drop_ref("host", &host, "resident_VMs", vm);
        }
        let parent = text(&rec, "snapshot_of");
        if parent != NULL_REF {
            self.drop_ref("vm", &parent, "snapshots", vm);
        }
        let appliance = text(&rec, "appliance");
        if appliance != NULL_REF {
            self.drop_ref("vm_appliance", &appliance, "VMs", vm);
            self.refresh_appliance(&appliance);
        }
        self.remove("vm", vm);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::{MockServer, NULL_REF};

    pub(crate) async fn login(server: &MockServer) -> String {
        let s = crate::dispatch(
            &server.state,
            "session.login_with_password",
            &[json!(crate::USERNAME), json!(crate::PASSWORD)],
        )
        .await
        .expect("login");
        s.as_str().unwrap().to_owned()
    }

    pub(crate) async fn call(
        server: &MockServer,
        session: &str,
        method: &str,
        args: &[Value],
    ) -> Result<Value, (String, Vec<String>)> {
        let mut params = vec![json!(session)];
        params.extend_from_slice(args);
        crate::dispatch(&server.state, method, &params).await
    }

    /// Waits for `task` to end; its status and result or error.
    pub(crate) async fn wait_task(server: &MockServer, task: &str) -> (String, Value) {
        for _ in 0..400 {
            let status = server
                .field("task", task, "status")
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            if status != "pending" {
                let out = if status == "success" {
                    server.field("task", task, "result").unwrap_or_default()
                } else {
                    server.field("task", task, "error_info").unwrap_or_default()
                };
                return (status, out);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("task {task} did not end");
    }

    fn code(r: Result<Value, (String, Vec<String>)>) -> String {
        r.expect_err("an error").0
    }

    /// A halted VM with one disk on the local SR and one interface, on lab
    /// server `tag`.
    async fn lab_vm(server: &MockServer, s: &str, tag: &str) -> (String, String, String) {
        let vm = call(
            server,
            s,
            "VM.create",
            &[json!({
                "name_label": "mover", "memory_static_max": 1073741824_i64,
                "memory_dynamic_max": 1073741824_i64, "memory_dynamic_min": 1073741824_i64,
                "memory_static_min": 536870912_i64, "VCPUs_max": 1, "VCPUs_at_startup": 1,
            })],
        )
        .await
        .unwrap();
        let vm = vm.as_str().unwrap().to_owned();
        let sr = format!("OpaqueRef:sr-local-01-{tag}");
        let vdi = call(
            server,
            s,
            "VDI.create",
            &[
                json!({"name_label": "mover disk", "SR": sr, "virtual_size": 104857600_i64,
                     "type": "user", "sharable": false, "read_only": false}),
            ],
        )
        .await
        .unwrap();
        let vdi = vdi.as_str().unwrap().to_owned();
        call(
            server,
            s,
            "VBD.create",
            &[
                json!({"VM": vm, "VDI": vdi, "userdevice": "0", "bootable": true,
                     "mode": "RW", "type": "Disk", "empty": false}),
            ],
        )
        .await
        .unwrap();
        let vif = call(
            server,
            s,
            "VIF.create",
            &[
                json!({"VM": vm, "network": format!("OpaqueRef:net-eth0-{tag}"),
                     "device": "0", "MAC": "02:00:00:aa:bb:cc", "MTU": 1500}),
            ],
        )
        .await
        .unwrap();
        (vm, vdi, vif.as_str().unwrap().to_owned())
    }

    fn refs_of(server: &MockServer, class: &str, r: &str, field: &str) -> Vec<String> {
        server
            .field(class, r, field)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    #[tokio::test]
    async fn migrate_between_two_pools() {
        let a = MockServer::start_lab(false, "ma", 1).await;
        let b = MockServer::start_lab(false, "mb", 2).await;
        a.set_task_timing(3, Duration::from_millis(10));
        let (sa, sb) = (login(&a).await, login(&b).await);
        let (vm, vdi, vif) = lab_vm(&a, &sa, "ma").await;
        let allowed = a.field("vm", &vm, "allowed_operations").unwrap();
        assert!(allowed.as_array().unwrap().contains(&json!("migrate_send")));
        let (host_b, sr_b, net_b) = (
            "OpaqueRef:host-0001-mb",
            "OpaqueRef:sr-local-01-mb",
            "OpaqueRef:net-eth0-mb",
        );
        let dest = call(
            &b,
            &sb,
            "host.migrate_receive",
            &[json!(host_b), json!(net_b), json!({})],
        )
        .await
        .unwrap();
        for key in ["session_id", "host", "master", "SM", "xenops", "network"] {
            assert!(dest.get(key).is_some(), "{key}");
        }
        assert_eq!(
            code(
                call(
                    &b,
                    &sb,
                    "host.migrate_receive",
                    &[json!("OpaqueRef:x"), json!(net_b), json!({})]
                )
                .await
            ),
            "HANDLE_INVALID"
        );
        let args = |vdis: Value, vifs: Value, options: Value| {
            vec![json!(vm), dest.clone(), json!(true), vdis, vifs, options]
        };
        let maps = || (json!({ &vdi: sr_b }), json!({ &vif: net_b }));
        // Refusals.
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(json!({}), maps().1, json!({})),
        )
        .await;
        assert_eq!(
            r.unwrap_err(),
            ("VDI_NOT_IN_MAP".to_owned(), vec![vdi.clone()])
        );
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(maps().0, json!({}), json!({})),
        )
        .await;
        assert_eq!(code(r), "VIF_NOT_IN_MAP");
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(json!({ &vdi: "OpaqueRef:nope" }), maps().1, json!({})),
        )
        .await;
        assert_eq!(code(r), "HANDLE_INVALID");
        let mut bad = dest.clone();
        bad["master"] = json!("https://127.0.0.1:1/");
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &[json!(vm), bad, json!(true), maps().0, maps().1, json!({})],
        )
        .await;
        assert_eq!(code(r), "CANNOT_CONTACT_HOST");
        b.refuse_migration_to(host_b, Some(("VM_HAS_PCI_ATTACHED", &["x"])));
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(maps().0, maps().1, json!({})),
        )
        .await;
        assert_eq!(code(r), "VM_HAS_PCI_ATTACHED");
        b.refuse_migration_to(host_b, None);
        a.fail_method(
            "VM.assert_can_migrate",
            Some(("LICENCE_RESTRICTION", &["x"])),
        );
        let r = call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(maps().0, maps().1, json!({})),
        )
        .await;
        assert_eq!(code(r), "LICENCE_RESTRICTION");
        a.fail_method("VM.assert_can_migrate", None);
        call(
            &a,
            &sa,
            "VM.assert_can_migrate",
            &args(maps().0, maps().1, json!({})),
        )
        .await
        .expect("can migrate");

        // A failure mid-migration changes nothing.
        a.fail_method("VM.migrate_send", Some(("VDI_COPY_FAILED", &["boom"])));
        let task = call(
            &a,
            &sa,
            "Async.VM.migrate_send",
            &args(maps().0, maps().1, json!({})),
        )
        .await
        .unwrap();
        let (status, info) = wait_task(&a, task.as_str().unwrap()).await;
        assert_eq!(status, "failure");
        assert_eq!(info[0], json!("VDI_COPY_FAILED"));
        a.fail_method("VM.migrate_send", None);
        assert!(a.record("vm", &vm).is_some());
        assert!(b.refs("vm").len() == 1, "only the control domain on b");

        // Cancelled: nothing changes, the VM is free again.
        a.set_task_timing(20, Duration::from_millis(20));
        let task = call(
            &a,
            &sa,
            "Async.VM.migrate_send",
            &args(maps().0, maps().1, json!({})),
        )
        .await
        .unwrap();
        let task = task.as_str().unwrap().to_owned();
        let ops = a.field("vm", &vm, "current_operations").unwrap();
        assert!(ops.get(&task).is_some(), "busy while migrating");
        call(&a, &sa, "task.cancel", &[json!(task)]).await.unwrap();
        let (status, _) = wait_task(&a, &task).await;
        assert_eq!(status, "cancelled");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(a.record("vm", &vm).is_some());
        let allowed = a.field("vm", &vm, "allowed_operations").unwrap();
        assert!(allowed.as_array().unwrap().contains(&json!("migrate_send")));

        // A copy: both have it.
        a.set_task_timing(3, Duration::from_millis(10));
        let dest2 = call(
            &b,
            &sb,
            "host.migrate_receive",
            &[json!(host_b), json!(net_b), json!({})],
        )
        .await
        .unwrap();
        let copied = call(
            &a,
            &sa,
            "VM.migrate_send",
            &[
                json!(vm),
                dest2,
                json!(true),
                maps().0,
                maps().1,
                json!({"copy": "true"}),
            ],
        )
        .await
        .unwrap();
        let copied = copied.as_str().unwrap().to_owned();
        assert!(a.record("vm", &vm).is_some());
        assert_ne!(b.field("vm", &copied, "uuid"), a.field("vm", &vm, "uuid"));
        assert_eq!(
            b.field("vm", &copied, "name_label").unwrap(),
            json!("mover")
        );

        // The move.
        a.state
            .db()
            .xva
            .digests
            .insert(vdi.clone(), "digest-of-mover".to_owned());
        let uuid = a.field("vm", &vm, "uuid").unwrap();
        let dest3 = call(
            &b,
            &sb,
            "host.migrate_receive",
            &[json!(host_b), json!(net_b), json!({})],
        )
        .await
        .unwrap();
        let session_b = dest3["session_id"].as_str().unwrap().to_owned();
        let task = call(
            &a,
            &sa,
            "Async.VM.migrate_send",
            &[json!(vm), dest3, json!(true), maps().0, maps().1, json!({})],
        )
        .await
        .unwrap();
        let (status, result) = wait_task(&a, task.as_str().unwrap()).await;
        assert_eq!(status, "success");
        let moved = result
            .as_str()
            .unwrap()
            .trim_start_matches("<value>")
            .trim_end_matches("</value>")
            .to_owned();
        assert_eq!(b.field("vm", &moved, "uuid").unwrap(), uuid);
        assert!(a.record("vm", &vm).is_none());
        assert!(a.record("vdi", &vdi).is_none());
        assert!(a.record("vif", &vif).is_none());
        let vbds = refs_of(&b, "vm", &moved, "VBDs");
        let new_vdi = b.field("vbd", &vbds[0], "VDI").unwrap();
        let new_vdi = new_vdi.as_str().unwrap();
        assert_eq!(b.field("vdi", new_vdi, "SR").unwrap(), json!(sr_b));
        assert_eq!(b.vdi_digest(new_vdi).as_deref(), Some("digest-of-mover"));
        assert!(refs_of(&b, "sr", sr_b, "VDIs").contains(&new_vdi.to_owned()));
        let vifs = refs_of(&b, "vm", &moved, "VIFs");
        assert_eq!(b.field("vif", &vifs[0], "network").unwrap(), json!(net_b));
        assert_eq!(
            b.field("vif", &vifs[0], "MAC").unwrap(),
            json!("02:00:00:aa:bb:cc")
        );
        assert!(refs_of(&b, "network", net_b, "VIFs").contains(&vifs[0]));
        assert_eq!(
            b.field("vm", &moved, "resident_on").unwrap(),
            json!(NULL_REF)
        );
        assert!(
            !b.state.db().sessions.contains_key(&session_b),
            "session closed"
        );
    }

    #[tokio::test]
    async fn running_vm_moves_storage_and_host_within_the_pool() {
        let m = MockServer::start(false).await;
        let s = login(&m).await;
        let vm = "OpaqueRef:vm-web";
        let dest = call(
            &m,
            &s,
            "host.migrate_receive",
            &[
                json!("OpaqueRef:host-0002"),
                json!("OpaqueRef:net-eth0"),
                json!({}),
            ],
        )
        .await
        .unwrap();
        // Within the pool, unmapped disks and interfaces may stay.
        let moved = call(
            &m,
            &s,
            "VM.migrate_send",
            &[
                json!(vm),
                dest,
                json!(true),
                json!({"OpaqueRef:vdi-web-data": "OpaqueRef:sr-local-02"}),
                json!({"OpaqueRef:vif-web-1": "OpaqueRef:net-vlan20"}),
                json!({}),
            ],
        )
        .await
        .unwrap();
        assert_eq!(moved, json!(vm));
        assert_eq!(
            m.field("vm", vm, "resident_on").unwrap(),
            json!("OpaqueRef:host-0002")
        );
        assert_eq!(m.field("vm", vm, "power_state").unwrap(), json!("Running"));
        assert!(m.record("vdi", "OpaqueRef:vdi-web-data").is_none());
        let data = m.field("vbd", "OpaqueRef:vbd-web-1", "VDI").unwrap();
        assert_eq!(
            m.field("vdi", data.as_str().unwrap(), "SR").unwrap(),
            json!("OpaqueRef:sr-local-02")
        );
        assert_eq!(
            m.field("vbd", "OpaqueRef:vbd-web-0", "VDI").unwrap(),
            json!("OpaqueRef:vdi-web-root")
        );
        assert_eq!(
            m.field("vif", "OpaqueRef:vif-web-1", "network").unwrap(),
            json!("OpaqueRef:net-vlan20")
        );
    }

    #[tokio::test]
    async fn memory_power_state_and_template_checks() {
        let m = MockServer::start(false).await;
        let s = login(&m).await;
        let dest = call(
            &m,
            &s,
            "host.migrate_receive",
            &[
                json!("OpaqueRef:host-0002"),
                json!("OpaqueRef:net-eth0"),
                json!({}),
            ],
        )
        .await
        .unwrap();
        let metrics = m.field("host", "OpaqueRef:host-0002", "metrics").unwrap();
        m.set_field(
            "host_metrics",
            metrics.as_str().unwrap(),
            "memory_free",
            json!(1024),
        );
        let args = |vm: &str, options: Value| {
            vec![
                json!(vm),
                dest.clone(),
                json!(true),
                json!({}),
                json!({}),
                options,
            ]
        };
        let r = call(
            &m,
            &s,
            "VM.assert_can_migrate",
            &args("OpaqueRef:vm-web", json!({})),
        )
        .await;
        assert_eq!(code(r), "HOST_NOT_ENOUGH_FREE_MEMORY");
        m.set_vm_power_state("OpaqueRef:vm-web", "Paused");
        let r = call(
            &m,
            &s,
            "VM.assert_can_migrate",
            &args("OpaqueRef:vm-web", json!({})),
        )
        .await;
        assert_eq!(code(r), "VM_BAD_POWER_STATE");
        let template = m
            .refs("vm")
            .into_iter()
            .find(|r| m.field("vm", r, "is_a_template") == Some(json!(true)))
            .unwrap();
        let r = call(&m, &s, "VM.assert_can_migrate", &args(&template, json!({}))).await;
        assert_eq!(code(r), "VM_IS_TEMPLATE");
        call(
            &m,
            &s,
            "VM.assert_can_migrate",
            &args(&template, json!({"copy": "true"})),
        )
        .await
        .expect("a template may be copied");
        let r = call(
            &m,
            &s,
            "VM.assert_can_migrate",
            &args("OpaqueRef:nope", json!({})),
        )
        .await;
        assert_eq!(code(r), "HANDLE_INVALID");
    }
}
