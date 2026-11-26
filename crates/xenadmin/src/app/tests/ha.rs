//! F10b in the window, against the mock's 2-host pool: the Configure HA
//! wizard turns HA on (broken SR warning, heartbeat SR scan, agility and
//! the failure tolerance, summary), the HA tab shows it, the settings
//! dialog changes a priority, Disable HA turns it off; the HA questions of
//! other operations (a server leaving, the tolerance raised again, a VM
//! start that breaks the plan, a protected VM that is not agile); the Users
//! tab: join a domain, add users, change a role, log a user out, remove
//! access, leave; a domain user with the VM Operator role gets the role
//! elevation dialog on forbidden operations.

use std::sync::Arc as StdArc;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Ref, Session, TrustStore};
use xapi_mock::MockServer;
use xen_core::ha::RestartPriority;

use super::provisioning::{catch_up, last_enabled, press, select, shows, tab, type_into};
use super::*;
use crate::ha_strings::ha_strings;
use crate::provision_ui::DialogRequest;

const POOL: &str = "OpaqueRef:pool-0001";
const H2: &str = "OpaqueRef:host-0002";
const BUILD: &str = "OpaqueRef:vm-build";
const LEGACY: &str = "OpaqueRef:vm-legacy";
const NFS: &str = "OpaqueRef:sr-nfs";

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> serde_json::Value {
    server.field(class, r, f).unwrap_or(serde_json::Value::Null)
}

/// The latest operation titled `title`, once ended: its error if it failed.
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

/// The fixture pool, its second server out of maintenance mode, connected
/// as `user` in the window.
fn pool_window(
    runtime: &tokio::runtime::Runtime,
    user: &str,
    password: &str,
) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start(false));
    server.set_field("host", H2, "enabled", json!(true));
    server.set_field("host", H2, "other_config", json!({}));
    server.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(runtime);
    if user == xapi_mock::USERNAME {
        connect_through_dialog(&mut h, &server.address(), password);
    } else {
        h.state_mut().connect.username = user.into();
        connect_through_dialog_as(&mut h, &server.address(), password);
    }
    step_until(&mut h, "tree", |h| {
        shows(h, "Lab Pool")
            && first_conn(h)
                .is_some_and(|c| c.rbac.is_some() && c.model.hosts.values().all(|x| x.enabled))
    });
    settle(&mut h);
    (server, h)
}

/// Asks for `request` as a menu would (questions, RBAC check).
fn request(h: &mut Harness<'_, XenAdminApp>, request: xen_core::ActionRequest) {
    let conn = first_conn(h).unwrap();
    let app = h.state_mut();
    app.ops.request(&conn, request, &app.backend);
    settle(h);
}

fn conn_id(h: &Harness<'_, XenAdminApp>) -> ConnectionId {
    first_conn(h).unwrap().id
}

fn select_pool(h: &mut Harness<'_, XenAdminApp>) {
    let c = conn_id(h);
    select(h, Selection::Pool(c, Ref::new(POOL)));
}

/// A session outside the window.
fn raw_login(
    runtime: &tokio::runtime::Runtime,
    server: &MockServer,
    user: &str,
    password: &str,
) -> xapi::Result<Session> {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = StdArc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    runtime.block_on(Session::login_with_password(
        client, user, password, "f10b-ui",
    ))
}

/// Opens a top menu of the menu bar and the HA submenu's `item`.
fn ha_menu(h: &mut Harness<'_, XenAdminApp>, item: &str) {
    h.get_all_by_label(crate::pool_strings::pool_strings().menu_pool)
        .next()
        .expect("Pool menu")
        .click();
    settle(h);
    press(h, ha_strings().menu_high_availability);
    press(h, item);
}

