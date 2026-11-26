//! The Search tab (`SearchPage`, `Searcher`, `QueryElement`,
//! `GroupingControl`, `QueryPanel`): the search options (what to search
//! for, the filter tree, the grouping), the saved and built-in searches,
//! Save / Export / Import, and the results grid with its columns.
//!
//! The query builder edits the filter classes the original's builder
//! makes; a filter of another class read from a saved search is kept as
//! it is and shown as such. The grid shows the original's columns (Name,
//! CPU Usage, Used Memory, Disks, Network, Address, HA, Uptime and one
//! per custom field); the performance figures come from
//! [`xen_core::search_metrics`], refreshed every 30 seconds while the tab
//! is shown.

use std::collections::{HashMap, HashSet};

use egui::{ComboBox, DragValue, Grid, Id, RichText, ScrollArea, TextEdit, Ui};
use xen_core::org::{self, CustomFieldType, CustomFieldValue};
use xen_core::search::{
    DateMatch, Filter, GroupKind, Grouping, GroupingKind, IntMatch, ObjectTypes, PropertyName,
    Query, Search, Sort, StringMatch,
};
use xen_core::search_eval::{
    self, GroupTag, GroupValue, HA_PRIORITIES, Node, NodeItem, ObjKey, POWER_STATES, World, tools,
};
use xen_core::search_metrics::{Metrics, MetricsHandle};
use xen_core::{AppState, BackendHandle, Command, ConnectionId};

use crate::i18n::fill;
use crate::org_strings::{self, org_strings, property_name, type_name};

/// The default columns (`QueryPanel.DEFAULT_COLUMNS`).
pub const DEFAULT_COLUMNS: [&str; 8] = [
    "name", "cpu", "memory", "disks", "network", "ip", "ha", "uptime",
];

/// What the user asked for in the tab.
pub enum SearchAction {
    /// Select this object in the tree and show its General tab.
    Select(ObjKey),
    /// Open the Save dialog for this search.
    Save(Search),
    /// Open the Export dialog for this search.
    Export(Search),
    /// Open the Import dialog.
    Import,
}

/// The Search tab's state.
pub struct SearchPanel {
    /// The search the selection gave, as text (a new one resets the tab).
    base_key: String,
    pub search: Search,
    pub title: String,
    pub options_open: bool,
    /// The column sorted by, ascending.
    pub sort: Option<(String, bool)>,
    collapsed: HashSet<String>,
    pub hidden_columns: HashSet<String>,
    metrics: HashMap<ConnectionId, MetricsHandle>,
    shown: bool,
}

impl Default for SearchPanel {
    fn default() -> Self {
        Self {
            base_key: String::new(),
            search: Search::new(Query::new(None, None), None, String::new()),
            title: String::new(),
            options_open: false,
            sort: None,
            collapsed: HashSet::new(),
            hidden_columns: HashSet::new(),
            metrics: HashMap::new(),
            shown: false,
        }
    }
}

/// The source list of a world built from the app state.
pub fn sources(state: &AppState) -> Vec<search_eval::Source<'_>> {
    state
        .connections
        .iter()
        .map(|c| search_eval::Source {
            connection: c.id,
            model: &c.model,
            connected: c.status.is_connected() && !c.model.pools.is_empty(),
            name: c.server.clone(),
        })
        .collect()
}

/// Unix seconds now.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A world over every connection.
pub fn world(state: &AppState) -> World<'_> {
    World::new(
        sources(state),
        state.settings.preferences.show_hidden_objects,
        now(),
    )
}

/// The built-in searches with their names.
pub fn default_searches() -> Vec<Search> {
    let s = org_strings();
    search_eval::default_searches()
        .into_iter()
        .map(|(key, mut search)| {
            search.name = match key {
                "vms_by_os" => s.search_vms_by_os,
                "vms_without_tools" => s.search_vms_without_tools,
                "vms_by_power_state" => s.search_vms_by_power_state,
                "vms_by_network" => s.search_vms_by_network,
                "vms_by_vapp" => s.search_vms_by_vapp,
                "objects_by_tag" => s.search_objects_by_tag,
                _ => s.search_snapshots_by_vm,
            }
            .to_owned();
            search
        })
        .collect()
}

/// Every search of the Saved Searches menus, the built-in ones first
/// (`Search.CompareTo`: default searches last in the original's sort, but
/// its menu lists them first with their own icon; here by name within
/// each kind).
pub fn all_searches(w: &World<'_>) -> Vec<(Option<ConnectionId>, Search)> {
    let mut defaults: Vec<(Option<ConnectionId>, Search)> =
        default_searches().into_iter().map(|s| (None, s)).collect();
    defaults.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    let mut saved: Vec<(Option<ConnectionId>, Search)> = search_eval::saved_searches(w)
        .into_iter()
        .map(|(c, s)| (Some(c), s))
        .collect();
    saved.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    defaults.extend(saved);
    defaults
}

/// The label of a group (`Grouping.GetGroupName`).
pub fn group_label(w: &World<'_>, tag: &GroupTag) -> String {
    let s = org_strings();
    match &tag.value {
        GroupValue::Obj(o) => w.name(o),
        GroupValue::Str(x) | GroupValue::FieldName(x) => x.clone(),
        GroupValue::Type(t) => type_name(*t).to_owned(),
        GroupValue::Enum(e) => match tag.grouping.kind {
            GroupingKind::Property(PropertyName::PowerState) => {
                org_strings::power_state_name(e).to_owned()
            }
            GroupingKind::Property(PropertyName::HaRestartPriority) => {
                org_strings::ha_priority_name(e).to_owned()
            }
            _ => e.clone(),
        },
        GroupValue::Tools(t) => org_strings::tools_name(*t).to_owned(),
        GroupValue::Bool(b) => match (&tag.grouping.kind, b) {
            (GroupingKind::Bool(PropertyName::ReadCachingEnabled), true) => {
                s.p_read_caching.to_owned()
            }
            (GroupingKind::Bool(PropertyName::ReadCachingEnabled), false) => {
                s.p_read_caching_false.to_owned()
            }
            (GroupingKind::Bool(PropertyName::VendorDeviceState), true) => {
                s.p_windows_update.to_owned()
            }
            (GroupingKind::Bool(PropertyName::VendorDeviceState), false) => {
                s.p_windows_update_false.to_owned()
            }
            (_, true) => s.yes.to_owned(),
            (_, false) => s.no.to_owned(),
        },
        GroupValue::Date(t) => xen_core::format::datetime_utc(*t, true),
        GroupValue::Field(v) => v.display(),
    }
}

