//! User-interface strings of the scheduled snapshots (F12b): VM Snapshot
//! Schedules, the New Snapshot Schedule wizard, the policy Properties, the
//! VM menus and Snapshots tab line, the operation titles and steps.
//!
//! The English texts are the original console's
//! (`ScheduledSnapshotsDialog`, `NewPolicyWizard` pages,
//! `DaysWeekCheckboxes` and `NewVMGroupVMsPage` resources,
//! `XenModel/Messages.resx`, `FriendlyNames.resx`), without the `&`
//! access keys, `{0}` written as named placeholders. A few are this
//! console's own; each says so.

use xen_core::operations::VmssKind;

use crate::i18n::fill;
use crate::locale::string_table;

string_table! {
    namespace = "vmss", struct VmssStrings, fn vmss_strings;

    // ---- menus -------------------------------------------------------------
    /// `VMSS_MAIN_MENU`.
    menu_manage = "VM Snapshot Schedules...",
    /// `ASSIGN_VMSS_POLICY`.
    menu_assign = "Assign to Snapshot Schedule",
    /// `NEW_SCHEDULE`.
    menu_new = "New Snapshot Schedule...",
    menu_item = "{index}   {name}",

    // ---- VM Snapshot Schedules ---------------------------------------------
    dialog_title = "VM Snapshot Schedules",
    top_blurb = "Create a snapshot schedule for your VMs.",
    defined_in_pool = "Snapshot schedules defined in pool '{pool}' ({protected} out of {total} VMs currently in schedule):",
    defined_in_server = "Snapshot schedules defined in server '{pool}' ({protected} out of {total} VMs currently in schedule):",
    loading = "Loading ...",
    col_name = "Name",
    col_status = "Status",
    col_vms = "#VMs",
    col_next = "Next Snapshot Time",
    col_server_time = "Corresponding Server Time",
    col_last = "Last Result",
    button_new = "New...",
    button_properties = "Properties",
    button_enable = "Enable",
    button_disable = "Disable",
    button_run_now = "Run Now",
    button_delete = "Delete",
    button_close = "Close",
    enabled = "Enabled",
    /// `{time}`: a time in UTC (this console's own).
    time_utc = "{time} UTC",
    disabled = "Disabled",
    not_yet_run = "Not yet run",
    host_not_live = "Pool coordinator unreachable",
    failed = "Failed",
    warning = "Warning",
    succeeded = "Succeeded",
    show_history = "Show Run History",
    hide_history = "Hide Run History",
    history_for = "Run history for policy '{name}':",
    show = "Show:",
    span_last_10 = "Last 10 messages",
    span_24h = "Messages from last 24 hours",
    span_7d = "Messages from last 7 days",
    col_date = "Date and Time",
    col_description = "Description",
    /// This console's own: the list is empty.
    no_policies = "No snapshot schedules.",
    // Run history (`PolicyAlert`).
    /// `VM_SNAPSHOT_SCHEDULE_FAILED`.
    alert_failed_count = "{title} ({count} VMs failed)",
    /// `VMSS_ALERT_VM_ERROR_FORMAT`.
    alert_vm_error = "- {vm}: {error}",
    // Delete.
    delete_title = "Delete Snapshot Schedule",
    delete_one_0 = "Do you want to delete policy '{name}'?",
    delete_one = "Do you want to delete policy '{name}'?\n    \n{count} VMs are currently protected with this policy and will no longer be protected if you delete the policy.",
    delete_many_0 = "Do you want to delete the selected policies?",
    delete_many = "Do you want to delete the selected policies?\n    \n{count} VMs are currently protected with these policies and will no longer be protected if you delete the policies.",
    yes = "Yes",
    no = "No",

    // ---- New Snapshot Schedule ---------------------------------------------
    wizard_title = "New snapshot schedule",
    page_rbac = "Permissions",
    rbac_create = "A {role} user cannot create a snapshot schedule. Log in as a different user with sufficient privileges and try again.",
    rbac_click_next = "Click Next to continue.",
    page_name = "Schedule Name",
    name_title = "What do you want to call this schedule?",
    name_wizard_blurb = "Use this wizard to define a schedule that will run automatic snapshot jobs for your VMs.",
    name_blurb = "To begin, enter a name for the schedule and, optionally, a description, then click Next.",
    name_label = "Schedule name:",
    description_label = "Description:",
    page_vms = "Virtual Machines",
    vms_title = "Select the VMs that you want to schedule snapshots for",
    vms_rubric = "Select a checkbox to add a VM to the snapshot schedule; clear its checkbox to remove it from the schedule.",
    col_current = "Current schedule",
    page_type = "Snapshot Type",
    type_title = "What type of snapshot do you want to use for scheduled snapshots?",
    type_blurb = "Choose the type of snapshot you would like to take.",
    disk_only = "Disk only snapshot",
    disk_only_blurb = "The VM's disks (storage) and metadata will be saved. This type of snapshot is crash-consistent and can be performed on all VM types, including Linux VMs.",
    disk_and_memory = "Disk and memory snapshot",
    disk_and_memory_blurb = "In addition to the VM's disks and metadata, this type of snapshot also stores the VM's memory state (RAM). Note that this type of snapshot can be very large and you should ensure that you make sufficient disk space available for all your scheduled snapshots. ",
    /// `labelWarning`, `{tools}` being the guest tools.
    checkpoint_warning = "Note that scheduled disk and memory snapshots will only be taken of VMs in this schedule that meet all of the following conditions:\n\n1. The VM is running or suspended at the time the snapshot is scheduled.\n2. The VM has {tools} installed.\n3. If the VM has a dedicated GPU assigned to it, this GPU supports disk and memory snapshots.",
    /// This console's own name of the guest tools.
    vm_tools = "XCP-ng Guest Tools",
    quiesce = "Quiesce the VM before taking snapshot (Windows only)",
    /// `label6`: the console's platform and version in place of the
    /// original's product.
    quiesce_removed = "As of {product} quiesced snapshots are no longer available. Please deselect this option to ensure your scheduled snapshots are taken successfully.",
    quiesce_product = "XCP-ng 8.1",
    disks_only = "Disks only",
    disks_and_memory = "Disks and memory",
    quiesced = "Quiesced snapshots",
    unknown = "Unknown",
    page_schedule = "Snapshot Schedule",
    schedule_title = "When do you want snapshots to be taken?",
    schedule_blurb = "Snapshots will be automatically taken according to the schedule specified below.",
    hourly = "Hourly snapshots",
    daily = "Daily snapshots",
    weekly = "Weekly snapshots",
    recur_at = "Recur at:",
    minutes_past = "minutes past each hour",
    recur_daily = "Recur every day at this time:",
    recur_weekly = "Recur at this time:",
    retention_blurb = "When the number of scheduled snapshots taken exceeds the maximum number specified here, the oldest scheduled snapshot will be deleted.",
    retention = "Maximum number of scheduled snapshots:",
    /// The console shows times in UTC.
    next_client = "The next scheduled local run time is {time}.",
    next_server = "The corresponding server local time is {time}.",
    hourly_format = "Hourly; at {min} minutes past each hour",
    daily_format = "Daily; at {time}",
    weekly_format = "Weekly; at {time} every {days}",
    invalid_schedule = "Invalid or incomplete schedule",
    page_finish = "Finish",
    finish_title = "Create the new schedule",
    finish_blurb = "Review the new snapshot schedule and click Previous if you want to change any settings or Finish to create the new schedule.",
    run_now = "Run the new snapshot schedule job when I click Finish",
    summary = "Job name:\n\t{name}\n\nSelected VMs:\n\t{vms}\n\nSnapshot type:\n\t{type}\n\nSchedule:\n\t{schedule}",

    // Days (`DaysWeekCheckboxes`, `*_LONG`, `*_SHORT`).
    sunday = "Sunday",
    monday = "Monday",
    tuesday = "Tuesday",
    wednesday = "Wednesday",
    thursday = "Thursday",
    friday = "Friday",
    saturday = "Saturday",
    sun = "Sun",
    mon = "Mon",
    tue = "Tue",
    wed = "Wed",
    thu = "Thu",
    fri = "Fri",
    sat = "Sat",

    // ---- Properties ----------------------------------------------------------
    /// The properties window title (`PropertiesDialog`: "'{0}' Properties").
    properties_title = "'{name}' Properties",
    tab_general = "General",
    general_blurb = "Enter the snapshot schedule name and an optional description.",
    ok = "OK",
    cancel = "Cancel",

    // ---- change warnings -----------------------------------------------------
    change_title = "Assign VM to snapshot schedule",
    change_one = "The VM '{vm}' is already assigned to the snapshot schedule '{old}'.\n    \nDo you want to assign it to the schedule '{new}' instead?",
    change_many = "Some of the VMs you selected are already assigned to a different snapshot schedule.\n    \nDo you want to assign these VMs to the schedule '{new}' instead?",

    // ---- Snapshots tab --------------------------------------------------------
    tab_in = "This VM is currently part of the snapshot schedule '{name}'",
    tab_not_in = "This VM is not currently part of a snapshot schedule",
    tab_view = "View snapshot schedules",

    // ---- operations ----------------------------------------------------------
    op_create = "Creating snapshot schedule '{name}'",
    op_status = "Change policy '{name}' status",
    op_run = "Running policy '{name}'",
    op_delete = "Delete policies",
    op_assign = "Assign VM to snapshot schedule '{policy}'",
    op_remove_one = "Remove VM '{vm}' from snapshot schedule '{policy}'",
    op_remove_many = "Remove VMs from snapshot schedule '{policy}'",
    /// This console's own (one operation for the dialog's save).
    op_properties = "Saving the properties of snapshot schedule '{name}'",
    step_deleting = "Deleting snapshot schedule '{name}'",
}

