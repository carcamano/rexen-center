//! The Migrate, Move and Copy VM wizard (`CrossPoolMigrateWizard`, F12a):
//! when each command is offered, the destinations, the storage and network
//! mappings, the summary and the requests the wizard ends with. How they
//! run is [`crate::migrate_run`]'s.
//!
//! The wizard has three modes ([`WizardMode`]), as the original:
//!
//! * **Migrate** (`CrossPoolMigrateCommand`, the "Migrate VM wizard..." of
//!   the Migrate to Server submenu, and Move VM of a suspended VM):
//!   Destination, (RBAC), Storage, Networking (another pool only),
//!   Migration Network, Finish.
//! * **Move** (`CrossPoolMoveVMCommand`, Move VM of a halted VM): the same
//!   pages; a move within the pool skips the Migration Network page and
//!   runs as `VMMoveAction` (a copy of each disk on its new SR).
//! * **Copy** (`CopyVMCommand` / `CopyTemplateCommand` of a halted VM or a
//!   template that may migrate): a first page chooses a copy within the
//!   pool (Name and Storage: the existing clone or full copy) or to another
//!   pool (the Migrate pages, ending in `VM.migrate_send` with
//!   `copy=true`).
//!
//! The Destination page lists each connected pool (or standalone server)
//! and, per VM, its servers. Each is checked as `CrossPoolMigrateFilter`
//! does: the VM's current server is refused ("Current server"), an older
//! server is refused, and the server is asked (`host.migrate_receive` on
//! the destination, `VM.assert_can_migrate` on the source; within the
//! pool, `VM.assert_can_boot_here` for a plain live migration) through
//! [`crate::Precheck::Migrate`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use xapi::{
    Host, HostRecord, Network, NetworkRecord, Ref, Sr, SrRecord, Vbd, VbdType, Vdi, VdiRecord, Vif,
    VifRecord, Vm, VmOperations, VmPowerState, VmRecord,
};

use crate::backend::ConnectionId;
use crate::model::ConnectionModel;
use crate::objects::{self, VmKind};

/// The wizard's mode (`WizardMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WizardMode {
    Migrate,
    Move,
    Copy,
}

/// Why `CrossPoolMigrateCommand.CanRun` refuses a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CantMigrate {
    /// `MIGRATION_NOT_ALLOWED`: the server does not offer `migrate_send`.
    NotAllowed,
    /// `MIGRATION_NOT_ALLOWED_USUPPORTED_SR`: a disk is on a raw HBA SR.
    UnsupportedSr,
}

/// The SRs of a VM's disks (`VM.SRs`).
fn vm_srs<'a>(model: &'a ConnectionModel, vm: &VmRecord) -> Vec<Option<&'a Arc<SrRecord>>> {
    vm.vbds
        .iter()
        .filter_map(|r| model.get(r))
        .filter_map(|b| model.get(&b.vdi))
        .map(|vdi| model.get(&vdi.sr))
        .collect()
}

/// `SR.HBALunPerVDI`: a raw HBA SR (one LUN per disk).
pub fn hba_lun_per_vdi(sr: &SrRecord) -> bool {
    sr.r#type == "rawhba"
}

/// `CrossPoolMigrateCommand.CanRun(vm, null)`.
pub fn can_migrate_send(model: &ConnectionModel, vm: &VmRecord) -> Result<(), CantMigrate> {
    if !vm.allowed_operations.contains(&VmOperations::MigrateSend) {
        return Err(CantMigrate::NotAllowed);
    }
    if !vm_srs(model, vm)
        .iter()
        .all(|sr| sr.is_some_and(|s| !hba_lun_per_vdi(s)))
    {
        return Err(CantMigrate::UnsupportedSr);
    }
    Ok(())
}

/// "Migrate VM wizard..." (`CrossPoolMigrateCommand` without a server):
/// a guest this client is not busy with, which may migrate.
pub fn can_open_migrate(model: &ConnectionModel, vm: &VmRecord, busy: bool) -> bool {
    !busy && objects::vm_kind(vm) == VmKind::Guest && can_migrate_send(model, vm).is_ok()
}

/// `CrossPoolMoveVMCommand.CanRun`: not a template, not running, may
/// migrate.
pub fn cross_pool_move_can_run(model: &ConnectionModel, vm: &VmRecord, busy: bool) -> bool {
    !busy
        && !vm.is_a_template
        && !vm.is_a_snapshot
        && !vm.is_control_domain
        && vm.power_state != VmPowerState::Running
        && can_migrate_send(model, vm).is_ok()
}

