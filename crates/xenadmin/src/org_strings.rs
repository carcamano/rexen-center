//! User-interface strings of F13: the navigation views, the Search tab
//! (query builder, grouping, result columns), folders, tags, custom
//! fields, saved searches and the operation titles.
//!
//! The English texts are the original console's (`XenModel/Messages.resx`,
//! `NavigationPane`, `Searcher`, `SearchPage`, `FolderChangeDialog`,
//! `NewTagDialog`, `NewCustomFieldDialog`, `NameAndConnectionPrompt` and
//! `GeneralEditPage` resources), without the `&` access keys, `{0}`
//! written as named placeholders. A few are this console's own; each says
//! so.

use xen_core::operations::OrgKind;
use xen_core::search::{ObjectTypes, PropertyName};
use xen_core::search_eval::tools;

use crate::i18n::fill;
use crate::locale::string_table;

string_table! {
    namespace = "org", struct OrgStrings, fn org_strings;

    // ---- navigation pane ----------------------------------------------------
    nav_infrastructure = "Infrastructure",
    nav_objects = "Objects",
    nav_organization = "Organization Views",
    nav_saved = "Saved Searches",
    /// The navigation tabs' last segment: the organization views and the
    /// saved searches (this console's own).
    nav_more = "More",
    nav_tags = "Objects by Tag",
    nav_folders = "Objects by Folder",
    nav_fields = "Objects by Custom Field",
    nav_vapps = "vApps",
    /// `VIEW_OBJECTS`.
    view_objects = "Objects by Type",
    /// `TAGS`.
    view_tags = "Tags",
    /// `FOLDERS`.
    view_folders = "Folders",
    /// `CUSTOM_FIELDS`.
    view_fields = "Custom Fields",
    /// `VM_APPLIANCES`.
    view_vapps = "vApps",
    /// This console's: an empty view.
    view_empty = "Nothing to show.",

    // ---- default searches ---------------------------------------------------
    search_vms_by_os = "VMs by Operating System",
    search_vms_without_tools = "VMs without XCP-ng Guest Tools",
    search_vms_by_power_state = "VMs by Power State",
    search_vms_by_network = "VMs by Network",
    search_vms_by_vapp = "VMs by vApp",
    search_objects_by_tag = "Resources by Tag",
    search_snapshots_by_vm = "VMs and Snapshots",
    /// `SEARCH_TITLE_OVERVIEW`.
    search_overview = "All Servers, VMs, Custom Templates & Remote SRs",
    /// `SEARCH_TITLE_POOL` / `SEARCH_TITLE_HOST`.
    search_object_overview = "{name}: Overview",

    // ---- Search tab ---------------------------------------------------------
    tab_search = "Search",
    new_search = "New Search",
    edit_search = "Edit Search",
    hide_options = "Hide Search Options",
    saved_searches = "Saved Searches",
    export = "Export...",
    import = "Import...",
    save = "Save...",
    search_for = "Search for",
    filters = "Filters",
    group_by = "Group by",
    add_group = "More",
    remove = "Remove",
    /// `SELECT_A_FILTER`.
    select_filter = "Select a filter...",
    all_of = "All of the following:",
    any_of = "Any of the following:",
    none_of = "None of the following:",
    add_filter = "Add filter",
    /// This console's: a filter of a class the builder does not edit.
    unsupported_filter = "(a filter this console keeps but does not edit)",
    /// This console's: shown while nothing matches.
    no_results = "No results.",
    /// This console's: the grid's column chooser.
    columns = "Columns",

    // search for (`SearchFor`)
    type_pools = "Pools",
    type_servers = "Servers",
    type_disconnected = "Disconnected servers",
    type_vms = "VMs",
    type_snapshots = "Snapshots",
    type_custom_templates = "Custom Templates",
    type_default_templates = "XCP-ng Templates",
    type_remote_srs = "Remote Storage Repositories",
    type_local_srs = "Local Storage Repositories",
    type_all_srs = "All Storage Repositories",
    type_vdis = "Virtual Disks",
    type_networks = "Networks",
    type_folders = "Folders",
    type_vapp = "vApp",
    type_servers_vms = "Servers and VMs",
    type_servers_vms_templates_remote = "Servers, VMs, Custom Templates and Remote SRs",
    type_servers_vms_templates_all = "Servers, VMs, Custom Templates and all SRs",
    type_all = "All resources",
    type_all_folders = "All resources and folders",
    /// This console's: a scope the list does not name.
    type_custom = "Custom...",

    // properties (`PropertyNames_i18n`)
    p_name = "Name",
    p_description = "Description",
    p_uuid = "UUID",
    p_type = "Type",
    p_tags = "Tags",
    p_server = "Server",
    p_pool = "Pool",
    p_network = "Network",
    p_sr = "Storage Repository",
    p_disk = "Virtual Disk",
    p_memory = "Memory",
    p_os = "Operating System",
    p_power = "Power state",
    p_tools = "Virtualization",
    p_start_time = "Start time",
    p_ha_priority = "HA restart priority",
    p_size = "Size",
    p_address = "Address",
    p_uptime = "Uptime",
    p_ha = "HA",
    p_upgraded = "Partially upgraded",
    p_vapp = "vApp",
    p_shared = "Shared",
    p_vm = "VM",
    p_read_caching = "Using read caching",
    p_read_caching_false = "Not using read caching",
    p_folder = "Parent folder",
    p_folders = "Any ancestor folder",
    p_sr_type = "Type",
    p_custom_fields = "Has any custom field",
    p_in_vapp = "Is in a vApp",
    p_windows_update = "Windows Update capable",
    p_windows_update_false = "Not Windows Update capable",
    p_folder_grouping = "Folder",
    p_custom_fields_grouping = "Custom Fields",
    not_in_folder = "Not in any folder",
    in_a_pool = "Is in a pool",
    standalone = "Is standalone",

    // match types
    contains = "contains",
    not_contains = "does not contain",
    starts_with = "starts with",
    ends_with = "ends with",
    exact_match = "exactly matches",
    is = "is",
    is_not = "is not",
    is_today = "is today",
    was_yesterday = "was yesterday",
    is_this_week = "is this week",
    was_last_week = "was last week",
    is_before = "is before",
    is_after = "is after",
    is_exactly_date = "is exactly",
    bigger_than = "bigger than",
    smaller_than = "smaller than",
    is_exactly = "is exactly",
    /// This console's: tags of none.
    tags_empty = "are empty",
    tags_not_empty = "are not empty",
    yes = "Yes",
    no = "No",
    mb = "MB",
    gb = "GB",
    /// This console's: the date box.
    date_hint = "yyyy-mm-dd",

    // tools status (`VirtualisationStatus_i18n`)
    tools_not_optimized = "Not optimized",
    tools_out_of_date = "Out of date",
    tools_unknown = "Unknown",
    tools_io_only = "I/O optimized only",
    tools_agent_only = "Management Agent installed only",
    tools_optimized = "Fully optimized",

    // columns
    col_name = "Name",
    col_cpu = "CPU Usage",
    col_memory = "Used Memory",
    col_disks = "Disks",
    col_network = "Network",
    col_address = "Address",
    col_uptime = "Uptime",
    col_ha = "HA",
    cpu_one = "{percent}% of 1 CPU",
    cpu_many = "{percent}% of {count} CPUs",
    memory_usage = "{used} of {total}",
    data_avg_max = "{avg}/{max}",
    hyphen = "-",
    ha_disabled = "Disabled",
    ha_tolerate_one = "{count} failure tolerated",
    ha_tolerate = "{count} failures tolerated",
    ha_heartbeat = "HA Heartbeat SR",
    ha_restart = "Restart",
    ha_best_effort = "Restart if possible",
    ha_do_not_restart = "Do not restart",

    // saving, importing, exporting
    save_title = "Save Search",
    save_name = "Name:",
    save_location = "Location:",
    save_button = "Save",
    cancel = "Cancel",
    ok = "OK",
    delete = "Delete",
    /// `DELETE_SEARCH_PROMPT`.
    delete_search_prompt = "Are you sure you want to delete saved search '{name}'?",
    /// This console's: the file box of Import and Export.
    file_label = "File:",
    export_title = "Export Search",
    import_title = "Import Search",
    /// `UNABLE_TO_IMPORT_SEARCH`.
    import_failed = "{console} was unable to import a saved search from file '{file}'.",
    /// This console's: why an import or export failed.
    file_error = "{file}: {error}",

    // ---- folders ------------------------------------------------------------
    new_folder = "New Folder...",
    new_folder_title = "New Folder",
    new_folder_name = "New folder name:",
    rename_folder = "Rename Folder...",
    rename_folder_title = "Rename Folder",
    rename = "Rename",
    delete_folder = "Delete Folder",
    /// This console's: the question before a folder is deleted.
    delete_folder_prompt = "Delete folder '{name}'? The objects in it are not deleted; they are no longer in any folder.",
    remove_from_folder = "Remove from folder",
    change_folder_title = "Change Folder",
    change = "Change...",
    no_folder = "Do not place this resource in any folder",
    in_folder = "Place this resource in this folder:",
    folder_label = "Folder:",

    // ---- tags ---------------------------------------------------------------
    tags_label = "Tags:",
    edit_tags = "Edit tags...",
    new_tag = "New Tag:",
    create = "Create",
    untag = "Untag Object",
    rename_tag = "Rename Tag...",
    rename_tag_title = "Rename Tag",
    delete_tag = "Delete Tag",
    /// `CONFIRM_DELETE_TAG`.
    delete_tag_prompt = "Are you sure you want to delete the tag '{tag}' from all managed resources?",
    none = "<None>",

    // ---- custom fields ------------------------------------------------------
    page_custom_fields = "Custom Fields",
    custom_fields_blurb = "Custom fields can be used to store information about this resource. The fields are available to all resources in the pool.",
    new_custom_field = "New Custom Field",
    add_custom_field = "Add...",
    custom_field_blurb = "Enter a name for the new custom field and select its type. The new field will be available to all resources in the pool.",
    custom_field_duplicate = "A custom filed with this name already exists.",
    field_name = "Name:",
    field_type = "Type:",
    field_text = "Text",
    field_date = "Date & Time",
    /// `MESSAGEBOX_DELETE_CUSTOM_FIELD`.
    delete_field_prompt = "This will delete '{name}' permanently, making it unavailable to all resources in the pool and destroying all data associated with it. Do you want to continue?",
    /// This console's: the date box of a custom field.
    field_date_hint = "yyyy-mm-dd hh:mm:ss (UTC)",
    field_date_invalid = "Enter a date as yyyy-mm-dd hh:mm:ss.",
    properties = "Properties",
    /// This console's: the VM Properties dialog this phase adds.
    vm_properties_title = "'{name}' Properties",
    page_general = "General",
    general_blurb = "{console} provides several methods for labelling and organizing your resources. By using features such as folders and tags you can quickly group and manage resources across your connected servers.",
    name_label = "Name:",
    description_label = "Description:",

    // ---- drag and drop (`DragDrop*Command` status texts) -------------------
    drop_folder = "Move to folder '{folder}'",
    drop_tag = "Tag with '{tag}'",
    drop_migrate = "Migrate to '{host}'",

    // ---- operation titles ---------------------------------------------------
    op_move_one = "Move object '{object}' to folder '{folder}'...",
    op_move_many = "Move selected objects to folder '{folder}'...",
    op_delete_one = "Deleting object...",
    op_delete_many = "Deleting objects...",
    op_rename_folder = "Renaming folder '{folder}' to '{name}'...",
    op_new_folder = "Create new folder '{path}'",
    op_new_folders = "Create new folders '{path}'",
    op_add_tag = "Add tag '{tag}'",
    op_remove_tag = "Delete tag '{tag}'",
    op_rename_tag = "Rename tag '{tag}'",
    op_delete_tag = "Delete tag '{tag}'",
    op_folder_tags = "Save folder and tags",
    op_add_field = "Add custom field '{name}'",
    op_remove_field = "Delete custom field '{name}'",
    op_save_fields = "Save Custom Fields",
    op_save_search = "Save Search '{name}'",
    op_delete_search = "Delete Search '{name}'",
    /// `UPDATE_PROPERTIES`.
    op_vm_names = "Saving properties of {name}",
}

