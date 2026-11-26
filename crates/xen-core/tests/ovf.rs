//! F11b on the mock: OVF/OVA export and import, and disk image import,
//! through the backend.
//!
//! * a VM is exported as an OVA (manifest, verified on completion) with a
//!   4 KiB buffer and imported back: the new VM has the exported memory,
//!   vCPUs, boot settings, interface (MAC kept, on the network chosen) and
//!   its disk's content is the exported one; the same for an OVF folder
//!   with gzipped disks;
//! * export: cancelled mid-transfer, a server error and a dropped link
//!   leave no file and no folder;
//! * import: cancelled mid-upload, a server error and a dropped link leave
//!   no VM and no disk; a corrupt manifest and a bad signature are found
//!   before anything is made; a signed package is accepted;
//! * disk images: a raw image and a fixed VHD become a VM whose disk holds
//!   the image's bytes; a VMDK is refused.

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::ovf_package::Package;
use xen_core::ovf_run::{OvfExport, OvfImport, VmMapping, errors};
use xen_core::xva_run::TransferFaults;
use xen_core::{
    ActionRequest, BackendOptions, Command, ConnectionId, Operation, OperationStatus, ova, ovf,
};

use common::{memory_store, spawn};

const DB_VM: &str = "OpaqueRef:vm-db";
const DB_VDI: &str = "OpaqueRef:vdi-db-root";
const NFS: &str = "OpaqueRef:sr-nfs";
const HOST1: &str = "OpaqueRef:host-0001";
const VLAN20: &str = "OpaqueRef:net-vlan20";

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
    let dir = std::env::temp_dir().join(format!("xen-f11b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn server() -> MockServer {
    let server = MockServer::start(false).await;
    server.set_task_timing(2, Duration::from_millis(20));
    server.set_xva_blocks(64 << 10, 6);
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

fn failed(op: &Operation) {
    assert!(
        matches!(op.status, OperationStatus::Failed(_)),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> Value {
    server.field(class, r, f).unwrap_or(Value::Null)
}

fn export_spec(dir: &Path, ova: bool, compress: bool, verify: bool) -> ActionRequest {
    ActionRequest::ExportOvf(Box::new(OvfExport {
        vms: vec![Ref::new(DB_VM)],
        folder: dir.display().to_string(),
        name: "pkg".into(),
        ova,
        compress,
        manifest: true,
        eulas: vec!["Be nice to the VM.".into()],
        verify,
    }))
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The wizard's choices for every system of `package`: disks on `sr`,
/// interfaces on `net`.
fn mappings(package: &Package, sr: &str, net: &str, home: Option<&str>) -> Vec<VmMapping> {
    let env = &package.envelope;
    env.systems
        .iter()
        .map(|s| {
            let hw = ovf::hardware(s).unwrap();
            VmMapping {
                system: s.id.clone(),
                name: ovf::vm_name(env, s, "pkg"),
                affinity: home.map(Ref::new),
                srs: hw
                    .items
                    .iter()
                    .filter(|r| r.is_disk())
                    .map(|r| (r.instance_id.clone(), Ref::new(sr)))
                    .collect(),
                networks: hw
                    .items
                    .iter()
                    .filter(|r| r.resource_type == ovf::resource::NIC)
                    .map(|r| (r.instance_id.clone(), Ref::new(net)))
                    .collect(),
            }
        })
        .collect()
}

fn import_spec(path: &Path, vms: Vec<VmMapping>, verify: bool) -> ActionRequest {
    ActionRequest::ImportOvf(Box::new(OvfImport {
        path: path.display().to_string(),
        image: None,
        vms,
        verify_manifest: verify,
        verify_signature: false,
        start: false,
    }))
}

/// The objects an import could leave.
fn counts(server: &MockServer) -> (usize, usize, usize, usize) {
    (
        server.refs("vm").len(),
        server.refs("vdi").len(),
        server.refs("vbd").len(),
        server.refs("vif").len(),
    )
}

fn vm_named(server: &MockServer, name: &str) -> Vec<String> {
    server
        .refs("vm")
        .into_iter()
        .filter(|r| field(server, "vm", r, "name_label") == name)
        .collect()
}

/// The disks (not CDs) of `vm`.
fn disks(server: &MockServer, vm: &str) -> Vec<String> {
    field(server, "vm", vm, "VBDs")
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_str().unwrap())
        .filter(|b| field(server, "vbd", b, "type") == "Disk")
        .map(|b| field(server, "vbd", b, "VDI").as_str().unwrap().to_owned())
        .collect()
}

/// Exports db-01 as an OVA; returns the archive.
async fn exported_ova(h: &common::Harness, conn: ConnectionId, dir: &Path) -> PathBuf {
    ok(&run(h, conn, export_spec(dir, true, false, false)).await);
    dir.join("pkg").join("pkg.ova")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ova_export_then_import_round_trip() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();

    let op = run(&h, conn, export_spec(&dir, true, false, true)).await;
    ok(&op);
    assert_eq!(op.progress, Some(1.0));
    assert_eq!(files_in(&dir.join("pkg")), vec!["pkg.ova"]);
    let ova_path = dir.join("pkg").join("pkg.ova");
    // Descriptor first, the disk, the manifest last.
    let entries = ova::index(&ova_path).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let uuid = field(&server, "vdi", DB_VDI, "uuid");
    assert_eq!(
        names,
        [
            "pkg.ovf",
            &format!("{}.vhd", uuid.as_str().unwrap()),
            "pkg.mf"
        ]
    );
    assert!(entries[1].size > 100 * BUFFER as u64);
    let package = Package::open(&ova_path).unwrap();
    package.verify_manifest(&|_, _| {}, &|| false).unwrap();
    let env = &package.envelope;
    assert_eq!(env.all_eulas(), ["Be nice to the VM."]);
    assert!(ovf::validate(env, &|f| package.has_file(f)).is_empty());
    assert_eq!(server.call_count("http/export_raw_vdi"), 1);
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());
    let exported_digest = server.vdi_digest(DB_VDI).expect("exported");

    // Import it into NFS with host 1 as home server, the interface on
    // VLAN 20, the manifest checked first.
    let before_counts = counts(&server);
    let maps = mappings(&package, NFS, VLAN20, Some(HOST1));
    assert_eq!(maps[0].name, "db-01");
    let op = run(&h, conn, import_spec(&ova_path, maps, true)).await;
    ok(&op);
    let vm = op.created.clone().expect("the new VM").to_string();
    assert_eq!(field(&server, "vm", &vm, "name_label"), "db-01");
    assert_eq!(field(&server, "vm", &vm, "affinity"), HOST1);
    assert_eq!(field(&server, "vm", &vm, "power_state"), "Halted");
    for f in [
        "memory_static_max",
        "memory_dynamic_max",
        "VCPUs_max",
        "HVM_boot_policy",
    ] {
        assert_eq!(
            field(&server, "vm", &vm, f),
            field(&server, "vm", DB_VM, f),
            "{f}"
        );
    }
    assert_eq!(field(&server, "vm", &vm, "platform")["secureboot"], "true");
    assert!(
        field(&server, "vm", &vm, "other_config")
            .get("HideFromXenCenter")
            .is_none()
    );
    let d = disks(&server, &vm);
    assert_eq!(d.len(), 1);
    assert_eq!(field(&server, "vdi", &d[0], "SR"), NFS);
    assert_eq!(
        field(&server, "vdi", &d[0], "virtual_size"),
        field(&server, "vdi", DB_VDI, "virtual_size")
    );
    assert_eq!(server.vdi_digest(&d[0]), Some(exported_digest));
    let vifs = field(&server, "vm", &vm, "VIFs");
    let vif = vifs[0].as_str().unwrap();
    assert_eq!(field(&server, "vif", vif, "network"), VLAN20);
    assert_eq!(field(&server, "vif", vif, "MAC"), "02:00:00:00:0b:01");
    let after = counts(&server);
    assert_eq!(after.0, before_counts.0 + 1);
    assert_eq!(after.1, before_counts.1 + 1);
    // F12a: the package became a vApp named after it, holding the VM
    // (`Import.Process`).
    assert_eq!(server.call_count("VM_appliance.create"), 1);
    let app = field(&server, "vm", &vm, "appliance");
    let app = app.as_str().unwrap();
    let stem = std::path::Path::new(&ova_path)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        field(&server, "vm_appliance", app, "name_label"),
        stem.as_str()
    );
    assert_eq!(field(&server, "vm_appliance", app, "VMs"), json!([vm]));
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compressed_ovf_folder_round_trip() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    ok(&run(&h, conn, export_spec(&dir, false, true, true)).await);
    let uuid = field(&server, "vdi", DB_VDI, "uuid");
    let disk = format!("{}.vhd.gz", uuid.as_str().unwrap());
    assert_eq!(
        files_in(&dir.join("pkg")),
        vec![disk.clone(), "pkg.mf".into(), "pkg.ovf".into()]
    );
    let package = Package::open(&dir.join("pkg").join("pkg.ovf")).unwrap();
    package.verify_manifest(&|_, _| {}, &|| false).unwrap();
    let f = &package.envelope.files[0];
    assert_eq!(f.compression.as_deref(), Some("gzip"));
    assert_eq!(
        f.size,
        Some(
            std::fs::metadata(dir.join("pkg").join(&disk))
                .unwrap()
                .len()
        )
    );
    let op = run(
        &h,
        conn,
        import_spec(
            &dir.join("pkg").join("pkg.ovf"),
            mappings(&package, NFS, VLAN20, None),
            true,
        ),
    )
    .await;
    ok(&op);
    let vm = op.created.unwrap().to_string();
    let d = disks(&server, &vm);
    assert_eq!(server.vdi_digest(&d[0]), server.vdi_digest(DB_VDI));
    // The expanded copy is gone.
    assert_eq!(files_in(&dir.join("pkg")).len(), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_exports_leave_no_file() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;

    // Cancelled mid-transfer.
    server.set_transfer_pace(4096, Duration::from_millis(5));
    let dir = temp_dir();
    let before = start(&h, conn, export_spec(&dir, true, false, false));
    let op = op_when(&h, before, "exporting", |o| {
        matches!(o.step, Some(xen_core::OperationStep::ExportingDisk { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    h.backend
        .send(Command::CancelOperation { operation: op.id });
    let op = ended(&h, before).await;
    assert!(
        matches!(op.status, OperationStatus::Cancelled),
        "{:?}",
        op.status
    );
    assert_eq!(files_in(&dir), Vec::<String>::new());
    server.set_transfer_pace(16 << 10, Duration::ZERO);

    // The server refuses.
    let dir = temp_dir();
    server.fail_method(
        "http/export_raw_vdi",
        Some(("VDI_IN_USE", &["x", "export"])),
    );
    let op = run(&h, conn, export_spec(&dir, false, false, false)).await;
    failed_with(&op, xen_core::xva_run::errors::EXPORT_VDI_IN_USE);
    assert_eq!(files_in(&dir), Vec::<String>::new());
    server.fail_method("http/export_raw_vdi", Some(("SR_BACKEND_FAILURE_47", &[])));
    let op = run(&h, conn, export_spec(&dir, false, false, false)).await;
    failed_with(&op, "SR_BACKEND_FAILURE_47");
    assert_eq!(files_in(&dir), Vec::<String>::new());
    server.fail_method("http/export_raw_vdi", None);

    // The link drops mid-transfer.
    server.drop_transfer_after(Some(300 << 10));
    let op = run(&h, conn, export_spec(&dir, true, false, false)).await;
    failed(&op);
    assert_eq!(files_in(&dir), Vec::<String>::new());
    server.drop_transfer_after(None);

    // A disk fills up.
    let h2 = harness(TransferFaults {
        disk_full_after: Some(200 << 10),
        ..Default::default()
    });
    let conn2 = connect(&h2, &server).await;
    let op = run(&h2, conn2, export_spec(&dir, false, false, false)).await;
    failed_with(&op, xen_core::xva_run::errors::EXPORT_DISK_FULL);
    assert_eq!(files_in(&dir), Vec::<String>::new());

    // An existing folder is kept, only the files are removed.
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    std::fs::write(dir.join("pkg").join("mine.txt"), "keep").unwrap();
    server.drop_transfer_after(Some(100 << 10));
    let op = run(&h, conn, export_spec(&dir, false, false, false)).await;
    failed(&op);
    assert_eq!(files_in(&dir.join("pkg")), vec!["mine.txt"]);
    server.drop_transfer_after(None);
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_imports_leave_nothing() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    let ova_path = exported_ova(&h, conn, &dir).await;
    let package = Package::open(&ova_path).unwrap();
    let before = counts(&server);

    // Cancelled mid-upload.
    server.set_transfer_pace(4096, Duration::from_millis(5));
    let b = start(
        &h,
        conn,
        import_spec(&ova_path, mappings(&package, NFS, VLAN20, None), false),
    );
    let op = op_when(&h, b, "uploading", |o| o.progress.is_some_and(|p| p > 0.05)).await;
    h.backend
        .send(Command::CancelOperation { operation: op.id });
    let op = ended(&h, b).await;
    assert!(
        matches!(op.status, OperationStatus::Cancelled),
        "{:?}",
        op.status
    );
    assert_eq!(counts(&server), before);
    assert!(server.refs("vm_appliance").is_empty(), "no vApp is left");
    server.set_transfer_pace(16 << 10, Duration::ZERO);

    // The server refuses.
    server.fail_method("http/import_raw_vdi", Some(("SR_FULL", &["1", "0"])));
    let op = run(
        &h,
        conn,
        import_spec(&ova_path, mappings(&package, NFS, VLAN20, None), false),
    )
    .await;
    failed_with(&op, "SR_FULL");
    assert_eq!(counts(&server), before);
    assert!(server.refs("vm_appliance").is_empty(), "no vApp is left");
    server.fail_method("http/import_raw_vdi", None);

    // The link drops mid-transfer.
    server.drop_transfer_after(Some(200 << 10));
    let op = run(
        &h,
        conn,
        import_spec(&ova_path, mappings(&package, NFS, VLAN20, None), false),
    )
    .await;
    failed(&op);
    assert_eq!(counts(&server), before);
    assert!(server.refs("vm_appliance").is_empty(), "no vApp is left");
    server.drop_transfer_after(None);

    // The file ends early.
    let h2 = harness(TransferFaults {
        short_read_after: Some(100 << 10),
        ..Default::default()
    });
    let conn2 = connect(&h2, &server).await;
    let op = run(
        &h2,
        conn2,
        import_spec(&ova_path, mappings(&package, NFS, VLAN20, None), false),
    )
    .await;
    failed_with(&op, errors::OVF_FILE_ERROR);
    assert_eq!(counts(&server), before);
    assert!(server.refs("vm_appliance").is_empty(), "no vApp is left");

    // VM.create refused: nothing made either.
    server.fail_method("VM.create", Some(("INTERNAL_ERROR", &["x"])));
    let op = run(
        &h,
        conn,
        import_spec(&ova_path, mappings(&package, NFS, VLAN20, None), false),
    )
    .await;
    failed_with(&op, "INTERNAL_ERROR");
    server.fail_method("VM.create", None);
    assert_eq!(counts(&server), before);
    assert!(server.refs("vm_appliance").is_empty(), "no vApp is left");
    assert!(server.tasks().is_empty(), "{:?}", server.tasks());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The entries of the OVA at `path`: name and bytes.
fn entries_of(path: &Path) -> Vec<(String, Vec<u8>)> {
    ova::index(path)
        .unwrap()
        .into_iter()
        .map(|e| {
            let mut f = std::fs::File::open(path).unwrap();
            std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(e.offset)).unwrap();
            let mut b = Vec::new();
            f.take(e.size).read_to_end(&mut b).unwrap();
            (e.name, b)
        })
        .collect()
}

/// An OVA `name` next to `path` holding `entries`.
fn write_ova(path: &Path, name: &str, entries: &[(String, Vec<u8>)]) -> PathBuf {
    let out = path.with_file_name(name);
    let mut t = ova::TarWriter::new(std::fs::File::create(&out).unwrap());
    for (n, b) in entries {
        t.entry(n, b).unwrap();
    }
    t.finish().unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifests_and_signatures_are_checked_first() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    let ova_path = exported_ova(&h, conn, &dir).await;
    let before = counts(&server);
    let creates = server.call_count("VM.create");

    let original = entries_of(&ova_path);
    let manifest = original
        .iter()
        .find(|(n, _)| n == "pkg.mf")
        .unwrap()
        .1
        .clone();
    let import = |path: &Path, signature: bool| {
        let package = Package::open(path).unwrap();
        ActionRequest::ImportOvf(Box::new(OvfImport {
            path: path.display().to_string(),
            image: None,
            vms: mappings(&package, NFS, VLAN20, None),
            verify_manifest: true,
            verify_signature: signature,
            start: false,
        }))
    };

    // A corrupt manifest: the disk's digest changed.
    let mut e = original.clone();
    let text = String::from_utf8(manifest.clone()).unwrap();
    let digest = text
        .lines()
        .nth(1)
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .to_owned();
    e.iter_mut().find(|(n, _)| n == "pkg.mf").unwrap().1 =
        text.replace(&digest, &"0".repeat(64)).into_bytes();
    let bad = write_ova(&ova_path, "bad1.ova", &e);
    failed_with(
        &run(&h, conn, import(&bad, false)).await,
        errors::OVF_MANIFEST,
    );
    // A manifest line that cannot be read.
    let mut e = original.clone();
    e.iter_mut().find(|(n, _)| n == "pkg.mf").unwrap().1 = b"garbage line\n".to_vec();
    let bad = write_ova(&ova_path, "bad2.ova", &e);
    failed_with(
        &run(&h, conn, import(&bad, false)).await,
        errors::OVF_MANIFEST,
    );
    // A disk whose bytes changed.
    let mut e = original.clone();
    let disk = &mut e.iter_mut().find(|(n, _)| n.ends_with(".vhd")).unwrap().1;
    let n = disk.len();
    disk[n - 600] ^= 1;
    let bad = write_ova(&ova_path, "bad3.ova", &e);
    failed_with(
        &run(&h, conn, import(&bad, false)).await,
        errors::OVF_MANIFEST,
    );
    // An entry the manifest does not list.
    let mut e = original.clone();
    e.push(("notes.txt".into(), b"x".to_vec()));
    let bad = write_ova(&ova_path, "bad4.ova", &e);
    failed_with(
        &run(&h, conn, import(&bad, false)).await,
        errors::OVF_MANIFEST,
    );
    // A signature over another manifest.
    let mut e = original.clone();
    e.push(("pkg.cert".into(), cert(b"another manifest")));
    let bad = write_ova(&ova_path, "bad5.ova", &e);
    assert!(Package::open(&bad).unwrap().has_signature());
    failed_with(
        &run(&h, conn, import(&bad, true)).await,
        errors::OVF_SIGNATURE,
    );
    assert_eq!(counts(&server), before);
    assert_eq!(server.call_count("VM.create"), creates, "nothing was made");

    // Signed over its own manifest: accepted and imported.
    let mut e = original.clone();
    e.push(("pkg.cert".into(), cert(&manifest)));
    let good = write_ova(&ova_path, "good.ova", &e);
    ok(&run(&h, conn, import(&good, true)).await);
    assert_eq!(counts(&server).0, before.0 + 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A `.cert` signing `manifest` with the test key.
fn cert(manifest: &[u8]) -> Vec<u8> {
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    let key = RsaKeyPair::from_pkcs8(include_bytes!("../src/testdata/ovf-sign-key.pk8")).unwrap();
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        manifest,
        &mut sig,
    )
    .unwrap();
    let hex: String = sig.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "SHA256(pkg.mf)= {hex}\n{}",
        include_str!("../src/testdata/ovf-sign-cert.pem")
    )
    .into_bytes()
}

fn image_request(path: &Path, name: &str, size: u64, file_size: u64) -> ActionRequest {
    let env = ovf::image_envelope(
        name,
        1,
        256,
        "",
        "",
        size,
        &path.display().to_string(),
        file_size,
    );
    let s = &env.systems[0];
    let hw = ovf::hardware(s).unwrap();
    let mapping = VmMapping {
        system: s.id.clone(),
        name: name.to_owned(),
        affinity: None,
        srs: hw
            .items
            .iter()
            .filter(|r| r.is_disk())
            .map(|r| (r.instance_id.clone(), Ref::new(NFS)))
            .collect(),
        networks: hw
            .items
            .iter()
            .filter(|r| r.resource_type == ovf::resource::NIC)
            .map(|r| (r.instance_id.clone(), Ref::new(VLAN20)))
            .collect(),
    };
    ActionRequest::ImportOvf(Box::new(OvfImport {
        path: path.display().to_string(),
        image: Some(Box::new(env)),
        vms: vec![mapping],
        verify_manifest: false,
        verify_signature: false,
        start: false,
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_images_become_vms() {
    let server = server().await;
    let h = harness(TransferFaults::default());
    let conn = connect(&h, &server).await;
    let dir = temp_dir();
    // A raw image of 3 MiB with data in two places.
    let mut raw = vec![0u8; 3 << 20];
    raw[..4096].fill(0xAB);
    raw[(2 << 20) + 77] = 9;
    let path = dir.join("disk.img");
    std::fs::write(&path, &raw).unwrap();
    let op = run(
        &h,
        conn,
        image_request(&path, "img-vm", raw.len() as u64, raw.len() as u64),
    )
    .await;
    ok(&op);
    let vm = op.created.unwrap().to_string();
    assert_eq!(field(&server, "vm", &vm, "name_label"), "img-vm");
    assert_eq!(field(&server, "vm", &vm, "memory_static_max"), 256 << 20);
    assert_eq!(field(&server, "vm", &vm, "HVM_boot_params")["order"], "dc");
    let d = disks(&server, &vm);
    assert_eq!(d.len(), 1);
    assert_eq!(field(&server, "vdi", &d[0], "virtual_size"), 3 << 20);
    assert_eq!(
        server.vdi_digest(&d[0]),
        Some(xapi_mock::vhd::content_digest(&raw))
    );
    let vbd = field(&server, "vm", &vm, "VBDs")[0]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(field(&server, "vbd", &vbd, "bootable"), true);
    assert_eq!(
        field(
            &server,
            "vif",
            field(&server, "vm", &vm, "VIFs")[0].as_str().unwrap(),
            "network"
        ),
        VLAN20
    );

    // Exported as an OVA (once the cache has its disk), its disk holds the
    // image's bytes.
    let vm_ref: Ref<xapi::Vm> = Ref::new(&vm);
    h.wait_conn("the disk in the cache", |c| {
        c.model.vms.get(&vm_ref).is_some_and(|v| {
            v.vbds
                .iter()
                .filter_map(|b| c.model.vbds.get(b))
                .any(|b| c.model.vdis.contains_key(&b.vdi))
        })
    })
    .await;
    let out = temp_dir();
    let export = ActionRequest::ExportOvf(Box::new(OvfExport {
        vms: vec![Ref::new(&vm)],
        folder: out.display().to_string(),
        name: "img".into(),
        ova: true,
        compress: false,
        manifest: true,
        eulas: Vec::new(),
        verify: true,
    }));
    ok(&run(&h, conn, export).await);
    let ova_path = out.join("img").join("img.ova");
    let entry = ova::index(&ova_path)
        .unwrap()
        .into_iter()
        .find(|e| e.name.ends_with(".vhd"))
        .unwrap();
    let mut f = std::fs::File::open(&ova_path).unwrap();
    let (sha, size) =
        xen_core::disk_image::vhd_content_sha256(&mut f, entry.offset, entry.size).unwrap();
    assert_eq!(size, raw.len() as u64);
    assert_eq!(
        sha,
        ring::digest::digest(&ring::digest::SHA256, &raw)
            .as_ref()
            .to_vec()
    );
    std::fs::remove_dir_all(&out).unwrap();

    // A fixed VHD: its bytes without the footer.
    let mut vhd = raw.clone();
    vhd.extend_from_slice(&xen_core::disk_image::fixed_vhd_footer(
        raw.len() as u64,
        [3; 16],
    ));
    let vpath = dir.join("disk.vhd");
    std::fs::write(&vpath, &vhd).unwrap();
    let op = run(
        &h,
        conn,
        image_request(&vpath, "vhd-vm", raw.len() as u64, vhd.len() as u64),
    )
    .await;
    ok(&op);
    let d = disks(&server, &op.created.unwrap().to_string());
    assert_eq!(
        server.vdi_digest(&d[0]),
        Some(xapi_mock::vhd::content_digest(&raw))
    );

    // A VMDK is refused, nothing made.
    let before = counts(&server);
    let mut vmdk = b"KDMV".to_vec();
    vmdk.resize(1 << 20, 0);
    let kpath = dir.join("disk.vmdk");
    std::fs::write(&kpath, &vmdk).unwrap();
    let op = run(&h, conn, image_request(&kpath, "vmdk-vm", 1 << 20, 1 << 20)).await;
    failed_with(&op, errors::OVF_UNSUPPORTED_DISK);
    assert_eq!(counts(&server), before);
    assert!(vm_named(&server, "vmdk-vm").is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