/// `VM.CanBeMoved`: no raw HBA disk; a guest that may be exported, not
/// suspended, with a disk it owns.
pub fn can_be_moved(model: &ConnectionModel, vm: &VmRecord, busy: bool) -> bool {
    if vm_srs(model, vm)
        .iter()
        .any(|sr| sr.is_some_and(|s| hba_lun_per_vdi(s)))
    {
        return false;
    }
    !vm.is_a_template
        && !busy
        && vm.allowed_operations.contains(&VmOperations::Export)
        && vm.power_state != VmPowerState::Suspended
        && vm
            .vbds
            .iter()
            .filter_map(|r| model.get(r))
            .any(|b| crate::provisioning::vbd_is_owner(b))
}

/// No disk of the VM has changed block tracking on (`MoveVMCommand.CBTDisabled`).
fn cbt_disabled(model: &ConnectionModel, vm: &VmRecord) -> bool {
    !vm.vbds
        .iter()
        .filter_map(|r| model.get(r))
        .filter_map(|b| model.get(&b.vdi))
        .any(|v| v.cbt_enabled)
}

/// Move VM (`MoveVMCommand`): the wizard when the VM may migrate,
/// otherwise the Move VM dialog for a VM that can be moved.
pub fn can_move(model: &ConnectionModel, vm: &VmRecord, busy: bool) -> bool {
    objects::vm_kind(vm) == VmKind::Guest
        && cbt_disabled(model, vm)
        && (cross_pool_move_can_run(model, vm, busy) || can_be_moved(model, vm, busy))
}

/// Whether Move VM opens the wizard (rather than the Move VM dialog).
pub fn move_uses_wizard(model: &ConnectionModel, vm: &VmRecord, busy: bool) -> bool {
    cross_pool_move_can_run(model, vm, busy)
}

/// `CrossPoolMoveVMCommand.GetWizardMode`: a suspended VM migrates, any
/// other moves. Its menu item reads "Migrate VM..." then.
pub fn move_mode(vm: &VmRecord) -> WizardMode {
    if vm.power_state == VmPowerState::Suspended {
        WizardMode::Migrate
    } else {
        WizardMode::Move
    }
}

/// Whether Copy VM / Copy Template opens this wizard
/// (`CanLaunchMigrateWizard`): a halted guest, or a template that is not
/// a default one, which may migrate.
pub fn copy_uses_wizard(model: &ConnectionModel, vm: &VmRecord) -> bool {
    let kind_ok = match objects::vm_kind(vm) {
        VmKind::Guest => vm.power_state == VmPowerState::Halted,
        VmKind::UserTemplate => true,
        _ => false,
    };
    kind_ok && can_migrate_send(model, vm).is_ok()
}

/// `Host.RestrictCrossPoolMigrate` / `RestrictDMC`: a licence restriction
/// in the host's `license_params` (never set on XCP-ng).
fn restricted(host: &HostRecord, key: &str) -> bool {
    host.license_params.get(key).is_some_and(|v| v == "true")
}

/// `Helpers.FeatureForbidden(connection, Host.RestrictCrossPoolMigrate)`.
pub fn cross_pool_restricted(model: &ConnectionModel) -> bool {
    model
        .hosts
        .values()
        .any(|h| restricted(h, "restrict_cross_pool_migrate"))
}

// ---- destinations ---------------------------------------------------------------------

/// One choice of the Destination list: a pool shown as such, or a
/// standalone server (the coordinator of an unnamed pool of one).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Destination {
    pub connection: ConnectionId,
    /// `None`: the pool (several servers or a named pool).
    pub host: Option<Ref<Host>>,
}

/// Where one VM goes: the destination's connection and, optionally, a
/// server ("None selected" leaves it to the pool: its coordinator receives
/// the VM).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VmTarget {
    pub connection: ConnectionId,
    pub host: Option<Ref<Host>>,
}

/// A refusal found without asking the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRefusal {
    /// `HOST_MENU_CURRENT_SERVER`.
    CurrentServer,
    /// `OLDER_THAN_CURRENT_SERVER`.
    OlderServer,
    /// `DYNAMIC_MEMORY_CONTROL_UNAVAILABLE` (a licence restriction).
    DynamicMemory,
}

/// `ResidentHostIsSameAsSelectionFilter` for one VM and a server or pool
/// (`host: None`): its home server is that server, or the pool is a pool
/// of one whose coordinator is its home.
pub fn is_current_server(
    source: &ConnectionModel,
    vm: &VmRecord,
    target: &ConnectionModel,
    host: Option<&Ref<Host>>,
    same_connection: bool,
) -> bool {
    if !same_connection {
        return false;
    }
    let Some(home) = objects::vm_home(source, vm) else {
        return false;
    };
    match host {
        Some(h) => h == home,
        None => target.hosts.len() <= 1 && target.pool().is_some_and(|(_, p)| &p.master == home),
    }
}

