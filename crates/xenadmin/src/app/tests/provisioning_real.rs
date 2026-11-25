//! F7 on a real host, through the real window code (ignored tests, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` / `XENADMIN_REAL_SHOTS` outside the repository).
//!
//! * `real_f7_fluxer` acts on the VM named exactly "09 - Ubuntu Fluxer"
//!   only (refused unless exactly one VM has that name and it is halted):
//!   it takes a snapshot, loads an ISO into its DVD drive and ejects it,
//!   reverts to the snapshot it took, and deletes that snapshot. The VM
//!   ends halted with its DVD drive as found.
//! * `real_f7_test_vm_create` makes the one disposable test VM
//!   ([`TEST_VM`]) with the New VM wizard: Debian Trixie 13, the Debian
//!   netinst ISO, its disk on the SR named "Local storage", one network,
//!   started; the console shows the installer.
//! * `real_f7_console_driver` types into the test VM's console from
//!   command files (`XENADMIN_REAL_DRIVER`), taking a snapshot of the
//!   screen after each, to go through the installer.
//! * `real_f7_test_vm_operations` runs the disk, DVD, tools, snapshot,
//!   copy and template operations on the test VM and what it makes, then
//!   deletes all of it (and checks nothing is left).
//!
//! Every object these tests make is named with the [`TEST_VM`] prefix and
//! lives on "Local storage"; nothing else on the host is changed.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use xapi::{Ref, Sr, Vdi, Vm, VmPowerState};
use xen_core::console::ConsoleTarget;
use xen_vnc::KeyCode;

use super::console::{connected, image_rect, note, snapshot, tap, type_us, view};
use super::provisioning::{open_combo, press, select, shows, tab, type_into, wizard};
use super::*;
use crate::console_view::ConsoleKey;
use crate::prov_strings::prov_strings;

/// The name of the one disposable test VM (and the prefix of everything
/// these tests make).
pub const TEST_VM: &str = "xenadmin-rs-test-f7";
/// The only SR the test VM's disks may use.
const TEST_SR: &str = "Local storage";
const TEST_TEMPLATE: &str = "Debian Trixie 13";
const TEST_ISO: &str = "debian-13.8.0-amd64-netinst.iso";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn conn(h: &Harness<'_, XenAdminApp>) -> Arc<ConnectionView> {
    first_conn(h).expect("connected")
}

/// Steps until the operation whose title starts with `prefix` (and that
/// started after `after`) ends; `Err` with its failure, or on timeout.
fn wait_op(
    h: &mut Harness<'_, XenAdminApp>,
    prefix: &str,
    after: u64,
    limit: Duration,
) -> Result<(), String> {
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
    if !step_for(h, limit, |h| found(h).is_some()) {
        return Err(format!("timed out waiting for {prefix:?}"));
    }
    let op = found(h).unwrap();
    // Let the cache take the last events in.
    step_for(h, Duration::from_millis(600), |_| false);
    match &op.status {
        xen_core::OperationStatus::Succeeded => Ok(()),
        other => Err(format!("{prefix:?}: {other:?}")),
    }
}

/// The newest operation id so far.
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

/// What the last operation made (`Operation::created`).
fn created_by_last(h: &Harness<'_, XenAdminApp>, prefix: &str) -> Option<Ref<Vm>> {
    h.state()
        .backend
        .state()
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server).starts_with(prefix))
        .and_then(|o| o.created.clone())
}

fn vms_named(h: &Harness<'_, XenAdminApp>, name: &str) -> Vec<Ref<Vm>> {
    conn(h)
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.name_label == name && !v.is_control_domain)
        .map(|(r, _)| r.clone())
        .collect()
}

fn sr_named(h: &Harness<'_, XenAdminApp>, name: &str) -> Vec<Ref<Sr>> {
    conn(h)
        .model
        .srs
        .iter()
        .filter(|(_, s)| s.name_label == name)
        .map(|(r, _)| r.clone())
        .collect()
}

fn vdi_named(h: &Harness<'_, XenAdminApp>, name: &str) -> Vec<Ref<Vdi>> {
    conn(h)
        .model
        .vdis
        .iter()
        .filter(|(_, v)| v.name_label == name)
        .map(|(r, _)| r.clone())
        .collect()
}

