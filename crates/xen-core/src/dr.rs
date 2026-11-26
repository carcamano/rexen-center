//! Disaster recovery (F12b): the Configure dialog's SRs, the Disaster
//! Recovery wizard's lists and pre-checks, and its report. How the calls
//! run is [`crate::dr_run`]'s.
//!
//! As the original:
//!
//! * **Configure** (`DRConfigureDialog`, Pool > Disaster Recovery): the
//!   pool's SRs that can hold its metadata
//!   (`SR.assert_supports_database_replication`), ticked when they do
//!   (a `metadata` VDI of this pool); at most 8; OK enables or disables the
//!   replication of the SRs whose tick changed.
//! * **Disaster Recovery wizard** (`DRFailoverWizard`): Welcome (Failover,
//!   Failback, Test Failover), Before You Start, Storage (the SRs that can
//!   hold metadata, and those found on iSCSI / Fibre Channel devices, with
//!   whether pool metadata was detected; the found ones are introduced by a
//!   `DR_task`), vApps and VMs (the pools whose metadata was found, with
//!   their vApps and VMs, and what to do after the recovery), Pre-checks,
//!   Recover and the Summary report. Cancelling, or leaving the Recover
//!   page, cleans up: the `DR_task`s are destroyed, a test failover's
//!   recovered VMs and vApps and the SRs it introduced go, and what a
//!   pre-check's fix changed is changed back.
//!
//! **Pre-checks** (the only `Diagnostics/Checks` the console ports, plan
//! §6): `DrHAEnabledCheck` (test failover), `RunningVmCheck` /
//! `RunningVmApplianceCheck` in the source pool when this console is
//! connected to it (failover and failback), `ExistingVmCheck` /
//! `ExistingVmApplianceCheck` in this pool and `AssertCanBeRecoveredCheck`
//! (asked of the server, see [`crate::Precheck::DrRecoverable`]). Each
//! problem says how it can be fixed, as `Problem.GetSolutionAction`.

use std::collections::BTreeMap;
use std::sync::Arc;

use xapi::{
    HostRecord, PoolRecord, Ref, Sr, SrRecord, Vdi, VdiType, Vm, VmAppliance, VmApplianceRecord,
    VmPowerState, VmRecord,
};

use crate::model::ConnectionModel;
use crate::objects;

/// `ACTION_START_VMS_AND_APPLIANCES_NONE_SELECTED` (a client error).
pub const NOTHING_TO_START: &str = "XENADMIN_DR_NOTHING_TO_START";

/// `MAX_SR_SELECTED` of the Configure dialog.
pub const MAX_SRS: usize = 8;

/// `DRWizardType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WizardType {
    Failover,
    Failback,
    Dryrun,
}

/// `StartActionAfterRecovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AfterRecovery {
    Start,
    StartPaused,
    DoNotStart,
}

// ---- Configure --------------------------------------------------------------------------

/// Whether this pool replicates its metadata to `sr` (`SrRow.IsDrEnabled`:
/// a `metadata` VDI of this pool on it).
pub fn is_dr_enabled(model: &ConnectionModel, sr: &Ref<Sr>) -> bool {
    let Some((pool, _)) = model.pool() else {
        return false;
    };
    model
        .vdis
        .values()
        .any(|v| v.r#type == VdiType::Metadata && &v.metadata_of_pool == pool && &v.sr == sr)
}

/// The SRs the Configure dialog and the Storage page ask the server about
/// (`Connection.Cache.SRs`), sorted by name.
pub fn candidate_srs(model: &ConnectionModel) -> Vec<Ref<Sr>> {
    let mut out: Vec<(&Ref<Sr>, &Arc<SrRecord>)> = model.srs.iter().collect();
    out.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label).then(a.0.cmp(b.0)));
    out.into_iter().map(|(r, _)| r.clone()).collect()
}

/// What the Configure dialog shows under the list (`ToggleWarningsVisibleState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureNote {
    /// The information line; OK enabled.
    Info,
    /// Every SR unticked while replication was on: the warning; OK enabled.
    Disabling,
    /// More than [`MAX_SRS`] ticked: OK disabled.
    TooMany,
}

