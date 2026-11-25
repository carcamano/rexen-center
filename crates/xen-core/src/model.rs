//! Live object cache of one connection and the tree built from it.
//!
//! [`ConnectionModel`] has one map per XenAPI class the cache follows: every
//! class listed by [`xapi::for_each_cached_class!`] (all classes with a record
//! and a `get_all_records` call, except sessions, users and secrets). The
//! struct, the event dispatch and the initial-load table are produced from
//! that list, so a datamodel update adds classes here automatically.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use xapi::{
    Event, EventOperation, Host, HostRecord, Pool, PoolRecord, Ref, Session, Sr, Vm, VmOperations,
    VmPowerState, VmRecord, XapiClass, XapiObject,
};

use crate::objects::{self, SrIcon, VmIcon, VmKind};

/// Classes `event.from` is asked for: everything. Events of classes the
/// model does not keep (sessions, secrets, removed classes…) are ignored.
pub const EVENT_CLASSES: &[&str] = &["*"];

/// Records of one class, keyed by reference.
pub type ClassMap<T> = HashMap<Ref<T>, Arc<<T as XapiObject>::Record>>;

/// A class kept by [`ConnectionModel`].
pub trait ModelClass: XapiObject {
    /// This class's map in `model`.
    fn map(model: &ConnectionModel) -> &ClassMap<Self>;
    /// This class's map in `model`, mutably.
    fn map_mut(model: &mut ConnectionModel) -> &mut ClassMap<Self>;
}

/// Applies the records loaded for one class to a model.
pub type ApplyLoaded = Box<dyn FnOnce(&mut ConnectionModel) + Send>;

/// The `get_all_records` call of one class, ready to run.
pub struct ClassLoader {
    /// Class name as the API spells it.
    pub class: &'static str,
    /// Fetches the records; the result replaces that class's map.
    pub load: Pin<Box<dyn Future<Output = xapi::Result<ApplyLoaded>> + Send>>,
}

fn wrap<K: std::hash::Hash + Eq, V>(m: HashMap<K, V>) -> HashMap<K, Arc<V>> {
    m.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
}

macro_rules! define_model {
    ($( ($marker:ident, $module:ident, $field:ident) ),* $(,)?) => {
        /// Snapshot of the objects of one connection: one map per cached class.
        ///
        /// Records are behind `Arc`, so cloning the model copies pointers only;
        /// the backend clones it to publish a new immutable snapshot after each
        /// batch.
        #[derive(Debug, Clone, Default)]
        pub struct ConnectionModel {
            $(
                #[doc = concat!("`", stringify!($marker), "` objects.")]
                pub $field: ClassMap<xapi::$marker>,
            )*
        }

        $(
            impl ModelClass for xapi::$marker {
                fn map(model: &ConnectionModel) -> &ClassMap<Self> {
                    &model.$field
                }
                fn map_mut(model: &mut ConnectionModel) -> &mut ClassMap<Self> {
                    &mut model.$field
                }
            }
        )*

        impl ConnectionModel {
            /// Applies one event to the map of its class. `None` if the model
            /// does not keep that class.
            fn apply_class_event(&mut self, event: &Event) -> Option<bool> {
                $(
                    if <xapi::$marker as XapiClass>::is_class(&event.class) {
                        return Some(apply(&mut self.$field, event));
                    }
                )*
                None
            }

            /// Number of objects of each class, in datamodel order.
            pub fn class_counts(&self) -> Vec<(&'static str, usize)> {
                vec![$( (<xapi::$marker as XapiClass>::NAME, self.$field.len()), )*]
            }

            /// Total number of cached objects.
            pub fn object_count(&self) -> usize {
                0 $( + self.$field.len() )*
            }
        }

        /// One `get_all_records` call per cached class, for the initial load.
        pub fn class_loaders(session: &Session) -> Vec<ClassLoader> {
            vec![$(
                ClassLoader {
                    class: <xapi::$marker as XapiClass>::NAME,
                    load: {
                        let session = session.clone();
                        Box::pin(async move {
                            let records = xapi::$module::get_all_records(&session).await?;
                            let apply: ApplyLoaded = Box::new(move |model: &mut ConnectionModel| {
                                model.$field = wrap(records);
                            });
                            Ok(apply)
                        })
                    },
                },
            )*]
        }
    };
}

xapi::for_each_cached_class!(define_model);

