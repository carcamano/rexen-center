//! F14a against the mock (backend only), with the device fixture: the GPU
//! rules over the live cache, allowed vGPU types, placement policy,
//! integrated GPU pass-through, a VM's vGPUs, USB pass-through and
//! devices, vTPMs, memory settings (live range, halted limits, the
//! reboot around a static change), Remote Desktop's switch and address,
//! and every refusal — asserting the exact calls and what the server ends
//! up with.

mod common;

use std::sync::Arc;

use common::{memory_store, spawn};
use serde_json::{Value, json};
use xapi::{AllocationAlgorithm, Ref};
use xapi_mock::MockServer;
use xen_core::devices::{
    self, DeviceBlock, DeviceRequest, PageVgpu, PlacementPolicy, VgpuChoiceText, VtpmRefusal,
};
use xen_core::memory::{MemoryConfirm, MemoryEdit, MemoryField, MemoryMode};
use xen_core::operations::{OperationFailure, PrecheckResult};
use xen_core::{
    ActionRequest, AppState, BackendOptions, Command, ConnectionId, Notification, Operation,
    OperationStatus, Precheck,
};

const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";
const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const BUILD: &str = "OpaqueRef:vm-build";
const PA: &str = "OpaqueRef:pgpu-k2-a";
const PB: &str = "OpaqueRef:pgpu-k2-b";
const PC: &str = "OpaqueRef:pgpu-k2-c";
const PI: &str = "OpaqueRef:pgpu-intel";
const GK: &str = "OpaqueRef:gpu-group-k2";
const GI: &str = "OpaqueRef:gpu-group-intel";
const PT: &str = "OpaqueRef:vgpu-type-passthrough";
const K260: &str = "OpaqueRef:vgpu-type-k260q";
const K240: &str = "OpaqueRef:vgpu-type-k240q";
const K220: &str = "OpaqueRef:vgpu-type-k220q";
const VWEB: &str = "OpaqueRef:vgpu-web";
const U1: &str = "OpaqueRef:pusb-1";
const U2: &str = "OpaqueRef:pusb-2";
const U3: &str = "OpaqueRef:pusb-3";
const U4: &str = "OpaqueRef:pusb-4";
const G1: &str = "OpaqueRef:usb-group-1";
const VUSB_DB: &str = "OpaqueRef:vusb-db";
const GIB: i64 = 1 << 30;

fn harness() -> common::Harness {
    spawn(BackendOptions {
        ..common::fast_options(memory_store().0)
    })
}

async fn device_pool() -> MockServer {
    let server = MockServer::start(false).await;
    server.add_device_fixtures();
    server.set_task_timing(2, std::time::Duration::from_millis(20));
    server
}

async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
    let address = server.address();
    let state = h
        .wait_for("connected", |s| {
            s.connections.iter().any(|c| {
                c.server == address
                    && c.status.is_connected()
                    && c.rbac.is_some()
                    && !c.model.pgpus.is_empty()
            })
        })
        .await;
    state
        .connections
        .iter()
        .find(|c| c.server == address)
        .unwrap()
        .id
}

async fn run(h: &common::Harness, conn: ConnectionId, request: DeviceRequest) -> Operation {
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
        request: ActionRequest::Device(Box::new(request)),
    });
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running())
        })
        .await;
    (**state.operations.iter().find(|o| o.id > before).unwrap()).clone()
}

