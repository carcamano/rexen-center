//! User-interface strings of F14a: the GPU and USB tabs, the VM
//! Properties dialog's GPU and USB pages, the vTPM manager, the memory
//! settings dialog and wizard, the external SSH and Remote Desktop
//! consoles, and the operation titles.
//!
//! The English texts are the original console's (`XenModel/Messages.resx`
//! and the `GpuPage`, `GpuRow`, `GpuConfiguration`, `PoolGpuEditPage`,
//! `GpuEditPage`, `AddVGPUDialog`, `UsbPage`, `USBEditPage`,
//! `AttachUsbDialog`, `VtpmManagementDialog`, `VtpmManagementPage`,
//! `BallooningDialog`, `BallooningWizard`, `VMMemoryControls*`,
//! `VNCTabView` and `ExternalToolsOptionsPage` resources), without the `&`
//! access keys, `{0}` written as named placeholders. A few are this
//! console's own; each says so.

use xen_core::devices::{
    DeviceBlock, PageVgpuName, PusbDescription, VgpuChoiceText, VgpuTypeName, VtpmRefusal,
    VtpmUnavailable,
};
use xen_core::memory::{DmcUnavailable, MemoryConfirm};
use xen_core::operations::DeviceKind;

use crate::i18n::fill;

macro_rules! dev_strings {
    ($( $(#[$doc:meta])* $field:ident = $en:literal, )*) => {
        /// One language's strings.
        pub struct DevStrings {
            $( $(#[$doc])* pub $field: &'static str, )*
        }

        /// English.
        pub static EN: DevStrings = DevStrings {
            $( $field: $en, )*
        };
    };
}

dev_strings! {
    // ---- GPU tab ------------------------------------------------------------
    tab_gpu = "GPU",
    gpu_none_pool = "GPU configuration and monitoring are disabled, because there are no GPUs available in this pool.",
    gpu_none_host = "GPU configuration and monitoring are disabled, because there are no GPUs available on this host.",
    gpu_placement = "Placement policy: {policy}",
    policy_density = "Maximum density: put as many VMs as possible on the same GPU",
    policy_performance = "Maximum performance: put VMs on as many GPUs as possible",
    policy_mixed = "Mixed: different settings for different GPU groups",
    gpu_edit = "Edit...",
    gpu_edit_selected = "Edit Selected GPUs...",
    gpu_allowed_types = "Virtual GPU types:",
    gpu_select_all = "Select All",
    gpu_clear_all = "Clear All",
    gpu_on_host = "On {host}:",
    passthrough_whole = "Pass-through whole GPU",
    vgpu_description = "{model} virtual GPU ({per} per GPU)",
    vgpu_description_multiple = "{model} virtual GPU ({per} per GPU, multiple vGPU support)",
    gpu_title_one = "{name} (1 GPU)",
    gpu_title_many = "{name} ({count} GPUs)",
    gpu_rubric_one = "Select which virtual GPU types are allowed on this GPU.",
    gpu_rubric_many = "Select which virtual GPU types are allowed on these GPUs.",
    col_name = "Name",
    col_per_gpu = "Virtual GPUs per GPU",
    col_video_ram = "Video RAM",
    /// This console's own: why a row's box cannot be cleared.
    gpu_type_in_use = "This type is in use on the selected GPUs.",
    // ---- pool / server Properties: GPU page --------------------------------
    page_gpu = "GPU",
    placement_group = "Placement policy",
    placement_rubric = "Set a placement policy for assigning VMs to GPUs to achieve either maximum density or maximum performance.",
    sub_density = "Maximum density",
    sub_performance = "Maximum performance",
    sub_mixed = "Mixed",
    integrated_group = "Integrated GPU pass-through",
    integrated_rubric = "Choose whether you want this server to use the integrated GPU. When a server is using the integrated GPU, that GPU cannot be passed through to a VM.",
    integrated_using = "This server is currently using the integrated GPU.",
    integrated_not_using = "This server is currently not using the integrated GPU.",
    integrated_after_reboot = "After next reboot:",
    integrated_will_use = "This server will use the integrated GPU",
    integrated_will_not_use = "This server will not use the integrated GPU",
    sub_integrated_using = "Using the integrated GPU",
    sub_integrated_not_using = "Not using the integrated GPU",
    // ---- VM Properties: GPU page ---------------------------------------------
    vm_gpu_title = "Assign a virtual GPU",
    vm_gpu_rubric = "You can improve graphics performance by assigning one or more virtual graphics processing units to this VM.",
    col_pci_slot = "PCI slot number",
    col_vgpu_type = "Virtual GPU type",
    gpu_add = "Add...",
    gpu_delete = "Delete",
    gpu_experimental = "This type of vGPU is in Tech Preview, and is currently not suitable for production use.",
    gpu_rdp_warning = "Before making this change, you must make sure that you can connect to your VM over Remote Desktop. The Default (VNC) Desktop will not work with a dedicated GPU, so you will not be able to connect to your VM except over Remote Desktop.",
    gpu_need_driver = "It is essential that the appropriate graphics card drivers are installed on the VM once it has been booted.",
    gpu_need_gpu = "If there is no available virtual GPU of the correct type when the VM is next booted, it will not be able to start.",
    gpu_stop_vm = "The virtual GPUs can only be changed when the VM is shut down.",
    gpu_ha = "The virtual GPUs cannot be changed because the VM is currently protected by HA.",
    gpu_multiple = "The selected virtual GPU type supports multiple instances.",
    gpu_single = "The selected virtual GPU type does not support multiple instances.",
    gpu_sub_none = "None",
    gpu_sub_unavailable = "Not available",
    add_vgpu_title = "Add virtual GPU",
    add_vgpu_label = "Select virtual GPU type:",
    add_vgpu_add = "Add",
    // ---- USB --------------------------------------------------------------------
    tab_usb = "USB",
    usb_devices = "USB devices",
    col_location = "Location",
    col_description = "Description",
    col_passthrough = "Pass-through",
    col_vm = "Virtual machine",
    usb_enable = "Enable pass-through",
    usb_disable = "Disable pass-through",
    enabled = "Enabled",
    disabled = "Disabled",
    usb_vendor_product = "Vendor: {vendor}; Product: {product}",
    usb_confirm_disable = "Are you sure that you want to disable pass-through on this device? This device will no longer be allowed to be passed through to virtual machines.",
    usb_confirm_enable = "Are you sure that you want to enable pass-through on this device? This will allow this device to be passed through to virtual machines.\n\nPlugging in untrustworthy USB devices to your computer may put your computer at risk. USB devices with modifiable behaviour should only be assigned to trustworthy guest VMs.",
    usb_yes_disable = "Yes, Disable pass-through",
    usb_yes_enable = "Yes, Enable pass-through",
    page_usb = "USB",
    usb_page_title = "USB Devices",
    usb_sub_none = "No devices attached",
    usb_sub_one = "1 device attached",
    usb_sub_many = "{count} devices attached",
    col_attached = "Attached",
    usb_attach = "Attach",
    usb_detach = "Detach",
    usb_not_halted = "USB devices can only be attached when the VM is shut down.",
    usb_ha = "Cannot attach USB devices because the VM is currently protected by HA.",
    usb_detach_title = "Detach USB device",
    usb_detach_confirm = "Are you sure you want to detach the selected USB device?",
    attach_usb_title = "Attach USB",
    attach_usb_note = "Select a device to be passed through to this virtual machine.",
    attach_usb_warn1 = "Plugging in untrustworthy USB devices to your computer may put your computer at risk.",
    attach_usb_warn2 = "USB devices with modifiable behaviour should only be assigned to trustworthy guest VMs.",
    attach_usb_warn3 = "A major component update of VM will be performed the first time when a USB device is passed through to it. This update is irreversible.",
    attach_usb_none = "No devices available",
    yes = "Yes",
    no = "No",
    // ---- vTPM ---------------------------------------------------------------------
    vtpm_menu = "Manage vTPMs...",
    vtpm_non_uefi = "TPM is supported only for VMs using the UEFI or UEFI secure boot modes.",
    vtpm_title = "Trusted Platform Module (TPM) Manager - {vm}",
    vtpm_blurb = "Attach, remove, and configure vTPMs on '{vm}'.",
    vtpm_add = "Add vTPM",
    vtpm_none = "There are no vTPMs attached to this VM.",
    vtpm_name = "Trusted Platform Module",
    vtpm_properties = "Properties",
    vtpm_protected = "Protected:",
    vtpm_protected_info = "The contents of a protected vTPM are shielded from unauthorized access and undetected tampering when saved to persistent storage or sent via network.",
    vtpm_unique = "Unique:",
    vtpm_unique_info = "The contents of a unique vTPM cannot be duplicated through any VM operation such as copy, clone, export, or creation from a snapshot.",
    vtpm_remove = "Remove vTPM",
    vtpm_remove_warning = "Removing the vTPM may result in the deactivation of some security features on the VM and prevent it from starting. Do you want to continue?",
    vtpm_add_not_allowed = "Attaching a new vTPM to this VM is currently not allowed.",
    vtpm_max = "The maximum number of vTPMs that can be attached to this VM is {max}.",
    vtpm_add_halt = "You need to shut down this VM before you can attach a new vTPM to it.",
    vtpm_remove_not_allowed = "Removing this vTPM from the VM is currently not allowed.",
    vtpm_remove_halt = "You need to shut down the VM before you can remove this vTPM.",
    close = "Close",
    // ---- memory -----------------------------------------------------------------
    mem_edit = "Edit...",
    mem_title = "Memory Settings",
    mem_rubric = "Set a fixed memory for this VM, or set a memory range and allow the VM's memory allocation to be adjusted automatically.",
    mem_rubric_template = "Set a fixed memory for VMs created from this template, or set a memory range and allow the VMs' memory allocation to be adjusted automatically.",
    mem_rubric_advanced = "Set the memory range and static maximum memory for this VM.",
    mem_rubric_advanced_template = "Set the memory range and static maximum memory for VMs created from this template.",
    mem_fixed = "Set a fixed memory of",
    mem_dynamic = "Automatically allocate memory within this range:",
    mem_min = "Minimum memory:",
    mem_max = "Maximum memory:",
    mem_static_max = "Static maximum:",
    mem_mb = "MB",
    mem_gb = "GB",
    dmc_license = "The Dynamic Memory Control (DMC) is not available due to license restrictions on the server.",
    dmc_template = "A memory range cannot be set for this template.",
    dmc_vms = "A memory range cannot be set for these VMs.",
    dmc_not_supported = "A memory range cannot be set because the operating system on this VM does not support it.",
    dmc_not_supported_plural = "A memory range cannot be set because the operating systems on these VMs do not support it.",
    dmc_no_io = "A memory range cannot be set because I/O drivers are not installed on this VM.",
    dmc_no_io_plural = "A memory range cannot be set because I/O drivers are not installed on these VMs.",
    dmc_no_tools = "A memory range cannot be set because {tools} are not installed on this VM.",
    dmc_no_tools_plural = "A memory range cannot be set because {tools} are not installed on these VMs.",
    /// `BrandManager.VmTools` for XCP-ng.
    vm_tools = "Guest Tools",
    mem_confirm_static_max = "Changing the static maximum memory of this VM will cause it to be rebooted.\n\nDo you want to continue?",
    mem_confirm_static_max_plural = "Changing the static maximum memory of these VMs will cause them to be rebooted.\n\nDo you want to continue?",
    mem_confirm_max = "Changing the maximum memory of this VM will cause it to be rebooted.\n\nDo you want to continue?",
    mem_confirm_max_plural = "Changing the maximum memory of these VMs will cause them to be rebooted.\n\nDo you want to continue?",
    mem_confirm = "Changing the memory of this VM while it is running will cause it to be forcibly rebooted. This operation can cause data loss.\n\nDo you want to continue?",
    mem_confirm_plural = "Changing the memory of these VMs while they are running will cause them to be forcibly rebooted. This operation can cause data loss.\n\nDo you want to continue?",
    wiz_choose_vms_title = "Choose the VMs whose memory settings you want to adjust",
    wiz_choose_vms_text = "Select which VMs you would like to configure from the list below.",
    wiz_memory_title = "Configure the memory settings for your selected VMs",
    wiz_memory_text = "Assign the VMs a fixed memory or indicate the range you want them to use. When ready, press Finish to apply the configuration.",
    back = "< Back",
    next = "Next >",
    finish = "Finish",
    ok = "OK",
    cancel = "Cancel",
    // ---- external consoles --------------------------------------------------
    ssh_button = "Launch SSH Console",
    ssh_not_configured = "An external SSH console has not been configured.",
    ssh_missing = "The file associated with your external SSH console doesn't exist. Please check that you have selected a valid file.",
    ssh_failed = "Your external SSH console could not be launched. Please check that you have selected a valid file.",
    ssh_configure = "Configure SSH Console",
    /// This console's own (external client instead of the embedded one).
    rdp_open = "Open Remote Desktop",
    rdp_turn_on = "Turn on Remote Desktop",
    rdp_not_enabled = "Remote access is not enabled on this guest",
    rdp_force_enable = "Would you like to configure this VM to accept Remote Desktop connections, and then connect to it over Remote Desktop?",
    /// This console's own.
    rdp_looking = "Looking for the guest's Remote Desktop address...",
    /// This console's own.
    rdp_no_address = "No address of this VM accepts Remote Desktop connections.",
    /// This console's own.
    rdp_failed = "The Remote Desktop client could not be launched: {error}",
    /// This console's own: the command line shown after a launch.
    console_launched = "Started: {command}",
    gpu_passthrough_console = "This VM has a pass-through GPU assigned. You must connect to it using Remote Desktop.",
    // ---- Options > External Tools ------------------------------------------
    tools_heading = "External Tools",
    tools_info = "{console} allows you to use an external SSH console to connect to the server and VMs that support SSH connection. Select below the console of your preference.",
    tools_ssh_group = "External SSH Console",
    /// This console's own: the platform's terminal with OpenSSH.
    tools_system = "System terminal (ssh)",
    tools_putty = "PuTTY",
    tools_openssh = "OpenSSH",
    /// This console's own.
    tools_rdp_group = "Remote Desktop client",
    /// This console's own.
    tools_rdp_hint = "Program run with /v:<address>; empty for the system's client (mstsc on Windows, the .rdp handler on macOS, xfreerdp elsewhere).",
    // ---- operation titles ---------------------------------------------------
    op_vgpu_config = "Saving virtual GPU configuration",
    op_placement = "Set GPU placement policy",
    op_integrated = "Updating integrated GPU pass-through on '{host}'",
    /// The original's "Set GPU", with the VM named (this console's).
    op_set_gpu_vm = "Set GPU on '{vm}'",
    op_usb_enabling = "Enabling USB pass-through for {device}",
    op_usb_disabling = "Disabling USB pass-through for {device}",
    op_vusb_creating = "Attaching USB device '{device}' to VM '{vm}'",
    op_vusb_deleting = "Detaching USB device '{device}' from VM '{vm}'",
    op_vtpm_attach = "Attaching a new vTPM to VM '{vm}'",
    op_vtpm_remove = "Removing a vTPM from VM '{vm}'",
    op_memory = "Changing memory settings for {vm}...",
    /// This console's own.
    op_enable_rdp = "Turning on Remote Desktop on '{vm}'",
    step_shutting_down = "Shutting down VM",
    /// This console's own.
    step_memory = "Changing memory settings",
    step_starting = "Starting VM",
}

/// The strings of the current language.
pub fn dev_strings() -> &'static DevStrings {
    &EN
}

/// `VGPU_type.Description` (`multiple`) or `Name`.
pub fn vgpu_type_text(name: &VgpuTypeName, with_multiple: bool) -> String {
    let s = dev_strings();
    match name {
        VgpuTypeName::Passthrough => s.passthrough_whole.to_owned(),
        VgpuTypeName::Virtual {
            model,
            per_gpu,
            multiple,
        } => fill(
            if *multiple && with_multiple {
                s.vgpu_description_multiple
            } else {
                s.vgpu_description
            },
            &[("model", model), ("per", &per_gpu.to_string())],
        ),
    }
}

/// A row name of the VM's GPU page.
pub fn page_vgpu_text(name: &PageVgpuName) -> String {
    match name {
        PageVgpuName::Passthrough => dev_strings().passthrough_whole.to_owned(),
        PageVgpuName::Group(g) | PageVgpuName::Model(g) => g.clone(),
    }
}

/// An Add vGPU entry.
pub fn choice_text(text: &VgpuChoiceText) -> String {
    match text {
        VgpuChoiceText::Group(g) => g.clone(),
        VgpuChoiceText::Type(t) => format!("    {}", vgpu_type_text(t, true)),
    }
}

/// `PUSB.Description`.
pub fn pusb_text(d: &PusbDescription) -> String {
    match d {
        PusbDescription::Text(t) => t.clone(),
        PusbDescription::Ids { vendor, product } => fill(
            dev_strings().usb_vendor_product,
            &[("vendor", vendor), ("product", product)],
        ),
    }
}

/// Why vGPUs or USB devices cannot change.
pub fn gpu_block_text(b: DeviceBlock) -> &'static str {
    match b {
        DeviceBlock::NotHalted => dev_strings().gpu_stop_vm,
        DeviceBlock::Ha => dev_strings().gpu_ha,
    }
}

/// Why USB devices cannot be attached.
pub fn usb_block_text(b: DeviceBlock) -> &'static str {
    match b {
        DeviceBlock::NotHalted => dev_strings().usb_not_halted,
        DeviceBlock::Ha => dev_strings().usb_ha,
    }
}

/// Why a vTPM cannot be added or removed.
pub fn vtpm_refusal_text(r: VtpmRefusal) -> String {
    let s = dev_strings();
    match r {
        VtpmRefusal::AddNotAllowed => s.vtpm_add_not_allowed.to_owned(),
        VtpmRefusal::MaxReached => fill(
            s.vtpm_max,
            &[("max", &xen_core::devices::MAX_VTPMS.to_string())],
        ),
        VtpmRefusal::AddNotHalted => s.vtpm_add_halt.to_owned(),
        VtpmRefusal::RemoveNotAllowed => s.vtpm_remove_not_allowed.to_owned(),
        VtpmRefusal::RemoveNotHalted => s.vtpm_remove_halt.to_owned(),
    }
}

/// The disabled menu item's tooltip.
pub fn vtpm_unavailable_text(u: VtpmUnavailable) -> Option<&'static str> {
    match u {
        VtpmUnavailable::Hidden => None,
        VtpmUnavailable::NotUefi => Some(dev_strings().vtpm_non_uefi),
    }
}

