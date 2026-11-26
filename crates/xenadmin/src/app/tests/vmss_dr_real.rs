//! F12b on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository).
//!
//! `real_f12b_snapshot_schedule_and_dr_read_only` writes nothing but its
//! disposable objects, named with the [`TEST`] prefix and refused if one
//! already exists:
//!
//! 1. the VM [`TEST`] is made from the "Other install media" template with
//!    one 64 MiB disk on the SR named "Local storage", never started;
//! 2. the snapshot schedule [`TEST_POLICY`] is made with the New Snapshot
//!    Schedule wizard **without VMs**, so it is created disabled (the
//!    original enables a new schedule only when it has VMs), weekly, 2
//!    snapshots kept; the run refuses to finish the wizard if the request
//!    would make it enabled or with VMs;
//! 3. the test VM is assigned to it (Assign to Snapshot Schedule); the
//!    schedule stays disabled;
//! 4. its description, retention and time are changed in its Properties;
//! 5. it is deleted from VM Snapshot Schedules (its VM leaves it), then
//!    the VM with its disk.
//!
//! The schedule is never enabled and never run: Enable and Run Now are
//! never pressed. Disaster recovery is only looked at: the Configure
//! dialog lists the SRs that can hold the metadata
//! (`SR.assert_supports_database_replication`, a check) and is cancelled;
//! the wizard goes as far as its Storage page (the same check) and is
//! cancelled, which runs no clean-up call since it made nothing. No
//! replication, `DR_task`, `VDI.open_database`, recovery, SR introduce or
//! forget, probe or `pool.sync_database` happens. Should a step fail, the
//! test's schedules (only ones whose VMs are test VMs) and VMs are deleted.
//!
//! The same flow runs on the mock (`f12b_real_run_on_the_mock`), which
//! checks the calls it makes and that nothing else changes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xapi::{Ref, Sr, Vm, VmPowerState, Vmss, VmssFrequency};
use xapi_mock::MockServer;
use xen_core::ActionRequest;
use xen_core::provisioning::{DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif};

use super::provisioning::press;
use super::*;
use crate::dr_strings::dr_strings;
use crate::provision_ui::DialogRequest;
use crate::vmss_strings::vmss_strings;

/// The prefix of everything the test makes; its VM.
const TEST: &str = "xenadmin-rs-test-f12b";
/// The test's snapshot schedule.
const TEST_POLICY: &str = "xenadmin-rs-test-f12b-schedule";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 64 << 20;

