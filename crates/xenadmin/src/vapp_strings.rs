//! User-interface strings of the vApps (F12a): Manage vApps, the New vApp
//! wizard, vApp Properties, the vApp commands, the operation titles and
//! steps.
//!
//! The English texts are the original console's (`VMAppliancesDialog`,
//! `NewVMApplianceWizard` and `NewVMGroupVMsPage` resources,
//! `XenModel/Messages.resx`), without the `&` access keys, `{0}` written
//! as named placeholders. A few are this console's own; each says so.

use crate::i18n::fill;
use crate::locale::string_table;

string_table! {
    namespace = "vapp", struct VAppStrings, fn vapp_strings;

    // ---- menus -------------------------------------------------------------
    menu_manage = "Manage vApps...",
    menu_start = "Start vApp",
    menu_shut_down = "Shut Down vApp",
    menu_assign = "Assign to vApp",
    menu_new = "New vApp...",
    /// `DYNAMIC_MENUITEM_WITH_ACCESS_KEY` without the access key.
    menu_vapp_item = "{index}   {name}",

    // ---- Manage vApps ------------------------------------------------------
    manage_title = "Manage vApps - pool {pool}",
    defined_in_pool = "vApps defined in pool '{pool}': ",
    defined_in_server = "vApps defined in server '{pool}': ",
    col_name = "Name",
    col_description = "Description",
    col_vms = "#VMs",
    button_new = "New vApp...",
    button_delete = "Delete",
    button_properties = "Properties",
    button_start = "Start",
    button_shut_down = "Shut down",
    button_import = "Import",
    button_export = "Export",
    button_close = "Close",
    /// This console's own: the list is empty.
    no_vapps = "No vApps.",

    // Delete.
    delete_title = "Delete vApp",
    delete_one_0 = "Do you want to delete vApp '{name}'?",
    delete_one_1 = "Do you want to delete vApp '{name}'?\n    \n{count} VM is currently included in this vApp.",
    delete_one_n = "Do you want to delete vApp '{name}'?\n    \n{count} VMs are currently included in this vApp.",
    delete_many_0 = "Do you want to delete the selected vApps?",
    delete_many_1 = "Do you want to delete the selected vApps?\n    \n{count} VM is currently included in one of these vApps.",
    delete_many_n = "Do you want to delete the selected vApps?\n    \n{count} VMs are currently included in these vApps.",

    // Shut down.
    shut_down_title = "Shut down all VMs in vApp",
    confirm_shut_down_one = "Are you sure you want to shut down the selected vApp?",
    confirm_shut_down = "Are you sure you want to shut down the following vApps?\n\n{names}",
    yes = "Yes",
    no = "No",

    // ---- New vApp wizard ---------------------------------------------------
    wizard_title = "New vApp",
    page_rbac = "Permissions",
    page_name = "Name",
    page_vms = "Virtual Machines",
    page_order = "VM Startup Sequence",
    page_finish = "Finish",
    name_title = "What do you want to call this vApp?",
    name_blurb = "Use this wizard to define a vApp that will allow you to easily recover your critical VMs and services on a secondary site in the event of the loss of your primary production site.\n\nTo begin, enter a name for the vApp and a description (optional), then click Next.",
    name_label = "Name:",
    description_label = "Description:",
    vms_title = "Choose which VMs to include in this vApp",
    vms_in_pool = "VMs in pool '{pool}':",
    vms_in_server = "VMs in server '{pool}':",
    vms_rubric = "Select a checkbox to add a VM to the vApp; clear its checkbox to remove it from the vApp.",
    col_vm = "VM",
    col_current = "Current vApp",
    none = "<None>",
    one_vm_selected = "1 VM selected",
    vms_selected = "{count} VMs selected",
    select_all = "Select All",
    clear_all = "Clear All",
    search_hint = "Search",
    order_title = "Configure the startup sequence for the VMs in this vApp",
    order_blurb = "These options specify the preferred startup sequence and timing for VMs when they are automatically started with a vApp or during an HA operation. VMs with a start order of 0 (zero) will be started first. You can also configure a delay interval which specifies how long after starting the VM to wait before starting up the next group of VMs in the startup sequence.",
    col_order = "Start order",
    col_delay = "Delay interval",
    group_startup = "Startup sequence",
    start_order = "Start order:",
    start_delay = "Attempt to start next VM after:",
    seconds = "seconds",
    finish_title = "Create the new vApp",
    finish_intro = "Review the new vApp below and click Previous if you want to change any settings or Finish to create the new vApp.",
    summary = "vApp name:\n    {name}\n\nSelected VMs:\n    {vms}",
    rbac_create = "A {role} user cannot create vApps. Log in as a different user with sufficient privileges and try again.",
    rbac_click_next = "Your role allows this operation. Click Next to continue.",

    // Assign warnings.
    change_title = "Assign VM to vApp",
    change_one = "The VM '{vm}' is already assigned to the vApp '{old}'.\n    \nDo you want to assign it to the vApp '{new}' instead?",
    change_many = "Some of the VMs you selected are already assigned to a different vApp.\n    \nDo you want to assign these VMs to the vApp '{new}' instead?",

    // ---- Properties --------------------------------------------------------
    properties_title = "'{name}' Properties",
    page_general = "General",
    general_title = "General",
    ok = "OK",
    cancel = "Cancel",

    // ---- operations --------------------------------------------------------
    op_create = "Creating vApp '{name}'...",
    /// This console's own: the original's dialog saves page by page.
    op_properties = "Saving the properties of vApp '{name}'",
    op_assign_one = "Assign VM '{vm}' to vApp '{vapp}'",
    op_assign_many = "Assign VMs to vApp '{vapp}'",
    op_remove_one = "Remove VM '{vm}' from vApp '{vapp}'",
    op_remove_many = "Remove VMs from vApp '{vapp}'",
    op_delete_one = "Deleting vApp '{name}'",
    op_delete_many = "Delete vApps",
    op_start = "Starting vApp '{name}'",
    op_shut_down = "Shutting down vApp '{name}'",
    step_startup = "Setting VM startup options on {vm}",
    step_deleting = "Deleting vApp '{name}'",
    step_creating_appliance = "Creating appliance '{name}'...",
}

