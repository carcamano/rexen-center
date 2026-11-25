//! The VM and host operations of the console, and when each is offered.
//!
//! The rules follow the original console's commands (`XenAdmin/Commands`):
//! an operation is offered when the server lists it in the object's
//! `allowed_operations` and the original's extra conditions hold (a live
//! host, an enabled host to run on…). [`ActionRequest::rbac_methods`] lists
//! the API calls an operation makes, for the RBAC check made before it runs
//! (see [`crate::rbac`]).

use xapi::{
    Host, HostAllowedOperations, HostRecord, Ref, Vm, VmOperations, VmPowerState, VmRecord,
};

use crate::model::{ConnectionModel, is_guest};
use crate::objects;

/// VM operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VmAction {
    Start,
    CleanShutdown,
    CleanReboot,
    Suspend,
    Resume,
    Pause,
    Unpause,
    /// Force Shut Down.
    HardShutdown,
    /// Force Reboot.
    HardReboot,
    /// Start on a chosen server.
    StartOn(Ref<Host>),
    /// Resume on a chosen server.
    ResumeOn(Ref<Host>),
    /// Live migration to another server of the same pool.
    Migrate(Ref<Host>),
}

impl VmAction {
    /// The menu this action belongs to when it targets a chosen server.
    pub fn host_menu(&self) -> Option<HostMenu> {
        match self {
            Self::StartOn(_) => Some(HostMenu::StartOn),
            Self::ResumeOn(_) => Some(HostMenu::ResumeOn),
            Self::Migrate(_) => Some(HostMenu::Migrate),
            _ => None,
        }
    }

    /// The server this action targets, if any.
    pub fn target(&self) -> Option<&Ref<Host>> {
        match self {
            Self::StartOn(h) | Self::ResumeOn(h) | Self::Migrate(h) => Some(h),
            _ => None,
        }
    }
}

/// The VM submenus that list the pool's servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostMenu {
    StartOn,
    ResumeOn,
    Migrate,
}

impl HostMenu {
    /// The action for server `host`.
    pub fn action(self, host: Ref<Host>) -> VmAction {
        match self {
            Self::StartOn => VmAction::StartOn(host),
            Self::ResumeOn => VmAction::ResumeOn(host),
            Self::Migrate => VmAction::Migrate(host),
        }
    }
}

/// Host operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostAction {
    Reboot,
    Shutdown,
    PowerOn,
    /// Disable the host and migrate its VMs away (`host.evacuate`).
    EnterMaintenance,
    ExitMaintenance,
    RestartToolstack,
}

/// One operation on one object.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionRequest {
    Vm { vm: Ref<Vm>, action: VmAction },
    Host { host: Ref<Host>, action: HostAction },
}

/// `task.destroy`, called once a task has been followed to its end.
const TASK: &[&str] = &["task.destroy"];

impl ActionRequest {
    /// The API calls the operation makes, as RBAC permission names
    /// (lowercase `class.method`, without `async_`). The original lists the
    /// same calls in each action's `ApiMethodsToRoleCheck`.
    pub fn rbac_methods(&self) -> Vec<&'static str> {
        let own: &[&str] = match self {
            Self::Vm { action, .. } => match action {
                VmAction::Start => &["vm.start"],
                VmAction::CleanShutdown => &["vm.clean_shutdown"],
                VmAction::CleanReboot => &["vm.clean_reboot"],
                VmAction::Suspend => &["vm.suspend"],
                VmAction::Resume => &["vm.resume"],
                VmAction::Pause => &["vm.pause"],
                VmAction::Unpause => &["vm.unpause"],
                VmAction::HardShutdown => &["vm.hard_shutdown"],
                VmAction::HardReboot => &["vm.hard_reboot"],
                VmAction::StartOn(_) => &["vm.start_on"],
                VmAction::ResumeOn(_) => &["vm.resume_on"],
                VmAction::Migrate(_) => &["vm.pool_migrate"],
            },
            Self::Host { action, .. } => match action {
                HostAction::Reboot => &[
                    "host.disable",
                    "host.enable",
                    "vm.clean_shutdown",
                    "vm.hard_shutdown",
                    "host.reboot",
                ],
                HostAction::Shutdown => &[
                    "host.disable",
                    "host.enable",
                    "vm.clean_shutdown",
                    "vm.hard_shutdown",
                    "host.shutdown",
                ],
                HostAction::PowerOn => &["host.power_on"],
                HostAction::EnterMaintenance => &[
                    "host.disable",
                    "host.enable",
                    "host.evacuate",
                    "host.add_to_other_config",
                    "host.remove_from_other_config",
                ],
                HostAction::ExitMaintenance => &["host.enable", "host.remove_from_other_config"],
                HostAction::RestartToolstack => &["host.restart_agent"],
            },
        };
        own.iter().chain(TASK).copied().collect()
    }
}