/// `ToggleWarningsVisibleState`.
pub fn configure_note(originally_enabled: bool, checked: usize) -> ConfigureNote {
    if checked > MAX_SRS {
        ConfigureNote::TooMany
    } else if originally_enabled && checked == 0 {
        ConfigureNote::Disabling
    } else {
        ConfigureNote::Info
    }
}

// ---- Storage page -------------------------------------------------------------------------

/// An SR found on a scanned device (`SR.SRInfo` with the device).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScannedSr {
    pub uuid: String,
    pub name_label: String,
    pub name_description: String,
    pub pool_metadata_detected: bool,
    /// `lvmoiscsi` or `lvmohba`.
    pub sr_type: String,
    /// The device's `device_config` (the `DR_task`'s).
    pub device_config: crate::storage::DeviceConfig,
}

/// A row of the Storage page (`SrRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRow {
    pub uuid: String,
    pub name: String,
    pub description: String,
    /// The SR's type (its `type` field, or the scanned device's).
    pub sr_type: String,
    pub metadata: bool,
    /// Found by a scan, not yet in this pool.
    pub scanned: Option<ScannedSr>,
}

/// The Storage page's rows: the SRs of this pool that can hold metadata
/// (`supported`), with whether one holds a metadata VDI, then the scanned
/// ones this pool does not have; the ones with metadata first (`SortRows`).
pub fn storage_rows(
    model: &ConnectionModel,
    supported: &[Ref<Sr>],
    scanned: &[ScannedSr],
) -> Vec<StorageRow> {
    let mut rows: Vec<StorageRow> = supported
        .iter()
        .filter_map(|r| model.srs.get(r).map(|sr| (r, sr)))
        .map(|(_, sr)| {
            let metadata = sr.vdis.iter().any(|v| {
                model
                    .vdis
                    .get(v)
                    .is_some_and(|vdi| vdi.r#type == VdiType::Metadata)
            });
            StorageRow {
                uuid: sr.uuid.clone(),
                name: sr.name_label.clone(),
                description: sr.name_description.clone(),
                sr_type: sr.r#type.clone(),
                metadata,
                scanned: None,
            }
        })
        .collect();
    for s in scanned {
        if rows.iter().any(|r| r.uuid == s.uuid) {
            continue;
        }
        rows.push(StorageRow {
            uuid: s.uuid.clone(),
            name: s.name_label.clone(),
            description: s.name_description.clone(),
            sr_type: s.sr_type.clone(),
            metadata: s.pool_metadata_detected,
            scanned: Some(s.clone()),
        });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.metadata));
    rows
}

/// `SelectedNewDevices`: the scanned SRs chosen, by device (one `DR_task`
/// per device, for its chosen SRs).
pub fn devices_to_introduce(rows: &[StorageRow], selected: &[String]) -> Vec<DrTaskSpec> {
    let mut out: Vec<DrTaskSpec> = Vec::new();
    for row in rows.iter().filter(|r| selected.contains(&r.uuid)) {
        let Some(s) = &row.scanned else { continue };
        match out
            .iter_mut()
            .find(|d| d.sr_type == s.sr_type && d.device_config == s.device_config)
        {
            Some(d) => d.srs.push(s.uuid.clone()),
            None => out.push(DrTaskSpec {
                sr_type: s.sr_type.clone(),
                device_config: s.device_config.clone(),
                srs: vec![s.uuid.clone()],
            }),
        }
    }
    out
}

/// A `DR_task.create`'s arguments (`ScannedDeviceInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DrTaskSpec {
    pub sr_type: String,
    pub device_config: crate::storage::DeviceConfig,
    /// The SR uuids to introduce.
    pub srs: Vec<String>,
}

// ---- metadata -------------------------------------------------------------------------------

/// The metadata of one pool, read from a metadata VDI (`PoolMetadata`).
/// The references of `vm_appliances` and `vms` are the database's.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolMetadata {
    /// The metadata VDI, in this pool.
    pub vdi: Ref<Vdi>,
    pub vdi_name: String,
    pub sr_name: String,
    pub pool: PoolRecord,
    /// The pool's name, else its coordinator's (`VdiLoadMetadataAction`).
    pub pool_name: String,
    pub vm_appliances: Vec<(Ref<VmAppliance>, VmApplianceRecord)>,
    /// Real VMs only.
    pub vms: Vec<(Ref<Vm>, VmRecord)>,
}

