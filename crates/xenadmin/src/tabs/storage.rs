//! Storage tabs: a VM's disks (`VMStoragePage`), the SRs of a host or pool
//! (`PhysicalStoragePage`) and the virtual disks of an SR (`SrStoragePage`).
//! The StorageLink "Volume" column of the original is left out.

use std::collections::HashSet;

use xapi::{Host, Ref, SrRecord, VbdMode, VbdType, VdiRecord, Vm, VmRecord};
use xen_core::format::disk_size;
use xen_core::objects;

use super::general::sr_type_name;
use super::{Ctx, Table, vm_display_name, yes_no};
use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// `VDI.SizeText()`: empty for snapshots.
fn vdi_size(vdi: &VdiRecord) -> String {
    if vdi.is_a_snapshot {
        String::new()
    } else {
        disk_size(vdi.virtual_size)
    }
}

/// The disks of a VM (CD drives excluded), by position.
pub fn vm_disks(ctx: Ctx<'_>, vm: &VmRecord) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_position,
        t.col_name,
        t.col_description,
        t.col_sr,
        t.col_size,
        t.col_read_only,
        t.col_priority,
        t.col_active,
        t.col_device_path,
    ]);
    let mut rows = Vec::new();
    for vbd in vm.vbds.iter().filter_map(|r| m.get(r)) {
        if vbd.r#type == VbdType::Cd {
            continue;
        }
        let Some(vdi) = m
            .get(&vbd.vdi)
            .filter(|v| objects::vdi_is_shown(v, ctx.show_hidden))
        else {
            continue;
        };
        let Some(sr) = m.get(&vdi.sr).filter(|sr| !objects::is_tools_sr(sr)) else {
            continue;
        };
        let priority: i64 = vbd
            .qos_algorithm_params
            .get("class")
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let priority = match priority {
            0 => fill(t.priority_lowest, &[("n", "0")]),
            7 => fill(t.priority_highest, &[("n", "7")]),
            n => n.to_string(),
        };
        rows.push(vec![
            vbd.userdevice.clone(),
            objects::vdi_name(m, vdi),
            vdi.name_description.clone(),
            sr.name_label.clone(),
            vdi_size(vdi),
            yes_no(vbd.mode == VbdMode::Ro),
            priority,
            yes_no(vbd.currently_attached),
            if vbd.device.is_empty() {
                t.unknown_angle.to_owned()
            } else {
                format!("/dev/{}", vbd.device)
            },
        ]);
    }
    rows.sort_by(|a, b| objects::natural_cmp(&a[0], &b[0]));
    for row in rows {
        table.push(row);
    }
    table
}

/// "{percent}% ({used} used)".
pub fn sr_usage(sr: &SrRecord) -> String {
    let percent = if sr.physical_size > 0 {
        (100.0 * sr.physical_utilisation as f64 / sr.physical_size as f64) as i64
    } else {
        0
    };
    fill(
        tab_strings().usage_value,
        &[
            ("percent", &percent.to_string()),
            ("used", &disk_size(sr.physical_utilisation)),
        ],
    )
}

