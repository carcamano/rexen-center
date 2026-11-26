//! Searches (`XenModel/XenSearch`): what to look for ([`Query`]: a
//! scope of object types and a filter tree), how to group the results
//! ([`Grouping`]), the result columns and their sorting, and the XML the
//! original saves them as (`SearchMarshalling`): on the pool in
//! `gui_config["xensearch.Search-<uuid>"]`, and in `.xensearch` files.
//!
//! This is a lean port (plan §6): the filter and grouping classes the
//! original's query builder makes are read, written and evaluated; the
//! classes it never makes and the plug-in-only properties are read when
//! well formed but some evaluate to "no match" (see
//! [`PropertyName::evaluated`]). Docker containers are not supported.
//! Evaluation, grouping, the default searches and the result columns are
//! in [`crate::search_eval`].

use std::fmt::Write as _;

#[cfg(test)]
use crate::org::CustomFieldType;
use crate::org::{CustomFieldDefinition, xml_attr};

/// `ObjectTypes`: one bit per kind of object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ObjectTypes(pub u32);

impl ObjectTypes {
    pub const NONE: Self = Self(0);
    pub const POOL: Self = Self(1 << 0);
    pub const SERVER: Self = Self(1 << 1);
    pub const DISCONNECTED_SERVER: Self = Self(1 << 2);
    pub const VM: Self = Self(1 << 3);
    pub const SNAPSHOT: Self = Self(1 << 4);
    pub const USER_TEMPLATE: Self = Self(1 << 5);
    pub const DEFAULT_TEMPLATE: Self = Self(1 << 6);
    pub const REMOTE_SR: Self = Self(1 << 7);
    pub const LOCAL_SR: Self = Self(1 << 8);
    pub const VDI: Self = Self(1 << 9);
    pub const NETWORK: Self = Self(1 << 10);
    pub const FOLDER: Self = Self(1 << 11);
    pub const ALL_INC_FOLDERS: Self = Self((1 << 12) - 1);
    pub const ALL_EXC_FOLDERS: Self = Self(((1 << 12) - 1) & !(1 << 11));
    pub const APPLIANCE: Self = Self(1 << 13);
    pub const DOCKER_CONTAINER: Self = Self(1 << 14);

    /// The single-bit types with their XML (enum) names, in value order.
    pub const NAMED: [(Self, &'static str); 14] = [
        (Self::POOL, "Pool"),
        (Self::SERVER, "Server"),
        (Self::DISCONNECTED_SERVER, "DisconnectedServer"),
        (Self::VM, "VM"),
        (Self::SNAPSHOT, "Snapshot"),
        (Self::USER_TEMPLATE, "UserTemplate"),
        (Self::DEFAULT_TEMPLATE, "DefaultTemplate"),
        (Self::REMOTE_SR, "RemoteSR"),
        (Self::LOCAL_SR, "LocalSR"),
        (Self::VDI, "VDI"),
        (Self::NETWORK, "Network"),
        (Self::FOLDER, "Folder"),
        (Self::APPLIANCE, "Appliance"),
        (Self::DOCKER_CONTAINER, "DockerContainer"),
    ];

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The enum name of a single type (`Enum.ToString`).
    pub fn name(self) -> Option<&'static str> {
        Self::NAMED
            .iter()
            .find(|(t, _)| *t == self)
            .map(|(_, n)| *n)
    }

    /// `Enum.Parse` of one name (`SR` reads as `RemoteSR`, as the
    /// original's `EnumPropertyQuery` does).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "None" => Some(Self::NONE),
            "SR" => Some(Self::REMOTE_SR),
            "AllIncFolders" => Some(Self::ALL_INC_FOLDERS),
            "AllExcFolders" => Some(Self::ALL_EXC_FOLDERS),
            n => Self::NAMED.iter().find(|(_, x)| *x == n).map(|(t, _)| *t),
        }
    }
}

/// `PropertyNames`: the properties a filter, grouping or sort reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyName {
    Type,
    Label,
    Uuid,
    Description,
    Tags,
    Host,
    Pool,
    Networks,
    Storage,
    Disks,
    Memory,
    OsName,
    PowerState,
    VirtualisationStatus,
    StartTime,
    HaRestartPriority,
    Size,
    IpAddress,
    Uptime,
    HaEnabled,
    IsNotFullyUpgraded,
    Appliance,
    Shared,
    Vm,
    Dockervm,
    ReadCachingEnabled,
    Folder,
    Folders,
    MemoryText,
    MemoryValue,
    MemoryRank,
    CpuText,
    CpuValue,
    DiskText,
    NetworkText,
    HaText,
    ConnectionHostname,
    SrType,
    License,
    HasCustomFields,
    InAnyAppliance,
    VendorDeviceState,
}