impl PoolMetadata {
    pub fn vm(&self, r: &Ref<Vm>) -> Option<&VmRecord> {
        self.vms.iter().find(|(x, _)| x == r).map(|(_, v)| v)
    }

    pub fn vapp(&self, r: &Ref<VmAppliance>) -> Option<&VmApplianceRecord> {
        self.vm_appliances
            .iter()
            .find(|(x, _)| x == r)
            .map(|(_, a)| a)
    }

    /// The VMs not in a vApp (`IsAssignedToVapp`), as the tree lists them.
    pub fn loose_vms(&self) -> Vec<&(Ref<Vm>, VmRecord)> {
        self.vms
            .iter()
            .filter(|(_, v)| {
                v.appliance.is_null() || !self.vm_appliances.iter().any(|(a, _)| *a == v.appliance)
            })
            .collect()
    }
}

/// `VdiLoadMetadataAction`'s pool name: its own, else its coordinator's.
pub fn metadata_pool_name(pool: &PoolRecord, coordinator: Option<&HostRecord>) -> String {
    if pool.name_label.is_empty() {
        coordinator
            .map(|h| h.name_label.clone())
            .unwrap_or_default()
    } else {
        pool.name_label.clone()
    }
}

/// What can be recovered from one metadata VDI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DrItem {
    Vm(Ref<Vm>),
    VApp(Ref<VmAppliance>),
}

/// The vApps and VMs chosen in one pool's metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chosen {
    pub vapps: Vec<Ref<VmAppliance>>,
    pub vms: Vec<Ref<Vm>>,
}

impl Chosen {
    pub fn is_empty(&self) -> bool {
        self.vapps.is_empty() && self.vms.is_empty()
    }

    /// What is recovered, in the order of the Recover page: vApps first,
    /// then the VMs not in a vApp.
    pub fn items(&self, meta: &PoolMetadata) -> Vec<DrItem> {
        let mut out: Vec<DrItem> = meta
            .vm_appliances
            .iter()
            .filter(|(r, _)| self.vapps.contains(r))
            .map(|(r, _)| DrItem::VApp(r.clone()))
            .collect();
        out.extend(
            meta.loose_vms()
                .into_iter()
                .filter(|(r, _)| self.vms.contains(r))
                .map(|(r, _)| DrItem::Vm(r.clone())),
        );
        out
    }
}

/// The name of an item of `meta`.
pub fn item_name(meta: &PoolMetadata, item: &DrItem) -> String {
    match item {
        DrItem::Vm(r) => meta.vm(r).map(|v| v.name_label.clone()),
        DrItem::VApp(r) => meta.vapp(r).map(|a| a.name_label.clone()),
    }
    .unwrap_or_default()
}

/// The uuid of an item of `meta`.
pub fn item_uuid(meta: &PoolMetadata, item: &DrItem) -> String {
    match item {
        DrItem::Vm(r) => meta.vm(r).map(|v| v.uuid.clone()),
        DrItem::VApp(r) => meta.vapp(r).map(|a| a.uuid.clone()),
    }
    .unwrap_or_default()
}

// ---- pre-checks -------------------------------------------------------------------------------

/// Who a pre-check is about (its `Description`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckSubject {
    /// `DR_HA_CHECK_DESCRIPTION`.
    Ha { pool: String },
    /// `DR_WIZARD_VM_CHECK_DESCRIPTION`.
    Vm { name: String },
    /// `DR_WIZARD_APPLIANCE_CHECK_DESCRIPTION`.
    VApp { name: String },
}

/// A group of pre-checks (`DR_WIZARD_CHECKING_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckGroup {
    HaStatus,
    SourcePowerState,
    Existing { pool: String },
    Recoverable { pool: String },
}

/// A fix that runs on another connection than this pool's (the source
/// pool's), or here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Where {
    Here,
    Source(crate::backend::ConnectionId),
}