/// The title of an F13 operation.
pub fn org_title(kind: &OrgKind) -> String {
    let s = org_strings();
    match kind {
        OrgKind::MoveToFolder {
            object: Some(o),
            folder,
        } => fill(s.op_move_one, &[("object", o), ("folder", folder)]),
        OrgKind::MoveToFolder {
            object: None,
            folder,
        } => fill(s.op_move_many, &[("folder", folder)]),
        OrgKind::DeleteFromFolders { count } => if *count == 1 {
            s.op_delete_one
        } else {
            s.op_delete_many
        }
        .to_owned(),
        OrgKind::RenameFolder { folder, name } => {
            fill(s.op_rename_folder, &[("folder", folder), ("name", name)])
        }
        OrgKind::NewFolders { paths } => {
            let t = if paths.len() == 1 {
                s.op_new_folder
            } else {
                s.op_new_folders
            };
            fill(t, &[("path", &paths.join("; "))])
        }
        OrgKind::AddTag { tag } => fill(s.op_add_tag, &[("tag", tag)]),
        OrgKind::RemoveTag { tag } => fill(s.op_remove_tag, &[("tag", tag)]),
        OrgKind::RenameTag { old, .. } => fill(s.op_rename_tag, &[("tag", old)]),
        OrgKind::DeleteTag { tag } => fill(s.op_delete_tag, &[("tag", tag)]),
        OrgKind::FolderAndTags { .. } => s.op_folder_tags.to_owned(),
        OrgKind::AddCustomField { name } => fill(s.op_add_field, &[("name", name)]),
        OrgKind::RemoveCustomField { name } => fill(s.op_remove_field, &[("name", name)]),
        OrgKind::SaveCustomFields { .. } => s.op_save_fields.to_owned(),
        OrgKind::SaveSearch { name } => fill(s.op_save_search, &[("name", name)]),
        OrgKind::DeleteSearch { name } => fill(s.op_delete_search, &[("name", name)]),
        OrgKind::VmNames { name } => fill(s.op_vm_names, &[("name", name)]),
    }
}

