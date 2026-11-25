//! VM and host operations, RBAC and prechecks of the mock.
//!
//! **Users.** Besides [`crate::USERNAME`] (a local superuser), the users in
//! [`RBAC_USERS`] log in with one XAPI role each. Every write call checks
//! the session's role against a permission table that mirrors XAPI's role
//! ladder (read-only < vm-operator < vm-admin < vm-power-admin <
//! pool-operator < pool-admin) for the calls the console makes; a missing
//! permission fails with `RBAC_PERMISSION_DENIED`. `role` objects describing
//! the same table are added to a database that has none.
//!
//! **VM operations.** `VM.start`, `start_on`, `clean_shutdown`,
//! `hard_shutdown`, `clean_reboot`, `hard_reboot`, `suspend`, `resume`,
//! `resume_on`, `pause`, `unpause` and `pool_migrate` (and their `Async.*`
//! forms) check the power state, `allowed_operations` and the target host
//! like XAPI, then change `power_state`, `resident_on`,
//! `allowed_operations` and the hosts' `resident_VMs`. While an `Async.*`
//! operation runs, the VM lists it in `current_operations` and only the
//! forced operations stay allowed. `VM.assert_can_boot_here` checks the host.
//!
//! **Host operations.** `host.disable` / `enable`, `add_to_other_config` /
//! `remove_from_other_config`, `evacuate` (migrates the running guests to
//! another live, enabled host), `reboot` / `shutdown` (the host must be
//! disabled and run no guest; shutdown makes it not live and offers
//! `power_on`), `power_on`, `restart_agent` and
//! `get_vms_which_prevent_evacuation`. Rebooting a host or restarting its
//! toolstack forgets every session once the task ends, as the real server
//! would.

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// Test users with one role each: `(user, password, role)`. Test-only
/// credentials.
pub const RBAC_USERS: &[(&str, &str, &str)] = &[
    ("read-only-user", "read-only-password", "read-only"),
    ("vm-operator-user", "vm-operator-password", "vm-operator"),
    (
        "vm-power-admin-user",
        "vm-power-admin-password",
        "vm-power-admin",
    ),
    (
        "pool-operator-user",
        "pool-operator-password",
        "pool-operator",
    ),
    ("pool-admin-user", "pool-admin-password", "pool-admin"),
];

const ROLES: &[&str] = &[
    "read-only",
    "vm-operator",
    "vm-admin",
    "vm-power-admin",
    "pool-operator",
    "pool-admin",
];