/// A pre-check's problem or warning, with its fix (`Problem`).
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// `DrHAEnabledProblem`: disable HA; turned on again at the end.
    HaEnabled {
        heartbeat_sr: Option<Ref<Sr>>,
        ntol: i64,
    },
    /// `RunningVMProblem` (`hard`: forced shut down).
    RunningVm {
        at: Where,
        vm: Ref<Vm>,
        pool: String,
        hard: bool,
        clean: bool,
    },
    /// `RunningVmApplianceProblem`.
    RunningVApp {
        at: Where,
        vapp: Ref<VmAppliance>,
        pool: String,
        hard: bool,
    },
    /// `ExistingVmProblem`: a VM as new or newer is here; delete it.
    ExistingVm {
        vm: Ref<Vm>,
        name: String,
        pool: String,
    },
    /// `ExistingVmApplianceProblem`.
    ExistingVApp {
        vms: Vec<(Ref<Vm>, String)>,
        pool: String,
    },
    /// `ExistingVmWarning` / `ExistingVmApplianceWarning` (it will be
    /// replaced).
    ExistingWarning { pool: String },
    /// `MissingSRProblem`: `sr` `None` when the server said nothing more.
    MissingSr {
        sr: Option<MissingSr>,
        fix: Option<DrTaskSpec>,
    },
    /// `MissingMultipleFCSRsProblem`.
    MissingSrs { fixes: Vec<DrTaskSpec> },
}

/// An SR a recovery needs, as the metadata describes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MissingSr {
    pub uuid: String,
    pub name: String,
    pub shared: bool,
    pub sr_type: String,
    /// The `device_config` of its first PBD, if the metadata has one.
    pub device_config: Option<crate::storage::DeviceConfig>,
}

impl Problem {
    /// A warning does not stop the wizard.
    pub fn is_warning(&self) -> bool {
        matches!(self, Self::ExistingWarning { .. })
    }

    /// `IsFixable` with a help message.
    pub fn is_fixable(&self) -> bool {
        match self {
            Self::ExistingWarning { .. } => false,
            Self::MissingSr { fix, sr } => fix.is_some() && sr.as_ref().is_some_and(|s| s.shared),
            Self::MissingSrs { fixes } => !fixes.is_empty(),
            _ => true,
        }
    }
}

/// One row of the Pre-checks page.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub subject: CheckSubject,
    pub problem: Option<Problem>,
}

/// What `AssertCanBeRecoveredCheck` asks the server, and its answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Recoverable {
    Yes,
    /// `VM_REQUIRES_SR`: the SR named in the error (`None`: unknown to the
    /// metadata) and the SRs required.
    NeedsSrs {
        sr: Option<MissingSr>,
        required: Vec<MissingSr>,
    },
}

/// `AssertCanBeRecoveredCheck.RunCheck` from the server's answer.
pub fn recoverable_problem(answer: &Recoverable) -> Option<Problem> {
    let Recoverable::NeedsSrs { sr, required } = answer else {
        return None;
    };
    let Some(sr) = sr else {
        return Some(Problem::MissingSr {
            sr: None,
            fix: None,
        });
    };
    let mut required = required.clone();
    if !required.iter().any(|r| r.uuid == sr.uuid) {
        required.push(sr.clone());
    }
    if let Some(local) = required.iter().find(|r| !r.shared) {
        return Some(Problem::MissingSr {
            sr: Some(local.clone()),
            fix: None,
        });
    }
    // The original offers a `DR_task` for Fibre Channel SRs and the New SR
    // wizard for the others; this console offers the `DR_task` for every
    // shared SR whose device configuration the metadata has.
    let fixes: Vec<DrTaskSpec> = required
        .iter()
        .filter_map(|r| {
            r.device_config.as_ref().map(|d| DrTaskSpec {
                sr_type: r.sr_type.clone(),
                device_config: d.clone(),
                srs: vec![r.uuid.clone()],
            })
        })
        .collect();
    match fixes.len() {
        0 => Some(Problem::MissingSr {
            sr: required.first().cloned(),
            fix: None,
        }),
        1 => {
            let fix = fixes.into_iter().next();
            let uuid = fix.as_ref().and_then(|f| f.srs.first().cloned());
            Some(Problem::MissingSr {
                sr: required
                    .iter()
                    .find(|r| Some(&r.uuid) == uuid.as_ref())
                    .cloned(),
                fix,
            })
        }
        _ => Some(Problem::MissingSrs { fixes }),
    }
}

fn running(vm: &VmRecord) -> bool {
    matches!(
        vm.power_state,
        VmPowerState::Running | VmPowerState::Paused | VmPowerState::Suspended
    )
}