#[test]
fn ha_wizard_turns_ha_on_and_off_on_the_two_host_pool() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime, xapi_mock::USERNAME, xapi_mock::PASSWORD);
    let hs = ha_strings();
    let n = crate::net_strings::net_strings();

    // The pool's HA tab: not enabled; Configure HA offered, no Disable HA.
    select_pool(&mut h);
    tab(&mut h, hs.tab_ha);
    assert!(shows(&h, &fill(hs.ha_panel_blurb, &[("pool", "Lab Pool")])));
    assert!(!shows(&h, hs.disable_ha));

    // The Pool menu's High Availability submenu opens the wizard; the
    // broken ISO library is reported first (`HAWizard.OnShown`).
    ha_menu(&mut h, hs.configure_ha);
    assert!(shows(
        &h,
        &fill(hs.srs_broken_warning, &[("srs", "ISO library")])
    ));
    press(&mut h, hs.ok);
    assert!(h.state().ha.wizard_open());
    assert!(shows(&h, hs.intro_heading));
    // A superuser: no RBAC page.
    assert!(!shows(&h, hs.page_rbac));
    press(&mut h, n.next);

    // Heartbeat SR: NFS can be chosen, the ISO library cannot.
    step_until(&mut h, "SR scan", |h| shows(h, "NFS VM storage"));
    settle(&mut h);
    assert!(!last_enabled(&h, n.next), "no SR chosen yet");
    assert!(!last_enabled(&h, "ISO library"));
    assert!(shows(&h, "This SR does not support this operation"));
    press(&mut h, "NFS VM storage");
    assert!(last_enabled(&h, n.next));
    press(&mut h, n.next);

    // Priorities: agile VMs proposed for restart, the others best effort;
    // the planner allows 1 failure.
    let guaranteed = fill(hs.guaranteed_message, &[("max", "1")]);
    step_until(&mut h, "planner", |h| shows(h, &guaranteed));
    settle(&mut h);
    assert!(shows(&h, &fill(hs.ntol_max, &[("max", "1")])));
    assert!(
        h.query_all_by_label(hs.calculating_agility)
            .next()
            .is_none()
    );
    // db-01 is set to "Do not restart" through the priority list.
    press(&mut h, "db-01");
    h.get_by_role(Role::ComboBox).click();
    settle(&mut h);
    press(&mut h, hs.priority_do_not_restart);
    settle(&mut h);
    // web-01 (local disk) cannot be set to restart.
    press(&mut h, "web-01");
    h.get_by_role(Role::ComboBox).click();
    settle(&mut h);
    assert!(!last_enabled(&h, hs.priority_restart));
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    step_until(&mut h, "planner again", |h| shows(h, &guaranteed));
    press(&mut h, n.next);

    // The summary, then Finish.
    assert!(shows(&h, hs.finish_summary));
    assert!(shows(&h, hs.finish_sr) && shows(&h, "NFS VM storage"));
    press(&mut h, n.finish);
    wait_ok(&mut h, "Enabling HA on pool 'Lab Pool'");
    assert!(!h.state().ha.wizard_open());
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(true));
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(1)
    );
    assert_eq!(
        field(&server, "vm", BUILD, "ha_restart_priority"),
        json!("restart")
    );
    assert_eq!(
        field(&server, "vm", "OpaqueRef:vm-db", "ha_restart_priority"),
        json!("")
    );
    assert_eq!(
        field(&server, "vm", "OpaqueRef:vm-web", "ha_restart_priority"),
        json!("best-effort")
    );
    let calls = server.calls();
    assert!(calls.iter().any(|c| c == "Async.pool.enable_ha"));

    // The HA tab: configuration and heartbeating (still initializing).
    step_until(&mut h, "HA tab on", |h| {
        shows(
            h,
            &fill(hs.ha_tab_configured_blurb, &[("pool", "Lab Pool")]),
        )
    });
    for label in [
        hs.configured_capacity,
        hs.current_capacity,
        hs.heartbeating_status,
        hs.network,
        "NFS VM storage",
        "xcp-host-01",
        "xcp-host-02",
        hs.heartbeat_initialising,
    ] {
        assert!(shows(&h, label), "{label}");
    }
    assert!(shows(&h, hs.disable_ha));

    // Configure HA again: the settings dialog; db-01 back to restart.
    press(&mut h, hs.configure_ha);
    assert!(h.state().ha.edit_open());
    assert!(shows(&h, &fill(hs.edit_title, &[("pool", "Lab Pool")])));
    step_until(&mut h, "edit planner", |h| shows(h, &guaranteed));
    press(&mut h, "db-01");
    h.get_by_role(Role::ComboBox).click();
    settle(&mut h);
    press(&mut h, hs.priority_restart);
    step_until(&mut h, "edit planner again", |h| shows(h, &guaranteed));
    press(&mut h, hs.ok);
    wait_ok(&mut h, hs.op_set_priorities);
    assert_eq!(
        field(&server, "vm", "OpaqueRef:vm-db", "ha_restart_priority"),
        json!("restart")
    );
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "Async.pool.sync_database")
    );

    // Disable HA: asked, then off.
    press(&mut h, hs.disable_ha);
    assert!(shows(&h, &fill(hs.disable_query, &[("pool", "Lab Pool")])));
    press(&mut h, crate::ops_strings::ops_strings().yes);
    wait_ok(&mut h, "Disabling HA on pool 'Lab Pool'");
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(false));
    step_until(&mut h, "HA tab off", |h| {
        shows(h, &fill(hs.ha_panel_blurb, &[("pool", "Lab Pool")]))
    });
}

