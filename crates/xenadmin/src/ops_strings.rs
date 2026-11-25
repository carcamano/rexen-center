//! Strings of the VM and host operations: menu items, confirmations, the
//! role elevation and maintenance mode dialogs, and the operation titles of
//! the Events panel. The texts are the original console's
//! (`XenModel/Messages.resx`), without the `&` access keys. Templates use
//! `{name}` placeholders filled by [`crate::i18n::fill`].

/// One language's operation strings.
pub struct OpsStrings {
    // Menus and buttons.
    pub menu_server: &'static str,
    pub shut_down: &'static str,
    pub reboot: &'static str,
    pub suspend: &'static str,
    pub resume: &'static str,
    pub pause: &'static str,
    pub unpause: &'static str,
    pub force_shut_down: &'static str,
    pub force_reboot: &'static str,
    pub start_on_server: &'static str,
    pub resume_on_server: &'static str,
    pub migrate_to_server: &'static str,
    pub home_server: &'static str,
    pub power_on: &'static str,
    pub enter_maintenance: &'static str,
    pub exit_maintenance: &'static str,
    pub restart_toolstack: &'static str,
    /// `{host}`
    pub host_checking: &'static str,
    /// `{host}`, `{reason}`
    pub host_with_reason: &'static str,
    pub host_menu_empty: &'static str,
    pub reason_current_server: &'static str,
    pub reason_offline: &'static str,
    pub reason_disabled: &'static str,
    pub reason_older: &'static str,
    pub reason_cpu: &'static str,
    pub reason_no_home: &'static str,

    // Confirmations.
    pub yes: &'static str,
    pub no: &'static str,
    pub confirm_shutdown_vm_title: &'static str,
    pub confirm_shutdown_vm: &'static str,
    pub ha_confirm_shutdown_vm: &'static str,
    pub confirm_reboot_vm_title: &'static str,
    pub confirm_reboot_vm: &'static str,
    pub confirm_suspend_vm_title: &'static str,
    pub confirm_suspend_vm: &'static str,
    pub ha_confirm_suspend_vm: &'static str,
    pub confirm_force_shutdown_vm_title: &'static str,
    pub confirm_force_shutdown_vm: &'static str,
    pub confirm_force_shutdown_vm_no_tasks: &'static str,
    pub ha_confirm_force_shutdown_vm: &'static str,
    pub ha_confirm_force_shutdown_vm_no_tasks: &'static str,
    pub confirm_force_reboot_vm_title: &'static str,
    pub confirm_force_reboot_vm: &'static str,
    pub confirm_force_reboot_vm_no_tasks: &'static str,
    pub confirm_migrate_title: &'static str,
    /// `{vm}`, `{host}`
    pub confirm_migrate: &'static str,
    pub confirm_reboot_host_title: &'static str,
    /// `{host}`
    pub confirm_reboot_host: &'static str,
    pub confirm_reboot_host_yes: &'static str,
    pub confirm_shutdown_host_title: &'static str,
    /// `{host}`
    pub confirm_shutdown_host: &'static str,
    pub confirm_shutdown_host_yes: &'static str,
    /// `{host}`
    pub shutdown_coordinator: &'static str,
    pub confirm_restart_toolstack_title: &'static str,
    /// `{host}`, `{product}`
    pub confirm_restart_toolstack: &'static str,
    /// `{host}`
    pub ha_cannot_evacuate_coordinator: &'static str,

    // Maintenance mode dialog.
    /// `{host}`
    pub evacuate_title: &'static str,
    pub evacuate_blurb: &'static str,
    pub evacuate_vms: &'static str,
    pub evacuate_no_vms: &'static str,
    pub evacuate_checking: &'static str,
    pub evacuate_ok: &'static str,
    pub evacuate_button: &'static str,
    pub evacuate_check_again: &'static str,
    /// `{error}`
    pub evacuate_check_failed: &'static str,
    /// `{product}`
    pub evacuate_coordinator: &'static str,

    // Role elevation dialog.
    pub elevation_blurb: &'static str,
    pub elevation_action: &'static str,
    pub elevation_server: &'static str,
    pub elevation_pool: &'static str,
    pub elevation_user: &'static str,
    pub elevation_role: &'static str,
    pub elevation_authorized: &'static str,
    pub elevation_username: &'static str,
    pub elevation_password: &'static str,
    pub elevation_authorize: &'static str,
    pub elevation_checking: &'static str,
    pub permission_denied: &'static str,
    pub user_not_authorized: &'static str,
    /// `{product}`, `{user}`, `{error}`
    pub user_authorization_failed: &'static str,
    pub superuser_role: &'static str,

