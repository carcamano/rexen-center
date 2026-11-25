//! Facts derived from cached objects, shared by the tree and the detail tabs.
//!
//! These follow the original console's `XenAPI-Extensions` (`Host.cs`,
//! `VM.cs`, `SR.cs`, `PIF.cs`, `Network.cs`, `VDI.cs`) so the tree and the
//! tabs show the same states and numbers as the original.

use std::cmp::Ordering;
use std::sync::Arc;

use xapi::{
    Host, HostAllowedOperations, HostRecord, Network, NetworkRecord, Pif, PifRecord, Ref, Sr,
    SrRecord, VbdType, VdiRecord, Vm, VmOperations, VmPowerState, VmRecord,
};

use crate::model::ConnectionModel;

const HIDE_FROM_XENCENTER: &str = "HideFromXenCenter";

/// `other_config["HideFromXenCenter"] == "true"`.
fn hidden_flag(other_config: &std::collections::HashMap<String, String>) -> bool {
    other_config.get(HIDE_FROM_XENCENTER).map(String::as_str) == Some("true")
}

/// Natural ("human") ordering: digit runs compare by value, the rest
/// case-insensitively, so `vm-2` sorts before `vm-10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.len() - a.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let db = b.len() - b.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                let (na, nb) = (&a[..da], &b[..db]);
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[x.len_utf8()..];
                b = &b[y.len_utf8()..];
            }
        }
    }
}

// ---- hosts -----------------------------------------------------------------

/// `host_metrics.live`. A host whose metrics are not cached yet counts as
/// live (the original would hide it; showing it is less surprising).
pub fn host_is_live(model: &ConnectionModel, host: &HostRecord) -> bool {
    model.get(&host.metrics).is_none_or(|m| m.live)
}

/// The host is in maintenance mode (or being evacuated): the tree shows the
/// maintenance icon. Like the original, this is `!enabled`; the
/// `MAINTENANCE_MODE` key only changes the General tab's wording.
pub fn host_in_maintenance(host: &HostRecord) -> bool {
    !host.enabled
        || host
            .current_operations
            .values()
            .any(|op| *op == HostAllowedOperations::Evacuate)
}

/// `other_config["MAINTENANCE_MODE"] == "true"`.
pub fn host_maintenance_flag(host: &HostRecord) -> bool {
    host.other_config
        .get("MAINTENANCE_MODE")
        .map(String::as_str)
        == Some("true")
}

/// Whether `host` is the pool coordinator.
pub fn is_coordinator(model: &ConnectionModel, host: &Ref<Host>) -> bool {
    model.pool().is_some_and(|(_, p)| &p.master == host)
}

/// Hosts sorted like the original: coordinator first, then by name.
pub fn sorted_hosts(model: &ConnectionModel) -> Vec<(&Ref<Host>, &Arc<HostRecord>)> {
    let mut hosts: Vec<_> = model.hosts.iter().collect();
    hosts.sort_by(|(ra, a), (rb, b)| {
        is_coordinator(model, rb)
            .cmp(&is_coordinator(model, ra))
            .then_with(|| natural_cmp(&a.name_label, &b.name_label))
            .then_with(|| ra.cmp(rb))
    });
    hosts
}

/// The pool is "visible" (a real pool rather than a standalone host): it has
/// a name or more than one host.
pub fn pool_is_visible(model: &ConnectionModel) -> bool {
    model
        .pool()
        .is_some_and(|(_, p)| !p.name_label.is_empty() || model.hosts.len() > 1)
}

/// The control domain (dom0) of a host.
pub fn control_domain<'a>(model: &'a ConnectionModel, host: &HostRecord) -> Option<&'a VmRecord> {
    if let Some(vm) = model.get(&host.control_domain) {
        return Some(vm);
    }
    host.resident_vms
        .iter()
        .filter_map(|r| model.get(r))
        .find(|vm| vm.is_control_domain && vm.domid == 0)
        .map(|vm| &**vm)
}

