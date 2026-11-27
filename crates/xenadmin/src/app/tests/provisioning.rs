//! F7 in the window, against the mock: the New VM wizard, the Snapshots
//! tab and its dialogs, Copy VM / Convert to Template / Delete VM, the VM
//! Storage tab's disk buttons and DVD drives, Install Tools, and role
//! elevation for these operations. Plus the real-host checks on the VM
//! named exactly by `XENADMIN_REAL_NAMED_VM` and on the one disposable test VM
//! (ignored tests).

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;
use xapi::{Ref, Vdi, VmPowerState};
use xapi_mock::MockServer;

use super::*;
use crate::prov_strings::prov_strings;
use crate::provision_ui::install_tools_label;

const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";

pub(super) fn select(h: &mut Harness<'_, XenAdminApp>, selection: Selection) {
    h.state_mut().selection = Some(selection);
    settle(h);
}

pub(super) fn conn_id(h: &Harness<'_, XenAdminApp>) -> ConnectionId {
    first_conn(h).unwrap().id
}

pub(super) fn select_vm(h: &mut Harness<'_, XenAdminApp>, r: &str) {
    let c = conn_id(h);
    select(h, Selection::Vm(c, Ref::new(r)));
}

/// Shows a tab of the selection by its title.
pub(super) fn tab(h: &mut Harness<'_, XenAdminApp>, title: &str) {
    click_tab(h, title);
    settle(h);
}

/// Clicks the last node labelled `label` and lets the window catch up.
pub(super) fn press(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    click_last(h, label);
    settle(h);
}

/// Whether some node is labelled `label`.
pub(super) fn shows(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

/// Whether the last node labelled `label` (a dialog's button, after its
/// title) is enabled.
pub(super) fn last_enabled(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
    h.query_all_by_label(label)
        .last()
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

/// Opens the only drop-down list shown.
pub(super) fn open_combo(h: &mut Harness<'_, XenAdminApp>) {
    settle(h);
    h.get_by_role(Role::ComboBox).click();
    settle(h);
}

/// Types `text` into the text field labelled `label`.
pub(super) fn type_into(h: &mut Harness<'_, XenAdminApp>, label: &str, text: &str) {
    h.get_by_role_and_label(Role::TextInput, label).focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, label)
        .type_text(text);
    settle(h);
}

/// Waits for the operation titled `title` and checks it succeeded.
fn wait_ok(h: &mut Harness<'_, XenAdminApp>, title: &str) {
    step_until(h, title, |h| op_done(h, title));
    let state = h.state().backend.state();
    let op = state
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server) == title)
        .unwrap();
    assert!(
        matches!(op.status, xen_core::OperationStatus::Succeeded),
        "{title}: {:?}",
        op.status
    );
    catch_up(h);
}

/// Lets the window's cache take in the last events (the backend publishes
/// a new snapshot at most every 100 ms).
pub(super) fn catch_up(h: &mut Harness<'_, XenAdminApp>) {
    for _ in 0..15 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits for an operation whose title starts with `prefix` to succeed.
fn wait_prefix(h: &mut Harness<'_, XenAdminApp>, prefix: &str) {
    step_until(h, prefix, |h| {
        h.state().backend.state().operations.iter().any(|o| {
            !o.is_running() && i18n::operation_title(&o.kind, &o.server).starts_with(prefix)
        })
    });
    let state = h.state().backend.state();
    let op = state
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server).starts_with(prefix))
        .unwrap();
    assert!(
        matches!(op.status, xen_core::OperationStatus::Succeeded),
        "{prefix}: {:?}",
        op.status
    );
    catch_up(h);
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> serde_json::Value {
    server.field(class, r, f).unwrap_or(serde_json::Value::Null)
}

fn vm_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("vm")
        .into_iter()
        .find(|v| field(server, "vm", v, "name_label") == json!(name))
}

