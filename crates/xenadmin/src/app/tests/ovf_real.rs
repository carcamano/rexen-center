//! F11b on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests, and
//! `XENADMIN_REAL_LOG` / `XENADMIN_REAL_OVF_DIR` outside the repository).
//!
//! `real_f11b_ova_and_image` writes nothing but its disposable objects, one
//! VM at a time, all named with the [`TEST`] prefix and refused if one
//! already exists:
//!
//! 1. the VM [`TEST`] is made from the "Other install media" template with
//!    one 100 MiB disk on the SR named "Local storage" and one interface on
//!    the management network, never started;
//! 2. exported as an OVA (manifest, verified on completion) with the Export
//!    wizard into the folder of `XENADMIN_REAL_OVF_DIR`, and checked
//!    (manifest, VHD structure);
//! 3. deleted with its disk; the OVA imported back with the Import wizard
//!    onto "Local storage" and the management network, its manifest
//!    checked first, not started; checked; deleted;
//! 4. a small raw disk image (16 MiB, written into that folder) imported
//!    with the Import wizard as the VM [`TEST_IMAGE`] (its disk named the
//!    same) onto "Local storage"; checked;
//! 5. that VM exported as an OVA the same way, its disk's content checked
//!    against the image (SHA-256 of the expanded VHD), deleted, imported
//!    back, checked and deleted.
//!
//! Nothing else is exported, imported or acted on. Should a step fail,
//! every VM named with the prefix is destroyed with its disks.
//!
//! The same flow runs on the mock (`f11b_real_run_on_the_mock`), which
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
use crate::ovf_strings::ovf_strings;
use crate::ovf_ui::OvfPage;
use crate::xva_strings::xva_strings;
use crate::xva_ui::{ExportFormat, ExportPage};

/// The prefix of everything the test makes; the OVA round trip's VM.
const TEST: &str = "xenadmin-rs-test-f11b";
/// The disk image's VM and disk.
const TEST_IMAGE: &str = "xenadmin-rs-test-f11b-img";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 100 << 20;
const IMAGE: usize = 16 << 20;

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

