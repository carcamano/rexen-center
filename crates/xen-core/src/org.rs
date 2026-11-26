//! Folders, tags and custom fields, stored exactly as XenCenter stores
//! them so both consoles see each other's data, and the original's
//! actions on them as plans of API calls ([`OrgCall`]) that
//! [`crate::org_run`] makes.
//!
//! **Folders** (`XenModel/Folders.cs`, `Actions/Folders/FolderAction.cs`):
//! an object's folder is the path in its `other_config["folder"]`
//! (`/Production/Web`); pools, hosts, VMs, SRs, VDIs and networks can be
//! in folders. A folder exists while something is in it or under it, or
//! while it is listed in the pool's `other_config["EMPTY_FOLDERS"]`
//! (paths joined by `;`). Moving the last object out of a folder adds the
//! folder to that list, so it stays; putting an object into a listed
//! folder takes the folder out of the list again. Every value write is
//! `remove_from_other_config` then `add_to_other_config`, as the
//! original's `Helpers.SetOtherConfig`.
//!
//! **Tags** (`XenModel/Tags.cs`): the object's `tags` field, changed one
//! tag at a time with `<class>.add_tags` / `remove_tags` (skipped when
//! there is nothing to do).
//!
//! **Custom fields** (`XenModel/CustomFields`): the definitions are XML in
//! the pool's `gui_config["XenCenter.CustomFields"]`
//! (`<CustomFieldDefinitions><CustomFieldDefinition name="…" type="String"
//! defaultValue="" /></CustomFieldDefinitions>`, written with
//! `remove_from_gui_config` then `add_to_gui_config`); an object's value
//! is `other_config["XenCenter.CustomFields.<name>"]`, the text itself or,
//! for a date, `yyyy-MM-dd HH:mm:ssZ` in UTC (.NET's `"u"` format).
//!
//! Plans are computed from the connection's cached model; the folder
//! planner replays the original's reads of the cache as each call lands
//! (which folder is about to become empty, what the empty-folder list
//! holds), so the calls and their order are the original's.

use std::collections::{BTreeSet, HashMap};

use xapi::{Host, Network, Pool, Ref, Sr, Vdi, Vm};

use crate::model::ConnectionModel;

/// `Folders.FOLDER`: the `other_config` key of an object's folder.
pub const FOLDER_KEY: &str = "folder";
/// `Folders.EMPTY_FOLDERS`: the pool's `other_config` key listing empty
/// folders.
pub const EMPTY_FOLDERS_KEY: &str = "EMPTY_FOLDERS";
/// `Folders.EMPTY_FOLDERS_SEPARATOR`.
pub const EMPTY_FOLDERS_SEPARATOR: &str = ";";
/// `CustomFieldsManager.CUSTOM_FIELD_BASE_KEY`: the pool's `gui_config`
/// key of the definitions, and the prefix of the value keys.
pub const CUSTOM_FIELDS_KEY: &str = "XenCenter.CustomFields";
/// `CustomFieldsManager.CUSTOM_FIELD`: the prefix of a custom field's
/// column name in a saved search.
pub const CUSTOM_FIELD_COLUMN_PREFIX: &str = "CustomField:";
/// `BrandManager.ExtensionSearch + ".Search-"`: the prefix of a saved
/// search's `gui_config` key (followed by its UUID).
pub const SEARCH_KEY_PREFIX: &str = "xensearch.Search-";

/// An object that can be in a folder and carry tags and custom fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OrgObject {
    Pool(Ref<Pool>),
    Host(Ref<Host>),
    Vm(Ref<Vm>),
    Sr(Ref<Sr>),
    Vdi(Ref<Vdi>),
    Network(Ref<Network>),
}

impl OrgObject {
    /// The API class name (`VM.add_tags`, `SR.add_to_other_config`…).
    pub fn class(&self) -> &'static str {
        match self {
            Self::Pool(_) => "pool",
            Self::Host(_) => "host",
            Self::Vm(_) => "VM",
            Self::Sr(_) => "SR",
            Self::Vdi(_) => "VDI",
            Self::Network(_) => "network",
        }
    }

    /// The raw opaque reference.
    pub fn opaque(&self) -> &str {
        match self {
            Self::Pool(r) => r.as_str(),
            Self::Host(r) => r.as_str(),
            Self::Vm(r) => r.as_str(),
            Self::Sr(r) => r.as_str(),
            Self::Vdi(r) => r.as_str(),
            Self::Network(r) => r.as_str(),
        }
    }

    /// The object's `other_config`, if it is cached.
    pub fn other_config<'a>(
        &self,
        model: &'a ConnectionModel,
    ) -> Option<&'a HashMap<String, String>> {
        Some(match self {
            Self::Pool(r) => &model.pools.get(r)?.other_config,
            Self::Host(r) => &model.hosts.get(r)?.other_config,
            Self::Vm(r) => &model.vms.get(r)?.other_config,
            Self::Sr(r) => &model.srs.get(r)?.other_config,
            Self::Vdi(r) => &model.vdis.get(r)?.other_config,
            Self::Network(r) => &model.networks.get(r)?.other_config,
        })
    }

    /// The object's tags, if it is cached.
    pub fn tags<'a>(&self, model: &'a ConnectionModel) -> Option<&'a [String]> {
        Some(match self {
            Self::Pool(r) => &model.pools.get(r)?.tags,
            Self::Host(r) => &model.hosts.get(r)?.tags,
            Self::Vm(r) => &model.vms.get(r)?.tags,
            Self::Sr(r) => &model.srs.get(r)?.tags,
            Self::Vdi(r) => &model.vdis.get(r)?.tags,
            Self::Network(r) => &model.networks.get(r)?.tags,
        })
    }

    /// The object's `name_label` (the pool's may be empty).
    pub fn name<'a>(&self, model: &'a ConnectionModel) -> Option<&'a str> {
        Some(match self {
            Self::Pool(r) => &model.pools.get(r)?.name_label,
            Self::Host(r) => &model.hosts.get(r)?.name_label,
            Self::Vm(r) => &model.vms.get(r)?.name_label,
            Self::Sr(r) => &model.srs.get(r)?.name_label,
            Self::Vdi(r) => &model.vdis.get(r)?.name_label,
            Self::Network(r) => &model.networks.get(r)?.name_label,
        })
    }

    /// Every cached object of the six classes, in a stable order.
    pub fn all(model: &ConnectionModel) -> Vec<OrgObject> {
        let mut out: Vec<OrgObject> = Vec::new();
        out.extend(model.pools.keys().cloned().map(Self::Pool));
        out.extend(model.hosts.keys().cloned().map(Self::Host));
        out.extend(model.vms.keys().cloned().map(Self::Vm));
        out.extend(model.srs.keys().cloned().map(Self::Sr));
        out.extend(model.vdis.keys().cloned().map(Self::Vdi));
        out.extend(model.networks.keys().cloned().map(Self::Network));
        out.sort();
        out
    }
}

/// One API call of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgCall {
    RemoveFromOtherConfig {
        object: OrgObject,
        key: String,
    },
    AddToOtherConfig {
        object: OrgObject,
        key: String,
        value: String,
    },
    AddTag {
        object: OrgObject,
        tag: String,
    },
    RemoveTag {
        object: OrgObject,
        tag: String,
    },
    RemoveFromGuiConfig {
        pool: Ref<Pool>,
        key: String,
    },
    AddToGuiConfig {
        pool: Ref<Pool>,
        key: String,
        value: String,
    },
    /// `VM.set_name_label` (the VM Properties dialog's General page).
    SetName {
        vm: Ref<Vm>,
        name: String,
    },
    /// `VM.set_name_description`.
    SetDescription {
        vm: Ref<Vm>,
        description: String,
    },
}

