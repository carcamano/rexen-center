//! F11a in the window, against the mock: the VM menu's Export wizard
//! (checks of the first page, the VMs page, verify on completion), Export
//! to File of a template, the File menu's Import wizard (an `.xva.gz`
//! uncompressed first, the home server, the SR, the transfer, the
//! imported VM's networks, start after import), a transfer cancelled from
//! the wizard, and the menus' conditions.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi_mock::MockServer;

use super::provisioning::{catch_up, press, select_vm, shows};
use super::*;
use crate::xva_strings::xva_strings;
use crate::xva_ui::{ExportPage, ImportPage};

const DB: &str = "OpaqueRef:vm-db";
const WEB: &str = "OpaqueRef:vm-web";
const TEMPLATE: &str = "OpaqueRef:vm-template-user";
const NFS: &str = "OpaqueRef:sr-nfs";
const H2: &str = "OpaqueRef:host-0002";
const VLAN20: &str = "OpaqueRef:net-vlan20";

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("xen-f11a-ui-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture pool (both servers enabled) connected in the window; disks
/// of 4 blocks of 64 KiB.
fn pool_window(runtime: &tokio::runtime::Runtime) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start(false));
    server.set_field("host", H2, "enabled", json!(true));
    server.set_field("host", H2, "other_config", json!({}));
    server.set_task_timing(2, Duration::from_millis(20));
    server.set_xva_blocks(64 << 10, 4);
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| {
        shows(h, "Lab Pool") && first_conn(h).is_some_and(|c| c.rbac.is_some())
    });
    settle(&mut h);
    (server, h)
}

/// Opens the menu-bar menu `menu`.
fn open_menu(h: &mut Harness<'_, XenAdminApp>, menu: &str) {
    settle(h);
    h.get_all_by_label(menu).next().expect("menu").click();
    settle(h);
}

/// Waits for the latest operation titled `title` to end; its status.
fn wait_end(h: &mut Harness<'_, XenAdminApp>, title: &str) -> xen_core::OperationStatus {
    step_until(h, title, |h| op_done(h, title));
    catch_up(h);
    h.state()
        .backend
        .state()
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server) == title)
        .unwrap()
        .status
        .clone()
}

fn succeeded(status: &xen_core::OperationStatus) -> bool {
    matches!(status, xen_core::OperationStatus::Succeeded)
}

fn export_page(h: &Harness<'_, XenAdminApp>) -> Option<ExportPage> {
    h.state().xva.export.as_ref().map(|w| w.page)
}

fn import_page(h: &Harness<'_, XenAdminApp>) -> Option<ImportPage> {
    h.state().xva.import.as_ref().map(|w| w.page)
}

fn imported(server: &MockServer) -> Vec<String> {
    server
        .refs("vm")
        .into_iter()
        .filter(|r| {
            server
                .field("vm", r, "other_config")
                .is_some_and(|o| o.get("import_task").is_some())
        })
        .collect()
}

/// Exports db-01 through the VM menu's wizard into `dir`.
fn export_db(h: &mut Harness<'_, XenAdminApp>, dir: &Path, verify: bool) {
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    select_vm(h, DB);
    open_menu(h, strings().menu_vm);
    press(h, x.menu_export);
    assert_eq!(export_page(h), Some(ExportPage::Appliance));
    // The name is the VM's; the folder starts at Downloads.
    assert_eq!(h.state().xva.export.as_ref().unwrap().name, "db-01");
    h.state_mut().xva.export.as_mut().unwrap().folder = dir.display().to_string();
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Vms));
    assert!(shows(h, x.page_vms_title));
    // db-01 is ticked; the running web-01 is not offered.
    assert_eq!(
        h.state().xva.export.as_ref().unwrap().ticked,
        vec![Ref::new(DB)]
    );
    let conn = first_conn(h).unwrap();
    let offered: Vec<String> = xen_core::xva::exportable_vms(&conn.model, false, &conn.busy_vms)
        .iter()
        .map(|(_, v)| v.name_label.clone())
        .collect();
    assert!(offered.contains(&"db-01".to_owned()) && !offered.contains(&"web-01".to_owned()));
    assert!(shows(h, x.one_vm_selected));
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Finish));
    assert!(shows(h, x.summary_package) && shows(h, &dir.display().to_string()));
    if verify {
        press(h, x.verify);
        assert!(shows(h, x.verify_warning));
    }
    press(h, n.finish);
    assert!(export_page(h).is_none());
}