fn delete(h: &mut Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> xen_core::Operation {
    let disks = disks(h, vm);
    {
        let c = conn(h);
        assert!(
            c.model.vms[vm].name_label.starts_with(TEST),
            "refusing: not a test VM"
        );
        for d in &disks {
            let vdi = &c.model.vdis[d];
            assert_eq!(
                c.model.srs[&vdi.sr].name_label, TEST_SR,
                "refusing: a disk off {TEST_SR}"
            );
            assert!(
                vdi.name_label.starts_with(TEST),
                "refusing: a disk not named {TEST}*"
            );
            assert_eq!(vdi.vbds.len(), 1, "refusing: a disk shared with another VM");
        }
    }
    // F12a: an imported package is a vApp of its own (named after the
    // package); it goes with its only VM.
    let vapp = {
        let c = conn(h);
        xen_core::vapp::vapp_of(&c.model, &c.model.vms[vm])
            .filter(|(_, a)| a.name_label.starts_with(TEST) && a.vms.len() == 1)
            .map(|(r, _)| r.clone())
    };
    let before = last_op(h);
    h.state().backend.send(xen_core::Command::Action {
        connection: conn(h).id,
        request: ActionRequest::DeleteVm {
            vm: vm.clone(),
            disks,
            snapshots: Vec::new(),
        },
    });
    let op = wait_op(h, "Deleting VM", before, Duration::from_secs(300));
    if let Some(vapp) = vapp {
        let before = last_op(h);
        h.state().backend.send(xen_core::Command::Action {
            connection: conn(h).id,
            request: ActionRequest::DeleteVApps { vapps: vec![vapp] },
        });
        let deleted = wait_op(h, "Deleting vApp", before, Duration::from_secs(120));
        assert!(
            matches!(deleted.status, xen_core::OperationStatus::Succeeded),
            "{:?}",
            deleted.status
        );
    }
    op
}

fn ovf_page(h: &Harness<'_, XenAdminApp>) -> Option<OvfPage> {
    h.state().xva.import_ovf.as_ref().map(|w| w.page)
}

/// Opens the File menu's Import wizard on `path` and goes past its source
/// page.
fn import_wizard(h: &mut Harness<'_, XenAdminApp>, path: &Path) {
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    settle(h);
    h.get_all_by_label(strings().menu_file)
        .next()
        .unwrap()
        .click();
    settle(h);
    press(h, x.menu_import);
    h.state_mut().xva.import.as_mut().unwrap().path = path.display().to_string();
    press(h, n.next);
}

/// Storage on `sr`, every interface on `net`, then on to the next page.
fn storage_and_networks(h: &mut Harness<'_, XenAdminApp>, sr: &Ref<Sr>, net: &Ref<xapi::Network>) {
    let n = crate::net_strings::net_strings();
    assert_eq!(ovf_page(h), Some(OvfPage::Storage));
    {
        let w = h.state_mut().xva.import_ovf.as_mut().unwrap();
        w.all_on_one = true;
        w.sr = Some(sr.clone());
    }
    settle(h);
    press(h, n.next);
    assert_eq!(ovf_page(h), Some(OvfPage::Network));
    {
        let w = h.state_mut().xva.import_ovf.as_mut().unwrap();
        for (_, net_choice) in &mut w.nics {
            *net_choice = Some(net.clone());
        }
    }
    settle(h);
    press(h, n.next);
}

/// The round trip and the image. `template`: the test VM's template.
fn f11b_run(h: &mut Harness<'_, XenAdminApp>, dir: &Path, template: &str) -> String {
    let x = xva_strings();
    let o = ovf_strings();
    let n = crate::net_strings::net_strings();
    let mut log = String::new();
    let c = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST}* VM already exists"
    );
    assert!(
        !c.model
            .vdis
            .values()
            .any(|v| v.name_label.starts_with(TEST)),
        "refusing: a {TEST}* disk already exists"
    );
    let package_dir = dir.join(TEST);
    assert!(
        !package_dir.exists(),
        "refusing: {} exists",
        package_dir.display()
    );
    let templates: Vec<_> = c
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && v.name_label == template)
        .collect();
    assert_eq!(templates.len(), 1, "the template {template:?}");
    let (template_ref, template_rec) = (templates[0].0.clone(), templates[0].1.clone());
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
            "preconditions: no {TEST}* VM or disk; template {template:?}; {TEST_SR} holds {vdis_on_sr} disks; interfaces go on {:?}",
            c.model.networks[&management].name_label
        ),
    );
    let cid = c.id;

    let mut ours: Option<Ref<Vm>> = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. The disposable VM.
        let memory = template_rec.memory_static_max.max(256 << 20);
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST.to_owned(),
            description: "F11b OVA round trip (disposable)".to_owned(),
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
        let op = run(h, ActionRequest::NewVm(Box::new(spec)), "Create VM");
        ok(&op);
        let vm = op.created.clone().expect("the new VM");
        ours = Some(vm.clone());
        let created = conn(h).model.vms[&vm].clone();
        assert_eq!(created.power_state, VmPowerState::Halted);
        let cds = |h: &Harness<'_, XenAdminApp>, vm: &Ref<Vm>| {
            let c = conn(h);
            c.model.vms[vm]
                .vbds
                .iter()
                .filter_map(|b| c.model.vbds.get(b))
                .filter(|b| b.r#type == xapi::VbdType::Cd)
                .count()
        };
        let created_cds = cds(h, &vm);
        let d = disks(h, &vm);
        assert_eq!(d.len(), 1);
        assert_eq!(conn(h).model.vdis[&d[0]].virtual_size, DISK);
        assert_eq!(created.vifs.len(), 1);
        let mac = conn(h).model.vifs[&created.vifs[0]].mac.clone();
        note(
            &mut log,
            format!("created {TEST}: halted, 1 disk of 100 MiB on {TEST_SR}, 1 interface"),
        );

        // 2. Export as an OVA with the wizard (VM menu > Export...).
        h.state_mut().selection = Some(Selection::Vm(cid, vm.clone()));
        settle(h);
        h.get_all_by_label(strings().menu_vm)
            .next()
            .unwrap()
            .click();
        settle(h);
        press(h, x.menu_export);
        {
            let w = h.state_mut().xva.export.as_mut().unwrap();
            assert_eq!(w.name, TEST);
            assert_eq!(w.format, ExportFormat::Ovf);
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
        press(h, n.next);
        assert_eq!(
            h.state().xva.export.as_ref().map(|w| w.page),
            Some(ExportPage::Options)
        );
        press(h, o.create_manifest);
        press(h, o.create_ova);
        press(h, n.next);
        press(h, x.verify);
        let before = last_op(h);
        let started = Instant::now();
        press(h, n.finish);
        let op = wait_op(h, "Export OVA package", before, Duration::from_secs(900));
        ok(&op);
        let ova = package_dir.join(format!("{TEST}.ova"));
        assert!(ova.is_file());
        let size = std::fs::metadata(&ova).unwrap().len();
        let package = xen_core::ovf_package::Package::open(&ova).unwrap();
        package.verify_manifest(&|_, _| {}, &|| false).unwrap();
        let entries = xen_core::ova::index(&ova).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let disk_entry = entries.iter().find(|e| e.name.ends_with(".vhd")).unwrap();
        let footer = {
            use std::io::{Read, Seek};
            let mut f = std::fs::File::open(&ova).unwrap();
            f.seek(std::io::SeekFrom::Start(disk_entry.offset)).unwrap();
            xen_core::disk_image::check_vhd(&mut f.take(disk_entry.size), disk_entry.size).unwrap()
        };
        assert_eq!(footer.current_size, DISK as u64);
        assert_eq!(std::fs::read_dir(&package_dir).unwrap().count(), 1);
        note(
            &mut log,
            format!(
                "exported as an OVA with the wizard in {:.1} s (verified on completion): {size} bytes, entries {names:?}; manifest checked; the disk is a dynamic VHD of {} ({} bytes)",
                started.elapsed().as_secs_f64(),
                xen_core::format::disk_size(footer.current_size as i64),
                disk_entry.size
            ),
        );

        // 3. Delete it, then import the OVA back.
        ok(&delete(h, &vm));
        assert!(test_vms(h).is_empty());
        ours = None;
        note(&mut log, format!("deleted {TEST} and its disk"));
        h.state_mut().selection = Some(Selection::Host(cid, host.clone()));
        settle(h);
        import_wizard(h, &ova);
        assert_eq!(ovf_page(h), Some(OvfPage::Location));
        assert_eq!(
            h.state().xva.import_ovf.as_ref().unwrap().target,
            Some((cid, Some(host.clone())))
        );
        press(h, n.next);
        storage_and_networks(h, &sr, &management);
        assert_eq!(ovf_page(h), Some(OvfPage::Security));
        press(h, o.verify_manifest);
        press(h, n.next);
        assert_eq!(ovf_page(h), Some(OvfPage::Finish));
        h.state_mut().xva.import_ovf.as_mut().unwrap().start = false;
        settle(h);
        let before = last_op(h);
        let started = Instant::now();
        press(h, n.finish);
        let op = wait_op(h, "Import Appliance", before, Duration::from_secs(900));
        ok(&op);
        let imported = op.created.clone().expect("the imported VM");
        ours = Some(imported.clone());
        assert!(step_for(h, Duration::from_secs(10), |h| disks(
            h, &imported
        )
        .len()
            == 1
            && conn(h)
                .model
                .vms
                .get(&imported)
                .is_some_and(|v| v.vifs.len() == 1)));
        let rec = conn(h).model.vms[&imported].clone();
        assert_eq!(rec.name_label, TEST);
        assert_eq!(rec.power_state, VmPowerState::Halted);
        assert_eq!(rec.affinity, host);
        assert!(!rec.other_config.contains_key("HideFromXenCenter"));
        let d = disks(h, &imported);
        assert_eq!(d.len(), 1);
        let disk = conn(h).model.vdis[&d[0]].clone();
        assert_eq!((disk.virtual_size, &disk.sr), (DISK, &sr));
        assert_eq!(disk.name_label, format!("{TEST} disk"));
        let vif = conn(h).model.vifs[&rec.vifs[0]].clone();
        assert_eq!(vif.network, management);
        assert_eq!(vif.mac, mac, "the MAC is kept");
        assert_eq!(
            cds(h, &imported),
            created_cds.min(1),
            "the (empty) DVD drive"
        );
        note(
            &mut log,
            format!(
                "imported the OVA with the wizard in {:.1} s (manifest checked first): {TEST}, halted, home server set, disk {:?} of 100 MiB on {TEST_SR}, interface on the management network with the exported MAC",
                started.elapsed().as_secs_f64(),
                disk.name_label
            ),
        );
        ok(&delete(h, &imported));
        ours = None;
        assert!(test_vms(h).is_empty());
        note(
            &mut log,
            format!("deleted the imported {TEST} and its disk"),
        );

        // 4. A raw disk image.
        let image = dir.join(format!("{TEST_IMAGE}.img"));
        let mut bytes = vec![0u8; IMAGE];
        for (i, b) in bytes[..1 << 20].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        bytes[IMAGE - 4096..].fill(0xA5);
        std::fs::write(&image, &bytes).unwrap();
        import_wizard(h, &image);
        assert_eq!(ovf_page(h), Some(OvfPage::VmDefinition));
        {
            let w = h.state_mut().xva.import_ovf.as_mut().unwrap();
            w.vm_name = TEST_IMAGE.to_owned();
            w.cpus = 1;
            w.memory_mb = 256;
        }
        settle(h);
        press(h, n.next);
        assert_eq!(ovf_page(h), Some(OvfPage::Location));
        press(h, n.next);
        storage_and_networks(h, &sr, &management);
        assert_eq!(ovf_page(h), Some(OvfPage::Boot));
        press(h, n.next);
        assert_eq!(ovf_page(h), Some(OvfPage::Finish));
        h.state_mut().xva.import_ovf.as_mut().unwrap().start = false;
        settle(h);
        let before = last_op(h);
        let started = Instant::now();
        press(h, n.finish);
        let op = wait_op(h, "Import VM", before, Duration::from_secs(900));
        ok(&op);
        let vm = op.created.clone().expect("the image's VM");
        ours = Some(vm.clone());
        assert!(step_for(h, Duration::from_secs(10), |h| disks(h, &vm)
            .len()
            == 1));
        let rec = conn(h).model.vms[&vm].clone();
        assert_eq!(rec.name_label, TEST_IMAGE);
        assert_eq!(rec.power_state, VmPowerState::Halted);
        assert_eq!(rec.memory_static_max, 256 << 20);
        let d = disks(h, &vm);
        assert_eq!(d.len(), 1);
        let disk = conn(h).model.vdis[&d[0]].clone();
        assert_eq!((disk.virtual_size, &disk.sr), (IMAGE as i64, &sr));
        assert_eq!(disk.name_label, TEST_IMAGE);
        note(
            &mut log,
            format!(
                "imported the raw image ({} bytes) with the wizard in {:.1} s: VM {TEST_IMAGE}, halted, 256 MiB, disk {TEST_IMAGE} of {} on {TEST_SR}",
                bytes.len(),
                started.elapsed().as_secs_f64(),
                xen_core::format::disk_size(disk.virtual_size)
            ),
        );
        // 5. Its content round trip: exported as an OVA, the disk's bytes
        // are the image's; deleted, imported back, checked, deleted.
        let image_sha = xen_core::disk_image::sha256_of(&mut bytes.as_slice()).unwrap();
        h.state_mut().selection = Some(Selection::Vm(cid, vm.clone()));
        settle(h);
        h.get_all_by_label(strings().menu_vm)
            .next()
            .unwrap()
            .click();
        settle(h);
        press(h, x.menu_export);
        {
            let w = h.state_mut().xva.export.as_mut().unwrap();
            assert_eq!(w.name, TEST_IMAGE);
            w.folder = dir.display().to_string();
        }
        press(h, n.next);
        assert_eq!(
            h.state().xva.export.as_ref().unwrap().ticked,
            vec![vm.clone()]
        );
        press(h, n.next);
        press(h, n.next);
        press(h, o.create_manifest);
        press(h, o.create_ova);
        press(h, n.next);
        press(h, x.verify);
        let before = last_op(h);
        let started = Instant::now();
        press(h, n.finish);
        ok(&wait_op(
            h,
            "Export OVA package",
            before,
            Duration::from_secs(900),
        ));
        let ova2 = dir.join(TEST_IMAGE).join(format!("{TEST_IMAGE}.ova"));
        let package = xen_core::ovf_package::Package::open(&ova2).unwrap();
        package.verify_manifest(&|_, _| {}, &|| false).unwrap();
        let entry = xen_core::ova::index(&ova2)
            .unwrap()
            .into_iter()
            .find(|e| e.name.ends_with(".vhd"))
            .unwrap();
        let (sha, size) = {
            let mut f = std::fs::File::open(&ova2).unwrap();
            xen_core::disk_image::vhd_content_sha256(&mut f, entry.offset, entry.size).unwrap()
        };
        assert_eq!(size, IMAGE as u64);
        assert_eq!(sha, image_sha, "the exported disk holds the image's bytes");
        note(
            &mut log,
            format!(
                "exported {TEST_IMAGE} as an OVA in {:.1} s (verified on completion, manifest checked): the disk's VHD ({} bytes) expands to the image's {} bytes, same SHA-256",
                started.elapsed().as_secs_f64(),
                entry.size,
                size
            ),
        );
        ok(&delete(h, &vm));
        ours = None;
        note(&mut log, format!("deleted {TEST_IMAGE} and its disk"));
        h.state_mut().selection = Some(Selection::Host(cid, host.clone()));
        settle(h);
        import_wizard(h, &ova2);
        assert_eq!(ovf_page(h), Some(OvfPage::Location));
        press(h, n.next);
        storage_and_networks(h, &sr, &management);
        assert_eq!(ovf_page(h), Some(OvfPage::Security));
        press(h, o.verify_manifest);
        press(h, n.next);
        h.state_mut().xva.import_ovf.as_mut().unwrap().start = false;
        settle(h);
        let before = last_op(h);
        let started = Instant::now();
        press(h, n.finish);
        let op = wait_op(h, "Import Appliance", before, Duration::from_secs(900));
        ok(&op);
        let vm = op.created.clone().expect("the imported VM");
        ours = Some(vm.clone());
        assert!(step_for(h, Duration::from_secs(10), |h| disks(h, &vm)
            .len()
            == 1));
        let rec = conn(h).model.vms[&vm].clone();
        assert_eq!(rec.name_label, TEST_IMAGE);
        assert_eq!(rec.power_state, VmPowerState::Halted);
        let d = disks(h, &vm);
        let disk = conn(h).model.vdis[&d[0]].clone();
        assert_eq!((disk.virtual_size, &disk.sr), (IMAGE as i64, &sr));
        assert_eq!(disk.name_label, TEST_IMAGE);
        note(
            &mut log,
            format!(
                "imported that OVA back in {:.1} s (manifest checked first): {TEST_IMAGE}, halted, disk {TEST_IMAGE} of {} on {TEST_SR} (physical utilisation {} bytes)",
                started.elapsed().as_secs_f64(),
                xen_core::format::disk_size(disk.virtual_size),
                disk.physical_utilisation
            ),
        );
        ok(&delete(h, &vm));
        ours = None;
        note(
            &mut log,
            format!("deleted the imported {TEST_IMAGE} and its disk"),
        );
    }));
    if outcome.is_err() {
        note(&mut log, "a step failed: removing the test VMs".into());
        for vm in test_vms(h).into_iter().chain(ours) {
            if conn(h).model.vms.contains_key(&vm) {
                let op = delete(h, &vm);
                note(&mut log, format!("cleanup: {:?}", op.status));
            }
        }
    }
    // Let the last events in.
    step_for(h, Duration::from_secs(2), |_| false);
    let c = conn(h);
    let left_vms = test_vms(h).len();
    let left_disks = c
        .model
        .vdis
        .values()
        .filter(|v| v.name_label.starts_with(TEST))
        .count();
    note(
        &mut log,
        format!(
            "after: {left_vms} {TEST}* VMs, {left_disks} {TEST}* disks; {TEST_SR} holds {} disks (before {vdis_on_sr})",
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
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); makes, exports, imports and deletes disposable test VMs and disks"]
fn real_f11b_ova_and_image() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let dir = std::env::var("XENADMIN_REAL_OVF_DIR").expect("set XENADMIN_REAL_OVF_DIR");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f11b_run(&mut h, dir, "Other install media");
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock's pool: only the calls it needs, and
/// nothing but its own objects changed.
#[test]
fn f11b_real_run_on_the_mock() {
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
    let dir = std::env::temp_dir().join(format!("xen-f11b-rehearsal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = f11b_run(&mut h, &dir, "Debian Bookworm 12");
    assert!(log.contains("imported the raw image"));
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
        "http/export_raw_vdi",
        "http/import_raw_vdi",
        "VM.create",
        // F12a: the package becomes a vApp (`Import.Process`).
        "VM_appliance.create",
        // Delete (the package's vApp too).
        "VM_appliance.destroy",
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