/// The name of a grouping (`Grouping.GroupingName`).
pub fn grouping_name(kind: &GroupingKind) -> String {
    let s = org_strings();
    match kind {
        GroupingKind::Folder => s.p_folder_grouping.to_owned(),
        GroupingKind::AllCustomFields => s.p_custom_fields_grouping.to_owned(),
        GroupingKind::CustomField(d) => d.name.clone(),
        GroupingKind::Bool(p) | GroupingKind::Property(p) | GroupingKind::Object(p) => {
            property_name(*p).to_owned()
        }
    }
}

/// The "Search for" entries (`SearchFor.InitializeDictionaries`).
fn scope_choices() -> Vec<ObjectTypes> {
    let servers_vms = ObjectTypes::SERVER
        .union(ObjectTypes::DISCONNECTED_SERVER)
        .union(ObjectTypes::VM);
    let remote = servers_vms
        .union(ObjectTypes::USER_TEMPLATE)
        .union(ObjectTypes::REMOTE_SR);
    vec![
        ObjectTypes::POOL,
        ObjectTypes::SERVER,
        ObjectTypes::DISCONNECTED_SERVER,
        ObjectTypes::VM,
        ObjectTypes::SNAPSHOT,
        ObjectTypes::USER_TEMPLATE,
        ObjectTypes::DEFAULT_TEMPLATE,
        ObjectTypes::REMOTE_SR,
        ObjectTypes::REMOTE_SR.union(ObjectTypes::LOCAL_SR),
        ObjectTypes::VDI,
        ObjectTypes::NETWORK,
        ObjectTypes::FOLDER,
        servers_vms,
        remote,
        remote.union(ObjectTypes::LOCAL_SR),
        ObjectTypes::ALL_EXC_FOLDERS,
        ObjectTypes::ALL_INC_FOLDERS,
    ]
}

/// The grouping choices (`GroupingControl.potentialGroups`).
fn grouping_choices(w: &World<'_>) -> Vec<GroupingKind> {
    use PropertyName as P;
    let mut out = vec![
        GroupingKind::Object(P::Pool),
        GroupingKind::Object(P::Host),
        GroupingKind::Property(P::OsName),
        GroupingKind::Property(P::PowerState),
        GroupingKind::Property(P::VirtualisationStatus),
        GroupingKind::Property(P::Type),
        GroupingKind::Object(P::Networks),
        GroupingKind::Object(P::Storage),
        GroupingKind::Object(P::Disks),
        GroupingKind::Property(P::HaRestartPriority),
        GroupingKind::Bool(P::ReadCachingEnabled),
        GroupingKind::Bool(P::VendorDeviceState),
        GroupingKind::Object(P::Appliance),
        GroupingKind::Property(P::Tags),
        GroupingKind::Object(P::Vm),
        GroupingKind::AllCustomFields,
        GroupingKind::Folder,
    ];
    out.extend(
        w.definitions()
            .iter()
            .cloned()
            .map(GroupingKind::CustomField),
    );
    out
}

/// The kinds of filter the builder offers (`QueryElement.queryTypes`).
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Dummy,
    Group(GroupKind),
    Type,
    Text(PropertyName),
    Tags,
    Pool,
    Server,
    Memory,
    Ip,
    StartTime,
    Power,
    Tools,
    Os,
    HaPriority,
    Bool(PropertyName),
    InPool(bool),
    Network,
    Size,
    ParentFolder,
    AnyFolder,
    NotInFolder,
    VApp,
    Field(String),
    Unsupported,
}

impl Kind {
    fn label(&self) -> String {
        let s = org_strings();
        match self {
            Self::Dummy => s.select_filter.to_owned(),
            Self::Group(GroupKind::And) => s.all_of.to_owned(),
            Self::Group(GroupKind::Or) => s.any_of.to_owned(),
            Self::Group(GroupKind::Nor) => s.none_of.to_owned(),
            Self::Type => s.p_type.to_owned(),
            Self::Text(p) => property_name(*p).to_owned(),
            Self::Tags => s.p_tags.to_owned(),
            Self::Pool => s.p_pool.to_owned(),
            Self::Server => s.p_server.to_owned(),
            Self::Memory => s.p_memory.to_owned(),
            Self::Ip => s.p_address.to_owned(),
            Self::StartTime => s.p_start_time.to_owned(),
            Self::Power => s.p_power.to_owned(),
            Self::Tools => s.p_tools.to_owned(),
            Self::Os => s.p_os.to_owned(),
            Self::HaPriority => s.p_ha_priority.to_owned(),
            Self::Bool(p) => property_name(*p).to_owned(),
            Self::InPool(true) => s.in_a_pool.to_owned(),
            Self::InPool(false) => s.standalone.to_owned(),
            Self::Network => s.p_network.to_owned(),
            Self::Size => s.p_size.to_owned(),
            Self::ParentFolder => s.p_folder.to_owned(),
            Self::AnyFolder => s.p_folders.to_owned(),
            Self::NotInFolder => s.not_in_folder.to_owned(),
            Self::VApp => s.p_vapp.to_owned(),
            Self::Field(n) => n.clone(),
            Self::Unsupported => s.unsupported_filter.to_owned(),
        }
    }

    fn choices(w: &World<'_>) -> Vec<Kind> {
        use PropertyName as P;
        let mut out = vec![
            Kind::Dummy,
            Kind::Group(GroupKind::And),
            Kind::Group(GroupKind::Or),
            Kind::Group(GroupKind::Nor),
            Kind::Type,
            Kind::Text(P::Label),
            Kind::Text(P::Description),
            Kind::Text(P::Uuid),
            Kind::Tags,
            Kind::Pool,
            Kind::Server,
            Kind::Memory,
            Kind::Ip,
            Kind::StartTime,
            Kind::Power,
            Kind::Tools,
            Kind::Os,
            Kind::HaPriority,
            Kind::Bool(P::ReadCachingEnabled),
            Kind::Bool(P::VendorDeviceState),
            Kind::Size,
            Kind::Bool(P::Shared),
            Kind::Bool(P::HaEnabled),
            Kind::Bool(P::IsNotFullyUpgraded),
            Kind::InPool(true),
            Kind::InPool(false),
            Kind::Network,
            Kind::VApp,
            Kind::Bool(P::InAnyAppliance),
            Kind::ParentFolder,
            Kind::AnyFolder,
            Kind::NotInFolder,
            Kind::Bool(P::HasCustomFields),
        ];
        out.extend(w.definitions().iter().map(|d| Kind::Field(d.name.clone())));
        out
    }