/// `ObjectTypes_i18n` (the group names of "Objects by Type") and the
/// "Search for" entries (`SearchFor.typeNames`).
pub fn type_name(t: ObjectTypes) -> &'static str {
    let s = org_strings();
    let all_srs = ObjectTypes::LOCAL_SR.union(ObjectTypes::REMOTE_SR);
    let servers_vms = ObjectTypes::SERVER
        .union(ObjectTypes::DISCONNECTED_SERVER)
        .union(ObjectTypes::VM);
    let remote = servers_vms
        .union(ObjectTypes::USER_TEMPLATE)
        .union(ObjectTypes::REMOTE_SR);
    match t {
        ObjectTypes::POOL => s.type_pools,
        ObjectTypes::SERVER => s.type_servers,
        ObjectTypes::DISCONNECTED_SERVER => s.type_disconnected,
        ObjectTypes::VM => s.type_vms,
        ObjectTypes::SNAPSHOT => s.type_snapshots,
        ObjectTypes::USER_TEMPLATE => s.type_custom_templates,
        ObjectTypes::DEFAULT_TEMPLATE => s.type_default_templates,
        ObjectTypes::REMOTE_SR => s.type_remote_srs,
        ObjectTypes::LOCAL_SR => s.type_local_srs,
        ObjectTypes::VDI => s.type_vdis,
        ObjectTypes::NETWORK => s.type_networks,
        ObjectTypes::FOLDER => s.type_folders,
        ObjectTypes::APPLIANCE => s.type_vapp,
        ObjectTypes::ALL_EXC_FOLDERS => s.type_all,
        ObjectTypes::ALL_INC_FOLDERS => s.type_all_folders,
        t if t == all_srs => s.type_all_srs,
        t if t == servers_vms => s.type_servers_vms,
        t if t == remote => s.type_servers_vms_templates_remote,
        t if t == remote.union(ObjectTypes::LOCAL_SR) => s.type_servers_vms_templates_all,
        _ => s.type_custom,
    }
}

