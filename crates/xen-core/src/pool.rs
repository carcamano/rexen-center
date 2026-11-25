//! Pools: creating one, adding and removing servers, making a pool of one
//! into a standalone server, and when each is offered.
//!
//! The rules follow the original console:
//!
//! * [`can_join_pool`] is `PoolJoinRules.CanJoinPool`: whether the server
//!   of one connection can join the pool of another, or why not
//!   ([`JoinReason`], worded by the UI as `PoolJoinRules.ReasonMessage`).
//!   The New Pool dialog lists every connected server with its reason
//!   (`ConnectionWrapperWithMoreStuff`), and Add Server refuses the
//!   servers that are not [`JoinReason::Allowed`] (`AddHostToPoolCommand`).
//! * [`join_warnings`]: the questions asked before a join (licensing, CPU
//!   masking, Active Directory).
//! * [`remove_problem`]: `RemoveHostFromPoolCommand.CanRun` and its
//!   reasons; [`standalone_problem`]: `DeletePoolCommand` (Make into
//!   standalone server).
//! * [`pool_join_address`]: the coordinator address given to `pool.join`.
//!
//! Version checks use the coordinator's `software_version`, as the
//! original's `Helpers.*OrGreater`.

use std::cmp::Ordering;
use std::collections::HashMap;

use xapi::{Host, HostRecord, PoolAllowedOperations, PoolRecord, Ref, VmPowerState};

use crate::backend::ConnectionView;
use crate::model::{ConnectionModel, is_guest};
use crate::objects;
use crate::rbac::{self, RbacCheck};

// ---- versions ---------------------------------------------------------------

/// `Helpers.ProductVersionCompare` for dotted numeric versions ("3.2.50",
/// "22.33.0"); a missing version sorts first.
pub fn version_cmp(a: Option<&str>, b: Option<&str>) -> Ordering {
    fn parse(v: &str) -> Vec<u64> {
        v.split(['.', '-'])
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => {
            let (a, b) = (parse(a), parse(b));
            for i in 0..a.len().max(b.len()) {
                let o = a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0));
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        }
    }
}

/// `Host.PlatformVersion`.
pub fn platform_version(host: &HostRecord) -> Option<&str> {
    host.software_version
        .get("platform_version")
        .map(String::as_str)
}

/// `Host.GetXapiVersion`.
pub fn xapi_version(host: &HostRecord) -> Option<&str> {
    host.software_version
        .get("xapi_build")
        .or_else(|| host.software_version.get("xapi"))
        .map(String::as_str)
}

fn platform_at_least(host: Option<&HostRecord>, version: &str) -> bool {
    // No coordinator yet counts as new enough, as the original's helpers.
    host.is_none_or(|h| version_cmp(platform_version(h), Some(version)) != Ordering::Less)
}

fn xapi_at_least(host: Option<&HostRecord>, version: &str) -> bool {
    host.is_none_or(|h| version_cmp(xapi_version(h), Some(version)) != Ordering::Less)
}

/// `Helpers.StockholmOrGreater` (Citrix Hypervisor 8.2 / XCP-ng 8.2).
pub fn stockholm_or_greater(host: Option<&HostRecord>) -> bool {
    platform_at_least(host, "3.1.50")
}

/// `Helpers.CloudOrGreater` (XenServer 8 / XCP-ng 8.3).
pub fn cloud_or_greater(host: Option<&HostRecord>) -> bool {
    platform_at_least(host, "3.2.50")
}

/// `Helpers.XapiEqualOrGreater_1_290_0`.
pub fn xapi_1_290_or_greater(host: Option<&HostRecord>) -> bool {
    xapi_at_least(host, "1.290.0")
}

/// `Helpers.XapiEqualOrGreater_22_33_0`.
pub fn xapi_22_33_or_greater(host: Option<&HostRecord>) -> bool {
    xapi_at_least(host, "22.33.0")
}

/// `Helpers.XapiEqualOrGreater_23_27_0`.
pub fn xapi_23_27_or_greater(host: Option<&HostRecord>) -> bool {
    xapi_at_least(host, "23.27.0")
}

