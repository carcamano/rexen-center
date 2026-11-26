//! F12a in the window, against two mock pools: Move VM... through the
//! wizard to another pool (destination list with its checks, a disk per
//! SR, the interface's network, the migration network, the summary), a
//! refused destination shown with the server's reason, Copy VM within the
//! pool and to another pool, and vApps (Manage vApps, New vApp, Properties,
//! Start, Shut Down, Delete, Assign to vApp).

use std::sync::Arc;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Session, TrustStore};
use xapi_mock::MockServer;
use xen_core::migrate::WizardMode;

use super::provisioning::{catch_up, press, shows};
use super::xva::{open_menu, succeeded, wait_end};
use super::*;
use crate::cpm_strings::cpm_strings;
use crate::migrate_ui::Page;
use crate::vapp_strings::vapp_strings;

fn conn_to(h: &Harness<'_, XenAdminApp>, server: &MockServer) -> ConnectionId {
    h.state()
        .backend
        .state()
        .connections
        .iter()
        .find(|c| c.server == server.address() && c.status.is_connected())
        .map(|c| c.id)
        .expect("connected")
}

/// Presses `label` once the window settled (a dialog that changed size
/// re-centres over a few frames).
fn press_settled(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    for _ in 0..4 {
        settle(h);
    }
    press(h, label);
}

