//! JSON views of the cached objects, for the read tools and the resources.
//!
//! The views are curated rather than whole records: only fields an operator
//! reads in the console's tabs, with references replaced by names and
//! UUIDs. Maps that can carry credentials (a PBD's `device_config` holds
//! CIFS and iSCSI CHAP passwords; `other_config` maps are free-form) are
//! never included.

use std::sync::Arc;

use serde_json::{Value, json};
use xapi::{
    Host, HostRecord, Network, NetworkRecord, Pool, PoolRecord, Ref, Sr, SrRecord, Vm,
    VmPowerState, VmRecord,
};
use xen_core::ConnectionModel;
use xen_core::alerts::{Alert, AlertObject, AlertPriority};
use xen_core::objects::{self, VmKind};
use xen_core::provisioning;

use crate::audit::unix_rfc3339;

/// A name and UUID pair for a referenced object (`null` when unset or not
/// cached).
fn host_ref(model: &ConnectionModel, r: &Ref<Host>) -> Value {
    match model.get(r) {
        Some(h) => json!({"uuid": h.uuid, "name": h.name_label}),
        None => Value::Null,
    }
}

fn sr_ref(model: &ConnectionModel, r: &Ref<Sr>) -> Value {
    match model.get(r) {
        Some(s) => json!({"uuid": s.uuid, "name": s.name_label}),
        None => Value::Null,
    }
}

fn vm_ref(model: &ConnectionModel, r: &Ref<Vm>) -> Value {
    match model.get(r) {
        Some(v) => json!({"uuid": v.uuid, "name": v.name_label}),
        None => Value::Null,
    }
}

fn time(dt: &xapi::DateTime) -> Value {
    dt.unix_seconds()
        .filter(|s| *s > 0)
        .map_or(Value::Null, |s| Value::String(unix_rfc3339(s)))
}

/// What kind of VM record this is, as a word.
pub fn vm_kind_name(vm: &VmRecord) -> &'static str {
    match objects::vm_kind(vm) {
        VmKind::ControlDomain => "control_domain",
        VmKind::Guest => "vm",
        VmKind::Snapshot => "snapshot",
        VmKind::UserTemplate => "template",
        VmKind::DefaultTemplate => "default_template",
    }
}

// ---- pools -----------------------------------------------------------------

pub fn pool_summary(model: &ConnectionModel, pool: &PoolRecord) -> Value {
    let guests = model
        .vms
        .values()
        .filter(|v| objects::vm_kind(v) == VmKind::Guest)
        .count();
    json!({
        "uuid": pool.uuid,
        "name": objects::pool_name(model, pool),
        "description": pool.name_description,
        "coordinator": host_ref(model, &pool.master),
        "hosts": model.hosts.len(),
        "vms": guests,
        "running_vms": model.vms.values().filter(|v| objects::vm_kind(v) == VmKind::Guest && v.power_state == VmPowerState::Running).count(),
        "srs": model.srs.values().filter(|s| objects::sr_is_shown(s, false)).count(),
        "ha_enabled": pool.ha_enabled,
        "default_sr": sr_ref(model, &pool.default_sr),
        "tags": pool.tags,
    })
}

pub fn pool_detail(model: &ConnectionModel, _r: &Ref<Pool>, pool: &PoolRecord) -> Value {
    let mut out = pool_summary(model, pool);
    let hosts: Vec<Value> = objects::sorted_hosts(model)
        .into_iter()
        .map(|(r, h)| host_summary(model, r, h))
        .collect();
    out["host_list"] = Value::Array(hosts);
    out["ha"] = json!({
        "enabled": pool.ha_enabled,
        "host_failures_to_tolerate": pool.ha_host_failures_to_tolerate,
        "plan_exists_for": pool.ha_plan_exists_for,
        "overcommitted": pool.ha_overcommitted,
    });
    out["migration_compression"] = json!(pool.migration_compression);
    out
}

// ---- hosts -----------------------------------------------------------------

pub fn host_summary(model: &ConnectionModel, r: &Ref<Host>, host: &HostRecord) -> Value {
    let memory = objects::host_memory(model, host);
    let running = host
        .resident_vms
        .iter()
        .filter_map(|v| model.get(v))
        .filter(|v| !v.is_control_domain)
        .count();
    json!({
        "uuid": host.uuid,
        "name": host.name_label,
        "address": host.address,
        "enabled": host.enabled,
        "live": objects::host_is_live(model, host),
        "maintenance_mode": objects::host_in_maintenance(host),
        "coordinator": objects::is_coordinator(model, r),
        "memory_total": memory.map(|m| m.total),
        "memory_free": memory.map(|m| m.free),
        "cpus": host.host_cpus.len(),
        "running_vms": running,
        "version": host.software_version.get("product_version"),
    })
}

