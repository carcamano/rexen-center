//! User-interface strings of disaster recovery (F12b): the Disaster
//! Recovery menu, Configure, the Disaster Recovery wizard, its pre-checks,
//! the iSCSI device dialog of its Storage page, the operation titles and
//! steps.
//!
//! The English texts are the original console's (`DRConfigureDialog`,
//! `DRFailoverWizard*` and `IscsiDeviceConfigDialog` resources,
//! `XenModel/Messages.resx`), without the `&` access keys, `{0}` written
//! as named placeholders. A few are this console's own; each says so.

use xen_core::operations::DrKind;

use crate::i18n::fill;
use crate::locale::string_table;

string_table! {
    namespace = "dr", struct DrStrings, fn dr_strings;

    // ---- menus -------------------------------------------------------------
    menu = "Disaster Recovery",
    menu_configure = "Configure...",
    menu_wizard = "Disaster Recovery Wizard...",

    // ---- Configure ---------------------------------------------------------
    configure_title = "Configure DR on '{pool}'",
    configure_blurb1 = "The configuration data needed to recover VMs and vApps in the event of a disaster must be stored on one or more SRs in this pool. ",
    configure_blurb2 = "Select the SRs where the VM configuration data will be stored: ",
    col_name = "Name",
    col_description = "Description",
    col_type = "Type",
    col_available = "Available",
    enable_note = "DR configuration is not complete until storage mirroring has been set up and the selected SRs are being replicated. ",
    disable_note1 = "You have not selected any SRs ",
    disable_note2 = "Because no VM configuration data will be stored anywhere in the pool, it will not be possible to recover VMs or vApp from this pool in the event of a disaster.",
    limit_note1 = "You have selected too many SRs ",
    limit_note2 = "You can only select up to {count} SRs to store the VM configuration data.",
    ok = "OK",
    cancel = "Cancel",
    /// This console's own: while the SRs are asked about.
    checking_srs = "Checking the SRs...",
    /// This console's own: no SR can hold the metadata.
    no_srs = "No SR of this pool can hold the VM configuration data.",

    // ---- the wizard --------------------------------------------------------
    wizard_title = "Disaster Recovery",
    title_failover = "Disaster Recovery - Failover to pool '{pool}'",
    title_failback = "Disaster Recovery - Failback to pool '{pool}'",
    title_dryrun = "Disaster Recovery - Test Failover to pool '{pool}'",
    page_rbac = "Permissions",
    rbac_message = "A {role} user does not have sufficient permissions to use Disaster Recovery. Login as a different user with sufficient privileges and try again.",
    rbac_click_next = "Click Next to continue.",
    back = "< Previous",
    next = "Next >",
    finish = "Finish",
    // Welcome.
    page_welcome = "Welcome",
    welcome_title = "What do you want to do?",
    welcome_blurb = "Welcome to the Disaster Recovery wizard. What do you want to do? ",
    failover = "Failover",
    failover_blurb = "Fail over critical vApps and virtual machines to pool '{pool}' on your disaster recovery (DR) site. ",
    failback = "Failback",
    failback_blurb = "Fail back vApps and virtual machines to your primary data site.",
    dryrun = "Test Failover",
    dryrun_blurb = "In a test failover, vApps and VMs are recovered to pool '{pool}' on the DR site but will not be started up, enabling you to validate your failover and recovery procedures while maintaining regular business operations at your primary data site.",
    // Before You Start.
    page_first = "Before You Start",
    first_title_failover = "Failover prerequisites",
    first_title_failback = "Failback prerequisites",
    first_title_dryrun = "Test failover prerequisites",
    important = "IMPORTANT",
    failover_line1 = "This wizard will take you through the steps needed to fail over your critical vApps and VMs in the event of a site/datacenter failure at your primary data site.",
    failover_line2 = "Configuration information for the vApps and VMs to be recovered will be retrieved from remote storage mirrors. Before failover begins, the wizard will run a number of checks to ensure that the selected vApps and VMs can be brought back up on your DR site.",
    failover_line3 = "You will need to provide information about the following as you step though this wizard:\n\n \u{2022}  The storage mirrors where the vApp and VM configuration data is currently stored.\n \u{2022}  Which vApps and VMs you want to fail over to the DR site.",
    failover_line4 = "Before failover can take place, storage mirroring \u{2013} remote replication of your VM data \u{2013} must be halted.",
    failback_line1 = "This wizard will take you through the steps needed to fail back vApps and VMs to your primary data site.",
    failback_line2 = "Configuration information for the vApps and VMs to be failed back will be retrieved from remote storage mirrors. Before failback begins, the wizard will run a number of checks to ensure that the selected vApps and VMs can be recovered to your primary data site.",
    failback_line3 = "You will need to provide information about the following as you step through this wizard:\n\n \u{2022}  The storage mirrors where the vApp and VM configuration data is currently stored.\n \u{2022}  Which vApps and VMs you want to fail back.",
    failback_line4 = "Before failback can take place, storage mirroring \u{2013} remote replication of your VM data \u{2013} must be halted.",
    dryrun_line1 = "This wizard will take you through the steps needed to perform a test failover of your critical vApps and VMs. The vApps and VMs you select will be recovered to pool \u{2018}{pool}\u{2019} on the DR site but will not be started up.",
    dryrun_line2 = "Configuration information for the vApps and VMs to be failed over will be retrieved from remote storage mirrors. Before test failover begins, the wizard will run a number of checks to ensure that the selected vApps and VMs can be recovered to your DR site.",
    dryrun_line3 = "You will need to provide information about the following as you step through this wizard:\n\n \u{2022}  The storage mirrors where the vApp and VM configuration data is currently stored.\n \u{2022}  Which vApps and VMs you want to fail over to the DR site.",
    // Storage.
    page_storage = "Locate Mirrored SRs",
    storage_title = "Locate the storage repositories where your recovery data is stored",
    storage_failover = "Select the storage repositories (SRs) containing the VM configuration data for the VMs and vApps that you want to fail over. If you know which SRs contain your VMs' disks, you can also include them now.",
    storage_failback = "Select the storage repositories (SRs) containing the configuration data for the VMs and vApps that you want to fail back. If you know which SRs contain your VMs' disks, you can also include them now.",
    col_metadata = "VM configuration data",
    yes = "Yes",
    no = "No",
    find_srs = "Find Storage Repositories",
    find_hba = "Find Hardware HBA SRs...",
    find_iscsi = "Find Software iSCSI SRs...",
    select_all = "Select All",
    clear_all = "Clear All",
    scan_none = "No Storage Repositories were found",
    /// This console's own: while the metadata is read.
    loading_metadata = "Loading the VM configuration data...",
    /// This console's own: while SRs are introduced.
    preparing = "Preparing Storage Repositories",
    // iSCSI device dialog.
    iscsi_title = "iSCSI Device configuration",
    iscsi_blurb = "Provide a target host for your iSCSI storage, indicating your target IQN and your target LUN before proceeding.",
    iscsi_host = "Target Host:",
    iscsi_chap = "Use CHAP",
    iscsi_user = "User:",
    iscsi_password = "Password:",
    iscsi_iqn = "Target IQN:",
    iscsi_lun = "Target LUN:",
    iscsi_discover_iqns = "Discover IQNs",
    iscsi_discover_luns = "Discover LUNs",
    // vApps & VMs.
    page_vapps = "Select vApps & VMs",
    vapps_title_failover = "Select the vApps or individual virtual machines to fail over to the target pool",
    vapps_title_failback = "Select the vApps or individual virtual machines to fail back to the target pool",
    vapps_failover = "Select the vApps and individual VMs you want to recover and specify the required power state for them after they have been recovered.",
    vapps_failback = "Select the vApps and individual VMs you want to fail back and specify the required power state for them after they have been recovered to pool \u{2018}{pool}\u{2019}.",
    vapps_found = "vApps and VMs found on the mirrored SRs: ",
    current_pool = "Current pool",
    power_state = "Power state after recovery ",
    failover_start = "Start up all recovered vApps and VMs immediately",
    failover_paused = "Start up recovered vApps and VMs in a paused state",
    failover_none = "Do not start up recovered vApps and VMs",
    failback_start = "Start up all failed back vApps and VMs immediately",
    failback_paused = "Start up failed back vApps and VMs in a paused state",
    failback_none = "Do not start up failed back vApps and VMs",
    /// This console's own: no metadata was found on the chosen SRs.
    no_metadata = "No VM configuration data was found on the selected SRs.",
    // Pre-checks.
    page_prechecks = "Pre-checks",
    prechecks_title_failover = "Check that the selected vApps and VMs can be failed over",
    prechecks_title_failback = "Check that the selected vApps and VMs can be failed back",
    prechecks_failover = "Pre-checks are performed to ensure that the selected vApps and VMs can be failed over. Please take appropriate action to resolve any issues.",
    prechecks_failback = "Failover pre-checks are performed to ensure that the selected vApps and VMs can be failed back to the selected pool. Please take appropriate action to resolve any issues.",
    running_prechecks = "Running failover pre-checks...",
    hide_successful = "Hide successful pre-checks",
    check_again = "Check Again",
    resolve_all = "Resolve All",
    must_resolve = "You need to resolve the issues found during pre-checks before you can continue.",
    continue_failover = "Once done, click Fail Over to begin recovery of the selected vApps and VMs.",
    continue_failback = "Once done, click Fail Back to begin recovery of the selected vApps and VMs to your primary data site.",
    continue_dryrun = "Once done, click Fail Over to begin the test recovery of the selected vApps and VMs.",
    next_failover = "Fail Over",
    next_failback = "Fail Back",
    header = "Pre-check {index} of {count}: {group}",
    group_ha = "Checking HA status",
    group_power = "Checking power state in source pools",
    group_existing = "Checking for existing vApps and VMs in '{pool}'",
    group_storage = "Checking storage availability in '{pool}'",
    check_ok = "{check}: OK",
    check_problem = "{check}: {problem}",
    check_ha = "HA on pool {pool}",
    check_vm = "VM '{name}'",
    check_vapp = "vApp '{name}'",
    problem_ha = "Enabled",
    problem_ha_fix = "Turn HA off",
    problem_running_vm = "Is running on pool '{pool}'",
    problem_running_vm_fix = "Shut down VM",
    problem_running_vapp = "Is running on pool '{pool}'",
    problem_running_vapp_fix = "Shut down vApp",
    problem_existing_vm = "Newer version already exists on pool {pool}",
    problem_existing_vm_fix = "Delete VM",
    problem_existing_vapp = "Newer versions of its VMs already exist on pool {pool}",
    problem_existing_vapp_fix = "Delete VMs...",
    warning_existing = "Already exists on pool {pool}. It will be replaced.",
    problem_missing_sr = "Missing SR '{name}'",
    problem_missing_sr_fix = "Attach SR",
    problem_local_sr = "Uses local storage '{name}'",
    problem_missing_sr_no_info = "Missing SR information not found",
    problem_missing_srs = "Missing {count} SRs",
    problem_missing_srs_fix = "Attach SRs",
    confirm_delete_vm = "Are you sure you want to delete VM '{vm}' from '{pool}'?",
    confirm_delete_vms = "Are you sure you want to delete the following VMs from '{pool}'?\n\n{vms}",
    // Recover.
    page_recover = "Progress",
    recover_title_failover = "{product} Disaster Recovery progress",
    recover_title_failback = "Progress of failback to pool '{pool}'",
    recover_title_dryrun = "Progress of test failover to pool '{pool}'",
    recover_blurb_failover = "{product} Disaster Recovery is in progress. This may take some time.\n\nOnce done, click Next to see the summary report.",
    recover_blurb_failback = "Failback is in progress. This may take some time.\n\nOnce done,  click Next to see the summary report.",
    recover_blurb_dryrun = "Test failover is in progress. This may take some time.\n\nOnce done, click Next to remove the vApps and VMs that were failed over and to see the summary report.",
    recover_done_failover = "{product} Disaster Recovery is complete.",
    recover_done_failback = "Failback to pool '{pool}' is complete.",
    recover_done_dryrun = "Test failover to pool '{pool}' is complete.",
    overall = "Overall progress: {done} of {count} tasks completed",
    col_task = "Task",
    col_status = "Status",
    status_pending = "Pending",
    status_working = "Working",
    status_completed = "Completed",
    status_failed = "Failed",
    // Report.
    page_report = "Summary",
    report_title_failover = "Summary of failover to pool '{pool}'",
    report_title_failback = "Summary of failback to pool '{pool}'",
    report_title_dryrun = "Summary of test failover to pool '{pool}'",
    report_selected_srs = "Selected SRs: ",
    report_selected = "Selected VMs and vApps, grouped by pool: ",
    report_power = "Power state after recovery: {state}",
    report_introduced = "SRs introduced during this wizard: {srs}",
    report_introduced_none = "SRs introduced during this wizard: None",
    report_warnings = "Warnings: ",
    report_warnings_none = "Warnings: None",
    report_started = "Recovery process started",
    report_succeeded = "{title} - Succeeded",
    report_failed = "{title} - Failed: {error}",
    report_dr_cleanup = "DR cleanup",
    report_dryrun_cleanup = "Test Failover cleanup",
    report_revert = "Revert pre-check actions",

    // ---- operations ----------------------------------------------------------
    op_configure = "Configuring DR",
    op_task_create = "Prepare Storage Repositories",
    op_task_destroy = "DR cleanup ({srs})",
    op_recover_vm = "Recover VM '{name}'",
    op_recover_vapp = "Recover vApp '{name}'",
    op_start = "Start VMs and vApps",
    op_destroy_vm = "Delete VM",
    op_destroy_vms = "Delete VMs",
    op_destroy_vapps = "Delete vApps",
    op_hard_shut_down_vapp = "Shutting down vApp '{name}'",
    step_enabling = "Enabling pool metadata replication on storage '{sr}'",
    step_disabling = "Disabling pool metadata replication on storage {sr}",
    step_starting_vm = "Starting VM '{vm}'",
    step_starting_vapp = "Starting vApp '{name}'",
    err_nothing_to_start = "There are no VMs or vApps to start",
}