/// Two standalone lab servers (two pools), both connected in the window.
fn lab(
    runtime: &tokio::runtime::Runtime,
    ta: &str,
    tb: &str,
) -> (MockServer, MockServer, Harness<'static, XenAdminApp>) {
    let a = runtime.block_on(MockServer::start_lab(false, ta, 1));
    let b = runtime.block_on(MockServer::start_lab(false, tb, 2));
    a.set_task_timing(2, Duration::from_millis(20));
    b.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &a.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "a connected", |h| {
        shows(h, &format!("lab-host-{ta}"))
    });
    h.state().backend.send(Command::Connect {
        server: b.address(),
        username: xapi_mock::USERNAME.into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    step_until(&mut h, "b connected", |h| {
        shows(h, &format!("lab-host-{tb}"))
    });
    settle(&mut h);
    (a, b, h)
}

async fn session(server: &MockServer) -> Session {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = Arc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    Session::login_with_password(client, xapi_mock::USERNAME, xapi_mock::PASSWORD, "f12a-ui")
        .await
        .unwrap()
}

/// A halted VM on lab server `tag`: two disks on its local SR, one
/// interface on eth0; (vm, disks).
fn lab_vm(
    runtime: &tokio::runtime::Runtime,
    server: &MockServer,
    tag: &str,
    name: &str,
    mac: &str,
) -> (String, Vec<String>) {
    runtime.block_on(async {
        let s = session(server).await;
        let vm: String = s
            .call(
                "VM.create",
                vec![json!({
                    "name_label": name, "memory_static_max": 1073741824_i64,
                    "memory_dynamic_max": 1073741824_i64, "memory_dynamic_min": 1073741824_i64,
                    "memory_static_min": 536870912_i64, "VCPUs_max": 1, "VCPUs_at_startup": 1,
                })],
            )
            .await
            .unwrap();
        let mut vdis = Vec::new();
        for i in 0..2 {
            let vdi: String = s
                .call(
                    "VDI.create",
                    vec![json!({"name_label": format!("{name} disk {i}"),
                        "SR": format!("OpaqueRef:sr-local-01-{tag}"),
                        "virtual_size": 104857600_i64, "type": "user", "sharable": false,
                        "read_only": false})],
                )
                .await
                .unwrap();
            let _: String = s
                .call(
                    "VBD.create",
                    vec![json!({"VM": vm, "VDI": vdi, "userdevice": i.to_string(),
                        "bootable": i == 0, "mode": "RW", "type": "Disk", "empty": false,
                        "other_config": {"owner": "true"}})],
                )
                .await
                .unwrap();
            vdis.push(vdi);
        }
        let _: String = s
            .call(
                "VIF.create",
                vec![
                    json!({"VM": vm, "network": format!("OpaqueRef:net-eth0-{tag}"),
                    "device": "0", "MAC": mac, "MTU": 1500}),
                ],
            )
            .await
            .unwrap();
        (vm, vdis)
    })
}

/// A shared NFS SR attached to lab server `tag`.
fn add_shared_sr(server: &MockServer, tag: &str) -> String {
    let sr = format!("OpaqueRef:sr-shared-{tag}");
    let pbd = format!("OpaqueRef:pbd-shared-{tag}");
    let host = format!("OpaqueRef:host-0001-{tag}");
    server.add_object(
        "pbd",
        &pbd,
        json!({"uuid": "5bd00000-0000-4000-8000-00000000ff01", "host": host, "SR": sr,
            "device_config": {}, "currently_attached": true, "other_config": {}}),
    );
    server.add_object(
        "sr",
        &sr,
        json!({"uuid": "5bd00000-0000-4000-8000-00000000ff02", "name_label": "Shared NFS",
            "name_description": "", "type": "nfs", "content_type": "", "shared": true,
            "PBDs": [pbd], "VDIs": [], "physical_size": 1099511627776_i64,
            "physical_utilisation": 0, "virtual_allocation": 0,
            "allowed_operations": ["vdi_create"], "current_operations": {},
            "other_config": {}, "sm_config": {}, "tags": []}),
    );
    sr
}

fn wizard<'a>(h: &'a mut Harness<'_, XenAdminApp>) -> &'a mut crate::migrate_ui::MigrateWizard {
    h.state_mut().migrate.wizard.as_mut().expect("the wizard")
}

fn page(h: &Harness<'_, XenAdminApp>) -> Option<Page> {
    h.state().migrate.wizard.as_ref().map(|w| w.page)
}

fn vm_on(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("vm")
        .into_iter()
        .find(|v| server.field("vm", v, "name_label") == Some(json!(name)))
}

#[test]
fn move_vm_to_another_pool_through_the_wizard() {
    let runtime = runtime();
    let (ta, tb) = ("f12a-wa", "f12a-wb");
    let (a, b, mut h) = lab(&runtime, ta, tb);
    let shared = add_shared_sr(&b, tb);
    let (vm, vdis) = lab_vm(&runtime, &a, ta, "mover", "02:00:00:ee:00:01");
    step_until(&mut h, "the VM is shown", |h| shows(h, "mover"));
    let (ca, cb) = (conn_to(&h, &a), conn_to(&h, &b));
    let c = cpm_strings();
    let n = crate::net_strings::net_strings();
    h.state_mut().selection = Some(Selection::Vm(ca, Ref::new(&vm)));
    settle(&mut h);
    // VM menu: Move VM... of a halted VM opens the wizard in Move mode.
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, c.menu_move_vm);
    assert_eq!(page(&h), Some(Page::Destination));
    assert_eq!(wizard(&mut h).mode, WizardMode::Move);
    assert!(shows(&h, c.destination_intro_move));
    // The destination list: a (its current server) is refused, b checked
    // by the servers then offered.
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    settle(&mut h);
    step_until(&mut h, "b checked", |h| {
        shows(h, "lab-host-f12a-wb")
            && shows(
                h,
                &crate::i18n::fill(
                    c.with_reason,
                    &[("target", "lab-host-f12a-wa"), ("reason", c.current_server)],
                ),
            )
    });
    press(&mut h, "lab-host-f12a-wb");
    step_until(&mut h, "the server chosen", |h| {
        h.state()
            .migrate
            .wizard
            .as_ref()
            .is_some_and(|w| w.destination == Some(cb) && !w.hosts.is_empty())
    });
    assert!(shows(&h, "Move VM to lab-host-f12a-wb"));
    assert!(b.calls().iter().any(|c| c == "host.migrate_receive"));
    assert!(a.calls().iter().any(|c| c == "VM.assert_can_migrate"));
    press(&mut h, n.next);
    // Storage: one disk per SR.
    assert_eq!(page(&h), Some(Page::Storage));
    assert!(shows(&h, c.storage_title));
    {
        let w = wizard(&mut h);
        w.all_same = false;
        w.disk_srs.insert(
            Ref::new(&vdis[0]),
            Some(Ref::new(format!("OpaqueRef:sr-local-01-{tb}"))),
        );
        w.disk_srs
            .insert(Ref::new(&vdis[1]), Some(Ref::new(&shared)));
    }
    settle(&mut h);
    press(&mut h, n.next);
    // Networking: another pool, so each interface is mapped.
    assert_eq!(page(&h), Some(Page::Network));
    assert!(shows(&h, c.network_intro_single));
    {
        let w = wizard(&mut h);
        for v in w.networks.values_mut() {
            *v = Some(Ref::new(format!("OpaqueRef:net-eth1-{tb}")));
        }
    }
    settle(&mut h);
    press(&mut h, n.next);
    assert_eq!(page(&h), Some(Page::Transfer));
    assert!(shows(&h, c.transfer_label));
    press(&mut h, n.next);
    assert_eq!(page(&h), Some(Page::Finish));
    for text in [
        c.summary_vm,
        c.summary_destination,
        c.summary_transfer,
        c.summary_storage,
        c.summary_network,
        "mover disk 1 -> Shared NFS",
        "mover disk 0 -> Local storage",
    ] {
        assert!(shows(&h, text), "{text}");
    }
    {
        let state = h.state().backend.state();
        let (src, dst) = (state.connection(ca).unwrap(), state.connection(cb).unwrap());
        let from = &src.model.networks[&Ref::new(format!("OpaqueRef:net-eth0-{ta}"))];
        let to = &dst.model.networks[&Ref::new(format!("OpaqueRef:net-eth1-{tb}"))];
        let row = format!(
            "{} -> {}",
            xen_core::objects::network_name(&src.model, from),
            xen_core::objects::network_name(&dst.model, to)
        );
        assert!(shows(&h, &row), "{row}");
    }
    press(&mut h, n.finish);
    assert!(h.state().migrate.wizard.is_none());
    let status = wait_end(&mut h, "Migrating VM 'mover' to 'lab-host-f12a-wb'");
    assert!(succeeded(&status), "{status:?}");
    catch_up(&mut h);
    let moved = vm_on(&b, "mover").expect("the VM is on b");
    assert!(vm_on(&a, "mover").is_none());
    let vifs = b.field("vm", &moved, "VIFs").unwrap();
    let vif = vifs[0].as_str().unwrap();
    assert_eq!(
        b.field("vif", vif, "network"),
        Some(json!(format!("OpaqueRef:net-eth1-{tb}")))
    );
}