/// The SRs of a host (its PBDs) or, with `host` `None`, of the whole pool.
pub fn physical_storage(ctx: Ctx<'_>, host: Option<&Ref<Host>>) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_name,
        t.col_description,
        t.col_type,
        t.col_shared,
        t.col_usage,
        t.col_size,
        t.col_virtual_allocation,
    ]);
    let pbds: Vec<&xapi::PbdRecord> = match host.and_then(|h| m.hosts.get(h)) {
        Some(host) => host
            .pbds
            .iter()
            .filter_map(|p| m.get(p))
            .map(|p| &**p)
            .collect(),
        None => m.pbds.values().map(|p| &**p).collect(),
    };
    let mut seen = HashSet::new();
    let mut srs: Vec<(&Ref<xapi::Sr>, &SrRecord)> = Vec::new();
    for pbd in pbds {
        let Some((sr_ref, sr)) = m.srs.get_key_value(&pbd.sr) else {
            continue;
        };
        if objects::is_tools_sr(sr)
            || !objects::sr_is_shown(sr, ctx.show_hidden)
            || !seen.insert(sr_ref)
        {
            continue;
        }
        srs.push((sr_ref, sr));
    }
    // The original keeps the server's PBD order; a cache has none, so sort
    // by name for a stable list.
    srs.sort_by(|(ra, a), (rb, b)| {
        objects::natural_cmp(&a.name_label, &b.name_label).then_with(|| ra.cmp(rb))
    });
    for (_, sr) in srs {
        table.push(vec![
            sr.name_label.clone(),
            sr.name_description.clone(),
            sr_type_name(ctx, sr),
            yes_no(sr.shared),
            sr_usage(sr),
            disk_size(sr.physical_size),
            disk_size(sr.virtual_allocation),
        ]);
    }
    table
}

