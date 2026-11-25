//! Networking (F9): the requests of the New Network wizard, the network
//! Properties dialog, the IP address configuration dialog, the NICs tab and
//! a VM's Networking tab, and the original console's rules for them.
//!
//! The runs are in [`crate::network_run`]. The helpers follow the
//! original's `XenAPI-Extensions` (`PIF`, `Network`, `Bond`, `VIF`) and
//! `NetworkingHelper`.

use std::collections::{HashMap, HashSet};

use xapi::{
    Bond, BondMode, BondRecord, Host, IpConfigurationMode, Network, NetworkRecord, Pif, PifRecord,
    Ref, Vif, VifOperations, VifRecord, Vm, VmPowerState, VmRecord,
};

use crate::model::ConnectionModel;
use crate::objects::{self, natural_cmp};

/// `Network.MTU_MIN` / `MTU_DEFAULT` / `MTU_MAX`.
pub const MTU_MIN: i64 = 1500;
pub const MTU_DEFAULT: i64 = 1500;
pub const MTU_MAX: i64 = 9216;
/// The highest VLAN tag the wizard accepts.
pub const VLAN_MAX: i64 = 4094;
/// `Network.CREATE_IN_PROGRESS`: set on a bond's network while it is made.
pub const CREATE_IN_PROGRESS: &str = "XenCenterCreateInProgress";
/// `other_config` key naming a secondary interface.
pub const MANAGEMENT_PURPOSE: &str = "management_purpose";
/// `VIF.RATE_LIMIT_QOS_VALUE` and `KBPS_QOS_PARAMS_KEY`.
pub const QOS_RATE_LIMIT: &str = "ratelimit";
pub const QOS_KBPS: &str = "kbps";
/// `VM.DEFAULT_NUM_VIFS_ALLOWED`.
pub const DEFAULT_MAX_VIFS: usize = 7;

/// Errors the client reports itself (`xapi::Error::Api` with these codes).
pub mod errors {
    /// The coordinator is not in the cache (`POOL_COORDINATOR_GONE`).
    pub const NO_COORDINATOR: &str = "XENADMIN_NETWORK_NO_COORDINATOR";
    /// The new bond is not in the cache (`BOND_GONE`).
    pub const BOND_GONE: &str = "XENADMIN_BOND_GONE";
    /// The VLAN tag is already used on that NIC (`PIF_VLAN_EXISTS`);
    /// params: the NIC's device and the tag.
    pub const VLAN_EXISTS: &str = "XENADMIN_PIF_VLAN_EXISTS";
    /// A network the request names is gone (`Network has gone away`).
    pub const NETWORK_GONE: &str = "XENADMIN_NETWORK_GONE";
}

/// A client-side error.
pub fn client_error(code: &str, params: &[&str]) -> xapi::Error {
    crate::storage::client_error(code, params)
}

// ---- PIFs -------------------------------------------------------------------

/// `PIF.IsBondNIC()`: the interface of a bond.
pub fn pif_is_bond_nic(pif: &PifRecord) -> bool {
    !pif.bond_master_of.is_empty()
}

/// `PIF.IsTunnelAccessPIF()`.
pub fn pif_is_tunnel_access(pif: &PifRecord) -> bool {
    !pif.tunnel_access_pif_of.is_empty()
}

/// `PIF.BondMemberOf()`.
pub fn pif_bond_member_of<'a>(
    model: &'a ConnectionModel,
    pif: &PifRecord,
) -> Option<(&'a Ref<Bond>, &'a BondRecord)> {
    model
        .bonds
        .get_key_value(&pif.bond_slave_of)
        .map(|(r, b)| (r, &**b))
}

/// `PIF.IsBondMember()`.
pub fn pif_is_bond_member(model: &ConnectionModel, pif: &PifRecord) -> bool {
    pif_bond_member_of(model, pif).is_some()
}

/// `PIF.IsInUseBondMember()`: a member of a bond whose interface is plugged.
pub fn pif_is_in_use_bond_member(model: &ConnectionModel, pif: &PifRecord) -> bool {
    pif_bond_member_of(model, pif)
        .and_then(|(_, b)| model.get(&b.master))
        .is_some_and(|m| m.currently_attached)
}

/// `PIF.Show(showHidden)`.
pub fn pif_is_shown(pif: &PifRecord, show_hidden: bool) -> bool {
    pif.managed
        && (show_hidden
            || pif
                .other_config
                .get("HideFromXenCenter")
                .map(String::as_str)
                != Some("true"))
}

/// `PIF.GetManagementPurpose()`: the name of a secondary interface.
pub fn management_purpose(pif: &PifRecord) -> Option<&str> {
    pif.other_config
        .get(MANAGEMENT_PURPOSE)
        .map(String::as_str)
        .filter(|p| !p.is_empty())
}

/// `PIF.IsPrimaryManagementInterface()`.
pub fn pif_is_primary_management(model: &ConnectionModel, pif: &PifRecord) -> bool {
    pif.management
        && model
            .get(&pif.network)
            .is_some_and(|n| !objects::is_guest_installer_network(n))
}

/// `PIF.IsManagementInterface(showHidden)`: the primary management
/// interface or a secondary one (an interface with an IP address).
pub fn pif_is_management_interface(
    model: &ConnectionModel,
    pif: &PifRecord,
    show_hidden: bool,
) -> bool {
    pif_is_primary_management(model, pif)
        || objects::pif_is_secondary_management(model, pif, show_hidden)
}

/// `PIF.IsUsedByClustering()`.
pub fn pif_used_by_clustering(model: &ConnectionModel, pif: &Ref<Pif>) -> bool {
    model.cluster_hosts.values().any(|c| &c.pif == pif)
}