/// The servers a destination check covers (`CollateHosts`): the server, or
/// every server of the pool, sorted.
pub fn collate_hosts(model: &ConnectionModel, host: Option<&Ref<Host>>) -> Vec<Ref<Host>> {
    match host {
        Some(h) => vec![h.clone()],
        None => objects::sorted_hosts(model)
            .into_iter()
            .map(|(r, _)| r.clone())
            .collect(),
    }
}

/// The local part of `CrossPoolMigrateFilter.FailureFoundFor` for one VM
/// and server: the server may be skipped (it is the VM's home, or a move
/// within the pool needs no check), refused, or must be asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCheck {
    /// No question: the server is fine (move within the pool, or the
    /// VM's own home server in a pool).
    Skip,
    Refused(LocalRefusal),
    /// Ask the servers ([`crate::Precheck::Migrate`]).
    Ask,
}

/// The local checks of one VM on one destination server.
pub fn local_check(
    source: &ConnectionModel,
    vm: &VmRecord,
    target: &ConnectionModel,
    host: &Ref<Host>,
    mode: WizardMode,
    same_connection: bool,
) -> LocalCheck {
    if mode == WizardMode::Move && same_connection {
        return LocalCheck::Skip;
    }
    if same_connection && objects::vm_home(source, vm) == Some(host) {
        return LocalCheck::Skip;
    }
    let Some(dest) = target.hosts.get(host) else {
        return LocalCheck::Refused(LocalRefusal::OlderServer);
    };
    let source_host = objects::vm_home(source, vm)
        .and_then(|h| source.hosts.get(h))
        .or_else(|| source.coordinator());
    let dest_version = crate::pool::platform_version(dest);
    let source_version = source_host.and_then(|h| crate::pool::platform_version(h));
    if crate::pool::version_cmp(dest_version, source_version) == std::cmp::Ordering::Less {
        return LocalCheck::Refused(LocalRefusal::OlderServer);
    }
    let live = matches!(
        vm.power_state,
        VmPowerState::Running | VmPowerState::Paused | VmPowerState::Suspended
    );
    let dynamic = vm.memory_static_min > vm.memory_dynamic_min
        || vm.memory_dynamic_min != vm.memory_dynamic_max
        || vm.memory_dynamic_max != vm.memory_static_max;
    if restricted(dest, "restrict_dmc") && live && dynamic {
        return LocalCheck::Refused(LocalRefusal::DynamicMemory);
    }
    LocalCheck::Ask
}

/// What the servers said about one VM and server
/// ([`crate::PrecheckResult::Migrate`]).
#[derive(Debug, Clone)]
pub enum ServerVerdict {
    /// `VM.assert_can_migrate` passed (or failed only for snapshot
    /// interfaces, which the original accepts), or the VM can live
    /// migrate within the pool.
    Ok,
    /// Refused: the storage migration's error and, within the pool, why a
    /// plain live migration is refused too.
    Refused {
        error: Arc<xapi::Error>,
        boot: Option<Arc<xapi::Error>>,
    },
}

/// The refusal shown for a server, combined from the local checks and the
/// servers' answers.
#[derive(Debug, Clone)]
pub enum Refusal {
    Local(LocalRefusal),
    Server {
        error: Arc<xapi::Error>,
        boot: Option<Arc<xapi::Error>>,
    },
}

/// The pool-level verdict (`FailureFoundFor` on a pool): fine when no VM
/// is refused by every server it was checked on.
pub fn combine<'a>(verdicts: impl IntoIterator<Item = Option<&'a Refusal>>) -> Option<&'a Refusal> {
    let mut last = None;
    for v in verdicts {
        last = Some(v?);
    }
    last
}

// ---- storage --------------------------------------------------------------------------

/// `SR.SupportsStorageMigration`: not ISO or memory, and its driver can
/// snapshot and mirror disks.
pub fn supports_storage_migration(model: &ConnectionModel, sr: &SrRecord) -> bool {
    if sr.content_type == "iso" || sr.r#type == "tmpfs" {
        return false;
    }
    crate::storage::sm_for(model, &sr.r#type).is_some_and(|sm| {
        let has = |c: &str| sm.capabilities.iter().any(|x| x == c) || sm.features.contains_key(c);
        has("VDI_SNAPSHOT") && has("VDI_MIRROR")
    })
}

/// `CrossPoolMigrateStoragePage.SrsAreSuitable`.
pub fn srs_are_suitable(
    source: &ConnectionModel,
    source_sr: Option<&SrRecord>,
    target: &ConnectionModel,
    target_sr: &SrRecord,
    mode: WizardMode,
    same_connection: bool,
) -> bool {
    let Some(source_sr) = source_sr else {
        return false;
    };
    if hba_lun_per_vdi(source_sr) || hba_lun_per_vdi(target_sr) {
        return false;
    }
    if mode == WizardMode::Move && same_connection {
        return true;
    }
    supports_storage_migration(source, source_sr) && supports_storage_migration(target, target_sr)
}