    fn of(f: &Filter) -> Kind {
        use PropertyName as P;
        match f {
            Filter::Dummy => Kind::Dummy,
            Filter::Group { kind, .. } => Kind::Group(*kind),
            Filter::Enum {
                property: P::Type, ..
            } => Kind::Type,
            Filter::String {
                property: p @ (P::Label | P::Description | P::Uuid),
                ..
            } => Kind::Text(*p),
            Filter::ListContainsString {
                property: P::Tags, ..
            }
            | Filter::ListEmpty {
                property: P::Tags, ..
            } => Kind::Tags,
            Filter::Object {
                property: P::Pool, ..
            } => Kind::Pool,
            Filter::ListContainsObject {
                property: P::Host, ..
            } => Kind::Server,
            Filter::Int {
                property: P::Memory,
                ..
            } => Kind::Memory,
            Filter::Int {
                property: P::Size, ..
            } => Kind::Size,
            Filter::Ip { .. } => Kind::Ip,
            Filter::Date {
                property: P::StartTime,
                ..
            } => Kind::StartTime,
            Filter::Enum {
                property: P::PowerState,
                ..
            } => Kind::Power,
            Filter::Enum {
                property: P::VirtualisationStatus,
                ..
            } => Kind::Tools,
            Filter::Value {
                property: P::OsName,
                ..
            } => Kind::Os,
            Filter::Enum {
                property: P::HaRestartPriority,
                ..
            } => Kind::HaPriority,
            Filter::Bool { property, .. } => Kind::Bool(*property),
            Filter::Null {
                property: P::Pool,
                query,
            } => Kind::InPool(!*query),
            Filter::Null {
                property: P::Folder,
                query: true,
            } => Kind::NotInFolder,
            Filter::ListContainsObject {
                property: P::Networks,
                ..
            } => Kind::Network,
            Filter::Recursive {
                property: P::Folder,
                ..
            } => Kind::ParentFolder,
            Filter::RecursiveList {
                property: P::Folders,
                ..
            } => Kind::AnyFolder,
            Filter::Recursive {
                property: P::Appliance,
                ..
            } => Kind::VApp,
            Filter::CustomField { definition, .. } | Filter::CustomFieldDate { definition, .. } => {
                Kind::Field(definition.name.clone())
            }
            _ => Kind::Unsupported,
        }
    }

    fn default_filter(&self, w: &World<'_>) -> Filter {
        use PropertyName as P;
        let first = |t: ObjectTypes| {
            w.objects()
                .into_iter()
                .filter(|o| w.object_type(o) == Some(t))
                .find_map(|o| w.uuid(&o))
                .unwrap_or_default()
        };
        let uuid_is = |u: String| {
            Box::new(Filter::String {
                property: P::Uuid,
                query: u,
                kind: StringMatch::ExactMatch,
                case_sensitive: true,
            })
        };
        let first_folder = || {
            w.folder_paths()
                .into_iter()
                .find(|p| p != "/")
                .unwrap_or_default()
        };
        match self {
            Kind::Dummy | Kind::Unsupported => Filter::Dummy,
            Kind::Group(kind) => Filter::Group {
                kind: *kind,
                subs: vec![Filter::Dummy],
            },
            Kind::Type => Filter::Enum {
                property: P::Type,
                value: "VM".into(),
                equals: true,
            },
            Kind::Text(p) => Filter::String {
                property: *p,
                query: String::new(),
                kind: StringMatch::Contains,
                case_sensitive: false,
            },
            Kind::Tags => Filter::ListContainsString {
                property: P::Tags,
                query: w.tags().into_iter().next().unwrap_or_default(),
                contains: true,
            },
            Kind::Pool => Filter::Object {
                property: P::Pool,
                uuid: first(ObjectTypes::POOL),
                equals: true,
            },
            Kind::Server => Filter::ListContainsObject {
                property: P::Host,
                uuid: first(ObjectTypes::SERVER),
                contains: true,
            },
            Kind::Memory => Filter::Int {
                property: P::Memory,
                query: 0,
                kind: IntMatch::Gt,
            },
            Kind::Size => Filter::Int {
                property: P::Size,
                query: 0,
                kind: IntMatch::Gt,
            },
            Kind::Ip => Filter::Ip {
                property: P::IpAddress,
                address: String::new(),
                contains: true,
            },
            Kind::StartTime => Filter::Date {
                property: P::StartTime,
                day: w.now.div_euclid(86_400),
                kind: DateMatch::Today,
            },
            Kind::Power => Filter::Enum {
                property: P::PowerState,
                value: "Running".into(),
                equals: true,
            },
            Kind::Tools => Filter::Enum {
                property: P::VirtualisationStatus,
                value: tools::name(tools::IO_DRIVERS_INSTALLED | tools::MANAGEMENT_INSTALLED),
                equals: true,
            },
            Kind::Os => Filter::Value {
                property: P::OsName,
                query: String::new(),
                equals: true,
            },
            Kind::HaPriority => Filter::Enum {
                property: P::HaRestartPriority,
                value: "Restart".into(),
                equals: true,
            },
            Kind::Bool(p) => Filter::Bool {
                property: *p,
                query: true,
            },
            Kind::InPool(in_pool) => Filter::Null {
                property: P::Pool,
                query: !*in_pool,
            },
            Kind::Network => Filter::ListContainsObject {
                property: P::Networks,
                uuid: first(ObjectTypes::NETWORK),
                contains: true,
            },
            Kind::ParentFolder => Filter::Recursive {
                property: P::Folder,
                sub: uuid_is(first_folder()),
            },
            Kind::AnyFolder => Filter::RecursiveList {
                property: P::Folders,
                sub: uuid_is(first_folder()),
            },
            Kind::NotInFolder => Filter::Null {
                property: P::Folder,
                query: true,
            },
            Kind::VApp => Filter::Recursive {
                property: P::Appliance,
                sub: uuid_is(first(ObjectTypes::APPLIANCE)),
            },
            Kind::Field(name) => match w.definition(name) {
                Some(d) if d.kind == CustomFieldType::Date => Filter::CustomFieldDate {
                    definition: d.clone(),
                    day: w.now.div_euclid(86_400),
                    kind: DateMatch::Today,
                },
                Some(d) => Filter::CustomField {
                    definition: d.clone(),
                    query: String::new(),
                    kind: StringMatch::Contains,
                },
                None => Filter::Dummy,
            },
        }
    }
}

fn string_match_label(m: StringMatch) -> &'static str {
    let s = org_strings();
    match m {
        StringMatch::Contains => s.contains,
        StringMatch::NotContain => s.not_contains,
        StringMatch::StartsWith => s.starts_with,
        StringMatch::EndsWith => s.ends_with,
        StringMatch::ExactMatch => s.exact_match,
    }
}