/// The title of an F12a vApp operation.
pub fn vapp_title(kind: &xen_core::operations::VAppKind) -> String {
    use xen_core::operations::VAppKind;
    let s = vapp_strings();
    match kind {
        VAppKind::Create { name } => fill(s.op_create, &[("name", name)]),
        VAppKind::Properties { name } => fill(s.op_properties, &[("name", name)]),
        VAppKind::Assign { vm, vapp } => match vm {
            Some(vm) => fill(s.op_assign_one, &[("vm", vm), ("vapp", vapp)]),
            None => fill(s.op_assign_many, &[("vapp", vapp)]),
        },
        VAppKind::Remove { vm, vapp } => match vm {
            Some(vm) => fill(s.op_remove_one, &[("vm", vm), ("vapp", vapp)]),
            None => fill(s.op_remove_many, &[("vapp", vapp)]),
        },
        VAppKind::Delete { name } => match name {
            Some(name) => fill(s.op_delete_one, &[("name", name)]),
            None => s.op_delete_many.to_owned(),
        },
        VAppKind::Start { name } => fill(s.op_start, &[("name", name)]),
        VAppKind::ShutDown { name } => fill(s.op_shut_down, &[("name", name)]),
    }
}

/// The words of an F12a vApp step, when it is one.
pub fn vapp_step(step: &xen_core::OperationStep) -> Option<String> {
    use xen_core::OperationStep;
    let s = vapp_strings();
    Some(match step {
        OperationStep::SettingStartupOptions { vm } => fill(s.step_startup, &[("vm", vm)]),
        OperationStep::DeletingVApp { name } => fill(s.step_deleting, &[("name", name)]),
        OperationStep::CreatingVApp { name } => fill(s.step_creating_appliance, &[("name", name)]),
        _ => return None,
    })
}
