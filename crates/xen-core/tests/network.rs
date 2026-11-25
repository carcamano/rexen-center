//! F9: networking against the mock (backend only, no UI): every New
//! Network type (external VLAN, single-server and cross-server private,
//! bonded), removing networks and bonds, the network properties, NIC
//! rescans, IP configuration of management and secondary interfaces on a
//! host and across a pool, and a VM's interfaces; the XAPI calls made, what
//! is left on the server, failures and rollbacks.

mod common;

use std::time::Duration;

use common::{first, memory_store, spawn};
use serde_json::Value;
use xapi::{BondMode, IpConfigurationMode, Ref};
use xapi_mock::MockServer;
use xapi_mock::networking::bond_lab_records;
use xen_core::network::{
    self, HashingAlgorithm, IpPage, NetworkChanges, NewBondSpec, NewNetworkKind, NewNetworkSpec,
    PifChange, VifSpec,
};
use xen_core::operations::{NetworkKind, OperationNotice};
use xen_core::{
    ActionRequest, BackendOptions, Command, ConnectionId, ConnectionModel, Operation,
    OperationKind, OperationStatus,
};

const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";
const NET_ETH0: &str = "OpaqueRef:net-eth0";
const NET_ETH1: &str = "OpaqueRef:net-eth1";
const NET_VLAN20: &str = "OpaqueRef:net-vlan20";
const PIF1_ETH0: &str = "OpaqueRef:pif-01-eth0";
const PIF1_ETH1: &str = "OpaqueRef:pif-01-eth1";
const PIF2_ETH0: &str = "OpaqueRef:pif-02-eth0";
const PIF2_ETH1: &str = "OpaqueRef:pif-02-eth1";
const PIF1_ETH2: &str = "OpaqueRef:pif-01-eth2";
const PIF1_ETH3: &str = "OpaqueRef:pif-01-eth3";
const PIF2_ETH2: &str = "OpaqueRef:pif-02-eth2";
const VM_WEB: &str = "OpaqueRef:vm-web";
const VM_DB: &str = "OpaqueRef:vm-db";

fn r<T>(s: &str) -> Ref<T> {
    Ref::new(s)
}