pub fn host_detail(model: &ConnectionModel, r: &Ref<Host>, host: &HostRecord) -> Value {
    let mut out = host_summary(model, r, host);
    let pick = |m: &std::collections::HashMap<String, String>, keys: &[&str]| -> Value {
        Value::Object(
            keys.iter()
                .filter_map(|k| m.get(*k).map(|v| ((*k).to_owned(), json!(v))))
                .collect(),
        )
    };
    out["description"] = json!(host.name_description);
    out["hostname"] = json!(host.hostname);
    out["cpu"] = pick(
        &host.cpu_info,
        &["cpu_count", "socket_count", "vendor", "modelname", "speed"],
    );
    out["software"] = pick(
        &host.software_version,
        &[
            "product_brand",
            "product_version",
            "product_version_text",
            "build_number",
            "xapi",
            "xen",
            "linux",
        ],
    );
    out["api_version"] = json!(format!(
        "{}.{}",
        host.api_version_major, host.api_version_minor
    ));
    out["edition"] = json!(host.edition);
    out["multipathing"] = json!(host.multipathing);
    out["tags"] = json!(host.tags);
    out["memory"] = objects::host_memory(model, host).map_or(Value::Null, |m| {
        json!({
            "total": m.total,
            "free": m.free,
            "xen_and_control_domain": m.xen,
            "control_domain": m.dom0,
            "available": m.available,
        })
    });
    let mut pifs: Vec<Value> = host
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .filter(|p| objects::pif_is_physical(p) || p.management)
        .map(|p| {
            json!({
                "name": objects::pif_name(model, p),
                "device": p.device,
                "network": model.get(&p.network).map(|n| objects::network_name(model, n)),
                "mac": p.mac,
                "ip": objects::pif_friendly_ip(p),
                "management": p.management,
                "attached": p.currently_attached,
            })
        })
        .collect();
    pifs.sort_by(|a, b| a["device"].as_str().cmp(&b["device"].as_str()));
    out["nics"] = Value::Array(pifs);
    let mut srs: Vec<Value> = host
        .pbds
        .iter()
        .filter_map(|p| model.get(p))
        .filter_map(|pbd| {
            let sr = model.get(&pbd.sr)?;
            objects::sr_is_shown(sr, false).then(|| {
                json!({"uuid": sr.uuid, "name": sr.name_label, "attached": pbd.currently_attached})
            })
        })
        .collect();
    srs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out["storage"] = Value::Array(srs);
    let mut vms: Vec<(&Ref<Vm>, &Arc<VmRecord>)> = host
        .resident_vms
        .iter()
        .filter_map(|v| model.vms.get_key_value(v))
        .filter(|(_, v)| !v.is_control_domain)
        .collect();
    vms.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
    out["vms"] = Value::Array(
        vms.into_iter()
            .map(|(_, v)| json!({"uuid": v.uuid, "name": v.name_label, "power_state": v.power_state.as_str()}))
            .collect(),
    );
    out
}

// ---- VMs -------------------------------------------------------------------

fn vm_ips(model: &ConnectionModel, vm: &VmRecord) -> Vec<String> {
    let mut ips: Vec<String> = vm
        .vifs
        .iter()
        .filter_map(|v| model.get(v))
        .flat_map(|vif| objects::vif_ip_addresses(model, vm, &vif.device))
        .collect();
    ips.dedup();
    ips
}

pub fn vm_summary(model: &ConnectionModel, vm: &VmRecord) -> Value {
    json!({
        "uuid": vm.uuid,
        "name": vm.name_label,
        "kind": vm_kind_name(vm),
        "power_state": vm.power_state.as_str(),
        "host": host_ref(model, &vm.resident_on),
        "vcpus": vm.vcpus_at_startup,
        "memory": vm.memory_static_max,
        "os": objects::vm_os_name(model, vm),
        "ip_addresses": vm_ips(model, vm),
        "tags": vm.tags,
    })
}