/// `memory_actual` of a VM, if its metrics are cached.
pub fn vm_memory_actual(model: &ConnectionModel, vm: &VmRecord) -> Option<i64> {
    model.get(&vm.metrics).map(|m| m.memory_actual)
}

/// Host memory figures as the original computes them (`Host.cs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostMemory {
    /// `host_metrics.memory_total`.
    pub total: i64,
    /// Total minus what Xen, the control domain and the VMs use.
    pub free: i64,
    /// Memory of Xen itself plus the control domains.
    pub xen: i64,
    /// The control domain's current memory.
    pub dom0: i64,
    /// Total minus Xen and the guests' minimum (dynamic-min) memory.
    pub available: i64,
    /// Sum of the guests' dynamic maximum (static maximum without ballooning).
    pub total_dynamic_max: i64,
}

/// Memory figures of `host`, `None` without host metrics.
pub fn host_memory(model: &ConnectionModel, host: &HostRecord) -> Option<HostMemory> {
    let metrics = model.get(&host.metrics)?;
    let total = metrics.memory_total;
    let resident: Vec<&VmRecord> = host
        .resident_vms
        .iter()
        .filter_map(|r| model.get(r))
        .map(|v| &**v)
        .collect();
    let used: i64 = host.memory_overhead
        + resident
            .iter()
            .map(|vm| vm.memory_overhead + vm_memory_actual(model, vm).unwrap_or(0))
            .sum::<i64>();
    let xen = host.memory_overhead
        + resident.iter().map(|vm| vm.memory_overhead).sum::<i64>()
        + resident
            .iter()
            .filter(|vm| vm.is_control_domain)
            .map(|vm| vm_memory_actual(model, vm).unwrap_or(0))
            .sum::<i64>();
    let guests = || resident.iter().filter(|vm| !vm.is_control_domain);
    let dyn_min: i64 = guests()
        .map(|vm| {
            if vm_supports_ballooning(model, vm) {
                vm.memory_dynamic_min
            } else {
                vm.memory_static_max
            }
        })
        .sum();
    let dyn_max: i64 = guests()
        .map(|vm| {
            if vm_supports_ballooning(model, vm) {
                vm.memory_dynamic_max
            } else {
                vm.memory_static_max
            }
        })
        .sum();
    let dom0 = control_domain(model, host)
        .map(|vm| vm_memory_actual(model, vm).unwrap_or(vm.memory_dynamic_min))
        .unwrap_or(0);
    Some(HostMemory {
        total,
        free: (total - used).max(0),
        xen,
        dom0,
        available: (total - dyn_min - xen).max(0),
        total_dynamic_max: dyn_max,
    })
}

// ---- VMs -------------------------------------------------------------------

/// What kind of VM record this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VmKind {
    ControlDomain,
    Guest,
    Snapshot,
    UserTemplate,
    DefaultTemplate,
}

/// Classifies a VM record.
pub fn vm_kind(vm: &VmRecord) -> VmKind {
    if vm.is_control_domain {
        VmKind::ControlDomain
    } else if vm.is_a_snapshot {
        VmKind::Snapshot
    } else if vm.is_a_template {
        if vm.is_default_template || vm.other_config.contains_key("default_template") {
            VmKind::DefaultTemplate
        } else {
            VmKind::UserTemplate
        }
    } else {
        VmKind::Guest
    }
}

/// `VM.Show(showHidden)`: internal templates and `__gui__` objects are never
/// shown; `HideFromXenCenter` ones only with hidden objects on.
pub fn vm_is_shown(vm: &VmRecord, show_hidden: bool) -> bool {
    if vm.name_label.starts_with("__gui__") || vm.other_config.contains_key("xensource_internal") {
        return false;
    }
    show_hidden || !hidden_flag(&vm.other_config)
}

/// The icon state of a VM in the tree and the panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmIcon {
    Running,
    Halted,
    Suspended,
    Paused,
    /// An operation is in progress (starting, shutting down, migrating…).
    Busy,
    Snapshot {
        with_memory: bool,
    },
    UserTemplate,
    DefaultTemplate,
}