impl PropertyName {
    const ALL: [(Self, &'static str); 42] = [
        (Self::Type, "type"),
        (Self::Label, "label"),
        (Self::Uuid, "uuid"),
        (Self::Description, "description"),
        (Self::Tags, "tags"),
        (Self::Host, "host"),
        (Self::Pool, "pool"),
        (Self::Networks, "networks"),
        (Self::Storage, "storage"),
        (Self::Disks, "disks"),
        (Self::Memory, "memory"),
        (Self::OsName, "os_name"),
        (Self::PowerState, "power_state"),
        (Self::VirtualisationStatus, "virtualisation_status"),
        (Self::StartTime, "start_time"),
        (Self::HaRestartPriority, "ha_restart_priority"),
        (Self::Size, "size"),
        (Self::IpAddress, "ip_address"),
        (Self::Uptime, "uptime"),
        (Self::HaEnabled, "ha_enabled"),
        (Self::IsNotFullyUpgraded, "isNotFullyUpgraded"),
        (Self::Appliance, "appliance"),
        (Self::Shared, "shared"),
        (Self::Vm, "vm"),
        (Self::Dockervm, "dockervm"),
        (Self::ReadCachingEnabled, "read_caching_enabled"),
        (Self::Folder, "folder"),
        (Self::Folders, "folders"),
        (Self::MemoryText, "memoryText"),
        (Self::MemoryValue, "memoryValue"),
        (Self::MemoryRank, "memoryRank"),
        (Self::CpuText, "cpuText"),
        (Self::CpuValue, "cpuValue"),
        (Self::DiskText, "diskText"),
        (Self::NetworkText, "networkText"),
        (Self::HaText, "haText"),
        (Self::ConnectionHostname, "connection_hostname"),
        (Self::SrType, "sr_type"),
        (Self::License, "license"),
        (Self::HasCustomFields, "has_custom_fields"),
        (Self::InAnyAppliance, "in_any_appliance"),
        (Self::VendorDeviceState, "vendor_device_state"),
    ];

    /// The XML name.
    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(p, _)| *p == self)
            .map(|(_, n)| *n)
            .unwrap_or("")
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == name).map(|(p, _)| *p)
    }

    /// Whether this console evaluates the property in filters and
    /// groupings. The others (the plug-in-only `connection_hostname` and
    /// `license`, Docker's `dockervm`, and the column-only metrics) read
    /// as no value, so their filters never match.
    pub fn evaluated(self) -> bool {
        !matches!(
            self,
            Self::Dockervm
                | Self::ConnectionHostname
                | Self::License
                | Self::MemoryText
                | Self::MemoryValue
                | Self::MemoryRank
                | Self::CpuText
                | Self::CpuValue
                | Self::DiskText
                | Self::NetworkText
                | Self::HaText
        )
    }
}

/// `StringPropertyQuery.PropertyQueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringMatch {
    ExactMatch,
    Contains,
    StartsWith,
    EndsWith,
    NotContain,
}

impl StringMatch {
    pub const ALL: [Self; 5] = [
        Self::Contains,
        Self::NotContain,
        Self::StartsWith,
        Self::EndsWith,
        Self::ExactMatch,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::ExactMatch => "exactmatch",
            Self::Contains => "contains",
            Self::StartsWith => "startswith",
            Self::EndsWith => "endswith",
            Self::NotContain => "notcontain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::ExactMatch,
            Self::Contains,
            Self::StartsWith,
            Self::EndsWith,
            Self::NotContain,
        ]
        .into_iter()
        .find(|m| m.name() == s)
    }

    /// `StringPropertyQuery.MatchString`.
    pub fn matches(self, candidate: &str, query: &str, case_sensitive: bool) -> bool {
        if query.is_empty() {
            return true;
        }
        let (c, q) = if case_sensitive {
            (candidate.to_owned(), query.to_owned())
        } else {
            (candidate.to_lowercase(), query.to_lowercase())
        };
        match self {
            Self::Contains => c.contains(&q),
            Self::StartsWith => c.starts_with(&q),
            Self::EndsWith => c.ends_with(&q),
            Self::ExactMatch => c == q,
            Self::NotContain => !c.contains(&q),
        }
    }
}

/// `DatePropertyQuery.PropertyQueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateMatch {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    Before,
    After,
    Exact,
}

impl DateMatch {
    pub const ALL: [Self; 7] = [
        Self::Today,
        Self::Yesterday,
        Self::ThisWeek,
        Self::LastWeek,
        Self::Before,
        Self::After,
        Self::Exact,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::ThisWeek => "thisweek",
            Self::LastWeek => "lastweek",
            Self::Before => "before",
            Self::After => "after",
            Self::Exact => "exact",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }

    /// Whether the match needs a date (`before`, `after`, `exact`).
    pub fn takes_date(self) -> bool {
        matches!(self, Self::Before | Self::After | Self::Exact)
    }

    /// `DatePropertyQuery.MatchDate`, on day numbers (days since
    /// 1970-01-01, UTC on this console's side).
    pub fn matches(self, value_day: i64, query_day: i64, today: i64) -> bool {
        let diff = today - value_day;
        match self {
            Self::Today => diff == 0,
            Self::Yesterday => diff == 1,
            Self::ThisWeek => (0..7).contains(&diff),
            Self::LastWeek => (7..14).contains(&diff),
            Self::Before => value_day <= query_day,
            Self::After => value_day >= query_day,
            Self::Exact => value_day == query_day,
        }
    }
}

/// `IntPropertyQuery.PropertyQueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntMatch {
    ExactMatch,
    Gt,
    Lt,
}

impl IntMatch {
    pub fn name(self) -> &'static str {
        match self {
            Self::ExactMatch => "exactmatch",
            Self::Gt => "gt",
            Self::Lt => "lt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exactmatch" => Some(Self::ExactMatch),
            "gt" => Some(Self::Gt),
            "lt" => Some(Self::Lt),
            _ => None,
        }
    }
}

/// `GroupQuery.GroupQueryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    And,
    Or,
    Nor,
}

impl GroupKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::And => "And",
            Self::Or => "Or",
            Self::Nor => "Nor",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "And" => Some(Self::And),
            "Or" => Some(Self::Or),
            "Nor" => Some(Self::Nor),
            _ => None,
        }
    }
}