/// The Basic memory control's reason text.
pub fn dmc_text(u: DmcUnavailable) -> String {
    let s = dev_strings();
    let tools = [("tools", s.vm_tools)];
    match u {
        DmcUnavailable::License => s.dmc_license.to_owned(),
        DmcUnavailable::Template => s.dmc_template.to_owned(),
        DmcUnavailable::Vms => s.dmc_vms.to_owned(),
        DmcUnavailable::NotSupported { plural: false } => s.dmc_not_supported.to_owned(),
        DmcUnavailable::NotSupported { plural: true } => s.dmc_not_supported_plural.to_owned(),
        DmcUnavailable::NoIo { plural: false } => s.dmc_no_io.to_owned(),
        DmcUnavailable::NoIo { plural: true } => s.dmc_no_io_plural.to_owned(),
        DmcUnavailable::NoTools { plural: false } => fill(s.dmc_no_tools, &tools),
        DmcUnavailable::NoTools { plural: true } => fill(s.dmc_no_tools_plural, &tools),
    }
}

/// The question before a memory change reboots VMs.
pub fn memory_confirm_text(c: MemoryConfirm) -> &'static str {
    let s = dev_strings();
    match c {
        MemoryConfirm::StaticMax { plural: false } => s.mem_confirm_static_max,
        MemoryConfirm::StaticMax { plural: true } => s.mem_confirm_static_max_plural,
        MemoryConfirm::MemoryMax { plural: false } => s.mem_confirm_max,
        MemoryConfirm::MemoryMax { plural: true } => s.mem_confirm_max_plural,
        MemoryConfirm::Memory { plural: false } => s.mem_confirm,
        MemoryConfirm::Memory { plural: true } => s.mem_confirm_plural,
    }
}

