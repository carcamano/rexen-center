//! vApps (`VM_appliance`, F12a): when the vApp commands are offered, the
//! Manage vApps dialog's and the New vApp wizard's lists, and the requests
//! they end with. How they run is [`crate::vapp_run`]'s.
//!
//! As the original:
//!
//! * **Manage vApps** (`VMAppliancesDialog`, Pool menu): the pool's vApps
//!   with their VMs; New vApp, Delete, Properties, Start, Shut Down,
//!   Export and Import.
//! * **New vApp** (`NewVMApplianceWizard`): Name, Virtual Machines (the
//!   pool's real VMs, with their current vApp), VM Startup Sequence (start
//!   order and delay per VM) and Finish; `CreateVMApplianceAction`, then
//!   `SetVMStartupOptionsAction` for the changed rows.
//! * **vApp Properties**: General (name, description), Virtual Machines
//!   and VM Startup Sequence, saved by `AssignVMsToVMApplianceAction` and
//!   `SetVMStartupOptionsAction`.
//! * **Start vApp / Shut Down vApp** (`VappStartCommand`,
//!   `VappShutDownCommand`) for a vApp or the VMs of one vApp, offered by
//!   the vApp's `allowed_operations`; shutting down asks first.
//! * **Assign to vApp** (`AssignGroupToolStripMenuItem<VM_appliance>`):
//!   New vApp... and each vApp; assigning VMs that belong to another vApp
//!   asks first (`ChangesOK`).

use std::sync::Arc;

use xapi::{Ref, Vm, VmAppliance, VmApplianceOperation, VmApplianceRecord, VmRecord};

use crate::model::ConnectionModel;
use crate::objects::{self, VmKind};

/// The vApps of a pool, sorted by name.
pub fn vapps(model: &ConnectionModel) -> Vec<(&Ref<VmAppliance>, &Arc<VmApplianceRecord>)> {
    let mut out: Vec<_> = model.vm_appliances.iter().collect();
    out.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label).then(a.0.cmp(b.0)));
    out
}

/// The vApp a VM belongs to.
pub fn vapp_of<'a>(
    model: &'a ConnectionModel,
    vm: &VmRecord,
) -> Option<(&'a Ref<VmAppliance>, &'a Arc<VmApplianceRecord>)> {
    model.vm_appliances.get_key_value(&vm.appliance)
}

/// The VMs of a vApp, sorted as the console sorts VMs.
pub fn members<'a>(
    model: &'a ConnectionModel,
    app: &VmApplianceRecord,
) -> Vec<(&'a Ref<Vm>, &'a Arc<VmRecord>)> {
    let mut out: Vec<_> = app
        .vms
        .iter()
        .filter_map(|r| model.vms.get_key_value(r))
        .collect();
    out.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
    out
}

/// `CanStartAppliance`.
pub fn can_start(app: &VmApplianceRecord) -> bool {
    app.allowed_operations
        .contains(&VmApplianceOperation::Start)
}

/// `CanShutDownAppliance`.
pub fn can_shut_down(app: &VmApplianceRecord) -> bool {
    app.allowed_operations
        .contains(&VmApplianceOperation::CleanShutdown)
        || app
            .allowed_operations
            .contains(&VmApplianceOperation::HardShutdown)
}

/// `VM.IsRealVm`: not a snapshot, template or control domain.
pub fn is_real_vm(vm: &VmRecord) -> bool {
    !vm.is_a_snapshot && !vm.is_a_template && !vm.is_control_domain
}

/// The vApp of the selected VMs, when they all belong to the same one
/// (`VappStartCommand` / `VappShutDownCommand` on VMs).
pub fn common_vapp<'a>(
    model: &'a ConnectionModel,
    vms: &[Ref<Vm>],
) -> Option<(&'a Ref<VmAppliance>, &'a Arc<VmApplianceRecord>)> {
    let first = model.vms.get(vms.first()?)?;
    let (r, app) = vapp_of(model, first)?;
    vms.iter()
        .all(|v| model.vms.get(v).is_some_and(|vm| &vm.appliance == r))
        .then_some((r, app))
}

/// Assign to vApp (`AssignVMsToGroup.CanRun`): a real VM this client is
/// not busy with.
pub fn can_assign(vm: &VmRecord, busy: bool) -> bool {
    !busy && objects::vm_kind(vm) == VmKind::Guest
}

/// The rows of the Virtual Machines page (`NewVMGroupVMsPage`): the
/// pool's real VMs that are shown, sorted.
pub fn selectable_vms(
    model: &ConnectionModel,
    show_hidden: bool,
) -> Vec<(&Ref<Vm>, &Arc<VmRecord>)> {
    let mut out: Vec<_> = model
        .vms
        .iter()
        .filter(|(_, v)| is_real_vm(v) && objects::vm_is_shown(v, show_hidden))
        .collect();
    out.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
    out
}

/// What `ChangesOK` asks before VMs of another vApp join `group`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeWarning {
    /// `CHANGE_VMS_APPLIANCE_WARNING`.
    One {
        vm: String,
        old: String,
        new: String,
    },
    /// `CHANGE_VMS_APPLIANCES_WARNING`.
    Many { new: String },
}