/// A filter (`QueryFilter` subclasses), with the original's class names in
/// the XML.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Filter {
    /// "Select a filter..." (`DummyQuery`): matches as "don't know"; never
    /// written.
    Dummy,
    /// `GroupQuery`: all of / any of / none of.
    Group { kind: GroupKind, subs: Vec<Filter> },
    /// `StringPropertyQuery`.
    String {
        property: PropertyName,
        query: String,
        kind: StringMatch,
        case_sensitive: bool,
    },
    /// `DatePropertyQuery`; the date as `yyyyMMdd` (day number here).
    Date {
        property: PropertyName,
        day: i64,
        kind: DateMatch,
    },
    /// `EnumPropertyQuery<T>`: `value` is the enum's name(s) as written
    /// (`Running`, `IoDriversInstalled, ManagementInstalled`).
    Enum {
        property: PropertyName,
        value: String,
        equals: bool,
    },
    /// `ValuePropertyQuery` (the OS name).
    Value {
        property: PropertyName,
        query: String,
        equals: bool,
    },
    /// `XenModelObjectPropertyQuery<T>`: the object, by UUID.
    Object {
        property: PropertyName,
        uuid: String,
        equals: bool,
    },
    /// `XenModelObjectListContainsQuery<T>`.
    ListContainsObject {
        property: PropertyName,
        uuid: String,
        contains: bool,
    },
    /// `XenModelObjectListContainsNameQuery<T>`.
    ListContainsName {
        property: PropertyName,
        query: String,
        kind: StringMatch,
        contains: bool,
    },
    /// `StringListContainsQuery` (tags).
    ListContainsString {
        property: PropertyName,
        query: String,
        contains: bool,
    },
    /// `ListEmptyQuery<T>`.
    ListEmpty { property: PropertyName, empty: bool },
    /// `IPAddressQuery`: the address as written (an address, or a
    /// network `a.b.c.d/n`).
    Ip {
        property: PropertyName,
        address: String,
        contains: bool,
    },
    /// `BooleanQuery`.
    Bool { property: PropertyName, query: bool },
    /// `NullQuery<T>`: whether the property is unset.
    Null { property: PropertyName, query: bool },
    /// `IntPropertyQuery`.
    Int {
        property: PropertyName,
        query: i64,
        kind: IntMatch,
    },
    /// `CustomFieldQuery`.
    CustomField {
        definition: CustomFieldDefinition,
        query: String,
        kind: StringMatch,
    },
    /// `CustomFieldDateQuery`.
    CustomFieldDate {
        definition: CustomFieldDefinition,
        day: i64,
        kind: DateMatch,
    },
    /// `RecursiveXMOPropertyQuery<T>`: the related object (pool, folder,
    /// vApp) matches `sub`.
    Recursive {
        property: PropertyName,
        sub: Box<Filter>,
    },
    /// `RecursiveXMOListPropertyQuery<T>`: any of the related objects
    /// (hosts, VMs, networks, SRs, disks, ancestor folders) matches `sub`.
    RecursiveList {
        property: PropertyName,
        sub: Box<Filter>,
    },
}

impl Filter {
    /// `QueryFilter.GetSubQueryFor`: the parts of the filter on `property`.
    pub fn sub_query_for(&self, property: PropertyName) -> Option<Filter> {
        match self {
            Self::Group { kind, subs } => {
                let found: Vec<Filter> = subs
                    .iter()
                    .filter_map(|s| s.sub_query_for(property))
                    .collect();
                (!found.is_empty()).then_some(Self::Group {
                    kind: *kind,
                    subs: found,
                })
            }
            Self::Dummy | Self::CustomField { .. } | Self::CustomFieldDate { .. } => None,
            other => (other.property() == Some(property)).then(|| other.clone()),
        }
    }

    /// The property a property filter reads.
    pub fn property(&self) -> Option<PropertyName> {
        match self {
            Self::String { property, .. }
            | Self::Date { property, .. }
            | Self::Enum { property, .. }
            | Self::Value { property, .. }
            | Self::Object { property, .. }
            | Self::ListContainsObject { property, .. }
            | Self::ListContainsName { property, .. }
            | Self::ListContainsString { property, .. }
            | Self::ListEmpty { property, .. }
            | Self::Ip { property, .. }
            | Self::Bool { property, .. }
            | Self::Null { property, .. }
            | Self::Int { property, .. }
            | Self::Recursive { property, .. }
            | Self::RecursiveList { property, .. } => Some(*property),
            _ => None,
        }
    }

    /// The `.NET` type argument of a generic filter class (for reading;
    /// only used to check that the XML names a known property).
    fn class_name(&self) -> &'static str {
        match self {
            Self::Dummy => "DummyQuery",
            Self::Group { .. } => "GroupQuery",
            Self::String { .. } => "StringPropertyQuery",
            Self::Date { .. } => "DatePropertyQuery",
            Self::Enum { .. } => "EnumPropertyQuery",
            Self::Value { .. } => "ValuePropertyQuery",
            Self::Object { .. } => "XenModelObjectPropertyQuery",
            Self::ListContainsObject { .. } => "XenModelObjectListContainsQuery",
            Self::ListContainsName { .. } => "XenModelObjectListContainsNameQuery",
            Self::ListContainsString { .. } => "StringListContainsQuery",
            Self::ListEmpty { .. } => "ListEmptyQuery",
            Self::Ip { .. } => "IPAddressQuery",
            Self::Bool { .. } => "BooleanQuery",
            Self::Null { .. } => "NullQuery",
            Self::Int { .. } => "IntPropertyQuery",
            Self::CustomField { .. } => "CustomFieldQuery",
            Self::CustomFieldDate { .. } => "CustomFieldDateQuery",
            Self::Recursive { .. } => "RecursiveXMOPropertyQuery",
            Self::RecursiveList { .. } => "RecursiveXMOListPropertyQuery",
        }
    }
}