fn ok(op: &Operation) {
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn fails(op: &Operation, code: &str) {
    match &op.status {
        OperationStatus::Failed(OperationFailure::Xapi(e)) => {
            assert!(e.is_api(code), "{:?}: expected {code}, got {e}", op.kind)
        }
        other => panic!("{:?}: expected failure {code}, got {other:?}", op.kind),
    }
}

/// The device calls since `from` (the class and call, `Async.` kept).
fn calls_since(server: &MockServer, from: usize, prefixes: &[&str]) -> Vec<String> {
    server.calls()[from..]
        .iter()
        .filter(|c| {
            let sync = c.trim_start_matches("Async.");
            prefixes.iter().any(|p| sync.starts_with(p))
        })
        .cloned()
        .collect()
}

async fn synced(
    h: &common::Harness,
    conn: ConnectionId,
    pred: impl Fn(&xen_core::ConnectionModel) -> bool,
) -> Arc<AppState> {
    h.wait_for("cache", |s| {
        s.connection(conn).is_some_and(|c| pred(&c.model))
    })
    .await
}

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

fn strs(v: Option<Value>) -> Vec<String> {
    v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

async fn precheck(h: &mut common::Harness, conn: ConnectionId, check: Precheck) -> PrecheckResult {
    static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(5000);
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

#[tokio::test]
async fn gpu_rules_over_the_live_cache() {
    let server = device_pool().await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let state = h.backend.state();
    let m = &state.connection(conn).unwrap().model;

    assert!(devices::gpu_capability(m) && devices::vgpu_capability(m));
    assert!(devices::gpus_available(m));
    // Rows: the two K2s of host 1 group with the one of host 2 (same
    // name and types); the Intel GPU alone. Pool order: host, then name.
    let pool = devices::gpu_tab_groups(m, None);
    assert_eq!(pool, vec![vec![r(PA), r(PB), r(PC)], vec![r(PI)]]);
    let h1 = r(H1);
    assert_eq!(
        devices::gpu_tab_groups(m, Some(&h1)),
        vec![vec![r(PA), r(PB)], vec![r(PI)]]
    );
    // Types, pass-through first, then fewest per GPU.
    let first = m.get(&r::<xapi::Pgpu>(PA)).unwrap();
    assert_eq!(
        devices::sorted_types_desc(m, &first.supported_vgpu_types),
        vec![r(PT), r(K260), r(K240), r(K220)]
    );
    assert_eq!(
        devices::vgpu_type_name(m, &r(K240)),
        Some(devices::VgpuTypeName::Virtual {
            model: "GRID K240Q".into(),
            per_gpu: 4,
            multiple: true
        })
    );
    // The bar: 4 slots of K240Q, web-01 in the first.
    let bar = devices::gpu_bar(m, first);
    assert_eq!((bar.capacity, bar.max_capacity), (4, 8));
    assert_eq!(bar.segments, vec![("web-01".into(), "GRID K240Q".into())]);
    let empty = devices::gpu_bar(m, m.get(&r::<xapi::Pgpu>(PB)).unwrap());
    assert_eq!((empty.capacity, empty.segments.len()), (8, 0));
    assert_eq!(devices::pgpu_name(m, first), "GK104GL [GRID K2]");
    assert_eq!(devices::placement_policy(m), PlacementPolicy::Density);
    // A type in use on the selection cannot be disallowed.
    assert!(devices::type_in_use(m, &[r(PA)], &r(K240)));
    assert!(!devices::type_in_use(m, &[r(PB)], &r(K240)));
    // Integrated GPU: host 1's Intel GPU is in use.
    let ig = devices::integrated_gpu(m, &h1);
    assert!(ig.current && ig.next);
    assert!(devices::can_change_integrated_gpu(m));

    // db-01 (halted HVM): the VM page, its Add list.
    let db = m.get(&r::<xapi::Vm>(DB)).unwrap();
    assert!(devices::vm_gpu_page_shown(m, db));
    let choices = devices::add_vgpu_choices(m, db, &[]);
    let texts: Vec<(Option<String>, bool)> = choices
        .iter()
        .map(|c| (c.vgpu_type.as_ref().map(|t| t.to_string()), c.selectable))
        .collect();
    assert_eq!(
        texts,
        vec![
            // Intel (pass-through only: under the group's name).
            (Some("OpaqueRef:vgpu-type-intel-passthrough".into()), true),
            // NVIDIA: header, then the types; K220Q is not allowed.
            (None, false),
            (Some(PT.into()), true),
            (Some(K260.into()), true),
            (Some(K240.into()), true),
            (Some(K220.into()), false),
        ]
    );
    assert!(
        matches!(&choices[0].text, VgpuChoiceText::Group(g) if g == "Intel Corporation HD Graphics P630 GPUs")
    );
    assert_eq!(devices::default_vgpu_choice(&choices), None);
    // After a K240Q, only K240Q (its compatible types).
    let k240 = PageVgpu {
        existing: None,
        group: r(GK),
        vgpu_type: Some(r(K240)),
        device: None,
    };
    let after = devices::add_vgpu_choices(m, db, std::slice::from_ref(&k240));
    let types: Vec<String> = after
        .iter()
        .filter_map(|c| c.vgpu_type.as_ref().map(|t| t.to_string()))
        .collect();
    assert_eq!(types, vec![K240.to_owned()]);
    assert_eq!(devices::default_vgpu_choice(&after), Some(1));
    let s = devices::gpu_page_state(m, db, std::slice::from_ref(&k240), "");
    assert!(s.can_add && s.need_gpu && s.block.is_none());
    assert_eq!(s.multiple, Some(true));
    assert!(!s.rdp_warning);
    // A pass-through GPU first: the Remote Desktop warning; no more adds.
    let pass = PageVgpu {
        vgpu_type: Some(r(PT)),
        ..k240.clone()
    };
    let s = devices::gpu_page_state(m, db, std::slice::from_ref(&pass), "");
    assert!(s.rdp_warning && !s.can_add);
    assert_eq!(s.multiple, Some(false));
    // web-01 runs: refused; HA "restart" too.
    let web = m.get(&r::<xapi::Vm>(WEB)).unwrap();
    assert_eq!(
        devices::gpu_page_state(m, web, &PageVgpu::of_vm(m, web), "").block,
        Some(DeviceBlock::NotHalted)
    );
    assert_eq!(PageVgpu::of_vm(m, web)[0].pci_slot().as_deref(), Some("11"));
    drop(state);
    server.set_field("pool", "OpaqueRef:pool-0001", "ha_enabled", json!(true));
    let state = synced(&h, conn, |m| m.pool().is_some_and(|(_, p)| p.ha_enabled)).await;
    let m = &state.connection(conn).unwrap().model;
    let db = m.get(&r::<xapi::Vm>(DB)).unwrap();
    assert_eq!(
        devices::device_block(m, db, "restart"),
        Some(DeviceBlock::Ha)
    );
    assert_eq!(devices::device_block(m, db, "best-effort"), None);
}

#[tokio::test]
async fn allowed_types_placement_and_integrated_gpu() {
    let server = device_pool().await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let model = h.backend.state().connection(conn).unwrap().model.clone();

    // Allow K220Q, disallow K260Q on the two K2s of host 1.
    let plan = devices::allowed_types_plan(
        &model,
        &[r(PA), r(PB)],
        &[(r(K220), true), (r(K260), false)],
    );
    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].1, vec![r(PT), r(K240), r(K220)]);
    let from = server.calls().len();
    let op = run(&h, conn, DeviceRequest::SetAllowedVgpuTypes { pgpus: plan }).await;
    ok(&op);
    assert_eq!(
        calls_since(&server, from, &["PGPU."]),
        vec!["PGPU.set_enabled_VGPU_types", "PGPU.set_enabled_VGPU_types"]
    );
    assert_eq!(
        strs(server.field("pgpu", PA, "enabled_VGPU_types")),
        vec![PT, K240, K220]
    );
    // The group's allowed types: those of its GPUs (host 2's still has
    // K260Q).
    assert_eq!(
        strs(server.field("gpu_group", GK, "enabled_VGPU_types")),
        vec![PT, K240, K220, K260]
    );
    // An unsupported type is refused.
    let op = run(
        &h,
        conn,
        DeviceRequest::SetAllowedVgpuTypes {
            pgpus: vec![(r(PI), vec![r(K240)])],
        },
    )
    .await;
    fails(&op, "VGPU_TYPE_NOT_SUPPORTED");

    // Placement: every group.
    let from = server.calls().len();
    let op = run(
        &h,
        conn,
        DeviceRequest::SetPlacementPolicy {
            algorithm: AllocationAlgorithm::BreadthFirst,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        calls_since(&server, from, &["GPU_group."]),
        vec![
            "GPU_group.set_allocation_algorithm",
            "GPU_group.set_allocation_algorithm"
        ]
    );
    for g in [GK, GI] {
        assert_eq!(
            server.field("gpu_group", g, "allocation_algorithm"),
            Some(json!("breadth_first"))
        );
    }
    let state = synced(&h, conn, |m| {
        devices::placement_policy(m) == PlacementPolicy::Performance
    })
    .await;
    drop(state);

    // Integrated GPU off: the display, then the system display device.
    let from = server.calls().len();
    let op = run(
        &h,
        conn,
        DeviceRequest::SetIntegratedGpu {
            host: r(H1),
            enable: false,
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        calls_since(&server, from, &["host.disable_display", "PGPU."]),
        vec![
            "Async.host.disable_display",
            "Async.PGPU.disable_dom0_access"
        ]
    );
    assert_eq!(
        server.field("host", H1, "display"),
        Some(json!("disable_on_reboot"))
    );
    assert_eq!(
        server.field("pgpu", PI, "dom0_access"),
        Some(json!("disable_on_reboot"))
    );
    let state = synced(&h, conn, |m| !devices::integrated_gpu(m, &r(H1)).next).await;
    let ig = devices::integrated_gpu(&state.connection(conn).unwrap().model, &r(H1));
    assert!(ig.current && !ig.next);
    // And back on before the reboot.
    ok(&run(
        &h,
        conn,
        DeviceRequest::SetIntegratedGpu {
            host: r(H1),
            enable: true,
        },
    )
    .await);
    assert_eq!(server.field("host", H1, "display"), Some(json!("enabled")));
    assert_eq!(
        server.field("pgpu", PI, "dom0_access"),
        Some(json!("enabled"))
    );
    let _ = H2;
}

#[tokio::test]
async fn a_vms_vgpus() {
    let server = device_pool().await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // db-01 (halted) gets a K260Q and a pass-through of the Intel group.
    let from = server.calls().len();
    let op = run(
        &h,
        conn,
        DeviceRequest::AssignVgpus {
            vm: r(DB),
            keep: vec![],
            add: vec![
                (r(GK), Some(r(K260))),
                (r(GI), Some(r("OpaqueRef:vgpu-type-intel-passthrough"))),
            ],
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        calls_since(&server, from, &["VGPU."]),
        vec!["Async.VGPU.create", "Async.VGPU.create"]
    );
    let vgpus = strs(server.field("vm", DB, "VGPUs"));
    assert_eq!(vgpus.len(), 2);
    assert_eq!(server.field("vgpu", &vgpus[0], "device"), Some(json!("0")));
    assert_eq!(server.field("vgpu", &vgpus[0], "type"), Some(json!(K260)));
    assert_eq!(
        server.field("vgpu", &vgpus[0], "other_config"),
        Some(json!({}))
    );
    synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(DB))
            .is_some_and(|v| v.vgpus.len() == 2)
    })
    .await;
    // Keep the first, drop the second.
    let from = server.calls().len();
    ok(&run(
        &h,
        conn,
        DeviceRequest::AssignVgpus {
            vm: r(DB),
            keep: vec![r(&vgpus[0])],
            add: vec![],
        },
    )
    .await);
    assert_eq!(calls_since(&server, from, &["VGPU."]), vec!["VGPU.destroy"]);
    assert_eq!(
        strs(server.field("vm", DB, "VGPUs")),
        vec![vgpus[0].clone()]
    );
    synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(DB))
            .is_some_and(|v| v.vgpus.len() == 1)
    })
    .await;
    // A type the group does not allow, and a running VM, are refused.
    fails(
        &run(
            &h,
            conn,
            DeviceRequest::AssignVgpus {
                vm: r(DB),
                keep: vec![r(&vgpus[0])],
                add: vec![(r(GK), Some(r(K220)))],
            },
        )
        .await,
        "VGPU_TYPE_NOT_ENABLED",
    );
    fails(
        &run(
            &h,
            conn,
            DeviceRequest::AssignVgpus {
                vm: r(WEB),
                keep: vec![],
                add: vec![],
            },
        )
        .await,
        "VM_BAD_POWER_STATE",
    );
    assert_eq!(strs(server.field("vm", WEB, "VGPUs")), vec![VWEB]);
}

