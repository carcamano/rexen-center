//! User-interface strings of F11b: the Export wizard's OVF/OVA pages, the
//! Import wizard's OVF/OVA and disk image pages, the operation titles and
//! steps, and the errors.
//!
//! The English texts are the original console's (the wizard pages'
//! resources, `XenModel/Messages.resx` and `XenOvfApi/Messages.resx`),
//! without the `&` access keys, `{0}` written as named placeholders. A few
//! are this console's own, where it differs from the original (no file
//! dialogs, no signing, no converter); each says so.

use crate::i18n::fill;
use crate::locale::string_table;

string_table! {
    namespace = "ovf", struct OvfStrings, fn ovf_strings;

    // ---- Export wizard -----------------------------------------------------
    export_ovf_title = "Export OVF/OVA Package",
    format_ovf = "OVF/OVA Package (*.ovf, *.ova)",
    error_appliance_exists = "Appliance already exists, please select a new destination.",
    page_vms_title_ovf = "Confirm which VMs to include in the OVF/OVA package",
    ovf_disk_too_large = "VMs with disks greater than {size} cannot be exported as an OVF/OVA package.",
    vtpm_warning = "The OVF/OVA format does not preserve vTPM data. Some of the VMs' security features may be deactivated and the VMs may not start. Use the XVA format to preserve vTPM data.",
    rbac_export_appliance = "A {role} user cannot export appliances. Login as a different user with sufficient privileges and try again.",
    page_eula = "EULAs",
    page_export_eula_title = "Optionally, include EULAs in the OVF/OVA package",
    export_eula_intro = "You can include end user license agreement (EULA) documents in the exported OVF/OVA package.",
    eula_files = "EULA files:",
    eula_add = "Add...",
    eula_remove = "Remove",
    eula_view = "View",
    /// This console's own: no file dialog, the path is typed.
    eula_path_hint = "Path of a text (.txt) or rich text (.rtf) file",
    eula_max = "Maximum number of EULAs  ({max}) has been reached.",
    eula_invalid = "{file} is not a valid EULA document.",
    page_options = "Advanced Options",
    page_options_title = "Select the manifest, signing and output file format options you want",
    group_manifest = "Manifest and digital signing",
    create_manifest = "Create a manifest",
    sign_package = "Sign the OVF package",
    /// This console's own: signing is not available.
    sign_unavailable = "Signing needs the private key of a .pfx certificate, which this console cannot read yet; the package can carry a manifest only.",
    group_output = "Output file options",
    create_ova = "Create OVA package (single .ova file)",
    compress = "Compress package",
    compress_ova_hint = "The OVA package will be compressed to an .ova.gz file.",
    compress_ovf_hint = "Each of the disks of the OVF package will be compressed to a .vhd.gz file.",
    finish_export_ovf_intro = "All the necessary information has been collected and the wizard is now ready to export the selected VM(s) using the settings shown below.\n\nPlease review these settings and click Previous if you need to go back and make any changes. Otherwise, click Finish to export the selected VM(s) and close this wizard. Export may take several minutes.",
    summary_eulas = "Selected EULA files:",
    summary_manifest = "A manifest will be created:",
    summary_signed = "The package will be digitally signed:",
    summary_ova = "An OVA package will be created:",
    summary_compressed = "Files will be compressed:",
    yes = "Yes",
    no = "No",
    // ---- Import wizard: source ---------------------------------------------
    import_ovf_title = "Import OVF/OVA Package",
    import_image_title = "Import Disk Image",
    error_unknown_type = "Unknown file type.",
    error_encrypted = "The selected package is encrypted. {console} does not support import of encrypted OVF/OVA packages. Please select a different file.",
    error_not_valid_image = "The selected file is not a valid virtual hard disk image file.",
    /// This console's own (plan decision 12: no converter).
    error_needs_conversion = "The selected file is a {format} disk image. {console} does not convert disk images: convert it to VHD or raw first (for example with qemu-img), then import that file.",
    error_vmdk_in_package = "The package holds {format} disks ({file}). {console} does not convert disk images: convert them to VHD first, then import the package.",
    error_no_ovf = "This appliance does not contain a valid OVF file.",
    error_file_missing_in_package = "Failed to find file {file} listed in the reference section.",
    error_ova_readonly = "You do not have write permission on the location provided. Read-write access is required to import this type of file.",
    validation_warnings = "The following warnings were issued while validating the package to import:",
    warn_no_version = "The OVF version is not set; assuming 1.0.0.",
    warn_version = "OVF version '{version}' is not supported.",
    warn_extension = "File {file} has an unsupported extension.",
    warn_hardware = "Found unexpected virtual hardware type '{type}'.",
    warn_cpu = "Processor quantity is invalid.",
    warn_cpu_limit = "Processor quantity {count} exceeds the limit of {limit}.",
    warn_memory_id = "Memory has an empty InstanceID, creating a new one.",
    warn_memory = "Memory quantity is invalid.",
    warn_memory_units = "Memory allocation unit is invalid.",
    warn_network = "A network is not connected to a device.",
    warn_required = "A required resource with id {id} and name {name} was not recognized.",
    warn_disk_link = "Disk linkage (file to RASD) does not exist for {name}.",
    // ---- Import wizard: VM definition (images) -----------------------------
    page_vm_definition = "VM Definition",
    page_vm_definition_title = "Define Virtual Machine",
    vm_definition_intro = "Enter a name that will help you identify the virtual machine later, specify the number of virtual CPUs and the amount of memory that will be initially allocated to the new VM and select the boot mode that will be applied to the new VM.",
    label_vm_name = "VM Name",
    label_cpus = "Number of CPUs",
    label_memory = "Memory",
    mb = "MB",
    // ---- Import wizard: EULAs ----------------------------------------------
    page_import_eula_title = "Review End User License Agreement (EULA)",
    import_eula_intro = "Review and accept the EULAs to proceed or press Cancel to exit the wizard.",
    eula_tab = "EULA",
    eula_accept = "I accept the End User License Agreements.",
    eula_too_many = "Too many EULAs to display, cannot continue.",
    // ---- Import wizard: location -------------------------------------------
    page_location = "Location",
    page_location_title = "Select the location where the imported VM will be placed",
    location_intro = "Choose the pool or standalone server where you want to import the VM(s).",
    rbac_import_appliance = "A {role} user cannot import appliances. Login as a different user with sufficient privileges and try again.",
    rbac_import_image = "A {role} user cannot import virtual disk images. Login as a different user with sufficient privileges and try again.",
    // ---- Import wizard: storage --------------------------------------------
    page_storage_title = "Select target storage",
    storage_intro = "Place the virtual disks in the VMs you are importing onto storage repositories (SRs) in the destination pool or standalone server.",
    all_on_one = "Place all imported virtual disks on this target SR:",
    each_on_own = "Place imported virtual disks onto specified target SRs:",
    col_vm_disk = "VM - Virtual Disk",
    col_target_sr = "Storage Repository",
    sr_too_small = "SR '{sr}' does not have {size} of free space to import virtual disk {disk}.",
    // ---- Import wizard: networking -----------------------------------------
    page_network_title = "Select network to connect VM",
    network_intro = "Map the virtual network interfaces in the VMs you are importing to networks in the destination pool or standalone server.",
    network_label = "Virtual network interfaces in imported VMs:",
    col_vm_vif = "VM - Virtual Network Interface",
    col_target_network = "Target Network",
    none = "<None>",
    auto_mac = "<autogenerated MAC>",
    // ---- Import wizard: security -------------------------------------------
    page_security = "Security",
    page_security_title = "Select your import security settings",
    security_intro = "Security settings allow you to validate the OVF/OVA package you are importing.",
    verify_signature = "Verify digital signature",
    verify_signature_text = "The appliance has been digitally signed. You can choose to verify the digital signature of the appliance is valid.",
    verify_manifest = "Verify manifest content",
    verify_manifest_text = "The appliance has a manifest file. You can choose to verify the manifest matches the appliance files.",
    view_certificate = "View Certificate",
    certificate_missing = "Could not find the appliance certificate. The file is missing.",
    /// This console's own: what the signature check covers.
    certificate_subject = "Signed by: {subject}. The signature is checked against this certificate's key; whether the certificate itself is trusted (its chain, dates and revocation) is not checked by this console.",
    // ---- Import wizard: boot options ---------------------------------------
    page_boot = "Boot options",
    page_boot_title = "Select the boot mode for the new virtual machine",
    boot_intro = "Select the boot mode that will be applied to the new virtual machine.",
    boot_group = "Boot Mode",
    boot_bios = "BIOS Boot",
    boot_uefi = "UEFI Boot",
    boot_secure = "UEFI Secure Boot",
    // ---- Import wizard: finish ---------------------------------------------
    summary_package_name = "Package name:",
    summary_verify_manifest = "Verify manifest content:",
    summary_verify_signature = "Verify digital signature:",
    summary_image = "Virtual Hard Disk image:",
    summary_vm_name = "VM name:",
    summary_cpus = "CPU count:",
    summary_memory = "Memory:",
    summary_memory_value = "{mb} MB",
    summary_boot = "Boot Mode",
    summary_storage = "Storage:",
    summary_network = "Network:",
    summary_vm_storage = "{vm} storage:",
    summary_vm_network = "{vm} network:",
    // ---- operations --------------------------------------------------------
    op_export_ovf = "Export OVF package '{name}' from '{pool}'",
    op_export_ova = "Export OVA package '{name}' from '{pool}'",
    op_import_appliance = "Import Appliance '{name}' to '{pool}'",
    op_import_image = "Import VM '{vm}' from Disk Image to '{pool}'",
    step_exporting_disk = "Exporting disk '{disk}' to '{file}' ({index} of {count})...",
    step_verifying_disk = "Verifying disk '{disk}'...",
    step_checksum = "Calculating checksum for file '{file}'...",
    step_manifest = "Creating manifest file...",
    step_archiving = "Archiving appliance files into single package file '{file}'...",
    step_verifying_signature = "Verifying Digital Signature...",
    step_verifying_manifest = "Verifying Manifest Content...",
    step_creating_vm = "Creating VM '{vm}'...",
    step_attached_cd = "Attached CD/DVD Drive.",
    step_expanding = "Expanding {file}...",
    step_preparing_vdi = "Preparing VDI for '{disk}'...",
    step_importing_disk = "Importing '{file}' ({index} of {count})...",
    // ---- errors ------------------------------------------------------------
    err_vm_running = "Please shut down or suspend VM '{vm}' before exporting.",
    err_disk_verify = "Verification for disk '{disk}' failed. See logs for details.",
    err_export_failed = "Export failed: {reason}",
    err_signature = "Signature verification failed. {reason}",
    err_manifest = "Manifest verification failed. {reason}",
    err_package = "The package could not be read: {reason}",
    err_file_missing = "File {file} not found",
    err_unsupported_disk = "{file} is a {format} disk image, which {console} does not convert. Convert it to VHD or raw first.",
    err_sr_space = "SR '{sr}' does not have {size} of free space to import virtual disk {disk}.",
    err_no_sr = "The storage repository was not found and a default was not assigned.",
    err_file_error = "The file {file} could not be read: {reason}",
    err_import_failed = "Importing {file} failed: {reason}",
}