#[test]
fn ha_questions_of_other_operations() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime, xapi_mock::USERNAME, xapi_mock::PASSWORD);
    let hs = ha_strings();
    let o = crate::ops_strings::ops_strings();
    let c = conn_id(&h);
    // HA on with build-runner protected, ntol 1.
    h.state().backend.send(Command::Action {
        connection: c,
        request: xen_core::ActionRequest::EnableHa {
            heartbeat_sr: Ref::new(NFS),
            ntol: 1,
            startup: vec![xen_core::ha::VmStartup {
                vm: Ref::new(BUILD),
                priority: RestartPriority::Restart,
                order: 0,
                start_delay: 0,
            }],
        },
    });
    wait_ok(&mut h, "Enabling HA on pool 'Lab Pool'");
    step_until(&mut h, "HA seen", |h| {
        first_conn(h).is_some_and(|c| c.model.pool().is_some_and(|(_, p)| p.ha_enabled))
    });

    // Maintenance mode on server 2 would leave one server: tolerance 0.
    request(
        &mut h,
        xen_core::ActionRequest::Host {
            host: Ref::new(H2),
            action: xen_core::HostAction::EnterMaintenance,
        },
    );
    let question = fill(
        hs.host_disable_ntol_zero,
        &[("pool", "Lab Pool"), ("host", "xcp-host-02")],
    );
    step_until(&mut h, "ntol question", |h| shows(h, &question));
    press(&mut h, o.yes);
    wait_ok(&mut h, "Server 'xcp-host-02' entering maintenance mode");
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(0)
    );
    assert_eq!(field(&server, "host", H2, "enabled"), json!(false));

    // Out of maintenance mode: raise it again?
    request(
        &mut h,
        xen_core::ActionRequest::Host {
            host: Ref::new(H2),
            action: xen_core::HostAction::ExitMaintenance,
        },
    );
    let raise = fill(
        hs.host_enable_ntol_raise,
        &[
            ("pool", "Lab Pool"),
            ("host", "xcp-host-02"),
            ("current", "0"),
            ("max", "1"),
        ],
    );
    step_until(&mut h, "raise question", |h| shows(h, &raise));
    press(&mut h, o.yes);
    wait_ok(&mut h, "Setting the failover tolerance of pool 'Lab Pool'");
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(1)
    );

    // A start that breaks the plan: ntol 2 asked, the pool can do 1.
    server.set_field("pool", POOL, "ha_host_failures_to_tolerate", json!(2));
    server.set_field("pool", POOL, "ha_plan_exists_for", json!(2));
    server.set_field("vm", BUILD, "memory_static_max", json!(8_i64 << 30));
    catch_up(&mut h);
    step_until(&mut h, "ntol 2 seen", |h| {
        first_conn(h).is_some_and(|c| {
            c.model
                .pool()
                .is_some_and(|(_, p)| p.ha_host_failures_to_tolerate == 2)
        })
    });
    request(
        &mut h,
        xen_core::ActionRequest::Vm {
            vm: Ref::new(BUILD),
            action: xen_core::VmAction::Start,
        },
    );
    let drop = fill(
        hs.vm_start_ntol_drop,
        &[
            ("pool", "Lab Pool"),
            ("current", "2"),
            ("vm", "build-runner"),
            ("target", "1"),
        ],
    );
    step_until(&mut h, "drop question", |h| shows(h, &drop));
    press(&mut h, o.yes);
    step_until(&mut h, "build-runner running", |_| {
        server.vm_power_state(BUILD) == "Running"
    });
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(1)
    );

    // A protected VM that is not agile: its priority is cleared first.
    server.set_field("vm", LEGACY, "ha_restart_priority", json!("restart"));
    step_until(&mut h, "legacy protected", |h| {
        first_conn(h)
            .is_some_and(|c| xen_core::ha::priority(&c.model.vms[&Ref::new(LEGACY)]).is_restart())
    });
    request(
        &mut h,
        xen_core::ActionRequest::Vm {
            vm: Ref::new(LEGACY),
            action: xen_core::VmAction::Resume,
        },
    );
    let invalid = fill(hs.invalid_config_resume, &[("vm", "legacy-app")]);
    step_until(&mut h, "invalid config question", |h| shows(h, &invalid));
    press(&mut h, hs.ok);
    step_until(&mut h, "legacy resumed", |_| {
        server.vm_power_state(LEGACY) == "Running"
    });
    assert_eq!(
        field(&server, "vm", LEGACY, "ha_restart_priority"),
        json!("")
    );
}