/// Icon state of a VM (`Images.GetIconFor(VM)`).
pub fn vm_icon(vm: &VmRecord) -> VmIcon {
    match vm_kind(vm) {
        VmKind::Snapshot => {
            return VmIcon::Snapshot {
                with_memory: vm.power_state == VmPowerState::Suspended,
            };
        }
        VmKind::DefaultTemplate => return VmIcon::DefaultTemplate,
        VmKind::UserTemplate => return VmIcon::UserTemplate,
        _ => {}
    }
    let busy = vm.current_operations.values().any(|op| {
        !matches!(
            op,
            VmOperations::ChangingDynamicRange
                | VmOperations::ChangingStaticRange
                | VmOperations::ChangingMemoryLimits
        )
    });
    if busy {
        return VmIcon::Busy;
    }
    match vm.power_state {
        VmPowerState::Running => VmIcon::Running,
        VmPowerState::Suspended => VmIcon::Suspended,
        VmPowerState::Paused => VmIcon::Paused,
        _ => VmIcon::Halted,
    }
}

/// `VM.Home()`: the host a VM belongs to in the tree. A running or paused VM
/// belongs to the host it runs on; otherwise to the host of its first disk
/// on local storage, otherwise to its home server (`affinity`) if that host
/// is live. Templates and snapshots have no home.
pub fn vm_home<'a>(model: &'a ConnectionModel, vm: &'a VmRecord) -> Option<&'a Ref<Host>> {
    if vm.is_a_template || vm.is_a_snapshot {
        return None;
    }
    let running = matches!(vm.power_state, VmPowerState::Running | VmPowerState::Paused);
    if running {
        return model
            .hosts
            .contains_key(&vm.resident_on)
            .then_some(&vm.resident_on);
    }
    for vbd in vm.vbds.iter().filter_map(|r| model.get(r)) {
        if vbd.r#type == VbdType::Cd {
            continue;
        }
        if let Some(sr) = model.get(&vbd.vdi).and_then(|vdi| model.get(&vdi.sr))
            && is_local_sr(sr)
            && let Some(host) = sr_home(model, sr)
        {
            return Some(host);
        }
    }
    vm.affinity.non_null().filter(|h| {
        model
            .hosts
            .get(*h)
            .is_some_and(|rec| host_is_live(model, rec))
    })
}

/// `VM.SupportsBallooning`: a template when its dynamic minimum differs from
/// its static maximum; another VM when its guest agent reports
/// `feature-balloon`.
pub fn vm_supports_ballooning(model: &ConnectionModel, vm: &VmRecord) -> bool {
    if vm.is_a_template {
        return vm.memory_dynamic_min != vm.memory_static_max;
    }
    model
        .get(&vm.guest_metrics)
        .is_some_and(|gm| gm.other.contains_key("feature-balloon"))
}

/// `VM.IsHVM`.
pub fn vm_is_hvm(vm: &VmRecord) -> bool {
    !vm.hvm_boot_policy.is_empty()
}

/// Operating system name reported by the guest agent (`os_version["name"]`
/// up to the first `|`, without a leading "Microsoft"), if any.
pub fn vm_os_name(model: &ConnectionModel, vm: &VmRecord) -> Option<String> {
    let gm = model.get(&vm.guest_metrics)?;
    let name = gm.os_version.get("name")?;
    let name = name.split('|').next().unwrap_or_default();
    let name = name
        .strip_prefix("Microsoft®")
        .or_else(|| name.strip_prefix("Microsoft"))
        .unwrap_or(name)
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// IP addresses the guest agent reports for VIF `device` (`0/ip`,
/// `0/ipv4/0`, `0/ipv6/1`…), in key order, de-duplicated.
pub fn vif_ip_addresses(model: &ConnectionModel, vm: &VmRecord, device: &str) -> Vec<String> {
    let Some(gm) = model.get(&vm.guest_metrics) else {
        return Vec::new();
    };
    let prefix = format!("{device}/ip");
    let mut keys: Vec<&String> = gm
        .networks
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .collect();
    keys.sort();
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        for ip in gm.networks[key].split(['\n']).flat_map(|s| s.split("%n")) {
            let ip = ip.trim();
            if !ip.is_empty() && !out.iter().any(|o| o == ip) {
                out.push(ip.to_owned());
            }
        }
    }
    out
}