/// `CrossPoolMigrateStoragePage.IsExtraSpaceNeeded`: a disk staying on its
/// SR (migrating or moving) needs no room.
pub fn extra_space_needed(source_sr: &Ref<Sr>, target_sr: &Ref<Sr>, mode: WizardMode) -> bool {
    !(source_sr == target_sr && matches!(mode, WizardMode::Migrate | WizardMode::Move))
}

/// A disk of the Storage page (`CrossPoolMigrationStorageResource`): the
/// VM's non-ISO disks; its space need is the disk's physical utilisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRow {
    pub vm: Ref<Vm>,
    pub vdi: Ref<Vdi>,
    pub name: String,
    pub sr: Ref<Sr>,
    pub required: i64,
}

/// The disks of `vm` the Storage page maps (`ResourceData`).
pub fn disk_rows(model: &ConnectionModel, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<DiskRow> {
    let mut out = Vec::new();
    for vbd in vm.vbds.iter().filter_map(|r| model.get(r)) {
        let Some(vdi) = model.get(&vbd.vdi) else {
            continue;
        };
        if model.get(&vdi.sr).is_none_or(|sr| sr.content_type == "iso") {
            continue;
        }
        out.push(DiskRow {
            vm: vm_ref.clone(),
            vdi: vbd.vdi.clone(),
            name: vdi.name_label.clone(),
            sr: vdi.sr.clone(),
            required: vdi.physical_utilisation.max(0),
        });
    }
    out
}

/// The SRs a disk may go to (`FillGridComboBox`): attached, shown, not ISO,
/// shared or on the VM's chosen server, with room (or staying put) and
/// suitable. Sorted by name; `default` marks the pool's default SR.
pub fn target_srs<'a>(
    source: &ConnectionModel,
    target: &'a ConnectionModel,
    disk: &DiskRow,
    host: Option<&Ref<Host>>,
    mode: WizardMode,
    same_connection: bool,
    show_hidden: bool,
) -> Vec<(&'a Ref<Sr>, &'a Arc<SrRecord>)> {
    let source_sr = source.get(&disk.sr).map(|s| &**s);
    let mut out: Vec<(&Ref<Sr>, &Arc<SrRecord>)> = Vec::new();
    for pbd in target.pbds.values() {
        let Some((sr_ref, sr)) = target.srs.get_key_value(&pbd.sr) else {
            continue;
        };
        if objects::sr_is_detached(target, sr) || !objects::sr_is_shown(sr, show_hidden) {
            continue;
        }
        if sr.content_type.eq_ignore_ascii_case("iso") || sr.r#type.eq_ignore_ascii_case("iso") {
            continue;
        }
        let on_host = host.is_some_and(|h| &pbd.host == h);
        if !(sr.shared || on_host) {
            continue;
        }
        let fits = !extra_space_needed(&disk.sr, sr_ref, mode)
            || (crate::provisioning::sr_free(sr) > disk.required
                && srs_are_suitable(source, source_sr, target, sr, mode, same_connection));
        if fits && !out.iter().any(|(r, _)| *r == sr_ref) {
            out.push((sr_ref, sr));
        }
    }
    out.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label));
    out
}

/// The SR a disk row starts with: the pool's default SR if offered, else
/// the first.
pub fn default_sr<'a>(
    target: &ConnectionModel,
    srs: &[(&'a Ref<Sr>, &'a Arc<SrRecord>)],
) -> Option<&'a Ref<Sr>> {
    srs.iter()
        .find(|(r, _)| crate::storage::is_default_sr(target, r))
        .or_else(|| srs.first())
        .map(|(r, _)| *r)
}