impl OrgCall {
    /// The method name, `VM.add_tags` style (for logs and tests).
    pub fn method(&self) -> String {
        match self {
            Self::RemoveFromOtherConfig { object, .. } => {
                format!("{}.remove_from_other_config", object.class())
            }
            Self::AddToOtherConfig { object, .. } => {
                format!("{}.add_to_other_config", object.class())
            }
            Self::AddTag { object, .. } => format!("{}.add_tags", object.class()),
            Self::RemoveTag { object, .. } => format!("{}.remove_tags", object.class()),
            Self::RemoveFromGuiConfig { .. } => "pool.remove_from_gui_config".to_owned(),
            Self::AddToGuiConfig { .. } => "pool.add_to_gui_config".to_owned(),
            Self::SetName { .. } => "VM.set_name_label".to_owned(),
            Self::SetDescription { .. } => "VM.set_name_description".to_owned(),
        }
    }

    /// Whether the call writes to the pool record.
    pub fn writes_pool(&self) -> bool {
        matches!(
            self,
            Self::RemoveFromGuiConfig { .. }
                | Self::AddToGuiConfig { .. }
                | Self::RemoveFromOtherConfig {
                    object: OrgObject::Pool(_),
                    ..
                }
                | Self::AddToOtherConfig {
                    object: OrgObject::Pool(_),
                    ..
                }
                | Self::AddTag {
                    object: OrgObject::Pool(_),
                    ..
                }
                | Self::RemoveTag {
                    object: OrgObject::Pool(_),
                    ..
                }
        )
    }

    /// The RBAC method, with its key when the original checks one
    /// (`vm.add_to_other_config/key:folder`).
    pub fn rbac_method(&self) -> String {
        let lower = |o: &OrgObject| o.class().to_lowercase();
        match self {
            Self::RemoveFromOtherConfig { object, key } => {
                format!("{}.remove_from_other_config/key:{}", lower(object), key)
            }
            Self::AddToOtherConfig { object, key, .. } => {
                format!("{}.add_to_other_config/key:{}", lower(object), key)
            }
            Self::AddTag { object, .. } => format!("{}.add_tags", lower(object)),
            Self::RemoveTag { object, .. } => format!("{}.remove_tags", lower(object)),
            Self::RemoveFromGuiConfig { key, .. } => {
                format!("pool.remove_from_gui_config/key:{key}")
            }
            Self::AddToGuiConfig { key, .. } => format!("pool.add_to_gui_config/key:{key}"),
            Self::SetName { .. } => "vm.set_name_label".to_owned(),
            Self::SetDescription { .. } => "vm.set_name_description".to_owned(),
        }
    }
}

/// `Helpers.SetOtherConfig`: remove, then add.
fn set_other_config(calls: &mut Vec<OrgCall>, object: &OrgObject, key: &str, value: &str) {
    calls.push(OrgCall::RemoveFromOtherConfig {
        object: object.clone(),
        key: key.to_owned(),
    });
    calls.push(OrgCall::AddToOtherConfig {
        object: object.clone(),
        key: key.to_owned(),
        value: value.to_owned(),
    });
}

/// The `remove_from_gui_config` + `add_to_gui_config` pair the original
/// uses to replace a pool `gui_config` value.
pub fn set_gui_config(pool: &Ref<Pool>, key: &str, value: &str) -> Vec<OrgCall> {
    vec![
        OrgCall::RemoveFromGuiConfig {
            pool: pool.clone(),
            key: key.to_owned(),
        },
        OrgCall::AddToGuiConfig {
            pool: pool.clone(),
            key: key.to_owned(),
            value: value.to_owned(),
        },
    ]
}

// ---- folder paths -------------------------------------------------------------

/// `Folders.PointToPath`: the names along a path (`None` for an empty
/// path; `Some([])` for the root `/`).
pub fn point_to_path(path: &str) -> Option<Vec<String>> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    Some(
        path.trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// `Folders.PathToPoint`: the path of the first `depth` names.
pub fn path_to_point(names: &[String], depth: usize) -> String {
    format!("/{}", names[..depth.min(names.len())].join("/"))
}

/// The normalised path of a folder value (`a//b/` → `/a/b`), `None` when
/// the value names no folder.
pub fn normalise(path: &str) -> Option<String> {
    point_to_path(path).map(|names| path_to_point(&names, names.len()))
}

/// `Folders.FixupRelativePath` (CA-26707): trims, turns tabs into
/// spaces, collapses `//`, `/ ` and ` /`, and drops leading and trailing
/// slashes.
pub fn fixup_relative_path(name: &str) -> String {
    let mut name = name.trim().replace('\t', " ");
    loop {
        let before = name.len();
        name = name
            .replace("//", "/")
            .replace("/ ", "/")
            .replace(" /", "/");
        if name.len() == before {
            break;
        }
    }
    name.trim_matches('/').to_owned()
}

/// `Folders.AppendPath`.
pub fn append_path(first: &str, second: &str) -> String {
    if first.ends_with('/') {
        format!("{first}{second}")
    } else {
        format!("{first}/{second}")
    }
}

/// `Folders.GetParent`: the parent of a folder path (`/` for a top-level
/// folder), `None` for an empty value.
pub fn parent_of(path: &str) -> Option<String> {
    let names = point_to_path(path)?;
    Some(path_to_point(&names, names.len().saturating_sub(1)))
}

/// The last name of a folder path (`Folders` for the root).
pub fn folder_name(path: &str) -> String {
    point_to_path(path)
        .and_then(|n| n.last().cloned())
        .unwrap_or_default()
}

/// The ancestors of a folder, nearest first, root excluded
/// (`Folders.GetAncestorFolders`).
pub fn ancestors(path: &str) -> Vec<String> {
    let Some(names) = point_to_path(path) else {
        return Vec::new();
    };
    (1..=names.len())
        .rev()
        .map(|d| path_to_point(&names, d))
        .collect()
}

/// Whether `path` is `ancestor` or under it.
pub fn is_within(path: &str, ancestor: &str) -> bool {
    match (normalise(path), normalise(ancestor)) {
        (Some(p), Some(a)) => a == "/" || p == a || p.starts_with(&format!("{a}/")),
        _ => false,
    }
}

/// An object's raw folder value (`Folders.GetFolderString`).
pub fn folder_value(other_config: &HashMap<String, String>) -> Option<&str> {
    other_config.get(FOLDER_KEY).map(String::as_str)
}

/// The normalised folder an object is in, if any (`Folders.GetFolder`).
pub fn folder_of(model: &ConnectionModel, object: &OrgObject) -> Option<String> {
    object
        .other_config(model)
        .and_then(folder_value)
        .and_then(normalise)
}

/// `Folders.GetEmptyFolders`: the pool's empty-folder list, only entries
/// starting with `/`, sorted.
pub fn empty_folders(model: &ConnectionModel) -> Vec<String> {
    let raw = model
        .pool()
        .and_then(|(_, p)| p.other_config.get(EMPTY_FOLDERS_KEY).cloned())
        .unwrap_or_default();
    parse_empty_folders(&raw)
}

/// Splits an `EMPTY_FOLDERS` value.
pub fn parse_empty_folders(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = raw
        .split(EMPTY_FOLDERS_SEPARATOR)
        .filter(|s| s.starts_with('/'))
        .map(str::to_owned)
        .collect();
    out.sort();
    out
}

/// What is directly in a folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FolderItem {
    /// A sub-folder, by path.
    Folder(String),
    Object(OrgObject),
}

