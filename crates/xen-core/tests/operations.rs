//! F4: every VM and host operation against the mock (backend only, no UI):
//! the XAPI calls made, the state reflected through `event.from`, the
//! history entries, failures, prechecks, and RBAC (the session's rights,
//! a server-side denial and role elevation).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{first, harness, wait_until};
use serde_json::json;
use xapi::{Host, Ref, Vm, VmPowerState};
use xapi_mock::MockServer;
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, AppState, Command, ConnectionId, ElevationOutcome, HostAction, Notification,
    Operation, OperationKind, OperationObject, OperationStatus, Precheck, PrecheckResult, VmAction,
};

const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";

fn vm(r: &str) -> Ref<Vm> {
    Ref::new(r)
}

fn host(r: &str) -> Ref<Host> {
    Ref::new(r)
}

fn power(state: &AppState, r: &str) -> VmPowerState {
    first(state).model.vms[&vm(r)].power_state.clone()
}

fn resident(state: &AppState, r: &str) -> String {
    first(state).model.vms[&vm(r)].resident_on.to_string()
}

async fn connect(
    h: &common::Harness,
    server: &MockServer,
    user: &str,
    password: &str,
) -> ConnectionId {
    h.backend.send(Command::Connect {
        server: server.address(),
        username: user.into(),
        password: Some(password.into()),
        remember_password: false,
    });
    let state = h
        .wait_conn("connected", |c| c.status.is_connected() && c.rbac.is_some())
        .await;
    first(&state).id
}

/// Sends `request` and waits for its history entry to end.
async fn run(h: &common::Harness, conn: ConnectionId, request: ActionRequest) -> Operation {
    let before = h
        .backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0);
    h.backend.send(match request {
        ActionRequest::Vm { vm, action } => Command::Vm {
            connection: conn,
            vm,
            action,
        },
        ActionRequest::Host { host, action } => Command::Host {
            connection: conn,
            host,
            action,
        },
    });
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running() && o.object.is_some())
        })
        .await;
    let op = state
        .operations
        .iter()
        .find(|o| o.id > before && o.object.is_some())
        .unwrap();
    (**op).clone()
}

async fn vm_op(h: &common::Harness, conn: ConnectionId, r: &str, action: VmAction) -> Operation {
    let op = run(
        h,
        conn,
        ActionRequest::Vm {
            vm: vm(r),
            action: action.clone(),
        },
    )
    .await;
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{action:?}: {:?}",
        op.status
    );
    op
}

