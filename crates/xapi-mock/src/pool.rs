//! Pools in the mock: several mock servers in one process can form a pool,
//! as XAPI servers do, and the pool and server settings of F10a.
//!
//! **A lab of standalone servers.** [`lab_records`] is the database of one
//! standalone XCP-ng 8.3 server (`"a"`, `"b"`, …): an unnamed pool of one,
//! the host with its control domain, local storage, two NICs on their own
//! networks, a certificate and the SR drivers. Every mock registers itself
//! by its address (`127.0.0.1:port`), and a lab host's `address` is that
//! address, so `pool.join` given it finds the coordinator.
//!
//! **Calls.**
//!
//! * `pool.join(address, user, password)` on the joining server: the
//!   coordinator must answer at `address` and accept the credentials
//!   (`POOL_JOINING_HOST_CONNECTION_FAILED`,
//!   `SESSION_AUTHENTICATION_FAILED`), have HA off (`HA_IS_ENABLED`); the
//!   joining server must be alone (`JOINING_HOST_CANNOT_BE_MASTER_OF_OTHER_HOSTS`),
//!   have no running VM (`JOINING_HOST_CANNOT_HAVE_RUNNING_VMS`) and no
//!   shared SR (`JOINING_HOST_CANNOT_CONTAIN_SHARED_SRS`). These checks come
//!   first, so a failed join changes nothing. On success the server's
//!   host, metrics, CPUs, certificates, control domain, local SRs (with
//!   their PBDs and disks) and NICs move to the coordinator's database
//!   (each NIC on the coordinator's network of the same bridge); its other
//!   VMs are not kept, as XAPI drops them. The joining server then answers
//!   `HOST_IS_SLAVE` and, a moment later, forgets its sessions (its API
//!   restarts).
//! * `pool.eject(host)` on the coordinator (`HOST_IS_MASTER`,
//!   `HOST_HAS_RESIDENT_VMS`): the server's objects leave the pool, and the
//!   ejected server comes back as the standalone server it was (without
//!   its VMs: ejecting resets a server).
//! * `pool.designate_new_master(host)`: the pool's database moves to that
//!   server's mock, and the old coordinator answers `HOST_IS_SLAVE` (with
//!   no mock at that server's address, this one goes on serving the pool
//!   with its new coordinator).
//! * `pool.rotate_secret`, `pool.enable_tls_verification`,
//!   `host.syslog_reconfigure`, `host.set_power_on_mode` (with its
//!   configuration), `host.install_server_certificate`
//!   (`SERVER_CERTIFICATE_INVALID`, `SERVER_CERTIFICATE_KEY_INVALID`; a new
//!   certificate record), `host.reset_server_certificate`, `secret.create`
//!   / `destroy`, `Cluster.pool_create` / `pool_destroy` / `get_network`,
//!   `task.create`, `session.change_password`
//!   (`CHANGE_PASSWORD_REJECTED`), and the `nrpe` plugin of
//!   `host.call_plugin` (`status`, `get-config`, `get-threshold`,
//!   `enable`, `disable`, `control`, `set-config`, `set-threshold`,
//!   `restart`);
//! * `GET /host_backup` and `PUT /host_restore` (with `session_id` and
//!   `task_id`): a backup is a small text naming the server; a restore
//!   accepts only a backup of that server (`RESTORE_TARGET_MISMATCH`) and
//!   finishes the task.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, State, api_error, lock, str_param};

/// Calls handled here.
const CALLS: &[(&str, &[&str])] = &[
    (
        "pool",
        &[
            "join",
            "eject",
            "designate_new_master",
            "rotate_secret",
            "enable_tls_verification",
        ],
    ),
    (
        "host",
        &[
            "syslog_reconfigure",
            "set_power_on_mode",
            "install_server_certificate",
            "reset_server_certificate",
        ],
    ),
    ("secret", &["create", "destroy"]),
    ("Cluster", &["pool_create", "pool_destroy", "get_network"]),
    ("task", &["create"]),
    ("session", &["change_password"]),
];

/// Whether `class.call` is handled here.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    CALLS
        .iter()
        .any(|(c, calls)| *c == class && calls.contains(&call))
}

/// Cross-server work done once the database lock is released.
pub(crate) enum After {
    /// Add the joined server's objects to the coordinator at `coordinator`.
    JoinInto {
        coordinator: String,
        objects: Vec<((String, String), Value)>,
        host: String,
    },
    /// The server at `member` was ejected: it is standalone again.
    Restore { member: String },
    /// The database moves to the server at `to`, the new coordinator.
    HandOver { to: String },
}

/// A server's NRPE configuration.
#[derive(Debug, Clone)]
pub(crate) struct Nrpe {
    enabled: bool,
    allowed_hosts: String,
    debug: String,
    ssl_logging: String,
    thresholds: BTreeMap<String, (String, String)>,
}