#[test]
fn users_tab_joins_a_domain_and_manages_users() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime, xapi_mock::USERNAME, xapi_mock::PASSWORD);
    // XCP-ng 8.3 with a licence that allows pool secret rotation (the
    // exit advice).
    for host in ["OpaqueRef:host-0001", H2] {
        server.set_field(
            "host",
            host,
            "license_params",
            json!({"restrict_pool_secret_rotation": "false"}),
        );
        server.set_field(
            "host",
            host,
            "software_version",
            json!({"product_brand": "XCP-ng", "product_version": "8.3.0",
                   "platform_version": "3.4.0", "xapi": "25.6"}),
        );
    }
    let hs = ha_strings();
    let o = crate::ops_strings::ops_strings();
    let (admin, admin_password) = xapi_mock::AD_ADMIN;
    select_pool(&mut h);
    tab(&mut h, hs.tab_users);
    assert!(shows(
        &h,
        &fill(hs.ad_not_configured_blurb, &[("pool", "Lab Pool")])
    ));
    assert!(shows(&h, hs.ad_disabled));

    // Join Domain: OK only once every field is filled.
    press(&mut h, hs.join_domain);
    assert!(shows(&h, hs.join_blurb_short));
    assert!(!last_enabled(&h, hs.ok));
    type_into(&mut h, hs.domain, xapi_mock::AD_DOMAIN);
    type_into(&mut h, hs.username, admin);
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .type_text(admin_password);
    settle(&mut h);
    press(&mut h, hs.ok);
    wait_ok(
        &mut h,
        "Enabling Active Directory Authentication on pool 'Lab Pool'",
    );
    step_until(&mut h, "joined", |h| {
        shows(
            h,
            &fill(
                hs.ad_configured_blurb,
                &[("pool", "Lab Pool"), ("domain", xapi_mock::AD_DOMAIN)],
            ),
        )
    });
    assert!(shows(&h, hs.local_root));
    // The root row cannot be changed.
    assert!(!last_enabled(&h, hs.remove));
    assert!(!last_enabled(&h, hs.change_role));

    // Add users: one resolves, one does not.
    press(&mut h, hs.add);
    assert!(shows(&h, hs.add_users_title));
    type_into(&mut h, hs.names, "LAB\\alice; nobody");
    press(&mut h, hs.grant_access);
    step_until(&mut h, "names resolved", |h| {
        shows(h, &fill(hs.resolved_as, &[("name", "LAB\\alice")]))
            && shows(h, hs.added_to_list)
            && shows(h, hs.could_not_resolve)
            && shows(h, hs.completed_with_errors)
    });
    press(&mut h, hs.close);
    step_until(&mut h, "alice listed", |h| shows(h, "Alice Operator"));
    assert!(shows(&h, "Pool Admin"), "new subjects are pool admins");

    // Change Role: VM Operator.
    press(&mut h, "Alice Operator");
    press(&mut h, hs.change_role);
    assert!(shows(
        &h,
        &fill(hs.select_role_user, &[("name", "Alice Operator")])
    ));
    assert!(!last_enabled(&h, hs.roles_save), "nothing changed yet");
    press(&mut h, "VM Operator");
    // Taking pool-admin away shows the exit advice.
    assert!(shows(&h, hs.roles_admin_advice));
    press(&mut h, hs.roles_save);
    wait_ok(&mut h, "Assigning new roles to subject 'Alice Operator'");
    assert_eq!(
        server.field("subject", &server.refs("subject")[0], "roles"),
        Some(json!(["OpaqueRef:role-vm-operator"]))
    );

    // alice logs in elsewhere: Logged In says so; Log Out ends it.
    let alice = raw_login(&runtime, &server, "LAB\\alice", "alice-password").unwrap();
    step_until(&mut h, "alice logged in", |h| {
        h.query_all_by_label(o.yes).next().is_some()
    });
    press(&mut h, "Alice Operator");
    assert!(last_enabled(&h, hs.logout_user));
    press(&mut h, hs.logout_user);
    assert!(shows(
        &h,
        &fill(hs.logout_user_one, &[("name", "Alice Operator")])
    ));
    press(&mut h, o.yes);
    wait_ok(&mut h, "Terminating session for user 'Alice Operator'");
    let e = runtime
        .block_on(xapi::session::get_all_subject_identifiers(&alice))
        .unwrap_err();
    assert!(e.is_api("SESSION_INVALID"), "{e:?}");

    // Remove her access.
    press(&mut h, "Alice Operator");
    press(&mut h, hs.remove);
    assert!(shows(
        &h,
        &fill(hs.remove_user_one, &[("name", "Alice Operator")])
    ));
    press(&mut h, o.yes);
    wait_ok(
        &mut h,
        "Adding/removing users from Active Directory ACL on 'Lab Pool'",
    );
    assert!(server.refs("subject").is_empty());

    // Leave the domain (as root: the plain question), without an account.
    press(&mut h, hs.leave_domain);
    assert!(shows(
        &h,
        &fill(
            hs.leave_confirm,
            &[("pool", "Lab Pool"), ("domain", xapi_mock::AD_DOMAIN)]
        )
    ));
    press(&mut h, o.yes);
    assert!(shows(&h, hs.leaving_ad));
    press(&mut h, hs.ignore);
    wait_ok(
        &mut h,
        "Disabling Active Directory Authentication on pool 'Lab Pool'",
    );
    assert_eq!(
        field(&server, "host", "OpaqueRef:host-0001", "external_auth_type"),
        json!("")
    );
}

