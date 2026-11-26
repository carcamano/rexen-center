//! VM creation, templates, snapshots and disks in the mock.
//!
//! **VMs.** `VM.clone` / `copy` (disks cloned in their SR or copied to a
//! given one, interfaces and vTPMs cloned), `provision` (the disks of a
//! template's `other_config["disks"]`), `snapshot` / `checkpoint`
//! (snapshot disks, `snapshot_of`, `snapshots`), `revert` (the VM takes the
//! snapshot's settings, power state and disk clones; its old disks are
//! destroyed), `destroy` (with its VBDs, VIFs and vTPMs, never its VDIs),
//! `set_is_a_template`, `get_allowed_VBD_devices` and
//! `get_allowed_VIF_devices`.
//!
//! **Disks.** `VDI.create` (`SR_FULL` when the SR has no room),
//! `destroy` (`VDI_IN_USE` while a VBD is attached), `resize` /
//! `resize_online` (never smaller), `copy`, `pool_migrate` (a running VM's
//! disk moves to another SR, its VBD pointing at the new VDI); `VBD.create`
//! (`autodetect` picks the first free position), `destroy`, `plug`,
//! `unplug`, `insert` and `eject` (CD drives), `VIF.create` / `destroy`,
//! and `VTPM.create` / `destroy`.
//!
//! **Fields.** Every class gets the setters `set_<field>` and the map calls
//! `add_to_<map>` / `remove_from_<map>`; `VM.set_memory_limits`,
//! `set_memory` and `set_VCPUs_*` check the limits XAPI checks.
//!
//! The `Async.*` forms of all these return a task that runs the call when
//! it ends, with the call's result in the task's `result` (as XAPI writes
//! it: `<value>…</value>`) or its error in `error_info`.

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// Calls handled here, by class (as in the API) and call name.
const CALLS: &[(&str, &[&str])] = &[
    (
        "VM",
        &[
            "clone",
            "copy",
            "provision",
            "snapshot",
            "checkpoint",
            "revert",
            "destroy",
            "set_is_a_template",
            "get_allowed_VBD_devices",
            "get_allowed_VIF_devices",
            "set_memory_limits",
            "set_memory",
            "set_VCPUs_max",
            "set_VCPUs_at_startup",
        ],
    ),
    (
        "VDI",
        &[
            "create",
            "destroy",
            "resize",
            "resize_online",
            "copy",
            "pool_migrate",
        ],
    ),
    (
        "VBD",
        &["create", "destroy", "plug", "unplug", "insert", "eject"],
    ),
    ("VIF", &["create", "destroy", "move"]),
    ("VTPM", &["create", "destroy"]),
];

/// Whether `class.call` is handled here (setters and map calls included,
/// for every class but the ones the mock handles elsewhere).
pub(crate) fn handles(class: &str, call: &str) -> bool {
    if crate::storage::handles(class, call)
        || crate::networking::handles(class, call)
        || crate::pool::handles(class, call)
        || crate::ha::handles(class, call)
        || crate::ad::handles(class, call)
    {
        return true;
    }
    if CALLS
        .iter()
        .any(|(c, calls)| *c == class && calls.contains(&call))
    {
        return true;
    }
    let generic =
        call.starts_with("set_") || call.starts_with("add_to_") || call.starts_with("remove_from_");
    generic
        && !matches!(class, "session" | "event" | "task" | "message")
        && call != "add_to_other_config"
        && call != "remove_from_other_config"
}

/// What a VM in this state allows (templates and snapshots included).
pub(crate) fn vm_allowed(rec: &Map<String, Value>) -> Vec<&'static str> {
    let flag = |name: &str| rec.get(name).and_then(Value::as_bool).unwrap_or(false);
    if flag("is_a_snapshot") {
        return vec!["revert", "clone", "copy", "export", "destroy"];
    }
    if flag("is_a_template") {
        let mut ops = vec!["clone", "copy", "export", "provision"];
        if !flag("is_default_template") {
            ops.push("destroy");
        }
        return ops;
    }
    let state = rec
        .get("power_state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    crate::ops::allowed_for(state).to_vec()
}