/// The SRs offered to "Place all virtual disks on the same SR"
/// (`commonSRs` with `PopulateSrComboBox`): shared SRs (or, with one
/// destination server for every VM, its local SRs) suitable for every disk,
/// with the room they all need; ordered enabled first, then by name.
/// `bool`: enough room.
pub fn common_srs<'a>(
    source: &ConnectionModel,
    target: &'a ConnectionModel,
    disks: &[DiskRow],
    hosts: &[Option<Ref<Host>>],
    mode: WizardMode,
    same_connection: bool,
    show_hidden: bool,
) -> Vec<(&'a Ref<Sr>, &'a Arc<SrRecord>, bool)> {
    let single_host = match hosts {
        [first, rest @ ..] if rest.iter().all(|h| h == first) => first.clone(),
        _ => None,
    };
    let mut candidates: Vec<(&Ref<Sr>, &Arc<SrRecord>)> = Vec::new();
    for disk in disks {
        let host = hosts.first().cloned().flatten();
        for (r, sr) in target_srs(
            source,
            target,
            disk,
            host.as_ref(),
            mode,
            same_connection,
            show_hidden,
        ) {
            let on_single = single_host.as_ref().is_some_and(|h| {
                sr.pbds
                    .iter()
                    .filter_map(|p| target.get(p))
                    .any(|p| &p.host == h)
            });
            if (sr.shared || on_single) && !candidates.iter().any(|(c, _)| *c == r) {
                candidates.push((r, sr));
            }
        }
    }
    let mut out = Vec::new();
    for (r, sr) in candidates {
        let mut required = 0i64;
        let mut suitable = true;
        for disk in disks {
            if extra_space_needed(&disk.sr, r, mode) {
                required += disk.required;
                let source_sr = source.get(&disk.sr).map(|s| &**s);
                if !srs_are_suitable(source, source_sr, target, sr, mode, same_connection) {
                    suitable = false;
                    break;
                }
            }
        }
        if suitable {
            out.push((r, sr, crate::provisioning::sr_free(sr) > required));
        }
    }
    if !out.iter().any(|(_, _, ok)| *ok) {
        return Vec::new();
    }
    out.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| objects::natural_cmp(&a.1.name_label, &b.1.name_label))
    });
    out
}

/// `CheckStorageRequirements` for per-disk choices: the first SR whose
/// free space does not cover the disks put on it.
pub fn short_of_space<'a>(
    target: &'a ConnectionModel,
    disks: &[DiskRow],
    choices: &[Option<Ref<Sr>>],
    mode: WizardMode,
) -> Option<&'a SrRecord> {
    let mut needs: HashMap<&Ref<Sr>, i64> = HashMap::new();
    for (disk, choice) in disks.iter().zip(choices) {
        let Some(sr) = choice else { continue };
        let need = if extra_space_needed(&disk.sr, sr, mode) {
            disk.required
        } else {
            0
        };
        let total = needs.entry(sr).or_insert(0);
        *total += need;
        if let Some(rec) = target.srs.get(sr)
            && crate::provisioning::sr_free(rec) <= *total
        {
            return Some(rec);
        }
    }
    None
}

/// `AddAdditionalMappings`: the disks of the VM's snapshots go where the
/// disk they were taken of goes (else to the first SR chosen for the VM),
/// and, within the pool, its DVD's ISO stays on its SR.
pub fn additional_mappings(
    model: &ConnectionModel,
    vm: &VmRecord,
    storage: &mut BTreeMap<Ref<Vdi>, Ref<Sr>>,
    same_connection: bool,
) {
    let suitable = storage.values().next().cloned();
    for snap in vm.snapshots.iter().filter_map(|r| model.get(r)) {
        for vbd in snap.vbds.iter().filter_map(|r| model.get(r)) {
            let Some(vdi) = model.get(&vbd.vdi) else {
                continue;
            };
            if model.get(&vdi.sr).is_none_or(|sr| sr.content_type == "iso") {
                continue;
            }
            if let Some(sr) = storage.get(&vdi.snapshot_of).cloned() {
                storage.insert(vbd.vdi.clone(), sr);
            } else if let Some(sr) = &suitable {
                storage.insert(vbd.vdi.clone(), sr.clone());
            }
        }
    }
    if suitable.is_none() || !same_connection {
        return;
    }
    for vbd in vm.vbds.iter().filter_map(|r| model.get(r)) {
        if vbd.r#type != VbdType::Cd {
            continue;
        }
        let Some(vdi) = model.get(&vbd.vdi) else {
            continue;
        };
        if model
            .get(&vdi.sr)
            .is_some_and(|sr| objects::is_tools_sr(sr))
        {
            continue;
        }
        storage.insert(vbd.vdi.clone(), vdi.sr.clone());
    }
}

// ---- networks -------------------------------------------------------------------------

/// An interface of the Networking page (`CrossPoolMigrationNetworkResource`):
/// the VM's interfaces, then its snapshots' interfaces whose MAC the VM
/// does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VifRow {
    pub vm: Ref<Vm>,
    /// The VM (or snapshot) the interface belongs to.
    pub owner_name: String,
    pub vif: Ref<Vif>,
    pub mac: String,
    pub network: Ref<Network>,
    pub network_name: String,
}

