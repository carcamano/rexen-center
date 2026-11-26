//! F10b against the mock (backend only, no UI): HA turned on and off on
//! the fixture's 2-host pool (heartbeat SR scan, agility, the planner and
//! the HCI limit, priorities, order and delay, the ntol), the HA settings
//! edited, the HA blocks of other operations (a VM start that breaks the
//! plan, a server leaving, the ntol raised afterwards), HA failure paths;
//! a simulated Active Directory domain: join and leave, users and groups
//! added, roles changed, sessions of a restricted domain user logged out,
//! access removed; the AD join during Add Server; RBAC on the new
//! operations, with role elevation.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{first, harness, memory_store, spawn};
use serde_json::{Value, json};
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Ref, Session, TrustStore, Vm};
use xapi_mock::MockServer;
use xen_core::ad::{self, SubjectProgress};
use xen_core::ha::{self, RestartPriority, VmStartup};
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, BackendOptions, Command, ConnectionId, ElevationOutcome, HostAction,
    Notification, Operation, OperationStatus, Password, Precheck, PrecheckResult, VmAction,
};

const POOL: &str = "OpaqueRef:pool-0001";
const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";
const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const BUILD: &str = "OpaqueRef:vm-build";
const LEGACY: &str = "OpaqueRef:vm-legacy";
const NFS: &str = "OpaqueRef:sr-nfs";
const ISO: &str = "OpaqueRef:sr-iso";
const LOCAL1: &str = "OpaqueRef:sr-local-01";
const ALICE_SID: &str = "S-1-5-21-1000-1101";

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> Value {
    server.field(class, r, f).unwrap_or(Value::Null)
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
    let address = server.address();
    let state = h
        .wait_for("connected", |s| {
            s.connections
                .iter()
                .any(|c| c.server == address && c.status.is_connected() && c.rbac.is_some())
        })
        .await;
    state
        .connections
        .iter()
        .find(|c| c.server == address)
        .unwrap()
        .id
}

fn last_id(h: &common::Harness) -> u64 {
    h.backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0)
}

async fn ended_after(h: &common::Harness, before: u64) -> Operation {
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running())
        })
        .await;
    (**state
        .operations
        .iter()
        .find(|o| o.id > before && !o.is_running())
        .unwrap())
    .clone()
}

async fn run(h: &common::Harness, conn: ConnectionId, request: ActionRequest) -> Operation {
    let before = last_id(h);
    h.backend.send(Command::Action {
        connection: conn,
        request,
    });
    ended_after(h, before).await
}

fn ok(op: &Operation) {
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn failed_with(op: &Operation, code: &str) -> Arc<xapi::Error> {
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            assert!(e.is_api(code), "{:?}: expected {code}, got {e}", op.kind);
            e.clone()
        }
        other => panic!("{:?}: expected failure {code}, got {other:?}", op.kind),
    }
}

/// Asks `check` and returns its answer.
async fn precheck(h: &mut common::Harness, conn: ConnectionId, check: Precheck) -> PrecheckResult {
    static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    let id = ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    h.backend.send(Command::Precheck {
        connection: conn,
        id,
        check,
    });
    loop {
        if let Notification::Precheck {
            id: got, result, ..
        } = h.next_notification().await
            && got == id
        {
            return result;
        }
    }
}

/// The fixture pool with its second server out of maintenance mode.
async fn ha_pool() -> MockServer {
    let server = MockServer::start(false).await;
    server.set_field("host", H2, "enabled", json!(true));
    server.set_field("host", H2, "other_config", json!({}));
    server.set_task_timing(2, std::time::Duration::from_millis(20));
    server
}

fn startup(vm: &str, priority: RestartPriority, order: i64, delay: i64) -> VmStartup {
    VmStartup {
        vm: r(vm),
        priority,
        order,
        start_delay: delay,
    }
}