// ---- when an operation is offered ------------------------------------------

fn allowed(vm: &VmRecord, op: VmOperations) -> bool {
    vm.allowed_operations.contains(&op)
}

/// `Helpers.EnabledTargetExists`: the VM's host is enabled, or (when it has
/// none) some host of the pool is.
fn enabled_target_exists(model: &ConnectionModel, host: Option<&Ref<Host>>) -> bool {
    match host.and_then(|h| model.hosts.get(h)) {
        Some(h) => h.enabled,
        None => model.hosts.values().any(|h| h.enabled),
    }
}

/// `VM.GetStorageHost`: the host whose local storage holds one of the VM's
/// disks, if any (a halted VM can only start there).
pub fn storage_host<'a>(model: &'a ConnectionModel, vm: &VmRecord) -> Option<&'a Ref<Host>> {
    vm.vbds
        .iter()
        .filter_map(|r| model.get(r))
        .filter(|vbd| vbd.r#type != xapi::VbdType::Cd)
        .filter_map(|vbd| model.get(&vbd.vdi).and_then(|vdi| model.get(&vdi.sr)))
        .find(|sr| objects::is_local_sr(sr))
        .and_then(|sr| objects::sr_home(model, sr))
}

/// Whether `action` is offered for `vm`. `busy` means this client has an
/// operation running on it: everything but the forced operations waits
/// (`VM.Locked` in the original); the forced ones are then offered even
/// when the server does not list them (CA-16960 in the original).
pub fn vm_action_available(
    model: &ConnectionModel,
    vm: &VmRecord,
    action: &VmAction,
    busy: bool,
) -> bool {
    if !is_guest(vm) {
        return false;
    }
    let home = objects::vm_home(model, vm);
    let running = vm.power_state == VmPowerState::Running;
    match action {
        VmAction::HardShutdown => (running && busy) || allowed(vm, VmOperations::HardShutdown),
        VmAction::HardReboot => {
            (running && busy)
                || (allowed(vm, VmOperations::HardReboot) && enabled_target_exists(model, home))
        }
        _ if busy => false,
        VmAction::Start => {
            vm.power_state == VmPowerState::Halted
                && allowed(vm, VmOperations::Start)
                && enabled_target_exists(model, storage_host(model, vm))
        }
        VmAction::CleanShutdown => allowed(vm, VmOperations::CleanShutdown),
        VmAction::CleanReboot => {
            allowed(vm, VmOperations::CleanReboot) && enabled_target_exists(model, home)
        }
        VmAction::Suspend => running && allowed(vm, VmOperations::Suspend),
        VmAction::Resume => {
            vm.power_state == VmPowerState::Suspended
                && allowed(vm, VmOperations::Resume)
                && enabled_target_exists(model, home)
        }
        VmAction::Pause => allowed(vm, VmOperations::Pause),
        VmAction::Unpause => allowed(vm, VmOperations::Unpause),
        VmAction::StartOn(h) | VmAction::ResumeOn(h) | VmAction::Migrate(h) => {
            let menu = action.host_menu().expect("host menu action");
            host_menu_available(model, vm, menu, busy)
                && model
                    .hosts
                    .get(h)
                    .is_some_and(|host| target_host_problem(model, vm, h, host, menu).is_none())
        }
    }
}

/// Whether the "Start on Server", "Resume on Server" or "Migrate to Server"
/// submenu is offered for `vm`.
///
/// The original offers Start/Resume on Server only in pools of two or more
/// servers; here one server is enough (choosing it is harmless and lets a
/// single-host pool start a VM on a named server).
pub fn host_menu_available(
    model: &ConnectionModel,
    vm: &VmRecord,
    menu: HostMenu,
    busy: bool,
) -> bool {
    if !is_guest(vm) || busy || model.hosts.is_empty() {
        return false;
    }
    let home = objects::vm_home(model, vm);
    match menu {
        HostMenu::StartOn => {
            allowed(vm, VmOperations::Start)
                && enabled_target_exists(model, storage_host(model, vm))
        }
        HostMenu::ResumeOn => {
            !allowed(vm, VmOperations::Suspend)
                && allowed(vm, VmOperations::Resume)
                && enabled_target_exists(model, home)
        }
        HostMenu::Migrate => allowed(vm, VmOperations::PoolMigrate),
    }
}