/// The title of an F11b transfer, when it is one.
pub fn ovf_title(kind: &xen_core::operations::TransferKind) -> Option<String> {
    use xen_core::operations::TransferKind;
    let s = ovf_strings();
    Some(match kind {
        TransferKind::ExportOvf { name, pool, ova } => fill(
            if *ova {
                s.op_export_ova
            } else {
                s.op_export_ovf
            },
            &[("name", name), ("pool", pool)],
        ),
        TransferKind::ImportAppliance { name, pool, .. } => {
            fill(s.op_import_appliance, &[("name", name), ("pool", pool)])
        }
        TransferKind::ImportImage { vm, pool, .. } => {
            fill(s.op_import_image, &[("vm", vm), ("pool", pool)])
        }
        _ => return None,
    })
}

/// The step of a running F11b transfer, when it is one.
pub fn ovf_step(step: &xen_core::OperationStep) -> Option<String> {
    use xen_core::OperationStep as S;
    let s = ovf_strings();
    Some(match step {
        S::ExportingDisk {
            disk,
            file,
            index,
            count,
        } => fill(
            s.step_exporting_disk,
            &[
                ("disk", disk),
                ("file", file),
                ("index", &index.to_string()),
                ("count", &count.to_string()),
            ],
        ),
        S::VerifyingDisk { disk } => fill(s.step_verifying_disk, &[("disk", disk)]),
        S::CalculatingChecksum { file } => fill(s.step_checksum, &[("file", file)]),
        S::CreatingManifest => s.step_manifest.to_owned(),
        S::ArchivingPackage { file } => fill(s.step_archiving, &[("file", file)]),
        S::VerifyingSignature => s.step_verifying_signature.to_owned(),
        S::VerifyingManifest => s.step_verifying_manifest.to_owned(),
        S::CreatingVm { vm } => fill(s.step_creating_vm, &[("vm", vm)]),
        S::AttachedCd => s.step_attached_cd.to_owned(),
        S::ExpandingFile { file } => fill(s.step_expanding, &[("file", file)]),
        S::PreparingVdi { disk } => fill(s.step_preparing_vdi, &[("disk", disk)]),
        S::ImportingDisk { file, index, count } => fill(
            s.step_importing_disk,
            &[
                ("file", file),
                ("index", &index.to_string()),
                ("count", &count.to_string()),
            ],
        ),
        _ => return None,
    })
}