#[test]
fn the_export_wizard_and_export_to_file() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    let dir = temp_dir();

    // A running VM cannot be exported: the entry is off.
    select_vm(&mut h, WEB);
    open_menu(&mut h, strings().menu_vm);
    assert!(!super::provisioning::last_enabled(&h, x.menu_export));
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    export_db(&mut h, &dir, true);
    let title = "Export VM 'db-01' from 'Lab Pool'";
    assert!(succeeded(&wait_end(&mut h, title)));
    let file = dir.join("db-01.xva");
    assert!(file.is_file());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

    // Again: the file exists now, the first page says so.
    select_vm(&mut h, DB);
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, x.menu_export);
    h.state_mut().xva.export.as_mut().unwrap().folder = dir.display().to_string();
    press(&mut h, n.next);
    assert!(shows(&h, x.error_exists));
    assert_eq!(export_page(&h), Some(ExportPage::Appliance));
    // A folder that does not exist.
    h.state_mut().xva.export.as_mut().unwrap().folder = dir.join("missing").display().to_string();
    press(&mut h, n.next);
    assert!(shows(&h, x.error_dir_missing));
    // A name with a slash.
    {
        let w = h.state_mut().xva.export.as_mut().unwrap();
        w.folder = dir.display().to_string();
        w.name = "a/b".into();
    }
    settle(&mut h);
    assert!(shows(&h, x.error_invalid_name));
    press(&mut h, n.cancel);
    assert!(export_page(&h).is_none());

    // Export to File of a template (VM menu with the template selected).
    let c = first_conn(&h).unwrap().id;
    h.state_mut().selection = Some(Selection::Vm(c, Ref::new(TEMPLATE)));
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, x.menu_export_template);
    let path = dir.join("template");
    {
        let d = h.state_mut().xva.export_file.as_mut().expect("the dialog");
        assert!(d.verify, "verify is on by default");
        d.path = path.display().to_string();
    }
    press(&mut h, x.export_button);
    assert!(h.state().xva.export_file.is_none());
    assert!(succeeded(&wait_end(
        &mut h,
        "Export VM 'Web server template' from 'Lab Pool'"
    )));
    assert!(dir.join("template.xva").is_file(), "the extension is added");
    drop(server);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_import_wizard_from_a_compressed_file() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    let dir = temp_dir();
    export_db(&mut h, &dir, false);
    assert!(succeeded(&wait_end(
        &mut h,
        "Export VM 'db-01' from 'Lab Pool'"
    )));
    let xva = dir.join("db-01.xva");
    // Compress it: the wizard uncompresses it first.
    let gz = dir.join("db-01.xva.gz");
    {
        use std::io::Write;
        let mut e = flate2_writer(&gz);
        e.write_all(&std::fs::read(&xva).unwrap()).unwrap();
        e.finish().unwrap();
    }
    std::fs::remove_file(&xva).unwrap();

    // File menu, with the pool selected: the pool is the target.
    let c = first_conn(&h).unwrap().id;
    h.state_mut().selection = Some(Selection::Pool(c, Ref::new("OpaqueRef:pool-0001")));
    open_menu(&mut h, strings().menu_file);
    press(&mut h, x.menu_import);
    assert_eq!(import_page(&h), Some(ImportPage::Source));
    // Another format is refused for now.
    h.state_mut().xva.import.as_mut().unwrap().path = dir.join("a.ova").display().to_string();
    press(&mut h, n.next);
    assert!(shows(&h, x.error_later_phase));
    // A file that does not exist.
    h.state_mut().xva.import.as_mut().unwrap().path = dir.join("none.xva").display().to_string();
    press(&mut h, n.next);
    assert!(shows(&h, x.error_path_missing));
    h.state_mut().xva.import.as_mut().unwrap().path = gz.display().to_string();
    press(&mut h, n.next);
    // The question, then the uncompression.
    assert!(
        h.query_all_by_label_contains("will be deleted")
            .next()
            .is_some()
    );
    press(&mut h, crate::ops_strings::ops_strings().yes);
    step_until(&mut h, "uncompressed", |h| {
        import_page(h) == Some(ImportPage::Home)
    });
    assert!(
        xva.is_file() && !gz.exists(),
        "the .gz is replaced by the .xva"
    );
    assert_eq!(
        h.state().xva.import.as_ref().unwrap().path,
        xva.display().to_string()
    );
    // Home server: the pool, as selected; pick server 1 instead.
    assert_eq!(
        h.state().xva.import.as_ref().unwrap().target,
        Some((c, None))
    );
    press(&mut h, "xcp-host-01");
    press(&mut h, n.next);
    assert_eq!(import_page(&h), Some(ImportPage::Storage));
    // The pool's default SR is chosen; take NFS anyway, then Import.
    assert_eq!(
        h.state()
            .xva
            .import
            .as_ref()
            .unwrap()
            .sr
            .as_ref()
            .map(|r| r.as_str()),
        Some(NFS)
    );
    press(&mut h, x.import_button);
    step_until(&mut h, "networks", |h| {
        import_page(h) == Some(ImportPage::Network)
    });
    let vm = h.state().xva.import.as_ref().unwrap().vm.clone().unwrap();
    // The imported VM's interface 0, on its network; moved to VLAN 20.
    {
        let w = h.state_mut().xva.import.as_mut().unwrap();
        assert_eq!(w.vifs.len(), 1);
        assert_eq!(w.vifs[0].device, "0");
        w.vifs[0].network = Ref::new(VLAN20);
    }
    // A bad MAC holds the page.
    h.state_mut().xva.import.as_mut().unwrap().vifs[0].mac = "zz".into();
    settle(&mut h);
    assert!(!super::provisioning::last_enabled(&h, n.next));
    h.state_mut().xva.import.as_mut().unwrap().vifs[0].mac = String::new();
    press(&mut h, n.next);
    assert_eq!(import_page(&h), Some(ImportPage::Finish));
    assert!(shows(&h, x.summary_vm_name) && shows(&h, x.summary_home));
    assert!(shows(&h, x.summary_storage) && shows(&h, x.summary_network));
    assert!(shows(&h, x.start_vms));
    press(&mut h, n.finish);
    assert!(import_page(&h).is_none());
    let title = format!(
        "Import VM from '{}' to 'xcp-host-01 in 'Lab Pool''",
        xva.display()
    );
    assert!(succeeded(&wait_end(&mut h, &title)));
    // Started after import, on VLAN 20, with server 1 as home.
    step_until(&mut h, "running", |_| {
        server.vm_power_state(vm.as_str()) == "Running"
    });
    let vif = server.field("vm", vm.as_str(), "VIFs").unwrap()[0]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(server.field("vif", &vif, "network").unwrap(), VLAN20);
    assert_eq!(
        server.field("vm", vm.as_str(), "affinity").unwrap(),
        "OpaqueRef:host-0001"
    );
    assert_eq!(
        server.field("vm", vm.as_str(), "name_label").unwrap(),
        "db-01 (1)"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancelling_the_import_wizard_cancels_the_transfer() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let x = xva_strings();
    let n = crate::net_strings::net_strings();
    let dir = temp_dir();
    server.set_xva_blocks(64 << 10, 16);
    export_db(&mut h, &dir, false);
    assert!(succeeded(&wait_end(
        &mut h,
        "Export VM 'db-01' from 'Lab Pool'"
    )));
    let xva = dir.join("db-01.xva");
    let vdis = server.field("sr", NFS, "VDIs").unwrap();
    // A slow server.
    server.set_transfer_pace(4096, Duration::from_millis(4));
    let c = first_conn(&h).unwrap().id;
    h.state_mut().xva.open_import(Some((c, None)));
    h.state_mut().xva.import.as_mut().unwrap().path = xva.display().to_string();
    press(&mut h, n.next);
    press(&mut h, n.next);
    assert_eq!(import_page(&h), Some(ImportPage::Storage));
    press(&mut h, x.import_button);
    step_until(&mut h, "uploading", |h| {
        h.state()
            .xva
            .import
            .as_ref()
            .and_then(|w| w.operation.as_ref())
            .is_some_and(|o| matches!(o.step, Some(xen_core::OperationStep::Uploading { percent, .. }) if percent >= 5))
    });
    // The transfer's progress shows on the page.
    assert!(shows(
        &h,
        &format!(
            "Import VM from '{}' to 'xcp-host-01 in 'Lab Pool''",
            xva.display()
        )
    ));
    press(&mut h, n.cancel);
    assert!(import_page(&h).is_none());
    let title = format!(
        "Import VM from '{}' to 'xcp-host-01 in 'Lab Pool''",
        xva.display()
    );
    let status = wait_end(&mut h, &title);
    assert!(
        matches!(status, xen_core::OperationStatus::Cancelled),
        "{status:?}"
    );
    step_until(&mut h, "nothing left", |_| imported(&server).is_empty());
    assert_eq!(server.field("sr", NFS, "VDIs").unwrap(), vdis);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn flate2_writer(path: &Path) -> flate2::write::GzEncoder<std::fs::File> {
    flate2::write::GzEncoder::new(
        std::fs::File::create(path).unwrap(),
        flate2::Compression::fast(),
    )
}