fn ref_list(rec: &Map<String, Value>, field: &str) -> Vec<String> {
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

pub(crate) fn int(rec: &Map<String, Value>, field: &str) -> i64 {
    match rec.get(field) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn int_param(params: &[Value], i: usize) -> Option<i64> {
    match params.get(i)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// `xvda`, `xvdb`… for user device `n`.
fn device_name(userdevice: &str) -> String {
    let n: u8 = userdevice.parse().unwrap_or(0);
    format!("xvd{}", (b'a' + n.min(25)) as char)
}

/// A random-looking but deterministic MAC for VIF `n`.
fn mac(n: u64) -> String {
    format!(
        "02:f7:{:02x}:{:02x}:{:02x}:{:02x}",
        (n >> 24) & 0xff,
        (n >> 16) & 0xff,
        (n >> 8) & 0xff,
        n & 0xff
    )
}

/// One `<disk>` of a template's provision XML.
struct ProvisionDisk {
    device: String,
    size: i64,
    sr_uuid: String,
    bootable: bool,
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_owned())
}

fn provision_disks(xml: &str) -> Vec<ProvisionDisk> {
    xml.split("<disk")
        .skip(1)
        .map(|tag| {
            let tag = tag.split('>').next().unwrap_or_default();
            ProvisionDisk {
                device: attr(tag, "device").unwrap_or_else(|| "0".into()),
                size: attr(tag, "size").and_then(|s| s.parse().ok()).unwrap_or(0),
                sr_uuid: attr(tag, "sr").unwrap_or_default(),
                bootable: attr(tag, "bootable").as_deref() == Some("true"),
            }
        })
        .collect()
}

/// Like XAPI's unmarshalling of a record: an enum field (or date) with a
/// value XAPI does not know fails with `INTERNAL_ERROR`
/// `Failure("Unknown tag/contents")`, even when it is empty.
fn check_fields(
    rec: &Map<String, Value>,
    fields: &[(&str, &[&str])],
) -> Result<(), (String, Vec<String>)> {
    for (field, allowed) in fields {
        let Some(value) = rec.get(*field) else {
            continue;
        };
        let ok = match value.as_str() {
            Some(v) if allowed.is_empty() => !v.is_empty(),
            Some(v) => allowed.contains(&v),
            None => false,
        };
        if !ok {
            return Err((
                "INTERNAL_ERROR".into(),
                vec!["Failure(\"Unknown tag/contents\")".into()],
            ));
        }
    }
    Ok(())
}

const VDI_TYPES: &[&str] = &[
    "system",
    "user",
    "ephemeral",
    "suspend",
    "crashdump",
    "ha_statefile",
    "metadata",
    "redo_log",
    "rrd",
    "pvs_cache",
    "cbt_metadata",
];

impl Db {
    pub(crate) fn record(&self, class: &str, r: &str) -> Option<&Map<String, Value>> {
        self.objects
            .get(&(class.to_owned(), r.to_owned()))
            .and_then(Value::as_object)
    }

    pub(crate) fn exists(&self, class: &str, r: &str) -> bool {
        self.record(class, r).is_some()
    }

    /// A new reference and uuid for an object of `class`.
    pub(crate) fn new_ref(&mut self, class: &str) -> (String, String) {
        let n = self.next_object;
        self.next_object += 1;
        (
            format!("OpaqueRef:mock-{class}-{n}"),
            format!("f7000000-0000-4000-8000-{n:012}"),
        )
    }

    pub(crate) fn insert(&mut self, class: &str, r: &str, record: Value) {
        self.objects
            .insert((class.to_owned(), r.to_owned()), record.clone());
        self.push_event(class, "add", r, Some(record));
    }

    pub(crate) fn remove(&mut self, class: &str, r: &str) {
        if self
            .objects
            .remove(&(class.to_owned(), r.to_owned()))
            .is_some()
        {
            self.push_event(class, "del", r, None);
        }
    }

    pub(crate) fn push_ref(&mut self, class: &str, r: &str, field: &str, item: &str) {
        self.modify(class, r, |rec| {
            let list = rec.entry(field).or_insert_with(|| json!([]));
            if let Value::Array(l) = list
                && !l.iter().any(|v| v.as_str() == Some(item))
            {
                l.push(json!(item));
            }
        });
    }

    pub(crate) fn drop_ref(&mut self, class: &str, r: &str, field: &str, item: &str) {
        if !self.exists(class, r) {
            return;
        }
        self.modify(class, r, |rec| {
            if let Some(Value::Array(l)) = rec.get_mut(field) {
                l.retain(|v| v.as_str() != Some(item));
            }
        });
    }

    fn sr_by_uuid(&self, uuid: &str) -> Option<String> {
        self.objects
            .iter()
            .find(|((c, _), rec)| {
                c == "sr" && rec.get("uuid").and_then(Value::as_str) == Some(uuid)
            })
            .map(|((_, r), _)| r.clone())
    }

    fn default_sr(&self) -> Option<String> {
        self.objects
            .iter()
            .find(|((c, _), _)| c == "pool")
            .and_then(|(_, p)| p.get("default_SR"))
            .and_then(Value::as_str)
            .filter(|r| *r != NULL_REF)
            .map(str::to_owned)
    }

    pub(crate) fn vm_power(&self, vm: &str) -> String {
        self.get_str("vm", vm, "power_state").unwrap_or_default()
    }

    /// Recomputes a VM's `allowed_operations` (after a template or
    /// snapshot change).
    fn refresh_vm_ops(&mut self, vm: &str) {
        let Some(rec) = self.record("vm", vm) else {
            return;
        };
        let ops = vm_allowed(rec);
        self.modify("vm", vm, |rec| {
            rec.insert("allowed_operations".into(), json!(ops));
        });
    }

    /// `allowed_operations` of a VBD from its VM's power state.
    pub(crate) fn refresh_vbd_ops(&mut self, vbd: &str) {
        let Some(rec) = self.record("vbd", vbd) else {
            return;
        };
        let vm = rec.get("VM").and_then(Value::as_str).unwrap_or_default();
        // Drives of a paused VM cannot be plugged or unplugged.
        let running = self.vm_power(vm) == "Running";
        let attached = rec
            .get("currently_attached")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let cd = rec.get("type").and_then(Value::as_str) == Some("CD");
        let empty = rec.get("empty").and_then(Value::as_bool).unwrap_or(false);
        let mut ops: Vec<&str> = Vec::new();
        if running {
            ops.push(if attached { "unplug" } else { "plug" });
            if attached {
                ops.push("unplug_force");
            }
        } else {
            ops.push("attach");
        }
        if cd && (!running || attached) {
            ops.push(if empty { "insert" } else { "eject" });
        }
        self.modify("vbd", vbd, |rec| {
            rec.insert("allowed_operations".into(), json!(ops));
        });
    }

    /// Recomputes the VBD operations of a VM's drives and their attached
    /// state after its power state changed.
    pub(crate) fn sync_vbds(&mut self, vm: &str) {
        let running = matches!(self.vm_power(vm).as_str(), "Running" | "Paused");
        let vbds = self.record("vm", vm).map(|r| ref_list(r, "VBDs"));
        for vbd in vbds.unwrap_or_default() {
            let attached = self
                .record("vbd", &vbd)
                .and_then(|r| r.get("currently_attached"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if attached != running {
                let device = self
                    .get_str("vbd", &vbd, "userdevice")
                    .map(|u| device_name(&u))
                    .unwrap_or_default();
                self.modify("vbd", &vbd, |rec| {
                    rec.insert("currently_attached".into(), json!(running));
                    rec.insert(
                        "device".into(),
                        json!(if running { device } else { String::new() }),
                    );
                });
            }
            self.refresh_vbd_ops(&vbd);
        }
    }

    /// Runs `class.call`; `None` if it is not one of the calls handled here.
    pub(crate) fn provisioning_call(
        &mut self,
        class: &str,
        call: &str,
        params: &[Value],
    ) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        if let Some(reply) = self.storage_call(class, call, params) {
            return Some(reply);
        }
        if let Some(reply) = self.networking_call(class, call, params) {
            return Some(reply);
        }
        if let Some(reply) = self.pool_call(class, call, params) {
            return Some(reply);
        }
        if let Some(reply) = self.ha_call(class, call, params) {
            return Some(reply);
        }
        if let Some(reply) = self.ad_call(class, call, params) {
            return Some(reply);
        }
        let lower = class.to_lowercase();
        Some(match (class, call) {
            ("VM", "clone") => self.vm_clone(params, None),
            ("VM", "copy") => {
                let sr = str_param(params, 3).to_owned();
                self.vm_clone(params, Some(sr))
            }
            ("VM", "provision") => self.vm_provision(str_param(params, 1)),
            ("VM", "snapshot") => self.vm_snapshot(params, false),
            ("VM", "checkpoint") => self.vm_snapshot(params, true),
            ("VM", "revert") => self.vm_revert(str_param(params, 1)),
            ("VM", "destroy") => self.vm_destroy(str_param(params, 1)),
            ("VM", "set_is_a_template") => self.vm_make_template(params),
            ("VM", "get_allowed_VBD_devices") => self.allowed_devices(params, "VBDs", 16),
            ("VM", "get_allowed_VIF_devices") => self.allowed_devices(params, "VIFs", 7),
            ("VM", "set_memory_limits") => self.vm_memory_limits(params),
            ("VM", "set_memory")
                if self
                    .record("vm", str_param(params, 1))
                    .is_some_and(|r| r.get("is_control_domain") == Some(&json!(true))) =>
            {
                // A control domain's memory is set while it runs; it takes
                // effect when the server reboots.
                let vm = str_param(params, 1).to_owned();
                let v = int_param(params, 2).unwrap_or(0);
                let min = self
                    .record("vm", &vm)
                    .map_or(0, |r| int(r, "memory_static_min"));
                if v < min {
                    return Some(api_error(
                        "MEMORY_CONSTRAINT_VIOLATION",
                        &[
                            "Memory limits must satisfy: static_min <= dynamic_min <= dynamic_max <= static_max",
                        ],
                    ));
                }
                self.modify("vm", &vm, |rec| {
                    for f in [
                        "memory_dynamic_min",
                        "memory_dynamic_max",
                        "memory_static_max",
                        "memory_target",
                    ] {
                        rec.insert(f.into(), json!(v));
                    }
                });
                Ok(json!(""))
            }
            ("VM", "set_memory") => {
                let v = params.get(2).cloned().unwrap_or(json!(0));
                let vm = json!(str_param(params, 1));
                let min = self
                    .record("vm", str_param(params, 1))
                    .map(|r| int(r, "memory_static_min"))
                    .unwrap_or(0);
                self.vm_memory_limits(&[json!(""), vm, json!(min), v.clone(), v.clone(), v])
            }
            ("VM", "set_VCPUs_max" | "set_VCPUs_at_startup") => self.vm_vcpus(call, params),
            ("VDI", "create") => self.vdi_create(params.get(1)),
            ("VDI", "destroy") => self.vdi_destroy(str_param(params, 1)),
            ("VDI", "resize" | "resize_online") => self.vdi_resize(call, params),
            ("VDI", "copy") => {
                self.vdi_copy(str_param(params, 1), str_param(params, 2), false, false)
            }
            ("VDI", "pool_migrate") => self.vdi_pool_migrate(params),
            ("VBD", "create") => self.vbd_create(params.get(1)),
            ("VBD", "destroy") => self.vbd_destroy(str_param(params, 1)),
            ("VBD", "plug" | "unplug") => self.vbd_plug(str_param(params, 1), call == "plug"),
            ("VBD", "insert") => self.vbd_insert(str_param(params, 1), str_param(params, 2)),
            ("VBD", "eject") => self.vbd_eject(str_param(params, 1)),
            ("VIF", "create") => self.vif_create(params.get(1)),
            ("VIF", "destroy") => self.vif_destroy(str_param(params, 1)),
            ("VIF", "move") => self.vif_move(str_param(params, 1), str_param(params, 2)),
            ("VTPM", "create") => self.vtpm_create(str_param(params, 1)),
            ("VTPM", "destroy") => {
                let r = str_param(params, 1).to_owned();
                match self.get_str("vtpm", &r, "VM") {
                    None => api_error("HANDLE_INVALID", &["VTPM", &r]),
                    Some(vm) => {
                        self.drop_ref("vm", &vm, "VTPMs", &r);
                        self.remove("vtpm", &r);
                        Ok(json!(""))
                    }
                }
            }
            _ if call.starts_with("set_") => self.set_field(&lower, call, params),
            _ => self.map_call(&lower, call, params),
        })
    }

    /// `<class>.set_<field>`.
    fn set_field(&mut self, class: &str, call: &str, params: &[Value]) -> Reply {
        let r = str_param(params, 1).to_owned();
        let Some(rec) = self.record(class, &r) else {
            return api_error("HANDLE_INVALID", &[class, &r]);
        };
        let wanted = &call["set_".len()..];
        let field = rec
            .keys()
            .find(|k| k.as_str() == wanted)
            .or_else(|| rec.keys().find(|k| k.eq_ignore_ascii_case(wanted)))
            .cloned()
            .unwrap_or_else(|| wanted.to_owned());
        let value = params.get(2).cloned().unwrap_or(Value::Null);
        if class == "vm"
            && field == "affinity"
            && value
                .as_str()
                .is_some_and(|h| h != NULL_REF && !self.exists("host", h))
        {
            return api_error(
                "HANDLE_INVALID",
                &["host", value.as_str().unwrap_or_default()],
            );
        }
        self.modify(class, &r, |rec| {
            rec.insert(field, value);
        });
        Ok(json!(""))
    }

    /// `<class>.add_to_<map>` / `remove_from_<map>`.
    fn map_call(&mut self, class: &str, call: &str, params: &[Value]) -> Reply {
        let r = str_param(params, 1).to_owned();
        let key = str_param(params, 2).to_owned();
        let (add, map) = match call.strip_prefix("add_to_") {
            Some(m) => (true, m),
            None => (false, call.trim_start_matches("remove_from_")),
        };
        let Some(rec) = self.record(class, &r) else {
            return api_error("HANDLE_INVALID", &[class, &r]);
        };
        let field = rec
            .keys()
            .find(|k| k.eq_ignore_ascii_case(map))
            .cloned()
            .unwrap_or_else(|| map.to_owned());
        let present = rec
            .get(&field)
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key(&key));
        if add && present {
            return api_error("MAP_DUPLICATE_KEY", &[class, &field, &r, &key]);
        }
        let value = str_param(params, 3).to_owned();
        self.modify(class, &r, |rec| {
            let m = rec.entry(field).or_insert_with(|| json!({}));
            if let Value::Object(m) = m {
                if add {
                    m.insert(key, json!(value));
                } else {
                    m.remove(&key);
                }
            }
        });
        Ok(json!(""))
    }

    fn vm_check(&self, vm: &str, op: &str) -> Result<&Map<String, Value>, (String, Vec<String>)> {
        let Some(rec) = self.record("vm", vm) else {
            return Err(("HANDLE_INVALID".into(), vec!["VM".into(), vm.into()]));
        };
        let allowed = rec
            .get("allowed_operations")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|o| o.as_str() == Some(op)));
        if !allowed {
            let state = rec
                .get("power_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(op, "clone" | "copy" | "destroy" | "make_into_template")
                && state != "Halted"
            {
                return Err((
                    "VM_BAD_POWER_STATE".into(),
                    vec![vm.into(), "halted".into(), state.to_lowercase()],
                ));
            }
            return Err(("OPERATION_NOT_ALLOWED".into(), vec![op.into()]));
        }
        Ok(rec)
    }

    /// A new VDI like `vdi` in `sr` (`snapshot`: a snapshot of it).
    fn clone_vdi(&mut self, vdi: &str, sr: Option<&str>, snapshot: bool) -> Option<String> {
        let mut rec = self.record("vdi", vdi)?.clone();
        let (r, uuid) = self.new_ref("vdi");
        let sr = sr
            .map(str::to_owned)
            .or_else(|| rec.get("SR").and_then(Value::as_str).map(str::to_owned))?;
        rec.insert("uuid".into(), json!(uuid));
        rec.insert("SR".into(), json!(sr));
        rec.insert("VBDs".into(), json!([]));
        rec.insert("is_a_snapshot".into(), json!(snapshot));
        rec.insert(
            "snapshot_of".into(),
            json!(if snapshot { vdi } else { NULL_REF }),
        );
        rec.insert("snapshots".into(), json!([]));
        self.insert("vdi", &r, Value::Object(rec));
        self.push_ref("sr", &sr, "VDIs", &r);
        if snapshot {
            self.push_ref("vdi", vdi, "snapshots", &r);
        }
        Some(r)
    }

    /// A VBD like `vbd` for `vm` on `vdi`.
    fn clone_vbd(&mut self, vbd: &Map<String, Value>, vm: &str, vdi: Option<&str>) {
        let mut rec = vbd.clone();
        let (r, uuid) = self.new_ref("vbd");
        rec.insert("uuid".into(), json!(uuid));
        rec.insert("VM".into(), json!(vm));
        rec.insert("VDI".into(), json!(vdi.unwrap_or(NULL_REF)));
        rec.insert("empty".into(), json!(vdi.is_none()));
        rec.insert("currently_attached".into(), json!(false));
        rec.insert("device".into(), json!(""));
        self.insert("vbd", &r, Value::Object(rec));
        self.push_ref("vm", vm, "VBDs", &r);
        if let Some(vdi) = vdi {
            self.push_ref("vdi", vdi, "VBDs", &r);
        }
        self.refresh_vbd_ops(&r);
    }

    /// Copies the record of `source` as a new VM, with its drives (disks
    /// cloned into `sr`, or their own SR), interfaces and vTPMs.
    fn duplicate_vm(
        &mut self,
        source: &str,
        name: &str,
        sr: Option<&str>,
        kind: Duplicate,
    ) -> Option<String> {
        let mut rec = self.record("vm", source)?.clone();
        let (r, uuid) = self.new_ref("vm");
        rec.insert("uuid".into(), json!(uuid));
        rec.insert("name_label".into(), json!(name));
        rec.insert("VBDs".into(), json!([]));
        rec.insert("VIFs".into(), json!([]));
        rec.insert("VTPMs".into(), json!([]));
        rec.insert("snapshots".into(), json!([]));
        rec.insert("consoles".into(), json!([]));
        rec.insert("current_operations".into(), json!({}));
        rec.insert("resident_on".into(), json!(NULL_REF));
        rec.insert("is_default_template".into(), json!(false));
        let running = matches!(
            rec.get("power_state").and_then(Value::as_str),
            Some("Running" | "Paused")
        );
        match kind {
            Duplicate::Copy => {
                rec.insert("is_a_snapshot".into(), json!(false));
                rec.insert("snapshot_of".into(), json!(NULL_REF));
                rec.insert("power_state".into(), json!("Halted"));
            }
            Duplicate::Snapshot { checkpoint } => {
                rec.insert("is_a_snapshot".into(), json!(true));
                rec.insert("is_a_template".into(), json!(true));
                rec.insert("snapshot_of".into(), json!(source));
                rec.insert("snapshot_time".into(), json!("20261125T12:00:00Z"));
                let state = rec
                    .get("power_state")
                    .and_then(Value::as_str)
                    .unwrap_or("Halted")
                    .to_owned();
                rec.insert(
                    "snapshot_info".into(),
                    json!({ "power-state-at-snapshot": state }),
                );
                rec.insert(
                    "power_state".into(),
                    json!(if checkpoint && running {
                        "Suspended"
                    } else {
                        "Halted"
                    }),
                );
            }
        }
        let ops = vm_allowed(&rec);
        rec.insert("allowed_operations".into(), json!(ops));
        self.insert("vm", &r, Value::Object(rec.clone()));
        let snapshot = matches!(kind, Duplicate::Snapshot { .. });
        let source_rec = self.record("vm", source)?.clone();
        for vbd in ref_list(&source_rec, "VBDs") {
            let Some(vbd_rec) = self.record("vbd", &vbd).cloned() else {
                continue;
            };
            let vdi = vbd_rec
                .get("VDI")
                .and_then(Value::as_str)
                .filter(|v| *v != NULL_REF && self.exists("vdi", v))
                .map(str::to_owned);
            let cd = vbd_rec.get("type").and_then(Value::as_str) == Some("CD");
            let new_vdi = match vdi {
                // CDs keep their ISO.
                Some(v) if cd => Some(v),
                Some(v) => {
                    let target = if cd { None } else { sr };
                    Some(self.clone_vdi(&v, target, snapshot)?)
                }
                None => None,
            };
            self.clone_vbd(&vbd_rec, &r, new_vdi.as_deref());
        }
        for vif in ref_list(&source_rec, "VIFs") {
            let Some(mut vif_rec) = self.record("vif", &vif).cloned() else {
                continue;
            };
            let (vr, vuuid) = self.new_ref("vif");
            let n = self.next_object;
            vif_rec.insert("uuid".into(), json!(vuuid));
            vif_rec.insert("VM".into(), json!(r));
            vif_rec.insert("currently_attached".into(), json!(false));
            if !snapshot {
                vif_rec.insert("MAC".into(), json!(mac(n)));
            }
            let network = vif_rec
                .get("network")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            self.insert("vif", &vr, Value::Object(vif_rec));
            self.push_ref("vm", &r, "VIFs", &vr);
            if self.exists("network", &network) {
                self.push_ref("network", &network, "VIFs", &vr);
            }
        }
        for vtpm in ref_list(&source_rec, "VTPMs") {
            if self.exists("vtpm", &vtpm) {
                let (tr, tuuid) = self.new_ref("vtpm");
                self.insert(
                    "vtpm",
                    &tr,
                    json!({"uuid": tuuid, "VM": r, "is_unique": false}),
                );
                self.push_ref("vm", &r, "VTPMs", &tr);
            }
        }
        if snapshot {
            self.push_ref("vm", source, "snapshots", &r);
        }
        Some(r)
    }

    fn vm_clone(&mut self, params: &[Value], sr: Option<String>) -> Reply {
        let (vm, name) = (str_param(params, 1).to_owned(), str_param(params, 2));
        let op = if sr.is_some() { "copy" } else { "clone" };
        self.vm_check(&vm, op)?;
        let sr = sr.filter(|s| s != NULL_REF && !s.is_empty());
        if let Some(sr) = &sr {
            if !self.exists("sr", sr) {
                return api_error("HANDLE_INVALID", &["SR", sr]);
            }
            let size: i64 = self.vm_disk_sizes(&vm);
            if size > self.sr_free(sr) {
                return api_error(
                    "SR_FULL",
                    &[&size.to_string(), &self.sr_free(sr).to_string()],
                );
            }
        }
        match self.duplicate_vm(&vm, name, sr.as_deref(), Duplicate::Copy) {
            Some(r) => Ok(json!(r)),
            None => api_error("INTERNAL_ERROR", &["clone failed"]),
        }
    }

    fn vm_disk_sizes(&self, vm: &str) -> i64 {
        let Some(rec) = self.record("vm", vm) else {
            return 0;
        };
        ref_list(rec, "VBDs")
            .iter()
            .filter_map(|b| self.record("vbd", b))
            .filter(|b| b.get("type").and_then(Value::as_str) != Some("CD"))
            .filter_map(|b| b.get("VDI").and_then(Value::as_str))
            .filter_map(|v| self.record("vdi", v))
            .map(|v| int(v, "virtual_size"))
            .sum()
    }

    /// Room left on an SR (what `VDI.create` may still allocate).
    fn sr_free(&self, sr: &str) -> i64 {
        self.record("sr", sr)
            .map(|r| int(r, "physical_size") - int(r, "virtual_allocation"))
            .unwrap_or(0)
    }

    fn vm_provision(&mut self, vm: &str) -> Reply {
        let Some(rec) = self.record("vm", vm).cloned() else {
            return api_error("HANDLE_INVALID", &["VM", vm]);
        };
        if rec.get("power_state").and_then(Value::as_str) != Some("Halted") {
            return api_error("VM_BAD_POWER_STATE", &[vm, "halted", "running"]);
        }
        let xml = rec
            .get("other_config")
            .and_then(|o| o.get("disks"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        for disk in provision_disks(&xml) {
            let sr = if disk.sr_uuid.is_empty() {
                self.default_sr()
            } else {
                self.sr_by_uuid(&disk.sr_uuid)
            };
            let Some(sr) = sr else {
                return api_error("SR_REQUIRES_UPGRADE", &["no SR for a provisioned disk"]);
            };
            if disk.size > self.sr_free(&sr) {
                return api_error(
                    "SR_FULL",
                    &[&disk.size.to_string(), &self.sr_free(&sr).to_string()],
                );
            }
            let name = format!(
                "{} {}",
                rec.get("name_label")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                disk.device
            );
            let vdi = self.new_vdi(&sr, &name, disk.size, false, false);
            self.new_vbd(
                vm,
                Some(&vdi),
                &disk.device,
                "Disk",
                "RW",
                disk.bootable,
                false,
            );
        }
        // Provisioning makes the template a VM.
        self.modify("vm", vm, |rec| {
            if let Some(Value::Object(oc)) = rec.get_mut("other_config") {
                oc.remove("disks");
            }
            rec.insert("is_a_template".into(), json!(false));
        });
        self.refresh_vm_ops(vm);
        Ok(json!(""))
    }

    pub(crate) fn new_vdi(
        &mut self,
        sr: &str,
        name: &str,
        size: i64,
        sharable: bool,
        read_only: bool,
    ) -> String {
        let (r, uuid) = self.new_ref("vdi");
        self.insert(
            "vdi",
            &r,
            json!({
                "uuid": uuid, "name_label": name, "name_description": "", "SR": sr,
                "VBDs": [], "virtual_size": size, "physical_utilisation": 0, "type": "user",
                "sharable": sharable, "read_only": read_only, "managed": true, "missing": false,
                "is_a_snapshot": false, "snapshot_of": NULL_REF, "snapshots": [],
                "sm_config": {}, "other_config": {}, "xenstore_data": {}, "tags": [],
                "allowed_operations": ["clone", "copy", "resize", "destroy", "snapshot"],
                "current_operations": {}, "storage_lock": false, "location": uuid,
                "is_tools_iso": false, "cbt_enabled": false
            }),
        );
        self.push_ref("sr", sr, "VDIs", &r);
        self.modify("sr", sr, |rec| {
            let alloc = int(rec, "virtual_allocation") + size;
            rec.insert("virtual_allocation".into(), json!(alloc));
        });
        r
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_vbd(
        &mut self,
        vm: &str,
        vdi: Option<&str>,
        userdevice: &str,
        kind: &str,
        mode: &str,
        bootable: bool,
        attached: bool,
    ) -> String {
        let (r, uuid) = self.new_ref("vbd");
        self.insert(
            "vbd",
            &r,
            json!({
                "uuid": uuid, "VM": vm, "VDI": vdi.unwrap_or(NULL_REF), "userdevice": userdevice,
                "device": if attached { device_name(userdevice) } else { String::new() },
                "currently_attached": attached, "type": kind, "mode": mode, "bootable": bootable,
                "empty": vdi.is_none(), "unpluggable": true, "allowed_operations": [],
                "current_operations": {}, "qos_algorithm_type": "", "qos_algorithm_params": {},
                "other_config": {}, "metrics": NULL_REF
            }),
        );
        self.push_ref("vm", vm, "VBDs", &r);
        if let Some(vdi) = vdi {
            self.push_ref("vdi", vdi, "VBDs", &r);
        }
        self.refresh_vbd_ops(&r);
        r
    }

    fn vm_snapshot(&mut self, params: &[Value], checkpoint: bool) -> Reply {
        let (vm, name) = (str_param(params, 1).to_owned(), str_param(params, 2));
        let op = if checkpoint { "checkpoint" } else { "snapshot" };
        let rec = self.vm_check(&vm, op)?;
        if rec.get("is_a_template").and_then(Value::as_bool) == Some(true) {
            return api_error("OPERATION_NOT_ALLOWED", &[op]);
        }
        match self.duplicate_vm(&vm, name, None, Duplicate::Snapshot { checkpoint }) {
            Some(r) => Ok(json!(r)),
            None => api_error("INTERNAL_ERROR", &["snapshot failed"]),
        }
    }

    fn vm_revert(&mut self, snapshot: &str) -> Reply {
        let Some(snap) = self.record("vm", snapshot).cloned() else {
            return api_error("HANDLE_INVALID", &["VM", snapshot]);
        };
        if snap.get("is_a_snapshot").and_then(Value::as_bool) != Some(true) {
            return api_error("VM_NOT_A_SNAPSHOT", &[snapshot]);
        }
        let vm = snap
            .get("snapshot_of")
            .and_then(Value::as_str)
            .unwrap_or(NULL_REF)
            .to_owned();
        let Some(current) = self.record("vm", &vm).cloned() else {
            return api_error("VM_SNAPSHOT_FAILED", &["the snapshot's VM is gone"]);
        };
        // The VM stops first (as XAPI does), then takes the snapshot's
        // settings and clones of its disks; its own disks go.
        let before = current
            .get("power_state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if before != "Halted" {
            self.move_vm(&vm, "Halted", None);
        }
        for vbd in ref_list(&current, "VBDs") {
            let Some(rec) = self.record("vbd", &vbd).cloned() else {
                continue;
            };
            let vdi = rec.get("VDI").and_then(Value::as_str).unwrap_or(NULL_REF);
            if rec.get("type").and_then(Value::as_str) != Some("CD") && self.exists("vdi", vdi) {
                let vdi = vdi.to_owned();
                self.destroy_vdi_record(&vdi);
            }
            self.drop_ref("vm", &vm, "VBDs", &vbd);
            self.remove("vbd", &vbd);
        }
        for vbd in ref_list(&snap, "VBDs") {
            let Some(rec) = self.record("vbd", &vbd).cloned() else {
                continue;
            };
            let vdi = rec
                .get("VDI")
                .and_then(Value::as_str)
                .filter(|v| *v != NULL_REF && self.exists("vdi", v))
                .map(str::to_owned);
            let cd = rec.get("type").and_then(Value::as_str) == Some("CD");
            let new_vdi = match vdi {
                Some(v) if cd => Some(v),
                Some(v) => self.clone_vdi(&v, None, false),
                None => None,
            };
            self.clone_vbd(&rec, &vm, new_vdi.as_deref());
        }
        let target = match snap.get("power_state").and_then(Value::as_str) {
            Some("Suspended") => "Suspended",
            _ => "Halted",
        };
        self.modify("vm", &vm, |rec| {
            for field in [
                "memory_static_min",
                "memory_dynamic_min",
                "memory_dynamic_max",
                "memory_static_max",
                "VCPUs_max",
                "VCPUs_at_startup",
                "platform",
                "HVM_boot_params",
                "name_description",
            ] {
                if let Some(v) = snap.get(field) {
                    rec.insert(field.to_owned(), v.clone());
                }
            }
            rec.insert("power_state".into(), json!(target));
        });
        let ops = crate::ops::allowed_for(target);
        self.modify("vm", &vm, |rec| {
            rec.insert("allowed_operations".into(), json!(ops));
        });
        self.sync_vbds(&vm);
        Ok(json!(""))
    }

    pub(crate) fn destroy_vdi_record(&mut self, vdi: &str) {
        let Some(rec) = self.record("vdi", vdi).cloned() else {
            return;
        };
        let sr = rec
            .get("SR")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let size = int(&rec, "virtual_size");
        self.drop_ref("sr", &sr, "VDIs", vdi);
        if self.exists("sr", &sr) {
            self.modify("sr", &sr, |r| {
                let alloc = (int(r, "virtual_allocation") - size).max(0);
                r.insert("virtual_allocation".into(), json!(alloc));
            });
        }
        if let Some(parent) = rec.get("snapshot_of").and_then(Value::as_str)
            && parent != NULL_REF
        {
            let parent = parent.to_owned();
            self.drop_ref("vdi", &parent, "snapshots", vdi);
        }
        self.remove("vdi", vdi);
    }

    pub(crate) fn vm_destroy(&mut self, vm: &str) -> Reply {
        let rec = self.vm_check(vm, "destroy")?.clone();
        for vbd in ref_list(&rec, "VBDs") {
            if let Some(vdi) = self.get_str("vbd", &vbd, "VDI") {
                self.drop_ref("vdi", &vdi, "VBDs", &vbd);
            }
            self.remove("vbd", &vbd);
        }
        for vif in ref_list(&rec, "VIFs") {
            if let Some(net) = self.get_str("vif", &vif, "network") {
                self.drop_ref("network", &net, "VIFs", &vif);
            }
            self.remove("vif", &vif);
        }
        for vtpm in ref_list(&rec, "VTPMs") {
            self.remove("vtpm", &vtpm);
        }
        if let Some(parent) = rec.get("snapshot_of").and_then(Value::as_str)
            && parent != NULL_REF
        {
            let parent = parent.to_owned();
            self.drop_ref("vm", &parent, "snapshots", vm);
        }
        // Snapshots of a destroyed VM stay, orphaned.
        for snap in ref_list(&rec, "snapshots") {
            if self.exists("vm", &snap) {
                self.modify("vm", &snap, |r| {
                    r.insert("snapshot_of".into(), json!(NULL_REF));
                });
            }
        }
        self.remove("vm", vm);
        Ok(json!(""))
    }

    fn vm_make_template(&mut self, params: &[Value]) -> Reply {
        let vm = str_param(params, 1).to_owned();
        let value = params.get(2).and_then(Value::as_bool).unwrap_or(false);
        let Some(rec) = self.record("vm", &vm) else {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        };
        let is_template = rec.get("is_a_template").and_then(Value::as_bool) == Some(true);
        if value && !is_template {
            self.vm_check(&vm, "make_into_template")?;
        }
        self.modify("vm", &vm, |rec| {
            rec.insert("is_a_template".into(), json!(value));
        });
        self.refresh_vm_ops(&vm);
        Ok(json!(""))
    }

    fn allowed_devices(&self, params: &[Value], field: &str, max: u32) -> Reply {
        let vm = str_param(params, 1);
        let Some(rec) = self.record("vm", vm) else {
            return api_error("HANDLE_INVALID", &["VM", vm]);
        };
        let (class, key) = if field == "VBDs" {
            ("vbd", "userdevice")
        } else {
            ("vif", "device")
        };
        let used: Vec<String> = ref_list(rec, field)
            .iter()
            .filter_map(|r| self.get_str(class, r, key))
            .collect();
        Ok(json!(
            (0..max)
                .map(|n| n.to_string())
                .filter(|n| !used.contains(n))
                .collect::<Vec<_>>()
        ))
    }

    fn vm_memory_limits(&mut self, params: &[Value]) -> Reply {
        let vm = str_param(params, 1).to_owned();
        let Some(rec) = self.record("vm", &vm) else {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        };
        // static_min, static_max, dynamic_min, dynamic_max (the API's
        // order), kept below as min ≤ dynamic min ≤ dynamic max ≤ max.
        let p: Vec<i64> = (2..6).map(|i| int_param(params, i).unwrap_or(0)).collect();
        let v = [p[0], p[2], p[3], p[1]];
        if !(v[0] <= v[1] && v[1] <= v[2] && v[2] <= v[3]) || v[0] <= 0 {
            return api_error(
                "MEMORY_CONSTRAINT_VIOLATION",
                &[
                    "Memory limits must satisfy: static_min <= dynamic_min <= dynamic_max <= static_max",
                ],
            );
        }
        let halted = rec.get("power_state").and_then(Value::as_str) == Some("Halted");
        if !halted
            && (int(rec, "memory_static_max") != v[3] || int(rec, "memory_static_min") != v[0])
        {
            return api_error("VM_BAD_POWER_STATE", &[&vm, "halted", "running"]);
        }
        self.modify("vm", &vm, |rec| {
            for (field, value) in [
                "memory_static_min",
                "memory_dynamic_min",
                "memory_dynamic_max",
                "memory_static_max",
            ]
            .iter()
            .zip(&v)
            {
                rec.insert((*field).to_owned(), json!(value));
            }
            rec.insert("memory_target".into(), json!(v[2]));
        });
        Ok(json!(""))
    }

    fn vm_vcpus(&mut self, call: &str, params: &[Value]) -> Reply {
        let vm = str_param(params, 1).to_owned();
        let Some(rec) = self.record("vm", &vm) else {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        };
        let n = int_param(params, 2).unwrap_or(0);
        let (max, startup) = (int(rec, "VCPUs_max"), int(rec, "VCPUs_at_startup"));
        let halted = rec.get("power_state").and_then(Value::as_str) == Some("Halted");
        if n < 1 {
            return api_error(
                "VALUE_NOT_SUPPORTED",
                &["VCPU", &n.to_string(), "must be at least 1"],
            );
        }
        let field = if call == "set_VCPUs_max" {
            if !halted {
                return api_error("VM_BAD_POWER_STATE", &[&vm, "halted", "running"]);
            }
            if n < startup {
                return api_error(
                    "VALUE_NOT_SUPPORTED",
                    &[
                        "VCPU values",
                        &n.to_string(),
                        "VCPUs_at_startup must be <= VCPUs_max",
                    ],
                );
            }
            "VCPUs_max"
        } else {
            if n > max {
                return api_error(
                    "VALUE_NOT_SUPPORTED",
                    &[
                        "VCPU values",
                        &n.to_string(),
                        "VCPUs_at_startup must be <= VCPUs_max",
                    ],
                );
            }
            "VCPUs_at_startup"
        };
        self.modify("vm", &vm, |rec| {
            rec.insert(field.to_owned(), json!(n));
        });
        Ok(json!(""))
    }

    fn vdi_create(&mut self, record: Option<&Value>) -> Reply {
        let rec = record
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        check_fields(
            &rec,
            &[
                ("type", VDI_TYPES),
                ("on_boot", &["persist", "reset"]),
                ("snapshot_time", &[]),
            ],
        )?;
        let sr = rec
            .get("SR")
            .and_then(Value::as_str)
            .unwrap_or(NULL_REF)
            .to_owned();
        let Some(sr_rec) = self.record("sr", &sr) else {
            return api_error("HANDLE_INVALID", &["SR", &sr]);
        };
        if sr_rec.get("content_type").and_then(Value::as_str) == Some("iso") {
            return api_error("SR_OPERATION_NOT_SUPPORTED", &[&sr]);
        }
        let size = int(&rec, "virtual_size");
        if size <= 0 {
            return api_error("VDI_SIZE_TOO_SMALL", &["", &size.to_string()]);
        }
        if size > self.sr_free(&sr) {
            return api_error(
                "SR_FULL",
                &[&size.to_string(), &self.sr_free(&sr).to_string()],
            );
        }
        let name = rec
            .get("name_label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let flag = |f: &str| rec.get(f).and_then(Value::as_bool).unwrap_or(false);
        let r = self.new_vdi(&sr, name, size, flag("sharable"), flag("read_only"));
        let description = rec.get("name_description").cloned().unwrap_or(json!(""));
        let other = rec.get("other_config").cloned().unwrap_or(json!({}));
        let sm = rec.get("sm_config").cloned().unwrap_or(json!({}));
        let kind = rec.get("type").cloned().unwrap_or(json!("user"));
        self.modify("vdi", &r, |v| {
            v.insert("type".into(), kind);
            v.insert("name_description".into(), description);
            v.insert("other_config".into(), other);
            v.insert("sm_config".into(), sm);
        });
        Ok(json!(r))
    }

    fn vdi_in_use(&self, vdi: &str) -> bool {
        self.record("vdi", vdi).is_some_and(|rec| {
            ref_list(rec, "VBDs").iter().any(|b| {
                self.record("vbd", b)
                    .and_then(|r| r.get("currently_attached"))
                    .and_then(Value::as_bool)
                    == Some(true)
            })
        })
    }

    fn vdi_destroy(&mut self, vdi: &str) -> Reply {
        let Some(rec) = self.record("vdi", vdi).cloned() else {
            return api_error("HANDLE_INVALID", &["VDI", vdi]);
        };
        if self.vdi_in_use(vdi) {
            return api_error("VDI_IN_USE", &[vdi, "destroy"]);
        }
        // Detached VBDs go with it, as in XAPI.
        for vbd in ref_list(&rec, "VBDs") {
            if let Some(vm) = self.get_str("vbd", &vbd, "VM") {
                self.drop_ref("vm", &vm, "VBDs", &vbd);
            }
            self.remove("vbd", &vbd);
        }
        self.destroy_vdi_record(vdi);
        Ok(json!(""))
    }

    fn vdi_resize(&mut self, call: &str, params: &[Value]) -> Reply {
        let vdi = str_param(params, 1).to_owned();
        let Some(rec) = self.record("vdi", &vdi) else {
            return api_error("HANDLE_INVALID", &["VDI", &vdi]);
        };
        let size = int_param(params, 2).unwrap_or(0);
        let current = int(rec, "virtual_size");
        let sr = rec
            .get("SR")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let attached = self.vdi_in_use(&vdi);
        // `resize` works on attached disks too (XAPI 25 has no
        // `resize_online`); the old call still needs one.
        if call == "resize_online" && !attached {
            return api_error("VDI_NOT_AVAILABLE", &[&vdi]);
        }
        if size < current {
            return api_error("VDI_SIZE_TOO_SMALL", &[&vdi, &current.to_string()]);
        }
        if size - current > self.sr_free(&sr) {
            return api_error(
                "SR_FULL",
                &[
                    &(size - current).to_string(),
                    &self.sr_free(&sr).to_string(),
                ],
            );
        }
        self.modify("vdi", &vdi, |r| {
            r.insert("virtual_size".into(), json!(size));
        });
        self.modify("sr", &sr, |r| {
            let alloc = int(r, "virtual_allocation") + size - current;
            r.insert("virtual_allocation".into(), json!(alloc));
        });
        Ok(json!(""))
    }

    /// `VDI.copy`: a full copy of `vdi` in `sr`.
    fn vdi_copy(&mut self, vdi: &str, sr: &str, _keep: bool, _x: bool) -> Reply {
        let Some(rec) = self.record("vdi", vdi) else {
            return api_error("HANDLE_INVALID", &["VDI", vdi]);
        };
        if !self.exists("sr", sr) {
            return api_error("HANDLE_INVALID", &["SR", sr]);
        }
        let size = int(rec, "virtual_size");
        if size > self.sr_free(sr) {
            return api_error(
                "SR_FULL",
                &[&size.to_string(), &self.sr_free(sr).to_string()],
            );
        }
        let r = self.clone_vdi(vdi, Some(sr), false);
        if let Some(r) = &r {
            self.modify("sr", sr, |s| {
                let alloc = int(s, "virtual_allocation") + size;
                s.insert("virtual_allocation".into(), json!(alloc));
            });
            let _ = r;
        }
        r.map(|r| json!(r))
            .ok_or_else(|| ("INTERNAL_ERROR".to_owned(), vec!["copy failed".to_owned()]))
    }

    /// `VDI.pool_migrate`: storage live migration of a running VM's disk.
    fn vdi_pool_migrate(&mut self, params: &[Value]) -> Reply {
        let (vdi, sr) = (
            str_param(params, 1).to_owned(),
            str_param(params, 2).to_owned(),
        );
        let Some(rec) = self.record("vdi", &vdi).cloned() else {
            return api_error("HANDLE_INVALID", &["VDI", &vdi]);
        };
        if !self.vdi_in_use(&vdi) {
            return api_error("VDI_NOT_AVAILABLE", &[&vdi]);
        }
        if rec.get("SR").and_then(Value::as_str) == Some(sr.as_str()) {
            return api_error("VDI_LOCATION_MISSING", &[&sr, &vdi]);
        }
        let copied = self.vdi_copy(&vdi, &sr, false, false)?;
        let new_vdi = copied.as_str().unwrap_or_default().to_owned();
        for vbd in ref_list(&rec, "VBDs") {
            self.modify("vbd", &vbd, |b| {
                b.insert("VDI".into(), json!(new_vdi));
            });
            self.push_ref("vdi", &new_vdi, "VBDs", &vbd);
        }
        self.modify("vdi", &vdi, |v| {
            v.insert("VBDs".into(), json!([]));
        });
        self.destroy_vdi_record(&vdi);
        Ok(json!(new_vdi))
    }

    fn vbd_create(&mut self, record: Option<&Value>) -> Reply {
        let rec = record
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        check_fields(
            &rec,
            &[("mode", &["RO", "RW"]), ("type", &["CD", "Disk", "Floppy"])],
        )?;
        let s = |f: &str| {
            rec.get(f)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (vm, vdi, kind) = (s("VM"), s("VDI"), s("type"));
        if !self.exists("vm", &vm) {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        }
        let vdi = (vdi != NULL_REF && !vdi.is_empty()).then_some(vdi);
        if let Some(v) = &vdi {
            if !self.exists("vdi", v) {
                return api_error("HANDLE_INVALID", &["VDI", v]);
            }
            let sharable = self
                .record("vdi", v)
                .and_then(|r| r.get("sharable"))
                .and_then(Value::as_bool)
                == Some(true);
            if !sharable && self.vdi_in_use(v) && kind != "CD" {
                return api_error("VDI_IN_USE", &[v, "attach"]);
            }
        } else if kind != "CD" {
            return api_error("VBD_NOT_EMPTY", &["a disk drive needs a VDI"]);
        }
        let allowed = match self.allowed_devices(&[json!(""), json!(vm)], "VBDs", 255)? {
            Value::Array(a) => a
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let mut device = s("userdevice");
        if device == "autodetect" || device.is_empty() {
            match allowed.first() {
                Some(d) => device = d.clone(),
                None => return api_error("VM_TOO_MANY_VBDS", &[&vm]),
            }
        } else if !allowed.contains(&device) {
            return api_error("DEVICE_ALREADY_EXISTS", &[&device]);
        }
        let mode = if s("mode").is_empty() {
            "RW".to_owned()
        } else {
            s("mode")
        };
        let bootable = rec
            .get("bootable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let kind = if kind.is_empty() {
            "Disk".to_owned()
        } else {
            kind
        };
        let r = self.new_vbd(&vm, vdi.as_deref(), &device, &kind, &mode, bootable, false);
        let other = rec.get("other_config").cloned().unwrap_or(json!({}));
        self.modify("vbd", &r, |b| {
            b.insert("other_config".into(), other);
        });
        Ok(json!(r))
    }

    fn vbd_destroy(&mut self, vbd: &str) -> Reply {
        let Some(rec) = self.record("vbd", vbd).cloned() else {
            return api_error("HANDLE_INVALID", &["VBD", vbd]);
        };
        if rec.get("currently_attached").and_then(Value::as_bool) == Some(true) {
            return api_error("DEVICE_ALREADY_ATTACHED", &[vbd]);
        }
        let s = |f: &str| {
            rec.get(f)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        self.drop_ref("vm", &s("VM"), "VBDs", vbd);
        self.drop_ref("vdi", &s("VDI"), "VBDs", vbd);
        self.remove("vbd", vbd);
        Ok(json!(""))
    }

    fn vbd_plug(&mut self, vbd: &str, plug: bool) -> Reply {
        let Some(rec) = self.record("vbd", vbd).cloned() else {
            return api_error("HANDLE_INVALID", &["VBD", vbd]);
        };
        let vm = rec
            .get("VM")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !matches!(self.vm_power(&vm).as_str(), "Running") {
            return api_error(
                "VM_BAD_POWER_STATE",
                &[&vm, "running", &self.vm_power(&vm).to_lowercase()],
            );
        }
        let attached = rec.get("currently_attached").and_then(Value::as_bool) == Some(true);
        if plug == attached {
            return api_error(
                if plug {
                    "DEVICE_ALREADY_ATTACHED"
                } else {
                    "DEVICE_ALREADY_DETACHED"
                },
                &[vbd],
            );
        }
        let device = device_name(rec.get("userdevice").and_then(Value::as_str).unwrap_or("0"));
        self.modify("vbd", vbd, |b| {
            b.insert("currently_attached".into(), json!(plug));
            b.insert(
                "device".into(),
                json!(if plug { device } else { String::new() }),
            );
        });
        self.refresh_vbd_ops(vbd);
        Ok(json!(""))
    }

    fn vbd_insert(&mut self, vbd: &str, vdi: &str) -> Reply {
        let Some(rec) = self.record("vbd", vbd).cloned() else {
            return api_error("HANDLE_INVALID", &["VBD", vbd]);
        };
        if rec.get("type").and_then(Value::as_str) != Some("CD") {
            return api_error("VBD_NOT_REMOVABLE_MEDIA", &[vbd]);
        }
        if rec.get("empty").and_then(Value::as_bool) != Some(true) {
            return api_error("VBD_NOT_EMPTY", &[vbd]);
        }
        let Some(vdi_rec) = self.record("vdi", vdi) else {
            return api_error("HANDLE_INVALID", &["VDI", vdi]);
        };
        let sr = vdi_rec
            .get("SR")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if self.get_str("sr", sr, "content_type").as_deref() != Some("iso") {
            return api_error("VDI_INCOMPATIBLE_TYPE", &[vdi, "not an ISO"]);
        }
        self.modify("vbd", vbd, |b| {
            b.insert("VDI".into(), json!(vdi));
            b.insert("empty".into(), json!(false));
        });
        self.push_ref("vdi", vdi, "VBDs", vbd);
        self.refresh_vbd_ops(vbd);
        Ok(json!(""))
    }

    fn vbd_eject(&mut self, vbd: &str) -> Reply {
        let Some(rec) = self.record("vbd", vbd).cloned() else {
            return api_error("HANDLE_INVALID", &["VBD", vbd]);
        };
        if rec.get("type").and_then(Value::as_str) != Some("CD") {
            return api_error("VBD_NOT_REMOVABLE_MEDIA", &[vbd]);
        }
        if rec.get("empty").and_then(Value::as_bool) == Some(true) {
            return api_error("VBD_IS_EMPTY", &[vbd]);
        }
        let vdi = rec
            .get("VDI")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.drop_ref("vdi", &vdi, "VBDs", vbd);
        self.modify("vbd", vbd, |b| {
            b.insert("VDI".into(), json!(NULL_REF));
            b.insert("empty".into(), json!(true));
        });
        self.refresh_vbd_ops(vbd);
        Ok(json!(""))
    }

    /// `VIF.move`: the interface goes to another network (unplugged and
    /// plugged again by XAPI when attached; the mock just moves it).
    fn vif_move(&mut self, vif: &str, network: &str) -> Reply {
        let Some(old) = self.get_str("vif", vif, "network") else {
            return api_error("HANDLE_INVALID", &["VIF", vif]);
        };
        if !self.exists("network", network) {
            return api_error("HANDLE_INVALID", &["network", network]);
        }
        self.drop_ref("network", &old, "VIFs", vif);
        self.modify("vif", vif, |r| {
            r.insert("network".into(), json!(network));
        });
        self.push_ref("network", network, "VIFs", vif);
        Ok(json!(""))
    }

    pub(crate) fn vif_create(&mut self, record: Option<&Value>) -> Reply {
        let rec = record
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        check_fields(
            &rec,
            &[
                (
                    "locking_mode",
                    &["network_default", "locked", "unlocked", "disabled"],
                ),
                ("ipv4_configuration_mode", &["None", "Static"]),
                ("ipv6_configuration_mode", &["None", "Static"]),
            ],
        )?;
        let s = |f: &str| {
            rec.get(f)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (vm, network, device) = (s("VM"), s("network"), s("device"));
        if !self.exists("vm", &vm) {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        }
        if !self.exists("network", &network) {
            return api_error("HANDLE_INVALID", &["network", &network]);
        }
        let used = self
            .record("vm", &vm)
            .map(|r| ref_list(r, "VIFs"))
            .unwrap_or_default()
            .iter()
            .any(|v| self.get_str("vif", v, "device").as_deref() == Some(device.as_str()));
        if used {
            return api_error("DEVICE_ALREADY_EXISTS", &[&device]);
        }
        let (r, uuid) = self.new_ref("vif");
        let mac_addr = if s("MAC").is_empty() {
            mac(self.next_object)
        } else {
            s("MAC")
        };
        let mtu = rec.get("MTU").cloned().unwrap_or(json!(1500));
        self.insert(
            "vif",
            &r,
            json!({
                "uuid": uuid, "device": device, "VM": vm, "network": network, "MAC": mac_addr,
                "MTU": mtu, "currently_attached": false,
                "qos_algorithm_type": rec.get("qos_algorithm_type").cloned().unwrap_or(json!("")),
                "qos_algorithm_params": rec.get("qos_algorithm_params").cloned().unwrap_or(json!({})),
                "other_config": rec.get("other_config").cloned().unwrap_or(json!({})),
                "allowed_operations": [], "current_operations": {},
                "locking_mode": rec.get("locking_mode").cloned().unwrap_or(json!("network_default")),
                "ipv4_allowed": rec.get("ipv4_allowed").cloned().unwrap_or(json!([])),
                "ipv6_allowed": rec.get("ipv6_allowed").cloned().unwrap_or(json!([])),
                "MAC_autogenerated": s("MAC").is_empty()
            }),
        );
        self.push_ref("vm", &vm, "VIFs", &r);
        self.push_ref("network", &network, "VIFs", &r);
        self.refresh_vif_ops(&r);
        Ok(json!(r))
    }

    fn vif_destroy(&mut self, vif: &str) -> Reply {
        let Some(rec) = self.record("vif", vif).cloned() else {
            return api_error("HANDLE_INVALID", &["VIF", vif]);
        };
        let s = |f: &str| {
            rec.get(f)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let attached = rec
            .get("currently_attached")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if attached && self.vm_power(&s("VM")) == "Running" {
            return api_error(
                "OPERATION_NOT_ALLOWED",
                &["VIF currently attached to a running VM"],
            );
        }
        self.drop_ref("vm", &s("VM"), "VIFs", vif);
        self.drop_ref("network", &s("network"), "VIFs", vif);
        self.remove("vif", vif);
        Ok(json!(""))
    }

    fn vtpm_create(&mut self, vm: &str) -> Reply {
        let Some(rec) = self.record("vm", vm) else {
            return api_error("HANDLE_INVALID", &["VM", vm]);
        };
        if rec.get("power_state").and_then(Value::as_str) != Some("Halted") {
            return api_error("VM_BAD_POWER_STATE", &[vm, "halted", "running"]);
        }
        if !ref_list(rec, "VTPMs").is_empty() {
            return api_error("VTPM_MAX_AMOUNT_REACHED", &["1"]);
        }
        let (r, uuid) = self.new_ref("vtpm");
        self.insert(
            "vtpm",
            &r,
            json!({"uuid": uuid, "VM": vm, "is_unique": false, "is_protected": false}),
        );
        self.push_ref("vm", vm, "VTPMs", &r);
        Ok(json!(r))
    }
}

#[derive(Clone, Copy)]
enum Duplicate {
    Copy,
    Snapshot { checkpoint: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_xml() {
        let disks = provision_disks(
            r#"<provision><disk bootable="true" device="0" size="10737418240" sr="" type="system"/><disk device="1" size="5" sr="abc" bootable="false"/></provision>"#,
        );
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].size, 10737418240);
        assert!(disks[0].bootable);
        assert_eq!(disks[1].sr_uuid, "abc");
        assert!(provision_disks("<provision/>").is_empty());
    }

    /// XAPI refuses a record with an empty enum value (seen on a real
    /// XCP-ng 8.3 host with `VIF.create` and an empty `locking_mode`).
    #[test]
    fn empty_enum_values_are_refused() {
        let fields: &[(&str, &[&str])] = &[("locking_mode", &["network_default", "locked"])];
        let rec = |v: &str| json!({ "locking_mode": v }).as_object().unwrap().clone();
        assert!(check_fields(&rec("network_default"), fields).is_ok());
        let err = check_fields(&rec(""), fields).unwrap_err();
        assert_eq!(err.0, "INTERNAL_ERROR");
        assert_eq!(err.1, vec!["Failure(\"Unknown tag/contents\")".to_owned()]);
        assert!(check_fields(&rec("x"), &[("locking_mode", &[])]).is_ok());
        assert!(check_fields(&rec(""), &[("locking_mode", &[])]).is_err());
        assert_eq!(device_name("3"), "xvdd");
    }
}