/// `Query`: a scope and an optional filter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    pub scope: ObjectTypes,
    pub filter: Option<Filter>,
}

impl Query {
    /// `new Query(scope, filter)`: no scope means all types but folders.
    pub fn new(scope: Option<ObjectTypes>, filter: Option<Filter>) -> Self {
        Self {
            scope: scope.unwrap_or(ObjectTypes::ALL_EXC_FOLDERS),
            filter,
        }
    }
}

/// A grouping (`Grouping` subclasses) and what is grouped under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Grouping {
    pub kind: GroupingKind,
    pub sub: Option<Box<Grouping>>,
}

/// The kinds of grouping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupingKind {
    /// `FolderGrouping`.
    Folder,
    /// `PropertyGrouping<T>` (type, OS, power state, tools, tags, HA
    /// restart priority…).
    Property(PropertyName),
    /// `BoolGrouping`.
    Bool(PropertyName),
    /// `XenModelObjectPropertyGrouping<T>` (pool, server, network, SR,
    /// disk, VM, vApp).
    Object(PropertyName),
    /// `CustomFieldGrouping`.
    CustomField(CustomFieldDefinition),
    /// `AllCustomFieldsGrouping`.
    AllCustomFields,
}

impl Grouping {
    pub fn new(kind: GroupingKind, sub: Option<Grouping>) -> Self {
        Self {
            kind,
            sub: sub.map(Box::new),
        }
    }

    /// The groupings from the outside in.
    pub fn levels(&self) -> Vec<&GroupingKind> {
        let mut out = vec![&self.kind];
        let mut g = self;
        while let Some(sub) = &g.sub {
            out.push(&sub.kind);
            g = sub;
        }
        out
    }

    /// Builds a nested grouping from levels, outside first.
    pub fn from_levels(levels: &[GroupingKind]) -> Option<Self> {
        let mut out: Option<Grouping> = None;
        for kind in levels.iter().rev() {
            out = Some(Grouping::new(kind.clone(), out));
        }
        out
    }
}

/// A result column sort (`Sort`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sort {
    pub column: String,
    pub ascending: bool,
}

/// A search (`Search`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Search {
    pub name: String,
    /// `None` until saved.
    pub uuid: Option<String>,
    pub query: Query,
    pub grouping: Option<Grouping>,
    /// Column names and widths (`name`, `cpu`, … or `CustomField:<name>`);
    /// `None` keeps the grid's own.
    pub columns: Option<Vec<(String, i32)>>,
    pub sorting: Vec<Sort>,
    /// One of the built-in searches.
    pub default_search: bool,
}

impl Search {
    pub fn new(query: Query, grouping: Option<Grouping>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            uuid: None,
            query,
            grouping,
            columns: None,
            sorting: Vec::new(),
            default_search: false,
        }
    }

    /// `Search.AddFilter`: this search narrowed by `filter` (unnamed).
    pub fn add_filter(&self, filter: Option<Filter>) -> Self {
        let filter = match (&self.query.filter, filter) {
            (None, f) => f,
            (f, None) => f.clone(),
            (Some(a), Some(b)) => Some(Filter::Group {
                kind: GroupKind::And,
                subs: vec![a.clone(), b],
            }),
        };
        Self {
            name: String::new(),
            uuid: Some(String::new()),
            query: Query {
                scope: self.query.scope,
                filter,
            },
            grouping: self.grouping.clone(),
            columns: self.columns.clone(),
            sorting: self.sorting.clone(),
            default_search: false,
        }
    }

    /// `Search.FolderForNavigator`: the folder of a "folder is" search.
    pub fn folder_for_navigator(&self) -> Option<&str> {
        match &self.query.filter {
            Some(Filter::Recursive {
                property: PropertyName::Folder,
                sub,
            }) => match &**sub {
                Filter::String {
                    property: PropertyName::Uuid,
                    query,
                    ..
                } => Some(query),
                _ => None,
            },
            _ => None,
        }
    }

    /// `Search.EffectiveGrouping`: none for a folder search.
    pub fn effective_grouping(&self) -> Option<&Grouping> {
        if self.folder_for_navigator().is_some() {
            None
        } else {
            self.grouping.as_ref()
        }
    }
}

// ---- XML ----------------------------------------------------------------------

/// `SearchMarshalling.CURRENT_SEARCH_MAJOR_VERSION`.
pub const MAJOR_VERSION: i32 = 2;
const XMLNS: &str = "http://www.citrix.com/XenCenter/XenSearch/schema";

/// Why a search could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchXmlError(pub String);

impl std::fmt::Display for SearchXmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, SearchXmlError> {
    Err(SearchXmlError(msg.into()))
}

/// `yes` / `no` (`SearchMarshalling.AddAttribute(bool)`).
fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

/// `SearchMarshalling.ParseBool`: `yes` or `True`.
fn parse_bool(v: &str) -> bool {
    v == "yes" || v == "True"
}

fn day_text(day: i64) -> String {
    let t = day * 86_400;
    let s = crate::org::format_u(t);
    format!("{}{}{}", &s[0..4], &s[5..7], &s[8..10])
}

fn parse_day(v: &str) -> Option<i64> {
    if v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()) {
        let y: i64 = v[0..4].parse().ok()?;
        let m: u32 = v[4..6].parse().ok()?;
        let d: u32 = v[6..8].parse().ok()?;
        return Some(crate::org::days_from_civil(y, m, d));
    }
    // Old style: ISO 8601.
    crate::org::parse_date(&v.replace('T', " ")).map(|t| t.div_euclid(86_400))
}

/// A minimal XML element writer with .NET `XmlDocument` output (attributes
/// in order, `<x a="b" />` for empty elements).
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Element>,
}

