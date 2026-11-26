//! F12b: scheduled snapshots and disaster recovery against the mock
//! (backend only, no UI).
//!
//! * Snapshot schedules are made (enabled with VMs, disabled without),
//!   run on schedule by the mock's clock with their retention, run now,
//!   fail with a per-VM history line, are edited, disabled (no more runs),
//!   assigned and deleted.
//! * Disaster recovery between two standalone lab servers (two pools)
//!   that share an NFS SR: Configure, a test failover (dry run) listing the
//!   recoverable VMs and vApps then cleaned up, each failing pre-check and
//!   its fix, a failure in the middle of a recovery, a cancelled recovery,
//!   a failover and a failback.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::{memory_store, spawn};
use serde_json::{Value, json};
use xapi::{
    ClientOptions, Endpoint, JsonRpcClient, Ref, Session, TrustStore, VmssFrequency, VmssType,
};
use xapi_mock::MockServer;
use xen_core::dr::{self, Chosen, DrItem, Problem, WizardType};
use xen_core::vmss::{self, NewPolicySpec, PolicyChanges, ScheduleChoice};
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

async fn ask(
    h: &mut common::Harness,
    conn: ConnectionId,
    id: u64,
    check: Precheck,
) -> PrecheckResult {
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
        "f12b-tests",
    )
    .await
    .unwrap()
}

// ---- snapshot schedules -------------------------------------------------------------------