/// `NetworkingHelper.ContainsPrimaryManagement`.
pub fn contains_primary_management(model: &ConnectionModel, pifs: &[Ref<Pif>]) -> bool {
    pifs.iter()
        .filter_map(|p| model.get(p))
        .any(|p| p.management)
}

/// `NetworkingHelper.ContainsSecondaryManagement`.
pub fn contains_secondary_management(model: &ConnectionModel, pifs: &[Ref<Pif>]) -> bool {
    pifs.iter()
        .filter_map(|p| model.get(p))
        .any(|p| objects::pif_is_secondary_management(model, p, true))
}

/// `NetworkingHelper.ContainsClusteringPif`.
pub fn contains_clustering_pif(model: &ConnectionModel, pifs: &[Ref<Pif>]) -> bool {
    pifs.iter().any(|p| pif_used_by_clustering(model, p))
}

fn sort_pifs<'a>(model: &ConnectionModel, pifs: &mut [(&'a Ref<Pif>, &'a PifRecord)]) {
    pifs.sort_by(|(ra, a), (rb, b)| {
        natural_cmp(&objects::pif_name(model, a), &objects::pif_name(model, b))
            .then_with(|| ra.cmp(rb))
    });
}

/// `NetworkingHelper.GetAllPhysicalPIFs(host)`: the NICs of a host that are
/// not bond interfaces, by name.
pub fn host_nics<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
) -> Vec<(&'a Ref<Pif>, &'a PifRecord)> {
    let mut pifs: Vec<_> = model
        .pifs
        .iter()
        .filter(|(_, p)| &p.host == host && objects::pif_is_physical(p) && !pif_is_bond_nic(p))
        .map(|(r, p)| (r, &**p))
        .collect();
    sort_pifs(model, &mut pifs);
    pifs
}

/// `NetworkingHelper.GetAllPhysicalPIFs(pool)`: one NIC per device name
/// over every host (the coordinator's first), by name.
pub fn pool_nics(model: &ConnectionModel) -> Vec<(&Ref<Pif>, &PifRecord)> {
    let mut out: Vec<(&Ref<Pif>, &PifRecord)> = Vec::new();
    for (host, _) in objects::sorted_hosts(model) {
        for (r, p) in host_nics(model, host) {
            if !out.iter().any(|(_, q)| q.device == p.device) {
                out.push((r, p));
            }
        }
    }
    sort_pifs(model, &mut out);
    out
}

/// `NetworkingHelper.PIFsOnAllHosts`: per host, its PIFs with the devices
/// of `pifs`, by name.
pub fn pifs_on_all_hosts(
    model: &ConnectionModel,
    pifs: &[Ref<Pif>],
) -> Vec<(Ref<Host>, Vec<Ref<Pif>>)> {
    let devices: HashSet<&str> = pifs
        .iter()
        .filter_map(|p| model.get(p))
        .map(|p| p.device.as_str())
        .collect();
    objects::sorted_hosts(model)
        .into_iter()
        .map(|(h, _)| {
            let mut on: Vec<_> = model
                .pifs
                .iter()
                .filter(|(_, p)| &p.host == h && devices.contains(p.device.as_str()))
                .map(|(r, p)| (r, &**p))
                .collect();
            sort_pifs(model, &mut on);
            (h.clone(), on.into_iter().map(|(r, _)| r.clone()).collect())
        })
        .collect()
}

/// `PIF.BondSuffix`: the members' NIC numbers, sorted, joined by `+`.
pub fn bond_suffix(model: &ConnectionModel, pifs: &[Ref<Pif>]) -> String {
    let mut ids: Vec<String> = pifs
        .iter()
        .filter_map(|p| model.get(p))
        .map(|p| p.device.replace("eth", ""))
        .collect();
    ids.sort();
    ids.join("+")
}

/// The bond of the interface `pif` (`PIF.BondInterfaceOf()`).
pub fn bond_of_interface<'a>(
    model: &'a ConnectionModel,
    pif: &PifRecord,
) -> Option<(&'a Ref<Bond>, &'a BondRecord)> {
    pif.bond_master_of
        .first()
        .and_then(|b| model.bonds.get_key_value(b))
        .map(|(r, b)| (r, &**b))
}

/// `NetworkingHelper.FindBond`: the bond on `host` matching `bond` (same
/// member devices, one a subset of the other).
pub fn find_bond<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
    bond: &BondRecord,
) -> Option<(&'a Ref<Bond>, &'a BondRecord)> {
    let devices = |b: &BondRecord| -> Vec<String> {
        b.slaves
            .iter()
            .filter_map(|p| model.get(p))
            .map(|p| p.device.clone())
            .collect()
    };
    let wanted = devices(bond);
    model
        .bonds
        .iter()
        .filter(|(_, b)| model.get(&b.master).is_some_and(|m| &m.host == host))
        .find(|(_, b)| {
            let mut s1 = devices(b);
            let mut s2 = wanted.clone();
            if s1.len() > s2.len() {
                std::mem::swap(&mut s1, &mut s2);
            }
            s1.iter()
                .all(|d| s2.iter().filter(|x| *x == d).count() == 1)
        })
        .map(|(r, b)| (r, &**b))
}

// ---- networks ---------------------------------------------------------------

/// `Network.GetAutoPlug()`: added to new VMs unless `automatic` is `false`.
pub fn network_auto_plug(network: &NetworkRecord) -> bool {
    network.other_config.get("automatic").map(String::as_str) != Some("false")
}

/// `Network.CreateInProgress()`.
pub fn network_create_in_progress(network: &NetworkRecord) -> bool {
    network
        .other_config
        .get(CREATE_IN_PROGRESS)
        .map(String::as_str)
        == Some("true")
}