#[tokio::test]
async fn usb_pass_through_and_devices() {
    let server = device_pool().await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    {
        let state = h.backend.state();
        let m = &state.connection(conn).unwrap().model;
        let host = m.get(&r::<xapi::Host>(H1)).unwrap();
        assert!(devices::usb_tab_shown(m, host));
        let list: Vec<String> = devices::host_pusbs(m, host)
            .iter()
            .map(|(r, _)| r.to_string())
            .collect();
        assert_eq!(list, vec![U1, U2, U3]);
        let u3 = m.get(&r::<xapi::Pusb>(U3)).unwrap();
        assert!(devices::pusb_in_use(m, u3));
        assert_eq!(devices::pusb_vm(m, u3).unwrap().name_label, "db-01");
        let u2 = m.get(&r::<xapi::Pusb>(U2)).unwrap();
        assert_eq!(
            devices::pusb_description(u2),
            devices::PusbDescription::Ids {
                vendor: "046d".into(),
                product: "c52b".into()
            }
        );
        // build-runner (halted HVM) has the page; a template does not.
        let build = m.get(&r::<xapi::Vm>(BUILD)).unwrap();
        assert!(devices::vm_usb_page_shown(m, build));
        let template = m
            .get(&r::<xapi::Vm>("OpaqueRef:vm-template-debian"))
            .unwrap();
        assert!(!devices::vm_usb_page_shown(m, template));
        let web = m.get(&r::<xapi::Vm>(WEB)).unwrap();
        assert_eq!(
            devices::device_block(m, web, ""),
            Some(DeviceBlock::NotHalted)
        );
    }
    // The servers build-runner can run on, and what can be attached.
    let PrecheckResult::PossibleHosts(Ok(hosts)) =
        precheck(&mut h, conn, Precheck::PossibleHosts { vm: r(BUILD) }).await
    else {
        panic!("possible hosts")
    };
    assert_eq!(hosts, vec![r(H1)]); // host 2 is in maintenance mode
    let state = h.backend.state();
    let m = state.connection(conn).unwrap().model.clone();
    assert_eq!(
        devices::attachable_pusbs(&m, &hosts),
        vec![(r(H1), vec![r(U1)])]
    );
    assert_eq!(
        devices::attachable_pusbs(&m, &[r(H1), r(H2)]),
        vec![(r(H1), vec![r(U1)]), (r(H2), vec![r(U4)])]
    );

    // Disabling a device in use is refused; enabling one works.
    fails(
        &run(
            &h,
            conn,
            DeviceRequest::SetUsbPassthrough {
                pusb: r(U3),
                enabled: false,
            },
        )
        .await,
        "USB_ALREADY_ATTACHED",
    );
    ok(&run(
        &h,
        conn,
        DeviceRequest::SetUsbPassthrough {
            pusb: r(U2),
            enabled: true,
        },
    )
    .await);
    assert_eq!(
        server.field("pusb", U2, "passthrough_enabled"),
        Some(json!(true))
    );

    // Attach: the upstream device model first, then the VUSB.
    let request = DeviceRequest::attach_usb(&m, &r(BUILD), &r(U1));
    assert_eq!(
        request.rbac_methods(),
        vec!["vm.set_platform", "vusb.create"]
    );
    let from = server.calls().len();
    ok(&run(&h, conn, request).await);
    assert_eq!(
        calls_since(&server, from, &["VM.set_platform", "VUSB."]),
        vec!["VM.set_platform", "VUSB.create"]
    );
    assert_eq!(
        server.field("vm", BUILD, "platform").unwrap()["device-model"],
        json!("qemu-upstream-compat")
    );
    let vusbs = strs(server.field("vm", BUILD, "VUSBs"));
    assert_eq!(vusbs.len(), 1);
    assert_eq!(
        server.field("vusb", &vusbs[0], "USB_group"),
        Some(json!(G1))
    );
    // Attached again: the device model is not set twice; the group is
    // taken (refused).
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(BUILD))
            .is_some_and(|v| devices::uses_upstream_qemu(v) && !v.vusbs.is_empty())
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    let again = DeviceRequest::attach_usb(&m, &r(DB), &r(U1));
    assert!(matches!(
        again,
        DeviceRequest::AttachUsb {
            set_platform: true,
            ..
        }
    ));
    fails(&run(&h, conn, again).await, "USB_GROUP_CONFLICT");

    // Detach build-runner's (not attached): destroy only.
    let from = server.calls().len();
    ok(&run(
        &h,
        conn,
        DeviceRequest::detach_usb(&m, &r(BUILD), &r(&vusbs[0])),
    )
    .await);
    assert_eq!(
        calls_since(&server, from, &["VUSB."]),
        vec!["Async.VUSB.destroy"]
    );
    assert!(strs(server.field("vm", BUILD, "VUSBs")).is_empty());
    // db-01's, plugged in with unplug allowed: unplug, then destroy.
    server.set_field("vusb", VUSB_DB, "currently_attached", json!(true));
    server.set_field("vusb", VUSB_DB, "allowed_operations", json!(["unplug"]));
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vusb>(VUSB_DB))
            .is_some_and(|v| v.currently_attached)
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    let from = server.calls().len();
    ok(&run(&h, conn, DeviceRequest::detach_usb(&m, &r(DB), &r(VUSB_DB))).await);
    assert_eq!(
        calls_since(&server, from, &["VUSB."]),
        vec![
            "VUSB.get_allowed_operations",
            "Async.VUSB.unplug",
            "Async.VUSB.destroy"
        ]
    );
    assert!(server.record("vusb", VUSB_DB).is_none());
}

