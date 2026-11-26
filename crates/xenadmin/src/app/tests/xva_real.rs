//! F11a on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests, and
//! `XENADMIN_REAL_LOG` / `XENADMIN_REAL_XVA_DIR` outside the repository).
//!
//! `real_f11a_xva_round_trip` writes nothing but its one disposable VM
//! ([`TEST_VM`], refused if one already exists): it is made from the
//! "Other install media" template with one 100 MiB disk on the SR named
//! "Local storage" and one interface on the management network, never
//! started; exported with the Export wizard (verified on completion) into
//! the folder of `XENADMIN_REAL_XVA_DIR`; deleted with its disk; imported
//! back with the Import wizard onto "Local storage" (not started); checked;
//! and deleted again. Nothing else is exported or imported, and no other
//! object is acted on. Should a step fail, every VM named [`TEST_VM`] is
//! destroyed with its disks.
//!
//! The same flow runs on the mock (`f11a_real_run_on_the_mock`), which
//! checks that it makes no call but the ones listed and changes no other
//! VM, disk, SR or network.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use xapi::{Ref, Sr, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::ActionRequest;
use xen_core::provisioning::{DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif};

use super::provisioning::press;
use super::*;
use crate::xva_strings::xva_strings;
use crate::xva_ui::{ExportPage, ImportPage};

/// The one disposable VM (and the prefix of its disk).
const TEST_VM: &str = "xenadmin-rs-test-f11a";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 100 << 20;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

fn conn(h: &Harness<'_, XenAdminApp>) -> Arc<xen_core::ConnectionView> {
    first_conn(h).expect("connected")
}

/// Steps until `done` or `limit`.
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

/// The first operation after `after` titled with `prefix`, once ended.
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
        "timed out waiting for {prefix:?}"
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

/// Runs `request` on the connection, as a menu does.
fn run(
    h: &mut Harness<'_, XenAdminApp>,
    request: ActionRequest,
    prefix: &str,
) -> xen_core::Operation {
    let before = last_op(h);
    let c = conn(h);
    let app = h.state_mut();
    app.ops.request(&c, request, &app.backend);
    wait_op(h, prefix, before, Duration::from_secs(600))
}

fn test_vms(h: &Harness<'_, XenAdminApp>) -> Vec<Ref<Vm>> {
    conn(h)
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.name_label.starts_with(TEST_VM))
        .map(|(r, _)| r.clone())
        .collect()
}

/// The disks of `vm` (not DVD drives).
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