// ---- licence restrictions ---------------------------------------------------

/// `BoolKeyPreferTrue`: restricted unless the key says "false".
fn restricted_prefer_true(host: &HostRecord, key: &str) -> bool {
    host.license_params
        .get(key)
        .is_none_or(|v| !v.eq_ignore_ascii_case("false"))
}

/// `BoolKey`: restricted only when the key says "true".
fn restricted(host: &HostRecord, key: &str) -> bool {
    host.license_params
        .get(key)
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// The licence restrictions the pool rules and pages look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restriction {
    Pooling,
    PoolSize,
    ManagementOnVlan,
    PoolSecretRotation,
    CertificateVerification,
    Corosync,
    IgmpSnooping,
    LivePatching,
    VswitchController,
}

/// Whether `host`'s licence forbids `what` (`Host.Restrict*`).
pub fn host_restricts(host: &HostRecord, what: Restriction) -> bool {
    match what {
        Restriction::Pooling => restricted_prefer_true(host, "restrict_pooling"),
        Restriction::PoolSize => restricted(host, "restrict_pool_size"),
        Restriction::ManagementOnVlan => {
            restricted_prefer_true(host, "restrict_management_on_vlan")
        }
        Restriction::PoolSecretRotation => {
            restricted_prefer_true(host, "restrict_pool_secret_rotation")
        }
        Restriction::CertificateVerification => {
            restricted_prefer_true(host, "restrict_certificate_verification")
        }
        Restriction::Corosync => restricted_prefer_true(host, "restrict_corosync"),
        Restriction::IgmpSnooping => restricted_prefer_true(host, "restrict_igmp_snooping"),
        Restriction::LivePatching => restricted_prefer_true(host, "restrict_live_patching"),
        Restriction::VswitchController => {
            restricted_prefer_true(host, "restrict_vswitch_controller")
        }
    }
}

/// `Helpers.FeatureForbidden`: some host of the connection forbids it.
pub fn feature_forbidden(model: &ConnectionModel, what: Restriction) -> bool {
    model.hosts.values().any(|h| host_restricts(h, what))
}

/// `Host.IsFreeLicense`.
fn is_free_license(host: &HostRecord) -> bool {
    matches!(host.edition.as_str(), "free" | "express" | "trial")
}

/// `Host.GetEdition`, folded to what the rules compare: every unknown
/// edition (XCP-ng's included) is "free".
fn edition_kind(edition: &str) -> &str {
    match edition {
        "xendesktop"
        | "per-socket"
        | "enterprise-per-socket"
        | "premium-per-socket"
        | "enterprise-per-user"
        | "premium-per-user"
        | "standard-per-socket"
        | "desktop"
        | "desktop-plus"
        | "desktop-cloud"
        | "premium"
        | "standard" => edition,
        _ => "free",
    }
}

// ---- pool join --------------------------------------------------------------

/// `PoolJoinRules.Reason`, in the original's order (the New Pool dialog
/// sorts its list by it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JoinReason {
    WillBeCoordinator,
    Allowed,
    Connecting,
    CoordinatorNotConnected,
    CoordinatorConnecting,
    DifferentAdConfig,
    HasRunningVms,
    HasSharedStorage,
    IsAPool,
    LicenseRestriction,
    NotSameLinuxPack,
    LicensedHostUnlicensedCoordinator,
    UnlicensedHostLicensedCoordinator,
    LicenseMismatch,
    CoordinatorPoolMaxNumberHostReached,
    WillExceedPoolMaxSize,
    DifferentServerVersion,
    DifferentHomogeneousUpdatesFromCoordinator,
    DifferentHomogeneousUpdatesFromPool,
    DifferentCpus,
    DifferentNetworkBackends,
    CoordinatorHasHa,
    NotPhysicalPif,
    NonCompatibleManagementInterface,
    WrongRoleOnCoordinator,
    WrongRoleOnSupporter,
    HasClusteringEnabled,
    WrongNumberOfIpsCluster,
    WrongNumberOfIpsBond,
    NotConnected,
    TlsVerificationOnlyOnPool,
    TlsVerificationOnlyOnPoolJoiner,
    TlsVerificationOnlyOnCoordinator,
    TlsVerificationOnlyOnCoordinatorJoiner,
}