#[tokio::test]
async fn snapshot_schedules_run_on_schedule_keep_their_retention_and_are_deleted() {
    let server = MockServer::start(false).await;
    server.set_task_timing(1, Duration::from_millis(10));
    // A server two hours ahead of UTC.
    server.set_server_utc_offset(2 * 3600);
    let mut h = harness();
    let conn = connect(&h, &server).await;
    let state = h.backend.state();
    let coordinator = view(&state, conn)
        .model
        .pool()
        .map(|(_, p)| p.master.clone())
        .unwrap();
    let time = match ask(&mut h, conn, 1, Precheck::ServerTime { host: coordinator }).await {
        PrecheckResult::ServerTime(Ok(t)) => t,
        other => panic!("server time: {other:?}"),
    };
    assert_eq!(time.diff, -2 * 3600);
    let (web, db) = (
        r::<xapi::Vm>("OpaqueRef:vm-web"),
        r::<xapi::Vm>("OpaqueRef:vm-db"),
    );
    // Hourly at 15 past (this console's time; the same minute there).
    let choice = ScheduleChoice {
        frequency: VmssFrequency::Hourly,
        hourly_min: 15,
        retention: 2,
        ..ScheduleChoice::default()
    };
    let computed = choice.compute(server.clock(), Some(time));
    assert_eq!(computed.schedule.get("min").map(String::as_str), Some("15"));
    let spec = NewPolicySpec {
        name: "hourly web".into(),
        description: "made by the test".into(),
        kind: VmssType::Snapshot,
        frequency: VmssFrequency::Hourly,
        schedule: computed.schedule.clone(),
        retained: 2,
        enabled: true,
        vms: vec![web.clone()],
        run_now: false,
    };
    ok(&run(&h, conn, ActionRequest::NewPolicy(Box::new(spec))).await);
    let state = h
        .wait_for("policy", |s| {
            view(s, conn)
                .model
                .vmss
                .values()
                .any(|p| p.name_label == "hourly web" && p.vms.len() == 1)
        })
        .await;
    let (policy, rec) = vmss::policies(&view(&state, conn).model)
        .into_iter()
        .map(|(r, p)| (r.clone(), p.clone()))
        .find(|(_, p)| p.name_label == "hourly web")
        .unwrap();
    assert!(rec.enabled);
    assert_eq!(rec.retained_snapshots, 2);
    let vm_rec = view(&state, conn).model.vms.get(&web).unwrap().clone();
    assert_eq!(
        vmss::policy_of(&view(&state, conn).model, &vm_rec).map(|(r, _)| r.clone()),
        Some(policy.clone())
    );
    // Next run: 12:15 UTC here (the mock clock is 12:00 UTC), 14:15 there.
    let (client, srv) = vmss::next_runs(&rec, Some(time)).unwrap();
    assert_eq!(client, server.clock() + 15 * 60);
    assert_eq!(srv, client + 2 * 3600);

    // The schedule runs three times; two scheduled snapshots stay.
    server.advance_clock(3 * 3600);
    assert_eq!(server.vmss_runs().len(), 3);
    let state = h
        .wait_for("history", |s| {
            let m = &view(s, conn).model;
            m.vmss
                .get(&policy)
                .is_some_and(|p| vmss::policy_messages(m, p).len() == 3)
        })
        .await;
    let m = &view(&state, conn).model;
    let scheduled: Vec<_> = m
        .vms
        .values()
        .filter(|v| v.snapshot_of == web && v.is_vmss_snapshot)
        .collect();
    assert_eq!(scheduled.len(), 2, "retention keeps two");
    let history = vmss::policy_messages(m, m.vmss.get(&policy).unwrap());
    assert!(
        history
            .iter()
            .all(|(_, msg)| msg.name == "VMSS_SNAPSHOT_SUCCEEDED")
    );
    assert_eq!(
        vmss::PolicyAlertType::from_priority(history[0].1.priority),
        vmss::PolicyAlertType::Info
    );

    // A failing VM: the run's history line names it.
    server.fail_vmss_snapshot("OpaqueRef:vm-web", Some("SR_FULL"));
    server.advance_clock(3600);
    let state = h
        .wait_for("failure", |s| {
            view(s, conn)
                .model
                .messages
                .values()
                .any(|m| m.name == "VMSS_SNAPSHOT_FAILED")
        })
        .await;
    let msg = view(&state, conn)
        .model
        .messages
        .values()
        .find(|m| m.name == "VMSS_SNAPSHOT_FAILED")
        .unwrap()
        .clone();
    assert_eq!(
        vmss::PolicyAlertType::from_priority(msg.priority),
        vmss::PolicyAlertType::Error
    );
    assert_eq!(
        vmss::vm_failures(&msg.body),
        vec![(vm_rec.name_label.clone(), "SR_FULL".to_owned())]
    );
    server.fail_vmss_snapshot("OpaqueRef:vm-web", None);

    // Run now.
    ok(&run(
        &h,
        conn,
        ActionRequest::RunPolicyNow {
            policy: policy.clone(),
        },
    )
    .await);
    assert_eq!(server.vmss_runs().last().map(|r| r.1), Some(false));

    // Disabled: the clock no longer runs it.
    ok(&run(
        &h,
        conn,
        ActionRequest::SetPolicyEnabled {
            policy: policy.clone(),
            enabled: false,
        },
    )
    .await);
    let runs = server.vmss_runs().len();
    server.advance_clock(5 * 3600);
    assert_eq!(server.vmss_runs().len(), runs);

    // Properties: weekly on Monday at 03:00 here, 4 kept, renamed, db added.
    let state = h.backend.state();
    let rec = view(&state, conn).model.vmss.get(&policy).unwrap().clone();
    let weekly = ScheduleChoice {
        frequency: VmssFrequency::Weekly,
        weekly: (3, 0),
        days: vec![1],
        retention: 4,
        ..ScheduleChoice::default()
    }
    .compute(server.clock(), Some(time));
    let changes = PolicyChanges::between(
        &rec,
        "weekly",
        &rec.name_description,
        VmssType::Checkpoint,
        VmssFrequency::Weekly,
        &weekly.schedule,
        4,
        &[web.clone(), db.clone()],
    );
    assert!(changes.description.is_none());
    ok(&run(
        &h,
        conn,
        ActionRequest::SetPolicy {
            policy: policy.clone(),
            changes: Box::new(changes),
        },
    )
    .await);
    let state = h
        .wait_for("edited", |s| {
            view(s, conn)
                .model
                .vmss
                .get(&policy)
                .is_some_and(|p| p.name_label == "weekly" && p.vms.len() == 2)
        })
        .await;
    let rec = view(&state, conn).model.vmss.get(&policy).unwrap().clone();
    assert_eq!(rec.frequency, VmssFrequency::Weekly);
    assert_eq!(rec.r#type, VmssType::Checkpoint);
    assert_eq!(rec.retained_snapshots, 4);
    assert_eq!(rec.schedule.get("hour").map(String::as_str), Some("5"));
    assert_eq!(rec.schedule.get("days").map(String::as_str), Some("monday"));
    assert!(!rec.enabled, "editing does not enable");
    // Read back for the page: this console's time.
    let back = ScheduleChoice::from_policy(&rec, time);
    assert_eq!((back.weekly, back.days.clone()), ((3, 0), vec![1]));

    // A new policy without VMs is disabled; assigning a VM keeps it so.
    let spec = NewPolicySpec {
        name: "empty".into(),
        description: String::new(),
        kind: VmssType::Snapshot,
        frequency: VmssFrequency::Daily,
        schedule: ScheduleChoice::default()
            .compute(server.clock(), Some(time))
            .schedule,
        retained: 7,
        enabled: false,
        vms: Vec::new(),
        run_now: false,
    };
    ok(&run(&h, conn, ActionRequest::NewPolicy(Box::new(spec))).await);
    let state = h
        .wait_for("empty policy", |s| {
            view(s, conn)
                .model
                .vmss
                .values()
                .any(|p| p.name_label == "empty")
        })
        .await;
    let (empty, rec) = view(&state, conn)
        .model
        .vmss
        .iter()
        .find(|(_, p)| p.name_label == "empty")
        .map(|(r, p)| (r.clone(), p.clone()))
        .unwrap();
    assert!(!rec.enabled);
    assert!(!vmss::can_toggle(&rec) && !vmss::can_run_now(&rec));
    // Moving web to it is asked about first.
    assert!(matches!(
        vmss::change_warning(
            &view(&state, conn).model,
            std::slice::from_ref(&web),
            Some(&empty),
            "empty"
        ),
        Some(xen_core::vapp::ChangeWarning::One { .. })
    ));
    ok(&run(
        &h,
        conn,
        ActionRequest::AssignPolicy {
            policy: empty.clone(),
            vms: vec![web.clone()],
        },
    )
    .await);
    let state = h
        .wait_for("moved", |s| {
            view(s, conn)
                .model
                .vms
                .get(&web)
                .is_some_and(|v| v.snapshot_schedule == empty)
        })
        .await;
    assert!(!view(&state, conn).model.vmss.get(&empty).unwrap().enabled);
    ok(&run(
        &h,
        conn,
        ActionRequest::RemoveFromPolicy {
            policy: empty.clone(),
            vms: vec![web.clone()],
        },
    )
    .await);
    // Deleting releases the VMs.
    ok(&run(
        &h,
        conn,
        ActionRequest::DeletePolicies {
            policies: vec![policy.clone(), empty.clone()],
        },
    )
    .await);
    let state = h
        .wait_for("deleted", |s| view(s, conn).model.vmss.is_empty())
        .await;
    assert!(
        view(&state, conn)
            .model
            .vms
            .get(&db)
            .unwrap()
            .snapshot_schedule
            .is_null()
    );
}

// ---- disaster recovery -------------------------------------------------------------------

struct Site {
    server: MockServer,
    conn: ConnectionId,
    session: Session,
    tag: &'static str,
}

/// A shared NFS SR at `/exports/<path>` on `site`.
async fn nfs_sr(site: &Site, path: &str, name: &str) -> String {
    site.session
        .call(
            "SR.create",
            vec![
                json!(format!("OpaqueRef:host-0001-{}", site.tag)),
                json!({"server": "nfs.lab", "serverpath": format!("/exports/{path}")}),
                json!(0),
                json!(name),
                json!(""),
                json!("nfs"),
                json!("user"),
                json!(true),
                json!({}),
            ],
        )
        .await
        .unwrap()
}

/// A halted VM with one disk on `sr` and one interface on eth0.
async fn dr_vm(site: &Site, name: &str, sr: &str, mac: &str) -> String {
    let s = &site.session;
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
            vec![
                json!({"name_label": format!("{name} disk"), "SR": sr, "virtual_size": 1048576,
                "type": "user", "sharable": false, "read_only": false}),
            ],
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
                json!({"VM": vm, "network": format!("OpaqueRef:net-eth0-{}", site.tag),
                "device": "0", "MAC": mac, "MTU": 1500}),
            ],
        )
        .await
        .unwrap();
    vm
}

