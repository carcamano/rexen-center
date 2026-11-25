//! F8 in the window, against the mock: the New SR wizard for every type
//! (scans, the iSCSI IQN / LUN lists, the LUN choice, the multi-pool
//! warning, failures shown on the pages), the Storage menu and the SR
//! context menu (detach, forget, reattach, destroy, set as default, trim
//! with their confirmations), the Repair and Properties dialogs, the SR
//! Storage tab's Rescan, and role elevation.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;
use xapi::Ref;
use xapi_mock::MockServer;
use xapi_mock::storage::{CHAP_PASSWORD, CHAP_USER, OLD_NFS_SR, SMB_PASSWORD, SMB_USER};
use xen_core::storage::{LocalFs, SrKind};

use super::provisioning::{catch_up, conn_id, last_enabled, press, select, shows, tab, type_into};
use super::*;
use crate::sr_strings::sr_strings;
use crate::storage_ui::PropertiesTab;

const NFS: &str = "OpaqueRef:sr-nfs";
const ISO: &str = "OpaqueRef:sr-iso";
const LOCAL1: &str = "OpaqueRef:sr-local-01";
const LOCAL2: &str = "OpaqueRef:sr-local-02";
const POOL: &str = "OpaqueRef:pool-0001";

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> serde_json::Value {
    server.field(class, r, f).unwrap_or(serde_json::Value::Null)
}

fn sr_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("sr")
        .into_iter()
        .find(|s| field(server, "sr", s, "name_label") == json!(name))
}

fn wizard<'a>(h: &'a mut Harness<'_, XenAdminApp>) -> &'a mut crate::new_sr::NewSrWizard {
    h.state_mut()
        .storage
        .wizard
        .as_mut()
        .expect("the wizard is open")
}

/// Waits for the operation titled `title`; returns whether it succeeded
/// and the error text the window shows if not.
fn wait_op(h: &mut Harness<'_, XenAdminApp>, title: &str) -> Result<(), String> {
    step_until(h, title, |h| op_done(h, title));
    let state = h.state().backend.state();
    let op = state
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server) == title)
        .unwrap()
        .clone();
    catch_up(h);
    match &op.status {
        xen_core::OperationStatus::Succeeded => Ok(()),
        xen_core::OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            Err(i18n::describe_xapi_error(e))
        }
        other => Err(format!("{other:?}")),
    }
}

fn wait_ok(h: &mut Harness<'_, XenAdminApp>, title: &str) {
    if let Err(e) = wait_op(h, title) {
        panic!("{title}: {e}");
    }
}

pub(super) fn select_sr(h: &mut Harness<'_, XenAdminApp>, r: &str) {
    let c = conn_id(h);
    select(h, Selection::Sr(c, Ref::new(r)));
}

/// Opens the menu bar's Storage menu and chooses `item`.
fn storage_menu(h: &mut Harness<'_, XenAdminApp>, item: &str) {
    let s = sr_strings();
    settle(h);
    h.get_all_by_label(s.menu_storage)
        .next()
        .expect("the Storage menu")
        .click();
    step_until(h, "storage menu", |h| h.query_by_label(item).is_some());
    click_last(h, item);
    settle(h);
}

/// Whether the Storage menu's `item` is enabled (the menu is closed again).
pub(super) fn storage_menu_enabled(h: &mut Harness<'_, XenAdminApp>, item: &str) -> bool {
    let s = sr_strings();
    settle(h);
    h.get_all_by_label(s.menu_storage).next().unwrap().click();
    step_until(h, "storage menu", |h| h.query_by_label(item).is_some());
    let enabled = menu_item_enabled(h, item);
    h.key_press(egui::Key::Escape);
    settle(h);
    enabled
}

/// Shows a page of the Properties dialog (a window just opened may take
/// the first click to come to the front).
fn properties_tab(h: &mut Harness<'_, XenAdminApp>, label: &str, page: PropertiesTab) {
    for _ in 0..3 {
        click_last(h, label);
        settle(h);
        if h.state_mut().storage.properties().map(|d| d.tab) == Some(page) {
            return;
        }
    }
    panic!("the {label} page did not open");
}

