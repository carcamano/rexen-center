//! F9 on a real host through the real window code (an ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository), rehearsed on the mock.
//!
//! **Read only:** the pool's and the host's Networking tabs (networks and
//! IP configuration) and the NICs tab are checked against the cache's
//! records; for each network the Properties dialog is opened and its
//! values compared, then cancelled; the Configure IP Addresses dialog, the
//! New Network wizard and the Create Bond dialog are opened and cancelled.
//! Remove, Delete Bond, Rescan, OK and Finish are never pressed there: no
//! network, VLAN, bond, tunnel or PIF is made, changed or removed.
//!
//! **One VM's interfaces:** on the halted VM named
//! [`super::named_vm`] only, an interface is added on the network of its
//! first interface (an existing network), its QoS limit changed, then it is
//! removed; its other interfaces are left as they were, and the test checks
//! the VM ends with exactly the interfaces it had, still halted. If a step
//! fails, interfaces the test made are removed before it panics.

use egui_kittest::Harness;
use xapi::{Vif, VmPowerState};
use xen_core::network;

use super::provisioning::{last_enabled, press, select, shows, tab};
use super::*;
use crate::net_strings::net_strings;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); changes only the interfaces of the VM named by XENADMIN_REAL_NAMED_VM"]
fn real_f9_networking() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f9_run(&mut h, Duration::from_secs(120));
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run against the mock (the sanitized capture of the real host,
/// one halted guest renamed [`named_vm`]), so the real run cannot fail on a
/// test mistake.
#[test]
fn f9_real_run_on_the_mock() {
    let vm_label = named_vm();
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start_with_records(
        true,
        xapi_mock::capture_records(),
    ));
    server.set_name_label("vm", "OpaqueRef:vm-0010", vm_label);
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "certificate prompt", |h| {
        h.query_by_label(strings().cert_trust).is_some()
    });
    h.get_by_label(strings().cert_trust).click();
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    let before: Vec<String> = server.refs("vif");
    let log = f9_run(&mut h, Duration::from_secs(20));
    assert!(log.contains("interfaces restored"), "{log}");
    // Nothing networking-wide was called; the VM's interfaces are as before.
    for call in server.calls() {
        assert!(
            ![
                "network.",
                "PIF.",
                "Bond.",
                "VLAN.",
                "tunnel.",
                "pool.",
                "host.management"
            ]
            .iter()
            .any(|p| call.trim_start_matches("Async.").starts_with(p) && !call.contains(".get_")),
            "{call}"
        );
    }
    assert_eq!(server.refs("vif"), before);
    assert!(server.calls().iter().any(|c| c == "Async.VIF.create"));
    assert!(server.calls().iter().any(|c| c == "Async.VIF.destroy"));
}