/// `VM_appliance.IsRunning`: a member running, paused or suspended.
fn vapp_running(model: &ConnectionModel, app: &VmApplianceRecord) -> bool {
    app.vms
        .iter()
        .any(|v| model.vms.get(v).is_some_and(|vm| running(vm)))
}

/// `RunningVmCheck` against the source pool's cache.
pub fn running_vm_check(source: &ConnectionModel, at: Where, vm: &VmRecord) -> Option<Problem> {
    let pool = pool_name(source);
    let (r, existing) = source.vms.iter().find(|(_, v)| v.uuid == vm.uuid)?;
    running(existing).then(|| Problem::RunningVm {
        at,
        vm: r.clone(),
        pool,
        hard: false,
        clean: existing
            .allowed_operations
            .contains(&xapi::VmOperations::CleanShutdown),
    })
}

/// `RunningVmApplianceCheck` against the source pool's cache.
pub fn running_vapp_check(
    source: &ConnectionModel,
    at: Where,
    app: &VmApplianceRecord,
) -> Option<Problem> {
    let (r, existing) = source
        .vm_appliances
        .iter()
        .find(|(_, a)| a.uuid == app.uuid)?;
    vapp_running(source, existing).then(|| Problem::RunningVApp {
        at,
        vapp: r.clone(),
        pool: pool_name(source),
        hard: false,
    })
}

fn pool_name(model: &ConnectionModel) -> String {
    model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default()
}

/// `ExistingVmCheck` in this pool.
pub fn existing_vm_check(model: &ConnectionModel, vm: &VmRecord) -> Option<Problem> {
    let pool = pool_name(model);
    let (r, existing) = model.vms.iter().find(|(_, v)| v.uuid == vm.uuid)?;
    Some(if existing.version >= vm.version {
        Problem::ExistingVm {
            vm: r.clone(),
            name: existing.name_label.clone(),
            pool,
        }
    } else if running(existing) {
        Problem::RunningVm {
            at: Where::Here,
            vm: r.clone(),
            pool,
            hard: true,
            clean: false,
        }
    } else {
        Problem::ExistingWarning { pool }
    })
}

/// `ExistingVmApplianceCheck` in this pool (`vms`: the vApp's VMs in the
/// metadata).
pub fn existing_vapp_check(
    model: &ConnectionModel,
    app: &VmApplianceRecord,
    vms: &[&VmRecord],
) -> Option<Problem> {
    let pool = pool_name(model);
    let newer: Vec<(Ref<Vm>, String)> = vms
        .iter()
        .filter_map(|vm| {
            let (r, existing) = model.vms.iter().find(|(_, v)| v.uuid == vm.uuid)?;
            (existing.version >= vm.version).then(|| (r.clone(), existing.name_label.clone()))
        })
        .collect();
    if !newer.is_empty() {
        return Some(Problem::ExistingVApp { vms: newer, pool });
    }
    let (r, existing) = model
        .vm_appliances
        .iter()
        .find(|(_, a)| a.uuid == app.uuid)?;
    Some(if vapp_running(model, existing) {
        Problem::RunningVApp {
            at: Where::Here,
            vapp: r.clone(),
            pool,
            hard: true,
        }
    } else {
        Problem::ExistingWarning { pool }
    })
}

/// `DrHAEnabledCheck` (test failover).
pub fn ha_check(model: &ConnectionModel) -> Option<Problem> {
    let (_, pool) = model.pool()?;
    pool.ha_enabled.then(|| Problem::HaEnabled {
        heartbeat_sr: crate::ha::heartbeat_srs(model, pool)
            .into_iter()
            .next()
            .map(|(r, _)| r),
        ntol: pool.ha_host_failures_to_tolerate,
    })
}