/// Why `host` cannot take `vm` for `menu`, known without asking the server
/// (`VMOperationHostCommand.VmCanBootOnHost` before its
/// `VM.assert_can_boot_here` call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostProblem {
    /// The VM already runs there.
    CurrentServer,
    /// The host is not live.
    Offline,
    /// The host is disabled (maintenance mode).
    Disabled,
    /// The host runs an older product version than the VM's host.
    OlderThanCurrent,
    /// The VM's CPU vendor differs from the host's.
    IncompatibleCpu,
}

/// The local reason why `host` cannot take `vm`, if any.
pub fn target_host_problem(
    model: &ConnectionModel,
    vm: &VmRecord,
    host_ref: &Ref<Host>,
    host: &HostRecord,
    menu: HostMenu,
) -> Option<HostProblem> {
    if vm.power_state == VmPowerState::Running
        && let Some(resident) = model.hosts.get(&vm.resident_on)
    {
        if &vm.resident_on == host_ref {
            return Some(HostProblem::CurrentServer);
        }
        if version_cmp(product_version(host), product_version(resident)).is_lt() {
            return Some(HostProblem::OlderThanCurrent);
        }
    }
    if !objects::host_is_live(model, host) {
        return Some(HostProblem::Offline);
    }
    if !host.enabled {
        return Some(HostProblem::Disabled);
    }
    if matches!(menu, HostMenu::Migrate | HostMenu::ResumeOn)
        && matches!(
            vm.power_state,
            VmPowerState::Running | VmPowerState::Suspended
        )
        && let (Some(a), Some(b)) = (
            vm.last_boot_cpu_flags.get("vendor"),
            host.cpu_info.get("vendor"),
        )
        && a != b
    {
        return Some(HostProblem::IncompatibleCpu);
    }
    None
}

fn product_version(host: &HostRecord) -> &str {
    host.software_version
        .get("product_version")
        .map(String::as_str)
        .unwrap_or_default()
}

/// Compares dotted versions numerically (`8.10` > `8.9`); missing parts
/// compare as equal so an unknown version never blocks.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_empty() || b.is_empty() {
        return std::cmp::Ordering::Equal;
    }
    let parts = |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a).cmp(&parts(b))
}

/// Whether `action` is offered for `host`. `busy` means this client has an
/// operation running on it.
pub fn host_action_available(
    model: &ConnectionModel,
    host: &HostRecord,
    action: HostAction,
    busy: bool,
) -> bool {
    let live = objects::host_is_live(model, host);
    match action {
        HostAction::Reboot | HostAction::RestartToolstack => live,
        HostAction::Shutdown => live && !busy,
        HostAction::PowerOn => {
            !live
                && host
                    .allowed_operations
                    .contains(&HostAllowedOperations::PowerOn)
                && !host.power_on_mode.is_empty()
                && !busy
        }
        HostAction::EnterMaintenance => host.enabled && model.coordinator().is_some(),
        HostAction::ExitMaintenance => !host.enabled && model.coordinator().is_some(),
    }
}

/// `VM.HAIsProtected`: HA is on in the pool and the VM has a restart
/// priority (any but "do not restart", which is the empty string).
pub fn vm_ha_protected(model: &ConnectionModel, vm: &VmRecord) -> bool {
    model.pool().is_some_and(|(_, p)| p.ha_enabled) && !vm.ha_restart_priority.is_empty()
}