/// The title of an F12b snapshot schedule operation.
pub fn vmss_title(kind: &VmssKind) -> String {
    let s = vmss_strings();
    match kind {
        VmssKind::Create { name } => fill(s.op_create, &[("name", name)]),
        VmssKind::SetEnabled { name, .. } => fill(s.op_status, &[("name", name)]),
        VmssKind::RunNow { name } => fill(s.op_run, &[("name", name)]),
        VmssKind::Delete => s.op_delete.to_owned(),
        VmssKind::Assign { policy } => fill(s.op_assign, &[("policy", policy)]),
        VmssKind::Remove { vm, policy } => match vm {
            Some(vm) => fill(s.op_remove_one, &[("vm", vm), ("policy", policy)]),
            None => fill(s.op_remove_many, &[("policy", policy)]),
        },
        VmssKind::Properties { name } => fill(s.op_properties, &[("name", name)]),
    }
}

/// A day's name (`long`) or short name; 0 = Sunday.
pub fn day_name(day: u8, long: bool) -> &'static str {
    let s = vmss_strings();
    let (l, sh) = match day {
        0 => (s.sunday, s.sun),
        1 => (s.monday, s.mon),
        2 => (s.tuesday, s.tue),
        3 => (s.wednesday, s.wed),
        4 => (s.thursday, s.thu),
        5 => (s.friday, s.fri),
        _ => (s.saturday, s.sat),
    };
    if long { l } else { sh }
}