/// The folders of one connection: every folder that exists (with its
/// ancestors) and what is directly in each.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderIndex {
    /// Every folder path, the root `/` included when anything exists.
    pub folders: BTreeSet<String>,
    /// The objects directly in each folder.
    pub objects: HashMap<String, Vec<OrgObject>>,
}

impl FolderIndex {
    /// Builds the index from the objects' folder values and the pool's
    /// empty-folder list.
    pub fn build(model: &ConnectionModel) -> Self {
        let mut folders: HashMap<OrgObject, String> = HashMap::new();
        for object in OrgObject::all(model) {
            if let Some(f) = folder_of(model, &object) {
                folders.insert(object, f);
            }
        }
        Self::from_parts(&folders, &empty_folders(model))
    }

    fn from_parts(folders: &HashMap<OrgObject, String>, empty: &[String]) -> Self {
        let mut index = Self::default();
        let add_with_ancestors = |path: &str, index: &mut Self| {
            if let Some(names) = point_to_path(path) {
                for d in 0..=names.len() {
                    index.folders.insert(path_to_point(&names, d));
                }
            }
        };
        let mut sorted: Vec<(&OrgObject, &String)> = folders.iter().collect();
        sorted.sort();
        for (object, path) in sorted {
            add_with_ancestors(path, &mut index);
            index
                .objects
                .entry(path.clone())
                .or_default()
                .push(object.clone());
        }
        for path in empty {
            add_with_ancestors(path, &mut index);
        }
        index
    }

    /// Whether the folder exists.
    pub fn contains(&self, path: &str) -> bool {
        normalise(path).is_some_and(|p| self.folders.contains(&p))
    }

    /// `Folders.Children`: the sub-folders and objects directly in a
    /// folder, sub-folders first by name.
    pub fn children(&self, path: &str) -> Vec<FolderItem> {
        let Some(path) = normalise(path) else {
            return Vec::new();
        };
        let mut out: Vec<FolderItem> = self
            .folders
            .iter()
            .filter(|f| *f != "/" && parent_of(f).as_deref() == Some(path.as_str()))
            .cloned()
            .map(FolderItem::Folder)
            .collect();
        if let Some(objects) = self.objects.get(&path) {
            out.extend(objects.iter().cloned().map(FolderItem::Object));
        }
        out
    }

    /// Every object in a folder or under it.
    pub fn descendants(&self, path: &str) -> Vec<OrgObject> {
        self.objects
            .iter()
            .filter(|(p, _)| is_within(p, path))
            .flat_map(|(_, o)| o.iter().cloned())
            .collect()
    }
}

/// The original's `FolderAction`s as a call plan, replaying its reads of
/// the cache as each call lands.
struct FolderPlanner {
    pool: Option<Ref<Pool>>,
    /// Objects' normalised folders as the calls so far leave them.
    folders: HashMap<OrgObject, String>,
    /// The pool's empty-folder list as the calls so far leave it.
    empty: Vec<String>,
    marked_empty: Vec<String>,
    marked_non_empty: Vec<String>,
    calls: Vec<OrgCall>,
}

impl FolderPlanner {
    fn new(model: &ConnectionModel) -> Self {
        let mut folders = HashMap::new();
        for object in OrgObject::all(model) {
            if let Some(f) = folder_of(model, &object) {
                folders.insert(object, f);
            }
        }
        Self {
            pool: model.pool().map(|(r, _)| r.clone()),
            folders,
            empty: empty_folders(model),
            marked_empty: Vec::new(),
            marked_non_empty: Vec::new(),
            calls: Vec::new(),
        }
    }

    fn index(&self) -> FolderIndex {
        FolderIndex::from_parts(&self.folders, &self.empty)
    }

    /// The folder an item is in (`IXenObject.Path`): for a folder its
    /// parent.
    fn parent(&self, item: &FolderItem) -> Option<String> {
        match item {
            FolderItem::Folder(path) => parent_of(path),
            FolderItem::Object(o) => self.folders.get(o).cloned(),
        }
    }

    /// `FolderAction.DeleteOrMove`.
    fn delete_or_move(&mut self, item: &FolderItem, to: Option<&str>) {
        if let Some(parent) = self.parent(item)
            && self.index().children(&parent).len() == 1
        {
            self.marked_empty.push(parent);
        }
        self.move_contents(item, to);
    }

    /// `FolderAction.MoveContents`.
    fn move_contents(&mut self, item: &FolderItem, to: Option<&str>) {
        match item {
            FolderItem::Object(object) => {
                match to {
                    None => self.calls.push(OrgCall::RemoveFromOtherConfig {
                        object: object.clone(),
                        key: FOLDER_KEY.to_owned(),
                    }),
                    Some(to) => set_other_config(&mut self.calls, object, FOLDER_KEY, to),
                }
                match to.and_then(normalise) {
                    Some(to) => {
                        self.folders.insert(object.clone(), to);
                    }
                    None => {
                        self.folders.remove(object);
                    }
                }
                if let Some(to) = to {
                    self.marked_non_empty.push(to.to_owned());
                }
            }
            FolderItem::Folder(path) => {
                let new_location = to.map(|t| append_path(t, &folder_name(path)));
                let children = self.index().children(path);
                let empty = children.is_empty();
                for child in &children {
                    self.move_contents(child, new_location.as_deref());
                }
                self.marked_non_empty.push(path.clone());
                if empty && let Some(new_location) = new_location {
                    self.marked_empty.push(new_location);
                }
            }
        }
    }

    /// `FolderAction.UpdateEmptyFolders`: the marked folders added to the
    /// list (one write if any is new), then the non-empty ones taken out
    /// (one write if any was there; this console works per connection).
    fn update_empty_folders(&mut self) {
        let Some(pool) = self.pool.clone() else {
            return;
        };
        if !self.marked_empty.is_empty() {
            let mut list = self.empty.clone();
            let mut added = false;
            for path in std::mem::take(&mut self.marked_empty) {
                if !list.contains(&path) {
                    list.push(path);
                    added = true;
                }
            }
            if added {
                self.write_empty(&pool, list);
            }
        }
        let mut list = self.empty.clone();
        let before = list.len();
        for path in std::mem::take(&mut self.marked_non_empty) {
            list.retain(|p| *p != path);
        }
        if list.len() != before {
            self.write_empty(&pool, list);
        }
    }

    /// `FolderAction.SetEmptyFolders`.
    fn write_empty(&mut self, pool: &Ref<Pool>, list: Vec<String>) {
        let value = list.join(EMPTY_FOLDERS_SEPARATOR);
        set_other_config(
            &mut self.calls,
            &OrgObject::Pool(pool.clone()),
            EMPTY_FOLDERS_KEY,
            &value,
        );
        self.empty = parse_empty_folders(&value);
    }
}

