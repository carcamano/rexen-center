//! Disaster recovery (F12b) in the mock: pool metadata on shared storage,
//! `DR_task` and the recovery of VMs and vApps into another pool.
//!
//! **Shared storage.** Mock servers keep their own storage world, so the
//! storage two pools share lives in a process-wide *fabric*: while a pool
//! replicates its database to an SR, every shared SR of that pool (NFS,
//! iSCSI, HBA) is in the fabric with its location (the place its
//! `device_config` designates), its disks and the metadata databases
//! written to it. The fabric is refreshed after every call the replicating
//! server answers (and by [`crate::MockServer::sync_metadata`]), as XAPI
//! keeps the metadata VDI up to date. Any mock that probes, introduces or
//! plugs an SR at a fabric location finds it there, with its disks, as two
//! pools on one SAN would.
//!
//! **Replication.** `SR.assert_supports_database_replication(sr)`: the SR
//! must be shared (`SR_OPERATION_NOT_SUPPORTED`), of a type whose SM plugin
//! offers `VDI_GENERATE_CONFIG` (NFS, iSCSI, HBA: else
//! `SR_OPERATION_NOT_SUPPORTED`), and plugged on every server
//! (`SR_HAS_NO_PBDS`). `SR.enable_database_replication(sr)` checks the same,
//! refuses a ninth SR (`NO_MORE_REDO_LOGS_ALLOWED`), and makes the metadata
//! VDI ("Metadata for DR", type `metadata`, `metadata_of_pool` this pool,
//! `metadata_latest`) attached to the coordinator's control domain, listed
//! in the pool's `metadata_VDIs`. `SR.disable_database_replication(sr)`
//! destroys it. Seen from another pool (after an introduce and plug, or a
//! scan) that VDI is not attached and belongs to no pool it knows.
//!
//! **Probing.** `SR.probe` with `sm_config["metadata"] = "true"` also lists
//! the fabric's SRs at the probed place, with `<pool_metadata_detected>`
//! true when one holds a metadata database.
//!
//! **`DR_task`.** `DR_task.create(type, device_config, whitelist)`
//! introduces the fabric SRs at that place whose UUID is whitelisted and
//! that this pool does not know: the SR, a plugged PBD per server with
//! `device_config`, its disks; the task's `introduced_SRs` lists them.
//! `DR_task.destroy` detaches and forgets the introduced SRs no VM uses any
//! more, then goes. A place with no SR fails as `SR.probe` would.
//!
//! **Metadata sessions.** `VDI.open_database(vdi)` on a metadata VDI of
//! another pool (`VDI_INCOMPATIBLE_TYPE` for another VDI,
//! `VDI_CONTAINS_METADATA_OF_THIS_POOL` for this pool's own, `VDI_MISSING`
//! when the fabric has no database for it) returns a session on that
//! database. On it: every class's `get_all_records`, `get_all`,
//! `get_record` and field getters answer from the database;
//! `session.logout` closes it; and, with `session_to` a session of this
//! server (else `SESSION_INVALID`):
//!
//! * `VM.assert_can_be_recovered(vm, session_to)` and
//!   `VM_appliance.assert_can_be_recovered`: every disk of the VM (of each
//!   member) must be in an SR this pool has plugged, found by UUID, else
//!   `VM_REQUIRES_SR [vm, sr]` (references of the database);
//!   `get_SRs_required_for_recovery` lists those SRs;
//! * `VM.recover(vm, session_to, force)` / `VM_appliance.recover` (and
//!   their `Async.*` forms, tasks of this server's database): the checks
//!   above; a VM with the same UUID here is replaced when `force` (it must
//!   be halted: `VM_BAD_POWER_STATE`), else `VM_TO_IMPORT_IS_NOT_NEWER_VERSION`
//!   unless the one recovered is newer. The VM comes back halted with its
//!   UUID, settings, `version` + 1, a drive per disk (the disks found by
//!   UUID; CD drives empty) and its interfaces (MAC kept) on the network
//!   with the same UUID, else the same name; a vApp comes back with its
//!   UUID and its members. A VM made to fail
//!   ([`crate::MockServer::fail_recovery`]) fails its recovery with that
//!   error, leaving nothing behind; a cancelled task recovers nothing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use serde_json::{Map, Value, json};

use crate::provisioning::int;
use crate::{Db, NULL_REF, Reply, State, api_error, str_param};

type Error = (String, Vec<String>);

fn err(code: &str, params: &[&str]) -> Error {
    (
        code.to_owned(),
        params.iter().map(|s| (*s).to_owned()).collect(),
    )
}

/// The SR types whose SM plugin offers `VDI_GENERATE_CONFIG`.
const DR_TYPES: &[&str] = &["nfs", "lvmoiscsi", "lvmohba", "gfs2"];
/// XAPI's limit of SRs a pool replicates to.
const MAX_REDO_LOGS: usize = 8;
/// The classes a metadata database holds.
const CLASSES: &[&str] = &[
    "pool",
    "host",
    "vm",
    "vm_metrics",
    "vm_appliance",
    "vbd",
    "vdi",
    "vif",
    "network",
    "sr",
    "pbd",
];

/// DR state of one mock.
#[derive(Default)]
pub(crate) struct World {
    /// SR uuids this pool replicates to, with the metadata VDI's uuid.
    pub(crate) replicating: Vec<(String, String)>,
    /// Metadata sessions: session → metadata VDI uuid.
    pub(crate) sessions: BTreeMap<String, String>,
    /// VM uuids whose recovery fails, with the error code.
    pub(crate) failing: Vec<(String, String)>,
}

// ---- the fabric -------------------------------------------------------------------------

/// A metadata database on a fabric SR.
#[derive(Clone)]
struct Metadata {
    vdi_uuid: String,
    pool_uuid: String,
    db: BTreeMap<(String, String), Value>,
}

/// A shared SR as the storage sees it.
#[derive(Clone)]
struct FabricSr {
    location: String,
    uuid: String,
    sr_type: String,
    name_label: String,
    name_description: String,
    size: i64,
    /// Its disks' records (without references), by uuid.
    vdis: BTreeMap<String, Map<String, Value>>,
    metadata: Vec<Metadata>,
}

