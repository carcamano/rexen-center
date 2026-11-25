//! F9 in the window, against the mock: a VM's Networking tab (Add
//! Interface with the MAC and QoS checks and the duplicate MAC question,
//! Properties, Deactivate / Activate, Remove with its confirmation, the
//! interface limit), the New Network wizard for every type (VLAN checks on
//! the page, bonds with the original's warnings), a network's Properties,
//! Remove Network, the NICs tab (Create Bond, Delete Bond, Rescan), the
//! Configure IP Addresses dialog (secondary interfaces, the management
//! warning, the in-use check, HA) and role elevation.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::Ref;
use xapi_mock::MockServer;
use xapi_mock::networking::bond_lab_records;

use super::provisioning::{catch_up, conn_id, last_enabled, press, select, select_vm, shows, tab};
use super::*;
use crate::net_strings::net_strings;
use crate::new_network::{BondChoice, NetType, Page};

const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const POOL: &str = "OpaqueRef:pool-0001";
const H1: &str = "OpaqueRef:host-0001";

fn field(server: &MockServer, class: &str, r: &str, f: &str) -> serde_json::Value {
    server.field(class, r, f).unwrap_or(serde_json::Value::Null)
}

fn text(server: &MockServer, class: &str, r: &str, f: &str) -> String {
    field(server, class, r, f)
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn network_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("network")
        .into_iter()
        .find(|n| text(server, "network", n, "name_label") == name)
}

fn vif_of(server: &MockServer, vm: &str, device: &str) -> Option<String> {
    field(server, "vm", vm, "VIFs")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|v| text(server, "vif", v, "device") == device)
        .map(str::to_owned)
}

/// Waits for the operation titled `title`; its error text if it failed.
fn wait_op(h: &mut Harness<'_, XenAdminApp>, title: &str) -> Result<(), String> {
    // The latest operation with that title has ended.
    step_until(h, title, |h| {
        let state = h.state().backend.state();
        let ops: Vec<_> = state
            .operations
            .iter()
            .filter(|o| i18n::operation_title(&o.kind, &o.server) == title)
            .collect();
        !ops.is_empty() && ops.iter().all(|o| !o.is_running())
    });
    let state = h.state().backend.state();
    let op = state
        .operations
        .iter()
        .rev()
        .find(|o| i18n::operation_title(&o.kind, &o.server) == title)
        .unwrap()
        .clone();
    catch_up(h);
    match &op.status {
        xen_core::OperationStatus::Succeeded => Ok(()),
        xen_core::OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            Err(i18n::describe_xapi_error(e))
        }
        other => Err(format!("{other:?}")),
    }
}

fn wait_ok(h: &mut Harness<'_, XenAdminApp>, title: &str) {
    if let Err(e) = wait_op(h, title) {
        panic!("{title}: {e}");
    }
}

/// The app connected to a mock with `records`.
fn connect_with(
    runtime: &tokio::runtime::Runtime,
    records: serde_json::Value,
) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start_with_records(false, records));
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| h.query_by_label("Lab Pool").is_some());
    (server, h)
}

fn wizard<'a>(h: &'a mut Harness<'_, XenAdminApp>) -> &'a mut crate::new_network::NewNetworkWizard {
    h.state_mut()
        .network
        .wizard
        .as_mut()
        .expect("the wizard is open")
}

fn select_pool(h: &mut Harness<'_, XenAdminApp>) {
    let c = conn_id(h);
    select(h, Selection::Pool(c, Ref::new(POOL)));
}

fn select_host(h: &mut Harness<'_, XenAdminApp>, r: &str) {
    let c = conn_id(h);
    select(h, Selection::Host(c, Ref::new(r)));
}

/// Presses a dialog's `label` until the dialog closes (a window that just
/// changed size re-centers over a few frames and may miss a click).
fn close_with(h: &mut Harness<'_, XenAdminApp>, label: &str) {
    for _ in 0..5 {
        settle(h);
        settle(h);
        press(h, label);
        if !dialog_open(h) {
            return;
        }
    }
    panic!("{label} did not close the dialog");
}