impl Default for Nrpe {
    fn default() -> Self {
        let t = |w: &str, c: &str| (w.to_owned(), c.to_owned());
        Self {
            enabled: false,
            allowed_hosts: "127.0.0.1,::1".to_owned(),
            debug: "0".to_owned(),
            ssl_logging: "0x00".to_owned(),
            thresholds: BTreeMap::from([
                ("check_host_load".to_owned(), t("3", "4")),
                ("check_host_cpu".to_owned(), t("80", "90")),
                ("check_host_memory".to_owned(), t("80", "90")),
                ("check_vgpu".to_owned(), t("80", "90")),
                ("check_vgpu_memory".to_owned(), t("80", "90")),
                ("check_load".to_owned(), t("2.7,2.6,2.5", "3.2,3.1,3")),
                ("check_cpu".to_owned(), t("80", "90")),
                ("check_memory".to_owned(), t("80", "90")),
                ("check_swap".to_owned(), t("20", "10")),
                ("check_disk_root".to_owned(), t("20", "10")),
                ("check_disk_log".to_owned(), t("20", "10")),
            ]),
        }
    }
}

/// Pool and server settings state that is not in the database.
#[derive(Default)]
pub(crate) struct World {
    /// The root password ([`crate::PASSWORD`] until changed).
    pub(crate) password: Option<String>,
    /// `127.0.0.1:port` of this server.
    pub(crate) authority: String,
    /// The database this server had as a standalone server, kept while it
    /// is a pool member (for when it is ejected).
    pub(crate) standalone: Option<BTreeMap<(String, String), Value>>,
    pub(crate) nrpe: HashMap<String, Nrpe>,
    pub(crate) after: Vec<After>,
    /// The coordinator a `pool.join` in progress joins (from [`check_join`]).
    pub(crate) join_target: Option<String>,
    /// The coordinator's external authentication (type, service name),
    /// found with `join_target`.
    pub(crate) join_auth: Option<(String, String)>,
    pub(crate) secret_rotations: u32,
    /// `PUT /host_restore` bodies accepted, by host.
    pub(crate) restored: Vec<String>,
}

impl World {
    pub(crate) fn root_password(&self) -> &str {
        self.password.as_deref().unwrap_or(crate::PASSWORD)
    }
}

// ---- the registry of mock servers ---------------------------------------------------

type Registry = Mutex<HashMap<String, Weak<State>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Registry::default)
}

/// Makes `state` reachable at `authority` (`127.0.0.1:port`).
pub(crate) fn register(authority: &str, state: &Arc<State>) {
    let mut r = lock(registry());
    r.retain(|_, w| w.strong_count() > 0);
    r.insert(authority.to_owned(), Arc::downgrade(state));
}

/// The mock answering at `address` (`host:port`, `localhost` same as
/// `127.0.0.1`).
pub(crate) fn lookup(address: &str) -> Option<Arc<State>> {
    let address = address
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .replace("localhost", "127.0.0.1");
    lock(registry()).get(&address).and_then(Weak::upgrade)
}

// ---- the lab fixture ------------------------------------------------------------------