/// `MoveToFolderAction`: puts objects and folders into folder `target`
/// (made if needed). Items already directly in it, and a folder moved
/// into itself or under itself, are left alone (`Folders.Move`).
pub fn plan_move(model: &ConnectionModel, items: &[FolderItem], target: &str) -> Vec<OrgCall> {
    let Some(target) = normalise(target) else {
        return Vec::new();
    };
    let mut planner = FolderPlanner::new(model);
    let items: Vec<&FolderItem> = items
        .iter()
        .filter(|item| match item {
            FolderItem::Folder(path) => {
                !is_within(&target, path) && parent_of(path).as_deref() != Some(target.as_str())
            }
            FolderItem::Object(o) => planner.folders.get(o) != Some(&target),
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    for item in items {
        planner.delete_or_move(item, Some(&target));
    }
    planner.update_empty_folders();
    planner.calls
}

/// `DeleteFolderAction`: takes objects out of their folder ("Remove from
/// folder"), or deletes folders (their objects end up in no folder).
pub fn plan_delete(model: &ConnectionModel, items: &[FolderItem]) -> Vec<OrgCall> {
    let mut planner = FolderPlanner::new(model);
    for item in items {
        planner.delete_or_move(item, None);
    }
    planner.update_empty_folders();
    planner.calls
}

/// `RenameFolderAction`: the folder's contents move to the renamed path.
pub fn plan_rename(model: &ConnectionModel, folder: &str, new_name: &str) -> Vec<OrgCall> {
    let Some(folder) = normalise(folder) else {
        return Vec::new();
    };
    let new_name = fixup_relative_path(new_name);
    let Some(parent) = parent_of(&folder) else {
        return Vec::new();
    };
    let path = append_path(&parent, &new_name);
    let mut planner = FolderPlanner::new(model);
    if folder != path && !new_name.is_empty() {
        planner.marked_empty.push(path.clone());
        for child in planner.index().children(&folder) {
            planner.move_contents(&child, Some(&path));
        }
        planner.marked_non_empty.push(folder);
        planner.update_empty_folders();
    }
    planner.calls
}

/// `CreateFolderAction`: adds the paths to the pool's empty-folder list.
/// `Err` (`FOLDER_ALREADY_EXISTS`) when none of them is new.
pub fn plan_create(model: &ConnectionModel, paths: &[String]) -> Result<Vec<OrgCall>, PlanError> {
    let mut planner = FolderPlanner::new(model);
    let Some(pool) = planner.pool.clone() else {
        return Err(PlanError::NoPool);
    };
    let mut list = planner.empty.clone();
    let mut added = false;
    for path in paths {
        if !list.contains(path) {
            list.push(path.clone());
            added = true;
        }
    }
    if !added {
        return Err(PlanError::FolderExists);
    }
    planner.write_empty(&pool, list);
    Ok(planner.calls)
}

/// `GeneralEditPageAction`'s folder half: `Folders.Move` to the new folder
/// or `Folders.Unfolder`, as the folder changed.
pub fn plan_set_folder(
    model: &ConnectionModel,
    object: &OrgObject,
    folder: Option<&str>,
) -> Vec<OrgCall> {
    let current = folder_of(model, object);
    let wanted = folder.and_then(normalise);
    if current == wanted {
        return Vec::new();
    }
    let item = [FolderItem::Object(object.clone())];
    match wanted {
        Some(target) => plan_move(model, &item, &target),
        None => plan_delete(model, &item),
    }
}

// ---- tags ---------------------------------------------------------------------

/// Every tag in use on the connection with how many objects carry it
/// (`Tags.GetTagCounts`).
pub fn tag_counts(model: &ConnectionModel) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for object in OrgObject::all(model) {
        for tag in object.tags(model).unwrap_or_default() {
            *counts.entry(tag).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> =
        counts.into_iter().map(|(t, n)| (t.to_owned(), n)).collect();
    out.sort();
    out
}

/// The objects carrying a tag (`Tags.Users`).
pub fn tag_users(model: &ConnectionModel, tag: &str) -> Vec<OrgObject> {
    OrgObject::all(model)
        .into_iter()
        .filter(|o| o.tags(model).is_some_and(|t| t.iter().any(|x| x == tag)))
        .collect()
}

/// `Tags.AddTag` for each object that does not have the tag yet.
pub fn plan_add_tag(model: &ConnectionModel, objects: &[OrgObject], tag: &str) -> Vec<OrgCall> {
    objects
        .iter()
        .filter(|o| !o.tags(model).is_some_and(|t| t.iter().any(|x| x == tag)))
        .map(|o| OrgCall::AddTag {
            object: o.clone(),
            tag: tag.to_owned(),
        })
        .collect()
}

/// `Tags.RemoveTag` for each object that has the tag.
pub fn plan_remove_tag(model: &ConnectionModel, objects: &[OrgObject], tag: &str) -> Vec<OrgCall> {
    objects
        .iter()
        .filter(|o| o.tags(model).is_some_and(|t| t.iter().any(|x| x == tag)))
        .map(|o| OrgCall::RemoveTag {
            object: o.clone(),
            tag: tag.to_owned(),
        })
        .collect()
}

/// `Tags.RenameTagGlobally` on this connection: each object with the old
/// tag loses it and gets the new one (unless it already has it).
pub fn plan_rename_tag(model: &ConnectionModel, old: &str, new: &str) -> Vec<OrgCall> {
    let mut calls = Vec::new();
    for object in tag_users(model, old) {
        calls.push(OrgCall::RemoveTag {
            object: object.clone(),
            tag: old.to_owned(),
        });
        if !object
            .tags(model)
            .is_some_and(|t| t.iter().any(|x| x == new))
        {
            calls.push(OrgCall::AddTag {
                object,
                tag: new.to_owned(),
            });
        }
    }
    calls
}

/// `Tags.RemoveTagGlobally` on this connection.
pub fn plan_delete_tag(model: &ConnectionModel, tag: &str) -> Vec<OrgCall> {
    plan_remove_tag(model, &tag_users(model, tag), tag)
}

/// `GeneralEditPageAction`: the folder change, then the tags removed, then
/// the tags added (each in sorted order).
pub fn plan_folder_and_tags(
    model: &ConnectionModel,
    object: &OrgObject,
    folder: Option<&str>,
    tags: &[String],
) -> Vec<OrgCall> {
    let mut calls = plan_set_folder(model, object, folder);
    let mut old: Vec<String> = object.tags(model).unwrap_or_default().to_vec();
    old.sort();
    let mut new: Vec<String> = tags.to_vec();
    new.sort();
    new.dedup();
    for tag in old.iter().filter(|t| !new.contains(t)) {
        calls.push(OrgCall::RemoveTag {
            object: object.clone(),
            tag: tag.clone(),
        });
    }
    for tag in new.iter().filter(|t| !old.contains(t)) {
        calls.push(OrgCall::AddTag {
            object: object.clone(),
            tag: tag.clone(),
        });
    }
    calls
}

// ---- custom fields ------------------------------------------------------------

/// `CustomFieldDefinition.Types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CustomFieldType {
    String,
    Date,
}

impl CustomFieldType {
    /// The XML `type` attribute.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "String",
            Self::Date => "Date",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "String" => Some(Self::String),
            "Date" => Some(Self::Date),
            _ => None,
        }
    }
}

/// `CustomFieldDefinition`: equal when name and type are.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomFieldDefinition {
    pub name: String,
    pub kind: CustomFieldType,
}