impl ConnectionModel {
    /// Builds a model from a fixture-style database
    /// (`{"<class>": {"OpaqueRef:…": record, …}, …}`, class names in any
    /// case). Unknown classes and undecodable records are skipped.
    pub fn from_database(db: &serde_json::Value) -> Self {
        let mut model = Self::default();
        let Some(classes) = db.as_object() else {
            return model;
        };
        for (class, objects) in classes {
            let Some(objects) = objects.as_object() else {
                continue;
            };
            for (opaque_ref, record) in objects {
                model.apply_event(&Event {
                    id: serde_json::Value::Null,
                    class: class.clone(),
                    operation: EventOperation::Add,
                    opaque_ref: opaque_ref.clone(),
                    snapshot: Some(record.clone()),
                });
            }
        }
        model
    }

    /// The record behind `r`, if cached.
    pub fn get<T: ModelClass>(&self, r: &Ref<T>) -> Option<&Arc<T::Record>> {
        T::map(self).get(r)
    }

    /// Every cached object of class `T`.
    pub fn all<T: ModelClass>(&self) -> &ClassMap<T> {
        T::map(self)
    }

    /// Applies a batch of events; returns whether anything changed.
    pub fn apply_events(&mut self, events: &[Event]) -> bool {
        let mut changed = false;
        for event in events {
            changed |= self.apply_event(event);
        }
        changed
    }

    /// Applies one event; returns whether anything changed. Events of
    /// classes the model does not keep are ignored.
    pub fn apply_event(&mut self, event: &Event) -> bool {
        self.apply_class_event(event).unwrap_or(false)
    }

    /// The pool (every XAPI server has exactly one pool object).
    pub fn pool(&self) -> Option<(&Ref<Pool>, &Arc<PoolRecord>)> {
        self.pools.iter().min_by(|a, b| a.0.cmp(b.0))
    }

    /// The pool coordinator's host record.
    pub fn coordinator(&self) -> Option<&Arc<HostRecord>> {
        self.pool().and_then(|(_, p)| self.hosts.get(&p.master))
    }