#[tokio::test]
async fn ha_turned_on_and_off_on_the_two_host_pool() {
    let server = ha_pool().await;
    let mut h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let state = h
        .wait_for("server 2 enabled", |s| first(s).model.hosts[&r(H2)].enabled)
        .await;
    let model = first(&state).model.clone();

    // Nothing to configure yet: the commands are offered, the wizard warns
    // about the broken ISO library (unplugged on server 2).
    assert_eq!(ha::configure_problem(&model, None), None);
    assert_eq!(ha::broken_shared_srs(&model), ["ISO library"]);

    // Heartbeat SRs: the shared ones but the tools SR; only NFS can hold
    // the statefile.
    let candidates = ha::heartbeat_candidates(&model);
    assert_eq!(candidates, [r(ISO), r(NFS)]);
    let PrecheckResult::HeartbeatSrs(srs) = precheck(
        &mut h,
        conn,
        Precheck::HeartbeatSrs {
            srs: candidates.clone(),
        },
    )
    .await
    else {
        panic!("heartbeat answer");
    };
    let why: BTreeMap<String, Option<String>> = srs
        .iter()
        .map(|(sr, e)| {
            (
                sr.to_string(),
                e.as_ref().map(|e| match &**e {
                    xapi::Error::Api { code, .. } => code.clone(),
                    other => other.to_string(),
                }),
            )
        })
        .collect();
    assert_eq!(why[NFS], None);
    assert_eq!(why[ISO].as_deref(), Some("SR_OPERATION_NOT_SUPPORTED"));
    // The scan is a plain check: no task.
    assert!(!server.calls().iter().any(|c| c.starts_with("Async.SR.")));

    // Agility: web-01 and legacy-app have local disks.
    let vms: Vec<Ref<Vm>> = ha::protectable_vms(&model, false)
        .into_iter()
        .map(|(r, _)| r.clone())
        .collect();
    let PrecheckResult::Agility(agility) =
        precheck(&mut h, conn, Precheck::Agility { vms: vms.clone() }).await
    else {
        panic!("agility answer");
    };
    let agile: BTreeMap<String, Option<String>> = agility
        .into_iter()
        .map(|(v, w)| (v.to_string(), w))
        .collect();
    assert_eq!(agile[DB], None);
    assert_eq!(agile[BUILD], None);
    assert_eq!(
        agile[WEB].as_deref(),
        Some("HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED")
    );
    assert_eq!(
        agile[LEGACY].as_deref(),
        Some("HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED")
    );
    // The first time, agile VMs are proposed for restart, the others for
    // best effort.
    assert!(ha::first_time(&ha::protectable_vms(&model, false)));

    // The planner: 2 servers, the protected running VMs fit on one.
    let restart: BTreeMap<Ref<Vm>, String> = BTreeMap::from([
        (r(DB), "restart".to_owned()),
        (r(BUILD), "restart".to_owned()),
    ]);
    let max = |result: PrecheckResult| match result {
        PrecheckResult::HaMaxFailures(Ok(n)) => n,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        max(precheck(
            &mut h,
            conn,
            Precheck::HaMaxFailures {
                restart: restart.clone()
            }
        )
        .await),
        1
    );
    // The pool's HCI limit wins when it is lower.
    server.set_field(
        "pool",
        POOL,
        "other_config",
        json!({ha::HCI_LIMIT_KEY: "0"}),
    );
    h.wait_for("limit seen", |s| {
        first(s)
            .model
            .pool()
            .is_some_and(|(_, p)| p.other_config.contains_key(ha::HCI_LIMIT_KEY))
    })
    .await;
    assert_eq!(
        max(precheck(
            &mut h,
            conn,
            Precheck::HaMaxFailures {
                restart: restart.clone()
            }
        )
        .await),
        0
    );
    server.set_field("pool", POOL, "other_config", json!({}));
    h.wait_for("limit gone", |s| {
        first(s)
            .model
            .pool()
            .is_some_and(|(_, p)| p.other_config.is_empty())
    })
    .await;

    // Configure HA: priorities, order and delay, ntol 1, NFS heartbeat.
    let calls_before = server.calls().len();
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(NFS),
            ntol: 1,
            startup: vec![
                startup(DB, RestartPriority::Restart, 1, 10),
                startup(BUILD, RestartPriority::Restart, 2, 0),
                startup(WEB, RestartPriority::BestEffort, 0, 0),
                startup(LEGACY, RestartPriority::DoNotRestart, 0, 0),
            ],
        },
    )
    .await;
    ok(&op);
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(true));
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(1)
    );
    assert_eq!(field(&server, "pool", POOL, "ha_plan_exists_for"), json!(1));
    assert_eq!(
        field(&server, "vm", DB, "ha_restart_priority"),
        json!("restart")
    );
    assert_eq!(field(&server, "vm", DB, "order"), json!(1));
    assert_eq!(field(&server, "vm", DB, "start_delay"), json!(10));
    assert_eq!(
        field(&server, "vm", WEB, "ha_restart_priority"),
        json!("best-effort")
    );
    assert_eq!(
        field(&server, "vm", LEGACY, "ha_restart_priority"),
        json!("")
    );
    // `EnableHAAction`'s order: priorities, ntol, then the task.
    let calls: Vec<String> = server.calls()[calls_before..].to_vec();
    let at = |m: &str| {
        calls
            .iter()
            .position(|c| c == m)
            .unwrap_or_else(|| panic!("{m}"))
    };
    assert!(at("VM.set_ha_restart_priority") < at("pool.set_ha_host_failures_to_tolerate"));
    assert!(at("pool.set_ha_host_failures_to_tolerate") < at("Async.pool.enable_ha"));
    // The tab's rows once the cache follows.
    let state = h
        .wait_for("HA seen on", |s| {
            first(s).model.pool().is_some_and(|(_, p)| p.ha_enabled)
                && first(s)
                    .model
                    .hosts
                    .values()
                    .all(|h| !h.ha_statefiles.is_empty())
        })
        .await;
    let model = first(&state).model.clone();
    let pool = model.pool().unwrap().1.clone();
    assert!(!ha::statefiles_missing(&model, &pool));
    assert_eq!(
        ha::heartbeat_srs(&model, &pool)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        ["NFS VM storage"]
    );
    let rows = ha::heartbeat_rows(&model, &pool, true);
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.network == ha::Heartbeat::Healthy && r.srs == [ha::Heartbeat::Healthy])
    );
    assert!(
        ha::heartbeat_rows(&model, &pool, false)
            .iter()
            .all(|r| r.network == ha::Heartbeat::Initializing)
    );
    assert!(!ha::overcommitted(&pool));
    // HA on blocks what the original blocks.
    assert_eq!(
        xen_core::pool::standalone_problem(&model),
        Some(xen_core::pool::StandaloneProblem::SeveralHosts)
    );

    // Edit the settings: db-01 stops being restarted, web stays.
    let op = run(
        &h,
        conn,
        ActionRequest::SetHaPriorities {
            startup: vec![startup(DB, RestartPriority::BestEffort, 3, 5)],
            ntol: 1,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        field(&server, "vm", DB, "ha_restart_priority"),
        json!("best-effort")
    );
    assert_eq!(field(&server, "vm", DB, "order"), json!(3));
    assert!(
        server
            .calls()
            .iter()
            .any(|c| c == "Async.pool.sync_database")
    );

    // A VM start that breaks the plan: build-runner needs more memory than
    // a single server has left.
    server.set_field("vm", BUILD, "memory_static_max", json!(20_i64 << 30));
    h.wait_for("build-runner grew", |s| {
        first(s).model.vms[&r(BUILD)].memory_static_max == 20 << 30
    })
    .await;
    let op = run(
        &h,
        conn,
        ActionRequest::Vm {
            vm: r(BUILD),
            action: VmAction::Start,
        },
    )
    .await;
    failed_with(&op, "HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN");
    // `StartDiagnosisForm`: plan 1, ntol 1 → would need 0: HA off instead.
    let state = h.backend.state();
    assert_eq!(
        xen_core::ha_run::vm_start_with_lower_ntol(
            &first(&state).model,
            &r(BUILD),
            &VmAction::Start
        ),
        None
    );
    // With an ntol of 0 accepted, it starts.
    let op = run(
        &h,
        conn,
        ActionRequest::HaVm {
            vm: r(BUILD),
            action: VmAction::Start,
            unprotect: false,
            ntol: Some(0),
        },
    )
    .await;
    ok(&op);
    assert_eq!(server.vm_power_state(BUILD), "Running");
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(0)
    );
    run(
        &h,
        conn,
        ActionRequest::Vm {
            vm: r(BUILD),
            action: VmAction::HardShutdown,
        },
    )
    .await;
    server.set_field("vm", BUILD, "memory_static_max", json!(2_i64 << 30));
    let op = run(&h, conn, ActionRequest::SetHaNtol { ntol: 1 }).await;
    ok(&op);

    // A server leaving: maintenance mode on server 2 needs ntol 0 first.
    let op = run(
        &h,
        conn,
        ActionRequest::Host {
            host: r(H2),
            action: HostAction::EnterMaintenance,
        },
    )
    .await;
    failed_with(&op, "HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN");
    let state = h.backend.state();
    let PrecheckResult::HaMaxFailures(Ok(max)) = precheck(
        &mut h,
        conn,
        Precheck::HaMaxFailures {
            restart: ha::current_priorities(&first(&state).model)
                .into_iter()
                .collect(),
        },
    )
    .await
    else {
        panic!("planner");
    };
    assert_eq!(ha::ntol_before_host_operation(1, max), Some(0));
    let op = run(
        &h,
        conn,
        ActionRequest::HaHost {
            host: r(H2),
            action: HostAction::EnterMaintenance,
            ntol: 0,
        },
    )
    .await;
    ok(&op);
    assert_eq!(field(&server, "host", H2, "enabled"), json!(false));
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(0)
    );
    // Back from maintenance: the console offers to raise the ntol again.
    h.wait_for("server 2 disabled", |s| {
        !first(s).model.hosts[&r(H2)].enabled
    })
    .await;
    h.backend.send(Command::Host {
        connection: conn,
        host: r(H2),
        action: HostAction::ExitMaintenance,
    });
    loop {
        match h.next_notification().await {
            Notification::HaNtolRaise {
                connection,
                host,
                current,
                max,
            } => {
                assert_eq!((connection, host, current, max), (conn, r(H2), 0, 1));
                break;
            }
            Notification::Precheck { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    let op = run(&h, conn, ActionRequest::SetHaNtol { ntol: 1 }).await;
    ok(&op);
    assert_eq!(
        field(&server, "pool", POOL, "ha_host_failures_to_tolerate"),
        json!(1)
    );

    // A protected VM that is not agile is unprotected before it starts.
    server.set_field("vm", LEGACY, "ha_restart_priority", json!("restart"));
    h.wait_for("legacy protected", |s| {
        ha::priority(&first(s).model.vms[&r(LEGACY)]).is_restart()
    })
    .await;
    let op = run(
        &h,
        conn,
        ActionRequest::HaVm {
            vm: r(LEGACY),
            action: VmAction::Resume,
            unprotect: true,
            ntol: None,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        field(&server, "vm", LEGACY, "ha_restart_priority"),
        json!("")
    );
    assert_eq!(server.vm_power_state(LEGACY), "Running");

    // Disable HA: statefiles gone.
    let statefile = field(&server, "pool", POOL, "ha_statefiles")[0]
        .as_str()
        .unwrap()
        .to_owned();
    let op = run(&h, conn, ActionRequest::DisableHa).await;
    ok(&op);
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(false));
    assert!(server.record("vdi", &statefile).is_none());
    assert_eq!(field(&server, "host", H1, "ha_statefiles"), json!([]));
    // Disabled again: the server refuses.
    let op = run(&h, conn, ActionRequest::DisableHa).await;
    failed_with(&op, "HA_NOT_ENABLED");
}

#[tokio::test]
async fn ha_failure_paths() {
    let server = ha_pool().await;
    let h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;

    // A local SR cannot hold the statefile: nothing is turned on.
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(LOCAL1),
            ntol: 0,
            startup: vec![],
        },
    )
    .await;
    failed_with(&op, "SR_NOT_SHARED");
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(false));

    // More failures than the plan allows: refused before enabling.
    server.fail_method(
        "pool.enable_ha",
        Some(("HA_HEARTBEAT_DAEMON_STARTUP_FAILED", &[])),
    );
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(NFS),
            ntol: 1,
            startup: vec![startup(DB, RestartPriority::Restart, 0, 0)],
        },
    )
    .await;
    failed_with(&op, "HA_HEARTBEAT_DAEMON_STARTUP_FAILED");
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(false));

    // `VDI_NOT_AVAILABLE` names the statefile disk by its UUID.
    server.fail_method(
        "pool.enable_ha",
        Some(("VDI_NOT_AVAILABLE", &["OpaqueRef:vdi-db-root"])),
    );
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(NFS),
            ntol: 0,
            startup: vec![],
        },
    )
    .await;
    let e = failed_with(&op, "VDI_NOT_AVAILABLE");
    match &*e {
        xapi::Error::Api { params, .. } => {
            assert_eq!(params, &["ad000000-0000-4000-8000-000000000003"])
        }
        other => panic!("{other:?}"),
    }
    server.fail_method("pool.enable_ha", None);

    // A restart priority on a VM that is not agile, with HA on: refused.
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(NFS),
            ntol: 1,
            startup: vec![],
        },
    )
    .await;
    ok(&op);
    let op = run(
        &h,
        conn,
        ActionRequest::SetHaPriorities {
            startup: vec![startup(WEB, RestartPriority::Restart, 0, 0)],
            ntol: 1,
        },
    )
    .await;
    failed_with(&op, "HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED");
    // More failures than the plan allows.
    let op = run(&h, conn, ActionRequest::SetHaNtol { ntol: 2 }).await;
    failed_with(&op, "HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN");
    // Enabling twice.
    let op = run(
        &h,
        conn,
        ActionRequest::EnableHa {
            heartbeat_sr: r(NFS),
            ntol: 1,
            startup: vec![],
        },
    )
    .await;
    failed_with(&op, "HA_IS_ENABLED");
    ok(&run(&h, conn, ActionRequest::DisableHa).await);
}