/// The cache pre-checks of the wizard, by group, in the original's order
/// (`GenerateChecks`); the "can be recovered" group comes from the server.
/// `sources`: the connected pools by pool uuid (their connection and
/// cache).
pub fn cache_checks(
    wizard: WizardType,
    model: &ConnectionModel,
    selected: &[(PoolMetadata, Chosen)],
    sources: &BTreeMap<String, (crate::backend::ConnectionId, Arc<ConnectionModel>)>,
) -> Vec<(CheckGroup, Vec<CheckRow>)> {
    let mut out = Vec::new();
    let pool = pool_name(model);
    if wizard == WizardType::Dryrun {
        out.push((
            CheckGroup::HaStatus,
            vec![CheckRow {
                subject: CheckSubject::Ha { pool: pool.clone() },
                problem: ha_check(model),
            }],
        ));
    } else {
        let mut rows = Vec::new();
        for (meta, chosen) in selected {
            let Some((id, source)) = sources.get(&meta.pool.uuid) else {
                continue;
            };
            for (r, app) in &meta.vm_appliances {
                if chosen.vapps.contains(r) {
                    rows.push(CheckRow {
                        subject: CheckSubject::VApp {
                            name: app.name_label.clone(),
                        },
                        problem: running_vapp_check(source, Where::Source(*id), app),
                    });
                }
            }
            for (r, vm) in meta.loose_vms() {
                if chosen.vms.contains(r) {
                    rows.push(CheckRow {
                        subject: CheckSubject::Vm {
                            name: vm.name_label.clone(),
                        },
                        problem: running_vm_check(source, Where::Source(*id), vm),
                    });
                }
            }
        }
        if !rows.is_empty() {
            out.push((CheckGroup::SourcePowerState, rows));
        }
    }
    let mut rows = Vec::new();
    for (meta, chosen) in selected {
        for (r, app) in &meta.vm_appliances {
            if !chosen.vapps.contains(r) {
                continue;
            }
            let vms: Vec<&VmRecord> = app.vms.iter().filter_map(|v| meta.vm(v)).collect();
            rows.push(CheckRow {
                subject: CheckSubject::VApp {
                    name: app.name_label.clone(),
                },
                problem: existing_vapp_check(model, app, &vms),
            });
        }
        for (r, vm) in meta.loose_vms() {
            if chosen.vms.contains(r) {
                rows.push(CheckRow {
                    subject: CheckSubject::Vm {
                        name: vm.name_label.clone(),
                    },
                    problem: existing_vm_check(model, vm),
                });
            }
        }
    }
    if !rows.is_empty() {
        out.push((CheckGroup::Existing { pool: pool.clone() }, rows));
    }
    out
}

// ---- the report --------------------------------------------------------------------------------

/// `SummaryReport`: lines with an indent (a bullet below level 0) and an
/// optional time stamp.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    lines: Vec<String>,
}

impl Report {
    /// `AddLine(line, indent, timeStamp)`; `stamp` is the time text.
    pub fn line(&mut self, text: &str, indent: usize, stamp: Option<&str>) {
        let mut prefix = String::new();
        if indent > 0 {
            prefix = "  ".repeat(indent);
            prefix.push_str("\u{2022} ");
        }
        self.lines.push(match stamp {
            Some(t) => format!("{t} - {prefix}{text}"),
            None => format!("{prefix}{text}"),
        });
    }