/// Permissions checked by the mock, with the lowest role holding each.
const PERMISSIONS: &[(&str, &str)] = &[
    ("task.destroy", "read-only"),
    ("task.cancel", "read-only"),
    ("vm.assert_can_boot_here", "read-only"),
    ("host.get_vms_which_prevent_evacuation", "read-only"),
    ("vm.start", "vm-operator"),
    ("vm.clean_shutdown", "vm-operator"),
    ("vm.clean_reboot", "vm-operator"),
    ("vm.hard_shutdown", "vm-operator"),
    ("vm.hard_reboot", "vm-operator"),
    ("vm.suspend", "vm-operator"),
    ("vm.resume", "vm-operator"),
    ("vm.pause", "vm-operator"),
    ("vm.unpause", "vm-operator"),
    ("vm.start_on", "vm-power-admin"),
    ("vm.resume_on", "vm-power-admin"),
    ("vm.pool_migrate", "vm-power-admin"),
    ("host.disable", "pool-operator"),
    ("host.enable", "pool-operator"),
    ("host.evacuate", "pool-operator"),
    ("host.reboot", "pool-operator"),
    ("host.shutdown", "pool-operator"),
    ("host.power_on", "pool-operator"),
    ("host.restart_agent", "pool-operator"),
    ("host.add_to_other_config", "pool-operator"),
    ("host.remove_from_other_config", "pool-operator"),
    ("message.destroy", "pool-operator"),
    ("message.create", "pool-operator"),
    ("vm.add_to_other_config/key:perfmon", "pool-operator"),
    ("vm.remove_from_other_config/key:perfmon", "pool-operator"),
    ("host.add_to_other_config/key:perfmon", "pool-operator"),
    ("host.remove_from_other_config/key:perfmon", "pool-operator"),
    ("sr.add_to_other_config/key:perfmon", "pool-operator"),
    ("sr.remove_from_other_config/key:perfmon", "pool-operator"),
    ("vbd.insert", "vm-operator"),
    ("vbd.eject", "vm-operator"),
    ("vm.snapshot", "vm-power-admin"),
    ("vm.checkpoint", "vm-power-admin"),
    ("vm.revert", "vm-power-admin"),
    ("vm.clone", "vm-admin"),
    ("vm.copy", "vm-admin"),
    ("vm.provision", "vm-admin"),
    ("vm.destroy", "vm-admin"),
    ("vm.set_is_a_template", "vm-admin"),
    ("vm.set_name_label", "vm-admin"),
    ("vm.set_name_description", "vm-admin"),
    ("vm.set_affinity", "vm-admin"),
    ("vm.set_memory_limits", "vm-admin"),
    ("vm.set_vcpus_max", "vm-admin"),
    ("vm.set_vcpus_at_startup", "vm-admin"),
    ("vm.set_hvm_boot_params", "vm-admin"),
    ("vm.set_platform", "vm-admin"),
    ("vm.set_other_config", "vm-admin"),
    ("vm.add_to_platform", "vm-admin"),
    ("vm.remove_from_platform", "vm-admin"),
    ("vm.add_to_hvm_boot_params", "vm-admin"),
    ("vm.remove_from_hvm_boot_params", "vm-admin"),
    ("vm.add_to_other_config", "vm-admin"),
    ("vm.remove_from_other_config", "vm-admin"),
    ("vdi.create", "vm-admin"),
    ("vdi.destroy", "vm-admin"),
    ("vdi.resize", "vm-admin"),
    ("vdi.resize_online", "vm-admin"),
    ("vdi.copy", "vm-admin"),
    ("vdi.set_name_label", "vm-admin"),
    ("vdi.set_name_description", "vm-admin"),
    ("vdi.pool_migrate", "vm-power-admin"),
    ("vbd.create", "vm-admin"),
    ("vbd.destroy", "vm-admin"),
    ("vbd.plug", "vm-admin"),
    ("vbd.unplug", "vm-admin"),
    ("vif.create", "vm-admin"),
    ("vif.destroy", "vm-admin"),
    ("vtpm.create", "vm-admin"),
    ("vtpm.destroy", "vm-admin"),
    ("vm.set_suspend_sr", "vm-admin"),
    ("vm.set_has_vendor_device", "vm-admin"),
    ("vdi.set_sm_config", "vm-admin"),
    ("vdi.add_to_sm_config", "vm-admin"),
    ("vdi.remove_from_sm_config", "vm-admin"),
];

fn rank(role: &str) -> usize {
    ROLES.iter().position(|r| *r == role).unwrap_or(0)
}

/// The permission a call needs, if the mock checks it.
pub(crate) fn required_permission(method: &str) -> Option<&'static str> {
    let name = method.trim_start_matches("Async.").to_lowercase();
    PERMISSIONS
        .iter()
        .find(|(p, _)| *p == name)
        .map(|(p, _)| *p)
}

/// Whether a session with `role` (`None`: superuser) holds `permission`.
pub(crate) fn may(role: Option<&str>, permission: &str) -> bool {
    let Some(role) = role else { return true };
    PERMISSIONS
        .iter()
        .find(|(p, _)| *p == permission)
        .is_none_or(|(_, min)| rank(role) >= rank(min))
}

/// `session.get_rbac_permissions`: the role's name and its permissions.
pub(crate) fn permissions_of(role: Option<&str>) -> Vec<String> {
    let role = role.unwrap_or("pool-admin");
    std::iter::once(role.to_owned())
        .chain(
            PERMISSIONS
                .iter()
                .filter(|(_, min)| rank(role) >= rank(min))
                .map(|(p, _)| (*p).to_owned()),
        )
        .collect()
}

/// The role of `user` with `password`: `Some(None)` for the superuser,
/// `Some(Some(role))` for an RBAC user, `None` if the credentials are wrong.
pub(crate) fn login(user: &str, password: &str) -> Option<Option<&'static str>> {
    if user == crate::USERNAME && password == crate::PASSWORD {
        return Some(None);
    }
    RBAC_USERS
        .iter()
        .find(|(u, p, _)| *u == user && *p == password)
        .map(|(_, _, r)| Some(*r))
}