fn dialog_open(h: &mut Harness<'_, XenAdminApp>) -> bool {
    let net = &mut h.state_mut().network;
    net.properties().is_some()
        || net.ip_dialog().is_some()
        || net.vif_dialog().is_some()
        || net.bond_form().is_some()
}

/// Waits until the window's cache satisfies `pred`.
fn cached(h: &mut Harness<'_, XenAdminApp>, what: &str, pred: impl Fn(&ConnectionModel) -> bool) {
    step_until(h, what, |h| first_conn(h).is_some_and(|c| pred(&c.model)));
    catch_up(h);
}

#[test]
fn a_vms_interfaces_are_added_edited_toggled_and_removed() {
    let runtime = runtime();
    let (server, mut h) = connect_with(&runtime, xapi_mock::fixture_records());
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    let c = conn_id(&h);
    select_vm(&mut h, WEB);
    tab(&mut h, t.tab_networking);
    assert!(last_enabled(&h, n.add_interface));
    assert!(
        !last_enabled(&h, n.interface_properties),
        "nothing selected"
    );
    assert!(!last_enabled(&h, n.remove_interface));

    // Add Interface: the next free device, the first network.
    press(&mut h, n.add_interface);
    h.get_by_label(n.add_vif_title);
    {
        let d = h.state_mut().network.vif_dialog().unwrap();
        d.network = Some(Ref::new("OpaqueRef:net-eth1"));
        d.auto_mac = false;
        d.mac = "02:00:00:00:0a".into();
    }
    settle(&mut h);
    assert!(shows(&h, n.mac_invalid));
    assert!(!last_enabled(&h, n.add));
    {
        let d = h.state_mut().network.vif_dialog().unwrap();
        // db-01's MAC: the original's question.
        d.mac = "02:00:00:00:0b:01".into();
        d.qos = true;
        d.qos_limit = "0".into();
    }
    settle(&mut h);
    assert!(shows(&h, n.qos_invalid));
    h.state_mut().network.vif_dialog().unwrap().qos_limit = "4096".into();
    settle(&mut h);
    press(&mut h, n.add);
    h.get_by_label(n.duplicate_mac_title);
    assert!(shows(
        &h,
        &fill(
            n.duplicate_mac,
            &[("mac", "02:00:00:00:0b:01"), ("vm", "db-01")]
        )
    ));
    press(&mut h, n.yes);
    wait_ok(
        &mut h,
        "Creating new virtual network interface on VM web-01",
    );
    let vif = vif_of(&server, WEB, "2").expect("device 2");
    assert_eq!(text(&server, "vif", &vif, "network"), "OpaqueRef:net-eth1");
    assert_eq!(text(&server, "vif", &vif, "MAC"), "02:00:00:00:0b:01");
    assert_eq!(
        field(&server, "vif", &vif, "qos_algorithm_params")["kbps"],
        "4096"
    );
    // web-01 has its drivers: hot-plugged.
    assert_eq!(
        field(&server, "vif", &vif, "currently_attached"),
        json!(true)
    );
    cached(&mut h, "vif cached", |m| {
        m.vifs.contains_key(&Ref::new(&vif))
    });

    // Properties: the QoS limit off, an automatic MAC.
    h.state_mut().network.select_vif(c, Ref::new(&vif));
    settle(&mut h);
    assert!(last_enabled(&h, n.interface_properties));
    press(&mut h, n.interface_properties);
    h.get_by_label(n.vif_properties_title);
    {
        let d = h.state_mut().network.vif_dialog().unwrap();
        assert!(d.qos && d.qos_limit == "4096" && !d.auto_mac);
        d.qos = false;
        d.auto_mac = true;
    }
    settle(&mut h);
    press(&mut h, n.ok);
    wait_ok(
        &mut h,
        "Updating virtual network interface 'Storage network' on VM 'web-01'",
    );
    let vif = vif_of(&server, WEB, "2").unwrap();
    assert_eq!(text(&server, "vif", &vif, "qos_algorithm_type"), "");
    assert_ne!(text(&server, "vif", &vif, "MAC"), "02:00:00:00:0b:01");
    cached(&mut h, "new vif cached", |m| {
        m.vifs.contains_key(&Ref::new(&vif))
    });

    // Deactivate, Activate.
    h.state_mut().network.select_vif(c, Ref::new(&vif));
    settle(&mut h);
    press(&mut h, n.deactivate);
    wait_ok(
        &mut h,
        "Deactivating virtual network interface on VM 'web-01'",
    );
    assert_eq!(
        field(&server, "vif", &vif, "currently_attached"),
        json!(false)
    );
    cached(&mut h, "unplugged", |m| {
        m.vifs
            .get(&Ref::new(&vif))
            .is_some_and(|v| !v.currently_attached)
    });
    press(&mut h, n.activate);
    wait_ok(
        &mut h,
        "Activating virtual network interface on VM 'web-01'",
    );
    assert_eq!(
        field(&server, "vif", &vif, "currently_attached"),
        json!(true)
    );
    cached(&mut h, "plugged", |m| {
        m.vifs
            .get(&Ref::new(&vif))
            .is_some_and(|v| v.currently_attached)
    });

    // Remove Interface asks first; No keeps it.
    press(&mut h, n.remove_interface);
    h.get_by_label(n.vif_delete_text);
    press(&mut h, n.no);
    assert!(server.record("vif", &vif).is_some());
    press(&mut h, n.remove_interface);
    press(&mut h, n.yes);
    wait_ok(
        &mut h,
        "Deleting virtual network interface 'Storage network' from VM 'web-01'",
    );
    assert!(server.record("vif", &vif).is_none());

    // The interface limit (the template's recommendation): an error, no
    // dialog.
    server.set_field(
        "vm",
        WEB,
        "recommendations",
        json!("<restrictions><restriction max=\"2\" property=\"number-of-vifs\"/></restrictions>"),
    );
    cached(&mut h, "limit cached", |m| {
        m.vms[&Ref::new(WEB)]
            .recommendations
            .contains("number-of-vifs")
    });
    press(&mut h, n.add_interface);
    step_until(&mut h, "limit error", |h| {
        h.state().errors.iter().any(|e| {
            e.contains("maximum number of virtual network interfaces this VM can have is 2")
        })
    });
    assert!(h.state_mut().network.vif_dialog().is_none());

    // A halted VM: no Activate.
    select_vm(&mut h, DB);
    tab(&mut h, t.tab_networking);
    h.state_mut()
        .network
        .select_vif(c, Ref::new("OpaqueRef:vif-db-0"));
    settle(&mut h);
    assert!(!last_enabled(&h, n.activate));
    assert!(last_enabled(&h, n.interface_properties));
    assert!(last_enabled(&h, n.remove_interface));
}

