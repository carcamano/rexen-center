//! GPUs, USB pass-through, vTPMs, memory ranges and the guest agent's
//! Remote Desktop switch (F14a) in the mock, as XAPI behaves:
//!
//! * `PGPU.set_enabled_VGPU_types` (each type must be supported:
//!   `VGPU_TYPE_NOT_SUPPORTED`), keeping the types' `enabled_on_PGPUs` and
//!   the groups' `enabled_VGPU_types` in step;
//!   `GPU_group.set_allocation_algorithm`;
//! * `host.enable_display` / `disable_display` and
//!   `PGPU.enable_dom0_access` / `disable_dom0_access`: the change waits
//!   for the next reboot (`enable_on_reboot`, `disable_on_reboot`), and
//!   undoing it before then puts the value back; they return the new
//!   value;
//! * `VGPU.create(vm, group, device, other_config, type)` on a halted VM
//!   (`VM_BAD_POWER_STATE`) with a type enabled on the group
//!   (`VGPU_TYPE_NOT_ENABLED`), and `VGPU.destroy` (halted VM only);
//! * `PUSB.set_passthrough_enabled` (not while a VUSB uses the device:
//!   `USB_ALREADY_ATTACHED`); `VUSB.create(vm, group, other_config)` (the
//!   device must allow pass-through: `PASSTHROUGH_NOT_ENABLED`, and its
//!   group may have one VUSB: `USB_GROUP_CONFLICT`), `VUSB.unplug`,
//!   `VUSB.destroy` (not while attached) and `get_allowed_operations`;
//!   `VM.get_possible_hosts` (the live, enabled servers);
//! * vTPMs (created and destroyed in [`crate::provisioning`]) follow
//!   XAPI 23.11's allowed operations: a halted VM without one allows
//!   `create_vtpm`, a halted VM's vTPM allows `destroy`;
//! * `VM.set_memory_dynamic_range` (within the static range, on a halted
//!   or running VM; `MEMORY_CONSTRAINT_VIOLATION` /
//!   `VM_BAD_POWER_STATE`);
//! * `VM.call_plugin(vm, "guest-agent-operation", "request-rdp-on", {})`:
//!   the guest agent turns Remote Desktop on (`data-ts` becomes 1).
//!
//! [`crate::MockServer::add_device_fixtures`] adds the fixture of
//! `tests/fixtures/devices.json`: licences, GPUs with vGPU types and a
//! running vGPU, USB devices (one in use), and UEFI on a VM.

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// The device fixture: `{"set": {class: {ref: {field: value}}}, "add":
/// {class: {ref: record}}}`.
pub const DEVICES_JSON: &str = include_str!("../../../tests/fixtures/devices.json");

/// Whether `class.call` is handled here.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        (
            "PGPU",
            "set_enabled_VGPU_types" | "enable_dom0_access" | "disable_dom0_access"
        ) | ("GPU_group", "set_allocation_algorithm")
            | ("host", "enable_display" | "disable_display")
            | ("VGPU", "create" | "destroy")
            | ("PUSB", "set_passthrough_enabled")
            | (
                "VUSB",
                "create" | "destroy" | "unplug" | "get_allowed_operations"
            )
            | (
                "VM",
                "get_possible_hosts" | "set_memory_dynamic_range" | "call_plugin"
            )
    )
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