/// Calls that must never reach the server in this run.
const FORBIDDEN: &[&str] = &[
    "VMSS.set_enabled",
    "VMSS.snapshot_now",
    "SR.enable_database_replication",
    "SR.disable_database_replication",
    "DR_task.create",
    "DR_task.destroy",
    "VDI.open_database",
    "VM.recover",
    "VM_appliance.recover",
    "VM.assert_can_be_recovered",
    "SR.introduce",
    "SR.forget",
    "SR.probe",
    "PBD.unplug",
    "pool.sync_database",
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

fn test_policies(h: &Harness<'_, XenAdminApp>) -> Vec<Ref<Vmss>> {
    conn(h)
        .model
        .vmss
        .iter()
        .filter(|(_, p)| p.name_label.starts_with(TEST))
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

/// Deletes test schedules whose VMs are all test VMs (refused otherwise).
fn delete_policies(
    h: &mut Harness<'_, XenAdminApp>,
    policies: Vec<Ref<Vmss>>,
) -> xen_core::Operation {
    {
        let c = conn(h);
        for p in &policies {
            let rec = &c.model.vmss[p];
            assert!(
                rec.name_label.starts_with(TEST),
                "refusing: not a test schedule"
            );
            assert!(
                rec.vms.iter().all(|v| c
                    .model
                    .vms
                    .get(v)
                    .is_some_and(|r| r.name_label.starts_with(TEST))),
                "refusing: a schedule with other VMs"
            );
        }
    }
    let before = last_op(h);
    h.state().backend.send(xen_core::Command::Action {
        connection: conn(h).id,
        request: ActionRequest::DeletePolicies { policies },
    });
    wait_op(h, "Delete policies", before, Duration::from_secs(120))
}

fn open(h: &mut Harness<'_, XenAdminApp>, request: DialogRequest) {
    let cid = conn(h).id;
    h.state_mut().ops.dialog_requests.push((cid, request));
    settle(h);
}

fn f12b_run(h: &mut Harness<'_, XenAdminApp>, template: &str) -> String {
    let s = vmss_strings();
    let d = dr_strings();
    let n = crate::net_strings::net_strings();
    let mut log = String::new();
    let cn = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST}* VM already exists"
    );
    assert!(
        test_policies(h).is_empty(),
        "refusing: a {TEST}* schedule already exists"
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
    let policies_before = cn.model.vmss.len();
    let vdis_on_sr = cn.model.srs[&sr].vdis.len();
    note(
        &mut log,
        format!(
            "preconditions: no {TEST}* VM, disk or schedule; template {template:?}; {TEST_SR} holds {vdis_on_sr} disks; {policies_before} snapshot schedules"
        ),
    );

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. The disposable VM.
        let memory = template_rec.memory_static_max.max(256 << 20);
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST.to_owned(),
            description: "F12b snapshot schedule test (disposable)".to_owned(),
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
        assert_eq!(conn(h).model.vms[&vm].power_state, VmPowerState::Halted);
        assert_eq!(disks(h, &vm).len(), 1);
        note(
            &mut log,
            format!("created {TEST}: halted, 1 disk of 64 MiB on {TEST_SR}, 1 interface"),
        );

        // 2. New Snapshot Schedule, without VMs: disabled.
        open(h, DialogRequest::ManagePolicies);
        assert!(h.state().vmss.manage.is_some());
        press_settled(h, s.button_new);
        assert!(h.state().vmss.wizard.is_some());
        {
            let w = h.state_mut().vmss.wizard.as_mut().unwrap();
            w.name = TEST_POLICY.into();
            w.description = "F12b test schedule (disposable, disabled)".into();
        }
        press_settled(h, n.next);
        assert!(
            h.state()
                .vmss
                .wizard
                .as_ref()
                .unwrap()
                .vms
                .selected
                .is_empty()
        );
        press_settled(h, n.next);
        press_settled(h, n.next);
        assert!(
            step_for(h, Duration::from_secs(30), |h| h
                .query_by_label(s.weekly)
                .is_some()),
            "the server's time"
        );
        press(h, s.weekly);
        {
            let w = h.state_mut().vmss.wizard.as_mut().unwrap();
            assert_eq!(w.schedule.frequency, VmssFrequency::Weekly);
            // Sunday 03:00 (this console's time, UTC).
            w.schedule.days = vec![0];
            w.schedule.weekly = (3, 0);
            w.schedule.retention = 2;
        }
        settle(h);
        press_settled(h, n.next);
        assert!(h.query_by_label(s.finish_title).is_some());
        {
            let w = h.state().vmss.wizard.as_ref().unwrap();
            assert!(!w.run_now);
            match w.request() {
                ActionRequest::NewPolicy(spec) => {
                    assert!(!spec.enabled, "refusing: the schedule would be enabled");
                    assert!(
                        spec.vms.is_empty() && !spec.run_now,
                        "refusing: VMs or run now"
                    );
                    assert!(!spec.schedule.is_empty(), "the server's schedule");
                    note(
                        &mut log,
                        format!(
                            "New Snapshot Schedule request: disabled, no VMs, weekly, schedule {:?} (server time), 2 kept",
                            {
                                let mut v: Vec<_> = spec.schedule.iter().collect();
                                v.sort();
                                v
                            }
                        ),
                    );
                }
                other => panic!("{other:?}"),
            }
        }
        let before = last_op(h);
        press_settled(h, n.finish);
        ok(&wait_op(
            h,
            "Creating snapshot schedule",
            before,
            Duration::from_secs(120),
        ));
        let policies = test_policies(h);
        assert_eq!(policies.len(), 1);
        let policy = policies[0].clone();
        {
            let c = conn(h);
            let p = &c.model.vmss[&policy];
            assert!(!p.enabled, "created disabled");
            assert!(p.vms.is_empty());
            assert_eq!(p.retained_snapshots, 2);
            assert_eq!(p.frequency, VmssFrequency::Weekly);
        }
        note(
            &mut log,
            format!("created {TEST_POLICY} with the wizard: disabled, no VMs, weekly, 2 kept"),
        );

        // 3. The test VM assigned to it; still disabled.
        let before = last_op(h);
        open(
            h,
            DialogRequest::AssignPolicy {
                policy: policy.clone(),
                vms: vec![vm.clone()],
            },
        );
        ok(&wait_op(
            h,
            "Assign VM to snapshot schedule",
            before,
            Duration::from_secs(60),
        ));
        {
            let c = conn(h);
            assert_eq!(c.model.vms[&vm].snapshot_schedule, policy);
            let p = &c.model.vmss[&policy];
            assert_eq!(p.vms, vec![vm.clone()]);
            assert!(!p.enabled, "still disabled");
        }
        note(
            &mut log,
            format!("assigned {TEST} to {TEST_POLICY}; it stays disabled"),
        );

        // 4. Properties: description, retention, time.
        open(h, DialogRequest::PolicyProperties(policy.clone()));
        assert!(h.state().vmss.properties.is_some());
        // The schedule tab's time comes from the server first.
        assert!(step_for(h, Duration::from_secs(30), |h| {
            h.state()
                .vmss
                .properties
                .as_ref()
                .is_some_and(|p| p.has_time())
        }));
        {
            let p = h.state_mut().vmss.properties.as_mut().unwrap();
            p.description = "F12b test schedule (edited, disabled)".into();
            p.set_schedule(|c| {
                c.retention = 3;
                c.weekly = (4, 0);
            });
            let changes = p.changes();
            assert!(changes.frequency.is_none() && changes.members.is_none());
            assert!(changes.schedule.is_some() && changes.retained == Some(3));
        }
        let before = last_op(h);
        press_settled(h, s.ok);
        ok(&wait_op(
            h,
            "Saving the properties of snapshot schedule",
            before,
            Duration::from_secs(60),
        ));
        {
            let c = conn(h);
            let p = &c.model.vmss[&policy];
            assert_eq!(p.name_description, "F12b test schedule (edited, disabled)");
            assert_eq!(p.retained_snapshots, 3);
            assert!(!p.enabled, "still disabled");
        }
        note(
            &mut log,
            "Properties: description edited, 3 kept, the time an hour later; still disabled".into(),
        );

        // 5. Deleted from VM Snapshot Schedules; the VM leaves it.
        open(h, DialogRequest::ManagePolicies);
        press_settled(h, TEST_POLICY);
        assert_eq!(
            h.state().vmss.manage.as_ref().unwrap().selected,
            vec![policy.clone()]
        );
        press_settled(h, s.button_delete);
        let question = i18n::fill(s.delete_one, &[("name", TEST_POLICY), ("count", "1")]);
        assert!(h.query_by_label(&question).is_some(), "{question}");
        let before = last_op(h);
        press_settled(h, s.yes);
        ok(&wait_op(
            h,
            "Delete policies",
            before,
            Duration::from_secs(60),
        ));
        assert!(test_policies(h).is_empty());
        assert!(conn(h).model.vms[&vm].snapshot_schedule.is_null());
        press_settled(h, s.button_close);
        note(
            &mut log,
            format!("deleted {TEST_POLICY} from VM Snapshot Schedules; {TEST} left it"),
        );

        // 6. Disaster recovery, looked at only.
        open(h, DialogRequest::ConfigureDr);
        assert!(step_for(h, Duration::from_secs(60), |h| {
            h.state().dr.configure.as_ref().is_some_and(|d| d.checked())
        }));
        let rows = h.state().dr.configure.as_ref().unwrap().row_names(&conn(h));
        note(
            &mut log,
            format!("DR Configure: SRs that can hold the metadata: {rows:?}; cancelled"),
        );
        press_settled(h, d.cancel);
        assert!(h.state().dr.configure.is_none());
        open(h, DialogRequest::DrWizard);
        press_settled(h, d.failover);
        press_settled(h, d.next);
        press_settled(h, d.next);
        assert!(step_for(h, Duration::from_secs(60), |h| {
            h.state()
                .dr
                .wizard
                .as_ref()
                .is_some_and(|w| w.page == crate::dr_ui::Page::Storage && w.idle())
        }));
        let names = h.state().dr.wizard.as_ref().unwrap().row_names(&conn(h));
        note(
            &mut log,
            format!(
                "DR wizard: Failover, Before You Start, Locate Mirrored SRs lists {names:?}; cancelled"
            ),
        );
        press_settled(h, d.cancel);
        assert!(h.state().dr.wizard.is_none());
        ok(&delete_vm(h, &vm));
        note(&mut log, format!("deleted {TEST} and its disk"));
    }));
    if outcome.is_err() {
        note(
            &mut log,
            "a step failed: removing the test schedules and VMs".into(),
        );
        let policies = test_policies(h);
        if !policies.is_empty() {
            let op = delete_policies(h, policies);
            note(&mut log, format!("cleanup schedules: {:?}", op.status));
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
        test_policies(h).len(),
        cn.model
            .vdis
            .values()
            .filter(|v| v.name_label.starts_with(TEST))
            .count(),
    );
    note(
        &mut log,
        format!(
            "after: {} {TEST}* VMs, {} schedules, {} disks; {TEST_SR} holds {} disks (before {vdis_on_sr}); {} snapshot schedules (before {policies_before})",
            left.0,
            left.1,
            left.2,
            cn.model.srs[&sr].vdis.len(),
            cn.model.vmss.len()
        ),
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!(left, (0, 0, 0));
    assert_eq!(cn.model.srs[&sr].vdis.len(), vdis_on_sr);
    assert_eq!(cn.model.vmss.len(), policies_before);
    log
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); makes and deletes a disposable test VM and a disabled snapshot schedule"]
fn real_f12b_snapshot_schedule_and_dr_read_only() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f12b_run(&mut h, "Other install media");
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock's pool: only the calls it needs (no
/// enable, run now, replication, DR task, recovery or SR change), the
/// schedule never enabled, nothing else changed.
#[test]
fn f12b_real_run_on_the_mock() {
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
        [
            "vm",
            "vdi",
            "sr",
            "network",
            "vm_appliance",
            "vmss",
            "pool",
            "pbd",
            "host",
        ]
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
    let log = f12b_run(&mut h, "Debian Bookworm 12");
    assert!(
        log.contains("created xenadmin-rs-test-f12b-schedule with the wizard: disabled"),
        "{log}"
    );
    assert_eq!(snapshot(&server), before, "only the test's objects changed");
    assert!(server.vmss_runs().is_empty(), "the schedule never ran");
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
        // The schedule: create, assign, edit, delete.
        "Async.VMSS.create",
        "VM.set_snapshot_schedule",
        "VMSS.set_name_description",
        "VMSS.set_retained_snapshots",
        "VMSS.set_schedule",
        "VMSS.destroy",
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
    // Reads made by the run: the server's clock and the DR check.
    let reads: Vec<_> = unexpected
        .iter()
        .filter(|c| {
            !matches!(
                c.as_str(),
                "host.get_server_localtime" | "SR.assert_supports_database_replication"
            )
        })
        .collect();
    assert!(reads.is_empty(), "unexpected calls: {reads:?}");
    assert!(
        calls
            .iter()
            .any(|c| c == "SR.assert_supports_database_replication")
    );
}