impl CustomFieldDefinition {
    pub fn new(name: impl Into<String>, kind: CustomFieldType) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }

    /// `CustomFieldsManager.GetCustomFieldKey`.
    pub fn value_key(&self) -> String {
        format!("{CUSTOM_FIELDS_KEY}.{}", self.name)
    }

    /// `CustomFieldDefinition.ToXmlNode`, as .NET's `OuterXml` writes it.
    pub fn to_xml(&self) -> String {
        format!(
            "<CustomFieldDefinition name=\"{}\" type=\"{}\" defaultValue=\"\" />",
            xml_attr(&self.name),
            self.kind.as_str()
        )
    }

    /// From a `<CustomFieldDefinition>` element.
    pub fn from_node(node: roxmltree::Node<'_, '_>) -> Option<Self> {
        Some(Self {
            name: node.attribute("name")?.to_owned(),
            kind: CustomFieldType::parse(node.attribute("type")?)?,
        })
    }
}

/// Escapes an attribute value as .NET's `XmlDocument` writes it.
pub fn xml_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            '\t' => out.push_str("&#x9;"),
            c => out.push(c),
        }
    }
    out
}

/// `CustomFieldsManager.GetCustomFieldDefinitionXML`.
pub fn definitions_xml(definitions: &[CustomFieldDefinition]) -> String {
    if definitions.is_empty() {
        return "<CustomFieldDefinitions />".to_owned();
    }
    let mut out = String::from("<CustomFieldDefinitions>");
    for d in definitions {
        out.push_str(&d.to_xml());
    }
    out.push_str("</CustomFieldDefinitions>");
    out
}

/// `CustomFieldsCache.GetCustomFieldDefinitions`: bad entries are skipped,
/// bad XML gives none.
pub fn parse_definitions(xml: &str) -> Vec<CustomFieldDefinition> {
    let xml = xml.trim();
    if xml.is_empty() {
        return Vec::new();
    }
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    doc.root_element()
        .children()
        .filter(|n| n.is_element())
        .filter_map(CustomFieldDefinition::from_node)
        .collect()
}

/// The custom fields defined on the connection's pool.
pub fn definitions(model: &ConnectionModel) -> Vec<CustomFieldDefinition> {
    model
        .pool()
        .and_then(|(_, p)| p.gui_config.get(CUSTOM_FIELDS_KEY))
        .map(|xml| parse_definitions(xml))
        .unwrap_or_default()
}

/// A custom field's value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CustomFieldValue {
    Text(String),
    /// Unix seconds, UTC.
    Date(i64),
}

impl CustomFieldValue {
    /// `CustomField.ValueAsInvariantString`.
    pub fn stored(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Date(t) => format_u(*t),
        }
    }

    /// As shown (dates in UTC, `yyyy-MM-dd HH:mm:ss`).
    pub fn display(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Date(t) => crate::format::datetime_utc(*t, true),
        }
    }
}

/// .NET's `"u"` format: `yyyy-MM-dd HH:mm:ssZ`.
pub fn format_u(t: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(t);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (secs / 3600) as u32,
        (secs % 3600 / 60) as u32,
        (secs % 60) as u32,
    )
}

/// Days since 1970-01-01 of a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads a stored date (`DateTime.TryParse` of what XenCenter writes):
/// `yyyy-MM-dd HH:mm:ssZ`, the same with `T`, without the `Z` (taken as
/// UTC), or a bare `yyyy-MM-dd`.
pub fn parse_date(value: &str) -> Option<i64> {
    let v = value.trim();
    let v = v.strip_suffix('Z').unwrap_or(v);
    let (date, time) = match v.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t)),
        None => (v, None),
    };
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = 0i64;
    if let Some(time) = time {
        let time = time.split(['.', '+']).next()?;
        let mut t = time.split(':');
        let h: i64 = t.next()?.parse().ok()?;
        let mi: i64 = t.next().unwrap_or("0").parse().ok()?;
        let s: i64 = t.next().unwrap_or("0").parse().ok()?;
        if h > 23 || mi > 59 || s > 59 {
            return None;
        }
        secs = h * 3600 + mi * 60 + s;
    }
    Some(days_from_civil(y, m, d) * 86_400 + secs)
}

/// `CustomFieldsManager.GetCustomFieldValue`: missing or empty is none; a
/// date that does not parse is none.
pub fn value_of(
    other_config: &HashMap<String, String>,
    def: &CustomFieldDefinition,
) -> Option<CustomFieldValue> {
    let raw = other_config.get(&def.value_key())?;
    if raw.is_empty() {
        return None;
    }
    match def.kind {
        CustomFieldType::String => Some(CustomFieldValue::Text(raw.clone())),
        CustomFieldType::Date => parse_date(raw).map(CustomFieldValue::Date),
    }
}

/// `CustomFieldsManager.CustomFieldValues`: the fields with a value, in
/// definition order.
pub fn values(
    definitions: &[CustomFieldDefinition],
    other_config: &HashMap<String, String>,
) -> Vec<(CustomFieldDefinition, CustomFieldValue)> {
    definitions
        .iter()
        .filter_map(|d| value_of(other_config, d).map(|v| (d.clone(), v)))
        .collect()
}

/// `CustomFieldsManager.HasCustomFields`.
pub fn has_custom_fields(
    definitions: &[CustomFieldDefinition],
    other_config: &HashMap<String, String>,
) -> bool {
    definitions.iter().any(|d| {
        other_config
            .get(&d.value_key())
            .is_some_and(|v| !v.is_empty())
    })
}

/// `AddCustomFieldAction`: the definition list rewritten with the new
/// one; nothing when it is already there.
pub fn plan_add_definition(model: &ConnectionModel, def: &CustomFieldDefinition) -> Vec<OrgCall> {
    let mut defs = definitions(model);
    let Some((pool, _)) = model.pool() else {
        return Vec::new();
    };
    if defs.contains(def) {
        return Vec::new();
    }
    defs.push(def.clone());
    set_gui_config(pool, CUSTOM_FIELDS_KEY, &definitions_xml(&defs))
}

/// `RemoveCustomFieldAction`: the definition list rewritten without it,
/// then its value removed from every pool, host, VM, SR, VDI and network
/// that has one.
pub fn plan_remove_definition(
    model: &ConnectionModel,
    def: &CustomFieldDefinition,
) -> Vec<OrgCall> {
    let mut defs = definitions(model);
    let Some((pool, _)) = model.pool() else {
        return Vec::new();
    };
    let before = defs.len();
    defs.retain(|d| d != def);
    if defs.len() == before {
        return Vec::new();
    }
    let mut calls = set_gui_config(pool, CUSTOM_FIELDS_KEY, &definitions_xml(&defs));
    let key = def.value_key();
    let mut order: Vec<OrgObject> = Vec::new();
    order.extend(model.pools.keys().cloned().map(OrgObject::Pool));
    order.extend(model.hosts.keys().cloned().map(OrgObject::Host));
    order.extend(model.vms.keys().cloned().map(OrgObject::Vm));
    order.extend(model.srs.keys().cloned().map(OrgObject::Sr));
    order.extend(model.vdis.keys().cloned().map(OrgObject::Vdi));
    order.extend(model.networks.keys().cloned().map(OrgObject::Network));
    for object in order {
        if object
            .other_config(model)
            .is_some_and(|oc| oc.contains_key(&key))
        {
            calls.push(OrgCall::RemoveFromOtherConfig {
                object,
                key: key.clone(),
            });
        }
    }
    calls
}