fn ref_params(params: &[Value], i: usize) -> Vec<String> {
    params
        .get(i)
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
    /// A device call.
    pub(crate) fn device_call(&mut self, class: &str, call: &str, params: &[Value]) -> Reply {
        let r = str_param(params, 1).to_owned();
        let lower = class.to_lowercase();
        let creates = matches!(class, "VGPU" | "VUSB") && call == "create";
        if !creates && self.record(&lower, &r).is_none() {
            return api_error("HANDLE_INVALID", &[class, &r]);
        }
        match (class, call) {
            ("PGPU", "set_enabled_VGPU_types") => {
                self.set_enabled_types(&r, &ref_params(params, 2))
            }
            ("PGPU", "enable_dom0_access" | "disable_dom0_access") => {
                let v = self.on_reboot("pgpu", &r, "dom0_access", call.starts_with("enable"));
                Ok(json!(v))
            }
            ("host", "enable_display" | "disable_display") => {
                let v = self.on_reboot("host", &r, "display", call.starts_with("enable"));
                Ok(json!(v))
            }
            ("GPU_group", "set_allocation_algorithm") => {
                let alg = str_param(params, 2);
                if !matches!(alg, "breadth_first" | "depth_first") {
                    return api_error("INVALID_VALUE", &["allocation_algorithm", alg]);
                }
                let alg = alg.to_owned();
                self.modify("gpu_group", &r, |rec| {
                    rec.insert("allocation_algorithm".into(), json!(alg));
                });
                Ok(json!(""))
            }
            ("VGPU", "create") => self.vgpu_create(params),
            ("VGPU", "destroy") => {
                let rec = self.record("vgpu", &r).cloned().unwrap_or_default();
                let vm = rec
                    .get("VM")
                    .and_then(Value::as_str)
                    .unwrap_or(NULL_REF)
                    .to_owned();
                if !self.halted(&vm) {
                    return api_error("VM_BAD_POWER_STATE", &[&vm, "halted", "running"]);
                }
                let group = rec
                    .get("GPU_group")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let t = rec
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.drop_ref("vm", &vm, "VGPUs", &r);
                self.drop_ref("gpu_group", &group, "VGPUs", &r);
                self.drop_ref("vgpu_type", &t, "VGPUs", &r);
                self.remove("vgpu", &r);
                Ok(json!(""))
            }
            ("PUSB", "set_passthrough_enabled") => {
                let on = params.get(2).and_then(Value::as_bool).unwrap_or(false);
                if !on && self.pusb_used(&r) {
                    return api_error("USB_ALREADY_ATTACHED", &[&r]);
                }
                self.modify("pusb", &r, |rec| {
                    rec.insert("passthrough_enabled".into(), json!(on));
                });
                Ok(json!(""))
            }
            ("VUSB", "create") => self.vusb_create(params),
            ("VUSB", "unplug") => {
                if self
                    .record("vusb", &r)
                    .and_then(|v| v.get("currently_attached"))
                    .and_then(Value::as_bool)
                    != Some(true)
                {
                    return api_error("DEVICE_ALREADY_DETACHED", &[&r]);
                }
                self.modify("vusb", &r, |rec| {
                    rec.insert("currently_attached".into(), json!(false));
                    rec.insert("allowed_operations".into(), json!([]));
                });
                Ok(json!(""))
            }
            ("VUSB", "get_allowed_operations") => Ok(self
                .record("vusb", &r)
                .and_then(|v| v.get("allowed_operations"))
                .cloned()
                .unwrap_or(json!([]))),
            ("VUSB", "destroy") => {
                let rec = self.record("vusb", &r).cloned().unwrap_or_default();
                if rec.get("currently_attached").and_then(Value::as_bool) == Some(true) {
                    return api_error("OPERATION_NOT_ALLOWED", &["VUSB is currently attached"]);
                }
                let vm = rec
                    .get("VM")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let group = rec
                    .get("USB_group")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.drop_ref("vm", &vm, "VUSBs", &r);
                self.drop_ref("usb_group", &group, "VUSBs", &r);
                self.remove("vusb", &r);
                Ok(json!(""))
            }
            ("VM", "get_possible_hosts") => {
                let mut hosts: Vec<String> = self
                    .objects
                    .keys()
                    .filter(|(c, _)| c == "host")
                    .map(|(_, h)| h.clone())
                    .filter(|h| {
                        self.host_live(h)
                            && self
                                .record("host", h)
                                .and_then(|r| r.get("enabled"))
                                .and_then(Value::as_bool)
                                != Some(false)
                    })
                    .collect();
                hosts.sort();
                Ok(json!(hosts))
            }
            ("VM", "set_memory_dynamic_range") => self.dynamic_range(&r, params),
            ("VM", "call_plugin") => {
                let (plugin, function) = (str_param(params, 2), str_param(params, 3));
                if plugin != "guest-agent-operation" || function != "request-rdp-on" {
                    return api_error("XENAPI_MISSING_PLUGIN", &[plugin]);
                }
                let gm = self
                    .record("vm", &r)
                    .and_then(|v| v.get("guest_metrics"))
                    .and_then(Value::as_str)
                    .unwrap_or(NULL_REF)
                    .to_owned();
                if self.record("vm_guest_metrics", &gm).is_none() {
                    return api_error("VM_MISSING_PV_DRIVERS", &[&r]);
                }
                self.modify("vm_guest_metrics", &gm, |rec| {
                    let other = rec.entry("other").or_insert_with(|| json!({}));
                    if let Value::Object(o) = other {
                        o.insert("data-ts".into(), json!("1"));
                    }
                });
                Ok(json!(""))
            }
            _ => api_error("MESSAGE_METHOD_UNKNOWN", &[&format!("{class}.{call}")]),
        }
    }

    fn halted(&self, vm: &str) -> bool {
        self.record("vm", vm)
            .and_then(|v| v.get("power_state"))
            .and_then(Value::as_str)
            == Some("Halted")
    }

    fn pusb_used(&self, pusb: &str) -> bool {
        let group = self
            .record("pusb", pusb)
            .and_then(|p| p.get("USB_group"))
            .and_then(Value::as_str)
            .unwrap_or(NULL_REF);
        self.record("usb_group", group)
            .is_some_and(|g| !refs(g, "VUSBs").is_empty())
    }

    /// The display / dom0 access toggles: the change waits for a reboot.
    fn on_reboot(&mut self, class: &str, r: &str, field: &str, enable: bool) -> String {
        let cur = self
            .record(class, r)
            .and_then(|x| x.get(field))
            .and_then(Value::as_str)
            .unwrap_or("enabled")
            .to_owned();
        let new = match (cur.as_str(), enable) {
            ("disabled", true) => "enable_on_reboot",
            ("disable_on_reboot", true) => "enabled",
            ("enabled", false) => "disable_on_reboot",
            ("enable_on_reboot", false) => "disabled",
            (other, _) => other,
        }
        .to_owned();
        let field = field.to_owned();
        let value = new.clone();
        self.modify(class, r, |rec| {
            rec.insert(field, json!(value));
        });
        new
    }

    fn set_enabled_types(&mut self, pgpu: &str, types: &[String]) -> Reply {
        let rec = self.record("pgpu", pgpu).cloned().unwrap_or_default();
        let supported = refs(&rec, "supported_VGPU_types");
        if let Some(t) = types.iter().find(|t| !supported.contains(t)) {
            return api_error("VGPU_TYPE_NOT_SUPPORTED", &[t]);
        }
        let mut list: Vec<String> = Vec::new();
        for t in types {
            if !list.contains(t) {
                list.push(t.clone());
            }
        }
        let old = refs(&rec, "enabled_VGPU_types");
        self.modify("pgpu", pgpu, |r| {
            r.insert("enabled_VGPU_types".into(), json!(list));
        });
        for t in old.iter().filter(|t| !list.contains(t)) {
            self.drop_ref("vgpu_type", t, "enabled_on_PGPUs", pgpu);
        }
        for t in list.iter().filter(|t| !old.contains(t)) {
            self.push_ref("vgpu_type", t, "enabled_on_PGPUs", pgpu);
        }
        // The group's enabled types: those of its GPUs.
        let group = rec
            .get("GPU_group")
            .and_then(Value::as_str)
            .unwrap_or(NULL_REF)
            .to_owned();
        if let Some(g) = self.record("gpu_group", &group).cloned() {
            let mut enabled: Vec<String> = Vec::new();
            for p in refs(&g, "PGPUs") {
                for t in self
                    .record("pgpu", &p)
                    .map(|r| refs(r, "enabled_VGPU_types"))
                    .unwrap_or_default()
                {
                    if !enabled.contains(&t) {
                        enabled.push(t);
                    }
                }
            }
            self.modify("gpu_group", &group, |r| {
                r.insert("enabled_VGPU_types".into(), json!(enabled));
            });
        }
        Ok(json!(""))
    }

    fn vgpu_create(&mut self, params: &[Value]) -> Reply {
        let vm = str_param(params, 1).to_owned();
        let group = str_param(params, 2).to_owned();
        let device = params
            .get(3)
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_owned();
        let other = params.get(4).cloned().unwrap_or(json!({}));
        let Some(g) = self.record("gpu_group", &group).cloned() else {
            return api_error("HANDLE_INVALID", &["GPU_group", &group]);
        };
        if self.record("vm", &vm).is_none() {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        }
        if !self.halted(&vm) {
            return api_error("VM_BAD_POWER_STATE", &[&vm, "halted", "running"]);
        }
        // No type: the group's pass-through type (the default).
        let t = match params.get(5).and_then(Value::as_str) {
            Some(t) => t.to_owned(),
            None => refs(&g, "supported_VGPU_types")
                .into_iter()
                .find(|t| {
                    self.record("vgpu_type", t)
                        .and_then(|r| r.get("model_name"))
                        .and_then(Value::as_str)
                        == Some("passthrough")
                })
                .unwrap_or_else(|| NULL_REF.into()),
        };
        if !refs(&g, "enabled_VGPU_types").contains(&t) {
            return api_error("VGPU_TYPE_NOT_ENABLED", &[&t, &group]);
        }
        let (r, uuid) = self.new_ref("vgpu");
        self.insert(
            "vgpu",
            &r,
            json!({"uuid": uuid, "VM": vm, "GPU_group": group, "device": device,
                   "currently_attached": false, "other_config": other, "type": t,
                   "resident_on": NULL_REF, "scheduled_to_be_resident_on": NULL_REF,
                   "compatibility_metadata": {}, "extra_args": "", "PCI": NULL_REF}),
        );
        self.push_ref("vm", &vm, "VGPUs", &r);
        self.push_ref("gpu_group", &group, "VGPUs", &r);
        self.push_ref("vgpu_type", &t, "VGPUs", &r);
        Ok(json!(r))
    }

    fn vusb_create(&mut self, params: &[Value]) -> Reply {
        let vm = str_param(params, 1).to_owned();
        let group = str_param(params, 2).to_owned();
        let other = params.get(3).cloned().unwrap_or(json!({}));
        if self.record("vm", &vm).is_none() {
            return api_error("HANDLE_INVALID", &["VM", &vm]);
        }
        let Some(g) = self.record("usb_group", &group).cloned() else {
            return api_error("HANDLE_INVALID", &["USB_group", &group]);
        };
        let pusbs = refs(&g, "PUSBs");
        let Some(pusb) = pusbs.first() else {
            return api_error("USB_GROUP_CONTAINS_NO_PUSBS", &[&group]);
        };
        if self
            .record("pusb", pusb)
            .and_then(|p| p.get("passthrough_enabled"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            return api_error("PASSTHROUGH_NOT_ENABLED", &[pusb]);
        }
        if !refs(&g, "VUSBs").is_empty() {
            return api_error("USB_GROUP_CONFLICT", &[&group]);
        }
        let (r, uuid) = self.new_ref("vusb");
        self.insert(
            "vusb",
            &r,
            json!({"uuid": uuid, "VM": vm, "USB_group": group, "other_config": other,
                   "currently_attached": false, "allowed_operations": [],
                   "current_operations": {}}),
        );
        self.push_ref("vm", &vm, "VUSBs", &r);
        self.push_ref("usb_group", &group, "VUSBs", &r);
        Ok(json!(r))
    }

    fn dynamic_range(&mut self, vm: &str, params: &[Value]) -> Reply {
        let rec = self.record("vm", vm).cloned().unwrap_or_default();
        let state = rec
            .get("power_state")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(state, "Halted" | "Running") {
            return api_error("VM_BAD_POWER_STATE", &[vm, "running", state]);
        }
        let int = |v: Option<&Value>| match v {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        let (dmin, dmax) = (
            int(params.get(2)).unwrap_or(0),
            int(params.get(3)).unwrap_or(0),
        );
        let (smin, smax) = (
            int(rec.get("memory_static_min")).unwrap_or(0),
            int(rec.get("memory_static_max")).unwrap_or(0),
        );
        if !(smin <= dmin && dmin <= dmax && dmax <= smax) {
            return api_error(
                "MEMORY_CONSTRAINT_VIOLATION",
                &[
                    "Memory limits must satisfy: static_min <= dynamic_min <= dynamic_max <= static_max",
                ],
            );
        }
        self.modify("vm", vm, |r| {
            r.insert("memory_dynamic_min".into(), json!(dmin));
            r.insert("memory_dynamic_max".into(), json!(dmax));
            r.insert("memory_target".into(), json!(dmax));
        });
        Ok(json!(""))
    }

    /// XAPI 23.11's vTPM operations of `vm` and its vTPMs.
    pub(crate) fn sync_vtpms(&mut self, vm: &str) {
        let Some(rec) = self.record("vm", vm).cloned() else {
            return;
        };
        let halted = rec.get("power_state").and_then(Value::as_str) == Some("Halted");
        let template = rec
            .get("is_a_template")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || rec
                .get("is_a_snapshot")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let vtpms = refs(&rec, "VTPMs");
        let allow = halted && !template && vtpms.is_empty();
        let has = rec
            .get("allowed_operations")
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|o| o.as_str() == Some("create_vtpm")));
        if allow != has {
            self.modify("vm", vm, |r| {
                let list = r.entry("allowed_operations").or_insert_with(|| json!([]));
                if let Value::Array(l) = list {
                    l.retain(|o| o.as_str() != Some("create_vtpm"));
                    if allow {
                        l.push(json!("create_vtpm"));
                    }
                }
            });
        }
        for t in vtpms {
            let ops = if halted {
                json!(["destroy"])
            } else {
                json!([])
            };
            if self
                .record("vtpm", &t)
                .and_then(|x| x.get("allowed_operations"))
                != Some(&ops)
            {
                self.modify("vtpm", &t, |r| {
                    r.insert("allowed_operations".into(), ops);
                });
            }
        }
    }
}