/// VM ordering of the original: control domain, guests, snapshots, user
/// templates, default templates; each by name.
pub fn vm_cmp(a: (&Ref<Vm>, &VmRecord), b: (&Ref<Vm>, &VmRecord)) -> Ordering {
    vm_kind(a.1)
        .cmp(&vm_kind(b.1))
        .then_with(|| natural_cmp(&a.1.name_label, &b.1.name_label))
        .then_with(|| a.0.cmp(b.0))
}

// ---- storage ---------------------------------------------------------------

/// SR types whose storage is local to one host (`SR.IsLocalSR`).
pub fn is_local_sr(sr: &SrRecord) -> bool {
    matches!(
        sr.r#type.as_str(),
        "local" | "file" | "ext" | "lvm" | "udev" | "egeneracd" | "dummy" | "ext4" | "xfs"
    )
}

/// The tools SR (guest tools ISO library).
pub fn is_tools_sr(sr: &SrRecord) -> bool {
    sr.is_tools_sr || sr.name_label == "XenServer Tools"
}

/// `SR.Show(showHidden)`.
pub fn sr_is_shown(sr: &SrRecord, show_hidden: bool) -> bool {
    if sr.name_label.starts_with("__gui__") || matches!(sr.r#type.as_str(), "local" | "tmpfs") {
        return false;
    }
    show_hidden || (sr.introduced_by.is_null() && !hidden_flag(&sr.other_config))
}

/// `SR.Home()`: the host of an unshared SR with exactly one PBD.
pub fn sr_home<'a>(model: &'a ConnectionModel, sr: &SrRecord) -> Option<&'a Ref<Host>> {
    if sr.shared || sr.pbds.len() != 1 {
        return None;
    }
    let pbd = model.get(&sr.pbds[0])?;
    model.hosts.get_key_value(&pbd.host).map(|(k, _)| k)
}

/// No PBD of the SR is plugged.
pub fn sr_is_detached(model: &ConnectionModel, sr: &SrRecord) -> bool {
    !sr.pbds
        .iter()
        .filter_map(|r| model.get(r))
        .any(|p| p.currently_attached)
}

/// `SR.IsBroken`: a PBD is missing or unplugged.
pub fn sr_is_broken(model: &ConnectionModel, sr: &SrRecord) -> bool {
    if sr.pbds.is_empty() {
        return true;
    }
    let in_pool = pool_is_visible(model);
    if (!in_pool || !sr.shared) && sr.pbds.len() != 1 {
        return true;
    }
    if in_pool && sr.shared && sr.pbds.len() != model.hosts.len() {
        return true;
    }
    sr.pbds
        .iter()
        .any(|r| model.get(r).is_none_or(|p| !p.currently_attached))
}

/// Paths of a multipathed PBD (`other_config["mpath-<scsi id>"]` =
/// `"[current, max]"`), if reported.
pub fn mpath_paths(value: &str) -> Option<(i64, i64)> {
    let inner = value.trim().strip_prefix('[')?;
    let mut parts = inner.split(',');
    let cur = parts.next()?.trim().trim_end_matches('L').parse().ok()?;
    let max = parts
        .next()?
        .trim()
        .trim_end_matches([']', 'L'])
        .trim_end_matches('L')
        .parse()
        .ok()?;
    Some((cur, max))
}

/// `SR.MultipathAOK`: every active multipath PBD has all its paths.
pub fn sr_multipath_ok(model: &ConnectionModel, sr: &SrRecord) -> bool {
    sr.pbds.iter().filter_map(|r| model.get(r)).all(|pbd| {
        if pbd.other_config.get("multipathed").map(String::as_str) != Some("true") {
            return true;
        }
        pbd.other_config
            .iter()
            .filter(|(k, _)| k.starts_with("mpath-"))
            .filter_map(|(_, v)| mpath_paths(v))
            .all(|(cur, max)| cur >= max)
    })
}

