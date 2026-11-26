//! User-interface strings of F10b: the High Availability menu, wizard,
//! settings dialog and tab, the HA questions other operations ask, the
//! Users (Active Directory) tab and its dialogs, and the operation titles.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's
//! (`XenModel/Messages.resx`, `XenModel/FriendlyNames.resx` and the
//! dialogs', pages' and wizard pages' own resources), without the `&`
//! access keys. Templates use `{name}` placeholders filled by
//! [`crate::i18n::fill`].

use crate::locale::string_table;

string_table! {
    namespace = "ha", struct HaStrings, fn ha_strings;

    // ---- HA: menus and commands -------------------------------------------
    high_availability = "High Availability",
    menu_high_availability = "High Availability",
    configure_ha = "Configure HA...",
    disable_ha = "Disable HA",
    disable_ha_title = "Disable HA",
    ha_standalone = "HA is not available on standalone servers.",
    ha_page_enabling = "HA is currently being enabled for '{pool}'.",
    ha_page_disabling = "HA is currently being disabled for '{pool}'.",
    psr_pending = "You cannot configure HA while a pool secret rotation is in progress.",
    configure_no_statefile = "Cannot configure HA for pool '{pool}' because an HA Statefile VDI could not be found in the pool.",
    disable_no_statefile = "Cannot disable HA for pool '{pool}' because an HA Statefile VDI could not be found in the pool.",
    disable_query = "Are you sure you want to disable HA for pool '{pool}'?",
    rbac_configure_warning = "Your current role is '{current}'.\n\nTo configure HA settings, switch to an account with one of the following roles: \n\n{roles}",
    rbac_enable_warning = "A {role} user cannot configure HA settings. Log in as a different user with sufficient privileges and try again.",
    srs_broken_warning = "The following SRs are broken. It is recommended you fix them before enabling HA.\n\n{srs}",
    // ---- HA wizard ----------------------------------------------------------
    wizard_title = "Configure HA",
    page_intro = "Prerequisites",
    page_intro_title = "HA configuration prerequisites",
    intro_heading = "Welcome to the Configure HA wizard",
    intro_blurb = "This wizard helps you configure HA (high availability) on a resource pool.\n\nBefore you begin, ensure that the following requirements are satisfied for all servers and virtual machines in the pool:\n\n•    Shared storage must be available, including at least one iSCSI, NFS or Fibre Channel LUN. This LUN will be used for the heartbeat SR.\n\n•    All the virtual machines you want to protect with HA must be agile.\n\nWe strongly recommend the use of a bonded management interface on the servers in the pool if HA is enabled, and multipathed storage for the heartbeat SR.\n\n\nTo continue, click Next.",
    page_rbac = "Before you start",
    rbac_description = "Before you can proceed through this wizard, role-based security checks are made to ensure that you have sufficient privileges to create new virtual machines and use all the features in this wizard.",
    rbac_click_next = "Click Next to continue.",
    page_sr = "Heartbeat SR",
    page_sr_title = "Choose a heartbeat SR",
    sr_blurb = "Select a heartbeat SR that will be used to monitor the availability and health of servers in the pool. Heartbeat SRs must use shared storage.",
    col_sr = "Storage repository",
    col_description = "Description",
    col_comment = "Comment",
    no_shared_srs = "You have no shared SRs.",
    scanning_srs = "Scanning for HA heartbeat SRs on {pool}",
    sr_detached = "Detached",
    page_priorities = "HA Plan",
    page_priorities_title = "Configure the HA restart priority, restart order and delay interval for the VMs in this pool",
    col_vm = "Virtual machine",
    col_restart_priority = "Restart priority",
    col_start_order = "Start order",
    col_delay = "Delay interval",
    col_agile = "Agile",
    label_priority = "HA restart priority:",
    label_start_order = "Start order:",
    label_start_delay = "Attempt to start next VM after:",
    seconds = "seconds",
    time_seconds = "{n} seconds",
    mixed_levels = "(Multiple values)",
    calculating_agility = "Calculating...",
    yes = "Yes",
    no = "No",
    unable_to_calculate = "Unable to calculate maximum pool failure capacity.",
    overcommit_message = "HA cannot be guaranteed with the specified settings and the current pool resources.",
    guaranteed_message = "HA is guaranteed. The maximum number of server failures that HA can protect against is {max}.",
    ntol_group = "Server failure limit",
    ntol_blurb = "You can specify the number of server failures you need to be able to tolerate in the pool.",
    ntol_label = "Failures tolerated:",
    ntol_max = "(max = {max})",
    ntol_status = "This number may not exceed the pool’s maximum failure capacity.",
    ntol_calculating = "Calculating...",
    not_agile_sr = "The VM is using a non-shared SR. Restart cannot be guaranteed.",
    not_agile_network = "The VM is not using a shared network. Restart cannot be guaranteed.",
    not_agile_vgpu = "The VM has one or more virtual GPUs. Restart cannot be guaranteed.",
    not_agile_vusb = "The VM has one or more USB devices attached. Restart cannot be guaranteed.",
    not_agile_sriov = "The VM has one or more SR-IOV VFs attached. Restart cannot be guaranteed.",
    not_agile_unknown = "The VM is not agile. Restart cannot be guaranteed.",
    page_finish = "Finish",
    page_finish_title = "Review configuration and activate HA",
    finish_blurb = "Review the final HA configuration below. You can press Finish to enable HA with this configuration, or Previous to alter the configuration.",
    finish_summary = "Configuration summary",
    finish_sr = "Heartbeat SR:",
    finish_ntol = "Failure Tolerance:",
    finish_restart = "Restart:",
    finish_best_effort = "Restart if possible:",
    finish_do_not_restart = "Do not restart:",
    vms_one = "1 VM",
    vms_many = "{n} VMs",
    finish_no_vms_protected = "No virtual machines are protected. This configuration will protect the pool coordinator, but none of your services.",
    finish_no_ha_guaranteed = "Under this configuration, the failover tolerance of your pool would be 0 (zero) and it may not be possible to restart your virtual machines in the event of a server failure.",
    // Restart priorities (`FriendlyNames.resx`).
    priority_restart = "Restart",
    priority_restart_first = "Restart first",
    priority_best_effort = "Restart if possible",
    priority_do_not_restart = "Do not restart",
    // ---- HA settings dialog ------------------------------------------------
    edit_title = "Configure HA - '{pool}'",
    edit_dead_hosts = "HA is currently enabled for pool {pool}. To edit VM restart priorities, all servers in the pool must be reachable.",
    ntol_zero_query = "Under this HA configuration, the failover tolerance of your pool would be 0 (zero) and it may not be possible to restart your virtual machines in the event of a server failure.\n\nDo you want to apply this HA configuration anyway?",
    ha_was_disabled = "HA has been disabled on pool '{pool}'.",
    // ---- HA tab --------------------------------------------------------------
    tab_ha = "HA",
    ha_panel_blurb = "HA is not currently enabled for pool '{pool}'.\n\nClick Configure HA to enable HA for this pool and allow your virtual machines to be automatically restarted in the event of unexpected server failure.",
    ha_tab_configured_blurb = "HA is currently enabled for pool '{pool}'.\n\nClick Configure HA to alter the settings displayed below.",
    configuration_title = "Configuration",
    pool_ha_enabled = "Pool HA enabled",
    configured_capacity = "Configured failure capacity:",
    current_capacity = "Current failure capacity:",
    ntol_zero_hapage = "0 - HA cannot guarantee to restart your protected VMs after server failure",
    overcommitted_ntol = "{n} - pool is overcommitted",
    heartbeating_status = "Heartbeating status",
    network = "Network",
    heartbeat_healthy = "Healthy",
    heartbeat_unhealthy = "Error",
    heartbeat_servers = "{n} out of {m} servers",
    heartbeat_initialising = "Initializing...",
    // ---- HA questions of other operations ------------------------------------
    host_disable_ntol_drop = "Pool '{pool}' has HA enabled, and failover tolerance is currently {current}.  You cannot put server '{host}' into maintenance mode\nand keep this configuration. Do you want to reduce the failover tolerance to {target} and continue?",
    host_disable_ntol_zero = "Pool '{pool}' has HA enabled. If you put server '{host}' into maintenance mode, HA would no longer be guaranteed for this pool.\nAre you sure you want to continue?",
    host_reboot_ntol_drop = "Pool '{pool}' has HA enabled, and failover tolerance is currently {current}. You cannot reboot server '{host}' and keep this configuration.\nDo you want to reduce the failover tolerance to {target} and continue?",
    host_reboot_ntol_zero = "Pool '{pool}' has HA enabled. If you reboot server '{host}', HA will no longer be guaranteed.\nAre you sure you want to continue?",
    host_shutdown_ntol_drop = "Pool '{pool}' has HA enabled, and failover tolerance is currently {current}. You cannot shut down server '{host}' and keep this configuration.\nDo you want to reduce the failover tolerance to {target} and continue?",
    host_shutdown_ntol_zero = "Pool '{pool}' has HA enabled. If you shut down server '{host}', HA will no longer be guaranteed.\nAre you sure you want to continue?",
    host_enable_ntol_raise = "Pool '{pool}' has HA enabled. You have just taken server '{host}' out of maintenance mode, and may now increase the pool's failover tolerance from {current} to {max}. Would you like to do this?",
    vm_start_ntol_drop = "Pool '{pool}' has HA enabled, and is currently configured to tolerate {current} server failures. You cannot start VM '{vm}' and still tolerate this number of failures. Do you want to reduce the number of tolerable failures to {target} and try again?",
    vm_resume_ntol_drop = "Pool '{pool}' has HA enabled, and is currently configured to tolerate {current} server failures. You cannot resume VM '{vm}' and still tolerate this number of failures. Do you want to reduce the number of tolerable failures to {target} and try again?",
    vm_start_ntol_zero = "Pool '{pool}' has HA enabled. If you start VM '{vm}', the pool will not be able to tolerate any further server failures, and your HA configuration would no longer be able to restart your protected VMs. To perform the action anyway, turn off HA.",
    vm_resume_ntol_zero = "Pool '{pool}' has HA enabled. If you resume VM '{vm}', the pool will not be able to tolerate any further server failures, and your HA configuration would no longer be able to restart your protected VMs. To perform the action anyway, turn off HA.",
    invalid_config_start = "VM '{vm}' has an invalid HA configuration that prevents it from starting. It is necessary to clear the VM's HA configuration to proceed.\n\nWould you like to clear the VM's HA restart priority?",
    invalid_config_resume = "VM '{vm}' has an invalid HA configuration that prevents it from being resumed. It is necessary to clear the VM's HA configuration to proceed.\n\nWould you like to clear the VM's HA restart priority?",
    // ---- HA operations --------------------------------------------------------
    op_enable_ha = "Enabling HA on pool '{pool}'",
    op_disable_ha = "Disabling HA on pool '{pool}'",
    op_set_priorities = "Setting HA protection levels",
    op_raise_ntol = "Setting the failover tolerance of pool '{pool}'",
    step_priority_on = "Setting HA restart priority on {vm}",
    step_lowering_ntol = "Lowering failover tolerance and retrying",
    step_unprotect = "Setting HA restart priority on '{vm}' to '{priority}'",
    step_joining_domain = "Joining the Active Directory domain",
    err_ad_credentials = "A domain account is needed to join the server to the pool's domain.",
    // ---- AD: Users tab ----------------------------------------------------------
    tab_users = "Users",
    ad_tab_title = "Active Directory Users",
    ad_feature_name = "AD Authentication",
    ad_configured_blurb = "Pool '{pool}' belongs to domain '{domain}'.",
    ad_configured_blurb_host = "Server '{pool}' belongs to domain '{domain}'.",
    ad_not_configured_blurb = "AD is not currently configured for pool '{pool}'. To enable AD authentication, click Join Domain.",
    ad_not_configured_blurb_host = "AD is not currently configured for server '{pool}'. To enable AD authentication, click Join Domain.",
    ad_configuring_blurb = "AD authentication is currently being configured for pool '{pool}'.",
    ad_configuring_blurb_host = "AD authentication is currently being configured for server '{pool}'.",
    ad_coordinator_unavailable = "Please wait while {product} establishes your current external authentication configuration.",
    join_domain = "Join Domain...",
    leave_domain = "Leave Domain...",
    users_with_access = "Users and Groups with Access",
    changes_take_effect = "All changes made to users and groups on the access list will take effect when a user next logs in.",
    ad_disabled = "Active Directory is disabled",
    col_subject = "Subject",
    col_roles = "Roles",
    col_logged_in = "Logged In",
    add = "Add...",
    remove = "Remove",
    logout_user = "Logout User",
    change_role = "Change Role...",
    local_root = "Local root account",
    always_granted = "(Always granted access)",
    cannot_modify_root = "Cannot modify the root account",
    group = "Group",
    user = "User",
    // ---- AD: dialogs ----------------------------------------------------------------
    join_title = "Enabling AD authentication",
    leave_title = "Disabling AD authentication",
    domain = "Domain:",
    username = "Username:",
    password = "Password:",
    ok = "OK",
    cancel = "Cancel",
    ignore = "Ignore",
    disable = "Disable",
    join_blurb_short = "Enter your fully qualified AD domain name, and a user name and password with sufficient privileges to add servers to your domain.",
    joining_ad = "Please enter a user name and password with sufficient privileges to add servers to domain '{domain}'.",
    leaving_ad = "Enter a user name and password with sufficient privileges to remove your machine account from AD. Authentication will be disabled even if the machine account cannot be removed.",
    leaving_additional = "Alternatively, select ignore to leave the machine account enabled.",
    leave_confirm = "You are about to remove '{pool}' from the AD domain '{domain}'. Any users authenticated using AD will be disconnected.\n\nDo you want to continue?",
    leave_warning = "You are currently connected to pool '{pool}' using Active Directory authentication.\nIf you leave the AD domain '{domain}', you will be disconnected from the pool.\nTo reconnect, you will need to supply the local root account credentials.\n\nDo you want to continue?",
    leave_warning_host = "You are currently connected to server '{pool}' using Active Directory authentication.\nIf you leave the AD domain '{domain}', you will be disconnected from the server.\nTo reconnect, you will need to supply the local root account credentials.\n\nDo you want to continue?",
    add_users_title = "Add Users",
    add_users_blurb = "Adding users or groups to the access list of {pool} lets them connect to the server using their AD credentials. To grant access to users, enter their AD name or group name, separating multiple entries by commas or semicolons.",
    names = "Names:",
    grant_access = "Grant Access",
    close = "Close",
    col_name = "Name",
    col_resolution = "Resolution Status",
    col_grant = "Grant Access Status",
    resolving_names = "Resolving names against the domain...",
    adding_resolved = "Adding resolved names to access list...",
    completed = "Completed.",
    completed_with_errors = "Completed, with some errors encountered. See the application log files for more information.",
    resolving = "Resolving...",
    resolved_as = "Resolved as {name}",
    could_not_resolve = "Subject could not be resolved in your AD",
    adding_to_list = "Adding to access list...",
    added_to_list = "Added to access list",
    failed_to_add = "Failed to add to access list",
    unknown_ad_user = "<unknown AD user>",
    already_has_access = "Subject '{name}' already has access to the server.",
    remove_user_one = "Are you sure you want to remove the user '{name}'?",
    remove_user_many = "Are you sure you want to remove these {n} users?",
    admin_exit_one = "The user you are about to remove is a Pool Administrator.",
    admin_exit_many = "The users you are about to remove are Pool Administrators.",
    admin_exit_one_of_many = "One of the users you are about to remove is a Pool Administrator.",
    admin_exit_some_of_many = "{n} of the users you are about to remove are Pool Administrators.",
    admin_exit_advisory = "To reduce the risk of unauthorized access to your system, it is recommended that you also change the server root password and rotate the pool secret.",
    confirm_logout_current_user = "You are about to revoke the permissions for user '{name}'.\n\nYou are currently logged in as '{name}' on '{pool}', and doing this will disconnect you from '{pool}'. To reconnect, you will need to supply the local root account credentials, or the credentials for another authorized AD user.\n\nDo you want to continue?",
    confirm_logout_current_group = "You are about to revoke the permissions for group '{name}'.\n\nThe user you are currently logged into '{pool}' with is a member of '{name}'. Proceeding will disconnect you and any other members of this group from '{pool}'. To reconnect, you will need to supply the local root account credentials, or the credentials for another authorized AD user.\n\nDo you want to continue?",
    logout_current_one = "You are currently logged in as the selected user. If you continue, you will be logged out of '{pool}'.\n\nAre you sure you want to continue?",
    logout_current_many = "You are currently logged in as one of the selected users. If you continue, you will be logged out of '{pool}'. All other selected users will also be logged out.\n\nAre you sure you want to continue?",
    logout_current_one_role = "You are currently logged in as the selected user. If you assign a new role, you will be logged out of '{pool}'.\n\nAre you sure you want to continue?",
    logout_current_many_role = "You are currently logged in as one of the selected users. If you assign a new role, you will be logged out of '{pool}'. All other selected users that are logged in will also be logged out.\n\nAre you sure you want to continue?",
    logout_user_one = "Are you sure you want to log out the user '{name}'?",
    logout_user_many = "Are you sure you want to log out these {n} users?",
    logout_user_one_role = "If you assign a new role to user '{name}', the user will be logged out.\n\nAre you sure you want to continue?",
    logout_user_many_role = "If you assign a new role to logged in users, they will be logged out.\n\nAre you sure you want to continue?",
    roles_title = "Select Roles",
    roles_save = "Save",
    roles_column = "Roles",
    select_role_user = "Select the role you want to assign to user '{name}'. Each role inherits all the privileges of the subordinate role below it in the list.",
    select_role_group = "Select the role you want to assign to group '{name}'. Each role inherits all the privileges of the subordinate role below it in the list.",
    select_role_user_many = "Select the role you want to assign to these users. Each role inherits all the privileges of the subordinate role below it in the list.",
    select_role_group_many = "Select the role you want to assign to these groups. Each role inherits all the privileges of the subordinate role below it in the list.",
    select_role_mixed = "Select the role you want to assign to these users and groups. Each role inherits all the privileges of the subordinate role below it in the list.",
    roles_admin_advice = "To reduce the risk of unauthorized access to your system after you have removed the Pool Administrator role, it is recommended that you also change the server root password and rotate the pool secret.",
    // Role descriptions (`FriendlyNames.resx`).
    role_desc_pool_admin = "Pool Admins have full access to all features and settings. They can access the server console and manage the roles of other users.\n \n- No restrictions\n- Role and user management\n- Access to server console",
    role_desc_pool_operator = "Pool Operators manage pool-wide resources, including setting up storage, managing patches and creating resource pools.\n \n- Configure pool resources\n- Full access to High Availability (HA), Workload Balancing (WLB) and patch management features\n- No access to role management",
    role_desc_vm_power_admin = "VM Power Admins have full access to VM and Template management. They can choose where to start VMs, and can use the dynamic memory control and VM snapshot features.\n \n- Manages VMs and Templates\n- Full access to dynamic memory control features\n- Full access to VM snapshot features\n- Can set Home Server and choose where to run workloads",
    role_desc_vm_admin = "VM Admins can manage VMs and Templates and can access the storage necessary to complete these tasks. This role relies on the server to choose where to run each VM, and on templates to provide values for dynamic memory control (DMC) and Home Server settings.\n \n- Manage VMs and Templates\n- No access to dynamic memory control features\n- No access to VM snapshot features\n- Cannot set Home Server or choose where to run workloads",
    role_desc_vm_operator = "VM Operators are granted access to use the VMs within the resource pool. They can interact with VM consoles, and can start or stop VMs when there are sufficient server resources available.\n \n- Full access to VM consoles\n- Basic VM lifecycle control\n- Cannot alter VM properties or server resources",
    role_desc_read_only = "With a Read Only role, the user has basic read access. They can connect to the server and view its contents, but will be unable to issue commands or alter any settings.\n \n- Read access only",
    // Subject details (`FriendlyNames.resx`: `AD.PropertyKey-*`).
    key_account_disabled = "Account disabled",
    key_account_expired = "Account expired",
    key_account_locked = "Account locked",
    key_gecos = "Gecos",
    key_name = "Name",
    key_password_expired = "Password expired",
    // ---- AD operations ----------------------------------------------------------------
    op_enable_ad = "Enabling Active Directory Authentication on pool '{pool}'",
    op_disable_ad = "Disabling Active Directory Authentication on pool '{pool}'",
    op_add_remove = "Adding/removing users from Active Directory ACL on '{pool}'",
    op_roles_one = "Assigning new roles to subject '{names}'",
    op_roles_many = "Assigning new roles to subjects '{names}'",
    op_logout_one = "Terminating session for user '{names}'",
    op_logout_many = "Terminating session for users '{names}'",
}