impl Element {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    fn attr(mut self, name: &str, value: impl Into<String>) -> Self {
        self.attrs.push((name.to_owned(), value.into()));
        self
    }

    fn child(mut self, child: Element) -> Self {
        self.children.push(child);
        self
    }

    /// `OuterXml`.
    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            let _ = write!(out, " {k}=\"{}\"", xml_attr(v));
        }
        if self.children.is_empty() {
            out.push_str(" />");
            return;
        }
        out.push('>');
        for c in &self.children {
            c.write(out);
        }
        let _ = write!(out, "</{}>", self.name);
    }

    /// Indented with 4 spaces (`XmlTextWriter`, `Formatting.Indented`).
    fn write_indented(&self, out: &mut String, depth: usize) {
        out.push_str(&" ".repeat(depth * 4));
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            let _ = write!(out, " {k}=\"{}\"", xml_attr(v));
        }
        if self.children.is_empty() {
            out.push_str(" />");
            return;
        }
        out.push('>');
        for c in &self.children {
            out.push_str("\r\n");
            c.write_indented(out, depth + 1);
        }
        out.push_str("\r\n");
        out.push_str(&" ".repeat(depth * 4));
        let _ = write!(out, "</{}>", self.name);
    }
}

fn definition_element(d: &CustomFieldDefinition) -> Element {
    Element::new("CustomFieldDefinition")
        .attr("name", &d.name)
        .attr("type", d.kind.as_str())
        .attr("defaultValue", "")
}

fn scope_element(scope: ObjectTypes) -> Element {
    let mut e = Element::new("QueryScope");
    for (t, name) in ObjectTypes::NAMED {
        if scope.contains(t) {
            e = e.child(Element::new(name));
        }
    }
    e
}

fn filter_element(f: &Filter) -> Option<Element> {
    let prop = |e: Element, p: &PropertyName| e.attr("property", p.name());
    let e = Element::new(f.class_name());
    Some(match f {
        Filter::Dummy => return None,
        Filter::Group { kind, subs } => {
            let mut e = e.attr("type", kind.name());
            for s in subs {
                if let Some(c) = filter_element(s) {
                    e = e.child(c);
                }
            }
            e
        }
        Filter::String {
            property,
            query,
            kind,
            case_sensitive,
        } => prop(e, property)
            .attr("casesensitive", yes_no(*case_sensitive))
            .attr("query", query)
            .attr("type", kind.name()),
        Filter::Date {
            property,
            day,
            kind,
        } => prop(e, property)
            .attr("query", day_text(*day))
            .attr("type", kind.name()),
        Filter::Enum {
            property,
            value,
            equals,
        } => prop(e, property)
            .attr("equals", yes_no(*equals))
            .attr("query", value),
        Filter::Value {
            property,
            query,
            equals,
        } => prop(e, property)
            .attr("equals", yes_no(*equals))
            .attr("query", query),
        Filter::Object {
            property,
            uuid,
            equals,
        } => prop(e, property)
            .attr("equals", yes_no(*equals))
            .attr("uuid", uuid),
        Filter::ListContainsObject {
            property,
            uuid,
            contains,
        } => prop(e, property)
            .attr("contains", yes_no(*contains))
            .attr("uuid", uuid),
        Filter::ListContainsName {
            property,
            query,
            kind,
            contains,
        } => prop(e, property)
            .attr("contains", yes_no(*contains))
            .attr("query", query)
            .attr("type", kind.name()),
        Filter::ListContainsString {
            property,
            query,
            contains,
        } => prop(e, property)
            .attr("contains", yes_no(*contains))
            .attr("query", query),
        Filter::ListEmpty { property, empty } => prop(e, property).attr("empty", yes_no(*empty)),
        Filter::Ip {
            property,
            address,
            contains,
        } => prop(e, property)
            .attr("contains", yes_no(*contains))
            .attr("address", address),
        Filter::Bool { property, query } => prop(e, property).attr("query", yes_no(*query)),
        Filter::Null { property, query } => prop(e, property).attr("query", yes_no(*query)),
        Filter::Int {
            property,
            query,
            kind,
        } => prop(e, property)
            .attr("query", query.to_string())
            .attr("type", kind.name()),
        Filter::CustomField {
            definition,
            query,
            kind,
        } => e
            .child(definition_element(definition))
            .attr("query", query)
            .attr("type", kind.name()),
        Filter::CustomFieldDate {
            definition,
            day,
            kind,
        } => e
            .child(definition_element(definition))
            .attr("query", day_text(*day))
            .attr("type", kind.name()),
        Filter::Recursive { property, sub } | Filter::RecursiveList { property, sub } => {
            let e = prop(e, property);
            match filter_element(sub) {
                Some(c) => e.child(c),
                None => e,
            }
        }
    })
}

fn grouping_element(g: &Grouping) -> Element {
    let mut e = match &g.kind {
        GroupingKind::Folder => Element::new("FolderGrouping"),
        GroupingKind::Property(p) => Element::new("PropertyGrouping").attr("property", p.name()),
        GroupingKind::Bool(p) => Element::new("BoolGrouping").attr("property", p.name()),
        GroupingKind::Object(p) => {
            Element::new("XenModelObjectPropertyGrouping").attr("property", p.name())
        }
        GroupingKind::CustomField(_) => Element::new("CustomFieldGrouping"),
        GroupingKind::AllCustomFields => Element::new("AllCustomFieldsGrouping"),
    };
    if let Some(sub) = &g.sub {
        e = e.child(grouping_element(sub));
    }
    if let GroupingKind::CustomField(d) = &g.kind {
        e = e.child(definition_element(d));
    }
    e
}