#[test]
fn the_wizard_makes_external_and_private_networks() {
    let runtime = runtime();
    let (server, mut h) = connect_with(&runtime, xapi_mock::fixture_records());
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    select_pool(&mut h);
    tab(&mut h, t.tab_networking);

    // External (the default): name, NIC and VLAN checks on the page.
    press(&mut h, n.add_network);
    h.get_by_label(n.page_type_title);
    assert_eq!(wizard(&mut h).kind, NetType::External);
    // No Open vSwitch backend here: no cross-server private network.
    assert!(shows(&h, n.chin_needs_vswitch));
    press(&mut h, n.next);
    assert_eq!(wizard(&mut h).page, Page::Name);
    assert_eq!(wizard(&mut h).name, n.new_network_name);
    wizard(&mut h).name = "Guests VLAN 20".into();
    settle(&mut h);
    press(&mut h, n.next);
    // A name in use gets a suffix.
    assert_eq!(wizard(&mut h).name, "Guests VLAN 20 (1)");
    assert_eq!(wizard(&mut h).page, Page::Details);
    // NIC 0 first, the first free tag.
    assert_eq!(wizard(&mut h).nic, Some(Ref::new("OpaqueRef:pif-01-eth0")));
    assert_eq!(wizard(&mut h).vlan, "1");
    {
        let w = wizard(&mut h);
        w.nic = Some(Ref::new("OpaqueRef:pif-01-eth1"));
        w.vlan = "20".into();
    }
    settle(&mut h);
    assert!(shows(&h, n.vlan_in_use));
    assert!(!last_enabled(&h, n.finish));
    wizard(&mut h).vlan = "5000".into();
    settle(&mut h);
    assert!(shows(
        &h,
        &fill(n.vlan_range, &[("min", "1"), ("max", "4094")])
    ));
    wizard(&mut h).vlan = "21".into();
    wizard(&mut h).auto_plug = true;
    settle(&mut h);
    assert!(last_enabled(&h, n.finish));
    press(&mut h, n.finish);
    wait_ok(&mut h, "Creating network 'Guests VLAN 20 (1)' on Lab Pool");
    assert!(h.state().network.wizard.is_none());
    let net = network_named(&server, "Guests VLAN 20 (1)").unwrap();
    assert_eq!(
        field(&server, "network", &net, "other_config")["automatic"],
        "true"
    );
    let pifs = field(&server, "network", &net, "PIFs");
    assert_eq!(pifs.as_array().unwrap().len(), 2);
    step_until(&mut h, "listed", |h| {
        h.query_by_label("Guests VLAN 20 (1)").is_some()
    });

    // Single-server private network.
    press(&mut h, n.add_network);
    press(&mut h, n.type_internal);
    press(&mut h, n.next);
    assert_eq!(wizard(&mut h).name, n.new_private_name);
    press(&mut h, n.next);
    h.get_by_label(n.internal_blurb);
    press(&mut h, n.finish);
    wait_ok(&mut h, "Creating network 'New Private Network' on Lab Pool");
    let private = network_named(&server, "New Private Network").unwrap();
    assert!(
        field(&server, "network", &private, "PIFs")
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        field(&server, "network", &private, "other_config")["automatic"],
        "false"
    );

    // A failure undoes the network and lands in the error list.
    server.fail_method(
        "pool.create_VLAN_from_PIF",
        Some(("INTERNAL_ERROR", &["vlan"])),
    );
    press(&mut h, n.add_network);
    press(&mut h, n.next);
    wizard(&mut h).name = "Doomed".into();
    press(&mut h, n.next);
    press(&mut h, n.finish);
    assert!(wait_op(&mut h, "Creating network 'Doomed' on Lab Pool").is_err());
    assert!(network_named(&server, "Doomed").is_none());
    step_until(&mut h, "error shown", |h| !h.state().errors.is_empty());
}