/// `Helpers.RestartPriorityI18n`.
pub fn priority_name(p: xen_core::ha::RestartPriority) -> &'static str {
    use xen_core::ha::RestartPriority as P;
    let s = ha_strings();
    match p {
        P::AlwaysRestartHighPriority => s.priority_restart_first,
        P::AlwaysRestart | P::Restart => s.priority_restart,
        P::BestEffort => s.priority_best_effort,
        P::DoNotRestart => s.priority_do_not_restart,
    }
}

/// `AssignPriorities.VmWithSettingsRow.FriendlyNonAgileReason`.
pub fn not_agile_reason(code: &str) -> &'static str {
    let s = ha_strings();
    match code {
        "HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED" => s.not_agile_sr,
        "HA_CONSTRAINT_VIOLATION_NETWORK_NOT_SHARED" => s.not_agile_network,
        "VM_HAS_VGPU" => s.not_agile_vgpu,
        "VM_HAS_VUSB" => s.not_agile_vusb,
        "VM_HAS_SRIOV_VIF" => s.not_agile_sriov,
        _ => s.not_agile_unknown,
    }
}

/// `Role.FriendlyDescription`.
pub fn role_description(name: &str) -> &'static str {
    let s = ha_strings();
    match name.to_lowercase().as_str() {
        "pool-admin" => s.role_desc_pool_admin,
        "pool-operator" => s.role_desc_pool_operator,
        "vm-power-admin" => s.role_desc_vm_power_admin,
        "vm-admin" => s.role_desc_vm_admin,
        "vm-operator" => s.role_desc_vm_operator,
        "read-only" => s.role_desc_read_only,
        _ => "",
    }
}

