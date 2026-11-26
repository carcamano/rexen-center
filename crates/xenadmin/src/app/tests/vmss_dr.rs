//! F12b in the window, against the mock: VM Snapshot Schedules (New
//! Snapshot Schedule wizard, the schedule running on the mock's clock and
//! its run history, Disable, Properties, Delete, the VM menu and Snapshots
//! tab), and disaster recovery between two mock pools (Configure, then the
//! Disaster Recovery wizard's test failover through an iSCSI scan, its
//! pre-checks, recovery, clean-up and report, and a cancelled wizard that
//! cleans up after itself).

use std::sync::Arc;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Ref, Session, TrustStore, VmssFrequency};
use xapi_mock::MockServer;

use super::provisioning::{catch_up, press, select, shows};
use super::xva::{open_menu, pool_window, succeeded, wait_end};
use super::*;
use crate::dr_strings::dr_strings;
use crate::dr_ui::Page;
use crate::vmss_strings::vmss_strings;

const POOL: &str = "OpaqueRef:pool-0001";
const WEB: &str = "OpaqueRef:vm-web";

fn press_settled(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    for _ in 0..4 {
        settle(h);
    }
    press(h, label);
}

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

/// Opens the Pool menu, then `submenu` (if any) and `item`.
fn pool_menu(h: &mut Harness<'_, XenAdminApp>, submenu: Option<&str>, item: &str) {
    open_menu(h, crate::pool_strings::pool_strings().menu_pool);
    if let Some(sub) = submenu {
        press(h, sub);
    }
    press(h, item);
}