fn fabric() -> MutexGuard<'static, Vec<FabricSr>> {
    static F: OnceLock<Mutex<Vec<FabricSr>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The fabric SRs at `location`: `(uuid, size, name, description,
/// metadata detected)`.
pub(crate) fn srs_at(location: &str) -> Vec<(String, i64, String, String, bool)> {
    fabric()
        .iter()
        .filter(|s| s.location == location)
        .map(|s| {
            (
                s.uuid.clone(),
                s.size,
                s.name_label.clone(),
                s.name_description.clone(),
                !s.metadata.is_empty(),
            )
        })
        .collect()
}

/// Whether the fabric has SR `uuid` at `location`.
pub(crate) fn has(location: &str, uuid: &str) -> bool {
    fabric()
        .iter()
        .any(|s| s.location == location && s.uuid == uuid)
}

/// The record fields of a disk kept in the fabric.
const VDI_FIELDS: &[&str] = &[
    "uuid",
    "name_label",
    "name_description",
    "virtual_size",
    "physical_utilisation",
    "type",
    "sharable",
    "read_only",
    "other_config",
    "sm_config",
    "location",
    "is_a_snapshot",
    "metadata_latest",
    "on_boot",
    "allow_caching",
    "cbt_enabled",
];

fn text(rec: &Map<String, Value>, field: &str) -> String {
    rec.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn refs(rec: &Map<String, Value>, field: &str) -> Vec<String> {
    rec.get(field)
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Refreshes what the fabric holds of a replicating server (its shared
/// SRs, their disks and its metadata databases), then lists on this
/// server the fabric disks of its attached SRs it does not list yet, as
/// XAPI's periodic SR scan would.
pub(crate) fn sync(state: &Arc<State>) {
    publish(state);
    let known: Vec<String> = fabric().iter().map(|s| s.uuid.clone()).collect();
    if known.is_empty() {
        return;
    }
    let mut db = state.db();
    let mine: Vec<(String, String)> = db
        .objects
        .iter()
        .filter(|((c, _), rec)| {
            c == "sr"
                && rec
                    .get("uuid")
                    .and_then(Value::as_str)
                    .is_some_and(|u| known.iter().any(|k| k == u))
        })
        .map(|((_, r), rec)| {
            (
                r.clone(),
                rec.get("uuid")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    for (sr, uuid) in mine {
        let attached = db.record("sr", &sr).is_some_and(|r| {
            refs(r, "PBDs").iter().any(|p| {
                db.record("pbd", p).is_some_and(|p| {
                    p.get("currently_attached").and_then(Value::as_bool) == Some(true)
                })
            })
        });
        if attached {
            db.add_fabric_vdis(&sr, &uuid);
        }
    }
}

/// The replicating server's part of [`sync`].
fn publish(state: &Arc<State>) {
    let (pool_uuid, snapshot, shared, replicating) = {
        let db = state.db();
        if db.dr.replicating.is_empty() {
            return;
        }
        let pool_uuid = db
            .objects
            .iter()
            .find(|((c, _), _)| c == "pool")
            .and_then(|(_, p)| p.get("uuid").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned();
        let snapshot: BTreeMap<(String, String), Value> = db
            .objects
            .iter()
            .filter(|((c, _), _)| CLASSES.contains(&c.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let shared: Vec<FabricSr> = db
            .objects
            .iter()
            .filter(|((c, _), rec)| {
                c == "sr"
                    && rec.get("shared").and_then(Value::as_bool) == Some(true)
                    && DR_TYPES.contains(&rec.get("type").and_then(Value::as_str).unwrap_or(""))
            })
            .filter_map(|((_, r), rec)| {
                let rec = rec.as_object()?;
                let location = db.sr_location(r)?.key;
                let vdis = refs(rec, "VDIs")
                    .iter()
                    .filter_map(|v| db.record("vdi", v))
                    .map(|v| {
                        let mut out: Map<String, Value> = VDI_FIELDS
                            .iter()
                            .filter_map(|f| v.get(*f).map(|x| ((*f).to_owned(), x.clone())))
                            .collect();
                        out.insert("metadata_of_pool_uuid".into(), json!(pool_uuid));
                        (text(v, "uuid"), out)
                    })
                    .collect();
                Some(FabricSr {
                    location,
                    uuid: text(rec, "uuid"),
                    sr_type: text(rec, "type"),
                    name_label: text(rec, "name_label"),
                    name_description: text(rec, "name_description"),
                    size: int(rec, "physical_size"),
                    vdis,
                    metadata: Vec::new(),
                })
            })
            .collect();
        (pool_uuid, snapshot, shared, db.dr.replicating.clone())
    };
    let mut f = fabric();
    for mut sr in shared {
        let mine: Vec<Metadata> = replicating
            .iter()
            .filter(|(s, _)| *s == sr.uuid)
            .map(|(_, vdi)| Metadata {
                vdi_uuid: vdi.clone(),
                pool_uuid: pool_uuid.clone(),
                db: snapshot.clone(),
            })
            .collect();
        match f.iter_mut().find(|s| s.uuid == sr.uuid) {
            Some(existing) => {
                // Disks of other pools on it stay; this pool's are refreshed.
                existing.vdis.retain(|_, v| {
                    v.get("metadata_of_pool_uuid").and_then(Value::as_str)
                        != Some(pool_uuid.as_str())
                });
                existing.vdis.append(&mut sr.vdis);
                existing.metadata.retain(|m| m.pool_uuid != pool_uuid);
                existing.metadata.extend(mine);
                existing.location = sr.location;
            }
            None => {
                sr.metadata = mine;
                f.push(sr);
            }
        }
    }
}

impl Db {
    fn dr_pool(&self) -> Option<String> {
        self.objects
            .keys()
            .find(|(c, _)| c == "pool")
            .map(|(_, r)| r.clone())
    }

    fn dr_hosts(&self) -> Vec<String> {
        self.objects
            .keys()
            .filter(|(c, _)| c == "host")
            .map(|(_, r)| r.clone())
            .collect()
    }

    fn by_uuid(&self, class: &str, uuid: &str) -> Option<String> {
        self.objects
            .iter()
            .find(|((c, _), rec)| {
                c == class && rec.get("uuid").and_then(Value::as_str) == Some(uuid)
            })
            .map(|((_, r), _)| r.clone())
    }

    /// `SR.assert_supports_database_replication`.
    fn assert_dr(&self, sr: &str) -> Result<(), Error> {
        let Some(rec) = self.record("sr", sr) else {
            return Err(err("HANDLE_INVALID", &["SR", sr]));
        };
        let shared = rec.get("shared").and_then(Value::as_bool) == Some(true);
        if !shared || !DR_TYPES.contains(&text(rec, "type").as_str()) {
            return Err(err("SR_OPERATION_NOT_SUPPORTED", &[sr]));
        }
        let pbds = refs(rec, "PBDs");
        let plugged_on: Vec<String> = pbds
            .iter()
            .filter_map(|p| self.record("pbd", p))
            .filter(|p| p.get("currently_attached").and_then(Value::as_bool) == Some(true))
            .map(|p| text(p, "host"))
            .collect();
        if self.dr_hosts().iter().any(|h| !plugged_on.contains(h)) {
            return Err(err("SR_HAS_NO_PBDS", &[sr]));
        }
        Ok(())
    }
}

/// Whether `class.call` is a DR call of a normal session.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        (
            "SR",
            "assert_supports_database_replication"
                | "enable_database_replication"
                | "disable_database_replication"
        ) | ("DR_task", "create" | "destroy")
            | ("VDI", "open_database" | "read_database_pool_uuid")
    )
}

impl Db {
    /// The DR calls answered on a normal session.
    pub(crate) fn dr_call(&mut self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        let p = |i| str_param(params, i).to_owned();
        Some(match (class, call) {
            ("SR", "assert_supports_database_replication") => {
                self.assert_dr(&p(1)).map(|()| json!(""))
            }
            ("SR", "enable_database_replication") => self.enable_dr(&p(1)),
            ("SR", "disable_database_replication") => self.disable_dr(&p(1)),
            ("DR_task", "create") => self.dr_task_create(params),
            ("DR_task", "destroy") => self.dr_task_destroy(&p(1)),
            ("VDI", "open_database") => self.open_database(&p(1)),
            ("VDI", "read_database_pool_uuid") => {
                let vdi = p(1);
                let Some(uuid) = self.get_str("vdi", &vdi, "uuid") else {
                    return Some(api_error("HANDLE_INVALID", &["VDI", &vdi]));
                };
                match fabric()
                    .iter()
                    .flat_map(|s| s.metadata.iter())
                    .find(|m| m.vdi_uuid == uuid)
                {
                    Some(m) => Ok(json!(m.pool_uuid)),
                    None => api_error("VDI_MISSING", &[&vdi]),
                }
            }
            _ => return None,
        })
    }

    fn enable_dr(&mut self, sr: &str) -> Reply {
        self.assert_dr(sr)?;
        let uuid = self.get_str("sr", sr, "uuid").unwrap_or_default();
        if self.dr.replicating.iter().any(|(s, _)| *s == uuid) {
            return Ok(json!(""));
        }
        if self.dr.replicating.len() >= MAX_REDO_LOGS {
            return api_error("NO_MORE_REDO_LOGS_ALLOWED", &[]);
        }
        let pool = self.dr_pool().unwrap_or_else(|| NULL_REF.to_owned());
        let (vdi, vdi_uuid) = self.new_ref("vdi");
        self.insert(
            "vdi",
            &vdi,
            json!({
                "uuid": vdi_uuid, "name_label": "Metadata for DR",
                "name_description": "Used for disaster recovery", "SR": sr,
                "VBDs": [], "type": "metadata", "virtual_size": 268_435_456_i64,
                "physical_utilisation": 268_435_456_i64, "read_only": false,
                "sharable": false, "managed": true, "missing": false,
                "is_a_snapshot": false, "snapshot_of": NULL_REF, "snapshots": [],
                "other_config": {}, "sm_config": {}, "location": vdi_uuid,
                "metadata_of_pool": pool, "metadata_latest": true,
                "allowed_operations": [], "current_operations": {}, "cbt_enabled": false
            }),
        );
        self.push_ref("sr", sr, "VDIs", &vdi);
        // Attached to the coordinator's control domain while replicating.
        let dom0 = self
            .objects
            .iter()
            .find(|((c, _), rec)| {
                c == "vm" && rec.get("is_control_domain").and_then(Value::as_bool) == Some(true)
            })
            .map(|((_, r), _)| r.clone());
        if let Some(dom0) = dom0 {
            let (vbd, vbd_uuid) = self.new_ref("vbd");
            self.insert(
                "vbd",
                &vbd,
                json!({"uuid": vbd_uuid, "VM": dom0, "VDI": vdi, "type": "Disk", "mode": "RW",
                       "userdevice": "", "bootable": false, "currently_attached": true,
                       "empty": false, "allowed_operations": [], "current_operations": {},
                       "other_config": {}}),
            );
            self.push_ref("vdi", &vdi, "VBDs", &vbd);
            self.push_ref("vm", &dom0, "VBDs", &vbd);
        }
        if self.exists("pool", &pool) {
            self.push_ref("pool", &pool, "metadata_VDIs", &vdi);
        }
        self.dr.replicating.push((uuid, vdi_uuid));
        Ok(json!(""))
    }

    fn disable_dr(&mut self, sr: &str) -> Reply {
        if !self.exists("sr", sr) {
            return api_error("HANDLE_INVALID", &["SR", sr]);
        }
        let uuid = self.get_str("sr", sr, "uuid").unwrap_or_default();
        let Some(i) = self.dr.replicating.iter().position(|(s, _)| *s == uuid) else {
            return Ok(json!(""));
        };
        let (_, vdi_uuid) = self.dr.replicating.remove(i);
        if let Some(vdi) = self.by_uuid("vdi", &vdi_uuid) {
            for vbd in self
                .record("vdi", &vdi)
                .map(|r| refs(r, "VBDs"))
                .unwrap_or_default()
            {
                if let Some(vm) = self.get_str("vbd", &vbd, "VM") {
                    self.drop_ref("vm", &vm, "VBDs", &vbd);
                }
                self.remove("vbd", &vbd);
            }
            if let Some(pool) = self.dr_pool() {
                self.drop_ref("pool", &pool, "metadata_VDIs", &vdi);
            }
            self.destroy_vdi_record(&vdi);
        }
        let mut f = fabric();
        for s in f.iter_mut() {
            s.metadata.retain(|m| m.vdi_uuid != vdi_uuid);
            s.vdis.remove(&vdi_uuid);
        }
        Ok(json!(""))
    }

    /// Adds the fabric's disks of SR `sr` (uuid `uuid`) this pool does not
    /// list yet.
    pub(crate) fn add_fabric_vdis(&mut self, sr: &str, uuid: &str) {
        let my_pool = self
            .dr_pool()
            .and_then(|p| self.get_str("pool", &p, "uuid"))
            .unwrap_or_default();
        let Some(found) = fabric().iter().find(|s| s.uuid == uuid).cloned() else {
            return;
        };
        let known: Vec<String> = self
            .record("sr", sr)
            .map(|r| refs(r, "VDIs"))
            .unwrap_or_default()
            .iter()
            .filter_map(|v| self.get_str("vdi", v, "uuid"))
            .collect();
        for (vdi_uuid, fields) in &found.vdis {
            if known.contains(vdi_uuid) || self.by_uuid("vdi", vdi_uuid).is_some() {
                continue;
            }
            let (vdi, _) = self.new_ref("vdi");
            let mut rec = fields.clone();
            let owner = text(&rec, "metadata_of_pool_uuid");
            rec.remove("metadata_of_pool_uuid");
            rec.insert("SR".into(), json!(sr));
            rec.insert("VBDs".into(), json!([]));
            rec.insert("snapshot_of".into(), json!(NULL_REF));
            rec.insert("snapshots".into(), json!([]));
            rec.insert("managed".into(), json!(true));
            rec.insert("missing".into(), json!(false));
            rec.insert(
                "allowed_operations".into(),
                json!(["destroy", "copy", "snapshot"]),
            );
            rec.insert("current_operations".into(), json!({}));
            let own = owner == my_pool;
            let pool = self.dr_pool().unwrap_or_else(|| NULL_REF.to_owned());
            rec.insert(
                "metadata_of_pool".into(),
                json!(if own && text(&rec, "type") == "metadata" {
                    pool
                } else {
                    NULL_REF.to_owned()
                }),
            );
            self.insert("vdi", &vdi, Value::Object(rec));
            self.push_ref("sr", sr, "VDIs", &vdi);
        }
    }

    fn dr_task_create(&mut self, params: &[Value]) -> Reply {
        let sr_type = str_param(params, 1).to_owned();
        let dconf = params
            .get(2)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let whitelist: Vec<String> = params
            .get(3)
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let located = self.locate_pub(&sr_type, &dconf)?;
        let found: Vec<FabricSr> = fabric()
            .iter()
            .filter(|s| s.location == located)
            .filter(|s| whitelist.is_empty() || whitelist.contains(&s.uuid))
            .cloned()
            .collect();
        let mut introduced = Vec::new();
        for s in found {
            if self.by_uuid("sr", &s.uuid).is_some() {
                continue;
            }
            let (sr, _) = self.new_ref("sr");
            self.insert(
                "sr",
                &sr,
                json!({
                    "uuid": s.uuid, "name_label": s.name_label,
                    "name_description": s.name_description, "type": s.sr_type,
                    "content_type": "user", "shared": true, "PBDs": [], "VDIs": [],
                    "physical_size": s.size, "physical_utilisation": 0,
                    "virtual_allocation": 0, "other_config": {}, "sm_config": {},
                    "tags": [], "introduced_by": NULL_REF, "is_tools_sr": false,
                    "allowed_operations": [], "current_operations": {},
                    "local_cache_enabled": false, "clustered": false, "blobs": {}
                }),
            );
            for host in self.dr_hosts() {
                let (pbd, pbd_uuid) = self.new_ref("pbd");
                self.insert(
                    "pbd",
                    &pbd,
                    json!({"uuid": pbd_uuid, "host": host, "SR": sr,
                           "device_config": dconf, "currently_attached": true,
                           "other_config": {}}),
                );
                self.push_ref("sr", &sr, "PBDs", &pbd);
                self.push_ref("host", &host, "PBDs", &pbd);
            }
            self.add_fabric_vdis(&sr, &s.uuid);
            self.refresh_sr_ops(&sr);
            introduced.push(sr);
        }
        let (task, uuid) = self.new_ref("dr_task");
        self.insert(
            "dr_task",
            &task,
            json!({"uuid": uuid, "introduced_SRs": introduced}),
        );
        for sr in &introduced {
            self.modify("sr", sr, |r| {
                r.insert("introduced_by".into(), json!(task));
            });
        }
        Ok(json!(task))
    }

    fn dr_task_destroy(&mut self, task: &str) -> Reply {
        let Some(rec) = self.record("dr_task", task).cloned() else {
            return api_error("HANDLE_INVALID", &["DR_task", task]);
        };
        for sr in refs(&rec, "introduced_SRs") {
            let Some(sr_rec) = self.record("sr", &sr).cloned() else {
                continue;
            };
            let used = refs(&sr_rec, "VDIs").iter().any(|v| {
                self.record("vdi", v).is_some_and(|r| {
                    refs(r, "VBDs").iter().any(|vbd| {
                        self.get_str("vbd", vbd, "VM")
                            .is_some_and(|vm| self.exists("vm", &vm))
                    })
                })
            });
            if used {
                self.modify("sr", &sr, |r| {
                    r.insert("introduced_by".into(), json!(NULL_REF));
                });
                continue;
            }
            for pbd in refs(&sr_rec, "PBDs") {
                if let Some(host) = self.get_str("pbd", &pbd, "host") {
                    self.drop_ref("host", &host, "PBDs", &pbd);
                }
                self.remove("pbd", &pbd);
            }
            for vdi in refs(&sr_rec, "VDIs") {
                self.remove("vdi", &vdi);
            }
            self.remove("sr", &sr);
        }
        self.remove("dr_task", task);
        Ok(json!(""))
    }

    fn open_database(&mut self, vdi: &str) -> Reply {
        let Some(rec) = self.record("vdi", vdi).cloned() else {
            return api_error("HANDLE_INVALID", &["VDI", vdi]);
        };
        if text(&rec, "type") != "metadata" {
            return api_error("VDI_INCOMPATIBLE_TYPE", &[vdi, &text(&rec, "type")]);
        }
        if self
            .dr_pool()
            .is_some_and(|p| text(&rec, "metadata_of_pool") == p)
        {
            return api_error("VDI_CONTAINS_METADATA_OF_THIS_POOL", &[vdi]);
        }
        let uuid = text(&rec, "uuid");
        let known = fabric()
            .iter()
            .flat_map(|s| s.metadata.iter())
            .any(|m| m.vdi_uuid == uuid);
        if !known {
            return api_error("VDI_MISSING", &[&text(&rec, "SR"), vdi]);
        }
        let session = format!("OpaqueRef:mock-session-{}", self.next_session);
        self.next_session += 1;
        self.dr.sessions.insert(session.clone(), uuid);
        Ok(json!(session))
    }
}

// ---- metadata sessions ------------------------------------------------------------------

/// The database a metadata session reads.
fn database(vdi_uuid: &str) -> Option<BTreeMap<(String, String), Value>> {
    fabric()
        .iter()
        .flat_map(|s| s.metadata.iter())
        .find(|m| m.vdi_uuid == vdi_uuid)
        .map(|m| m.db.clone())
}

/// Whether `session` is a metadata session of this server.
pub(crate) fn is_metadata_session(state: &State, session: &str) -> bool {
    state.db().dr.sessions.contains_key(session)
}

fn meta_rec<'a>(
    meta: &'a BTreeMap<(String, String), Value>,
    class: &str,
    r: &str,
) -> Option<&'a Map<String, Value>> {
    meta.get(&(class.to_owned(), r.to_owned()))
        .and_then(Value::as_object)
}

/// The VMs a recovery of `obj` (a VM or a vApp of `meta`) brings back.
fn members(meta: &BTreeMap<(String, String), Value>, class: &str, obj: &str) -> Vec<String> {
    if class == "vm_appliance" {
        meta_rec(meta, "vm_appliance", obj)
            .map(|a| refs(a, "VMs"))
            .unwrap_or_default()
    } else {
        vec![obj.to_owned()]
    }
}

impl Db {
    /// The metadata SRs `vms` need that this pool has not plugged.
    fn missing_srs(
        &self,
        meta: &BTreeMap<(String, String), Value>,
        vms: &[String],
    ) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for vm in vms {
            let Some(vm_rec) = meta_rec(meta, "vm", vm) else {
                continue;
            };
            for vbd in refs(vm_rec, "VBDs") {
                let Some(vbd_rec) = meta_rec(meta, "vbd", &vbd) else {
                    continue;
                };
                if text(vbd_rec, "type") == "CD" {
                    continue;
                }
                let vdi = text(vbd_rec, "VDI");
                let Some(vdi_rec) = meta_rec(meta, "vdi", &vdi) else {
                    continue;
                };
                let sr = text(vdi_rec, "SR");
                let sr_uuid = meta_rec(meta, "sr", &sr)
                    .map(|s| text(s, "uuid"))
                    .unwrap_or_default();
                let here = self.by_uuid("sr", &sr_uuid).is_some_and(|local| {
                    self.record("sr", &local).is_some_and(|r| {
                        refs(r, "PBDs").iter().any(|p| {
                            self.record("pbd", p).is_some_and(|p| {
                                p.get("currently_attached").and_then(Value::as_bool) == Some(true)
                            })
                        })
                    })
                }) && self.by_uuid("vdi", &text(vdi_rec, "uuid")).is_some();
                if !here && !out.iter().any(|(_, s)| *s == sr) {
                    out.push((vm.clone(), sr));
                }
            }
        }
        out
    }

    /// Recovers VM `vm` of `meta`; its new reference here.
    fn recover_vm(
        &mut self,
        meta: &BTreeMap<(String, String), Value>,
        vm: &str,
        force: bool,
        appliance: &str,
    ) -> Result<String, Error> {
        let Some(src) = meta_rec(meta, "vm", vm).cloned() else {
            return Err(err("HANDLE_INVALID", &["VM", vm]));
        };
        let uuid = text(&src, "uuid");
        if let Some((_, code)) = self.dr.failing.iter().find(|(u, _)| *u == uuid) {
            return Err(err(code, &[vm]));
        }
        if let Some((vm_ref, sr)) = self.missing_srs(meta, &[vm.to_owned()]).first().cloned() {
            return Err(err("VM_REQUIRES_SR", &[&vm_ref, &sr]));
        }
        let version = int(&src, "version");
        if let Some(existing) = self.by_uuid("vm", &uuid) {
            let rec = self.record("vm", &existing).cloned().unwrap_or_default();
            if !force && int(&rec, "version") >= version {
                return Err(err(
                    "VM_TO_IMPORT_IS_NOT_NEWER_VERSION",
                    &[
                        &existing,
                        &int(&rec, "version").to_string(),
                        &version.to_string(),
                    ],
                ));
            }
            let power = text(&rec, "power_state");
            if power != "Halted" {
                return Err(err(
                    "VM_BAD_POWER_STATE",
                    &[&existing, "halted", &power.to_lowercase()],
                ));
            }
            self.vm_destroy(&existing)?;
        }
        let (new_vm, _) = self.new_ref("vm");
        let (metrics, metrics_uuid) = self.new_ref("vm_metrics");
        self.insert(
            "vm_metrics",
            &metrics,
            json!({"uuid": metrics_uuid, "memory_actual": 0, "VCPUs_number": 0,
                   "VCPUs_utilisation": {}, "start_time": "19700101T00:00:00Z",
                   "install_time": "19700101T00:00:00Z", "state": [], "last_updated": "19700101T00:00:00Z",
                   "other_config": {}, "hvm": true, "nested_virt": false, "nomigrate": false,
                   "current_domain_type": "hvm"}),
        );
        let mut rec = src.clone();
        for (k, v) in [
            ("power_state", json!("Halted")),
            ("resident_on", json!(NULL_REF)),
            ("scheduled_to_be_resident_on", json!(NULL_REF)),
            ("affinity", json!(NULL_REF)),
            ("VBDs", json!([])),
            ("VIFs", json!([])),
            ("VTPMs", json!([])),
            ("VUSBs", json!([])),
            ("VGPUs", json!([])),
            ("consoles", json!([])),
            ("crash_dumps", json!([])),
            ("snapshots", json!([])),
            ("children", json!([])),
            ("parent", json!(NULL_REF)),
            ("snapshot_of", json!(NULL_REF)),
            ("suspend_VDI", json!(NULL_REF)),
            ("suspend_SR", json!(NULL_REF)),
            ("guest_metrics", json!(NULL_REF)),
            ("metrics", json!(metrics)),
            ("snapshot_schedule", json!(NULL_REF)),
            ("protection_policy", json!(NULL_REF)),
            ("appliance", json!(NULL_REF)),
            ("current_operations", json!({})),
            ("version", json!(version + 1)),
        ] {
            rec.insert(k.into(), v);
        }
        let allowed = crate::provisioning::vm_allowed(&rec);
        rec.insert("allowed_operations".into(), json!(allowed));
        self.insert("vm", &new_vm, Value::Object(rec));
        // Drives: the disks found by uuid; CD drives empty.
        for vbd in refs(&src, "VBDs") {
            let Some(vbd_rec) = meta_rec(meta, "vbd", &vbd) else {
                continue;
            };
            let cd = text(vbd_rec, "type") == "CD";
            let vdi = if cd {
                None
            } else {
                meta_rec(meta, "vdi", &text(vbd_rec, "VDI"))
                    .and_then(|v| self.by_uuid("vdi", &text(v, "uuid")))
            };
            let (r, vbd_uuid) = self.new_ref("vbd");
            self.insert(
                "vbd",
                &r,
                json!({"uuid": vbd_uuid, "VM": new_vm,
                       "VDI": vdi.clone().unwrap_or_else(|| NULL_REF.to_owned()),
                       "type": text(vbd_rec, "type"), "mode": text(vbd_rec, "mode"),
                       "userdevice": text(vbd_rec, "userdevice"), "device": "",
                       "bootable": vbd_rec.get("bootable").cloned().unwrap_or(json!(false)),
                       "empty": cd, "currently_attached": false, "unpluggable": true,
                       "allowed_operations": [], "current_operations": {},
                       "other_config": {}, "qos_algorithm_type": "",
                       "qos_algorithm_params": {}, "metrics": NULL_REF}),
            );
            self.push_ref("vm", &new_vm, "VBDs", &r);
            if let Some(vdi) = vdi {
                self.push_ref("vdi", &vdi, "VBDs", &r);
            }
            self.refresh_vbd_ops(&r);
        }
        // Interfaces: the network with the same uuid, else the same name.
        for vif in refs(&src, "VIFs") {
            let Some(vif_rec) = meta_rec(meta, "vif", &vif) else {
                continue;
            };
            let Some(net) = meta_rec(meta, "network", &text(vif_rec, "network")) else {
                continue;
            };
            let local = self.by_uuid("network", &text(net, "uuid")).or_else(|| {
                self.objects
                    .iter()
                    .find(|((c, _), rec)| {
                        c == "network"
                            && rec.get("name_label").and_then(Value::as_str)
                                == Some(text(net, "name_label").as_str())
                    })
                    .map(|((_, r), _)| r.clone())
            });
            let Some(local) = local else {
                continue;
            };
            let (r, vif_uuid) = self.new_ref("vif");
            self.insert(
                "vif",
                &r,
                json!({"uuid": vif_uuid, "VM": new_vm, "network": local,
                       "device": text(vif_rec, "device"), "MAC": text(vif_rec, "MAC"),
                       "MTU": vif_rec.get("MTU").cloned().unwrap_or(json!(1500)),
                       "currently_attached": false, "allowed_operations": [],
                       "current_operations": {}, "other_config": {},
                       "qos_algorithm_type": "", "qos_algorithm_params": {},
                       "locking_mode": "network_default", "ipv4_allowed": [], "ipv6_allowed": [],
                       "metrics": NULL_REF, "MAC_autogenerated": false}),
            );
            self.push_ref("vm", &new_vm, "VIFs", &r);
            self.push_ref("network", &local, "VIFs", &r);
        }
        if appliance != NULL_REF {
            self.assign_appliance(&new_vm, appliance);
        }
        Ok(new_vm)
    }

    /// Recovers vApp `app` of `meta` with its members.
    fn recover_appliance(
        &mut self,
        meta: &BTreeMap<(String, String), Value>,
        app: &str,
        force: bool,
    ) -> Result<String, Error> {
        let Some(src) = meta_rec(meta, "vm_appliance", app).cloned() else {
            return Err(err("HANDLE_INVALID", &["VM_appliance", app]));
        };
        let vms = refs(&src, "VMs");
        if let Some((vm, sr)) = self.missing_srs(meta, &vms).first().cloned() {
            return Err(err("VM_REQUIRES_SR", &[&vm, &sr]));
        }
        let uuid = text(&src, "uuid");
        let local = match self.by_uuid("vm_appliance", &uuid) {
            Some(r) => r,
            None => {
                let (r, _) = self.new_ref("vm_appliance");
                self.insert(
                    "vm_appliance",
                    &r,
                    json!({"uuid": uuid, "name_label": text(&src, "name_label"),
                           "name_description": text(&src, "name_description"),
                           "allowed_operations": [], "current_operations": {}, "VMs": []}),
                );
                r
            }
        };
        for vm in vms {
            self.recover_vm(meta, &vm, force, &local)?;
        }
        self.refresh_appliance(&local);
        Ok(local)
    }
}

/// A call on a metadata session.
pub(crate) async fn metadata_call(
    state: &Arc<State>,
    method: &str,
    session: &str,
    params: &[Value],
) -> Reply {
    let vdi = state
        .db()
        .dr
        .sessions
        .get(session)
        .cloned()
        .unwrap_or_default();
    if method == "session.logout" {
        state.db().dr.sessions.remove(session);
        return Ok(json!(""));
    }
    let Some(meta) = database(&vdi) else {
        return api_error("SESSION_INVALID", &[session]);
    };
    let is_async = method.starts_with("Async.");
    let sync = method.trim_start_matches("Async.");
    let (class, call) = sync.split_once('.').unwrap_or((sync, ""));
    let lower = class.to_lowercase();
    let obj = str_param(params, 1).to_owned();
    let session_to = str_param(params, 2).to_owned();
    let force = params.get(3).and_then(Value::as_bool).unwrap_or(false);
    let needs_live = matches!(
        call,
        "assert_can_be_recovered" | "get_SRs_required_for_recovery" | "recover"
    );
    if needs_live && !state.db().sessions.contains_key(&session_to) {
        return api_error("SESSION_INVALID", &[&session_to]);
    }
    match (lower.as_str(), call) {
        ("vm" | "vm_appliance", "assert_can_be_recovered") => {
            if meta_rec(&meta, &lower, &obj).is_none() {
                return api_error("HANDLE_INVALID", &[class, &obj]);
            }
            let vms = members(&meta, &lower, &obj);
            match state.db().missing_srs(&meta, &vms).first() {
                Some((vm, sr)) => api_error("VM_REQUIRES_SR", &[vm, sr]),
                None => Ok(json!("")),
            }
        }
        ("vm" | "vm_appliance", "get_SRs_required_for_recovery") => {
            let vms = members(&meta, &lower, &obj);
            let srs: Vec<String> = state
                .db()
                .missing_srs(&meta, &vms)
                .into_iter()
                .map(|(_, s)| s)
                .collect();
            Ok(json!(srs))
        }
        ("vm" | "vm_appliance", "recover") => {
            let failure = state.db().failures.get(sync).cloned();
            let is_vm = lower == "vm";
            let run = move |db: &mut Db| -> Result<(), Error> {
                if let Some(e) = failure.clone() {
                    return Err(e);
                }
                if is_vm {
                    db.recover_vm(&meta, &obj, force, NULL_REF).map(drop)
                } else {
                    db.recover_appliance(&meta, &obj, force).map(drop)
                }
            };
            if !is_async {
                let mut db = state.db();
                let before = db.objects.clone();
                let events = db.events.len();
                let r = run(&mut db);
                if r.is_err() {
                    // Nothing is left behind.
                    db.objects = before;
                    db.events.truncate(events);
                }
                drop(db);
                state.changed.notify_waiters();
                return r.map(|()| json!(""));
            }
            let task = crate::create_task(state, method);
            let state2 = state.clone();
            let task_ref = task.clone();
            state.runtime.clone().spawn(async move {
                let (steps, step) = {
                    let db = state2.db();
                    (db.task_steps, db.task_step)
                };
                for i in 1..=steps {
                    tokio::time::sleep(step).await;
                    let mut db = state2.db();
                    if db.get_str("task", &task_ref, "status").as_deref() != Some("pending") {
                        return;
                    }
                    if i < steps {
                        let progress = f64::from(i) / f64::from(steps);
                        db.modify("task", &task_ref, |rec| {
                            rec.insert("progress".into(), json!(progress));
                        });
                        continue;
                    }
                    let before = db.objects.clone();
                    let events = db.events.len();
                    match run(&mut db) {
                        Ok(()) => db.finish_task(&task_ref, "success", "", &[]),
                        Err((code, ps)) => {
                            db.objects = before;
                            db.events.truncate(events);
                            let mut info = vec![code.as_str()];
                            info.extend(ps.iter().map(String::as_str));
                            db.finish_task(&task_ref, "failure", "", &info);
                        }
                    }
                }
                state2.changed.notify_waiters();
            });
            state.changed.notify_waiters();
            Ok(json!(task))
        }
        ("host", "get_name_label") => match meta_rec(&meta, "host", &obj) {
            Some(h) => Ok(json!(text(h, "name_label"))),
            None => api_error("HANDLE_INVALID", &["host", &obj]),
        },
        (_, "get_all_records") => Ok(Value::Object(
            meta.iter()
                .filter(|((c, _), _)| *c == lower)
                .map(|((_, r), v)| (r.clone(), v.clone()))
                .collect(),
        )),
        (_, "get_all") => Ok(json!(
            meta.keys()
                .filter(|(c, _)| *c == lower)
                .map(|(_, r)| r.clone())
                .collect::<Vec<_>>()
        )),
        (_, "get_record") => match meta.get(&(lower.clone(), obj.clone())) {
            Some(v) => Ok(v.clone()),
            None => api_error("HANDLE_INVALID", &[class, &obj]),
        },
        (_, field) if field.starts_with("get_") => {
            let field = field.trim_start_matches("get_");
            match meta
                .get(&(lower.clone(), obj.clone()))
                .and_then(|r| r.get(field))
            {
                Some(v) => Ok(v.clone()),
                None => api_error("HANDLE_INVALID", &[class, &obj]),
            }
        }
        _ => api_error("OPERATION_NOT_ALLOWED", &["not on a metadata session"]),
    }
}

/// `MockServer::fail_recovery`.
pub(crate) fn fail_recovery(state: &State, vm_uuid: &str, code: Option<&str>) {
    let mut db = state.db();
    db.dr.failing.retain(|(u, _)| u != vm_uuid);
    if let Some(code) = code {
        db.dr.failing.push((vm_uuid.to_owned(), code.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::MockServer;
    use crate::migrate::tests::{call, login, wait_task};

    fn s(v: &Value) -> String {
        v.as_str().unwrap_or_default().to_owned()
    }

    #[tokio::test]
    async fn metadata_on_a_shared_sr_recovers_into_another_pool() {
        let a = MockServer::start_lab(false, "dra", 1).await;
        let b = MockServer::start_lab(false, "drb", 2).await;
        a.set_task_timing(1, Duration::from_millis(5));
        b.set_task_timing(1, Duration::from_millis(5));
        let (sa, sb) = (login(&a).await, login(&b).await);
        let host_a = "OpaqueRef:host-0001-dra";
        let dconf = json!({"server": "nfs.lab", "serverpath": "/exports/dr-mock"});
        let sr = call(
            &a,
            &sa,
            "SR.create",
            &[
                json!(host_a),
                dconf.clone(),
                json!(0),
                json!("DR NFS"),
                json!(""),
                json!("nfs"),
                json!("user"),
                json!(true),
                json!({}),
            ],
        )
        .await
        .unwrap();
        let sr = s(&sr);
        // A local SR cannot hold the metadata.
        let local = "OpaqueRef:sr-local-01-dra";
        let r = call(&a, &sa, "SR.enable_database_replication", &[json!(local)]).await;
        assert_eq!(r.unwrap_err().0, "SR_OPERATION_NOT_SUPPORTED");
        call(&a, &sa, "SR.enable_database_replication", &[json!(sr)])
            .await
            .unwrap();
        // A VM with its disk on the shared SR.
        let vm = s(&call(
            &a,
            &sa,
            "VM.create",
            &[
                json!({"name_label": "app", "memory_static_max": 1024, "memory_dynamic_max": 1024,
                     "memory_dynamic_min": 1024, "memory_static_min": 1024, "VCPUs_max": 1,
                     "VCPUs_at_startup": 1}),
            ],
        )
        .await
        .unwrap());
        let vdi = s(&call(
            &a,
            &sa,
            "VDI.create",
            &[
                json!({"name_label": "app disk", "SR": sr, "virtual_size": 1048576,
                     "type": "user", "sharable": false, "read_only": false, "other_config": {}}),
            ],
        )
        .await
        .unwrap());
        call(
            &a,
            &sa,
            "VBD.create",
            &[
                json!({"VM": vm, "VDI": vdi, "userdevice": "0", "bootable": true, "mode": "RW",
                     "type": "Disk", "empty": false, "other_config": {}}),
            ],
        )
        .await
        .unwrap();
        let vm_uuid = s(&a.field("vm", &vm, "uuid").unwrap());
        a.sync_metadata();
        // B finds the SR with metadata and introduces it.
        let host_b = "OpaqueRef:host-0001-drb";
        let probe = call(
            &b,
            &sb,
            "SR.probe",
            &[
                json!(host_b),
                dconf.clone(),
                json!("nfs"),
                json!({"metadata": "true"}),
            ],
        )
        .await
        .unwrap();
        assert!(s(&probe).contains("<pool_metadata_detected>true"));
        let sr_uuid = s(&a.field("sr", &sr, "uuid").unwrap());
        let task = s(&call(
            &b,
            &sb,
            "DR_task.create",
            &[json!("nfs"), dconf.clone(), json!([sr_uuid])],
        )
        .await
        .unwrap());
        let introduced = b.field("dr_task", &task, "introduced_SRs").unwrap();
        let sr_b = s(&introduced[0]);
        let vdis = b.field("sr", &sr_b, "VDIs").unwrap();
        let meta_vdi = vdis
            .as_array()
            .unwrap()
            .iter()
            .map(s)
            .find(|v| b.field("vdi", v, "type") == Some(json!("metadata")))
            .unwrap();
        let meta = s(&call(&b, &sb, "VDI.open_database", &[json!(meta_vdi)])
            .await
            .unwrap());
        let vms = call(&b, &meta, "VM.get_all_records", &[]).await.unwrap();
        assert!(vms.get(&vm).is_some());
        call(
            &b,
            &meta,
            "VM.assert_can_be_recovered",
            &[json!(vm), json!(sb)],
        )
        .await
        .unwrap();
        // A failing recovery leaves nothing.
        b.fail_recovery(&vm_uuid, Some("VM_REQUIRES_SR"));
        let t = s(&call(
            &b,
            &meta,
            "Async.VM.recover",
            &[json!(vm), json!(sb), json!(true)],
        )
        .await
        .unwrap());
        assert_eq!(wait_task(&b, &t).await.0, "failure");
        assert!(
            b.refs("vm")
                .iter()
                .all(|r| b.field("vm", r, "uuid") != Some(json!(vm_uuid)))
        );
        b.fail_recovery(&vm_uuid, None);
        let t = s(&call(
            &b,
            &meta,
            "Async.VM.recover",
            &[json!(vm), json!(sb), json!(true)],
        )
        .await
        .unwrap());
        assert_eq!(wait_task(&b, &t).await.0, "success");
        let recovered = b
            .refs("vm")
            .into_iter()
            .find(|r| b.field("vm", r, "uuid") == Some(json!(vm_uuid)))
            .unwrap();
        assert_eq!(
            b.field("vm", &recovered, "power_state").unwrap(),
            json!("Halted")
        );
        assert_eq!(
            b.field("vm", &recovered, "VBDs")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        call(&b, &meta, "session.logout", &[]).await.unwrap();
        // The SR stays: the recovered VM uses it.
        call(&b, &sb, "DR_task.destroy", &[json!(task)])
            .await
            .unwrap();
        assert!(b.record("sr", &sr_b).is_some());
        // A's own metadata VDI cannot be opened by A.
        let own = a
            .field("pool", "OpaqueRef:pool-0001-dra", "metadata_VDIs")
            .unwrap();
        let r = call(&a, &sa, "VDI.open_database", &[own[0].clone()]).await;
        assert_eq!(r.unwrap_err().0, "VDI_CONTAINS_METADATA_OF_THIS_POOL");
        call(&a, &sa, "SR.disable_database_replication", &[json!(sr)])
            .await
            .unwrap();
        assert_eq!(
            a.field("pool", "OpaqueRef:pool-0001-dra", "metadata_VDIs")
                .unwrap(),
            json!([])
        );
    }
}
