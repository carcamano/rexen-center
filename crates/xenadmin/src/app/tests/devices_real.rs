//! F14a on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository).
//!
//! `real_f14a_devices_memory_vtpm_and_consoles` reads everything and
//! writes nothing but its disposable VM, named [`TEST`] and refused if one
//! already exists:
//!
//! 1. read only: the GPU tab of the pool and the coordinator (what it
//!    shows), the USB tab (when the server has devices), the physical
//!    GPUs and the integrated GPU's state, the server Properties' GPU page
//!    (opened and cancelled), and the Memory tab's figures;
//! 2. the VM [`TEST`] is made from the template with one 64 MiB disk on
//!    "Local storage", UEFI when the template allows it, never started;
//!    its VM Properties dialog (opened, cancelled) and its memory
//!    settings dialog (opened, cancelled);
//! 3. its memory: the Memory tab's Edit..., a new fixed memory, OK
//!    (`VM.set_memory_limits`, no question: the VM is halted);
//! 4. its vTPM: the VM menu's Manage vTPMs..., Add vTPM, then Remove vTPM
//!    (asked first) — or the server's refusal, recorded;
//! 5. the external consoles: dry runs only (the halted VM offers no SSH;
//!    the command lines for a local address are shown, nothing started);
//! 6. the VM is deleted with its disk.
//!
//! No GPU, USB, host, pool, SR or network write; every physical GPU, GPU
//! group, USB device and the servers' records must come out as they went
//! in.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::Value;
use xapi::{Ref, Sr, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::ActionRequest;
use xen_core::consoles::{self, Platform};
use xen_core::devices;
use xen_core::memory::{MemUnit, MemoryField, MemoryMode};
use xen_core::provisioning::{BootMode, DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif};
use xen_core::settings::{ExternalTools, SshConsole};

use super::*;
use crate::dev_strings::dev_strings;

/// The test VM.
const TEST: &str = "xenadmin-rs-test-f14a";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 64 << 20;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

fn conn(h: &Harness<'_, XenAdminApp>) -> Arc<xen_core::ConnectionView> {
    first_conn(h).expect("connected")
}

fn step_for(
    h: &mut Harness<'_, XenAdminApp>,
    limit: Duration,
    done: impl Fn(&Harness<'_, XenAdminApp>) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        h.step();
        if done(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn last_op(h: &Harness<'_, XenAdminApp>) -> u64 {
    h.state()
        .backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0)
}

fn wait_op(
    h: &mut Harness<'_, XenAdminApp>,
    prefix: &str,
    after: u64,
    limit: Duration,
) -> xen_core::Operation {
    let found = |h: &Harness<'_, XenAdminApp>| {
        h.state()
            .backend
            .state()
            .operations
            .iter()
            .find(|o| {
                o.id > after
                    && !o.is_running()
                    && i18n::operation_title(&o.kind, &o.server).starts_with(prefix)
            })
            .map(|o| (**o).clone())
    };
    assert!(
        step_for(h, limit, |h| found(h).is_some()),
        "timed out waiting for {prefix}"
    );
    step_for(h, Duration::from_millis(600), |_| false);
    found(h).unwrap()
}

fn ok(op: &xen_core::Operation) {
    assert!(
        matches!(op.status, xen_core::OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn shows(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn press(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    settle(h);
    h.get_all_by_label(label)
        .last()
        .unwrap_or_else(|| panic!("no {label:?}"))
        .click();
    h.step();
    settle(h);
}

fn enabled(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
    h.query_all_by_label(label)
        .last()
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

fn test_vms(h: &Harness<'_, XenAdminApp>) -> Vec<Ref<Vm>> {
    conn(h)
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.name_label.starts_with(TEST))
        .map(|(r, _)| r.clone())
        .collect()
}

fn disks(h: &Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> Vec<Ref<xapi::Vdi>> {
    let c = conn(h);
    let Some(v) = c.model.vms.get(vm) else {
        return Vec::new();
    };
    v.vbds
        .iter()
        .filter_map(|b| c.model.vbds.get(b))
        .filter(|b| b.r#type != xapi::VbdType::Cd)
        .map(|b| b.vdi.clone())
        .filter(|d| c.model.vdis.contains_key(d))
        .collect()
}

/// Deletes the test VM with its disk (refused for anything else).
fn delete_vm(h: &mut Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> xen_core::Operation {
    let disks = disks(h, vm);
    {
        let c = conn(h);
        let rec = &c.model.vms[vm];
        assert!(rec.name_label.starts_with(TEST), "refusing: not a test VM");
        assert_eq!(
            rec.power_state,
            VmPowerState::Halted,
            "refusing: not halted"
        );
        assert!(rec.snapshots.is_empty(), "refusing: a VM with snapshots");
        for d in &disks {
            let vdi = &c.model.vdis[d];
            assert_eq!(c.model.srs[&vdi.sr].name_label, TEST_SR);
            assert!(
                vdi.name_label.starts_with(TEST),
                "refusing: a disk not named {TEST}*"
            );
            assert_eq!(vdi.vbds.len(), 1, "refusing: a shared disk");
        }
    }
    let before = last_op(h);
    h.state().backend.send(xen_core::Command::Action {
        connection: conn(h).id,
        request: ActionRequest::DeleteVm {
            vm: vm.clone(),
            disks,
            snapshots: Vec::new(),
        },
    });
    wait_op(h, "Deleting VM", before, Duration::from_secs(300))
}

/// The records this run must not change: the pool, the servers, their
/// physical GPUs, GPU groups, vGPU types and USB devices, as text.
fn device_records(h: &Harness<'_, XenAdminApp>) -> Vec<String> {
    let m = &conn(h).model;
    let mut out = Vec::new();
    if let Some((_, p)) = m.pool() {
        let mut oc: Vec<_> = p.other_config.iter().collect();
        oc.sort();
        let mut gui: Vec<_> = p.gui_config.iter().collect();
        gui.sort();
        out.push(format!(
            "pool {} {oc:?} {gui:?} ha {}",
            p.uuid, p.ha_enabled
        ));
    }
    for h in m.hosts.values() {
        let mut lic: Vec<_> = h.license_params.iter().collect();
        lic.sort();
        out.push(format!(
            "host {} display {:?} enabled {} pgpus {:?} pusbs {:?} {lic:?}",
            h.uuid, h.display, h.enabled, h.pgpus, h.pusbs
        ));
    }
    for p in m.pgpus.values() {
        out.push(format!(
            "pgpu {} {:?} dom0 {:?} enabled {:?} supported {:?} resident {:?}",
            p.uuid,
            p.gpu_group,
            p.dom0_access,
            p.enabled_vgpu_types,
            p.supported_vgpu_types,
            p.resident_vgpus
        ));
    }
    for g in m.gpu_groups.values() {
        out.push(format!(
            "gpu_group {} {:?} {:?} {:?}",
            g.uuid, g.allocation_algorithm, g.enabled_vgpu_types, g.vgpus
        ));
    }
    for p in m.pusbs.values() {
        out.push(format!(
            "pusb {} {} {}",
            p.uuid, p.path, p.passthrough_enabled
        ));
    }
    out.push(format!(
        "vgpus {} vusbs {} usb_groups {} vgpu_types {}",
        m.vgpus.len(),
        m.vusbs.len(),
        m.usb_groups.len(),
        m.vgpu_types.len()
    ));
    out.sort();
    out
}

/// Waits until the cache has caught up.
fn until(
    h: &mut Harness<'_, XenAdminApp>,
    what: &str,
    done: impl Fn(&xen_core::ConnectionModel) -> bool,
) {
    assert!(
        step_for(h, Duration::from_secs(60), |h| done(&conn(h).model)),
        "timed out: {what}"
    );
}

pub(super) fn f14a_run(h: &mut Harness<'_, XenAdminApp>, template: &str) -> String {
    let d = dev_strings();
    let mut log = String::new();
    let cn = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST}* VM already exists"
    );
    assert!(
        !cn.model
            .vdis
            .values()
            .any(|v| v.name_label.starts_with(TEST)),
        "refusing: a {TEST}* disk already exists"
    );
    let records_before = device_records(h);
    h.state_mut().dev.dry_run = true;
    let m = cn.model.clone();
    let (pool_ref, pool) = m.pool().map(|(r, p)| (r.clone(), p.clone())).unwrap();
    let host = pool.master.clone();

    // 1. Read only.
    note(
        &mut log,
        format!(
            "licence: restrict gpu {} vgpu {} integrated {} usb {} vtpm {} dmc {}",
            devices::forbidden(&m, devices::DeviceRestriction::Gpu),
            devices::forbidden(&m, devices::DeviceRestriction::Vgpu),
            devices::forbidden(&m, devices::DeviceRestriction::IntegratedGpu),
            devices::forbidden(&m, devices::DeviceRestriction::Usb),
            devices::forbidden(&m, devices::DeviceRestriction::Vtpm),
            devices::forbidden(&m, devices::DeviceRestriction::Dmc),
        ),
    );
    for p in m.pgpus.values() {
        note(
            &mut log,
            format!(
                "physical GPU '{}': system display device {}, dom0 access {}, supported types {}, allowed {}, running vGPUs {}",
                devices::pgpu_name(&m, p),
                p.is_system_display_device,
                p.dom0_access.as_str(),
                p.supported_vgpu_types.len(),
                p.enabled_vgpu_types.len(),
                p.resident_vgpus.len()
            ),
        );
    }
    for g in m.gpu_groups.values() {
        note(
            &mut log,
            format!(
                "GPU group '{}': {} GPUs, {} supported types, {:?}",
                devices::gpu_group_name(g),
                g.pgpus.len(),
                g.supported_vgpu_types.len(),
                g.allocation_algorithm.as_str()
            ),
        );
    }
    note(
        &mut log,
        format!(
            "gpu capability {}, vGPU capability {}, GPUs available to VMs {}, placement {:?}",
            devices::gpu_capability(&m),
            devices::vgpu_capability(&m),
            devices::gpus_available(&m),
            devices::placement_policy(&m)
        ),
    );
    let overview = if xen_core::objects::pool_is_visible(&m) {
        Selection::Pool(cn.id, pool_ref.clone())
    } else {
        Selection::Host(cn.id, host.clone())
    };
    for (what, sel, host_arg) in [
        ("pool", overview, None),
        (
            "coordinator",
            Selection::Host(cn.id, host.clone()),
            Some(&host),
        ),
    ] {
        h.state_mut().selection = Some(sel);
        settle(h);
        let shown = devices::gpu_tab_shown(&m);
        if shown {
            h.state_mut().tab = crate::tabs::Tab::Gpu;
            settle(h);
            settle(h);
            let groups = devices::gpu_tab_groups(&m, host_arg);
            let none = if host_arg.is_some() {
                d.gpu_none_host
            } else {
                d.gpu_none_pool
            };
            assert_eq!(
                shows(h, none),
                groups.is_empty(),
                "the GPU tab of the {what}"
            );
            note(
                &mut log,
                format!(
                    "GPU tab of the {what}: {} rows; placement strip shown {}; text: {}",
                    groups.len(),
                    shows(h, &fill(d.gpu_placement, &[("policy", d.policy_density)]))
                        || shows(
                            h,
                            &fill(d.gpu_placement, &[("policy", d.policy_performance)])
                        )
                        || shows(h, &fill(d.gpu_placement, &[("policy", d.policy_mixed)])),
                    if groups.is_empty() { none } else { "(rows)" }
                ),
            );
        } else {
            note(
                &mut log,
                format!("GPU tab of the {what}: not shown (licence)"),
            );
        }
    }
    let host_rec = m.hosts[&host].clone();
    let usb = devices::usb_tab_shown(&m, &host_rec);
    if usb {
        h.state_mut().tab = crate::tabs::Tab::Usb;
        settle(h);
        for (_, p) in devices::host_pusbs(&m, &host_rec) {
            note(
                &mut log,
                format!(
                    "USB device {} '{}': pass-through {}, VM {:?}",
                    p.path,
                    crate::dev_strings::pusb_text(&devices::pusb_description(p)),
                    p.passthrough_enabled,
                    devices::pusb_vm(&m, p).map(|v| v.name_label.clone())
                ),
            );
        }
    } else {
        note(
            &mut log,
            format!(
                "USB tab of the coordinator: not shown ({} USB devices)",
                host_rec.pusbs.len()
            ),
        );
    }
    let ig = devices::integrated_gpu(&m, &host);
    note(
        &mut log,
        format!(
            "integrated GPU: changeable {}, in use now {}, after reboot {}",
            devices::can_change_integrated_gpu(&m),
            ig.current,
            ig.next
        ),
    );
    // The server Properties: its GPU page, opened and cancelled.
    h.state_mut().ops.dialog_requests.push((
        cn.id,
        crate::provision_ui::DialogRequest::Properties(
            xen_core::properties::PropertiesTarget::Host(host.clone()),
        ),
    ));
    settle(h);
    let has_gpu_page = h
        .state_mut()
        .pool
        .props()
        .is_some_and(|p| p.pages.contains(&xen_core::properties::Page::Gpu));
    if has_gpu_page {
        h.state_mut()
            .pool
            .select_properties_page(xen_core::properties::Page::Gpu);
        settle(h);
        note(
            &mut log,
            format!(
                "server Properties, GPU page: shows '{}': {}",
                if ig.current {
                    d.integrated_using
                } else {
                    d.integrated_not_using
                },
                shows(
                    h,
                    if ig.current {
                        d.integrated_using
                    } else {
                        d.integrated_not_using
                    }
                )
            ),
        );
    } else {
        note(&mut log, "server Properties: no GPU page".into());
    }
    press(h, crate::pool_strings::pool_strings().cancel);
    assert!(
        h.state_mut().pool.props().is_none(),
        "the Properties dialog closed"
    );
    // The Memory tab's figures.
    h.state_mut().selection = Some(Selection::Host(cn.id, host.clone()));
    h.state_mut().tab = crate::tabs::Tab::Memory;
    settle(h);
    let ctx = crate::tabs::Ctx {
        model: &m,
        now: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        show_hidden: false,
    };
    let rows = crate::tabs::memory::memory_rows(ctx, crate::tabs::Subject::Host(&host, &host_rec));
    for row in &rows {
        match row {
            crate::tabs::memory::MemoryRow::Host {
                name,
                values,
                percent,
                ..
            } => note(
                &mut log,
                format!("Memory tab, server {name}: {values:?} {percent}"),
            ),
            crate::tabs::memory::MemoryRow::Vms { .. } => {}
        }
    }
    let vm_rows = rows
        .iter()
        .filter(|r| matches!(r, crate::tabs::memory::MemoryRow::Vms { .. }))
        .count();
    let editable = rows
        .iter()
        .filter(|r| {
            matches!(
                r,
                crate::tabs::memory::MemoryRow::Vms { editable: true, .. }
            )
        })
        .count();
    note(
        &mut log,
        format!("Memory tab: {vm_rows} VM rows, {editable} with Edit... (none pressed)"),
    );
    assert!(shows(h, &host_rec.name_label));

    // 2. The disposable VM.
    let templates: Vec<_> = m
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && v.name_label == template)
        .collect();
    assert_eq!(templates.len(), 1, "the template {template:?}");
    let (template_ref, template_rec) = (templates[0].0.clone(), templates[0].1.clone());
    let srs: Vec<Ref<Sr>> = m
        .srs
        .iter()
        .filter(|(_, x)| {
            x.name_label == TEST_SR
                && x.pbds
                    .iter()
                    .filter_map(|p| m.pbds.get(p))
                    .any(|p| p.host == host)
        })
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(srs.len(), 1, "one {TEST_SR} on the coordinator");
    let sr = srs[0].clone();
    let management = m
        .pifs
        .values()
        .find(|p| p.management && p.host == host)
        .map(|p| p.network.clone())
        .expect("the management network");
    let vdis_on_sr = m.srs[&sr].vdis.len();
    let uefi =
        xen_core::provisioning::supported_boot_modes(&template_rec).contains(&BootMode::Uefi);

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let memory = template_rec.memory_static_max.clamp(256 << 20, 1 << 30);
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST.to_owned(),
            description: "F14a memory and vTPM test (disposable)".to_owned(),
            media: InstallMedia::None,
            affinity: Some(host.clone()),
            vcpus_max: 1,
            vcpus_at_startup: 1,
            cores_per_socket: Some(1),
            memory_static_max: memory,
            memory_dynamic_min: memory,
            memory_dynamic_max: memory,
            disks: vec![NewVmDisk {
                source: DiskSource::New,
                system: false,
                userdevice: "0".to_owned(),
                name: format!("{TEST} disk"),
                description: String::new(),
                size: DISK,
                sr: sr.clone(),
                bootable: true,
            }],
            full_copy_sr: None,
            vifs: vec![NewVmVif {
                network: management.clone(),
                mac: String::new(),
            }],
            boot_mode: if uefi {
                Some(BootMode::Uefi)
            } else {
                xen_core::provisioning::default_boot_mode(&template_rec)
            },
            vtpm: false,
            start: false,
        };
        let before = last_op(h);
        {
            let cc = conn(h);
            let app = h.state_mut();
            app.ops
                .request(&cc, ActionRequest::NewVm(Box::new(spec)), &app.backend);
        }
        let op = wait_op(h, "Create VM", before, Duration::from_secs(600));
        ok(&op);
        let vm = op.created.clone().expect("the new VM");
        until(h, "the new VM", |m| m.vms.contains_key(&vm));
        let rec = conn(h).model.vms[&vm].clone();
        assert_eq!(rec.power_state, VmPowerState::Halted);
        note(
            &mut log,
            format!(
                "created {TEST}: halted, 1 disk of 64 MiB on {TEST_SR}, 1 interface, boot {:?}, memory {} MiB (static min {} MiB)",
                rec.hvm_boot_params.get("firmware"),
                rec.memory_static_max >> 20,
                rec.memory_static_min >> 20
            ),
        );

        // Its VM Properties: the pages it has (opened, cancelled).
        h.state_mut().selection = Some(Selection::Vm(conn(h).id, vm.clone()));
        h.state_mut().tab = crate::tabs::Tab::General;
        settle(h);
        press(h, crate::org_strings::org_strings().properties);
        {
            let p = h.state().org.vm_properties.as_ref().expect("VM Properties");
            note(
                &mut log,
                format!(
                    "VM Properties of {TEST}: GPU page {}, USB page {}",
                    p.gpu.is_some(),
                    p.usb.is_some()
                ),
            );
        }
        press(h, crate::org_strings::org_strings().cancel);
        assert!(h.state().org.vm_properties.is_none());

        // 3. Memory: the Memory tab's Edit..., opened and cancelled once,
        // then a new fixed memory.
        h.state_mut().tab = crate::tabs::Tab::Memory;
        settle(h);
        assert!(enabled(h, d.mem_edit), "Edit... for the halted test VM");
        press(h, d.mem_edit);
        assert!(shows(h, d.mem_title));
        press(h, d.cancel);
        assert!(h.state().dev.memory.is_none());
        press(h, d.mem_edit);
        let (mode, unit, dynamic_allowed, unavailable) = {
            let e = h
                .state()
                .dev
                .memory
                .as_ref()
                .unwrap()
                .edit
                .as_ref()
                .unwrap();
            (e.mode, e.unit, e.dynamic_allowed(), e.unavailable)
        };
        let target = rec.memory_static_max + (256 << 20);
        let shown = match unit {
            MemUnit::Mb => (target >> 20) as f64,
            MemUnit::Gb => (target as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0,
        };
        let field = if mode == MemoryMode::Advanced {
            MemoryField::StaticMax
        } else {
            MemoryField::Fixed
        };
        h.state_mut()
            .dev
            .memory
            .as_mut()
            .unwrap()
            .edit
            .as_mut()
            .unwrap()
            .set(field, shown);
        let (dmin, dmax, smax) = h
            .state()
            .dev
            .memory
            .as_ref()
            .unwrap()
            .edit
            .as_ref()
            .unwrap()
            .values();
        note(
            &mut log,
            format!(
                "memory dialog: {mode:?}, unit {unit:?}, range allowed {dynamic_allowed} ({}); asking for {dmin}/{dmax}/{smax} bytes",
                unavailable
                    .map(crate::dev_strings::dmc_text)
                    .unwrap_or_default()
            ),
        );
        let before = last_op(h);
        press(h, d.ok);
        assert!(
            h.state().dev.memory.is_none(),
            "no question for a halted VM"
        );
        let op = wait_op(
            h,
            "Changing memory settings for",
            before,
            Duration::from_secs(120),
        );
        ok(&op);
        until(h, "the new memory", |m| {
            m.vms.get(&vm).is_some_and(|v| v.memory_static_max == smax)
        });
        let after = conn(h).model.vms[&vm].clone();
        note(
            &mut log,
            format!(
                "memory set: static {}–{} MiB, dynamic {}–{} MiB (VM.set_memory_limits); still {:?}",
                after.memory_static_min >> 20,
                after.memory_static_max >> 20,
                after.memory_dynamic_min >> 20,
                after.memory_dynamic_max >> 20,
                after.power_state
            ),
        );
        assert_eq!(after.power_state, VmPowerState::Halted);

        // 4. vTPM.
        let can = devices::vtpm_command(&conn(h).model, &after);
        note(&mut log, format!("Manage vTPMs...: {can:?}"));
        if can.is_ok() {
            let id = conn(h).id;
            h.state_mut().ops.dialog_requests.push((
                id,
                crate::provision_ui::DialogRequest::ManageVtpm(vm.clone()),
            ));
            settle(h);
            assert!(shows(h, d.vtpm_none));
            let add = devices::can_add_vtpm(&conn(h).model, &after);
            note(&mut log, format!("Add vTPM: {add:?}"));
            if add.is_ok() {
                let before = last_op(h);
                press(h, d.vtpm_add);
                let op = wait_op(h, "Attaching a new vTPM", before, Duration::from_secs(120));
                match &op.status {
                    xen_core::OperationStatus::Succeeded => {
                        until(h, "the vTPM", |m| {
                            m.vms.get(&vm).is_some_and(|v| !v.vtpms.is_empty())
                        });
                        let c = conn(h);
                        let t = &c.model.vms[&vm].vtpms[0];
                        let rec = c.model.vtpms.get(t).cloned();
                        note(
                            &mut log,
                            format!(
                                "vTPM added (VTPM.create): unique {:?}, protected {:?}",
                                rec.as_ref().map(|r| r.is_unique),
                                rec.as_ref().map(|r| r.is_protected)
                            ),
                        );
                        step_for(h, Duration::from_secs(10), |h| enabled(h, d.vtpm_remove));
                        let before = last_op(h);
                        press(h, d.vtpm_remove);
                        assert!(shows(h, d.vtpm_remove_warning));
                        press(h, d.yes);
                        let op = wait_op(h, "Removing a vTPM", before, Duration::from_secs(120));
                        ok(&op);
                        until(h, "no vTPM", |m| {
                            m.vms.get(&vm).is_some_and(|v| v.vtpms.is_empty())
                        });
                        note(&mut log, "vTPM removed (VTPM.destroy)".into());
                    }
                    other => note(
                        &mut log,
                        format!("vTPM refused by the server: {other:?} (not worked around)"),
                    ),
                }
            }
            press(h, d.close);
        }

        // 5. External consoles: dry runs only.
        h.state_mut().tab = crate::tabs::Tab::Console;
        settle(h);
        note(
            &mut log,
            format!(
                "Console of the halted test VM: SSH button shown {}, Remote Desktop button shown {}",
                shows(h, d.ssh_button),
                shows(h, d.rdp_open) || shows(h, d.rdp_turn_on)
            ),
        );
        let tools = ExternalTools::default();
        let ssh = consoles::ssh_command(&tools, Platform::current(), "root", "127.0.0.1", |_| true)
            .unwrap();
        let putty = ExternalTools {
            ssh_console: SshConsole::Putty,
            putty_location: "/usr/bin/putty".into(),
            ..ExternalTools::default()
        };
        let rdp = consoles::rdp_command(
            &tools,
            Platform::current(),
            "127.0.0.1",
            "dry-run",
            &std::env::temp_dir(),
        );
        note(
            &mut log,
            format!(
                "dry run (nothing started, local address): SSH '{ssh}', PuTTY '{}', Remote Desktop '{rdp}'",
                consoles::ssh_command(&putty, Platform::Windows, "root", "127.0.0.1", |_| true)
                    .unwrap()
            ),
        );
        assert!(h.state().dev.launched.is_empty(), "nothing launched");

        // 6. The VM deleted.
        h.state_mut().selection = None;
        settle(h);
        let op = delete_vm(h, &vm);
        ok(&op);
        note(&mut log, format!("deleted {TEST} with its disk"));
    }));
    if outcome.is_err() {
        note(&mut log, "a step failed: removing the test VM".into());
        for vm in test_vms(h) {
            let op = delete_vm(h, &vm);
            note(&mut log, format!("cleanup VM: {:?}", op.status));
        }
    }
    step_for(h, Duration::from_secs(2), |_| false);
    let cn = conn(h);
    let left_vms = test_vms(h).len();
    let left_disks = cn
        .model
        .vdis
        .values()
        .filter(|v| v.name_label.starts_with(TEST))
        .count();
    let records_after = device_records(h);
    note(
        &mut log,
        format!(
            "after: {left_vms} {TEST}* VMs, {left_disks} disks; {TEST_SR} holds {} disks (before {vdis_on_sr}); pool, servers, GPUs, GPU groups and USB devices unchanged: {}",
            cn.model.srs[&sr].vdis.len(),
            records_after == records_before
        ),
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!((left_vms, left_disks), (0, 0));
    assert_eq!(records_after, records_before, "the device records");
    log
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); reads the GPUs, USB devices and memory, and changes the memory and vTPM of a disposable test VM"]
fn real_f14a_devices_memory_vtpm_and_consoles() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f14a_run(&mut h, "Other install media");
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run on the mock with the device fixture: only the test VM's
/// calls, no GPU, USB, host or pool write, nothing started, nothing else
/// changed.
#[test]
fn f14a_real_run_on_the_mock() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    server.add_device_fixtures();
    server.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| {
            c.status.is_connected() && c.rbac.is_some() && !c.model.pgpus.is_empty()
        })
    });
    settle(&mut h);
    let snapshot = |s: &MockServer| {
        [
            "vm",
            "vdi",
            "sr",
            "network",
            "pool",
            "pbd",
            "host",
            "pgpu",
            "gpu_group",
            "vgpu_type",
            "vgpu",
            "pusb",
            "usb_group",
            "vusb",
            "vtpm",
            "pci",
        ]
        .iter()
        .flat_map(|c| s.refs(c).into_iter().map(move |r| (c.to_string(), r)))
        .map(|(c, r)| {
            let mut rec: Value = s.record(&c, &r).unwrap();
            for f in [
                "physical_utilisation",
                "virtual_allocation",
                "VDIs",
                "VIFs",
                "VBDs",
            ] {
                rec.as_object_mut().unwrap().remove(f);
            }
            (c, r, rec)
        })
        .collect::<Vec<_>>()
    };
    let before = snapshot(&server);
    let calls_before = server.calls().len();
    let log = f14a_run(&mut h, "Debian Bookworm 12");
    assert!(log.contains("vTPM added (VTPM.create)"), "{log}");
    assert!(log.contains("vTPM removed (VTPM.destroy)"), "{log}");
    assert!(log.contains("memory set:"), "{log}");
    assert!(
        log.contains("pool, servers, GPUs, GPU groups and USB devices unchanged: true"),
        "{log}"
    );
    assert_eq!(snapshot(&server), before, "only the test VM changed");
    let calls: Vec<String> = server.calls()[calls_before..].to_vec();
    let forbidden = |c: &&String| {
        let c = c.trim_start_matches("Async.");
        (c.starts_with("pool.") && !c.contains(".get_"))
            || c.starts_with("PGPU.")
            || c.starts_with("GPU_group.")
            || c.starts_with("PUSB.")
            || c.starts_with("VGPU.")
            || c.starts_with("VUSB.")
            || c.starts_with("host.enable_display")
            || c.starts_with("host.disable_display")
            || c.starts_with("VM.start")
            || c == "pool.sync_database"
    };
    let bad: Vec<&String> = calls.iter().filter(forbidden).collect();
    assert!(bad.is_empty(), "forbidden calls: {bad:?}");
    let writes: Vec<&String> = calls
        .iter()
        .filter(|c| {
            c.contains("set_memory") || c.starts_with("VTPM.") || c.starts_with("Async.VTPM.")
        })
        .collect();
    // New VM sets the memory first, then the memory dialog does.
    assert_eq!(
        writes,
        vec![
            "VM.set_memory_limits",
            "VM.set_memory_limits",
            "VTPM.create",
            "VTPM.destroy"
        ],
        "the test VM's writes"
    );
}
