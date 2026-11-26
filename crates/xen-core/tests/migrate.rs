//! F12a: the Migrate / Move / Copy wizard's operations and checks, and
//! vApps, against the mock (backend only, no UI). Two standalone lab
//! servers in one process are two pools: a VM is migrated (live, with
//! storage and network mapping) and copied between them; each refused
//! feasibility check, a failure in the middle of the migration and a
//! cancelled one change nothing; Move VM within a pool; vApps are made,
//! edited, started in order, shut down and deleted.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::{memory_store, spawn};
use serde_json::{Value, json};
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Ref, Session, TrustStore};
use xapi_mock::MockServer;
use xen_core::migrate::{self, ServerVerdict, VmPlan, VmTarget, WizardMode};
use xen_core::vapp::{NewVAppSpec, StartupRow, VAppChanges};
use xen_core::{
    ActionRequest, AppState, BackendOptions, Command, ConnectionId, ConnectionView, Notification,
    Operation, OperationStatus, Precheck, PrecheckResult, VmAction,
};

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

fn harness() -> common::Harness {
    spawn(BackendOptions {
        ..common::fast_options(memory_store().0)
    })
}

async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
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

fn view(state: &AppState, id: ConnectionId) -> &ConnectionView {
    state.connection(id).expect("connection")
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

fn failed_with(op: &Operation, code: &str) {
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            assert!(e.is_api(code), "{:?}: expected {code}, got {e}", op.kind);
        }
        other => panic!("{:?}: expected failure {code}, got {other:?}", op.kind),
    }
}

/// A session on `server`, outside the backend.
async fn login(server: &MockServer) -> Session {
    let endpoint = Endpoint::parse(&server.address()).unwrap();
    let client = Arc::new(
        JsonRpcClient::new(&endpoint, &TrustStore::new(), &ClientOptions::default()).unwrap(),
    );
    Session::login_with_password(
        client,
        xapi_mock::USERNAME,
        xapi_mock::PASSWORD,
        "f12a-tests",
    )
    .await
    .unwrap()
}