/// The PIFs of a network.
pub fn network_pifs<'a>(
    model: &'a ConnectionModel,
    network: &NetworkRecord,
) -> Vec<(&'a Ref<Pif>, &'a PifRecord)> {
    network
        .pifs
        .iter()
        .filter_map(|r| model.pifs.get_key_value(r))
        .map(|(r, p)| (r, &**p))
        .collect()
}

/// `Network.TheBonds()`: the bonds whose interfaces are on this network.
pub fn network_bonds<'a>(
    model: &'a ConnectionModel,
    network: &NetworkRecord,
) -> Vec<(&'a Ref<Bond>, &'a BondRecord)> {
    network_pifs(model, network)
        .into_iter()
        .filter_map(|(_, p)| bond_of_interface(model, p))
        .collect()
}

/// `Network.IsBond()`.
pub fn network_is_bond(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    !network_bonds(model, network).is_empty()
}

/// `Network.IsVLAN()`.
pub fn network_is_vlan(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    network_pifs(model, network)
        .iter()
        .any(|(_, p)| p.vlan >= 0)
}

/// `Network.IsSriov()`.
pub fn network_is_sriov(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    network_pifs(model, network)
        .iter()
        .any(|(_, p)| !p.sriov_logical_pif_of.is_empty())
}

/// A cross-server private network: its PIFs are tunnel access PIFs.
pub fn network_is_chin(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    let pifs = network_pifs(model, network);
    !pifs.is_empty() && pifs.iter().all(|(_, p)| pif_is_tunnel_access(p))
}

/// `Network.CanUseJumboFrames()`: not a cross-server private network.
pub fn network_can_use_jumbo(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    !network_pifs(model, network)
        .iter()
        .any(|(_, p)| pif_is_tunnel_access(p))
}

/// `NetworkList.HasPhysicalNonBondNIC`: a network on a NIC (cannot be
/// removed).
pub fn network_has_physical_nic(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    network_pifs(model, network)
        .iter()
        .any(|(_, p)| objects::pif_is_physical(p) && !pif_is_bond_nic(p))
}

/// The coordinator (the pool's master).
pub fn coordinator(model: &ConnectionModel) -> Option<&Ref<Host>> {
    model
        .pool()
        .map(|(_, p)| &p.master)
        .filter(|h| model.hosts.contains_key(*h))
}

/// `Host.vSwitchNetworkBackend()` of the coordinator.
pub fn vswitch_backend(model: &ConnectionModel) -> bool {
    coordinator(model)
        .and_then(|h| model.hosts.get(h))
        .is_some_and(|h| {
            h.software_version
                .get("network_backend")
                .map(String::as_str)
                == Some("openvswitch")
        })
}

/// `Helpers.BondSizeLimit`: 4 NICs on the vSwitch backend, else 2.
pub fn bond_size_limit(model: &ConnectionModel) -> usize {
    if vswitch_backend(model) { 4 } else { 2 }
}

/// `Helpers.VLAN0Allowed` and `SupportsLinkAggregationBond`: the vSwitch
/// backend.
pub fn vlan0_allowed(model: &ConnectionModel) -> bool {
    vswitch_backend(model)
}

/// See [`vlan0_allowed`].
pub fn supports_lacp(model: &ConnectionModel) -> bool {
    vswitch_backend(model)
}

/// `NetWDetails.GetVLANList`: the VLAN tags used on NIC `device` (on any
/// host), SR-IOV networks apart.
pub fn vlans_in_use(model: &ConnectionModel, device: &str) -> Vec<i64> {
    model
        .pifs
        .values()
        .filter(|p| p.device == device && p.sriov_logical_pif_of.is_empty())
        .map(|p| p.vlan)
        .collect()
}

/// The first VLAN tag from 1 not in `used` (CA-19111).
pub fn first_free_vlan(used: &[i64]) -> i64 {
    (1..=VLAN_MAX).find(|v| !used.contains(v)).unwrap_or(1)
}

/// The NICs of the External network page (`NetWDetails.PopulatePage`): the
/// host's physical PIFs and bond interfaces that are not bond members.
pub fn vlan_nics<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
    show_hidden: bool,
) -> Vec<(&'a Ref<Pif>, &'a PifRecord)> {
    let mut pifs: Vec<_> = model
        .pifs
        .iter()
        .filter(|(_, p)| {
            &p.host == host
                && objects::pif_is_physical(p)
                && pif_is_shown(p, show_hidden)
                && !pif_is_bond_member(model, p)
        })
        .map(|(r, p)| (r, &**p))
        .collect();
    sort_pifs(model, &mut pifs);
    pifs
}

/// The management interfaces a cross-server private network can use
/// (`NetWChinDetails.PopulateInterfaces`): the networks of the management
/// interfaces, of `host` only when given.
pub fn chin_interfaces<'a>(
    model: &'a ConnectionModel,
    host: Option<&Ref<Host>>,
    show_hidden: bool,
) -> Vec<(&'a Ref<Network>, &'a NetworkRecord)> {
    let mut out: Vec<(&Ref<Network>, &NetworkRecord)> = Vec::new();
    for pif in model.pifs.values() {
        if host.is_some_and(|h| &pif.host != h) {
            continue;
        }
        if !pif_is_management_interface(model, pif, show_hidden) {
            continue;
        }
        if let Some((r, n)) = model.networks.get_key_value(&pif.network)
            && !out.iter().any(|(x, _)| *x == r)
        {
            out.push((r, n));
        }
    }
    out.sort_by(|(ra, a), (rb, b)| {
        natural_cmp(
            &objects::network_name(model, a),
            &objects::network_name(model, b),
        )
        .then_with(|| ra.cmp(rb))
    });
    out
}