fn search_element(s: &Search) -> Element {
    let mut e = Element::new("Search")
        .attr("uuid", s.uuid.clone().unwrap_or_default())
        .attr("name", &s.name)
        .attr("major_version", MAJOR_VERSION.to_string())
        .attr("minor_version", "0");
    let mut q = Element::new("Query").child(scope_element(s.query.scope));
    if let Some(f) = s.query.filter.as_ref().and_then(filter_element) {
        q = q.child(f);
    }
    e = e.child(q);
    if let Some(g) = &s.grouping {
        e = e.child(grouping_element(g));
    }
    if let Some(columns) = &s.columns {
        let mut c = Element::new("columns");
        for (name, width) in columns {
            c = c.child(
                Element::new("column")
                    .attr("name", name)
                    .attr("width", width.to_string()),
            );
        }
        e = e.child(c);
    }
    let mut sorting = Element::new("sorting");
    for sort in &s.sorting {
        sorting = sorting.child(
            Element::new("sort")
                .attr("column", &sort.column)
                .attr("ascending", yes_no(sort.ascending)),
        );
    }
    e.child(sorting)
}

/// `SearchMarshalling.SearchToXML`: one `<Search>` element, as stored in
/// the pool's `gui_config`.
pub fn search_to_xml(search: &Search) -> String {
    let mut out = String::new();
    search_element(search).write(&mut out);
    out
}

/// `SearchMarshalling.SearchesToXML`: a `.xensearch` file.
pub fn searches_to_xml(searches: &[Search]) -> String {
    let mut root = Element::new("Searches").attr("xmlns", XMLNS);
    for s in searches {
        root = root.child(search_element(s));
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<!DOCTYPE xensearch PUBLIC \"-//XENSEARCH//DTD XENSEARCH 1//EN\" \"xensearch-1.dtd\">\r\n",
    );
    root.write_indented(&mut out, 0);
    out
}

fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Result<&'a str, SearchXmlError> {
    node.attribute(name)
        .ok_or_else(|| SearchXmlError(format!("<{}> has no '{name}'", node.tag_name().name())))
}

fn property(node: roxmltree::Node<'_, '_>) -> Result<PropertyName, SearchXmlError> {
    let p = attr(node, "property")?;
    PropertyName::parse(p).ok_or_else(|| SearchXmlError(format!("unknown property '{p}'")))
}

fn elements<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(|n| n.is_element())
}

fn string_match(node: roxmltree::Node<'_, '_>) -> Result<StringMatch, SearchXmlError> {
    let t = attr(node, "type")?;
    StringMatch::parse(t).ok_or_else(|| SearchXmlError(format!("bad match type '{t}'")))
}

fn date_match(node: roxmltree::Node<'_, '_>) -> Result<DateMatch, SearchXmlError> {
    let t = attr(node, "type")?;
    DateMatch::parse(t).ok_or_else(|| SearchXmlError(format!("bad date match '{t}'")))
}

fn definition_child(
    node: roxmltree::Node<'_, '_>,
) -> Result<CustomFieldDefinition, SearchXmlError> {
    elements(node)
        .find(|n| n.tag_name().name() == "CustomFieldDefinition")
        .and_then(CustomFieldDefinition::from_node)
        .ok_or_else(|| SearchXmlError("custom field query without a definition".into()))
}

/// Reads one filter element; `Ok(None)` for one the original skips.
fn parse_filter(node: roxmltree::Node<'_, '_>) -> Result<Option<Filter>, SearchXmlError> {
    let name = node.tag_name().name();
    let f = match name {
        "DummyQuery" => return Ok(None),
        "GroupQuery" => {
            let kind = GroupKind::parse(attr(node, "type")?)
                .ok_or_else(|| SearchXmlError("bad group type".into()))?;
            let mut subs = Vec::new();
            for c in elements(node) {
                if let Some(f) = parse_filter(c)? {
                    subs.push(f);
                }
            }
            Filter::Group { kind, subs }
        }
        "StringPropertyQuery" => Filter::String {
            property: property(node)?,
            query: attr(node, "query")?.to_owned(),
            kind: string_match(node)?,
            case_sensitive: parse_bool(attr(node, "casesensitive")?),
        },
        "DatePropertyQuery" => Filter::Date {
            property: property(node)?,
            day: parse_day(attr(node, "query")?).unwrap_or(i64::MIN / 86_400),
            kind: date_match(node)?,
        },
        "EnumPropertyQuery" => Filter::Enum {
            property: property(node)?,
            value: attr(node, "query")?.to_owned(),
            equals: parse_bool(attr(node, "equals")?),
        },
        "ValuePropertyQuery" => Filter::Value {
            property: property(node)?,
            query: attr(node, "query")?.to_owned(),
            equals: parse_bool(attr(node, "equals")?),
        },
        "XenModelObjectPropertyQuery" => Filter::Object {
            property: property(node)?,
            // Very old searches name the object by reference ("query"),
            // which means nothing to another console: kept as given.
            uuid: node
                .attribute("uuid")
                .or_else(|| node.attribute("query"))
                .unwrap_or("invalid")
                .to_owned(),
            equals: parse_bool(attr(node, "equals")?),
        },
        "XenModelObjectListContainsQuery" => Filter::ListContainsObject {
            property: property(node)?,
            uuid: attr(node, "uuid")?.to_owned(),
            contains: parse_bool(attr(node, "contains")?),
        },
        "XenModelObjectListContainsNameQuery" => Filter::ListContainsName {
            property: property(node)?,
            query: attr(node, "query")?.to_owned(),
            kind: string_match(node)?,
            contains: parse_bool(attr(node, "contains")?),
        },
        "StringListContainsQuery" => Filter::ListContainsString {
            property: property(node)?,
            query: attr(node, "query")?.to_owned(),
            contains: parse_bool(attr(node, "contains")?),
        },
        "ListEmptyQuery" => Filter::ListEmpty {
            property: property(node)?,
            empty: parse_bool(attr(node, "empty")?),
        },
        "IPAddressQuery" => Filter::Ip {
            property: property(node)?,
            address: attr(node, "address")?.to_owned(),
            contains: node.attribute("contains").is_none_or(parse_bool),
        },
        "BooleanQuery" => Filter::Bool {
            property: property(node)?,
            query: parse_bool(attr(node, "query")?),
        },
        "NullQuery" => Filter::Null {
            property: property(node)?,
            query: parse_bool(attr(node, "query")?),
        },
        "IntPropertyQuery" => Filter::Int {
            property: property(node)?,
            query: attr(node, "query")?
                .parse()
                .map_err(|_| SearchXmlError("bad integer".into()))?,
            kind: IntMatch::parse(attr(node, "type")?)
                .ok_or_else(|| SearchXmlError("bad integer match".into()))?,
        },
        "CustomFieldQuery" => Filter::CustomField {
            definition: definition_child(node)?,
            query: attr(node, "query")?.to_owned(),
            kind: string_match(node)?,
        },
        "CustomFieldDateQuery" => Filter::CustomFieldDate {
            definition: definition_child(node)?,
            day: parse_day(attr(node, "query")?)
                .ok_or_else(|| SearchXmlError("bad date".into()))?,
            kind: date_match(node)?,
        },
        "RecursiveXMOPropertyQuery" | "RecursiveXMOListPropertyQuery" => {
            let property = property(node)?;
            let sub = elements(node)
                .next()
                .ok_or_else(|| SearchXmlError("recursive query without a sub-query".into()))?;
            let sub = Box::new(parse_filter(sub)?.unwrap_or(Filter::Dummy));
            if name == "RecursiveXMOPropertyQuery" {
                Filter::Recursive { property, sub }
            } else {
                Filter::RecursiveList { property, sub }
            }
        }
        other => return err(format!("unsupported filter <{other}>")),
    };
    Ok(Some(f))
}