fn date_match_label(m: DateMatch) -> &'static str {
    let s = org_strings();
    match m {
        DateMatch::Today => s.is_today,
        DateMatch::Yesterday => s.was_yesterday,
        DateMatch::ThisWeek => s.is_this_week,
        DateMatch::LastWeek => s.was_last_week,
        DateMatch::Before => s.is_before,
        DateMatch::After => s.is_after,
        DateMatch::Exact => s.is_exactly_date,
    }
}

fn int_match_label(m: IntMatch) -> &'static str {
    let s = org_strings();
    match m {
        IntMatch::Gt => s.bigger_than,
        IntMatch::Lt => s.smaller_than,
        IntMatch::ExactMatch => s.is_exactly,
    }
}

fn is_combo(ui: &mut Ui, id: Id, equals: &mut bool) {
    let s = org_strings();
    ComboBox::from_id_salt(id.with("is"))
        .selected_text(if *equals { s.is } else { s.is_not })
        .show_ui(ui, |ui| {
            ui.selectable_value(equals, true, s.is);
            ui.selectable_value(equals, false, s.is_not);
        });
}

fn contains_combo(ui: &mut Ui, id: Id, contains: &mut bool) {
    let s = org_strings();
    ComboBox::from_id_salt(id.with("contains"))
        .selected_text(if *contains {
            s.contains
        } else {
            s.not_contains
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(contains, true, s.contains);
            ui.selectable_value(contains, false, s.not_contains);
        });
}

fn object_combo(ui: &mut Ui, id: Id, w: &World<'_>, types: ObjectTypes, uuid: &mut String) {
    let objects: Vec<(String, String)> = {
        let mut v: Vec<(String, String)> = w
            .objects()
            .into_iter()
            .filter(|o| w.object_type(o).is_some_and(|t| types.contains(t)))
            .filter_map(|o| w.uuid(&o).map(|u| (w.name(&o), u)))
            .collect();
        v.sort();
        v
    };
    let current = objects
        .iter()
        .find(|(_, u)| u == uuid)
        .map(|(n, _)| n.clone())
        .unwrap_or_default();
    ComboBox::from_id_salt(id.with("object"))
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (name, u) in &objects {
                ui.selectable_value(uuid, u.clone(), name);
            }
        });
}

fn folder_combo(ui: &mut Ui, id: Id, w: &World<'_>, sub: &mut Filter) {
    let Filter::String { query, .. } = sub else {
        ui.label(org_strings().unsupported_filter);
        return;
    };
    ComboBox::from_id_salt(id.with("folder"))
        .selected_text(query.clone())
        .show_ui(ui, |ui| {
            for path in w.folder_paths().into_iter().filter(|p| p != "/") {
                ui.selectable_value(query, path.clone(), &path);
            }
        });
}

fn date_box(ui: &mut Ui, id: Id, day: &mut i64) {
    let text_id = id.with("date-text");
    let mut text = ui
        .data(|d| d.get_temp::<String>(text_id))
        .unwrap_or_else(|| org::format_u(*day * 86_400)[..10].to_owned());
    let r = ui.add(
        TextEdit::singleline(&mut text)
            .desired_width(90.0)
            .hint_text(org_strings().date_hint),
    );
    if r.changed()
        && let Some(t) = org::parse_date(&text)
    {
        *day = t.div_euclid(86_400);
    }
    ui.data_mut(|d| d.insert_temp(text_id, text));
}