/// `Helpers.MakeUniqueName`: `stub`, else `stub (1)`, `stub (2)`…
pub fn unique_name(stub: &str, existing: &[String]) -> String {
    let mut name = stub.to_owned();
    let mut i = 1;
    while existing.iter().any(|e| e == &name) {
        name = format!("{stub} ({i})");
        i += 1;
    }
    name
}

/// The shown names of the networks (`GetExistingNetworkNames`).
pub fn network_names(model: &ConnectionModel) -> Vec<String> {
    model
        .networks
        .values()
        .map(|n| objects::network_name(model, n))
        .collect()
}

/// `NetWName.CheckUniqueName`: a name already in use gets a free
/// " (n)" suffix on its base.
pub fn checked_unique_name(name: &str, existing: &[String]) -> String {
    if !existing.iter().any(|e| e == name) {
        return name.to_owned();
    }
    let base = name.split('(').next().unwrap_or(name).trim_end();
    unique_name(base, existing)
}

/// Whether a network row's Properties button is enabled
/// (`NetworkList.UpdateEnablement`).
pub fn network_can_edit(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    !objects::is_bond_member_network(model, network)
        && !network_create_in_progress(network)
        && !objects::is_guest_installer_network(network)
}

/// Whether Remove Network is enabled: as Properties, and not a NIC's own
/// network.
pub fn network_can_remove(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    network_can_edit(model, network) && !network_has_physical_nic(model, network)
}

// ---- VIFs -------------------------------------------------------------------

/// `VIF.GetDeviceId`: the lowest device number the VM's interfaces do not
/// use.
pub fn next_vif_device(model: &ConnectionModel, vm: &VmRecord) -> u32 {
    let used: HashSet<String> = vm
        .vifs
        .iter()
        .filter_map(|v| model.get(v))
        .map(|v| v.device.clone())
        .collect();
    (0..)
        .find(|i: &u32| !used.contains(&i.to_string()))
        .unwrap_or(0)
}

/// `VM.MaxVIFsAllowed()`.
pub fn max_vifs(vm: &VmRecord) -> usize {
    crate::provisioning::recommendation(vm, "number-of-vifs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_VIFS)
}

/// `Helpers.IsValidMAC`: six hex pairs separated by colons.
pub fn is_valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `VIF.LimitString()`: the QoS limit in kB/s, or empty.
pub fn vif_limit(vif: &VifRecord) -> &str {
    vif.qos_algorithm_params
        .get(QOS_KBPS)
        .map(String::as_str)
        .unwrap_or_default()
}

/// The networks the VIF dialog offers (`VIFDialog.LoadNetworks`): shown,
/// not bond members, not SR-IOV; sorted by name.
pub fn vif_networks(
    model: &ConnectionModel,
    show_hidden: bool,
) -> Vec<(&Ref<Network>, &NetworkRecord)> {
    objects::shown_networks(model, show_hidden)
        .into_iter()
        .filter(|(_, n)| !objects::is_bond_member_network(model, n) && !network_is_sriov(model, n))
        .map(|(r, n)| (r, &**n))
        .collect()
}

/// I/O drivers are running in the guest (`VirtualizationStatus.IoDriversInstalled`).
pub fn io_drivers_installed(model: &ConnectionModel, vm: &VmRecord) -> bool {
    vm.power_state == VmPowerState::Running
        && model
            .get(&vm.guest_metrics)
            .is_some_and(|g| g.pv_drivers_detected)
}

/// Whether Remove Interface and Properties are enabled for `vif`.
pub fn vif_can_remove(vif: &VifRecord) -> bool {
    vif.allowed_operations.contains(&VifOperations::Unplug) || !vif.currently_attached
}

/// Whether Activate / Deactivate is enabled for `vif`.
pub fn vif_can_toggle(vif: &VifRecord) -> bool {
    if vif.currently_attached {
        vif.allowed_operations.contains(&VifOperations::Unplug)
    } else {
        vif.allowed_operations.contains(&VifOperations::Plug)
    }
}

/// Why a VM's interface buttons may not work (their tooltips).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VifHint {
    /// The VM is suspended.
    Suspended,
    /// A running VM without I/O drivers.
    NoIoDrivers,
}

/// The tooltip case of a VM's interface buttons, if any.
pub fn vif_hint(model: &ConnectionModel, vm: &VmRecord) -> Option<VifHint> {
    match vm.power_state {
        VmPowerState::Suspended => Some(VifHint::Suspended),
        VmPowerState::Running if !io_drivers_installed(model, vm) => Some(VifHint::NoIoDrivers),
        _ => None,
    }
}

/// The MAC address of another VM's interface equal to `mac`, with that
/// VM (`VIFDialog_FormClosing`).
pub fn duplicate_mac<'a>(
    model: &'a ConnectionModel,
    mac: &str,
    except: Option<&Ref<Vif>>,
) -> Option<&'a VmRecord> {
    if mac.is_empty() {
        return None;
    }
    model
        .vifs
        .iter()
        .filter(|(r, _)| Some(*r) != except)
        .filter(|(_, v)| v.mac.eq_ignore_ascii_case(mac))
        .filter_map(|(_, v)| model.vms.get(&v.vm))
        .map(|v| &**v)
        .find(|vm| crate::model::is_guest(vm))
}

// ---- requests ---------------------------------------------------------------