/// The interfaces of `vm`: (reference, device, network, MAC, QoS type,
/// QoS limit).
fn vifs_of(
    h: &Harness<'_, XenAdminApp>,
    vm: &Ref<Vm>,
) -> Vec<(Ref<Vif>, String, String, String, String, String)> {
    let conn = first_conn(h).unwrap();
    let m = &conn.model;
    let mut out: Vec<_> = m.vms[vm]
        .vifs
        .iter()
        .filter_map(|r| m.vifs.get_key_value(r))
        .map(|(r, v)| {
            (
                r.clone(),
                v.device.clone(),
                v.network.to_string(),
                v.mac.clone(),
                v.qos_algorithm_type.clone(),
                network::vif_limit(v).to_owned(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn f9_run(h: &mut Harness<'static, XenAdminApp>, long: Duration) -> String {
    let vm_label = named_vm();
    let mut log = String::new();
    let n = net_strings();
    let t = tab_strings();
    let state = h.state().backend.state();
    let conn = state.connections[0].clone();
    let m = conn.model.clone();
    let ctx = tabs::Ctx {
        model: &m,
        now: 0,
        show_hidden: false,
    };
    let pool = m.pool().map(|(r, _)| r.clone()).expect("a pool");
    note(
        &mut log,
        format!(
            "hosts {} networks {} PIFs {} bonds {} VLANs {} tunnels {} VIFs {}",
            m.hosts.len(),
            m.networks.len(),
            m.pifs.len(),
            m.bonds.len(),
            m.vlans.len(),
            m.tunnels.len(),
            m.vifs.len()
        ),
    );

    // ---- read only: the pool's Networking tab -------------------------------
    select(h, Selection::Pool(conn.id, pool.clone()));
    tab(h, t.tab_networking);
    let (table, refs) = tabs::network::network_rows(ctx, None);
    for row in &table.rows {
        assert!(shows(h, &row.cells[0]), "network {:?} listed", row.cells[0]);
        note(&mut log, format!("pool network {:?}", row.cells));
    }
    assert_eq!(
        refs.len(),
        xen_core::objects::shown_networks(&m, false).len()
    );
    for (i, r) in refs.iter().enumerate() {
        let net = &m.networks[r];
        h.state_mut().network.select_network(conn.id, r.clone());
        settle(h);
        let can_edit = network::network_can_edit(&m, net);
        let can_remove = network::network_can_remove(&m, net);
        assert_eq!(
            last_enabled(h, n.network_properties),
            can_edit,
            "Properties of {:?}",
            table.rows[i].cells[0]
        );
        assert_eq!(
            last_enabled(h, n.remove_network),
            can_remove,
            "Remove of {:?}",
            table.rows[i].cells[0]
        );
        if !can_edit {
            note(
                &mut log,
                format!("   {:?}: properties not offered", table.rows[i].cells[0]),
            );
            continue;
        }
        press(h, n.network_properties);
        step_until(h, "properties", |h| {
            h.query_by_label(&fill(
                n.properties_title,
                &[("name", &xen_core::objects::network_name(&m, net))],
            ))
            .is_some()
        });
        let (name, description, tags, auto, mtu, nic, vlan) = {
            let d = h.state_mut().network.properties().expect("open");
            (
                d.name.clone(),
                d.description.clone(),
                d.tags.clone(),
                d.auto_plug,
                d.mtu.clone(),
                d.nic.clone(),
                d.vlan.clone(),
            )
        };
        assert_eq!(name, net.name_label);
        assert_eq!(description, net.name_description);
        assert_eq!(tags, net.tags.join(", "));
        assert_eq!(auto, network::network_auto_plug(net));
        assert_eq!(mtu, net.mtu.to_string());
        note(
            &mut log,
            format!(
                "   {:?}: properties as recorded (auto {auto}, MTU {mtu}, NIC/VLAN editable {}, VLAN {vlan}), remove offered {can_remove}",
                table.rows[i].cells[0],
                nic.is_some()
            ),
        );
        for _ in 0..4 {
            if h.state_mut().network.properties().is_none() {
                break;
            }
            settle(h);
            press(h, n.cancel);
        }
        assert!(
            h.state_mut().network.properties().is_none(),
            "properties cancelled"
        );
    }
    let (ip_table, _) = tabs::network::ip_configuration_rows(ctx, None);
    for row in &ip_table.rows {
        note(
            &mut log,
            format!("pool IP configuration {:?}", &row.cells[1..6]),
        );
    }

    // ---- read only: each host's Networking and NICs tabs ---------------------
    let hosts: Vec<_> = m.hosts.keys().cloned().collect();
    for host in &hosts {
        select(h, Selection::Host(conn.id, host.clone()));
        tab(h, t.tab_networking);
        let (table, _) = tabs::network::network_rows(ctx, Some(host));
        for row in &table.rows {
            assert!(shows(h, &row.cells[0]), "host network {:?}", row.cells[0]);
        }
        let (ip_table, ip_refs) = tabs::network::ip_configuration_rows(ctx, Some(host));
        for (row, r) in ip_table.rows.iter().zip(&ip_refs) {
            let pif = &m.pifs[r];
            assert_eq!(row.cells[5], pif.ip);
            assert_eq!(row.cells[6], pif.netmask);
            assert!(shows(h, &row.cells[1]), "interface {:?}", row.cells[1]);
        }
        note(
            &mut log,
            format!(
                "host networks {} IP rows {}",
                table.rows.len(),
                ip_table.rows.len()
            ),
        );
        // Configure IP Addresses: its pages, then Cancel.
        press(h, n.configure_ip);
        let pages = h.state_mut().network.ip_dialog().map(|d| {
            d.pages
                .iter()
                .map(|p| p.purpose.clone().unwrap_or_else(|| "Management".into()))
                .collect::<Vec<_>>()
        });
        let pages = pages.expect("the dialog opened");
        assert_eq!(pages.len(), ip_rows_of(&m, host));
        note(
            &mut log,
            format!("   Configure IP Addresses pages {pages:?} (cancelled)"),
        );
        for _ in 0..4 {
            if h.state_mut().network.ip_dialog().is_none() {
                break;
            }
            settle(h);
            press(h, n.cancel);
        }
        assert!(
            h.state_mut().network.ip_dialog().is_none(),
            "IP dialog cancelled"
        );

        tab(h, t.tab_nics);
        let (nics, nic_refs) = tabs::network::nic_rows(ctx, host);
        for (row, r) in nics.rows.iter().zip(&nic_refs) {
            let pif = &m.pifs[r];
            assert_eq!(row.cells[1], pif.mac);
            assert!(shows(h, &row.cells[0]), "NIC {:?}", row.cells[0]);
            note(&mut log, format!("   NIC {:?}", &row.cells[..5]));
        }
        assert!(!last_enabled(h, n.delete_bond_button), "nothing selected");
        // Create Bond: the host's NICs, then Cancel.
        press(h, n.create_bond_button);
        let rows = h
            .state_mut()
            .network
            .bond_form()
            .map(|f| f.rows.len())
            .expect("open");
        assert_eq!(rows, network::host_nics(&m, host).len());
        note(
            &mut log,
            format!("   Create Bond lists {rows} NICs (cancelled)"),
        );
        for _ in 0..4 {
            if h.state_mut().network.bond_form().is_none() {
                break;
            }
            settle(h);
            press(h, n.cancel);
        }
        assert!(
            h.state_mut().network.bond_form().is_none(),
            "bond dialog cancelled"
        );
    }

    // The New Network wizard's first page, cancelled.
    select(h, Selection::Pool(conn.id, pool));
    tab(h, t.tab_networking);
    press(h, n.add_network);
    h.get_by_label(n.page_type_title);
    let chin = crate::new_network::NewNetworkWizard::chin_offered(&m);
    let shown = crate::new_network::NewNetworkWizard::chin_shown(&m);
    note(
        &mut log,
        format!(
            "New Network wizard opened (cross-server shown {shown}, offered {chin}), cancelled"
        ),
    );
    for _ in 0..4 {
        if h.state().network.wizard.is_none() {
            break;
        }
        settle(h);
        press(h, n.cancel);
    }
    assert!(h.state().network.wizard.is_none());
    let others: Vec<_> = h
        .state()
        .backend
        .state()
        .operations
        .iter()
        .filter(|o| {
            !matches!(
                o.kind,
                xen_core::OperationKind::Connect | xen_core::OperationKind::Reconnect
            )
        })
        .map(|o| format!("{:?}", o.kind))
        .collect();
    assert!(
        others.is_empty(),
        "read-only part ran operations: {others:?}"
    );
    note(&mut log, "read-only part: no operation ran".into());

    // ---- one VM's interfaces -------------------------------------------------
    let named: Vec<(Ref<Vm>, Arc<VmRecord>)> = m
        .vms
        .iter()
        .filter(|(_, v)| v.name_label == vm_label)
        .map(|(r, v)| (r.clone(), v.clone()))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "refusing: {} VMs are named {vm_label:?}",
        named.len()
    );
    let (vm, record) = named.into_iter().next().unwrap();
    assert!(model::is_guest(&record), "refusing: not a guest");
    assert_eq!(
        record.power_state,
        VmPowerState::Halted,
        "refusing: {vm_label:?} is not halted"
    );
    let original = vifs_of(h, &vm);
    assert!(
        !original.is_empty(),
        "refusing: {vm_label:?} has no interface whose network to reuse"
    );
    note(
        &mut log,
        format!(
            "{vm_label:?} halted with {} interfaces: {:?}",
            original.len(),
            original
                .iter()
                .map(|v| (&v.1, &v.4, &v.5))
                .collect::<Vec<_>>()
        ),
    );
    let net = m.vifs[&original[0].0].network.clone();
    let net_name = xen_core::objects::network_name(&m, &m.networks[&net]);
    let vm_name = vm_label;
    let wait = |h: &mut Harness<'static, XenAdminApp>, title: &str| -> Result<(), String> {
        let ok = step_for(h, long, |h| {
            let state = h.state().backend.state();
            let ops: Vec<_> = state
                .operations
                .iter()
                .filter(|o| i18n::operation_title(&o.kind, &o.server) == title)
                .collect();
            !ops.is_empty() && ops.iter().all(|o| !o.is_running())
        });
        if !ok {
            return Err(format!("{title}: timed out"));
        }
        let state = h.state().backend.state();
        let op = state
            .operations
            .iter()
            .rev()
            .find(|o| i18n::operation_title(&o.kind, &o.server) == title)
            .unwrap();
        for _ in 0..15 {
            h.step();
            std::thread::sleep(Duration::from_millis(50));
        }
        match &op.status {
            xen_core::OperationStatus::Succeeded => Ok(()),
            other => Err(format!("{title}: {other:?}")),
        }
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut log = String::new();
        select(h, Selection::Vm(conn.id, vm.clone()));
        tab(h, t.tab_networking);
        // Add Interface on the first interface's network, QoS 1024 kB/s.
        press(h, n.add_interface);
        h.get_by_label(n.add_vif_title);
        {
            let d = h.state_mut().network.vif_dialog().expect("open");
            d.network = Some(net.clone());
            d.auto_mac = true;
            d.qos = true;
            d.qos_limit = "1024".into();
        }
        settle(h);
        press(h, n.add);
        wait(h, &fill(n.op_new_vif, &[("vm", vm_name)])).unwrap();
        let after = vifs_of(h, &vm);
        let added: Vec<_> = after
            .iter()
            .filter(|v| !original.iter().any(|o| o.0 == v.0))
            .cloned()
            .collect();
        assert_eq!(added.len(), 1, "one interface added");
        let added = added.into_iter().next().unwrap();
        assert_eq!(added.2, net.to_string());
        assert_eq!((added.4.as_str(), added.5.as_str()), ("ratelimit", "1024"));
        let vif_record = first_conn(h).unwrap().model.vifs[&added.0].clone();
        assert!(
            !vif_record.currently_attached,
            "a halted VM's interface is not plugged"
        );
        note(
            &mut log,
            format!(
                "added device {} on {net_name:?} MAC (generated) QoS 1024: ok",
                added.1
            ),
        );
        // Activate is not offered on a halted VM.
        h.state_mut().network.select_vif(conn.id, added.0.clone());
        settle(h);
        assert!(!last_enabled(h, n.activate), "Activate on a halted VM");
        // Properties: QoS 2048.
        press(h, n.interface_properties);
        h.get_by_label(n.vif_properties_title);
        h.state_mut().network.vif_dialog().expect("open").qos_limit = "2048".into();
        settle(h);
        for _ in 0..4 {
            if h.state_mut().network.vif_dialog().is_none() {
                break;
            }
            settle(h);
            press(h, n.ok);
        }
        wait(
            h,
            &fill(n.op_update_vif, &[("network", &net_name), ("vm", vm_name)]),
        )
        .unwrap();
        let after = vifs_of(h, &vm);
        let edited: Vec<_> = after
            .iter()
            .filter(|v| !original.iter().any(|o| o.0 == v.0))
            .cloned()
            .collect();
        assert_eq!(edited.len(), 1);
        let edited = edited.into_iter().next().unwrap();
        assert_eq!(
            (edited.1.as_str(), edited.3.as_str(), edited.5.as_str()),
            (added.1.as_str(), added.3.as_str(), "2048")
        );
        note(
            &mut log,
            format!("properties: device {} QoS 2048, same MAC: ok", edited.1),
        );
        // Remove Interface, confirmed.
        h.state_mut().network.select_vif(conn.id, edited.0.clone());
        settle(h);
        press(h, n.remove_interface);
        h.get_by_label(n.vif_delete_text);
        press(h, n.yes);
        wait(
            h,
            &fill(n.op_delete_vif, &[("network", &net_name), ("vm", vm_name)]),
        )
        .unwrap();
        log
    }));
    // Whatever happened: interfaces the test made go away.
    let now = vifs_of(h, &vm);
    for extra in now.iter().filter(|v| !original.iter().any(|o| o.0 == v.0)) {
        note(
            &mut log,
            format!("cleanup: removing interface device {}", extra.1),
        );
        h.state().backend.send(xen_core::Command::Action {
            connection: conn.id,
            request: xen_core::ActionRequest::DeleteVif {
                vif: extra.0.clone(),
            },
        });
        step_for(h, long, |h| {
            first_conn(h).is_some_and(|c| !c.model.vifs.contains_key(&extra.0))
        });
    }
    match outcome {
        Ok(steps) => log.push_str(&steps),
        Err(e) => {
            note(&mut log, "a step failed; cleaned up".into());
            std::panic::resume_unwind(e);
        }
    }
    let end = vifs_of(h, &vm);
    assert_eq!(end, original, "the VM's interfaces are as they were");
    let power = first_conn(h).unwrap().model.vms[&vm].power_state.clone();
    assert_eq!(power, VmPowerState::Halted);
    note(
        &mut log,
        format!(
            "interfaces restored ({} as found), VM still halted",
            end.len()
        ),
    );
    // Every operation was on that VM.
    only_on(h, &vm);
    let ops: Vec<String> = h
        .state()
        .backend
        .state()
        .operations
        .iter()
        .map(|o| i18n::operation_title(&o.kind, "<host>"))
        .collect();
    note(&mut log, format!("history: {ops:?}"));
    log
}

/// The IP configuration rows of `host` (the dialog's pages).
fn ip_rows_of(m: &ConnectionModel, host: &Ref<Host>) -> usize {
    let ctx = tabs::Ctx {
        model: m,
        now: 0,
        show_hidden: false,
    };
    tabs::network::ip_configuration_rows(ctx, Some(host))
        .1
        .len()
}