/// The VMs using a VDI (`VDI.VMsOfVDI`): through VBDs, as a crash dump or as
/// a suspend image. Snapshots are marked.
fn vms_of_vdi(ctx: Ctx<'_>, vdi_ref: &Ref<xapi::Vdi>, vdi: &VdiRecord) -> String {
    let m = ctx.model;
    let mut vms: Vec<(&Ref<Vm>, &VmRecord)> = Vec::new();
    let mut push = |r: &Ref<Vm>| {
        if let Some((r, vm)) = m.vms.get_key_value(r)
            && objects::vm_is_shown(vm, true)
            && !vms.iter().any(|(x, _)| *x == r)
        {
            vms.push((r, vm));
        }
    };
    for vbd in vdi.vbds.iter().filter_map(|r| m.get(r)) {
        push(&vbd.vm);
    }
    for dump in vdi.crash_dumps.iter().filter_map(|r| m.get(r)) {
        push(&dump.vm);
    }
    for (r, vm) in &m.vms {
        if &vm.suspend_vdi == vdi_ref {
            push(r);
        }
    }
    vms.sort_by(|a, b| objects::vm_cmp(*a, *b));
    vms.iter()
        .map(|(_, vm)| {
            if vm.is_a_snapshot {
                fill(tab_strings().snapshot_suffix, &[("name", &vm.name_label)])
            } else {
                vm_display_name(vm)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The virtual disks of an SR.
pub fn sr_disks(ctx: Ctx<'_>, sr: &SrRecord) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_name,
        t.col_description,
        t.col_size,
        t.col_vm,
        t.col_cbt,
    ]);
    let mut vdis: Vec<(&Ref<xapi::Vdi>, &VdiRecord)> = sr
        .vdis
        .iter()
        .filter_map(|r| m.vdis.get_key_value(r))
        .map(|(r, v)| (r, &**v))
        .filter(|(_, v)| {
            objects::vdi_is_shown(v, ctx.show_hidden) && !v.sm_config.contains_key("base_mirror")
        })
        .collect();
    // The original keeps the server's order; sort by name for stability.
    vdis.sort_by(|(ra, a), (rb, b)| {
        objects::natural_cmp(&objects::vdi_name(m, a), &objects::vdi_name(m, b))
            .then_with(|| ra.cmp(rb))
    });
    for (r, vdi) in vdis {
        table.push(vec![
            objects::vdi_name(m, vdi),
            vdi.name_description.clone(),
            vdi_size(vdi),
            vms_of_vdi(ctx, r, vdi),
            if vdi.cbt_enabled {
                t.cbt_enabled
            } else {
                t.cbt_disabled
            }
            .to_owned(),
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn vm_disks_skip_cds_and_hidden_disks() {
        let m = fixture();
        let t = tab_strings();
        let web = &m.vms[&Ref::new("OpaqueRef:vm-web")];
        let table = vm_disks(ctx(&m), web);
        assert_eq!(table.column(t.col_position), ["0", "1"]);
        assert_eq!(table.column(t.col_name), ["web-01 root", "web-01 data"]);
        assert_eq!(table.column(t.col_sr), ["Local storage", "NFS VM storage"]);
        assert_eq!(table.column(t.col_size), ["20 GB", "100 GB"]);
        assert_eq!(table.column(t.col_priority), ["0 (Lowest)", "7 (Highest)"]);
        assert_eq!(table.column(t.col_device_path), ["/dev/xvda", "/dev/xvdb"]);
        assert_eq!(table.column(t.col_active), ["Yes", "Yes"]);
        let db = &m.vms[&Ref::new("OpaqueRef:vm-db")];
        let table = vm_disks(ctx(&m), db);
        assert_eq!(table.column(t.col_device_path), [t.unknown_angle]);
        assert_eq!(table.column(t.col_active), ["No"]);
        // Snapshot disks have no size.
        let snap = &m.vms[&Ref::new("OpaqueRef:vm-web-snap")];
        assert_eq!(vm_disks(ctx(&m), snap).column(t.col_size), [""]);
        let legacy = &m.vms[&Ref::new("OpaqueRef:vm-legacy")];
        assert_eq!(vm_disks(ctx(&m), legacy).column(t.col_read_only), ["Yes"]);
    }

    #[test]
    fn physical_storage_of_host_and_pool() {
        let m = fixture();
        let t = tab_strings();
        let host = Ref::new("OpaqueRef:host-0001");
        let table = physical_storage(ctx(&m), Some(&host));
        assert_eq!(
            table.column(t.col_name),
            ["ISO library", "Local storage", "NFS VM storage"]
        );
        assert_eq!(table.column(t.col_type), ["ISO", "LVM", "NFS"]);
        assert_eq!(table.column(t.col_shared), ["Yes", "No", "Yes"]);
        assert_eq!(
            table.column(t.col_usage),
            ["40% (40 GB used)", "24% (120 GB used)", "29% (600 GB used)"]
        );
        assert_eq!(table.column(t.col_size), ["100 GB", "500 GB", "2 TB"]);
        assert_eq!(table.column(t.col_virtual_allocation)[2], "158 GB");
        let pool = physical_storage(ctx(&m), None);
        assert_eq!(pool.rows.len(), 4);
    }

    #[test]
    fn sr_disks_list_vms_and_hide_hidden_disks() {
        let m = fixture();
        let t = tab_strings();
        let nfs = &m.srs[&Ref::new("OpaqueRef:sr-nfs")];
        let table = sr_disks(ctx(&m), nfs);
        assert_eq!(table.column(t.col_name), ["db-01 root", "web-01 data"]);
        assert_eq!(table.column(t.col_vm), ["db-01", "web-01"]);
        assert_eq!(table.column(t.col_cbt), [t.cbt_disabled, t.cbt_enabled]);
        let hidden = Ctx {
            show_hidden: true,
            ..ctx(&m)
        };
        assert_eq!(sr_disks(hidden, nfs).rows.len(), 3);
        let local = &m.srs[&Ref::new("OpaqueRef:sr-local-01")];
        let table = sr_disks(ctx(&m), local);
        assert_eq!(
            table.column(t.col_vm),
            ["legacy-app", "web-01", "web-01 before upgrade (snapshot)"]
        );
        assert_eq!(table.column(t.col_size), ["8 GB", "20 GB", ""]);
    }

    #[test]
    fn real_capture_storage_tables_build() {
        let m = capture();
        let c = ctx(&m);
        let total: usize = m.srs.values().map(|sr| sr_disks(c, sr).rows.len()).sum();
        assert!(total > 0);
        for vm in m.vms.values() {
            vm_disks(c, vm);
        }
        assert!(!physical_storage(c, None).rows.is_empty());
    }
}