/// A session of `user` on `server`, outside the backend.
async fn login(server: &MockServer, user: &str, password: &str) -> xapi::Result<Session> {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = Arc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    Session::login_with_password(client, user, password, "f10b-tests").await
}

fn subject_by_name(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("subject")
        .into_iter()
        .find(|s| field(server, "subject", s, "other_config")["subject-name"] == json!(name))
}

fn role_ref(name: &str) -> Ref<xapi::Role> {
    r(&format!("OpaqueRef:role-{name}"))
}

#[tokio::test]
async fn ad_domain_users_roles_and_sessions() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let (admin, admin_password) = xapi_mock::AD_ADMIN;
    let state = h.backend.state();
    assert_eq!(
        ad::state(&first(&state).model, false),
        ad::AdState::Disabled
    );
    // Not joined: domain users cannot log in.
    assert!(
        login(&server, "LAB\\alice", "alice-password")
            .await
            .is_err()
    );

    // Join: wrong account, unknown domain, then the right one.
    let join = |domain: &str, password: &str| ActionRequest::EnableAd {
        domain: domain.into(),
        user: admin.into(),
        password: Password::new(password),
    };
    let op = run(&h, conn, join(xapi_mock::AD_DOMAIN, "wrong")).await;
    let e = failed_with(&op, "POOL_AUTH_ENABLE_FAILED_WRONG_CREDENTIALS");
    assert!(ad::is_credentials_failure(&e));
    let op = run(&h, conn, join("other.example", admin_password)).await;
    failed_with(&op, "POOL_AUTH_ENABLE_FAILED_DOMAIN_LOOKUP_FAILED");
    let op = run(&h, conn, join(xapi_mock::AD_DOMAIN, admin_password)).await;
    ok(&op);
    for host in [H1, H2] {
        assert_eq!(
            field(&server, "host", host, "external_auth_type"),
            json!("AD")
        );
        assert_eq!(
            field(&server, "host", host, "external_auth_service_name"),
            json!(xapi_mock::AD_DOMAIN)
        );
    }
    // `EnableAdAction`: any earlier configuration is dropped first.
    let calls = server.calls();
    let disable = calls
        .iter()
        .rposition(|c| c == "pool.disable_external_auth")
        .unwrap();
    let enable = calls
        .iter()
        .rposition(|c| c == "pool.enable_external_auth")
        .unwrap();
    assert!(disable < enable);
    h.wait_for("joined", |s| {
        matches!(
            ad::state(&first(s).model, false),
            ad::AdState::Enabled { .. }
        )
    })
    .await;

    // A user with no access cannot log in yet.
    let e = login(&server, "LAB\\alice", "alice-password")
        .await
        .unwrap_err();
    assert!(e.is_api("SESSION_AUTHORIZATION_FAILED"), "{e:?}");

    // Add users and a group; one name cannot be resolved.
    let op = run(
        &h,
        conn,
        ActionRequest::AddSubjects {
            names: ad::split_names("LAB\\alice; nobody, VM Admins, dave@lab.example"),
        },
    )
    .await;
    failed_with(&op, "SUBJECT_CANNOT_BE_RESOLVED");
    let progress: Vec<(&str, &SubjectProgress)> = op
        .subjects
        .iter()
        .map(|e| (e.name.as_str(), &e.progress))
        .collect();
    assert!(matches!(
        progress[0],
        ("LAB\\alice", SubjectProgress::Added { resolved }) if resolved == "LAB\\alice"
    ));
    assert!(matches!(
        progress[1],
        ("nobody", SubjectProgress::ResolveFailed { .. })
    ));
    assert!(matches!(
        progress[2],
        ("VM Admins", SubjectProgress::Added { .. })
    ));
    assert!(matches!(
        progress[3],
        ("dave@lab.example", SubjectProgress::Added { .. })
    ));
    let alice = subject_by_name(&server, "LAB\\alice").expect("alice added");
    let group = subject_by_name(&server, "LAB\\VM Admins").expect("group added");
    // New subjects are pool admins (the datamodel's default).
    assert_eq!(
        field(&server, "subject", &alice, "roles"),
        json!(["OpaqueRef:role-pool-admin"])
    );
    // Adding alice again: already has access.
    let op = run(
        &h,
        conn,
        ActionRequest::AddSubjects {
            names: vec!["alice".into()],
        },
    )
    .await;
    failed_with(&op, "SUBJECT_ALREADY_EXISTS");
    assert!(matches!(
        op.subjects[0].progress,
        SubjectProgress::AlreadyHasAccess { .. }
    ));

    // Change roles: alice VM Operator, the group VM Admin.
    let state = h
        .wait_for("subjects seen", |s| first(s).model.subjects.len() == 3)
        .await;
    let model = first(&state).model.clone();
    let selectable: Vec<String> = ad::selectable_roles(&model)
        .into_iter()
        .map(|(_, r)| r.name_label.clone())
        .collect();
    assert_eq!(
        selectable,
        [
            "pool-admin",
            "pool-operator",
            "vm-power-admin",
            "vm-admin",
            "vm-operator",
            "read-only"
        ]
    );
    let alice_ref: Ref<xapi::Subject> = r(&alice);
    let group_ref: Ref<xapi::Subject> = r(&group);
    assert!(ad::roles_change_anything(
        &model,
        std::slice::from_ref(&alice_ref),
        &[role_ref("vm-operator")]
    ));
    assert!(!ad::roles_change_anything(
        &model,
        std::slice::from_ref(&alice_ref),
        &[role_ref("pool-admin")]
    ));
    // Removing every pool admin shows the exit advice on XCP-ng 8.x.
    assert_eq!(
        ad::admins_removed(&model, std::slice::from_ref(&alice_ref)),
        (1, 1)
    );
    for (subject, role) in [(&alice_ref, "vm-operator"), (&group_ref, "vm-admin")] {
        let op = run(
            &h,
            conn,
            ActionRequest::SetSubjectRoles {
                subjects: vec![subject.clone()],
                roles: vec![role_ref(role)],
                log_out: vec![],
            },
        )
        .await;
        ok(&op);
    }
    assert_eq!(
        field(&server, "subject", &alice, "roles"),
        json!(["OpaqueRef:role-vm-operator"])
    );

    // Domain users log in with their role: alice directly, bob through
    // the group, carol not at all.
    let alice_session = login(&server, "LAB\\alice", "alice-password")
        .await
        .unwrap();
    assert!(
        login(&server, "alice", "wrong").await.is_err(),
        "a wrong password is refused"
    );
    let rights = rbac::fetch(&alice_session).await.unwrap();
    assert!(!rights.is_local_superuser);
    assert!(rights.permissions.contains("vm-operator"));
    assert_eq!(rights.user_sid.as_deref(), Some(ALICE_SID));
    assert_eq!(rights.subject, Some(alice_ref.clone()));
    let bob = login(&server, "bob@lab.example", "bob-password")
        .await
        .unwrap();
    let bob_rights = rbac::fetch(&bob).await.unwrap();
    assert!(bob_rights.permissions.contains("vm-admin"));
    assert_eq!(bob_rights.subject, Some(group_ref.clone()));
    let e = login(&server, "carol", "carol-password").await.unwrap_err();
    assert!(e.is_api("SESSION_AUTHORIZATION_FAILED"), "{e:?}");
    // A VM Operator may start VMs, not migrate them or change HA or users.
    for (request, allowed) in [
        (
            ActionRequest::Vm {
                vm: r(DB),
                action: VmAction::Start,
            },
            true,
        ),
        (
            ActionRequest::Vm {
                vm: r(WEB),
                action: VmAction::Migrate(r(H2)),
            },
            false,
        ),
        (ActionRequest::DisableHa, false),
        (
            ActionRequest::AddSubjects {
                names: vec!["carol".into()],
            },
            false,
        ),
    ] {
        assert_eq!(
            matches!(
                rbac::check(&model, Some(&rights), &request.rbac_methods()),
                RbacCheck::Allowed
            ),
            allowed,
            "{request:?}"
        );
    }

    // Logged in: alice and bob.
    let PrecheckResult::LoggedInSubjects(Ok(sids)) =
        precheck(&mut h, conn, Precheck::LoggedInSubjects).await
    else {
        panic!("logged in");
    };
    assert!(sids.contains(&ALICE_SID.to_owned()));
    // Who changes roles while logged in is logged out after.
    let state = h
        .wait_for("alice's role seen", |s| {
            first(s)
                .model
                .subjects
                .get(&alice_ref)
                .is_some_and(|a| a.roles == [role_ref("vm-operator")])
        })
        .await;
    let model = first(&state).model.clone();
    assert_eq!(
        ad::logged_in_to_log_out(
            &model,
            std::slice::from_ref(&alice_ref),
            &[role_ref("read-only")],
            &sids
        ),
        vec![alice_ref.clone()]
    );
    assert!(
        ad::logged_in_to_log_out(
            &model,
            std::slice::from_ref(&alice_ref),
            &[role_ref("vm-operator")],
            &sids
        )
        .is_empty()
    );
    let op = run(
        &h,
        conn,
        ActionRequest::SetSubjectRoles {
            subjects: vec![alice_ref.clone()],
            roles: vec![role_ref("read-only")],
            log_out: vec![alice_ref.clone()],
        },
    )
    .await;
    ok(&op);
    let e = xapi::session::get_all_subject_identifiers(&alice_session)
        .await
        .unwrap_err();
    assert!(e.is_api("SESSION_INVALID"), "{e:?}");

    // Log Out ends bob's sessions.
    let dave = subject_by_name(&server, "LAB\\dave").unwrap();
    let _dave_session = login(&server, "LAB\\dave", "dave-password").await.unwrap();
    let op = run(
        &h,
        conn,
        ActionRequest::LogoutSubjects {
            subjects: vec![r(&dave)],
        },
    )
    .await;
    ok(&op);
    let PrecheckResult::LoggedInSubjects(Ok(sids)) =
        precheck(&mut h, conn, Precheck::LoggedInSubjects).await
    else {
        panic!("logged in");
    };
    assert!(!sids.contains(&"S-1-5-21-1000-1104".to_owned()));

    // Remove access.
    let op = run(
        &h,
        conn,
        ActionRequest::RemoveSubjects {
            subjects: vec![r(&dave), group_ref.clone()],
        },
    )
    .await;
    ok(&op);
    assert!(server.record("subject", &dave).is_none());
    assert!(server.record("subject", &group).is_none());
    let e = xapi::session::get_all_subject_identifiers(&bob)
        .await
        .unwrap_err();
    assert!(e.is_api("SESSION_INVALID"), "{e:?}");

    // Leave the domain: the subjects stay, as in XAPI.
    let op = run(
        &h,
        conn,
        ActionRequest::DisableAd {
            credentials: Some((admin.into(), Password::new(admin_password))),
        },
    )
    .await;
    ok(&op);
    assert_eq!(field(&server, "host", H1, "external_auth_type"), json!(""));
    assert!(server.record("subject", &alice).is_some());
    let op = run(&h, conn, ActionRequest::DisableAd { credentials: None }).await;
    failed_with(&op, "POOL_AUTH_DISABLE_FAILED");
}

