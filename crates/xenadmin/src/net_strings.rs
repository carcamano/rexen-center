//! User-interface strings of networking: the Networking and NICs tabs'
//! buttons, the New Network wizard, the Create Bond, network Properties,
//! Configure IP Addresses and virtual interface dialogs, the confirmations
//! and the operation titles.
//!
//! Like [`crate::i18n::Strings`], one table per language; only English
//! exists. The English texts are the original console's
//! (`XenModel/Messages.resx` and the wizard pages' and dialogs' own
//! resources), without the `&` access keys. Templates use `{name}`
//! placeholders filled by [`crate::i18n::fill`].

use crate::locale::string_table;

string_table! {
    namespace = "net", struct NetStrings, fn net_strings;

    // Buttons of the tabs.
    add_network = "Add Network...",
    network_properties = "Properties",
    remove_network = "Remove Network",
    add_interface = "Add Interface...",
    interface_properties = "Properties",
    remove_interface = "Remove Interface",
    activate = "Activate",
    deactivate = "Deactivate",
    configure_ip = "Configure...",
    create_bond_button = "Create Bond...",
    /// `{suffix}`: the new bond's name (`NEW_BOND_NAME`).
    bond_name = "Bond {suffix}",
    delete_bond_button = "Delete Bond",
    rescan = "Rescan",
    yes = "Yes",
    no = "No",
    ok = "OK",
    cancel = "Cancel",
    previous = "< Previous",
    next = "Next >",
    finish = "Finish",
    none = "<None>",

    // Tooltips of the tabs.
    tooltip_remove_pif = "This network cannot be removed because it is a physical device",
    tooltip_remove_suspended = "Networks cannot be removed from suspended VMs",
    tooltip_edit_io_drivers = "I/O drivers must be installed to edit a network on a running VM",
    tooltip_remove_io_drivers = "I/O drivers must be installed to remove a network on a running VM",
    tooltip_deactivate_suspended = "Networks cannot be deactivated while the VM is suspended",
    tooltip_activate_suspended = "Networks cannot be activated while the VM is suspended",
    tooltip_deactivate_tools = "Networks cannot be deactivated on a running VM without tools installed",
    tooltip_activate_tools = "Networks cannot be activated on a running VM without tools installed",
    disable_ha_configure_ip = "Disable HA if you want to configure IP addresses.",

    // Confirmations.
    vif_delete_title = "Delete Network Interface",
    vif_delete_text = "This will delete the selected network interface permanently. Continue?",
    network_delete_title = "Delete Network",
    network_delete_text = "This will delete the selected network permanently. Continue?",
    delete_bond_title = "Delete Bond",
    /// `{bond}`
    delete_bond_text = "Are you sure that you want to delete {bond}?",
    /// `{question}`, `{console}`
    bond_delete_primary = "{question}\n\nDeleting this bond will automatically transfer the management interface on the bond to the first of the bonded members: \n\n- {console} connections to the pool will temporarily be disturbed\n\n- If your network configuration is incorrect then {console} may permanently lose the connection to the pool, or pool members may be unable to contact each other.",
    /// `{question}`, `{console}`
    bond_delete_both = "{question}\n\nDeleting this bond will automatically transfer the management and secondary interfaces on the bond to the first other member of the bond: \n\n- {console} connections to the pool will temporarily be disturbed\n\n- Traffic through the secondary interface will be disrupted\n\n- If your network configuration is incorrect then {console} may permanently lose the connection to the pool, or pool members may be unable to contact each other.",
    /// `{question}`
    bond_delete_secondary = "{question}\n\nDeleting this bond will disrupt traffic through the secondary interface on the bond while the interface is moved to the first of the bonded members.",
    bond_delete_continue = "Delete bond anyway",
    /// `{bond}`, `{pool}`
    bond_delete_ha = "Can’t delete this bond while HA is enabled\n\nA management interface exists on one of the bonded NICs. Deleting the  bond could result in HA network heartbeat failure, so this action is not allowed while HA is on.\n\nTo continue, disable HA, delete the bond, and then enable HA again.",
    bond_create_title = "Create Bond",
    bond_create_both = "This bond cannot be created because both management and secondary interfaces exist on your selected bond members.\n\nBoth types of interface cannot be added to the same bond. Choose different bond members, or move the management interfaces before creating the bond.",
    /// `{pool}`
    bond_create_ha = "Can’t create this bond while HA is enabled\n\nA management interface exists on one of the selected NICs. Moving the management interface to the bond could result in HA network heartbeat failure, so this action is not allowed while HA is on.\n\nTo continue, either select different NICs for the bond, or disable HA, create the bond, and then enable HA again.",
    /// `{console}`
    bond_create_primary = "Creating this bond will automatically move the management interface on one of your selected bond members to the bond itself:\n\n- {console} connections to the pool will temporarily be disturbed\n\n- If your network configuration is incorrect then {console} may permanently lose the connection to the pool, or pool members may be unable to contact each other.",
    bond_create_continue = "Create bond anyway",
    bond_create_clustering = "Creating this bond will automatically move the clustering network on one of your selected bond members to the bond itself:\n\n- If your network configuration is incorrect then hosts may permanently lose the connection to the clustering network, which will cause undesired host fences.",
    bond_create_secondary = "Creating this bond will disrupt traffic through the secondary interfaces on the bond members while the interfaces are moved onto the bond itself.",
    duplicate_mac_title = "Duplicate MAC address",
    /// `{mac}`, `{vm}`
    duplicate_mac = "The MAC address entered has already been assigned to the VM:\n{vm}\n\nAre you sure you want to continue?",
    /// `{console}`
    warn_management_host = "You are reconfiguring the primary management interface.  If the new settings are incorrect then {console} may permanently lose the connection to the server.\n\nYou should only proceed if you have verified that these settings are correct.",
    /// `{console}`
    warn_management_pool = "You are reconfiguring the primary management interface.  If the new settings are incorrect then {console} may permanently lose the connection to the server, or pool members may be unable to contact each other.\n\nYou should only proceed if you have verified that these settings are correct.",
    reconfig_connection_lost = "The connection to the server was lost. No changes have been made.",
    hotplug_text = "The virtual network device changes will take effect when you shut down and then restart the VM.",
    /// `{max}`
    vifs_max = "The maximum number of virtual network interfaces this VM can have is {max}.",

    // New Network wizard.
    /// `{pool}`
    wizard_title = "New Network - {pool}",
    page_type = "Select Type",
    page_type_title = "Choose the type of network to create",
    page_name = "Name",
    page_name_title = "Enter a name and description for the new network",
    page_details = "Network settings",
    page_details_title = "Configure the new network",
    page_bond = "Bond Members",
    page_bond_title = "Select members for the new bonded network",
    type_blurb = "Select the type of new network you would like to create:",
    type_external = "External Network",
    type_external_blurb = "Create a network that passes traffic over one of your VLANs.",
    type_internal = "Single-Server Private Network",
    type_internal_blurb = "Create a network that does not leave each server.\nThis can be used as a private connection between VMs on the same host.",
    type_bonded = "Bonded Network",
    type_bonded_blurb = "Create a network that bonds together two or more of your NICs.\nThis will create a single higher performing channel.",
    type_chin = "Cross-Server Private Network",
    type_chin_blurb = "Create a network that does not leave the pool.\nThis can be used as a private connection between VMs in the pool.\nThis type of network requires the vSwitch Controller to be running.",
    chin_needs_vswitch = "Cross-server private networks require the vSwitch Controller to be configured and running.",
    name_blurb = "Provide a name and optional description for the new network.",
    name_label = "Name:",
    description_label = "Description:",
    new_network_name = "New Network",
    new_private_name = "New Private Network",
    external_blurb = "Your new network will be mapped to an existing physical network interface and assigned a VLAN number to use on that interface. You can select the physical interface you would like to use below.",
    internal_blurb = "Select whether you would like to automatically add this network to new VMs.",
    nic_label = "NIC:",
    vlan_label = "VLAN:",
    mtu_label = "MTU:",
    auto_add = "Automatically add this network to new virtual machines",
    /// `{min}`, `{max}`
    vlan_range = "Allowed VLAN range: {min} to {max}",
    vlan_in_use = "This VLAN number is already in use",
    vlan_zero = "VLAN 0 will receive all traffic not on any other VLAN",
    invalid_number = "Invalid number",
    /// `{min}`, `{max}`
    mtu_range = "Allowed MTU range: {min} to {max}",
    /// `{value}`
    mtu_value = "Allowed MTU value: {value}",
    chin_blurb = "Select a management interface for the new network to use and configure any additional settings before proceeding.",
    chin_interface_label = "Management interface:",

    // Bond members (wizard page and Create Bond dialog).
    col_nic = "NIC",
    col_mac = "MAC",
    col_link_status = "Link Status",
    col_speed = "Speed",
    col_duplex = "Duplex",
    col_vendor = "Vendor",
    col_device = "Device",
    col_pci = "PCI Bus Path",
    /// `{bond}`
    already_in_bond = "already in {bond}",
    bond_mode = "Bond mode",
    mode_active_active = "Active-active",
    mode_active_passive = "Active-passive",
    mode_lacp_src_mac = "LACP with load balancing based on source MAC address",
    mode_lacp_tcpudp = "LACP with load balancing based on IP and port of source and destination",
    lacp_warning = "LACP must also be configured on the switch ports",
    create = "Create",

    // Network Properties.
    /// `{name}`
    properties_title = "'{name}' Properties",
    tab_general = "General",
    tab_network_settings = "Network Settings",
    tags_label = "Tags:",
    tags_hint = "comma-separated",
    settings_blurb = "Use the controls below to configure advanced settings for your network. If your changes will result in temporary disruption of the network you will be informed below. Changes will apply to all hosts in a pool.",
    nic_help = "Network interface card. Can also be an <internal> Virtual Network.",
    internal = "<internal>",
    disruption_warning = "Warning: Your network will be temporarily disrupted when you apply these settings",
    /// `{interface}`
    cannot_change_nic_management = "(The network's NIC and VLAN cannot be changed as it is in use as the management interface '{interface}')",
    cannot_change_nic_vms = "(The network's NIC and VLAN cannot be changed as there are active VMs attached)",
    cannot_change_mtu_no_tools = "(The network's MTU cannot be changed because there are active VMs attached without tools installed)",
    /// `{interface}`
    cannot_change_mtu_management = "(The network's MTU cannot be changed because it is in use as the management interface '{interface}')",
    auto_add_properties = "Automatically add this network to new virtual machines.",

    // Configure IP Addresses.
    /// `{name}`
    ip_title = "Configure IP Addresses - {name}",
    /// `{name}`
    ip_blurb_host = "Configure IP address settings for server management traffic (the management interface) on '{name}' here.\nYou can also assign IP addresses for dedicated storage or other types of network traffic (secondary interfaces).",
    /// `{name}`
    ip_blurb_pool = "Configure IP address settings for server management traffic (the management interface) on all of the servers in pool '{name}' here.\nYou can also assign IP addresses for dedicated storage or other types of network traffic (secondary interfaces) in the pool.",
    management_tab = "Management Interface",
    management = "Management",
    purpose_unknown = "unknown",
    /// `{n}`
    aux_tab_name = "Storage {n}",
    add_ip = "Add IP address",
    remove_ip = "Remove IP Address",
    network_label = "Network:",
    ip_settings = "IP address settings:",
    ip_dns_settings = "IP address and DNS server settings:",
    dhcp = "Automatically obtain settings using DHCP",
    static_ip = "Use these settings:",
    ip_address = "IP address:",
    ip_range = "IP address range:",
    /// `{end}`
    ip_range_end = "to {end}",
    subnet = "Subnet mask:",
    gateway = "Gateway:",
    dns_preferred = "Preferred DNS server:",
    dns_alt1 = "Alternate DNS server 1:",
    dns_alt2 = "Alternate DNS server 2:",
    ha_enabled = "The primary interface cannot be reconfigured because HA is enabled",
    /// `{network}`
    in_use_management = "{network} is currently in use by the primary interface",
    /// `{network}`, `{purpose}`
    in_use = "{network} is currently in use by {purpose}",
    /// `{network}`, `{purpose}`
    network_in_use_by = "{network} (in use by {purpose})",
    /// `{network}`, `{mode}`
    page_subtext = "{network}; {mode}",
    pif_dhcp = "DHCP",
    pif_static = "Static",

    // Virtual interface dialog.
    add_vif_title = "Add Virtual Interface",
    vif_properties_title = "Virtual Interface Properties",
    vif_blurb = "Select your network and MAC address for this virtual interface and, optionally, specify a QoS limit.",
    add = "Add",
    mac_auto = "Auto-generate a MAC address",
    mac_use = "Use this MAC address:",
    mac_label = "MAC address:",
    qos_label = "QoS settings:",
    qos_enable = "Enable a QoS limit of:",
    qos_units = "Kbytes/s",
    no_networks = "There are no networks available for this network interface",
    mac_invalid = "Invalid MAC address.",
    qos_invalid = "The QoS limit value should be an integer greater than zero",

    // Operation titles and steps.
    /// `{name}`, `{pool}`
    op_create_network = "Creating network '{name}' on {pool}",
    /// `{name}`, `{pool}`
    op_remove_network = "Removing network '{name}' from {pool}",
    /// `{name}`, `{pool}`
    op_update_network = "Updating network '{name}' on {pool}",
    /// `{bond}`
    op_create_bond = "Creating {bond}",
    /// `{bond}`
    op_destroy_bond = "Destroying {bond}",
    /// `{host}`
    op_rescan = "Scanning NICs on server '{host}'",
    op_change_networking = "Reconfiguring server networking",
    op_rename_interfaces = "Renaming server network interface",
    /// `{vm}`
    op_new_vif = "Creating new virtual network interface on VM {vm}",
    /// `{network}`, `{vm}`
    op_delete_vif = "Deleting virtual network interface '{network}' from VM '{vm}'",
    /// `{network}`, `{vm}`
    op_update_vif = "Updating virtual network interface '{network}' on VM '{vm}'",
    /// `{vm}`
    op_plug_vif = "Activating virtual network interface on VM '{vm}'",
    /// `{vm}`
    op_unplug_vif = "Deactivating virtual network interface on VM '{vm}'",
    step_creating = "Creating network",
    step_removing = "Removing network",
    step_updating = "Updating network",
    /// `{pif}`
    step_bringing_up = "Configuring IP address on {pif}...",
    /// `{pif}`
    step_depurposing = "Deconfiguring management facility on {pif}...",
    /// `{pif}`
    step_management = "Configuring management interface on {pif}...",
    /// `{pif}`
    step_bringing_down = "Removing IP address from {pif}...",
    step_waiting_members = "Waiting for other pool members to recover...",
    step_scanning = "Scanning...",
    step_refreshing = "Refreshing networks...",
    step_undo = "Undoing the changes made",

    // Errors the client reports itself.
    /// `{console}`
    err_no_coordinator = "Could not find the pool coordinator in {console}'s cache.",
    /// `{console}`
    err_bond_gone = "Could not find the bond in {console}'s cache.",
    err_vlan_exists = "A VLAN with the specified tag already exists on that NIC.",
    err_network_gone = "Network has gone away",
}

/// The wording of the errors [`xen_core::network`] reports itself, if
/// `code` is one.
pub fn network_error(code: &str) -> Option<String> {
    use crate::i18n::fill;
    use xen_core::network::errors;
    let n = net_strings();
    let console = ("console", crate::branding::PRODUCT_NAME);
    Some(match code {
        errors::NO_COORDINATOR => fill(n.err_no_coordinator, &[console]),
        errors::BOND_GONE => fill(n.err_bond_gone, &[console]),
        errors::VLAN_EXISTS => n.err_vlan_exists.to_owned(),
        errors::NETWORK_GONE => n.err_network_gone.to_owned(),
        _ => return None,
    })
}
