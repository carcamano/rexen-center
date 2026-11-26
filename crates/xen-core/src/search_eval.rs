//! Running searches over the connected servers: the searchable objects
//! (`Cache.XenSearchableObjects`), their properties
//! (`PropertyAccessors`), filter matching (`QueryFilter.Match`), the
//! grouping algorithm (`GroupAlg.cs`) that builds both the Search tab's
//! groups and the navigation views' trees, the built-in searches
//! (`Search.InitDefaultSearches`), the organization views
//! (`OrganizationalView.cs`) and the searches made for a selection
//! (`Search.SearchFor`).
//!
//! Folders are shared by path across connections, as in the original;
//! every other object belongs to one connection.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use xapi::{Host, Network, Pool, Ref, Sr, Vdi, Vm, VmAppliance, VmPowerState, VmRecord};

use crate::backend::ConnectionId;
use crate::model::ConnectionModel;
use crate::objects::{self, natural_cmp};
use crate::org::{
    self, CustomFieldDefinition, CustomFieldType, CustomFieldValue, FolderIndex, OrgObject,
};
use crate::search::{
    DateMatch, Filter, GroupKind, Grouping, GroupingKind, IntMatch, ObjectTypes, PropertyName,
    Query, Search, StringMatch,
};

/// One connection searched.
pub struct Source<'a> {
    pub connection: ConnectionId,
    pub model: &'a ConnectionModel,
    /// Connected with its objects loaded; otherwise it shows as a
    /// disconnected server.
    pub connected: bool,
    /// The server's address, the name of a disconnected server.
    pub name: String,
}

/// A searchable object.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjKey {
    Pool(ConnectionId, Ref<Pool>),
    Host(ConnectionId, Ref<Host>),
    Vm(ConnectionId, Ref<Vm>),
    Sr(ConnectionId, Ref<Sr>),
    Vdi(ConnectionId, Ref<Vdi>),
    Network(ConnectionId, Ref<Network>),
    VApp(ConnectionId, Ref<VmAppliance>),
    /// A folder, by path (shared by every connection).
    Folder(String),
    /// A server that is not connected (or has no pool yet).
    DisconnectedServer(ConnectionId),
}

impl ObjKey {
    /// The connection of the object (none for a folder).
    pub fn connection(&self) -> Option<ConnectionId> {
        match self {
            Self::Pool(c, _)
            | Self::Host(c, _)
            | Self::Vm(c, _)
            | Self::Sr(c, _)
            | Self::Vdi(c, _)
            | Self::Network(c, _)
            | Self::VApp(c, _)
            | Self::DisconnectedServer(c) => Some(*c),
            Self::Folder(_) => None,
        }
    }

    /// The object as something that can be in a folder or carry tags.
    pub fn org_object(&self) -> Option<OrgObject> {
        Some(match self {
            Self::Pool(_, r) => OrgObject::Pool(r.clone()),
            Self::Host(_, r) => OrgObject::Host(r.clone()),
            Self::Vm(_, r) => OrgObject::Vm(r.clone()),
            Self::Sr(_, r) => OrgObject::Sr(r.clone()),
            Self::Vdi(_, r) => OrgObject::Vdi(r.clone()),
            Self::Network(_, r) => OrgObject::Network(r.clone()),
            _ => return None,
        })
    }

    /// From an organizable object of a connection.
    pub fn from_org(connection: ConnectionId, o: &OrgObject) -> Self {
        match o {
            OrgObject::Pool(r) => Self::Pool(connection, r.clone()),
            OrgObject::Host(r) => Self::Host(connection, r.clone()),
            OrgObject::Vm(r) => Self::Vm(connection, r.clone()),
            OrgObject::Sr(r) => Self::Sr(connection, r.clone()),
            OrgObject::Vdi(r) => Self::Vdi(connection, r.clone()),
            OrgObject::Network(r) => Self::Network(connection, r.clone()),
        }
    }
}

/// `VirtualizationStatus` flags.
pub mod tools {
    pub const NOT_INSTALLED: u32 = 0;
    pub const UNKNOWN: u32 = 1;
    pub const PV_DRIVERS_OUT_OF_DATE: u32 = 2;
    pub const IO_DRIVERS_INSTALLED: u32 = 4;
    pub const MANAGEMENT_INSTALLED: u32 = 8;
    /// The names, in value order, for `Enum.ToString` / `Enum.Parse`.
    pub const NAMES: [(u32, &str); 4] = [
        (UNKNOWN, "Unknown"),
        (PV_DRIVERS_OUT_OF_DATE, "PvDriversOutOfDate"),
        (IO_DRIVERS_INSTALLED, "IoDriversInstalled"),
        (MANAGEMENT_INSTALLED, "ManagementInstalled"),
    ];

    /// `Enum.ToString` of the flags.
    pub fn name(flags: u32) -> String {
        if flags == 0 {
            return "NotInstalled".to_owned();
        }
        NAMES
            .iter()
            .filter(|(f, _)| flags & f != 0)
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `Enum.Parse` of `A, B`.
    pub fn parse(s: &str) -> Option<u32> {
        let mut out = 0;
        for part in s.split(',').map(str::trim) {
            if part == "NotInstalled" {
                continue;
            }
            out |= NAMES.iter().find(|(_, n)| *n == part)?.0;
        }
        Some(out)
    }
}

/// `VM.HaRestartPriority` names.
pub const HA_PRIORITIES: [&str; 5] = [
    "AlwaysRestartHighPriority",
    "AlwaysRestart",
    "Restart",
    "BestEffort",
    "DoNotRestart",
];

/// `SR.SRTypes` names.
pub const SR_TYPES: [&str; 26] = [
    "local",
    "file",
    "ext",
    "ext4",
    "lvmoiscsi",
    "iso",
    "nfs",
    "lvm",
    "udev",
    "lvmofc",
    "lvmohba",
    "egenera",
    "egeneracd",
    "dummy",
    "unknown",
    "shm",
    "iscsi",
    "tmpfs",
    "xfs",
    "ebs",
    "rawhba",
    "smb",
    "lvmofcoe",
    "gfs2",
    "nutanix",
    "nutanixiso",
];

/// `vm_power_state` names.
pub const POWER_STATES: [&str; 5] = ["Halted", "Paused", "Running", "Suspended", "unknown"];

/// A property value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Str(String),
    Strs(Vec<String>),
    Obj(ObjKey),
    Objs(Vec<ObjKey>),
    Type(ObjectTypes),
    /// An enum, by its name (power state, HA restart priority, SR type).
    Enum(String),
    /// The tools flags.
    Tools(u32),
    Bool(bool),
    Int(i64),
    /// Unix seconds.
    Date(i64),
    /// IP addresses.
    Ips(Vec<String>),
}

/// The value a group is keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupValue {
    Obj(ObjKey),
    Str(String),
    Type(ObjectTypes),
    Enum(String),
    Tools(u32),
    Bool(bool),
    Date(i64),
    /// The first level of "all custom fields": a field name.
    FieldName(String),
    /// A custom field value.
    Field(CustomFieldValue),
}

/// What a non-object group node stands for (`GroupingTag`): the grouping
/// at that level, the group above it and the value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupTag {
    pub grouping: Grouping,
    pub parent: Option<GroupValue>,
    pub value: GroupValue,
}

/// A node of a grouped result.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub item: NodeItem,
    pub children: Vec<Node>,
}

/// An object, or a group that is not an object.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeItem {
    Object(ObjKey),
    Group(GroupTag),
}

impl Node {
    /// Every object in this subtree, depth first.
    pub fn objects(&self) -> Vec<&ObjKey> {
        let mut out = Vec::new();
        if let NodeItem::Object(o) = &self.item {
            out.push(o);
        }
        for c in &self.children {
            out.extend(c.objects());
        }
        out
    }
}

/// What a search looks at.
pub struct World<'a> {
    pub sources: Vec<Source<'a>>,
    /// View > Hidden Objects.
    pub show_hidden: bool,
    /// Unix seconds now (date filters).
    pub now: i64,
    folders: Vec<FolderIndex>,
    definitions: Vec<Vec<CustomFieldDefinition>>,
    all_definitions: Vec<CustomFieldDefinition>,
}

/// A real VM (`VM.IsRealVm`).
pub fn is_real_vm(vm: &VmRecord) -> bool {
    !vm.is_a_template && !vm.is_control_domain && !vm.is_a_snapshot
}

fn power_name(p: &VmPowerState) -> &'static str {
    match p {
        VmPowerState::Halted => "Halted",
        VmPowerState::Paused => "Paused",
        VmPowerState::Running => "Running",
        VmPowerState::Suspended => "Suspended",
        _ => "unknown",
    }
}

impl<'a> World<'a> {
    pub fn new(sources: Vec<Source<'a>>, show_hidden: bool, now: i64) -> Self {
        let folders = sources
            .iter()
            .map(|s| {
                if s.connected {
                    FolderIndex::build(s.model)
                } else {
                    FolderIndex::default()
                }
            })
            .collect();
        let definitions: Vec<Vec<CustomFieldDefinition>> =
            sources.iter().map(|s| org::definitions(s.model)).collect();
        let mut all_definitions: Vec<CustomFieldDefinition> = Vec::new();
        for d in definitions.iter().flatten() {
            if !all_definitions.contains(d) {
                all_definitions.push(d.clone());
            }
        }
        Self {
            sources,
            show_hidden,
            now,
            folders,
            definitions,
            all_definitions,
        }
    }

    fn source(&self, c: ConnectionId) -> Option<(usize, &Source<'a>)> {
        self.sources
            .iter()
            .enumerate()
            .find(|(_, s)| s.connection == c)
    }