    /// Builds the infrastructure tree, as the original's Infrastructure view:
    /// pool → hosts → (VMs, local SRs); VMs without a home server, shared
    /// SRs and templates hang off the pool. Snapshots, when shown, sit under
    /// the VM they were taken of.
    ///
    /// Control domains are never shown; templates, snapshots, local storage
    /// and hidden objects follow `options`, and `options.filter` keeps only
    /// objects whose name contains it (case-insensitive) plus their parents.
    /// A matching pool or host keeps all its children.
    pub fn tree(&self, options: &TreeOptions) -> Vec<TreePool> {
        let hidden = options.hidden_objects;
        let mut hosts: HashMap<&Ref<Host>, TreeHost> = self
            .hosts
            .iter()
            .map(|(r, h)| {
                (
                    r,
                    TreeHost {
                        host_ref: r.clone(),
                        name: h.name_label.clone(),
                        enabled: h.enabled,
                        live: objects::host_is_live(self, h),
                        maintenance: objects::host_in_maintenance(h),
                        vms: Vec::new(),
                        srs: Vec::new(),
                    },
                )
            })
            .collect();

        // Snapshots by the VM they were taken of.
        let mut snapshots: HashMap<&Ref<Vm>, Vec<TreeVm>> = HashMap::new();
        let mut orphan_snapshots = Vec::new();
        if options.snapshots {
            for (vm_ref, vm) in &self.vms {
                if objects::vm_kind(vm) != VmKind::Snapshot || !objects::vm_is_shown(vm, hidden) {
                    continue;
                }
                let node = TreeVm::new(vm_ref, vm);
                match self.vms.get_key_value(&vm.snapshot_of) {
                    Some((parent, _)) => snapshots.entry(parent).or_default().push(node),
                    None => orphan_snapshots.push(node),
                }
            }
        }

        let mut pool_vms = Vec::new();
        let mut templates = Vec::new();
        for (vm_ref, vm) in &self.vms {
            if !objects::vm_is_shown(vm, hidden) {
                continue;
            }
            match objects::vm_kind(vm) {
                VmKind::Guest => {
                    let mut node = TreeVm::new(vm_ref, vm);
                    node.snapshots = snapshots.remove(vm_ref).unwrap_or_default();
                    match objects::vm_home(self, vm).and_then(|h| hosts.get_mut(h)) {
                        Some(host) => host.vms.push(node),
                        None => pool_vms.push(node),
                    }
                }
                VmKind::UserTemplate if options.user_templates => {
                    templates.push(TreeVm::new(vm_ref, vm));
                }
                VmKind::DefaultTemplate if options.default_templates => {
                    templates.push(TreeVm::new(vm_ref, vm));
                }
                _ => {}
            }
        }
        // Snapshots of hidden or non-guest VMs.
        for nodes in snapshots.into_values() {
            orphan_snapshots.extend(nodes);
        }
        templates.extend(orphan_snapshots);

        let mut pool_srs = Vec::new();
        for (sr_ref, sr) in &self.srs {
            if objects::is_tools_sr(sr) || !objects::sr_is_shown(sr, hidden) {
                continue;
            }
            if objects::is_local_sr(sr) && !options.local_storage {
                continue;
            }
            let node = TreeSr {
                sr_ref: sr_ref.clone(),
                name: sr.name_label.clone(),
                icon: objects::sr_icon(self, sr_ref, sr),
            };
            match objects::sr_home(self, sr).and_then(|h| hosts.get_mut(h)) {
                Some(host) => host.srs.push(node),
                None => pool_srs.push(node),
            }
        }

        let order: HashMap<Ref<Host>, usize> = objects::sorted_hosts(self)
            .into_iter()
            .enumerate()
            .map(|(i, (r, _))| (r.clone(), i))
            .collect();
        let mut hosts: Vec<TreeHost> = hosts.into_values().collect();
        hosts.sort_by_key(|h| order.get(&h.host_ref).copied());
        for host in &mut hosts {
            self.sort_vms(&mut host.vms);
            sort_srs(&mut host.srs);
        }
        self.sort_vms(&mut pool_vms);
        self.sort_vms(&mut templates);
        sort_srs(&mut pool_srs);

        let mut pools: Vec<TreePool> = self
            .pools
            .iter()
            .map(|(pool_ref, pool)| TreePool {
                pool_ref: pool_ref.clone(),
                name: if pool.name_label.is_empty() {
                    self.hosts
                        .get(&pool.master)
                        .map(|h| h.name_label.clone())
                        .unwrap_or_default()
                } else {
                    pool.name_label.clone()
                },
                standalone: pool.name_label.is_empty(),
                hosts: Vec::new(),
                vms: Vec::new(),
                srs: Vec::new(),
                templates: Vec::new(),
            })
            .collect();
        pools.sort_by(|a, b| a.pool_ref.cmp(&b.pool_ref));
        // One pool per connection; any extra pool object would be a server
        // bug, so everything hangs off the first one.
        if let Some(first) = pools.first_mut() {
            first.hosts = hosts;
            first.vms = pool_vms;
            first.srs = pool_srs;
            first.templates = templates;
        }
        let needle = options.filter.trim().to_lowercase();
        if needle.is_empty() {
            return pools;
        }
        pools
            .into_iter()
            .filter_map(|p| p.retain_matching(&needle))
            .collect()
    }

    fn sort_vms(&self, vms: &mut [TreeVm]) {
        let record = |n: &TreeVm| self.vms.get(&n.vm_ref).cloned();
        vms.sort_by(|a, b| match (record(a), record(b)) {
            (Some(ra), Some(rb)) => objects::vm_cmp((&a.vm_ref, &ra), (&b.vm_ref, &rb)),
            _ => name_order(&a.name, &b.name).then_with(|| a.vm_ref.cmp(&b.vm_ref)),
        });
        for vm in vms {
            self.sort_vms(&mut vm.snapshots);
        }
    }
}

/// Whether a VM record is a real guest (not a template, snapshot or control domain).
pub fn is_guest(vm: &VmRecord) -> bool {
    !vm.is_a_template && !vm.is_a_snapshot && !vm.is_control_domain
}

/// Whether "Start" should be offered for this VM.
pub fn can_start(vm: &VmRecord) -> bool {
    is_guest(vm)
        && vm.power_state == VmPowerState::Halted
        && vm.allowed_operations.contains(&VmOperations::Start)
}

/// Whether "Clean shutdown" should be offered for this VM.
pub fn can_clean_shutdown(vm: &VmRecord) -> bool {
    is_guest(vm)
        && vm.power_state == VmPowerState::Running
        && vm.allowed_operations.contains(&VmOperations::CleanShutdown)
}

