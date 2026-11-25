//! User-interface strings of storage repositories: the Storage menu, the
//! New SR wizard, the Repair and Properties dialogs, the confirmations and
//! the operation titles.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's
//! (`XenModel/Messages.resx` and the wizard pages' and dialogs' own
//! resources), without the `&` access keys. Templates use `{name}`
//! placeholders filled by [`crate::i18n::fill`].

macro_rules! sr_strings {
    ($( $(#[$doc:meta])* $field:ident = $en:literal, )*) => {
        /// One language's strings.
        pub struct SrStrings {
            $( $(#[$doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: SrStrings = SrStrings {
            $( $field: $en, )*
        };
    };
}

sr_strings! {
    // Menus and buttons.
    menu_storage = "Storage",
    new_sr = "New SR...",
    new_storage_toolbar = "New Storage",
    repair = "Repair...",
    set_default = "Set as Default",
    trim = "Reclaim freed space",
    detach = "Detach...",
    reattach = "Reattach...",
    forget = "Forget",
    destroy = "Destroy...",
    properties = "Properties",
    rescan = "Rescan",
    yes = "Yes",
    no = "No",
    ok = "OK",
    cancel = "Cancel",
    close = "Close",
    previous = "< Previous",
    next = "Next >",
    finish = "Finish",

    // Confirmations.
    detach_title = "Detach Storage Repository",
    detach_text = "Detaching this storage repository will make the virtual disks that it contains inaccessible. The contents of the virtual disks themselves will remain intact.\n\nIf you subsequently reattach the storage repository, you will need to provide the correct device configuration details.\n\nAre you sure you want to detach this storage repository?",
    forget_title = "Forget Storage Repository",
    /// `{name}`
    forget_text = "Are you sure you want to forget '{name}'?\n\nForgetting this storage repository will permanently remove the information used to connect the virtual machines to the virtual disks in the storage repository. The contents of the virtual disks themselves will remain intact.\n\nThis action is final and unrecoverable.",
    forget_yes = "Yes, Forget",
    destroy_title = "Destroy Storage Repository",
    /// `{name}`
    destroy_text = "Are you sure you want to destroy '{name}'?\n\nDestroying this storage repository will permanently remove all associated virtual disks from the underlying storage device.\n\nThis action is final and unrecoverable.",
    destroy_yes = "Yes, Destroy",
    trim_title = "Reclaim freed space",
    trim_text = "The process of reclaiming space may place significant load on the storage controller, temporarily affecting the storage's performance.\nWould you like to carry out the operation at this time?",

    // Why an operation is not offered.
    problem_detached = "The SR is currently detached.",
    problem_running_vms = "This SR contains running VMs.",
    /// `{console}`
    problem_not_by_console = "This SR cannot be destroyed or forgotten with {console}.",
    problem_in_progress = "There is currently an action in progress on this SR.",
    problem_trim_unsupported = "Reclaiming freed space is not supported on this SR",
    problem_scan_in_progress = "This SR already has a scan in progress",

    // The wizard.
    /// `{pool}`
    wizard_title = "New Storage Repository - {pool}",
    /// `{pool}`
    wizard_title_attach = "Attach Storage Repository - {pool}",
    page_type = "Type",
    page_name = "Name",
    page_location = "Location",
    type_title = "Choose the type of new storage",
    label_vdi_storage = "Virtual disk storage",
    label_iso_library = "ISO library",
    type_nfs = "NFS",
    type_iscsi = "iSCSI",
    type_smb = "SMB",
    type_local = "Local Storage",
    type_nfs_iso = "NFS ISO",
    type_smb_iso = "Windows File Sharing (SMB/CIFS)",
    blurb_nfs = "NFS servers are a common form of shared filesystem infrastructure, and can be used as a storage repository substrate for virtual disks.\n\nAs NFS storage repositories are shared, the virtual disks stored in them allow VMs to be started on any server in a resource pool and to be migrated between them using Live Migration.\n\nWhen you configure an NFS storage repository, you simply provide the host name or IP address of the NFS server and the path to a directory that will be used to contain the storage repository. The NFS server must be configured to export the specified path to all servers in the pool.",
    blurb_iscsi = "iSCSI or Fibre Channel access to a shared LUN can be configured to host fully provisioned virtual disks using LVM or be formatted with the GFS2 cluster file system for hosting thinly provisioned virtual disks.",
    blurb_smb = "SMB servers are a common form of Windows shared filesystem infrastructure, and can be used as a storage repository substrate for virtual disks.\n\nAs SMB storage repositories are shared, the virtual disks stored in them allow VMs to be started on any server in a resource pool and to be migrated between them using Live Migration.\n\nWhen you configure a SMB storage repository, you simply provide the host name or IP address of the SMB server, the user name, the password and the path to a directory that will be used to contain the storage repository. The SMB server must support version 3 and be configured to export the specified path to all servers in the pool.",
    blurb_local = "Select this option to create a local storage repository.",
    blurb_nfs_iso = "Select this option if you have a library of VM installation ISO images available as a NFS share that you want to attach to your host or pool.",
    blurb_smb_iso = "Select this option if you have a library of VM installation ISO images available as a Windows (SMB/CIFS) share that you want to attach to your host or pool.",
    default_nfs = "NFS virtual disk storage",
    default_iscsi = "iSCSI virtual disk storage",
    default_smb = "SMB virtual storage",
    default_local = "Local virtual disk storage",
    default_nfs_iso = "NFS ISO library",
    default_smb_iso = "SMB ISO library",
    name_title = "What do you want to call this Storage Repository?",
    name_blurb = "Provide a name and a description (optional) for your SR.",
    name_label = "Name:",
    description_label = "Description:",
    auto_description = "Autogenerate description based on SR settings (e.g., IP address, LUN etc.)",
    path_nfs = "Enter a path for your NFS storage",
    path_iscsi = "Enter a path for your iSCSI storage",
    path_smb = "Enter a path for your SMB storage",
    path_nfs_iso = "Enter a path for your NFS ISO storage",
    path_smb_iso = "Enter a path for your SMB ISO storage",
    path_local = "Enter your Local Storage Device information",
    share_blurb = "Provide the name of the share where your SR is located, optionally specifying advanced options. Indicate whether you want to create a new SR or reattach an existing SR before proceeding.",
    share_name = "Share Name:",
    nfs_example = "Example: server:/path",
    advanced_options = "Advanced Options:",
    nfs_version = "NFS Version:",
    nfs_v3 = "NFSv3",
    nfs_v4 = "NFSv4",
    nfs_v41 = "NFSv4.1",
    create_new = "Create a new SR",
    reattach_existing = "Reattach an existing SR:",
    scan = "Scan",
    no_srs_found = "<No SRs were found>",
    nfs_iso_blurb = "Provide the name of the share where your NFS ISO storage is located.",
    /// `{pool}`
    nfs_iso_attached = "This NFS ISO storage is already attached to '{pool}'",
    smb_iso_blurb = "Provide the name of the share where your SR is located. You can optionally specify alternative credentials by setting the server options.",
    smb_iso_example = "Example: \\\\server\\sharename",
    smb_example = "Example: \\\\server\\path",
    use_different_user = "Use different user name",
    username = "Username:",
    password = "Password:",
    /// `{pool}`
    smb_iso_attached = "This SMB ISO storage is already attached to '{pool}'",
    iscsi_blurb = "Provide a target host for your iSCSI storage, indicating your target IQN and your target LUN before proceeding.",
    target_host = "Target host name/IP address:",
    use_chap = "Use CHAP authentication",
    chap_user = "CHAP username:",
    chap_password = "CHAP password:",
    scan_target = "Scan Target Host",
    iscsi_target_group = "iSCSI target",
    target_iqn = "Target IQN:",
    target_lun = "Target LUN:",
    select_iqn = "Select target IQN",
    select_lun = "Select target LUN",
    iscsi_login_failed = "Logging in to the iSCSI target failed.",
    invalid_host = "Invalid target host.",
    iscsi_unreachable = "Unable to connect to iSCSI target",
    /// `{sr}`, `{pool}`
    lun_in_use = "This LUN is already in use as SR {sr} on pool {pool}.",
    /// `{name}`
    incorrect_lun = "This LUN does not contain the Storage Repository '{name}'.  Please select the correct LUN.",
    lvm_dialog_title = "New Storage Repository",
    /// `{type}`
    lvm_found = "An existing {type} SR was found on the selected LUN",
    lvm_none = "No existing SRs were found on the selected LUN",
    lvm_select = "Click the option below to add the SR.",
    lvm_reattach = "Reattach",
    /// `{type}`
    lvm_reattach_info = "Use the existing {type} SR",
    lvm_nothing_to_reattach = "There is no existing SR to reattach",
    lvm_format = "Format",
    /// `{type}`
    lvm_format_info = "Destroy any data present on the disk and create a new {type} SR",
    /// `{product}`, `{console}`
    lvm_warning = "To prevent data loss you must ensure that the LUN is not in use by any other system, including {product} hosts that are not connected to {console}.",
    /// `{size}`, `{uuid}`
    lvm_sr_details = "SR size: {size}\nSR UUID: {uuid}",
    local_blurb = "Provide the path to a device (ex: /dev/sda1) and the type of filesystem to create",
    device_path = "Device Path:",
    filesystem = "Filesystem Type:",
    fs_ext = "ext",
    fs_lvm = "LVM",
    fs_xfs = "xfs",
    /// `{share}`
    desc_nfs = "NFS SR [{share}]",
    /// `{target}`, `{iqn}`, `{lun}`
    desc_iscsi = "iSCSI SR [{target} ({iqn}; {lun})]",
    /// `{share}`
    desc_smb = "SMB SR [{share}]",
    /// `{share}`
    desc_nfs_iso = "NFS ISO Library [{share}]",
    /// `{share}`
    desc_smb_iso = "SMB ISO Library [{share}]",
    /// `{device}`
    desc_local = "Local Store Device: {device}",
    /// `{console}`, `{sr}`
    multi_pool_warning = "Warning: you must ensure that the following SR is not in use by any server not connected to {console}. Failure to do so may result in data loss.\n\nSR:  {sr}\n\nDo you want to reattach the SR?",
    /// `{type}`, `{target}`
    scanning = "Scanning for {type} SRs on {target}",
    /// `{target}`
    scanning_iqns = "Scanning for IQNs on iSCSI filer {target}",
    /// `{target}`
    scanning_luns = "Scanning for LUNs on iSCSI filer {target}",

    // The Repair dialog.
    /// `{name}`
    repair_title = "Repair Storage Repository - {name}",
    /// `{console}`
    repair_blurb = "Click Repair to have {console} attempt to diagnose and resolve the connection problems with this storage repository. If the problem cannot be resolved, look at {console}'s logs for more detailed error messages.",
    repair_status = "Current status:",
    repair_button = "Repair",
    pbd_connected = "Connected",
    pbd_unplugged = "Unplugged",
    pbd_missing = "Connection Missing",
    /// `{product}`
    multipath_failed = "One or more paths to at least one of your storages has failed. Refer to the \"{product} Administrator's Guide\" for information about configuring storage multipathing.",

    // The Properties dialog.
    /// `{name}`
    properties_title = "'{name}' Properties",
    tab_general = "General",
    tab_alerts = "Alerts",
    tab_read_caching = "Read Caching",
    tags_label = "Tags:",
    tags_hint = "Comma-separated",
    read_caching_blurb = "Read caching improves a VM\u{2019}s disk performance as, after the initial read from external disk, data is cached within the host\u{2019}s free memory. Select whether you want to enable read caching on this Storage Repository.",
    read_caching_enable = "Enable read caching",
    read_caching_change = "When read caching enablement is changed, the running VMs will only benefit from the change after they are rebooted. ",
    read_caching_vdis = "Read caching only applies to read-only VDIs and VDI parents. These VDIs exist where VMs are created from \u{2018}Fast Clone\u{2019} or disk snapshots. The greatest performance improvements can be seen when many VMs are cloned from a single \u{2018}golden\u{2019} image.",
    read_caching_memory = "Performance improvements depend on the amount of free memory available in the host\u{2019}s Control Domain. ",

    // Operation titles and steps.
    /// `{type}`, `{name}`, `{pool}`
    op_create = "Creating {type} SR '{name}' on '{pool}'",
    /// `{name}`, `{pool}`
    op_attach = "Attaching SR '{name}' to '{pool}'",
    /// `{name}`
    op_repair = "Repairing SR {name}",
    /// `{name}`, `{pool}`
    op_detach = "Detaching SR '{name}' from '{pool}'",
    /// `{name}`, `{pool}`
    op_forget = "Forgetting SR '{name}' on '{pool}'...",
    /// `{name}`, `{pool}`
    op_destroy = "Destroying SR '{name}' on '{pool}'...",
    /// `{name}`, `{pool}`
    op_set_default = "Setting '{name}' as default SR on '{pool}'",
    /// `{name}`
    op_rescan = "Scanning SR '{name}'",
    /// `{name}`
    op_trim = "Reclaiming freed space on SR '{name}'",
    /// `{name}`
    op_properties = "Saving properties of '{name}'",
    step_creating = "Creating SR",
    step_attaching = "Attaching SR",
    /// `{host}`
    step_create_pbd = "Creating PBD for {host}",
    /// `{host}`
    step_plug_pbd = "Plugging PBD for {host}",
    step_trim = "Reclaiming freed space",
    step_undo = "Undoing the changes to the SR",

    // Errors this client reports.
    /// `{pool}`
    err_connection_lost = "Connection to {pool} has been lost",
    /// `{target}`
    err_no_iqns = "No IQNs were found on {target}.",
    /// `{target}`
    err_no_luns = "No LUNs were found on {target}.",
    /// `{target}`
    err_bad_response = "The server '{target}' gave a bad response to the query.",
    err_scan_created = "Interrogation failed -- call succeeded!",
    err_trim_detached = "Cannot reclaim freed space because the SR is detached.",
    err_trim_unsupported = "Reclaiming freed space not supported on this SR",
    err_unknown = "An unknown error occurred.",
    /// `{name}`, `{pool}`
    err_sr_in_use = "The SR '{name}' is in use on '{pool}' and cannot be introduced.  You must detach the SR from '{pool}' before using it again.",
}

/// The strings of the current language.
pub fn sr_strings() -> &'static SrStrings {
    &EN
}

/// The wording of the errors [`xen_core::storage`] reports itself, if
/// `code` is one.
pub fn storage_error(code: &str, params: &[String]) -> Option<String> {
    use crate::i18n::fill;
    use xen_core::storage::errors;
    let s = sr_strings();
    let p = |i: usize| params.get(i).map(String::as_str).unwrap_or_default();
    Some(match code {
        errors::NO_COORDINATOR => fill(s.err_connection_lost, &[("pool", p(0))]),
        errors::NO_IQNS => fill(s.err_no_iqns, &[("target", p(0))]),
        errors::NO_LUNS => fill(s.err_no_luns, &[("target", p(0))]),
        errors::BAD_RESPONSE => fill(s.err_bad_response, &[("target", p(0))]),
        errors::SCAN_CREATED => s.err_scan_created.to_owned(),
        errors::TRIM_DETACHED => s.err_trim_detached.to_owned(),
        errors::TRIM_FAILED => match (p(0), p(1)) {
            ("UnsupportedSRForTrim", _) => s.err_trim_unsupported.to_owned(),
            (_, message) if !message.is_empty() => message.to_owned(),
            _ => s.err_unknown.to_owned(),
        },
        errors::SR_IN_USE => fill(s.err_sr_in_use, &[("name", p(0)), ("pool", p(1))]),
        _ => return None,
    })
}