pub(super) fn wizard<'a>(
    h: &'a mut Harness<'_, XenAdminApp>,
) -> &'a mut crate::new_vm::NewVmWizard {
    h.state_mut()
        .provision
        .wizard
        .as_mut()
        .expect("the wizard is open")
}

/// The New VM wizard end to end: Debian template, a name, an ISO, UEFI
/// Secure Boot, a home server, vCPUs, the provisioned disk plus a new one,
/// two networks; the VM is made, started and selected.
#[test]
fn the_new_vm_wizard_creates_and_starts_a_vm() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let p = prov_strings();
    let c = conn_id(&h);
    select(&mut h, Selection::Host(c, Ref::new("OpaqueRef:host-0001")));
    // Template page.
    click_first(&mut h, p.new_vm_toolbar);
    step_until(&mut h, "wizard", |h| {
        h.query_by_label(p.title_template).is_some()
    });
    assert!(!is_enabled(&h, p.next), "no template chosen yet");
    // The list: custom templates, then default ones, then snapshots.
    for name in [
        "Web server template",
        "Debian Bookworm 12",
        "web-01 before upgrade",
    ] {
        assert!(shows(&h, name), "{name}");
    }
    click_last(&mut h, "Debian Bookworm 12");
    settle(&mut h);
    press(&mut h, p.next);
    // Name page: the default name, then ours.
    h.get_by_label(p.title_name);
    assert_eq!(wizard(&mut h).name, "Debian Bookworm 12 (1)");
    wizard(&mut h).name = "deb-ui".into();
    settle(&mut h);
    press(&mut h, p.next);
    // Media page: an ISO is required for this template.
    h.get_by_label(p.title_media);
    assert!(!is_enabled(&h, p.next));
    open_combo(&mut h);
    click_last(&mut h, "    debian-12.iso");
    assert!(is_enabled(&h, p.next));
    // UEFI Secure Boot is preselected (secureboot=auto); add a vTPM.
    assert_eq!(
        wizard(&mut h).boot_mode,
        Some(xen_core::provisioning::BootMode::UefiSecure)
    );
    click_last(&mut h, p.vtpm_check);
    press(&mut h, p.next);
    // Home server: the host selected in the tree.
    h.get_by_label(p.title_home);
    assert_eq!(
        wizard(&mut h)
            .affinity
            .as_ref()
            .map(|r| r.as_str().to_owned()),
        Some("OpaqueRef:host-0001".to_owned())
    );
    press(&mut h, p.next);
    // CPU & memory.
    h.get_by_label(p.title_cpu_mem);
    wizard(&mut h).vcpus_max = 2;
    wizard(&mut h).vcpus_at_startup = 2;
    settle(&mut h);
    press(&mut h, p.next);
    // Storage: the provisioned 10 GiB disk on the default SR, plus a new one.
    h.get_by_label(p.title_storage);
    h.get_by_label("deb-ui 0");
    click_last(&mut h, p.add);
    step_until(&mut h, "add disk", |h| {
        h.query_by_label(p.add_disk_title).is_some()
    });
    click_last(&mut h, p.ok);
    settle(&mut h);
    assert_eq!(wizard(&mut h).disks.len(), 2);
    press(&mut h, p.next);
    // Networking: one row per shown network, at most four.
    h.get_by_label(p.title_networking);
    let vifs = wizard(&mut h).vifs.len();
    assert!((1..=4).contains(&vifs), "{vifs}");
    press(&mut h, p.next);
    // Finish.
    h.get_by_label(p.title_finish);
    assert!(shows(&h, "deb-ui"));
    click_last(&mut h, p.create_now);
    wait_ok(&mut h, "Create VM 'deb-ui'");
    let vm = vm_named(&server, "deb-ui").expect("made");
    // The new VM is selected once made.
    step_until(&mut h, "selected", |h| {
        h.state().selection == Some(Selection::Vm(c, Ref::new(&vm)))
    });
    let rec = server.record("vm", &vm).unwrap();
    assert_eq!(rec["power_state"], json!("Running"));
    assert_eq!(rec["platform"]["secureboot"], json!("true"));
    assert_eq!(rec["VTPMs"].as_array().unwrap().len(), 1);
    assert_eq!(rec["VIFs"].as_array().unwrap().len(), vifs);
    assert_eq!(rec["VBDs"].as_array().unwrap().len(), 3);
    assert!(h.state().provision.wizard.is_none());
}