impl JoinReason {
    /// `ConnectionWrapperWithMoreStuff.CanBeCoordinator`.
    pub fn can_be_coordinator(self) -> bool {
        !matches!(
            self,
            Self::Connecting | Self::NotConnected | Self::LicenseRestriction | Self::IsAPool
        )
    }
}

/// `Helpers.GetPool`: the pool, unless it is a pool of one.
pub fn visible_pool(model: &ConnectionModel) -> Option<(&Ref<xapi::Pool>, &PoolRecord)> {
    objects::pool_is_visible(model)
        .then(|| model.pool().map(|(r, p)| (r, &**p)))
        .flatten()
}

fn may_join(conn: &ConnectionView) -> bool {
    matches!(
        rbac::check(&conn.model, conn.rbac.as_deref(), &["pool.join"]),
        RbacCheck::Allowed
    )
}

/// `PoolJoinRules.CompatibleCPUs`.
pub fn compatible_cpus(
    supporter: &ConnectionModel,
    s: &HostRecord,
    coordinator: &ConnectionModel,
    c: &HostRecord,
) -> bool {
    if !s.cpu_info.is_empty() && !c.cpu_info.is_empty() {
        return s.cpu_info.get("vendor") == c.cpu_info.get("vendor");
    }
    supporter.host_cpus.values().all(|a| {
        coordinator.host_cpus.values().all(|b| {
            a.vendor == b.vendor && a.family == b.family && a.model == b.model && a.flags == b.flags
        })
    })
}

fn different_server_version(s: &HostRecord, c: &HostRecord) -> bool {
    let schema = |h: &HostRecord| h.software_version.get("db_schema").cloned();
    if s.api_version_major != c.api_version_major || s.api_version_minor != c.api_version_minor {
        return true;
    }
    if schema(s).is_none_or(|v| v.is_empty()) || schema(c).is_none_or(|v| v.is_empty()) {
        return true;
    }
    if schema(s) != schema(c) {
        return true;
    }
    platform_version(s) != platform_version(c)
        || s.software_version.get("product_brand") != c.software_version.get("product_brand")
}

fn homogeneous_updates(model: &ConnectionModel, host: &HostRecord) -> Vec<String> {
    let mut v: Vec<String> = host
        .updates
        .iter()
        .filter_map(|u| model.pool_updates.get(u))
        .filter(|u| u.enforce_homogeneity)
        .map(|u| u.uuid.clone())
        .collect();
    v.sort();
    v
}

/// `PoolJoinRules.CompatibleAdConfig`.
pub fn compatible_ad_config(s: &HostRecord, c: &HostRecord, allow_supporter_config: bool) -> bool {
    if s.external_auth_type != c.external_auth_type
        || s.external_auth_service_name != c.external_auth_service_name
    {
        return s.external_auth_type.is_empty() && allow_supporter_config;
    }
    true
}

/// `PoolJoinRules.FreeHostPaidCoordinator`.
pub fn free_host_paid_coordinator(s: &HostRecord, c: &HostRecord, allow_upgrade: bool) -> bool {
    is_free_license(s) && !is_free_license(c) && !allow_upgrade
}

fn has_shared_storage(model: &ConnectionModel) -> bool {
    model
        .srs
        .values()
        .any(|sr| sr.shared && !objects::is_tools_sr(sr))
}

fn has_running_vms(model: &ConnectionModel) -> bool {
    model
        .vms
        .values()
        .any(|vm| is_guest(vm) && vm.power_state == VmPowerState::Running)
}

/// `PoolJoinRules.HasCompatibleManagementInterface`.
fn compatible_management_interface(model: &ConnectionModel) -> bool {
    let non_physical: Vec<_> = model
        .pifs
        .values()
        .filter(|p| !objects::pif_is_physical(p))
        .collect();
    match non_physical.len() {
        0 => true,
        1 => non_physical[0].management && non_physical[0].vlan != -1,
        _ => false,
    }
}