/// `SaveCustomFieldsAction`: each changed field written (remove, then add)
/// or, when cleared, removed.
pub fn plan_save_values(
    object: &OrgObject,
    values: &[(CustomFieldDefinition, Option<CustomFieldValue>)],
) -> Vec<OrgCall> {
    let mut calls = Vec::new();
    for (def, value) in values {
        let key = def.value_key();
        match value.as_ref().map(CustomFieldValue::stored) {
            Some(v) if !v.is_empty() => set_other_config(&mut calls, object, &key, &v),
            _ => calls.push(OrgCall::RemoveFromOtherConfig {
                object: object.clone(),
                key,
            }),
        }
    }
    calls
}

/// `CustomFieldsDisplayPage.SaveSettings`: only the fields whose value
/// changed.
pub fn changed_values(
    model: &ConnectionModel,
    object: &OrgObject,
    edited: &[(CustomFieldDefinition, Option<CustomFieldValue>)],
) -> Vec<(CustomFieldDefinition, Option<CustomFieldValue>)> {
    let oc = object.other_config(model).cloned().unwrap_or_default();
    edited
        .iter()
        .filter(|(def, new)| {
            let current = value_of(&oc, def);
            !(current.is_none() && new.is_none()) && current != *new
        })
        .cloned()
        .collect()
}

// ---- requests -----------------------------------------------------------------

/// The F13 operations, run by [`crate::org_run`] on one connection: the
/// plan is made from that connection's model when the operation starts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OrgRequest {
    /// `MoveToFolderAction` (drag and drop, Move to folder).
    MoveToFolder {
        items: Vec<FolderItem>,
        folder: String,
    },
    /// `DeleteFolderAction`: "Remove from folder" for objects, "Delete" for
    /// folders.
    DeleteFromFolders { items: Vec<FolderItem> },
    /// `RenameFolderAction`.
    RenameFolder { folder: String, name: String },
    /// `CreateFolderAction`.
    NewFolders { paths: Vec<String> },
    /// `Tags.AddTagAction` for each object (drag and drop onto a tag).
    AddTag {
        objects: Vec<OrgObject>,
        tag: String,
    },
    /// `UntagCommand`: the tag taken off the objects.
    RemoveTag {
        objects: Vec<OrgObject>,
        tag: String,
    },
    /// `RenameTagCommand` on this connection.
    RenameTag { old: String, new: String },
    /// `DeleteTagCommand` on this connection.
    DeleteTag { tag: String },
    /// `GeneralEditPageAction`: the Properties dialog's folder and tags.
    FolderAndTags {
        object: OrgObject,
        folder: Option<String>,
        tags: Vec<String>,
    },
    /// `AddCustomFieldAction`.
    AddCustomField(CustomFieldDefinition),
    /// `RemoveCustomFieldAction`.
    RemoveCustomField(CustomFieldDefinition),
    /// `SaveCustomFieldsAction`: changed values only.
    SaveCustomFields {
        object: OrgObject,
        values: Vec<(CustomFieldDefinition, Option<CustomFieldValue>)>,
    },
    /// `Search.Save`: the search's XML under `xensearch.Search-<uuid>`.
    SaveSearch {
        uuid: String,
        name: String,
        xml: String,
    },
    /// `Search.Delete` on this connection.
    DeleteSearch { uuid: String, name: String },
    /// The VM Properties dialog's name and description.
    VmNames {
        vm: Ref<Vm>,
        name: Option<String>,
        description: Option<String>,
    },
}

/// Why a request has nothing it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// `FOLDER_ALREADY_EXISTS`.
    FolderExists,
    /// The pool is not in the cache (`POOL_GONE`).
    NoPool,
}

impl OrgRequest {
    /// The calls the request makes on `model`.
    pub fn plan(&self, model: &ConnectionModel) -> Result<Vec<OrgCall>, PlanError> {
        let pool = || {
            model
                .pool()
                .map(|(r, _)| r.clone())
                .ok_or(PlanError::NoPool)
        };
        Ok(match self {
            Self::MoveToFolder { items, folder } => plan_move(model, items, folder),
            Self::DeleteFromFolders { items } => plan_delete(model, items),
            Self::RenameFolder { folder, name } => plan_rename(model, folder, name),
            Self::NewFolders { paths } => plan_create(model, paths)?,
            Self::AddTag { objects, tag } => plan_add_tag(model, objects, tag),
            Self::RemoveTag { objects, tag } => plan_remove_tag(model, objects, tag),
            Self::RenameTag { old, new } => plan_rename_tag(model, old, new),
            Self::DeleteTag { tag } => plan_delete_tag(model, tag),
            Self::FolderAndTags {
                object,
                folder,
                tags,
            } => plan_folder_and_tags(model, object, folder.as_deref(), tags),
            Self::AddCustomField(def) => plan_add_definition(model, def),
            Self::RemoveCustomField(def) => plan_remove_definition(model, def),
            Self::SaveCustomFields { object, values } => plan_save_values(object, values),
            Self::SaveSearch { uuid, xml, .. } => {
                set_gui_config(&pool()?, &format!("{SEARCH_KEY_PREFIX}{uuid}"), xml)
            }
            Self::DeleteSearch { uuid, .. } => vec![OrgCall::RemoveFromGuiConfig {
                pool: pool()?,
                key: format!("{SEARCH_KEY_PREFIX}{uuid}"),
            }],
            Self::VmNames {
                vm,
                name,
                description,
            } => {
                let mut calls = Vec::new();
                if let Some(n) = name {
                    calls.push(OrgCall::SetName {
                        vm: vm.clone(),
                        name: n.clone(),
                    });
                }
                if let Some(d) = description {
                    calls.push(OrgCall::SetDescription {
                        vm: vm.clone(),
                        description: d.clone(),
                    });
                }
                calls
            }
        })
    }

    /// The objects the request writes to (for the classes of its RBAC
    /// check).
    fn objects(&self) -> Vec<&OrgObject> {
        match self {
            Self::MoveToFolder { items, .. } | Self::DeleteFromFolders { items } => items
                .iter()
                .filter_map(|i| match i {
                    FolderItem::Object(o) => Some(o),
                    FolderItem::Folder(_) => None,
                })
                .collect(),
            Self::AddTag { objects, .. } | Self::RemoveTag { objects, .. } => {
                objects.iter().collect()
            }
            Self::FolderAndTags { object, .. } | Self::SaveCustomFields { object, .. } => {
                vec![object]
            }
            _ => Vec::new(),
        }
    }

