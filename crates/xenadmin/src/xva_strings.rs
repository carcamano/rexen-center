//! User-interface strings of F11a: the Import and Export menus, the
//! Export wizard (XVA), the Export to File dialog, the Import wizard (XVA),
//! the operation titles and steps, and the transfer errors.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's
//! (`XenModel/Messages.resx` and the wizard pages' and dialogs' own
//! resources), without the `&` access keys. Templates use `{name}`
//! placeholders filled by [`crate::i18n::fill`].

use crate::i18n::fill;

macro_rules! xva_strings {
    ($( $(#[$doc:meta])* $field:ident = $en:literal, )*) => {
        /// One language's strings.
        pub struct XvaStrings {
            $( $(#[$doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: XvaStrings = XvaStrings {
            $( $field: $en, )*
        };
    };
}

xva_strings! {
    // ---- menus -------------------------------------------------------------
    menu_import = "Import...",
    menu_export = "Export...",
    menu_export_template = "Export to File...",
    menu_export_snapshot = "Export Snapshot As Template...",
    shutdown_before_export = "You need to shut down or suspend the VM before exporting it.",
    no_halted_vms = "There are no halted or suspended VMs to export",
    // ---- Export wizard -----------------------------------------------------
    export_wizard_title = "Export to File",
    page_appliance = "Export File Details",
    page_appliance_title = "Choose the VM export format, location and filename.",
    appliance_intro = "vApps and multiple VMs can be exported as an OVF/OVA package.\nA single VM can be exported as an OVF/OVA package or as an XVA file.",
    label_name = "Name:",
    label_location = "Location:",
    label_format = "Format:",
    format_xva = "XVA File (*.xva)",
    location_hint = "Folder path",
    error_invalid_name = "Appliance name is invalid.",
    error_invalid_dir = "Destination directory is invalid.",
    error_dir_missing = "Destination directory does not exist.",
    error_exists = "Appliance already exists, please select a new destination.",
    error_permissions = "You do not have write permission on the destination directory.",
    page_rbac = "Before you start",
    rbac_description = "Before you can proceed through this wizard, role-based security checks are made to ensure that you have sufficient privileges to use all the features in this wizard.",
    rbac_click_next = "Click Next to continue.",
    rbac_export = "A {role} user cannot export VMs. Login as a different user with sufficient privileges and try again.",
    rbac_import = "A {role} user cannot import VMs. Log in as a different user with sufficient privileges and try again.",
    rbac_affinity = "A {role} user cannot alter the Home Server setting. The value will be taken from the template, and if it relates to a server outside the resource pool then it will be ignored.",
    page_vms = "Virtual Machines",
    page_vms_title = "Confirm which VM to export",
    vms_intro = "Select the VM(s) you want to export from the list of available VMs and then click Next.\nNote that only VMs that are currently shut down or suspended are listed here.",
    vms_in_pool = "VMs in pool '{pool}':",
    vms_in_server = "VMs in server '{pool}':",
    search_hint = "Search",
    col_vm = "VM",
    col_description = "Description",
    col_size = "Disk Size",
    col_vapp = "vApp",
    vapp_none = "<None>",
    select_all = "Select All",
    clear_all = "Clear All",
    one_vm_selected = "1 VM selected",
    vms_selected = "{count} VMs selected",
    only_one_xva = "Only one VM may be selected for export in XVA format. To export multiple VMs, go back to the previous page and select OVF/OVA Package as the export format.",
    error_space = "Target destination has only {free} of free space and you need at least {needed} to export the selected VMs.",
    page_finish = "Finish",
    page_finish_export_title = "Review the export settings",
    finish_export_intro = "All the necessary information has been collected and the wizard is now ready to export the selected VM(s) using the settings shown below.\n\nPlease review these settings and click Previous if you need to go back and make any changes. Otherwise, click Finish to export the selected VM(s) and close this wizard. Export may take several minutes.",
    summary_package = "Package name:",
    summary_destination = "Destination path:",
    summary_vms = "Selected VMs:",
    verify = "Verify export on completion",
    verify_warning = "Verify export can take up to 5 hours per TB.",
    // ---- Export to File dialog (templates, snapshots) ----------------------
    file_name = "File name:",
    export_file_title = "Export to File",
    export_button = "Export",
    not_enough_space = "There might not be enough free disk space for the export operation to complete.\n\nEstimated space required: {needed}\nFree space: {free}\n\nAre you sure you want to export VM {vm}?",
    continue_export = "Continue with export",
    choose_destination = "Choose another destination",
    // ---- Import wizard -----------------------------------------------------
    import_wizard_title = "Import",
    import_xva_title = "Import XVA",
    page_source = "Import Source",
    page_source_title = "Locate the file you want to import",
    source_intro = "Enter the pathname of an exported VM or template, an OVF/OVA package or a virtual hard disk image file.",
    label_filename = "Filename:",
    path_hint = "File path (*.xva, *.xva.gz)",
    error_path_missing = "Path does not exist.",
    error_source_readonly = "You do not have write permission on the location provided. Read-write access is required to import this type of file.",
    uncompress_question = "{console} will uncompress the file to '{out}'. After uncompression the original file '{file}' will be deleted. Continue?",
    uncompressing = "Uncompressing file {file} ({done} done)...",
    uncompress_failed = "Failed to uncompress file {file}. Please see the logs for more information.",
    disk_capacity = "Disk size: {size}",
    page_home = "Home Server",
    page_home_title = "Select the location where the imported VM will be placed",
    home_blurb = "Click on a server to nominate it as the home server for the imported VM or for any new VMs to be based on the imported template. The home server will be used by default to start up the VM and to provide resources such as local storage.\n\nClick on a pool if you do not want to nominate a home server: the most suitable available server will be used.",
    page_storage = "Storage",
    page_storage_title = "Select target storage",
    storage_hint = "Select a storage repository where virtual disks for the new VM will be stored.\n\nWhen you have finished, click \"Import\" to begin importing the VM and proceed to configure networking options.",
    col_sr = "Name",
    col_sr_description = "Description",
    col_sr_free = "Free",
    sr_size_free = "{free} free of {size}",
    import_button = "Import >",
    no_srs = "No SR available with enough free space.",
    page_network = "Networking",
    page_network_title = "Select network to connect VM",
    network_blurb = "The default virtual network interfaces for the template you have selected are listed below. You can add, modify or delete virtual network interfaces, if required.\n\nWhen you have finished, click \"Next\" to continue to the next page.",
    network_label = "Virtual network interfaces installed on the new virtual machines:",
    col_name = "Name",
    col_mac = "MAC Address",
    col_network = "Network",
    interface = "interface {device}",
    auto_mac = "auto-generate",
    add = "Add",
    delete = "Delete",
    max_vifs = "The maximum number of virtual network interfaces has been reached.",
    page_finish_import_title = "Review the import settings",
    finish_import_intro = "All the necessary information has been collected and the wizard is ready to import using the settings shown below.\n\nPlease review these settings and click Previous if you need to go back and make any changes, otherwise click Finish to import and close this wizard. Import may take several minutes.",
    start_vms = "Start the new VM(s) automatically as soon as the import is complete",
    summary_vm_name = "VM name:",
    summary_home = "Home server:",
    summary_storage = "Storage:",
    summary_network = "Network:",
    // ---- operations --------------------------------------------------------
    op_export = "Export VM '{vm}' from '{pool}'",
    op_import = "Import VM from '{file}' to '{host}'",
    in_pool = "{name} in '{pool}'",
    on_server = "{name} on '{pool}'",
    step_exporting = "Exporting... ({received})",
    step_verifying = "Verifying Export...",
    step_uploading = "Uploading '{file}' to '{pool}' ({percent}% done)...",
    step_updating_vm = "Updating VM...",
    step_updating_template = "Updating Template...",
    step_waiting_vm = "Waiting for Import VM wizard to complete...",
    step_waiting_template = "Waiting for Import Template wizard to complete...",
    step_networks_vm = "Updating VM networking...",
    step_networks_template = "Updating Template networking...",
    step_complete_vm = "Import VM complete",
    step_complete_template = "Import Template complete.",
    step_removing = "Removing what the failed import left...",
    // ---- errors ------------------------------------------------------------
    err_header_checksum = "Export failed due to a header checksum mismatch.  Please retry the export.",
    err_block_checksum = "Export failed due to a block checksum mismatch.  Please retry the export.",
    err_disk_full = "Export failed: disk full.",
    err_vdi_in_use = "Export failed: disk in use elsewhere.",
    err_export_failed = "Export failed",
    err_import_missing = "Path {path} does not exist.",
    err_import_file = "The file could not be read: {reason}",
    err_import_failed = "Import failed: {reason}",
    err_template_exists = "The {product} Template that you are attempting to import already exists in the selected pool.",
}

pub fn xva_strings() -> &'static XvaStrings {
    &EN
}

/// The title of an XVA transfer.
pub fn xva_title(kind: &xen_core::operations::TransferKind) -> String {
    use xen_core::operations::TransferKind;
    let s = xva_strings();
    match kind {
        TransferKind::Export { vm, pool } => fill(s.op_export, &[("vm", vm), ("pool", pool)]),
        TransferKind::Import {
            file,
            host,
            pool,
            in_pool,
        } => {
            let location = fill(
                if *in_pool { s.in_pool } else { s.on_server },
                &[("name", host), ("pool", pool)],
            );
            fill(s.op_import, &[("file", file), ("host", &location)])
        }
        // F11b's (`crate::ovf_strings::ovf_title`).
        _ => String::new(),
    }
}

/// The step of a running transfer, when it is one.
pub fn xva_step(step: &xen_core::OperationStep) -> Option<String> {
    use xen_core::OperationStep as S;
    let s = xva_strings();
    let pick = |template: bool, vm: &'static str, t: &'static str| if template { t } else { vm };
    Some(match step {
        S::Exporting { received } => fill(
            s.step_exporting,
            &[("received", &xen_core::format::memory_size(*received, false))],
        ),
        S::VerifyingExport => s.step_verifying.to_owned(),
        S::Uploading {
            file,
            pool,
            percent,
        } => fill(
            s.step_uploading,
            &[
                ("file", file),
                ("pool", pool),
                ("percent", &percent.to_string()),
            ],
        ),
        S::UpdatingImported { template } => {
            pick(*template, s.step_updating_vm, s.step_updating_template).to_owned()
        }
        S::WaitingForImportWizard { template } => {
            pick(*template, s.step_waiting_vm, s.step_waiting_template).to_owned()
        }
        S::UpdatingImportNetworks { template } => {
            pick(*template, s.step_networks_vm, s.step_networks_template).to_owned()
        }
        S::ImportComplete { template } => {
            pick(*template, s.step_complete_vm, s.step_complete_template).to_owned()
        }
        S::RemovingImported => s.step_removing.to_owned(),
        _ => return None,
    })
}

/// The words of a transfer error, when it is one.
pub fn xva_error(code: &str, params: &[String]) -> Option<String> {
    use xen_core::xva_run::errors;
    let s = xva_strings();
    let p = |i: usize| params.get(i).map(String::as_str).unwrap_or_default();
    Some(match code {
        errors::EXPORT_HEADER_CHECKSUM => s.err_header_checksum.to_owned(),
        errors::EXPORT_BLOCK_CHECKSUM => s.err_block_checksum.to_owned(),
        errors::EXPORT_DISK_FULL => s.err_disk_full.to_owned(),
        errors::EXPORT_VDI_IN_USE => s.err_vdi_in_use.to_owned(),
        errors::EXPORT_FAILED if p(0).is_empty() => s.err_export_failed.to_owned(),
        errors::EXPORT_FAILED => format!("{}: {}", s.err_export_failed, p(0)),
        errors::IMPORT_FILE_MISSING => fill(s.err_import_missing, &[("path", p(0))]),
        errors::IMPORT_FILE_ERROR => fill(s.err_import_file, &[("reason", p(0))]),
        errors::IMPORT_FAILED => fill(s.err_import_failed, &[("reason", p(0))]),
        errors::IMPORT_TEMPLATE_EXISTS => fill(
            s.err_template_exists,
            &[("product", crate::branding::PLATFORM_NAME)],
        ),
        _ => return None,
    })
}