#[test]
fn snapshot_schedules_in_the_window() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let s = vmss_strings();
    let n = crate::net_strings::net_strings();
    let c = first_conn(&h).unwrap().id;
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    pool_menu(&mut h, None, s.menu_manage);
    assert!(h.state().vmss.manage.is_some());
    assert!(shows(&h, s.no_policies));
    press_settled(&mut h, s.button_new);
    assert!(h.state().vmss.wizard.is_some());
    // Name.
    h.state_mut().vmss.wizard.as_mut().unwrap().name = "web hourly".into();
    settle(&mut h);
    press_settled(&mut h, n.next);
    // Virtual Machines: web-01 ticked.
    assert!(shows(&h, s.vms_title) && shows(&h, s.col_current));
    press(&mut h, "web-01");
    assert_eq!(
        h.state().vmss.wizard.as_ref().unwrap().vms.selected.len(),
        1
    );
    press_settled(&mut h, n.next);
    // Snapshot Type: disk only.
    assert!(shows(&h, s.type_title) && shows(&h, s.disk_only));
    press_settled(&mut h, n.next);
    // Snapshot Schedule: hourly at 15, two kept; the next runs shown once
    // the server's time is known.
    assert!(shows(&h, s.schedule_title));
    step_until(&mut h, "the server's time", |h| shows(h, s.hourly));
    press(&mut h, s.hourly);
    {
        let w = h.state_mut().vmss.wizard.as_mut().unwrap();
        assert_eq!(w.schedule.frequency, VmssFrequency::Hourly);
        assert_eq!(w.schedule.retention, 10, "hourly keeps 10 by default");
        w.schedule.retention = 2;
    }
    settle(&mut h);
    press_settled(&mut h, n.next);
    // Finish: the summary.
    assert!(shows(&h, s.finish_title));
    assert!(
        h.query_all_by_label_contains("Hourly; at 15 minutes past each hour")
            .next()
            .is_some()
    );
    press_settled(&mut h, n.finish);
    let status = wait_end(&mut h, "Creating snapshot schedule 'web hourly'");
    assert!(succeeded(&status), "{status:?}");
    let policy = server.refs("vmss").pop().expect("the policy");
    assert_eq!(server.field("vmss", &policy, "enabled"), Some(json!(true)));
    assert_eq!(
        server.field("vmss", &policy, "frequency"),
        Some(json!("hourly"))
    );
    assert_eq!(
        server.field("vmss", &policy, "retained_snapshots"),
        Some(json!(2))
    );
    assert_eq!(
        server.field("vm", WEB, "snapshot_schedule"),
        Some(json!(policy))
    );
    // The dialog lists it, enabled with one VM; three hours run it three
    // times and its history says so.
    assert!(shows(&h, "web hourly") && shows(&h, s.enabled));
    server.advance_clock(3 * 3600);
    catch_up(&mut h);
    press_settled(&mut h, "web hourly");
    press_settled(&mut h, s.show_history);
    step_until(&mut h, "the history", |h| {
        h.query_all_by_label(s.succeeded).count() >= 3
    });
    assert!(shows(
        &h,
        &crate::i18n::fill(s.history_for, &[("name", "web hourly")])
    ));
    let kept = server
        .refs("vm")
        .into_iter()
        .filter(|r| server.field("vm", r, "is_vmss_snapshot") == Some(json!(true)))
        .count();
    assert_eq!(kept, 2, "retention");
    // Disable: the clock no longer runs it.
    press_settled(&mut h, s.button_disable);
    let status = wait_end(&mut h, "Change policy 'web hourly' status");
    assert!(succeeded(&status), "{status:?}");
    assert_eq!(server.field("vmss", &policy, "enabled"), Some(json!(false)));
    let runs = server.vmss_runs().len();
    server.advance_clock(2 * 3600);
    assert_eq!(server.vmss_runs().len(), runs);
    catch_up(&mut h);
    // Properties: a description.
    press_settled(&mut h, s.button_properties);
    assert!(h.state().vmss.properties.is_some());
    step_until(&mut h, "the schedule tab's time", |h| {
        h.state().vmss.properties.is_some()
    });
    h.state_mut().vmss.properties.as_mut().unwrap().description = "edited".into();
    for _ in 0..10 {
        settle(&mut h);
    }
    press_settled(&mut h, s.ok);
    let status = wait_end(
        &mut h,
        "Saving the properties of snapshot schedule 'web hourly'",
    );
    assert!(succeeded(&status), "{status:?}");
    assert_eq!(
        server.field("vmss", &policy, "name_description"),
        Some(json!("edited"))
    );
    assert_eq!(
        server.field("vmss", &policy, "frequency"),
        Some(json!("hourly"))
    );
    catch_up(&mut h);
    // The VM's menu and Snapshots tab know its schedule.
    let conn = h.state().backend.state().connection(c).cloned().unwrap();
    let record = conn.model.vms.get(&Ref::new(WEB)).cloned().unwrap();
    let entries = crate::provision_ui::vm_entries(&conn, &Ref::new(WEB), &record);
    assert!(entries.iter().any(|e| matches!(e,
        crate::ops_ui::Entry::Submenu { label, enabled: true, .. } if label == s.menu_assign)));
    // Delete: asked with the number of VMs.
    press_settled(&mut h, "web hourly");
    press_settled(&mut h, s.button_delete);
    assert!(shows(
        &h,
        &crate::i18n::fill(s.delete_one, &[("name", "web hourly"), ("count", "1")])
    ));
    press_settled(&mut h, s.yes);
    let status = wait_end(&mut h, "Delete policies");
    assert!(succeeded(&status), "{status:?}");
    assert!(server.refs("vmss").is_empty());
    assert_eq!(
        server.field("vm", WEB, "snapshot_schedule"),
        Some(json!("OpaqueRef:NULL"))
    );
    press_settled(&mut h, s.button_close);
    assert!(h.state().vmss.manage.is_none());
    // The Snapshots tab's line.
    select(&mut h, Selection::Vm(c, Ref::new(WEB)));
    super::provisioning::tab(&mut h, crate::tab_strings::tab_strings().view_snapshots);
    assert!(shows(&h, s.tab_not_in));
    press_settled(&mut h, s.tab_view);
    assert!(h.state().vmss.manage.is_some());
}

// ---- disaster recovery ------------------------------------------------------------------