/// A plain XAPI session for the cleanup (never used by the steps).
fn cleanup_session(runtime: &tokio::runtime::Runtime) -> xapi::Session {
    let (host, password, fingerprint) = (
        env("XENADMIN_REAL_HOST"),
        env("XENADMIN_REAL_PASSWORD"),
        env("XENADMIN_REAL_FINGERPRINT"),
    );
    let endpoint = xapi::Endpoint::parse(&host).unwrap();
    let trust = xapi::TrustStore::new();
    trust.trust(
        &endpoint.authority(),
        xapi::Fingerprint::parse(&fingerprint).unwrap(),
    );
    let client =
        xapi::JsonRpcClient::new(&endpoint, &trust, &xapi::ClientOptions::default()).unwrap();
    runtime
        .block_on(xapi::Session::login_with_password(
            Arc::new(client),
            "root",
            &password,
            "xenadmin-rs-f7-cleanup",
        ))
        .expect("cleanup login")
}

fn write_log(log: &str) {
    if let Ok(path) = std::env::var("XENADMIN_REAL_LOG") {
        std::fs::write(path, log).expect("write the log");
    }
}

/// The VM named exactly [`FLUXER`]: snapshot, DVD drive, revert, delete.
#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); writes to 09 - Ubuntu Fluxer"]
fn real_f7_fluxer() {
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let p = prov_strings();
    let t = tab_strings();
    let mut log = String::new();
    let named = vms_named(&h, FLUXER)
        .into_iter()
        .filter(|r| {
            let v = &conn(&h).model.vms[r];
            !v.is_a_template && !v.is_a_snapshot
        })
        .collect::<Vec<_>>();
    assert_eq!(
        named.len(),
        1,
        "refusing: {} VMs are named {FLUXER:?}",
        named.len()
    );
    let vm = named[0].clone();
    let rec = conn(&h).model.vms[&vm].clone();
    assert_eq!(
        rec.power_state,
        VmPowerState::Halted,
        "refusing: {FLUXER:?} is not halted"
    );
    let snapshots_before: HashSet<Ref<Vm>> = rec.snapshots.iter().cloned().collect();
    let cd = {
        let c = conn(&h);
        let drives = xen_core::provisioning::cd_drives(&c.model, &rec);
        assert_eq!(drives.len(), 1, "refusing: expected one DVD drive");
        (
            drives[0].0.clone(),
            (!drives[0].1.empty).then(|| drives[0].1.vdi.clone()),
        )
    };
    let disk_before: Vec<(String, Ref<Vdi>)> =
        xen_core::provisioning::vm_disks(&conn(&h).model, &rec)
            .into_iter()
            .map(|(_, b, v, _)| (b.userdevice.clone(), v.clone()))
            .collect();
    let disk_names: Vec<String> = disk_before
        .iter()
        .filter_map(|(_, v)| conn(&h).model.vdis.get(v).map(|x| x.name_label.clone()))
        .collect();
    let disks_named_before = conn(&h)
        .model
        .vdis
        .values()
        .filter(|v| disk_names.contains(&v.name_label))
        .count();
    let snap_name = format!(
        "xenadmin-rs-f7-test {}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    note(
        &mut log,
        format!(
            "{FLUXER:?} found once, halted; {} snapshots before; DVD drive {} ; disks {:?}",
            snapshots_before.len(),
            if cd.1.is_some() {
                "holds an ISO"
            } else {
                "empty"
            },
            disk_before.iter().map(|d| &d.0).collect::<Vec<_>>()
        ),
    );
    let mut ours: Option<Ref<Vm>> = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let c = conn(&h).id;
        // 1. Take a snapshot from the Snapshots tab.
        select(&mut h, Selection::Vm(c, vm.clone()));
        tab(&mut h, p.tab_snapshots);
        press(&mut h, p.take_snapshot_button);
        assert!(shows(&h, p.take_snapshot_title));
        type_into(&mut h, p.name_label, &snap_name);
        let before = last_op(&h);
        press(&mut h, p.take_snapshot_ok);
        let title = format!("Taking a snapshot of VM '{FLUXER}'...");
        wait_op(&mut h, &title, before, Duration::from_secs(300)).unwrap();
        ours = created_by_last(&h, &title);
        let snap = ours.clone().expect("the snapshot");
        let s = conn(&h).model.vms[&snap].clone();
        assert!(s.is_a_snapshot && s.snapshot_of == vm && s.name_label == snap_name);
        note(
            &mut log,
            format!(
                "snapshot taken: {:?}, {} disk(s)",
                s.name_label,
                s.vbds.len()
            ),
        );

        // 2. The DVD drive: load the Debian ISO, then eject it.
        tab(&mut h, t.tab_storage);
        open_combo(&mut h);
        let before = last_op(&h);
        press(&mut h, &format!("    {TEST_ISO}"));
        wait_op(
            &mut h,
            &format!("Loading ISO '{TEST_ISO}' into the DVD Drive on VM {FLUXER}"),
            before,
            Duration::from_secs(120),
        )
        .unwrap();
        let loaded = conn(&h).model.vbds[&cd.0].clone();
        assert!(!loaded.empty);
        assert_eq!(conn(&h).model.vdis[&loaded.vdi].name_label, TEST_ISO);
        note(&mut log, format!("DVD drive loaded with {TEST_ISO}"));
        let before = last_op(&h);
        press(&mut h, p.eject);
        wait_op(
            &mut h,
            &format!("Setting DVD Drive on {FLUXER} as empty."),
            before,
            Duration::from_secs(120),
        )
        .unwrap();
        assert!(conn(&h).model.vbds[&cd.0].empty);
        note(&mut log, "DVD drive ejected (empty again)".into());

        // 3. Revert to our snapshot (no new snapshot first).
        tab(&mut h, p.tab_snapshots);
        click_first(&mut h, &snap_name);
        settle(&mut h);
        press(&mut h, p.revert_button);
        assert!(shows(&h, &fill(p.revert_blurb, &[("name", &snap_name)])));
        // The box is checked by default (snapshots are allowed): uncheck.
        press(&mut h, p.revert_new_snapshot);
        let before = last_op(&h);
        press(&mut h, p.yes);
        wait_op(
            &mut h,
            &format!("Revert snapshot '{snap_name}'"),
            before,
            Duration::from_secs(300),
        )
        .unwrap();
        assert_eq!(vm_rec(&h, vm.as_str()).power_state, VmPowerState::Halted);
        note(
            &mut log,
            "reverted to the test's snapshot; VM halted".into(),
        );

        // 4. Delete our snapshot.
        click_first(&mut h, &snap_name);
        settle(&mut h);
        press(&mut h, p.delete);
        let before = last_op(&h);
        press(&mut h, p.yes);
        wait_op(
            &mut h,
            &format!("Deleting snapshot '{snap_name}'..."),
            before,
            Duration::from_secs(300),
        )
        .unwrap();
        assert!(!conn(&h).model.vms.contains_key(&snap));
        ours = None;
        note(&mut log, "the test's snapshot deleted".into());
    }));
    // Cleanup, whatever happened: our snapshot (with its disks) and the
    // DVD drive as found.
    let session = cleanup_session(&runtime);
    if let Some(snap) = &ours {
        let r = runtime.block_on(async {
            let rec = xapi::vm::get_record(&session, snap).await?;
            if rec.name_label != snap_name || !rec.is_a_snapshot {
                return Ok::<_, xapi::Error>(());
            }
            let mut disks = Vec::new();
            for b in &rec.vbds {
                let v = xapi::vbd::get_record(&session, b).await?;
                if v.r#type == xapi::VbdType::Disk && !v.vdi.is_null() {
                    disks.push(v.vdi);
                }
            }
            xapi::vm::destroy(&session, snap).await?;
            for d in disks {
                xapi::vdi::destroy(&session, &d).await?;
            }
            Ok(())
        });
        note(
            &mut log,
            format!("cleanup: removed the test's snapshot: {r:?}"),
        );
    }
    let after = runtime
        .block_on(xapi::vm::get_record(&session, &vm))
        .unwrap();
    // A revert makes the VM's drives again: find the DVD drive anew.
    let cd_now = after.vbds.iter().find_map(|b| {
        runtime
            .block_on(xapi::vbd::get_record(&session, b))
            .ok()
            .filter(|r| r.r#type == xapi::VbdType::Cd)
            .map(|r| (b.clone(), r))
    });
    if let Some((r, rec)) = &cd_now
        && cd.1.is_none()
        && !rec.empty
    {
        let e = runtime.block_on(xapi::vbd::eject(&session, r));
        note(&mut log, format!("cleanup: ejected the DVD drive: {e:?}"));
    }
    let cd_empty = cd_now.as_ref().map(|(r, rec)| {
        runtime
            .block_on(xapi::vbd::get_empty(&session, r))
            .unwrap_or(rec.empty)
    });
    // Disks named like the VM's own (its snapshots' disks carry the same
    // name): as many as before.
    let vdis = runtime
        .block_on(xapi::vdi::get_all_records(&session))
        .unwrap();
    let same_name = vdis
        .values()
        .filter(|v| disk_names.contains(&v.name_label))
        .count();
    note(
        &mut log,
        format!(
            "end: power {:?}; snapshots {} (before {}); DVD drive empty={:?}; disks named like its own: {same_name} (before {disks_named_before})",
            after.power_state,
            after.snapshots.len(),
            snapshots_before.len(),
            cd_empty
        ),
    );
    runtime.block_on(session.logout()).ok();
    write_log(&log);
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!(after.power_state, VmPowerState::Halted);
    assert_eq!(
        after.snapshots.iter().cloned().collect::<HashSet<_>>(),
        snapshots_before
    );
    assert_eq!(same_name, disks_named_before);
    assert_eq!(cd_empty, Some(cd.1.is_none()));
}