/// `PropertyNames_i18n`.
pub fn property_name(p: PropertyName) -> &'static str {
    let s = org_strings();
    use PropertyName as P;
    match p {
        P::Description => s.p_description,
        P::Host => s.p_server,
        P::Label => s.p_name,
        P::Uuid => s.p_uuid,
        P::Networks => s.p_network,
        P::OsName => s.p_os,
        P::Pool => s.p_pool,
        P::PowerState => s.p_power,
        P::StartTime => s.p_start_time,
        P::Storage => s.p_sr,
        P::Disks => s.p_disk,
        P::Type => s.p_type,
        P::VirtualisationStatus => s.p_tools,
        P::HaRestartPriority => s.p_ha_priority,
        P::Appliance => s.p_vapp,
        P::Tags => s.p_tags,
        P::Shared => s.p_shared,
        P::HaEnabled => s.p_ha,
        P::IsNotFullyUpgraded => s.p_upgraded,
        P::IpAddress => s.p_address,
        P::Vm => s.p_vm,
        P::ReadCachingEnabled => s.p_read_caching,
        P::Memory => s.p_memory,
        P::SrType => s.p_sr_type,
        P::Folder => s.p_folder,
        P::Folders => s.p_folders,
        P::HasCustomFields => s.p_custom_fields,
        P::InAnyAppliance => s.p_in_vapp,
        P::VendorDeviceState => s.p_windows_update,
        P::Size => s.p_size,
        P::Uptime => s.p_uptime,
        other => other.name(),
    }
}