    // Errors.
    /// `{operation}`, `{error}`
    pub operation_failed: &'static str,
    /// `{roles}`, `{authorized}`
    pub rbac_denied: &'static str,

    // Events panel titles.
    /// `{vm}`
    pub op_starting: &'static str,
    /// `{vm}`, `{host}`
    pub op_starting_on: &'static str,
    /// `{vm}`, `{host}`
    pub op_shutting_down_on: &'static str,
    /// `{vm}`, `{host}`
    pub op_rebooting_on: &'static str,
    /// `{vm}`
    pub op_suspending: &'static str,
    /// `{vm}`
    pub op_resuming: &'static str,
    /// `{vm}`, `{host}`
    pub op_resuming_on: &'static str,
    /// `{vm}`
    pub op_pausing: &'static str,
    /// `{vm}`
    pub op_unpausing: &'static str,
    /// `{vm}`, `{from}`, `{to}`
    pub op_migrating_from: &'static str,
    /// `{vm}`, `{to}`
    pub op_migrating: &'static str,
    /// `{host}`
    pub op_host_rebooting: &'static str,
    /// `{host}`
    pub op_host_shutting_down: &'static str,
    /// `{host}`
    pub op_host_power_on: &'static str,
    /// `{host}`
    pub op_host_evacuating: &'static str,
    /// `{host}`
    pub op_host_exiting_maintenance: &'static str,
    /// `{host}`
    pub op_toolstack_restart: &'static str,
    /// `{user}`
    pub op_elevated: &'static str,
    /// `{vm}`, `{index}`, `{count}`
    pub step_reboot_vm_shutdown: &'static str,
    /// `{vm}`, `{index}`, `{count}`
    pub step_shutdown_vm_shutdown: &'static str,
    pub step_disabling: &'static str,
    pub step_evacuating: &'static str,
    pub step_reenabling: &'static str,
}