async fn session(server: &MockServer) -> Session {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = Arc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    Session::login_with_password(client, xapi_mock::USERNAME, xapi_mock::PASSWORD, "f12b-ui")
        .await
        .unwrap()
}

/// Two standalone lab servers, both connected in the window.
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

/// On pool A: an iSCSI SR on LUN 1 of `iscsi.lab`, a VM "app" and a vApp
/// "stack" with a VM "db", both with a disk on it; (sr, app, db).
fn pool_a(
    runtime: &tokio::runtime::Runtime,
    a: &MockServer,
    tag: &str,
) -> (String, String, String) {
    runtime.block_on(async {
        let s = session(a).await;
        let sr: String = s
            .call(
                "SR.create",
                vec![
                    json!(format!("OpaqueRef:host-0001-{tag}")),
                    json!({"target": "iscsi.lab", "port": "3260",
                           "targetIQN": "iqn.2026-11.lab.mock:storage",
                           "SCSIid": "36001405000000000000000000000a001"}),
                    json!(0),
                    json!("DR iSCSI"),
                    json!(""),
                    json!("lvmoiscsi"),
                    json!("user"),
                    json!(true),
                    json!({}),
                ],
            )
            .await
            .unwrap();
        let mut vms = Vec::new();
        for (name, mac) in [("app", "02:00:00:0b:00:01"), ("db", "02:00:00:0b:00:02")] {
            let vm: String = s
                .call(
                    "VM.create",
                    vec![
                        json!({"name_label": name, "memory_static_max": 1073741824_i64,
                        "memory_dynamic_max": 1073741824_i64, "memory_dynamic_min": 1073741824_i64,
                        "memory_static_min": 536870912_i64, "VCPUs_max": 1, "VCPUs_at_startup": 1}),
                    ],
                )
                .await
                .unwrap();
            let vdi: String = s
                .call(
                    "VDI.create",
                    vec![json!({"name_label": format!("{name} disk"), "SR": sr,
                        "virtual_size": 1048576, "type": "user", "sharable": false,
                        "read_only": false})],
                )
                .await
                .unwrap();
            let _: String = s
                .call(
                    "VBD.create",
                    vec![
                        json!({"VM": vm, "VDI": vdi, "userdevice": "0", "bootable": true,
                        "mode": "RW", "type": "Disk", "empty": false, "other_config": {}}),
                    ],
                )
                .await
                .unwrap();
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
            vms.push(vm);
        }
        let app: String = s
            .call("VM_appliance.create", vec![json!({"name_label": "stack"})])
            .await
            .unwrap();
        let _: serde_json::Value = s
            .call("VM.set_appliance", vec![json!(vms[1]), json!(app)])
            .await
            .unwrap();
        (sr, vms[0].clone(), vms[1].clone())
    })
}

fn uuid(server: &MockServer, class: &str, r: &str) -> String {
    server
        .field(class, r, "uuid")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap()
}

fn wizard_page(h: &Harness<'_, XenAdminApp>) -> Option<Page> {
    h.state().dr.wizard.as_ref().map(|w| w.page)
}