/// Whether the open menu's `item` is enabled.
fn menu_item_enabled(h: &Harness<'_, XenAdminApp>, item: &str) -> bool {
    h.query_all_by_label(item)
        .last()
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

/// Opens the New SR wizard from the toolbar on the type page.
pub(super) fn open_wizard(h: &mut Harness<'_, XenAdminApp>) {
    let s = sr_strings();
    click_first(h, s.new_storage_toolbar);
    step_until(h, "wizard", |h| h.query_by_label(s.type_title).is_some());
}

/// Type page → `kind` → Name page (name `name`) → Location page.
fn to_location(h: &mut Harness<'_, XenAdminApp>, kind_label: &str, name: &str) {
    let s = sr_strings();
    click_last(h, kind_label);
    settle(h);
    press(h, s.next);
    h.get_by_label(s.name_title);
    wizard(h).name = name.into();
    settle(h);
    press(h, s.next);
}

#[test]
fn the_wizard_creates_nfs_smb_iso_and_local_srs() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let s = sr_strings();
    let c = conn_id(&h);
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));

    // NFS: the type page offers the six types, iSCSI preselected; the
    // name page fills the type's default name.
    open_wizard(&mut h);
    for label in [
        s.type_nfs,
        s.type_iscsi,
        s.type_smb,
        s.type_local,
        s.type_nfs_iso,
        s.type_smb_iso,
    ] {
        assert!(shows(&h, label), "{label}");
    }
    assert_eq!(wizard(&mut h).kind, SrKind::Iscsi);
    assert!(shows(&h, s.blurb_iscsi));
    click_last(&mut h, s.type_nfs);
    settle(&mut h);
    assert!(shows(&h, s.blurb_nfs));
    press(&mut h, s.next);
    assert_eq!(wizard(&mut h).name, s.default_nfs);
    wizard(&mut h).name = "ui-nfs".into();
    settle(&mut h);
    press(&mut h, s.next);
    h.get_by_label(s.path_nfs);
    assert!(!last_enabled(&h, s.finish), "no share yet");
    assert!(!last_enabled(&h, s.scan));
    // A server that does not resolve: the scan's error on the page.
    type_into(&mut h, s.share_name, "nas.lab:/exports/x");
    press(&mut h, s.scan);
    step_until(&mut h, "scan error", |h| {
        shows(
            h,
            "DNS lookup failed: the given servername could not be resolved.",
        )
    });
    // The right server: nothing there yet; create a new SR, NFSv4.1.
    wizard(&mut h).share = "nfs.lab:/exports/ui".into();
    settle(&mut h);
    press(&mut h, s.scan);
    step_until(&mut h, "scanned", |h| {
        h.query_by_label(s.no_srs_found).is_some()
    });
    click_last(&mut h, s.nfs_v41);
    assert!(last_enabled(&h, s.finish));
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating NFS SR 'ui-nfs' on 'Lab Pool'");
    assert!(h.state().storage.wizard.is_none());
    let sr = sr_named(&server, "ui-nfs").unwrap();
    assert_eq!(
        field(&server, "sr", &sr, "name_description"),
        json!("NFS SR [nfs.lab:/exports/ui]")
    );
    let pbd = field(&server, "sr", &sr, "PBDs")[0]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        field(&server, "pbd", &pbd, "device_config")["nfsversion"],
        json!("4.1")
    );
    step_until(&mut h, "in the tree", |h| {
        h.query_by_label("ui-nfs").is_some()
    });

    // SMB with credentials and its own description.
    open_wizard(&mut h);
    click_last(&mut h, s.type_smb);
    settle(&mut h);
    press(&mut h, s.next);
    wizard(&mut h).name = "ui-smb".into();
    wizard(&mut h).auto_description = false;
    wizard(&mut h).description = "typed by hand".into();
    settle(&mut h);
    press(&mut h, s.next);
    h.get_by_label(s.path_smb);
    type_into(&mut h, s.share_name, r"\\smb.lab\ui");
    type_into(&mut h, s.username, SMB_USER);
    h.get_by_role_and_label(Role::PasswordInput, s.password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, s.password)
        .type_text(SMB_PASSWORD);
    settle(&mut h);
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating SMB SR 'ui-smb' on 'Lab Pool'");
    let sr = sr_named(&server, "ui-smb").unwrap();
    assert_eq!(
        field(&server, "sr", &sr, "name_description"),
        json!("typed by hand")
    );
    assert!(server.refs("secret").is_empty());

    // NFS ISO: the library already attached is refused; another share is
    // made.
    open_wizard(&mut h);
    to_location(&mut h, s.type_nfs_iso, "ui-isos");
    h.get_by_label(s.path_nfs_iso);
    type_into(&mut h, s.share_name, "nfs.lab:/exports/iso");
    h.get_by_label(&fill(s.nfs_iso_attached, &[("pool", "Lab Pool")]));
    assert!(!last_enabled(&h, s.finish));
    wizard(&mut h).iso_share = "nfs.lab:/exports/iso2".into();
    settle(&mut h);
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating ISO SR 'ui-isos' on 'Lab Pool'");
    let sr = sr_named(&server, "ui-isos").unwrap();
    assert_eq!(field(&server, "sr", &sr, "content_type"), json!("iso"));

    // SMB ISO with another user: the user name is required.
    open_wizard(&mut h);
    to_location(&mut h, s.type_smb_iso, "ui-smb-isos");
    type_into(&mut h, s.share_name, r"\\smb.lab\private");
    click_last(&mut h, s.use_different_user);
    assert!(!last_enabled(&h, s.finish), "a user name is required");
    wizard(&mut h).smb_iso_user = SMB_USER.into();
    wizard(&mut h).smb_iso_password = SMB_PASSWORD.into();
    settle(&mut h);
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating ISO SR 'ui-smb-isos' on 'Lab Pool'");
    assert_eq!(
        field(
            &server,
            "sr",
            &sr_named(&server, "ui-smb-isos").unwrap(),
            "sm_config"
        )["iso_type"],
        json!("cifs")
    );

    // Local storage, LVM.
    open_wizard(&mut h);
    to_location(&mut h, s.type_local, "ui-local");
    h.get_by_label(s.path_local);
    type_into(&mut h, s.device_path, "/dev/sdc");
    click_last(&mut h, s.fs_lvm);
    assert_eq!(wizard(&mut h).kind, SrKind::Local(LocalFs::Lvm));
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating LVM SR 'ui-local' on 'Lab Pool'");
    let sr = sr_named(&server, "ui-local").unwrap();
    assert_eq!(field(&server, "sr", &sr, "shared"), json!(false));

    // A failed create shows in the error list; nothing is left.
    open_wizard(&mut h);
    to_location(&mut h, s.type_local, "ui-bad");
    type_into(&mut h, s.device_path, "/dev/nope");
    press(&mut h, s.finish);
    let err = wait_op(&mut h, "Creating Ext3 SR 'ui-bad' on 'Lab Pool'").unwrap_err();
    assert_eq!(err, "The attempt to create a logical volume group failed");
    assert!(sr_named(&server, "ui-bad").is_none());
    step_until(&mut h, "error shown", |h| !h.state().errors.is_empty());
}