/// Snapshots tab: take a snapshot (with its dialog), revert to it with a
/// snapshot of the current state taken first, make a template and a VM from
/// it, and delete snapshots.
#[test]
fn the_snapshots_tab_takes_reverts_and_deletes() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let p = prov_strings();
    select_vm(&mut h, DB);
    tab(&mut h, p.tab_snapshots);
    h.get_by_label(p.snapshots_empty);
    click_last(&mut h, p.take_snapshot_button);
    step_until(&mut h, "dialog", |h| shows(h, p.take_snapshot_title));
    assert!(!last_enabled(&h, p.take_snapshot_ok), "a name is required");
    // Disks and memory need a running VM.
    assert!(!is_enabled(&h, p.snapshot_memory));
    type_into(&mut h, p.name_label, "before-upgrade");
    click_last(&mut h, p.take_snapshot_ok);
    wait_ok(&mut h, "Taking a snapshot of VM 'db-01'...");
    step_until(&mut h, "listed", |h| {
        h.query_by_label("before-upgrade").is_some()
    });
    h.get_by_label(p.disks_only);

    // Revert, taking a snapshot first.
    click_first(&mut h, "before-upgrade");
    settle(&mut h);
    click_last(&mut h, p.revert_button);
    step_until(&mut h, "revert question", |h| {
        h.query_by_label(&fill(p.revert_blurb, &[("name", "before-upgrade")]))
            .is_some()
    });
    click_last(&mut h, p.yes);
    step_until(&mut h, "snapshot first", |h| {
        shows(h, p.take_snapshot_title)
    });
    type_into(&mut h, p.name_label, "pre-revert");
    click_last(&mut h, p.take_snapshot_ok);
    wait_ok(&mut h, "Revert snapshot 'before-upgrade'");
    assert!(vm_named(&server, "pre-revert").is_some());
    assert!(server.calls().iter().any(|c| c == "Async.VM.revert"));

    // A template from the snapshot (Actions menu).
    step_until(&mut h, "both listed", |h| {
        h.query_by_label("pre-revert").is_some()
    });
    click_first(&mut h, "before-upgrade");
    settle(&mut h);
    click_last(&mut h, p.snapshot_actions);
    click_last(&mut h, p.template_from_snapshot);
    step_until(&mut h, "prompt", |h| {
        h.query_by_label(p.save_as_template).is_some()
    });
    click_last(&mut h, p.ok);
    wait_ok(
        &mut h,
        "Creating 'Template from snapshot 'before-upgrade'' by cloning 'before-upgrade'",
    );
    let t = vm_named(&server, "Template from snapshot 'before-upgrade'").unwrap();
    assert_eq!(field(&server, "vm", &t, "is_a_template"), json!(true));

    // New VM from the snapshot opens the wizard on it.
    click_first(&mut h, "before-upgrade");
    settle(&mut h);
    click_last(&mut h, p.snapshot_actions);
    click_last(&mut h, p.new_vm_from_snapshot);
    settle(&mut h);
    let snap = vm_named(&server, "before-upgrade").unwrap();
    assert_eq!(
        wizard(&mut h)
            .template
            .as_ref()
            .map(|r| r.as_str().to_owned()),
        Some(snap.clone())
    );
    press(&mut h, p.cancel);
    assert!(h.state().provision.wizard.is_none());

    // Delete both snapshots (checkboxes select several).
    assert_eq!(h.get_all_by_label(p.disks_only).count(), 2);
    // A name selects its row alone; a checkbox adds the other row.
    click_first(&mut h, "pre-revert");
    settle(&mut h);
    h.get_all_by_label(p.disks_only).next().unwrap().click();
    settle(&mut h);
    h.get_by_label(&fill(p.snapshots_selected, &[("count", "2")]));
    click_last(&mut h, p.delete);
    step_until(&mut h, "question", |h| {
        h.query_by_label(&fill(
            p.remove_snapshot_text,
            &[("what", &fill(p.remove_snapshot_many, &[("count", "2")]))],
        ))
        .is_some()
    });
    click_last(&mut h, p.yes);
    wait_ok(&mut h, p.op_delete_snapshots);
    assert!(vm_named(&server, "before-upgrade").is_none());
    assert!(vm_named(&server, "pre-revert").is_none());
    step_until(&mut h, "empty", |h| {
        h.query_by_label(p.snapshots_empty).is_some()
    });
}

