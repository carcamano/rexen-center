//! F12a on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository).
//!
//! The lab has one host and one pool, so nothing migrates there: no
//! `VM.migrate_send`, `VM.pool_migrate`, `VDI.pool_migrate`,
//! `host.migrate_receive` or `VM.assert_can_migrate` is ever called, and no
//! vApp is started or shut down. `real_f12a_vapp_and_copy_wizard` writes
//! nothing but its disposable objects, named with the [`TEST`] prefix and
//! refused if one already exists:
//!
//! 1. the VM [`TEST`] is made from the "Other install media" template with
//!    one 64 MiB disk on the SR named "Local storage" and one interface on
//!    the management network, never started;
//! 2. Copy VM... opens the wizard on it (Copy mode: Destination, then Name
//!    and Storage within the pool, which ask the server nothing) and is
//!    cancelled;
//! 3. the vApp [`TEST_VAPP`] is made with the New vApp wizard, that VM its
//!    only member, with a start order and delay;
//! 4. its order, delay and description are changed in vApp Properties;
//! 5. it is deleted from Manage vApps, then the VM with its disk.
//!
//! Should a step fail, the test's vApps and VMs are deleted. The same flow
//! runs on the mock (`f12a_real_run_on_the_mock`), which checks the calls
//! it makes (none of the migration or vApp power calls) and that nothing
//! else changes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xapi::{Ref, Sr, Vm, VmAppliance, VmPowerState};
use xapi_mock::MockServer;
use xen_core::ActionRequest;
use xen_core::provisioning::{DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif};

use super::provisioning::press;
use super::*;
use crate::cpm_strings::cpm_strings;
use crate::migrate_ui::Page;
use crate::vapp_strings::vapp_strings;

/// The prefix of everything the test makes; its VM.
const TEST: &str = "xenadmin-rs-test-f12a";
/// The test's vApp.
const TEST_VAPP: &str = "xenadmin-rs-test-f12a-vapp";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 64 << 20;

/// Calls that must never reach the server in this run.
const FORBIDDEN: &[&str] = &[
    "VM.migrate_send",
    "VM.pool_migrate",
    "VDI.pool_migrate",
    "host.migrate_receive",
    "VM.assert_can_migrate",
    "VM_appliance.start",
    "VM_appliance.shutdown",
    "VM_appliance.clean_shutdown",
    "VM_appliance.hard_shutdown",
    "VM.start",
];

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
    // Let the events that follow it in.
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