/// `PoolJoinRules.HasIpForClusterNetwork`: `Err(in_bond)` when the joining
/// server lacks exactly one IP address on the pool's cluster network.
fn ip_for_cluster_network(
    coordinator: &ConnectionModel,
    supporter: &ConnectionModel,
    s: &HostRecord,
) -> Result<(), bool> {
    let Some(cluster_host) = coordinator.cluster_hosts.values().next() else {
        return Ok(());
    };
    let Some(mut pif) = coordinator.pifs.get(&cluster_host.pif) else {
        return Ok(());
    };
    if pif.vlan >= 0
        && let Some(tagged) = coordinator
            .vlans
            .get(&pif.vlan_master_of)
            .and_then(|v| coordinator.pifs.get(&v.tagged_pif))
    {
        pif = tagged;
    }
    let in_bond = !pif.bond_master_of.is_empty();
    let devices: Vec<&str> = if in_bond {
        pif.bond_master_of
            .iter()
            .filter_map(|b| coordinator.bonds.get(b))
            .flat_map(|b| b.slaves.iter())
            .filter_map(|p| coordinator.pifs.get(p))
            .map(|p| p.device.as_str())
            .collect()
    } else {
        vec![pif.device.as_str()]
    };
    let count = s
        .pifs
        .iter()
        .filter_map(|p| supporter.pifs.get(p))
        .filter(|p| {
            crate::network::pif_is_management_interface(supporter, p, false)
                && devices.contains(&p.device.as_str())
        })
        .count();
    if count == 1 { Ok(()) } else { Err(in_bond) }
}