fn delete(h: &mut Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> xen_core::Operation {
    let disks = disks(h, vm);
    // Only our own disks, on the test SR.
    {
        let c = conn(h);
        for d in &disks {
            let vdi = &c.model.vdis[d];
            assert_eq!(
                c.model.srs[&vdi.sr].name_label, TEST_SR,
                "refusing: a disk off {TEST_SR}"
            );
            assert_eq!(vdi.vbds.len(), 1, "refusing: a disk shared with another VM");
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

/// The round trip. `template`: the template the test VM is made from.
fn f11a_run(h: &mut Harness<'_, XenAdminApp>, dir: &Path, template: &str) -> String {
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    let mut log = String::new();
    let c = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST_VM} VM already exists"
    );
    assert!(
        !c.model
            .vdis
            .values()
            .any(|v| v.name_label.starts_with(TEST_VM)),
        "refusing: a {TEST_VM} disk already exists"
    );
    let file = dir.join(format!("{TEST_VM}.xva"));
    assert!(!file.exists(), "refusing: {} exists", file.display());
    let templates: Vec<_> = c
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && v.name_label == template)
        .collect();
    assert_eq!(templates.len(), 1, "the template {template:?}");
    let (template_ref, template_rec) = (templates[0].0.clone(), templates[0].1.clone());
    // The coordinator and its "Local storage".
    let host = c.model.pool().expect("pool").1.master.clone();
    let srs: Vec<Ref<Sr>> = c
        .model
        .srs
        .iter()
        .filter(|(_, s)| {
            s.name_label == TEST_SR
                && s.pbds
                    .iter()
                    .filter_map(|p| c.model.pbds.get(p))
                    .any(|p| p.host == host)
        })
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(srs.len(), 1, "one {TEST_SR} on the coordinator");
    let sr = srs[0].clone();
    let management = c
        .model
        .pifs
        .values()
        .find(|p| p.management && p.host == host)
        .map(|p| p.network.clone())
        .expect("the management network");
    let vdis_on_sr = c.model.srs[&sr].vdis.len();
    note(
        &mut log,
        format!(
            "preconditions: no {TEST_VM} VM or disk; template {template:?}; {TEST_SR} holds {vdis_on_sr} disks; the interface goes on {:?}",
            c.model.networks[&management].name_label
        ),
    );

    let mut ours: Option<Ref<Vm>> = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. The disposable VM: 100 MiB on Local storage, never started.
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST_VM.to_owned(),
            description: "F11a XVA round trip (disposable)".to_owned(),
            media: InstallMedia::None,
            affinity: Some(host.clone()),
            vcpus_max: 1,
            vcpus_at_startup: 1,
            cores_per_socket: Some(1),
            memory_static_max: template_rec.memory_static_max.max(256 << 20),
            memory_dynamic_min: template_rec.memory_static_max.max(256 << 20),
            memory_dynamic_max: template_rec.memory_static_max.max(256 << 20),
            disks: vec![NewVmDisk {
                source: DiskSource::New,
                system: false,
                userdevice: "0".to_owned(),
                name: format!("{TEST_VM} disk"),
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
        let op = run(h, ActionRequest::NewVm(Box::new(spec)), "Create VM");
        ok(&op);
        let vm = op.created.clone().expect("the new VM");
        ours = Some(vm.clone());
        let created = conn(h).model.vms[&vm].clone();
        assert_eq!(created.name_label, TEST_VM);
        assert_eq!(created.power_state, VmPowerState::Halted);
        let d = disks(h, &vm);
        assert_eq!(d.len(), 1);
        let disk = conn(h).model.vdis[&d[0]].clone();
        assert_eq!(disk.virtual_size, DISK);
        assert_eq!(disk.sr, sr);
        assert_eq!(created.vifs.len(), 1);
        note(
            &mut log,
            format!(
                "created {TEST_VM}: halted, 1 disk of {} on {TEST_SR}, 1 interface",
                xen_core::format::disk_size(disk.virtual_size)
            ),
        );

        // 2. Export with the wizard (VM menu > Export...), verified.
        let cid = conn(h).id;
        h.state_mut().selection = Some(Selection::Vm(cid, vm.clone()));
        settle(h);
        h.get_all_by_label(strings().menu_vm)
            .next()
            .unwrap()
            .click();
        settle(h);
        press(h, x.menu_export);
        assert_eq!(
            h.state().xva.export.as_ref().map(|w| w.page),
            Some(ExportPage::Appliance)
        );
        {
            let w = h.state_mut().xva.export.as_mut().unwrap();
            assert_eq!(w.name, TEST_VM);
            w.folder = dir.display().to_string();
        }
        press(h, n.next);
        assert_eq!(
            h.state().xva.export.as_ref().map(|w| w.page),
            Some(ExportPage::Vms)
        );
        assert_eq!(
            h.state().xva.export.as_ref().unwrap().ticked,
            vec![vm.clone()]
        );
        press(h, n.next);
        assert_eq!(
            h.state().xva.export.as_ref().map(|w| w.page),
            Some(ExportPage::Finish)
        );
        press(h, x.verify);
        assert!(h.state().xva.export.as_ref().unwrap().verify);
        let before = last_op(h);
        press(h, n.finish);
        let started = Instant::now();
        let op = wait_op(
            h,
            &format!("Export VM '{TEST_VM}'"),
            before,
            Duration::from_secs(900),
        );
        ok(&op);
        assert!(file.is_file());
        let size = std::fs::metadata(&file).unwrap().len();
        let mut v = xen_core::xva::Verifier::default().keep_metadata();
        let mut f = std::fs::File::open(&file).unwrap();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            use std::io::Read;
            let k = f.read(&mut buf).unwrap();
            if k == 0 {
                break;
            }
            v.feed(&buf[..k]).unwrap();
        }
        let summary = v.finish().unwrap();
        let capacity = xen_core::xva::disk_capacity(&file);
        assert_eq!(capacity, DISK as u64);
        assert!(
            summary
                .metadata
                .as_deref()
                .is_some_and(|m| m.contains(TEST_VM))
        );
        assert_eq!(
            std::fs::read_dir(dir).unwrap().count(),
            1,
            "no temporary file left"
        );
        note(
            &mut log,
            format!(
                "exported with the wizard in {:.1} s (verified on completion): {} bytes, {} disk blocks, checksums {:?}, disks {}",
                started.elapsed().as_secs_f64(),
                size,
                summary.blocks,
                summary.checksum,
                xen_core::format::disk_size(capacity as i64)
            ),
        );

        // 3. Delete it with its disk.
        let op = delete(h, &vm);
        ok(&op);
        assert!(test_vms(h).is_empty());
        ours = None;
        note(&mut log, format!("deleted {TEST_VM} and its disk"));

        // 4. Import with the wizard (File > Import...) onto Local storage.
        h.state_mut().selection = Some(Selection::Host(cid, host.clone()));
        settle(h);
        h.get_all_by_label(strings().menu_file)
            .next()
            .unwrap()
            .click();
        settle(h);
        press(h, x.menu_import);
        h.state_mut().xva.import.as_mut().unwrap().path = file.display().to_string();
        press(h, n.next);
        assert_eq!(
            h.state().xva.import.as_ref().map(|w| w.page),
            Some(ImportPage::Home)
        );
        assert_eq!(
            h.state().xva.import.as_ref().unwrap().target,
            Some((cid, Some(host.clone())))
        );
        press(h, n.next);
        assert_eq!(
            h.state().xva.import.as_ref().map(|w| w.page),
            Some(ImportPage::Storage)
        );
        h.state_mut().xva.import.as_mut().unwrap().sr = Some(sr.clone());
        settle(h);
        let before = last_op(h);
        let started = Instant::now();
        press(h, x.import_button);
        assert!(
            step_for(h, Duration::from_secs(900), |h| {
                h.state().xva.import.as_ref().map(|w| w.page) == Some(ImportPage::Network)
            }),
            "the import did not reach the network page: {:?}",
            h.state()
                .xva
                .import
                .as_ref()
                .and_then(|w| w.operation.clone())
        );
        let imported = h.state().xva.import.as_ref().unwrap().vm.clone().unwrap();
        ours = Some(imported.clone());
        let vifs = h.state().xva.import.as_ref().unwrap().vifs.clone();
        assert_eq!(vifs.len(), 1);
        assert_eq!(vifs[0].network, management);
        note(
            &mut log,
            format!(
                "import: the VM appeared after {:.1} s; network page shows interface {} on its network",
                started.elapsed().as_secs_f64(),
                vifs[0].device
            ),
        );
        press(h, n.next);
        assert_eq!(
            h.state().xva.import.as_ref().map(|w| w.page),
            Some(ImportPage::Finish)
        );
        // Never started.
        h.state_mut().xva.import.as_mut().unwrap().start = false;
        settle(h);
        press(h, n.finish);
        let op = wait_op(h, "Import VM from", before, Duration::from_secs(900));
        ok(&op);
        let rec = conn(h).model.vms[&imported].clone();
        assert_eq!(rec.name_label, TEST_VM);
        assert_eq!(rec.power_state, VmPowerState::Halted);
        assert_eq!(rec.affinity, host);
        let d = disks(h, &imported);
        assert_eq!(d.len(), 1);
        let disk = conn(h).model.vdis[&d[0]].clone();
        assert_eq!((disk.virtual_size, &disk.sr), (DISK, &sr));
        assert_eq!(rec.vifs.len(), 1);
        let vif = conn(h).model.vifs[&rec.vifs[0]].clone();
        assert_eq!(vif.network, management);
        note(
            &mut log,
            format!(
                "imported with the wizard in {:.1} s: {TEST_VM}, halted, home server set, 1 disk of {} on {TEST_SR}, interface {} on the management network",
                started.elapsed().as_secs_f64(),
                xen_core::format::disk_size(disk.virtual_size),
                vif.device
            ),
        );

        // 5. Delete it.
        let op = delete(h, &imported);
        ok(&op);
        ours = None;
        assert!(test_vms(h).is_empty());
        note(
            &mut log,
            format!("deleted the imported {TEST_VM} and its disk"),
        );
    }));
    if outcome.is_err() {
        note(&mut log, "a step failed: removing the test VM".into());
        for vm in test_vms(h).into_iter().chain(ours) {
            if conn(h).model.vms.contains_key(&vm) {
                let op = delete(h, &vm);
                note(&mut log, format!("cleanup: {:?}", op.status));
            }
        }
    }
    let c = conn(h);
    let left_vms = test_vms(h).len();
    let left_disks = c
        .model
        .vdis
        .values()
        .filter(|v| v.name_label.starts_with(TEST_VM))
        .count();
    note(
        &mut log,
        format!(
            "after: {left_vms} {TEST_VM} VMs, {left_disks} {TEST_VM} disks; {TEST_SR} holds {} disks (before {vdis_on_sr})",
            c.model.srs[&sr].vdis.len()
        ),
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!((left_vms, left_disks), (0, 0));
    assert_eq!(c.model.srs[&sr].vdis.len(), vdis_on_sr);
    log
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); makes, exports, imports and deletes one disposable VM"]
fn real_f11a_xva_round_trip() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let dir = std::env::var("XENADMIN_REAL_XVA_DIR").expect("set XENADMIN_REAL_XVA_DIR");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f11a_run(&mut h, dir, "Other install media");
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock's lab pool: only the calls it needs, and
/// nothing but its own objects changed.
#[test]
fn f11a_real_run_on_the_mock() {
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
        ["vm", "vdi", "sr", "network"]
            .iter()
            .flat_map(|c| {
                s.refs(c)
                    .into_iter()
                    .map(move |r| (c.to_string(), r.clone()))
            })
            .map(|(c, r)| {
                let mut rec = s.record(&c, &r).unwrap();
                // Used-space figures and the lists that hold the test's
                // objects for a while.
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
    let dir = std::env::temp_dir().join(format!("xen-f11a-rehearsal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = f11a_run(&mut h, &dir, "Debian Bookworm 12");
    assert!(log.contains("imported with the wizard"));
    assert_eq!(snapshot(&server), before, "only the test's objects changed");
    let writes: HashSet<String> = server.calls()[calls_before..]
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
        // Export and import.
        "task.create",
        "task.destroy",
        "http/export",
        "http/import",
        // Delete.
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
    std::fs::remove_dir_all(&dir).unwrap();
}