#[tokio::test]
async fn vtpms_follow_the_allowed_operations() {
    let server = device_pool().await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let state = h.backend.state();
    let m = state.connection(conn).unwrap().model.clone();
    let build = m.get(&r::<xapi::Vm>(BUILD)).unwrap();
    let db = m.get(&r::<xapi::Vm>(DB)).unwrap();
    let web = m.get(&r::<xapi::Vm>(WEB)).unwrap();
    // The command: UEFI HVM VMs only.
    assert_eq!(devices::vtpm_command(&m, build), Ok(()));
    assert_eq!(devices::vtpm_command(&m, db), Ok(()));
    assert_eq!(
        devices::vtpm_command(&m, web),
        Err(devices::VtpmUnavailable::NotUefi)
    );
    // db-01 has one already: adding is not allowed; its vTPM can go.
    assert_eq!(
        devices::can_add_vtpm(&m, db),
        Err(VtpmRefusal::AddNotAllowed)
    );
    assert_eq!(
        devices::can_remove_vtpm(&m, &r("OpaqueRef:vtpm-db")),
        Ok(())
    );
    assert_eq!(devices::can_add_vtpm(&m, build), Ok(()));
    drop(state);

    ok(&run(&h, conn, DeviceRequest::AddVtpm { vm: r(BUILD) }).await);
    let vtpms = strs(server.field("vm", BUILD, "VTPMs"));
    assert_eq!(vtpms.len(), 1);
    assert_eq!(
        server.field("vtpm", &vtpms[0], "is_unique"),
        Some(json!(false))
    );
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(BUILD))
            .is_some_and(|v| !v.vtpms.is_empty())
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    let build = m.get(&r::<xapi::Vm>(BUILD)).unwrap();
    assert_eq!(
        devices::can_add_vtpm(&m, build),
        Err(VtpmRefusal::AddNotAllowed)
    );
    // A second is refused by the server.
    fails(
        &run(&h, conn, DeviceRequest::AddVtpm { vm: r(BUILD) }).await,
        "VTPM_MAX_AMOUNT_REACHED",
    );
    ok(&run(
        &h,
        conn,
        DeviceRequest::RemoveVtpm {
            vm: r(BUILD),
            vtpm: r(&vtpms[0]),
        },
    )
    .await);
    assert!(server.record("vtpm", &vtpms[0]).is_none());
    // A running VM's vTPM cannot be removed.
    server.set_vm_power_state(DB, "Running");
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vtpm>("OpaqueRef:vtpm-db"))
            .is_some_and(|t| t.allowed_operations.is_empty())
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    assert_eq!(
        devices::can_remove_vtpm(&m, &r("OpaqueRef:vtpm-db")),
        Err(VtpmRefusal::RemoveNotAllowed)
    );
    fails(
        &run(
            &h,
            conn,
            DeviceRequest::RemoveVtpm {
                vm: r(DB),
                vtpm: r("OpaqueRef:vtpm-db"),
            },
        )
        .await,
        "VM_BAD_POWER_STATE",
    );
}