/// The title of an F12b disaster recovery operation.
pub fn dr_title(kind: &DrKind) -> String {
    let s = dr_strings();
    match kind {
        DrKind::Configure => s.op_configure.to_owned(),
        DrKind::TaskCreate => s.op_task_create.to_owned(),
        DrKind::TaskDestroy { srs } => fill(s.op_task_destroy, &[("srs", srs)]),
        DrKind::Recover { name, vapp } => fill(
            if *vapp {
                s.op_recover_vapp
            } else {
                s.op_recover_vm
            },
            &[("name", name)],
        ),
        DrKind::Start => s.op_start.to_owned(),
        DrKind::DestroyVms { count } => if *count == 1 {
            s.op_destroy_vm
        } else {
            s.op_destroy_vms
        }
        .to_owned(),
        DrKind::DestroyVApps => s.op_destroy_vapps.to_owned(),
        DrKind::HardShutDownVApp { name } => fill(s.op_hard_shut_down_vapp, &[("name", name)]),
    }
}

/// The words of an F12b step (snapshot schedules and DR), when it is one.
pub fn f12b_step(step: &xen_core::OperationStep) -> Option<String> {
    use xen_core::OperationStep;
    let s = dr_strings();
    Some(match step {
        OperationStep::EnablingDr { sr } => fill(s.step_enabling, &[("sr", sr)]),
        OperationStep::DisablingDr { sr } => fill(s.step_disabling, &[("sr", sr)]),
        OperationStep::StartingRecoveredVm { vm } => fill(s.step_starting_vm, &[("vm", vm)]),
        OperationStep::StartingVApp { name } => fill(s.step_starting_vapp, &[("name", name)]),
        OperationStep::DeletingPolicy { name } => fill(
            crate::vmss_strings::vmss_strings().step_deleting,
            &[("name", name)],
        ),
        _ => return None,
    })
}