#[test]
fn network_properties_and_remove_network() {
    let runtime = runtime();
    let (server, mut h) = connect_with(&runtime, xapi_mock::fixture_records());
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    let c = conn_id(&h);
    select_pool(&mut h);
    tab(&mut h, t.tab_networking);

    // A NIC's network cannot be removed.
    h.state_mut()
        .network
        .select_network(c, Ref::new("OpaqueRef:net-eth0"));
    settle(&mut h);
    assert!(last_enabled(&h, n.network_properties));
    assert!(!last_enabled(&h, n.remove_network));

    // VLAN 20: its VM interface blocks the NIC and VLAN.
    h.state_mut()
        .network
        .select_network(c, Ref::new("OpaqueRef:net-vlan20"));
    settle(&mut h);
    press(&mut h, n.network_properties);
    h.get_by_label("'Guests VLAN 20' Properties");
    {
        let d = h.state_mut().network.properties().unwrap();
        d.name = "Guests".into();
        d.tags = "lab, vlan".into();
        d.tab = crate::net_ui::PropertiesTab::Settings;
    }
    settle(&mut h);
    assert!(shows(&h, n.cannot_change_nic_vms));
    close_with(&mut h, n.ok);
    wait_ok(&mut h, "Updating network 'Guests VLAN 20' on Lab Pool");
    assert_eq!(
        text(&server, "network", "OpaqueRef:net-vlan20", "name_label"),
        "Guests"
    );
    assert_eq!(
        field(&server, "network", "OpaqueRef:net-vlan20", "tags"),
        json!(["lab", "vlan"])
    );

    // A new private network becomes a VLAN on NIC 0 from its Properties.
    press(&mut h, n.add_network);
    press(&mut h, n.type_internal);
    press(&mut h, n.next);
    wizard(&mut h).name = "Spare".into();
    press(&mut h, n.next);
    press(&mut h, n.finish);
    wait_ok(&mut h, "Creating network 'Spare' on Lab Pool");
    let spare = network_named(&server, "Spare").unwrap();
    cached(&mut h, "spare cached", |m| {
        m.networks.contains_key(&Ref::new(&spare))
    });
    h.state_mut().network.select_network(c, Ref::new(&spare));
    settle(&mut h);
    press(&mut h, n.network_properties);
    {
        let d = h.state_mut().network.properties().unwrap();
        d.tab = crate::net_ui::PropertiesTab::Settings;
        d.nic = Some(crate::net_ui::NicChoice::Nic(Ref::new(
            "OpaqueRef:pif-01-eth0",
        )));
        d.vlan = "77".into();
    }
    settle(&mut h);
    assert!(shows(&h, n.disruption_warning));
    close_with(&mut h, n.ok);
    wait_ok(&mut h, "Updating network 'Spare' on Lab Pool");
    let pifs = field(&server, "network", &spare, "PIFs");
    assert_eq!(pifs.as_array().unwrap().len(), 2);
    for p in pifs.as_array().unwrap() {
        assert_eq!(
            field(&server, "pif", p.as_str().unwrap(), "VLAN"),
            json!(77)
        );
    }
    cached(&mut h, "VLAN 77 cached", |m| {
        m.pifs.values().any(|p| p.vlan == 77)
    });

    // Remove Network asks first.
    h.state_mut().network.select_network(c, Ref::new(&spare));
    settle(&mut h);
    assert!(last_enabled(&h, n.remove_network));
    press(&mut h, n.remove_network);
    h.get_by_label(n.network_delete_text);
    press(&mut h, n.yes);
    wait_ok(&mut h, "Removing network 'Spare' from Lab Pool");
    assert!(network_named(&server, "Spare").is_none());
    assert!(
        server
            .refs("pif")
            .iter()
            .all(|p| field(&server, "pif", p, "VLAN") != json!(77))
    );
}