#[tokio::test]
async fn memory_settings_live_halted_and_with_a_reboot() {
    let server = device_pool().await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let m = h.backend.state().connection(conn).unwrap().model.clone();

    // web-01 balloons (dynamic 1–2 GiB, static 4 GiB): Advanced.
    let mut edit = MemoryEdit::new(&m, &[r(WEB)]).unwrap();
    assert_eq!(edit.mode, MemoryMode::Advanced);
    assert_eq!(
        (
            edit.shown(MemoryField::DynMin),
            edit.shown(MemoryField::DynMax),
            edit.shown(MemoryField::StaticMax)
        ),
        (1.0, 2.0, 4.0)
    );
    // Only the range: live, no question.
    edit.set(MemoryField::DynMax, 3.0);
    let (confirm, changes) = edit.apply(&m);
    assert_eq!(confirm, None);
    assert_eq!(changes[0].static_max, 4 * GIB);
    assert_eq!(changes[0].dynamic_max, 3 * GIB);
    assert_eq!(changes[0].methods, vec!["vm.set_memory_dynamic_range"]);
    let from = server.calls().len();
    ok(&run(&h, conn, DeviceRequest::SetMemory(changes[0].clone())).await);
    assert_eq!(
        calls_since(&server, from, &["VM."]),
        vec!["VM.set_memory_dynamic_range"]
    );
    assert_eq!(
        server.field("vm", WEB, "memory_dynamic_max"),
        Some(json!(3 * GIB))
    );
    assert_eq!(server.vm_power_state(WEB), "Running");

    // The static maximum: the question, then shut down, set, start.
    let m = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(WEB))
            .is_some_and(|v| v.memory_dynamic_max == 3 * GIB)
    })
    .await
    .connection(conn)
    .unwrap()
    .model
    .clone();
    let mut edit = MemoryEdit::new(&m, &[r(WEB)]).unwrap();
    edit.set(MemoryField::StaticMax, 8.0);
    // The minimum went up with the ratio (8 GiB × 0.25 = 2 GiB).
    assert_eq!(edit.shown(MemoryField::DynMin), 2.0);
    let (confirm, changes) = edit.apply(&m);
    assert_eq!(confirm, Some(MemoryConfirm::StaticMax { plural: false }));
    assert_eq!(
        changes[0].methods,
        vec![
            "vm.set_memory_limits",
            "vm.clean_shutdown",
            "vm.start_on",
            "task.destroy"
        ]
    );
    let from = server.calls().len();
    ok(&run(&h, conn, DeviceRequest::SetMemory(changes[0].clone())).await);
    assert_eq!(
        calls_since(&server, from, &["VM.clean", "VM.set_memory", "VM.start"]),
        vec![
            "Async.VM.clean_shutdown",
            "VM.set_memory_limits",
            "Async.VM.start_on"
        ]
    );
    assert_eq!(
        server.field("vm", WEB, "memory_static_max"),
        Some(json!(8 * GIB))
    );
    assert_eq!(
        server.field("vm", WEB, "memory_dynamic_min"),
        Some(json!(2 * GIB))
    );
    assert_eq!(server.vm_power_state(WEB), "Running");

    // build-runner (halted, no ballooning): Basic, fixed; set_memory_limits.
    let mut edit = MemoryEdit::new(&m, &[r(BUILD)]).unwrap();
    assert_eq!(edit.mode, MemoryMode::Basic);
    assert!(!edit.dynamic_allowed());
    assert_eq!(
        edit.unavailable,
        Some(xen_core::memory::DmcUnavailable::NoTools { plural: false })
    );
    edit.set(MemoryField::Fixed, 3.0);
    let (confirm, changes) = edit.apply(&m);
    assert_eq!(confirm, None);
    let c = &changes[0];
    assert_eq!(
        (c.static_min, c.dynamic_min, c.dynamic_max, c.static_max),
        (GIB, 3 * GIB, 3 * GIB, 3 * GIB)
    );
    let from = server.calls().len();
    ok(&run(&h, conn, DeviceRequest::SetMemory(c.clone())).await);
    assert_eq!(
        calls_since(&server, from, &["VM."]),
        vec!["VM.set_memory_limits"]
    );
    assert_eq!(
        server.field("vm", BUILD, "memory_static_max"),
        Some(json!(3 * GIB))
    );

    // Below the static minimum: the server refuses.
    let mut bad = c.clone();
    bad.dynamic_min = GIB / 2;
    fails(
        &run(&h, conn, DeviceRequest::SetMemory(bad)).await,
        "MEMORY_CONSTRAINT_VIOLATION",
    );
}

