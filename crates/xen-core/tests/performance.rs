//! F6: performance data, alerts and performance alert settings through the
//! backend against the mock (the real-host capture): the whole RRD then
//! `rrd_updates` every interval, in JSON or XML, for a host and a VM that
//! stops and starts again; alerts from messages and their dismissal; and
//! saving `other_config["perfmon"]` with the perfmon refresh.

mod common;

use std::time::Duration;

use common::{first, memory_store, spawn, wait_until};
use xapi::{Host, Ref, Vm};
use xapi_mock::MockServer;
use xen_core::perfmon::{self, PerfmonConfig};
use xen_core::performance::{self, PerfHandle, PerfStatus, PerfTarget};
use xen_core::rbac::{self, RbacCheck};
use xen_core::{ActionRequest, Command, ConnectionId, Notification, PerfmonObjectRef, VmAction};
use xen_rrd::{Format, Interval};

const GUEST: &str = "OpaqueRef:vm-0001";
const HALTED: &str = "OpaqueRef:vm-0010";
const HOST: &str = "OpaqueRef:host-0001";

fn options(perfmon_refresh: bool) -> xen_core::BackendOptions {
    xen_core::BackendOptions {
        perf_interval: Duration::from_millis(150),
        perfmon_refresh,
        ..common::fast_options(memory_store().0)
    }
}

async fn connect(
    h: &mut common::Harness,
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
    match h.next_notification().await {
        Notification::CertificatePrompt(p) => p.accept(),
        other => panic!("unexpected {other:?}"),
    }
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    first(&state).id
}

fn open(h: &common::Harness, conn: ConnectionId, target: PerfTarget) -> PerfHandle {
    let (handle, link) = performance::pair();
    h.backend.send(Command::OpenPerformance {
        connection: conn,
        target,
        link,
    });
    handle
}