fn uuid_of(server: &MockServer, class: &str, r: &str) -> String {
    server
        .field(class, r, "uuid")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The VM with `uuid` in a connection's cache.
fn vm_by_uuid(
    state: &AppState,
    conn: ConnectionId,
    uuid: &str,
) -> Option<(Ref<xapi::Vm>, xapi::VmRecord)> {
    view(state, conn)
        .model
        .vms
        .iter()
        .find(|(_, v)| v.uuid == uuid)
        .map(|(r, v)| (r.clone(), (**v).clone()))
}

/// All the pre-checks of the wizard for `chosen` (`GenerateChecks`): the
/// cache ones, then the server's storage check; the problems found.
async fn prechecks(
    h: &mut common::Harness,
    conn: ConnectionId,
    wizard: WizardType,
    selected: &[(dr::PoolMetadata, Chosen)],
) -> Vec<Problem> {
    let state = h.backend.state();
    let sources: BTreeMap<String, (ConnectionId, Arc<xen_core::ConnectionModel>)> = state
        .connections
        .iter()
        .filter(|c| c.id != conn && c.status.is_connected())
        .filter_map(|c| {
            let (_, p) = c.model.pool()?;
            Some((p.uuid.clone(), (c.id, c.model.clone())))
        })
        .collect();
    let model = view(&state, conn).model.clone();
    let mut problems: Vec<Problem> = dr::cache_checks(wizard, &model, selected, &sources)
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().filter_map(|r| r.problem))
        .collect();
    for (i, (meta, chosen)) in selected.iter().enumerate() {
        let items = chosen.items(meta);
        match ask(
            h,
            conn,
            100 + i as u64,
            Precheck::DrRecoverable {
                vdi: meta.vdi.clone(),
                items,
            },
        )
        .await
        {
            PrecheckResult::DrRecoverable(Ok(rows)) => {
                problems.extend(rows.iter().filter_map(|(_, a)| dr::recoverable_problem(a)));
            }
            other => panic!("recoverable: {other:?}"),
        }
    }
    problems
}