/// Icon state of an SR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrIcon {
    Normal,
    /// The pool's default SR.
    Default,
    /// Detached, broken (a PBD missing or unplugged) or a multipath failure.
    Broken,
    /// No PBDs at all, or hidden.
    Disabled,
}

/// Icon state of an SR (`Images.GetIconFor(SR)`).
pub fn sr_icon(model: &ConnectionModel, sr_ref: &Ref<Sr>, sr: &SrRecord) -> SrIcon {
    if sr.pbds.is_empty() || hidden_flag(&sr.other_config) {
        SrIcon::Disabled
    } else if sr_is_detached(model, sr) || sr_is_broken(model, sr) || !sr_multipath_ok(model, sr) {
        SrIcon::Broken
    } else if model.pool().is_some_and(|(_, p)| &p.default_sr == sr_ref) {
        SrIcon::Default
    } else {
        SrIcon::Normal
    }
}

/// `VDI.Show(showHidden)`.
pub fn vdi_is_shown(vdi: &VdiRecord, show_hidden: bool) -> bool {
    !vdi.missing && vdi.managed && (show_hidden || !hidden_flag(&vdi.other_config))
}

/// `VDI.Name()`: a physical CD drive reads "DVD drive on" plus its host.
pub fn vdi_name(model: &ConnectionModel, vdi: &VdiRecord) -> String {
    if let Some(sr) = model.get(&vdi.sr) {
        let physical = sr.r#type == "local"
            || (sr.r#type == "udev" && sr.sm_config.get("type").map(String::as_str) == Some("cd"));
        if physical
            && sr.pbds.len() == 1
            && let Some(host) = model.get(&sr.pbds[0]).and_then(|pbd| model.get(&pbd.host))
        {
            return format!("DVD drive on {}", host.name_label);
        }
    }
    vdi.name_label.clone()
}

// ---- networking ------------------------------------------------------------

fn nic_number(device: &str) -> &str {
    device.strip_prefix("eth").unwrap_or(device)
}

/// `PIF.Name()`: "NIC 0", "Bond 0+1"; a VLAN, tunnel or SR-IOV PIF takes
/// the name of the PIF it sits on.
pub fn pif_name(model: &ConnectionModel, pif: &PifRecord) -> String {
    if pif.physical || !pif.bond_master_of.is_empty() {
        if let Some(bond) = pif.bond_master_of.first().and_then(|b| model.get(b)) {
            let mut members: Vec<&str> = bond
                .slaves
                .iter()
                .filter_map(|s| model.get(s))
                .map(|s| nic_number(&s.device))
                .collect();
            members.sort_by(|a, b| natural_cmp(a, b));
            return format!("Bond {}", members.join("+"));
        }
        return format!("NIC {}", nic_number(&pif.device));
    }
    if let Some(tagged) = model
        .get(&pif.vlan_master_of)
        .and_then(|vlan| model.get(&vlan.tagged_pif))
    {
        return pif_name(model, tagged);
    }
    if let Some(transport) = pif
        .tunnel_access_pif_of
        .first()
        .and_then(|t| model.get(t))
        .and_then(|t| model.get(&t.transport_pif))
    {
        return pif_name(model, transport);
    }
    if let Some(physical) = pif
        .sriov_logical_pif_of
        .first()
        .and_then(|s| model.get(s))
        .and_then(|s| model.get(&s.physical_pif))
    {
        return pif_name(model, physical);
    }
    format!("NIC {}", nic_number(&pif.device))
}

/// `PIF.IsPhysical()`: not a VLAN, tunnel or SR-IOV logical PIF (bond
/// masters count).
pub fn pif_is_physical(pif: &PifRecord) -> bool {
    pif.vlan == -1 && pif.tunnel_access_pif_of.is_empty() && pif.sriov_logical_pif_of.is_empty()
}

/// Link state of a PIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    Connected,
    Disconnected,
    Unknown,
}

