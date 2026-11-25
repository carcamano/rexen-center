//! F7: VM creation, templates, snapshots and disks against the mock
//! (backend only, no UI): the XAPI calls made, the objects left on the
//! server, the history entries, failures and rollbacks.

mod common;

use common::{first, harness};
use serde_json::{Value, json};
use xapi::{Network, Ref, Vbd, Vm};
use xapi_mock::MockServer;
use xen_core::operations::{OperationNotice, ProvisionKind};
use xen_core::provisioning::{
    BootMode, CopyMode, DiskSource, DiskSpec, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif,
};
use xen_core::{ActionRequest, Command, ConnectionId, Operation, OperationKind, OperationStatus};

const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const DEBIAN: &str = "OpaqueRef:vm-template-debian";
const USER_TEMPLATE: &str = "OpaqueRef:vm-template-user";
const WEB_SNAP: &str = "OpaqueRef:vm-web-snap";
const NFS: &str = "OpaqueRef:sr-nfs";
const LOCAL1: &str = "OpaqueRef:sr-local-01";
const H1: &str = "OpaqueRef:host-0001";
const ISO: &str = "OpaqueRef:vdi-debian-iso";

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

/// The VM named `name` on the server.
fn vm_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("vm")
        .into_iter()
        .find(|v| field(server, "vm", v, "name_label") == json!(name))
}

