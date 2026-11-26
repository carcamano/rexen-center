//! User-interface strings of the detail tabs.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's (`Messages.resx`,
//! `FriendlyNames.resx`, the tab pages' resources), branded XCP-ng as in
//! its 25.04 XCP-ng build. Templates use `{name}` placeholders filled by
//! [`crate::i18n::fill`].

use crate::locale::string_table;

string_table! {
    namespace = "tab", struct TabStrings, fn tab_strings;

    tab_general = "General",
    tab_memory = "Memory",
    tab_storage = "Storage",
    tab_networking = "Networking",
    tab_nics = "NICs",
    tab_console = "Console",

    hdr_vm_general = "VM General Properties",
    hdr_template_general = "Template General Properties",
    hdr_snapshot_general = "Snapshot General Properties",
    hdr_host_general = "Server General Properties",
    hdr_pool_general = "Pool General Properties",
    hdr_sr_general = "Storage General Properties",
    hdr_memory = "Memory",
    hdr_vm_disks = "Virtual Disks",
    hdr_vm_networks = "Virtual Network Interfaces",
    hdr_host_networks = "Server Networks",
    hdr_pool_networks = "Pool Networks",
    hdr_nics = "Network Interface Cards",
    hdr_physical_storage = "Storage Repositories",
    hdr_virtual_disks = "Virtual Disks",

    con_send_cad = "Send Ctrl+Alt+Del (Ctrl+Alt+Insert)",
    con_scale = "Scale",
    con_undock = "Undock (Alt+Shift+U)",
    con_redock = "Redock (Alt+Shift+U)",
    con_fullscreen = "Fullscreen (Ctrl+Alt+F)",
    con_exit_fullscreen = "Exit Fullscreen (Ctrl+Alt+F)",
    con_host = "{name} server console",
    con_halted = "This VM is currently shut down.",
    con_halted_start = "This VM is currently shut down.  Click here to start it.",
    con_paused = "This VM is currently paused.",
    con_suspended = "This VM is currently suspended.",
    con_suspended_resume = "This VM is currently suspended.  Click here to resume it.",
    con_host_dead = "This server is unavailable.",
    con_looking = "Looking for guest console...",
    con_retrying = "Could not open the console ({error}); trying again...",
    con_disconnected = "Waiting for the connection to the server...",
    con_gone = "This console is no longer available.",
    con_undocked_here = "The console is in its own window. Use Redock to bring it back.",
    con_window_title = "{name} - Console",

    type_sr = "Storage repository",
    type_template = "Template",
    type_snapshot = "Snapshot",

    sec_general = "General",
    sec_certificates = "Certificates",
    sec_version = "Version Details",
    sec_management = "Management Interfaces",
    sec_memory = "Memory",
    sec_cpus = "CPUs",
    sec_boot = "Boot Options",
    sec_ha = "High Availability",
    sec_status = "Status",
    sec_multipathing = "Multipathing",
    sec_device_security = "Device Security",

    yes = "Yes",
    no = "No",
    none = "<None>",
    unknown = "Unknown",
    unknown_angle = "<unknown>",
    hyphen = "-",
    spaced_hyphen = " - ",
    empty_list = "There is nothing to show here.",

    name = "Name",
    description = "Description",
    tags = "Tags",
    folder = "Folder",
    uuid = "UUID",
    /// `{host}`: a control domain's name, as the original shows it.
    control_domain_on = "Control domain on host {host}",

    os = "Operating System",
    virt_mode = "Virtualization mode",
    virt_hvm = "Hardware-assisted Virtualization (HVM)",
    virt_pv = "Paravirtualization (PV)",
    bios_copied = "BIOS strings copied",
    vapp = "vApp",
    snapshot_of = "Snapshot of",
    creation_time = "Creation time",
    virt_state = "Virtualization state",
    vm_uptime = "Time since startup",
    home_server = "Home Server",
    vs_unknown = "Unknown (please wait)",
    vs_io_optimized = "I/O optimized",
    vs_io_not_optimized = "I/O not optimized",
    vs_agent_installed = "Management Agent installed",
    vs_agent_not_installed = "Management Agent not installed",
    vs_wu_yes = "Able to receive updates from Windows Update",
    vs_wu_no = "Not able to receive updates from Windows Update",
    vs_tools_missing = "XCP-ng VM Tools not installed",
    /// `{version}`
    vs_tools_old = "Tools out of date (version {version} installed)",
    /// `{version}`
    vs_optimized = "Optimized (version {version} installed)",

    coordinator = "Pool coordinator",
    enabled = "Enabled",
    host_unreachable = "Server is not reachable",
    host_maintenance = "In maintenance mode",
    host_disabled = "Disabled",
    autoboot = "Autoboot of VMs enabled",
    cert_verification = "Certificate Verification",
    cert_verification_enabled = "Enabled",
    cert_verification_disabled = "Disabled",
    cert_verification_pool_only = "Disabled, but enabled on the pool",
    cert_verification_some_hosts = "Enabled on the pool, but disabled on:",
    iscsi_iqn = "iSCSI IQN",
    log_destination = "Log destination",
    log_local = "Local",
    /// `{destination}`
    log_remote = "Local and Remote ({destination})",
    host_uptime = "Server uptime",
    agent_uptime = "Toolstack uptime",
    domain = "Domain",
    sockets = "Number of Sockets",
    version = "Version",
    sr_type = "Type",
    sr_size = "Size",
    /// `{used}`, `{total}`, `{allocated}`
    sr_size_value = "{used} used of {total} total ({allocated} allocated)",
    scsi_id = "SCSI ID",

    cert_ca = "Trusted CA certificate",
    cert_host = "Server identity certificate",
    cert_host_internal = "Server internal identity certificate",
    /// `{from}`, `{to}`, `{thumbprint}`
    cert_value = "Valid from {from} to {to}\nThumbprint: {thumbprint}",

    build_date = "Build date",
    dbv = "DBV",

    dns_hostname = "DNS hostname",
    management_interface = "Management interface",
    /// `{label}`, `{host}`
    on_host = "{label} on '{host}'",

    mem_server = "Server",
    /// `{available}`, `{total}`
    mem_server_value = "{available} RAM available ({total} total)",
    mem_vms = "VMs",
    /// `{vm}`, `{memory}`
    mem_vm_line = "{vm}: using {memory}",
    mem_xen = "XCP-ng",
    not_found = "Not Found",

    /// `{last}`
    cpu_range = "CPU 0 - {last}",
    /// `{number}`
    cpu_one = "CPU {number}",
    /// `{vendor}`, `{model}`, `{speed}`
    cpu_value = "Vendor: {vendor}\nModel: {model}\nSpeed: {speed} MHz",

    auto_boot = "Auto-boot",
    boot_order = "Boot order",
    boot_hdd = "Hard Disk",
    boot_dvd = "DVD-Drive",
    boot_network = "Network",
    boot_mode = "Boot mode",
    boot_uefi_secure = "UEFI Secure Boot",
    boot_uefi_secure_missing = "UEFI Secure Boot (UEFI certificates are missing from your system)",
    boot_uefi = "UEFI Boot",
    boot_bios = "BIOS Boot",
    os_boot_params = "OS boot parameters",

    ha_priority = "HA restart priority",
    ha_restart = "Restart",
    ha_best_effort = "Restart if possible",
    ha_do_not_restart = "Do not restart",
    ha_restart_first = "Restart first",

    sr_state = "State",
    sr_detached = "Detached",
    sr_broken = "Broken",
    sr_multipath_failure = "Multipath Failure",
    sr_ok = "OK",
    pbd_missing = "Connection Missing",
    pbd_unplugged = "Unplugged",
    pbd_connected = "Connected",
    multipath_capable = "Multipath capable",
    /// `{current}`, `{max}`
    paths_active = "{current} of {max} paths active",
    /// `{count}`
    iscsi_sessions = " ({count} iSCSI sessions)",
    multipath_active = "Active",
    multipath_not_active = "Not active",

    vcpus = "Virtual CPUs",
    vcpus_max = "Max number of vCPUs",
    topology = "Topology",
    topology_1s_1c = "1 socket with 1 core per socket",
    /// `{cores}`
    topology_1s_nc = "1 socket with {cores} cores per socket",
    /// `{sockets}`
    topology_ns_1c = "{sockets} sockets with 1 core per socket",
    /// `{sockets}`, `{cores}`
    topology_ns_nc = "{sockets} sockets with {cores} cores per socket",
    topology_invalid_1c = "1 core per socket (Invalid configuration)",
    /// `{cores}`
    topology_invalid_nc = "{cores} cores per socket (Invalid configuration)",
    vtpm = "Trusted Platform Module",
    vtpm_one = "A vTPM is attached to this VM.",
    /// `{count}`
    vtpm_many = "{count} vTPMs are attached to this VM.",

    /// `{n}`
    day = "{n} day",
    /// `{n}`
    days = "{n} days",
    /// `{n}`
    hour = "{n} hour",
    /// `{n}`
    hours = "{n} hours",
    /// `{n}`
    minute = "{n} minute",
    /// `{n}`
    minutes = "{n} minutes",

    mem_total = "Total memory",
    mem_used = "Currently used",
    mem_dom0 = "Control domain memory",
    mem_available = "Available memory",
    mem_total_max = "Total max memory",
    /// `{percent}`
    mem_percent = "({percent}% of total memory)",
    mem_min = "Minimum memory",
    mem_max = "Maximum memory",
    mem_static_max = "Static maximum",
    mem_memory = "Memory",
    /// `{host}`
    mem_dom0_caption = "Control domain on host {host}",
    /// `{memory}`
    mem_current = "Current memory usage: {memory}",
    /// `{memory}`
    mem_current_per_vm = "Current memory usage: {memory} per VM",

    col_position = "Position",
    col_name = "Name",
    col_description = "Description",
    col_sr = "SR",
    col_size = "Size",
    col_read_only = "Read Only",
    col_priority = "Priority",
    col_active = "Active",
    col_device_path = "Device Path",
    /// `{n}`
    priority_lowest = "{n} (Lowest)",
    /// `{n}`
    priority_highest = "{n} (Highest)",

    col_device = "Device",
    col_mac = "MAC",
    col_limit = "Limit",
    col_network = "Network",
    col_ip = "IP Address",
    mac_auto = "auto-generate",

    col_nic = "NIC",
    col_vlan = "VLAN",
    col_auto = "Auto",
    col_link_status = "Link Status",
    col_mtu = "MTU",
    link_connected = "Connected",
    link_disconnected = "Disconnected",
    link_partial = "Partially connected",
    bond_member_suffix = " (Bonded member)",
    hidden_suffix = " (Hidden)",
    networks_label = "Networks",
    ip_config_label = "IP Address Configuration",
    col_server = "Server",
    col_interface = "Interface",
    col_ip_setup = "IP Setup",
    col_subnet = "Subnet mask",
    col_gateway = "Gateway",
    col_dns = "DNS",
    interface_management = "Management",
    interface_unknown = "unknown",
    ip_none = "None",
    ip_dhcp = "DHCP",
    ip_static = "Static",

    col_speed = "Speed",
    col_duplex = "Duplex",
    col_vendor = "Vendor",
    col_device_name = "Device",
    col_pci = "PCI Bus Path",
    col_fcoe = "FCoE Capable",
    col_sriov_capable = "SR-IOV Capable",
    duplex_full = "Full",
    duplex_half = "Half",
    /// `{speed}`
    nic_speed = "{speed} Mbit/s",
    sriov_create = "Yes (To enable SR-IOV create an SR-IOV network on this NIC)",
    sriov_reboot = "Yes (Host needs reboot)",
    sriov_unplugged = "Yes (SR-IOV logical PIF is unplugged)",

    col_type = "Type",
    col_shared = "Shared",
    col_usage = "Usage",
    col_virtual_allocation = "Virtual allocation",
    /// `{percent}`, `{used}`
    usage_value = "{percent}% ({used} used)",

    col_vm = "Virtual Machine",
    col_cbt = "Changed Block Tracking",
    cbt_enabled = "Enabled",
    cbt_disabled = "Disabled",
    /// `{name}`
    snapshot_suffix = "{name} (snapshot)",

    view_custom_templates = "Custom Templates",
    view_default_templates = "XCP-ng Templates",
    view_local_storage = "Local Storage",
    view_snapshots = "Snapshots",
    view_hidden = "Hidden Objects",
    tree_filter_label = "Filter:",
    tree_filter = "Filter by name",
    tree_filter_clear = "Clear filter",
    tree_no_match = "No objects match the filter.",
}