fn parse_grouping(node: roxmltree::Node<'_, '_>) -> Result<Grouping, SearchXmlError> {
    let kind = match node.tag_name().name() {
        "FolderGrouping" => GroupingKind::Folder,
        "PropertyGrouping" => GroupingKind::Property(property(node)?),
        "BoolGrouping" => GroupingKind::Bool(property(node)?),
        "XenModelObjectPropertyGrouping" => GroupingKind::Object(property(node)?),
        "CustomFieldGrouping" => GroupingKind::CustomField(definition_child(node)?),
        "AllCustomFieldsGrouping" => GroupingKind::AllCustomFields,
        other => return err(format!("unsupported grouping <{other}>")),
    };
    let sub = elements(node)
        .find(|c| c.tag_name().name() != "CustomFieldDefinition")
        .map(parse_grouping)
        .transpose()?;
    Ok(Grouping::new(kind, sub))
}

fn parse_scope(node: roxmltree::Node<'_, '_>) -> Result<ObjectTypes, SearchXmlError> {
    let mut t = ObjectTypes::NONE;
    for c in elements(node) {
        let n = c.tag_name().name();
        if n == "NONE" || n.starts_with("ALL") {
            continue;
        }
        let one =
            ObjectTypes::parse(n).ok_or_else(|| SearchXmlError(format!("unknown type '{n}'")))?;
        t = t.union(one);
    }
    Ok(t)
}

/// `SearchMarshalling.LoadSearch(XmlNode)`.
fn parse_search(node: roxmltree::Node<'_, '_>) -> Result<Search, SearchXmlError> {
    let uuid = node.attribute("uuid").map(str::to_owned);
    let name = node.attribute("name").unwrap_or_default().to_owned();
    let major: i32 = attr(node, "major_version")?
        .parse()
        .map_err(|_| SearchXmlError("bad version".into()))?;
    if major > MAJOR_VERSION {
        return err(format!(
            "a search version {major} was loaded; the most this console reads is {MAJOR_VERSION}"
        ));
    }
    let mut children = elements(node);
    let first = children
        .next()
        .ok_or_else(|| SearchXmlError("search without a query".into()))?;
    let query = if major <= 1 {
        Query::new(None, parse_filter(first)?)
    } else {
        if first.tag_name().name() != "Query" {
            return err("search without a query");
        }
        let mut scope = None;
        let mut filter = None;
        for c in elements(first) {
            if c.tag_name().name() == "QueryScope" {
                scope = Some(parse_scope(c)?);
            } else {
                filter = parse_filter(c)?;
            }
        }
        Query {
            scope: scope.ok_or_else(|| SearchXmlError("query without a scope".into()))?,
            filter,
        }
    };
    let mut search = Search::new(query, None, name);
    search.uuid = uuid;
    for c in children {
        match c.tag_name().name() {
            "columns" => {
                let mut columns = Vec::new();
                for col in elements(c) {
                    let name = attr(col, "name")?;
                    if name == "state" {
                        continue;
                    }
                    if let Ok(w) = attr(col, "width")?.parse::<i32>() {
                        columns.push((name.to_owned(), w.abs()));
                    }
                }
                search.columns = Some(columns);
            }
            "sorting" => {
                for s in elements(c) {
                    search.sorting.push(Sort {
                        column: attr(s, "column")?.to_owned(),
                        ascending: parse_bool(attr(s, "ascending")?),
                    });
                }
            }
            _ => search.grouping = Some(parse_grouping(c)?),
        }
    }
    Ok(search)
}

