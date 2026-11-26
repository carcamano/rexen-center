//! F10a in the window, against two lab mocks: the Pool menu, New Pool
//! (reasons listed, a member ticked, Create Pool), Remove Server with its
//! confirmation and the removed server connected again, Add Server with
//! its question, a failed New Pool undone and a refused Add Server listing
//! why; the maintenance dialog choosing a new coordinator; every page of
//! the pool and server Properties dialogs (checks, saving, Cancel); Change
//! Server Password, Control Domain Memory, Install Certificate, Back Up /
//! Restore, Rotate Pool Secret, Enable Certificate Verification and Make
//! into standalone server.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::properties::{Page, PropertiesTarget};

use super::provisioning::{catch_up, last_enabled, press, select, shows, type_into};
use super::*;
use crate::pool_strings::pool_strings;
use crate::provision_ui::DialogRequest;

const HOST_A: &str = "OpaqueRef:host-0001-a";
const HOST_B: &str = "OpaqueRef:host-0001-b";
const POOL_A: &str = "OpaqueRef:pool-0001-a";

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> serde_json::Value {
    server.field(class, r, f).unwrap_or(serde_json::Value::Null)
}

/// The latest operation titled `title`, once ended: its error text if it
/// failed.
fn wait_op(h: &mut Harness<'_, XenAdminApp>, title: &str) -> Result<(), String> {
    step_until(h, title, |h| {
        let state = h.state().backend.state();
        let ops: Vec<_> = state
            .operations
            .iter()
            .filter(|o| i18n::operation_title(&o.kind, &o.server) == title)
            .collect();
        !ops.is_empty() && ops.iter().all(|o| !o.is_running())
    });
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

fn conn_to(h: &Harness<'_, XenAdminApp>, server: &MockServer) -> Option<ConnectionId> {
    h.state()
        .backend
        .state()
        .connections
        .iter()
        .find(|c| c.server == server.address() && c.status.is_connected())
        .map(|c| c.id)
}

/// Two standalone lab servers, both connected in the window.
fn lab(
    runtime: &tokio::runtime::Runtime,
) -> (MockServer, MockServer, Harness<'static, XenAdminApp>) {
    let a = runtime.block_on(MockServer::start_lab(false, "a", 1));
    let b = runtime.block_on(MockServer::start_lab(false, "b", 2));
    a.set_task_timing(2, Duration::from_millis(20));
    b.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &a.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "a connected", |h| shows(h, "lab-host-a"));
    h.state().backend.send(Command::Connect {
        server: b.address(),
        username: xapi_mock::USERNAME.into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    step_until(&mut h, "b connected", |h| shows(h, "lab-host-b"));
    settle(&mut h);
    (a, b, h)
}

/// Presses `label` after the window settled (a dialog that changed size
/// re-centers over a few frames).
fn press_settled(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    settle(h);
    settle(h);
    press(h, label);
}

fn open(h: &mut Harness<'_, XenAdminApp>, conn: ConnectionId, request: DialogRequest) {
    h.state_mut().ops.dialog_requests.push((conn, request));
    settle(h);
    settle(h);
}

/// Opens a top menu of the menu bar and clicks `item` in it.
fn menu(h: &mut Harness<'_, XenAdminApp>, top: &str, item: &str) {
    h.get_all_by_label(top).next().expect("menu").click();
    settle(h);
    press(h, item);
}

fn pool_name(server: &MockServer, pool: &str) -> String {
    field(server, "pool", pool, "name_label")
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn new_pool_remove_and_add_server_through_the_window() {
    let runtime = runtime();
    let (a, b, mut h) = lab(&runtime);
    let p = pool_strings();
    let ca = conn_to(&h, &a).unwrap();
    // The Pool menu with a standalone server selected: New Pool only, of
    // the pool entries that need a pool.
    select(&mut h, Selection::Host(ca, Ref::new(HOST_A)));
    menu(&mut h, p.menu_pool, p.new_pool);
    assert!(h.state_mut().pool.new_pool.is_some());
    // Both servers are listed: a as coordinator, b allowed.
    assert!(shows(&h, p.reason_coordinator));
    assert!(!last_enabled(&h, p.create_pool), "a name is needed first");
    type_into(&mut h, p.name_label, "UI Pool");
    h.get_by_role_and_label(Role::CheckBox, "lab-host-b")
        .click();
    settle(&mut h);
    assert!(last_enabled(&h, p.create_pool));
    press(&mut h, p.create_pool);
    wait_ok(
        &mut h,
        "Creating new pool 'UI Pool' with coordinator 'lab-host-a'",
    );
    step_until(&mut h, "pool of two in the tree", |h| {
        shows(h, "UI Pool") && h.state().backend.state().connections.len() == 1
    });
    assert!(b.is_pool_member());
    assert_eq!(a.refs("host").len(), 2);

    // Remove Server: the confirmation, then b comes back on its own.
    let ca = conn_to(&h, &a).unwrap();
    select(&mut h, Selection::Pool(ca, Ref::new(POOL_A)));
    let state = h.state().backend.state();
    let conn = state.connection(ca).unwrap();
    let entries = crate::pool_ui::pool_entries(&state, conn);
    let remove = entries
        .iter()
        .find_map(|e| match e {
            crate::ops_ui::Entry::Submenu { label, entries, .. } if label == p.remove_server => {
                Some(entries.len())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(remove, 1, "only lab-host-b can be removed");
    open(
        &mut h,
        ca,
        DialogRequest::Run(xen_core::ActionRequest::EjectHost {
            host: Ref::new(HOST_B),
        }),
    );
    assert!(shows(&h, p.remove_title));
    press(&mut h, p.remove_yes);
    wait_ok(&mut h, "Removing server 'lab-host-b' from pool 'UI Pool'");
    // The window connects to the removed server once it answers (after
    // 30 s, as the original); the mock answers at once, the test does not
    // wait that long: the backend's own option decides, so connect here.
    assert!(!b.is_pool_member());
    assert_eq!(a.refs("host").len(), 1);
    h.state().backend.send(Command::Connect {
        server: b.address(),
        username: xapi_mock::USERNAME.into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    step_until(&mut h, "b standalone again", |h| conn_to(h, &b).is_some());
    let cb = conn_to(&h, &b).unwrap();

    // Add Server from the Pool menu: the question, then the join.
    open(
        &mut h,
        ca,
        DialogRequest::AddToPool {
            pool: ca,
            hosts: vec![cb],
            confirm: true,
        },
    );
    assert!(shows(
        &h,
        "Do you want to move the server lab-host-b into the pool UI Pool?"
    ));
    press(&mut h, p.yes);
    wait_ok(&mut h, "Adding server 'lab-host-b' to pool 'UI Pool'");
    assert!(b.is_pool_member());

    // Enter maintenance mode on the coordinator: the dialog offers b as the
    // new coordinator, and the connection follows it.
    step_until(&mut h, "two hosts seen", |h| {
        h.state()
            .backend
            .state()
            .connection(ca)
            .is_some_and(|c| c.model.hosts.len() == 2)
    });
    open(
        &mut h,
        ca,
        DialogRequest::Run(xen_core::ActionRequest::Host {
            host: Ref::new(HOST_A),
            action: xen_core::HostAction::EnterMaintenance,
        }),
    );
    assert!(shows(&h, p.new_coordinator_label));
    step_until(&mut h, "evacuation checked", |h| {
        last_enabled(h, crate::ops_strings::ops_strings().evacuate_button)
    });
    press(&mut h, crate::ops_strings::ops_strings().evacuate_button);
    step_until(&mut h, "the pool follows b", |h| {
        h.state().backend.state().connection(ca).is_some_and(|c| {
            c.server == b.address()
                && c.status.is_connected()
                && c.model
                    .pool()
                    .is_some_and(|(_, p)| p.master == Ref::new(HOST_B))
        })
    });
}

#[test]
fn failed_new_pool_is_undone_and_refused_joins_say_why() {
    let runtime = runtime();
    let (a, b, mut h) = lab(&runtime);
    let p = pool_strings();
    let ca = conn_to(&h, &a).unwrap();
    let cb = conn_to(&h, &b).unwrap();
    b.fail_method(
        "pool.join",
        Some(("JOINING_HOST_CANNOT_HAVE_RUNNING_VMS", &[])),
    );
    open(
        &mut h,
        ca,
        DialogRequest::NewPool {
            servers: vec![ca, cb],
        },
    );
    type_into(&mut h, p.name_label, "Doomed");
    press(&mut h, p.create_pool);
    let err = wait_op(
        &mut h,
        "Creating new pool 'Doomed' with coordinator 'lab-host-a'",
    )
    .unwrap_err();
    assert!(err.contains("running"), "{err}");
    assert_eq!(pool_name(&a, POOL_A), "");
    assert!(!b.is_pool_member());
    assert!(conn_to(&h, &b).is_some(), "b stays connected on its own");
    step_until(&mut h, "error listed", |h| {
        h.state().errors.iter().any(|e| e.contains("Doomed"))
    });
    h.state_mut().errors.clear();

    // A named pool with HA on: Add Server lists why b cannot join.
    a.set_name_label("pool", POOL_A, "HA pool");
    a.set_field("pool", POOL_A, "ha_enabled", json!(true));
    // Two changes, maybe two event batches: wait for the cache to have both.
    step_until(&mut h, "pool named, HA seen", |h| {
        shows(h, "HA pool")
            && h.state().backend.state().connections.iter().any(|c| {
                c.server == a.address() && c.model.pool().is_some_and(|(_, p)| p.ha_enabled)
            })
    });
    open(
        &mut h,
        ca,
        DialogRequest::AddToPool {
            pool: ca,
            hosts: vec![cb],
            confirm: true,
        },
    );
    let errors = h.state().errors.join("\n");
    assert!(errors.contains(p.add_error_title), "{errors}");
    assert!(
        errors.contains("lab-host-b: HA is enabled on the pool"),
        "{errors}"
    );
    assert!(!b.is_pool_member());
}

#[test]
fn properties_pages_check_and_save() {
    let runtime = runtime();
    let a = runtime.block_on(MockServer::start_lab(false, "a", 1));
    a.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &a.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "a connected", |h| shows(h, "lab-host-a"));
    let p = pool_strings();
    let ca = conn_to(&h, &a).unwrap();

    // Server Properties: every page the original shows for a standalone
    // server.
    open(
        &mut h,
        ca,
        DialogRequest::Properties(PropertiesTarget::Host(Ref::new(HOST_A))),
    );
    step_until(&mut h, "NRPE read", |h| {
        h.state()
            .pool
            .properties
            .as_ref()
            .is_some_and(|d| matches!(d.nrpe_state, crate::props_ui::NrpeState::Loaded(_)))
    });
    let pages = h.state_mut().pool.props().unwrap().pages.clone();
    assert_eq!(
        pages,
        vec![
            Page::General,
            Page::CustomFields,
            Page::Alerts,
            Page::EmailOptions,
            Page::Multipath,
            Page::LogDestination,
            Page::PowerOn,
            Page::Autostart,
            Page::NetworkOptions,
            Page::Clustering,
            Page::Nrpe,
        ]
    );
    assert!(shows(&h, "'lab-host-a' Properties"));
    // A bad IQN is refused on its page.
    {
        let d = h.state_mut().pool.props().unwrap();
        d.iqn = "not-an-iqn".into();
        d.name = "renamed-a".into();
        d.tags = "lab, ui".into();
    }
    settle(&mut h);
    press_settled(&mut h, p.ok);
    assert!(shows(&h, p.invalid_iqn));
    // The other pages: log destination, email, power on, NRPE.
    {
        let d = h.state_mut().pool.props().unwrap();
        d.iqn = "iqn.2026-11.org.example:ui".into();
        d.log_remote = true;
        d.log_server = "bad host".into();
    }
    // The next page with a problem: the log destination.
    press_settled(&mut h, p.ok);
    assert!(shows(&h, p.log_invalid));
    assert_eq!(
        h.state_mut().pool.props().unwrap().page,
        Page::LogDestination
    );
    {
        let d = h.state_mut().pool.props().unwrap();
        d.log_server = "syslog.example.com".into();
        d.email.enabled = true;
        d.email.address = "ops@example.com".into();
        d.email.server = "smtp.example.com".into();
        d.nrpe.enabled = true;
        d.nrpe.allow_hosts = "192.0.2.9".into();
        d.page = Page::PowerOn;
    }
    settle(&mut h);
    step_until(&mut h, "power on loaded", |h| {
        h.state()
            .pool
            .properties
            .as_ref()
            .is_some_and(|d| d.power.as_ref().is_some_and(|p| p.pending.is_none()))
    });
    press(&mut h, p.power_drac);
    {
        let d = h.state_mut().pool.props().unwrap();
        let (_, _, _, mode) = &mut d.power.as_mut().unwrap().hosts[0];
        if let xen_core::properties::PowerOnMode::Drac { ip, user, password } = mode {
            *ip = "192.0.2.77".into();
            *user = "root".into();
            *password = "bmc".into();
        } else {
            panic!("DRAC chosen: {mode:?}");
        }
    }
    press_settled(&mut h, p.ok);
    let left = h
        .state_mut()
        .pool
        .props()
        .map(|d| (d.page, d.problem.clone()));
    assert!(left.is_none(), "the dialog stayed open: {left:?}");
    wait_ok(&mut h, "Saving properties of lab-host-a");
    assert!(h.state().pool.properties.is_none());
    assert_eq!(field(&a, "host", HOST_A, "name_label"), json!("renamed-a"));
    assert_eq!(field(&a, "host", HOST_A, "tags"), json!(["lab", "ui"]));
    assert_eq!(
        field(&a, "host", HOST_A, "iscsi_iqn"),
        json!("iqn.2026-11.org.example:ui")
    );
    assert_eq!(
        field(&a, "host", HOST_A, "logging")["syslog_destination"],
        json!("syslog.example.com")
    );
    assert_eq!(
        field(&a, "pool", POOL_A, "other_config")["ssmtp-mailhub"],
        json!("smtp.example.com:25")
    );
    assert_eq!(field(&a, "host", HOST_A, "power_on_mode"), json!("DRAC"));
    assert!(a.nrpe_enabled(HOST_A));

    // Pool Properties: the pool pages; Cancel sends nothing.
    let calls = a.calls().len();
    open(
        &mut h,
        ca,
        DialogRequest::Properties(PropertiesTarget::Pool(Ref::new(POOL_A))),
    );
    let pages = h.state_mut().pool.props().unwrap().pages.clone();
    assert!(pages.contains(&Page::Advanced) && !pages.contains(&Page::Multipath));
    h.state_mut().pool.props().unwrap().compression = true;
    press(&mut h, p.cancel);
    assert!(h.state().pool.properties.is_none());
    assert_eq!(
        field(&a, "pool", POOL_A, "migration_compression"),
        json!(false)
    );
    assert!(
        !a.calls()[calls..]
            .iter()
            .any(|c| c.starts_with("pool.set") || c.starts_with("host.set")),
        "Cancel changed nothing"
    );
    open(
        &mut h,
        ca,
        DialogRequest::Properties(PropertiesTarget::Pool(Ref::new(POOL_A))),
    );
    {
        let d = h.state_mut().pool.props().unwrap();
        d.compression = true;
        d.igmp = true;
        d.name = "Lab".into();
        d.page = Page::Clustering;
    }
    settle(&mut h);
    // Clustering needs a network.
    h.state_mut().pool.props().unwrap().clustering = true;
    press_settled(&mut h, p.ok);
    assert!(shows(&h, p.clustering_select_network));
    let net = h
        .state()
        .backend
        .state()
        .connection(ca)
        .map(|c| {
            xen_core::properties::cluster_networks(&c.model)
                .first()
                .map(|(r, _)| (*r).clone())
        })
        .unwrap()
        .expect("a network with an address");
    h.state_mut().pool.props().unwrap().cluster_network = Some(net);
    press_settled(&mut h, p.ok);
    wait_ok(&mut h, "Saving properties of renamed-a");
    assert_eq!(
        field(&a, "pool", POOL_A, "migration_compression"),
        json!(true)
    );
    assert_eq!(
        field(&a, "pool", POOL_A, "igmp_snooping_enabled"),
        json!(true)
    );
    assert_eq!(a.refs("cluster").len(), 1);
}

#[test]
fn server_dialogs_of_the_server_menu() {
    let runtime = runtime();
    let a = runtime.block_on(MockServer::start_lab(false, "a", 1));
    a.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &a.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "a connected", |h| shows(h, "lab-host-a"));
    let p = pool_strings();
    let ca = conn_to(&h, &a).unwrap();

    // The server menu's entries for a live standalone server.
    let state = h.state().backend.state();
    let conn = state.connection(ca).unwrap();
    let host = conn.model.hosts.get(&Ref::new(HOST_A)).unwrap().clone();
    let entries = crate::pool_ui::host_entries(&state, conn, &Ref::new(HOST_A), &host);
    let labels: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            crate::ops_ui::Entry::Dialog { label, enabled, .. } => {
                Some(format!("{label}:{enabled}"))
            }
            crate::ops_ui::Entry::Named { label, enabled, .. }
            | crate::ops_ui::Entry::Submenu { label, enabled, .. } => {
                Some(format!("{label}:{enabled}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "Add to Pool:true",
            "Remove From Pool:false",
            "Back Up...:true",
            "Restore From Backup...:true",
            "Certificate:true",
            "Control Domain Memory...:true",
            "Password:true",
            "Properties:true",
        ]
    );

    // Change Server Password: mismatch, wrong current password, then OK.
    open(&mut h, ca, DialogRequest::ChangePassword);
    {
        let d = h.state_mut().pool.password.as_mut().unwrap();
        d.current = "wrong".into();
        d.new = "new-secret".into();
        d.confirm = "other".into();
    }
    settle(&mut h);
    press_settled(&mut h, p.ok);
    assert!(shows(&h, p.passwords_dont_match));
    {
        let d = h.state_mut().pool.password.as_mut().unwrap();
        d.confirm = "new-secret".into();
    }
    press_settled(&mut h, p.ok);
    step_until(&mut h, "current password checked", |h| {
        h.state()
            .pool
            .password
            .as_ref()
            .is_some_and(|d| d.error.is_some())
    });
    assert!(shows(&h, p.password_wrong));
    {
        let d = h.state_mut().pool.password.as_mut().unwrap();
        d.current = xapi_mock::PASSWORD.into();
        d.error = None;
    }
    press_settled(&mut h, p.ok);
    wait_ok(&mut h, "Changing administrator password on lab-host-a");
    assert_eq!(a.root_password(), "new-secret");

    // Install Certificate: a missing file is shown on the dialog.
    open(
        &mut h,
        ca,
        DialogRequest::InstallCertificate(Ref::new(HOST_A)),
    );
    {
        let d = h.state_mut().pool.install.as_mut().unwrap();
        d.key = "/nonexistent/key.pem".into();
        d.certificate = "/nonexistent/cert.pem".into();
    }
    press(&mut h, p.install_button);
    assert!(shows(&h, p.path_missing));
    press(&mut h, p.cancel);

    // Back up then restore (the restore's next steps are shown).
    let path = std::env::temp_dir().join(format!("xen-f10a-ui-{}.xbk", std::process::id()));
    open(&mut h, ca, DialogRequest::Backup(Ref::new(HOST_A)));
    // Chosen in the native Save dialog.
    crate::file_dialog::tests::answer_next(Some(path.to_str().unwrap()));
    press(&mut h, strings().browse);
    assert_eq!(
        h.state().pool.transfer.as_ref().unwrap().path,
        path.display().to_string()
    );
    press(&mut h, p.backup_button);
    wait_ok(&mut h, "Backing up server 'lab-host-a'");
    assert!(path.is_file());
    open(&mut h, ca, DialogRequest::Restore(Ref::new(HOST_A)));
    crate::file_dialog::tests::answer_next(Some(path.to_str().unwrap()));
    press(&mut h, strings().browse);
    assert_eq!(
        crate::file_dialog::tests::opened(),
        ["save Server backup files", "open Server backup files"]
    );
    press(&mut h, p.restore_button);
    wait_ok(&mut h, "Restoring server 'lab-host-a'");
    step_until(&mut h, "restore notice", |h| {
        h.query_all_by_label_contains("Administrator's Guide")
            .next()
            .is_some()
    });
    press_settled(&mut h, p.ok);
    assert_eq!(a.restored(), vec![HOST_A.to_owned()]);
    let _ = std::fs::remove_file(&path);

    // Rotate Pool Secret and Enable Certificate Verification ask first.
    open(
        &mut h,
        ca,
        DialogRequest::Run(xen_core::ActionRequest::RotatePoolSecret),
    );
    assert!(shows(&h, p.rotate_remind));
    press_settled(&mut h, p.ok);
    wait_ok(&mut h, "Rotating pool secret");
    assert_eq!(a.secret_rotations(), 1);
    a.set_field("pool", POOL_A, "tls_verification_enabled", json!(false));
    catch_up(&mut h);
    open(
        &mut h,
        ca,
        DialogRequest::Run(xen_core::ActionRequest::EnableTlsVerification),
    );
    press(&mut h, p.tls_yes);
    wait_ok(&mut h, "Enabling certificate verification on lab-host-a...");
    assert_eq!(
        field(&a, "pool", POOL_A, "tls_verification_enabled"),
        json!(true)
    );

    // Control Domain Memory: only in maintenance mode; then reboots.
    open(&mut h, ca, DialogRequest::Dom0Memory(Ref::new(HOST_A)));
    assert!(shows(&h, p.dom0_maintenance));
    press(&mut h, p.cancel);
    let mut oc = field(&a, "host", HOST_A, "other_config");
    oc["MAINTENANCE_MODE"] = json!("true");
    a.set_field("host", HOST_A, "other_config", oc);
    catch_up(&mut h);
    open(&mut h, ca, DialogRequest::Dom0Memory(Ref::new(HOST_A)));
    assert!(shows(&h, p.dom0_reboot));
    let before = h.state().pool.dom0.as_ref().unwrap().memory;
    h.state_mut().pool.dom0.as_mut().unwrap().memory = before + 1024;
    press_settled(&mut h, p.ok);
    assert!(
        h.query_all_by_label_contains("will cause the server to be rebooted")
            .next()
            .is_some()
    );
    press(&mut h, crate::ops_strings::ops_strings().yes);
    wait_ok(
        &mut h,
        "Changing control domain memory settings on 'lab-host-a'",
    );
    assert_eq!(
        field(&a, "vm", "OpaqueRef:vm-dom0-01-a", "memory_dynamic_min"),
        json!((before + 1024) * 1024 * 1024)
    );

    // Make into standalone server.
    a.set_name_label("pool", POOL_A, "Named");
    step_until(&mut h, "named", |h| shows(h, "Named"));
    open(&mut h, ca, DialogRequest::MakeStandalone);
    wait_ok(&mut h, "Making pool 'Named' into standalone server");
    assert_eq!(pool_name(&a, POOL_A), "");
}