/// `(type, userdevice, VDI)` of each drive of a VM.
fn drives(server: &MockServer, vm: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<_> = refs(&field(server, "vm", vm, "VBDs"))
        .iter()
        .map(|b| {
            let rec = server.record("vbd", b).unwrap();
            (
                rec["type"].as_str().unwrap().to_owned(),
                rec["userdevice"].as_str().unwrap().to_owned(),
                rec["VDI"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    out.sort();
    out
}

fn debian_spec(name: &str) -> NewVmSpec {
    NewVmSpec {
        template: r(DEBIAN),
        name: name.into(),
        description: "made by the wizard".into(),
        media: InstallMedia::Iso(r(ISO)),
        affinity: Some(r(H1)),
        vcpus_max: 4,
        vcpus_at_startup: 2,
        cores_per_socket: Some(2),
        memory_static_max: 4 << 30,
        memory_dynamic_min: 2 << 30,
        memory_dynamic_max: 4 << 30,
        disks: vec![
            NewVmDisk {
                source: DiskSource::Provision,
                system: true,
                userdevice: "0".into(),
                name: format!("{name} 0"),
                description: "Created by template provisioner".into(),
                size: 12 << 30,
                sr: r(NFS),
                bootable: true,
            },
            NewVmDisk {
                source: DiskSource::New,
                system: false,
                userdevice: "1".into(),
                name: "data".into(),
                description: String::new(),
                size: 1 << 30,
                sr: r(LOCAL1),
                bootable: false,
            },
        ],
        full_copy_sr: None,
        vifs: vec![
            NewVmVif {
                network: r::<Network>("OpaqueRef:net-eth0"),
                mac: String::new(),
            },
            NewVmVif {
                network: r("OpaqueRef:net-vlan20"),
                mac: "02:12:34:56:78:9a".into(),
            },
        ],
        boot_mode: Some(BootMode::UefiSecure),
        vtpm: true,
        start: true,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_vm_from_a_default_template_with_iso_disks_networks_uefi_and_vtpm() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let op = run(
        &h,
        conn,
        ActionRequest::NewVm(Box::new(debian_spec("deb-01"))),
    )
    .await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Provision(ProvisionKind::CreateVm {
            vm: "deb-01".into(),
            template: "Debian Bookworm 12".into()
        })
    );
    let vm = vm_named(&server, "deb-01").expect("the new VM");
    assert_eq!(op.created.as_ref().map(|c| c.to_string()), Some(vm.clone()));
    let rec = server.record("vm", &vm).unwrap();
    assert_eq!(rec["is_a_template"], json!(false));
    assert_eq!(rec["name_description"], json!("made by the wizard"));
    assert_eq!(rec["power_state"], json!("Running"));
    assert_eq!(rec["affinity"], json!(H1));
    assert_eq!(rec["VCPUs_max"], json!(4));
    assert_eq!(rec["VCPUs_at_startup"], json!(2));
    assert_eq!(rec["platform"]["cores-per-socket"], json!("2"));
    assert_eq!(rec["platform"]["secureboot"], json!("true"));
    assert_eq!(rec["HVM_boot_params"]["firmware"], json!("uefi"));
    assert_eq!(rec["memory_static_max"], json!(4i64 << 30));
    assert_eq!(rec["memory_dynamic_min"], json!(2i64 << 30));
    assert_eq!(rec["suspend_SR"], json!(NFS));
    assert!(rec["other_config"].get("disks").is_none());
    assert_eq!(refs(&rec["VTPMs"]).len(), 1);

    // The DVD drive holds the ISO; disk 0 (system, 12 GiB on NFS) is not
    // bootable because the ISO installs; disk 1 is a new user disk.
    let d = drives(&server, &vm);
    assert_eq!(d.len(), 3, "{d:?}");
    assert_eq!(d[0], ("CD".into(), "3".into(), ISO.into()));
    let disk0 = &d.iter().find(|x| x.0 == "Disk" && x.1 == "0").unwrap().2;
    let disk1 = &d.iter().find(|x| x.0 == "Disk" && x.1 == "1").unwrap().2;
    let vdi0 = server.record("vdi", disk0).unwrap();
    assert_eq!(vdi0["SR"], json!(NFS));
    assert_eq!(vdi0["virtual_size"], json!(12i64 << 30));
    assert_eq!(vdi0["name_label"], json!("deb-01 0"));
    assert_eq!(vdi0["type"], json!("system"));
    assert_eq!(vdi0["sm_config"]["vmhint"], json!(vm));
    let vdi1 = server.record("vdi", disk1).unwrap();
    assert_eq!(vdi1["SR"], json!(LOCAL1));
    assert_eq!(vdi1["type"], json!("user"));
    for b in refs(&rec["VBDs"]) {
        let vbd = server.record("vbd", &b).unwrap();
        if vbd["type"] == json!("Disk") {
            assert_eq!(vbd["other_config"]["owner"], json!("true"));
            assert_eq!(vbd["bootable"], json!(false));
        }
    }
    // Two interfaces, in order, the second with its MAC.
    let vifs: Vec<Value> = refs(&rec["VIFs"])
        .iter()
        .map(|v| server.record("vif", v).unwrap())
        .collect();
    assert_eq!(vifs.len(), 2);
    let second = vifs.iter().find(|v| v["device"] == json!("1")).unwrap();
    assert_eq!(second["network"], json!("OpaqueRef:net-vlan20"));
    assert_eq!(second["MAC"], json!("02:12:34:56:78:9a"));

    // The original's order: clone (hidden name), provision, properties,
    // the DVD drive, disks, networks, vTPM, start.
    let calls = server.calls();
    let pos = |m: &str| {
        calls
            .iter()
            .position(|c| c == m)
            .unwrap_or_else(|| panic!("{m} not called"))
    };
    assert!(pos("Async.VM.clone") < pos("Async.VM.provision"));
    assert!(pos("Async.VM.provision") < pos("VM.set_name_label"));
    assert!(pos("VM.set_name_label") < pos("Async.VBD.insert"));
    assert!(pos("Async.VBD.insert") < pos("Async.VDI.create"));
    assert!(pos("Async.VDI.create") < pos("Async.VIF.create"));
    assert!(pos("Async.VIF.create") < pos("VTPM.create"));
    assert!(pos("VTPM.create") < pos("Async.VM.start"));
    assert!(vm_named(&server, "__gui__deb-01").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_vm_network_boot_without_start_and_bios() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let mut spec = debian_spec("pxe-01");
    spec.media = InstallMedia::Network;
    spec.boot_mode = Some(BootMode::Bios);
    spec.vtpm = false;
    spec.start = false;
    spec.affinity = None;
    spec.disks.truncate(1);
    spec.disks[0].sr = r(NFS);
    spec.vifs.truncate(1);
    ok(&run(&h, conn, ActionRequest::NewVm(Box::new(spec))).await);
    let vm = vm_named(&server, "pxe-01").unwrap();
    let rec = server.record("vm", &vm).unwrap();
    assert_eq!(rec["power_state"], json!("Halted"));
    assert_eq!(rec["HVM_boot_params"]["order"], json!("ncd"));
    assert_eq!(rec["HVM_boot_params"]["firmware"], json!("bios"));
    assert_eq!(rec["platform"]["secureboot"], json!("false"));
    assert!(refs(&rec["VTPMs"]).is_empty());
    // The DVD drive is there, empty; disk 0 boots.
    let d = drives(&server, &vm);
    assert_eq!(d[0], ("CD".into(), "3".into(), "OpaqueRef:NULL".into()));
    let disk = refs(&rec["VBDs"])
        .into_iter()
        .map(|b| server.record("vbd", &b).unwrap())
        .find(|b| b["type"] == json!("Disk"))
        .unwrap();
    assert_eq!(disk["bootable"], json!(true));
    assert_eq!(server.call_count("Async.VM.start"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_vm_failure_before_the_point_of_no_return_rolls_back() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let vdis_before = server.refs("vdi").len();
    let vms_before = server.refs("vm").len();
    server.fail_method(
        "VIF.create",
        Some(("NETWORK_INCOMPATIBLE_PURPOSES", &["x"])),
    );
    let op = run(
        &h,
        conn,
        ActionRequest::NewVm(Box::new(debian_spec("doomed"))),
    )
    .await;
    failed_with(&op, "NETWORK_INCOMPATIBLE_PURPOSES");
    assert!(op.created.is_none());
    // The half-made VM and the disks it owned are gone; nothing else.
    assert_eq!(server.refs("vm").len(), vms_before);
    assert_eq!(server.refs("vdi").len(), vdis_before);
    assert!(vm_named(&server, "__gui__doomed").is_none());
    assert!(vm_named(&server, "doomed").is_none());

    // After the point of no return (the start fails), the VM stays.
    server.fail_method("VIF.create", None);
    server.fail_method("VM.start", Some(("NO_HOSTS_AVAILABLE", &[])));
    let op = run(
        &h,
        conn,
        ActionRequest::NewVm(Box::new(debian_spec("kept"))),
    )
    .await;
    failed_with(&op, "NO_HOSTS_AVAILABLE");
    assert!(vm_named(&server, "kept").is_some());

    // An SR without room fails while making the disks, and rolls back.
    server.fail_method("VM.start", None);
    let mut spec = debian_spec("too-big");
    spec.disks[0].size = 1 << 50;
    let op = run(&h, conn, ActionRequest::NewVm(Box::new(spec))).await;
    failed_with(&op, "SR_FULL");
    assert!(vm_named(&server, "too-big").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_vm_from_a_snapshot_and_a_custom_template_copied_to_an_sr() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let state = h.backend.state();
    let snap_disk = first(&state).model.vbds[&r::<Vbd>("OpaqueRef:vbd-snap-0")]
        .vdi
        .clone();
    // From the snapshot: its disk is cloned, kept on its SR.
    let spec = NewVmSpec {
        template: r(WEB_SNAP),
        name: "web-restored".into(),
        description: String::new(),
        media: InstallMedia::None,
        affinity: None,
        vcpus_max: 4,
        vcpus_at_startup: 2,
        cores_per_socket: None,
        memory_static_max: 4 << 30,
        memory_dynamic_min: 1 << 30,
        memory_dynamic_max: 2 << 30,
        disks: vec![NewVmDisk {
            source: DiskSource::Template(snap_disk),
            system: false,
            userdevice: "0".into(),
            name: "web-01 root".into(),
            description: String::new(),
            size: 20 << 30,
            sr: r(LOCAL1),
            bootable: true,
        }],
        full_copy_sr: None,
        vifs: vec![],
        boot_mode: Some(BootMode::Bios),
        vtpm: false,
        start: false,
    };
    ok(&run(&h, conn, ActionRequest::NewVm(Box::new(spec))).await);
    let vm = vm_named(&server, "web-restored").unwrap();
    let rec = server.record("vm", &vm).unwrap();
    assert_eq!(rec["is_a_template"], json!(false));
    assert_eq!(rec["is_a_snapshot"], json!(false));
    let d = drives(&server, &vm);
    assert_eq!(
        d.len(),
        1,
        "a custom template without a DVD drive gets none"
    );
    assert_eq!(field(&server, "vdi", &d[0].2, "SR"), json!(LOCAL1));
    assert_ne!(d[0].2, "OpaqueRef:vdi-web-root-snap");
    assert_eq!(server.call_count("Async.VDI.copy"), 0);

    // The same disk asked on another SR is copied there.
    let snap_disk = first(&state).model.vbds[&r::<Vbd>("OpaqueRef:vbd-snap-0")]
        .vdi
        .clone();
    let mut spec2 = debian_spec("web-moved");
    spec2.template = r(WEB_SNAP);
    spec2.disks.truncate(1);
    spec2.disks[0].source = DiskSource::Template(snap_disk);
    spec2.disks[0].size = 20 << 30;
    spec2.disks[0].sr = r(NFS);
    spec2.media = InstallMedia::None;
    spec2.vifs.clear();
    spec2.vtpm = false;
    spec2.start = false;
    ok(&run(&h, conn, ActionRequest::NewVm(Box::new(spec2))).await);
    let vm2 = vm_named(&server, "web-moved").unwrap();
    let d2 = drives(&server, &vm2);
    let disk = d2.iter().find(|x| x.0 == "Disk").unwrap();
    assert_eq!(field(&server, "vdi", &disk.2, "SR"), json!(NFS));
    assert_eq!(server.call_count("Async.VDI.copy"), 1);

    // A custom template copied in full to an SR (`VM.copy`).
    let mut spec3 = debian_spec("from-user-template");
    spec3.template = r(USER_TEMPLATE);
    spec3.full_copy_sr = Some(r(NFS));
    spec3.disks.clear();
    spec3.media = InstallMedia::None;
    spec3.start = false;
    spec3.vtpm = false;
    ok(&run(&h, conn, ActionRequest::NewVm(Box::new(spec3))).await);
    assert_eq!(server.call_count("Async.VM.copy"), 1);
    let vm3 = vm_named(&server, "from-user-template").unwrap();
    assert_eq!(field(&server, "vm", &vm3, "is_a_template"), json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_clone_template_and_delete() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // Fast clone.
    let op = run(
        &h,
        conn,
        ActionRequest::CopyVm {
            vm: r(DB),
            name: "Copy of db-01".into(),
            description: "PostgreSQL".into(),
            mode: CopyMode::Clone,
        },
    )
    .await;
    ok(&op);
    let clone = vm_named(&server, "Copy of db-01").unwrap();
    assert_eq!(op.created.as_ref().unwrap().to_string(), clone);
    assert_eq!(
        field(&server, "vm", &clone, "name_description"),
        json!("PostgreSQL")
    );
    let clone_disk = drives(&server, &clone)[0].2.clone();
    assert_eq!(field(&server, "vdi", &clone_disk, "SR"), json!(NFS));
    // Full copy to the local SR.
    ok(&run(
        &h,
        conn,
        ActionRequest::CopyVm {
            vm: r(DB),
            name: "db-01 full".into(),
            description: String::new(),
            mode: CopyMode::Copy(r(LOCAL1)),
        },
    )
    .await);
    let copy = vm_named(&server, "db-01 full").unwrap();
    assert_eq!(
        field(&server, "vdi", &drives(&server, &copy)[0].2, "SR"),
        json!(LOCAL1)
    );
    // A running VM cannot be cloned.
    let op = run(
        &h,
        conn,
        ActionRequest::CopyVm {
            vm: r(WEB),
            name: "nope".into(),
            description: String::new(),
            mode: CopyMode::Clone,
        },
    )
    .await;
    failed_with(&op, "VM_BAD_POWER_STATE");

    // Convert to template: `instant` then `is_a_template`.
    ok(&run(&h, conn, ActionRequest::MakeTemplate { vm: r(&copy) }).await);
    let rec = server.record("vm", &copy).unwrap();
    assert_eq!(rec["is_a_template"], json!(true));
    assert_eq!(rec["other_config"]["instant"], json!("true"));
    h.wait_conn("template in cache", |c| {
        c.model
            .vms
            .get(&r::<Vm>(&copy))
            .is_some_and(|v| v.is_a_template)
    })
    .await;

    // Template from a snapshot.
    let op = run(
        &h,
        conn,
        ActionRequest::TemplateFromSnapshot {
            snapshot: r(WEB_SNAP),
            name: "Template from snapshot 'web-01 before upgrade'".into(),
            description: "generated".into(),
        },
    )
    .await;
    ok(&op);
    let t = vm_named(&server, "Template from snapshot 'web-01 before upgrade'").unwrap();
    let rec = server.record("vm", &t).unwrap();
    assert_eq!(rec["is_a_template"], json!(true));
    assert_eq!(rec["is_a_snapshot"], json!(false));
    assert_eq!(rec["other_config"]["instant"], json!("true"));

    // Delete the clone with its disk chosen; delete the template, keeping
    // its disk.
    let vdis = server.refs("vdi").len();
    ok(&run(
        &h,
        conn,
        ActionRequest::DeleteVm {
            vm: r(&clone),
            disks: vec![r(&clone_disk)],
            snapshots: vec![],
        },
    )
    .await);
    assert!(server.record("vm", &clone).is_none());
    assert!(server.record("vdi", &clone_disk).is_none());
    assert_eq!(server.refs("vdi").len(), vdis - 1);
    let copy_disk = drives(&server, &copy)[0].2.clone();
    ok(&run(
        &h,
        conn,
        ActionRequest::DeleteVm {
            vm: r(&copy),
            disks: vec![],
            snapshots: vec![],
        },
    )
    .await);
    assert!(server.record("vm", &copy).is_none());
    assert!(server.record("vdi", &copy_disk).is_some());
    // A running VM cannot be deleted.
    let op = run(
        &h,
        conn,
        ActionRequest::DeleteVm {
            vm: r(WEB),
            disks: vec![],
            snapshots: vec![],
        },
    )
    .await;
    failed_with(&op, "VM_BAD_POWER_STATE");
    assert!(server.record("vm", WEB).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_take_revert_and_delete() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // A disk snapshot of the halted db-01.
    let op = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(DB),
            name: "before".into(),
            description: "first".into(),
            memory: false,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Provision(ProvisionKind::Snapshot { vm: "db-01".into() })
    );
    let snap = op.created.clone().unwrap().to_string();
    let rec = server.record("vm", &snap).unwrap();
    assert_eq!(rec["is_a_snapshot"], json!(true));
    assert_eq!(rec["snapshot_of"], json!(DB));
    assert_eq!(rec["name_description"], json!("first"));
    assert!(refs(&field(&server, "vm", DB, "snapshots")).contains(&snap));
    let snap_disk = drives(&server, &snap)[0].2.clone();
    assert_eq!(
        field(&server, "vdi", &snap_disk, "is_a_snapshot"),
        json!(true)
    );

    // Change the VM, then revert: its disk is replaced, settings restored.
    let old_disk = drives(&server, DB)[0].2.clone();
    server.set_field("vm", DB, "VCPUs_at_startup", json!(1));
    ok(&run(
        &h,
        conn,
        ActionRequest::Revert {
            snapshot: r(&snap),
            snapshot_first: None,
        },
    )
    .await);
    assert_eq!(field(&server, "vm", DB, "VCPUs_at_startup"), json!(4));
    assert_eq!(field(&server, "vm", DB, "power_state"), json!("Halted"));
    let new_disk = drives(&server, DB)[0].2.clone();
    assert_ne!(new_disk, old_disk);
    assert!(server.record("vdi", &old_disk).is_none());
    // Halted at the snapshot: not started again.
    assert_eq!(server.call_count("Async.VM.start"), 0);

    // A snapshot of the running web-01 with memory, then a revert: the VM
    // is resumed on the server it ran on.
    let op = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(WEB),
            name: "live".into(),
            description: String::new(),
            memory: true,
        },
    )
    .await;
    ok(&op);
    let live = op.created.clone().unwrap().to_string();
    assert_eq!(
        field(&server, "vm", &live, "power_state"),
        json!("Suspended")
    );
    ok(&run(
        &h,
        conn,
        ActionRequest::Revert {
            snapshot: r(&live),
            snapshot_first: None,
        },
    )
    .await);
    assert_eq!(field(&server, "vm", WEB, "power_state"), json!("Running"));
    assert_eq!(field(&server, "vm", WEB, "resident_on"), json!(H1));
    assert_eq!(server.call_count("Async.VM.resume_on"), 1);
    // A disk snapshot of a running VM: reverting starts it again.
    let op = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(WEB),
            name: "disks".into(),
            description: String::new(),
            memory: false,
        },
    )
    .await;
    let disks_only = op.created.clone().unwrap().to_string();
    ok(&run(
        &h,
        conn,
        ActionRequest::Revert {
            snapshot: r(&disks_only),
            snapshot_first: None,
        },
    )
    .await);
    assert_eq!(field(&server, "vm", WEB, "power_state"), json!("Running"));
    assert_eq!(server.call_count("Async.VM.start_on"), 1);

    // Delete two snapshots: the one with memory is forced off first; the
    // disks their drives own go too.
    let live_disks: Vec<String> = drives(&server, &live)
        .into_iter()
        .filter(|d| d.0 == "Disk")
        .map(|d| d.2)
        .collect();
    assert!(!live_disks.is_empty());
    let op = run(
        &h,
        conn,
        ActionRequest::DeleteSnapshots {
            snapshots: vec![r(&snap), r(&live)],
        },
    )
    .await;
    ok(&op);
    assert!(server.record("vm", &snap).is_none());
    assert!(server.record("vm", &live).is_none());
    assert!(server.record("vdi", &snap_disk).is_none());
    for d in live_disks {
        assert!(server.record("vdi", &d).is_none());
    }
    assert_eq!(server.call_count("VM.hard_shutdown"), 1);
    assert!(!refs(&field(&server, "vm", DB, "snapshots")).contains(&snap));

    // Delete a VM with one of its snapshots.
    let keep = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(DB),
            name: "kept".into(),
            description: String::new(),
            memory: false,
        },
    )
    .await
    .created
    .unwrap();
    let gone = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(DB),
            name: "gone".into(),
            description: String::new(),
            memory: false,
        },
    )
    .await
    .created
    .unwrap();
    ok(&run(
        &h,
        conn,
        ActionRequest::DeleteVm {
            vm: r(DB),
            disks: vec![],
            snapshots: vec![gone.clone()],
        },
    )
    .await);
    assert!(server.record("vm", DB).is_none());
    assert!(server.record("vm", &gone.to_string()).is_none());
    assert!(server.record("vm", &keep.to_string()).is_some());

    // Errors: a template cannot be snapshotted; a missing snapshot.
    let op = run(
        &h,
        conn,
        ActionRequest::Snapshot {
            vm: r(DEBIAN),
            name: "x".into(),
            description: String::new(),
            memory: false,
        },
    )
    .await;
    failed_with(&op, "OPERATION_NOT_ALLOWED");
    let op = run(
        &h,
        conn,
        ActionRequest::Revert {
            snapshot: r("OpaqueRef:gone"),
            snapshot_first: None,
        },
    )
    .await;
    failed_with(&op, "HANDLE_INVALID");
}

#[tokio::test(flavor = "multi_thread")]
async fn disks_new_attach_detach_resize_move_and_delete() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // A new disk on the halted db-01: made, attached at position 1, not
    // plugged.
    let op = run(
        &h,
        conn,
        ActionRequest::NewDisk {
            vm: r(DB),
            disk: DiskSpec {
                name: "New virtual disk".into(),
                description: "scratch".into(),
                sr: r(NFS),
                size: 2 << 30,
            },
        },
    )
    .await;
    ok(&op);
    assert!(op.notice.is_none());
    let d = drives(&server, DB);
    let new = d.iter().find(|x| x.1 == "1").expect("position 1").2.clone();
    let rec = server.record("vdi", &new).unwrap();
    assert_eq!(rec["virtual_size"], json!(2i64 << 30));
    assert_eq!(rec["name_description"], json!("scratch"));
    assert_eq!(server.call_count("Async.VBD.plug"), 0);

    // On the running web-01 the new disk is plugged in.
    ok(&run(
        &h,
        conn,
        ActionRequest::NewDisk {
            vm: r(WEB),
            disk: DiskSpec {
                name: "hot".into(),
                description: String::new(),
                sr: r(LOCAL1),
                size: 1 << 30,
            },
        },
    )
    .await);
    assert_eq!(server.call_count("Async.VBD.plug"), 1);
    let web_new = drives(&server, WEB)
        .into_iter()
        .find(|x| field(&server, "vdi", &x.2, "name_label") == json!("hot"))
        .unwrap();
    let web_vbd = refs(&field(&server, "vm", WEB, "VBDs"))
        .into_iter()
        .find(|b| field(&server, "vbd", b, "VDI") == json!(web_new.2))
        .unwrap();
    assert_eq!(
        field(&server, "vbd", &web_vbd, "currently_attached"),
        json!(true)
    );

    // No room: SR_FULL.
    let op = run(
        &h,
        conn,
        ActionRequest::NewDisk {
            vm: r(DB),
            disk: DiskSpec {
                name: "huge".into(),
                description: String::new(),
                sr: r(LOCAL1),
                size: 1 << 50,
            },
        },
    )
    .await;
    failed_with(&op, "SR_FULL");

    // Resize: grow works, shrink fails.
    ok(&run(
        &h,
        conn,
        ActionRequest::ResizeDisk {
            vdi: r(&new),
            size: 3 << 30,
        },
    )
    .await);
    assert_eq!(
        field(&server, "vdi", &new, "virtual_size"),
        json!(3i64 << 30)
    );
    let op = run(
        &h,
        conn,
        ActionRequest::ResizeDisk {
            vdi: r(&new),
            size: 1 << 30,
        },
    )
    .await;
    failed_with(&op, "VDI_SIZE_TOO_SMALL");

    // Move while halted: copied to the local SR, drive made again at the
    // same position, old disk gone.
    ok(&run(
        &h,
        conn,
        ActionRequest::MoveDisk {
            vdi: r(&new),
            sr: r(LOCAL1),
        },
    )
    .await);
    assert!(server.record("vdi", &new).is_none());
    let moved = drives(&server, DB)
        .into_iter()
        .find(|x| x.1 == "1")
        .unwrap()
        .2;
    assert_eq!(field(&server, "vdi", &moved, "SR"), json!(LOCAL1));
    assert_eq!(
        field(&server, "vdi", &moved, "name_label"),
        json!("New virtual disk")
    );
    assert_eq!(server.call_count("Async.VDI.copy"), 1);
    // Move while the running VM uses it: storage live migration.
    ok(&run(
        &h,
        conn,
        ActionRequest::MoveDisk {
            vdi: r(&web_new.2),
            sr: r(NFS),
        },
    )
    .await);
    assert_eq!(server.call_count("Async.VDI.pool_migrate"), 1);
    let migrated = field(&server, "vbd", &web_vbd, "VDI")
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(field(&server, "vdi", &migrated, "SR"), json!(NFS));

    // Detach from the running VM: unplugged then destroyed; disk kept.
    ok(&run(
        &h,
        conn,
        ActionRequest::DetachDisk {
            vm: r(WEB),
            vbd: r(&web_vbd),
        },
    )
    .await);
    assert!(server.record("vbd", &web_vbd).is_none());
    assert!(server.record("vdi", &migrated).is_some());
    assert_eq!(server.call_count("Async.VBD.unplug"), 1);

    // Attach it to db-01 read-only (it owns it: nothing else uses it).
    h.wait_conn("the detached disk in the cache", |c| {
        c.model
            .vdis
            .get(&r::<xapi::Vdi>(&migrated))
            .is_some_and(|v| v.vbds.is_empty())
    })
    .await;
    ok(&run(
        &h,
        conn,
        ActionRequest::AttachDisk {
            vm: r(DB),
            vdi: r(&migrated),
            read_only: true,
        },
    )
    .await);
    let attached = refs(&field(&server, "vm", DB, "VBDs"))
        .into_iter()
        .find(|b| field(&server, "vbd", b, "VDI") == json!(migrated))
        .unwrap();
    assert_eq!(field(&server, "vbd", &attached, "mode"), json!("RO"));
    assert_eq!(
        field(&server, "vbd", &attached, "other_config")["owner"],
        json!("true")
    );

    // Delete: the drive goes with it.
    ok(&run(&h, conn, ActionRequest::DeleteDisk { vdi: r(&migrated) }).await);
    assert!(server.record("vdi", &migrated).is_none());
    assert!(server.record("vbd", &attached).is_none());
    // A disk in use that cannot be unplugged.
    server.fail_method(
        "VBD.unplug",
        Some(("DEVICE_DETACH_REJECTED", &["VBD", "x", "busy"])),
    );
    let root = drives(&server, WEB)
        .into_iter()
        .find(|x| x.1 == "0")
        .unwrap()
        .2;
    let op = run(&h, conn, ActionRequest::DeleteDisk { vdi: r(&root) }).await;
    failed_with(&op, "DEVICE_DETACH_REJECTED");
    assert!(server.record("vdi", &root).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn dvd_drives_isos_and_guest_tools() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let cd = "OpaqueRef:vbd-web-cd";
    // Eject, then load another ISO.
    let op = run(
        &h,
        conn,
        ActionRequest::ChangeCd {
            vm: r(WEB),
            vbd: r(cd),
            iso: None,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        op.kind,
        OperationKind::Provision(ProvisionKind::ChangeCd {
            vm: "web-01".into(),
            iso: None
        })
    );
    assert_eq!(field(&server, "vbd", cd, "empty"), json!(true));
    ok(&run(
        &h,
        conn,
        ActionRequest::ChangeCd {
            vm: r(WEB),
            vbd: r(cd),
            iso: Some(r("OpaqueRef:vdi-ubuntu-iso")),
        },
    )
    .await);
    assert_eq!(
        field(&server, "vbd", cd, "VDI"),
        json!("OpaqueRef:vdi-ubuntu-iso")
    );
    // Replacing it ejects first.
    ok(&run(
        &h,
        conn,
        ActionRequest::ChangeCd {
            vm: r(WEB),
            vbd: r(cd),
            iso: Some(r(ISO)),
        },
    )
    .await);
    assert_eq!(field(&server, "vbd", cd, "VDI"), json!(ISO));
    assert_eq!(server.call_count("Async.VBD.eject"), 2);
    // Not an ISO.
    let op = run(
        &h,
        conn,
        ActionRequest::ChangeCd {
            vm: r(WEB),
            vbd: r(cd),
            iso: Some(r("OpaqueRef:vdi-db-root")),
        },
    )
    .await;
    failed_with(&op, "VDI_INCOMPATIBLE_TYPE");

    // Install tools: the tools ISO into the first DVD drive.
    let state = h.backend.state();
    assert!(xen_core::provisioning::can_install_tools(
        &first(&state).model,
        &first(&state).model.vms[&r::<Vm>(WEB)],
        false
    ));
    ok(&run(&h, conn, ActionRequest::InstallTools { vm: r(WEB) }).await);
    assert_eq!(
        field(&server, "vbd", cd, "VDI"),
        json!("OpaqueRef:vdi-tools-iso")
    );
    // Without a DVD drive it fails.
    let op = run(&h, conn, ActionRequest::InstallTools { vm: r(DB) }).await;
    failed_with(&op, "VM_NO_CD_DRIVE");

    // A new DVD drive on the halted db-01, at position 3; on the running
    // web-01 the drive is plugged in.
    ok(&run(&h, conn, ActionRequest::NewCdDrive { vm: r(DB) }).await);
    let d = drives(&server, DB);
    assert!(
        d.contains(&("CD".into(), "3".into(), "OpaqueRef:NULL".into())),
        "{d:?}"
    );
    let op = run(&h, conn, ActionRequest::NewCdDrive { vm: r(WEB) }).await;
    ok(&op);
    assert_eq!(server.call_count("Async.VBD.plug"), 1);
    assert!(op.notice.is_none());
    let new_cd = drives(&server, WEB)
        .into_iter()
        .filter(|x| x.0 == "CD")
        .map(|x| x.1)
        .collect::<Vec<_>>();
    assert_eq!(new_cd.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disk_that_cannot_be_plugged_asks_for_a_restart() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // A VM that is not halted but whose new drive does not offer `plug`
    // (here: paused): the user is told to restart it.
    server.set_field("vm", WEB, "power_state", json!("Paused"));
    let op = run(
        &h,
        conn,
        ActionRequest::NewDisk {
            vm: r(WEB),
            disk: DiskSpec {
                name: "cold".into(),
                description: String::new(),
                sr: r(LOCAL1),
                size: 1 << 30,
            },
        },
    )
    .await;
    ok(&op);
    assert_eq!(op.notice, Some(OperationNotice::RestartForDisk));
}

#[tokio::test(flavor = "multi_thread")]
async fn rbac_of_the_f7_operations() {
    let server = MockServer::start(false).await;
    let h = harness();
    h.backend.send(Command::Connect {
        server: server.address(),
        username: "vm-operator-user".into(),
        password: Some("vm-operator-password".into()),
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
    // A VM operator may change the ISO, not snapshot or make disks.
    let cd = ActionRequest::ChangeCd {
        vm: r(WEB),
        vbd: r("OpaqueRef:vbd-web-cd"),
        iso: None,
    };
    assert_eq!(check(&cd), RbacCheck::Allowed);
    let snapshot = ActionRequest::Snapshot {
        vm: r(DB),
        name: "x".into(),
        description: String::new(),
        memory: false,
    };
    let RbacCheck::Denied { authorized } = check(&snapshot) else {
        panic!("a VM operator cannot snapshot");
    };
    assert!(authorized.iter().any(|r| r.name_label == "vm-power-admin"));
    let new_vm = ActionRequest::NewVm(Box::new(debian_spec("x")));
    assert!(matches!(check(&new_vm), RbacCheck::Denied { .. }));
    // The server refuses too.
    let id = conn.id;
    let op = run(&h, id, snapshot).await;
    failed_with(&op, "RBAC_PERMISSION_DENIED");
}
