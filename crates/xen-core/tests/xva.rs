//! F11a on the mock: XVA export and import through the backend.
//!
//! * a VM is exported (checked as it arrives and again on completion) with
//!   a 4 KiB buffer, then imported back: the new VM gets a free name, its
//!   home server, the wizard's interfaces (moved, created) and its disk's
//!   content is the exported one;
//! * export: cancelled mid-transfer, a server error, a disk in use, a full
//!   disk, a dropped link and a wrong block checksum all leave no file;
//! * import: cancelled mid-upload, a server error, a file that ends early,
//!   a dropped link all leave nothing on the server, even when the server
//!   does not clean up after itself; cancelling the wizard after the
//!   transfer keeps the VM (as the original); a template and starting the
//!   VM after import.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::Value;
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::xva_run::{ImportFinish, ImportVif, TransferFaults, errors};
use xen_core::{
    ActionRequest, BackendOptions, Command, ConnectionId, Operation, OperationStatus,
    OperationStep, xva,
};

use common::{memory_store, spawn};

const DB_VM: &str = "OpaqueRef:vm-db";
const DB_VDI: &str = "OpaqueRef:vdi-db-root";
const USER_TEMPLATE: &str = "OpaqueRef:vm-template-user";
const NFS: &str = "OpaqueRef:sr-nfs";
const HOST1: &str = "OpaqueRef:host-0001";
const VLAN20: &str = "OpaqueRef:net-vlan20";
const ETH1: &str = "OpaqueRef:net-eth1";

/// The transfer buffer of these tests: far smaller than the files.
const BUFFER: usize = 4096;