/// The kind of a new network (`NetworkTypes`; SR-IOV is not offered).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NewNetworkKind {
    /// External: a VLAN on a NIC (`pif` on the wizard's host).
    External { pif: Ref<Pif>, vlan: i64 },
    /// Single-server private network: no PIFs.
    Internal,
    /// Cross-server private network: tunnels over the management
    /// interfaces of `interface`.
    CrossServer { interface: Ref<Network> },
}

/// The New Network wizard's network (`NetworkAction`, `CreateChinAction`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NewNetworkSpec {
    pub name: String,
    pub description: String,
    pub auto_plug: bool,
    /// `None` keeps the server's default.
    pub mtu: Option<i64>,
    pub kind: NewNetworkKind,
}

/// The LACP load balancing of a bond (`Bond.hashing_algoritm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashingAlgorithm {
    SrcMac,
    TcpUdpPorts,
}

impl HashingAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SrcMac => "src_mac",
            Self::TcpUdpPorts => "tcpudp_ports",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "src_mac" => Some(Self::SrcMac),
            "tcpudp_ports" => Some(Self::TcpUdpPorts),
            _ => None,
        }
    }
}

/// `Bond.HashingAlgoritm()`.
pub fn bond_hashing(bond: &BondRecord) -> Option<HashingAlgorithm> {
    bond.properties
        .get("hashing_algorithm")
        .and_then(|h| HashingAlgorithm::parse(h))
}

/// A new bond (`CreateBondAction`): `pifs` are on the host the wizard or
/// dialog shows (the coordinator for a pool); the bond is made on every
/// host with the NICs of the same devices.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NewBondSpec {
    pub name: String,
    pub pifs: Vec<Ref<Pif>>,
    pub mode: BondMode,
    /// LACP only.
    pub hashing: Option<HashingAlgorithm>,
    pub mtu: i64,
    pub auto_plug: bool,
}

/// What a network's NIC and VLAN become (the Network Settings page).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PifChange {
    /// `<internal>`: the network's PIFs are destroyed.
    Internal,
    /// A VLAN on `pif` (a NIC of the coordinator).
    External { pif: Ref<Pif>, vlan: i64 },
}

/// The network Properties dialog: what changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NetworkChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub auto_plug: Option<bool>,
    pub mtu: Option<i64>,
    pub bond_mode: Option<BondMode>,
    pub hashing: Option<HashingAlgorithm>,
    pub pifs: Option<PifChange>,
}

impl NetworkChanges {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The network is replugged to apply a new MTU (`UnplugPlugNetworkAction`)
    /// when its PIFs do not change anyway.
    pub fn replug(&self) -> bool {
        self.mtu.is_some() && self.pifs.is_none()
    }
}

/// The IP settings of one interface (a page of the Configure IP Addresses
/// dialog, applied to `pif`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PifConfig {
    /// The PIF on the dialog's host.
    pub pif: Ref<Pif>,
    /// DHCP or Static.
    pub mode: IpConfigurationMode,
    /// The address; in a pool, the start of the range (host `i` of the
    /// sorted hosts gets the last octet plus `i`).
    pub ip: String,
    pub netmask: String,
    pub gateway: String,
    /// Comma-separated.
    pub dns: String,
    /// The primary management interface.
    pub management: bool,
    /// The name of a secondary interface.
    pub purpose: Option<String>,
}

/// The Configure IP Addresses dialog's changes (`ChangeNetworkingAction`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IpChange {
    /// Pool-wide (more than one host): each change applies to the PIFs of
    /// the same network on every host.
    pub pool: bool,
    /// The dialog's host (the coordinator for a pool).
    pub host: Ref<Host>,
    /// Interfaces to bring up or reconfigure.
    pub up: Vec<PifConfig>,
    /// Interfaces to bring down (their IP removed).
    pub down: Vec<Ref<Pif>>,
    /// The management interface moves from `down_management` to
    /// `new_management`.
    pub new_management: Option<PifConfig>,
    pub down_management: Option<Ref<Pif>>,
    /// The management interface's address changes (the connection is not
    /// expected to survive otherwise).
    pub management_ip_changed: bool,
}

/// One page of the Configure IP Addresses dialog, as the user left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpPage {
    /// The PIF the page was opened for (`None` for an added page).
    pub original: Option<Ref<Pif>>,
    /// The network chosen.
    pub network: Ref<Network>,
    pub dhcp: bool,
    pub ip: String,
    pub netmask: String,
    pub gateway: String,
    /// Up to three servers, empty ones skipped.
    pub dns: [String; 3],
    /// `None` for the management page.
    pub purpose: Option<String>,
}

/// What the dialog asks for once accepted (`NetworkingProperties.AcceptBtn_Click`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpPlan {
    /// Secondary interfaces only renamed (`SetSecondaryManagementPurposeAction`).
    pub renames: Vec<(Ref<Pif>, String)>,
    /// The reconfiguration, if any.
    pub change: Option<IpChange>,
    /// The management interface is touched: the original's warning is
    /// asked first.
    pub warn_management: bool,
}

/// The PIF of `network` on `host`.
pub fn pif_on_host<'a>(
    model: &'a ConnectionModel,
    network: &NetworkRecord,
    host: &Ref<Host>,
) -> Option<(&'a Ref<Pif>, &'a PifRecord)> {
    objects::network_pif(model, network, Some(host)).map(|(r, p)| (r, &**p))
}

/// The interfaces the dialog lists (`GetKnownPIFs`): per shown network,
/// its PIF on `host`.
pub fn known_pifs<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
    include_hidden: bool,
) -> Vec<(&'a Ref<Pif>, &'a PifRecord)> {
    model
        .networks
        .values()
        .filter(|n| objects::network_is_shown(model, n, include_hidden))
        .filter_map(|n| pif_on_host(model, n, host))
        .collect()
}