/// Joins the mock to the domain and gives `LAB\alice` the VM Operator
/// role, outside the window.
fn domain_with_vm_operator(runtime: &tokio::runtime::Runtime, server: &MockServer) {
    let root = raw_login(runtime, server, xapi_mock::USERNAME, xapi_mock::PASSWORD).unwrap();
    let (admin, password) = xapi_mock::AD_ADMIN;
    runtime.block_on(async {
        let config = std::collections::HashMap::from([
            ("domain".to_owned(), xapi_mock::AD_DOMAIN.to_owned()),
            ("user".to_owned(), admin.to_owned()),
            ("pass".to_owned(), password.to_owned()),
        ]);
        xapi::pool::enable_external_auth(
            &root,
            &Ref::new(POOL),
            &config,
            xapi_mock::AD_DOMAIN,
            "AD",
        )
        .await
        .unwrap();
        let sid = xapi::auth::get_subject_identifier(&root, "LAB\\alice")
            .await
            .unwrap();
        let info = xapi::auth::get_subject_information_from_identifier(&root, &sid)
            .await
            .unwrap();
        let subject = xapi::subject::create(
            &root,
            &xapi::SubjectRecord {
                subject_identifier: sid,
                other_config: info,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        xapi::subject::remove_from_roles(&root, &subject, &Ref::new("OpaqueRef:role-pool-admin"))
            .await
            .unwrap();
        xapi::subject::add_to_roles(&root, &subject, &Ref::new("OpaqueRef:role-vm-operator"))
            .await
            .unwrap();
    });
}

#[test]
fn a_domain_vm_operator_gets_role_elevation() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    server.set_field("host", H2, "enabled", json!(true));
    server.set_field("host", H2, "other_config", json!({}));
    domain_with_vm_operator(&runtime, &server);
    let mut h = app_harness(&runtime);
    h.state_mut().connect.username = "LAB\\alice".into();
    connect_through_dialog_as(&mut h, &server.address(), "alice-password");
    step_until(&mut h, "tree", |h| {
        shows(h, "Lab Pool") && first_conn(h).is_some_and(|c| c.rbac.is_some())
    });
    settle(&mut h);
    let o = crate::ops_strings::ops_strings();
    let hs = ha_strings();
    let rights = first_conn(&h).unwrap().rbac.clone().unwrap();
    assert!(!rights.is_local_superuser);
    assert_eq!(rights.user_sid.as_deref(), Some("S-1-5-21-1000-1101"));
    assert_eq!(
        i18n::session_roles_text(&first_conn(&h).unwrap()),
        "VM Operator"
    );

    // A forbidden VM operation: the elevation dialog, not a raw error.
    right_click(&mut h, "web-01");
    click_last(&mut h, o.migrate_to_server);
    step_until(&mut h, "server offered", |h| {
        rightmost_enabled(h, "xcp-host-02")
    });
    click_rightmost(&mut h, "xcp-host-02");
    step_until(&mut h, "confirmation", |h| {
        h.query_by_label("Do you want to migrate the VM web-01 to the server xcp-host-02?")
            .is_some()
    });
    click_last(&mut h, o.yes);
    step_until(&mut h, "elevation dialog", |h| {
        h.query_by_label(o.elevation_blurb).is_some()
    });
    h.get_by_label("VM Operator");
    h.get_by_label("Pool Admin, Pool Operator, VM Power Admin");
    assert!(!server.calls().iter().any(|c| c == "Async.VM.pool_migrate"));
    press(&mut h, strings().cancel);
    assert!(h.state().ops.elevation.is_none());

    // Disable HA is forbidden too: the server never sees the call.
    server.set_field("pool", POOL, "ha_enabled", json!(true));
    server.set_field(
        "pool",
        POOL,
        "ha_statefiles",
        json!(["OpaqueRef:vdi-db-root"]),
    );
    step_until(&mut h, "HA seen", |h| {
        first_conn(h).is_some_and(|c| c.model.pool().is_some_and(|(_, p)| p.ha_enabled))
    });
    let c = conn_id(&h);
    h.state_mut()
        .ops
        .dialog_requests
        .push((c, DialogRequest::DisableHa));
    settle(&mut h);
    press(&mut h, o.yes);
    step_until(&mut h, "elevation for Disable HA", |h| {
        h.query_by_label(o.elevation_blurb).is_some()
    });
    h.get_by_label("Pool Admin, Pool Operator");
    // Authorized as a Pool Operator, it runs.
    h.get_by_label(o.elevation_username)
        .type_text("pool-operator-user");
    h.get_by_role_and_label(Role::PasswordInput, o.elevation_password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, o.elevation_password)
        .type_text("pool-operator-password");
    h.step();
    h.get_by_label(o.elevation_authorize).click();
    wait_ok(&mut h, "Disabling HA on pool 'Lab Pool'");
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(false));

    // The HA wizard's RBAC page blocks a VM Operator, as the original's.
    h.state_mut()
        .ops
        .dialog_requests
        .push((c, DialogRequest::ConfigureHa));
    settle(&mut h);
    if shows(&h, hs.ok) && h.query_by_label(hs.intro_heading).is_none() {
        press(&mut h, hs.ok); // the broken SR warning
    }
    press(&mut h, crate::net_strings::net_strings().next);
    assert!(shows(
        &h,
        &fill(hs.rbac_enable_warning, &[("role", "VM Operator")])
    ));
    assert!(!last_enabled(&h, crate::net_strings::net_strings().next));
    press(&mut h, crate::net_strings::net_strings().cancel);

    // Adding users from the Users tab: elevation too.
    select_pool(&mut h);
    tab(&mut h, hs.tab_users);
    let creates = || {
        server
            .calls()
            .iter()
            .filter(|c| *c == "subject.create")
            .count()
    };
    let before = creates();
    press(&mut h, hs.add);
    type_into(&mut h, hs.names, "carol");
    press(&mut h, hs.grant_access);
    step_until(&mut h, "elevation for Add Users", |h| {
        h.query_by_label(o.elevation_blurb).is_some()
    });
    h.get_by_label("Pool Admin");
    assert_eq!(creates(), before);
}