    pub fn text(&self) -> String {
        let mut s = self.lines.join("\n");
        s.push('\n');
        s
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// A line as it is (already indented or stamped).
    pub fn push_raw(&mut self, line: &str) {
        self.lines.push(line.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_notes() {
        assert_eq!(configure_note(false, 0), ConfigureNote::Info);
        assert_eq!(configure_note(true, 0), ConfigureNote::Disabling);
        assert_eq!(configure_note(true, 9), ConfigureNote::TooMany);
        assert_eq!(configure_note(false, 8), ConfigureNote::Info);
    }

    #[test]
    fn missing_srs_become_the_original_problems() {
        let sr = |uuid: &str, shared: bool, dconf: bool| MissingSr {
            uuid: uuid.into(),
            name: uuid.into(),
            shared,
            sr_type: "nfs".into(),
            device_config: dconf
                .then(|| crate::storage::DeviceConfig(vec![("server".into(), "x".into())])),
        };
        assert!(recoverable_problem(&Recoverable::Yes).is_none());
        let p = recoverable_problem(&Recoverable::NeedsSrs {
            sr: None,
            required: vec![],
        });
        assert_eq!(
            p,
            Some(Problem::MissingSr {
                sr: None,
                fix: None
            })
        );
        // A local SR: no fix.
        let p = recoverable_problem(&Recoverable::NeedsSrs {
            sr: Some(sr("a", true, true)),
            required: vec![sr("l", false, false)],
        })
        .unwrap();
        assert!(matches!(&p, Problem::MissingSr { sr: Some(s), fix: None } if s.uuid == "l"));
        assert!(!p.is_fixable());
        let p = recoverable_problem(&Recoverable::NeedsSrs {
            sr: Some(sr("a", true, true)),
            required: vec![],
        })
        .unwrap();
        assert!(p.is_fixable());
        let p = recoverable_problem(&Recoverable::NeedsSrs {
            sr: Some(sr("a", true, true)),
            required: vec![sr("b", true, true)],
        })
        .unwrap();
        assert!(matches!(p, Problem::MissingSrs { fixes } if fixes.len() == 2));
    }

    /// A model with a vApp holding VM `vm` (`power` its state, `version`).
    fn with_vapp(
        power: VmPowerState,
        version: i64,
    ) -> (ConnectionModel, VmApplianceRecord, VmRecord) {
        let mut m = ConnectionModel::from_database(&xapi_mock::fixture_records());
        let r: Ref<Vm> = Ref::new("OpaqueRef:vm-web");
        let vm = (**m.vms.get(&r).unwrap()).clone();
        let app_ref: Ref<VmAppliance> = Ref::new("OpaqueRef:app-dr");
        let app = VmApplianceRecord {
            uuid: "app-uuid".into(),
            name_label: "stack".into(),
            vms: vec![r.clone()],
            ..Default::default()
        };
        m.vm_appliances
            .insert(app_ref.clone(), Arc::new(app.clone()));
        let mut vm = vm;
        vm.power_state = power;
        vm.version = version;
        vm.appliance = app_ref;
        m.vms.insert(r, Arc::new(vm.clone()));
        (m, app, vm)
    }

    #[test]
    fn vapp_checks_follow_the_original() {
        // Source pool: a member runs.
        let (source, app, vm) = with_vapp(VmPowerState::Running, 1);
        assert!(matches!(
            running_vapp_check(&source, Where::Source(7), &app),
            Some(Problem::RunningVApp {
                at: Where::Source(7),
                hard: false,
                ..
            })
        ));
        let (halted, _, _) = with_vapp(VmPowerState::Halted, 1);
        assert!(running_vapp_check(&halted, Where::Here, &app).is_none());
        // This pool: as new a VM is a problem (delete it) ...
        let mut meta_vm = vm.clone();
        meta_vm.version = 1;
        let (here, _, _) = with_vapp(VmPowerState::Halted, 1);
        let p = existing_vapp_check(&here, &app, &[&meta_vm]).unwrap();
        assert!(matches!(&p, Problem::ExistingVApp { vms, .. } if vms.len() == 1));
        assert!(p.is_fixable() && !p.is_warning());
        // ... an older one is replaced: a warning, or a forced shut down
        // while the vApp runs.
        meta_vm.version = 2;
        assert!(matches!(
            existing_vapp_check(&here, &app, &[&meta_vm]),
            Some(Problem::ExistingWarning { .. })
        ));
        let (running_here, _, _) = with_vapp(VmPowerState::Running, 1);
        assert!(matches!(
            existing_vapp_check(&running_here, &app, &[&meta_vm]),
            Some(Problem::RunningVApp {
                at: Where::Here,
                hard: true,
                ..
            })
        ));
        // A VM: running here and older is forced off.
        assert!(matches!(
            existing_vm_check(&running_here, &meta_vm),
            Some(Problem::RunningVm {
                at: Where::Here,
                hard: true,
                ..
            })
        ));
        // HA on: a dry run's problem.
        let mut ha = ConnectionModel::from_database(&xapi_mock::fixture_records());
        let (pr, mut pool) = ha.pool().map(|(r, p)| (r.clone(), (**p).clone())).unwrap();
        pool.ha_enabled = true;
        ha.pools.insert(pr, Arc::new(pool));
        assert!(matches!(ha_check(&ha), Some(Problem::HaEnabled { .. })));
    }

    #[test]
    fn the_report_indents_with_bullets() {
        let mut r = Report::default();
        r.line("Selected SRs:", 0, None);
        r.line("nfs", 1, None);
        r.line("done", 0, Some("Nov 25, 2026 12:00:00 PM"));
        assert_eq!(
            r.text(),
            "Selected SRs:\n  \u{2022} nfs\nNov 25, 2026 12:00:00 PM - done\n"
        );
    }
}