/// One filter row (and its children); returns whether its remove button
/// was pressed.
fn filter_ui(ui: &mut Ui, w: &World<'_>, f: &mut Filter, id: Id, removable: bool) -> bool {
    let s = org_strings();
    let mut remove = false;
    let kind = Kind::of(f);
    ui.horizontal(|ui| {
        let mut chosen = kind.clone();
        ComboBox::from_id_salt(id.with("kind"))
            .selected_text(kind.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for k in Kind::choices(w) {
                    let label = k.label();
                    ui.selectable_value(&mut chosen, k, label);
                }
            });
        if chosen != kind {
            *f = chosen.default_filter(w);
        }
        match f {
            Filter::Enum {
                property: PropertyName::Type,
                value,
                equals,
            } => {
                is_combo(ui, id, equals);
                ComboBox::from_id_salt(id.with("type"))
                    .selected_text(type_name(ObjectTypes::parse(value).unwrap_or_default()))
                    .show_ui(ui, |ui| {
                        for (t, name) in ObjectTypes::NAMED {
                            if t == ObjectTypes::DOCKER_CONTAINER {
                                continue;
                            }
                            ui.selectable_value(value, name.to_owned(), type_name(t));
                        }
                    });
            }
            Filter::String { query, kind, .. } => {
                ComboBox::from_id_salt(id.with("match"))
                    .selected_text(string_match_label(*kind))
                    .show_ui(ui, |ui| {
                        for m in StringMatch::ALL {
                            ui.selectable_value(kind, m, string_match_label(m));
                        }
                    });
                ui.add(TextEdit::singleline(query).desired_width(160.0));
            }
            Filter::ListContainsString { .. } | Filter::ListEmpty { .. } => {
                // contains / does not contain / are empty / are not empty.
                let mut op = match f {
                    Filter::ListContainsString { contains: true, .. } => 0,
                    Filter::ListContainsString {
                        contains: false, ..
                    } => 1,
                    Filter::ListEmpty { empty: true, .. } => 2,
                    _ => 3,
                };
                let labels = [s.contains, s.not_contains, s.tags_empty, s.tags_not_empty];
                let before = op;
                ComboBox::from_id_salt(id.with("tagop"))
                    .selected_text(labels[op])
                    .show_ui(ui, |ui| {
                        for (i, l) in labels.iter().enumerate() {
                            ui.selectable_value(&mut op, i, *l);
                        }
                    });
                if op != before {
                    let tag = match f {
                        Filter::ListContainsString { query, .. } => query.clone(),
                        _ => w.tags().into_iter().next().unwrap_or_default(),
                    };
                    *f = match op {
                        0 | 1 => Filter::ListContainsString {
                            property: PropertyName::Tags,
                            query: tag,
                            contains: op == 0,
                        },
                        _ => Filter::ListEmpty {
                            property: PropertyName::Tags,
                            empty: op == 2,
                        },
                    };
                }
                if let Filter::ListContainsString { query, .. } = f {
                    ComboBox::from_id_salt(id.with("tag"))
                        .selected_text(query.clone())
                        .show_ui(ui, |ui| {
                            for t in w.tags() {
                                ui.selectable_value(query, t.clone(), &t);
                            }
                        });
                }
            }
            Filter::Object { uuid, equals, .. } => {
                is_combo(ui, id, equals);
                object_combo(ui, id, w, ObjectTypes::POOL, uuid);
            }
            Filter::ListContainsObject {
                property,
                uuid,
                contains,
            } => {
                contains_combo(ui, id, contains);
                let t = if *property == PropertyName::Networks {
                    ObjectTypes::NETWORK
                } else {
                    ObjectTypes::SERVER
                };
                object_combo(ui, id, w, t, uuid);
            }
            Filter::Int {
                property,
                query,
                kind,
            } => {
                ComboBox::from_id_salt(id.with("int"))
                    .selected_text(int_match_label(*kind))
                    .show_ui(ui, |ui| {
                        for m in [IntMatch::Gt, IntMatch::Lt, IntMatch::ExactMatch] {
                            ui.selectable_value(kind, m, int_match_label(m));
                        }
                    });
                let (unit, label) = if *property == PropertyName::Size {
                    (1i64 << 30, s.gb)
                } else {
                    (1i64 << 20, s.mb)
                };
                let mut v = *query / unit;
                if ui
                    .add(DragValue::new(&mut v).range(0..=i64::MAX / unit))
                    .changed()
                {
                    *query = v * unit;
                }
                ui.label(label);
            }
            Filter::Ip { address, .. } => {
                ui.add(TextEdit::singleline(address).desired_width(160.0));
            }
            Filter::Date { day, kind, .. } | Filter::CustomFieldDate { day, kind, .. } => {
                ComboBox::from_id_salt(id.with("date"))
                    .selected_text(date_match_label(*kind))
                    .show_ui(ui, |ui| {
                        for m in DateMatch::ALL {
                            ui.selectable_value(kind, m, date_match_label(m));
                        }
                    });
                if kind.takes_date() {
                    date_box(ui, id, day);
                }
            }
            Filter::Enum {
                property,
                value,
                equals,
            } => {
                is_combo(ui, id, equals);
                let choices: Vec<(String, String)> = match property {
                    PropertyName::PowerState => POWER_STATES[..4]
                        .iter()
                        .map(|p| (p.to_string(), org_strings::power_state_name(p).to_owned()))
                        .collect(),
                    PropertyName::HaRestartPriority => HA_PRIORITIES[2..]
                        .iter()
                        .map(|p| (p.to_string(), org_strings::ha_priority_name(p).to_owned()))
                        .collect(),
                    PropertyName::VirtualisationStatus => [
                        tools::NOT_INSTALLED,
                        tools::PV_DRIVERS_OUT_OF_DATE,
                        tools::UNKNOWN,
                        tools::IO_DRIVERS_INSTALLED,
                        tools::MANAGEMENT_INSTALLED,
                        tools::IO_DRIVERS_INSTALLED | tools::MANAGEMENT_INSTALLED,
                    ]
                    .iter()
                    .map(|t| (tools::name(*t), org_strings::tools_name(*t).to_owned()))
                    .collect(),
                    _ => vec![(value.clone(), value.clone())],
                };
                let current = choices
                    .iter()
                    .find(|(v, _)| v == value)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_else(|| value.clone());
                ComboBox::from_id_salt(id.with("enum"))
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (v, l) in &choices {
                            ui.selectable_value(value, v.clone(), l);
                        }
                    });
            }
            Filter::Value { query, equals, .. } => {
                is_combo(ui, id, equals);
                ui.add(TextEdit::singleline(query).desired_width(160.0));
            }
            Filter::Bool { query, .. } => {
                is_combo(ui, id, query);
            }
            Filter::Recursive {
                property: PropertyName::Folder,
                sub,
            }
            | Filter::RecursiveList {
                property: PropertyName::Folders,
                sub,
            } => {
                ui.label(s.is);
                folder_combo(ui, id, w, sub);
            }
            Filter::Recursive {
                property: PropertyName::Appliance,
                sub,
            } => {
                ui.label(s.is);
                if let Filter::String { query, .. } = &mut **sub {
                    object_combo(ui, id, w, ObjectTypes::APPLIANCE, query);
                }
            }
            Filter::CustomField { query, kind, .. } => {
                ComboBox::from_id_salt(id.with("match"))
                    .selected_text(string_match_label(*kind))
                    .show_ui(ui, |ui| {
                        for m in StringMatch::ALL {
                            ui.selectable_value(kind, m, string_match_label(m));
                        }
                    });
                ui.add(TextEdit::singleline(query).desired_width(160.0));
            }
            _ => {}
        }
        if removable && ui.small_button("x").on_hover_text(s.remove).clicked() {
            remove = true;
        }
    });
    if let Filter::Group { subs, .. } = f {
        ui.indent(id.with("subs"), |ui| {
            let mut gone = None;
            for (i, sub) in subs.iter_mut().enumerate() {
                if filter_ui(ui, w, sub, id.with(i), true) {
                    gone = Some(i);
                }
            }
            if let Some(i) = gone {
                subs.remove(i);
            }
            if ui.small_button(s.add_filter).clicked() {
                subs.push(Filter::Dummy);
            }
        });
    }
    remove
}