/// The test VM made with the New VM wizard, started on the installer.
#[test]
#[ignore = "needs a real XCP-ng host; creates the one test VM on Local storage"]
fn real_f7_test_vm_create() {
    let shots = env("XENADMIN_REAL_SHOTS");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let p = prov_strings();
    let mut log = String::new();
    assert!(
        conn(&h)
            .model
            .vms
            .values()
            .all(|v| !v.name_label.starts_with(TEST_VM)),
        "refusing: a {TEST_VM}* VM exists already"
    );
    let local = sr_named(&h, TEST_SR);
    assert_eq!(
        local.len(),
        1,
        "refusing: {} SRs named {TEST_SR:?}",
        local.len()
    );
    let local = local[0].clone();
    assert_eq!(vms_named(&h, TEST_TEMPLATE).len(), 1, "the template");
    assert_eq!(vdi_named(&h, TEST_ISO).len(), 1, "the ISO");
    if let Ok(path) = std::env::var("XENADMIN_REAL_BASELINE") {
        let refs: Vec<String> = conn(&h)
            .model
            .vdis
            .iter()
            .filter(|(_, v)| v.sr == local)
            .map(|(r, _)| r.to_string())
            .collect();
        std::fs::write(path, refs.join("\n")).expect("write the baseline");
        note(
            &mut log,
            format!("baseline: {} disks on {TEST_SR}", refs.len()),
        );
    }
    let c = conn(&h).id;
    let pool = conn(&h).model.pool().unwrap().0.clone();
    select(&mut h, Selection::Pool(c, pool));
    press(&mut h, p.new_vm_toolbar);
    // Template page: search for it, choose it.
    type_into(&mut h, p.template_search, "Trixie");
    press(&mut h, TEST_TEMPLATE);
    press(&mut h, p.next);
    wizard(&mut h).name = TEST_VM.into();
    wizard(&mut h).description = "xenadmin-rs F7 real-host test VM (disposable)".into();
    settle(&mut h);
    press(&mut h, p.next);
    // Media: the netinst ISO; the boot mode the template asks for.
    open_combo(&mut h);
    press(&mut h, &format!("    {TEST_ISO}"));
    // `XENADMIN_REAL_BOOT=uefi`: plain UEFI (this pool's Secure Boot
    // keys refuse the Debian ISO's boot loader).
    if std::env::var("XENADMIN_REAL_BOOT").as_deref() == Ok("uefi") {
        press(&mut h, p.boot_uefi);
    }
    let boot = wizard(&mut h).boot_mode;
    press(&mut h, p.next);
    // Home server: whatever the wizard suggests (one host).
    press(&mut h, p.next);
    // CPU and memory: the template's (1 vCPU, 2 GiB).
    press(&mut h, p.next);
    // Storage: the provisioned 10 GiB disk, on Local storage only.
    {
        let w = wizard(&mut h);
        assert_eq!(w.disks.len(), 1);
        w.disks[0].sr = local.clone();
    }
    settle(&mut h);
    press(&mut h, p.next);
    // Networking: the eth0 network only.
    {
        let model = conn(&h).model.clone();
        let w = wizard(&mut h);
        w.vifs.retain(|v| {
            model
                .networks
                .get(&v.network)
                .is_some_and(|n| n.bridge == "xenbr0")
        });
        assert_eq!(w.vifs.len(), 1, "the eth0 network");
    }
    settle(&mut h);
    press(&mut h, p.next);
    assert!(shows(&h, p.title_finish));
    let before = last_op(&h);
    press(&mut h, p.create_now);
    let result = wait_op(
        &mut h,
        &format!("Create VM '{TEST_VM}'"),
        before,
        Duration::from_secs(600),
    );
    let vm = vms_named(&h, TEST_VM).first().cloned();
    note(
        &mut log,
        format!(
            "wizard: template {TEST_TEMPLATE:?}, ISO {TEST_ISO:?}, boot mode {boot:?}, disk 10 GiB on {TEST_SR:?}, network eth0; result {result:?}; VM made: {}",
            vm.is_some()
        ),
    );
    if let Some(vm) = &vm {
        let r = vm_rec(&h, vm.as_str());
        let m = conn(&h).model.clone();
        let disks: Vec<String> = xen_core::provisioning::vm_disks(&m, &r)
            .iter()
            .map(|(_, b, _, v)| {
                format!(
                    "{} {} on {}",
                    b.userdevice,
                    xen_core::format::disk_size(v.virtual_size),
                    m.srs
                        .get(&v.sr)
                        .map(|s| s.name_label.as_str())
                        .unwrap_or("?")
                )
            })
            .collect();
        note(
            &mut log,
            format!(
                "VM: power {:?}, vCPUs {}, memory {}, firmware {:?}, secureboot {:?}, disks {disks:?}, VIFs {}, DVD {:?}",
                r.power_state,
                r.vcpus_max,
                xen_core::format::memory_size(r.memory_static_max, false),
                r.hvm_boot_params.get("firmware"),
                r.platform.get("secureboot"),
                r.vifs.len(),
                xen_core::provisioning::cd_drives(&m, &r)
                    .first()
                    .and_then(|(_, b)| m.vdis.get(&b.vdi))
                    .map(|v| v.name_label.clone())
            ),
        );
        // The console: the installer's first screen.
        let key: ConsoleKey = (c, ConsoleTarget::Vm(vm.clone()));
        select(&mut h, Selection::Vm(c, vm.clone()));
        tab(&mut h, tab_strings().tab_console);
        let up = step_for(&mut h, Duration::from_secs(90), |h| connected(h, &key));
        step_for(&mut h, Duration::from_secs(20), |_| false);
        if up {
            let (w, hh) = snapshot(&h, &key, &format!("{shots}/create-installer.ppm"));
            note(
                &mut log,
                format!("console connected: {w}x{hh}, snapshot create-installer.ppm"),
            );
        } else {
            note(&mut log, "console did not connect".into());
        }
    }
    write_log(&log);
    result.unwrap();
}