#[test]
fn bonds_from_the_wizard_and_the_nics_tab() {
    let runtime = runtime();
    let (server, mut h) = connect_with(&runtime, bond_lab_records());
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    let c = conn_id(&h);
    select_pool(&mut h);
    tab(&mut h, t.tab_networking);

    // Bonded network: NIC 2 + NIC 3, LACP on the Open vSwitch backend.
    press(&mut h, n.add_network);
    // The cross-server network is offered here.
    assert!(!shows(&h, n.chin_needs_vswitch));
    press(&mut h, n.type_bonded);
    press(&mut h, n.next);
    h.get_by_label(n.page_bond_title);
    assert!(!last_enabled(&h, n.finish));
    {
        let m = first_conn(&h).unwrap().model.clone();
        let w = wizard(&mut h);
        assert_eq!(w.bond.rows.len(), 4);
        assert_eq!(w.bond.limit, 4);
        w.bond.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth2"));
        w.bond.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth3"));
    }
    settle(&mut h);
    press(&mut h, n.mode_lacp_tcpudp);
    assert!(shows(&h, n.lacp_warning));
    assert_eq!(wizard(&mut h).bond.choice, BondChoice::LacpTcpUdp);
    press(&mut h, n.finish);
    wait_ok(&mut h, "Creating Bond 2+3");
    assert_eq!(server.refs("bond").len(), 2);
    for b in server.refs("bond") {
        assert_eq!(text(&server, "bond", &b, "mode"), "lacp");
        assert_eq!(
            field(&server, "bond", &b, "properties")["hashing_algorithm"],
            "tcpudp_ports"
        );
    }
    cached(&mut h, "bonds cached", |m| m.bonds.len() == 2);

    // NICs tab: Delete Bond on its interface.
    select_host(&mut h, H1);
    tab(&mut h, t.tab_nics);
    step_until(&mut h, "bond row", |h| {
        h.query_by_label("Bond 2+3").is_some()
    });
    assert!(!last_enabled(&h, n.delete_bond_button), "nothing selected");
    let master = server
        .refs("bond")
        .into_iter()
        .map(|b| text(&server, "bond", &b, "master"))
        .find(|m| text(&server, "pif", m, "host") == H1)
        .unwrap();
    h.state_mut().network.select_nic(c, Ref::new(&master));
    settle(&mut h);
    press(&mut h, n.delete_bond_button);
    h.get_by_label(&fill(n.delete_bond_text, &[("bond", "Bond 2+3")]));
    press(&mut h, n.ok);
    wait_ok(&mut h, "Destroying Bond 2+3");
    assert!(server.refs("bond").is_empty());
    cached(&mut h, "bonds gone", |m| m.bonds.is_empty());

    // Create Bond over the management NIC: the original's warning first.
    press(&mut h, n.create_bond_button);
    {
        let m = first_conn(&h).unwrap().model.clone();
        let f = h.state_mut().network.bond_form().unwrap();
        f.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth0"));
        f.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth1"));
    }
    settle(&mut h);
    // Management and secondary interfaces together: refused.
    press(&mut h, n.create);
    h.get_by_label(n.bond_create_both);
    press(&mut h, n.ok);
    {
        let m = first_conn(&h).unwrap().model.clone();
        let f = h.state_mut().network.bond_form().unwrap();
        f.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth1"));
        f.toggle(&m, &Ref::new("OpaqueRef:pif-01-eth2"));
    }
    settle(&mut h);
    press(&mut h, n.create);
    h.get_by_label(&fill(
        n.bond_create_primary,
        &[("console", crate::branding::PRODUCT_NAME)],
    ));
    press(&mut h, n.bond_create_continue);
    wait_ok(&mut h, "Creating Bond 0+2");
    let master = server
        .refs("bond")
        .into_iter()
        .map(|b| text(&server, "bond", &b, "master"))
        .find(|m| text(&server, "pif", m, "host") == H1)
        .unwrap();
    assert_eq!(field(&server, "pif", &master, "management"), json!(true));
    cached(&mut h, "bond 0+2 cached", |m| m.bonds.len() == 2);

    // Deleting it warns about the management interface.
    h.state_mut().network.select_nic(c, Ref::new(&master));
    settle(&mut h);
    press(&mut h, n.delete_bond_button);
    h.get_by_label(n.bond_delete_continue);
    press(&mut h, n.bond_delete_continue);
    wait_ok(&mut h, "Destroying Bond 0+2");
    assert_eq!(
        field(&server, "pif", "OpaqueRef:pif-01-eth0", "management"),
        json!(true)
    );

    // Rescan finds a new NIC.
    server.add_spare_nic(H1, "eth4", "02:00:00:00:01:04");
    press(&mut h, n.rescan);
    wait_ok(&mut h, "Scanning NICs on server 'xcp-host-01'");
    step_until(&mut h, "new NIC", |h| h.query_by_label("NIC 4").is_some());
}