/// `FriendlyNameManager.GetFriendlyName("AD.PropertyKey-…")`, else the key.
pub fn subject_key(key: &str) -> String {
    let s = ha_strings();
    match key {
        "subject-account-disabled" => s.key_account_disabled.to_owned(),
        "subject-account-expired" => s.key_account_expired.to_owned(),
        "subject-account-locked" => s.key_account_locked.to_owned(),
        "subject-gecos" => s.key_gecos.to_owned(),
        "subject-name" => s.key_name.to_owned(),
        "subject-password-expired" => s.key_password_expired.to_owned(),
        other => other.to_owned(),
    }
}

/// `VMS_ONE` / `VMS_MANY`.
pub fn vm_count(n: usize) -> String {
    let s = ha_strings();
    if n == 1 {
        s.vms_one.to_owned()
    } else {
        crate::i18n::fill(s.vms_many, &[("n", &n.to_string())])
    }
}

/// The title of an HA operation.
pub fn ha_title(kind: &xen_core::operations::HaKind) -> String {
    use crate::i18n::fill;
    use xen_core::operations::HaKind;
    let s = ha_strings();
    match kind {
        HaKind::Enable { pool } => fill(s.op_enable_ha, &[("pool", pool)]),
        HaKind::Disable { pool } => fill(s.op_disable_ha, &[("pool", pool)]),
        HaKind::SetPriorities => s.op_set_priorities.to_owned(),
        HaKind::SetNtol { pool } => fill(s.op_raise_ntol, &[("pool", pool)]),
    }
}