    /// The model of a connection.
    pub fn model(&self, c: ConnectionId) -> Option<&'a ConnectionModel> {
        self.source(c).map(|(_, s)| s.model)
    }

    /// Every custom field defined on any connection
    /// (`CustomFieldsManager.GetCustomFields()`).
    pub fn definitions(&self) -> &[CustomFieldDefinition] {
        &self.all_definitions
    }

    /// The custom fields of one connection.
    pub fn definitions_of(&self, c: ConnectionId) -> &[CustomFieldDefinition] {
        self.source(c)
            .map(|(i, _)| self.definitions[i].as_slice())
            .unwrap_or_default()
    }

    /// `CustomFieldsManager.GetCustomFieldDefinition(name)`.
    pub fn definition(&self, name: &str) -> Option<&CustomFieldDefinition> {
        self.all_definitions.iter().find(|d| d.name == name)
    }

    /// Every folder path across connections.
    pub fn folder_paths(&self) -> BTreeSet<String> {
        self.folders
            .iter()
            .flat_map(|f| f.folders.iter().cloned())
            .collect()
    }

    /// The folder indexes with their connections.
    pub fn folder_indexes(&self) -> impl Iterator<Item = (ConnectionId, &FolderIndex)> {
        self.sources
            .iter()
            .zip(&self.folders)
            .map(|(s, f)| (s.connection, f))
    }

    /// Every tag in use on any connection, sorted.
    pub fn tags(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for s in self.sources.iter().filter(|s| s.connected) {
            for (t, _) in org::tag_counts(s.model) {
                out.insert(t);
            }
        }
        out.into_iter().collect()
    }

    /// `Cache.XenSearchableObjects` of every connection, plus a
    /// disconnected server for each connection without objects, minus
    /// what `GroupAlg.Hide` hides.
    pub fn objects(&self) -> Vec<ObjKey> {
        let mut out = Vec::new();
        for s in &self.sources {
            let c = s.connection;
            let m = s.model;
            if !s.connected || m.pools.is_empty() {
                out.push(ObjKey::DisconnectedServer(c));
                continue;
            }
            out.extend(m.vms.keys().map(|r| ObjKey::Vm(c, r.clone())));
            out.extend(m.vm_appliances.keys().map(|r| ObjKey::VApp(c, r.clone())));
            out.extend(m.hosts.keys().map(|r| ObjKey::Host(c, r.clone())));
            out.extend(m.srs.keys().map(|r| ObjKey::Sr(c, r.clone())));
            out.extend(m.networks.keys().map(|r| ObjKey::Network(c, r.clone())));
            out.extend(m.vdis.keys().map(|r| ObjKey::Vdi(c, r.clone())));
            if objects::pool_is_visible(m)
                && let Some((r, _)) = m.pool()
            {
                out.push(ObjKey::Pool(c, r.clone()));
            }
        }
        let mut seen = BTreeSet::new();
        for f in &self.folders {
            for path in &f.folders {
                if seen.insert(path.clone()) {
                    out.push(ObjKey::Folder(path.clone()));
                }
            }
        }
        out.retain(|o| !self.hidden(o));
        out
    }

    /// `GroupAlg.Hide`.
    fn hidden(&self, o: &ObjKey) -> bool {
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return matches!(o, ObjKey::Folder(p) if p == "/");
        };
        let live = |h: Option<&Ref<Host>>| {
            h.and_then(|h| m.hosts.get(h))
                .is_none_or(|h| objects::host_is_live(m, h))
        };
        match o {
            ObjKey::Vm(_, r) => m.vms.get(r).is_none_or(|vm| {
                vm.is_control_domain
                    || !objects::vm_is_shown(vm, self.show_hidden)
                    || !live(objects::vm_home(m, vm))
            }),
            ObjKey::Sr(_, r) => m.srs.get(r).is_none_or(|sr| {
                !objects::sr_is_shown(sr, self.show_hidden)
                    || objects::is_tools_sr(sr)
                    || !live(objects::sr_home(m, sr))
            }),
            ObjKey::Network(_, r) => m
                .networks
                .get(r)
                .is_none_or(|n| !objects::network_is_shown(m, n, self.show_hidden)),
            _ => false,
        }
    }

    /// `o.Show(showHidden)` (checked by `Query.Match`).
    fn shown(&self, o: &ObjKey) -> bool {
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return true;
        };
        match o {
            ObjKey::Vm(_, r) => m
                .vms
                .get(r)
                .is_some_and(|v| objects::vm_is_shown(v, self.show_hidden)),
            ObjKey::Sr(_, r) => m
                .srs
                .get(r)
                .is_some_and(|v| objects::sr_is_shown(v, self.show_hidden)),
            ObjKey::Vdi(_, r) => m
                .vdis
                .get(r)
                .is_some_and(|v| objects::vdi_is_shown(v, self.show_hidden)),
            ObjKey::Network(_, r) => m
                .networks
                .get(r)
                .is_some_and(|v| objects::network_is_shown(m, v, self.show_hidden)),
            _ => true,
        }
    }

    /// `PropertyNames.type`.
    pub fn object_type(&self, o: &ObjKey) -> Option<ObjectTypes> {
        if let ObjKey::Folder(_) = o {
            return Some(ObjectTypes::FOLDER);
        }
        if let ObjKey::DisconnectedServer(_) = o {
            return Some(ObjectTypes::DISCONNECTED_SERVER);
        }
        let m = self.model(o.connection()?)?;
        Some(match o {
            ObjKey::Vm(_, r) => {
                let vm = m.vms.get(r)?;
                match objects::vm_kind(vm) {
                    objects::VmKind::Snapshot => ObjectTypes::SNAPSHOT,
                    objects::VmKind::DefaultTemplate => ObjectTypes::DEFAULT_TEMPLATE,
                    objects::VmKind::UserTemplate => ObjectTypes::USER_TEMPLATE,
                    objects::VmKind::ControlDomain => return None,
                    _ => ObjectTypes::VM,
                }
            }
            ObjKey::VApp(..) => ObjectTypes::APPLIANCE,
            ObjKey::Host(c, _) => {
                if self.source(*c).is_some_and(|(_, s)| s.connected) {
                    ObjectTypes::SERVER
                } else {
                    ObjectTypes::DISCONNECTED_SERVER
                }
            }
            ObjKey::Pool(..) => ObjectTypes::POOL,
            ObjKey::Sr(_, r) => {
                if objects::is_local_sr(m.srs.get(r)?) {
                    ObjectTypes::LOCAL_SR
                } else {
                    ObjectTypes::REMOTE_SR
                }
            }
            ObjKey::Network(..) => ObjectTypes::NETWORK,
            ObjKey::Vdi(..) => ObjectTypes::VDI,
            _ => return None,
        })
    }

    /// `Helpers.GetName`.
    pub fn name(&self, o: &ObjKey) -> String {
        if let ObjKey::Folder(p) = o {
            return org::folder_name(p);
        }
        if let ObjKey::DisconnectedServer(c) = o {
            return self
                .source(*c)
                .map(|(_, s)| s.name.clone())
                .unwrap_or_default();
        }
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return String::new();
        };
        match o {
            ObjKey::Pool(_, r) => m.pools.get(r).map(|p| objects::pool_name(m, p)),
            ObjKey::Host(_, r) => m.hosts.get(r).map(|h| h.name_label.clone()),
            ObjKey::Vm(_, r) => m.vms.get(r).map(|v| v.name_label.clone()),
            ObjKey::Sr(_, r) => m.srs.get(r).map(|v| v.name_label.clone()),
            ObjKey::Vdi(_, r) => m.vdis.get(r).map(|v| objects::vdi_name(m, v)),
            ObjKey::Network(_, r) => m.networks.get(r).map(|v| objects::network_name(m, v)),
            ObjKey::VApp(_, r) => m.vm_appliances.get(r).map(|v| v.name_label.clone()),
            _ => None,
        }
        .unwrap_or_default()
    }

    /// `PropertyNames.description`.
    pub fn description(&self, o: &ObjKey) -> String {
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return String::new();
        };
        match o {
            ObjKey::Pool(_, r) => m.pools.get(r).map(|p| p.name_description.clone()),
            ObjKey::Host(_, r) => m.hosts.get(r).map(|h| h.name_description.clone()),
            ObjKey::Vm(_, r) => m.vms.get(r).map(|v| v.name_description.clone()),
            ObjKey::Sr(_, r) => m.srs.get(r).map(|v| v.name_description.clone()),
            ObjKey::Vdi(_, r) => m.vdis.get(r).map(|v| v.name_description.clone()),
            ObjKey::Network(_, r) => m.networks.get(r).map(|v| v.name_description.clone()),
            ObjKey::VApp(_, r) => m.vm_appliances.get(r).map(|v| v.name_description.clone()),
            _ => None,
        }
        .unwrap_or_default()
    }

    /// The object's UUID (a folder's path).
    pub fn uuid(&self, o: &ObjKey) -> Option<String> {
        if let ObjKey::Folder(p) = o {
            return Some(p.clone());
        }
        let m = self.model(o.connection()?)?;
        Some(match o {
            ObjKey::Pool(_, r) => m.pools.get(r)?.uuid.clone(),
            ObjKey::Host(_, r) => m.hosts.get(r)?.uuid.clone(),
            ObjKey::Vm(_, r) => m.vms.get(r)?.uuid.clone(),
            ObjKey::Sr(_, r) => m.srs.get(r)?.uuid.clone(),
            ObjKey::Vdi(_, r) => m.vdis.get(r)?.uuid.clone(),
            ObjKey::Network(_, r) => m.networks.get(r)?.uuid.clone(),
            ObjKey::VApp(_, r) => m.vm_appliances.get(r)?.uuid.clone(),
            _ => return None,
        })
    }

    /// The object with this UUID (or folder path), on any connection.
    pub fn find_uuid(&self, uuid: &str) -> Option<ObjKey> {
        if uuid.starts_with('/') {
            return Some(ObjKey::Folder(uuid.to_owned()));
        }
        for s in &self.sources {
            let c = s.connection;
            let m = s.model;
            macro_rules! find {
                ($map:ident, $variant:ident) => {
                    if let Some((r, _)) = m.$map.iter().find(|(_, x)| x.uuid == uuid) {
                        return Some(ObjKey::$variant(c, r.clone()));
                    }
                };
            }
            find!(pools, Pool);
            find!(hosts, Host);
            find!(vms, Vm);
            find!(srs, Sr);
            find!(vdis, Vdi);
            find!(networks, Network);
            find!(vm_appliances, VApp);
        }
        None
    }

    fn other_config(&self, o: &ObjKey) -> Option<&'a HashMap<String, String>> {
        let m = self.model(o.connection()?)?;
        o.org_object()?.other_config(m)
    }

    /// The custom field values of an object, in definition order.
    pub fn custom_fields(&self, o: &ObjKey) -> Vec<(CustomFieldDefinition, CustomFieldValue)> {
        match (o.connection(), self.other_config(o)) {
            (Some(c), Some(oc)) => org::values(self.definitions_of(c), oc),
            _ => Vec::new(),
        }
    }

    /// `CustomFieldsManager.GetCustomFieldValue`.
    pub fn custom_field(
        &self,
        o: &ObjKey,
        def: &CustomFieldDefinition,
    ) -> Option<CustomFieldValue> {
        self.other_config(o).and_then(|oc| org::value_of(oc, def))
    }

    /// The parent folder (`Folders.GetFolder`): the root for a top-level
    /// folder; for an object, its folder if it has one.
    pub fn folder(&self, o: &ObjKey) -> Option<String> {
        match o {
            ObjKey::Folder(p) => (p != "/").then(|| org::parent_of(p)).flatten(),
            _ => {
                let m = self.model(o.connection()?)?;
                org::folder_of(m, &o.org_object()?)
            }
        }
    }

    /// `Folders.GetAncestorFolders`: nearest first, root excluded.
    pub fn ancestor_folders(&self, o: &ObjKey) -> Vec<String> {
        self.folder(o)
            .map(|f| org::ancestors(&f))
            .unwrap_or_default()
    }

    /// The visible pool of a connection (`Helpers.GetPool`).
    fn pool_of(&self, c: ConnectionId) -> Option<ObjKey> {
        let m = self.model(c)?;
        if !objects::pool_is_visible(m) {
            return None;
        }
        m.pool().map(|(r, _)| ObjKey::Pool(c, r.clone()))
    }

    /// The tools flags (`VM.GetVirtualizationStatus`).
    pub fn tools_status(&self, m: &ConnectionModel, vm: &VmRecord) -> u32 {
        if vm.power_state != VmPowerState::Running || m.get(&vm.metrics).is_none() {
            return tools::UNKNOWN;
        }
        let gm = m.get(&vm.guest_metrics);
        let windows = gm.is_some_and(|g| {
            g.os_version
                .get("name")
                .is_some_and(|n| n.to_lowercase().contains("windows"))
        }) || vm.platform.get("viridian").map(String::as_str) == Some("true");
        if windows {
            let mut flags = tools::NOT_INSTALLED;
            if gm.is_some_and(|g| g.pv_drivers_detected) {
                flags |= tools::IO_DRIVERS_INSTALLED;
            }
            if gm.is_some_and(|g| {
                g.other
                    .get("feature-static-ip-setting")
                    .is_some_and(|v| v.parse::<i64>().unwrap_or(0) != 0)
            }) {
                flags |= tools::MANAGEMENT_INSTALLED;
            }
            return flags;
        }
        match gm {
            Some(g) if g.pv_drivers_version.contains_key("major") || g.pv_drivers_detected => {
                tools::IO_DRIVERS_INSTALLED | tools::MANAGEMENT_INSTALLED
            }
            _ => tools::NOT_INSTALLED,
        }
    }

    /// `PropertyAccessors.Get(property)(o)`.
    pub fn property(&self, o: &ObjKey, p: PropertyName) -> Value {
        use PropertyName as P;
        let c = o.connection();
        let m = c.and_then(|c| self.model(c));
        match p {
            P::Type => self.object_type(o).map_or(Value::None, Value::Type),
            P::Label => Value::Str(self.name(o)),
            P::Uuid => self.uuid(o).map_or(Value::None, Value::Str),
            P::Description => Value::Str(self.description(o)),
            P::Tags => match (m, o.org_object()) {
                (Some(m), Some(org)) => {
                    org.tags(m).map_or(Value::None, |t| Value::Strs(t.to_vec()))
                }
                _ => Value::None,
            },
            P::Pool => c
                .and_then(|c| self.pool_of(c))
                .map_or(Value::None, Value::Obj),
            P::Host => Value::Objs(self.hosts_of(o)),
            P::Vm => Value::Objs(self.vms_of(o)),
            P::Networks => Value::Objs(self.networks_of(o)),
            P::Storage => self.srs_of(o).map_or(Value::None, Value::Objs),
            P::Disks => Value::Objs(self.disks_of(o)),
            P::Folder => self
                .folder(o)
                .map_or(Value::None, |f| Value::Obj(ObjKey::Folder(f))),
            P::Folders => Value::Objs(
                self.ancestor_folders(o)
                    .into_iter()
                    .map(ObjKey::Folder)
                    .collect(),
            ),
            P::HasCustomFields => match (c, self.other_config(o)) {
                (Some(c), Some(oc)) => {
                    Value::Bool(org::has_custom_fields(self.definitions_of(c), oc))
                }
                _ => Value::Bool(false),
            },
            _ => {
                let Some(m) = m else {
                    return Value::None;
                };
                self.model_property(m, o, p)
            }
        }
    }

    fn model_property(&self, m: &ConnectionModel, o: &ObjKey, p: PropertyName) -> Value {
        use PropertyName as P;
        let real_vm = match o {
            ObjKey::Vm(_, r) => m.vms.get(r).filter(|v| is_real_vm(v)),
            _ => None,
        };
        match p {
            P::OsName => real_vm
                .and_then(|vm| objects::vm_os_name(m, vm))
                .map_or(Value::None, Value::Str),
            P::PowerState => real_vm.map_or(Value::None, |vm| {
                Value::Enum(power_name(&vm.power_state).to_owned())
            }),
            P::VirtualisationStatus => {
                real_vm.map_or(Value::None, |vm| Value::Tools(self.tools_status(m, vm)))
            }
            P::StartTime => real_vm
                .and_then(|vm| m.get(&vm.metrics))
                .and_then(|mt| mt.start_time.unix_seconds())
                .map_or(Value::None, Value::Date),
            P::ReadCachingEnabled => {
                real_vm.map_or(Value::None, |vm| Value::Bool(read_caching(m, vm)))
            }
            P::VendorDeviceState => {
                real_vm.map_or(Value::None, |vm| Value::Bool(vm.has_vendor_device))
            }
            P::Memory => real_vm
                .and_then(|vm| m.get(&vm.metrics))
                .map_or(Value::None, |mt| Value::Int(mt.memory_actual)),
            P::HaRestartPriority => match real_vm {
                Some(vm) if m.pool().is_some_and(|(_, p)| p.ha_enabled) => {
                    Value::Enum(ha_priority_name(&vm.ha_restart_priority).to_owned())
                }
                _ => Value::None,
            },
            P::Appliance => match o {
                ObjKey::VApp(..) => Value::Obj(o.clone()),
                ObjKey::Vm(c, _) => real_vm
                    .and_then(|vm| {
                        vm.appliance
                            .non_null()
                            .filter(|a| m.vm_appliances.contains_key(*a))
                    })
                    .map_or(Value::None, |a| Value::Obj(ObjKey::VApp(*c, a.clone()))),
                _ => Value::None,
            },
            P::InAnyAppliance => match o {
                ObjKey::VApp(..) => Value::Bool(true),
                _ => real_vm.map_or(Value::None, |vm| {
                    Value::Bool(
                        vm.appliance
                            .non_null()
                            .is_some_and(|a| m.vm_appliances.contains_key(a)),
                    )
                }),
            },
            P::Size => match o {
                ObjKey::Vdi(_, r) => m
                    .vdis
                    .get(r)
                    .map_or(Value::None, |v| Value::Int(v.virtual_size)),
                _ => Value::None,
            },
            P::Shared => match o {
                ObjKey::Sr(_, r) => m.srs.get(r).map_or(Value::None, |s| Value::Bool(s.shared)),
                ObjKey::Vdi(_, r) => m.vdis.get(r).map_or(Value::None, |v| {
                    let vms = v
                        .vbds
                        .iter()
                        .filter_map(|b| m.get(b))
                        .filter(|b| m.vms.contains_key(&b.vm))
                        .count();
                    Value::Bool(vms >= 2)
                }),
                _ => Value::None,
            },
            P::HaEnabled => match o {
                ObjKey::Pool(_, r) => m
                    .pools
                    .get(r)
                    .map_or(Value::None, |p| Value::Bool(p.ha_enabled)),
                _ => Value::None,
            },
            P::IsNotFullyUpgraded => match o {
                ObjKey::Pool(..) => {
                    let version = |h: &xapi::HostRecord| {
                        (
                            h.software_version.get("product_version").cloned(),
                            h.software_version.get("build_number").cloned(),
                        )
                    };
                    let coordinator = m.coordinator().map(|h| version(h));
                    Value::Bool(m.hosts.values().any(|h| Some(version(h)) != coordinator))
                }
                _ => Value::None,
            },
            P::SrType => match o {
                ObjKey::Sr(_, r) => m.srs.get(r).map_or(Value::None, |s| {
                    let t = if SR_TYPES.contains(&s.r#type.as_str()) {
                        s.r#type.clone()
                    } else {
                        "unknown".to_owned()
                    };
                    Value::Enum(t)
                }),
                _ => Value::None,
            },
            P::IpAddress => self.ips_of(m, o).map_or(Value::None, Value::Ips),
            P::Uptime => match o {
                ObjKey::Vm(..) => real_vm
                    .filter(|vm| vm.power_state == VmPowerState::Running)
                    .and_then(|vm| m.get(&vm.metrics))
                    .and_then(|mt| mt.start_time.unix_seconds())
                    .map_or(Value::None, |t| Value::Int(self.now - t)),
                ObjKey::Host(_, r) => m
                    .hosts
                    .get(r)
                    .and_then(|h| {
                        h.other_config
                            .get("boot_time")
                            .and_then(|v| v.trim().parse::<f64>().ok())
                    })
                    .map_or(Value::None, |t| Value::Int(self.now - t as i64)),
                _ => Value::None,
            },
            _ => Value::None,
        }
    }

    /// `HostProperty`.
    fn hosts_of(&self, o: &ObjKey) -> Vec<ObjKey> {
        let Some(c) = o.connection() else {
            return Vec::new();
        };
        let Some(m) = self.model(c) else {
            return Vec::new();
        };
        let host = |r: &Ref<Host>| ObjKey::Host(c, r.clone());
        if self.pool_of(c).is_none() {
            return m.hosts.keys().map(host).collect();
        }
        match o {
            ObjKey::Vm(_, r) => m
                .vms
                .get(r)
                .and_then(|vm| objects::vm_home(m, vm))
                .map(host)
                .into_iter()
                .collect(),
            ObjKey::Sr(_, r) => m
                .srs
                .get(r)
                .and_then(|sr| objects::sr_home(m, sr))
                .map(host)
                .into_iter()
                .collect(),
            ObjKey::Network(_, r) => match m.networks.get(r) {
                Some(n) if n.pifs.is_empty() => m.hosts.keys().map(host).collect(),
                _ => Vec::new(),
            },
            ObjKey::Host(_, r) => vec![host(r)],
            ObjKey::Vdi(_, r) => m
                .vdis
                .get(r)
                .and_then(|v| m.srs.get(&v.sr))
                .and_then(|sr| objects::sr_home(m, sr))
                .map(host)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `VMProperty`: real VMs only.
    fn vms_of(&self, o: &ObjKey) -> Vec<ObjKey> {
        let Some(c) = o.connection() else {
            return Vec::new();
        };
        let Some(m) = self.model(c) else {
            return Vec::new();
        };
        let mut vms: Vec<Ref<Vm>> = match o {
            ObjKey::Pool(..) => m.vms.keys().cloned().collect(),
            ObjKey::Host(_, r) => m
                .hosts
                .get(r)
                .map(|h| h.resident_vms.clone())
                .unwrap_or_default(),
            ObjKey::Sr(_, r) => m
                .srs
                .get(r)
                .map(|sr| {
                    sr.vdis
                        .iter()
                        .filter_map(|v| m.get(v))
                        .flat_map(|v| v.vbds.iter().filter_map(|b| m.get(b)).map(|b| b.vm.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            ObjKey::Network(_, r) => m
                .networks
                .get(r)
                .map(|n| {
                    n.vifs
                        .iter()
                        .filter_map(|v| m.get(v))
                        .map(|v| v.vm.clone())
                        .collect()
                })
                .unwrap_or_default(),
            ObjKey::Vdi(_, r) => m
                .vdis
                .get(r)
                .map(|v| {
                    v.vbds
                        .iter()
                        .filter_map(|b| m.get(b))
                        .map(|b| b.vm.clone())
                        .collect()
                })
                .unwrap_or_default(),
            ObjKey::Vm(_, r) => match m.vms.get(r) {
                Some(vm) if vm.is_a_snapshot => vec![vm.snapshot_of.clone()],
                Some(_) => vec![r.clone()],
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        vms.retain(|r| m.vms.get(r).is_some_and(|v| is_real_vm(v)));
        vms.into_iter().map(|r| ObjKey::Vm(c, r)).collect()
    }

    /// `NetworksProperty`.
    fn networks_of(&self, o: &ObjKey) -> Vec<ObjKey> {
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return Vec::new();
        };
        match o {
            ObjKey::Vm(c, r) => m
                .vms
                .get(r)
                .filter(|v| is_real_vm(v))
                .map(|vm| {
                    vm.vifs
                        .iter()
                        .filter_map(|v| m.get(v))
                        .filter(|v| m.networks.contains_key(&v.network))
                        .map(|v| ObjKey::Network(*c, v.network.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            ObjKey::Network(..) => vec![o.clone()],
            _ => Vec::new(),
        }
    }

    /// `StorageProperty` (`None` for a VDI whose SR is gone).
    fn srs_of(&self, o: &ObjKey) -> Option<Vec<ObjKey>> {
        let m = self.model(o.connection()?)?;
        Some(match o {
            ObjKey::Vm(c, r) => {
                let mut out: Vec<ObjKey> = Vec::new();
                if let Some(vm) = m.vms.get(r).filter(|v| is_real_vm(v)) {
                    for vbd in vm.vbds.iter().filter_map(|b| m.get(b)) {
                        if let Some(vdi) = m.get(&vbd.vdi)
                            && m.srs.contains_key(&vdi.sr)
                        {
                            let k = ObjKey::Sr(*c, vdi.sr.clone());
                            if !out.contains(&k) {
                                out.push(k);
                            }
                        }
                    }
                }
                out
            }
            ObjKey::Sr(..) => vec![o.clone()],
            ObjKey::Vdi(c, r) => {
                let sr = &m.vdis.get(r)?.sr;
                if !m.srs.contains_key(sr) {
                    return None;
                }
                vec![ObjKey::Sr(*c, sr.clone())]
            }
            _ => Vec::new(),
        })
    }

    /// `DisksProperty`.
    fn disks_of(&self, o: &ObjKey) -> Vec<ObjKey> {
        let Some(m) = o.connection().and_then(|c| self.model(c)) else {
            return Vec::new();
        };
        match o {
            ObjKey::Vdi(..) => vec![o.clone()],
            ObjKey::Vm(c, r) => {
                let mut out: Vec<ObjKey> = Vec::new();
                if let Some(vm) = m.vms.get(r).filter(|v| is_real_vm(v)) {
                    for vbd in vm.vbds.iter().filter_map(|b| m.get(b)) {
                        if m.vdis.contains_key(&vbd.vdi) {
                            let k = ObjKey::Vdi(*c, vbd.vdi.clone());
                            if !out.contains(&k) {
                                out.push(k);
                            }
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// `IPAddressProperty` (`None` when there are none, CA-28300).
    fn ips_of(&self, m: &ConnectionModel, o: &ObjKey) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        match o {
            ObjKey::Vm(_, r) => {
                let vm = m.vms.get(r).filter(|v| is_real_vm(v))?;
                m.get(&vm.guest_metrics)?;
                for vif in vm.vifs.iter().filter_map(|v| m.get(v)) {
                    for ip in objects::vif_ip_addresses(m, vm, &vif.device) {
                        if !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                }
            }
            ObjKey::Host(_, r) => {
                for pif in m.hosts.get(r)?.pifs.iter().filter_map(|p| m.get(p)) {
                    if !pif.ip.is_empty() {
                        out.push(pif.ip.clone());
                    }
                }
            }
            ObjKey::Sr(_, r) => {
                let sr = m.srs.get(r)?;
                for pbd in sr.pbds.iter().filter_map(|p| m.get(p)) {
                    if let Some(t) = pbd
                        .device_config
                        .get("target")
                        .or_else(|| pbd.device_config.get("server"))
                    {
                        if t.parse::<std::net::IpAddr>().is_ok() && !out.contains(t) {
                            out.push(t.clone());
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
        (!out.is_empty()).then_some(out)
    }

    // ---- matching ---------------------------------------------------------------

    /// `Query.Match`.
    pub fn query_matches(&self, q: &Query, o: &ObjKey) -> bool {
        self.object_type(o).is_some_and(|t| q.scope.contains(t))
            && self.shown(o)
            && q.filter
                .as_ref()
                .is_none_or(|f| self.matches(f, o) != Some(false))
    }

    /// `QueryFilter.Match`: `None` is "don't know" (a dummy filter).
    pub fn matches(&self, f: &Filter, o: &ObjKey) -> Option<bool> {
        match f {
            Filter::Dummy => None,
            Filter::Group { kind, subs } => {
                let mut result = None;
                for s in subs {
                    let b = self.matches(s, o);
                    match kind {
                        GroupKind::And => match b {
                            Some(false) => return Some(false),
                            Some(true) => result = Some(true),
                            None => {}
                        },
                        GroupKind::Or => match b {
                            Some(true) => return Some(true),
                            Some(false) => result = Some(false),
                            None => {}
                        },
                        GroupKind::Nor => match b {
                            Some(true) => return Some(false),
                            Some(false) => result = Some(true),
                            None => {}
                        },
                    }
                }
                result
            }
            Filter::CustomField {
                definition,
                query,
                kind,
            } => {
                let Some(CustomFieldValue::Text(v)) = self.custom_field(o, definition) else {
                    return Some(false);
                };
                Some(kind.matches(&v, &query.to_lowercase(), false))
            }
            Filter::CustomFieldDate {
                definition,
                day,
                kind,
            } => {
                let Some(CustomFieldValue::Date(t)) = self.custom_field(o, definition) else {
                    return Some(false);
                };
                Some(kind.matches(t.div_euclid(86_400), *day, self.now.div_euclid(86_400)))
            }
            other => {
                let p = other.property()?;
                let v = self.property(o, p);
                self.match_value(other, &v)
            }
        }
    }

    fn match_value(&self, f: &Filter, v: &Value) -> Option<bool> {
        Some(match (f, v) {
            (
                Filter::String {
                    query,
                    kind,
                    case_sensitive,
                    ..
                },
                Value::Str(s),
            ) => kind.matches(s, query, *case_sensitive),
            (Filter::Date { day, kind, .. }, Value::Date(t)) => {
                kind.matches(t.div_euclid(86_400), *day, self.now.div_euclid(86_400))
            }
            (
                Filter::Enum {
                    property,
                    value,
                    equals,
                },
                v,
            ) => {
                let eq = match (property, v) {
                    (PropertyName::Type, Value::Type(t)) => ObjectTypes::parse(value) == Some(*t),
                    (PropertyName::VirtualisationStatus, Value::Tools(t)) => {
                        tools::parse(value) == Some(*t)
                    }
                    (_, Value::Enum(e)) => e == value,
                    _ => return Some(false),
                };
                eq == *equals
            }
            (Filter::Value { query, equals, .. }, Value::Str(s)) => (s == query) == *equals,
            (Filter::Object { uuid, equals, .. }, Value::Obj(o)) => {
                if uuid == "invalid" || self.find_uuid(uuid).is_none() {
                    return Some(false);
                }
                (self.uuid(o).as_deref() == Some(uuid)) == *equals
            }
            (Filter::ListContainsObject { uuid, contains, .. }, Value::Objs(list)) => {
                list.iter().any(|o| self.uuid(o).as_deref() == Some(uuid)) == *contains
            }
            (
                Filter::ListContainsName {
                    query,
                    kind,
                    contains,
                    ..
                },
                Value::Objs(list),
            ) => {
                list.iter()
                    .any(|o| kind.matches(&self.name(o), query, false))
                    == *contains
            }
            (
                Filter::ListContainsString {
                    query, contains, ..
                },
                Value::Strs(list),
            ) => list.iter().any(|s| s == query) == *contains,
            (Filter::ListEmpty { empty, .. }, Value::Strs(l)) => l.is_empty() == *empty,
            (Filter::ListEmpty { empty, .. }, Value::Objs(l)) => l.is_empty() == *empty,
            (Filter::ListEmpty { empty, .. }, Value::Ips(l)) => l.is_empty() == *empty,
            (
                Filter::Ip {
                    address, contains, ..
                },
                Value::Ips(list),
            ) => list.iter().any(|ip| ip_matches(ip, address)) == *contains,
            (Filter::Bool { query, .. }, Value::Bool(b)) => b == query,
            (Filter::Null { query, .. }, v) => matches!(v, Value::None) == *query,
            (Filter::Int { query, kind, .. }, Value::Int(i)) => match kind {
                IntMatch::ExactMatch => i == query,
                IntMatch::Gt => i > query,
                IntMatch::Lt => i < query,
            },
            (Filter::Recursive { sub, .. }, Value::Obj(o)) => return self.matches(sub, o),
            (Filter::RecursiveList { sub, .. }, Value::Objs(list)) => {
                let (mut seen_false, mut seen_null) = (false, false);
                for o in list {
                    match self.matches(sub, o) {
                        Some(true) => return Some(true),
                        Some(false) => seen_false = true,
                        None => seen_null = true,
                    }
                }
                if seen_false {
                    false
                } else if seen_null {
                    return None;
                } else {
                    false
                }
            }
            _ => false,
        })
    }

    // ---- grouping ---------------------------------------------------------------

    /// `Group.GetGrouped(search)` populated: the top-level nodes.
    pub fn group(&self, search: &Search) -> Vec<Node> {
        let grouping = search.effective_grouping().cloned();
        let mut root = Group::for_grouping(None, grouping.as_ref());
        for o in self.objects() {
            if self.query_matches(&search.query, &o) {
                root.add(self, search, o);
            }
        }
        root.populate(self, search, None)
    }

    /// `Group.Compare`: the search's sorting, then object kind (folders,
    /// pools, servers, VMs, then by class), then name.
    pub fn compare_objects(&self, a: &ObjKey, b: &ObjKey, search: Option<&Search>) -> Ordering {
        if let Some(s) = search {
            for sort in &s.sorting {
                let r = self.compare_by_column(a, b, &sort.column);
                let r = if sort.ascending { r } else { r.reverse() };
                if r != Ordering::Equal {
                    return r;
                }
            }
        }
        self.type_rank(a)
            .cmp(&self.type_rank(b))
            .then_with(|| natural_cmp(&self.name(a), &self.name(b)))
            .then_with(|| a.cmp(b))
    }

    /// `Sort.Compare` for the columns this console can sort without
    /// metrics: the name and custom fields (others compare equal here and
    /// are sorted by the grid).
    fn compare_by_column(&self, a: &ObjKey, b: &ObjKey, column: &str) -> Ordering {
        if column == "name" {
            return natural_cmp(&self.name(a), &self.name(b));
        }
        if let Some(name) = column.strip_prefix(org::CUSTOM_FIELD_COLUMN_PREFIX)
            && let Some(def) = self.definition(name)
        {
            return self.custom_field(a, def).cmp(&self.custom_field(b, def));
        }
        Ordering::Equal
    }

    /// `Group.TypeOf`.
    fn type_rank(&self, o: &ObjKey) -> String {
        match o {
            ObjKey::Folder(_) => "10".into(),
            ObjKey::Pool(..) => "20".into(),
            ObjKey::Host(..) | ObjKey::DisconnectedServer(_) => "30".into(),
            ObjKey::Vm(c, r)
                if self
                    .model(*c)
                    .and_then(|m| m.vms.get(r))
                    .is_some_and(|v| is_real_vm(v)) =>
            {
                "40".into()
            }
            ObjKey::Network(..) => "XenAPI.Network".into(),
            ObjKey::Sr(..) => "XenAPI.SR".into(),
            ObjKey::Vdi(..) => "XenAPI.VDI".into(),
            ObjKey::Vm(..) => "XenAPI.VM".into(),
            ObjKey::VApp(..) => "XenAPI.VM_appliance".into(),
        }
    }

    fn compare_values(&self, a: &GroupValue, b: &GroupValue, search: &Search) -> Ordering {
        match (a, b) {
            (GroupValue::Obj(x), GroupValue::Obj(y)) => self.compare_objects(x, y, Some(search)),
            (GroupValue::Obj(_), _) => Ordering::Greater,
            (_, GroupValue::Obj(_)) => Ordering::Less,
            (GroupValue::Str(x), GroupValue::Str(y))
            | (GroupValue::FieldName(x), GroupValue::FieldName(y))
            | (GroupValue::Enum(x), GroupValue::Enum(y)) => {
                // Enums compare by declaration order.
                let rank = |s: &str| {
                    POWER_STATES
                        .iter()
                        .chain(HA_PRIORITIES.iter())
                        .chain(SR_TYPES.iter())
                        .position(|n| *n == s)
                };
                match (a, rank(x), rank(y)) {
                    (GroupValue::Enum(_), Some(i), Some(j)) => i.cmp(&j),
                    _ => x
                        .to_lowercase()
                        .cmp(&y.to_lowercase())
                        .then_with(|| x.cmp(y)),
                }
            }
            (GroupValue::Type(x), GroupValue::Type(y)) => x.cmp(y),
            (GroupValue::Tools(x), GroupValue::Tools(y)) => x.cmp(y),
            (GroupValue::Bool(x), GroupValue::Bool(y)) => x.cmp(y),
            (GroupValue::Date(x), GroupValue::Date(y)) => x.cmp(y),
            (GroupValue::Field(x), GroupValue::Field(y)) => x.cmp(y),
            _ => natural_cmp(&format!("{a:?}"), &format!("{b:?}")),
        }
    }

    /// `Grouping.GetGroup(o)`: none, one value, or several (a list, or a
    /// path of values for folders and custom fields).
    fn group_values(&self, kind: &GroupingKind, o: &ObjKey) -> GroupOf {
        match kind {
            GroupingKind::Folder => {
                let mut folders = self.ancestor_folders(o);
                if folders.is_empty() {
                    return GroupOf::None;
                }
                folders.reverse();
                GroupOf::Paths(vec![
                    folders
                        .into_iter()
                        .map(|f| GroupValue::Obj(ObjKey::Folder(f)))
                        .collect(),
                ])
            }
            GroupingKind::AllCustomFields => {
                let fields = self.custom_fields(o);
                if fields.is_empty() {
                    return GroupOf::None;
                }
                GroupOf::Paths(
                    fields
                        .into_iter()
                        .map(|(d, v)| vec![GroupValue::FieldName(d.name), GroupValue::Field(v)])
                        .collect(),
                )
            }
            GroupingKind::CustomField(def) => match self.custom_field(o, def) {
                Some(v) => GroupOf::One(GroupValue::Field(v)),
                None => GroupOf::None,
            },
            GroupingKind::Property(p) | GroupingKind::Bool(p) | GroupingKind::Object(p) => {
                match self.property(o, *p) {
                    Value::None => GroupOf::None,
                    Value::Str(s) => GroupOf::One(GroupValue::Str(s)),
                    Value::Strs(l) => GroupOf::Many(l.into_iter().map(GroupValue::Str).collect()),
                    Value::Obj(x) => GroupOf::One(GroupValue::Obj(x)),
                    Value::Objs(l) => GroupOf::Many(l.into_iter().map(GroupValue::Obj).collect()),
                    Value::Type(t) => GroupOf::One(GroupValue::Type(t)),
                    Value::Enum(e) => GroupOf::One(GroupValue::Enum(e)),
                    Value::Tools(t) => GroupOf::One(GroupValue::Tools(t)),
                    Value::Bool(b) => GroupOf::One(GroupValue::Bool(b)),
                    Value::Date(d) => GroupOf::One(GroupValue::Date(d)),
                    Value::Int(i) => GroupOf::One(GroupValue::Str(i.to_string())),
                    Value::Ips(l) => GroupOf::Many(l.into_iter().map(GroupValue::Str).collect()),
                }
            }
        }
    }

    /// `Grouping.BelongsAsGroupNotMember`.
    fn belongs_as_group(&self, kind: &GroupingKind, o: &ObjKey) -> bool {
        match kind {
            GroupingKind::Folder => {
                matches!(o, ObjKey::Folder(p) if org::parent_of(p).as_deref() == Some("/"))
            }
            GroupingKind::Object(p) => {
                let class_matches = matches!(
                    (p, o),
                    (PropertyName::Pool, ObjKey::Pool(..))
                        | (PropertyName::Host, ObjKey::Host(..))
                        | (PropertyName::Networks, ObjKey::Network(..))
                        | (PropertyName::Storage, ObjKey::Sr(..))
                        | (PropertyName::Disks, ObjKey::Vdi(..))
                        | (PropertyName::Appliance, ObjKey::VApp(..))
                        | (PropertyName::Vm, ObjKey::Vm(..))
                );
                if !class_matches {
                    return false;
                }
                if let ObjKey::Vm(c, r) = o {
                    return self
                        .model(*c)
                        .and_then(|m| m.vms.get(r))
                        .is_some_and(|v| is_real_vm(v));
                }
                true
            }
            _ => false,
        }
    }
}

/// `VM.ReadCachingEnabled`: a disk with read caching on the server the VM
/// runs on.
fn read_caching(m: &ConnectionModel, vm: &VmRecord) -> bool {
    let Some(host) = m.hosts.get(&vm.resident_on) else {
        return false;
    };
    let key = format!("read-caching-enabled-on-{}", host.uuid);
    vm.vbds
        .iter()
        .filter_map(|b| m.get(b))
        .filter_map(|b| m.get(&b.vdi))
        .any(|v| v.sm_config.get(&key).map(String::as_str) == Some("true"))
}

fn ha_priority_name(p: &str) -> &'static str {
    match p {
        "restart" | "0" | "1" | "2" | "3" | "always_restart" | "always_restart_high_priority" => {
            "Restart"
        }
        "best-effort" => "BestEffort",
        _ => "DoNotRestart",
    }
}

/// `IPAddressQuery`: an address, or a network `a.b.c.d/n`.
fn ip_matches(ip: &str, spec: &str) -> bool {
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    match spec.split_once('/') {
        None => spec.parse::<std::net::IpAddr>().is_ok_and(|s| s == addr),
        Some((net, bits)) => {
            let (Ok(net), Ok(bits)) = (net.parse::<std::net::IpAddr>(), bits.parse::<u32>()) else {
                return false;
            };
            match (net, addr) {
                (std::net::IpAddr::V4(n), std::net::IpAddr::V4(a)) if bits <= 32 => {
                    let mask = if bits == 0 {
                        0
                    } else {
                        u32::MAX << (32 - bits)
                    };
                    u32::from(n) & mask == u32::from(a) & mask
                }
                (std::net::IpAddr::V6(n), std::net::IpAddr::V6(a)) if bits <= 128 => {
                    let mask = if bits == 0 {
                        0
                    } else {
                        u128::MAX << (128 - bits)
                    };
                    u128::from(n) & mask == u128::from(a) & mask
                }
                _ => false,
            }
        }
    }
}

/// What `Grouping.GetGroup` returned.
enum GroupOf {
    None,
    One(GroupValue),
    /// A list: the object is in each group.
    Many(Vec<GroupValue>),
    /// Arrays: each a chain of nested groups.
    Paths(Vec<Vec<GroupValue>>),
}

/// A group under construction (`NodeGroup`, `FolderGroup`, `LeafGroup`).
enum Group {
    Node {
        grouping: Grouping,
        grouped: Vec<(GroupValue, Grouping, Group)>,
        ungrouped: Option<Box<Group>>,
    },
    Folder {
        grouping: Grouping,
        grouped: Vec<(GroupValue, Grouping, Group)>,
        ungrouped: Vec<ObjKey>,
    },
    Leaf(Vec<ObjKey>),
}

impl Group {
    /// `Group.GetGroupFor(grouping, search, subgrouping)`.
    fn for_grouping(parent: Option<&Grouping>, sub: Option<&Grouping>) -> Self {
        if let Some(p) = parent
            && p.kind == GroupingKind::Folder
        {
            return Self::Folder {
                grouping: p.clone(),
                grouped: Vec::new(),
                ungrouped: Vec::new(),
            };
        }
        match sub {
            None => Self::Leaf(Vec::new()),
            Some(g) => Self::Node {
                grouping: g.clone(),
                grouped: Vec::new(),
                ungrouped: None,
            },
        }
    }

    fn find_or_add<'g>(
        grouped: &'g mut Vec<(GroupValue, Grouping, Group)>,
        grouping: &Grouping,
        value: GroupValue,
        sub: Option<&Grouping>,
    ) -> &'g mut Group {
        let i = match grouped
            .iter()
            .position(|(v, g, _)| *v == value && g == grouping)
        {
            Some(i) => i,
            None => {
                grouped.push((
                    value,
                    grouping.clone(),
                    Group::for_grouping(Some(grouping), sub),
                ));
                grouped.len() - 1
            }
        };
        &mut grouped[i].2
    }

    fn add(&mut self, world: &World<'_>, search: &Search, o: ObjKey) {
        match self {
            Self::Leaf(items) => {
                if matches!(o, ObjKey::Folder(_)) && items.contains(&o) {
                    return;
                }
                items.push(o);
            }
            Self::Folder {
                grouping,
                grouped,
                ungrouped,
            } => {
                if let ObjKey::Folder(_) = &o {
                    let g = grouping.clone();
                    Group::find_or_add(grouped, &g, GroupValue::Obj(o), g.sub.as_deref());
                } else {
                    ungrouped.push(o);
                }
            }
            Self::Node {
                grouping,
                grouped,
                ungrouped,
            } => {
                let g = grouping.clone();
                if world.belongs_as_group(&g.kind, &o) {
                    Group::find_or_add(grouped, &g, GroupValue::Obj(o), g.sub.as_deref());
                    return;
                }
                let add_ungrouped = |ungrouped: &mut Option<Box<Group>>, o: ObjKey| {
                    ungrouped
                        .get_or_insert_with(|| {
                            Box::new(Group::for_grouping(Some(&g), g.sub.as_deref()))
                        })
                        .add(world, search, o);
                };
                match world.group_values(&g.kind, &o) {
                    GroupOf::None => add_ungrouped(ungrouped, o),
                    GroupOf::One(v) => Self::add_grouped(world, search, grouped, &g, o, vec![v]),
                    GroupOf::Many(vs) if vs.is_empty() => add_ungrouped(ungrouped, o),
                    GroupOf::Many(vs) => {
                        for v in vs {
                            Self::add_grouped(world, search, grouped, &g, o.clone(), vec![v]);
                        }
                    }
                    GroupOf::Paths(paths) => {
                        for path in paths {
                            Self::add_grouped(world, search, grouped, &g, o.clone(), path);
                        }
                    }
                }
            }
        }
    }

    /// `NodeGroup.AddGrouped`; a path of more than one value nests.
    fn add_grouped(
        world: &World<'_>,
        search: &Search,
        grouped: &mut Vec<(GroupValue, Grouping, Group)>,
        grouping: &Grouping,
        o: ObjKey,
        path: Vec<GroupValue>,
    ) {
        if let [GroupValue::Obj(group_object)] = path.as_slice()
            && let GroupingKind::Property(p) | GroupingKind::Object(p) = &grouping.kind
            && let Some(sub) = search
                .query
                .filter
                .as_ref()
                .and_then(|f| f.sub_query_for(*p))
            && world.matches(&sub, group_object) == Some(false)
        {
            return;
        }
        let n = path.len();
        let mut current: &mut Vec<(GroupValue, Grouping, Group)> = grouped;
        let mut target: Option<&mut Group> = None;
        for (i, v) in path.into_iter().enumerate() {
            let sub = if i == n - 1 {
                grouping.sub.as_deref()
            } else {
                Some(grouping)
            };
            let g = Group::find_or_add(current, grouping, v, sub);
            if i == n - 1 {
                target = Some(g);
                break;
            }
            current = match g {
                Group::Node { grouped, .. } | Group::Folder { grouped, .. } => grouped,
                Group::Leaf(_) => return,
            };
        }
        if let Some(t) = target {
            t.add(world, search, o);
        }
    }

    /// `AbstractNodeGroup.Populate` / `LeafGroup.Populate`.
    fn populate(
        &self,
        world: &World<'_>,
        search: &Search,
        parent: Option<&GroupValue>,
    ) -> Vec<Node> {
        match self {
            Self::Leaf(items) => {
                let mut items = items.clone();
                items.sort_by(|a, b| world.compare_objects(a, b, Some(search)));
                items
                    .into_iter()
                    .map(|o| Node {
                        item: NodeItem::Object(o),
                        children: Vec::new(),
                    })
                    .collect()
            }
            Self::Node {
                grouped, ungrouped, ..
            } => {
                let mut keys: Vec<Key<'_>> = grouped
                    .iter()
                    .map(|(v, g, grp)| Key::Group(v, g, grp))
                    .collect();
                if let Some(u) = ungrouped {
                    keys.extend(u.next_level());
                }
                populate_keys(world, search, parent, keys)
            }
            Self::Folder {
                grouped, ungrouped, ..
            } => {
                let mut keys: Vec<Key<'_>> = grouped
                    .iter()
                    .map(|(v, g, grp)| Key::Group(v, g, grp))
                    .collect();
                keys.extend(ungrouped.iter().map(Key::Item));
                populate_keys(world, search, parent, keys)
            }
        }
    }

    /// `GetNextLevel`: the keys an ungrouped part adds to its parent.
    fn next_level(&self) -> Vec<Key<'_>> {
        match self {
            Self::Leaf(items) => items.iter().map(Key::Item).collect(),
            Self::Node {
                grouped, ungrouped, ..
            } => {
                let mut keys: Vec<Key<'_>> = grouped
                    .iter()
                    .map(|(v, g, grp)| Key::Group(v, g, grp))
                    .collect();
                if let Some(u) = ungrouped {
                    keys.extend(u.next_level());
                }
                keys
            }
            Self::Folder {
                grouped, ungrouped, ..
            } => {
                let mut keys: Vec<Key<'_>> = grouped
                    .iter()
                    .map(|(v, g, grp)| Key::Group(v, g, grp))
                    .collect();
                keys.extend(ungrouped.iter().map(Key::Item));
                keys
            }
        }
    }
}

enum Key<'g> {
    Group(&'g GroupValue, &'g Grouping, &'g Group),
    Item(&'g ObjKey),
}

fn populate_keys(
    world: &World<'_>,
    search: &Search,
    parent: Option<&GroupValue>,
    mut keys: Vec<Key<'_>>,
) -> Vec<Node> {
    keys.sort_by(|a, b| {
        let va = match a {
            Key::Group(v, ..) => (*v).clone(),
            Key::Item(o) => GroupValue::Obj((*o).clone()),
        };
        let vb = match b {
            Key::Group(v, ..) => (*v).clone(),
            Key::Item(o) => GroupValue::Obj((*o).clone()),
        };
        world.compare_values(&va, &vb, search)
    });
    keys.into_iter()
        .map(|k| match k {
            Key::Item(o) => Node {
                item: NodeItem::Object(o.clone()),
                children: Vec::new(),
            },
            Key::Group(v, g, grp) => Node {
                item: match v {
                    GroupValue::Obj(o) => NodeItem::Object(o.clone()),
                    other => NodeItem::Group(GroupTag {
                        grouping: g.clone(),
                        parent: parent.cloned(),
                        value: other.clone(),
                    }),
                },
                children: grp.populate(world, search, Some(v)),
            },
        })
        .collect()
}

// ---- searches for groups and selections -------------------------------------------

/// `Grouping.GetSubquery(parent, value)`.
pub fn group_subquery(world: &World<'_>, tag: &GroupTag) -> Option<Filter> {
    match (&tag.grouping.kind, &tag.value) {
        (GroupingKind::Property(PropertyName::Tags), GroupValue::Str(t)) => {
            Some(Filter::ListContainsString {
                property: PropertyName::Tags,
                query: t.clone(),
                contains: true,
            })
        }
        (GroupingKind::Property(p), GroupValue::Str(s)) => Some(Filter::Value {
            property: *p,
            query: s.clone(),
            equals: true,
        }),
        (GroupingKind::Property(p), GroupValue::Type(t)) => Some(Filter::Enum {
            property: *p,
            value: t.name().unwrap_or_default().to_owned(),
            equals: true,
        }),
        (GroupingKind::Property(p), GroupValue::Enum(e)) => Some(Filter::Enum {
            property: *p,
            value: e.clone(),
            equals: true,
        }),
        (GroupingKind::Property(p), GroupValue::Tools(t)) => Some(Filter::Enum {
            property: *p,
            value: tools::name(*t),
            equals: true,
        }),
        (GroupingKind::CustomField(def), v) => {
            custom_field_subquery(world.definition(&def.name)?, v)
        }
        (GroupingKind::AllCustomFields, GroupValue::FieldName(name)) => {
            let def = world.definition(name)?;
            match def.kind {
                CustomFieldType::String => Some(Filter::CustomField {
                    definition: def.clone(),
                    query: String::new(),
                    kind: StringMatch::ExactMatch,
                }),
                CustomFieldType::Date => Some(Filter::CustomFieldDate {
                    definition: def.clone(),
                    day: world.now.div_euclid(86_400),
                    kind: DateMatch::Exact,
                }),
            }
        }
        (GroupingKind::AllCustomFields, v) => match &tag.parent {
            Some(GroupValue::FieldName(name)) => custom_field_subquery(world.definition(name)?, v),
            _ => None,
        },
        _ => None,
    }
}

fn custom_field_subquery(def: &CustomFieldDefinition, v: &GroupValue) -> Option<Filter> {
    match (def.kind, v) {
        (CustomFieldType::String, GroupValue::Field(CustomFieldValue::Text(s))) => {
            Some(Filter::CustomField {
                definition: def.clone(),
                query: s.clone(),
                kind: StringMatch::ExactMatch,
            })
        }
        (CustomFieldType::Date, GroupValue::Field(CustomFieldValue::Date(t))) => {
            Some(Filter::CustomFieldDate {
                definition: def.clone(),
                day: t.div_euclid(86_400),
                kind: DateMatch::Exact,
            })
        }
        _ => None,
    }
}

/// `Grouping.GetSubgrouping(value)`.
pub fn group_subgrouping(world: &World<'_>, tag: &GroupTag) -> Option<Grouping> {
    match (&tag.grouping.kind, &tag.value) {
        (GroupingKind::AllCustomFields, GroupValue::FieldName(name)) => world
            .definition(name)
            .map(|d| Grouping::new(GroupingKind::CustomField(d.clone()), None)),
        _ => None,
    }
}

/// `Search.SearchForNonVappGroup` (and the folder and vApp variants, by
/// `scope`): the objects of one group.
pub fn search_for_group(
    world: &World<'_>,
    tag: &GroupTag,
    scope: ObjectTypes,
    name: String,
) -> Search {
    Search::new(
        Query::new(Some(scope), group_subquery(world, tag)),
        group_subgrouping(world, tag),
        name,
    )
}

/// `Search.FullQueryFor`: the tree's search box (name or description
/// contains each word, or an address equals it).
pub fn full_query_for(text: &str) -> Filter {
    let mut subs = Vec::new();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        subs.push(Filter::String {
            property: PropertyName::Label,
            query: word.to_owned(),
            kind: StringMatch::Contains,
            case_sensitive: false,
        });
        subs.push(Filter::String {
            property: PropertyName::Description,
            query: word.to_owned(),
            kind: StringMatch::Contains,
            case_sensitive: false,
        });
        if word.parse::<std::net::IpAddr>().is_ok() {
            subs.push(Filter::Ip {
                property: PropertyName::IpAddress,
                address: word.to_owned(),
                contains: true,
            });
        }
    }
    if subs.is_empty() {
        subs.push(Filter::String {
            property: PropertyName::Label,
            query: String::new(),
            kind: StringMatch::Contains,
            case_sensitive: false,
        });
    }
    Filter::Group {
        kind: GroupKind::Or,
        subs,
    }
}

/// `Search.DefaultObjectTypes` plus custom templates
/// (`GetOverviewScope`).
pub fn overview_scope() -> ObjectTypes {
    ObjectTypes::DISCONNECTED_SERVER
        .union(ObjectTypes::SERVER)
        .union(ObjectTypes::VM)
        .union(ObjectTypes::REMOTE_SR)
        .union(ObjectTypes::USER_TEMPLATE)
}

fn uuid_is(uuid: &str) -> Filter {
    Filter::String {
        property: PropertyName::Uuid,
        query: uuid.to_owned(),
        kind: StringMatch::ExactMatch,
        case_sensitive: true,
    }
}

/// The name a `SearchFor` search gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTitle {
    /// "All Servers, VMs, Custom Templates & Remote SRs".
    Overview,
    /// "{name}: Overview" (a pool or a server).
    ObjectOverview(String),
    /// A folder's name.
    Folder(String),
}

/// `Search.SearchFor(object)`: a pool's or server's overview, a folder's
/// contents, or everything.
pub fn search_for(world: &World<'_>, object: Option<&ObjKey>) -> (Search, SearchTitle) {
    let scope = overview_scope();
    let host_grouping = Grouping::new(GroupingKind::Object(PropertyName::Host), None);
    match object {
        Some(ObjKey::Host(..)) => {
            let uuid = object.and_then(|o| world.uuid(o)).unwrap_or_default();
            let name = object.map(|o| world.name(o)).unwrap_or_default();
            (
                Search::new(
                    Query::new(
                        Some(scope),
                        Some(Filter::RecursiveList {
                            property: PropertyName::Host,
                            sub: Box::new(uuid_is(&uuid)),
                        }),
                    ),
                    Some(host_grouping),
                    String::new(),
                ),
                SearchTitle::ObjectOverview(name),
            )
        }
        Some(ObjKey::Pool(..)) => {
            let uuid = object.and_then(|o| world.uuid(o)).unwrap_or_default();
            let name = object.map(|o| world.name(o)).unwrap_or_default();
            (
                Search::new(
                    Query::new(
                        Some(scope),
                        Some(Filter::Recursive {
                            property: PropertyName::Pool,
                            sub: Box::new(uuid_is(&uuid)),
                        }),
                    ),
                    Some(Grouping::new(
                        GroupingKind::Object(PropertyName::Pool),
                        Some(host_grouping),
                    )),
                    String::new(),
                ),
                SearchTitle::ObjectOverview(name),
            )
        }
        Some(ObjKey::Folder(path)) => {
            let s = search_for_folder(path);
            let name = s.name.clone();
            (s, SearchTitle::Folder(name))
        }
        _ => (
            Search::new(
                Query::new(Some(scope), None),
                Some(Grouping::new(
                    GroupingKind::Object(PropertyName::Pool),
                    Some(Grouping::new(
                        GroupingKind::Object(PropertyName::Host),
                        Some(Grouping::new(
                            GroupingKind::Object(PropertyName::Dockervm),
                            None,
                        )),
                    )),
                )),
                String::new(),
            ),
            SearchTitle::Overview,
        ),
    }
}

/// `Search.SearchForTag` (named by the caller: "Objects with tag 'x'").
pub fn search_for_tag(tag: &str) -> Search {
    Search::new(
        Query::new(
            None,
            Some(Filter::ListContainsString {
                property: PropertyName::Tags,
                query: tag.to_owned(),
                contains: true,
            }),
        ),
        None,
        String::new(),
    )
}

/// `Search.SearchForFolder`: a folder's contents, named after it.
pub fn search_for_folder(path: &str) -> Search {
    let name = org::folder_name(path);
    Search::new(
        Query::new(
            Some(ObjectTypes::ALL_INC_FOLDERS),
            Some(Filter::Recursive {
                property: PropertyName::Folder,
                sub: Box::new(uuid_is(path)),
            }),
        ),
        Some(Grouping::new(GroupingKind::Folder, None)),
        name,
    )
}

/// The navigation views (`NavigationPane.NavigationMode`, without
/// Infrastructure, saved searches and notifications).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrgView {
    /// "Objects by Type".
    Objects,
    Tags,
    Folders,
    CustomFields,
    VApps,
}

impl OrgView {
    /// The view's filter (`OrganizationalView.Query`).
    pub fn filter(self) -> Option<Filter> {
        match self {
            Self::Objects => None,
            Self::Tags => Some(Filter::ListEmpty {
                property: PropertyName::Tags,
                empty: false,
            }),
            Self::Folders => Some(Filter::Null {
                property: PropertyName::Folder,
                query: false,
            }),
            Self::CustomFields => Some(Filter::Bool {
                property: PropertyName::HasCustomFields,
                query: true,
            }),
            Self::VApps => Some(Filter::Bool {
                property: PropertyName::InAnyAppliance,
                query: true,
            }),
        }
    }

    /// The view's grouping (`OrganizationalView.Grouping`).
    pub fn grouping(self) -> Grouping {
        Grouping::new(
            match self {
                Self::Objects => GroupingKind::Property(PropertyName::Type),
                Self::Tags => GroupingKind::Property(PropertyName::Tags),
                Self::Folders => GroupingKind::Folder,
                Self::CustomFields => GroupingKind::AllCustomFields,
                Self::VApps => GroupingKind::Object(PropertyName::Appliance),
            },
            None,
        )
    }

    /// The navigation pane's own search for the view
    /// (`Search.SearchForAllTypes`, `SearchForTags`…): what is offered
    /// before the view adds its filter and grouping.
    pub fn pane_search(self) -> Search {
        let scope = match self {
            Self::Objects => ObjectTypes::ALL_EXC_FOLDERS,
            _ => ObjectTypes::ALL_INC_FOLDERS,
        };
        Search::new(Query::new(Some(scope), self.filter()), None, String::new())
    }

    /// `OrganizationalView.OrgViewSearch`: the tree's search, narrowed by
    /// the tree's search box.
    pub fn tree_search(self, text: &str) -> Search {
        let base = self.pane_search();
        let base = if text.trim().is_empty() {
            base
        } else {
            base.add_filter(Some(full_query_for(text)))
        };
        let scope = if self == Self::Folders {
            base.query.scope
        } else {
            base.query.scope.minus(ObjectTypes::FOLDER)
        };
        let filter = match (base.query.filter.clone(), self.filter()) {
            (None, f) => f,
            (f, None) => f,
            (Some(a), Some(b)) => Some(Filter::Group {
                kind: GroupKind::And,
                subs: vec![a, b],
            }),
        };
        Search::new(
            Query { scope, filter },
            Some(self.grouping()),
            String::new(),
        )
    }

    /// The Search tab's search when the view's root (or, outside Objects,
    /// anything in the view) is selected.
    pub fn root_search(self) -> Search {
        let scope = match self {
            Self::Folders => ObjectTypes::ALL_INC_FOLDERS,
            Self::VApps => ObjectTypes::VM,
            _ => ObjectTypes::ALL_EXC_FOLDERS,
        };
        Search::new(
            Query::new(Some(scope), self.filter()),
            Some(self.grouping()),
            String::new(),
        )
    }
}

/// The built-in searches (`Search.InitDefaultSearches`), with their UUIDs
/// and a key for their names (the UI translates them).
pub fn default_searches() -> Vec<(&'static str, Search)> {
    let vm = || Some(ObjectTypes::VM);
    let mk = |uuid: &str, query: Query, grouping: Grouping| {
        let mut s = Search::new(query, Some(grouping), String::new());
        s.uuid = Some(uuid.to_owned());
        s.default_search = true;
        s
    };
    vec![
        (
            "vms_by_os",
            mk(
                "dead-beef-1234-vmsbyos",
                Query::new(vm(), None),
                Grouping::new(GroupingKind::Property(PropertyName::OsName), None),
            ),
        ),
        (
            "vms_without_tools",
            mk(
                "dead-beef-1234-vmswotools",
                Query::new(
                    vm(),
                    Some(Filter::Group {
                        kind: GroupKind::And,
                        subs: vec![
                            Filter::Enum {
                                property: PropertyName::PowerState,
                                value: "Running".into(),
                                equals: true,
                            },
                            Filter::Enum {
                                property: PropertyName::VirtualisationStatus,
                                value: "IoDriversInstalled, ManagementInstalled".into(),
                                equals: false,
                            },
                        ],
                    }),
                ),
                Grouping::new(
                    GroupingKind::Property(PropertyName::VirtualisationStatus),
                    None,
                ),
            ),
        ),
        (
            "vms_by_power_state",
            mk(
                "dead-beef-1234-vmsbyps",
                Query::new(vm(), None),
                Grouping::new(GroupingKind::Property(PropertyName::PowerState), None),
            ),
        ),
        (
            "vms_by_network",
            mk(
                "dead-beef-1234-vmsbynet",
                Query::new(vm(), None),
                Grouping::new(GroupingKind::Object(PropertyName::Networks), None),
            ),
        ),
        (
            "vms_by_vapp",
            mk(
                "dead-beef-1234-vmsbyappliance",
                Query::new(
                    vm(),
                    Some(Filter::Bool {
                        property: PropertyName::InAnyAppliance,
                        query: true,
                    }),
                ),
                Grouping::new(GroupingKind::Object(PropertyName::Appliance), None),
            ),
        ),
        (
            "objects_by_tag",
            mk(
                "dead-beef-1234-objbytags",
                Query::new(
                    None,
                    Some(Filter::ListEmpty {
                        property: PropertyName::Tags,
                        empty: false,
                    }),
                ),
                Grouping::new(GroupingKind::Property(PropertyName::Tags), None),
            ),
        ),
        (
            "snapshots_by_vm",
            mk(
                "dead-beef-1234-snapshotsbyvm",
                Query::new(Some(ObjectTypes::SNAPSHOT.union(ObjectTypes::VM)), None),
                Grouping::new(
                    GroupingKind::Object(PropertyName::Pool),
                    Some(Grouping::new(
                        GroupingKind::Object(PropertyName::Host),
                        Some(Grouping::new(GroupingKind::Object(PropertyName::Vm), None)),
                    )),
                ),
            ),
        ),
    ]
}

/// The searches saved on the connections' pools
/// (`Search.SynchroniseSearches`): `gui_config["xensearch.Search-<uuid>"]`
/// values that read, with their connection; one per UUID.
pub fn saved_searches(world: &World<'_>) -> Vec<(ConnectionId, Search)> {
    let mut out: Vec<(ConnectionId, Search)> = Vec::new();
    for s in world.sources.iter().filter(|s| s.connected) {
        for pool in s.model.pools.values() {
            let mut keys: Vec<&String> = pool
                .gui_config
                .keys()
                .filter(|k| k.starts_with(org::SEARCH_KEY_PREFIX))
                .collect();
            keys.sort();
            for key in keys {
                let Ok(mut search) = crate::search::load_search(&pool.gui_config[key]) else {
                    continue;
                };
                let uuid = key[org::SEARCH_KEY_PREFIX.len()..].to_owned();
                search.uuid = Some(uuid.clone());
                out.retain(|(_, x)| x.uuid.as_deref() != Some(uuid.as_str()));
                out.push((s.connection, search));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> ConnectionModel {
        ConnectionModel::from_database(&json!({
            "pool": {"OpaqueRef:pool": {"uuid": "pool-u", "name_label": "Lab", "master": "OpaqueRef:h1",
                "other_config": {"EMPTY_FOLDERS": "/Empty"},
                "gui_config": {
                    "XenCenter.CustomFields": "<CustomFieldDefinitions><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" /><CustomFieldDefinition name=\"Due\" type=\"Date\" defaultValue=\"\" /></CustomFieldDefinitions>",
                    "xensearch.Search-s1": "<Search uuid=\"s1\" name=\"Web VMs\" major_version=\"2\" minor_version=\"0\"><Query><QueryScope><VM /></QueryScope><StringPropertyQuery property=\"label\" casesensitive=\"no\" query=\"web\" type=\"contains\" /></Query><sorting /></Search>"
                }, "tags": ["site-a"]}},
            "host": {"OpaqueRef:h1": {"uuid": "h1-u", "name_label": "h1", "metrics": "OpaqueRef:hm1", "other_config": {}, "tags": [], "resident_VMs": ["OpaqueRef:web1"]},
                     "OpaqueRef:h2": {"uuid": "h2-u", "name_label": "h2", "metrics": "OpaqueRef:hm2", "other_config": {}, "tags": []}},
            "host_metrics": {"OpaqueRef:hm1": {"uuid": "hm1", "live": true}, "OpaqueRef:hm2": {"uuid": "hm2", "live": true}},
            "VM": {
                "OpaqueRef:web1": {"uuid": "web1-u", "name_label": "web1", "power_state": "Running", "resident_on": "OpaqueRef:h1",
                    "tags": ["prod", "web"], "other_config": {"folder": "/Prod/Web", "XenCenter.CustomFields.Owner": "ops"}},
                "OpaqueRef:db1": {"uuid": "db1-u", "name_label": "db1", "power_state": "Halted", "tags": ["prod"],
                    "other_config": {"folder": "/Prod", "XenCenter.CustomFields.Due": "2026-11-26 10:00:00Z"}, "appliance": "OpaqueRef:app"},
                "OpaqueRef:tpl": {"uuid": "tpl-u", "name_label": "tpl", "is_a_template": true, "power_state": "Halted", "tags": [], "other_config": {}},
                "OpaqueRef:dom0": {"uuid": "dom0-u", "name_label": "Control domain", "is_control_domain": true, "power_state": "Running", "tags": [], "other_config": {}}
            },
            "VM_appliance": {"OpaqueRef:app": {"uuid": "app-u", "name_label": "App", "VMs": ["OpaqueRef:db1"]}},
            "network": {"OpaqueRef:net": {"uuid": "net-u", "name_label": "LAN", "tags": ["infra"], "other_config": {}, "PIFs": [], "VIFs": []}},
        }))
    }

    fn world(m: &ConnectionModel) -> World<'_> {
        World::new(
            vec![Source {
                connection: 1,
                model: m,
                connected: true,
                name: "lab".into(),
            }],
            false,
            crate::org::days_from_civil(2026, 11, 26) * 86_400 + 3600,
        )
    }

    fn names(w: &World<'_>, nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| match &n.item {
                NodeItem::Object(o) => w.name(o),
                NodeItem::Group(t) => format!("[{:?}]", t.value),
            })
            .collect()
    }

    #[test]
    fn objects_and_types() {
        let m = fixture();
        let w = world(&m);
        let objs = w.objects();
        assert!(objs.contains(&ObjKey::Pool(1, Ref::new("OpaqueRef:pool"))));
        assert!(!objs.contains(&ObjKey::Vm(1, Ref::new("OpaqueRef:dom0"))));
        assert!(objs.contains(&ObjKey::Folder("/Prod/Web".into())));
        assert!(objs.contains(&ObjKey::Folder("/Empty".into())));
        assert!(!objs.contains(&ObjKey::Folder("/".into())));
        assert_eq!(
            w.object_type(&ObjKey::Vm(1, Ref::new("OpaqueRef:tpl"))),
            Some(ObjectTypes::USER_TEMPLATE)
        );
    }

    #[test]
    fn filters_follow_the_original() {
        let m = fixture();
        let w = world(&m);
        let web = ObjKey::Vm(1, Ref::new("OpaqueRef:web1"));
        let db = ObjKey::Vm(1, Ref::new("OpaqueRef:db1"));
        let tag = |t: &str| Filter::ListContainsString {
            property: PropertyName::Tags,
            query: t.into(),
            contains: true,
        };
        assert_eq!(w.matches(&tag("web"), &web), Some(true));
        assert_eq!(w.matches(&tag("web"), &db), Some(false));
        let in_prod = Filter::RecursiveList {
            property: PropertyName::Folders,
            sub: Box::new(uuid_is("/Prod")),
        };
        assert_eq!(w.matches(&in_prod, &web), Some(true));
        let parent_prod = Filter::Recursive {
            property: PropertyName::Folder,
            sub: Box::new(uuid_is("/Prod")),
        };
        assert_eq!(w.matches(&parent_prod, &web), Some(false));
        assert_eq!(w.matches(&parent_prod, &db), Some(true));
        let running = Filter::Enum {
            property: PropertyName::PowerState,
            value: "Running".into(),
            equals: true,
        };
        assert_eq!(w.matches(&running, &web), Some(true));
        assert_eq!(
            w.matches(&running, &ObjKey::Network(1, Ref::new("OpaqueRef:net"))),
            Some(false)
        );
        let owner = CustomFieldDefinition::new("Owner", CustomFieldType::String);
        let cf = Filter::CustomField {
            definition: owner,
            query: "OPS".into(),
            kind: StringMatch::ExactMatch,
        };
        assert_eq!(w.matches(&cf, &web), Some(true));
        let due = CustomFieldDefinition::new("Due", CustomFieldType::Date);
        let today = Filter::CustomFieldDate {
            definition: due,
            day: 0,
            kind: DateMatch::Today,
        };
        assert_eq!(w.matches(&today, &db), Some(true));
        let vapp = Filter::Bool {
            property: PropertyName::InAnyAppliance,
            query: true,
        };
        assert_eq!(w.matches(&vapp, &db), Some(true));
        assert_eq!(w.matches(&vapp, &web), Some(false));
        // A dummy in an And is ignored; alone it is "don't know".
        let and = Filter::Group {
            kind: GroupKind::And,
            subs: vec![Filter::Dummy, tag("prod")],
        };
        assert_eq!(w.matches(&and, &db), Some(true));
        assert!(w.query_matches(&Query::new(Some(ObjectTypes::VM), Some(Filter::Dummy)), &db));
        // Standalone pool hides nothing here (pool has a name): pool property set.
        assert_eq!(
            w.property(&web, PropertyName::Pool),
            Value::Obj(ObjKey::Pool(1, Ref::new("OpaqueRef:pool")))
        );
    }

    #[test]
    fn views_group_as_the_original() {
        let m = fixture();
        let w = world(&m);
        let folders = w.group(&OrgView::Folders.tree_search(""));
        assert_eq!(names(&w, &folders), vec!["Empty", "Prod"]);
        let prod = &folders[1];
        assert_eq!(names(&w, &prod.children), vec!["Web", "db1"]);
        assert_eq!(names(&w, &prod.children[0].children), vec!["web1"]);
        let tags = w.group(&OrgView::Tags.tree_search(""));
        assert_eq!(
            names(&w, &tags),
            vec![
                "[Str(\"infra\")]",
                "[Str(\"prod\")]",
                "[Str(\"site-a\")]",
                "[Str(\"web\")]"
            ]
        );
        assert_eq!(names(&w, &tags[1].children), vec!["db1", "web1"]);
        let fields = w.group(&OrgView::CustomFields.tree_search(""));
        assert_eq!(
            names(&w, &fields),
            vec!["[FieldName(\"Due\")]", "[FieldName(\"Owner\")]"]
        );
        assert_eq!(
            names(&w, &fields[1].children),
            vec![format!(
                "[{:?}]",
                GroupValue::Field(CustomFieldValue::Text("ops".into()))
            )]
        );
        let vapps = w.group(&OrgView::VApps.tree_search(""));
        assert_eq!(names(&w, &vapps), vec!["App"]);
        assert_eq!(names(&w, &vapps[0].children), vec!["db1"]);
        let objects = w.group(&OrgView::Objects.tree_search(""));
        let types: Vec<String> = names(&w, &objects);
        assert_eq!(
            types[0],
            format!("[{:?}]", GroupValue::Type(ObjectTypes::POOL))
        );
        // The search box narrows the tree.
        let narrowed = w.group(&OrgView::Tags.tree_search("db"));
        assert_eq!(names(&w, &narrowed), vec!["[Str(\"prod\")]"]);
    }

    #[test]
    fn default_and_saved_searches() {
        let m = fixture();
        let w = world(&m);
        let defaults = default_searches();
        assert_eq!(defaults.len(), 7);
        let by_power = &defaults
            .iter()
            .find(|(k, _)| *k == "vms_by_power_state")
            .unwrap()
            .1;
        let groups = w.group(by_power);
        assert_eq!(
            names(&w, &groups),
            vec!["[Enum(\"Halted\")]", "[Enum(\"Running\")]"]
        );
        let saved = saved_searches(&w);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].1.name, "Web VMs");
        assert_eq!(names(&w, &w.group(&saved[0].1)), vec!["web1"]);
        let (pool_search, _) = search_for(&w, Some(&ObjKey::Pool(1, Ref::new("OpaqueRef:pool"))));
        let g = w.group(&pool_search);
        assert_eq!(names(&w, &g), vec!["Lab"]);
        // Custom templates are in the overview scope.
        assert_eq!(names(&w, &g[0].children), vec!["h1", "h2", "db1", "tpl"]);
        assert_eq!(names(&w, &g[0].children[0].children), vec!["web1"]);
        let tag = GroupTag {
            grouping: OrgView::Tags.grouping(),
            parent: None,
            value: GroupValue::Str("prod".into()),
        };
        let s = search_for_group(&w, &tag, ObjectTypes::ALL_EXC_FOLDERS, "prod".into());
        assert_eq!(names(&w, &w.group(&s)), vec!["db1", "web1"]);
    }

    #[test]
    fn addresses_match_networks() {
        assert!(ip_matches("192.0.2.20", "192.0.2.0/24"));
        assert!(!ip_matches("192.0.3.20", "192.0.2.0/24"));
        assert!(ip_matches("192.0.2.20", "192.0.2.20"));
        assert_eq!(
            tools::name(tools::IO_DRIVERS_INSTALLED | tools::MANAGEMENT_INSTALLED),
            "IoDriversInstalled, ManagementInstalled"
        );
        assert_eq!(
            tools::parse("IoDriversInstalled, ManagementInstalled"),
            Some(12)
        );
    }
}