/// The history title of an F14a operation.
pub fn device_title(kind: &DeviceKind) -> String {
    let s = dev_strings();
    match kind {
        DeviceKind::AllowedVgpuTypes => s.op_vgpu_config.to_owned(),
        DeviceKind::PlacementPolicy => s.op_placement.to_owned(),
        DeviceKind::IntegratedGpu { host } => fill(s.op_integrated, &[("host", host)]),
        DeviceKind::AssignVgpus { vm } => fill(s.op_set_gpu_vm, &[("vm", vm)]),
        DeviceKind::UsbPassthrough { device, enabled } => fill(
            if *enabled {
                s.op_usb_enabling
            } else {
                s.op_usb_disabling
            },
            &[("device", device)],
        ),
        DeviceKind::AttachUsb { device, vm } => {
            fill(s.op_vusb_creating, &[("device", device), ("vm", vm)])
        }
        DeviceKind::DetachUsb { device, vm } => {
            fill(s.op_vusb_deleting, &[("device", device), ("vm", vm)])
        }
        DeviceKind::AddVtpm { vm } => fill(s.op_vtpm_attach, &[("vm", vm)]),
        DeviceKind::RemoveVtpm { vm } => fill(s.op_vtpm_remove, &[("vm", vm)]),
        DeviceKind::Memory { vm } => fill(s.op_memory, &[("vm", vm)]),
        DeviceKind::EnableRdp { vm } => fill(s.op_enable_rdp, &[("vm", vm)]),
    }
}