#[test]
fn a_refused_destination_shows_the_servers_reason() {
    let runtime = runtime();
    let (ta, tb) = ("f12a-ra", "f12a-rb");
    let (a, b, mut h) = lab(&runtime, ta, tb);
    let (vm, _) = lab_vm(&runtime, &a, ta, "pinned", "02:00:00:ee:00:02");
    step_until(&mut h, "the VM is shown", |h| shows(h, "pinned"));
    b.refuse_migration_to(
        &format!("OpaqueRef:host-0001-{tb}"),
        Some(("VM_HAS_PCI_ATTACHED", &[&vm])),
    );
    let ca = conn_to(&h, &a);
    let c = cpm_strings();
    let n = crate::net_strings::net_strings();
    h.state_mut().ops.dialog_requests.push((
        ca,
        crate::provision_ui::DialogRequest::Migrate {
            vms: vec![Ref::new(&vm)],
            mode: WizardMode::Migrate,
        },
    ));
    settle(&mut h);
    assert_eq!(page(&h), Some(Page::Destination));
    assert!(shows(&h, "Migrate VM"));
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    settle(&mut h);
    let reason = crate::i18n::describe_xapi_error_in(
        None,
        &xapi::Error::Api {
            code: "VM_HAS_PCI_ATTACHED".into(),
            params: vec![vm.clone()],
        },
    );
    let reason = reason.lines().next().unwrap().to_owned();
    let refused = crate::i18n::fill(
        c.with_reason,
        &[("target", "lab-host-f12a-rb"), ("reason", &reason)],
    );
    step_until(&mut h, "b refused", |h| shows(h, &refused));
    assert!(
        h.get_all_by_label(&refused)
            .all(|n| n.accesskit_node().is_disabled())
    );
    // Nothing chosen: Next stays off.
    assert!(!super::provisioning::last_enabled(&h, n.next));
    press(&mut h, n.cancel);
    assert!(h.state().migrate.wizard.is_none());
    assert!(vm_on(&b, "pinned").is_none());
}