/// `PoolJoinRules.CanJoinPool`: whether the server of `supporter` can join
/// the pool of `coordinator`. `increment` is how many servers join at once.
pub fn can_join_pool(
    supporter: Option<&ConnectionView>,
    coordinator: Option<&ConnectionView>,
    allow_license_upgrade: bool,
    allow_supporter_ad_config: bool,
    increment: usize,
) -> JoinReason {
    let Some(sup) = supporter.filter(|c| c.status.is_connected()) else {
        return JoinReason::NotConnected;
    };
    let Some(s) = sup.model.coordinator() else {
        return JoinReason::Connecting;
    };
    if host_restricts(s, Restriction::Pooling) {
        return JoinReason::LicenseRestriction;
    }
    if visible_pool(&sup.model).is_some() {
        return JoinReason::IsAPool;
    }
    let Some(coord) = coordinator.filter(|c| c.status.is_connected()) else {
        return JoinReason::CoordinatorNotConnected;
    };
    let Some(c) = coord.model.coordinator() else {
        return JoinReason::CoordinatorConnecting;
    };
    if sup.id == coord.id {
        return JoinReason::WillBeCoordinator;
    }
    if !may_join(coord) {
        return JoinReason::WrongRoleOnCoordinator;
    }
    if !compatible_cpus(&sup.model, s, &coord.model, c) {
        return JoinReason::DifferentCpus;
    }
    if different_server_version(s, c) {
        return JoinReason::DifferentServerVersion;
    }
    if homogeneous_updates(&sup.model, s) != homogeneous_updates(&coord.model, c) {
        return if coord.model.hosts.len() > 1 {
            JoinReason::DifferentHomogeneousUpdatesFromPool
        } else {
            JoinReason::DifferentHomogeneousUpdatesFromCoordinator
        };
    }
    if free_host_paid_coordinator(s, c, allow_license_upgrade) {
        return JoinReason::UnlicensedHostLicensedCoordinator;
    }
    if !is_free_license(s) && is_free_license(c) {
        return JoinReason::LicensedHostUnlicensedCoordinator;
    }
    let (se, ce) = (edition_kind(&s.edition), edition_kind(&c.edition));
    if se != "free" && ce != "free" && se != ce {
        return JoinReason::LicenseMismatch;
    }
    let size_restricted = feature_forbidden(&coord.model, Restriction::PoolSize);
    if size_restricted && coord.model.hosts.len() > 2 {
        return JoinReason::CoordinatorPoolMaxNumberHostReached;
    }
    if size_restricted && coord.model.hosts.len() + increment > 3 {
        return JoinReason::WillExceedPoolMaxSize;
    }
    if s.software_version.contains_key("xs:linux") != c.software_version.contains_key("xs:linux") {
        return JoinReason::NotSameLinuxPack;
    }
    if !may_join(sup) {
        return JoinReason::WrongRoleOnSupporter;
    }
    if has_shared_storage(&sup.model) {
        return JoinReason::HasSharedStorage;
    }
    if has_running_vms(&sup.model) {
        return JoinReason::HasRunningVms;
    }
    if different_network_backends(sup, s, coord, c) {
        return JoinReason::DifferentNetworkBackends;
    }
    if !compatible_ad_config(s, c, allow_supporter_ad_config) {
        return JoinReason::DifferentAdConfig;
    }
    if coord.model.pool().is_some_and(|(_, p)| p.ha_enabled) {
        return JoinReason::CoordinatorHasHa;
    }
    let vlan_forbidden = feature_forbidden(&sup.model, Restriction::ManagementOnVlan);
    if vlan_forbidden
        && sup
            .model
            .pifs
            .values()
            .any(|p| !objects::pif_is_physical(p))
    {
        return JoinReason::NotPhysicalPif;
    }
    if !vlan_forbidden && !compatible_management_interface(&sup.model) {
        return JoinReason::NonCompatibleManagementInterface;
    }
    if !sup.model.clusters.is_empty() {
        return JoinReason::HasClusteringEnabled;
    }
    if let Err(in_bond) = ip_for_cluster_network(&coord.model, &sup.model, s) {
        return if in_bond {
            JoinReason::WrongNumberOfIpsBond
        } else {
            JoinReason::WrongNumberOfIpsCluster
        };
    }
    let tls = |m: &ConnectionModel| m.pool().is_some_and(|(_, p)| p.tls_verification_enabled);
    let (coord_tls, sup_tls) = (tls(&coord.model), tls(&sup.model));
    let coordinator_is_pool = visible_pool(&coord.model).is_some();
    if coord_tls && !sup_tls {
        return if coordinator_is_pool {
            JoinReason::TlsVerificationOnlyOnPool
        } else {
            JoinReason::TlsVerificationOnlyOnCoordinator
        };
    }
    if !coord_tls && sup_tls {
        return if coordinator_is_pool {
            JoinReason::TlsVerificationOnlyOnPoolJoiner
        } else {
            JoinReason::TlsVerificationOnlyOnCoordinatorJoiner
        };
    }
    JoinReason::Allowed
}

fn different_network_backends(
    sup: &ConnectionView,
    s: &HostRecord,
    coord: &ConnectionView,
    c: &HostRecord,
) -> bool {
    if let (Some(a), Some(b)) = (
        s.software_version.get("network_backend"),
        c.software_version.get("network_backend"),
    ) && a != b
    {
        return true;
    }
    let controller = |conn: &ConnectionView| {
        let (_, pool) = conn.model.pool()?;
        let on = !pool.vswitch_controller.is_empty()
            && conn.model.hosts.values().all(|h| {
                !host_restricts(h, Restriction::VswitchController)
                    && h.software_version
                        .get("network_backend")
                        .map(String::as_str)
                        == Some("openvswitch")
            });
        on.then(|| pool.vswitch_controller.clone())
    };
    matches!((controller(sup), controller(coord)), (Some(a), Some(b)) if a != b)
}

/// A question the original asks before servers join a pool, with the
/// names of the servers it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinWarning {
    /// `*_LICENSE_MESSAGE`: free servers joining a licensed pool.
    License(Vec<String>),
    /// `*_CPU_MASKING_MESSAGE`: servers whose CPUs differ.
    CpuMasking(Vec<String>),
    /// `*_AD_MESSAGE`: servers without the pool's AD configuration.
    ActiveDirectory(Vec<String>),
}