#[tokio::test]
async fn remote_desktop_switch_and_address() {
    let server = device_pool().await;
    let mut h = harness();
    let conn = connect(&h, &server).await;
    server.set_field(
        "vm_guest_metrics",
        "OpaqueRef:vm-guest-metrics-web",
        "other",
        json!({"feature-balloon": "1", "feature-ts": "1", "feature-ts2": "1", "data-ts": "0"}),
    );
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(WEB))
            .is_some_and(|v| xen_core::consoles::rdp_state(m, v).control)
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    let web = m.get(&r::<xapi::Vm>(WEB)).unwrap();
    let rdp = xen_core::consoles::rdp_state(&m, web);
    assert!(rdp.can_enable() && !rdp.usable);
    // SSH: the first IPv4 address of the first interface.
    assert_eq!(
        xen_core::consoles::vm_ssh(&m, web).address.as_deref(),
        Some("192.0.2.101")
    );
    let from = server.calls().len();
    ok(&run(&h, conn, DeviceRequest::EnableRdp { vm: r(WEB) }).await);
    assert_eq!(
        calls_since(&server, from, &["VM.call_plugin"]),
        vec!["VM.call_plugin"]
    );
    let state = synced(&h, conn, |m| {
        m.get(&r::<xapi::Vm>(WEB))
            .is_some_and(|v| xen_core::consoles::rdp_state(m, v).usable)
    })
    .await;
    let m = state.connection(conn).unwrap().model.clone();
    let candidates = xen_core::consoles::rdp_candidates(&m, m.get(&r::<xapi::Vm>(WEB)).unwrap());
    assert!(candidates.contains(&"192.0.2.101".to_owned()));
    // The probe from this machine: a local listener stands in for the guest.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let PrecheckResult::RdpAddress(found) = precheck(
        &mut h,
        conn,
        Precheck::RdpAddress {
            candidates: vec!["192.0.2.1".into(), "127.0.0.1".into()],
            port,
        },
    )
    .await
    else {
        panic!("rdp address")
    };
    assert_eq!(found.as_deref(), Some("127.0.0.1"));
    // A VM without a guest agent cannot turn it on.
    fails(
        &run(&h, conn, DeviceRequest::EnableRdp { vm: r(BUILD) }).await,
        "VM_MISSING_PV_DRIVERS",
    );
    drop(listener);
}