#[test]
fn copy_vm_within_the_pool_and_to_another_pool() {
    let runtime = runtime();
    let (ta, tb) = ("f12a-ca", "f12a-cb");
    let (a, b, mut h) = lab(&runtime, ta, tb);
    let (vm, _) = lab_vm(&runtime, &a, ta, "orig", "02:00:00:ee:00:03");
    step_until(&mut h, "the VM is shown", |h| shows(h, "orig"));
    let ca = conn_to(&h, &a);
    let cb = conn_to(&h, &b);
    let c = cpm_strings();
    let n = crate::net_strings::net_strings();
    let p = crate::prov_strings::prov_strings();
    h.state_mut().selection = Some(Selection::Vm(ca, Ref::new(&vm)));
    settle(&mut h);
    // Within the pool: the F7 clone, from the wizard's Name and Storage page.
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, p.copy_vm);
    assert_eq!(page(&h), Some(Page::CopyMode));
    assert!(shows(&h, c.copy_rubric_vm) && shows(&h, c.within_pool) && shows(&h, c.cross_pool));
    press(&mut h, n.next);
    assert_eq!(page(&h), Some(Page::IntraCopy));
    assert_eq!(wizard(&mut h).copy.name, "Copy of orig");
    assert!(shows(&h, c.intra_rubric_vm));
    press(&mut h, n.finish);
    step_until(&mut h, "the clone", |h| {
        let _ = h;
        vm_on(&a, "Copy of orig").is_some()
    });
    catch_up(&mut h);
    // To another pool: the source pool is not offered.
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, p.copy_vm);
    press(&mut h, c.cross_pool);
    press(&mut h, n.next);
    assert_eq!(page(&h), Some(Page::Destination));
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    settle(&mut h);
    step_until(&mut h, "b offered", |h| shows(h, "lab-host-f12a-cb"));
    assert!(!shows(&h, "lab-host-f12a-ca (Current server)"));
    press(&mut h, "lab-host-f12a-cb");
    step_until(&mut h, "b chosen", |h| {
        h.state()
            .migrate
            .wizard
            .as_ref()
            .is_some_and(|w| w.destination == Some(cb) && !w.hosts.is_empty())
    });
    assert!(shows(&h, "Copy VM to lab-host-f12a-cb"));
    for expected in [Page::Storage, Page::Network, Page::Transfer, Page::Finish] {
        press(&mut h, n.next);
        assert_eq!(page(&h), Some(expected));
    }
    assert!(shows(&h, c.finish_intro_copy_single));
    press(&mut h, n.finish);
    let status = wait_end(
        &mut h,
        "Copying VM 'orig' from 'lab-host-f12a-ca' to 'lab-host-f12a-cb'",
    );
    assert!(succeeded(&status), "{status:?}");
    assert!(vm_on(&b, "orig").is_some());
    assert!(vm_on(&a, "orig").is_some(), "the original stays");
}