/// The questions asked before `supporters` join `coordinator`'s pool
/// (`AddHostToPoolCommand` / `NewPoolDialog.createPool`), in order.
pub fn join_warnings(
    supporters: &[&ConnectionView],
    coordinator: &ConnectionView,
) -> Vec<JoinWarning> {
    let Some(c) = coordinator.model.coordinator() else {
        return Vec::new();
    };
    let hosts: Vec<(&ConnectionView, &HostRecord)> = supporters
        .iter()
        .filter_map(|s| s.model.coordinator().map(|h| (*s, &**h)))
        .collect();
    let names = |f: &dyn Fn(&ConnectionView, &HostRecord) -> bool| -> Vec<String> {
        hosts
            .iter()
            .filter(|(conn, h)| f(conn, h))
            .map(|(_, h)| h.name_label.clone())
            .collect()
    };
    let mut out = Vec::new();
    let license = names(&|_, h| free_host_paid_coordinator(h, c, false));
    if !license.is_empty() {
        out.push(JoinWarning::License(license));
    }
    let cpus = names(&|conn, h| !compatible_cpus(&conn.model, h, &coordinator.model, c));
    if !cpus.is_empty() {
        out.push(JoinWarning::CpuMasking(cpus));
    }
    let ad = names(&|_, h| !compatible_ad_config(h, c, false));
    if !ad.is_empty() {
        out.push(JoinWarning::ActiveDirectory(ad));
    }
    out
}

/// The address the joining server is given for the coordinator
/// (`AddHostToPoolAction`: the coordinator's `address`, else the
/// connection's host name). A non-default port is kept (servers behind a
/// port forward, and the test mock).
pub fn pool_join_address(coordinator: &ConnectionView) -> String {
    let endpoint = xapi::Endpoint::parse(&coordinator.server).ok();
    let port = endpoint.as_ref().map(|e| e.port);
    let default_port = endpoint.as_ref().map(|e| if e.tls { 443 } else { 80 });
    let address = coordinator
        .model
        .coordinator()
        .map(|h| h.address.clone())
        .filter(|a| !a.is_empty())
        .or_else(|| endpoint.as_ref().map(|e| e.host.clone()))
        .unwrap_or_default();
    match (port, default_port) {
        (Some(p), Some(d)) if p != d && !address.contains(':') => format!("{address}:{p}"),
        _ => address,
    }
}

// ---- removing servers ---------------------------------------------------------

/// Why a server cannot be removed from its pool
/// (`RemoveHostFromPoolCommand.GetCantRunReasonCore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveProblem {
    /// "You cannot remove the coordinator from the pool."
    Coordinator,
    /// "The server is unreachable."
    Unreachable,
    /// "This server has running VMs".
    RunningVms,
    /// Not in a pool (no item to offer).
    NotInPool,
}

/// `RemoveHostFromPoolCommand.CanRun`: `None` when `host` can be removed.
pub fn remove_problem(model: &ConnectionModel, host: &Ref<Host>) -> Option<RemoveProblem> {
    let Some((_, pool)) = visible_pool(model) else {
        return Some(RemoveProblem::NotInPool);
    };
    let Some(rec) = model.hosts.get(host) else {
        return Some(RemoveProblem::NotInPool);
    };
    if pool.master == *host {
        return Some(RemoveProblem::Coordinator);
    }
    if !objects::host_is_live(model, rec) {
        return Some(RemoveProblem::Unreachable);
    }
    // The control domain is always resident.
    if rec.resident_vms.len() > 1 {
        return Some(RemoveProblem::RunningVms);
    }
    None
}

/// The servers Remove Server offers (`PoolRemoveServerToolStripMenuItem`).
pub fn removable_hosts(model: &ConnectionModel) -> Vec<(&Ref<Host>, &HostRecord)> {
    objects::sorted_hosts(model)
        .into_iter()
        .filter(|(r, _)| remove_problem(model, r).is_none())
        .map(|(r, h)| (r, &**h))
        .collect()
}

/// Why Make into standalone server cannot run (`DeletePoolCommand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandaloneProblem {
    HaEnabling,
    HaEnabled,
    /// More than one server: not offered.
    SeveralHosts,
    /// A pool of one already.
    NotAPool,
}