/// Settles, then presses (dialogs that change size re-centre).
fn press_settled(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    for _ in 0..4 {
        settle(h);
    }
    press(h, label);
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

fn test_vapps(h: &Harness<'_, XenAdminApp>) -> Vec<Ref<VmAppliance>> {
    conn(h)
        .model
        .vm_appliances
        .iter()
        .filter(|(_, a)| a.name_label.starts_with(TEST))
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

/// Deletes a test VM with its disks (refused for anything else).
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

/// Deletes a test vApp (refused for anything else).
fn delete_vapp(h: &mut Harness<'_, XenAdminApp>, vapp: &Ref<VmAppliance>) -> xen_core::Operation {
    {
        let c = conn(h);
        let app = &c.model.vm_appliances[vapp];
        assert!(
            app.name_label.starts_with(TEST),
            "refusing: not a test vApp"
        );
        assert!(
            app.vms.iter().all(|v| c
                .model
                .vms
                .get(v)
                .is_some_and(|r| r.name_label.starts_with(TEST))),
            "refusing: a vApp with other VMs"
        );
    }
    let before = last_op(h);
    h.state().backend.send(xen_core::Command::Action {
        connection: conn(h).id,
        request: ActionRequest::DeleteVApps {
            vapps: vec![vapp.clone()],
        },
    });
    wait_op(h, "Deleting vApp", before, Duration::from_secs(120))
}

fn f12a_run(h: &mut Harness<'_, XenAdminApp>, template: &str) -> String {
    let c = cpm_strings();
    let s = vapp_strings();
    let n = crate::net_strings::net_strings();
    let p = crate::prov_strings::prov_strings();
    let mut log = String::new();
    let cn = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST}* VM already exists"
    );
    assert!(
        test_vapps(h).is_empty(),
        "refusing: a {TEST}* vApp already exists"
    );
    assert!(
        !cn.model
            .vdis
            .values()
            .any(|v| v.name_label.starts_with(TEST)),
        "refusing: a {TEST}* disk already exists"
    );
    let templates: Vec<_> = cn
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && v.name_label == template)
        .collect();
    assert_eq!(templates.len(), 1, "the template {template:?}");
    let (template_ref, template_rec) = (templates[0].0.clone(), templates[0].1.clone());
    let host = cn.model.pool().expect("pool").1.master.clone();
    let srs: Vec<Ref<Sr>> = cn
        .model
        .srs
        .iter()
        .filter(|(_, x)| {
            x.name_label == TEST_SR
                && x.pbds
                    .iter()
                    .filter_map(|p| cn.model.pbds.get(p))
                    .any(|p| p.host == host)
        })
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(srs.len(), 1, "one {TEST_SR} on the coordinator");
    let sr = srs[0].clone();
    let management = cn
        .model
        .pifs
        .values()
        .find(|p| p.management && p.host == host)
        .map(|p| p.network.clone())
        .expect("the management network");
    let vapps_before = cn.model.vm_appliances.len();
    let vdis_on_sr = cn.model.srs[&sr].vdis.len();
    note(
        &mut log,
        format!(
            "preconditions: no {TEST}* VM, disk or vApp; template {template:?}; {TEST_SR} holds {vdis_on_sr} disks; {vapps_before} vApps"
        ),
    );
    let cid = cn.id;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. The disposable VM.
        let memory = template_rec.memory_static_max.max(256 << 20);
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST.to_owned(),
            description: "F12a vApp test (disposable)".to_owned(),
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
            boot_mode: xen_core::provisioning::default_boot_mode(&template_rec),
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
        let rec = conn(h).model.vms[&vm].clone();
        assert_eq!(rec.power_state, VmPowerState::Halted);
        assert_eq!(disks(h, &vm).len(), 1);
        note(
            &mut log,
            format!("created {TEST}: halted, 1 disk of 64 MiB on {TEST_SR}, 1 interface"),
        );

        // 2. Copy VM... opens the wizard; cancelled before any destination.
        h.state_mut().selection = Some(Selection::Vm(cid, vm.clone()));
        settle(h);
        h.get_all_by_label(strings().menu_vm)
            .next()
            .unwrap()
            .click();
        settle(h);
        press(h, p.copy_vm);
        settle(h);
        if let Some(w) = h.state().migrate.wizard.as_ref() {
            assert_eq!(w.page, Page::CopyMode);
            assert!(h.query_by_label(c.within_pool).is_some());
            assert!(h.query_by_label(c.cross_pool).is_some());
            press(h, n.next);
            assert_eq!(
                h.state().migrate.wizard.as_ref().map(|w| w.page),
                Some(Page::IntraCopy)
            );
            let name = h.state().migrate.wizard.as_ref().unwrap().copy.name.clone();
            press(h, n.cancel);
            assert!(h.state().migrate.wizard.is_none());
            note(
                &mut log,
                format!(
                    "Copy VM... opened the wizard (Copy mode: Within pool / Cross-pool), Next showed Name and Storage ({name:?}), then Cancel; nothing was asked of the server"
                ),
            );
        } else {
            // The server does not offer migrate_send: the F7 dialog opened.
            press(h, p.cancel);
            note(
                &mut log,
                "Copy VM... opened the F7 Copy dialog (no migrate_send offered); cancelled".into(),
            );
        }

        // 3. New vApp with that VM only.
        h.state_mut().ops.dialog_requests.push((
            cid,
            crate::provision_ui::DialogRequest::NewVApp(vec![vm.clone()]),
        ));
        settle(h);
        assert!(h.state().vapp.wizard.is_some());
        {
            let w = h.state_mut().vapp.wizard.as_mut().unwrap();
            w.name = TEST_VAPP.into();
            w.description = "F12a test vApp (disposable)".into();
        }
        press_settled(h, n.next);
        assert_eq!(
            h.state().vapp.wizard.as_ref().unwrap().vms.selected,
            vec![vm.clone()],
            "only the test VM"
        );
        press_settled(h, n.next);
        {
            let w = h.state_mut().vapp.wizard.as_mut().unwrap();
            assert_eq!(w.order.rows.len(), 1);
            assert_eq!(w.order.rows[0].vm, vm);
            w.order.rows[0].order = 1;
            w.order.rows[0].start_delay = 5;
        }
        press_settled(h, n.next);
        assert!(h.query_by_label(s.finish_title).is_some());
        let before = last_op(h);
        press_settled(h, n.finish);
        ok(&wait_op(
            h,
            "Creating vApp",
            before,
            Duration::from_secs(120),
        ));
        let vapps = test_vapps(h);
        assert_eq!(vapps.len(), 1);
        let vapp = vapps[0].clone();
        {
            let cc = conn(h);
            let app = &cc.model.vm_appliances[&vapp];
            assert_eq!(app.name_label, TEST_VAPP);
            assert_eq!(app.vms, vec![vm.clone()]);
            let r = &cc.model.vms[&vm];
            assert_eq!((r.order, r.start_delay, &r.appliance), (1, 5, &vapp));
            assert_eq!(r.power_state, VmPowerState::Halted);
        }
        note(
            &mut log,
            format!(
                "created vApp {TEST_VAPP} with the New vApp wizard: member {TEST} only, start order 1, delay 5 s; never started"
            ),
        );

        // 4. Properties: order, delay and description.
        h.state_mut().ops.dialog_requests.push((
            cid,
            crate::provision_ui::DialogRequest::VAppProperties(vapp.clone()),
        ));
        settle(h);
        {
            let d = h.state_mut().vapp.properties.as_mut().expect("properties");
            d.description = "F12a test vApp (edited)".into();
            d.order.rows[0].order = 2;
            d.order.rows[0].start_delay = 10;
        }
        let before = last_op(h);
        press_settled(h, s.ok);
        ok(&wait_op(
            h,
            "Saving the properties of vApp",
            before,
            Duration::from_secs(120),
        ));
        {
            let cc = conn(h);
            let app = &cc.model.vm_appliances[&vapp];
            assert_eq!(app.name_description, "F12a test vApp (edited)");
            let r = &cc.model.vms[&vm];
            assert_eq!((r.order, r.start_delay), (2, 10));
        }
        note(
            &mut log,
            "vApp Properties: description edited, start order 2, delay 10 s".into(),
        );

        // 5. Delete it from Manage vApps, then the VM.
        h.state_mut()
            .ops
            .dialog_requests
            .push((cid, crate::provision_ui::DialogRequest::ManageVApps));
        settle(h);
        press_settled(h, TEST_VAPP);
        assert_eq!(
            h.state().vapp.manage.as_ref().unwrap().selected,
            vec![vapp.clone()]
        );
        press_settled(h, s.button_delete);
        let question = i18n::fill(s.delete_one_1, &[("name", TEST_VAPP), ("count", "1")]);
        assert!(h.query_by_label(&question).is_some(), "{question}");
        let before = last_op(h);
        press_settled(h, s.yes);
        ok(&wait_op(
            h,
            "Deleting vApp",
            before,
            Duration::from_secs(120),
        ));
        assert!(test_vapps(h).is_empty());
        assert!(conn(h).model.vms[&vm].appliance.is_null());
        press_settled(h, s.button_close);
        note(
            &mut log,
            format!("deleted {TEST_VAPP} from Manage vApps; {TEST} left it"),
        );
        ok(&delete_vm(h, &vm));
        note(&mut log, format!("deleted {TEST} and its disk"));
    }));
    if outcome.is_err() {
        note(
            &mut log,
            "a step failed: removing the test vApps and VMs".into(),
        );
        for vapp in test_vapps(h) {
            let op = delete_vapp(h, &vapp);
            note(&mut log, format!("cleanup vApp: {:?}", op.status));
        }
        for vm in test_vms(h) {
            let op = delete_vm(h, &vm);
            note(&mut log, format!("cleanup VM: {:?}", op.status));
        }
    }
    step_for(h, Duration::from_secs(2), |_| false);
    let cn = conn(h);
    let left = (
        test_vms(h).len(),
        test_vapps(h).len(),
        cn.model
            .vdis
            .values()
            .filter(|v| v.name_label.starts_with(TEST))
            .count(),
    );
    note(
        &mut log,
        format!(
            "after: {} {TEST}* VMs, {} vApps, {} disks; {TEST_SR} holds {} disks (before {vdis_on_sr}); {} vApps (before {vapps_before})",
            left.0,
            left.1,
            left.2,
            cn.model.srs[&sr].vdis.len(),
            cn.model.vm_appliances.len()
        ),
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!(left, (0, 0, 0));
    assert_eq!(cn.model.srs[&sr].vdis.len(), vdis_on_sr);
    assert_eq!(cn.model.vm_appliances.len(), vapps_before);
    log
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); makes and deletes a disposable test VM and vApp"]
fn real_f12a_vapp_and_copy_wizard() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f12a_run(&mut h, "Other install media");
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock's pool: only the calls it needs (none of
/// the migration or vApp power calls), nothing else changed.
#[test]
fn f12a_real_run_on_the_mock() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    server.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    settle(&mut h);
    let snapshot = |s: &MockServer| {
        ["vm", "vdi", "sr", "network", "vm_appliance"]
            .iter()
            .flat_map(|c| s.refs(c).into_iter().map(move |r| (c.to_string(), r)))
            .map(|(c, r)| {
                let mut rec = s.record(&c, &r).unwrap();
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
    let log = f12a_run(&mut h, "Debian Bookworm 12");
    assert!(log.contains("Copy VM... opened the wizard"), "{log}");
    assert_eq!(snapshot(&server), before, "only the test's objects changed");
    let calls: Vec<String> = server.calls()[calls_before..].to_vec();
    for f in FORBIDDEN {
        assert!(
            !calls.iter().any(|c| c.trim_start_matches("Async.") == *f),
            "forbidden call {f}"
        );
    }
    let writes: HashSet<String> = calls
        .iter()
        .filter(|c| {
            let call = c.split('.').nth(1).unwrap_or_default();
            !(call.starts_with("get_") || c.starts_with("event.") || c.starts_with("session."))
        })
        .cloned()
        .collect();
    let allowed: HashSet<&str> = [
        // New VM.
        "Async.VM.clone",
        "VM.clone",
        "Async.VM.provision",
        "VM.provision",
        "VM.remove_from_other_config",
        "VM.add_to_other_config",
        "VM.set_other_config",
        "VM.set_name_label",
        "VM.set_name_description",
        "VM.set_VCPUs_max",
        "VM.set_VCPUs_at_startup",
        "VM.set_platform",
        "VM.set_memory_limits",
        "VM.set_affinity",
        "VM.set_HVM_boot_params",
        "VM.set_HVM_boot_policy",
        "VM.set_is_a_template",
        "VM.set_suspend_SR",
        "VM.add_to_platform",
        "VM.remove_from_platform",
        "VM.set_PV_bootloader",
        "VDI.create",
        "Async.VDI.create",
        "VBD.create",
        "Async.VBD.create",
        "VIF.create",
        "Async.VIF.create",
        "task.destroy",
        // New vApp, Properties, Delete.
        "Async.VM_appliance.create",
        "VM.set_appliance",
        "VM.set_order",
        "VM.set_start_delay",
        "Async.pool.sync_database",
        "VM_appliance.set_name_description",
        "VM_appliance.destroy",
        // Delete VM.
        "Async.VM.destroy",
        "VM.destroy",
        "Async.VDI.destroy",
        "VDI.destroy",
    ]
    .into_iter()
    .collect();
    let unexpected: Vec<_> = writes
        .iter()
        .filter(|c| !allowed.contains(c.as_str()))
        .collect();
    assert!(unexpected.is_empty(), "unexpected calls: {unexpected:?}");
}