fn last5(p: &PerfHandle, name: &str) -> Option<i64> {
    p.data()
        .archives
        .series(Interval::FiveSeconds, name)
        .last()
        .map(|x| x.0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_graphs_load_the_whole_rrd_then_follow_updates() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = spawn(options(true));
    let conn = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let perf = open(&h, conn, PerfTarget::Host(Ref::<Host>::new(HOST)));
    wait_until("live", || perf.data().status == PerfStatus::Live).await;
    let data = perf.data();
    assert_eq!(data.format, Some(Format::Json));
    assert!(!data.sources.is_empty(), "data sources with units");
    assert!(
        data.sources
            .iter()
            .any(|s| s.name_label == "cpu0" && s.units == "(fraction)")
    );
    // Every archive filled from host_rrds.
    for interval in Interval::ALL {
        let n = data.archives.series(interval, "cpu0").len();
        assert!(n >= interval.rows().min(120), "{interval:?}: {n}");
    }
    // Only the host's own columns are kept (rrd_updates host=true also
    // lists the VMs running there).
    assert!(
        data.archives
            .series(Interval::FiveSeconds, "memory_internal_free")
            .is_empty()
    );
    assert!(server.call_count("http/host_rrds") == 1);

    // Updates arrive as the clock moves.
    let before = last5(&perf, "cpu0").unwrap();
    server.advance_rrd_clock(30);
    wait_until("newer 5 s rows", || {
        last5(&perf, "cpu0").is_some_and(|t| t >= before + 30)
    })
    .await;
    assert!(perf.data().updates >= 1);
    // The graph points: CPU in percent, used memory in bytes.
    let d = perf.data();
    let now = last5(&perf, "cpu0").unwrap();
    let cpu = performance::plot_points(&d.archives, &d.sources, "cpu0", now - 599, now);
    assert!(cpu.len() >= 100 && cpu.iter().all(|p| (0.0..=100.0).contains(&p.1)));
    let used = performance::plot_points(&d.archives, &d.sources, "memory_free_kib", now - 599, now);
    assert!(used.iter().all(|p| p.1 > 1.0e9), "used memory in bytes");

    // Closing the handle stops the requests.
    drop(perf);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = server.call_count("http/rrd_updates");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(server.call_count("http/rrd_updates"), calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_json_answers_in_xml() {
    let server = MockServer::start_with_records(false, xapi_mock::capture_records()).await;
    server.set_rrd_json(false);
    let h = spawn(options(true));
    h.backend.send(Command::Connect {
        server: server.address(),
        username: xapi_mock::USERNAME.into(),
        password: Some(xapi_mock::PASSWORD.into()),
        remember_password: false,
    });
    let state = h.wait_conn("connected", |c| c.status.is_connected()).await;
    let conn = first(&state).id;
    let perf = open(&h, conn, PerfTarget::Vm(Ref::<Vm>::new(GUEST)));
    wait_until("live", || perf.data().status == PerfStatus::Live).await;
    let before = last5(&perf, "cpu0").unwrap();
    server.advance_rrd_clock(20);
    wait_until("newer rows", || {
        last5(&perf, "cpu0").is_some_and(|t| t > before)
    })
    .await;
    let data = perf.data();
    assert_eq!(data.format, Some(Format::Xml));
    assert!(!data.archives.series(Interval::OneDay, "memory").is_empty());
    assert!(
        !data
            .archives
            .series(Interval::FiveSeconds, "vbd_xvda_read")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vm_that_stops_keeps_its_graphs_and_resumes_when_started() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    server.set_task_timing(1, Duration::from_millis(20));
    let mut h = spawn(options(true));
    let conn = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let perf = open(&h, conn, PerfTarget::Vm(Ref::<Vm>::new(GUEST)));
    wait_until("live", || perf.data().status == PerfStatus::Live).await;
    assert!(server.call_count("http/vm_rrds") == 1);
    assert!(
        perf.data()
            .sources
            .iter()
            .any(|s| s.name_label == "memory_internal_free")
    );

    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new(GUEST),
        action: VmAction::HardShutdown,
    });
    wait_until("not running", || {
        perf.data().status == PerfStatus::NotRunning
    })
    .await;
    // What was drawn stays.
    assert!(
        !perf
            .data()
            .archives
            .series(Interval::FiveSeconds, "cpu0")
            .is_empty()
    );
    let updates = server.call_count("http/rrd_updates");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        server.call_count("http/rrd_updates"),
        updates,
        "no updates while halted"
    );

    h.backend.send(Command::Vm {
        connection: conn,
        vm: Ref::new(GUEST),
        action: VmAction::Start,
    });
    wait_until("live again", || perf.data().status == PerfStatus::Live).await;
    server.advance_rrd_clock(15);
    let calls = server.call_count("http/rrd_updates");
    wait_until("updates again", || {
        server.call_count("http/rrd_updates") > calls + 1
    })
    .await;
    // The archive was read again on stop (from the coordinator) and start.
    assert_eq!(server.call_count("http/vm_rrds"), 3);

    // A VM halted from the start: its archive, no updates.
    let halted = open(&h, conn, PerfTarget::Vm(Ref::<Vm>::new(HALTED)));
    wait_until("halted", || halted.data().status == PerfStatus::NotRunning).await;
    assert!(
        !halted
            .data()
            .archives
            .series(Interval::OneHour, "cpu0")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_sources_without_units_are_asked_for_again() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    server.set_data_source_units(false);
    let mut h = spawn(options(true));
    let conn = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let perf = open(&h, conn, PerfTarget::Vm(Ref::<Vm>::new(GUEST)));
    wait_until("live", || perf.data().status == PerfStatus::Live).await;
    let d = perf.data();
    assert!(d.sources.iter().all(|s| s.units.is_empty()));
    // Drawn from the names meanwhile: CPU in percent.
    assert_eq!(
        performance::unit_of("cpu0", &d.sources).0,
        performance::Unit::Percent
    );
    let asked = server.call_count("VM.get_data_sources");
    server.set_data_source_units(true);
    wait_until("complete data sources", || {
        perf.data().sources.iter().any(|s| s.units == "(fraction)")
    })
    .await;
    assert!(server.call_count("VM.get_data_sources") > asked);
    // Complete: not asked again.
    let asked = server.call_count("VM.get_data_sources");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(server.call_count("VM.get_data_sources"), asked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alerts_come_from_messages_and_are_dismissed() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = spawn(options(true));
    let conn = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let state = h.backend.state();
    let model = first(&state).model.clone();
    let before = xen_core::alerts::alerts(&model);
    let events_before =
        xen_core::alerts::graph_events(&model, &model.vms[&Ref::<Vm>::new(GUEST)].uuid).len();
    let vm_uuid = model.vms[&Ref::<Vm>::new(GUEST)].uuid.clone();
    let body = "value: 0.97\nconfig:\n<variable>\n<name value=\"cpu_usage\"/>\n<alarm_trigger_level value=\"0.9\"/>\n<alarm_trigger_period value=\"60\"/>\n</variable>";
    let alarm = server.create_message("ALARM", 3, "VM", &vm_uuid, body);
    // A lifecycle message is not an alert.
    server.create_message("VM_STARTED", 5, "VM", &vm_uuid, "started");
    let state = h
        .wait_conn("the alarm", |c| {
            c.model.messages.contains_key(&Ref::new(&alarm))
        })
        .await;
    let model = first(&state).model.clone();
    let alerts = xen_core::alerts::alerts(&model);
    assert_eq!(alerts.len(), before.len() + 1);
    let a = alerts.iter().find(|a| a.message.as_str() == alarm).unwrap();
    assert_eq!(a.object, xen_core::alerts::AlertObject::Vm(Ref::new(GUEST)));
    let parsed = a.alarm.as_ref().unwrap();
    assert_eq!(
        (parsed.variable.as_str(), parsed.value),
        ("cpu_usage", 0.97)
    );
    let lifecycle = xen_core::alerts::graph_events(&model, &vm_uuid);
    assert_eq!(lifecycle.len(), events_before + 1);

    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::DismissAlerts {
            messages: vec![Ref::new(&alarm)],
        },
    });
    h.wait_conn("dismissed", |c| {
        !c.model.messages.contains_key(&Ref::new(&alarm))
    })
    .await;
    let state = h.backend.state();
    assert_eq!(
        xen_core::alerts::alerts(&first(&state).model).len(),
        before.len()
    );
    // Dismissing one already gone is not an error.
    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::DismissAlerts {
            messages: vec![Ref::new(&alarm)],
        },
    });
    let state = h
        .wait_for("both done", |s| {
            s.operations.iter().filter(|o| !o.is_running()).count() >= 3
        })
        .await;
    assert!(
        state
            .operations
            .iter()
            .all(|o| !matches!(o.status, xen_core::OperationStatus::Failed(_)))
    );
    assert_eq!(server.call_count("message.destroy"), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn performance_alerts_are_saved_and_perfmon_refreshed() {
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = spawn(options(true));
    let conn = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let mut form = perfmon::PerfmonForm::new(perfmon::PerfmonObject::Vm, &PerfmonConfig::default());
    let cpu = form.setting_mut(perfmon::AlertGroup::Cpu).unwrap();
    cpu.enabled = true;
    cpu.level = 80.0;
    cpu.minutes = 2.0;
    let xml = form.apply(&PerfmonConfig::default()).to_xml();
    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::SetPerfmon {
            object: PerfmonObjectRef::Vm(Ref::new(GUEST)),
            config: Some(xml.clone()),
        },
    });
    let state = h
        .wait_conn("saved", |c| {
            c.model.vms[&Ref::<Vm>::new(GUEST)]
                .other_config
                .get("perfmon")
                == Some(&xml)
        })
        .await;
    let saved = PerfmonConfig::of(&first(&state).model.vms[&Ref::<Vm>::new(GUEST)].other_config);
    let v = saved.get(perfmon::CPU_USAGE).unwrap();
    assert_eq!(
        (v.level, v.period, v.inhibit),
        (Some(0.8), Some(120.0), Some(3600.0))
    );
    wait_until("refresh", || {
        server
            .plugin_calls()
            .contains(&("perfmon".into(), "refresh".into()))
    })
    .await;

    // Nothing enabled: the key goes away.
    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::SetPerfmon {
            object: PerfmonObjectRef::Vm(Ref::new(GUEST)),
            config: None,
        },
    });
    h.wait_conn("removed", |c| {
        !c.model.vms[&Ref::<Vm>::new(GUEST)]
            .other_config
            .contains_key("perfmon")
    })
    .await;
    // The host and SR forms too.
    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::SetPerfmon {
            object: PerfmonObjectRef::Host(Ref::new(HOST)),
            config: Some("<config><variable><name value=\"memory_free_kib\" /><alarm_trigger_level value=\"1024\" /></variable></config>".into()),
        },
    });
    h.wait_conn("host saved", |c| {
        c.model.hosts[&Ref::<Host>::new(HOST)]
            .other_config
            .contains_key("perfmon")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_refresh_only_the_object_is_written_and_rbac_is_checked() {
    // The lab pool, whose roles list the permissions.
    let lab = MockServer::start(true).await;
    let mut h = spawn(options(false));
    let conn = connect(&mut h, &lab, "vm-operator-user", "vm-operator-password").await;
    let state = h.backend.state();
    let view = first(&state);
    let set = ActionRequest::SetPerfmon {
        object: PerfmonObjectRef::Vm(Ref::new("OpaqueRef:vm-web")),
        config: Some("<config/>".into()),
    };
    let dismiss = ActionRequest::DismissAlerts { messages: vec![] };
    for request in [&set, &dismiss] {
        match rbac::check(&view.model, view.rbac.as_deref(), &request.rbac_methods()) {
            RbacCheck::Denied { authorized } => assert!(!authorized.is_empty()),
            RbacCheck::Allowed => panic!("a VM operator may not {request:?}"),
        }
    }
    drop(state);
    // As root, without the refresh.
    let set = ActionRequest::SetPerfmon {
        object: PerfmonObjectRef::Vm(Ref::new(GUEST)),
        config: Some("<config/>".into()),
    };
    let server = MockServer::start_with_records(true, xapi_mock::capture_records()).await;
    let mut h = spawn(options(false));
    let conn2 = connect(&mut h, &server, xapi_mock::USERNAME, xapi_mock::PASSWORD).await;
    let _ = conn;
    h.backend.send(Command::Action {
        connection: conn2,
        request: set,
    });
    h.wait_conn("saved", |c| {
        c.model.vms[&Ref::<Vm>::new(GUEST)]
            .other_config
            .contains_key("perfmon")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(server.plugin_calls().is_empty());
}