#[test]
fn the_wizard_scans_iscsi_targets_and_chooses_luns() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let s = sr_strings();
    let c = conn_id(&h);
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    open_wizard(&mut h);
    // iSCSI is preselected.
    press(&mut h, s.next);
    wizard(&mut h).name = "ui-iscsi".into();
    settle(&mut h);
    press(&mut h, s.next);
    h.get_by_label(s.path_iscsi);

    // CHAP: refused without it, at the CHAP fields.
    type_into(&mut h, s.target_host, "chap.lab");
    press(&mut h, s.scan_target);
    step_until(&mut h, "CHAP error", |h| {
        h.query_by_label(s.iscsi_login_failed).is_some()
    });
    click_last(&mut h, s.use_chap);
    type_into(&mut h, s.chap_user, CHAP_USER);
    h.get_by_role_and_label(Role::PasswordInput, s.chap_password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, s.chap_password)
        .type_text(CHAP_PASSWORD);
    settle(&mut h);
    assert!(
        !shows(&h, s.iscsi_login_failed),
        "a field change clears the error"
    );
    press(&mut h, s.scan_target);
    // One IQN: chosen, its one LUN listed and chosen.
    step_until(&mut h, "LUN", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.selected_lun().is_some())
    });
    assert_eq!(
        wizard(&mut h)
            .selected_lun()
            .map(xen_core::storage::lun_label)
            .as_deref(),
        Some("LUN 0: lun-c0: 20 GB (LIO-ORG)")
    );
    press(&mut h, s.finish);
    // The LUN holds no SR: format it.
    step_until(&mut h, "LUN choice", |h| {
        h.query_by_label(s.lvm_none).is_some()
    });
    assert!(!last_enabled(&h, s.lvm_reattach));
    press(&mut h, s.lvm_format);
    wait_ok(
        &mut h,
        "Creating LVM over iSCSI SR 'ui-iscsi' on 'Lab Pool'",
    );
    let sr = sr_named(&server, "ui-iscsi").unwrap();
    let pbd = field(&server, "sr", &sr, "PBDs")[0]
        .as_str()
        .unwrap()
        .to_owned();
    let dc = field(&server, "pbd", &pbd, "device_config");
    assert!(dc.get("chappassword_secret").is_some());
    assert_eq!(
        field(&server, "sr", &sr, "name_description"),
        json!(
            "iSCSI SR [192.0.2.61 (iqn.2026-11.lab.mock:secure; LUN 0: lun-c0: 20 GB (LIO-ORG))]"
        )
    );

    // Two IQNs: one is chosen from the list; LUN 0 holds an SR: reattach.
    open_wizard(&mut h);
    press(&mut h, s.next);
    wizard(&mut h).name = "ui-old-iscsi".into();
    settle(&mut h);
    press(&mut h, s.next);
    type_into(&mut h, s.target_host, "iscsi.lab");
    press(&mut h, s.scan_target);
    step_until(&mut h, "IQNs", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.iqns.is_some())
    });
    assert!(wizard(&mut h).iqn.is_none(), "two IQNs: none chosen");
    h.get_all_by_role(Role::ComboBox).next().unwrap().click();
    settle(&mut h);
    click_last(&mut h, "iqn.2026-11.lab.mock:storage (192.0.2.60:3260)");
    step_until(&mut h, "LUNs", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.luns.is_some())
    });
    h.get_all_by_role(Role::ComboBox).nth(1).unwrap().click();
    settle(&mut h);
    click_last(&mut h, "LUN 0: lun-a0: 100 GB (LIO-ORG)");
    settle(&mut h);
    press(&mut h, s.finish);
    step_until(&mut h, "LUN choice", |h| {
        h.query_by_label(&fill(s.lvm_found, &[("type", s.type_iscsi)]))
            .is_some()
    });
    assert!(h.state().storage.wizard.as_ref().unwrap().has_question());
    press(&mut h, s.lvm_reattach);
    wait_ok(&mut h, "Attaching SR 'ui-old-iscsi' to 'Lab Pool'");
    assert_eq!(server.call_count("Async.SR.introduce"), 1);

    // That LUN is now in use by this pool: the page says so.
    open_wizard(&mut h);
    press(&mut h, s.next);
    press(&mut h, s.next);
    type_into(&mut h, s.target_host, "iscsi.lab");
    press(&mut h, s.scan_target);
    step_until(&mut h, "IQNs", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.iqns.is_some())
    });
    wizard(&mut h).iqn = Some(1);
    step_until(&mut h, "LUNs", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.luns.is_some())
    });
    h.get_all_by_role(Role::ComboBox).nth(1).unwrap().click();
    settle(&mut h);
    click_last(&mut h, "LUN 0: lun-a0: 100 GB (LIO-ORG)");
    settle(&mut h);
    h.get_by_label(&fill(
        s.lun_in_use,
        &[("sr", "ui-old-iscsi"), ("pool", "Lab Pool")],
    ));
    assert!(!last_enabled(&h, s.finish));
    press(&mut h, s.cancel);
    assert!(h.state().storage.wizard.is_none());
    // Unreachable target: at the host field.
    open_wizard(&mut h);
    press(&mut h, s.next);
    press(&mut h, s.next);
    type_into(&mut h, s.target_host, "unreachable.lab");
    press(&mut h, s.scan_target);
    step_until(&mut h, "unreachable", |h| {
        h.query_by_label(s.iscsi_unreachable).is_some()
    });
}