fn apply<M, T: DeserializeOwned>(map: &mut HashMap<Ref<M>, Arc<T>>, event: &Event) -> bool {
    let key = Ref::new(event.opaque_ref.clone());
    match &event.operation {
        EventOperation::Del => map.remove(&key).is_some(),
        EventOperation::Add | EventOperation::Mod => {
            match event.snapshot.as_ref().filter(|s| !s.is_null()) {
                Some(snapshot) => match serde_json::from_value::<T>(snapshot.clone()) {
                    Ok(record) => {
                        map.insert(key, Arc::new(record));
                        true
                    }
                    Err(e) => {
                        tracing::warn!(class = %event.class, error = %e, "ignoring undecodable event snapshot");
                        false
                    }
                },
                None => false,
            }
        }
        EventOperation::Unknown(op) => {
            tracing::debug!(class = %event.class, op, "ignoring unknown event operation");
            false
        }
    }
}

/// Case-insensitive natural ordering for display names.
fn name_order(a: &str, b: &str) -> std::cmp::Ordering {
    objects::natural_cmp(a, b).then_with(|| a.cmp(b))
}

fn sort_srs(srs: &mut [TreeSr]) {
    srs.sort_by(|a, b| name_order(&a.name, &b.name).then_with(|| a.sr_ref.cmp(&b.sr_ref)));
}

fn matches(name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
}

/// What the infrastructure tree shows (the original's View menu) and the
/// name filter typed above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeOptions {
    /// "Custom Templates" (on by default).
    pub user_templates: bool,
    /// "XCP-ng Templates", the built-in ones (off by default).
    pub default_templates: bool,
    /// "Local Storage" (on by default).
    pub local_storage: bool,
    /// Snapshots under their VM (off by default).
    pub snapshots: bool,
    /// "Hidden Objects" (`HideFromXenCenter`; off by default).
    pub hidden_objects: bool,
    /// Keep only objects whose name contains this (case-insensitive).
    pub filter: String,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            user_templates: true,
            default_templates: false,
            local_storage: true,
            snapshots: false,
            hidden_objects: false,
            filter: String::new(),
        }
    }
}

/// A pool node of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreePool {
    pub pool_ref: Ref<Pool>,
    /// Pool name, or the coordinator's name for an unnamed (standalone) pool.
    pub name: String,
    /// The pool has no name: a single host not deliberately made into a pool.
    pub standalone: bool,
    pub hosts: Vec<TreeHost>,
    /// VMs without a home server.
    pub vms: Vec<TreeVm>,
    /// Shared storage.
    pub srs: Vec<TreeSr>,
    /// Templates (user first, then built-in) and snapshots of VMs not in the
    /// tree.
    pub templates: Vec<TreeVm>,
}

impl TreePool {
    fn retain_matching(mut self, needle: &str) -> Option<Self> {
        if matches(&self.name, needle) {
            return Some(self);
        }
        self.hosts = std::mem::take(&mut self.hosts)
            .into_iter()
            .filter_map(|h| h.retain_matching(needle))
            .collect();
        self.vms = retain_vms(std::mem::take(&mut self.vms), needle);
        self.templates = retain_vms(std::mem::take(&mut self.templates), needle);
        self.srs.retain(|s| matches(&s.name, needle));
        let empty = self.hosts.is_empty()
            && self.vms.is_empty()
            && self.templates.is_empty()
            && self.srs.is_empty();
        (!empty).then_some(self)
    }

    /// Every VM node (guests, their snapshots, templates) in display order.
    pub fn all_vms(&self) -> Vec<&TreeVm> {
        let mut out = Vec::new();
        fn push<'a>(out: &mut Vec<&'a TreeVm>, vms: &'a [TreeVm]) {
            for vm in vms {
                out.push(vm);
                push(out, &vm.snapshots);
            }
        }
        for host in &self.hosts {
            push(&mut out, &host.vms);
        }
        push(&mut out, &self.vms);
        push(&mut out, &self.templates);
        out
    }
}

fn retain_vms(vms: Vec<TreeVm>, needle: &str) -> Vec<TreeVm> {
    vms.into_iter()
        .filter_map(|mut vm| {
            if matches(&vm.name, needle) {
                return Some(vm);
            }
            vm.snapshots.retain(|s| matches(&s.name, needle));
            (!vm.snapshots.is_empty()).then_some(vm)
        })
        .collect()
}