/// Types into the test VM's console from numbered command files in
/// `XENADMIN_REAL_DRIVER` (`NNN.cmd`, one command per line: `type TEXT`,
/// `key NAME [COUNT]`, `wait SECONDS`, `shot NAME`, `end`), writing
/// `NNN.done` after each file and a snapshot `NNN.ppm`.
#[test]
#[ignore = "needs a real XCP-ng host; types into the test VM's console only"]
fn real_f7_console_driver() {
    let dir = env("XENADMIN_REAL_DRIVER");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let named = vms_named(&h, TEST_VM);
    assert_eq!(
        named.len(),
        1,
        "refusing: {} VMs named {TEST_VM:?}",
        named.len()
    );
    let vm = named[0].clone();
    let c = conn(&h).id;
    let key: ConsoleKey = (c, ConsoleTarget::Vm(vm.clone()));
    select(&mut h, Selection::Vm(c, vm.clone()));
    tab(&mut h, tab_strings().tab_console);
    assert!(step_for(&mut h, Duration::from_secs(90), |h| connected(
        h, &key
    )));
    step_for(&mut h, Duration::from_secs(3), |_| false);
    let rect = image_rect(&h, &key);
    super::console::click_at(&mut h, rect.center());
    snapshot(&h, &key, &format!("{dir}/000.ppm"));
    std::fs::write(format!("{dir}/000.done"), "ready\n").unwrap();
    let mut n = 1;
    let started = Instant::now();
    'files: loop {
        let cmd = format!("{dir}/{n:03}.cmd");
        if !std::path::Path::new(&cmd).exists() {
            if started.elapsed() > Duration::from_secs(3 * 3600) {
                break;
            }
            step_for(&mut h, Duration::from_millis(500), |_| false);
            continue;
        }
        let text = std::fs::read_to_string(&cmd).unwrap();
        let mut report = String::new();
        for line in text.lines() {
            let line = line.trim_end();
            let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
            match verb {
                "type" => type_us(&mut h, rest),
                "key" => {
                    let mut parts = rest.split_whitespace();
                    let name = parts.next().unwrap_or_default();
                    let count: usize = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
                    match KeyCode::ALL.iter().find(|k| format!("{k:?}") == name) {
                        Some(k) => {
                            for _ in 0..count {
                                tap(&mut h, *k, false);
                                step_for(&mut h, Duration::from_millis(150), |_| false);
                            }
                        }
                        None => report.push_str(&format!("unknown key {name}\n")),
                    }
                }
                "wait" => {
                    let secs: u64 = rest.parse().unwrap_or(1);
                    step_for(&mut h, Duration::from_secs(secs), |_| false);
                }
                "shot" => {
                    snapshot(&h, &key, &format!("{dir}/{rest}.ppm"));
                }
                "end" => {
                    std::fs::write(format!("{n:03}.done"), "end\n").ok();
                    std::fs::write(format!("{dir}/{n:03}.done"), "end\n").unwrap();
                    break 'files;
                }
                "" => {}
                other => report.push_str(&format!("unknown command {other}\n")),
            }
            if !connected(&h, &key) {
                step_for(&mut h, Duration::from_secs(60), |h| connected(h, &key));
            }
        }
        step_for(&mut h, Duration::from_secs(2), |_| false);
        if connected(&h, &key) {
            snapshot(&h, &key, &format!("{dir}/{n:03}.ppm"));
        }
        let status = view(&h, &key).map(|v| format!("{:?}", v.handle.status()));
        std::fs::write(
            format!("{dir}/{n:03}.done"),
            format!("{report}status {status:?}\n"),
        )
        .unwrap();
        n += 1;
    }
}