#[tokio::test]
async fn domain_users_are_offered_role_elevation() {
    let server = ha_pool().await;
    // Root joins the domain and gives alice VM Operator.
    let root = harness();
    let rc = connect(&root, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let (admin, admin_password) = xapi_mock::AD_ADMIN;
    ok(&run(
        &root,
        rc,
        ActionRequest::EnableAd {
            domain: xapi_mock::AD_DOMAIN.into(),
            user: admin.into(),
            password: Password::new(admin_password),
        },
    )
    .await);
    ok(&run(
        &root,
        rc,
        ActionRequest::AddSubjects {
            names: vec!["LAB\\alice".into()],
        },
    )
    .await);
    let alice = subject_by_name(&server, "LAB\\alice").unwrap();
    ok(&run(
        &root,
        rc,
        ActionRequest::SetSubjectRoles {
            subjects: vec![r(&alice)],
            roles: vec![role_ref("vm-operator")],
            log_out: vec![],
        },
    )
    .await);

    // alice connects with her own console.
    let mut h = spawn(BackendOptions {
        ..common::fast_options(memory_store().0)
    });
    let conn = connect(&h, &server, "LAB\\alice", "alice-password").await;
    let state = h.backend.state();
    let view = first(&state);
    let rights = view.rbac.as_ref().unwrap();
    assert!(!rights.is_local_superuser);
    assert_eq!(rights.subject, Some(r(&alice)));
    let roles = rbac::session_roles(&view.model, rights);
    assert_eq!(roles[0].name_label, "vm-operator");
    // Disable HA is not hers: denied client-side; the server agrees.
    let configure = ActionRequest::EnableHa {
        heartbeat_sr: r(NFS),
        ntol: 0,
        startup: vec![],
    };
    let RbacCheck::Denied { authorized } =
        rbac::check(&view.model, Some(rights), &configure.rbac_methods())
    else {
        panic!("denied");
    };
    let names: Vec<&str> = authorized.iter().map(|r| r.name_label.as_str()).collect();
    assert_eq!(names, ["pool-admin", "pool-operator"]);
    h.backend.send(Command::Action {
        connection: conn,
        request: configure.clone(),
    });
    match h.next_notification().await {
        Notification::ActionFailed { error, .. } => {
            assert!(error.is_api("RBAC_PERMISSION_DENIED"), "{error:?}")
        }
        other => panic!("{other:?}"),
    }
    // Elevated with a Pool Operator, it runs (and the server records the
    // statefile).
    h.backend.send(Command::RunElevated {
        connection: conn,
        request: configure,
        username: "pool-operator-user".into(),
        password: Password::new("pool-operator-password"),
        request_id: 7,
    });
    let op = loop {
        if let Notification::Elevation {
            request_id: 7,
            outcome,
        } = h.next_notification().await
        {
            match outcome {
                ElevationOutcome::Authorized { operation } => break operation,
                other => panic!("{other:?}"),
            }
        }
    };
    let state = h
        .wait_for("elevated HA done", |s| {
            s.operation(op).is_some_and(|o| !o.is_running())
        })
        .await;
    let o = state.operation(op).unwrap();
    ok(o);
    assert_eq!(o.elevated_as.as_deref(), Some("pool-operator-user"));
    assert_eq!(field(&server, "pool", POOL, "ha_enabled"), json!(true));
    ok(&run(&root, rc, ActionRequest::DisableHa).await);
}

#[tokio::test]
async fn add_server_joins_the_pools_domain_first() {
    let a = MockServer::start_lab(false, "f10b-a", 5).await;
    let b = MockServer::start_lab(false, "f10b-b", 6).await;
    let h = spawn(BackendOptions {
        join_wait: std::time::Duration::from_secs(5),
        ..common::fast_options(memory_store().0)
    });
    let ca = connect(&h, &a, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let cb = connect(&h, &b, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let (admin, admin_password) = xapi_mock::AD_ADMIN;
    // a becomes a pool joined to the domain.
    let before = last_id(&h);
    h.backend.send(Command::NewPool {
        coordinator: ca,
        members: vec![],
        name: "AD pool".into(),
        description: String::new(),
        ad: None,
    });
    ok(&ended_after(&h, before).await);
    ok(&run(
        &h,
        ca,
        ActionRequest::EnableAd {
            domain: xapi_mock::AD_DOMAIN.into(),
            user: admin.into(),
            password: Password::new(admin_password),
        },
    )
    .await);
    h.wait_for("a joined the domain", |s| {
        s.connection(ca)
            .and_then(|c| c.model.coordinator().map(|h| h.external_auth_type == "AD"))
            .unwrap_or(false)
    })
    .await;
    let b_host = "OpaqueRef:host-0001-f10b-b";

    // Without a domain account: refused before anything changes.
    let join = |ad: Option<(String, Password)>| Command::JoinPool {
        pool: ca,
        hosts: vec![cb],
        ad,
    };
    let before = last_id(&h);
    h.backend.send(join(None));
    let op = ended_after(&h, before).await;
    failed_with(&op, xen_core::pool_run::errors::AD_CREDENTIALS);
    assert_eq!(field(&b, "host", b_host, "external_auth_type"), json!(""));
    // A refused account: b stays standalone and out of the domain.
    let before = last_id(&h);
    h.backend
        .send(join(Some((admin.into(), Password::new("wrong")))));
    let op = ended_after(&h, before).await;
    let e = failed_with(&op, "POOL_AUTH_ENABLE_FAILED_WRONG_CREDENTIALS");
    assert!(ad::is_credentials_failure(&e));
    assert!(!b.is_pool_member());
    // The right one: b joins the domain, then the pool.
    let before = last_id(&h);
    h.backend
        .send(join(Some((admin.into(), Password::new(admin_password)))));
    let op = ended_after(&h, before).await;
    ok(&op);
    assert!(b.is_pool_member());
    let calls = b.calls();
    let enable = calls
        .iter()
        .position(|c| c == "pool.enable_external_auth")
        .unwrap();
    let joined = calls.iter().rposition(|c| c == "Async.pool.join").unwrap();
    assert!(enable < joined);
    h.wait_for("b in the pool", |s| {
        s.connection(ca)
            .is_some_and(|c| c.model.hosts.contains_key(&r(b_host)))
    })
    .await;
}