/// `VirtualisationStatus_i18n`.
pub fn tools_name(flags: u32) -> &'static str {
    let s = org_strings();
    let both = tools::IO_DRIVERS_INSTALLED | tools::MANAGEMENT_INSTALLED;
    if flags & both == both {
        s.tools_optimized
    } else if flags & tools::IO_DRIVERS_INSTALLED != 0 {
        s.tools_io_only
    } else if flags & tools::MANAGEMENT_INSTALLED != 0 {
        s.tools_agent_only
    } else if flags & tools::PV_DRIVERS_OUT_OF_DATE != 0 {
        s.tools_out_of_date
    } else if flags & tools::UNKNOWN != 0 {
        s.tools_unknown
    } else {
        s.tools_not_optimized
    }
}

/// `Helpers.RestartPriorityI18n`.
pub fn ha_priority_name(p: &str) -> &'static str {
    let s = org_strings();
    match p {
        "Restart" | "AlwaysRestart" | "AlwaysRestartHighPriority" => s.ha_restart,
        "BestEffort" => s.ha_best_effort,
        _ => s.ha_do_not_restart,
    }
}

/// `Label-VM.power_state-*`.
pub fn power_state_name(p: &str) -> &'static str {
    match p {
        "Halted" => "Halted",
        "Paused" => "Paused",
        "Running" => "Running",
        "Suspended" => "Suspended",
        _ => "Unknown",
    }
}