pub fn vm_detail(model: &ConnectionModel, vm: &VmRecord) -> Value {
    let mut out = vm_summary(model, vm);
    out["description"] = json!(vm.name_description);
    out["home_server"] = objects::vm_home(model, vm).map_or(Value::Null, |h| host_ref(model, h));
    out["affinity"] = host_ref(model, &vm.affinity);
    out["vcpus"] = json!({"at_startup": vm.vcpus_at_startup, "max": vm.vcpus_max});
    out["memory"] = json!({
        "static_min": vm.memory_static_min,
        "static_max": vm.memory_static_max,
        "dynamic_min": vm.memory_dynamic_min,
        "dynamic_max": vm.memory_dynamic_max,
        "actual": objects::vm_memory_actual(model, vm),
    });
    out["boot_mode"] = json!(match provisioning::BootMode::of(vm) {
        provisioning::BootMode::Bios => "bios",
        provisioning::BootMode::Uefi => "uefi",
        provisioning::BootMode::UefiSecure => "uefi_secure_boot",
    });
    out["ha_restart_priority"] = json!(vm.ha_restart_priority);
    out["allowed_operations"] = json!(
        vm.allowed_operations
            .iter()
            .map(|o| o.as_str())
            .collect::<Vec<_>>()
    );
    out["current_operations"] = json!(
        vm.current_operations
            .values()
            .map(|o| o.as_str())
            .collect::<Vec<_>>()
    );
    if let Some(m) = model.get(&vm.metrics) {
        out["start_time"] = time(&m.start_time);
    }
    if let Some(g) = model.get(&vm.guest_metrics) {
        out["guest_tools"] = json!({
            "detected": g.pv_drivers_detected,
            "version": [
                g.pv_drivers_version.get("major"),
                g.pv_drivers_version.get("minor"),
                g.pv_drivers_version.get("micro"),
            ],
        });
    }
    let disks: Vec<Value> = provisioning::vm_disks(model, vm)
        .into_iter()
        .map(|(_, vbd, _, vdi)| {
            json!({
                "uuid": vdi.uuid,
                "name": vdi.name_label,
                "position": vbd.userdevice,
                "size": vdi.virtual_size,
                "sr": sr_ref(model, &vdi.sr),
                "read_only": vbd.mode == xapi::VbdMode::Ro,
                "attached": vbd.currently_attached,
            })
        })
        .collect();
    out["disks"] = Value::Array(disks);
    let mut vifs: Vec<Value> = vm
        .vifs
        .iter()
        .filter_map(|v| model.get(v))
        .map(|vif| {
            json!({
                "device": vif.device,
                "mac": vif.mac,
                "network": model.get(&vif.network).map(|n| objects::network_name(model, n)),
                "ip_addresses": objects::vif_ip_addresses(model, vm, &vif.device),
                "attached": vif.currently_attached,
            })
        })
        .collect();
    vifs.sort_by(|a, b| a["device"].as_str().cmp(&b["device"].as_str()));
    out["vifs"] = Value::Array(vifs);
    out["snapshots"] = Value::Array(
        provisioning::snapshots(model, vm)
            .into_iter()
            .map(|(_, s)| snapshot_summary(s))
            .collect(),
    );
    if vm.is_a_snapshot {
        out["snapshot_of"] = vm_ref(model, &vm.snapshot_of);
        out["snapshot_time"] = time(&vm.snapshot_time);
        out["with_memory"] = json!(provisioning::snapshot_has_memory(vm));
    }
    out
}

pub fn snapshot_summary(s: &VmRecord) -> Value {
    json!({
        "uuid": s.uuid,
        "name": s.name_label,
        "time": time(&s.snapshot_time),
        "with_memory": provisioning::snapshot_has_memory(s),
    })
}

// ---- SRs -------------------------------------------------------------------

fn sr_hosts(model: &ConnectionModel, sr: &SrRecord) -> Vec<Value> {
    sr.pbds
        .iter()
        .filter_map(|p| model.get(p))
        .map(|pbd| {
            json!({
                "host": host_ref(model, &pbd.host),
                "attached": pbd.currently_attached,
            })
        })
        .collect()
}

pub fn sr_summary(model: &ConnectionModel, r: &Ref<Sr>, sr: &SrRecord) -> Value {
    let default = model.pool().is_some_and(|(_, p)| p.default_sr == *r);
    json!({
        "uuid": sr.uuid,
        "name": sr.name_label,
        "type": sr.r#type,
        "content_type": sr.content_type,
        "shared": sr.shared,
        "default": default,
        "size": sr.physical_size,
        "utilisation": sr.physical_utilisation,
        "virtual_allocation": sr.virtual_allocation,
        "broken": objects::sr_is_broken(model, sr),
        "hosts": sr_hosts(model, sr),
    })
}

