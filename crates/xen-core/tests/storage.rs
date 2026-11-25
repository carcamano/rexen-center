//! F8: storage repositories against the mock (backend only, no UI): the
//! probes and iSCSI scans, every New SR type (create, introduce, reattach),
//! every SR operation, the XAPI calls made, what is left on the server,
//! failures and rollbacks.

mod common;

use common::{first, harness};
use serde_json::{Value, json};
use xapi::{Ref, Sr};
use xapi_mock::MockServer;
use xapi_mock::storage::{
    CHAP_PASSWORD, CHAP_USER, OLD_ISCSI_SR, OLD_NFS_SR, SMB_PASSWORD, SMB_USER,
};
use xen_core::operations::{OperationStep, Precheck, PrecheckResult, StorageKind};
use xen_core::storage::{
    self, DeviceConfig, IqnInfo, LocalFs, LunInfo, NewSrSpec, ProbeResult, Secret, SrAction,
    SrChanges, SrKind, errors,
};
use xen_core::{
    ActionRequest, Command, ConnectionId, Notification, Operation, OperationKind, OperationStatus,
};

const NFS: &str = "OpaqueRef:sr-nfs";
const ISO: &str = "OpaqueRef:sr-iso";
const LOCAL1: &str = "OpaqueRef:sr-local-01";
const LOCAL2: &str = "OpaqueRef:sr-local-02";
const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";
const POOL: &str = "OpaqueRef:pool-0001";

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
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
    h.backend.send(Command::Action {
        connection: conn,
        request,
    });
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running())
        })
        .await;
    let op = state.operations.iter().find(|o| o.id > before).unwrap();
    (**op).clone()
}

/// Runs a precheck and returns its answer.
async fn precheck(h: &mut common::Harness, conn: ConnectionId, check: Precheck) -> PrecheckResult {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

async fn probe(
    h: &mut common::Harness,
    conn: ConnectionId,
    sr_type: &str,
    dc: Vec<(String, String)>,
) -> Result<ProbeResult, String> {
    match precheck(
        h,
        conn,
        Precheck::SrProbe {
            sr_type: sr_type.into(),
            device_config: DeviceConfig(dc),
        },
    )
    .await
    {
        PrecheckResult::SrProbe(r) => r.map_err(|e| code(&e)),
        other => panic!("not a probe answer: {other:?}"),
    }
}

async fn iqns(
    h: &mut common::Harness,
    conn: ConnectionId,
    target: &str,
    chap: Option<(&str, &str)>,
) -> Result<Vec<IqnInfo>, String> {
    let chap = chap.map(|(u, p)| (u.to_owned(), Secret(p.to_owned())));
    match precheck(
        h,
        conn,
        Precheck::IscsiIqns {
            target: target.into(),
            port: 3260,
            chap,
        },
    )
    .await
    {
        PrecheckResult::IscsiIqns(r) => r.map_err(|e| code(&e)),
        other => panic!("not an IQN answer: {other:?}"),
    }
}

async fn luns(
    h: &mut common::Harness,
    conn: ConnectionId,
    iqn: &IqnInfo,
    chap: Option<(&str, &str)>,
) -> Result<Vec<LunInfo>, String> {
    let chap = chap.map(|(u, p)| (u.to_owned(), Secret(p.to_owned())));
    match precheck(
        h,
        conn,
        Precheck::IscsiLuns {
            target: iqn.address.clone(),
            port: iqn.port,
            iqn: iqn.target_iqn.clone(),
            chap,
        },
    )
    .await
    {
        PrecheckResult::IscsiLuns(r) => r.map_err(|e| code(&e)),
        other => panic!("not a LUN answer: {other:?}"),
    }
}

fn code(e: &xapi::Error) -> String {
    match e {
        xapi::Error::Api { code, .. } => code.clone(),
        other => other.to_string(),
    }
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
            assert!(e.is_api(code), "{:?}: expected {code}, got {e}", op.kind)
        }
        other => panic!("{:?}: expected failure {code}, got {other:?}", op.kind),
    }
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> Value {
    server.field(class, r, f).unwrap_or(Value::Null)
}

fn refs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn sr_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("sr")
        .into_iter()
        .find(|s| field(server, "sr", s, "name_label") == json!(name))
}

fn sr_with_uuid(server: &MockServer, uuid: &str) -> Option<String> {
    server
        .refs("sr")
        .into_iter()
        .find(|s| field(server, "sr", s, "uuid") == json!(uuid))
}