/// Copy VM, Convert to Template and Delete VM from the context menu.
#[test]
fn copy_convert_and_delete_from_the_menus() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let p = prov_strings();
    // A running VM offers neither copy nor delete.
    right_click(&mut h, "web-01");
    assert!(h.query_by_label(p.copy_vm).is_none());
    assert!(h.query_by_label(p.delete_vm).is_none());
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    right_click(&mut h, "db-01");
    click_last(&mut h, p.copy_vm);
    // F12a: a halted VM that may migrate gets the Copy VM wizard
    // (`CopyVMCommand.CanLaunchMigrateWizard`); within the pool its Name
    // and Storage page is the F7 copy.
    let c = crate::cpm_strings::cpm_strings();
    let n = crate::net_strings::net_strings();
    step_until(&mut h, "copy wizard", |h| {
        h.query_by_label(c.within_pool).is_some()
    });
    click_last(&mut h, n.next);
    step_until(&mut h, "name and storage", |h| {
        h.query_by_label(c.intra_rubric_vm).is_some()
    });
    // Full copy to the SR of its disk.
    click_last(&mut h, c.full_copy);
    click_last(&mut h, n.finish);
    wait_ok(
        &mut h,
        "Copying VM 'db-01' to 'Copy of db-01' on SR 'NFS VM storage'",
    );
    let copy = vm_named(&server, "Copy of db-01").unwrap();
    assert_eq!(
        field(&server, "vm", &copy, "name_description"),
        json!("PostgreSQL")
    );
    step_until(&mut h, "in the tree", |h| {
        h.query_by_label("Copy of db-01").is_some()
    });

    // Convert the copy to a template.
    right_click(&mut h, "Copy of db-01");
    click_last(&mut h, p.convert_to_template);
    step_until(&mut h, "question", |h| {
        h.query_by_label(&fill(p.convert_text, &[("vm", "Copy of db-01")]))
            .is_some()
    });
    click_last(&mut h, p.convert_button);
    wait_ok(&mut h, "Converting VM 'Copy of db-01' to template");
    assert_eq!(field(&server, "vm", &copy, "is_a_template"), json!(true));
    step_until(&mut h, "a template in the cache", |h| {
        first_conn(h)
            .unwrap()
            .model
            .vms
            .get(&Ref::new(&copy))
            .is_some_and(|v| v.is_a_template)
    });

    // Delete it with its disk (checked: its drive owns it).
    right_click(&mut h, "Copy of db-01");
    click_last(&mut h, p.delete_template);
    step_until(&mut h, "delete dialog", |h| {
        h.query_by_label(&fill(p.delete_title, &[("what", p.what_template)]))
            .is_some()
    });
    h.get_by_label(p.attached_disks);
    let disk = field(
        &server,
        "vbd",
        field(&server, "vm", &copy, "VBDs")[0].as_str().unwrap(),
        "VDI",
    );
    click_last(&mut h, p.delete_button);
    wait_prefix(&mut h, "Deleting VM 'Copy of db-01' from '");
    assert!(server.record("vm", &copy).is_none());
    assert!(server.record("vdi", disk.as_str().unwrap()).is_none());
}