#[test]
fn vapps_in_the_window() {
    let runtime = runtime();
    let (ta, tb) = ("f12a-va", "f12a-vb");
    let (a, _b, mut h) = lab(&runtime, ta, tb);
    a.set_appliance_delay_unit(Duration::from_millis(5));
    let (db, _) = lab_vm(&runtime, &a, ta, "db", "02:00:00:ee:00:04");
    let (web, _) = lab_vm(&runtime, &a, ta, "web", "02:00:00:ee:00:05");
    step_until(&mut h, "the VMs are shown", |h| {
        shows(h, "db") && shows(h, "web")
    });
    let ca = conn_to(&h, &a);
    let s = vapp_strings();
    let n = crate::net_strings::net_strings();
    h.state_mut().selection = Some(Selection::Host(
        ca,
        Ref::new(format!("OpaqueRef:host-0001-{ta}")),
    ));
    settle(&mut h);
    // Pool menu: Manage vApps...
    open_menu(&mut h, crate::pool_strings::pool_strings().menu_pool);
    press(&mut h, s.menu_manage);
    assert!(h.state().vapp.manage.is_some());
    assert!(shows(&h, s.no_vapps));
    press(&mut h, s.button_new);
    assert!(h.state().vapp.wizard.is_some());
    // Name.
    {
        let w = h.state_mut().vapp.wizard.as_mut().unwrap();
        w.name = "shop".into();
        w.description = "web shop".into();
    }
    settle(&mut h);
    press(&mut h, n.next);
    // Virtual Machines: both ticked.
    assert!(shows(&h, s.vms_title) && shows(&h, s.col_current));
    press(&mut h, "db");
    press(&mut h, "web");
    assert!(shows(
        &h,
        &crate::i18n::fill(s.vms_selected, &[("count", "2")])
    ));
    press(&mut h, n.next);
    // Startup sequence: web starts first, db after it.
    assert!(shows(&h, s.order_title));
    {
        let w = h.state_mut().vapp.wizard.as_mut().unwrap();
        for row in &mut w.order.rows {
            if row.vm.as_str() == web {
                row.order = 0;
                row.start_delay = 3;
            } else {
                row.order = 1;
            }
        }
    }
    settle(&mut h);
    press(&mut h, n.next);
    assert!(shows(&h, s.finish_title));
    press(&mut h, n.finish);
    let status = wait_end(&mut h, "Creating vApp 'shop'...");
    assert!(succeeded(&status), "{status:?}");
    catch_up(&mut h);
    let app = a.refs("vm_appliance").pop().expect("the vApp");
    assert_eq!(a.field("vm", &db, "appliance"), Some(json!(app)));
    assert_eq!(a.field("vm", &db, "order"), Some(json!(1)));
    assert_eq!(a.field("vm", &web, "start_delay"), Some(json!(3)));
    // The dialog lists it; its VMs are shown once selected.
    assert!(shows(&h, "shop") && shows(&h, "web shop"));
    press_settled(&mut h, "shop");
    assert_eq!(
        h.state().vapp.manage.as_ref().unwrap().selected.len(),
        1,
        "selected"
    );
    assert!(super::provisioning::last_enabled(&h, s.button_start));
    // Start: web first, then db.
    press_settled(&mut h, s.button_start);
    let status = wait_end(&mut h, "Starting vApp 'shop'");
    assert!(succeeded(&status), "{status:?}");
    let started: Vec<String> = a
        .appliance_log()
        .into_iter()
        .filter(|(k, _)| k == "start")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(started, [web.clone(), db.clone()]);
    catch_up(&mut h);
    // The VM menu offers Shut Down vApp for its VMs, asked first.
    h.state_mut().selection = Some(Selection::Vm(ca, Ref::new(&db)));
    settle(&mut h);
    let conn = h.state().backend.state().connection(ca).cloned().unwrap();
    let record = conn.model.vms.get(&Ref::new(&db)).cloned().unwrap();
    let entries = crate::ops_ui::vm_entries(&conn, &Ref::new(&db), &record);
    assert!(entries.iter().any(|e| matches!(e,
        crate::ops_ui::Entry::Item { label, enabled: true, request: xen_core::ActionRequest::ShutDownVApp { .. } }
            if *label == s.menu_shut_down)));
    let entries = crate::provision_ui::vm_entries(&conn, &Ref::new(&db), &record);
    assert!(entries.iter().any(|e| matches!(e,
        crate::ops_ui::Entry::Submenu { label, .. } if label == s.menu_assign)));
    press_settled(&mut h, s.button_shut_down);
    assert!(shows(&h, s.confirm_shut_down_one));
    press_settled(&mut h, s.yes);
    let status = wait_end(&mut h, "Shutting down vApp 'shop'");
    assert!(succeeded(&status), "{status:?}");
    // Properties: a new description.
    press_settled(&mut h, s.button_properties);
    assert!(h.state().vapp.properties.is_some());
    h.state_mut().vapp.properties.as_mut().unwrap().description = "the shop".into();
    settle(&mut h);
    press_settled(&mut h, s.ok);
    let status = wait_end(&mut h, "Saving the properties of vApp 'shop'");
    assert!(succeeded(&status), "{status:?}");
    assert_eq!(
        a.field("vm_appliance", &app, "name_description"),
        Some(json!("the shop"))
    );
    catch_up(&mut h);
    // Delete: asked with the number of VMs.
    press_settled(&mut h, s.button_delete);
    assert!(shows(
        &h,
        &crate::i18n::fill(s.delete_one_n, &[("name", "shop"), ("count", "2")])
    ));
    press_settled(&mut h, s.yes);
    let status = wait_end(&mut h, "Deleting vApp 'shop'");
    assert!(succeeded(&status), "{status:?}");
    assert!(a.refs("vm_appliance").is_empty());
    assert_eq!(
        a.field("vm", &db, "appliance"),
        Some(json!("OpaqueRef:NULL"))
    );
}