#[test]
fn add_server_asks_for_the_domain_account() {
    let runtime = runtime();
    let a = runtime.block_on(MockServer::start_lab(false, "ua", 7));
    let b = runtime.block_on(MockServer::start_lab(false, "ub", 8));
    a.set_task_timing(2, Duration::from_millis(20));
    b.set_task_timing(2, Duration::from_millis(20));
    // a: a named pool joined to the domain.
    let root = raw_login(&runtime, &a, xapi_mock::USERNAME, xapi_mock::PASSWORD).unwrap();
    let (admin, password) = xapi_mock::AD_ADMIN;
    runtime.block_on(async {
        let pool = Ref::new("OpaqueRef:pool-0001-ua");
        xapi::pool::set_name_label(&root, &pool, "AD pool")
            .await
            .unwrap();
        let config = std::collections::HashMap::from([
            ("user".to_owned(), admin.to_owned()),
            ("pass".to_owned(), password.to_owned()),
        ]);
        xapi::pool::enable_external_auth(&root, &pool, &config, xapi_mock::AD_DOMAIN, "AD")
            .await
            .unwrap();
    });
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &a.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "a connected", |h| shows(h, "AD pool"));
    h.state().backend.send(Command::Connect {
        server: b.address(),
        username: xapi_mock::USERNAME.into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    step_until(&mut h, "b connected", |h| shows(h, "lab-host-ub"));
    let state = h.state().backend.state();
    let ca = state.connection_to(&a.address()).unwrap().id;
    let cb = state.connection_to(&b.address()).unwrap().id;
    h.state_mut().ops.dialog_requests.push((
        ca,
        DialogRequest::AddToPool {
            pool: ca,
            hosts: vec![cb],
            confirm: false,
        },
    ));
    settle(&mut h);
    let hs = ha_strings();
    // Any join warnings first, then the account of the pool's domain.
    let prompt = fill(hs.joining_ad, &[("domain", xapi_mock::AD_DOMAIN)]);
    for _ in 0..4 {
        if shows(&h, &prompt) {
            break;
        }
        press(&mut h, crate::pool_strings::pool_strings().yes);
    }
    assert!(shows(&h, &prompt));
    // A refused account: asked again with the reason.
    type_into(&mut h, hs.username, admin);
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .type_text("wrong");
    settle(&mut h);
    press(&mut h, hs.ok);
    step_until(&mut h, "asked again", |h| {
        shows(
            h,
            "Failed to enable external authentication, the supplied credentials were invalid.",
        ) && shows(h, &prompt)
    });
    assert!(!b.is_pool_member());
    type_into(&mut h, hs.username, admin);
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .focus();
    h.step();
    h.get_by_role_and_label(Role::PasswordInput, hs.password)
        .type_text(password);
    settle(&mut h);
    press(&mut h, hs.ok);
    step_until(&mut h, "b joined", |_| b.is_pool_member());
    let calls = b.calls();
    assert!(
        calls.iter().position(|c| c == "pool.enable_external_auth")
            < calls.iter().rposition(|c| c == "Async.pool.join")
    );
}