/// `role` objects for the permission table (added when the database has
/// none).
pub(crate) fn role_objects() -> Vec<(String, Value)> {
    let perm_ref = |p: &str| format!("OpaqueRef:role-{}", p.replace(['.', '_'], "-"));
    let mut out: Vec<(String, Value)> = PERMISSIONS
        .iter()
        .map(|(p, _)| {
            (
                perm_ref(p),
                json!({"uuid": format!("role-{p}"), "name_label": p, "name_description": "",
                       "subroles": [], "is_internal": false}),
            )
        })
        .collect();
    for role in ROLES {
        let subroles: Vec<String> = PERMISSIONS
            .iter()
            .filter(|(_, min)| rank(role) >= rank(min))
            .map(|(p, _)| perm_ref(p))
            .collect();
        out.push((
            format!("OpaqueRef:role-{role}"),
            json!({"uuid": format!("role-{role}"), "name_label": role, "name_description": "",
                   "subroles": subroles, "is_internal": false}),
        ));
    }
    out
}

const ALLOWED_HALTED: &[&str] = &[
    "start",
    "start_on",
    "clone",
    "copy",
    "export",
    "destroy",
    "snapshot",
    "make_into_template",
];
const ALLOWED_RUNNING: &[&str] = &[
    "clean_shutdown",
    "clean_reboot",
    "hard_shutdown",
    "hard_reboot",
    "suspend",
    "pause",
    "snapshot",
    "checkpoint",
    "pool_migrate",
];
const ALLOWED_SUSPENDED: &[&str] = &["resume", "resume_on", "hard_shutdown", "snapshot"];
const ALLOWED_PAUSED: &[&str] = &["unpause", "hard_shutdown", "hard_reboot"];
/// What a live host allows.
pub(crate) const HOST_ALLOWED_LIVE: &[&str] = &[
    "provision",
    "evacuate",
    "shutdown",
    "reboot",
    "vm_start",
    "vm_resume",
    "vm_migrate",
];

/// `allowed_operations` of a VM in `power_state`.
pub(crate) fn allowed_for(power_state: &str) -> &'static [&'static str] {
    match power_state {
        "Running" => ALLOWED_RUNNING,
        "Halted" => ALLOWED_HALTED,
        "Suspended" => ALLOWED_SUSPENDED,
        "Paused" => ALLOWED_PAUSED,
        _ => &["hard_shutdown"],
    }
}

/// What a task does when it ends.
pub(crate) enum Work {
    Nothing,
    /// A VM ends up in `power_state` on `host` (`None`: keep or choose).
    Vm {
        vm: String,
        power_state: &'static str,
        host: Option<String>,
    },
    Host {
        host: String,
        effect: HostEffect,
    },
    Fail((String, Vec<String>)),
    /// Runs `class.call` (see [`crate::provisioning`]) when the task ends;
    /// its result becomes the task's.
    Call {
        class: String,
        call: String,
        params: Vec<Value>,
    },
}

pub(crate) enum HostEffect {
    Disable,
    Enable,
    Evacuate,
    Reboot,
    Shutdown,
    PowerOn,
    RestartAgent,
}

impl Db {
    fn rec(&self, class: &str, r: &str) -> Option<&Map<String, Value>> {
        self.objects
            .get(&(class.to_owned(), r.to_owned()))
            .and_then(Value::as_object)
    }

    fn vm_allows(&self, vm: &str, op: &str) -> bool {
        self.rec("vm", vm)
            .and_then(|r| r.get("allowed_operations"))
            .and_then(Value::as_array)
            .is_some_and(|ops| ops.iter().any(|o| o.as_str() == Some(op)))
    }