/// A halted VM on lab server `tag` with two disks on its local SR and one
/// interface on its eth0 network; (vm, [vdis], vif).
async fn lab_vm(s: &Session, tag: &str, name: &str, mac: &str) -> (String, Vec<String>, String) {
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
    let sr = format!("OpaqueRef:sr-local-01-{tag}");
    let mut vdis = Vec::new();
    for (i, size) in [(0, 104857600_i64), (1, 52428800_i64)] {
        let vdi: String = s
            .call(
                "VDI.create",
                vec![json!({"name_label": format!("{name} disk {i}"), "SR": sr,
                    "virtual_size": size, "type": "user", "sharable": false, "read_only": false})],
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
    let vif: String = s
        .call(
            "VIF.create",
            vec![
                json!({"VM": vm, "network": format!("OpaqueRef:net-eth0-{tag}"),
                "device": "0", "MAC": mac, "MTU": 1500}),
            ],
        )
        .await
        .unwrap();
    (vm, vdis, vif)
}

/// A second, shared SR on lab server `tag` (an NFS SR attached to its host).
fn add_shared_sr(server: &MockServer, tag: &str) -> String {
    let sr = format!("OpaqueRef:sr-shared-{tag}");
    let pbd = format!("OpaqueRef:pbd-shared-{tag}");
    let host = format!("OpaqueRef:host-0001-{tag}");
    server.add_object(
        "pbd",
        &pbd,
        json!({"uuid": format!("5bd0{tag}00-0000-4000-8000-000000000001"), "host": host,
            "SR": sr, "device_config": {"server": "nfs", "serverpath": "/x"},
            "currently_attached": true, "other_config": {}}),
    );
    server.add_object(
        "sr",
        &sr,
        json!({"uuid": format!("5bd0{tag}00-0000-4000-8000-000000000002"),
            "name_label": "Shared NFS", "name_description": "", "type": "nfs",
            "content_type": "", "shared": true, "PBDs": [pbd], "VDIs": [],
            "physical_size": 1099511627776_i64, "physical_utilisation": 0,
            "virtual_allocation": 0, "allowed_operations": ["vdi_create", "vdi_destroy"],
            "current_operations": {}, "other_config": {}, "sm_config": {}, "tags": []}),
    );
    let mut pbds = server
        .field("host", &host, "PBDs")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    pbds.push(json!(pbd));
    server.set_field("host", &host, "PBDs", Value::Array(pbds));
    sr
}

/// Asks the servers about `checks` (the Destination page's question).
async fn precheck(
    h: &mut common::Harness,
    source: ConnectionId,
    target: ConnectionId,
    mode: WizardMode,
    checks: Vec<(Ref<xapi::Vm>, Ref<xapi::Host>)>,
) -> Vec<(Ref<xapi::Vm>, Ref<xapi::Host>, ServerVerdict)> {
    h.backend.send(Command::Precheck {
        connection: source,
        id: 7,
        check: Precheck::Migrate {
            target,
            mode,
            checks,
        },
    });
    loop {
        if let Notification::Precheck {
            id: 7,
            result: PrecheckResult::Migrate(rows),
            ..
        } = h.next_notification().await
        {
            return rows;
        }
    }
}

fn refused_code(v: &ServerVerdict) -> String {
    match v {
        ServerVerdict::Ok => "OK".into(),
        ServerVerdict::Refused { error, .. } => error.api_code().unwrap_or_default().to_owned(),
    }
}

/// The wizard's plan for `vm`: each disk to `sr`, each interface to
/// `network`, received by `host`.
fn plan(
    state: &AppState,
    source: ConnectionId,
    target: ConnectionId,
    vm: &Ref<xapi::Vm>,
    host: Option<Ref<xapi::Host>>,
    disks: &[(&str, &str)],
    network: Option<&str>,
) -> VmPlan {
    let src = view(state, source);
    let vm_rec = src.model.vms.get(vm).unwrap();
    let mut storage: BTreeMap<Ref<xapi::Vdi>, Ref<xapi::Sr>> =
        disks.iter().map(|(v, s)| (r(v), r(s))).collect();
    migrate::additional_mappings(&src.model, vm_rec, &mut storage, source == target);
    let networks = match network {
        Some(n) => migrate::vif_rows(&src.model, vm, vm_rec)
            .into_iter()
            .map(|row| (row.mac, r(n)))
            .collect(),
        None => BTreeMap::new(),
    };
    VmPlan {
        vm: vm.clone(),
        target: VmTarget {
            connection: target,
            host,
        },
        storage,
        networks,
    }
}

const A: &str = "f12a-a";
const B: &str = "f12a-b";

fn host(tag: &str) -> String {
    format!("OpaqueRef:host-0001-{tag}")
}

#[tokio::test]
async fn migrate_and_copy_between_two_pools() {
    let a = MockServer::start_lab(false, A, 1).await;
    let b = MockServer::start_lab(false, B, 2).await;
    a.set_task_timing(4, Duration::from_millis(20));
    let shared_b = add_shared_sr(&b, B);
    let sa = login(&a).await;
    let (vm, vdis, vif) = lab_vm(&sa, A, "mover", "02:00:00:aa:00:01").await;
    let (copied, copied_vdis, _) = lab_vm(&sa, A, "original", "02:00:00:aa:00:02").await;
    let mut h = harness();
    let ca = connect(&h, &a).await;
    let cb = connect(&h, &b).await;
    let vm_ref: Ref<xapi::Vm> = r(&vm);
    // The VM runs on a.
    let before = last_id(&h);
    h.backend.send(Command::Vm {
        connection: ca,
        vm: vm_ref.clone(),
        action: VmAction::Start,
    });
    ok(&ended_after(&h, before).await);
    let state = h
        .wait_for("running, migrate_send offered", |s| {
            view(s, ca).model.vms.get(&vm_ref).is_some_and(|v| {
                v.power_state == xapi::VmPowerState::Running
                    && migrate::can_open_migrate(&view(s, ca).model, v, false)
            }) && view(s, cb).model.srs.contains_key(&r(&shared_b))
        })
        .await;
    // The rules: b's SRs for each disk, b's networks, the migration network.
    {
        let src = view(&state, ca);
        let dst = view(&state, cb);
        let vm_rec = src.model.vms.get(&vm_ref).unwrap();
        assert_eq!(migrate::move_mode(vm_rec), WizardMode::Move);
        let rows = migrate::disk_rows(&src.model, &vm_ref, vm_rec);
        assert_eq!(rows.len(), 2);
        let host_b: Ref<xapi::Host> = r(&host(B));
        let srs = migrate::target_srs(
            &src.model,
            &dst.model,
            &rows[0],
            Some(&host_b),
            WizardMode::Migrate,
            false,
            false,
        );
        let names: Vec<&str> = srs.iter().map(|(_, s)| s.name_label.as_str()).collect();
        assert_eq!(names, ["Local storage", "Shared NFS"]);
        // With no server chosen, only the shared SR.
        let srs = migrate::target_srs(
            &src.model,
            &dst.model,
            &rows[0],
            None,
            WizardMode::Migrate,
            false,
            false,
        );
        assert_eq!(srs.len(), 1);
        let nets = migrate::target_networks(&dst.model, Some(&host_b), false);
        assert_eq!(nets.len(), 2);
        let rows = migrate::vif_rows(&src.model, &vm_ref, vm_rec);
        // The same name maps to b's eth0 network by default.
        assert_eq!(
            migrate::default_network(&dst.model, &nets, &rows[0].network_name),
            Some(&r(&format!("OpaqueRef:net-eth0-{B}")))
        );
        let transfer = migrate::transfer_networks(&dst.model);
        assert!(
            transfer
                .iter()
                .any(|(r, _, m)| *m && r.as_str().contains("eth0"))
        );
    }
    // The Destination page's question: b's server may receive it.
    let rows = precheck(
        &mut h,
        ca,
        cb,
        WizardMode::Migrate,
        vec![(vm_ref.clone(), r(&host(B)))],
    )
    .await;
    assert_eq!(refused_code(&rows[0].2), "OK");
    assert!(a.calls().iter().any(|c| c == "VM.assert_can_migrate"));
    assert!(b.calls().iter().any(|c| c == "host.migrate_receive"));

    // Live migration, one disk per SR, the interface on b's eth1.
    let state = h.backend.state();
    let p = plan(
        &state,
        ca,
        cb,
        &vm_ref,
        Some(r(&host(B))),
        &[
            (&vdis[0], &format!("OpaqueRef:sr-local-01-{B}")),
            (&vdis[1], &shared_b),
        ],
        Some(&format!("OpaqueRef:net-eth1-{B}")),
    );
    let request = migrate::finish_request(
        &view(&state, ca).model,
        ca,
        &view(&state, cb).model,
        &p,
        WizardMode::Migrate,
        Some(&r(&format!("OpaqueRef:net-eth0-{B}"))),
    )
    .unwrap();
    let migrate::FinishRequest::MigrateSend(spec) = request else {
        panic!("a cross-pool migration: {request:?}");
    };
    assert!(!spec.copy);
    assert_eq!(
        spec.vif_map,
        vec![(r(&vif), r(&format!("OpaqueRef:net-eth1-{B}")))]
    );
    let op = run(&h, ca, ActionRequest::MigrateSend(Box::new(spec))).await;
    ok(&op);
    assert!(
        matches!(&op.kind, xen_core::OperationKind::Migrate(xen_core::operations::MigrateKind::Migrate { vm, from: Some(f), to }) if vm == "mover" && f == "lab-host-f12a-a" && to == "lab-host-f12a-b"),
        "{:?}",
        op.kind
    );
    let state = h
        .wait_for("the VM is on b", |s| {
            !view(s, ca).model.vms.contains_key(&vm_ref)
                && view(s, cb)
                    .model
                    .vms
                    .values()
                    .any(|v| v.name_label == "mover")
        })
        .await;
    let dst = view(&state, cb);
    let (moved_ref, moved) = dst
        .model
        .vms
        .iter()
        .find(|(_, v)| v.name_label == "mover")
        .unwrap();
    assert_eq!(moved.power_state, xapi::VmPowerState::Running);
    assert_eq!(moved.resident_on, r(&host(B)));
    let disks = xen_core::provisioning::vm_disks(&dst.model, moved);
    let mut placed: Vec<(String, String)> = disks
        .iter()
        .map(|(_, _, _, v)| {
            (
                v.name_label.clone(),
                dst.model.srs.get(&v.sr).unwrap().name_label.clone(),
            )
        })
        .collect();
    placed.sort();
    assert_eq!(
        placed,
        [
            ("mover disk 0".to_owned(), "Local storage".to_owned()),
            ("mover disk 1".to_owned(), "Shared NFS".to_owned())
        ]
    );
    let vifs: Vec<_> = moved
        .vifs
        .iter()
        .filter_map(|v| dst.model.vifs.get(v))
        .collect();
    assert_eq!(vifs.len(), 1);
    assert_eq!(vifs[0].mac, "02:00:00:aa:00:01");
    assert_eq!(vifs[0].network, r(&format!("OpaqueRef:net-eth1-{B}")));
    // Nothing of it is left on a.
    for vdi in &vdis {
        assert!(a.record("vdi", vdi).is_none());
    }
    let _ = moved_ref;

    // Copy of a halted VM: the source stays, the copy lands halted on b.
    let copied_ref: Ref<xapi::Vm> = r(&copied);
    let state = h.backend.state();
    let p = plan(
        &state,
        ca,
        cb,
        &copied_ref,
        None,
        &[(&copied_vdis[0], &shared_b), (&copied_vdis[1], &shared_b)],
        Some(&format!("OpaqueRef:net-eth0-{B}")),
    );
    let Some(migrate::FinishRequest::MigrateSend(spec)) = migrate::finish_request(
        &view(&state, ca).model,
        ca,
        &view(&state, cb).model,
        &p,
        WizardMode::Copy,
        Some(&r(&format!("OpaqueRef:net-eth0-{B}"))),
    ) else {
        panic!("a cross-pool copy");
    };
    assert!(spec.copy);
    // "None selected": b's coordinator receives it.
    assert_eq!(spec.host, r(&host(B)));
    let op = run(&h, ca, ActionRequest::MigrateSend(Box::new(spec))).await;
    ok(&op);
    assert!(matches!(
        &op.kind,
        xen_core::OperationKind::Migrate(xen_core::operations::MigrateKind::Copy { .. })
    ));
    let state = h
        .wait_for("the copy is on b", |s| {
            view(s, cb)
                .model
                .vms
                .values()
                .any(|v| v.name_label == "original")
        })
        .await;
    assert!(view(&state, ca).model.vms.contains_key(&copied_ref));
    let dst = view(&state, cb);
    let copy = dst
        .model
        .vms
        .values()
        .find(|v| v.name_label == "original")
        .unwrap();
    assert_eq!(copy.power_state, xapi::VmPowerState::Halted);
    assert!(
        xen_core::provisioning::vm_disks(&dst.model, copy)
            .iter()
            .all(|(_, _, _, v)| v.sr == r(&shared_b))
    );
    assert_ne!(
        copy.uuid,
        view(&state, ca).model.vms.get(&copied_ref).unwrap().uuid
    );
}

#[tokio::test]
async fn each_refused_check_and_a_failed_or_cancelled_migration_change_nothing() {
    let a = MockServer::start_lab(false, "f12a-ra", 3).await;
    let b = MockServer::start_lab(false, "f12a-rb", 4).await;
    let sa = login(&a).await;
    let (vm, vdis, _) = lab_vm(&sa, "f12a-ra", "stay", "02:00:00:bb:00:01").await;
    let mut h = harness();
    let ca = connect(&h, &a).await;
    let cb = connect(&h, &b).await;
    let vm_ref: Ref<xapi::Vm> = r(&vm);
    let host_b: Ref<xapi::Host> = r(&host("f12a-rb"));
    let state = h
        .wait_for("the VM is known", |s| {
            view(s, ca).model.vms.contains_key(&vm_ref)
        })
        .await;

    // Local refusals: the current server; an older destination.
    {
        let src = view(&state, ca);
        let dst = view(&state, cb);
        let vm_rec = src.model.vms.get(&vm_ref).unwrap();
        let home: Ref<xapi::Host> = r(&host("f12a-ra"));
        assert!(migrate::is_current_server(
            &src.model,
            vm_rec,
            &src.model,
            Some(&home),
            true
        ));
        assert!(migrate::is_current_server(
            &src.model, vm_rec, &src.model, None, true
        ));
        assert!(!migrate::is_current_server(
            &src.model, vm_rec, &dst.model, None, false
        ));
        assert_eq!(
            migrate::local_check(
                &src.model,
                vm_rec,
                &dst.model,
                &host_b,
                WizardMode::Migrate,
                false
            ),
            migrate::LocalCheck::Ask
        );
        // A move within the pool needs no question.
        assert_eq!(
            migrate::local_check(
                &src.model,
                vm_rec,
                &src.model,
                &home,
                WizardMode::Move,
                true
            ),
            migrate::LocalCheck::Skip
        );
    }
    let mut version = b
        .field("host", &host("f12a-rb"), "software_version")
        .unwrap();
    version["platform_version"] = json!("1.0.0");
    b.set_field("host", &host("f12a-rb"), "software_version", version);
    let state = h
        .wait_for("b is older", |s| {
            view(s, cb)
                .model
                .hosts
                .get(&host_b)
                .is_some_and(|h| xen_core::pool::platform_version(h) == Some("1.0.0"))
        })
        .await;
    {
        let src = view(&state, ca);
        let vm_rec = src.model.vms.get(&vm_ref).unwrap();
        assert_eq!(
            migrate::local_check(
                &src.model,
                vm_rec,
                &view(&state, cb).model,
                &host_b,
                WizardMode::Migrate,
                false
            ),
            migrate::LocalCheck::Refused(migrate::LocalRefusal::OlderServer)
        );
    }

    // Server refusals, each with the server's error.
    for (code, params) in [
        ("VM_HAS_PCI_ATTACHED", vec![vm.as_str()]),
        ("LICENCE_RESTRICTION", vec!["Storage_motion"]),
        ("VM_REQUIRES_SR", vec![vm.as_str(), "OpaqueRef:sr"]),
    ] {
        b.refuse_migration_to(&host("f12a-rb"), Some((code, &params)));
        let rows = precheck(
            &mut h,
            ca,
            cb,
            WizardMode::Migrate,
            vec![(vm_ref.clone(), host_b.clone())],
        )
        .await;
        assert_eq!(refused_code(&rows[0].2), code);
    }
    b.refuse_migration_to(&host("f12a-rb"), None);
    // The VM no longer offers migrate_send.
    let allowed = a.field("vm", &vm, "allowed_operations").unwrap();
    a.set_field("vm", &vm, "allowed_operations", json!(["start"]));
    let rows = precheck(
        &mut h,
        ca,
        cb,
        WizardMode::Migrate,
        vec![(vm_ref.clone(), host_b.clone())],
    )
    .await;
    assert_eq!(refused_code(&rows[0].2), "OPERATION_NOT_ALLOWED");
    a.set_field("vm", &vm, "allowed_operations", allowed);
    // Too little memory for a running VM on b.
    let before = last_id(&h);
    h.backend.send(Command::Vm {
        connection: ca,
        vm: vm_ref.clone(),
        action: VmAction::Start,
    });
    ok(&ended_after(&h, before).await);
    let metrics = b
        .field("host", &host("f12a-rb"), "metrics")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap();
    let free = b.field("host_metrics", &metrics, "memory_free").unwrap();
    b.set_field("host_metrics", &metrics, "memory_free", json!(1024));
    let rows = precheck(
        &mut h,
        ca,
        cb,
        WizardMode::Migrate,
        vec![(vm_ref.clone(), host_b.clone())],
    )
    .await;
    assert_eq!(refused_code(&rows[0].2), "HOST_NOT_ENOUGH_FREE_MEMORY");
    b.set_field("host_metrics", &metrics, "memory_free", free);
    // A user whose role may not migrate: the server says so.
    let rb_user = xapi_mock::RBAC_USERS
        .iter()
        .find(|(_, _, role)| *role == "vm-operator")
        .unwrap();
    let h2 = harness();
    h2.backend.send(Command::Connect {
        server: a.address(),
        username: rb_user.0.into(),
        password: Some(rb_user.1.into()),
        remember_password: false,
    });
    let a_op = h2
        .wait_for("vm-operator connected", |s| {
            s.connections
                .first()
                .is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
        })
        .await
        .connections[0]
        .id;
    h2.connect(&b, xapi_mock::PASSWORD);
    let b_id = h2
        .wait_for("b connected", |s| {
            s.connections
                .iter()
                .filter(|c| c.status.is_connected())
                .count()
                == 2
        })
        .await
        .connections
        .iter()
        .find(|c| c.server == b.address())
        .unwrap()
        .id;
    let mut h2 = h2;
    let rows = precheck(
        &mut h2,
        a_op,
        b_id,
        WizardMode::Migrate,
        vec![(vm_ref.clone(), host_b.clone())],
    )
    .await;
    assert_eq!(refused_code(&rows[0].2), "RBAC_PERMISSION_DENIED");

    // The plan of every later attempt.
    let state = h.backend.state();
    let spec = || {
        let p = plan(
            &state,
            ca,
            cb,
            &vm_ref,
            Some(host_b.clone()),
            &[
                (&vdis[0], &format!("OpaqueRef:sr-local-01-{}", "f12a-rb")),
                (&vdis[1], &format!("OpaqueRef:sr-local-01-{}", "f12a-rb")),
            ],
            Some(&format!("OpaqueRef:net-eth0-{}", "f12a-rb")),
        );
        match migrate::finish_request(
            &view(&state, ca).model,
            ca,
            &view(&state, cb).model,
            &p,
            WizardMode::Migrate,
            Some(&r(&format!("OpaqueRef:net-eth0-{}", "f12a-rb"))),
        ) {
            Some(migrate::FinishRequest::MigrateSend(s)) => s,
            other => panic!("{other:?}"),
        }
    };
    let vms_on_b = || b.refs("vm").len();
    let vdis_on_b = || b.refs("vdi").len();
    let (vms_b, vdis_b) = (vms_on_b(), vdis_on_b());

    // A failure in the middle of the migration: the VM stays running on a.
    a.set_task_timing(4, Duration::from_millis(30));
    a.fail_method(
        "VM.migrate_send",
        Some(("VDI_COPY_FAILED", &["mirror broke"])),
    );
    let op = run(&h, ca, ActionRequest::MigrateSend(Box::new(spec()))).await;
    failed_with(&op, "VDI_COPY_FAILED");
    assert!(op.progress.is_some());
    a.fail_method("VM.migrate_send", None);
    assert_eq!(a.vm_power_state(&vm), "Running");
    assert_eq!((vms_on_b(), vdis_on_b()), (vms_b, vdis_b));

    // Cancelled while it runs: nothing moves.
    a.set_task_timing(50, Duration::from_millis(40));
    let before = last_id(&h);
    h.backend.send(Command::Action {
        connection: ca,
        request: ActionRequest::MigrateSend(Box::new(spec())),
    });
    let state = h
        .wait_for("the migration task runs", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && o.is_running() && o.can_cancel())
        })
        .await;
    let op_id = state
        .operations
        .iter()
        .find(|o| o.id > before && o.can_cancel())
        .unwrap()
        .id;
    h.backend
        .send(Command::CancelOperation { operation: op_id });
    let op = ended_after(&h, before).await;
    assert!(
        matches!(op.status, OperationStatus::Cancelled),
        "{:?}",
        op.status
    );
    assert_eq!(a.vm_power_state(&vm), "Running");
    assert!(a.record("vm", &vm).is_some());
    assert_eq!((vms_on_b(), vdis_on_b()), (vms_b, vdis_b));
    // The destination went away: the migration fails before any call.
    a.set_task_timing(2, Duration::from_millis(10));
    h.backend.send(Command::Disconnect { connection: cb });
    h.wait_for("b disconnected", |s| s.connection(cb).is_none())
        .await;
    let op = run(&h, ca, ActionRequest::MigrateSend(Box::new(spec()))).await;
    failed_with(&op, xen_core::migrate_run::errors::TARGET_DISCONNECTED);
    assert_eq!(a.vm_power_state(&vm), "Running");
}