/// `PIF.LinkStatus()`.
pub fn pif_link_status(model: &ConnectionModel, pif: &PifRecord) -> LinkStatus {
    if let Some(tunnel) = pif.tunnel_access_pif_of.first().and_then(|t| model.get(t)) {
        return if tunnel.status.get("active").map(String::as_str) == Some("true") {
            LinkStatus::Connected
        } else {
            LinkStatus::Disconnected
        };
    }
    match model.get(&pif.metrics) {
        Some(m) if m.carrier => LinkStatus::Connected,
        Some(_) => LinkStatus::Disconnected,
        None => LinkStatus::Unknown,
    }
}

/// Aggregate link state of a network over all its PIFs (pool view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkLinkStatus {
    None,
    Connected,
    PartiallyConnected,
    Disconnected,
    Unknown,
}

/// `Network.LinkStatus` over every PIF of the network.
pub fn network_link_status(model: &ConnectionModel, network: &NetworkRecord) -> NetworkLinkStatus {
    let states: Vec<LinkStatus> = network
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .map(|p| pif_link_status(model, p))
        .collect();
    if states.is_empty() {
        return NetworkLinkStatus::None;
    }
    let connected = states
        .iter()
        .filter(|s| **s == LinkStatus::Connected)
        .count();
    let unknown = states.contains(&LinkStatus::Unknown);
    if connected == states.len() {
        NetworkLinkStatus::Connected
    } else if connected > 0 {
        NetworkLinkStatus::PartiallyConnected
    } else if unknown {
        NetworkLinkStatus::Unknown
    } else {
        NetworkLinkStatus::Disconnected
    }
}

/// The guest installer / host-internal management network.
pub fn is_guest_installer_network(network: &NetworkRecord) -> bool {
    network
        .other_config
        .get("is_guest_installer_network")
        .map(String::as_str)
        == Some("true")
}

/// A network made of bond members (shown greyed, hidden by default).
pub fn is_bond_member_network(model: &ConnectionModel, network: &NetworkRecord) -> bool {
    network
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .any(|p| !p.bond_slave_of.is_null())
}

/// `Network.Show(showHidden)`.
pub fn network_is_shown(
    model: &ConnectionModel,
    network: &NetworkRecord,
    show_hidden: bool,
) -> bool {
    if is_guest_installer_network(network) && !show_hidden {
        return false;
    }
    let pifs_shown = network
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .all(|p| p.managed && (show_hidden || !hidden_flag(&p.other_config)));
    if !pifs_shown {
        return false;
    }
    show_hidden || (!is_bond_member_network(model, network) && !hidden_flag(&network.other_config))
}

/// `Network.Name()`: automatic names ("Pool-wide network associated with
/// eth0") become "Network 0" / "Network 0 on VLAN 20".
pub fn network_name(model: &ConnectionModel, network: &NetworkRecord) -> String {
    let label = &network.name_label;
    let automatic = label.starts_with("Network associated with ")
        || label.starts_with("Pool-wide network associated with ");
    if !automatic {
        return label.clone();
    }
    let coordinator = model.pool().map(|(_, p)| p.master.clone());
    let pifs: Vec<&PifRecord> = network
        .pifs
        .iter()
        .filter_map(|p| model.get(p))
        .map(|p| &**p)
        .collect();
    let pif = pifs
        .iter()
        .find(|p| Some(&p.host) == coordinator.as_ref())
        .or_else(|| pifs.first());
    match pif {
        Some(pif) => {
            let nic = pif_name(model, pif);
            let nic = nic
                .strip_prefix("NIC ")
                .or_else(|| nic.strip_prefix("Bond "))
                .unwrap_or(&nic)
                .to_owned();
            if pif.vlan >= 0 {
                format!("Network {nic} on VLAN {}", pif.vlan)
            } else {
                format!("Network {nic}")
            }
        }
        None => label.clone(),
    }
}