/// The networks a page may choose (`MakeProposedInUseMap` keys and
/// `RefreshNetworkComboBox`): shown networks with a PIF that is not a
/// tunnel's and not an in-use bond member; VLANs are fine for the
/// management interface on XCP-ng; never SR-IOV. Sorted by name.
pub fn ip_networks<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
    show_hidden: bool,
) -> Vec<(&'a Ref<Network>, &'a NetworkRecord)> {
    let mut out: Vec<_> = model
        .networks
        .iter()
        .filter(|(_, n)| objects::network_is_shown(model, n, show_hidden))
        .filter(|(_, n)| {
            network_pifs(model, n)
                .iter()
                .any(|(_, p)| !pif_is_tunnel_access(p))
        })
        .filter(|(_, n)| {
            !pif_on_host(model, n, host).is_some_and(|(_, p)| pif_is_in_use_bond_member(model, p))
        })
        .filter(|(_, n)| !network_is_sriov(model, n))
        .map(|(r, n)| (r, &**n))
        .collect();
    out.sort_by(|(ra, a), (rb, b)| {
        natural_cmp(
            &objects::network_name(model, a),
            &objects::network_name(model, b),
        )
        .then_with(|| ra.cmp(rb))
    });
    out
}

/// Whether `s` is a dotted IPv4 address (`StringUtility.IsIPAddress`).
pub fn is_ip_address(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok() && s.split('.').count() == 4
}

/// `StringUtility.IsValidNetmask`: contiguous ones.
pub fn is_valid_netmask(s: &str) -> bool {
    let Ok(addr) = s.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let bits = u32::from(addr);
    bits != 0 && bits.leading_ones() + bits.trailing_zeros() == 32
}

/// Whether a page's values are acceptable (`NetworkingPropertiesPage.Valid`
/// without the in-use check, which needs the other pages).
pub fn ip_page_valid(page: &IpPage) -> bool {
    let optional = |s: &str| s.is_empty() || is_ip_address(s);
    let ip_ok = page.dhcp
        || (is_ip_address(&page.ip) && is_valid_netmask(&page.netmask) && optional(&page.gateway));
    let dns_ok = page.purpose.is_some() || page.dns.iter().all(|d| optional(d));
    let name_ok = page.purpose.as_ref().is_none_or(|p| !p.is_empty());
    ip_ok && dns_ok && name_ok
}

/// The address of host number `index` in a pool range starting at `start`
/// (`GetIPInRange`).
pub fn ip_in_range(start: &str, index: usize) -> Option<String> {
    let bits: Vec<&str> = start.split('.').collect();
    let last: usize = bits.get(3)?.parse().ok()?;
    Some(format!(
        "{}.{}.{}.{}",
        bits[0],
        bits[1],
        bits[2],
        last + index
    ))
}

/// Whether a pool range starting at `start` fits for `hosts` hosts.
pub fn range_fits(start: &str, hosts: usize) -> bool {
    start
        .split('.')
        .nth(3)
        .and_then(|b| b.parse::<usize>().ok())
        .is_some_and(|b| b + hosts <= 256)
}

/// The dialog's changes, as `AcceptBtn_Click` and `CollateChanges`
/// compute them. `pages[0]` is the management page. A page's network
/// without a PIF on `host` means the network went away.
pub fn plan_ip_changes(
    model: &ConnectionModel,
    host: &Ref<Host>,
    pool: bool,
    pages: &[IpPage],
    show_hidden: bool,
) -> Result<IpPlan, xapi::Error> {
    let gone = || client_error(errors::NETWORK_GONE, &[]);
    let all = known_pifs(model, host, true);
    let mut down: Vec<Ref<Pif>> = all
        .iter()
        .filter(|(_, p)| pif_is_management_interface(model, p, show_hidden))
        .map(|(r, _)| (*r).clone())
        .collect();
    let mut up: Vec<(PifConfig, bool)> = Vec::new();
    let mut renames = Vec::new();
    for page in pages {
        let network = model.networks.get(&page.network).ok_or_else(gone)?;
        let (target_ref, target) = pif_on_host(model, network, host).ok_or_else(gone)?;
        let mut changed = page.original.as_ref() != Some(target_ref);
        let mut config = PifConfig {
            pif: target_ref.clone(),
            mode: target.ip_configuration_mode.clone(),
            ip: target.ip.clone(),
            netmask: target.netmask.clone(),
            gateway: target.gateway.clone(),
            dns: target.dns.clone(),
            management: page.purpose.is_none(),
            purpose: management_purpose(target).map(str::to_owned),
        };
        let set = |field: &mut String, value: &str, changed: &mut bool| {
            if field != value {
                *field = value.to_owned();
                *changed = true;
            }
        };
        if page.dhcp {
            if config.mode != IpConfigurationMode::Dhcp {
                config.mode = IpConfigurationMode::Dhcp;
                changed = true;
            }
        } else {
            if config.mode != IpConfigurationMode::Static {
                config.mode = IpConfigurationMode::Static;
                changed = true;
            }
            set(&mut config.netmask, &page.netmask, &mut changed);
            set(&mut config.gateway, &page.gateway, &mut changed);
            set(&mut config.ip, &page.ip, &mut changed);
            let dns: Vec<&str> = page
                .dns
                .iter()
                .map(String::as_str)
                .filter(|d| !d.is_empty())
                .collect();
            set(&mut config.dns, &dns.join(","), &mut changed);
        }
        if config.management != target.management {
            changed = true;
        }
        if let Some(purpose) = &page.purpose {
            let old = management_purpose(target).unwrap_or_default();
            if purpose != old {
                if changed {
                    config.purpose = Some(purpose.clone());
                } else {
                    renames.push((target_ref.clone(), purpose.clone()));
                }
            }
        }
        up.push((config, changed));
    }

    let down_management = down
        .iter()
        .find(|p| model.get(*p).is_some_and(|r| r.management))
        .cloned();
    let new_management = up.iter().map(|(c, _)| c).find(|c| c.management).cloned();
    let mut warn = false;
    let mut ip_changed = false;
    let mut down_management = down_management;
    if let (Some(old_ref), Some(new)) = (down_management.clone(), new_management.as_ref()) {
        let old = model.get(&old_ref).ok_or_else(gone)?;
        let address_changed =
            old.ip != new.ip || old.netmask != new.netmask || old.gateway != new.gateway;
        ip_changed = match (&old.ip_configuration_mode, &new.mode) {
            (IpConfigurationMode::Dhcp, IpConfigurationMode::Dhcp) => {
                old.dns != new.dns || address_changed
            }
            (IpConfigurationMode::Static, IpConfigurationMode::Dhcp) => true,
            _ => address_changed,
        };
        warn = ip_changed || old_ref != new.pif || old.ip_configuration_mode != new.mode;
        if old_ref == new.pif {
            down_management = None;
        }
    }
    let up_refs: HashSet<Ref<Pif>> = up.iter().map(|(c, _)| c.pif.clone()).collect();
    down.retain(|p| !up_refs.contains(p));
    let mut up: Vec<PifConfig> = up
        .into_iter()
        .filter(|(_, changed)| *changed)
        .map(|(c, _)| c)
        .collect();
    if up.is_empty() && down.is_empty() {
        return Ok(IpPlan {
            renames,
            change: None,
            warn_management: false,
        });
    }
    let new_management = match &down_management {
        None => None,
        Some(d) => {
            down.retain(|p| p != d);
            new_management
        }
    };
    up.reverse();
    down.reverse();
    Ok(IpPlan {
        renames,
        change: Some(IpChange {
            pool,
            host: host.clone(),
            up,
            down,
            new_management,
            down_management,
            management_ip_changed: ip_changed,
        }),
        warn_management: warn,
    })
}