#[test]
fn the_storage_menu_detaches_forgets_reattaches_and_destroys() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let s = sr_strings();
    // An SR made through the wizard to play with.
    let c = conn_id(&h);
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    open_wizard(&mut h);
    to_location(&mut h, s.type_nfs, "scratch");
    type_into(&mut h, s.share_name, "nfs.lab:/exports/scratch");
    press(&mut h, s.finish);
    wait_ok(&mut h, "Creating NFS SR 'scratch' on 'Lab Pool'");
    let sr = sr_named(&server, "scratch").unwrap();
    select_sr(&mut h, &sr);

    // Attached: Set as Default, Detach; not Forget, Destroy, Reattach.
    assert!(storage_menu_enabled(&mut h, s.set_default));
    assert!(storage_menu_enabled(&mut h, s.detach));
    assert!(!storage_menu_enabled(&mut h, s.forget));
    assert!(!storage_menu_enabled(&mut h, s.destroy));
    assert!(!storage_menu_enabled(&mut h, s.reattach));
    assert!(!storage_menu_enabled(&mut h, s.repair));
    storage_menu(&mut h, s.set_default);
    wait_ok(&mut h, "Setting 'scratch' as default SR on 'Lab Pool'");
    assert_eq!(field(&server, "pool", POOL, "default_SR"), json!(sr));

    // Detach asks first; No changes nothing.
    storage_menu(&mut h, s.detach);
    h.get_by_label(s.detach_title);
    press(&mut h, "No");
    assert_eq!(server.call_count("Async.PBD.unplug"), 0);
    storage_menu(&mut h, s.detach);
    press(&mut h, s.yes);
    wait_ok(&mut h, "Detaching SR 'scratch' from 'Lab Pool'");
    assert!(storage_menu_enabled(&mut h, s.forget));
    assert!(storage_menu_enabled(&mut h, s.destroy));
    assert!(!storage_menu_enabled(&mut h, s.detach));

    // Destroy, confirmed with its own button.
    storage_menu(&mut h, s.destroy);
    h.get_by_label(&fill(s.destroy_text, &[("name", "scratch")]));
    press(&mut h, s.destroy_yes);
    wait_ok(&mut h, "Destroying SR 'scratch' on 'Lab Pool'...");
    assert!(server.record("sr", &sr).is_none());

    // An SR with no PBDs: Reattach opens the Attach wizard on its location
    // page; the scan finds and selects it; the multi-pool warning asks.
    let orphan = "OpaqueRef:sr-orphan";
    server.add_object(
        "SR",
        orphan,
        json!({
            "uuid": OLD_NFS_SR, "name_label": "Orphan NFS", "name_description": "",
            "type": "nfs", "content_type": "", "shared": true, "PBDs": [], "VDIs": [],
            "physical_size": 0, "physical_utilisation": 0, "virtual_allocation": 0,
            "other_config": {}, "sm_config": {}, "tags": [], "introduced_by": "OpaqueRef:NULL",
            "is_tools_sr": false, "allowed_operations": ["forget", "pbd_create"],
            "current_operations": {}
        }),
    );
    step_until(&mut h, "orphan", |h| {
        h.query_by_label("Orphan NFS").is_some()
    });
    select_sr(&mut h, orphan);
    storage_menu(&mut h, s.reattach);
    step_until(&mut h, "attach wizard", |h| {
        h.query_by_label(&fill(s.wizard_title_attach, &[("pool", "Lab Pool")]))
            .is_some()
    });
    h.get_by_label(s.path_nfs);
    assert!(!last_enabled(&h, s.create_new), "a reattach cannot create");
    type_into(&mut h, s.share_name, "nfs.lab:/exports/old");
    press(&mut h, s.scan);
    step_until(&mut h, "found", |h| {
        h.state()
            .storage
            .wizard
            .as_ref()
            .is_some_and(|w| w.found.is_some())
    });
    // Listed (the SR's General tab behind shows its UUID too).
    assert!(h.query_all_by_label(OLD_NFS_SR).count() >= 2);
    assert_eq!(wizard(&mut h).selected_uuid.as_deref(), Some(OLD_NFS_SR));
    press(&mut h, s.finish);
    step_until(&mut h, "warning", |h| {
        h.query_by_label(&fill(
            s.multi_pool_warning,
            &[("console", branding::PRODUCT_NAME), ("sr", "Orphan NFS")],
        ))
        .is_some()
    });
    press(&mut h, s.yes);
    wait_ok(&mut h, "Attaching SR 'Orphan NFS' to 'Lab Pool'");
    assert_eq!(
        field(&server, "sr", orphan, "PBDs")
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Forget from the context menu, after a detach.
    right_click(&mut h, "Orphan NFS");
    click_last(&mut h, s.detach);
    press(&mut h, s.yes);
    wait_ok(&mut h, "Detaching SR 'Orphan NFS' from 'Lab Pool'");
    right_click(&mut h, "Orphan NFS");
    click_last(&mut h, s.forget);
    settle(&mut h);
    h.get_by_label(&fill(s.forget_text, &[("name", "Orphan NFS")]));
    press(&mut h, s.forget_yes);
    wait_ok(&mut h, "Forgetting SR 'Orphan NFS' on 'Lab Pool'...");
    assert!(server.record("sr", orphan).is_none());
    step_until(&mut h, "gone from the tree", |h| {
        h.query_by_label("Orphan NFS").is_none()
    });
}