#[test]
fn configure_ip_addresses() {
    let runtime = runtime();
    let (server, mut h) = connect_with(&runtime, bond_lab_records());
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    select_host(&mut h, H1);
    tab(&mut h, t.tab_networking);
    press(&mut h, n.configure_ip);
    h.get_by_label("Configure IP Addresses - xcp-host-01");
    {
        let d = h.state_mut().network.ip_dialog().unwrap();
        assert_eq!(d.pages.len(), 2);
        assert_eq!(d.pages[1].purpose.as_deref(), Some("Storage"));
        // The secondary interface on the management network: in use.
        d.pages[1].network = Some(Ref::new("OpaqueRef:net-eth0"));
        d.selected = 1;
    }
    settle(&mut h);
    assert!(shows(
        &h,
        &fill(n.in_use_management, &[("network", "Network 0")])
    ));
    assert!(!last_enabled(&h, n.ok));
    {
        let m = first_conn(&h).unwrap().model.clone();
        let d = h.state_mut().network.ip_dialog().unwrap();
        d.pages[1].network = Some(Ref::new("OpaqueRef:net-eth1"));
        d.pages[1].ip = "198.51.100.30".into();
        // A new interface on eth2.
        d.add_page(&m, false);
        let page = d.pages.last_mut().unwrap();
        assert_eq!(page.purpose.as_deref(), Some("Storage 1"));
        page.network = Some(Ref::new("OpaqueRef:net-eth2"));
        page.dhcp = false;
        page.ip = "203.0.113.20".into();
        page.netmask = "255.255.255.0".into();
    }
    settle(&mut h);
    assert!(last_enabled(&h, n.ok));
    press(&mut h, n.ok);
    wait_ok(&mut h, "Reconfiguring server networking");
    assert_eq!(
        text(&server, "pif", "OpaqueRef:pif-01-eth1", "IP"),
        "198.51.100.30"
    );
    assert_eq!(
        text(&server, "pif", "OpaqueRef:pif-01-eth2", "IP"),
        "203.0.113.20"
    );
    assert_eq!(
        field(&server, "pif", "OpaqueRef:pif-01-eth2", "other_config")["management_purpose"],
        "Storage 1"
    );
    cached(&mut h, "addresses cached", |m| {
        m.pifs
            .get(&Ref::new("OpaqueRef:pif-01-eth2"))
            .is_some_and(|p| p.ip == "203.0.113.20")
    });

    // The management interface's address: the original's warning; Cancel
    // keeps the dialog, OK runs it.
    press(&mut h, n.configure_ip);
    h.state_mut().network.ip_dialog().unwrap().pages[0].ip = "192.0.2.12".into();
    settle(&mut h);
    press(&mut h, n.ok);
    let warning = fill(
        n.warn_management_host,
        &[("console", crate::branding::PRODUCT_NAME)],
    );
    h.get_by_label(&warning);
    press(&mut h, n.cancel);
    assert!(h.state_mut().network.ip_dialog().is_some());
    press(&mut h, n.ok);
    h.get_by_label(&warning);
    press(&mut h, n.ok);
    wait_ok(&mut h, "Reconfiguring server networking");
    assert_eq!(
        text(&server, "pif", "OpaqueRef:pif-01-eth0", "IP"),
        "192.0.2.12"
    );
    cached(&mut h, "management cached", |m| {
        m.pifs
            .get(&Ref::new("OpaqueRef:pif-01-eth0"))
            .is_some_and(|p| p.ip == "192.0.2.12")
    });

    // Remove IP Address on the eth2 page brings it down.
    press(&mut h, n.configure_ip);
    {
        let d = h.state_mut().network.ip_dialog().unwrap();
        d.selected = d
            .pages
            .iter()
            .position(|p| p.purpose.as_deref() == Some("Storage 1"))
            .unwrap();
    }
    settle(&mut h);
    press(&mut h, n.remove_ip);
    press(&mut h, n.ok);
    wait_ok(&mut h, "Reconfiguring server networking");
    assert_eq!(
        text(
            &server,
            "pif",
            "OpaqueRef:pif-01-eth2",
            "ip_configuration_mode"
        ),
        "None"
    );

    // HA on: Configure is disabled.
    server.set_field("pool", POOL, "ha_enabled", json!(true));
    cached(&mut h, "HA cached", |m| {
        m.pool().is_some_and(|(_, p)| p.ha_enabled)
    });
    assert!(!last_enabled(&h, n.configure_ip));
}

/// A VM operator cannot add an interface: the role elevation dialog asks
/// for other credentials.
#[test]
fn interfaces_go_through_role_elevation() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    let mut h = app_harness(&runtime);
    h.step();
    let st = strings();
    h.get_by_label(st.connect_server)
        .type_text(&server.address());
    h.step();
    h.get_by_label(st.connect_user).focus();
    h.step();
    for _ in 0..4 {
        h.key_press(egui::Key::Backspace);
    }
    h.get_by_label(st.connect_user)
        .type_text("vm-operator-user");
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::PasswordInput, st.connect_password)
        .focus();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::PasswordInput, st.connect_password)
        .type_text("vm-operator-password");
    h.step();
    h.get_by_label(st.connect_button).click();
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    let n = net_strings();
    let t = crate::tab_strings::tab_strings();
    select_vm(&mut h, DB);
    tab(&mut h, t.tab_networking);
    press(&mut h, n.add_interface);
    h.get_by_label(n.add_vif_title);
    press(&mut h, n.add);
    step_until(&mut h, "elevation", |h| h.state().ops.elevation.is_some());
    assert!(vif_of(&server, DB, "1").is_none());
}