/// A host node of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeHost {
    pub host_ref: Ref<Host>,
    pub name: String,
    pub enabled: bool,
    /// `host_metrics.live`.
    pub live: bool,
    /// Disabled or being evacuated: shown with the maintenance icon.
    pub maintenance: bool,
    pub vms: Vec<TreeVm>,
    /// Local storage of this host.
    pub srs: Vec<TreeSr>,
}

impl TreeHost {
    fn retain_matching(mut self, needle: &str) -> Option<Self> {
        if matches(&self.name, needle) {
            return Some(self);
        }
        self.vms = retain_vms(std::mem::take(&mut self.vms), needle);
        self.srs.retain(|s| matches(&s.name, needle));
        (!self.vms.is_empty() || !self.srs.is_empty()).then_some(self)
    }
}

/// A VM, template or snapshot node of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeVm {
    pub vm_ref: Ref<Vm>,
    pub name: String,
    pub power_state: VmPowerState,
    pub icon: VmIcon,
    /// Snapshots of this VM (only when snapshots are shown).
    pub snapshots: Vec<TreeVm>,
}

impl TreeVm {
    fn new(vm_ref: &Ref<Vm>, vm: &VmRecord) -> Self {
        Self {
            vm_ref: vm_ref.clone(),
            name: vm.name_label.clone(),
            power_state: vm.power_state.clone(),
            icon: objects::vm_icon(vm),
            snapshots: Vec::new(),
        }
    }
}