/// `SearchMarshalling.LoadSearch(string)`: one `<Search>` (a pool
/// `gui_config` value).
pub fn load_search(xml: &str) -> Result<Search, SearchXmlError> {
    if xml == "null" {
        return err("no search");
    }
    let doc = roxmltree::Document::parse(xml).map_err(|e| SearchXmlError(e.to_string()))?;
    parse_search(doc.root_element())
}

/// `SearchMarshalling.LoadSearches`: every `<Search>` (or old `<search>`)
/// of a `.xensearch` file.
pub fn load_searches(xml: &str) -> Result<Vec<Search>, SearchXmlError> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(xml, opts)
        .map_err(|e| SearchXmlError(e.to_string()))?;
    let mut out = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        let name = n.tag_name().name();
        if name == "Search" || name == "search" {
            out.push(parse_search(n)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_properties_have_the_original_names() {
        assert_eq!(ObjectTypes::ALL_EXC_FOLDERS.0, 2047);
        assert_eq!(ObjectTypes::parse("SR"), Some(ObjectTypes::REMOTE_SR));
        assert_eq!(
            PropertyName::parse("isNotFullyUpgraded"),
            Some(PropertyName::IsNotFullyUpgraded)
        );
        for (p, n) in PropertyName::ALL {
            assert_eq!(PropertyName::parse(n), Some(p));
        }
    }

    #[test]
    fn a_search_round_trips_through_the_xml() {
        let def = CustomFieldDefinition::new("Owner", CustomFieldType::String);
        let mut s = Search::new(
            Query::new(
                Some(ObjectTypes::VM.union(ObjectTypes::SERVER)),
                Some(Filter::Group {
                    kind: GroupKind::And,
                    subs: vec![
                        Filter::String {
                            property: PropertyName::Label,
                            query: "web & <db>".into(),
                            kind: StringMatch::Contains,
                            case_sensitive: false,
                        },
                        Filter::Enum {
                            property: PropertyName::VirtualisationStatus,
                            value: "IoDriversInstalled, ManagementInstalled".into(),
                            equals: false,
                        },
                        Filter::Recursive {
                            property: PropertyName::Folder,
                            sub: Box::new(Filter::String {
                                property: PropertyName::Uuid,
                                query: "/A".into(),
                                kind: StringMatch::ExactMatch,
                                case_sensitive: true,
                            }),
                        },
                        Filter::CustomField {
                            definition: def.clone(),
                            query: "ops".into(),
                            kind: StringMatch::ExactMatch,
                        },
                        Filter::Date {
                            property: PropertyName::StartTime,
                            day: crate::org::days_from_civil(2026, 11, 26),
                            kind: DateMatch::After,
                        },
                        Filter::Dummy,
                    ],
                }),
            ),
            Some(Grouping::new(
                GroupingKind::Object(PropertyName::Pool),
                Some(Grouping::new(GroupingKind::CustomField(def), None)),
            )),
            "My search",
        );
        s.uuid = Some("1234".into());
        s.columns = Some(vec![("name".into(), 250), ("CustomField:Owner".into(), 80)]);
        s.sorting = vec![Sort {
            column: "name".into(),
            ascending: true,
        }];
        let xml = search_to_xml(&s);
        assert!(xml.starts_with(
            "<Search uuid=\"1234\" name=\"My search\" major_version=\"2\" minor_version=\"0\"><Query><QueryScope><Server /><VM /></QueryScope><GroupQuery type=\"And\"><StringPropertyQuery property=\"label\" casesensitive=\"no\" query=\"web &amp; &lt;db&gt;\" type=\"contains\" />"
        ), "{xml}");
        assert!(xml.contains(
            "<DatePropertyQuery property=\"start_time\" query=\"20261126\" type=\"after\" />"
        ));
        assert!(xml.contains("<CustomFieldGrouping><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" /></CustomFieldGrouping>"));
        assert!(xml.ends_with("<columns><column name=\"name\" width=\"250\" /><column name=\"CustomField:Owner\" width=\"80\" /></columns><sorting><sort column=\"name\" ascending=\"yes\" /></sorting></Search>"));
        let back = load_search(&xml).unwrap();
        let mut expected = s.clone();
        if let Some(Filter::Group { subs, .. }) = &mut expected.query.filter {
            subs.pop(); // the dummy is not written
        }
        assert_eq!(back, expected);
        let file = searches_to_xml(&[s.clone()]);
        assert!(
            file.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<!DOCTYPE xensearch")
        );
        assert_eq!(load_searches(&file).unwrap(), vec![expected]);
    }

    #[test]
    fn unsupported_and_newer_searches_are_refused() {
        let newer = "<Search uuid=\"x\" name=\"n\" major_version=\"3\" minor_version=\"0\"><Query><QueryScope><VM /></QueryScope></Query></Search>";
        assert!(load_search(newer).is_err());
        let unknown = "<Search uuid=\"x\" name=\"n\" major_version=\"2\" minor_version=\"0\"><Query><QueryScope><VM /></QueryScope><MysteryQuery /></Query></Search>";
        assert!(load_search(unknown).unwrap_err().0.contains("MysteryQuery"));
        // A version 1 search is a bare filter.
        let old = "<Search uuid=\"x\" name=\"n\" major_version=\"1\" minor_version=\"0\"><BooleanQuery property=\"shared\" query=\"True\" /></Search>";
        let s = load_search(old).unwrap();
        assert_eq!(s.query.scope, ObjectTypes::ALL_EXC_FOLDERS);
        assert_eq!(
            s.query.filter,
            Some(Filter::Bool {
                property: PropertyName::Shared,
                query: true
            })
        );
    }
}