/// The PIF of `network` on `host`, or the network's first PIF when no host
/// is given (pool view).
pub fn network_pif<'a>(
    model: &'a ConnectionModel,
    network: &NetworkRecord,
    host: Option<&Ref<Host>>,
) -> Option<(&'a Ref<Pif>, &'a Arc<PifRecord>)> {
    network
        .pifs
        .iter()
        .filter_map(|r| model.pifs.get_key_value(r))
        .find(|(_, p)| host.is_none_or(|h| &p.host == h))
}

/// `PIF.FriendlyIPAddress()`: the IP, "DHCP" when configured by DHCP and not
/// known yet, otherwise `None` ("Unknown").
pub fn pif_friendly_ip(pif: &PifRecord) -> Option<String> {
    if !pif.ip.is_empty() {
        Some(pif.ip.clone())
    } else if pif.ip_configuration_mode == xapi::IpConfigurationMode::Dhcp {
        Some("DHCP".to_owned())
    } else {
        None
    }
}

/// A PIF used as a secondary management interface (storage, …).
pub fn pif_is_secondary_management(
    model: &ConnectionModel,
    pif: &PifRecord,
    show_hidden: bool,
) -> bool {
    !pif.management
        && matches!(
            pif.ip_configuration_mode,
            xapi::IpConfigurationMode::Dhcp | xapi::IpConfigurationMode::Static
        )
        && model
            .get(&pif.network)
            .is_some_and(|n| network_is_shown(model, n, show_hidden))
}