async fn metadata(
    h: &mut common::Harness,
    conn: ConnectionId,
    srs: Vec<String>,
) -> Vec<dr::PoolMetadata> {
    match ask(h, conn, 50, Precheck::DrMetadata { srs }).await {
        PrecheckResult::DrMetadata(Ok(m)) => m,
        other => panic!("metadata: {other:?}"),
    }
}

/// Recovers `items` of `meta` one by one (the Recover page).
async fn recover(
    h: &common::Harness,
    conn: ConnectionId,
    meta: &dr::PoolMetadata,
    items: &[DrItem],
) -> Vec<Operation> {
    let mut out = Vec::new();
    for item in items {
        out.push(
            run(
                h,
                conn,
                ActionRequest::DrRecover {
                    vdi: meta.vdi.clone(),
                    item: item.clone(),
                    name: dr::item_name(meta, item),
                },
            )
            .await,
        );
    }
    out
}

#[tokio::test]
async fn disaster_recovery_between_two_pools() {
    let a = MockServer::start_lab(false, "dra", 1).await;
    let b = MockServer::start_lab(false, "drb", 2).await;
    for s in [&a, &b] {
        s.set_task_timing(1, Duration::from_millis(5));
    }
    let mut h = harness();
    let a = Site {
        conn: connect(&h, &a).await,
        session: login(&a).await,
        server: a,
        tag: "dra",
    };
    let b = Site {
        conn: connect(&h, &b).await,
        session: login(&b).await,
        server: b,
        tag: "drb",
    };
    // Pool A: a shared SR with the metadata, VMs on it, a vApp, a VM with
    // a disk on its local SR.
    let shared = nfs_sr(&a, "dr-core", "DR NFS").await;
    let other = nfs_sr(&a, "dr-core-other", "Other NFS").await;
    let app = dr_vm(&a, "app", &shared, "02:00:00:0a:00:01").await;
    let db = dr_vm(&a, "db", &shared, "02:00:00:0a:00:02").await;
    let web = dr_vm(&a, "web", &other, "02:00:00:0a:00:03").await;
    let local = dr_vm(
        &a,
        "local",
        "OpaqueRef:sr-local-01-dra",
        "02:00:00:0a:00:04",
    )
    .await;
    let vapp: String = a
        .session
        .call("VM_appliance.create", vec![json!({"name_label": "stack"})])
        .await
        .unwrap();
    let _: Value = a
        .session
        .call("VM.set_appliance", vec![json!(db), json!(vapp)])
        .await
        .unwrap();
    let uuids: BTreeMap<&str, String> =
        [("app", &app), ("db", &db), ("web", &web), ("local", &local)]
            .into_iter()
            .map(|(n, r)| (n, uuid_of(&a.server, "vm", r)))
            .collect();
    let shared_uuid = uuid_of(&a.server, "sr", &shared);
    let other_uuid = uuid_of(&a.server, "sr", &other);
    let state = h
        .wait_for("A's SRs", |s| {
            let m = &view(s, a.conn).model;
            m.srs.contains_key(&r(&shared)) && m.vms.contains_key(&r(&local))
        })
        .await;

    // ---- Configure: the shared SRs can hold the metadata, the local one not.
    let candidates = dr::candidate_srs(&view(&state, a.conn).model);
    let supported = match ask(
        &mut h,
        a.conn,
        1,
        Precheck::DrSupportedSrs { srs: candidates },
    )
    .await
    {
        PrecheckResult::DrSupportedSrs(rows) => rows,
        other => panic!("{other:?}"),
    };
    let ok_srs: Vec<String> = supported
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(s, _)| s.to_string())
        .collect();
    assert!(ok_srs.contains(&shared) && ok_srs.contains(&other));
    assert!(!ok_srs.contains(&"OpaqueRef:sr-local-01-dra".to_owned()));
    ok(&run(
        &h,
        a.conn,
        ActionRequest::ConfigureDr {
            enable: vec![r(&shared)],
            disable: vec![],
        },
    )
    .await);
    let state = h
        .wait_for("replicating", |s| {
            dr::is_dr_enabled(&view(s, a.conn).model, &r(&shared))
        })
        .await;
    assert!(!dr::is_dr_enabled(&view(&state, a.conn).model, &r(&other)));
    a.server.sync_metadata();

    // ---- Test failover (dry run) on B. The Storage page's scan finds the SR.
    let dconf = xen_core::storage::DeviceConfig(vec![
        ("server".into(), "nfs.lab".into()),
        ("serverpath".into(), "/exports/dr-core".into()),
    ]);
    let found = match ask(
        &mut h,
        b.conn,
        2,
        Precheck::DrProbe {
            sr_type: "nfs".into(),
            device_config: dconf.clone(),
        },
    )
    .await
    {
        PrecheckResult::DrProbe(Ok(p)) => p.srs,
        other => panic!("{other:?}"),
    };
    let found = found
        .iter()
        .find(|s| s.uuid == shared_uuid)
        .expect("the shared SR");
    assert!(found.pool_metadata_detected);
    let rows = dr::storage_rows(
        &view(&h.backend.state(), b.conn).model,
        &[],
        &[dr::ScannedSr {
            uuid: found.uuid.clone(),
            name_label: found.name_label.clone(),
            name_description: String::new(),
            pool_metadata_detected: true,
            sr_type: "nfs".into(),
            device_config: dconf.clone(),
        }],
    );
    let tasks = dr::devices_to_introduce(&rows, std::slice::from_ref(&shared_uuid));
    assert_eq!(tasks.len(), 1);
    let op = run(
        &h,
        b.conn,
        ActionRequest::DrTaskCreate(Box::new(tasks[0].clone())),
    )
    .await;
    ok(&op);
    let dr_task: Ref<xapi::DrTask> = r(op.result.as_deref().unwrap());
    let metas = metadata(&mut h, b.conn, vec![shared_uuid.clone()]).await;
    assert_eq!(metas.len(), 1, "A's metadata");
    let meta = metas[0].clone();
    let names: Vec<&str> = meta
        .vms
        .iter()
        .map(|(_, v)| v.name_label.as_str())
        .collect();
    for n in ["app", "db", "web", "local"] {
        assert!(names.contains(&n), "{n} is recoverable");
    }
    assert_eq!(meta.vm_appliances.len(), 1);
    let meta_vapp = meta.vm_appliances[0].0.clone();
    let loose: Vec<&str> = meta
        .loose_vms()
        .iter()
        .map(|(_, v)| v.name_label.as_str())
        .collect();
    assert!(!loose.contains(&"db"), "db shows under its vApp");
    let meta_vm = |name: &str| {
        meta.vms
            .iter()
            .find(|(_, v)| v.name_label == name)
            .unwrap()
            .0
            .clone()
    };

    // Pre-checks of a dry run: HA off, nothing here, local storage and a
    // missing shared SR are problems.
    let all = Chosen {
        vapps: vec![meta_vapp.clone()],
        vms: vec![meta_vm("app"), meta_vm("web"), meta_vm("local")],
    };
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Dryrun,
        &[(meta.clone(), all.clone())],
    )
    .await;
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::MissingSr { sr: Some(s), fix: None } if !s.shared))
    );
    let fix = problems
        .iter()
        .find_map(|p| match p {
            Problem::MissingSr {
                sr: Some(s),
                fix: Some(f),
            } if s.uuid == other_uuid => Some(f.clone()),
            _ => None,
        })
        .expect("the other SR can be attached");
    // HA on in this pool is a problem of a dry run.
    let pool_b = view(&h.backend.state(), b.conn)
        .model
        .pool()
        .map(|(r, _)| r.to_string())
        .unwrap();
    b.server
        .set_field("pool", &pool_b, "ha_enabled", json!(true));
    h.wait_for("HA on", |s| {
        view(s, b.conn)
            .model
            .pool()
            .is_some_and(|(_, p)| p.ha_enabled)
    })
    .await;
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Dryrun,
        &[(meta.clone(), all.clone())],
    )
    .await;
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::HaEnabled { .. }))
    );
    b.server
        .set_field("pool", &pool_b, "ha_enabled", json!(false));
    h.wait_for("HA off", |s| {
        view(s, b.conn)
            .model
            .pool()
            .is_some_and(|(_, p)| !p.ha_enabled)
    })
    .await;
    // Resolving the missing SR: a DR_task for it.
    let op = run(&h, b.conn, ActionRequest::DrTaskCreate(Box::new(fix))).await;
    ok(&op);
    let fix_task: Ref<xapi::DrTask> = r(op.result.as_deref().unwrap());
    let chosen = Chosen {
        vapps: vec![meta_vapp.clone()],
        vms: vec![meta_vm("app"), meta_vm("web")],
    };
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Dryrun,
        &[(meta.clone(), chosen.clone())],
    )
    .await;
    assert!(problems.is_empty(), "{problems:#?}");

    // Recover: the vApp first, then the VMs; a failure in the middle
    // leaves nothing of that item.
    b.server.fail_recovery(&uuids["web"], Some("VDI_MISSING"));
    let items = chosen.items(&meta);
    assert_eq!(items[0], DrItem::VApp(meta_vapp.clone()));
    let ops = recover(&h, b.conn, &meta, &items).await;
    ok(&ops[0]);
    ok(&ops[1]);
    failed_with(&ops[2], "VDI_MISSING");
    b.server.fail_recovery(&uuids["web"], None);
    let state = h
        .wait_for("recovered", |s| {
            vm_by_uuid(s, b.conn, &uuids["app"]).is_some()
                && vm_by_uuid(s, b.conn, &uuids["db"]).is_some()
        })
        .await;
    assert!(vm_by_uuid(&state, b.conn, &uuids["web"]).is_none());
    let (b_app, b_app_rec) = vm_by_uuid(&state, b.conn, &uuids["app"]).unwrap();
    assert_eq!(b_app_rec.vbds.len(), 1);
    assert_eq!(b_app_rec.vifs.len(), 1);
    let b_vapp = view(&state, b.conn)
        .model
        .vm_appliances
        .iter()
        .find(|(_, a)| a.name_label == "stack")
        .map(|(r, _)| r.clone())
        .unwrap();
    // A test failover starts them paused.
    ok(&run(
        &h,
        b.conn,
        ActionRequest::DrStart {
            vms: vec![b_app.clone()],
            vapps: vec![b_vapp.clone()],
            paused: true,
        },
    )
    .await);
    h.wait_for("paused", |s| {
        vm_by_uuid(s, b.conn, &uuids["app"])
            .is_some_and(|(_, v)| v.power_state == xapi::VmPowerState::Paused)
    })
    .await;
    // Clean-up: the DR tasks (their SRs stay while VMs use them), the
    // recovered VMs and vApps, then the SRs introduced.
    for t in [&dr_task, &fix_task] {
        ok(&run(
            &h,
            b.conn,
            ActionRequest::DrTaskDestroy {
                task: t.clone(),
                srs: String::new(),
            },
        )
        .await);
    }
    ok(&run(
        &h,
        b.conn,
        ActionRequest::DrDestroyVms {
            vms: vec![b_app.clone()],
        },
    )
    .await);
    ok(&run(
        &h,
        b.conn,
        ActionRequest::DrDestroyVApps {
            vapps: vec![b_vapp.clone()],
        },
    )
    .await);
    let state = h
        .wait_for("cleaned", |s| {
            vm_by_uuid(s, b.conn, &uuids["app"]).is_none()
                && vm_by_uuid(s, b.conn, &uuids["db"]).is_none()
                && view(s, b.conn).model.vm_appliances.is_empty()
        })
        .await;
    let introduced: Vec<Ref<xapi::Sr>> = view(&state, b.conn)
        .model
        .srs
        .iter()
        .filter(|(_, s)| s.uuid == shared_uuid || s.uuid == other_uuid)
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(introduced.len(), 1, "the unused SR went with its DR task");
    // `DetachAndForgetSRs`: detached, then forgotten.
    for sr in introduced {
        for action in [
            xen_core::storage::SrAction::Detach,
            xen_core::storage::SrAction::Forget,
        ] {
            ok(&run(
                &h,
                b.conn,
                ActionRequest::Sr {
                    sr: sr.clone(),
                    action,
                },
            )
            .await);
            h.wait_for("SR detached", |s| {
                view(s, b.conn).model.srs.get(&sr).is_none_or(|x| {
                    x.allowed_operations
                        .contains(&xapi::StorageOperations::Forget)
                })
            })
            .await;
        }
    }
    h.wait_for("B as before", |s| {
        view(s, b.conn)
            .model
            .srs
            .values()
            .all(|x| x.uuid != shared_uuid && x.uuid != other_uuid)
            && view(s, b.conn).model.dr_tasks.is_empty()
    })
    .await;
    // Pool A was not touched.
    for n in ["app", "db", "web", "local"] {
        assert!(vm_by_uuid(&h.backend.state(), a.conn, &uuids[n]).is_some());
    }

    // ---- A cancelled recovery recovers nothing.
    let op = run(
        &h,
        b.conn,
        ActionRequest::DrTaskCreate(Box::new(tasks[0].clone())),
    )
    .await;
    ok(&op);
    let dr_task: Ref<xapi::DrTask> = r(op.result.as_deref().unwrap());
    let meta = metadata(&mut h, b.conn, vec![shared_uuid.clone()])
        .await
        .remove(0);
    b.server.set_task_timing(20, Duration::from_millis(50));
    let before = last_id(&h);
    h.backend.send(Command::Action {
        connection: b.conn,
        request: ActionRequest::DrRecover {
            vdi: meta.vdi.clone(),
            item: DrItem::Vm(meta_vm("app")),
            name: "app".into(),
        },
    });
    let state = h
        .wait_for("recovery task", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && o.task.is_some())
        })
        .await;
    let op_id = state.operations.iter().find(|o| o.id > before).unwrap().id;
    h.backend
        .send(Command::CancelOperation { operation: op_id });
    let op = ended_after(&h, before).await;
    assert!(
        matches!(op.status, OperationStatus::Cancelled),
        "{:?}",
        op.status
    );
    assert!(vm_by_uuid(&h.backend.state(), b.conn, &uuids["app"]).is_none());
    b.server.set_task_timing(1, Duration::from_millis(5));

    // ---- Failover to B: app runs on A, which is a problem of the source
    // pool; it is shut down there, recovered here and started.
    let start_on_a = run(
        &h,
        a.conn,
        ActionRequest::Vm {
            vm: r(&app),
            action: VmAction::Start,
        },
    )
    .await;
    ok(&start_on_a);
    h.wait_for("app runs on A", |s| {
        view(s, a.conn)
            .model
            .vms
            .get(&r::<xapi::Vm>(&app))
            .is_some_and(|v| v.power_state == xapi::VmPowerState::Running)
    })
    .await;
    let just_app = Chosen {
        vapps: vec![],
        vms: vec![meta_vm("app")],
    };
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Failover,
        &[(meta.clone(), just_app.clone())],
    )
    .await;
    let Some(Problem::RunningVm { at, vm, clean, .. }) = problems.first().cloned() else {
        panic!("{problems:#?}");
    };
    assert_eq!(at, dr::Where::Source(a.conn));
    let action = if clean {
        VmAction::CleanShutdown
    } else {
        VmAction::HardShutdown
    };
    ok(&run(&h, a.conn, ActionRequest::Vm { vm, action }).await);
    h.wait_for("app halted on A", |s| {
        view(s, a.conn)
            .model
            .vms
            .get(&r::<xapi::Vm>(&app))
            .is_some_and(|v| v.power_state == xapi::VmPowerState::Halted)
    })
    .await;
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Failover,
        &[(meta.clone(), just_app.clone())],
    )
    .await;
    assert!(problems.is_empty(), "{problems:#?}");
    for op in recover(&h, b.conn, &meta, &just_app.items(&meta)).await {
        ok(&op);
    }
    let state = h
        .wait_for("failed over", |s| {
            vm_by_uuid(s, b.conn, &uuids["app"]).is_some()
        })
        .await;
    let (b_app, rec) = vm_by_uuid(&state, b.conn, &uuids["app"]).unwrap();
    let a_version = view(&state, a.conn)
        .model
        .vms
        .get(&r::<xapi::Vm>(&app))
        .unwrap()
        .version;
    assert_eq!(rec.version, a_version + 1);
    ok(&run(
        &h,
        b.conn,
        ActionRequest::DrStart {
            vms: vec![b_app.clone()],
            vapps: vec![],
            paused: false,
        },
    )
    .await);
    ok(&run(
        &h,
        b.conn,
        ActionRequest::DrTaskDestroy {
            task: dr_task.clone(),
            srs: String::new(),
        },
    )
    .await);
    let state = h
        .wait_for("running on B", |s| {
            vm_by_uuid(s, b.conn, &uuids["app"])
                .is_some_and(|(_, v)| v.power_state == xapi::VmPowerState::Running)
                && view(s, b.conn).model.dr_tasks.is_empty()
        })
        .await;
    assert!(
        view(&state, b.conn)
            .model
            .srs
            .values()
            .any(|x| x.uuid == shared_uuid),
        "the SR stays: the recovered VM uses it"
    );
    // Recovering again over the newer VM is a problem; deleting it fixes it.
    let problems = prechecks(
        &mut h,
        b.conn,
        WizardType::Failover,
        &[(meta.clone(), just_app.clone())],
    )
    .await;
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::ExistingVm { .. })),
        "{problems:#?}"
    );

    // ---- Failback to A: B replicates to the SR; A finds B's metadata (its
    // own is in use), app runs on B (source), A has an older app (warning).
    let state = h.backend.state();
    let b_shared = view(&state, b.conn)
        .model
        .srs
        .iter()
        .find(|(_, s)| s.uuid == shared_uuid)
        .map(|(r, _)| r.clone())
        .unwrap();
    ok(&run(
        &h,
        b.conn,
        ActionRequest::ConfigureDr {
            enable: vec![b_shared.clone()],
            disable: vec![],
        },
    )
    .await);
    b.server.sync_metadata();
    // XAPI's periodic scan lists B's metadata VDI on A.
    a.server.sync_metadata();
    let metas = metadata(&mut h, a.conn, vec![shared_uuid.clone()]).await;
    assert_eq!(metas.len(), 1, "B's metadata only: A's own is in use");
    let back = metas[0].clone();
    assert_eq!(
        back.pool.uuid,
        view(&h.backend.state(), b.conn)
            .model
            .pool()
            .unwrap()
            .1
            .uuid
    );
    let back_app = back
        .vms
        .iter()
        .find(|(_, v)| v.uuid == uuids["app"])
        .unwrap()
        .0
        .clone();
    let chosen = Chosen {
        vapps: vec![],
        vms: vec![back_app.clone()],
    };
    let problems = prechecks(
        &mut h,
        a.conn,
        WizardType::Failback,
        &[(back.clone(), chosen.clone())],
    )
    .await;
    let running = problems.iter().find_map(|p| match p {
        Problem::RunningVm {
            at: dr::Where::Source(c),
            vm,
            ..
        } => Some((*c, vm.clone())),
        _ => None,
    });
    assert_eq!(running.as_ref().map(|r| r.0), Some(b.conn), "{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::ExistingWarning { .. }))
    );
    ok(&run(
        &h,
        b.conn,
        ActionRequest::Vm {
            vm: running.unwrap().1,
            action: VmAction::HardShutdown,
        },
    )
    .await);
    h.wait_for("halted on B", |s| {
        vm_by_uuid(s, b.conn, &uuids["app"])
            .is_some_and(|(_, v)| v.power_state == xapi::VmPowerState::Halted)
    })
    .await;
    b.server.sync_metadata();
    let problems = prechecks(
        &mut h,
        a.conn,
        WizardType::Failback,
        &[(back.clone(), chosen.clone())],
    )
    .await;
    assert!(problems.iter().all(Problem::is_warning), "{problems:#?}");
    for op in recover(&h, a.conn, &back, &chosen.items(&back)).await {
        ok(&op);
    }
    let state = h
        .wait_for("failed back", |s| {
            vm_by_uuid(s, a.conn, &uuids["app"]).is_some_and(|(_, v)| v.version == a_version + 2)
        })
        .await;
    let (a_app, _) = vm_by_uuid(&state, a.conn, &uuids["app"]).unwrap();
    ok(&run(
        &h,
        a.conn,
        ActionRequest::DrStart {
            vms: vec![a_app],
            vapps: vec![],
            paused: false,
        },
    )
    .await);
    h.wait_for("running on A", |s| {
        vm_by_uuid(s, a.conn, &uuids["app"])
            .is_some_and(|(_, v)| v.power_state == xapi::VmPowerState::Running)
    })
    .await;
    // Nothing to start fails as the original's action does.
    let op = run(
        &h,
        a.conn,
        ActionRequest::DrStart {
            vms: vec![],
            vapps: vec![],
            paused: false,
        },
    )
    .await;
    failed_with(&op, dr::NOTHING_TO_START);
    // Turning replication off removes the metadata VDI.
    ok(&run(
        &h,
        a.conn,
        ActionRequest::ConfigureDr {
            enable: vec![],
            disable: vec![r(&shared)],
        },
    )
    .await);
    h.wait_for("not replicating", |s| {
        !dr::is_dr_enabled(&view(s, a.conn).model, &r(&shared))
    })
    .await;
}