/// The database of one standalone lab server, `tag` naming it ("a", "b"…):
/// built from the fixture's first host, every reference and UUID made
/// unique to `tag`. `index` (1–9) sets its IP address.
pub fn lab_records(tag: &str, index: u8) -> Value {
    let all = crate::fixture_records();
    let capture = crate::capture_records();
    let pick: &[(&str, &str)] = &[
        ("pool", "OpaqueRef:pool-0001"),
        ("host", "OpaqueRef:host-0001"),
        ("host_metrics", "OpaqueRef:host-metrics-01"),
        ("host_cpu", "OpaqueRef:host-cpu-01-0"),
        ("certificate", "OpaqueRef:cert-host-01"),
        ("vm", "OpaqueRef:vm-dom0-01"),
        ("vm_metrics", "OpaqueRef:vm-metrics-dom0-01"),
        ("sr", "OpaqueRef:sr-local-01"),
        ("pbd", "OpaqueRef:pbd-local-01"),
        ("pif", "OpaqueRef:pif-01-eth0"),
        ("pif", "OpaqueRef:pif-01-eth1"),
        ("pif_metrics", "OpaqueRef:pifm-01-eth0"),
        ("pif_metrics", "OpaqueRef:pifm-01-eth1"),
        ("network", "OpaqueRef:net-eth0"),
        ("network", "OpaqueRef:net-eth1"),
    ];
    let mut text = String::from("{");
    let mut first = true;
    let mut classes: BTreeMap<&str, Vec<(&str, &Value)>> = BTreeMap::new();
    for (class, r) in pick {
        if let Some(rec) = all.get(class).and_then(|c| c.get(r)) {
            classes.entry(class).or_default().push((r, rec));
        }
    }
    for (class, objs) in &classes {
        if !first {
            text.push(',');
        }
        first = false;
        text.push_str(&format!("\"{class}\":{{"));
        for (i, (r, rec)) in objs.iter().enumerate() {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!("\"{r}\":{rec}"));
        }
        text.push('}');
    }
    text.push('}');
    // References and UUIDs unique to this server.
    for (_, r) in pick {
        text = text.replace(&format!("\"{r}\""), &format!("\"{r}-{tag}\""));
    }
    let mut db: Value = serde_json::from_str(&text).expect("lab records");
    let n = u64::from(index);
    let mut serial = 0u64;
    let classes: Vec<String> = db
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for class in classes {
        if let Some(objs) = db.get_mut(&class).and_then(Value::as_object_mut) {
            for rec in objs.values_mut() {
                serial += 1;
                rec["uuid"] = json!(format!("1ab0{n:04x}-0000-4000-8000-{serial:012x}"));
            }
        }
    }
    let r = |base: &str| format!("{base}-{tag}");
    let keep = |db: &mut Value, class: &str, rec_ref: &str, field: &str| {
        let present: Vec<String> = db
            .get(class)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let _ = present;
        if let Some(list) = db
            .get_mut(class)
            .and_then(|c| c.get_mut(rec_ref))
            .and_then(|o| o.get_mut(field))
            .and_then(Value::as_array_mut)
        {
            list.retain(|v| v.as_str().is_some_and(|s| s.ends_with(&format!("-{tag}"))));
        }
    };
    let host = r("OpaqueRef:host-0001");
    for (class, rec_ref, field) in [
        ("host", host.as_str(), "PBDs"),
        ("host", host.as_str(), "PIFs"),
        ("host", host.as_str(), "host_CPUs"),
        ("host", host.as_str(), "resident_VMs"),
        ("host", host.as_str(), "certificates"),
    ] {
        keep(&mut db, class, rec_ref, field);
    }
    for net in ["OpaqueRef:net-eth0", "OpaqueRef:net-eth1"] {
        keep(&mut db, "network", &r(net), "PIFs");
        if let Some(n) = db["network"].get_mut(r(net)) {
            n["VIFs"] = json!([]);
        }
    }
    keep(&mut db, "sr", &r("OpaqueRef:sr-local-01"), "PBDs");
    if let Some(sr) = db["sr"].get_mut(r("OpaqueRef:sr-local-01")) {
        sr["VDIs"] = json!([]);
        sr["virtual_allocation"] = json!(0);
        sr["physical_utilisation"] = json!(4294967296_i64);
    }
    for pif in ["OpaqueRef:pif-01-eth0", "OpaqueRef:pif-01-eth1"] {
        if let Some(p) = db["pif"].get_mut(r(pif)) {
            p["VLAN_slave_of"] = json!([]);
            p["MAC"] = json!(format!(
                "02:00:00:0{index}:0{}:00",
                if pif.ends_with("eth0") { 0 } else { 1 }
            ));
            if p["management"] == json!(true) {
                p["IP"] = json!(format!("192.0.2.{}", 100 + n));
            }
        }
    }
    if let Some(pbd) = db["pbd"].get_mut(r("OpaqueRef:pbd-local-01")) {
        pbd["device_config"] = json!({"device": "/dev/sdb"});
    }
    let local = r("OpaqueRef:sr-local-01");
    let pool = &mut db["pool"][r("OpaqueRef:pool-0001")];
    pool["name_label"] = json!("");
    pool["name_description"] = json!("");
    pool["default_SR"] = json!(local);
    pool["suspend_image_SR"] = json!(local);
    pool["crash_dump_SR"] = json!(local);
    pool["ha_enabled"] = json!(false);
    pool["tls_verification_enabled"] = json!(true);
    pool["other_config"] = json!({"auto_poweron": "false"});
    pool["allowed_operations"] = json!([
        "ha_enable",
        "cluster_create",
        "designate_new_master",
        "eject"
    ]);
    pool["migration_compression"] = json!(false);
    pool["igmp_snooping_enabled"] = json!(false);
    pool["live_patching_disabled"] = json!(false);
    pool["is_psr_pending"] = json!(false);
    pool["vswitch_controller"] = json!("");
    let cap_host = capture["host"]
        .as_object()
        .and_then(|m| m.values().next())
        .cloned()
        .unwrap_or_default();
    let mut license = cap_host["license_params"].clone();
    license["restrict_corosync"] = json!("false");
    let h = &mut db["host"][host.as_str()];
    h["name_label"] = json!(format!("lab-host-{tag}"));
    h["hostname"] = json!(format!("lab-host-{tag}"));
    h["name_description"] = json!("Default install");
    h["enabled"] = json!(true);
    h["other_config"] = json!({"boot_time": "1795100000.", "agent_start_time": "1795100100."});
    h["software_version"] = cap_host["software_version"].clone();
    h["license_params"] = license;
    h["edition"] = json!("xcp-ng");
    h["cpu_info"] = json!({"vendor": "GenuineIntel", "cpu_count": "4", "socket_count": "1"});
    h["external_auth_type"] = json!("");
    h["external_auth_service_name"] = json!("");
    h["iscsi_iqn"] = json!(format!("iqn.2026-01.org.example:lab-host-{tag}"));
    h["multipathing"] = json!(false);
    h["logging"] = json!({});
    h["power_on_mode"] = json!("");
    h["power_on_config"] = json!({});
    h["updates"] = json!([]);
    h["ssl_legacy"] = json!(false);
    let dom0 = &mut db["vm"][r("OpaqueRef:vm-dom0-01")];
    dom0["name_label"] = json!(format!("Control domain on host: lab-host-{tag}"));
    if let Some(sm) = all.get("sm") {
        db["sm"] = sm.clone();
    }
    db
}

// ---- calls ------------------------------------------------------------------------------