/// The interfaces of `vm` the Networking page maps.
pub fn vif_rows(model: &ConnectionModel, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<VifRow> {
    let row = |owner: &VmRecord, r: &Ref<Vif>, v: &VifRecord| VifRow {
        vm: vm_ref.clone(),
        owner_name: owner.name_label.clone(),
        vif: r.clone(),
        mac: v.mac.clone(),
        network: v.network.clone(),
        network_name: model
            .get(&v.network)
            .map(|n| objects::network_name(model, n))
            .unwrap_or_default(),
    };
    let mut out: Vec<VifRow> = vm
        .vifs
        .iter()
        .filter_map(|r| model.vifs.get_key_value(r))
        .map(|(r, v)| row(vm, r, v))
        .collect();
    out.sort_by(|a, b| {
        let dev = |r: &Ref<Vif>| model.get(r).map(|v| v.device.clone()).unwrap_or_default();
        objects::natural_cmp(&dev(&a.vif), &dev(&b.vif))
    });
    let macs: Vec<String> = out.iter().map(|r| r.mac.clone()).collect();
    for snap in vm.snapshots.iter().filter_map(|r| model.get(r)) {
        for (r, v) in snap.vifs.iter().filter_map(|r| model.vifs.get_key_value(r)) {
            if !macs.contains(&v.mac) {
                out.push(row(snap, r, v));
            }
        }
    }
    out
}

/// The networks an interface may go to (`FillGridComboBox`): shown, not a
/// bond member or SR-IOV, seen by the chosen server (or by every server),
/// not the guest installer network; sorted by name.
pub fn target_networks<'a>(
    target: &'a ConnectionModel,
    host: Option<&Ref<Host>>,
    show_hidden: bool,
) -> Vec<(&'a Ref<Network>, &'a Arc<NetworkRecord>)> {
    let mut out: Vec<_> = crate::xva::import_networks(target, host, show_hidden)
        .into_iter()
        .filter(|(_, n)| !objects::is_guest_installer_network(n))
        .collect();
    out.sort_by(|a, b| {
        objects::natural_cmp(
            &objects::network_name(target, a.1),
            &objects::network_name(target, b.1),
        )
    });
    out
}

/// The network a row starts with: one of the same name, else the first.
pub fn default_network<'a>(
    target: &ConnectionModel,
    networks: &[(&'a Ref<Network>, &'a Arc<NetworkRecord>)],
    source_name: &str,
) -> Option<&'a Ref<Network>> {
    networks
        .iter()
        .find(|(_, n)| objects::network_name(target, n) == source_name)
        .or_else(|| networks.first())
        .map(|(r, _)| *r)
}

/// The networks offered for the storage migration
/// (`CrossPoolMigrateTransferNetworkPage`'s combo box): networks with a
/// plugged interface that has an IP address; the management network first
/// chosen. `bool`: it is the management network.
pub fn transfer_networks(
    model: &ConnectionModel,
) -> Vec<(&Ref<Network>, &Arc<NetworkRecord>, bool)> {
    let mut out: Vec<(&Ref<Network>, &Arc<NetworkRecord>, bool)> = Vec::new();
    for (r, n) in &model.networks {
        let pifs: Vec<_> = n.pifs.iter().filter_map(|p| model.get(p)).collect();
        let with_ip = pifs
            .iter()
            .any(|p| p.currently_attached && (!p.ip.is_empty() || !p.ipv6.is_empty()));
        if !with_ip {
            continue;
        }
        let management = pifs.iter().any(|p| p.management);
        out.push((r, n, management));
    }
    out.sort_by(|a, b| {
        objects::natural_cmp(
            &objects::network_name(model, a.1),
            &objects::network_name(model, b.1),
        )
    });
    out
}

// ---- requests -------------------------------------------------------------------------

/// `VM.migrate_send` (`VMCrossPoolMigrateAction`): the VM, the server that
/// receives it (on connection `target`), the network the disks travel on,
/// and the disk and interface maps (references of the destination).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MigrateSpec {
    pub vm: Ref<Vm>,
    pub target: ConnectionId,
    pub host: Ref<Host>,
    /// The destination server's name (the operation's title).
    pub host_name: String,
    pub transfer_network: Ref<Network>,
    pub vdi_map: Vec<(Ref<Vdi>, Ref<Sr>)>,
    pub vif_map: Vec<(Ref<Vif>, Ref<Network>)>,
    pub copy: bool,
}

/// What Finish asks for one VM (`FinishWizard`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishRequest {
    /// `VMCrossPoolMigrateAction` (another pool, or a storage motion).
    MigrateSend(MigrateSpec),
    /// `VMMigrateAction`: a plain live migration within the pool.
    PoolMigrate { vm: Ref<Vm>, host: Ref<Host> },
    /// `VMMoveAction`: the disks of a halted VM copied to their new SRs.
    Move {
        vm: Ref<Vm>,
        host: Ref<Host>,
        storage: Vec<(Ref<Vdi>, Ref<Sr>)>,
    },
    /// A move within the pool with nothing to move.
    Nothing,
}