/// Networks of the connection the tabs list: shown ones, ordered by name.
pub fn shown_networks(
    model: &ConnectionModel,
    show_hidden: bool,
) -> Vec<(&Ref<Network>, &Arc<NetworkRecord>)> {
    let mut networks: Vec<_> = model
        .networks
        .iter()
        .filter(|(_, n)| network_is_shown(model, n, show_hidden))
        .collect();
    networks.sort_by(|(ra, a), (rb, b)| {
        natural_cmp(&network_name(model, a), &network_name(model, b)).then_with(|| ra.cmp(rb))
    });
    networks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    fn vm<'a>(m: &'a ConnectionModel, r: &str) -> &'a VmRecord {
        &m.vms[&Ref::new(r)]
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        let mut v = vec!["vm-10", "VM-2", "vm-1", "alpha"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["alpha", "vm-1", "VM-2", "vm-10"]);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Equal);
    }

    #[test]
    fn vm_kinds_icons_and_homes() {
        let m = fixture();
        assert_eq!(
            vm_kind(vm(&m, "OpaqueRef:vm-dom0-01")),
            VmKind::ControlDomain
        );
        assert_eq!(
            vm_kind(vm(&m, "OpaqueRef:vm-template-debian")),
            VmKind::DefaultTemplate
        );
        assert_eq!(
            vm_kind(vm(&m, "OpaqueRef:vm-template-user")),
            VmKind::UserTemplate
        );
        assert_eq!(vm_kind(vm(&m, "OpaqueRef:vm-web-snap")), VmKind::Snapshot);
        assert_eq!(vm_icon(vm(&m, "OpaqueRef:vm-web")), VmIcon::Running);
        assert_eq!(vm_icon(vm(&m, "OpaqueRef:vm-legacy")), VmIcon::Suspended);
        assert_eq!(
            vm_icon(vm(&m, "OpaqueRef:vm-web-snap")),
            VmIcon::Snapshot { with_memory: false }
        );
        let mut busy = vm(&m, "OpaqueRef:vm-db").clone();
        busy.current_operations
            .insert("task".into(), VmOperations::Start);
        assert_eq!(vm_icon(&busy), VmIcon::Busy);
        busy.current_operations
            .insert("task".into(), VmOperations::ChangingDynamicRange);
        assert_eq!(vm_icon(&busy), VmIcon::Halted);

        // Running: where it runs. Suspended with a local disk: that disk's
        // host. Halted on shared storage: its live home server. None: pool.
        let home = |r: &str| vm_home(&m, vm(&m, r)).map(|h| h.as_str().to_owned());
        assert_eq!(
            home("OpaqueRef:vm-web").as_deref(),
            Some("OpaqueRef:host-0001")
        );
        assert_eq!(
            home("OpaqueRef:vm-legacy").as_deref(),
            Some("OpaqueRef:host-0001")
        );
        assert_eq!(
            home("OpaqueRef:vm-db").as_deref(),
            Some("OpaqueRef:host-0002")
        );
        assert_eq!(home("OpaqueRef:vm-build"), None);
        assert_eq!(home("OpaqueRef:vm-template-debian"), None);
    }

    #[test]
    fn host_memory_follows_the_original_formulas() {
        let m = fixture();
        let host = &m.hosts[&Ref::new("OpaqueRef:host-0001")];
        let mem = host_memory(&m, host).unwrap();
        let gib = 1i64 << 30;
        assert_eq!(mem.total, 32 * gib);
        // dom0 2 GiB + web-01 2 GiB actual, overheads 64 MiB host + 32 MiB
        // dom0 + 32 MiB web-01.
        let mib = 1i64 << 20;
        assert_eq!(mem.xen, 64 * mib + 32 * mib + 32 * mib + 2 * gib);
        assert_eq!(
            mem.free,
            32 * gib - (64 * mib + 32 * mib + 2 * gib + 32 * mib + 2 * gib)
        );
        assert_eq!(mem.dom0, 2 * gib);
        // web-01 balloons: dynamic min 1 GiB, dynamic max 2 GiB.
        assert_eq!(mem.available, 32 * gib - gib - mem.xen);
        assert_eq!(mem.total_dynamic_max, 2 * gib);
        assert!(host_in_maintenance(
            &m.hosts[&Ref::new("OpaqueRef:host-0002")]
        ));
        assert!(!host_in_maintenance(host));
    }

    #[test]
    fn storage_states() {
        let m = fixture();
        let sr = |r: &str| (Ref::<Sr>::new(r), m.srs[&Ref::new(r)].clone());
        let (r, nfs) = sr("OpaqueRef:sr-nfs");
        assert_eq!(sr_icon(&m, &r, &nfs), SrIcon::Default);
        let (r, iso) = sr("OpaqueRef:sr-iso");
        assert!(sr_is_broken(&m, &iso));
        assert!(!sr_is_detached(&m, &iso));
        assert_eq!(sr_icon(&m, &r, &iso), SrIcon::Broken);
        let (r, local) = sr("OpaqueRef:sr-local-01");
        assert_eq!(sr_icon(&m, &r, &local), SrIcon::Normal);
        assert!(is_local_sr(&local));
        assert_eq!(
            sr_home(&m, &local).map(Ref::as_str),
            Some("OpaqueRef:host-0001")
        );
        assert_eq!(sr_home(&m, &nfs), None);
        assert_eq!(mpath_paths("[2, 4]"), Some((2, 4)));
        assert_eq!(mpath_paths("[1L, 2L]"), Some((1, 2)));
        assert_eq!(mpath_paths("nonsense"), None);
    }

    #[test]
    fn networking_names_and_links() {
        let m = fixture();
        let net = |r: &str| m.networks[&Ref::new(r)].clone();
        assert_eq!(network_name(&m, &net("OpaqueRef:net-eth0")), "Network 0");
        assert_eq!(
            network_name(&m, &net("OpaqueRef:net-vlan20")),
            "Guests VLAN 20"
        );
        assert!(!network_is_shown(&m, &net("OpaqueRef:net-himn"), false));
        assert!(network_is_shown(&m, &net("OpaqueRef:net-himn"), true));
        assert_eq!(
            network_link_status(&m, &net("OpaqueRef:net-eth1")),
            NetworkLinkStatus::PartiallyConnected
        );
        let vlan_pif = &m.pifs[&Ref::new("OpaqueRef:pif-01-vlan20")];
        assert_eq!(pif_name(&m, vlan_pif), "NIC 1");
        assert!(!pif_is_physical(vlan_pif));
        let web = vm(&m, "OpaqueRef:vm-web");
        assert_eq!(
            vif_ip_addresses(&m, web, "0"),
            ["192.0.2.101", "2001:db8::101"]
        );
        assert_eq!(
            vm_os_name(&m, web).as_deref(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
    }
}