/// A new or changed virtual interface (`VIFDialog.GetNewSettings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VifSpec {
    pub network: Ref<Network>,
    /// Empty: generated by the server.
    pub mac: String,
    pub device: String,
    /// `ratelimit` or empty.
    pub qos_algorithm_type: String,
    pub qos_algorithm_params: HashMap<String, String>,
}

impl std::hash::Hash for VifSpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.network.hash(state);
        self.mac.hash(state);
        self.device.hash(state);
        self.qos_algorithm_type.hash(state);
        let mut params: Vec<_> = self.qos_algorithm_params.iter().collect();
        params.sort();
        params.hash(state);
    }
}

/// The record `VIF.create` gets for `spec` on `vm`: the existing
/// interface's other fields when it replaces one (`UpdateFrom`).
pub fn vif_record(vm: &Ref<Vm>, spec: &VifSpec, existing: Option<&VifRecord>) -> VifRecord {
    let mut record = existing.cloned().unwrap_or_default();
    record.vm = vm.clone();
    record.network = spec.network.clone();
    record.mac = spec.mac.clone();
    record.device = spec.device.clone();
    record.qos_algorithm_type = spec.qos_algorithm_type.clone();
    record.qos_algorithm_params = spec.qos_algorithm_params.clone();
    if existing.is_none() {
        record.mtu = MTU_DEFAULT;
    }
    // Empty enum values are refused by XAPI.
    if record.locking_mode.is_unknown() {
        record.locking_mode = xapi::VifLockingMode::NetworkDefault;
    }
    if record.ipv4_configuration_mode.is_unknown() {
        record.ipv4_configuration_mode = xapi::VifIpv4ConfigurationMode::None;
    }
    if record.ipv6_configuration_mode.is_unknown() {
        record.ipv6_configuration_mode = xapi::VifIpv6ConfigurationMode::None;
    }
    record
}

/// The interfaces of a VM shown on its Networking tab (the guest installer
/// network only with hidden objects), by device.
pub fn vm_vifs<'a>(
    model: &'a ConnectionModel,
    vm: &VmRecord,
    show_hidden: bool,
) -> Vec<(&'a Ref<Vif>, &'a VifRecord)> {
    let mut vifs: Vec<_> = vm
        .vifs
        .iter()
        .filter_map(|r| model.vifs.get_key_value(r))
        .map(|(r, v)| (r, &**v))
        .filter(|(_, v)| {
            show_hidden
                || !model
                    .get(&v.network)
                    .is_some_and(|n| objects::is_guest_installer_network(n))
        })
        .collect();
    vifs.sort_by(|(_, a), (_, b)| natural_cmp(&a.device, &b.device));
    vifs
}