/// The mapping of one VM as the wizard leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmPlan {
    pub vm: Ref<Vm>,
    pub target: VmTarget,
    pub storage: BTreeMap<Ref<Vdi>, Ref<Sr>>,
    /// MAC → network of the destination.
    pub networks: BTreeMap<String, Ref<Network>>,
}

/// The server that receives a VM: the chosen one, else the destination
/// pool's coordinator.
pub fn receiving_host<'a>(target: &'a ConnectionModel, plan: &'a VmPlan) -> Option<&'a Ref<Host>> {
    plan.target
        .host
        .as_ref()
        .or_else(|| target.pool().map(|(_, p)| &p.master))
}

/// `FinishWizard` for one VM.
#[allow(clippy::too_many_arguments)]
pub fn finish_request(
    source: &ConnectionModel,
    source_id: ConnectionId,
    target: &ConnectionModel,
    plan: &VmPlan,
    mode: WizardMode,
    transfer_network: Option<&Ref<Network>>,
) -> Option<FinishRequest> {
    let vm = source.vms.get(&plan.vm)?;
    let host = receiving_host(target, plan)?.clone();
    let host_name = target
        .hosts
        .get(&host)
        .map(|h| h.name_label.clone())
        .unwrap_or_default();
    let intra = plan.target.connection == source_id;
    if mode == WizardMode::Move && intra {
        let storage: Vec<(Ref<Vdi>, Ref<Sr>)> = plan
            .storage
            .iter()
            .filter(|(vdi, sr)| source.get(*vdi).is_some_and(|v| &v.sr != *sr))
            .map(|(v, s)| (v.clone(), s.clone()))
            .collect();
        if storage.is_empty() {
            return Some(FinishRequest::Nothing);
        }
        return Some(FinishRequest::Move {
            vm: plan.vm.clone(),
            host,
            storage: plan
                .storage
                .iter()
                .map(|(v, s)| (v.clone(), s.clone()))
                .collect(),
        });
    }
    let copy = mode == WizardMode::Copy;
    let storage_motion = !intra
        || plan
            .storage
            .iter()
            .any(|(vdi, sr)| source.get(vdi).is_some_and(|v| &v.sr != sr));
    if copy || storage_motion {
        let rows = vif_rows(source, &plan.vm, vm);
        let vif_map = plan
            .networks
            .iter()
            .filter_map(|(mac, net)| {
                rows.iter()
                    .find(|r| &r.mac == mac)
                    .map(|r| (r.vif.clone(), net.clone()))
            })
            .collect();
        return Some(FinishRequest::MigrateSend(MigrateSpec {
            vm: plan.vm.clone(),
            target: plan.target.connection,
            host,
            host_name,
            transfer_network: transfer_network.cloned().unwrap_or_else(Ref::null),
            vdi_map: plan
                .storage
                .iter()
                .map(|(v, s)| (v.clone(), s.clone()))
                .collect(),
            vif_map,
            copy,
        }));
    }
    Some(FinishRequest::PoolMigrate {
        vm: plan.vm.clone(),
        host,
    })
}

/// The disk map a feasibility check sends (`GetVdiMap` of the filter):
/// each non-ISO disk to the first SR of the destination that supports
/// storage migration and is not its own SR.
pub fn probe_vdi_map(
    source: &ConnectionModel,
    vm: &VmRecord,
    target: &ConnectionModel,
) -> Vec<(Ref<Vdi>, Ref<Sr>)> {
    let mut srs: Vec<(&Ref<Sr>, &Arc<SrRecord>)> = target
        .srs
        .iter()
        .filter(|(_, sr)| supports_storage_migration(target, sr))
        .collect();
    srs.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::new();
    for vbd in vm.vbds.iter().filter_map(|r| source.get(r)) {
        let Some(vdi) = source.get(&vbd.vdi) else {
            continue;
        };
        let Some(sr) = source.get(&vdi.sr) else {
            continue;
        };
        if sr.content_type == "iso" || sr.r#type == "iso" {
            continue;
        }
        if let Some((t, _)) = srs.iter().find(|(r, _)| **r != vdi.sr) {
            out.push((vbd.vdi.clone(), (*t).clone()));
        }
    }
    out
}