/// A storage repository node of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeSr {
    pub sr_ref: Ref<Sr>,
    pub name: String,
    pub icon: SrIcon,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture_model() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    fn names(vms: &[TreeVm]) -> Vec<&str> {
        vms.iter().map(|v| v.name.as_str()).collect()
    }

    #[test]
    fn tree_excludes_templates_snapshots_and_control_domains() {
        let tree = fixture_model().tree(&TreeOptions::default());
        assert_eq!(tree.len(), 1);
        let pool = &tree[0];
        assert_eq!(pool.name, "Lab Pool");
        assert!(!pool.standalone);
        let hosts: Vec<&str> = pool.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(hosts, ["xcp-host-01", "xcp-host-02"]);
        // Running VM under its host; suspended VM under its home server.
        assert_eq!(names(&pool.hosts[0].vms), ["legacy-app", "web-01"]);
        // Halted VM with affinity sits under that host.
        assert_eq!(names(&pool.hosts[1].vms), ["db-01"]);
        // Halted VM without affinity sits under the pool.
        assert_eq!(names(&pool.vms), ["build-runner"]);
        // Local storage under its host, shared storage under the pool.
        let srs = |s: &[TreeSr]| s.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        assert_eq!(srs(&pool.hosts[0].srs), ["Local storage"]);
        assert_eq!(srs(&pool.hosts[1].srs), ["Local storage"]);
        assert_eq!(srs(&pool.srs), ["ISO library", "NFS VM storage"]);
        // Custom templates are shown by default, built-in ones are not;
        // snapshots are not.
        assert_eq!(names(&pool.templates), ["Web server template"]);
        assert!(pool.hosts[0].vms.iter().all(|v| v.snapshots.is_empty()));
        // States for the icons.
        assert!(!pool.hosts[0].maintenance && pool.hosts[1].maintenance);
        assert!(pool.hosts.iter().all(|h| h.live));
        assert_eq!(pool.srs[0].icon, SrIcon::Broken);
        assert_eq!(pool.srs[1].icon, SrIcon::Default);
        assert_eq!(pool.hosts[0].vms[1].icon, VmIcon::Running);
        assert_eq!(pool.hosts[0].vms[0].icon, VmIcon::Suspended);
    }

    #[test]
    fn view_options_show_templates_snapshots_storage_and_hidden_objects() {
        let mut model = fixture_model();
        let all = TreeOptions {
            user_templates: true,
            default_templates: true,
            local_storage: true,
            snapshots: true,
            hidden_objects: false,
            filter: String::new(),
        };
        let pool = &model.tree(&all)[0];
        assert_eq!(
            names(&pool.templates),
            ["Web server template", "Debian Bookworm 12"]
        );
        let web = pool.hosts[0]
            .vms
            .iter()
            .find(|v| v.name == "web-01")
            .unwrap();
        assert_eq!(names(&web.snapshots), ["web-01 before upgrade"]);
        assert_eq!(
            web.snapshots[0].icon,
            VmIcon::Snapshot { with_memory: false }
        );

        let none = TreeOptions {
            user_templates: false,
            local_storage: false,
            ..TreeOptions::default()
        };
        let pool = &model.tree(&none)[0];
        assert!(pool.templates.is_empty());
        assert!(pool.hosts.iter().all(|h| h.srs.is_empty()));
        assert_eq!(pool.srs.len(), 2);

        // A hidden VM appears only with hidden objects on.
        let mut build = (*model.vms[&Ref::new("OpaqueRef:vm-build")]).clone();
        build
            .other_config
            .insert("HideFromXenCenter".into(), "true".into());
        model
            .vms
            .insert(Ref::new("OpaqueRef:vm-build"), Arc::new(build));
        assert!(model.tree(&TreeOptions::default())[0].vms.is_empty());
        let hidden = TreeOptions {
            hidden_objects: true,
            ..TreeOptions::default()
        };
        assert_eq!(names(&model.tree(&hidden)[0].vms), ["build-runner"]);
    }

    #[test]
    fn name_filter_keeps_matches_and_their_parents() {
        let model = fixture_model();
        let filter = |f: &str| {
            model.tree(&TreeOptions {
                filter: f.into(),
                snapshots: true,
                ..TreeOptions::default()
            })
        };
        // A VM: only it and its parents remain.
        let tree = filter("WEB");
        let pool = &tree[0];
        assert_eq!(pool.hosts.len(), 1);
        assert_eq!(names(&pool.hosts[0].vms), ["web-01"]);
        assert!(pool.hosts[0].srs.is_empty() && pool.srs.is_empty() && pool.vms.is_empty());
        // The VM matches, so its snapshot stays too; the template matches.
        assert_eq!(pool.hosts[0].vms[0].snapshots.len(), 1);
        assert_eq!(names(&pool.templates), ["Web server template"]);
        // Only a snapshot matches: its VM stays as its parent.
        let tree = filter("before upgrade");
        assert_eq!(names(&tree[0].hosts[0].vms), ["web-01"]);
        // A host keeps everything under it.
        let tree = filter("host-02");
        assert_eq!(tree[0].hosts.len(), 1);
        assert_eq!(names(&tree[0].hosts[0].vms), ["db-01"]);
        assert_eq!(tree[0].hosts[0].srs.len(), 1);
        // Storage.
        let tree = filter("nfs");
        assert_eq!(tree[0].srs.len(), 1);
        assert!(tree[0].hosts.is_empty());
        // The pool keeps everything; no match leaves nothing.
        assert_eq!(filter("lab pool")[0].hosts.len(), 2);
        assert!(filter("no such thing").is_empty());
        // Whitespace only: no filter.
        assert_eq!(filter("  ")[0].hosts.len(), 2);
    }

    #[test]
    fn events_update_the_tree() {
        let mut model = fixture_model();
        let mut snapshot =
            serde_json::to_value(&*model.vms[&Ref::new("OpaqueRef:vm-build")]).unwrap();
        snapshot["power_state"] = json!("Running");
        snapshot["resident_on"] = json!("OpaqueRef:host-0002");
        let event: Event = serde_json::from_value(json!({
            "class": "vm", "operation": "mod", "ref": "OpaqueRef:vm-build", "snapshot": snapshot
        }))
        .unwrap();
        assert!(model.apply_event(&event));
        let pool = &model.tree(&TreeOptions::default())[0];
        assert!(pool.vms.is_empty());
        assert_eq!(names(&pool.hosts[1].vms), ["build-runner", "db-01"]);
        assert_eq!(pool.hosts[1].vms[0].power_state, VmPowerState::Running);

        let del: Event = serde_json::from_value(
            json!({"class": "VM", "operation": "del", "ref": "OpaqueRef:vm-db"}),
        )
        .unwrap();
        assert!(model.apply_event(&del));
        assert_eq!(
            names(&model.tree(&TreeOptions::default())[0].hosts[1].vms),
            ["build-runner"]
        );

        let unknown: Event = serde_json::from_value(
            json!({"class": "session", "operation": "add", "ref": "OpaqueRef:s", "snapshot": {}}),
        )
        .unwrap();
        assert!(!model.apply_event(&unknown));
    }

    #[test]
    fn capture_fills_every_delivered_class() {
        let db = xapi_mock::capture_records();
        let model = ConnectionModel::from_database(&db);
        let counts: HashMap<&str, usize> = model.class_counts().into_iter().collect();
        for (class, objects) in db.as_object().unwrap() {
            if class.starts_with('_') {
                continue;
            }
            let expected = objects.as_object().unwrap().len();
            let cached = counts
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(class))
                .map(|(_, n)| *n);
            match class.as_str() {
                // Classes the datamodel marks removed: still sent, not cached.
                "vbd_metrics" | "vif_metrics" => assert_eq!(cached, None, "{class}"),
                _ => assert_eq!(cached, Some(expected), "{class}"),
            }
        }
        assert_eq!(model.srs.len(), 6);
        assert_eq!(model.roles.len(), 7);
        // Typed lookups across classes.
        let (vm_ref, vm) = model
            .vms
            .iter()
            .find(|(_, v)| v.name_label == "guest-01")
            .unwrap();
        let metrics = model.get(&vm.metrics).expect("VM_metrics of guest-01");
        assert_eq!(
            metrics.memory_actual > 0,
            vm.power_state == VmPowerState::Running
        );
        for vbd_ref in &vm.vbds {
            let vbd = model.get(vbd_ref).expect("VBD");
            assert_eq!(&vbd.vm, vm_ref);
        }
        let host = model.coordinator().unwrap();
        assert_eq!(host.name_label, "xcp-host-01");
        assert!(model.get(&host.metrics).unwrap().memory_total > 0);
        let tree = model.tree(&TreeOptions::default());
        assert_eq!(tree[0].name, "Lab Pool");
        let guests: usize =
            tree[0].hosts.iter().map(|h| h.vms.len()).sum::<usize>() + tree[0].vms.len();
        assert_eq!(guests, 10);
    }

    #[test]
    fn events_of_uncached_classes_are_ignored() {
        let mut model = fixture_model();
        for class in ["session", "secret", "user", "vbd_metrics", "no_such_class"] {
            let event: Event = serde_json::from_value(json!({
                "class": class, "operation": "add", "ref": "OpaqueRef:x", "snapshot": {"uuid": "u"}
            }))
            .unwrap();
            assert!(!model.apply_event(&event), "{class}");
        }
        let sr: Event = serde_json::from_value(json!({
            "class": "SR", "operation": "add", "ref": "OpaqueRef:sr-new",
            "snapshot": {"name_label": "NFS", "type": "nfs", "physical_size": "1000"}
        }))
        .unwrap();
        assert!(model.apply_event(&sr));
        let sr = model
            .get(&Ref::<xapi::Sr>::new("OpaqueRef:sr-new"))
            .unwrap();
        assert_eq!((sr.name_label.as_str(), sr.r#type.as_str()), ("NFS", "nfs"));
        // Lenient decoding: an integer sent as a string.
        assert_eq!(sr.physical_size, 1000);
    }

    #[test]
    fn standalone_pool_is_named_after_its_host() {
        let mut model = fixture_model();
        let (pool_ref, pool) = model
            .pool()
            .map(|(r, p)| (r.clone(), (**p).clone()))
            .unwrap();
        model.pools.insert(
            pool_ref,
            Arc::new(PoolRecord {
                name_label: String::new(),
                ..pool
            }),
        );
        let tree = model.tree(&TreeOptions::default());
        assert!(tree[0].standalone);
        assert_eq!(tree[0].name, "xcp-host-01");
    }

    #[test]
    fn button_rules_follow_power_state_and_allowed_operations() {
        let model = fixture_model();
        let vm = |r: &str| model.vms[&Ref::new(r)].clone();
        assert!(can_start(&vm("OpaqueRef:vm-db")));
        assert!(!can_clean_shutdown(&vm("OpaqueRef:vm-db")));
        assert!(can_clean_shutdown(&vm("OpaqueRef:vm-web")));
        assert!(!can_start(&vm("OpaqueRef:vm-web")));
        // Suspended: neither.
        assert!(!can_start(&vm("OpaqueRef:vm-legacy")));
        assert!(!can_clean_shutdown(&vm("OpaqueRef:vm-legacy")));
        // Control domain and template: never.
        assert!(!can_clean_shutdown(&vm("OpaqueRef:vm-dom0-01")));
        assert!(!can_start(&vm("OpaqueRef:vm-template-debian")));
        // Halted but "start" not allowed right now (e.g. operation in progress).
        let mut busy = (*vm("OpaqueRef:vm-db")).clone();
        busy.allowed_operations.clear();
        assert!(!can_start(&busy));
    }
}