pub fn sr_detail(model: &ConnectionModel, r: &Ref<Sr>, sr: &SrRecord) -> Value {
    let mut out = sr_summary(model, r, sr);
    out["description"] = json!(sr.name_description);
    out["tags"] = json!(sr.tags);
    let mut vdis: Vec<Value> = sr
        .vdis
        .iter()
        .filter_map(|v| model.get(v))
        .filter(|v| objects::vdi_is_shown(v, false))
        .map(|vdi| {
            let vms: Vec<Value> = vdi
                .vbds
                .iter()
                .filter_map(|b| model.get(b))
                .map(|vbd| vm_ref(model, &vbd.vm))
                .collect();
            json!({
                "uuid": vdi.uuid,
                "name": objects::vdi_name(model, vdi),
                "size": vdi.virtual_size,
                "utilisation": vdi.physical_utilisation,
                "snapshot": vdi.is_a_snapshot,
                "vms": vms,
            })
        })
        .collect();
    vdis.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out["vdis"] = Value::Array(vdis);
    out
}

// ---- networks --------------------------------------------------------------

pub fn network_summary(model: &ConnectionModel, network: &NetworkRecord) -> Value {
    let vlan = network
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .map(|p| p.vlan)
        .find(|v| *v >= 0);
    json!({
        "uuid": network.uuid,
        "name": objects::network_name(model, network),
        "description": network.name_description,
        "bridge": network.bridge,
        "mtu": network.mtu,
        "vlan": vlan,
        "managed": network.managed,
        "pifs": network.pifs.len(),
        "vifs": network.vifs.len(),
        "tags": network.tags,
    })
}

pub fn network_detail(
    model: &ConnectionModel,
    _r: &Ref<Network>,
    network: &NetworkRecord,
) -> Value {
    let mut out = network_summary(model, network);
    out["nics"] = Value::Array(
        network
            .pifs
            .iter()
            .filter_map(|p| model.get(p))
            .map(|p| {
                json!({
                    "host": host_ref(model, &p.host),
                    "name": objects::pif_name(model, p),
                    "device": p.device,
                    "vlan": p.vlan,
                    "ip": objects::pif_friendly_ip(p),
                    "attached": p.currently_attached,
                })
            })
            .collect(),
    );
    out["vm_interfaces"] = Value::Array(
        network
            .vifs
            .iter()
            .filter_map(|v| model.get(v))
            .map(|vif| {
                json!({
                    "vm": vm_ref(model, &vif.vm),
                    "device": vif.device,
                    "mac": vif.mac,
                    "attached": vif.currently_attached,
                })
            })
            .collect(),
    );
    out
}

// ---- alerts ----------------------------------------------------------------

pub fn priority_name(p: AlertPriority) -> &'static str {
    match p {
        AlertPriority::Critical => "critical",
        AlertPriority::Major => "major",
        AlertPriority::Warning => "warning",
        AlertPriority::Minor => "minor",
        AlertPriority::Information => "information",
        AlertPriority::Unknown => "unknown",
    }
}

/// The priority's rank, 1 (critical) to 5 (information), 6 unknown.
pub fn priority_rank(p: AlertPriority) -> u8 {
    match p {
        AlertPriority::Critical => 1,
        AlertPriority::Major => 2,
        AlertPriority::Warning => 3,
        AlertPriority::Minor => 4,
        AlertPriority::Information => 5,
        AlertPriority::Unknown => 6,
    }
}

pub fn alert(model: &ConnectionModel, a: &Alert) -> Value {
    let (kind, uuid) = match &a.object {
        AlertObject::Pool(r) => ("pool", model.get(r).map(|p| p.uuid.clone())),
        AlertObject::Host(r) => ("host", model.get(r).map(|h| h.uuid.clone())),
        AlertObject::Vm(r) => ("vm", model.get(r).map(|v| v.uuid.clone())),
        AlertObject::Sr(r) => ("sr", model.get(r).map(|s| s.uuid.clone())),
        AlertObject::Other => ("other", Some(a.record.obj_uuid.clone())),
    };
    let mut out = json!({
        "uuid": a.record.uuid,
        "name": a.record.name,
        "priority": priority_name(a.priority),
        "priority_rank": priority_rank(a.priority),
        "time": time(&a.record.timestamp),
        "object": {"kind": kind, "name": a.object_name, "uuid": uuid},
        "body": a.record.body,
    });
    if let Some(alarm) = &a.alarm {
        out["alarm"] = json!({"variable": alarm.variable, "value": alarm.value, "level": alarm.level, "period_seconds": alarm.period});
    }
    out
}