fn obj_map(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

impl Db {
    pub(crate) fn pool_call(&mut self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i);
        Some(match (class, call) {
            ("pool", "join") => self.join(),
            ("pool", "eject") => self.eject(p(1)),
            ("pool", "designate_new_master") => self.designate(p(1)),
            ("pool", "rotate_secret") => {
                self.pool.secret_rotations += 1;
                Ok(json!(""))
            }
            ("pool", "enable_tls_verification") => {
                let pools: Vec<String> = self.refs_of_class("pool");
                for r in pools {
                    self.modify("pool", &r, |rec| {
                        rec.insert("tls_verification_enabled".into(), json!(true));
                    });
                }
                for h in self.refs_of_class("host") {
                    self.modify("host", &h, |rec| {
                        rec.insert("tls_verification_enabled".into(), json!(true));
                    });
                }
                Ok(json!(""))
            }
            ("host", "syslog_reconfigure") => match self.rec("host", p(1)) {
                Some(_) => Ok(json!("")),
                None => api_error("HANDLE_INVALID", &["host", p(1)]),
            },
            ("host", "set_power_on_mode") => {
                let (h, mode) = (p(1).to_owned(), p(2).to_owned());
                if self.rec("host", &h).is_none() {
                    return Some(api_error("HANDLE_INVALID", &["host", &h]));
                }
                let config = params.get(3).cloned().unwrap_or(json!({}));
                self.modify("host", &h, |rec| {
                    rec.insert("power_on_mode".into(), json!(mode));
                    rec.insert("power_on_config".into(), config);
                });
                Ok(json!(""))
            }
            ("host", "install_server_certificate") => self.install_certificate(p(1), p(2), p(3)),
            ("host", "reset_server_certificate") => self.install_certificate(
                p(1),
                "-----BEGIN CERTIFICATE-----self-signed",
                "-----BEGIN PRIVATE KEY-----",
            ),
            ("secret", "create") => {
                let value = params
                    .get(1)
                    .and_then(|a| a.get("value"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let n = self.next_object;
                self.next_object += 1;
                let r = format!("OpaqueRef:mock-secret-{n}");
                let rec = json!({"uuid": format!("5ec0e700-0000-4000-8000-{n:012}"),
                                 "value": value, "other_config": {}});
                self.objects
                    .insert(("secret".into(), r.clone()), rec.clone());
                self.push_event("secret", "add", &r, Some(rec));
                Ok(json!(r))
            }
            ("secret", "destroy") => {
                let r = p(1).to_owned();
                match self.objects.remove(&("secret".to_owned(), r.clone())) {
                    Some(_) => {
                        self.push_event("secret", "del", &r, None);
                        Ok(json!(""))
                    }
                    None => api_error("HANDLE_INVALID", &["secret", &r]),
                }
            }
            ("Cluster", "pool_create") => self.cluster_create(p(1)),
            ("Cluster", "pool_destroy") => self.cluster_destroy(p(1)),
            ("Cluster", "get_network") => match self.rec("cluster", p(1)) {
                Some(rec) => Ok(rec.get("network").cloned().unwrap_or(json!(NULL_REF))),
                None => api_error("HANDLE_INVALID", &["Cluster", p(1)]),
            },
            ("task", "create") => {
                let n = self.next_task;
                self.next_task += 1;
                let task = format!("OpaqueRef:mock-task-{n}");
                let record = json!({
                    "uuid": format!("7a5c0000-0000-4000-8000-{n:012}"), "name_label": p(1),
                    "name_description": p(2), "allowed_operations": ["cancel"],
                    "current_operations": {}, "created": "20261125T12:00:00Z",
                    "finished": "19700101T00:00:00Z", "status": "pending",
                    "resident_on": NULL_REF, "progress": 0.0, "type": "", "result": "",
                    "error_info": [], "other_config": {}, "subtask_of": NULL_REF,
                    "subtasks": [], "backtrace": "()"
                });
                self.objects
                    .insert(("task".into(), task.clone()), record.clone());
                self.push_event("task", "add", &task, Some(record));
                Ok(json!(task))
            }
            ("session", "change_password") => {
                if p(1) != self.pool.root_password() {
                    return Some(api_error(
                        "CHANGE_PASSWORD_REJECTED",
                        &["Authentication failure"],
                    ));
                }
                if p(2).is_empty() {
                    return Some(api_error("CHANGE_PASSWORD_REJECTED", &["empty password"]));
                }
                self.pool.password = Some(p(2).to_owned());
                Ok(json!(""))
            }
            _ => return None,
        })
    }

    fn refs_of_class(&self, class: &str) -> Vec<String> {
        self.objects
            .keys()
            .filter(|(c, _)| c == class)
            .map(|(_, r)| r.clone())
            .collect()
    }

    fn pool_ref(&self) -> Option<String> {
        self.refs_of_class("pool").into_iter().next()
    }

    /// The joining server's side of `pool.join`, after the coordinator's
    /// checks passed (see [`check_join`]).
    /// The first server's external authentication (type, service name).
    pub(crate) fn external_auth(&self) -> (String, String) {
        let host = self.refs_of_class("host").into_iter().next();
        let get = |f: &str| {
            host.as_ref()
                .and_then(|h| self.rec("host", h))
                .and_then(|r| r.get(f))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        (get("external_auth_type"), get("external_auth_service_name"))
    }

    fn join(&mut self) -> Reply {
        let hosts = self.refs_of_class("host");
        if hosts.len() != 1 {
            return api_error("JOINING_HOST_CANNOT_BE_MASTER_OF_OTHER_HOSTS", &[]);
        }
        let host = hosts[0].clone();
        let running = self.objects.iter().any(|((c, _), rec)| {
            c == "vm"
                && rec.get("is_control_domain") != Some(&json!(true))
                && rec.get("is_a_template") != Some(&json!(true))
                && rec.get("power_state").and_then(Value::as_str) == Some("Running")
        });
        if running {
            return api_error("JOINING_HOST_CANNOT_HAVE_RUNNING_VMS", &[]);
        }
        let shared = self.objects.iter().any(|((c, _), rec)| {
            c == "sr"
                && rec.get("shared") == Some(&json!(true))
                && rec.get("is_tools_sr") != Some(&json!(true))
                && rec.get("content_type").and_then(Value::as_str) != Some("iso")
        });
        if shared {
            return api_error("JOINING_HOST_CANNOT_CONTAIN_SHARED_SRS", &[]);
        }
        // Found by `check_join` before the call runs.
        let Some(coordinator) = self.pool.join_target.take() else {
            return api_error("POOL_JOINING_HOST_CONNECTION_FAILED", &[]);
        };
        if self
            .pool
            .join_auth
            .take()
            .is_some_and(|a| a != self.external_auth())
        {
            return api_error("POOL_JOINING_EXTERNAL_AUTH_MISMATCH", &[]);
        }
        let objects = self.host_objects(&host);
        self.pool.standalone = Some(self.objects.clone());
        self.pool.after.push(After::JoinInto {
            coordinator: coordinator.clone(),
            objects,
            host,
        });
        self.coordinator = Some(coordinator);
        Ok(json!(""))
    }

    /// The objects of `host` that move with it: the host, its metrics,
    /// CPUs, certificates, control domain (with its metrics and consoles),
    /// NICs (with their metrics) and local SRs (with their PBDs and disks).
    fn host_objects(&self, host: &str) -> Vec<((String, String), Value)> {
        let mut out = Vec::new();
        let mut take = |class: &str, r: &str| {
            if let Some(rec) = self.objects.get(&(class.to_owned(), r.to_owned())) {
                out.push(((class.to_owned(), r.to_owned()), rec.clone()));
            }
        };
        let Some(h) = self.rec("host", host).cloned() else {
            return Vec::new();
        };
        take("host", host);
        let list = |rec: &Map<String, Value>, f: &str| -> Vec<String> {
            rec.get(f)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let one = |rec: &Map<String, Value>, f: &str| {
            rec.get(f)
                .and_then(Value::as_str)
                .unwrap_or(NULL_REF)
                .to_owned()
        };
        take("host_metrics", &one(&h, "metrics"));
        for c in list(&h, "host_CPUs") {
            take("host_cpu", &c);
        }
        for c in list(&h, "certificates") {
            take("certificate", &c);
        }
        let dom0 = one(&h, "control_domain");
        if let Some(vm) = self.rec("vm", &dom0).cloned() {
            take("vm", &dom0);
            take("vm_metrics", &one(&vm, "metrics"));
            for c in list(&vm, "consoles") {
                take("console", &c);
            }
        }
        for pif in list(&h, "PIFs") {
            if let Some(rec) = self.rec("pif", &pif).cloned() {
                take("pif", &pif);
                take("pif_metrics", &one(&rec, "metrics"));
            }
        }
        for pbd in list(&h, "PBDs") {
            let Some(rec) = self.rec("pbd", &pbd).cloned() else {
                continue;
            };
            let sr = one(&rec, "SR");
            let local = self
                .rec("sr", &sr)
                .is_some_and(|s| s.get("shared") != Some(&json!(true)));
            if local {
                take("pbd", &pbd);
                if let Some(s) = self.rec("sr", &sr).cloned() {
                    take("sr", &sr);
                    for vdi in list(&s, "VDIs") {
                        take("vdi", &vdi);
                    }
                }
            }
        }
        // The networks the NICs are on (mapped to the pool's by bridge).
        for pif in list(&h, "PIFs") {
            if let Some(rec) = self.rec("pif", &pif) {
                take("network", &one(rec, "network"));
            }
        }
        out
    }

    /// `pool.eject` on the coordinator.
    fn eject(&mut self, host: &str) -> Reply {
        let Some(h) = self.rec("host", host).cloned() else {
            return api_error("HANDLE_INVALID", &["host", host]);
        };
        let pool = self.pool_ref().unwrap_or_default();
        if self.get_str("pool", &pool, "master").as_deref() == Some(host) {
            return api_error("HOST_IS_MASTER", &[host]);
        }
        let resident = h
            .get("resident_VMs")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if resident > 1 {
            return api_error("HOST_HAS_RESIDENT_VMS", &[host]);
        }
        let objects = self.host_objects(host);
        for ((class, r), _) in &objects {
            if class == "network" {
                continue;
            }
            self.objects.remove(&(class.clone(), r.clone()));
            self.push_event(class, "del", r, None);
        }
        let gone: Vec<String> = objects.iter().map(|((_, r), _)| r.clone()).collect();
        self.unlink(&gone);
        let address = h.get("address").and_then(Value::as_str).unwrap_or_default();
        self.pool.after.push(After::Restore {
            member: address.to_owned(),
        });
        Ok(json!(""))
    }

    /// Removes `gone` from the reference lists of the remaining objects.
    fn unlink(&mut self, gone: &[String]) {
        let keys: Vec<(String, String)> = self.objects.keys().cloned().collect();
        for (class, r) in keys {
            let mut changed = false;
            if let Some(Value::Object(rec)) = self.objects.get_mut(&(class.clone(), r.clone())) {
                for v in rec.values_mut() {
                    if let Value::Array(a) = v {
                        let before = a.len();
                        a.retain(|x| !x.as_str().is_some_and(|s| gone.iter().any(|g| g == s)));
                        changed |= a.len() != before;
                    }
                }
            }
            if changed {
                let snapshot = self.objects.get(&(class.clone(), r.clone())).cloned();
                self.push_event(&class, "mod", &r, snapshot);
            }
        }
    }

    /// `pool.designate_new_master`.
    fn designate(&mut self, host: &str) -> Reply {
        let Some(address) = self.get_str("host", host, "address") else {
            return api_error("HANDLE_INVALID", &["host", host]);
        };
        let pool = self.pool_ref().unwrap_or_default();
        if self.get_str("pool", &pool, "master").as_deref() == Some(host) {
            return Ok(json!(""));
        }
        self.modify("pool", &pool, |rec| {
            rec.insert("master".into(), json!(host));
        });
        // With no mock at the new coordinator's address (a one-mock
        // fixture pool), this mock goes on answering for the pool.
        if lookup(&address).is_some() {
            self.pool.after.push(After::HandOver {
                to: address.clone(),
            });
            self.coordinator = Some(address);
        }
        Ok(json!(""))
    }

    fn install_certificate(&mut self, host: &str, certificate: &str, key: &str) -> Reply {
        let Some(h) = self.rec("host", host).cloned() else {
            return api_error("HANDLE_INVALID", &["host", host]);
        };
        if !certificate.contains("BEGIN CERTIFICATE") {
            return api_error("SERVER_CERTIFICATE_INVALID", &[]);
        }
        if !key.contains("PRIVATE KEY") {
            return api_error("SERVER_CERTIFICATE_KEY_INVALID", &[]);
        }
        let old: Vec<String> = h
            .get("certificates")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        for c in &old {
            self.objects.remove(&("certificate".to_owned(), c.clone()));
            self.push_event("certificate", "del", c, None);
        }
        let n = self.next_object;
        self.next_object += 1;
        let r = format!("OpaqueRef:mock-certificate-{n}");
        let rec = json!({
            "uuid": format!("ce000000-0000-4000-8000-{n:012}"), "name": "", "type": "host",
            "host": host, "not_before": "20261125T00:00:00Z", "not_after": "20361125T00:00:00Z",
            "fingerprint": "", "fingerprint_sha256": format!("AA:BB:CC:{:02X}", n % 256),
            "fingerprint_sha1": ""
        });
        self.objects
            .insert(("certificate".into(), r.clone()), rec.clone());
        self.push_event("certificate", "add", &r, Some(rec));
        self.modify("host", host, |rec| {
            rec.insert("certificates".into(), json!([r]));
        });
        Ok(json!(""))
    }

    fn cluster_create(&mut self, network: &str) -> Reply {
        if self.rec("network", network).is_none() {
            return api_error("HANDLE_INVALID", &["network", network]);
        }
        if !self.refs_of_class("cluster").is_empty() {
            return api_error("CLUSTER_ALREADY_EXISTS", &[]);
        }
        let n = self.next_object;
        self.next_object += 1;
        let cluster = format!("OpaqueRef:mock-cluster-{n}");
        let mut members = Vec::new();
        for host in self.refs_of_class("host") {
            let pif = self.objects.iter().find_map(|((c, r), rec)| {
                (c == "pif"
                    && rec.get("host").and_then(Value::as_str) == Some(host.as_str())
                    && rec.get("network").and_then(Value::as_str) == Some(network))
                .then(|| r.clone())
            });
            let ch = format!("OpaqueRef:mock-cluster-host-{n}-{}", members.len());
            let rec = json!({
                "uuid": format!("c1c1c1c1-0000-4000-8000-{n:06}{:06}", members.len()),
                "cluster": cluster, "host": host, "enabled": true,
                "PIF": pif.unwrap_or_else(|| NULL_REF.to_owned()), "joined": true, "live": true,
                "last_update_live": "20261125T12:00:00Z", "allowed_operations": [],
                "current_operations": {}, "other_config": {}
            });
            self.objects
                .insert(("cluster_host".into(), ch.clone()), rec.clone());
            self.push_event("cluster_host", "add", &ch, Some(rec));
            members.push(ch);
        }
        let rec = json!({
            "uuid": format!("c1c1c1c1-0000-4000-8000-{n:012}"), "cluster_hosts": members,
            "pending_forget": [], "cluster_token": "", "cluster_stack": "corosync",
            "cluster_stack_version": 3, "is_quorate": true, "quorum": 1, "live_hosts": 1,
            "expected_hosts": 1, "allowed_operations": [], "current_operations": {},
            "pool_auto_join": true, "token_timeout": 20.0, "token_timeout_coefficient": 1.0,
            "cluster_config": {}, "other_config": {}, "network": network
        });
        self.objects
            .insert(("cluster".into(), cluster.clone()), rec.clone());
        self.push_event("cluster", "add", &cluster, Some(rec));
        Ok(json!(cluster))
    }

    fn cluster_destroy(&mut self, cluster: &str) -> Reply {
        let Some(rec) = self
            .objects
            .remove(&("cluster".to_owned(), cluster.to_owned()))
        else {
            return api_error("HANDLE_INVALID", &["Cluster", cluster]);
        };
        self.push_event("cluster", "del", cluster, None);
        for ch in rec
            .get("cluster_hosts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            self.objects
                .remove(&("cluster_host".to_owned(), ch.to_owned()));
            self.push_event("cluster_host", "del", ch, None);
        }
        Ok(json!(""))
    }

    /// The `nrpe` plugin of `host.call_plugin`.
    pub(crate) fn nrpe_call(&mut self, host: &str, function: &str, args: &Value) -> Reply {
        if self.rec("host", host).is_none() {
            return api_error("HANDLE_INVALID", &["host", host]);
        }
        let n = self.pool.nrpe.entry(host.to_owned()).or_default();
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
        Ok(json!(match function {
            "status" => {
                if n.enabled {
                    "active enabled".to_owned()
                } else {
                    "inactive disabled".to_owned()
                }
            }
            "get-config" => format!(
                "allowed_hosts: {}\ndebug: {}\nssl_logging: {}\n",
                n.allowed_hosts, n.debug, n.ssl_logging
            ),
            "get-threshold" => n
                .thresholds
                .iter()
                .map(|(k, (w, c))| format!(
                    "{k} warning threshold is: {w} critical threshold is: {c}"
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            "enable" => {
                n.enabled = true;
                String::new()
            }
            "disable" => {
                n.enabled = false;
                String::new()
            }
            "control" | "restart" => String::new(),
            "set-config" => {
                if let Some(v) = arg("allowed_hosts") {
                    n.allowed_hosts = v.trim_end_matches(',').to_owned();
                }
                if let Some(v) = arg("debug") {
                    n.debug = v;
                }
                if let Some(v) = arg("ssl_logging") {
                    n.ssl_logging = v;
                }
                String::new()
            }
            "set-threshold" => {
                let (Some(w), Some(c)) = (arg("w"), arg("c")) else {
                    return api_error("XENAPI_PLUGIN_FAILURE", &["set-threshold", "missing w/c"]);
                };
                let name = args
                    .as_object()
                    .and_then(|m| m.keys().find(|k| k.starts_with("check_")).cloned());
                match name {
                    Some(name) => {
                        n.thresholds.insert(name, (w, c));
                    }
                    None => {
                        return api_error("XENAPI_PLUGIN_FAILURE", &["set-threshold", "no check"]);
                    }
                }
                String::new()
            }
            other => return api_error("XENAPI_PLUGIN_FAILURE", &[other, "unknown function"]),
        }))
    }

    /// The text of this server's backup.
    pub(crate) fn backup_body(&self, host: &str) -> Option<String> {
        let uuid = self.get_str("host", host, "uuid")?;
        let name = self.get_str("host", host, "name_label").unwrap_or_default();
        Some(format!(
            "XCP-NG-MOCK-HOST-BACKUP {uuid} {name}\n{}",
            "#".repeat(4096)
        ))
    }

    /// The host a task created by `task.create` names (its description
    /// is the host's address), or the coordinator.
    pub(crate) fn host_of_transfer(&self, task: &str) -> Option<String> {
        let address = self
            .get_str("task", task, "name_description")
            .unwrap_or_default();
        self.objects
            .iter()
            .find_map(|((c, r), rec)| {
                (c == "host"
                    && rec.get("address").and_then(Value::as_str) == Some(address.as_str()))
                .then(|| r.clone())
            })
            .or_else(|| {
                let pool = self.pool_ref()?;
                self.get_str("pool", &pool, "master")
            })
    }
}

/// The coordinator's checks of `pool.join(address, user, password)`, made
/// before the joining server does anything: `Ok(address)` of the
/// coordinator, or the error the join fails with.
/// The coordinator found by [`check_join`]: its address and its external
/// authentication (type, service name).
pub(crate) type JoinTarget = (String, (String, String));

pub(crate) fn check_join(params: &[Value]) -> Result<JoinTarget, (String, Vec<String>)> {
    let (address, user, password) = (
        str_param(params, 1),
        str_param(params, 2),
        str_param(params, 3),
    );
    let err = |code: &str, p: &[&str]| -> (String, Vec<String>) {
        (code.to_owned(), p.iter().map(|s| (*s).to_owned()).collect())
    };
    let Some(coordinator) = lookup(address) else {
        return Err(err("POOL_JOINING_HOST_CONNECTION_FAILED", &[]));
    };
    let db = coordinator.db();
    let root_ok = user == crate::USERNAME && password == db.pool.root_password();
    let rbac_ok = crate::ops::login(user, password).is_some_and(|r| r == Some("pool-admin"));
    if !root_ok && !rbac_ok {
        return Err(err(
            "SESSION_AUTHENTICATION_FAILED",
            &[user, "Authentication failure"],
        ));
    }
    if db.coordinator.is_some() {
        return Err(err("HOST_IS_SLAVE", &[address]));
    }
    let ha = db
        .objects
        .iter()
        .any(|((c, _), rec)| c == "pool" && rec.get("ha_enabled") == Some(&json!(true)));
    if ha {
        return Err(err("HA_IS_ENABLED", &[]));
    }
    Ok((db.pool.authority.clone(), db.external_auth()))
}

/// Runs the cross-server work left by the last call of `state`.
pub(crate) fn run_after(state: &Arc<State>) {
    let work: Vec<After> = std::mem::take(&mut state.db().pool.after);
    for w in work {
        match w {
            After::JoinInto {
                coordinator,
                objects,
                host,
            } => {
                if let Some(c) = lookup(&coordinator) {
                    let me = state.db().pool.authority.clone();
                    c.db().absorb(objects, &host, &me);
                    c.changed.notify_waiters();
                }
                // The joining server's API restarts a moment later.
                let state = state.clone();
                state.runtime.clone().spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    state.db().sessions.clear();
                    state.changed.notify_waiters();
                });
            }
            After::Restore { member } => {
                if let Some(m) = lookup(&member) {
                    let mut db = m.db();
                    if let Some(mut objects) = db.pool.standalone.take() {
                        // Ejecting resets the server: its VMs are gone.
                        objects.retain(|(c, _), rec| {
                            !(c == "vm" && rec.get("is_control_domain") != Some(&json!(true)))
                        });
                        db.objects = objects;
                    }
                    db.coordinator = None;
                    db.sessions.clear();
                    db.events.clear();
                    db.seq += 1;
                    drop(db);
                    m.changed.notify_waiters();
                }
            }
            After::HandOver { to } => {
                let objects = state.db().objects.clone();
                if let Some(n) = lookup(&to) {
                    let mut db = n.db();
                    db.objects = objects;
                    db.coordinator = None;
                    db.pool.standalone = None;
                    db.sessions.clear();
                    db.events.clear();
                    db.seq += 1;
                    drop(db);
                    n.changed.notify_waiters();
                }
                let state = state.clone();
                state.runtime.clone().spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    state.db().sessions.clear();
                    state.changed.notify_waiters();
                });
            }
        }
    }
}

impl Db {
    /// The coordinator's side of a join: adds the joined server's objects,
    /// each NIC on the pool's network of the same bridge.
    fn absorb(&mut self, objects: Vec<((String, String), Value)>, host: &str, address: &str) {
        let bridge_of = |objs: &[((String, String), Value)], net: &str| {
            objs.iter()
                .find(|((c, r), _)| c == "network" && r == net)
                .and_then(|(_, rec)| rec.get("bridge").and_then(Value::as_str))
                .map(str::to_owned)
        };
        let mut net_map: HashMap<String, String> = HashMap::new();
        for ((class, r), rec) in &objects {
            if class != "network" {
                continue;
            }
            let bridge = rec
                .get("bridge")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let existing = self.objects.iter().find_map(|((c, nr), nrec)| {
                (c == "network" && nrec.get("bridge").and_then(Value::as_str) == Some(bridge))
                    .then(|| nr.clone())
            });
            match existing {
                Some(e) => {
                    net_map.insert(r.clone(), e);
                }
                None => {
                    let mut copy = obj_map(rec);
                    copy.insert("PIFs".into(), json!([]));
                    copy.insert("VIFs".into(), json!([]));
                    self.objects
                        .insert(("network".into(), r.clone()), Value::Object(copy.clone()));
                    self.push_event("network", "add", r, Some(Value::Object(copy)));
                    net_map.insert(r.clone(), r.clone());
                }
            }
        }
        let _ = bridge_of;
        let pool_master = self
            .pool_ref()
            .and_then(|p| self.get_str("pool", &p, "master"));
        for ((class, r), rec) in objects {
            if class == "network" {
                continue;
            }
            let mut rec = obj_map(&rec);
            if class == "pif"
                && let Some(net) = rec
                    .get("network")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            {
                let target = net_map.get(&net).cloned().unwrap_or(net);
                rec.insert("network".into(), json!(target));
                self.modify("network", &target, |n| {
                    if let Some(Value::Array(p)) = n.get_mut("PIFs") {
                        p.push(json!(r));
                    }
                });
            }
            if class == "host" {
                rec.insert("address".into(), json!(address));
            }
            if class == "sr"
                && let Some(uuid) = rec.get("uuid").and_then(Value::as_str)
            {
                self.storage.register_local_sr(uuid);
            }
            let value = Value::Object(rec);
            self.objects
                .insert((class.clone(), r.clone()), value.clone());
            self.push_event(&class, "add", &r, Some(value));
        }
        let _ = (host, pool_master);
    }
}