/// English.
pub static OPS_EN: OpsStrings = OpsStrings {
    menu_server: "Server",
    shut_down: "Shut Down",
    reboot: "Reboot",
    suspend: "Suspend",
    resume: "Resume",
    pause: "Pause",
    unpause: "Unpause",
    force_shut_down: "Force Shut Down",
    force_reboot: "Force Reboot",
    start_on_server: "Start on Server",
    resume_on_server: "Resume on Server",
    migrate_to_server: "Migrate to Server",
    home_server: "Home Server",
    power_on: "Power On",
    enter_maintenance: "Enter Maintenance Mode…",
    exit_maintenance: "Exit Maintenance Mode",
    restart_toolstack: "Restart Toolstack",
    host_checking: "{host} (Updating…)",
    host_with_reason: "{host} ({reason})",
    host_menu_empty: "(empty)",
    reason_current_server: "Current server",
    reason_offline: "Server could not be contacted",
    reason_disabled: "Disabled",
    reason_older: "Older than current server",
    reason_cpu: "The host does not have some of the CPU features that the VM is currently using",
    reason_no_home: "Home Server is not set",

    yes: "Yes",
    no: "No",
    confirm_shutdown_vm_title: "Shut Down VM",
    confirm_shutdown_vm: "Are you sure you want to shut down the selected VM?",
    ha_confirm_shutdown_vm: "The selected VM is currently protected by HA. Are you sure you want to shut it down?",
    confirm_reboot_vm_title: "Reboot VM",
    confirm_reboot_vm: "Are you sure you want to reboot the selected VM?",
    confirm_suspend_vm_title: "Suspend VM",
    confirm_suspend_vm: "Are you sure you want to suspend the selected VM?",
    ha_confirm_suspend_vm: "The selected VM is currently protected by HA. Are you sure you want to suspend it?",
    confirm_force_shutdown_vm_title: "Force Shut Down VM",
    confirm_force_shutdown_vm: "Are you sure you want to forcibly shut down the selected VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    confirm_force_shutdown_vm_no_tasks: "Are you sure you want to forcibly shut down the selected VM? This operation can result in data loss.",
    ha_confirm_force_shutdown_vm: "The selected VM is currently protected by HA. Are you sure you want to force the shut down of this VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    ha_confirm_force_shutdown_vm_no_tasks: "The selected VM is currently protected by HA. Are you sure you want to force the shut down of this VM? This operation can result in data loss.",
    confirm_force_reboot_vm_title: "Force VM Reboot",
    confirm_force_reboot_vm: "Are you sure you want to forcibly reboot the selected VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    confirm_force_reboot_vm_no_tasks: "Are you sure you want to forcibly reboot the selected VM? This operation can result in data loss.",
    confirm_migrate_title: "Migrate VM",
    confirm_migrate: "Do you want to migrate the VM {vm} to the server {host}?",
    confirm_reboot_host_title: "Reboot Server",
    confirm_reboot_host: "Are you sure you want to reboot '{host}'?",
    confirm_reboot_host_yes: "Yes, Reboot",
    confirm_shutdown_host_title: "Shut Down Server",
    confirm_shutdown_host: "Are you sure you want to shut down '{host}'?",
    confirm_shutdown_host_yes: "Yes, Shut Down",
    shutdown_coordinator: "{host} is the pool coordinator. It is strongly recommended to nominate a new coordinator for the pool before proceeding.",
    confirm_restart_toolstack_title: "Restart Toolstack",
    confirm_restart_toolstack: "Are you sure you want to restart the toolstack on '{host}'? Note that {product} will temporarily lose connection to the server.",
    ha_cannot_evacuate_coordinator: "Server '{host}' cannot be placed in Maintenance Mode because it is the coordinator of an HA-enabled pool.",

    evacuate_title: "Enter Maintenance Mode - {host}",
    evacuate_blurb: "This operation will migrate all VMs running on this server and transition it into maintenance mode. If a VM cannot be migrated you need to suspend it or shut it down.",
    evacuate_vms: "VMs running on this server:",
    evacuate_no_vms: "No VMs are running on this server.",
    evacuate_checking: "Checking whether the VMs can be migrated…",
    evacuate_ok: "Will be migrated",
    evacuate_button: "Enter Maintenance Mode",
    evacuate_check_again: "Check Again",
    evacuate_check_failed: "Could not check the VMs: {error}",
    evacuate_coordinator: "This server is the pool coordinator. {product} will temporarily lose its connection to the pool while its VMs are migrated.",

    elevation_blurb: "Your current role is not authorized to perform this action.",
    elevation_action: "Current action:",
    elevation_server: "Server:",
    elevation_pool: "Pool:",
    elevation_user: "Current user:",
    elevation_role: "Current role:",
    elevation_authorized: "Authorized roles:",
    elevation_username: "Username:",
    elevation_password: "Password:",
    elevation_authorize: "Authorize",
    elevation_checking: "Checking credentials…",
    permission_denied: "Permission Denied",
    user_not_authorized: "The user you have entered is not authorized to complete this action",
    user_authorization_failed: "{product} was unable to authorize the action as {user}: {error}",
    superuser_role: "Local superuser",

    operation_failed: "{operation}: {error}",
    rbac_denied: "Your current role is not authorized to perform this action.\nCurrent Role: {roles}\nAuthorized Roles: {authorized}",

    op_starting: "Starting VM '{vm}'",
    op_starting_on: "Starting VM '{vm}' on '{host}'",
    op_shutting_down_on: "Shutting down VM '{vm}' on '{host}'",
    op_rebooting_on: "Rebooting VM '{vm}' on '{host}'",
    op_suspending: "Suspending VM '{vm}'",
    op_resuming: "Resuming VM '{vm}'",
    op_resuming_on: "Resuming VM '{vm}' on '{host}'",
    op_pausing: "Pausing VM '{vm}'",
    op_unpausing: "Unpausing VM '{vm}'",
    op_migrating_from: "Migrating VM '{vm}' from '{from}' to '{to}'",
    op_migrating: "Migrating VM '{vm}' to '{to}'",
    op_host_rebooting: "Rebooting server '{host}'",
    op_host_shutting_down: "Shutting down server '{host}'",
    op_host_power_on: "Powering on server '{host}'",
    op_host_evacuating: "Server '{host}' entering maintenance mode",
    op_host_exiting_maintenance: "Server '{host}' exiting maintenance mode",
    op_toolstack_restart: "Restart toolstack on '{host}'",
    op_elevated: "as {user}",
    step_reboot_vm_shutdown: "Preparing to reboot server: shutting down {vm} ({index} of {count})",
    step_shutdown_vm_shutdown: "Preparing to shut down server: shutting down {vm} ({index} of {count})",
    step_disabling: "Disabling the server…",
    step_evacuating: "Migrating the VMs away…",
    step_reenabling: "Enabling the server again…",
};

/// The active table.
pub fn ops_strings() -> &'static OpsStrings {
    &OPS_EN
}
