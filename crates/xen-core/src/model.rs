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
    Event, EventOperation, Host, HostRecord, Pool, PoolRecord, Ref, Session, Vm, VmOperations,
    VmPowerState, VmRecord, XapiClass, XapiObject,
};

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

    /// Builds the infrastructure tree: pool → hosts → VMs.
    ///
    /// Templates, snapshots and control domains are left out. A VM sits under
    /// the host it runs on; a VM that is not running sits under its home server
    /// (`affinity`) when that host exists, otherwise directly under the pool.
    pub fn tree(&self) -> Vec<TreePool> {
        let mut pools: Vec<TreePool> = self
            .pools
            .iter()
            .map(|(pool_ref, pool)| {
                let coordinator = self.hosts.get(&pool.master);
                TreePool {
                    pool_ref: pool_ref.clone(),
                    name: if pool.name_label.is_empty() {
                        coordinator
                            .map(|h| h.name_label.clone())
                            .unwrap_or_default()
                    } else {
                        pool.name_label.clone()
                    },
                    standalone: pool.name_label.is_empty(),
                    hosts: Vec::new(),
                    vms: Vec::new(),
                }
            })
            .collect();
        pools.sort_by(|a, b| a.pool_ref.cmp(&b.pool_ref));

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
                        vms: Vec::new(),
                    },
                )
            })
            .collect();
        let mut pool_vms = Vec::new();
        for (vm_ref, vm) in &self.vms {
            if !is_guest(vm) {
                continue;
            }
            let node = TreeVm {
                vm_ref: vm_ref.clone(),
                name: vm.name_label.clone(),
                power_state: vm.power_state.clone(),
            };
            match self.home_host(vm).and_then(|h| hosts.get_mut(h)) {
                Some(host) => host.vms.push(node),
                None => pool_vms.push(node),
            }
        }

        let mut hosts: Vec<TreeHost> = hosts.into_values().collect();
        for host in &mut hosts {
            host.vms
                .sort_by(|a, b| name_order(&a.name, &b.name).then_with(|| a.vm_ref.cmp(&b.vm_ref)));
        }
        hosts
            .sort_by(|a, b| name_order(&a.name, &b.name).then_with(|| a.host_ref.cmp(&b.host_ref)));
        pool_vms.sort_by(|a, b| name_order(&a.name, &b.name).then_with(|| a.vm_ref.cmp(&b.vm_ref)));

        // F1 handles one pool per connection; any extra pool object would be a
        // server bug, so everything hangs off the first one.
        if let Some(first) = pools.first_mut() {
            first.hosts = hosts;
            first.vms = pool_vms;
        }
        pools
    }

    fn home_host<'a>(&'a self, vm: &'a VmRecord) -> Option<&'a Ref<Host>> {
        let running = matches!(vm.power_state, VmPowerState::Running | VmPowerState::Paused);
        if running && self.hosts.contains_key(&vm.resident_on) {
            return Some(&vm.resident_on);
        }
        vm.affinity
            .non_null()
            .filter(|h| self.hosts.contains_key(*h))
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

/// Case-insensitive ordering for display names.
fn name_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
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
}

/// A host node of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeHost {
    pub host_ref: Ref<Host>,
    pub name: String,
    pub enabled: bool,
    pub vms: Vec<TreeVm>,
}

/// A VM leaf of the infrastructure tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeVm {
    pub vm_ref: Ref<Vm>,
    pub name: String,
    pub power_state: VmPowerState,
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
        let tree = fixture_model().tree();
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
        let pool = &model.tree()[0];
        assert!(pool.vms.is_empty());
        assert_eq!(names(&pool.hosts[1].vms), ["build-runner", "db-01"]);
        assert_eq!(pool.hosts[1].vms[0].power_state, VmPowerState::Running);

        let del: Event = serde_json::from_value(
            json!({"class": "VM", "operation": "del", "ref": "OpaqueRef:vm-db"}),
        )
        .unwrap();
        assert!(model.apply_event(&del));
        assert_eq!(names(&model.tree()[0].hosts[1].vms), ["build-runner"]);

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
        let tree = model.tree();
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
        let tree = model.tree();
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