/// Opens the DR wizard on pool `tag` and goes to the Storage page as a
/// test failover, then finds the iSCSI SR.
fn to_storage_with_iscsi(h: &mut Harness<'_, XenAdminApp>, c: ConnectionId, tag: &str) {
    let s = dr_strings();
    select(
        h,
        Selection::Pool(c, Ref::new(format!("OpaqueRef:pool-0001-{tag}"))),
    );
    pool_menu(h, Some(s.menu), s.menu_wizard);
    assert_eq!(wizard_page(h), Some(Page::Welcome));
    press_settled(h, s.dryrun);
    press_settled(h, s.next);
    assert_eq!(wizard_page(h), Some(Page::First));
    assert!(shows(h, s.first_title_dryrun));
    press_settled(h, s.next);
    assert_eq!(wizard_page(h), Some(Page::Storage));
    // Find Storage Repositories > Find Software iSCSI SRs...
    press_settled(h, s.find_srs);
    press(h, s.find_iscsi);
    settle(h);
    assert!(shows(h, s.iscsi_title));
    // Host, IQNs and LUNs discovered; LUN 1 chosen.
    super::provisioning::type_into(h, s.iscsi_host, "iscsi.lab");
    press_settled(h, s.iscsi_discover_iqns);
    step_until(h, "IQNs", |h| {
        h.state()
            .dr
            .wizard
            .as_ref()
            .and_then(|w| w.iscsi.as_ref())
            .is_some_and(|d| d.iqn.is_some())
    });
    // The target's "storage" IQN.
    {
        let d = h
            .state_mut()
            .dr
            .wizard
            .as_mut()
            .unwrap()
            .iscsi
            .as_mut()
            .unwrap();
        d.iqn = d
            .iqns
            .iter()
            .position(|q| q.target_iqn.ends_with(":storage"));
    }
    settle(h);
    press_settled(h, s.iscsi_discover_luns);
    step_until(h, "LUNs", |h| {
        h.state()
            .dr
            .wizard
            .as_ref()
            .and_then(|w| w.iscsi.as_ref())
            .is_some_and(|d| d.lun.is_some())
    });
    // LUN 1 (the one pool A uses).
    {
        let d = h
            .state_mut()
            .dr
            .wizard
            .as_mut()
            .unwrap()
            .iscsi
            .as_mut()
            .unwrap();
        d.lun = d.luns.iter().position(|l| l.lun_id == 1);
        assert!(d.lun.is_some());
    }
    settle(h);
    press_settled(h, s.ok);
    // Found with metadata: ticked.
    step_until(h, "the scan", |h| {
        h.state()
            .dr
            .wizard
            .as_ref()
            .is_some_and(|w| w.selected_srs.len() == 1 && w.iscsi.is_none())
    });
}

