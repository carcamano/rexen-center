//! Strings of the VM and host operations: menu items, confirmations, the
//! role elevation and maintenance mode dialogs, and the operation titles of
//! the Events panel. The texts are the original console's
//! (`XenModel/Messages.resx`), without the `&` access keys. Templates use
//! `{name}` placeholders filled by [`crate::i18n::fill`].

use crate::locale::string_table;

string_table! {
    namespace = "ops", struct OpsStrings, fn ops_strings;

    // Menus and buttons.
    menu_server = "Server",
    shut_down = "Shut Down",
    reboot = "Reboot",
    suspend = "Suspend",
    resume = "Resume",
    pause = "Pause",
    unpause = "Unpause",
    force_shut_down = "Force Shut Down",
    force_reboot = "Force Reboot",
    start_on_server = "Start on Server",
    resume_on_server = "Resume on Server",
    migrate_to_server = "Migrate to Server",
    home_server = "Home Server",
    power_on = "Power On",
    enter_maintenance = "Enter Maintenance Mode…",
    exit_maintenance = "Exit Maintenance Mode",
    restart_toolstack = "Restart Toolstack",
    /// `{host}`
    host_checking = "{host} (Updating…)",
    /// `{host}`, `{reason}`
    host_with_reason = "{host} ({reason})",
    host_menu_empty = "(empty)",
    reason_current_server = "Current server",
    reason_offline = "Server could not be contacted",
    reason_disabled = "Disabled",
    reason_older = "Older than current server",
    reason_cpu = "The host does not have some of the CPU features that the VM is currently using",
    reason_no_home = "Home Server is not set",

    // Confirmations.
    yes = "Yes",
    no = "No",
    confirm_shutdown_vm_title = "Shut Down VM",
    confirm_shutdown_vm = "Are you sure you want to shut down the selected VM?",
    ha_confirm_shutdown_vm = "The selected VM is currently protected by HA. Are you sure you want to shut it down?",
    confirm_reboot_vm_title = "Reboot VM",
    confirm_reboot_vm = "Are you sure you want to reboot the selected VM?",
    confirm_suspend_vm_title = "Suspend VM",
    confirm_suspend_vm = "Are you sure you want to suspend the selected VM?",
    ha_confirm_suspend_vm = "The selected VM is currently protected by HA. Are you sure you want to suspend it?",
    confirm_force_shutdown_vm_title = "Force Shut Down VM",
    confirm_force_shutdown_vm = "Are you sure you want to forcibly shut down the selected VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    confirm_force_shutdown_vm_no_tasks = "Are you sure you want to forcibly shut down the selected VM? This operation can result in data loss.",
    ha_confirm_force_shutdown_vm = "The selected VM is currently protected by HA. Are you sure you want to force the shut down of this VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    ha_confirm_force_shutdown_vm_no_tasks = "The selected VM is currently protected by HA. Are you sure you want to force the shut down of this VM? This operation can result in data loss.",
    confirm_force_reboot_vm_title = "Force VM Reboot",
    confirm_force_reboot_vm = "Are you sure you want to forcibly reboot the selected VM? This operation will cancel all running tasks for the VM and can result in data loss.",
    confirm_force_reboot_vm_no_tasks = "Are you sure you want to forcibly reboot the selected VM? This operation can result in data loss.",
    confirm_migrate_title = "Migrate VM",
    /// `{vm}`, `{host}`
    confirm_migrate = "Do you want to migrate the VM {vm} to the server {host}?",
    confirm_reboot_host_title = "Reboot Server",
    /// `{host}`
    confirm_reboot_host = "Are you sure you want to reboot '{host}'?",
    confirm_reboot_host_yes = "Yes, Reboot",
    confirm_shutdown_host_title = "Shut Down Server",
    /// `{host}`
    confirm_shutdown_host = "Are you sure you want to shut down '{host}'?",
    confirm_shutdown_host_yes = "Yes, Shut Down",
    /// `{host}`
    shutdown_coordinator = "{host} is the pool coordinator. It is strongly recommended to nominate a new coordinator for the pool before proceeding.",
    confirm_restart_toolstack_title = "Restart Toolstack",
    /// `{host}`, `{product}`
    confirm_restart_toolstack = "Are you sure you want to restart the toolstack on '{host}'? Note that {product} will temporarily lose connection to the server.",
    /// `{host}`
    ha_cannot_evacuate_coordinator = "Server '{host}' cannot be placed in Maintenance Mode because it is the coordinator of an HA-enabled pool.",

    // Maintenance mode dialog.
    /// `{host}`
    evacuate_title = "Enter Maintenance Mode - {host}",
    evacuate_blurb = "This operation will migrate all VMs running on this server and transition it into maintenance mode. If a VM cannot be migrated you need to suspend it or shut it down.",
    evacuate_vms = "VMs running on this server:",
    evacuate_no_vms = "No VMs are running on this server.",
    evacuate_checking = "Checking whether the VMs can be migrated…",
    evacuate_ok = "Will be migrated",
    evacuate_button = "Enter Maintenance Mode",
    evacuate_check_again = "Check Again",
    /// `{error}`
    evacuate_check_failed = "Could not check the VMs: {error}",
    /// `{product}`
    // Role elevation dialog.
    elevation_blurb = "Your current role is not authorized to perform this action.",
    elevation_action = "Current action:",
    elevation_server = "Server:",
    elevation_pool = "Pool:",
    elevation_user = "Current user:",
    elevation_role = "Current role:",
    elevation_authorized = "Authorized roles:",
    elevation_username = "Username:",
    elevation_password = "Password:",
    elevation_authorize = "Authorize",
    elevation_checking = "Checking credentials…",
    permission_denied = "Permission Denied",
    user_not_authorized = "The user you have entered is not authorized to complete this action",
    /// `{product}`, `{user}`, `{error}`
    user_authorization_failed = "{product} was unable to authorize the action as {user}: {error}",
    superuser_role = "Local superuser",

    // Errors.
    /// `{operation}`, `{error}`
    operation_failed = "{operation}: {error}",
    /// `{roles}`, `{authorized}`
    rbac_denied = "Your current role is not authorized to perform this action.\nCurrent Role: {roles}\nAuthorized Roles: {authorized}",

    // Events panel titles.
    /// `{vm}`
    op_starting = "Starting VM '{vm}'",
    /// `{vm}`, `{host}`
    op_starting_on = "Starting VM '{vm}' on '{host}'",
    /// `{vm}`, `{host}`
    op_shutting_down_on = "Shutting down VM '{vm}' on '{host}'",
    /// `{vm}`, `{host}`
    op_rebooting_on = "Rebooting VM '{vm}' on '{host}'",
    /// `{vm}`
    op_suspending = "Suspending VM '{vm}'",
    /// `{vm}`
    op_resuming = "Resuming VM '{vm}'",
    /// `{vm}`, `{host}`
    op_resuming_on = "Resuming VM '{vm}' on '{host}'",
    /// `{vm}`
    op_pausing = "Pausing VM '{vm}'",
    /// `{vm}`
    op_unpausing = "Unpausing VM '{vm}'",
    /// `{vm}`, `{from}`, `{to}`
    op_migrating_from = "Migrating VM '{vm}' from '{from}' to '{to}'",
    /// `{vm}`, `{to}`
    op_migrating = "Migrating VM '{vm}' to '{to}'",
    /// `{host}`
    op_host_rebooting = "Rebooting server '{host}'",
    /// `{host}`
    op_host_shutting_down = "Shutting down server '{host}'",
    /// `{host}`
    op_host_power_on = "Powering on server '{host}'",
    /// `{host}`
    op_host_evacuating = "Server '{host}' entering maintenance mode",
    /// `{host}`
    op_host_exiting_maintenance = "Server '{host}' exiting maintenance mode",
    /// `{host}`
    op_toolstack_restart = "Restart toolstack on '{host}'",
    /// `{user}`
    op_elevated = "as {user}",
    /// `{vm}`, `{index}`, `{count}`
    step_reboot_vm_shutdown = "Preparing to reboot server: shutting down {vm} ({index} of {count})",
    /// `{vm}`, `{index}`, `{count}`
    step_shutdown_vm_shutdown = "Preparing to shut down server: shutting down {vm} ({index} of {count})",
    step_disabling = "Disabling the server…",
    step_evacuating = "Migrating the VMs away…",
    step_reenabling = "Enabling the server again…",
}
