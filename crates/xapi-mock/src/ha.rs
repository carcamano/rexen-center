//! High availability in the mock.
//!
//! **The plan.** [`Db::ha_max_failures`] stands in for XAPI's planner: the
//! protected VMs (restart priority `restart`) that run need their static
//! memory; the pool tolerates `n` host failures when the `k - n` smallest
//! live, enabled hosts (by `memory_total` less `memory_overhead`) still
//! hold them all, and never more than `k - 1`.
//!
//! **Calls.**
//!
//! * `pool.ha_compute_hypothetical_max_host_failures_to_tolerate(config)`
//!   (VM → priority; only `restart` counts) and
//!   `pool.ha_compute_max_host_failures_to_tolerate()`.
//! * `SR.assert_can_host_ha_statefile(sr)`: the SR must be shared
//!   (`SR_NOT_SHARED`), hold disks (`SR_OPERATION_NOT_SUPPORTED` for ISO
//!   libraries) and be plugged on every server (`SR_HAS_NO_PBDS`,
//!   `SR_NO_PBDS`).
//! * `VM.assert_agile(vm)`: every disk on a shared SR
//!   (`HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED`) and every interface on a
//!   network of every server (`HA_CONSTRAINT_VIOLATION_NETWORK_NOT_SHARED`).
//! * `pool.enable_ha(srs, config)` (`HA_IS_ENABLED`, the SR checks): a
//!   statefile and a metadata disk on the first SR; the pool's and every
//!   server's `ha_statefiles`, the servers' `ha_network_peers`,
//!   `ha_plan_exists_for` and `ha_overcommitted`. `pool.disable_ha()`
//!   (`HA_NOT_ENABLED`) undoes it and destroys the disks.
//! * `pool.set_ha_host_failures_to_tolerate(n)`: with HA on, more than the
//!   plan allows is refused (`HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN`).
//! * `VM.set_ha_restart_priority(vm, p)` (`VALUE_NOT_SUPPORTED`; with HA on,
//!   `restart` needs an agile VM and a plan that still holds),
//!   `VM.set_order`, `VM.set_start_delay`, `pool.sync_database`.
//! * With HA on, starting or resuming a protected VM, and disabling a
//!   server (maintenance mode, reboot, shutdown), are refused with
//!   `HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN` when the plan would no longer
//!   cover the configured failures.

use serde_json::{Value, json};

use crate::provisioning::{int, int_param};
use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// Restart priorities the server accepts.
const PRIORITIES: &[&str] = &["", "restart", "best-effort", "0", "1"];

fn protects(priority: &str) -> bool {
    matches!(priority, "restart" | "0" | "1")
}

/// Calls handled by [`Db::ha_call`].
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        (
            "pool",
            "enable_ha"
                | "disable_ha"
                | "set_ha_host_failures_to_tolerate"
                | "sync_database"
                | "ha_compute_hypothetical_max_host_failures_to_tolerate"
                | "ha_compute_max_host_failures_to_tolerate"
        ) | ("SR", "assert_can_host_ha_statefile")
            | (
                "VM",
                "assert_agile" | "set_ha_restart_priority" | "set_order" | "set_start_delay"
            )
    )
}

impl Db {
    fn the_pool(&self) -> String {
        self.refs("pool").into_iter().next().unwrap_or_default()
    }