/// The words of an F11b error, when it is one.
pub fn ovf_error(code: &str, params: &[String]) -> Option<String> {
    use xen_core::ovf_run::errors as e;
    let s = ovf_strings();
    let p = |i: usize| params.get(i).map(String::as_str).unwrap_or_default();
    let console = crate::branding::PRODUCT_NAME;
    Some(match code {
        e::OVF_VM_RUNNING => fill(s.err_vm_running, &[("vm", p(0))]),
        e::OVF_DISK_VERIFY => fill(s.err_disk_verify, &[("disk", p(0))]),
        e::OVF_EXPORT_FAILED => fill(s.err_export_failed, &[("reason", p(0))]),
        e::OVF_SIGNATURE => fill(s.err_signature, &[("reason", p(0))]),
        e::OVF_MANIFEST => fill(s.err_manifest, &[("reason", p(0))]),
        e::OVF_PACKAGE => fill(s.err_package, &[("reason", p(0))]),
        e::OVF_FILE_MISSING => fill(s.err_file_missing, &[("file", p(0))]),
        e::OVF_UNSUPPORTED_DISK => fill(
            s.err_unsupported_disk,
            &[("file", p(0)), ("format", p(1)), ("console", console)],
        ),
        e::OVF_SR_SPACE => fill(
            s.err_sr_space,
            &[("sr", p(0)), ("size", p(1)), ("disk", p(2))],
        ),
        e::OVF_NO_SR => s.err_no_sr.to_owned(),
        e::OVF_FILE_ERROR => fill(s.err_file_error, &[("file", p(0)), ("reason", p(1))]),
        e::OVF_IMPORT_FAILED => fill(s.err_import_failed, &[("file", p(0)), ("reason", p(1))]),
        e::OVF_ENCRYPTED => fill(s.error_encrypted, &[("console", console)]),
        _ => return None,
    })
}