/// The title of an AD operation.
pub fn ad_title(kind: &xen_core::operations::AdKind) -> String {
    use crate::i18n::fill;
    use xen_core::operations::AdKind;
    let s = ha_strings();
    match kind {
        AdKind::Enable { pool } => fill(s.op_enable_ad, &[("pool", pool)]),
        AdKind::Disable { pool } => fill(s.op_disable_ad, &[("pool", pool)]),
        AdKind::AddRemove { pool } => fill(s.op_add_remove, &[("pool", pool)]),
        AdKind::Roles { subjects } => fill(
            if subjects.len() > 1 {
                s.op_roles_many
            } else {
                s.op_roles_one
            },
            &[("names", &subjects.join(", "))],
        ),
        AdKind::Logout { subjects } => fill(
            if subjects.len() > 1 {
                s.op_logout_many
            } else {
                s.op_logout_one
            },
            &[("names", &subjects.join(", "))],
        ),
    }
}

/// The wording of the errors F10b reports itself, if `code` is one.
pub fn ha_error(code: &str, params: &[String]) -> Option<String> {
    let s = ha_strings();
    let _ = params;
    Some(match code {
        xen_core::pool_run::errors::AD_CREDENTIALS => s.err_ad_credentials.to_owned(),
        _ => return None,
    })
}