#[tokio::test]
async fn move_vm_and_storage_motion_within_the_pool() {
    let a = MockServer::start_lab(false, "f12a-ma", 5).await;
    let shared = add_shared_sr(&a, "f12a-ma");
    let sa = login(&a).await;
    let (vm, vdis, _) = lab_vm(&sa, "f12a-ma", "halted", "02:00:00:cc:00:01").await;
    let (live, live_vdis, _) = lab_vm(&sa, "f12a-ma", "live", "02:00:00:cc:00:02").await;
    let h = harness();
    let ca = connect(&h, &a).await;
    let (vm_ref, live_ref): (Ref<xapi::Vm>, Ref<xapi::Vm>) = (r(&vm), r(&live));
    let before = last_id(&h);
    h.backend.send(Command::Vm {
        connection: ca,
        vm: live_ref.clone(),
        action: VmAction::Start,
    });
    ok(&ended_after(&h, before).await);
    let state = h
        .wait_for("both known", |s| {
            let m = &view(s, ca).model;
            m.vms.contains_key(&vm_ref)
                && m.vms
                    .get(&live_ref)
                    .is_some_and(|v| v.power_state == xapi::VmPowerState::Running)
                && m.srs.contains_key(&r(&shared))
        })
        .await;
    let home: Ref<xapi::Host> = r(&host("f12a-ma"));
    // Move VM (halted, within the pool): only the second disk moves.
    let p = plan(
        &state,
        ca,
        ca,
        &vm_ref,
        Some(home.clone()),
        &[
            (&vdis[0], &format!("OpaqueRef:sr-local-01-{}", "f12a-ma")),
            (&vdis[1], &shared),
        ],
        None,
    );
    let request = migrate::finish_request(
        &view(&state, ca).model,
        ca,
        &view(&state, ca).model,
        &p,
        WizardMode::Move,
        None,
    )
    .unwrap();
    let migrate::FinishRequest::Move {
        vm: v,
        host,
        storage,
    } = request
    else {
        panic!("a move: {request:?}");
    };
    let op = run(
        &h,
        ca,
        ActionRequest::MoveVm {
            vm: v,
            host,
            storage,
        },
    )
    .await;
    ok(&op);
    assert!(a.calls().iter().any(|c| c == "Async.VDI.copy"));
    assert!(a.record("vdi", &vdis[1]).is_none(), "the old disk is gone");
    assert!(
        a.record("vdi", &vdis[0]).is_some(),
        "the unmoved disk stays"
    );
    let state = h
        .wait_for("the disk moved", |s| {
            let m = &view(s, ca).model;
            m.vms.get(&vm_ref).is_some_and(|v| {
                xen_core::provisioning::vm_disks(m, v)
                    .iter()
                    .any(|(_, _, _, d)| d.sr == r(&shared))
                    && xen_core::provisioning::vm_disks(m, v).len() == 2
            })
        })
        .await;
    let m = &view(&state, ca).model;
    let moved = m.vms.get(&vm_ref).unwrap();
    assert_eq!(
        moved.suspend_sr,
        r(&format!("OpaqueRef:sr-local-01-{}", "f12a-ma"))
    );
    // A move with nothing to move does nothing.
    let p = plan(&state, ca, ca, &vm_ref, Some(home.clone()), &[], None);
    assert_eq!(
        migrate::finish_request(m, ca, m, &p, WizardMode::Move, None),
        Some(migrate::FinishRequest::Nothing)
    );
    // Within the pool without storage changes: a plain live migration.
    let p = plan(&state, ca, ca, &live_ref, Some(home.clone()), &[], None);
    assert_eq!(
        migrate::finish_request(m, ca, m, &p, WizardMode::Migrate, None),
        Some(migrate::FinishRequest::PoolMigrate {
            vm: live_ref.clone(),
            host: home.clone()
        })
    );
    // Storage motion of the running VM within the pool: VM.migrate_send.
    let p = plan(
        &state,
        ca,
        ca,
        &live_ref,
        Some(home.clone()),
        &[(&live_vdis[0], &shared), (&live_vdis[1], &shared)],
        None,
    );
    let Some(migrate::FinishRequest::MigrateSend(spec)) = migrate::finish_request(
        m,
        ca,
        m,
        &p,
        WizardMode::Migrate,
        Some(&r(&format!("OpaqueRef:net-eth0-{}", "f12a-ma"))),
    ) else {
        panic!("a storage motion");
    };
    assert!(
        spec.vif_map.is_empty(),
        "XAPI keeps the networks in the pool"
    );
    ok(&run(&h, ca, ActionRequest::MigrateSend(Box::new(spec))).await);
    let state = h
        .wait_for("live disks on the shared SR", |s| {
            let m = &view(s, ca).model;
            m.vms.get(&live_ref).is_some_and(|v| {
                let d = xen_core::provisioning::vm_disks(m, v);
                d.len() == 2 && d.iter().all(|(_, _, _, x)| x.sr == r(&shared))
            })
        })
        .await;
    assert_eq!(
        view(&state, ca)
            .model
            .vms
            .get(&live_ref)
            .unwrap()
            .power_state,
        xapi::VmPowerState::Running
    );
}