/// The running guests on `host` (what Reboot and Shut Down stop first and
/// what maintenance mode migrates away).
pub fn running_guests<'a>(
    model: &'a ConnectionModel,
    host: &'a HostRecord,
) -> Vec<(&'a Ref<Vm>, &'a VmRecord)> {
    host.resident_vms
        .iter()
        .filter_map(|r| model.vms.get(r).map(|v| (r, &**v)))
        .filter(|(_, v)| !v.is_control_domain && v.power_state == VmPowerState::Running)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ConnectionModel {
        ConnectionModel::from_database(
            &serde_json::from_str(include_str!("../../../tests/fixtures/records.json")).unwrap(),
        )
    }

    fn vm(m: &ConnectionModel, r: &str) -> VmRecord {
        (*m.vms[&Ref::new(r)]).clone()
    }

    #[test]
    fn vm_actions_follow_allowed_operations_and_power_state() {
        let m = model();
        let web = vm(&m, "OpaqueRef:vm-web"); // running
        let db = vm(&m, "OpaqueRef:vm-db"); // halted
        let build = vm(&m, "OpaqueRef:vm-build"); // halted, no local disk
        let legacy = vm(&m, "OpaqueRef:vm-legacy"); // suspended
        for a in [
            VmAction::CleanShutdown,
            VmAction::CleanReboot,
            VmAction::Suspend,
            VmAction::Pause,
            VmAction::HardShutdown,
            VmAction::HardReboot,
        ] {
            assert!(vm_action_available(&m, &web, &a, false), "{a:?}");
            assert!(!vm_action_available(&m, &db, &a, false), "{a:?}");
        }
        assert!(vm_action_available(&m, &build, &VmAction::Start, false));
        assert!(!vm_action_available(&m, &web, &VmAction::Start, false));
        // db-01's home server (host 2) is in maintenance mode, but its
        // disks are on shared storage: it can start elsewhere.
        assert!(vm_action_available(&m, &db, &VmAction::Start, false));
        assert!(vm_action_available(&m, &legacy, &VmAction::Resume, false));
        assert!(!vm_action_available(&m, &legacy, &VmAction::Suspend, false));
        assert!(!vm_action_available(&m, &web, &VmAction::Unpause, false));

        // Busy: only the forced operations stay.
        assert!(!vm_action_available(&m, &web, &VmAction::CleanReboot, true));
        assert!(vm_action_available(&m, &web, &VmAction::HardShutdown, true));
        let mut stuck = web.clone();
        stuck.allowed_operations.clear();
        assert!(vm_action_available(&m, &stuck, &VmAction::HardReboot, true));
        assert!(!vm_action_available(
            &m,
            &stuck,
            &VmAction::HardReboot,
            false
        ));

        // Never on templates, snapshots or control domains.
        for r in ["OpaqueRef:vm-template-debian", "OpaqueRef:vm-dom0-01"] {
            assert!(!vm_action_available(
                &m,
                &vm(&m, r),
                &VmAction::HardShutdown,
                false
            ));
        }
    }

    #[test]
    fn host_menus_and_targets() {
        let m = model();
        let web = vm(&m, "OpaqueRef:vm-web");
        let db = vm(&m, "OpaqueRef:vm-build");
        let (h1, h2) = (
            Ref::new("OpaqueRef:host-0001"),
            Ref::new("OpaqueRef:host-0002"),
        );
        assert!(host_menu_available(&m, &web, HostMenu::Migrate, false));
        assert!(!host_menu_available(&m, &web, HostMenu::StartOn, false));
        assert!(host_menu_available(&m, &db, HostMenu::StartOn, false));
        assert!(!host_menu_available(&m, &db, HostMenu::StartOn, true));
        // web-01 runs on host 1; host 2 is in maintenance.
        assert_eq!(
            target_host_problem(&m, &web, &h1, &m.hosts[&h1], HostMenu::Migrate),
            Some(HostProblem::CurrentServer)
        );
        assert_eq!(
            target_host_problem(&m, &web, &h2, &m.hosts[&h2], HostMenu::Migrate),
            Some(HostProblem::Disabled)
        );
        assert!(vm_action_available(
            &m,
            &db,
            &VmAction::StartOn(h1.clone()),
            false
        ));
        assert!(!vm_action_available(&m, &db, &VmAction::StartOn(h2), false));
    }

    #[test]
    fn host_actions() {
        let m = model();
        let h1 = &m.hosts[&Ref::new("OpaqueRef:host-0001")];
        let h2 = &m.hosts[&Ref::new("OpaqueRef:host-0002")];
        assert!(host_action_available(&m, h1, HostAction::Reboot, false));
        assert!(host_action_available(
            &m,
            h1,
            HostAction::EnterMaintenance,
            false
        ));
        assert!(!host_action_available(
            &m,
            h1,
            HostAction::ExitMaintenance,
            false
        ));
        assert!(host_action_available(
            &m,
            h2,
            HostAction::ExitMaintenance,
            false
        ));
        assert!(!host_action_available(&m, h1, HostAction::PowerOn, false));
        assert!(!host_action_available(&m, h1, HostAction::Shutdown, true));
        assert_eq!(running_guests(&m, h1).len(), 1);
        assert!(version_cmp("8.10.0", "8.9.1").is_gt());
    }

    #[test]
    fn rbac_methods_include_the_task_cleanup() {
        let r = ActionRequest::Vm {
            vm: Ref::new("OpaqueRef:x"),
            action: VmAction::Migrate(Ref::new("OpaqueRef:h")),
        };
        assert_eq!(r.rbac_methods(), vec!["vm.pool_migrate", "task.destroy"]);
    }
}