/// `(host, attached, device_config)` of each PBD of an SR, by host.
fn pbds(server: &MockServer, sr: &str) -> Vec<(String, bool, Value)> {
    let mut out: Vec<_> = refs(&field(server, "sr", sr, "PBDs"))
        .iter()
        .map(|p| {
            let rec = server.record("pbd", p).unwrap();
            (
                rec["host"].as_str().unwrap().to_owned(),
                rec["currently_attached"].as_bool().unwrap(),
                rec["device_config"].clone(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn spec(kind: SrKind, name: &str, dc: Vec<(String, String)>) -> NewSrSpec {
    NewSrSpec {
        kind,
        name: name.into(),
        description: format!("{name} (test)"),
        device_config: dc,
        sm_config: Vec::new(),
        uuid: None,
        reattach: None,
    }
}

fn new_sr(spec: NewSrSpec) -> ActionRequest {
    ActionRequest::NewSr(Box::new(spec))
}

fn sr_op(sr: &str, action: SrAction) -> ActionRequest {
    ActionRequest::Sr { sr: r(sr), action }
}

/// Waits until the client's cache sees `pred` on the SR named `name`.
async fn wait_sr(
    h: &common::Harness,
    what: &str,
    pred: impl Fn(&xen_core::ConnectionView) -> bool,
) {
    h.wait_conn(what, pred).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nfs_probe_then_create_a_new_sr() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;

    // The Scan button: an export with no SR, and the versions the server
    // supports.
    let found = probe(
        &mut h,
        conn,
        "nfs",
        storage::nfs_probe_config("nfs.lab:/exports/new", ""),
    )
    .await
    .unwrap();
    assert!(found.srs.is_empty());
    assert_eq!(found.nfs_versions, ["3", "4", "4.1"]);
    // Bad server, bad path.
    assert_eq!(
        probe(
            &mut h,
            conn,
            "nfs",
            storage::nfs_probe_config("nas.lab:/x", "")
        )
        .await
        .unwrap_err(),
        "SR_BACKEND_FAILURE_140"
    );
    assert_eq!(
        probe(
            &mut h,
            conn,
            "nfs",
            storage::nfs_probe_config("nfs.lab:/srv", "")
        )
        .await
        .unwrap_err(),
        "SR_BACKEND_FAILURE_73"
    );
    // The probe made no SR and left no task.
    assert!(sr_named(&server, "__gui__").is_none());

    let srs_before = server.refs("sr").len();
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Nfs,
            "lab-nfs",
            storage::nfs_device_config("nfs.lab:/exports/new", "", "4.1"),
        )),
    )
    .await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Storage(StorageKind::CreateSr {
            sr_type: "nfs".into(),
            name: "lab-nfs".into(),
            pool: "Lab Pool".into()
        })
    );
    assert_eq!(server.refs("sr").len(), srs_before + 1);
    let sr = sr_named(&server, "lab-nfs").unwrap();
    let rec = server.record("sr", &sr).unwrap();
    assert_eq!(rec["type"], json!("nfs"));
    assert_eq!(rec["content_type"], json!(""));
    assert_eq!(rec["shared"], json!(true));
    assert_eq!(rec["name_description"], json!("lab-nfs (test)"));
    assert_eq!(rec["other_config"]["auto-scan"], json!("false"));
    // One plugged PBD per host, with the page's device_config.
    let p = pbds(&server, &sr);
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|(_, attached, _)| *attached));
    assert_eq!(p[0].2["server"], json!("nfs.lab"));
    assert_eq!(p[0].2["serverpath"], json!("/exports/new"));
    assert_eq!(p[0].2["nfsversion"], json!("4.1"));
    // The pool already has a shared SR: the default stays.
    assert_eq!(field(&server, "pool", POOL, "default_SR"), json!(NFS));
    // No password, no secret; the original's order.
    assert_eq!(server.call_count("secret.create"), 0);
    let calls = server.calls();
    let pos = |m: &str| calls.iter().position(|c| c == m).unwrap();
    assert!(pos("Async.SR.create") < pos("SR.get_PBDs"));
    assert!(pos("SR.get_PBDs") < pos("SR.set_other_config"));
    assert_eq!(
        server.call_count("PBD.plug"),
        0,
        "both PBDs plugged by the create"
    );

    // The new SR is there for a later probe of the same export.
    let uuid = rec["uuid"].as_str().unwrap().to_owned();
    let found = probe(
        &mut h,
        conn,
        "nfs",
        storage::nfs_probe_config("nfs.lab:/exports/new", ""),
    )
    .await
    .unwrap();
    assert_eq!(found.srs.len(), 1);
    assert_eq!(found.srs[0].uuid, uuid);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_shared_sr_becomes_the_default() {
    let server = MockServer::start(false).await;
    let h = harness();
    // The pool's NFS SR is not shared here: the new one is the first.
    server.set_field("sr", NFS, "shared", json!(false));
    let conn = connect(&h, &server).await;
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Nfs,
            "first-shared",
            storage::nfs_device_config("nfs.lab:/exports/first", "", "3"),
        )),
    )
    .await;
    ok(&op);
    let sr = sr_named(&server, "first-shared").unwrap();
    for f in ["default_SR", "suspend_image_SR", "crash_dump_SR"] {
        assert_eq!(field(&server, "pool", POOL, f), json!(sr), "{f}");
    }
    assert!(
        !pbds(&server, &sr)[0]
            .2
            .as_object()
            .unwrap()
            .contains_key("nfsversion")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn smb_passwords_go_through_a_secret_destroyed_afterwards() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    let share = r"\\smb.lab\vms";
    // The Scan button with credentials: nothing there yet.
    let found = probe(
        &mut h,
        conn,
        "smb",
        storage::smb_device_config(share, SMB_USER, SMB_PASSWORD),
    )
    .await
    .unwrap();
    assert!(found.srs.is_empty());
    assert_eq!(
        probe(
            &mut h,
            conn,
            "smb",
            storage::smb_device_config(share, SMB_USER, "wrong")
        )
        .await
        .unwrap_err(),
        "SR_BACKEND_FAILURE_111"
    );

    // A wrong password: the create fails, the secret is gone anyway.
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Smb,
            "bad-smb",
            storage::smb_device_config(share, SMB_USER, "wrong"),
        )),
    )
    .await;
    failed_with(&op, "SR_BACKEND_FAILURE_111");
    assert!(sr_named(&server, "bad-smb").is_none());
    assert_eq!(server.call_count("secret.create"), 1);
    assert!(server.refs("secret").is_empty());

    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Smb,
            "lab-smb",
            storage::smb_device_config(share, SMB_USER, SMB_PASSWORD),
        )),
    )
    .await;
    ok(&op);
    let sr = sr_named(&server, "lab-smb").unwrap();
    assert_eq!(field(&server, "sr", &sr, "type"), json!("smb"));
    for (_, attached, dc) in pbds(&server, &sr) {
        assert!(attached);
        let dc = dc.as_object().unwrap();
        assert_eq!(dc["username"], json!(SMB_USER));
        assert!(dc.contains_key("password_secret"), "{dc:?}");
        assert!(
            !dc.contains_key("password"),
            "the password is not stored in clear"
        );
    }
    // The PBDs copied the secret; the console destroys its own.
    assert!(server.refs("secret").is_empty());
    assert_eq!(server.call_count("secret.destroy"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn iso_libraries_on_nfs_and_smb() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::NfsIso,
            "nfs-isos",
            storage::nfs_iso_device_config("nfs.lab:/exports/iso2", true),
        )),
    )
    .await;
    ok(&op);
    let sr = sr_named(&server, "nfs-isos").unwrap();
    let rec = server.record("sr", &sr).unwrap();
    assert_eq!(rec["type"], json!("iso"));
    assert_eq!(rec["content_type"], json!("iso"));
    assert_eq!(rec["sm_config"]["iso_type"], json!("nfs_iso"));
    assert_eq!(rec["other_config"]["auto-scan"], json!("true"));
    assert_eq!(pbds(&server, &sr)[0].2["nfsversion"], json!("4"));
    // ISO libraries never become the default SR.
    assert_eq!(field(&server, "pool", POOL, "default_SR"), json!(NFS));

    // A private SMB share needs the credentials (a secret); a wrong share
    // name fails.
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::SmbIso,
            "smb-private",
            storage::smb_iso_device_config(r"\\smb.lab\private", None),
        )),
    )
    .await;
    failed_with(&op, "SR_BACKEND_FAILURE_222");
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::SmbIso,
            "smb-private",
            storage::smb_iso_device_config(
                r"\\smb.lab\private\linux",
                Some((SMB_USER, SMB_PASSWORD)),
            ),
        )),
    )
    .await;
    ok(&op);
    let sr = sr_named(&server, "smb-private").unwrap();
    let rec = server.record("sr", &sr).unwrap();
    assert_eq!(rec["sm_config"]["iso_type"], json!("cifs"));
    let dc = &pbds(&server, &sr)[0].2;
    assert_eq!(dc["location"], json!("//smb.lab/private"));
    assert_eq!(dc["iso_path"], json!("/linux"));
    assert!(dc.get("cifspassword_secret").is_some());
    assert!(dc.get("cifspassword").is_none());
    assert!(server.refs("secret").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn iscsi_scans_create_and_chap() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;

    // IQNs, sorted; LUNs, sorted.
    let found = iqns(&mut h, conn, "iscsi.lab", None).await.unwrap();
    let names: Vec<_> = found.iter().map(|i| i.target_iqn.as_str()).collect();
    assert_eq!(
        names,
        [
            "iqn.2026-11.lab.mock:archive",
            "iqn.2026-11.lab.mock:storage"
        ]
    );
    assert_eq!(found[1].address, "192.0.2.60");
    let storage_iqn = found[1].clone();
    let lun_list = luns(&mut h, conn, &storage_iqn, None).await.unwrap();
    assert_eq!(lun_list.len(), 2);
    assert_eq!(lun_list[0].lun_id, 0);
    assert_eq!(
        storage::lun_label(&lun_list[1]),
        "LUN 1: lun-a1: 50 GB (LIO-ORG)"
    );
    // The scans' SR.create never leaves an SR behind.
    assert!(sr_named(&server, "__gui__").is_none());
    assert_eq!(server.call_count("Async.SR.create"), 2);

    // Before Next, the page probes the chosen LUN: LUN 0 holds an SR, LUN 1
    // none.
    let lun0 = storage::iscsi_device_config(&storage_iqn, &lun_list[0], None);
    let found = probe(&mut h, conn, "lvmoiscsi", lun0).await.unwrap();
    assert_eq!(found.srs.len(), 1);
    assert_eq!(found.srs[0].uuid, OLD_ISCSI_SR);
    assert_eq!(found.srs[0].size, 100 << 30);
    let lun1 = storage::iscsi_device_config(&storage_iqn, &lun_list[1], None);
    assert!(
        probe(&mut h, conn, "lvmoiscsi", lun1.clone())
            .await
            .unwrap()
            .srs
            .is_empty()
    );

    // Format LUN 1.
    let op = run(&h, conn, new_sr(spec(SrKind::Iscsi, "lab-iscsi", lun1))).await;
    ok(&op);
    let sr = sr_named(&server, "lab-iscsi").unwrap();
    assert_eq!(field(&server, "sr", &sr, "type"), json!("lvmoiscsi"));
    let dc = &pbds(&server, &sr)[0].2;
    assert_eq!(dc["SCSIid"], json!(lun_list[1].scsi_id));
    assert_eq!(dc["targetIQN"], json!("iqn.2026-11.lab.mock:storage"));
    assert_eq!(dc["port"], json!("3260"));

    // CHAP: refused without, listed with; the SR keeps a secret.
    assert_eq!(
        iqns(&mut h, conn, "chap.lab", None).await.unwrap_err(),
        "SR_BACKEND_FAILURE_68"
    );
    assert_eq!(
        iqns(&mut h, conn, "chap.lab", Some((CHAP_USER, "nope")))
            .await
            .unwrap_err(),
        "SR_BACKEND_FAILURE_68"
    );
    let chap = Some((CHAP_USER, CHAP_PASSWORD));
    let secure = iqns(&mut h, conn, "chap.lab", chap).await.unwrap();
    assert_eq!(secure.len(), 1);
    let secure_luns = luns(&mut h, conn, &secure[0], chap).await.unwrap();
    let dc = storage::iscsi_device_config(&secure[0], &secure_luns[0], chap);
    ok(&run(&h, conn, new_sr(spec(SrKind::Iscsi, "chap-iscsi", dc))).await);
    let sr = sr_named(&server, "chap-iscsi").unwrap();
    let dc = &pbds(&server, &sr)[0].2;
    assert!(dc.get("chappassword_secret").is_some());
    assert!(dc.get("chappassword").is_none());

    // Unreachable and unknown targets.
    assert_eq!(
        iqns(&mut h, conn, "unreachable.lab", None)
            .await
            .unwrap_err(),
        "SR_BACKEND_FAILURE_141"
    );
    assert_eq!(
        iqns(&mut h, conn, "nowhere.lab", None).await.unwrap_err(),
        "SR_BACKEND_FAILURE_140"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn iscsi_scan_answers_the_client_cannot_use() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    // No list in the error: no IQNs; garbage: a bad response.
    server.fail_method("SR.create", Some(("SR_BACKEND_FAILURE_96", &["", ""])));
    assert_eq!(
        iqns(&mut h, conn, "iscsi.lab", None).await.unwrap_err(),
        errors::NO_IQNS
    );
    server.fail_method(
        "SR.create",
        Some((
            "SR_BACKEND_FAILURE_96",
            &["", "", "<iscsi-target-iqns><TGT>"],
        )),
    );
    assert_eq!(
        iqns(&mut h, conn, "iscsi.lab", None).await.unwrap_err(),
        errors::BAD_RESPONSE
    );
    server.fail_method(
        "SR.create",
        Some((
            "SR_BACKEND_FAILURE_107",
            &["", "", "<iscsi-target></iscsi-target>"],
        )),
    );
    let iqn = IqnInfo {
        index: 0,
        target_iqn: "iqn.2026-11.lab.mock:storage".into(),
        address: "192.0.2.60".into(),
        port: 3260,
    };
    assert_eq!(
        luns(&mut h, conn, &iqn, None).await.unwrap_err(),
        errors::NO_LUNS
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn introduce_an_sr_the_probe_found() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    let found = probe(
        &mut h,
        conn,
        "nfs",
        storage::nfs_probe_config("nfs.lab:/exports/old", ""),
    )
    .await
    .unwrap();
    assert_eq!(found.srs.len(), 1);
    assert_eq!(found.srs[0].uuid, OLD_NFS_SR);

    let mut s = spec(
        SrKind::Nfs,
        "old-nfs",
        storage::nfs_device_config("nfs.lab:/exports/old", "", "3"),
    );
    s.uuid = Some(OLD_NFS_SR.into());
    let op = run(&h, conn, new_sr(s)).await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Storage(StorageKind::AttachSr {
            name: "old-nfs".into(),
            pool: "Lab Pool".into()
        })
    );
    let sr = sr_with_uuid(&server, OLD_NFS_SR).unwrap();
    assert_eq!(field(&server, "sr", &sr, "name_label"), json!("old-nfs"));
    assert_eq!(field(&server, "sr", &sr, "shared"), json!(true));
    let p = pbds(&server, &sr);
    assert_eq!(p.len(), 2);
    assert!(
        p.iter()
            .all(|(_, attached, dc)| *attached && dc["serverpath"] == json!("/exports/old"))
    );
    // The pre-emptive forget looked the UUID up first; nothing to forget.
    let calls = server.calls();
    let pos = |m: &str| calls.iter().position(|c| c == m).unwrap();
    assert!(pos("SR.get_by_uuid") < pos("Async.SR.introduce"));
    assert_eq!(server.call_count("Async.SR.forget"), 0);
    assert_eq!(server.call_count("Async.PBD.create"), 2);
    assert_eq!(server.call_count("Async.PBD.plug"), 2);
    // The steps the Events panel showed ended with the last host's plug.
    assert!(op.step.is_none());

    // The iSCSI LUN with an SR: introduced the same way.
    let iqn = IqnInfo {
        index: 0,
        target_iqn: "iqn.2026-11.lab.mock:storage".into(),
        address: "192.0.2.60".into(),
        port: 3260,
    };
    let lun0 = luns(&mut h, conn, &iqn, None).await.unwrap()[0].clone();
    let mut s = spec(
        SrKind::Iscsi,
        "old-iscsi",
        storage::iscsi_device_config(&iqn, &lun0, None),
    );
    s.uuid = Some(OLD_ISCSI_SR.into());
    ok(&run(&h, conn, new_sr(s)).await);
    let sr = sr_with_uuid(&server, OLD_ISCSI_SR).unwrap();
    assert_eq!(pbds(&server, &sr).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pbd_that_does_not_plug_rolls_the_create_back() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    server.fail_pbd_plug(H2, true);
    let srs_before = server.refs("sr");
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Smb,
            "half-smb",
            storage::smb_device_config(r"\\smb.lab\half", SMB_USER, SMB_PASSWORD),
        )),
    )
    .await;
    failed_with(&op, "SR_BACKEND_FAILURE_47");
    // The create made it with host 2's PBD unplugged; the plug by hand
    // failed; the SR was unplugged and forgotten.
    assert_eq!(server.call_count("PBD.plug"), 1);
    assert_eq!(server.call_count("PBD.unplug"), 1);
    assert_eq!(server.call_count("SR.forget"), 1);
    assert!(sr_named(&server, "half-smb").is_none());
    assert_eq!(server.refs("sr"), srs_before);
    assert!(server.refs("secret").is_empty());
    let pbd_srs: Vec<Value> = server
        .refs("pbd")
        .iter()
        .map(|p| field(&server, "pbd", p, "SR"))
        .collect();
    assert!(
        pbd_srs
            .iter()
            .all(|s| srs_before.contains(&s.as_str().unwrap().to_owned()))
    );

    // A create that fails outright leaves nothing either.
    server.fail_pbd_plug(H2, false);
    server.fail_method(
        "SR.create",
        Some(("SR_BACKEND_FAILURE_73", &["", "NFS mount error", ""])),
    );
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Nfs,
            "never",
            storage::nfs_device_config("nfs.lab:/exports/never", "", "3"),
        )),
    )
    .await;
    failed_with(&op, "SR_BACKEND_FAILURE_73");
    assert_eq!(server.refs("sr"), srs_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn local_storage_on_the_coordinator() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    for (fs, name) in [(LocalFs::Ext, "local-ext"), (LocalFs::Lvm, "local-lvm")] {
        let op = run(
            &h,
            conn,
            new_sr(spec(
                SrKind::Local(fs),
                name,
                storage::local_device_config(if fs == LocalFs::Ext {
                    "/dev/sdb"
                } else {
                    "/dev/sdc"
                }),
            )),
        )
        .await;
        ok(&op);
        let sr = sr_named(&server, name).unwrap();
        let rec = server.record("sr", &sr).unwrap();
        assert_eq!(rec["type"], json!(fs.sr_type()));
        assert_eq!(rec["shared"], json!(false));
        let p = pbds(&server, &sr);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].0, H1, "made on the coordinator");
        assert_eq!(
            p[0].2["device"],
            json!(if fs == LocalFs::Ext {
                "/dev/sdb"
            } else {
                "/dev/sdc"
            })
        );
    }
    let op = run(
        &h,
        conn,
        new_sr(spec(
            SrKind::Local(LocalFs::Xfs),
            "local-xfs",
            storage::local_device_config("/dev/nope"),
        )),
    )
    .await;
    failed_with(&op, "SR_BACKEND_FAILURE_77");
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_forget_then_reattach_the_forgotten_sr() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    let dc = storage::nfs_device_config("nfs.lab:/exports/cycle", "", "3");
    ok(&run(&h, conn, new_sr(spec(SrKind::Nfs, "cycle", dc.clone()))).await);
    let sr = sr_named(&server, "cycle").unwrap();
    let uuid = field(&server, "sr", &sr, "uuid")
        .as_str()
        .unwrap()
        .to_owned();
    wait_sr(&h, "the new SR in the cache", |c| {
        c.model.srs.contains_key(&r::<Sr>(&sr))
    })
    .await;

    // Attached: forget and destroy are not allowed by the server.
    let state = h.backend.state();
    let view = first(&state);
    let rec = view.model.srs[&r::<Sr>(&sr)].clone();
    assert!(storage::sr_op_available(
        &view.model,
        &r(&sr),
        &rec,
        storage::SrOp::Detach,
        false
    ));
    assert!(!storage::sr_op_available(
        &view.model,
        &r(&sr),
        &rec,
        storage::SrOp::Forget,
        false
    ));

    let op = run(&h, conn, sr_op(&sr, SrAction::Detach)).await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Storage(StorageKind::Sr {
            action: SrAction::Detach,
            name: "cycle".into(),
            pool: "Lab Pool".into()
        })
    );
    assert!(pbds(&server, &sr).iter().all(|(_, attached, _)| !attached));
    assert_eq!(server.call_count("Async.PBD.unplug"), 2);
    wait_sr(&h, "detached in the cache", |c| {
        c.model.srs.get(&r::<Sr>(&sr)).is_some_and(|s| {
            xen_core::objects::sr_is_detached(&c.model, s)
                && s.allowed_operations
                    .contains(&xapi::StorageOperations::Forget)
        })
    })
    .await;
    let state = h.backend.state();
    let view = first(&state);
    let rec = view.model.srs[&r::<Sr>(&sr)].clone();
    for op in [storage::SrOp::Forget, storage::SrOp::Destroy] {
        assert!(
            storage::sr_op_available(&view.model, &r(&sr), &rec, op, false),
            "{op:?}"
        );
    }
    assert!(!storage::sr_op_available(
        &view.model,
        &r(&sr),
        &rec,
        storage::SrOp::Detach,
        false
    ));
    assert!(!storage::sr_op_available(
        &view.model,
        &r(&sr),
        &rec,
        storage::SrOp::Rescan,
        false
    ));

    ok(&run(&h, conn, sr_op(&sr, SrAction::Forget)).await);
    assert!(server.record("sr", &sr).is_none());
    assert!(
        server
            .refs("pbd")
            .iter()
            .all(|p| field(&server, "pbd", p, "SR") != json!(sr))
    );

    // The data stayed on the export: the wizard finds and reattaches it.
    let found = probe(
        &mut h,
        conn,
        "nfs",
        storage::nfs_probe_config("nfs.lab:/exports/cycle", ""),
    )
    .await
    .unwrap();
    assert_eq!(found.srs.len(), 1);
    assert_eq!(found.srs[0].uuid, uuid);
    let mut s = spec(SrKind::Nfs, "cycle again", dc);
    s.uuid = Some(uuid.clone());
    ok(&run(&h, conn, new_sr(s)).await);
    let back = sr_with_uuid(&server, &uuid).unwrap();
    assert_eq!(
        field(&server, "sr", &back, "name_label"),
        json!("cycle again")
    );
    assert!(pbds(&server, &back).iter().all(|(_, a, _)| *a));
}