/// Disk, DVD, tools, snapshot, copy and template operations on the test
/// VM, then everything the tests made is deleted.
#[test]
#[ignore = "needs a real XCP-ng host; acts on the test VM and what it makes, then deletes them"]
fn real_f7_test_vm_operations() {
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let p = prov_strings();
    let t = tab_strings();
    let o = ops_strings();
    let mut log = String::new();
    let named = vms_named(&h, TEST_VM);
    assert_eq!(
        named.len(),
        1,
        "refusing: {} VMs named {TEST_VM:?}",
        named.len()
    );
    let vm = named[0].clone();
    let local = sr_named(&h, TEST_SR);
    assert_eq!(local.len(), 1);
    let local = local[0].clone();
    let c = conn(&h).id;
    let vdis_before: HashSet<Ref<Vdi>> = conn(&h).model.vdis.keys().cloned().collect();
    let names = |suffix: &str| format!("{TEST_VM}-{suffix}");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let step = |h: &mut Harness<'_, XenAdminApp>,
                    log: &mut String,
                    what: &str,
                    prefix: &str,
                    before: u64,
                    secs: u64| {
            let r = wait_op(h, prefix, before, Duration::from_secs(secs));
            note(log, format!("{what}: {r:?}"));
            r.unwrap();
        };
        select(&mut h, Selection::Vm(c, vm.clone()));
        let running = vm_rec(&h, vm.as_str()).power_state == VmPowerState::Running;
        note(&mut log, format!("test VM found; running: {running}"));

        // 1. Install tools (running), then eject.
        if running {
            right_click(&mut h, TEST_VM);
            press(&mut h, crate::provision_ui::install_tools_label());
            let before = last_op(&h);
            click_last(
                &mut h,
                &fill(p.install_tools_button, &[("tools", p.tools_name)]),
            );
            step(
                &mut h,
                &mut log,
                "install tools (tools ISO into the DVD drive)",
                &format!("Installing {} on VM {TEST_VM}", p.tools_name),
                before,
                120,
            );
            tab(&mut h, t.tab_storage);
            let before = last_op(&h);
            press(&mut h, p.eject);
            step(
                &mut h,
                &mut log,
                "eject",
                &format!("Setting DVD Drive on {TEST_VM} as empty."),
                before,
                120,
            );
        }

        // 2. A new 1 GiB disk on Local storage (plugged in if running).
        tab(&mut h, t.tab_storage);
        press(&mut h, p.add_disk);
        let disk_name = names("disk1");
        h.state_mut()
            .provision
            .set_dialog_fields(Some(&disk_name), Some(local.clone()));
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.create);
        step(
            &mut h,
            &mut log,
            "new disk",
            &format!("Creating disk '{disk_name}' on SR '{TEST_SR}'"),
            before,
            300,
        );

        // 3. Shut the VM down (forced if it does not answer).
        if vm_rec(&h, vm.as_str()).power_state == VmPowerState::Running {
            right_click(&mut h, TEST_VM);
            let clean = shows(&h, o.shut_down);
            let before = last_op(&h);
            press(
                &mut h,
                if clean {
                    o.shut_down
                } else {
                    o.force_shut_down
                },
            );
            press(&mut h, o.yes);
            let r = wait_op(
                &mut h,
                &format!("Shutting down VM '{TEST_VM}'"),
                before,
                Duration::from_secs(180),
            );
            note(
                &mut log,
                format!(
                    "shut down ({}): {r:?}",
                    if clean { "clean" } else { "forced" }
                ),
            );
            if vm_rec(&h, vm.as_str()).power_state != VmPowerState::Halted {
                right_click(&mut h, TEST_VM);
                let before = last_op(&h);
                press(&mut h, o.force_shut_down);
                press(&mut h, o.yes);
                step(
                    &mut h,
                    &mut log,
                    "force shut down",
                    &format!("Shutting down VM '{TEST_VM}'"),
                    before,
                    180,
                );
            }
        }
        assert_eq!(vm_rec(&h, vm.as_str()).power_state, VmPowerState::Halted);

        // 4. Resize the new disk to 2 GiB (Properties), detach, attach,
        // delete.
        tab(&mut h, t.tab_storage);
        click_last(&mut h, &disk_name);
        press(&mut h, p.disk_properties);
        h.state_mut().provision.set_resize_gib(2.0);
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.ok);
        step(
            &mut h,
            &mut log,
            "resize 1 → 2 GiB",
            &format!("Changing disk size for '{disk_name}'"),
            before,
            300,
        );
        let d = vdi_named(&h, &disk_name);
        assert_eq!(conn(&h).model.vdis[&d[0]].virtual_size, 2 << 30);
        click_last(&mut h, &disk_name);
        press(&mut h, p.detach_disk);
        let before = last_op(&h);
        press(&mut h, p.yes);
        step(
            &mut h,
            &mut log,
            "detach",
            &format!("Detaching disk '{disk_name}' from VM '{TEST_VM}'"),
            before,
            300,
        );
        press(&mut h, p.attach_disk);
        click_last(
            &mut h,
            &format!("    {disk_name} ({})", xen_core::format::disk_size(2 << 30)),
        );
        let before = last_op(&h);
        press(&mut h, p.attach_button);
        step(
            &mut h,
            &mut log,
            "attach",
            &format!("Attaching virtual disk '{disk_name}' to VM '{TEST_VM}'"),
            before,
            300,
        );
        click_last(&mut h, &disk_name);
        press(&mut h, p.delete_disk);
        let before = last_op(&h);
        press(&mut h, p.yes);
        step(
            &mut h,
            &mut log,
            "delete disk",
            &format!("Deleting disk '{disk_name}' from SR '{TEST_SR}'"),
            before,
            300,
        );
        assert!(vdi_named(&h, &disk_name).is_empty());

        // 5. Snapshot, revert, (the snapshot is deleted with the VM).
        tab(&mut h, p.tab_snapshots);
        press(&mut h, p.take_snapshot_button);
        let snap_name = names("snap");
        h.state_mut()
            .provision
            .set_dialog_fields(Some(&snap_name), None);
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.take_snapshot_ok);
        step(
            &mut h,
            &mut log,
            "snapshot",
            &format!("Taking a snapshot of VM '{TEST_VM}'"),
            before,
            300,
        );
        click_first(&mut h, &snap_name);
        settle(&mut h);
        press(&mut h, p.revert_button);
        press(&mut h, p.revert_new_snapshot);
        let before = last_op(&h);
        press(&mut h, p.yes);
        step(
            &mut h,
            &mut log,
            "revert",
            &format!("Revert snapshot '{snap_name}'"),
            before,
            300,
        );

        // 6. Template from the snapshot.
        click_first(&mut h, &snap_name);
        settle(&mut h);
        press(&mut h, p.snapshot_actions);
        press(&mut h, p.template_from_snapshot);
        let tfs = names("template-from-snap");
        h.state_mut().provision.set_dialog_fields(Some(&tfs), None);
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.ok);
        step(
            &mut h,
            &mut log,
            "template from snapshot",
            &format!("Creating '{tfs}' by cloning '{snap_name}'"),
            before,
            300,
        );

        // 7. Fast clone and full copy (to Local storage).
        right_click(&mut h, TEST_VM);
        press(&mut h, p.copy_vm);
        let clone = names("clone");
        h.state_mut()
            .provision
            .set_dialog_fields(Some(&clone), None);
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.copy_button);
        step(
            &mut h,
            &mut log,
            "fast clone",
            &format!("Creating '{clone}' by cloning '{TEST_VM}'"),
            before,
            600,
        );
        right_click(&mut h, TEST_VM);
        press(&mut h, p.copy_vm);
        let copy = names("copy");
        press(&mut h, p.full_copy);
        h.state_mut()
            .provision
            .set_dialog_fields(Some(&copy), Some(local.clone()));
        settle(&mut h);
        let before = last_op(&h);
        press(&mut h, p.copy_button);
        step(
            &mut h,
            &mut log,
            "full copy to Local storage",
            &format!("Copying VM '{TEST_VM}' to '{copy}' on SR '{TEST_SR}'"),
            before,
            1800,
        );

        // 8. Convert the copy to a template.
        let copy_ref = vms_named(&h, &copy)[0].clone();
        select(&mut h, Selection::Vm(c, copy_ref));
        click_first(&mut h, strings().menu_vm);
        settle(&mut h);
        press(&mut h, p.convert_to_template);
        let before = last_op(&h);
        press(&mut h, p.convert_button);
        step(
            &mut h,
            &mut log,
            "convert to template",
            &format!("Converting VM '{copy}' to template"),
            before,
            300,
        );

        // 9. Delete everything, disks and snapshots included.
        for name in [tfs.as_str(), copy.as_str(), clone.as_str(), TEST_VM] {
            let r = vms_named(&h, name);
            assert_eq!(r.len(), 1, "{name}");
            // The VM menu (the tree row may be scrolled out of view).
            select(&mut h, Selection::Vm(c, r[0].clone()));
            click_first(&mut h, strings().menu_vm);
            settle(&mut h);
            let is_template = vm_rec(&h, r[0].as_str()).is_a_template;
            press(
                &mut h,
                if is_template {
                    p.delete_template
                } else {
                    p.delete_vm
                },
            );
            press(&mut h, p.select_all);
            let before = last_op(&h);
            press(&mut h, p.delete_button);
            step(
                &mut h,
                &mut log,
                &format!("delete {name:?} with its disks and snapshots"),
                &format!("Deleting VM '{name}' from '"),
                before,
                600,
            );
        }
    }));
    // Cleanup, whatever happened: every VM named with the prefix, with the
    // disks its drives own, and the disks these tests made.
    let session = cleanup_session(&runtime);
    let cleanup = runtime.block_on(async {
        let mut removed = Vec::new();
        let vms = xapi::vm::get_all_records(&session).await?;
        for (r, v) in &vms {
            if !v.name_label.starts_with(TEST_VM) {
                continue;
            }
            if v.power_state != VmPowerState::Halted && !v.is_a_snapshot {
                let _ = xapi::vm::hard_shutdown(&session, r).await;
            }
            let mut disks = Vec::new();
            for b in &v.vbds {
                if let Ok(b) = xapi::vbd::get_record(&session, b).await
                    && b.r#type == xapi::VbdType::Disk
                    && !b.vdi.is_null()
                {
                    disks.push(b.vdi);
                }
            }
            let _ = xapi::vm::destroy(&session, r).await;
            for d in disks {
                let _ = xapi::vdi::destroy(&session, &d).await;
            }
            removed.push(v.name_label.clone());
        }
        // Every disk these tests make is named after the test VM.
        let vdis = xapi::vdi::get_all_records(&session).await?;
        let mut stray = Vec::new();
        for (r, v) in &vdis {
            if v.name_label.starts_with(TEST_VM) {
                stray.push(v.name_label.clone());
                let _ = xapi::vdi::destroy(&session, r).await;
            }
        }
        Ok::<_, xapi::Error>((removed, stray))
    });
    note(
        &mut log,
        format!("cleanup (left by the steps): {cleanup:?}"),
    );
    let vms = runtime
        .block_on(xapi::vm::get_all_records(&session))
        .unwrap();
    let left: Vec<&str> = vms
        .values()
        .map(|v| v.name_label.as_str())
        .filter(|n| n.starts_with(TEST_VM))
        .collect();
    let vdis = runtime
        .block_on(xapi::vdi::get_all_records(&session))
        .unwrap();
    let new_on_local: Vec<&str> = vdis
        .iter()
        .filter(|(r, v)| v.sr == local && !vdis_before.contains(r))
        .map(|(_, v)| v.name_label.as_str())
        .collect();
    let named_after_test: Vec<&str> = vdis
        .values()
        .map(|v| v.name_label.as_str())
        .filter(|n| n.starts_with(TEST_VM))
        .collect();
    // Against the disks of Local storage before the test VM was made.
    let baseline = std::env::var("XENADMIN_REAL_BASELINE")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().map(str::to_owned).collect::<HashSet<String>>());
    // Unmanaged "base copy" nodes (the parents of snapshot and clone
    // chains) are the SR's to remove: its garbage collector deletes them
    // once nothing uses them, minutes later.
    let since_baseline: Option<Vec<&str>> = baseline.as_ref().map(|b| {
        vdis.iter()
            .filter(|(r, v)| v.sr == local && !b.contains(r.as_str()) && v.managed)
            .map(|(_, v)| v.name_label.as_str())
            .collect()
    });
    let pending_gc = vdis
        .iter()
        .filter(|(r, v)| v.sr == local && !vdis_before.contains(r) && !v.managed)
        .count();
    note(
        &mut log,
        format!(
            "end: VMs named {TEST_VM}*: {left:?}; disks named {TEST_VM}*: {named_after_test:?}; new disks on {TEST_SR} since this test started: {new_on_local:?}; managed ones since before the test VM was made: {since_baseline:?}; unmanaged chain nodes left to the SR's garbage collector: {pending_gc}"
        ),
    );
    runtime.block_on(session.logout()).ok();
    write_log(&log);
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert!(left.is_empty());
    assert!(named_after_test.is_empty());
    if let Some(since) = since_baseline {
        assert!(since.is_empty(), "{since:?}");
    }
}