fn harness(faults: TransferFaults) -> common::Harness {
    spawn(BackendOptions {
        transfer_buffer: BUFFER,
        transfer_faults: faults,
        ..common::fast_options(memory_store().0)
    })
}

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("xen-f11a-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fixture pool with 8 disk blocks of 64 KiB per disk (over 512 KiB
/// per export, more than a hundred buffers).
async fn server() -> MockServer {
    let server = MockServer::start(false).await;
    server.set_task_timing(2, Duration::from_millis(20));
    server.set_xva_blocks(64 << 10, 8);
    server
}

async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
    let state = h
        .wait_conn("connected", |c| c.status.is_connected() && c.rbac.is_some())
        .await;
    common::first(&state).id
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

fn start(h: &common::Harness, conn: ConnectionId, request: ActionRequest) -> u64 {
    let before = last_id(h);
    h.backend.send(Command::Action {
        connection: conn,
        request,
    });
    before
}

/// The first operation after `before`, once `pred` holds for it.
async fn op_when(
    h: &common::Harness,
    before: u64,
    what: &str,
    pred: impl Fn(&Operation) -> bool,
) -> Operation {
    let state = h
        .wait_for(what, |s| {
            s.operations
                .iter()
                .find(|o| o.id > before)
                .is_some_and(|o| pred(o))
        })
        .await;
    (**state.operations.iter().find(|o| o.id > before).unwrap()).clone()
}

async fn ended(h: &common::Harness, before: u64) -> Operation {
    op_when(h, before, "operation ended", |o| !o.is_running()).await
}

async fn run(h: &common::Harness, conn: ConnectionId, request: ActionRequest) -> Operation {
    let before = start(h, conn, request);
    ended(h, before).await
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

fn export(vm: &str, path: &Path, verify: bool) -> ActionRequest {
    ActionRequest::ExportXva {
        vm: Ref::new(vm),
        path: path.display().to_string(),
        verify,
    }
}

fn import(path: &Path, sr: &str, affinity: Option<&str>) -> ActionRequest {
    ActionRequest::ImportXva {
        path: path.display().to_string(),
        sr: Ref::new(sr),
        affinity: affinity.map(Ref::new),
    }
}

/// Nothing but `keep` in `dir`.
fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> Value {
    server.field(class, r, f).unwrap_or(Value::Null)
}

/// VMs whose `other_config` marks them as imported.
fn imported_vms(server: &MockServer) -> Vec<String> {
    server
        .refs("vm")
        .into_iter()
        .filter(|r| {
            field(server, "vm", r, "other_config")
                .get("import_task")
                .is_some()
        })
        .collect()
}

/// Exports the db VM into a fresh folder; returns the file.
async fn exported(h: &common::Harness, conn: ConnectionId) -> PathBuf {
    let path = temp_dir().join("db-01.xva");
    ok(&run(h, conn, export(DB_VM, &path, false)).await);
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xva_export_then_import_round_trip() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    let path = dir.join("db-01.xva");

    // Export, verified on completion.
    let op = run(&h, conn, export(DB_VM, &path, true)).await;
    ok(&op);
    assert_eq!(op.progress, Some(1.0));
    assert_eq!(files_in(&dir), vec!["db-01.xva"], "no temporary file left");
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size > 100 * BUFFER as u64, "{size} bytes");
    // The file is a whole XVA whose metadata names the VM's 8 GiB disk.
    let mut v = xva::Verifier::default().keep_metadata();
    v.feed(&std::fs::read(&path).unwrap()).unwrap();
    let summary = v.finish().unwrap();
    assert_eq!(summary.blocks, 8);
    assert_eq!(summary.checksum, Some(xva::BlockChecksum::XxHash));
    assert!(summary.metadata.unwrap().contains("db-01"));
    let size = field(&server, "vdi", DB_VDI, "virtual_size")
        .as_u64()
        .unwrap();
    assert_eq!(xva::disk_capacity(&path), size);
    let calls = server.calls();
    let i = calls.iter().position(|c| c == "task.create").unwrap();
    assert!(calls[i..].iter().any(|c| c == "http/export"));
    // The VM is free again and the task gone.
    let allowed = field(&server, "vm", DB_VM, "allowed_operations");
    assert!(allowed.as_array().unwrap().iter().any(|o| o == "export"));
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());

    // Import it back into the NFS SR with host 1 as home server.
    let vdis_before = field(&server, "sr", NFS, "VDIs").as_array().unwrap().len();
    let before = start(&h, conn, import(&path, NFS, Some(HOST1)));
    let op = op_when(&h, before, "the VM is imported", |o| {
        matches!(
            o.step,
            Some(OperationStep::WaitingForImportWizard { template: false })
        )
    })
    .await;
    let vm = op.created.clone().expect("the imported VM").to_string();
    assert_eq!(field(&server, "vm", &vm, "name_label"), "db-01 (1)");
    assert_eq!(field(&server, "vm", &vm, "affinity"), HOST1);
    assert_eq!(imported_vms(&server), vec![vm.clone()]);
    // The wizard: interface 0 to VLAN 20, a new interface 1 on eth1.
    h.backend.send(Command::FinishImport {
        operation: op.id,
        finish: Some(ImportFinish {
            start: false,
            vifs: Some(vec![
                ImportVif {
                    device: "0".into(),
                    network: Ref::new(VLAN20),
                    mac: String::new(),
                },
                ImportVif {
                    device: "1".into(),
                    network: Ref::new(ETH1),
                    mac: "02:00:00:00:00:42".into(),
                },
            ]),
        }),
    });
    let op = ended(&h, before).await;
    ok(&op);
    let vifs: Vec<(String, String, String)> = field(&server, "vm", &vm, "VIFs")
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let v = v.as_str().unwrap();
            (
                field(&server, "vif", v, "device")
                    .as_str()
                    .unwrap()
                    .to_owned(),
                field(&server, "vif", v, "network")
                    .as_str()
                    .unwrap()
                    .to_owned(),
                field(&server, "vif", v, "MAC").as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        vifs.iter().any(|(d, n, _)| d == "0" && n == VLAN20),
        "{vifs:?}"
    );
    assert!(
        vifs.iter()
            .any(|(d, n, m)| d == "1" && n == ETH1 && m == "02:00:00:00:00:42"),
        "{vifs:?}"
    );
    assert!(server.calls().iter().any(|c| c == "VIF.move"));
    // The disk landed in the NFS SR with the exported content.
    let vdis: Vec<String> = field(&server, "sr", NFS, "VDIs")
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(vdis.len(), vdis_before + 1);
    let new_vdi = vdis.last().unwrap();
    assert_eq!(
        field(&server, "vdi", new_vdi, "virtual_size"),
        field(&server, "vdi", DB_VDI, "virtual_size")
    );
    assert_eq!(server.vdi_digest(new_vdi), server.vdi_digest(DB_VDI));
    assert!(server.vdi_digest(DB_VDI).is_some());
    assert_eq!(field(&server, "vm", &vm, "power_state"), "Halted");
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sha1_checksums_and_a_suspended_vm_with_start_after_import() {
    let server = server().await;
    server.set_xva_xxhash(false);
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let path = exported(&h, conn).await;
    let mut v = xva::Verifier::default();
    v.feed(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v.finish().unwrap().checksum, Some(xva::BlockChecksum::Sha1));
    // Imported and started (a separate Start operation, as the original).
    let before = start(&h, conn, import(&path, NFS, None));
    let op = op_when(&h, before, "imported", |o| {
        matches!(o.step, Some(OperationStep::WaitingForImportWizard { .. }))
    })
    .await;
    let vm = op.created.clone().unwrap();
    h.backend.send(Command::FinishImport {
        operation: op.id,
        finish: Some(ImportFinish {
            start: true,
            vifs: None,
        }),
    });
    ok(&ended(&h, before).await);
    common::wait_until("the imported VM runs", || {
        server.vm_power_state(vm.as_str()) == "Running"
    })
    .await;
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_template_round_trip() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let path = temp_dir().join("template.xva");
    ok(&run(&h, conn, export(USER_TEMPLATE, &path, true)).await);
    let before = start(&h, conn, import(&path, NFS, Some(HOST1)));
    let op = op_when(&h, before, "imported", |o| {
        matches!(
            o.step,
            Some(OperationStep::WaitingForImportWizard { template: true })
        )
    })
    .await;
    let vm = op.created.clone().unwrap().to_string();
    assert_eq!(field(&server, "vm", &vm, "is_a_template"), true);
    // A template gets no home server and is never started.
    assert_eq!(field(&server, "vm", &vm, "affinity"), "OpaqueRef:NULL");
    h.backend.send(Command::FinishImport {
        operation: op.id,
        finish: Some(ImportFinish {
            start: true,
            vifs: Some(Vec::new()),
        }),
    });
    ok(&ended(&h, before).await);
    assert_eq!(field(&server, "vm", &vm, "power_state"), "Halted");
    assert_eq!(field(&server, "vm", &vm, "VIFs"), serde_json::json!([]));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_export_removes_the_partial_file() {
    let server = server().await;
    server.set_transfer_pace(4096, Duration::from_millis(5));
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    let path = dir.join("cancelled.xva");
    let before = start(&h, conn, export(DB_VM, &path, true));
    let op = op_when(
        &h,
        before,
        "some bytes received",
        |o| matches!(o.step, Some(OperationStep::Exporting { received }) if received > 20_000),
    )
    .await;
    assert!(op.can_cancel());
    // The VM is busy meanwhile.
    assert!(
        !field(&server, "vm", DB_VM, "allowed_operations")
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "export")
    );
    assert_eq!(files_in(&dir), vec!["cancelled.xva.tmp"]);
    h.backend
        .send(Command::CancelOperation { operation: op.id });
    let op = ended(&h, before).await;
    assert!(matches!(op.status, OperationStatus::Cancelled), "{op:?}");
    assert!(files_in(&dir).is_empty(), "{:?}", files_in(&dir));
    common::wait_until("the VM is free again", || {
        field(&server, "vm", DB_VM, "allowed_operations")
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "export")
    })
    .await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_exports_leave_no_file() {
    let server = server().await;
    let dir = temp_dir();
    let path = dir.join("failed.xva");

    // The server refuses: a disk in use elsewhere, or another error.
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    server.fail_method("http/export", Some(("VDI_IN_USE", &[DB_VDI, "export"])));
    let op = run(&h, conn, export(DB_VM, &path, false)).await;
    failed_with(&op, errors::EXPORT_VDI_IN_USE);
    server.fail_method(
        "http/export",
        Some(("SR_BACKEND_FAILURE_47", &["", "down", ""])),
    );
    let op = run(&h, conn, export(DB_VM, &path, false)).await;
    failed_with(&op, "SR_BACKEND_FAILURE_47");
    server.fail_method("http/export", None);
    assert!(files_in(&dir).is_empty());

    // The link drops mid-transfer.
    server.drop_transfer_after(Some(200_000));
    let op = run(&h, conn, export(DB_VM, &path, false)).await;
    assert!(matches!(op.status, OperationStatus::Failed(_)), "{op:?}");
    assert!(files_in(&dir).is_empty());
    server.drop_transfer_after(None);

    // A block whose checksum is wrong.
    server.corrupt_export_block(true);
    let op = run(&h, conn, export(DB_VM, &path, false)).await;
    failed_with(&op, errors::EXPORT_BLOCK_CHECKSUM);
    assert!(files_in(&dir).is_empty());
    server.corrupt_export_block(false);
    common::wait_until("no task left", || server.tasks().is_empty()).await;

    // The local disk fills up.
    let full = harness(TransferFaults {
        disk_full_after: Some(150_000),
        short_read_after: None,
    });
    let conn = connect(&full, &server).await;
    let op = run(&full, conn, export(DB_VM, &path, false)).await;
    failed_with(&op, errors::EXPORT_DISK_FULL);
    assert!(files_in(&dir).is_empty());
    common::wait_until("the VM is free again", || {
        field(&server, "vm", DB_VM, "allowed_operations")
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "export")
    })
    .await;
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What the NFS SR and the VM list hold (to show an import left nothing).
fn footprint(server: &MockServer) -> (Value, usize, usize) {
    (
        field(server, "sr", NFS, "VDIs"),
        server.refs("vm").len(),
        server.refs("network").len(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_import_leaves_nothing() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let path = exported(&h, conn).await;
    let before_footprint = footprint(&server);
    // A slow server: 4 KiB pieces, each taking a while.
    server.set_transfer_pace(4096, Duration::from_millis(3));
    let before = start(&h, conn, import(&path, NFS, None));
    let op = op_when(&h, before, "the VM exists and some bytes went", |o| {
        o.created.is_some()
            && o.progress.is_some_and(|p| p > 0.1)
            && matches!(o.step, Some(OperationStep::Uploading { .. }))
    })
    .await;
    assert!(op.can_cancel());
    assert_eq!(imported_vms(&server).len(), 1);
    h.backend
        .send(Command::CancelOperation { operation: op.id });
    let op = ended(&h, before).await;
    assert!(matches!(op.status, OperationStatus::Cancelled), "{op:?}");
    assert!(op.created.is_none(), "{op:#?}");
    common::wait_until("the half-imported VM is gone", || {
        imported_vms(&server).is_empty()
    })
    .await;
    assert_eq!(footprint(&server), before_footprint);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_imports_leave_nothing() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let path = exported(&h, conn).await;
    let before_footprint = footprint(&server);

    // The server refuses (its SR is full).
    server.fail_method("http/import", Some(("SR_FULL", &["100", "10"])));
    let op = run(&h, conn, import(&path, NFS, None)).await;
    failed_with(&op, "SR_FULL");
    server.fail_method("http/import", None);
    assert_eq!(footprint(&server), before_footprint);

    // The link drops mid-transfer; the server cleans up.
    server.drop_transfer_after(Some(300_000));
    let op = run(&h, conn, import(&path, NFS, None)).await;
    assert!(matches!(op.status, OperationStatus::Failed(_)), "{op:?}");
    assert_eq!(footprint(&server), before_footprint);

    // Again with a server that leaves the half-imported VM: this side
    // removes it with its disk.
    server.keep_half_imported(true);
    let op = run(&h, conn, import(&path, NFS, None)).await;
    assert!(matches!(op.status, OperationStatus::Failed(_)), "{op:?}");
    common::wait_until("no imported VM left", || imported_vms(&server).is_empty()).await;
    assert_eq!(footprint(&server), before_footprint);
    server.keep_half_imported(false);
    server.drop_transfer_after(None);

    // The file ends early on this side (a short read).
    let short = harness(TransferFaults {
        disk_full_after: None,
        short_read_after: Some(250_000),
    });
    let conn = connect(&short, &server).await;
    let op = run(&short, conn, import(&path, NFS, None)).await;
    failed_with(&op, errors::IMPORT_FILE_ERROR);
    common::wait_until("no imported VM left", || imported_vms(&server).is_empty()).await;
    assert_eq!(footprint(&server), before_footprint);

    // A missing file.
    let op = run(
        &short,
        conn,
        import(&path.with_extension("nope"), NFS, None),
    )
    .await;
    failed_with(&op, errors::IMPORT_FILE_MISSING);
    common::wait_until("no task left", || server.tasks().is_empty()).await;
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_the_wizard_after_the_transfer_keeps_the_vm() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let path = exported(&h, conn).await;
    let before = start(&h, conn, import(&path, NFS, None));
    let op = op_when(&h, before, "imported", |o| {
        matches!(o.step, Some(OperationStep::WaitingForImportWizard { .. }))
    })
    .await;
    // `ImportWizard.OnCancel`: EndWizard(false, null), then Cancel.
    h.backend.send(Command::FinishImport {
        operation: op.id,
        finish: None,
    });
    let op = ended(&h, before).await;
    assert!(matches!(op.status, OperationStatus::Cancelled), "{op:?}");
    assert_eq!(imported_vms(&server).len(), 1);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
