//! User-interface strings of the Migrate, Move and Copy VM wizard (F12a):
//! its pages, the menu items, the operation titles and steps, and the
//! errors.
//!
//! The English texts are the original console's (`CrossPoolMigrateWizard`
//! and the generic wizard pages' resources, `XenModel/Messages.resx`),
//! without the `&` access keys, `{0}` written as named placeholders. A few
//! are this console's own; each says so.

use crate::i18n::fill;

macro_rules! cpm_strings {
    ($( $(#[$doc:meta])* $field:ident = $en:literal, )*) => {
        /// One language's strings.
        pub struct CpmStrings {
            $( $(#[$doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: CpmStrings = CpmStrings {
            $( $field: $en, )*
        };
    };
}

cpm_strings! {
    // ---- menus -------------------------------------------------------------
    menu_cpm = "Migrate VM wizard...",
    menu_move_vm = "Move VM...",
    menu_migrate_vm = "Migrate VM...",
    migration_not_allowed = "'{vm}' may not migrate at the moment. Please ensure you have installed {tools} on it",
    migration_unsupported_sr = "'{vm}' may not migrate because it is on storage that doesn't support migration",

    // ---- titles ------------------------------------------------------------
    title_migrate = "Migrate VM",
    title_migrate_to = "Migrate VM to {target}",
    title_move = "Move VM",
    title_move_to = "Move VM to {target}",
    title_copy_vm = "Copy VM",
    title_copy_vm_to = "Copy VM to {target}",
    title_copy_template = "Copy Template",
    title_copy_template_to = "Copy Template to {target}",

    // ---- pages -------------------------------------------------------------
    page_copy_mode = "Destination",
    page_intra_copy = "Name and Storage",
    page_destination = "Destination Pool",
    page_rbac = "Permissions",
    page_storage = "Storage",
    page_network = "Networking",
    page_transfer = "Migration Network",
    page_finish = "Finish",

    // Copy mode.
    copy_mode_title = "Destination",
    copy_rubric_vm = "Select where would you like to copy your VM.",
    copy_rubric_template = "Select where would you like to copy your template",
    within_pool = "Within pool",
    within_pool_text = "Make a copy in the same pool",
    cross_pool = "Cross-pool",
    cross_pool_text = "Make a copy in a different pool",
    cross_pool_licence = "Cross-pool copy is not allowed with your current license.",

    // Intra-pool copy (Name and Storage).
    intra_title = "Name and Storage",
    intra_rubric_vm = "Provide a name and a description (optional) for the new VM and then select a copy mode.",
    intra_rubric_template = "Provide a name and a description (optional) for the new template and then select a copy mode.",
    copy_name = "Name:",
    copy_description = "Description:",
    copy_mode_group = "Copy mode",
    fast_clone = "Fast clone",
    fast_clone_vm = "Clone the existing VM, using a storage-level fast disk clone operation",
    fast_clone_template = "Clone the existing template, using a storage-level fast disk clone operation",
    slow_clone = "Clone",
    slow_clone_text = "Clone the existing template",
    fast_clone_unavailable = "Fast clone is not supported by this storage type",
    full_copy = "Full copy",
    copy_select_sr_vm = "Create a full copy of the existing VM on this storage repository:",
    copy_select_sr_template = "Create a full copy of the existing template on this storage repository:",
    copy_default_name = "Copy of {name}",

    // Destination.
    destination_title = "Select the destination pool or standalone server",
    destination_intro_migrate = "Select the pool or standalone server where you want to migrate the selected VM(s) to.",
    destination_intro_move = "Select the pool or standalone server where you want to move the selected VM(s) to.",
    destination_intro_copy = "Select the pool or standalone server where you want to copy the selected VM(s) to.",
    destination_intro_copy_template = "Select the pool or standalone server where you want to copy the selected template(s) to.",
    destination_label = "Destination:",
    destination_choose = "Choose a pool or standalone server",
    table_intro_migrate = "If you want to migrate the disks of a VM to local storage, specify the server below.",
    table_intro_move = "If you want to move the disks of a VM to local storage, specify the server below.",
    table_intro_copy = "If you want to copy the disks of a VM to local storage, specify the server below.",
    table_intro_templates = "If you want to place the disks of a template on local storage, specify the server below.",
    col_vm = "VM",
    col_template = "Template",
    col_server = "Server",
    none_selected = "None selected",
    checking = "{target} (checking...)",
    with_reason = "{target} ({reason})",
    current_server = "Current server",
    older_server = "Older than current server",
    dynamic_memory = "Dynamic memory control is unavailable on the destination",
    /// This console's own: the original adds servers from the list.
    add_server_hint = "To migrate to another pool or standalone server, connect to it first.",
    no_destinations = "No other pool or standalone server is connected.",
    target_disconnected = "The destination host has become unreachable.\n\nPlease reconnect the host and try again.",
    vm_missing = "The selected VMs are no longer available in {console}'s cache. Please verify your selection is valid and relaunch the wizard.",

    // RBAC.
    rbac_title = "Permissions",
    rbac_intro = "Your role does not allow every action of this wizard.",
    rbac_click_next = "Your role allows this operation. Click Next to continue.",
    rbac_migrate_vm = "A {role} user cannot migrate VMs across pools. Log in as a different user with sufficient privileges on both source and destination servers and try again.",
    rbac_migrate_template = "A {role} user cannot migrate templates across pools. Log in as a different user with sufficient privileges on both source and destination servers and try again.",
    rbac_copy_vm = "A {role} user cannot copy VMs across pools. Log in as a different user with sufficient privileges on both source and destination servers and try again.",
    rbac_copy_template = "A {role} user cannot copy templates across pools. Log in as a different user with sufficient privileges on both source and destination servers and try again.",
    rbac_intra_copy_vm = "A {role} user cannot copy VMs. Log in as a different user with sufficient privileges and try again.",
    rbac_intra_copy_template = "A {role} user cannot copy templates. Log in as a different user with sufficient privileges and try again.",
    /// This console's own: which server's roles fall short.
    rbac_on_server = "{server}: {text}",

    // Storage.
    storage_title = "Configure storage",
    storage_intro = "Select one or more storage repositories (SR) in the destination pool or standalone server.",
    all_on_same_sr = "Place all virtual disks on the same SR:",
    specific_srs = "Place virtual disks onto specified SRs:",
    col_disk_vm = "VM - Virtual Disk",
    col_disk_template = "Template - Virtual Disk",
    col_sr = "Storage Repository",
    sr_item = "{sr}, {free} available",
    sr_not_enough = "{sr} - not enough available space on the SR",
    no_sr = "No SR available with enough free space.",
    no_common_sr = "No SR available with enough free space for all VMs.",
    space_limitation = "There is not enough available space on SR {sr}. Please review your SR selections.",
    space_limitation_common = "There is not enough available space on SR {sr} for all the VMs. Please consider importing the VMs on different target SRs.",
    disk_label = "{vm} - {disk}",
    disk_unnamed = "{vm} - Disk {index}",

    // Networking.
    network_title = "Configure networking",
    network_intro = "Map the virtual network interfaces in the selected VMs to networks in the destination pool or standalone server.",
    network_intro_single = "Map the virtual network interfaces in the selected VM to networks in the destination pool or standalone server.",
    network_intro_template = "Map the virtual network interfaces in the selected templates to networks in the destination pool or standalone server.",
    network_intro_template_single = "Map the virtual network interfaces in the selected template to networks in the destination pool or standalone server.",
    network_table_intro = "Virtual network interfaces:",
    col_vif_vm = "VM - Virtual Network Interface",
    col_vif_template = "Template - Virtual Network Interface",
    col_target_network = "Target Network",
    no_networks = "No available networks to connect.",

    // Migration network.
    transfer_title = "Configure storage migration settings",
    transfer_blurb_vm = "Select a storage network on the destination pool or standalone server that will be used for the live migration of the virtual disks. \n\nFor optimal performance and reliability during VM migration, ensure that the network used for the storage migration is not being used for management or virtual machine traffic.",
    transfer_blurb_template = "Select a storage network on the destination pool or standalone server that will be used for the live migration of the virtual disks. \n\nFor optimal performance and reliability during template migration, ensure that the network used for the storage migration is not being used for management or virtual machine traffic.",
    transfer_label = "Storage network:",
    transfer_item = "{network} ({pool})",
    no_transfer_networks = "No network with an IP address is available.",

    // Finish.
    finish_title = "Review settings",
    finish_intro = "The wizard is ready to begin migrating the selected VMs using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to migrate the VMs.",
    finish_intro_single = "The wizard is ready to begin migrating the selected VM using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to migrate the VM.",
    finish_intro_copy = "The wizard is ready to begin copying the selected VMs using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to copy the VMs.",
    finish_intro_copy_single = "The wizard is ready to begin copying the selected VM using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to copy the VM.",
    finish_intro_copy_template = "The wizard is ready to begin copying the selected templates using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to copy the templates.",
    finish_intro_copy_single_template = "The wizard is ready to begin copying the selected template using the settings shown below. Please review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to copy the template.",
    finish_errors = "You need to resolve the issues found before you can continue.",
    summary_vm = "VM:",
    summary_template = "Template:",
    summary_destination = "Destination:",
    summary_transfer = "Migration Network:",
    summary_storage = "Storage:",
    summary_network = "Networking:",
    summary_network_not_found = "Network not found",
    summary_unknown = "Unknown",

    // Move VM dialog (`MoveVMDialog`, when the wizard cannot run).
    move_dialog_title = "Move VM",
    move_dialog_intro = "Select the storage repository to move the disks of '{vm}' to:",
    move_button = "Move",

    // ---- operations --------------------------------------------------------
    op_migrating_from = "Migrating VM '{vm}' from '{from}' to '{to}'",
    op_migrating = "Migrating VM '{vm}' to '{to}'",
    op_copying = "Copying VM '{vm}' from '{pool}' to '{to}'",
    op_moving_host = "Moving VM '{vm}' from server '{from}' to server '{to}'",
    op_moving_sr = "Moving VM '{vm}' to SR '{sr}'",
    op_moving = "Moving VM '{vm}' to new storage",
    step_moving_disk = "Moving Virtual Disk '{disk}' from SR '{from}' to SR '{to}'",

    // ---- errors ------------------------------------------------------------
    err_move_destroy = "Failed to finalize moving VM '{vm}' to new storage. Please see logs for details.",
}

pub fn cpm_strings() -> &'static CpmStrings {
    &EN
}

/// The title of an F12a migration, copy or move.
pub fn cpm_title(kind: &xen_core::operations::MigrateKind) -> String {
    use xen_core::operations::MigrateKind;
    let s = cpm_strings();
    match kind {
        MigrateKind::Migrate { vm, from, to } => match from {
            Some(from) => fill(
                s.op_migrating_from,
                &[("vm", vm), ("from", from), ("to", to)],
            ),
            None => fill(s.op_migrating, &[("vm", vm), ("to", to)]),
        },
        MigrateKind::Copy { vm, pool, to } => {
            fill(s.op_copying, &[("vm", vm), ("pool", pool), ("to", to)])
        }
        MigrateKind::MoveHost { vm, from, to } => {
            fill(s.op_moving_host, &[("vm", vm), ("from", from), ("to", to)])
        }
        MigrateKind::MoveSr { vm, sr } => fill(s.op_moving_sr, &[("vm", vm), ("sr", sr)]),
        MigrateKind::Move { vm } => fill(s.op_moving, &[("vm", vm)]),
    }
}

/// The words of an F12a migration step, when it is one.
pub fn cpm_step(step: &xen_core::OperationStep) -> Option<String> {
    use xen_core::OperationStep;
    match step {
        OperationStep::MovingDisk { disk, from, to } => Some(fill(
            cpm_strings().step_moving_disk,
            &[("disk", disk), ("from", from), ("to", to)],
        )),
        _ => None,
    }
}

/// The words of an F12a migration error, when it is one.
pub fn cpm_error(code: &str, params: &[String]) -> Option<String> {
    use xen_core::migrate_run::errors as e;
    let s = cpm_strings();
    let p = |i: usize| params.get(i).map(String::as_str).unwrap_or_default();
    Some(match code {
        e::TARGET_DISCONNECTED => s.target_disconnected.to_owned(),
        e::MOVE_DESTROY_FAILED => fill(s.err_move_destroy, &[("vm", p(0))]),
        e::VM_MISSING => fill(s.vm_missing, &[("console", crate::branding::PRODUCT_NAME)]),
        _ => return None,
    })
}