/// The text of a result cell, with its sort key.
pub fn cell(w: &World<'_>, metrics: &Metrics, o: &ObjKey, column: &str) -> (String, CellKey) {
    let s = org_strings();
    let hyphen = || (s.hyphen.to_owned(), CellKey::None);
    if matches!(o, ObjKey::Folder(_)) && column != "name" {
        return (String::new(), CellKey::None);
    }
    let Some(c) = o.connection() else {
        return (w.name(o), CellKey::Text(w.name(o)));
    };
    let Some(m) = w.model(c) else {
        return hyphen();
    };
    let uuid = w.uuid(o).unwrap_or_default();
    let get = |name: &str| metrics.get(&uuid, name);
    let running_vm = match o {
        ObjKey::Vm(_, r) => m
            .vms
            .get(r)
            .filter(|v| search_eval::is_real_vm(v) && v.power_state == xapi::VmPowerState::Running),
        _ => None,
    };
    match column {
        "name" => (w.name(o), CellKey::Text(w.name(o).to_lowercase())),
        "cpu" => {
            let (count, prefix) = match o {
                ObjKey::Vm(..) => match running_vm.and_then(|vm| m.get(&vm.metrics)) {
                    Some(mt) => (mt.vcpus_number.max(0) as usize, "cpu"),
                    None => return hyphen(),
                },
                ObjKey::Host(_, r) => match m.hosts.get(r) {
                    Some(h) => (h.host_cpus.len(), "cpu"),
                    None => return hyphen(),
                },
                _ => return hyphen(),
            };
            let values: Option<Vec<f64>> =
                (0..count).map(|i| get(&format!("{prefix}{i}"))).collect();
            let Some(values) = values.filter(|v| !v.is_empty()) else {
                return hyphen();
            };
            let sum: f64 = values.iter().sum();
            let pct = sum * 100.0 / count as f64;
            let text = if count == 1 {
                fill(s.cpu_one, &[("percent", &format!("{:.0}", sum * 100.0))])
            } else {
                fill(
                    s.cpu_many,
                    &[
                        ("percent", &format!("{pct:.0}")),
                        ("count", &count.to_string()),
                    ],
                )
            };
            (text, CellKey::Num(pct))
        }
        "memory" => {
            let (used, total) = match o {
                ObjKey::Vm(..) => {
                    let Some(vm) = running_vm else {
                        return hyphen();
                    };
                    if w.tools_status(m, vm) & tools::MANAGEMENT_INSTALLED == 0 {
                        return hyphen();
                    }
                    let (Some(free), Some(total)) = (get("memory_internal_free"), get("memory"))
                    else {
                        return hyphen();
                    };
                    if total == 0.0 || total < free * 1024.0 {
                        return hyphen();
                    }
                    (total - free * 1024.0, total)
                }
                ObjKey::Host(..) => {
                    let (Some(free), Some(total)) =
                        (get("memory_free_kib"), get("memory_total_kib"))
                    else {
                        return hyphen();
                    };
                    if total == 0.0 {
                        return hyphen();
                    }
                    ((total - free) * 1024.0, total * 1024.0)
                }
                ObjKey::Vdi(_, r) => {
                    return match m.vdis.get(r).map(|v| v.virtual_size) {
                        Some(size) if size > 0 => (
                            xen_core::format::memory_size(size, false),
                            CellKey::Num(size as f64),
                        ),
                        _ => hyphen(),
                    };
                }
                _ => return hyphen(),
            };
            (
                fill(
                    s.memory_usage,
                    &[
                        ("used", &xen_core::format::memory_size(used as i64, false)),
                        ("total", &xen_core::format::memory_size(total as i64, false)),
                    ],
                ),
                CellKey::Num(used),
            )
        }
        "disks" | "network" => {
            let names: Vec<(String, String)> = match (o, column) {
                (ObjKey::Vm(..), "disks") => {
                    let Some(vm) = running_vm else {
                        return hyphen();
                    };
                    if w.tools_status(m, vm) & tools::IO_DRIVERS_INSTALLED == 0 {
                        return hyphen();
                    }
                    vm.vbds
                        .iter()
                        .filter_map(|b| m.get(b))
                        .map(|b| {
                            (
                                format!("vbd_{}_read", b.device),
                                format!("vbd_{}_write", b.device),
                            )
                        })
                        .collect()
                }
                (ObjKey::Vm(..), _) => {
                    let Some(vm) = running_vm else {
                        return hyphen();
                    };
                    if w.tools_status(m, vm) & tools::IO_DRIVERS_INSTALLED == 0 {
                        return hyphen();
                    }
                    vm.vifs
                        .iter()
                        .filter_map(|v| m.get(v))
                        .map(|v| {
                            (
                                format!("vif_{}_rx", v.device),
                                format!("vif_{}_tx", v.device),
                            )
                        })
                        .collect()
                }
                (ObjKey::Host(_, r), "network") => m
                    .hosts
                    .get(r)
                    .map(|h| {
                        h.pifs
                            .iter()
                            .filter_map(|p| m.get(p))
                            .filter(|p| p.physical)
                            .map(|p| {
                                (
                                    format!("pif_{}_rx", p.device),
                                    format!("pif_{}_tx", p.device),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                _ => return hyphen(),
            };
            if names.is_empty() {
                return hyphen();
            }
            let mut sum = 0.0;
            let mut max: f64 = 0.0;
            for (a, b) in &names {
                let (Some(x), y) = (get(a), get(b)) else {
                    return hyphen();
                };
                let v = x + y.unwrap_or(0.0);
                sum += v;
                max = max.max(v);
            }
            let avg = sum / (1024.0 * names.len() as f64);
            (
                fill(
                    s.data_avg_max,
                    &[
                        ("avg", &format!("{avg:.0}")),
                        ("max", &format!("{:.0}", max / 1024.0)),
                    ],
                ),
                CellKey::Num(avg),
            )
        }
        "ip" => match w.property(o, PropertyName::IpAddress) {
            search_eval::Value::Ips(l) => (l.join(", "), CellKey::Text(l.join(","))),
            _ => hyphen(),
        },
        "uptime" => match w.property(o, PropertyName::Uptime) {
            search_eval::Value::Int(secs) => (
                crate::tabs::pretty_duration(secs),
                CellKey::Num(secs as f64),
            ),
            _ => hyphen(),
        },
        "ha" => {
            let text = match o {
                ObjKey::Vm(_, r) => match m.vms.get(r) {
                    Some(vm) if search_eval::is_real_vm(vm) => match w
                        .property(o, PropertyName::HaRestartPriority)
                    {
                        search_eval::Value::Enum(p) => org_strings::ha_priority_name(&p).to_owned(),
                        _ => org_strings::ha_priority_name("DoNotRestart").to_owned(),
                    },
                    _ => s.hyphen.to_owned(),
                },
                ObjKey::Pool(_, r) => match m.pools.get(r) {
                    Some(p) if !p.ha_enabled => s.ha_disabled.to_owned(),
                    Some(p) => fill(
                        if p.ha_plan_exists_for == 1 {
                            s.ha_tolerate_one
                        } else {
                            s.ha_tolerate
                        },
                        &[("count", &p.ha_plan_exists_for.to_string())],
                    ),
                    None => String::new(),
                },
                ObjKey::Sr(_, r) => {
                    let heartbeat = m.pool().is_some_and(|(_, p)| {
                        p.ha_statefiles.first().is_some_and(|f| {
                            m.srs
                                .get(r)
                                .is_some_and(|sr| sr.vdis.iter().any(|v| v.as_str() == f))
                        })
                    });
                    if heartbeat {
                        s.ha_heartbeat.to_owned()
                    } else {
                        String::new()
                    }
                }
                _ => s.hyphen.to_owned(),
            };
            (text.clone(), CellKey::Text(text))
        }
        other => match other
            .strip_prefix(org::CUSTOM_FIELD_COLUMN_PREFIX)
            .and_then(|n| w.definition(n))
        {
            Some(def) => match w.custom_field(o, def) {
                Some(v) => {
                    let key = match &v {
                        CustomFieldValue::Date(t) => CellKey::Num(*t as f64),
                        CustomFieldValue::Text(t) => CellKey::Text(t.to_lowercase()),
                    };
                    (v.display(), key)
                }
                None => hyphen(),
            },
            None => hyphen(),
        },
    }
}

/// A cell's sort key.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum CellKey {
    None,
    Num(f64),
    Text(String),
}

/// The title of a column (`QueryPanel.GetI18nColumnName`).
pub fn column_title(column: &str) -> String {
    let s = org_strings();
    match column {
        "name" => s.col_name.to_owned(),
        "cpu" => s.col_cpu.to_owned(),
        "memory" => s.col_memory.to_owned(),
        "disks" => s.col_disks.to_owned(),
        "network" => s.col_network.to_owned(),
        "ha" => s.col_ha.to_owned(),
        "ip" => s.col_address.to_owned(),
        "uptime" => s.col_uptime.to_owned(),
        other => other
            .strip_prefix(org::CUSTOM_FIELD_COLUMN_PREFIX)
            .unwrap_or(other)
            .to_owned(),
    }
}

/// A row of the grid.
pub enum GridRow {
    Group {
        depth: usize,
        label: String,
        key: String,
        collapsed: bool,
    },
    Object {
        depth: usize,
        object: ObjKey,
    },
}

impl SearchPanel {
    /// The columns shown: the defaults (HA only when a pool has HA on),
    /// then one per custom field, minus those hidden.
    pub fn columns(&self, w: &World<'_>) -> Vec<String> {
        let ha = w
            .sources
            .iter()
            .any(|s| s.model.pools.values().any(|p| p.ha_enabled));
        let mut out: Vec<String> = DEFAULT_COLUMNS
            .iter()
            .filter(|c| **c != "ha" || ha)
            .map(|c| (*c).to_owned())
            .collect();
        out.extend(
            w.definitions()
                .iter()
                .map(|d| format!("{}{}", org::CUSTOM_FIELD_COLUMN_PREFIX, d.name)),
        );
        out.retain(|c| c == "name" || !self.hidden_columns.contains(c));
        out
    }

    /// Shows `base` unless it is the search already shown.
    pub fn set_base(&mut self, base: &Search, title: &str) {
        let key = format!("{base:?}{title}");
        if key != self.base_key {
            self.base_key = key;
            self.search = base.clone();
            self.title = title.to_owned();
            self.sort = base
                .sorting
                .first()
                .map(|s| (s.column.clone(), s.ascending));
            self.collapsed.clear();
            if let Some(columns) = &base.columns {
                let keep: HashSet<&str> = columns.iter().map(|(c, _)| c.as_str()).collect();
                self.hidden_columns = DEFAULT_COLUMNS
                    .iter()
                    .filter(|c| !keep.contains(**c))
                    .map(|c| (*c).to_owned())
                    .collect();
            } else {
                self.hidden_columns.clear();
            }
        }
    }

    /// The search as shown, with the grid's columns and sort (what Save
    /// and Export write).
    pub fn current(&self, w: &World<'_>) -> Search {
        let mut s = self.search.clone();
        s.columns = Some(self.columns(w).into_iter().map(|c| (c, 100)).collect());
        s.sorting = self
            .sort
            .iter()
            .map(|(c, a)| Sort {
                column: c.clone(),
                ascending: *a,
            })
            .collect();
        s
    }

    /// The figures of every connected connection, watched while the tab
    /// is shown.
    fn metrics(&mut self, state: &AppState, backend: &BackendHandle) -> Metrics {
        self.shown = true;
        self.metrics.retain(|c, _| {
            state
                .connection(*c)
                .is_some_and(|v| v.status.is_connected())
        });
        let mut merged = Metrics::default();
        for c in state.connections.iter().filter(|c| c.status.is_connected()) {
            let h = self.metrics.entry(c.id).or_insert_with(|| {
                let (handle, link) = xen_core::search_metrics::pair();
                backend.send(Command::WatchSearchMetrics {
                    connection: c.id,
                    link,
                });
                handle
            });
            let m = h.metrics();
            merged.rounds += m.rounds;
            for (k, v) in &m.values {
                merged.values.insert(k.clone(), v.clone());
            }
        }
        merged
    }

    /// Ends a frame: stops the figures when the tab was not shown.
    pub fn end_frame(&mut self) {
        if !self.shown {
            self.metrics.clear();
        }
        self.shown = false;
    }

    /// The grid rows of the grouped results.
    pub fn rows(&self, w: &World<'_>, metrics: &Metrics, nodes: &[Node]) -> Vec<GridRow> {
        let mut out = Vec::new();
        self.add_rows(w, metrics, nodes, 0, "", &mut out);
        out
    }

    fn add_rows(
        &self,
        w: &World<'_>,
        metrics: &Metrics,
        nodes: &[Node],
        depth: usize,
        path: &str,
        out: &mut Vec<GridRow>,
    ) {
        let mut nodes: Vec<&Node> = nodes.iter().collect();
        if let Some((column, asc)) = &self.sort {
            nodes.sort_by(|a, b| match (&a.item, &b.item) {
                (NodeItem::Object(x), NodeItem::Object(y))
                    if a.children.is_empty() && b.children.is_empty() =>
                {
                    let (_, kx) = cell(w, metrics, x, column);
                    let (_, ky) = cell(w, metrics, y, column);
                    let o = kx.partial_cmp(&ky).unwrap_or(std::cmp::Ordering::Equal);
                    if *asc { o } else { o.reverse() }
                }
                _ => std::cmp::Ordering::Equal,
            });
        }
        for n in nodes {
            match &n.item {
                NodeItem::Object(o) => {
                    out.push(GridRow::Object {
                        depth,
                        object: o.clone(),
                    });
                    if !n.children.is_empty() {
                        let key = format!("{path}/{o:?}");
                        if !self.collapsed.contains(&key) {
                            self.add_rows(w, metrics, &n.children, depth + 1, &key, out);
                        }
                    }
                }
                NodeItem::Group(tag) => {
                    let label = group_label(w, tag);
                    let key = format!("{path}/{:?}", tag.value);
                    let collapsed = self.collapsed.contains(&key);
                    out.push(GridRow::Group {
                        depth,
                        label,
                        key: key.clone(),
                        collapsed,
                    });
                    if !collapsed {
                        self.add_rows(w, metrics, &n.children, depth + 1, &key, out);
                    }
                }
            }
        }
    }

    /// Draws the tab for `base` (the search the selection gives).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        backend: &BackendHandle,
        base: &Search,
        title: &str,
    ) -> Option<SearchAction> {
        let s = org_strings();
        self.set_base(base, title);
        let metrics = self.metrics(state, backend);
        let w = world(state);
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.title).strong().size(15.0));
        });
        ui.horizontal(|ui| {
            if ui.button(s.new_search).clicked() {
                self.search = Search::new(
                    Query::new(Some(search_eval::overview_scope()), None),
                    None,
                    String::new(),
                );
                self.title = s.new_search.to_owned();
                self.options_open = true;
            }
            let toggle = if self.options_open {
                s.hide_options
            } else {
                s.edit_search
            };
            if ui.button(toggle).clicked() {
                self.options_open = !self.options_open;
            }
            ui.menu_button(s.saved_searches, |ui| {
                for (_, search) in all_searches(&w) {
                    if ui.button(&search.name).clicked() {
                        self.title = search.name.clone();
                        self.base_key.clear();
                        self.search = search;
                        ui.close();
                    }
                }
            });
            if ui.button(s.export).clicked() {
                action = Some(SearchAction::Export(self.current(&w)));
            }
            if ui.button(s.import).clicked() {
                action = Some(SearchAction::Import);
            }
        });
        if self.options_open {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                Grid::new("search-options").num_columns(2).show(ui, |ui| {
                    ui.label(s.search_for);
                    let scope = &mut self.search.query.scope;
                    ComboBox::from_id_salt("search-for")
                        .selected_text(type_name(*scope))
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for t in scope_choices() {
                                ui.selectable_value(scope, t, type_name(t));
                            }
                        });
                    ui.end_row();
                    ui.label(s.filters);
                    ui.vertical(|ui| {
                        let f = self.search.query.filter.get_or_insert(Filter::Dummy);
                        filter_ui(ui, &w, f, ui.scope_id().with("search-filter"), false);
                    });
                    ui.end_row();
                    ui.label(s.group_by);
                    ui.horizontal_wrapped(|ui| {
                        let mut levels: Vec<GroupingKind> = self
                            .search
                            .grouping
                            .as_ref()
                            .map(|g| g.levels().into_iter().cloned().collect())
                            .unwrap_or_default();
                        let mut changed = false;
                        let mut gone = None;
                        let choices = grouping_choices(&w);
                        for (i, level) in levels.iter_mut().enumerate() {
                            let before = level.clone();
                            ComboBox::from_id_salt(("group-by", i))
                                .selected_text(grouping_name(level))
                                .show_ui(ui, |ui| {
                                    for c in &choices {
                                        ui.selectable_value(level, c.clone(), grouping_name(c));
                                    }
                                });
                            changed |= *level != before;
                            if ui.small_button("x").on_hover_text(s.remove).clicked() {
                                gone = Some(i);
                            }
                        }
                        if let Some(i) = gone {
                            levels.remove(i);
                            changed = true;
                        }
                        if ui.button(s.add_group).clicked() {
                            let next = choices
                                .iter()
                                .find(|c| !levels.contains(c))
                                .cloned()
                                .unwrap_or(GroupingKind::Property(PropertyName::Type));
                            levels.push(next);
                            changed = true;
                        }
                        if changed {
                            self.search.grouping = Grouping::from_levels(&levels);
                        }
                    });
                    ui.end_row();
                });
                if ui.button(s.save).clicked() {
                    let mut current = self.current(&w);
                    if current.name.is_empty() {
                        current.name = self.title.clone();
                    }
                    action = Some(SearchAction::Save(current));
                }
            });
        }
        ui.separator();
        let nodes = w.group(&self.search);
        let columns = self.columns(&w);
        let rows = self.rows(&w, &metrics, &nodes);
        if rows.is_empty() {
            ui.label(RichText::new(s.no_results).weak());
            return action;
        }
        let mut toggle: Option<String> = None;
        let mut sort_by: Option<String> = None;
        let mut hide: Option<(String, bool)> = None;
        ScrollArea::both()
            .id_salt("search-results")
            .auto_shrink(false)
            .show(ui, |ui| {
                Grid::new("search-grid")
                    .striped(true)
                    .num_columns(columns.len())
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        for c in &columns {
                            let mut title = column_title(c);
                            if let Some((sc, asc)) = &self.sort
                                && sc == c
                            {
                                title.push_str(if *asc { " ^" } else { " v" });
                            }
                            let r = ui.button(RichText::new(title).strong());
                            if r.clicked() {
                                sort_by = Some(c.clone());
                            }
                            r.context_menu(|ui| {
                                ui.label(RichText::new(s.columns).weak());
                                let all: Vec<String> = DEFAULT_COLUMNS
                                    .iter()
                                    .map(|c| (*c).to_owned())
                                    .chain(w.definitions().iter().map(|d| {
                                        format!("{}{}", org::CUSTOM_FIELD_COLUMN_PREFIX, d.name)
                                    }))
                                    .filter(|c| c != "name")
                                    .collect();
                                for c in all {
                                    let mut on = !self.hidden_columns.contains(&c);
                                    if ui.checkbox(&mut on, column_title(&c)).changed() {
                                        hide = Some((c, !on));
                                    }
                                }
                            });
                        }
                        ui.end_row();
                        for row in &rows {
                            match row {
                                GridRow::Group {
                                    depth,
                                    label,
                                    key,
                                    collapsed,
                                } => {
                                    ui.horizontal(|ui| {
                                        ui.add_space(*depth as f32 * 16.0);
                                        let marker = if *collapsed { "+" } else { "-" };
                                        if ui
                                            .selectable_label(
                                                false,
                                                RichText::new(format!("{marker} {label}")).strong(),
                                            )
                                            .clicked()
                                        {
                                            toggle = Some(key.clone());
                                        }
                                    });
                                    for _ in 1..columns.len() {
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                                GridRow::Object { depth, object } => {
                                    for c in &columns {
                                        if c == "name" {
                                            ui.horizontal(|ui| {
                                                ui.add_space(*depth as f32 * 16.0);
                                                ui.vertical(|ui| {
                                                    if ui.link(w.name(object)).clicked() {
                                                        action = Some(SearchAction::Select(
                                                            object.clone(),
                                                        ));
                                                    }
                                                    let d = w.description(object);
                                                    if !d.is_empty() {
                                                        ui.label(RichText::new(d).weak().small());
                                                    }
                                                });
                                            });
                                        } else {
                                            ui.label(cell(&w, &metrics, object, c).0);
                                        }
                                    }
                                    ui.end_row();
                                }
                            }
                        }
                    });
            });
        if let Some(k) = toggle
            && !self.collapsed.remove(&k)
        {
            self.collapsed.insert(k);
        }
        if let Some(c) = sort_by {
            self.sort = match &self.sort {
                Some((sc, asc)) if *sc == c => Some((c, !asc)),
                _ => Some((c, true)),
            };
        }
        if let Some((c, hidden)) = hide {
            if hidden {
                self.hidden_columns.insert(c);
            } else {
                self.hidden_columns.remove(&c);
            }
        }
        action
    }
}