#[test]
fn disaster_recovery_wizard_test_failover_between_two_pools() {
    let runtime = runtime();
    let (ta, tb) = ("f12b-dra", "f12b-drb");
    let (a, b, mut h) = lab(&runtime, ta, tb);
    let (sr, app, db) = pool_a(&runtime, &a, ta);
    let (app_uuid, db_uuid) = (uuid(&a, "vm", &app), uuid(&a, "vm", &db));
    let (ca, cb) = (conn_to(&h, &a), conn_to(&h, &b));
    let s = dr_strings();
    step_until(&mut h, "A's SR", |h| shows(h, "DR iSCSI"));

    // ---- Configure on pool A: the iSCSI SR ticked.
    select(
        &mut h,
        Selection::Pool(ca, Ref::new(format!("OpaqueRef:pool-0001-{ta}"))),
    );
    pool_menu(&mut h, Some(s.menu), s.menu_configure);
    step_until(&mut h, "the SRs", |h| {
        h.state()
            .dr
            .configure
            .as_ref()
            .is_some_and(|d| !d.rows.is_empty())
    });
    {
        let d = h.state_mut().dr.configure.as_mut().unwrap();
        assert_eq!(d.rows.len(), 1, "only the shared SR can hold metadata");
        d.rows[0].1 = true;
    }
    settle(&mut h);
    assert!(shows(&h, s.enable_note));
    press_settled(&mut h, s.ok);
    let status = wait_end(&mut h, s.op_configure);
    assert!(succeeded(&status), "{status:?}");
    let pool_a_ref = format!("OpaqueRef:pool-0001-{ta}");
    assert_eq!(
        a.field("pool", &pool_a_ref, "metadata_VDIs")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let _ = sr;
    a.sync_metadata();

    // ---- The wizard on pool B: a test failover.
    to_storage_with_iscsi(&mut h, cb, tb);
    {
        let w = h.state().dr.wizard.as_ref().unwrap();
        assert_eq!(w.selected_srs.len(), 1, "found with metadata: ticked");
    }
    press_settled(&mut h, s.next);
    step_until(&mut h, "the metadata", |h| {
        wizard_page(h) == Some(Page::VApps)
    });
    assert!(shows(&h, "stack") && shows(&h, "app") && shows(&h, "db"));
    press_settled(&mut h, s.select_all);
    assert!(
        shows(&h, s.failover_paused),
        "a test failover starts them paused"
    );
    press_settled(&mut h, s.next);
    // Pre-checks: all fine.
    assert_eq!(wizard_page(&h), Some(Page::Prechecks));
    step_until(&mut h, "the pre-checks", |h| {
        h.state()
            .dr
            .wizard
            .as_ref()
            .is_some_and(|w| !w.checks.is_empty())
            && shows(h, s.continue_dryrun)
    });
    assert!(shows(
        &h,
        &crate::i18n::fill(
            s.header,
            &[("index", "1"), ("count", "3"), ("group", s.group_ha)]
        )
    ));
    press_settled(&mut h, s.next_failover);
    // Progress: the vApp, the VM, then the start.
    assert_eq!(wizard_page(&h), Some(Page::Recover));
    step_until(&mut h, "the recovery", |h| {
        h.state().dr.wizard.as_ref().is_some_and(|w| {
            w.rows.len() == 3
                && w.rows
                    .iter()
                    .all(|r| r.state == crate::dr_ui::RowState::Done)
        })
    });
    let recovered: Vec<String> = b
        .refs("vm")
        .into_iter()
        .filter(|r| {
            let u = b
                .field("vm", r, "uuid")
                .and_then(|v| v.as_str().map(str::to_owned));
            u.as_deref() == Some(app_uuid.as_str()) || u.as_deref() == Some(db_uuid.as_str())
        })
        .collect();
    assert_eq!(recovered.len(), 2);
    assert!(recovered.iter().all(|r| b.vm_power_state(r) == "Paused"));
    press_settled(&mut h, s.next);
    // The clean-up, then the report.
    step_until(&mut h, "the report", |h| {
        wizard_page(h) == Some(Page::Report)
    });
    let report = h.state().dr.wizard.as_ref().unwrap().report.text();
    for line in [
        s.report_started,
        "Recover vApp 'stack' - Succeeded",
        "Recover VM 'app' - Succeeded",
        s.report_dr_cleanup,
        s.report_dryrun_cleanup,
    ] {
        assert!(report.contains(line), "{line} in\n{report}");
    }
    assert!(
        report.contains(&crate::i18n::fill(
            s.report_introduced,
            &[("srs", "DR iSCSI")]
        )),
        "{report}"
    );
    press_settled(&mut h, s.finish);
    assert!(h.state().dr.wizard.is_none());
    // B is as before; A was not touched.
    catch_up(&mut h);
    assert!(b.refs("vm").iter().all(|r| {
        let u = b
            .field("vm", r, "uuid")
            .and_then(|v| v.as_str().map(str::to_owned));
        u.as_deref() != Some(app_uuid.as_str()) && u.as_deref() != Some(db_uuid.as_str())
    }));
    assert!(b.refs("vm_appliance").is_empty());
    assert!(b.refs("dr_task").is_empty());
    let sr_uuid = uuid(&a, "sr", &sr);
    assert!(
        b.refs("sr")
            .iter()
            .all(|r| b.field("sr", r, "uuid") != Some(json!(sr_uuid)))
    );
    assert_eq!(a.vm_power_state(&app), "Halted");

    // ---- Cancelled after the SRs were introduced: the DR task goes.
    to_storage_with_iscsi(&mut h, cb, tb);
    press_settled(&mut h, s.next);
    step_until(&mut h, "the metadata", |h| {
        wizard_page(h) == Some(Page::VApps)
    });
    assert_eq!(b.refs("dr_task").len(), 1);
    press_settled(&mut h, s.cancel);
    assert!(h.state().dr.wizard.is_none());
    step_until(&mut h, "the clean-up", |_| b.refs("dr_task").is_empty());
    let _ = cb;
}