/// `AssignGroupToVMCommand.ChangesOK`: the VMs already in another vApp
/// than `group` (`None`: a new one named `name`).
pub fn change_warning(
    model: &ConnectionModel,
    vms: &[Ref<Vm>],
    group: Option<&Ref<VmAppliance>>,
    name: &str,
) -> Option<ChangeWarning> {
    let moving: Vec<(&VmRecord, &VmApplianceRecord)> = vms
        .iter()
        .filter_map(|r| model.vms.get(r))
        .filter_map(|vm| {
            let old = model.vm_appliances.get(&vm.appliance)?;
            (group != Some(&vm.appliance)).then_some((&**vm, &**old))
        })
        .collect();
    match moving.as_slice() {
        [] => None,
        [(vm, old)] => Some(ChangeWarning::One {
            vm: vm.name_label.clone(),
            old: old.name_label.clone(),
            new: name.to_owned(),
        }),
        _ => Some(ChangeWarning::Many {
            new: name.to_owned(),
        }),
    }
}

/// A row of the VM Startup Sequence page: the VM's start order and the
/// delay (seconds) before the next group starts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StartupRow {
    pub vm: Ref<Vm>,
    pub order: i64,
    pub start_delay: i64,
}

/// The rows of `vms` with their current settings (`VMStartupOptions`),
/// sorted by VM.
pub fn startup_rows(model: &ConnectionModel, vms: &[Ref<Vm>]) -> Vec<StartupRow> {
    let mut list: Vec<(&Ref<Vm>, &Arc<VmRecord>)> = vms
        .iter()
        .filter_map(|r| model.vms.get_key_value(r))
        .collect();
    list.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
    list.into_iter()
        .map(|(r, v)| StartupRow {
            vm: r.clone(),
            order: v.order,
            start_delay: v.start_delay,
        })
        .collect()
}

/// `GetChangedSettings`: the rows that differ from the VMs' settings.
pub fn changed_startup(model: &ConnectionModel, rows: &[StartupRow]) -> Vec<StartupRow> {
    rows.iter()
        .filter(|row| {
            model
                .vms
                .get(&row.vm)
                .is_some_and(|v| v.order != row.order || v.start_delay != row.start_delay)
        })
        .cloned()
        .collect()
}

/// The New vApp wizard's request (`CreateVMApplianceAction` and
/// `SetVMStartupOptionsAction`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NewVAppSpec {
    pub name: String,
    pub description: String,
    pub vms: Vec<Ref<Vm>>,
    /// Changed startup settings.
    pub startup: Vec<StartupRow>,
}

/// The vApp Properties dialog's changes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct VAppChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    /// The new members (`AssignVMsToVMApplianceAction`), when changed.
    pub members: Option<Vec<Ref<Vm>>>,
    /// Changed startup settings.
    pub startup: Vec<StartupRow>,
}

impl VAppChanges {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.members.is_none()
            && self.startup.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_with_vapp() -> (ConnectionModel, Ref<VmAppliance>, Vec<Ref<Vm>>) {
        let mut m = ConnectionModel::from_database(&xapi_mock::fixture_records());
        let guests: Vec<Ref<Vm>> = m
            .vms
            .iter()
            .filter(|(_, v)| is_real_vm(v))
            .map(|(r, _)| r.clone())
            .take(2)
            .collect();
        let app: Ref<VmAppliance> = Ref::new("OpaqueRef:app-1");
        m.vm_appliances.insert(
            app.clone(),
            Arc::new(VmApplianceRecord {
                name_label: "web".into(),
                vms: vec![guests[0].clone()],
                allowed_operations: vec![VmApplianceOperation::Start],
                ..Default::default()
            }),
        );
        let mut vm = (**m.vms.get(&guests[0]).unwrap()).clone();
        vm.appliance = app.clone();
        m.vms.insert(guests[0].clone(), Arc::new(vm));
        (m, app, guests)
    }

    #[test]
    fn common_vapp_and_warnings() {
        let (m, app, guests) = model_with_vapp();
        assert_eq!(common_vapp(&m, &guests[..1]).map(|(r, _)| r), Some(&app));
        assert!(common_vapp(&m, &guests).is_none());
        assert!(change_warning(&m, &guests[..1], Some(&app), "web").is_none());
        assert_eq!(
            change_warning(&m, &guests, None, "db"),
            Some(ChangeWarning::One {
                vm: m.vms[&guests[0]].name_label.clone(),
                old: "web".into(),
                new: "db".into()
            })
        );
        let app_rec = &m.vm_appliances[&app];
        assert!(can_start(app_rec));
        assert!(!can_shut_down(app_rec));
    }

    #[test]
    fn startup_changes() {
        let (m, _, guests) = model_with_vapp();
        let mut rows = startup_rows(&m, &guests);
        assert!(changed_startup(&m, &rows).is_empty());
        rows[0].order = 3;
        assert_eq!(changed_startup(&m, &rows).len(), 1);
    }
}