#[test]
fn repair_properties_rescan_and_trim() {
    let runtime = runtime();
    let (server, mut h) = connect_to_mock(&runtime);
    let s = sr_strings();

    // Repair: the ISO library is unplugged on host 2.
    select_sr(&mut h, ISO);
    storage_menu(&mut h, s.repair);
    h.get_by_label(&fill(s.repair_title, &[("name", "ISO library")]));
    assert!(shows(&h, s.pbd_unplugged));
    assert!(shows(&h, s.pbd_connected));
    press(&mut h, s.repair_button);
    wait_ok(&mut h, "Repairing SR ISO library");
    step_until(&mut h, "all connected", |h| !shows(h, s.pbd_unplugged));
    press(&mut h, s.close);
    assert!(!storage_menu_enabled(&mut h, s.repair), "nothing to repair");

    // Rescan on the SR Storage tab finds a new ISO.
    server.add_iso_file("nfs.lab:/exports/iso", "alpine-3.22.iso");
    tab(&mut h, tab_strings().tab_storage);
    press(&mut h, s.rescan);
    wait_ok(&mut h, "Scanning SR 'ISO library'");
    step_until(&mut h, "new ISO", |h| {
        h.query_by_label("alpine-3.22.iso").is_some()
    });

    // Properties: name, tags, read caching off, an alert.
    select_sr(&mut h, NFS);
    click_last(&mut h, s.properties);
    step_until(&mut h, "properties", |h| {
        h.query_by_label(&fill(s.properties_title, &[("name", "NFS VM storage")]))
            .is_some()
    });
    assert!(shows(&h, s.tab_alerts) && shows(&h, s.tab_read_caching));
    {
        let d = h.state_mut().storage.properties().unwrap();
        d.name = "Shared NFS".into();
        d.tags = "gold, nfs".into();
    }
    properties_tab(&mut h, s.tab_read_caching, PropertiesTab::ReadCaching);
    click_last(&mut h, s.read_caching_enable);
    properties_tab(&mut h, s.tab_alerts, PropertiesTab::Alerts);
    {
        let d = h.state_mut().storage.properties().unwrap();
        let p = d.perfmon.as_mut().unwrap();
        let group = p
            .form
            .setting_mut(xen_core::perfmon::AlertGroup::Sr)
            .unwrap();
        group.enabled = true;
    }
    press(&mut h, s.ok);
    wait_ok(&mut h, "Saving properties of 'NFS VM storage'");
    let rec = server.record("sr", NFS).unwrap();
    assert_eq!(rec["name_label"], json!("Shared NFS"));
    assert_eq!(rec["tags"], json!(["gold", "nfs"]));
    assert_eq!(rec["other_config"]["o_direct"], json!("true"));
    step_until(&mut h, "alert saved", |_| {
        server
            .record("sr", NFS)
            .is_some_and(|r| r["other_config"].get("perfmon").is_some())
    });

    // Trim: LVM local storage asks, then runs; ext does not offer it.
    select_sr(&mut h, LOCAL2);
    assert!(!storage_menu_enabled(&mut h, s.trim));
    select_sr(&mut h, LOCAL1);
    storage_menu(&mut h, s.trim);
    h.get_by_label(s.trim_text);
    press(&mut h, s.yes);
    wait_ok(&mut h, "Reclaiming freed space on SR 'Local storage'");
    assert!(
        server
            .plugin_calls()
            .contains(&("trim".into(), "do_trim".into()))
    );
}

/// A pool operator may detach but not reclaim freed space: the role
/// elevation dialog asks for other credentials.
#[test]
fn storage_operations_go_through_role_elevation() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let mut h = app_harness(&runtime);
    h.step();
    let st = strings();
    h.get_by_label(st.connect_server)
        .type_text(&server.address());
    h.step();
    h.get_by_label(st.connect_user).focus();
    h.step();
    for _ in 0..4 {
        h.key_press(egui::Key::Backspace);
    }
    h.get_by_label(st.connect_user)
        .type_text("pool-operator-user");
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, st.connect_password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, st.connect_password)
        .type_text("pool-operator-password");
    h.step();
    h.get_by_label(st.connect_button).click();
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    let s = sr_strings();
    select_sr(&mut h, LOCAL1);
    storage_menu(&mut h, s.trim);
    press(&mut h, s.yes);
    step_until(&mut h, "elevation", |h| h.state().ops.elevation.is_some());
    assert!(server.plugin_calls().is_empty());
}