#[tokio::test]
async fn vapps_are_made_edited_started_in_order_and_deleted() {
    let a = MockServer::start_lab(false, "f12a-va", 6).await;
    a.set_appliance_delay_unit(Duration::from_millis(5));
    let sa = login(&a).await;
    let mut vms = Vec::new();
    for (i, name) in ["db", "app", "web"].iter().enumerate() {
        let (vm, _, _) = lab_vm(&sa, "f12a-va", name, &format!("02:00:00:dd:00:0{i}")).await;
        vms.push(r::<xapi::Vm>(&vm));
    }
    let h = harness();
    let ca = connect(&h, &a).await;
    h.wait_for("VMs known", |s| {
        vms.iter().all(|v| view(s, ca).model.vms.contains_key(v))
    })
    .await;
    // New vApp: db first, then app, then web; db waits 2 s before app.
    let startup = vec![
        StartupRow {
            vm: vms[0].clone(),
            order: 0,
            start_delay: 2,
        },
        StartupRow {
            vm: vms[1].clone(),
            order: 1,
            start_delay: 1,
        },
        StartupRow {
            vm: vms[2].clone(),
            order: 2,
            start_delay: 0,
        },
    ];
    let op = run(
        &h,
        ca,
        ActionRequest::NewVApp(Box::new(NewVAppSpec {
            name: "shop".into(),
            description: "the web shop".into(),
            vms: vms.clone(),
            startup,
        })),
    )
    .await;
    ok(&op);
    assert!(a.calls().iter().any(|c| c == "Async.VM_appliance.create"));
    assert!(a.calls().iter().any(|c| c == "Async.pool.sync_database"));
    let state = h
        .wait_for("the vApp with its VMs", |s| {
            let m = &view(s, ca).model;
            m.vm_appliances
                .values()
                .any(|a| a.name_label == "shop" && a.vms.len() == 3)
                && m.vms.get(&vms[1]).is_some_and(|v| v.order == 1)
        })
        .await;
    let (app_ref, app) = view(&state, ca)
        .model
        .vm_appliances
        .iter()
        .find(|(_, a)| a.name_label == "shop")
        .map(|(r, a)| (r.clone(), a.clone()))
        .unwrap();
    assert_eq!(app.name_description, "the web shop");
    assert!(xen_core::vapp::can_start(&app));
    assert!(!xen_core::vapp::can_shut_down(&app));
    // The VMs' vApp is known; Start vApp starts them in order.
    assert_eq!(
        xen_core::vapp::common_vapp(&view(&state, ca).model, &vms).map(|(r, _)| r.clone()),
        Some(app_ref.clone())
    );
    ok(&run(
        &h,
        ca,
        ActionRequest::StartVApp {
            vapp: app_ref.clone(),
        },
    )
    .await);
    let log = a.appliance_log();
    let started: Vec<&str> = log
        .iter()
        .filter(|(k, _)| k == "start")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(
        started,
        [vms[0].as_str(), vms[1].as_str(), vms[2].as_str()],
        "{log:?}"
    );
    let delays: Vec<&str> = log
        .iter()
        .filter(|(k, _)| k == "delay")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(delays, ["2", "1"]);
    for v in &vms {
        assert_eq!(a.vm_power_state(v.as_str()), "Running");
    }
    let state = h
        .wait_for("may shut down", |s| {
            view(s, ca)
                .model
                .vm_appliances
                .get(&app_ref)
                .is_some_and(|a| xen_core::vapp::can_shut_down(a))
        })
        .await;
    let _ = state;
    // Shut Down vApp: in reverse order.
    ok(&run(
        &h,
        ca,
        ActionRequest::ShutDownVApp {
            vapp: app_ref.clone(),
        },
    )
    .await);
    let stopped: Vec<String> = a
        .appliance_log()
        .into_iter()
        .filter(|(k, _)| k == "shutdown")
        .map(|(_, v)| v)
        .collect();
    assert_eq!(
        stopped,
        [vms[2].to_string(), vms[1].to_string(), vms[0].to_string()]
    );
    for v in &vms {
        assert_eq!(a.vm_power_state(v.as_str()), "Halted");
    }
    // Properties: renamed, web leaves, web's order changes.
    let op = run(
        &h,
        ca,
        ActionRequest::SetVApp {
            vapp: app_ref.clone(),
            changes: Box::new(VAppChanges {
                name: Some("shop v2".into()),
                description: None,
                members: Some(vec![vms[0].clone(), vms[1].clone()]),
                startup: vec![StartupRow {
                    vm: vms[0].clone(),
                    order: 5,
                    start_delay: 7,
                }],
            }),
        },
    )
    .await;
    ok(&op);
    let state = h
        .wait_for("properties saved", |s| {
            let m = &view(s, ca).model;
            m.vm_appliances
                .get(&app_ref)
                .is_some_and(|a| a.name_label == "shop v2" && a.vms.len() == 2)
                && m.vms
                    .get(&vms[0])
                    .is_some_and(|v| v.order == 5 && v.start_delay == 7)
                && m.vms.get(&vms[2]).is_some_and(|v| v.appliance.is_null())
        })
        .await;
    // Assign to vApp: web joins again; then leaves.
    let m = &view(&state, ca).model;
    let members: Vec<_> = m.vm_appliances[&app_ref].vms.clone();
    let mut with_web = members.clone();
    with_web.push(vms[2].clone());
    ok(&run(
        &h,
        ca,
        ActionRequest::AssignVApp {
            vapp: app_ref.clone(),
            vms: with_web,
        },
    )
    .await);
    h.wait_for("web joined", |s| {
        view(s, ca)
            .model
            .vms
            .get(&vms[2])
            .is_some_and(|v| v.appliance == app_ref)
    })
    .await;
    ok(&run(
        &h,
        ca,
        ActionRequest::RemoveFromVApp {
            vapp: app_ref.clone(),
            vms: vec![vms[2].clone()],
        },
    )
    .await);
    // Delete: the vApp goes, its VMs stay.
    ok(&run(
        &h,
        ca,
        ActionRequest::DeleteVApps {
            vapps: vec![app_ref.clone()],
        },
    )
    .await);
    let state = h
        .wait_for("the vApp is gone", |s| {
            view(s, ca).model.vm_appliances.is_empty()
        })
        .await;
    for v in &vms {
        let vm = view(&state, ca).model.vms.get(v).unwrap();
        assert!(vm.appliance.is_null());
    }
}