fn harness() -> common::Harness {
    spawn(BackendOptions {
        member_recovery_timeout: Duration::from_millis(200),
        member_recovery_poll: Duration::from_millis(20),
        ..common::fast_options(memory_store().0)
    })
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

/// The client's model once `pred` holds.
async fn model_when(
    h: &common::Harness,
    what: &str,
    pred: impl Fn(&ConnectionModel) -> bool,
) -> std::sync::Arc<ConnectionModel> {
    let state = h.wait_conn(what, |c| pred(&c.model)).await;
    first(&state).model.clone()
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

fn text(server: &MockServer, class: &str, r: &str, f: &str) -> String {
    field(server, class, r, f)
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn refs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The network named `name` on the server.
fn network_named(server: &MockServer, name: &str) -> Option<String> {
    server
        .refs("network")
        .into_iter()
        .find(|n| text(server, "network", n, "name_label") == name)
}

fn purpose(server: &MockServer, pif: &str) -> Option<String> {
    field(server, "pif", pif, "other_config")
        .get("management_purpose")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn spec(name: &str, kind: NewNetworkKind) -> ActionRequest {
    ActionRequest::NewNetwork(Box::new(NewNetworkSpec {
        name: name.into(),
        description: "made by a test".into(),
        auto_plug: true,
        mtu: None,
        kind,
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn external_network_is_a_vlan_on_every_host() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    let op = run(
        &h,
        conn,
        spec(
            "Guests VLAN 30",
            NewNetworkKind::External {
                pif: r(PIF1_ETH1),
                vlan: 30,
            },
        ),
    )
    .await;
    ok(&op);
    assert!(matches!(
        &op.kind,
        OperationKind::Network(NetworkKind::CreateNetwork { name, pool }) if name == "Guests VLAN 30" && pool == "Lab Pool"
    ));
    let net = network_named(&server, "Guests VLAN 30").expect("network made");
    assert_eq!(
        field(&server, "network", &net, "other_config")["automatic"],
        "true"
    );
    assert_eq!(
        text(&server, "network", &net, "name_description"),
        "made by a test"
    );
    // One VLAN interface on eth1 of each host.
    let pifs = refs(&field(&server, "network", &net, "PIFs"));
    assert_eq!(pifs.len(), 2);
    let mut hosts: Vec<String> = pifs
        .iter()
        .map(|p| text(&server, "pif", p, "host"))
        .collect();
    hosts.sort();
    assert_eq!(hosts, [H1, H2]);
    for p in &pifs {
        assert_eq!(field(&server, "pif", p, "VLAN"), 30);
        assert_eq!(text(&server, "pif", p, "device"), "eth1");
        let vlan = text(&server, "pif", p, "VLAN_master_of");
        assert_eq!(field(&server, "vlan", &vlan, "tag"), 30);
    }
    assert_eq!(server.call_count("Async.network.create"), 1);
    assert_eq!(server.call_count("Async.pool.create_VLAN_from_PIF"), 1);
    // The model shows it and its tag is now taken.
    let m = model_when(&h, "VLAN cached", |m| {
        m.networks
            .values()
            .any(|n| n.name_label == "Guests VLAN 30" && n.pifs.len() == 2)
    })
    .await;
    assert!(network::vlans_in_use(&m, "eth1").contains(&30));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_made_vlan_network_is_undone() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let networks = server.refs("network").len();

    // Tag 20 is already used on eth1 (host 1): the VLAN cannot be made and
    // the network made for it is destroyed again.
    let op = run(
        &h,
        conn,
        spec(
            "Clash",
            NewNetworkKind::External {
                pif: r(PIF1_ETH1),
                vlan: 20,
            },
        ),
    )
    .await;
    failed_with(&op, "PIF_VLAN_EXISTS");
    assert!(network_named(&server, "Clash").is_none());
    assert_eq!(server.refs("network").len(), networks);
    assert_eq!(server.call_count("network.destroy"), 1);

    // The server refuses outright.
    server.fail_method(
        "pool.create_VLAN_from_PIF",
        Some(("INTERNAL_ERROR", &["boom"])),
    );
    let op = run(
        &h,
        conn,
        spec(
            "Refused",
            NewNetworkKind::External {
                pif: r(PIF1_ETH0),
                vlan: 40,
            },
        ),
    )
    .await;
    failed_with(&op, "INTERNAL_ERROR");
    assert!(network_named(&server, "Refused").is_none());
    assert_eq!(server.refs("network").len(), networks);
    server.fail_method("pool.create_VLAN_from_PIF", None);

    // A bad tag.
    let op = run(
        &h,
        conn,
        spec(
            "Bad tag",
            NewNetworkKind::External {
                pif: r(PIF1_ETH0),
                vlan: 4095,
            },
        ),
    )
    .await;
    failed_with(&op, "VLAN_TAG_INVALID");
    assert_eq!(server.refs("network").len(), networks);
    assert!(server.refs("vlan").len() == 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn private_networks_single_and_cross_server() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // Single-server: no PIFs.
    let op = run(&h, conn, spec("Lab private", NewNetworkKind::Internal)).await;
    ok(&op);
    let net = network_named(&server, "Lab private").unwrap();
    assert!(refs(&field(&server, "network", &net, "PIFs")).is_empty());
    assert_eq!(server.call_count("Async.pool.create_VLAN_from_PIF"), 0);

    // Cross-server: a tunnel on each host's management interface.
    let op = run(
        &h,
        conn,
        ActionRequest::NewNetwork(Box::new(NewNetworkSpec {
            name: "Pool private".into(),
            description: String::new(),
            auto_plug: false,
            mtu: Some(1500),
            kind: NewNetworkKind::CrossServer {
                interface: r(NET_ETH0),
            },
        })),
    )
    .await;
    ok(&op);
    let chin = network_named(&server, "Pool private").unwrap();
    assert_eq!(
        field(&server, "network", &chin, "other_config")["automatic"],
        "false"
    );
    let pifs = refs(&field(&server, "network", &chin, "PIFs"));
    assert_eq!(pifs.len(), 2);
    let tunnels = server.refs("tunnel");
    assert_eq!(tunnels.len(), 2);
    let mut transports: Vec<String> = tunnels
        .iter()
        .map(|t| text(&server, "tunnel", t, "transport_PIF"))
        .collect();
    transports.sort();
    assert_eq!(transports, [PIF1_ETH0, PIF2_ETH0]);
    let m = model_when(&h, "tunnels cached", |m| {
        m.networks
            .values()
            .any(|n| n.name_label == "Pool private" && n.pifs.len() == 2)
    })
    .await;
    let chin_record = m.networks.get(&r(&chin)).unwrap();
    assert!(network::network_is_chin(&m, chin_record));
    assert!(!network::network_can_use_jumbo(&m, chin_record));

    // Removing it destroys the tunnels, then the network.
    let op = run(&h, conn, ActionRequest::RemoveNetwork { network: r(&chin) }).await;
    ok(&op);
    assert!(server.refs("tunnel").is_empty());
    assert!(network_named(&server, "Pool private").is_none());

    // A tunnel that cannot be made on host 2: the one on host 1 and the
    // network are undone.
    server.fail_on_host("tunnel.create", H2, true);
    let op = run(
        &h,
        conn,
        ActionRequest::NewNetwork(Box::new(NewNetworkSpec {
            name: "Half private".into(),
            description: String::new(),
            auto_plug: true,
            mtu: None,
            kind: NewNetworkKind::CrossServer {
                interface: r(NET_ETH0),
            },
        })),
    )
    .await;
    failed_with(&op, "OPENVSWITCH_NOT_ACTIVE");
    assert!(server.refs("tunnel").is_empty());
    assert!(network_named(&server, "Half private").is_none());
    assert!(
        server
            .refs("pif")
            .iter()
            .all(|p| refs(&field(&server, "pif", p, "tunnel_access_PIF_of")).is_empty())
    );
}

fn bond_spec(
    name: &str,
    pifs: &[&str],
    mode: BondMode,
    hashing: Option<HashingAlgorithm>,
) -> ActionRequest {
    ActionRequest::NewBond(Box::new(NewBondSpec {
        name: name.into(),
        pifs: pifs.iter().map(|p| r(p)).collect(),
        mode,
        hashing,
        mtu: 1500,
        auto_plug: true,
    }))
}

/// The bond whose interface is on `host`, with its interface.
fn bond_on(server: &MockServer, host: &str) -> Option<(String, String)> {
    server.refs("bond").into_iter().find_map(|b| {
        let master = text(server, "bond", &b, "master");
        (text(server, "pif", &master, "host") == host).then_some((b, master))
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn bonds_are_made_on_every_host_and_deleted() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // LACP over eth2 + eth3, chosen on the coordinator.
    let op = run(
        &h,
        conn,
        bond_spec(
            "Bond 2+3",
            &[PIF1_ETH2, PIF1_ETH3],
            BondMode::Lacp,
            Some(HashingAlgorithm::TcpUdpPorts),
        ),
    )
    .await;
    ok(&op);
    let net = network_named(&server, "Bond 2+3").unwrap();
    // The creation mark was removed.
    assert!(
        field(&server, "network", &net, "other_config")
            .get(network::CREATE_IN_PROGRESS)
            .is_none()
    );
    assert_eq!(server.refs("bond").len(), 2);
    for host in [H1, H2] {
        let (bond, master) = bond_on(&server, host).expect("bond on each host");
        assert_eq!(text(&server, "bond", &bond, "mode"), "lacp");
        assert_eq!(
            field(&server, "bond", &bond, "properties")["hashing_algorithm"],
            "tcpudp_ports"
        );
        assert_eq!(text(&server, "pif", &master, "network"), net);
        let mut devices: Vec<String> = refs(&field(&server, "bond", &bond, "slaves"))
            .iter()
            .map(|p| text(&server, "pif", p, "device"))
            .collect();
        devices.sort();
        assert_eq!(devices, ["eth2", "eth3"]);
    }
    // Made on host 2 first, the coordinator last.
    let creates: Vec<usize> = server
        .calls()
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == "Async.Bond.create")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(creates.len(), 2);

    // The bond's properties: mode and hashing.
    let m = model_when(&h, "bonds cached", |m| m.bonds.len() == 2).await;
    let op = run(
        &h,
        conn,
        ActionRequest::SetNetworkProperties {
            network: r(&net),
            changes: NetworkChanges {
                bond_mode: Some(BondMode::ActiveBackup),
                ..Default::default()
            },
        },
    )
    .await;
    ok(&op);
    for b in server.refs("bond") {
        assert_eq!(text(&server, "bond", &b, "mode"), "active-backup");
    }
    let op = run(
        &h,
        conn,
        ActionRequest::SetNetworkProperties {
            network: r(&net),
            changes: NetworkChanges {
                bond_mode: Some(BondMode::Lacp),
                hashing: Some(HashingAlgorithm::SrcMac),
                ..Default::default()
            },
        },
    )
    .await;
    ok(&op);
    for b in server.refs("bond") {
        assert_eq!(
            field(&server, "bond", &b, "properties")["hashing_algorithm"],
            "src_mac"
        );
    }

    // Delete Bond: every host's bond and the network.
    let (bond1, _) = bond_on(&server, H1).unwrap();
    assert!(m.bonds.contains_key(&r(&bond1)));
    let op = run(&h, conn, ActionRequest::DestroyBond { bond: r(&bond1) }).await;
    ok(&op);
    assert!(
        matches!(&op.kind, OperationKind::Network(NetworkKind::DestroyBond { name }) if name == "Bond 2+3")
    );
    assert!(server.refs("bond").is_empty());
    assert!(network_named(&server, "Bond 2+3").is_none());
    for p in [PIF1_ETH2, PIF1_ETH3, PIF2_ETH2] {
        assert_eq!(text(&server, "pif", p, "bond_slave_of"), "OpaqueRef:NULL");
        assert_eq!(field(&server, "pif", p, "currently_attached"), true);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bonding_management_and_secondary_interfaces() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // eth0 carries the management interface: it moves to the bond on each
    // host (XAPI moves it), with its address.
    let op = run(
        &h,
        conn,
        bond_spec(
            "Bond 0+2",
            &[PIF1_ETH0, PIF1_ETH2],
            BondMode::BalanceSlb,
            None,
        ),
    )
    .await;
    ok(&op);
    let (_, master1) = bond_on(&server, H1).unwrap();
    let (_, master2) = bond_on(&server, H2).unwrap();
    assert_eq!(field(&server, "pif", &master1, "management"), true);
    assert_eq!(text(&server, "pif", &master1, "IP"), "192.0.2.11");
    assert_eq!(field(&server, "pif", &master2, "management"), true);
    assert_eq!(
        text(&server, "pif", &master2, "ip_configuration_mode"),
        "DHCP"
    );
    assert_eq!(field(&server, "pif", PIF1_ETH0, "management"), false);

    // Deleting it brings the management interface back to eth0.
    let (bond1, _) = bond_on(&server, H1).unwrap();
    model_when(&h, "bonds cached", |m| m.bonds.len() == 2).await;
    let op = run(&h, conn, ActionRequest::DestroyBond { bond: r(&bond1) }).await;
    ok(&op);
    assert_eq!(field(&server, "pif", PIF1_ETH0, "management"), true);
    assert_eq!(text(&server, "pif", PIF1_ETH0, "IP"), "192.0.2.11");
    assert_eq!(field(&server, "pif", PIF2_ETH0, "management"), true);

    // eth1 holds the "Storage" secondary interface on host 1: its name
    // moves to the bond, and back to eth1 when the bond goes.
    model_when(&h, "bond gone", |m| m.bonds.is_empty()).await;
    let op = run(
        &h,
        conn,
        bond_spec(
            "Bond 1+3",
            &[PIF1_ETH1, PIF1_ETH3],
            BondMode::ActiveBackup,
            None,
        ),
    )
    .await;
    ok(&op);
    let (bond1, master1) = bond_on(&server, H1).unwrap();
    assert_eq!(purpose(&server, &master1).as_deref(), Some("Storage"));
    assert_eq!(purpose(&server, PIF1_ETH1), None);
    assert_eq!(text(&server, "pif", &master1, "IP"), "198.51.100.11");
    // The VLAN 20 on eth1 now sits on the bond.
    assert_eq!(
        text(&server, "vlan", "OpaqueRef:vlan-20", "tagged_PIF"),
        master1
    );
    model_when(&h, "bonds cached", |m| m.bonds.len() == 2).await;
    let op = run(&h, conn, ActionRequest::DestroyBond { bond: r(&bond1) }).await;
    ok(&op);
    assert_eq!(purpose(&server, PIF1_ETH1).as_deref(), Some("Storage"));
    assert_eq!(
        text(&server, "vlan", "OpaqueRef:vlan-20", "tagged_PIF"),
        PIF1_ETH1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_made_bond_is_undone() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let networks = server.refs("network").len();

    // The coordinator's bond (made last) fails: host 2's bond is destroyed
    // again, then the network; the secondary name is back on eth1.
    server.fail_on_host("Bond.create", H1, true);
    let op = run(
        &h,
        conn,
        bond_spec(
            "Bond 1+3",
            &[PIF1_ETH1, PIF1_ETH3],
            BondMode::BalanceSlb,
            None,
        ),
    )
    .await;
    failed_with(&op, "PIF_CONFIGURATION_ERROR");
    assert_eq!(server.call_count("Async.Bond.create"), 2);
    assert_eq!(server.call_count("Bond.destroy"), 1);
    assert!(server.refs("bond").is_empty());
    assert!(network_named(&server, "Bond 1+3").is_none());
    assert_eq!(server.refs("network").len(), networks);
    assert_eq!(purpose(&server, PIF1_ETH1).as_deref(), Some("Storage"));
    assert_eq!(
        text(&server, "pif", PIF2_ETH1, "bond_slave_of"),
        "OpaqueRef:NULL"
    );
    server.fail_on_host("Bond.create", H1, false);

    // Members that cannot be bonded: refused by the server, nothing left.
    let op = run(
        &h,
        conn,
        bond_spec(
            "Bond 0+1",
            &[PIF1_ETH0, PIF1_ETH1],
            BondMode::BalanceSlb,
            None,
        ),
    )
    .await;
    failed_with(&op, "PIF_BOND_MORE_THAN_ONE_IP");
    assert!(server.refs("bond").is_empty());
    assert_eq!(server.refs("network").len(), networks);
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_networks_and_their_failures() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // VLAN 20 still has a VM's interface: the server refuses, the VLAN
    // was destroyed first (as the original does).
    let op = run(
        &h,
        conn,
        ActionRequest::RemoveNetwork {
            network: r(NET_VLAN20),
        },
    )
    .await;
    failed_with(&op, "NETWORK_CONTAINS_VIF");

    // A new VLAN network without interfaces goes away with its VLANs.
    let op = run(
        &h,
        conn,
        spec(
            "Temp",
            NewNetworkKind::External {
                pif: r(PIF1_ETH0),
                vlan: 99,
            },
        ),
    )
    .await;
    ok(&op);
    let net = network_named(&server, "Temp").unwrap();
    let vlans = server.refs("vlan").len();
    model_when(&h, "temp cached", |m| {
        m.networks
            .values()
            .any(|n| n.name_label == "Temp" && n.pifs.len() == 2)
    })
    .await;
    let op = run(&h, conn, ActionRequest::RemoveNetwork { network: r(&net) }).await;
    ok(&op);
    assert!(network_named(&server, "Temp").is_none());
    assert_eq!(server.refs("vlan").len(), vlans - 2);
    assert!(
        server
            .refs("pif")
            .iter()
            .all(|p| field(&server, "pif", p, "VLAN") != 99)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn network_properties_and_pif_changes() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // Name, description, tags, automatic, MTU of a private network.
    ok(&run(&h, conn, spec("Props", NewNetworkKind::Internal)).await);
    let net = network_named(&server, "Props").unwrap();
    model_when(&h, "cached", |m| {
        m.networks.values().any(|n| n.name_label == "Props")
    })
    .await;
    let op = run(
        &h,
        conn,
        ActionRequest::SetNetworkProperties {
            network: r(&net),
            changes: NetworkChanges {
                name: Some("Props renamed".into()),
                description: Some("new words".into()),
                tags: Some(vec!["lab".into()]),
                auto_plug: Some(false),
                mtu: Some(9000),
                ..Default::default()
            },
        },
    )
    .await;
    ok(&op);
    assert_eq!(
        text(&server, "network", &net, "name_label"),
        "Props renamed"
    );
    assert_eq!(
        text(&server, "network", &net, "name_description"),
        "new words"
    );
    assert_eq!(field(&server, "network", &net, "tags")[0], "lab");
    assert_eq!(
        field(&server, "network", &net, "other_config")["automatic"],
        "false"
    );
    assert_eq!(field(&server, "network", &net, "MTU"), 9000);
    // A private network has no PIFs to replug.
    assert_eq!(server.call_count("PIF.unplug"), 0);

    // <internal> → a VLAN on eth0, then another tag, then <internal>.
    let change = |pifs| ActionRequest::SetNetworkProperties {
        network: r(&net),
        changes: NetworkChanges {
            pifs: Some(pifs),
            ..Default::default()
        },
    };
    ok(&run(
        &h,
        conn,
        change(PifChange::External {
            pif: r(PIF1_ETH0),
            vlan: 50,
        }),
    )
    .await);
    let pifs = refs(&field(&server, "network", &net, "PIFs"));
    assert_eq!(pifs.len(), 2);
    assert!(pifs.iter().all(|p| field(&server, "pif", p, "VLAN") == 50));
    model_when(&h, "vlan 50 cached", |m| {
        m.pifs.values().any(|p| p.vlan == 50)
    })
    .await;
    // A tag already in use is refused before anything is touched.
    let op = run(
        &h,
        conn,
        change(PifChange::External {
            pif: r(PIF1_ETH1),
            vlan: 20,
        }),
    )
    .await;
    failed_with(&op, network::errors::VLAN_EXISTS);
    assert_eq!(refs(&field(&server, "network", &net, "PIFs")), pifs);
    ok(&run(
        &h,
        conn,
        change(PifChange::External {
            pif: r(PIF1_ETH0),
            vlan: 51,
        }),
    )
    .await);
    let now = refs(&field(&server, "network", &net, "PIFs"));
    assert!(now.iter().all(|p| field(&server, "pif", p, "VLAN") == 51));
    assert!(
        server
            .refs("pif")
            .iter()
            .all(|p| field(&server, "pif", p, "VLAN") != 50)
    );
    model_when(&h, "vlan 51 cached", |m| {
        m.pifs.values().any(|p| p.vlan == 51)
    })
    .await;
    ok(&run(&h, conn, change(PifChange::Internal)).await);
    assert!(refs(&field(&server, "network", &net, "PIFs")).is_empty());

    // A new MTU alone replugs a network with PIFs and running VMs: web-01's
    // interface on VLAN 20 and the VLAN's PIF are unplugged and plugged.
    let op = run(
        &h,
        conn,
        ActionRequest::SetNetworkProperties {
            network: r(NET_VLAN20),
            changes: NetworkChanges {
                mtu: Some(1400),
                ..Default::default()
            },
        },
    )
    .await;
    ok(&op);
    assert_eq!(server.call_count("VIF.unplug"), 1);
    assert_eq!(server.call_count("VIF.plug"), 1);
    assert_eq!(server.call_count("PIF.unplug"), 1);
    assert_eq!(server.call_count("PIF.plug"), 1);
    assert_eq!(
        field(&server, "vif", "OpaqueRef:vif-web-1", "currently_attached"),
        true
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rescan_finds_new_nics() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    server.add_spare_nic(H2, "eth4", "02:00:00:00:02:04");
    let op = run(&h, conn, ActionRequest::RescanPifs { host: r(H2) }).await;
    ok(&op);
    assert!(
        matches!(&op.kind, OperationKind::Network(NetworkKind::RescanPifs { host }) if host == "xcp-host-02")
    );
    let found = server
        .refs("pif")
        .into_iter()
        .find(|p| text(&server, "pif", p, "device") == "eth4")
        .expect("new NIC");
    assert_eq!(text(&server, "pif", &found, "host"), H2);
    let net = text(&server, "pif", &found, "network");
    assert_eq!(
        text(&server, "network", &net, "name_label"),
        "Pool-wide network associated with eth4"
    );
}

/// The dialog's pages for `host` as found in the model.
fn pages(m: &ConnectionModel, host: &str) -> Vec<IpPage> {
    let host = r(host);
    let all = network::known_pifs(m, &host, true);
    let management = all.iter().find(|(_, p)| p.management).unwrap();
    let page = |r: &Ref<xapi::Pif>, p: &xapi::PifRecord, purpose: Option<String>| {
        let dns: Vec<&str> = p.dns.split(',').collect();
        IpPage {
            original: Some(r.clone()),
            network: p.network.clone(),
            dhcp: p.ip_configuration_mode == IpConfigurationMode::Dhcp,
            ip: p.ip.clone(),
            netmask: p.netmask.clone(),
            gateway: p.gateway.clone(),
            dns: [
                dns.first().copied().unwrap_or_default().into(),
                dns.get(1).copied().unwrap_or_default().into(),
                String::new(),
            ],
            purpose,
        }
    };
    let mut out = vec![page(management.0, management.1, None)];
    for (r, p) in &all {
        if !p.management && xen_core::objects::pif_is_secondary_management(m, p, true) {
            out.push(page(
                r,
                p,
                network::management_purpose(p).map(str::to_owned),
            ));
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn ip_configuration_of_one_host() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let m = model_when(&h, "loaded", |m| !m.pifs.is_empty()).await;

    // The Storage interface moves to a new address.
    let mut p = pages(&m, H1);
    p[1].ip = "198.51.100.21".into();
    let plan = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap();
    assert!(!plan.warn_management);
    let op = run(
        &h,
        conn,
        ActionRequest::ChangeNetworking(Box::new(plan.change.unwrap())),
    )
    .await;
    ok(&op);
    assert_eq!(text(&server, "pif", PIF1_ETH1, "IP"), "198.51.100.21");
    assert_eq!(field(&server, "pif", PIF1_ETH1, "disallow_unplug"), true);
    assert_eq!(purpose(&server, PIF1_ETH1).as_deref(), Some("Storage"));
    // Host 2's eth1 is untouched (one host only).
    assert_eq!(
        text(&server, "pif", PIF2_ETH1, "ip_configuration_mode"),
        "None"
    );

    // A new secondary interface on eth2, then removed again.
    let m = model_when(&h, "address cached", |m| {
        m.pifs
            .get(&r(PIF1_ETH1))
            .is_some_and(|p| p.ip == "198.51.100.21")
    })
    .await;
    let mut p = pages(&m, H1);
    p.push(IpPage {
        original: None,
        network: r("OpaqueRef:net-eth2"),
        dhcp: false,
        ip: "203.0.113.5".into(),
        netmask: "255.255.255.0".into(),
        gateway: String::new(),
        dns: Default::default(),
        purpose: Some("iSCSI".into()),
    });
    let change = network::plan_ip_changes(&m, &r(H1), false, &p, false)
        .unwrap()
        .change
        .unwrap();
    ok(&run(&h, conn, ActionRequest::ChangeNetworking(Box::new(change))).await);
    assert_eq!(text(&server, "pif", PIF1_ETH2, "IP"), "203.0.113.5");
    assert_eq!(purpose(&server, PIF1_ETH2).as_deref(), Some("iSCSI"));
    let m = model_when(&h, "iSCSI cached", |m| {
        m.pifs
            .get(&r(PIF1_ETH2))
            .is_some_and(|p| p.ip == "203.0.113.5")
    })
    .await;
    let p: Vec<IpPage> = pages(&m, H1)
        .into_iter()
        .filter(|p| p.purpose.as_deref() != Some("iSCSI"))
        .collect();
    let change = network::plan_ip_changes(&m, &r(H1), false, &p, false)
        .unwrap()
        .change
        .unwrap();
    assert_eq!(change.down, [r(PIF1_ETH2)]);
    ok(&run(&h, conn, ActionRequest::ChangeNetworking(Box::new(change))).await);
    assert_eq!(
        text(&server, "pif", PIF1_ETH2, "ip_configuration_mode"),
        "None"
    );
    assert_eq!(purpose(&server, PIF1_ETH2), None);
    assert_eq!(field(&server, "pif", PIF1_ETH2, "disallow_unplug"), false);

    // Only renamed.
    let m = model_when(&h, "removed cached", |m| {
        m.pifs.get(&r(PIF1_ETH2)).is_some_and(|p| p.ip.is_empty())
    })
    .await;
    let mut p = pages(&m, H1);
    p[1].purpose = Some("NFS".into());
    let plan = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap();
    assert!(plan.change.is_none());
    ok(&run(
        &h,
        conn,
        ActionRequest::RenameInterfaces {
            pool: false,
            pifs: plan.renames,
        },
    )
    .await);
    assert_eq!(purpose(&server, PIF1_ETH1).as_deref(), Some("NFS"));

    // The management interface's own address: warned, reconfigured.
    let m = model_when(&h, "renamed cached", |m| {
        m.pifs
            .get(&r(PIF1_ETH1))
            .is_some_and(|p| network::management_purpose(p) == Some("NFS"))
    })
    .await;
    let mut p = pages(&m, H1);
    p[0].ip = "192.0.2.111".into();
    let plan = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap();
    assert!(plan.warn_management);
    ok(&run(
        &h,
        conn,
        ActionRequest::ChangeNetworking(Box::new(plan.change.unwrap())),
    )
    .await);
    assert_eq!(text(&server, "pif", PIF1_ETH0, "IP"), "192.0.2.111");
    assert_eq!(field(&server, "pif", PIF1_ETH0, "management"), true);

    // The management interface moves to eth3 (static): eth3 is configured,
    // made the management interface, and eth0 loses its address.
    let m = model_when(&h, "address cached", |m| {
        m.pifs
            .get(&r(PIF1_ETH0))
            .is_some_and(|p| p.ip == "192.0.2.111")
    })
    .await;
    let mut p = pages(&m, H1);
    p[0].network = r("OpaqueRef:net-eth3");
    p[0].ip = "192.0.2.150".into();
    let plan = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap();
    assert!(plan.warn_management);
    let change = plan.change.unwrap();
    assert_eq!(change.down_management, Some(r(PIF1_ETH0)));
    ok(&run(&h, conn, ActionRequest::ChangeNetworking(Box::new(change))).await);
    assert_eq!(field(&server, "pif", PIF1_ETH3, "management"), true);
    assert_eq!(text(&server, "pif", PIF1_ETH3, "IP"), "192.0.2.150");
    assert_eq!(field(&server, "pif", PIF1_ETH0, "management"), false);
    assert_eq!(
        text(&server, "pif", PIF1_ETH0, "ip_configuration_mode"),
        "None"
    );
    assert_eq!(server.call_count("Async.host.management_reconfigure"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn ip_configuration_across_the_pool() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let m = model_when(&h, "loaded", |m| !m.pifs.is_empty()).await;

    // A secondary interface on eth2 of every host, numbered from .10: host
    // 2 is configured first (the coordinator's after), each with the next
    // address.
    let mut p = pages(&m, H1);
    p.push(IpPage {
        original: None,
        network: r("OpaqueRef:net-eth2"),
        dhcp: false,
        ip: "203.0.113.10".into(),
        netmask: "255.255.255.0".into(),
        gateway: String::new(),
        dns: Default::default(),
        purpose: Some("Storage 2".into()),
    });
    let change = network::plan_ip_changes(&m, &r(H1), true, &p, false)
        .unwrap()
        .change
        .unwrap();
    ok(&run(&h, conn, ActionRequest::ChangeNetworking(Box::new(change))).await);
    assert_eq!(text(&server, "pif", PIF1_ETH2, "IP"), "203.0.113.10");
    assert_eq!(text(&server, "pif", PIF2_ETH2, "IP"), "203.0.113.11");
    assert_eq!(purpose(&server, PIF2_ETH2).as_deref(), Some("Storage 2"));
    let calls = server.calls();
    let order: Vec<&String> = calls
        .iter()
        .filter(|c| *c == "Async.PIF.reconfigure_ip")
        .collect();
    assert_eq!(order.len(), 2);

    // The management interface of the pool moves to eth3 (DHCP): one
    // `pool.management_reconfigure`, then eth0 loses its address
    // everywhere.
    let m = model_when(&h, "storage 2 cached", |m| {
        m.pifs
            .get(&r(PIF2_ETH2))
            .is_some_and(|p| p.ip == "203.0.113.11")
    })
    .await;
    let mut p = pages(&m, H1);
    p[0].network = r("OpaqueRef:net-eth3");
    p[0].dhcp = true;
    let plan = network::plan_ip_changes(&m, &r(H1), true, &p, false).unwrap();
    assert!(plan.warn_management);
    ok(&run(
        &h,
        conn,
        ActionRequest::ChangeNetworking(Box::new(plan.change.unwrap())),
    )
    .await);
    assert_eq!(server.call_count("Async.pool.management_reconfigure"), 1);
    for (eth3, eth0) in [
        ("OpaqueRef:pif-01-eth3", PIF1_ETH0),
        ("OpaqueRef:pif-02-eth3", PIF2_ETH0),
    ] {
        assert_eq!(field(&server, "pif", eth3, "management"), true);
        assert_eq!(text(&server, "pif", eth3, "ip_configuration_mode"), "DHCP");
        assert_eq!(field(&server, "pif", eth0, "management"), false);
        assert_eq!(text(&server, "pif", eth0, "ip_configuration_mode"), "None");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ip_configuration_failures() {
    let server = MockServer::start_with_records(false, bond_lab_records()).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    let m = model_when(&h, "loaded", |m| !m.pifs.is_empty()).await;

    // Host 2 refuses the address: the operation fails; host 1 was not
    // reached (host 2 goes first).
    server.fail_on_host("PIF.reconfigure_ip", H2, true);
    let mut p = pages(&m, H1);
    p.push(IpPage {
        original: None,
        network: r("OpaqueRef:net-eth2"),
        dhcp: true,
        ip: String::new(),
        netmask: String::new(),
        gateway: String::new(),
        dns: Default::default(),
        purpose: Some("Backup".into()),
    });
    let change = network::plan_ip_changes(&m, &r(H1), true, &p, false)
        .unwrap()
        .change
        .unwrap();
    let op = run(&h, conn, ActionRequest::ChangeNetworking(Box::new(change))).await;
    failed_with(&op, "PIF_CONFIGURATION_ERROR");
    assert_eq!(
        text(&server, "pif", PIF1_ETH2, "ip_configuration_mode"),
        "None"
    );
    server.fail_on_host("PIF.reconfigure_ip", H2, false);

    // A management move to a network without an address on a host.
    let mut change = network::plan_ip_changes(&m, &r(H1), false, &pages(&m, H1), false).unwrap();
    assert!(change.change.is_none());
    let mut p = pages(&m, H1);
    p[0].network = r("OpaqueRef:net-eth3");
    p[0].ip = "not an address".into();
    change = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap();
    let op = run(
        &h,
        conn,
        ActionRequest::ChangeNetworking(Box::new(change.change.unwrap())),
    )
    .await;
    failed_with(&op, "INVALID_IP_ADDRESS_SPECIFIED");
    // The management interface did not move.
    assert_eq!(field(&server, "pif", PIF1_ETH0, "management"), true);
    assert_eq!(server.call_count("Async.host.management_reconfigure"), 0);

    // A network that went away.
    let mut p = pages(&m, H1);
    p[0].network = r("OpaqueRef:gone");
    let e = network::plan_ip_changes(&m, &r(H1), false, &p, false).unwrap_err();
    assert!(e.is_api(network::errors::NETWORK_GONE));
}

fn vif_spec(network: &str, device: &str, kbps: Option<&str>) -> VifSpec {
    VifSpec {
        network: r(network),
        mac: String::new(),
        device: device.into(),
        qos_algorithm_type: if kbps.is_some() {
            "ratelimit".into()
        } else {
            String::new()
        },
        qos_algorithm_params: kbps
            .map(|k| [("kbps".to_owned(), k.to_owned())].into())
            .unwrap_or_default(),
    }
}

/// The VIF of `vm` with `device`.
fn vif_of(server: &MockServer, vm: &str, device: &str) -> Option<String> {
    refs(&field(server, "vm", vm, "VIFs"))
        .into_iter()
        .find(|v| text(server, "vif", v, "device") == device)
}

#[tokio::test(flavor = "multi_thread")]
async fn interfaces_of_halted_and_running_vms() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;

    // A halted VM: made, never plugged.
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(VM_DB),
            spec: vif_spec(NET_ETH1, "1", Some("2048")),
        },
    )
    .await;
    ok(&op);
    assert!(op.notice.is_none());
    let vif = vif_of(&server, VM_DB, "1").unwrap();
    assert_eq!(field(&server, "vif", &vif, "currently_attached"), false);
    assert_eq!(
        text(&server, "vif", &vif, "qos_algorithm_type"),
        "ratelimit"
    );
    assert_eq!(
        field(&server, "vif", &vif, "qos_algorithm_params")["kbps"],
        "2048"
    );
    assert_eq!(server.call_count("VIF.plug"), 0);

    // Properties: deleted and made again with the new network and no QoS,
    // keeping the device.
    model_when(&h, "vif cached", |m| m.vifs.contains_key(&r(&vif))).await;
    let op = run(
        &h,
        conn,
        ActionRequest::UpdateVif {
            vif: r(&vif),
            spec: vif_spec(NET_ETH0, "1", None),
        },
    )
    .await;
    ok(&op);
    assert!(server.record("vif", &vif).is_none());
    let new = vif_of(&server, VM_DB, "1").unwrap();
    assert_eq!(text(&server, "vif", &new, "network"), NET_ETH0);
    assert_eq!(text(&server, "vif", &new, "qos_algorithm_type"), "");

    // Activate on a halted VM does nothing.
    model_when(&h, "new vif cached", |m| m.vifs.contains_key(&r(&new))).await;
    ok(&run(&h, conn, ActionRequest::PlugVif { vif: r(&new) }).await);
    assert_eq!(server.call_count("VIF.plug"), 0);
    ok(&run(&h, conn, ActionRequest::DeleteVif { vif: r(&new) }).await);
    assert!(vif_of(&server, VM_DB, "1").is_none());

    // A running VM: hot-plugged.
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(VM_WEB),
            spec: vif_spec(NET_ETH1, "2", None),
        },
    )
    .await;
    ok(&op);
    let hot = vif_of(&server, VM_WEB, "2").unwrap();
    assert_eq!(field(&server, "vif", &hot, "currently_attached"), true);
    assert_eq!(server.call_count("VIF.plug"), 1);
    // Deactivate / Activate.
    model_when(&h, "hot vif cached", |m| {
        m.vifs.get(&r(&hot)).is_some_and(|v| v.currently_attached)
    })
    .await;
    ok(&run(&h, conn, ActionRequest::UnplugVif { vif: r(&hot) }).await);
    assert_eq!(field(&server, "vif", &hot, "currently_attached"), false);
    model_when(&h, "unplugged cached", |m| {
        m.vifs.get(&r(&hot)).is_some_and(|v| !v.currently_attached)
    })
    .await;
    ok(&run(&h, conn, ActionRequest::PlugVif { vif: r(&hot) }).await);
    assert_eq!(field(&server, "vif", &hot, "currently_attached"), true);
    // Removing it unplugs it first.
    model_when(&h, "plugged cached", |m| {
        m.vifs.get(&r(&hot)).is_some_and(|v| v.currently_attached)
    })
    .await;
    ok(&run(&h, conn, ActionRequest::DeleteVif { vif: r(&hot) }).await);
    assert!(server.record("vif", &hot).is_none());
    assert_eq!(server.call_count("VIF.unplug"), 2);

    // The server cannot hot-plug: the user is told to restart the VM.
    server.fail_method("VIF.plug", Some(("INTERNAL_ERROR", &["x"])));
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(VM_WEB),
            spec: vif_spec(NET_ETH1, "3", None),
        },
    )
    .await;
    failed_with(&op, "INTERNAL_ERROR");
    server.fail_method("VIF.plug", None);
    // A device that is taken.
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(VM_WEB),
            spec: vif_spec(NET_ETH1, "0", None),
        },
    )
    .await;
    failed_with(&op, "DEVICE_ALREADY_EXISTS");
    // A suspended VM: made, not plugged, no notice.
    let suspended = "OpaqueRef:vm-legacy";
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(suspended),
            spec: vif_spec(NET_ETH1, "5", None),
        },
    )
    .await;
    ok(&op);
    assert!(op.notice.is_none(), "not running: no hot-plug, no notice");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_vm_without_drivers_needs_a_restart() {
    let server = MockServer::start(false).await;
    let h = harness();
    let conn = connect(&h, &server).await;
    // db-01 runs without PV drivers: no hot-plug.
    server.set_vm_power_state(VM_DB, "Running");
    model_when(&h, "running", |m| {
        m.vms
            .get(&r(VM_DB))
            .is_some_and(|v| v.power_state == xapi::VmPowerState::Running)
    })
    .await;
    let op = run(
        &h,
        conn,
        ActionRequest::NewVif {
            vm: r(VM_DB),
            spec: vif_spec(NET_ETH1, "1", None),
        },
    )
    .await;
    ok(&op);
    assert_eq!(op.notice, Some(OperationNotice::RestartForVif));
    let vif = vif_of(&server, VM_DB, "1").unwrap();
    assert_eq!(field(&server, "vif", &vif, "currently_attached"), false);
    assert_eq!(server.call_count("VIF.plug"), 0);
    // Deactivate / Activate are no-ops when the server does not allow
    // them; Remove only destroys.
    let attached = vif_of(&server, VM_DB, "0").unwrap();
    model_when(&h, "vifs cached", |m| m.vifs.contains_key(&r(&vif))).await;
    ok(&run(&h, conn, ActionRequest::UnplugVif { vif: r(&attached) }).await);
    assert_eq!(field(&server, "vif", &attached, "currently_attached"), true);
    assert_eq!(server.call_count("VIF.unplug"), 0);
    // Destroying an attached interface of a running VM is refused.
    let op = run(&h, conn, ActionRequest::DeleteVif { vif: r(&attached) }).await;
    failed_with(&op, "OPERATION_NOT_ALLOWED");
    ok(&run(&h, conn, ActionRequest::DeleteVif { vif: r(&vif) }).await);
    assert!(server.record("vif", &vif).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn rbac_of_the_networking_operations() {
    let server = MockServer::start(false).await;
    let h = harness();
    for (user, password, can_vif, can_network) in [
        ("vm-operator-user", "vm-operator-password", false, false),
        (
            "vm-power-admin-user",
            "vm-power-admin-password",
            true,
            false,
        ),
        ("pool-operator-user", "pool-operator-password", true, true),
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
        use xen_core::rbac::RbacCheck;
        let check = |request: &ActionRequest| {
            xen_core::rbac::check(&conn.model, conn.rbac.as_deref(), &request.rbac_methods())
                == RbacCheck::Allowed
        };
        let vif = ActionRequest::NewVif {
            vm: r(VM_DB),
            spec: vif_spec(NET_ETH0, "3", None),
        };
        let net = spec("RBAC", NewNetworkKind::Internal);
        let bond = bond_spec("B", &[PIF1_ETH0, PIF1_ETH1], BondMode::BalanceSlb, None);
        assert_eq!(check(&vif), can_vif, "{user} vif");
        assert_eq!(check(&net), can_network, "{user} network");
        assert_eq!(check(&bond), can_network, "{user} bond");
        if !can_network {
            // The server refuses too.
            let id = conn.id;
            let op = run(&h, id, net).await;
            failed_with(&op, "RBAC_PERMISSION_DENIED");
            assert!(network_named(&server, "RBAC").is_none());
        }
        h.backend.send(Command::Disconnect {
            connection: conn.id,
        });
        h.wait_for("disconnected", |s| s.connections.is_empty())
            .await;
    }
}