    /// The API calls the original's role check lists for the request
    /// (`ApiMethodsToRoleCheck`), as RBAC permission names.
    pub fn rbac_methods(&self) -> Vec<&'static str> {
        const EMPTY: [&str; 2] = [
            "pool.add_to_other_config/key:empty_folders",
            "pool.remove_from_other_config/key:empty_folders",
        ];
        const FIELDS: [&str; 2] = [
            "pool.add_to_gui_config/key:xencenter.customfields",
            "pool.remove_from_gui_config/key:xencenter.customfields",
        ];
        let classes: BTreeSet<&'static str> = self.objects().iter().map(|o| o.class()).collect();
        let mut m: Vec<&'static str> = Vec::new();
        let folder_keys = |m: &mut Vec<&'static str>, c: &str| {
            m.extend(match c {
                "VM" => [
                    "vm.add_to_other_config/key:folder",
                    "vm.remove_from_other_config/key:folder",
                ],
                "host" => [
                    "host.add_to_other_config/key:folder",
                    "host.remove_from_other_config/key:folder",
                ],
                "pool" => [
                    "pool.add_to_other_config/key:folder",
                    "pool.remove_from_other_config/key:folder",
                ],
                "SR" => [
                    "sr.add_to_other_config/key:folder",
                    "sr.remove_from_other_config/key:folder",
                ],
                "VDI" => [
                    "vdi.add_to_other_config/key:folder",
                    "vdi.remove_from_other_config/key:folder",
                ],
                _ => [
                    "network.add_to_other_config/key:folder",
                    "network.remove_from_other_config/key:folder",
                ],
            });
        };
        let tag_calls = |m: &mut Vec<&'static str>, c: &str| {
            m.extend(match c {
                "VM" => ["vm.add_tags", "vm.remove_tags"],
                "host" => ["host.add_tags", "host.remove_tags"],
                "pool" => ["pool.add_tags", "pool.remove_tags"],
                "SR" => ["sr.add_tags", "sr.remove_tags"],
                "VDI" => ["vdi.add_tags", "vdi.remove_tags"],
                _ => ["network.add_tags", "network.remove_tags"],
            });
        };
        let field_keys = |m: &mut Vec<&'static str>, c: &str| {
            m.extend(match c {
                "VM" => [
                    "vm.add_to_other_config/key:xencenter.customfields.*",
                    "vm.remove_from_other_config/key:xencenter.customfields.*",
                ],
                "host" => [
                    "host.add_to_other_config/key:xencenter.customfields.*",
                    "host.remove_from_other_config/key:xencenter.customfields.*",
                ],
                "pool" => [
                    "pool.add_to_other_config/key:xencenter.customfields.*",
                    "pool.remove_from_other_config/key:xencenter.customfields.*",
                ],
                "SR" => [
                    "sr.add_to_other_config/key:xencenter.customfields.*",
                    "sr.remove_from_other_config/key:xencenter.customfields.*",
                ],
                "VDI" => [
                    "vdi.add_to_other_config/key:xencenter.customfields.*",
                    "vdi.remove_from_other_config/key:xencenter.customfields.*",
                ],
                _ => [
                    "network.add_to_other_config/key:xencenter.customfields.*",
                    "network.remove_from_other_config/key:xencenter.customfields.*",
                ],
            });
        };
        match self {
            Self::MoveToFolder { .. }
            | Self::DeleteFromFolders { .. }
            | Self::RenameFolder { .. }
            | Self::NewFolders { .. } => {
                m.extend(EMPTY);
                for c in &classes {
                    folder_keys(&mut m, c);
                }
            }
            Self::AddTag { .. } | Self::RemoveTag { .. } => {
                for c in &classes {
                    tag_calls(&mut m, c);
                }
            }
            Self::RenameTag { .. } | Self::DeleteTag { .. } => {}
            Self::FolderAndTags { .. } => {
                m.extend(EMPTY);
                for c in &classes {
                    folder_keys(&mut m, c);
                    tag_calls(&mut m, c);
                }
            }
            Self::AddCustomField(_) => m.extend(FIELDS),
            Self::RemoveCustomField(_) => {
                m.extend(FIELDS);
                for c in ["pool", "host", "VM", "SR", "VDI", "network"] {
                    field_keys(&mut m, c);
                }
                // Only the removal half is made.
                m.retain(|x| !x.contains(".add_to_other_config"));
            }
            Self::SaveCustomFields { .. } => {
                for c in &classes {
                    field_keys(&mut m, c);
                }
            }
            Self::SaveSearch { .. } => {
                m.extend(["pool.add_to_gui_config", "pool.remove_from_gui_config"])
            }
            Self::DeleteSearch { .. } => m.push("pool.remove_from_gui_config"),
            Self::VmNames {
                name, description, ..
            } => {
                if name.is_some() {
                    m.push("vm.set_name_label");
                }
                if description.is_some() {
                    m.push("vm.set_name_description");
                }
            }
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(db: serde_json::Value) -> ConnectionModel {
        ConnectionModel::from_database(&db)
    }

    fn vm(name: &str, folder: Option<&str>) -> serde_json::Value {
        let mut oc = serde_json::Map::new();
        if let Some(f) = folder {
            oc.insert("folder".into(), json!(f));
        }
        json!({"uuid": format!("uuid-{name}"), "name_label": name, "other_config": oc, "tags": []})
    }

    fn pool(empty: Option<&str>) -> serde_json::Value {
        let mut oc = serde_json::Map::new();
        if let Some(e) = empty {
            oc.insert("EMPTY_FOLDERS".into(), json!(e));
        }
        json!({"uuid": "pool-uuid", "name_label": "p", "other_config": oc, "gui_config": {}, "tags": []})
    }

    fn calls_text(calls: &[OrgCall]) -> Vec<String> {
        calls
            .iter()
            .map(|c| match c {
                OrgCall::RemoveFromOtherConfig { object, key } => {
                    format!(
                        "{}.remove_from_other_config {} {key}",
                        object.class(),
                        object.opaque()
                    )
                }
                OrgCall::AddToOtherConfig { object, key, value } => format!(
                    "{}.add_to_other_config {} {key}={value}",
                    object.class(),
                    object.opaque()
                ),
                other => other.method(),
            })
            .collect()
    }

    #[test]
    fn paths_follow_the_original() {
        assert_eq!(point_to_path(""), None);
        assert_eq!(point_to_path("/"), Some(vec![]));
        assert_eq!(
            point_to_path("a//b/"),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(normalise("a//b/").as_deref(), Some("/a/b"));
        assert_eq!(fixup_relative_path("  /a / b//c\t d/ "), "a/b/c  d");
        assert_eq!(append_path("/", "x"), "/x");
        assert_eq!(append_path("/a", "x"), "/a/x");
        assert_eq!(parent_of("/a/b").as_deref(), Some("/a"));
        assert_eq!(parent_of("/a").as_deref(), Some("/"));
        assert_eq!(ancestors("/a/b"), vec!["/a/b", "/a"]);
        assert_eq!(parse_empty_folders("/z;junk;/a"), vec!["/a", "/z"]);
        assert!(is_within("/a/b", "/a") && !is_within("/ab", "/a"));
    }

    #[test]
    fn the_index_holds_objects_ancestors_and_empty_folders() {
        let m = model(json!({
            "pool": {"OpaqueRef:p": pool(Some("/Empty;/A/Inner"))},
            "VM": {"OpaqueRef:v1": vm("v1", Some("/A/B")), "OpaqueRef:v2": vm("v2", None)},
        }));
        let index = FolderIndex::build(&m);
        assert_eq!(
            index.folders.iter().cloned().collect::<Vec<_>>(),
            vec!["/", "/A", "/A/B", "/A/Inner", "/Empty"]
        );
        assert_eq!(
            index.children("/A"),
            vec![
                FolderItem::Folder("/A/B".into()),
                FolderItem::Folder("/A/Inner".into())
            ]
        );
        assert_eq!(
            index.children("/A/B"),
            vec![FolderItem::Object(OrgObject::Vm(Ref::new("OpaqueRef:v1")))]
        );
    }

    #[test]
    fn moving_writes_the_folder_and_keeps_the_emptied_one() {
        let m = model(json!({
            "pool": {"OpaqueRef:p": pool(Some("/Target"))},
            "VM": {"OpaqueRef:v1": vm("v1", Some("/Old"))},
        }));
        let calls = plan_move(
            &m,
            &[FolderItem::Object(OrgObject::Vm(Ref::new("OpaqueRef:v1")))],
            "/Target",
        );
        assert_eq!(
            calls_text(&calls),
            vec![
                "VM.remove_from_other_config OpaqueRef:v1 folder",
                "VM.add_to_other_config OpaqueRef:v1 folder=/Target",
                // /Old became empty: added to the list.
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/Target;/Old",
                // /Target is no longer empty: taken out.
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/Old",
            ]
        );
        // Already there: nothing.
        let m2 = model(
            json!({"pool": {"OpaqueRef:p": pool(None)}, "VM": {"OpaqueRef:v1": vm("v1", Some("/T"))}}),
        );
        assert!(
            plan_move(
                &m2,
                &[FolderItem::Object(OrgObject::Vm(Ref::new("OpaqueRef:v1")))],
                "/T"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_new_folder_with_no_empty_list_needs_no_pool_write() {
        let m = model(
            json!({"pool": {"OpaqueRef:p": pool(None)}, "VM": {"OpaqueRef:v1": vm("v1", None)}}),
        );
        let o = OrgObject::Vm(Ref::new("OpaqueRef:v1"));
        let set = plan_set_folder(&m, &o, Some("/xenadmin-rs-test"));
        assert!(set.iter().all(|c| !c.writes_pool()), "{set:?}");
        // Clearing it again makes the folder empty: the original keeps it
        // in EMPTY_FOLDERS, a pool write.
        let m = model(
            json!({"pool": {"OpaqueRef:p": pool(None)}, "VM": {"OpaqueRef:v1": vm("v1", Some("/xenadmin-rs-test"))}}),
        );
        let clear = plan_set_folder(&m, &o, None);
        assert_eq!(
            calls_text(&clear),
            vec![
                "VM.remove_from_other_config OpaqueRef:v1 folder",
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/xenadmin-rs-test",
            ]
        );
    }

    #[test]
    fn deleting_and_renaming_folders_move_their_contents() {
        let m = model(json!({
            "pool": {"OpaqueRef:p": pool(Some("/A/Empty;/Other"))},
            "VM": {"OpaqueRef:v1": vm("v1", Some("/A")), "OpaqueRef:v2": vm("v2", Some("/A/B"))},
        }));
        let rename = plan_rename(&m, "/A", "Z");
        assert_eq!(
            calls_text(&rename),
            vec![
                "VM.remove_from_other_config OpaqueRef:v2 folder",
                "VM.add_to_other_config OpaqueRef:v2 folder=/Z/B",
                "VM.remove_from_other_config OpaqueRef:v1 folder",
                "VM.add_to_other_config OpaqueRef:v1 folder=/Z",
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/A/Empty;/Other;/Z;/Z/Empty",
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/Other;/Z/Empty",
            ]
        );
        let delete = plan_delete(&m, &[FolderItem::Folder("/A".into())]);
        assert_eq!(
            calls_text(&delete),
            vec![
                "VM.remove_from_other_config OpaqueRef:v2 folder",
                "VM.remove_from_other_config OpaqueRef:v1 folder",
                // The deleted folder's empty sub-folder leaves the list.
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/Other",
            ]
        );
        assert_eq!(
            plan_create(&m, &["/Other".into()]),
            Err(PlanError::FolderExists)
        );
        assert_eq!(
            calls_text(&plan_create(&m, &["/New".into()]).unwrap()),
            vec![
                "pool.remove_from_other_config OpaqueRef:p EMPTY_FOLDERS",
                "pool.add_to_other_config OpaqueRef:p EMPTY_FOLDERS=/A/Empty;/Other;/New",
            ]
        );
    }

    #[test]
    fn tags_change_one_at_a_time() {
        let m = model(json!({
            "pool": {"OpaqueRef:p": pool(None)},
            "VM": {"OpaqueRef:v1": {"uuid": "u1", "name_label": "v1", "tags": ["a", "b"], "other_config": {}},
                   "OpaqueRef:v2": {"uuid": "u2", "name_label": "v2", "tags": ["c"], "other_config": {}}},
        }));
        let v1 = OrgObject::Vm(Ref::new("OpaqueRef:v1"));
        let v2 = OrgObject::Vm(Ref::new("OpaqueRef:v2"));
        assert_eq!(plan_add_tag(&m, &[v1.clone(), v2.clone()], "a").len(), 1);
        assert_eq!(plan_remove_tag(&m, &[v1.clone(), v2.clone()], "c").len(), 1);
        let edit = plan_folder_and_tags(&m, &v1, None, &["b".into(), "z".into()]);
        assert_eq!(
            edit,
            vec![
                OrgCall::RemoveTag {
                    object: v1.clone(),
                    tag: "a".into()
                },
                OrgCall::AddTag {
                    object: v1.clone(),
                    tag: "z".into()
                },
            ]
        );
        assert_eq!(
            tag_counts(&m),
            vec![("a".into(), 1), ("b".into(), 1), ("c".into(), 1)]
        );
        assert_eq!(
            plan_rename_tag(&m, "a", "b"),
            vec![OrgCall::RemoveTag {
                object: v1,
                tag: "a".into()
            }]
        );
    }

    #[test]
    fn custom_fields_use_xencenters_xml_and_keys() {
        let defs = vec![
            CustomFieldDefinition::new("Owner", CustomFieldType::String),
            CustomFieldDefinition::new("Due \"date\" & <time>", CustomFieldType::Date),
        ];
        let xml = definitions_xml(&defs);
        assert_eq!(
            xml,
            "<CustomFieldDefinitions><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" />\
             <CustomFieldDefinition name=\"Due &quot;date&quot; &amp; &lt;time&gt;\" type=\"Date\" defaultValue=\"\" />\
             </CustomFieldDefinitions>"
        );
        assert_eq!(parse_definitions(&xml), defs);
        assert_eq!(definitions_xml(&[]), "<CustomFieldDefinitions />");
        assert_eq!(defs[0].value_key(), "XenCenter.CustomFields.Owner");
        let t = parse_date("2026-11-26 14:30:05Z").unwrap();
        assert_eq!(format_u(t), "2026-11-26 14:30:05Z");
        assert_eq!(
            parse_date("2026-11-26"),
            Some(days_from_civil(2026, 11, 26) * 86_400)
        );
        assert_eq!(format_u(0), "1970-01-01 00:00:00Z");
        let mut oc = HashMap::new();
        oc.insert("XenCenter.CustomFields.Owner".to_owned(), "ops".to_owned());
        oc.insert(
            "XenCenter.CustomFields.Due \"date\" & <time>".to_owned(),
            "bad".to_owned(),
        );
        assert_eq!(
            values(&defs, &oc),
            vec![(defs[0].clone(), CustomFieldValue::Text("ops".into()))]
        );
        assert!(has_custom_fields(&defs, &oc));
        let o = OrgObject::Vm(Ref::new("OpaqueRef:v"));
        let save = plan_save_values(
            &o,
            &[
                (defs[0].clone(), None),
                (defs[1].clone(), Some(CustomFieldValue::Date(t))),
            ],
        );
        assert_eq!(
            save,
            vec![
                OrgCall::RemoveFromOtherConfig {
                    object: o.clone(),
                    key: "XenCenter.CustomFields.Owner".into()
                },
                OrgCall::RemoveFromOtherConfig {
                    object: o.clone(),
                    key: defs[1].value_key()
                },
                OrgCall::AddToOtherConfig {
                    object: o,
                    key: defs[1].value_key(),
                    value: "2026-11-26 14:30:05Z".into()
                },
            ]
        );
    }
}