/// An SR of this pool with no PBDs (what Reattach is for).
fn add_pbdless_sr(server: &MockServer, opaque: &str, uuid: &str) {
    server.add_object(
        "SR",
        opaque,
        json!({
            "uuid": uuid, "name_label": "Orphan NFS", "name_description": "",
            "type": "nfs", "content_type": "", "shared": true, "PBDs": [], "VDIs": [],
            "physical_size": 0, "physical_utilisation": 0, "virtual_allocation": 0,
            "other_config": {}, "sm_config": {}, "tags": [], "introduced_by": "OpaqueRef:NULL",
            "is_tools_sr": false, "allowed_operations": ["forget", "pbd_create"],
            "current_operations": {}
        }),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_a_detached_sr_and_roll_back_a_failed_one() {
    let server = MockServer::start(false).await;
    let h = harness();
    let orphan = "OpaqueRef:sr-orphan";
    add_pbdless_sr(&server, orphan, OLD_NFS_SR);
    let conn = connect(&h, &server).await;
    wait_sr(&h, "the orphan SR", |c| {
        c.model.srs.contains_key(&r::<Sr>(orphan))
    })
    .await;
    let state = h.backend.state();
    let view = first(&state);
    let rec = view.model.srs[&r::<Sr>(orphan)].clone();
    assert!(storage::sr_op_available(
        &view.model,
        &r(orphan),
        &rec,
        storage::SrOp::Reattach,
        false
    ));
    assert_eq!(SrKind::of_sr(&rec), Some(SrKind::Nfs));

    let reattach = |name: &str| {
        let mut s = spec(
            SrKind::Nfs,
            name,
            storage::nfs_device_config("nfs.lab:/exports/old", "", "3"),
        );
        s.uuid = Some(OLD_NFS_SR.into());
        s.reattach = Some(r(orphan));
        new_sr(s)
    };
    // Host 2 cannot plug: the PBDs made are unplugged and destroyed again.
    server.fail_pbd_plug(H2, true);
    let op = run(&h, conn, reattach("never")).await;
    failed_with(&op, "SR_BACKEND_FAILURE_47");
    assert!(pbds(&server, orphan).is_empty());
    assert_eq!(
        field(&server, "sr", orphan, "name_label"),
        json!("Orphan NFS")
    );
    assert_eq!(server.call_count("PBD.destroy"), 2);

    server.fail_pbd_plug(H2, false);
    wait_sr(&h, "the PBDs gone from the cache", |c| {
        c.model
            .srs
            .get(&r::<Sr>(orphan))
            .is_some_and(|s| s.pbds.is_empty())
    })
    .await;
    let op = run(&h, conn, reattach("Reattached NFS")).await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Storage(StorageKind::AttachSr {
            name: "Reattached NFS".into(),
            pool: "Lab Pool".into()
        })
    );
    assert_eq!(
        field(&server, "sr", orphan, "name_label"),
        json!("Reattached NFS")
    );
    assert_eq!(
        field(&server, "sr", orphan, "name_description"),
        json!("Reattached NFS (test)")
    );
    let p = pbds(&server, orphan);
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|(_, a, _)| *a));
    assert_eq!(server.call_count("Async.SR.introduce"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn repair_plugs_and_creates_the_missing_pbds() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // The ISO library is unplugged on host 2.
    let state = h.backend.state();
    let view = first(&state);
    let iso = view.model.srs[&r::<Sr>(ISO)].clone();
    assert!(storage::sr_op_available(
        &view.model,
        &r(ISO),
        &iso,
        storage::SrOp::Repair,
        false
    ));
    let op = run(&h, conn, sr_op(ISO, SrAction::Repair)).await;
    ok(&op);
    assert!(pbds(&server, ISO).iter().all(|(_, a, _)| *a));
    assert_eq!(server.call_count("Async.PBD.plug"), 1);
    assert_eq!(server.call_count("Async.PBD.create"), 0);
    wait_sr(&h, "the ISO library repaired", |c| {
        c.model
            .srs
            .get(&r::<Sr>(ISO))
            .is_some_and(|s| !xen_core::objects::sr_is_broken(&c.model, s))
    })
    .await;

    // The NFS SR loses host 2's PBD: repair copies host 1's configuration.
    server.remove_object("PBD", "OpaqueRef:pbd-nfs-02");
    server.set_field("sr", NFS, "PBDs", json!(["OpaqueRef:pbd-nfs-01"]));
    wait_sr(&h, "the NFS SR broken", |c| {
        c.model
            .srs
            .get(&r::<Sr>(NFS))
            .is_some_and(|s| xen_core::objects::sr_is_broken(&c.model, s))
    })
    .await;
    // Host 2 cannot plug yet: the PBD is made, the repair fails at the end.
    server.fail_pbd_plug(H2, true);
    let op = run(&h, conn, sr_op(NFS, SrAction::Repair)).await;
    failed_with(&op, "SR_BACKEND_FAILURE_47");
    let p = pbds(&server, NFS);
    assert_eq!(p.len(), 2);
    assert_eq!((p[1].0.as_str(), p[1].1), (H2, false));
    assert_eq!(p[1].2["serverpath"], json!("/exports/vm"));
    // Again once host 2 can: only the plug is left to do.
    server.fail_pbd_plug(H2, false);
    wait_sr(&h, "the new PBD in the cache", |c| {
        c.model
            .srs
            .get(&r::<Sr>(NFS))
            .is_some_and(|s| s.pbds.len() == 2)
    })
    .await;
    let creates = server.call_count("Async.PBD.create");
    ok(&run(&h, conn, sr_op(NFS, SrAction::Repair)).await);
    assert_eq!(server.call_count("Async.PBD.create"), creates);
    assert!(pbds(&server, NFS).iter().all(|(_, a, _)| *a));
}

#[tokio::test(flavor = "multi_thread")]
async fn set_default_destroy_rescan_and_trim() {
    let server = MockServer::start(false).await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    let dc = storage::nfs_device_config("nfs.lab:/exports/scratch", "", "3");
    ok(&run(&h, conn, new_sr(spec(SrKind::Nfs, "scratch", dc))).await);
    let sr = sr_named(&server, "scratch").unwrap();

    let op = run(&h, conn, sr_op(&sr, SrAction::SetDefault)).await;
    ok(&op);
    for f in ["default_SR", "suspend_image_SR", "crash_dump_SR"] {
        assert_eq!(field(&server, "pool", POOL, f), json!(sr), "{f}");
    }

    // Destroy while attached: the server refuses; detached: gone, and the
    // export no longer holds it.
    let op = run(&h, conn, sr_op(&sr, SrAction::Destroy)).await;
    failed_with(&op, "SR_HAS_PBD");
    ok(&run(&h, conn, sr_op(&sr, SrAction::Detach)).await);
    ok(&run(&h, conn, sr_op(&sr, SrAction::Destroy)).await);
    assert!(server.record("sr", &sr).is_none());
    assert_eq!(
        field(&server, "pool", POOL, "default_SR"),
        json!("OpaqueRef:NULL")
    );
    let found = probe(
        &mut h,
        conn,
        "nfs",
        storage::nfs_probe_config("nfs.lab:/exports/scratch", ""),
    )
    .await
    .unwrap();
    assert!(found.srs.is_empty());

    // Rescan finds a new ISO on the library's share.
    server.add_iso_file("nfs.lab:/exports/iso", "alpine-3.22.iso");
    let op = run(&h, conn, sr_op(ISO, SrAction::Rescan)).await;
    ok(&op);
    let names: Vec<Value> = refs(&field(&server, "sr", ISO, "VDIs"))
        .iter()
        .map(|v| field(&server, "vdi", v, "name_label"))
        .collect();
    assert!(names.contains(&json!("alpine-3.22.iso")), "{names:?}");
    assert_eq!(server.call_count("Async.SR.scan"), 1);

    // Trim: LVM supports it; ext answers the plugin's error.
    let op = run(&h, conn, sr_op(LOCAL1, SrAction::Trim)).await;
    ok(&op);
    assert!(
        server
            .plugin_calls()
            .contains(&("trim".into(), "do_trim".into()))
    );
    let op = run(&h, conn, sr_op(LOCAL2, SrAction::Trim)).await;
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            let xapi::Error::Api { code, params } = &**e else {
                panic!("{e}")
            };
            assert_eq!(code, errors::TRIM_FAILED);
            assert_eq!(params[0], "UnsupportedSRForTrim");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(op.step, None);
    let _ = OperationStep::ReclaimingSpace;
}

#[tokio::test(flavor = "multi_thread")]
async fn trim_and_forget_on_detached_or_attached_srs() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    ok(&run(&h, conn, sr_op(LOCAL2, SrAction::Detach)).await);
    wait_sr(&h, "local storage 2 detached", |c| {
        c.model
            .srs
            .get(&r::<Sr>(LOCAL2))
            .is_some_and(|s| xen_core::objects::sr_is_detached(&c.model, s))
    })
    .await;
    let op = run(&h, conn, sr_op(LOCAL2, SrAction::Trim)).await;
    failed_with(&op, errors::TRIM_DETACHED);
    // Forget on an attached SR: the original does nothing.
    let op = run(&h, conn, sr_op(LOCAL1, SrAction::Forget)).await;
    ok(&op);
    assert_eq!(server.call_count("Async.SR.forget"), 0);
    assert!(server.record("sr", LOCAL1).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn properties_change_only_what_changed() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let changes = SrChanges {
        name: Some("Shared NFS".into()),
        description: None,
        tags: Some(vec!["gold".into(), "nfs".into()]),
        read_caching: Some(false),
    };
    let op = run(
        &h,
        conn,
        ActionRequest::SetSrProperties {
            sr: r(NFS),
            changes,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Storage(StorageKind::SrProperties {
            name: "NFS VM storage".into()
        })
    );
    let rec = server.record("sr", NFS).unwrap();
    assert_eq!(rec["name_label"], json!("Shared NFS"));
    assert_eq!(rec["name_description"], json!("Shared VM disks"));
    assert_eq!(rec["tags"], json!(["gold", "nfs"]));
    assert_eq!(rec["other_config"]["o_direct"], json!("true"));
    assert_eq!(server.call_count("SR.set_name_description"), 0);
    let op = run(
        &h,
        conn,
        ActionRequest::SetSrProperties {
            sr: r(NFS),
            changes: SrChanges {
                read_caching: Some(true),
                ..SrChanges::default()
            },
        },
    )
    .await;
    ok(&op);
    assert!(
        server.record("sr", NFS).unwrap()["other_config"]
            .get("o_direct")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rbac_of_the_storage_operations() {
    let server = MockServer::start(false).await;
    let h = harness();
    for (user, password, can_create, can_trim) in [
        ("vm-operator-user", "vm-operator-password", false, false),
        ("pool-operator-user", "pool-operator-password", true, false),
        ("pool-admin-user", "pool-admin-password", true, true),
    ] {
        h.backend.send(Command::Connect {
            server: server.address(),
            username: user.into(),
            password: Some(password.into()),
            remember_password: false,
        });
        let state = h
            .wait_conn("connected", |c| c.status.is_connected() && c.rbac.is_some())
            .await;
        let conn = first(&state);
        let check = |request: &ActionRequest| {
            xen_core::rbac::check(&conn.model, conn.rbac.as_deref(), &request.rbac_methods())
        };
        use xen_core::rbac::RbacCheck;
        let create = new_sr(spec(
            SrKind::Smb,
            "x",
            storage::smb_device_config(r"\\smb.lab\x", SMB_USER, SMB_PASSWORD),
        ));
        assert_eq!(
            check(&create) == RbacCheck::Allowed,
            can_create,
            "{user} create"
        );
        assert_eq!(
            check(&sr_op(LOCAL1, SrAction::Trim)) == RbacCheck::Allowed,
            can_trim,
            "{user} trim"
        );
        if let RbacCheck::Denied { authorized } = check(&sr_op(LOCAL1, SrAction::Trim)) {
            assert!(authorized.iter().any(|r| r.name_label == "pool-admin"));
        }
        if !can_create {
            // The server refuses too.
            let id = conn.id;
            let op = run(&h, id, create).await;
            failed_with(&op, "RBAC_PERMISSION_DENIED");
        }
        h.backend.send(Command::Disconnect {
            connection: conn.id,
        });
        h.wait_for("disconnected", |s| s.connections.is_empty())
            .await;
    }
}