/// `DeletePoolCommand`: `None` when the pool can become a standalone server.
pub fn standalone_problem(model: &ConnectionModel) -> Option<StandaloneProblem> {
    let Some((_, pool)) = visible_pool(model) else {
        return Some(StandaloneProblem::NotAPool);
    };
    if model.hosts.len() != 1 {
        return Some(StandaloneProblem::SeveralHosts);
    }
    if pool
        .current_operations
        .values()
        .any(|o| *o == PoolAllowedOperations::HaEnable)
    {
        return Some(StandaloneProblem::HaEnabling);
    }
    if pool.ha_enabled {
        return Some(StandaloneProblem::HaEnabled);
    }
    None
}

// ---- the coordinator in maintenance mode -------------------------------------

/// The servers the Enter Maintenance Mode dialog offers as new
/// coordinator (`EvacuateHostDialog.PopulateHosts`): the other enabled,
/// live servers. Empty unless `host` is the coordinator of a pool of
/// several servers.
pub fn new_coordinator_candidates<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
) -> Vec<(&'a Ref<Host>, &'a HostRecord)> {
    if !objects::is_coordinator(model, host) || model.hosts.len() <= 1 {
        return Vec::new();
    }
    objects::sorted_hosts(model)
        .into_iter()
        .filter(|(r, h)| *r != host && h.enabled && objects::host_is_live(model, h))
        .map(|(r, h)| (r, &**h))
        .collect()
}

/// Why the new coordinator cannot be chosen now
/// (`EvacuateHostDialog.EnableComboBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewCoordinatorProblem {
    SecretRotationPending,
    HaEnabling,
    HaDisabling,
    ClusterCreating,
    /// The server does not allow it now, for another reason.
    NotAllowed,
}

/// `None` when a new coordinator can be designated.
pub fn new_coordinator_problem(model: &ConnectionModel) -> Option<NewCoordinatorProblem> {
    let (_, pool) = model.pool()?;
    let rotation_restricted = feature_forbidden(model, Restriction::PoolSecretRotation);
    if rotation_restricted
        || (!pool.is_psr_pending
            && pool
                .allowed_operations
                .contains(&PoolAllowedOperations::DesignateNewMaster))
    {
        return None;
    }
    let running = |op| pool.current_operations.values().any(|o| *o == op);
    Some(if pool.is_psr_pending {
        NewCoordinatorProblem::SecretRotationPending
    } else if running(PoolAllowedOperations::HaEnable) {
        NewCoordinatorProblem::HaEnabling
    } else if running(PoolAllowedOperations::HaDisable) {
        NewCoordinatorProblem::HaDisabling
    } else if running(PoolAllowedOperations::ClusterCreate) {
        NewCoordinatorProblem::ClusterCreating
    } else {
        NewCoordinatorProblem::NotAllowed
    })
}

/// The pool's servers by name (for messages listing several).
pub fn host_names(model: &ConnectionModel, hosts: &[Ref<Host>]) -> Vec<String> {
    let names: HashMap<&Ref<Host>, &str> = model
        .hosts
        .iter()
        .map(|(r, h)| (r, h.name_label.as_str()))
        .collect();
    hosts
        .iter()
        .map(|h| names.get(h).copied().unwrap_or_default().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(
            version_cmp(Some("3.4.0"), Some("3.2.50")),
            Ordering::Greater
        );
        assert_eq!(version_cmp(Some("3.2.1"), Some("3.2.50")), Ordering::Less);
        assert_eq!(
            version_cmp(Some("25.27.0"), Some("23.27.0")),
            Ordering::Greater
        );
        assert_eq!(version_cmp(Some("22.33"), Some("22.33.0")), Ordering::Equal);
        assert_eq!(version_cmp(None, Some("1")), Ordering::Less);
    }

    #[test]
    fn editions_fold_like_the_original() {
        assert_eq!(edition_kind("xcp-ng"), "free");
        assert_eq!(edition_kind("premium"), "premium");
    }
}