/// The interface map a feasibility check sends (`GetVifMap`): every
/// interface to one network the server sees; nothing within the pool.
pub fn probe_vif_map(
    source: &ConnectionModel,
    vm: &VmRecord,
    target: &ConnectionModel,
    host: &Ref<Host>,
    same_connection: bool,
) -> Vec<(Ref<Vif>, Ref<Network>)> {
    if same_connection {
        return Vec::new();
    }
    let mut nets: Vec<(&Ref<Network>, &Arc<NetworkRecord>)> = target.networks.iter().collect();
    nets.sort_by(|a, b| a.0.cmp(b.0));
    let sees = |n: &NetworkRecord| {
        n.pifs.is_empty()
            || n.pifs
                .iter()
                .filter_map(|p| target.get(p))
                .any(|p| &p.host == host)
    };
    let Some((net, _)) = nets.into_iter().find(|(_, n)| sees(n)) else {
        return Vec::new();
    };
    vm.vifs
        .iter()
        .filter(|r| source.get(*r).is_some())
        .map(|r| (r.clone(), net.clone()))
        .collect()
}

/// The management network of the destination (`migrate_receive` of the
/// feasibility check).
pub fn management_network(target: &ConnectionModel) -> Option<Ref<Network>> {
    let mut pifs: Vec<_> = target.pifs.iter().filter(|(_, p)| p.management).collect();
    pifs.sort_by(|a, b| a.0.cmp(b.0));
    pifs.first().map(|(_, p)| p.network.clone())
}

/// Whether a feasibility error is only about snapshot interfaces
/// (`VIF_NOT_IN_MAP` while snapshots have interfaces the VM lacks), which
/// the original accepts.
pub fn snapshots_have_extra_vifs(model: &ConnectionModel, vm: &VmRecord) -> bool {
    vm.snapshots
        .iter()
        .filter_map(|r| model.get(r))
        .flat_map(|s| s.vifs.iter())
        .any(|v| !vm.vifs.contains(v))
}

/// The VMs that went missing while the wizard was open
/// (`AllVMsAvailable`).
pub fn all_available(model: &ConnectionModel, vms: &[Ref<Vm>]) -> bool {
    !vms.is_empty() && vms.iter().all(|v| model.vms.contains_key(v))
}

/// The default name of an intra-pool copy (`GetDefaultCopyName`): "Copy of
/// {name}", made unique among the pool's VMs.
pub fn default_copy_name(model: &ConnectionModel, vm: &VmRecord, pattern: &str) -> String {
    let base = pattern.replace("{name}", &vm.name_label);
    crate::provisioning::unique_name(&base, crate::provisioning::vm_names(model))
}

/// The ISO-less disks of a VM (the intra-pool copy's SR picker).
pub fn copy_disks<'a>(
    model: &'a ConnectionModel,
    vm: &VmRecord,
) -> Vec<(&'a Ref<Vdi>, &'a VdiRecord)> {
    vm.vbds
        .iter()
        .filter_map(|r| model.get(r))
        .filter(|b| b.r#type != VbdType::Cd)
        .filter_map(|b| model.vdis.get_key_value(&b.vdi))
        .map(|(r, v)| (r, &**v))
        .collect()
}

/// The VBD of a VM that holds `vdi`.
pub fn vbd_of(model: &ConnectionModel, vm: &VmRecord, vdi: &Ref<Vdi>) -> Option<Ref<Vbd>> {
    vm.vbds
        .iter()
        .find(|r| model.get(*r).is_some_and(|b| &b.vdi == vdi))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    #[test]
    fn modes_and_commands() {
        let m = fixture();
        let (_, running) = m
            .vms
            .iter()
            .find(|(_, v)| v.power_state == VmPowerState::Running && !v.is_control_domain)
            .expect("a running VM");
        assert_eq!(move_mode(running), WizardMode::Move);
        assert!(!cross_pool_move_can_run(&m, running, false));
        let mut suspended = (**running).clone();
        suspended.power_state = VmPowerState::Suspended;
        assert_eq!(move_mode(&suspended), WizardMode::Migrate);
        let mut no_send = (**running).clone();
        no_send
            .allowed_operations
            .retain(|o| *o != VmOperations::MigrateSend);
        assert_eq!(can_migrate_send(&m, &no_send), Err(CantMigrate::NotAllowed));
    }

    #[test]
    fn combining_verdicts() {
        let refused = Refusal::Local(LocalRefusal::OlderServer);
        assert!(combine([Some(&refused), None]).is_none());
        assert!(combine([Some(&refused), Some(&refused)]).is_some());
        assert!(combine(std::iter::empty()).is_none());
    }

    #[test]
    fn extra_space() {
        let a: Ref<Sr> = Ref::new("OpaqueRef:a");
        let b: Ref<Sr> = Ref::new("OpaqueRef:b");
        assert!(!extra_space_needed(&a, &a, WizardMode::Migrate));
        assert!(extra_space_needed(&a, &a, WizardMode::Copy));
        assert!(extra_space_needed(&a, &b, WizardMode::Move));
    }
}