/// Whether the VM is running (`VIF` hot-plug happens only then).
pub fn vm_running(vm: &VmRecord) -> bool {
    vm.power_state == VmPowerState::Running
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    #[test]
    fn names_macs_and_addresses() {
        assert_eq!(unique_name("New Network", &[]), "New Network");
        let taken = vec!["New Network".to_owned(), "New Network (1)".to_owned()];
        assert_eq!(unique_name("New Network", &taken), "New Network (2)");
        assert_eq!(
            checked_unique_name("New Network (1)", &taken),
            "New Network (2)"
        );
        assert!(is_valid_mac("02:aB:00:00:00:0f"));
        assert!(!is_valid_mac("02:ab:00:00:00"));
        assert!(!is_valid_mac("02-ab-00-00-00-0f"));
        assert!(is_ip_address("10.0.0.1"));
        assert!(!is_ip_address("10.0.0"));
        assert!(is_valid_netmask("255.255.255.0"));
        assert!(!is_valid_netmask("255.0.255.0"));
        assert_eq!(ip_in_range("192.0.2.20", 2).as_deref(), Some("192.0.2.22"));
        assert!(range_fits("192.0.2.250", 6));
        assert!(!range_fits("192.0.2.251", 6));
        assert_eq!(first_free_vlan(&[-1, 1, 2, 4]), 3);
    }

    #[test]
    fn fixture_rules() {
        let m = fixture();
        let h1 = Ref::new("OpaqueRef:host-0001");
        let nics: Vec<String> = host_nics(&m, &h1)
            .iter()
            .map(|(_, p)| p.device.clone())
            .collect();
        assert_eq!(nics, ["eth0", "eth1"]);
        assert_eq!(pool_nics(&m).len(), 2);
        let mut used = vlans_in_use(&m, "eth1");
        used.sort();
        assert_eq!(used, [-1, -1, 20]);
        let eth0 = &m.networks[&Ref::new("OpaqueRef:net-eth0")];
        let vlan = &m.networks[&Ref::new("OpaqueRef:net-vlan20")];
        assert!(!network_can_remove(&m, eth0));
        assert!(network_can_edit(&m, eth0));
        assert!(network_can_remove(&m, vlan));
        assert!(network_is_vlan(&m, vlan));
        assert!(network_can_use_jumbo(&m, vlan));
        // The management networks: eth0 (primary) and eth1 (Storage).
        let chin: Vec<_> = chin_interfaces(&m, None, false)
            .iter()
            .map(|(r, _)| (*r).clone())
            .collect();
        assert_eq!(chin.len(), 2);
        let web = &m.vms[&Ref::new("OpaqueRef:vm-web")];
        assert_eq!(next_vif_device(&m, web), 2);
        assert_eq!(max_vifs(web), DEFAULT_MAX_VIFS);
        assert!(duplicate_mac(&m, "02:00:00:00:0A:01", None).is_some());
        assert!(
            duplicate_mac(
                &m,
                "02:00:00:00:0a:01",
                Some(&Ref::new("OpaqueRef:vif-web-0"))
            )
            .is_none()
        );
        let pifs = vec![
            Ref::new("OpaqueRef:pif-01-eth0"),
            Ref::new("OpaqueRef:pif-01-eth1"),
        ];
        assert_eq!(bond_suffix(&m, &pifs), "0+1");
        assert!(contains_primary_management(&m, &pifs));
        assert!(contains_secondary_management(&m, &pifs));
        let all = pifs_on_all_hosts(&m, &pifs[..1]);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].1, [Ref::new("OpaqueRef:pif-02-eth0")]);
    }

    #[test]
    fn ip_plan_follows_the_dialog() {
        let m = fixture();
        let h1 = Ref::new("OpaqueRef:host-0001");
        let eth0 = Ref::new("OpaqueRef:net-eth0");
        let eth1 = Ref::new("OpaqueRef:net-eth1");
        let page =
            |network: &Ref<Network>, original: &str, ip: &str, purpose: Option<&str>| IpPage {
                original: Some(Ref::new(original)),
                network: network.clone(),
                dhcp: false,
                ip: ip.to_owned(),
                netmask: "255.255.255.0".to_owned(),
                gateway: if purpose.is_none() {
                    "192.0.2.1".to_owned()
                } else {
                    String::new()
                },
                dns: if purpose.is_none() {
                    ["192.0.2.53".to_owned(), String::new(), String::new()]
                } else {
                    Default::default()
                },
                purpose: purpose.map(str::to_owned),
            };
        // Nothing changed.
        let pages = vec![
            page(&eth0, "OpaqueRef:pif-01-eth0", "192.0.2.11", None),
            page(
                &eth1,
                "OpaqueRef:pif-01-eth1",
                "198.51.100.11",
                Some("Storage"),
            ),
        ];
        let plan = plan_ip_changes(&m, &h1, false, &pages, false).unwrap();
        assert!(plan.change.is_none() && plan.renames.is_empty() && !plan.warn_management);
        // A rename only.
        let mut renamed = pages.clone();
        renamed[1].purpose = Some("iSCSI".into());
        let plan = plan_ip_changes(&m, &h1, false, &renamed, false).unwrap();
        assert_eq!(
            plan.renames,
            [(Ref::new("OpaqueRef:pif-01-eth1"), "iSCSI".to_owned())]
        );
        assert!(plan.change.is_none());
        // The secondary interface's address changes.
        let mut moved = pages.clone();
        moved[1].ip = "198.51.100.12".into();
        let change = plan_ip_changes(&m, &h1, false, &moved, false)
            .unwrap()
            .change
            .unwrap();
        assert_eq!(change.up.len(), 1);
        assert_eq!(change.up[0].ip, "198.51.100.12");
        assert!(change.down.is_empty() && change.down_management.is_none());
        // The secondary interface is removed: brought down.
        let plan = plan_ip_changes(&m, &h1, false, &pages[..1], false).unwrap();
        let change = plan.change.unwrap();
        assert!(change.up.is_empty());
        assert_eq!(change.down, [Ref::new("OpaqueRef:pif-01-eth1")]);
        // The management interface's address changes: warned.
        let mut mgmt = pages.clone();
        mgmt[0].ip = "192.0.2.12".into();
        let plan = plan_ip_changes(&m, &h1, false, &mgmt, false).unwrap();
        assert!(plan.warn_management);
        let change = plan.change.unwrap();
        assert!(change.management_ip_changed);
        assert!(change.down_management.is_none(), "same PIF: no move");
    }
}