    pub(crate) fn host_live(&self, host: &str) -> bool {
        let Some(metrics) = self.get_str("host", host, "metrics") else {
            return false;
        };
        self.rec("host_metrics", &metrics)
            .and_then(|m| m.get("live"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    fn host_enabled(&self, host: &str) -> bool {
        self.rec("host", host)
            .and_then(|h| h.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn host_refs(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .objects
            .keys()
            .filter(|(c, _)| c == "host")
            .map(|(_, r)| r.clone())
            .collect();
        // The coordinator first.
        let master = self
            .objects
            .iter()
            .find(|((c, _), _)| c == "pool")
            .and_then(|(_, p)| p.get("master"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        hosts.sort_by_key(|h| Some(h) != master.as_ref());
        hosts
    }

    /// Whether `host` can run a VM now (else the XAPI error).
    fn host_usable(&self, host: &str) -> Result<(), (String, Vec<String>)> {
        if self.rec("host", host).is_none() {
            return Err(("HANDLE_INVALID".into(), vec!["host".into(), host.into()]));
        }
        if !self.host_live(host) {
            return Err(("HOST_OFFLINE".into(), vec![host.into()]));
        }
        if !self.host_enabled(host) {
            return Err(("HOST_DISABLED".into(), vec![host.into()]));
        }
        Ok(())
    }

    /// The running guests resident on `host`.
    fn guests_on(&self, host: &str) -> Vec<String> {
        self.objects
            .iter()
            .filter(|((c, _), rec)| {
                c == "vm"
                    && rec.get("resident_on").and_then(Value::as_str) == Some(host)
                    && rec.get("is_control_domain").and_then(Value::as_bool) != Some(true)
                    && matches!(
                        rec.get("power_state").and_then(Value::as_str),
                        Some("Running" | "Paused")
                    )
            })
            .map(|((_, r), _)| r.clone())
            .collect()
    }

    /// Checks a VM operation; returns what it will do.
    pub(crate) fn check_vm_op(
        &self,
        call: &str,
        params: &[Value],
    ) -> Result<Work, (String, Vec<String>)> {
        let vm = str_param(params, 1).to_owned();
        let Some(rec) = self.rec("vm", &vm) else {
            return Err(("HANDLE_INVALID".into(), vec!["VM".into(), vm]));
        };
        let flag = |name: &str| rec.get(name).and_then(Value::as_bool).unwrap_or(false);
        let state = rec
            .get("power_state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (op, required, target): (&str, &[&str], &'static str) = match call {
            "start" => ("start", &["Halted"], "Running"),
            "start_on" => ("start_on", &["Halted"], "Running"),
            "clean_shutdown" => ("clean_shutdown", &["Running"], "Halted"),
            "hard_shutdown" => (
                "hard_shutdown",
                &["Running", "Paused", "Suspended"],
                "Halted",
            ),
            "clean_reboot" => ("clean_reboot", &["Running"], "Running"),
            "hard_reboot" => ("hard_reboot", &["Running", "Paused"], "Running"),
            "suspend" => ("suspend", &["Running"], "Suspended"),
            "resume" => ("resume", &["Suspended"], "Running"),
            "resume_on" => ("resume_on", &["Suspended"], "Running"),
            "pause" => ("pause", &["Running"], "Paused"),
            "unpause" => ("unpause", &["Paused"], "Running"),
            "pool_migrate" => ("pool_migrate", &["Running"], "Running"),
            _ => return Err(("MESSAGE_METHOD_UNKNOWN".into(), vec![format!("VM.{call}")])),
        };
        // A snapshot with memory can be forced off (before it is deleted).
        let suspended_snapshot =
            flag("is_a_snapshot") && op == "hard_shutdown" && state == "Suspended";
        if (flag("is_a_template") || flag("is_control_domain") || flag("is_a_snapshot"))
            && !suspended_snapshot
        {
            return Err(("OPERATION_NOT_ALLOWED".into(), vec![op.into()]));
        }
        if !required.contains(&state.as_str()) {
            return Err((
                "VM_BAD_POWER_STATE".into(),
                vec![vm, required[0].to_lowercase(), state.to_lowercase()],
            ));
        }
        if !self.vm_allows(&vm, op) && !suspended_snapshot {
            if op == "clean_shutdown" || op == "clean_reboot" {
                // No guest agent: the cooperative operations are missing.
                return Err(("VM_LACKS_FEATURE".into(), vec![vm]));
            }
            return Err(("OPERATION_NOT_ALLOWED".into(), vec![op.into()]));
        }
        let host = match call {
            "start_on" | "resume_on" | "pool_migrate" => {
                let host = str_param(params, 2).to_owned();
                self.host_usable(&host)?;
                if call == "pool_migrate"
                    && rec.get("resident_on").and_then(Value::as_str) == Some(host.as_str())
                {
                    return Err(("HOST_NOT_ENOUGH_FREE_MEMORY".into(), vec![]));
                }
                Some(host)
            }
            "start" | "resume" => {
                // Where XAPI would place it: its home server, else any.
                let affinity = rec
                    .get("affinity")
                    .and_then(Value::as_str)
                    .unwrap_or(NULL_REF);
                let host = if self.host_usable(affinity).is_ok() {
                    affinity.to_owned()
                } else {
                    match self
                        .host_refs()
                        .into_iter()
                        .find(|h| self.host_usable(h).is_ok())
                    {
                        Some(h) => h,
                        None => return Err(("NO_HOSTS_AVAILABLE".into(), vec![])),
                    }
                };
                Some(host)
            }
            _ => None,
        };
        Ok(Work::Vm {
            vm,
            power_state: target,
            host,
        })
    }

    /// Marks a VM busy with `op` (task `task`): only the forced operations
    /// stay allowed, as on a real server.
    pub(crate) fn begin_vm_op(&mut self, vm: &str, op: &str, task: &str) {
        self.modify("vm", vm, |rec| {
            rec.insert("current_operations".into(), json!({ task: op }));
            let forced: Vec<&str> = ["hard_shutdown", "hard_reboot"]
                .into_iter()
                .filter(|o| {
                    rec.get("allowed_operations")
                        .and_then(Value::as_array)
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(o)))
                })
                .collect();
            rec.insert("allowed_operations".into(), json!(forced));
        });
    }

    /// Recomputes a VM's operations from its power state (after a task
    /// ended without changing it).
    pub(crate) fn settle_vm(&mut self, vm: &str) {
        let state = self.get_str("vm", vm, "power_state").unwrap_or_default();
        self.modify("vm", vm, |rec| {
            rec.insert("allowed_operations".into(), json!(allowed_for(&state)));
            rec.insert("current_operations".into(), json!({}));
        });
    }

    /// `host.get_vms_which_prevent_evacuation`.
    pub(crate) fn evacuation_blockers(&self, host: &str) -> Map<String, Value> {
        let targets: Vec<String> = self
            .host_refs()
            .into_iter()
            .filter(|h| h != host && self.host_usable(h).is_ok())
            .collect();
        let mut out = Map::new();
        for vm in self.guests_on(host) {
            let reason = if !self.vm_allows(&vm, "pool_migrate") {
                vec!["VM_LACKS_FEATURE".to_owned(), vm.clone()]
            } else if targets.is_empty() {
                vec!["NO_HOSTS_AVAILABLE".to_owned()]
            } else {
                continue;
            };
            out.insert(vm, json!(reason));
        }
        out
    }

    /// Checks a host operation; returns what it will do.
    pub(crate) fn check_host_op(
        &self,
        call: &str,
        params: &[Value],
    ) -> Result<Work, (String, Vec<String>)> {
        let host = str_param(params, 1).to_owned();
        if self.rec("host", &host).is_none() {
            return Err(("HANDLE_INVALID".into(), vec!["host".into(), host]));
        }
        let live = self.host_live(&host);
        let effect = match call {
            "power_on" => {
                if live {
                    return Err(("OPERATION_NOT_ALLOWED".into(), vec!["host is live".into()]));
                }
                if self
                    .get_str("host", &host, "power_on_mode")
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err(("HOST_POWER_ON_MODE_DISABLED".into(), vec![]));
                }
                HostEffect::PowerOn
            }
            _ if !live => return Err(("HOST_NOT_LIVE".into(), vec![])),
            "disable" => HostEffect::Disable,
            "enable" => HostEffect::Enable,
            "restart_agent" => HostEffect::RestartAgent,
            "evacuate" => {
                if let Some((vm, reason)) = self.evacuation_blockers(&host).into_iter().next() {
                    let mut reason: Vec<String> = reason
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    if reason.len() == 1 {
                        reason.push(vm);
                    }
                    let code = reason.remove(0);
                    return Err((code, reason));
                }
                HostEffect::Evacuate
            }
            "reboot" | "shutdown" => {
                if self.host_enabled(&host) {
                    return Err(("HOST_NOT_DISABLED".into(), vec![]));
                }
                if let Some(vm) = self.guests_on(&host).into_iter().next() {
                    return Err(("HOST_IN_USE".into(), vec![host, "VM".into(), vm]));
                }
                if call == "reboot" {
                    HostEffect::Reboot
                } else {
                    HostEffect::Shutdown
                }
            }
            _ => {
                return Err((
                    "MESSAGE_METHOD_UNKNOWN".into(),
                    vec![format!("host.{call}")],
                ));
            }
        };
        Ok(Work::Host { host, effect })
    }

    /// Applies what a task did. Returns whether every session must be
    /// forgotten (the toolstack restarted).
    pub(crate) fn apply(&mut self, work: &Work) -> bool {
        match work {
            Work::Nothing | Work::Fail(_) | Work::Call { .. } => false,
            Work::Vm {
                vm,
                power_state,
                host,
            } => {
                self.move_vm(vm, power_state, host.clone());
                false
            }
            Work::Host { host, effect } => self.apply_host(host, effect),
        }
    }

    fn set_live(&mut self, host: &str, live: bool) {
        if let Some(metrics) = self.get_str("host", host, "metrics") {
            self.modify("host_metrics", &metrics, |m| {
                m.insert("live".into(), json!(live));
            });
        }
        let allowed: &[&str] = if live {
            HOST_ALLOWED_LIVE
        } else {
            &["power_on"]
        };
        self.modify("host", host, |rec| {
            rec.insert("allowed_operations".into(), json!(allowed));
        });
    }

    fn apply_host(&mut self, host: &str, effect: &HostEffect) -> bool {
        let set_enabled = |db: &mut Db, enabled: bool| {
            db.modify("host", host, |rec| {
                rec.insert("enabled".into(), json!(enabled));
            });
        };
        match effect {
            HostEffect::Disable => set_enabled(self, false),
            HostEffect::Enable => set_enabled(self, true),
            HostEffect::Evacuate => {
                let targets: Vec<String> = self
                    .host_refs()
                    .into_iter()
                    .filter(|h| h != host && self.host_usable(h).is_ok())
                    .collect();
                if let Some(target) = targets.first() {
                    for vm in self.guests_on(host) {
                        let state = self.get_str("vm", &vm, "power_state").unwrap_or_default();
                        let state = if state == "Paused" {
                            "Paused"
                        } else {
                            "Running"
                        };
                        self.move_vm(&vm, state, Some(target.clone()));
                    }
                }
            }
            HostEffect::Shutdown => {
                self.set_live(host, false);
            }
            HostEffect::PowerOn => {
                self.set_live(host, true);
                set_enabled(self, true);
            }
            HostEffect::Reboot => {
                // The host comes back enabled, with a new boot time.
                set_enabled(self, true);
                self.modify("host", host, |rec| {
                    if let Some(Value::Object(oc)) = rec.get_mut("other_config") {
                        oc.insert("boot_time".into(), json!("1795200000."));
                        oc.insert("agent_start_time".into(), json!("1795200060."));
                    }
                });
                return self.is_master(host);
            }
            HostEffect::RestartAgent => {
                self.modify("host", host, |rec| {
                    if let Some(Value::Object(oc)) = rec.get_mut("other_config") {
                        oc.insert("agent_start_time".into(), json!("1795200060."));
                    }
                });
                return self.is_master(host);
            }
        }
        false
    }

    fn is_master(&self, host: &str) -> bool {
        self.objects
            .iter()
            .any(|((c, _), p)| c == "pool" && p.get("master").and_then(Value::as_str) == Some(host))
    }

    /// `<class>.add_to_other_config` / `remove_from_other_config`.
    pub(crate) fn other_config(&mut self, class: &str, call: &str, params: &[Value]) -> Reply {
        let (r, key) = (
            str_param(params, 1).to_owned(),
            str_param(params, 2).to_owned(),
        );
        let Some(rec) = self.rec(class, &r) else {
            return api_error("HANDLE_INVALID", &[class, &r]);
        };
        let present = rec
            .get("other_config")
            .and_then(Value::as_object)
            .is_some_and(|m| m.contains_key(&key));
        if call == "add_to_other_config" {
            if present {
                return api_error("MAP_DUPLICATE_KEY", &[class, "other_config", &r, &key]);
            }
            let value = str_param(params, 3).to_owned();
            self.modify(class, &r, |rec| {
                let map = rec.entry("other_config").or_insert_with(|| json!({}));
                if let Value::Object(m) = map {
                    m.insert(key, json!(value));
                }
            });
        } else if present {
            self.modify(class, &r, |rec| {
                if let Some(Value::Object(m)) = rec.get_mut("other_config") {
                    m.remove(&key);
                }
            });
        }
        Ok(json!(""))
    }

    /// `VM.assert_can_boot_here`.
    pub(crate) fn assert_can_boot_here(&self, params: &[Value]) -> Reply {
        let (vm, host) = (str_param(params, 1), str_param(params, 2));
        if self.rec("vm", vm).is_none() {
            return api_error("HANDLE_INVALID", &["VM", vm]);
        }
        self.host_usable(host).map(|()| json!(""))
    }
}