async fn host_op(
    h: &common::Harness,
    conn: ConnectionId,
    r: &str,
    action: HostAction,
) -> Operation {
    let op = run(
        h,
        conn,
        ActionRequest::Host {
            host: host(r),
            action,
        },
    )
    .await;
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{action:?}: {:?}",
        op.status
    );
    op
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_vm_operation_runs_as_a_task_and_shows_in_the_cache() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    assert!(
        first(&h.backend.state())
            .rbac
            .as_ref()
            .unwrap()
            .is_local_superuser
    );

    // web-01 runs on host 1. Pause, unpause, suspend, resume, reboot.
    let steps = [
        (VmAction::Pause, VmPowerState::Paused, "Async.VM.pause"),
        (VmAction::Unpause, VmPowerState::Running, "Async.VM.unpause"),
        (
            VmAction::Suspend,
            VmPowerState::Suspended,
            "Async.VM.suspend",
        ),
        (VmAction::Resume, VmPowerState::Running, "Async.VM.resume"),
        (
            VmAction::CleanReboot,
            VmPowerState::Running,
            "Async.VM.clean_reboot",
        ),
        (
            VmAction::HardReboot,
            VmPowerState::Running,
            "Async.VM.hard_reboot",
        ),
    ];
    for (action, expected, call) in steps {
        let op = vm_op(&h, conn, WEB, action.clone()).await;
        assert_eq!(op.object, Some(OperationObject::Vm(vm(WEB))));
        assert_eq!(op.progress, Some(1.0));
        assert!(server.calls().iter().any(|c| c == call), "{call}");
        let state = h
            .wait_for("power state", |s| power(s, WEB) == expected)
            .await;
        // Operations allowed again once it ended.
        assert!(!first(&state).busy_vms.contains(&vm(WEB)));
    }
    let titles = h.backend.state();
    let kinds: Vec<&OperationKind> = titles.operations.iter().map(|o| &o.kind).collect();
    assert!(kinds.contains(&&OperationKind::VmAction {
        vm_name: "web-01".into(),
        action: VmAction::CleanReboot,
        location: "xcp-host-01".into(),
        from: Some("xcp-host-01".into()),
        target: None,
    }));

    // Host 2 out of maintenance mode, then live migration to it.
    host_op(&h, conn, H2, HostAction::ExitMaintenance).await;
    let op = vm_op(&h, conn, WEB, VmAction::Migrate(host(H2))).await;
    assert_eq!(
        op.kind,
        OperationKind::VmAction {
            vm_name: "web-01".into(),
            action: VmAction::Migrate(host(H2)),
            location: "xcp-host-01".into(),
            from: Some("xcp-host-01".into()),
            target: Some("xcp-host-02".into()),
        }
    );
    h.wait_for("migrated", |s| resident(s, WEB) == H2).await;
    assert!(server.calls().iter().any(|c| c == "Async.VM.pool_migrate"));

    // Suspend, then resume on host 1.
    vm_op(&h, conn, WEB, VmAction::Suspend).await;
    vm_op(&h, conn, WEB, VmAction::ResumeOn(host(H1))).await;
    h.wait_for("resumed on host 1", |s| {
        power(s, WEB) == VmPowerState::Running && resident(s, WEB) == H1
    })
    .await;

    // Force shut down, start on host 2, clean shut down, start.
    vm_op(&h, conn, WEB, VmAction::HardShutdown).await;
    h.wait_for("halted", |s| power(s, WEB) == VmPowerState::Halted)
        .await;
    vm_op(&h, conn, WEB, VmAction::StartOn(host(H2))).await;
    h.wait_for("running on host 2", |s| {
        power(s, WEB) == VmPowerState::Running && resident(s, WEB) == H2
    })
    .await;
    vm_op(&h, conn, WEB, VmAction::CleanShutdown).await;
    vm_op(&h, conn, WEB, VmAction::Start).await;
    h.wait_for("running", |s| power(s, WEB) == VmPowerState::Running)
        .await;
    for call in [
        "Async.VM.resume_on",
        "Async.VM.hard_shutdown",
        "Async.VM.start_on",
        "Async.VM.clean_shutdown",
        "Async.VM.start",
    ] {
        assert!(server.calls().iter().any(|c| c == call), "{call}");
    }
    // Every task was destroyed.
    wait_until("tasks destroyed", || server.tasks().is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_shut_down_cancels_what_still_runs_on_the_vm() {
    let server = MockServer::start(false).await;
    server.set_task_timing(200, Duration::from_millis(50));
    let h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    h.backend.send(Command::Vm {
        connection: conn,
        vm: vm(WEB),
        action: VmAction::CleanShutdown,
    });
    let state = h
        .wait_for("clean shutdown running", |s| {
            s.operations.iter().any(|o| o.can_cancel())
                && first(s).model.vms[&vm(WEB)]
                    .current_operations
                    .values()
                    .any(|op| *op == xapi::VmOperations::CleanShutdown)
        })
        .await;
    let clean = state.operations.iter().find(|o| o.can_cancel()).unwrap().id;
    // The forced operation itself is quick.
    server.set_task_timing(4, Duration::from_millis(50));
    // Busy: only the forced operations are offered now.
    let model = &first(&state).model;
    let rec = &model.vms[&vm(WEB)];
    assert!(!xen_core::actions::vm_action_available(
        model,
        rec,
        &VmAction::CleanReboot,
        true
    ));
    assert!(xen_core::actions::vm_action_available(
        model,
        rec,
        &VmAction::HardShutdown,
        true
    ));

    let op = vm_op(&h, conn, WEB, VmAction::HardShutdown).await;
    assert_eq!(
        op.kind,
        OperationKind::VmAction {
            vm_name: "web-01".into(),
            action: VmAction::HardShutdown,
            location: "xcp-host-01".into(),
            from: Some("xcp-host-01".into()),
            target: None,
        }
    );
    h.wait_for("first one cancelled", |s| {
        s.operation(clean)
            .is_some_and(|o| matches!(o.status, OperationStatus::Cancelled))
    })
    .await;
    h.wait_for("halted", |s| power(s, WEB) == VmPowerState::Halted)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_operations_follow_the_original_sequence() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;

    // Exit maintenance mode: flag removed, host enabled.
    host_op(&h, conn, H2, HostAction::ExitMaintenance).await;
    let state = h
        .wait_for("host 2 enabled", |s| {
            first(s).model.hosts[&host(H2)].enabled
        })
        .await;
    assert!(
        !first(&state).model.hosts[&host(H2)]
            .other_config
            .contains_key("MAINTENANCE_MODE")
    );

    // A VM on host 2, then maintenance mode: it is migrated to host 1.
    vm_op(&h, conn, DB, VmAction::StartOn(host(H2))).await;
    h.wait_for("db on host 2", |s| resident(s, DB) == H2).await;
    let op = host_op(&h, conn, H2, HostAction::EnterMaintenance).await;
    assert_eq!(
        op.kind,
        OperationKind::HostAction {
            host_name: "xcp-host-02".into(),
            action: HostAction::EnterMaintenance
        }
    );
    let state = h
        .wait_for("evacuated", |s| {
            resident(s, DB) == H1 && !first(s).model.hosts[&host(H2)].enabled
        })
        .await;
    assert_eq!(
        first(&state).model.hosts[&host(H2)]
            .other_config
            .get("MAINTENANCE_MODE")
            .map(String::as_str),
        Some("true")
    );
    for call in [
        "Async.host.disable",
        "host.add_to_other_config",
        "Async.host.evacuate",
    ] {
        assert!(server.calls().iter().any(|c| c == call), "{call}");
    }

    // Shut host 2 down (no guests left), then power it on again.
    host_op(&h, conn, H2, HostAction::Shutdown).await;
    let state = h.wait_for("host 2 off", |_| !server.host_live(H2)).await;
    let _ = state;
    let state = h
        .wait_for("power on offered", |s| {
            let c = first(s);
            let rec = &c.model.hosts[&host(H2)];
            xen_core::actions::host_action_available(&c.model, rec, HostAction::PowerOn, false)
        })
        .await;
    assert!(!xen_core::actions::host_action_available(
        &first(&state).model,
        &first(&state).model.hosts[&host(H2)],
        HostAction::Reboot,
        false
    ));
    host_op(&h, conn, H2, HostAction::PowerOn).await;
    h.wait_for("host 2 on and enabled", |s| {
        let c = first(s);
        let rec = &c.model.hosts[&host(H2)];
        xen_core::objects::host_is_live(&c.model, rec) && rec.enabled
    })
    .await;

    // Reboot host 2 with a guest on it: the guest is shut down first.
    vm_op(&h, conn, DB, VmAction::Migrate(host(H2))).await;
    h.wait_for("db on host 2", |s| resident(s, DB) == H2).await;
    let calls_before = server.calls().len();
    host_op(&h, conn, H2, HostAction::Reboot).await;
    let calls: Vec<String> = server.calls()[calls_before..].to_vec();
    let order: Vec<&str> = calls
        .iter()
        .map(String::as_str)
        .filter(|c| {
            [
                "host.disable",
                "Async.VM.clean_shutdown",
                "Async.host.reboot",
            ]
            .contains(c)
        })
        .collect();
    assert_eq!(
        order,
        [
            "host.disable",
            "Async.VM.clean_shutdown",
            "Async.host.reboot"
        ]
    );
    h.wait_for("db halted, host 2 back", |s| {
        power(s, DB) == VmPowerState::Halted && first(s).model.hosts[&host(H2)].enabled
    })
    .await;

    // Restart the coordinator's toolstack: the connection comes back.
    let logins = server.call_count("session.login_with_password");
    let op = host_op(&h, conn, H1, HostAction::RestartToolstack).await;
    assert_eq!(
        op.kind,
        OperationKind::HostAction {
            host_name: "xcp-host-01".into(),
            action: HostAction::RestartToolstack
        }
    );
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "Async.host.restart_agent")
    );
    wait_until("logged in again", || {
        server.call_count("session.login_with_password") > logins
    })
    .await;
    h.wait_conn("connected again", |c| c.status.is_connected())
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_are_reported_and_hosts_put_back() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;

    // A failing VM task: the error code reaches the history and the UI.
    server.fail_method("VM.clean_reboot", Some(("VM_MISSING_PV_DRIVERS", &[WEB])));
    h.backend.send(Command::Vm {
        connection: conn,
        vm: vm(WEB),
        action: VmAction::CleanReboot,
    });
    match h.next_notification().await {
        Notification::ActionFailed { kind, error, .. } => {
            assert!(matches!(
                kind,
                OperationKind::VmAction {
                    action: VmAction::CleanReboot,
                    ..
                }
            ));
            assert!(error.is_api("VM_MISSING_PV_DRIVERS"), "{error:?}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    // The VM is usable again.
    h.wait_for("web free again", |s| {
        let c = first(s);
        let rec = &c.model.vms[&vm(WEB)];
        xen_core::actions::vm_action_available(&c.model, rec, &VmAction::CleanReboot, false)
    })
    .await;

    // A wrong power state, refused by the server.
    let op = run(
        &h,
        conn,
        ActionRequest::Vm {
            vm: vm(DB),
            action: VmAction::Unpause,
        },
    )
    .await;
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            assert!(e.is_api("VM_BAD_POWER_STATE"), "{e:?}");
        }
        other => panic!("{other:?}"),
    }
    let _ = h.next_notification().await;

    // A host reboot that fails after disabling the host enables it again.
    server.fail_method("host.reboot", Some(("HOST_IN_USE", &[H1, "VM", WEB])));
    let op = run(
        &h,
        conn,
        ActionRequest::Host {
            host: host(H1),
            action: HostAction::Reboot,
        },
    )
    .await;
    assert!(matches!(op.status, OperationStatus::Failed(_)));
    let _ = h.next_notification().await;
    let calls = server.calls();
    assert!(calls.iter().any(|c| c == "host.disable"));
    assert!(calls.iter().any(|c| c == "host.enable"));
    h.wait_for("host 1 enabled again", |s| {
        first(s).model.hosts[&host(H1)].enabled
    })
    .await;
    // Its guest was shut down first.
    h.wait_for("web halted", |s| power(s, WEB) == VmPowerState::Halted)
        .await;

    // Maintenance mode refused by the server: the host is enabled again
    // and the flag removed.
    server.fail_method("host.evacuate", Some(("NO_HOSTS_AVAILABLE", &[])));
    let op = run(
        &h,
        conn,
        ActionRequest::Host {
            host: host(H1),
            action: HostAction::EnterMaintenance,
        },
    )
    .await;
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            assert!(e.is_api("NO_HOSTS_AVAILABLE"), "{e:?}");
        }
        other => panic!("{other:?}"),
    }
    h.wait_for("host 1 back", |s| {
        let rec = &first(s).model.hosts[&host(H1)];
        rec.enabled && !rec.other_config.contains_key("MAINTENANCE_MODE")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prechecks_ask_the_server() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;

    h.backend.send(Command::Precheck {
        connection: conn,
        id: 7,
        check: Precheck::BootHosts {
            vm: vm(DB),
            hosts: vec![host(H1), host(H2)],
        },
    });
    match h.next_notification().await {
        Notification::Precheck {
            id: 7,
            result: PrecheckResult::BootHosts(hosts),
            ..
        } => {
            assert!(hosts[0].1.is_none());
            assert!(hosts[1].1.as_ref().unwrap().is_api("HOST_DISABLED"));
        }
        other => panic!("{other:?}"),
    }

    // web-01 cannot be migrated off host 1 (no other enabled host).
    h.backend.send(Command::Precheck {
        connection: conn,
        id: 8,
        check: Precheck::Evacuation { host: host(H1) },
    });
    match h.next_notification().await {
        Notification::Precheck {
            id: 8,
            result: PrecheckResult::Evacuation(Ok(blocked)),
            ..
        } => {
            assert_eq!(
                blocked,
                vec![(vm(WEB), vec!["NO_HOSTS_AVAILABLE".to_owned()])]
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "VM.assert_can_boot_here")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rbac_denial_and_role_elevation() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server, "vm-operator-user", "vm-operator-password").await;
    let state = h.backend.state();
    let view = first(&state);
    let rights = view.rbac.as_ref().unwrap();
    assert!(!rights.is_local_superuser);
    assert!(rights.permissions.contains("vm.clean_reboot"));
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "session.get_rbac_permissions")
    );

    // Rebooting is allowed; migrating needs VM Power Admin or more.
    let reboot = ActionRequest::Vm {
        vm: vm(WEB),
        action: VmAction::CleanReboot,
    };
    assert_eq!(
        rbac::check(&view.model, Some(rights), &reboot.rbac_methods()),
        RbacCheck::Allowed
    );
    let migrate = ActionRequest::Vm {
        vm: vm(WEB),
        action: VmAction::Migrate(host(H2)),
    };
    let RbacCheck::Denied { authorized } =
        rbac::check(&view.model, Some(rights), &migrate.rbac_methods())
    else {
        panic!("migration must be denied");
    };
    let names: Vec<&str> = authorized.iter().map(|r| r.name_label.as_str()).collect();
    assert_eq!(names, ["pool-admin", "pool-operator", "vm-power-admin"]);
    let roles = rbac::session_roles(&view.model, rights);
    assert_eq!(roles[0].name_label, "vm-operator");

    // Without the client-side check, the server refuses.
    vm_op(&h, conn, WEB, VmAction::CleanReboot).await;
    server.set_field("host", H2, "enabled", json!(true));
    h.wait_for("host 2 enabled", |s| {
        first(s).model.hosts[&host(H2)].enabled
    })
    .await;
    h.backend.send(Command::Vm {
        connection: conn,
        vm: vm(WEB),
        action: VmAction::Migrate(host(H2)),
    });
    match h.next_notification().await {
        Notification::ActionFailed { error, .. } => {
            assert!(error.is_api("RBAC_PERMISSION_DENIED"), "{error:?}");
        }
        other => panic!("{other:?}"),
    }

    // Elevation: a wrong password, a user without the role, then one with it.
    let elevate = |user: &str, password: &str, id: u64| Command::RunElevated {
        connection: conn,
        request: migrate.clone(),
        username: user.into(),
        password: password.into(),
        request_id: id,
    };
    h.backend.send(elevate("vm-power-admin-user", "wrong", 1));
    match h.next_notification().await {
        Notification::Elevation {
            request_id: 1,
            outcome: ElevationOutcome::LoginFailed(e),
        } => {
            assert!(e.is_api("SESSION_AUTHENTICATION_FAILED"));
        }
        other => panic!("{other:?}"),
    }
    h.backend
        .send(elevate("read-only-user", "read-only-password", 2));
    match h.next_notification().await {
        Notification::Elevation {
            request_id: 2,
            outcome: ElevationOutcome::NotAuthorized,
        } => {}
        other => panic!("{other:?}"),
    }
    wait_until("refused session logged out", || {
        server.active_sessions() == 1
    })
    .await;
    h.backend
        .send(elevate("vm-power-admin-user", "vm-power-admin-password", 3));
    let op = match h.next_notification().await {
        Notification::Elevation {
            request_id: 3,
            outcome: ElevationOutcome::Authorized { operation },
        } => operation,
        other => panic!("{other:?}"),
    };
    let state = h
        .wait_for("migrated", |s| {
            s.operation(op)
                .is_some_and(|o| matches!(o.status, OperationStatus::Succeeded))
                && resident(s, WEB) == H2
        })
        .await;
    assert_eq!(
        state.operation(op).unwrap().elevated_as.as_deref(),
        Some("vm-power-admin-user")
    );
    // The elevated session is logged out once the operation ends.
    wait_until("elevated session logged out", || {
        server.active_sessions() == 1
    })
    .await;
    let _ = Arc::clone(&state);
}