/// The VM Storage tab: DVD drive ISO and Eject, Add / Properties (resize)
/// / Move / Detach / Attach / Delete a disk, a new DVD drive, Install Tools.
#[test]
fn the_storage_tab_manages_disks_and_dvd_drives() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let p = prov_strings();
    let t = tab_strings();
    let cd = "OpaqueRef:vbd-web-cd";
    select_vm(&mut h, WEB);
    tab(&mut h, t.tab_storage);
    h.get_by_label(&fill(p.dvd_drive, &[("n", "1")]));
    // Load another ISO from the drive's list.
    open_combo(&mut h);
    click_last(&mut h, "    ubuntu-24.04-live-server.iso");
    wait_ok(
        &mut h,
        "Loading ISO 'ubuntu-24.04-live-server.iso' into the DVD Drive on VM web-01",
    );
    assert_eq!(
        field(&server, "vbd", cd, "VDI"),
        json!("OpaqueRef:vdi-ubuntu-iso")
    );
    // The tools ISO is not in the list.
    open_combo(&mut h);
    assert!(h.query_by_label("    guest-tools.iso").is_none());
    open_combo(&mut h);
    click_last(&mut h, p.eject);
    wait_ok(&mut h, "Setting DVD Drive on web-01 as empty.");
    assert_eq!(field(&server, "vbd", cd, "empty"), json!(true));

    // Add a disk.
    click_last(&mut h, p.add_disk);
    step_until(&mut h, "dialog", |h| {
        h.query_by_label(p.add_disk_title).is_some()
    });
    click_last(&mut h, p.create);
    wait_ok(
        &mut h,
        "Creating disk 'New virtual disk' on SR 'NFS VM storage'",
    );
    step_until(&mut h, "listed", |h| {
        h.query_by_label("New virtual disk").is_some()
    });
    let vdi = server
        .refs("vdi")
        .into_iter()
        .find(|v| field(&server, "vdi", v, "name_label") == json!("New virtual disk"))
        .unwrap();
    // Plugged into the running VM.
    assert!(server.calls().iter().any(|c| c == "Async.VBD.plug"));

    // Properties: grow it by 1 GiB.
    click_last(&mut h, "New virtual disk");
    click_last(&mut h, p.disk_properties);
    step_until(&mut h, "resize", |h| {
        h.query_by_label(p.resize_title).is_some()
    });
    assert!(!is_enabled(&h, p.ok), "no change yet");
    h.state_mut().provision.set_resize_gib(2.0);
    settle(&mut h);
    click_last(&mut h, p.ok);
    wait_ok(&mut h, "Changing disk size for 'New virtual disk'...");
    assert_eq!(
        field(&server, "vdi", &vdi, "virtual_size"),
        json!(2i64 << 30)
    );

    // Move it (live, while web-01 runs) to the local SR.
    click_last(&mut h, "New virtual disk");
    click_last(&mut h, p.move_disk);
    step_until(&mut h, "move", |h| h.query_by_label(p.move_title).is_some());
    click_last(&mut h, p.move_button);
    wait_ok(
        &mut h,
        "Moving Virtual Disk 'New virtual disk' from SR 'NFS VM storage' to SR 'Local storage'",
    );
    assert!(server.calls().iter().any(|c| c == "Async.VDI.pool_migrate"));
    let moved = server
        .refs("vdi")
        .into_iter()
        .find(|v| field(&server, "vdi", v, "name_label") == json!("New virtual disk"))
        .unwrap();
    assert_eq!(
        field(&server, "vdi", &moved, "SR"),
        json!("OpaqueRef:sr-local-01")
    );

    // Detach it (kept), attach it again (db-01 cannot see that local SR),
    // then delete it while plugged in.
    step_until(&mut h, "moved in the cache", |h| {
        first_conn(h)
            .unwrap()
            .model
            .vdis
            .contains_key(&Ref::<Vdi>::new(&moved))
    });
    click_last(&mut h, "New virtual disk");
    click_last(&mut h, p.detach_disk);
    step_until(&mut h, "question", |h| {
        h.query_by_label(p.detach_disk_text).is_some()
    });
    click_last(&mut h, p.yes);
    wait_ok(&mut h, "Detaching disk 'New virtual disk' from VM 'web-01'");
    assert!(server.record("vdi", &moved).is_some());
    click_last(&mut h, p.attach_disk);
    step_until(&mut h, "attach", |h| {
        h.query_by_label(p.attach_title).is_some()
    });
    click_last(
        &mut h,
        &format!(
            "    New virtual disk ({})",
            xen_core::format::disk_size(2 << 30)
        ),
    );
    click_last(&mut h, p.attach_button);
    wait_ok(
        &mut h,
        "Attaching virtual disk 'New virtual disk' to VM 'web-01'",
    );
    step_until(&mut h, "listed again", |h| {
        h.query_by_label("New virtual disk").is_some()
    });
    click_last(&mut h, "New virtual disk");
    click_last(&mut h, p.delete_disk);
    step_until(&mut h, "question", |h| {
        h.query_by_label(p.delete_disk_text).is_some()
    });
    click_last(&mut h, p.yes);
    wait_ok(
        &mut h,
        "Deleting disk 'New virtual disk' from SR 'Local storage'",
    );
    assert!(server.record("vdi", &moved).is_none());
    select_vm(&mut h, DB);
    tab(&mut h, t.tab_storage);

    // db-01 has no DVD drive: create one (an HVM VM asks first).
    click_last(&mut h, p.create_dvd_drive);
    step_until(&mut h, "question", |h| {
        h.query_by_label(&fill(p.new_dvd_text, &[("vm", "db-01")]))
            .is_some()
    });
    click_last(&mut h, p.new_dvd_yes);
    wait_ok(&mut h, "Creating new DVD drive on VM db-01");
    step_until(&mut h, "drive shown", |h| {
        h.query_by_label(&fill(p.dvd_drive, &[("n", "1")]))
            .is_some()
    });

    // Install Tools on web-01 (VM menu).
    right_click(&mut h, "web-01");
    click_last(&mut h, install_tools_label());
    step_until(&mut h, "question", |h| {
        shows(h, &fill(p.install_tools_title, &[("tools", p.tools_name)]))
    });
    click_last(
        &mut h,
        &fill(p.install_tools_button, &[("tools", p.tools_name)]),
    );
    wait_ok(&mut h, "Installing XCP-ng VM Tools on VM web-01");
    assert_eq!(
        field(&server, "vbd", cd, "VDI"),
        json!("OpaqueRef:vdi-tools-iso")
    );
    assert_eq!(
        vm_rec(&h, WEB).power_state,
        VmPowerState::Running,
        "nothing else changed"
    );
}

/// A VM operator taking a snapshot is offered the role elevation dialog.
#[test]
fn f7_operations_go_through_role_elevation() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let mut h = app_harness(&runtime);
    h.step();
    let s = strings();
    h.get_by_label(s.connect_server)
        .type_text(&server.address());
    h.step();
    let user = h.get_by_label(s.connect_user);
    user.focus();
    h.step();
    // Replace "root".
    for _ in 0..4 {
        h.key_press(egui::Key::Backspace);
    }
    h.get_by_label(s.connect_user).type_text("vm-operator-user");
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, s.connect_password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, s.connect_password)
        .type_text("vm-operator-password");
    h.step();
    h.get_by_label(s.connect_button).click();
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    let p = prov_strings();
    select_vm(&mut h, DB);
    tab(&mut h, p.tab_snapshots);
    click_last(&mut h, p.take_snapshot_button);
    step_until(&mut h, "dialog", |h| shows(h, p.take_snapshot_title));
    type_into(&mut h, p.name_label, "x");
    click_last(&mut h, p.take_snapshot_ok);
    step_until(&mut h, "elevation", |h| h.state().ops.elevation.is_some());
    assert_eq!(server.call_count("Async.VM.snapshot"), 0);
}