    fn ha_on(&self) -> bool {
        let pool = self.the_pool();
        self.record("pool", &pool)
            .and_then(|p| p.get("ha_enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn ntol(&self) -> i64 {
        let pool = self.the_pool();
        self.record("pool", &pool)
            .map(|p| int(p, "ha_host_failures_to_tolerate"))
            .unwrap_or(0)
    }

    /// The VMs protected now (restart priority `restart`).
    fn protected_vms(&self) -> Vec<String> {
        self.refs("vm")
            .into_iter()
            .filter(|v| {
                self.record("vm", v).is_some_and(|r| {
                    protects(
                        r.get("ha_restart_priority")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                })
            })
            .collect()
    }

    /// The planner (see the module documentation): `restart` VMs are
    /// protected, `running` lists extra VMs counted as running, `excluded`
    /// a host counted as unavailable.
    pub(crate) fn ha_max_failures(
        &self,
        restart: &[String],
        running: &[&str],
        excluded: Option<&str>,
    ) -> i64 {
        let mut capacity: Vec<i64> = self
            .refs("host")
            .into_iter()
            .filter(|h| Some(h.as_str()) != excluded)
            .filter(|h| self.host_live(h))
            .filter(|h| {
                self.record("host", h)
                    .and_then(|r| r.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .map(|h| {
                let rec = self.record("host", &h);
                let overhead = rec.map(|r| int(r, "memory_overhead")).unwrap_or(0);
                let total = rec
                    .and_then(|r| r.get("metrics"))
                    .and_then(Value::as_str)
                    .and_then(|m| self.record("host_metrics", m))
                    .map(|m| int(m, "memory_total"))
                    .unwrap_or(0);
                total - overhead
            })
            .collect();
        if capacity.is_empty() {
            return 0;
        }
        capacity.sort_unstable();
        let need: i64 = restart
            .iter()
            .filter_map(|v| self.record("vm", v).map(|r| (v, r)))
            .filter(|(v, r)| {
                running.contains(&v.as_str())
                    || matches!(
                        r.get("power_state").and_then(Value::as_str),
                        Some("Running" | "Paused")
                    )
            })
            .map(|(_, r)| int(r, "memory_static_max"))
            .sum();
        let k = capacity.len() as i64;
        (0..k)
            .rev()
            .find(|n| capacity[..(k - n) as usize].iter().sum::<i64>() >= need)
            .unwrap_or(0)
    }

    fn would_break(&self) -> Reply {
        api_error("HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN", &[])
    }

    /// Whether starting or resuming `vm` breaks the plan (HA on, the VM
    /// protected).
    pub(crate) fn ha_blocks_start(&self, vm: &str) -> Option<(String, Vec<String>)> {
        let protected = self.protected_vms();
        if !self.ha_on() || !protected.iter().any(|v| v == vm) {
            return None;
        }
        (self.ha_max_failures(&protected, &[vm], None) < self.ntol())
            .then(|| ("HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN".to_owned(), vec![]))
    }

    /// Whether disabling `host` breaks the plan.
    pub(crate) fn ha_blocks_disable(&self, host: &str) -> Option<(String, Vec<String>)> {
        if !self.ha_on() {
            return None;
        }
        let protected = self.protected_vms();
        (self.ha_max_failures(&protected, &[], Some(host)) < self.ntol())
            .then(|| ("HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN".to_owned(), vec![]))
    }

    fn assert_statefile_sr(&self, sr: &str) -> Result<(), (String, Vec<String>)> {
        let err = |code: &str| Err((code.to_owned(), vec![sr.to_owned()]));
        let Some(rec) = self.record("sr", sr) else {
            return Err(("HANDLE_INVALID".into(), vec!["SR".into(), sr.into()]));
        };
        if rec.get("content_type").and_then(Value::as_str) == Some("iso") {
            return err("SR_OPERATION_NOT_SUPPORTED");
        }
        if rec.get("shared").and_then(Value::as_bool) != Some(true) {
            return err("SR_NOT_SHARED");
        }
        let pbds: Vec<&str> = rec
            .get("PBDs")
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if pbds.is_empty() {
            return err("SR_HAS_NO_PBDS");
        }
        let plugged = |h: &str| {
            pbds.iter().any(|p| {
                self.record("pbd", p).is_some_and(|r| {
                    r.get("host").and_then(Value::as_str) == Some(h)
                        && r.get("currently_attached").and_then(Value::as_bool) == Some(true)
                })
            })
        };
        if !self.refs("host").iter().all(|h| plugged(h)) {
            return err("SR_NO_PBDS");
        }
        Ok(())
    }

    fn assert_agile(&self, vm: &str) -> Result<(), (String, Vec<String>)> {
        let Some(rec) = self.record("vm", vm) else {
            return Err(("HANDLE_INVALID".into(), vec!["VM".into(), vm.into()]));
        };
        let list = |rec: &serde_json::Map<String, Value>, f: &str| -> Vec<String> {
            rec.get(f)
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        for vbd in list(rec, "VBDs") {
            let Some(vdi) = self
                .record("vbd", &vbd)
                .and_then(|b| b.get("VDI"))
                .and_then(Value::as_str)
                .filter(|v| *v != NULL_REF)
            else {
                continue;
            };
            let Some(sr) = self
                .record("vdi", vdi)
                .and_then(|d| d.get("SR"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let shared = self
                .record("sr", sr)
                .and_then(|s| s.get("shared"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !shared {
                return Err((
                    "HA_CONSTRAINT_VIOLATION_SR_NOT_SHARED".into(),
                    vec![sr.to_owned()],
                ));
            }
        }
        let hosts = self.refs("host");
        for vif in list(rec, "VIFs") {
            let Some(net) = self
                .record("vif", &vif)
                .and_then(|v| v.get("network"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let pif_hosts: Vec<String> = self
                .record("network", net)
                .map(|n| list(n, "PIFs"))
                .unwrap_or_default()
                .iter()
                .filter_map(|p| self.record("pif", p))
                .filter_map(|p| p.get("host").and_then(Value::as_str).map(str::to_owned))
                .collect();
            if !hosts.iter().all(|h| pif_hosts.contains(h)) {
                return Err((
                    "HA_CONSTRAINT_VIOLATION_NETWORK_NOT_SHARED".into(),
                    vec![net.to_owned()],
                ));
            }
        }
        Ok(())
    }

    /// `restart` VMs of a `config` map (VM → priority).
    fn restart_of(config: Option<&Value>) -> Vec<String> {
        config
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter(|(_, p)| protects(p.as_str().unwrap_or("")))
                    .map(|(v, _)| v.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The calls of [`handles`].
    pub(crate) fn ha_call(&mut self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i);
        let fail = |e: (String, Vec<String>)| -> Reply { Err(e) };
        Some(match (class, call) {
            ("pool", "ha_compute_hypothetical_max_host_failures_to_tolerate") => {
                let restart = Self::restart_of(params.get(1));
                Ok(json!(self.ha_max_failures(&restart, &[], None)))
            }
            ("pool", "ha_compute_max_host_failures_to_tolerate") => {
                let protected = self.protected_vms();
                Ok(json!(self.ha_max_failures(&protected, &[], None)))
            }
            ("pool", "sync_database") => Ok(json!("")),
            ("SR", "assert_can_host_ha_statefile") => self
                .assert_statefile_sr(p(1))
                .map_or_else(fail, |()| Ok(json!(""))),
            ("VM", "assert_agile") => self
                .assert_agile(p(1))
                .map_or_else(fail, |()| Ok(json!(""))),
            ("VM", "set_order" | "set_start_delay") => {
                let vm = p(1).to_owned();
                if !self.exists("vm", &vm) {
                    return Some(api_error("HANDLE_INVALID", &["VM", &vm]));
                }
                let value = int_param(params, 2).unwrap_or(0);
                let field = call.trim_start_matches("set_").to_owned();
                self.modify("vm", &vm, |rec| {
                    rec.insert(field, json!(value));
                });
                Ok(json!(""))
            }
            ("VM", "set_ha_restart_priority") => {
                let (vm, priority) = (p(1).to_owned(), p(2).to_owned());
                if !self.exists("vm", &vm) {
                    return Some(api_error("HANDLE_INVALID", &["VM", &vm]));
                }
                if !PRIORITIES.contains(&priority.as_str()) {
                    return Some(api_error(
                        "VALUE_NOT_SUPPORTED",
                        &["ha_restart_priority", &priority, "unknown restart priority"],
                    ));
                }
                if self.ha_on() && protects(&priority) {
                    if let Err(e) = self.assert_agile(&vm) {
                        return Some(Err(e));
                    }
                    let mut protected = self.protected_vms();
                    protected.push(vm.clone());
                    if self.ha_max_failures(&protected, &[], None) < self.ntol() {
                        return Some(self.would_break());
                    }
                }
                self.modify("vm", &vm, |rec| {
                    rec.insert("ha_restart_priority".into(), json!(priority));
                });
                self.ha_refresh_plan();
                Ok(json!(""))
            }
            ("pool", "set_ha_host_failures_to_tolerate") => {
                let n = int_param(params, 2).unwrap_or(-1);
                if n < 0 {
                    return Some(api_error(
                        "INVALID_VALUE",
                        &["ha_host_failures_to_tolerate", &n.to_string()],
                    ));
                }
                if self.ha_on() {
                    let protected = self.protected_vms();
                    if n > self.ha_max_failures(&protected, &[], None) {
                        return Some(self.would_break());
                    }
                }
                let pool = self.the_pool();
                self.modify("pool", &pool, |rec| {
                    rec.insert("ha_host_failures_to_tolerate".into(), json!(n));
                });
                self.ha_refresh_plan();
                Ok(json!(""))
            }
            ("pool", "enable_ha") => self.enable_ha(params),
            ("pool", "disable_ha") => self.disable_ha(),
            _ => api_error("MESSAGE_METHOD_UNKNOWN", &[call]),
        })
    }

    /// Recomputes `ha_plan_exists_for` and `ha_overcommitted` (HA on).
    fn ha_refresh_plan(&mut self) {
        if !self.ha_on() {
            return;
        }
        let protected = self.protected_vms();
        let max = self.ha_max_failures(&protected, &[], None);
        let ntol = self.ntol();
        let pool = self.the_pool();
        self.modify("pool", &pool, |rec| {
            rec.insert("ha_plan_exists_for".into(), json!(ntol.min(max)));
            rec.insert("ha_overcommitted".into(), json!(ntol > max));
        });
    }

    fn enable_ha(&mut self, params: &[Value]) -> Reply {
        if self.ha_on() {
            return api_error("HA_IS_ENABLED", &[]);
        }
        let srs: Vec<String> = params
            .get(1)
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let Some(sr) = srs.first().cloned() else {
            return api_error("HA_HEARTBEAT_DISK_NOT_SPECIFIED", &[]);
        };
        for s in &srs {
            self.assert_statefile_sr(s)?;
        }
        let statefile = self.new_vdi(&sr, "Statefile for HA", 4 << 20, true, false);
        let metadata = self.new_vdi(&sr, "Metadata for HA", 256 << 20, true, false);
        self.modify("vdi", &statefile, |rec| {
            rec.insert("type".into(), json!("ha_statefile"));
        });
        self.modify("vdi", &metadata, |rec| {
            rec.insert("type".into(), json!("metadata"));
        });
        let hosts = self.refs("host");
        let peers: Vec<String> = hosts
            .iter()
            .filter(|h| self.host_live(h))
            .filter_map(|h| {
                self.record("host", h)
                    .and_then(|r| r.get("uuid"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        for h in hosts {
            let live = self.host_live(&h);
            let (statefile, peers) = (statefile.clone(), peers.clone());
            self.modify("host", &h, |rec| {
                rec.insert(
                    "ha_statefiles".into(),
                    json!(if live { vec![statefile] } else { vec![] }),
                );
                rec.insert(
                    "ha_network_peers".into(),
                    json!(if live { peers } else { vec![] }),
                );
            });
        }
        let pool = self.the_pool();
        let config = params.get(2).cloned().unwrap_or(json!({}));
        self.modify("pool", &pool, |rec| {
            rec.insert("ha_enabled".into(), json!(true));
            rec.insert("ha_statefiles".into(), json!([statefile]));
            rec.insert("ha_configuration".into(), config);
        });
        self.ha_refresh_plan();
        Ok(json!(""))
    }

    fn disable_ha(&mut self) -> Reply {
        if !self.ha_on() {
            return api_error("HA_NOT_ENABLED", &[]);
        }
        let pool = self.the_pool();
        let vdis: Vec<String> = self
            .refs("vdi")
            .into_iter()
            .filter(|v| {
                self.record("vdi", v).is_some_and(|r| {
                    matches!(
                        r.get("type").and_then(Value::as_str),
                        Some("ha_statefile" | "metadata")
                    )
                })
            })
            .collect();
        for v in vdis {
            let sr = self
                .record("vdi", &v)
                .and_then(|r| r.get("SR"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(sr) = sr {
                self.drop_ref("sr", &sr, "VDIs", &v);
            }
            self.remove("vdi", &v);
        }
        for h in self.refs("host") {
            self.modify("host", &h, |rec| {
                rec.insert("ha_statefiles".into(), json!([]));
                rec.insert("ha_network_peers".into(), json!([]));
            });
        }
        self.modify("pool", &pool, |rec| {
            rec.insert("ha_enabled".into(), json!(false));
            rec.insert("ha_statefiles".into(), json!([]));
            rec.insert("ha_plan_exists_for".into(), json!(0));
            rec.insert("ha_overcommitted".into(), json!(false));
            rec.insert("ha_configuration".into(), json!({}));
        });
        Ok(json!(""))
    }
}
