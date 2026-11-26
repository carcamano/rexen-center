//! High availability: the rules of the original's HA wizard, its edit
//! dialog, the pool's HA tab and the HA checks other operations make.
//!
//! * Restart priorities ([`RestartPriority`], `VM.HaRestartPriority`): on
//!   XCP-ng 8.x a VM is restarted, restarted if possible ("best effort") or
//!   not restarted; the two older "always restart" values are still read.
//! * The number of server failures to tolerate ("ntol"): the server
//!   computes the most it can plan for
//!   (`pool.ha_compute_hypothetical_max_host_failures_to_tolerate`, given
//!   the VMs to restart); [`tolerable`] applies the pool's
//!   `hci-limit-fault-tolerance` like `Pool.GetMaximumTolerableHostFailures`.
//! * Operations that take a server away (maintenance mode, reboot, shut
//!   down) first lower the pool's ntol to one less than what the remaining
//!   servers can take ([`ntol_before_host_operation`]), after asking; a VM
//!   start refused with `HA_OPERATION_WOULD_BREAK_FAILOVER_PLAN` offers to
//!   lower it too ([`ntol_for_vm_start`]).

use std::collections::HashMap;

use xapi::{Host, HostRecord, PoolAllowedOperations, PoolRecord, Ref, Sr, Vdi, Vm, VmRecord};

use crate::model::ConnectionModel;
use crate::objects::{self, VmKind};
use crate::pool::{self, Restriction};

/// `Pool.FAULT_TOLERANCE_LIMIT_FOR_HCI`.
pub const HCI_LIMIT_KEY: &str = "hci-limit-fault-tolerance";

/// A VM's restart priority (`VM.HaRestartPriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RestartPriority {
    /// `"0"`: only on servers before XenServer 6.0.
    AlwaysRestartHighPriority,
    /// `"1"`: only on servers before XenServer 6.0.
    AlwaysRestart,
    /// `"restart"`.
    Restart,
    /// `"best-effort"`: "Restart if possible".
    BestEffort,
    /// `""`: "Do not restart".
    DoNotRestart,
}

impl RestartPriority {
    /// The priorities offered (`VM.GetAvailableRestartPriorities`).
    pub const AVAILABLE: [Self; 3] = [Self::Restart, Self::BestEffort, Self::DoNotRestart];

    /// `VM.StringToPriority`: an unknown value counts as "always restart".
    pub fn from_api(value: &str) -> Self {
        match value {
            "0" => Self::AlwaysRestartHighPriority,
            "restart" => Self::Restart,
            "" => Self::DoNotRestart,
            "best-effort" => Self::BestEffort,
            _ => Self::AlwaysRestart,
        }
    }

    /// `VM.PriorityToString`.
    pub fn to_api(self) -> &'static str {
        match self {
            Self::AlwaysRestartHighPriority => "0",
            Self::AlwaysRestart => "1",
            Self::Restart => "restart",
            Self::BestEffort => "best-effort",
            Self::DoNotRestart => "",
        }
    }

    /// `VM.HaPriorityIsRestart`.
    pub fn is_restart(self) -> bool {
        self == Self::Restart
    }

    /// The VM is protected (any priority but "do not restart").
    pub fn protects(self) -> bool {
        self != Self::DoNotRestart
    }
}

/// The startup options of one VM (`VMStartupOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VmStartup {
    pub vm: Ref<Vm>,
    pub priority: RestartPriority,
    /// Start order (`VM.order`).
    pub order: i64,
    /// Seconds to wait before starting the next VM (`VM.start_delay`).
    pub start_delay: i64,
}

/// A VM's restart priority.
pub fn priority(vm: &VmRecord) -> RestartPriority {
    RestartPriority::from_api(&vm.ha_restart_priority)
}

/// `VM.HaCanProtect`: a real VM (not a template, snapshot or control
/// domain) that is shown.
pub fn can_protect(vm: &VmRecord, show_hidden: bool) -> bool {
    objects::vm_kind(vm) == VmKind::Guest && objects::vm_is_shown(vm, show_hidden)
}

/// The VMs the HA pages list, in the cache's order by name.
pub fn protectable_vms(model: &ConnectionModel, show_hidden: bool) -> Vec<(&Ref<Vm>, &VmRecord)> {
    let mut vms: Vec<(&Ref<Vm>, &VmRecord)> = model
        .vms
        .iter()
        .map(|(r, v)| (r, &**v))
        .filter(|(_, v)| can_protect(v, show_hidden))
        .collect();
    vms.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label).then(a.0.cmp(b.0)));
    vms
}

/// `AssignPriorities.IsHaActivatedFirstTime`: when no VM has a priority
/// yet, the wizard proposes one per VM once it knows whether it is agile.
pub fn first_time(vms: &[(&Ref<Vm>, &VmRecord)]) -> bool {
    vms.iter().all(|(_, v)| v.ha_restart_priority.is_empty())
}

/// The priority proposed for a VM the first time
/// (`VM.HaHighestProtectionAvailable` when agile, else best effort).
pub fn default_priority(agile: bool) -> RestartPriority {
    if agile {
        RestartPriority::Restart
    } else {
        RestartPriority::BestEffort
    }
}

/// What the wizard does to a VM's priority once its agility is known: a
/// non-agile VM set to restart drops to best effort.
pub fn priority_after_agility(
    current: Option<RestartPriority>,
    agile: bool,
) -> Option<RestartPriority> {
    match current {
        None => Some(default_priority(agile)),
        Some(p) if !agile && p != RestartPriority::BestEffort && p.protects() => {
            Some(RestartPriority::BestEffort)
        }
        other => other,
    }
}

/// `Helpers.GetVmHaRestartPrioritiesForApi`: the VMs the server's planner
/// is told about (best-effort and do-not-restart VMs are left out).
pub fn priorities_for_api<'a>(
    settings: impl IntoIterator<Item = (&'a Ref<Vm>, RestartPriority)>,
) -> HashMap<Ref<Vm>, String> {
    settings
        .into_iter()
        .filter(|(_, p)| {
            !matches!(
                p,
                RestartPriority::BestEffort | RestartPriority::DoNotRestart
            )
        })
        .map(|(v, p)| (v.clone(), p.to_api().to_owned()))
        .collect()
}

/// The current priorities of the pool's VMs, for the planner
/// (`Helpers.GetVmHaRestartPriorities` with hidden VMs).
pub fn current_priorities(model: &ConnectionModel) -> HashMap<Ref<Vm>, String> {
    priorities_for_api(
        protectable_vms(model, true)
            .into_iter()
            .map(|(r, v)| (r, priority(v))),
    )
}

/// `Pool.GetMaximumTolerableHostFailures` once the server answered `max`:
/// the pool's HCI limit wins when it is set and not above `max`.
pub fn tolerable(pool: Option<&PoolRecord>, max: i64) -> i64 {
    pool.and_then(|p| p.other_config.get(HCI_LIMIT_KEY))
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|limit| *limit <= max)
        .unwrap_or(max)
}

/// `HostAbstractAction.MaybeReduceNtolBeforeOp`: the ntol to set before a
/// server leaves, if the current one is too high. `max` is what the planner
/// allows with the server still there.
pub fn ntol_before_host_operation(current: i64, max: i64) -> Option<i64> {
    let target = (max - 1).max(0);
    (current > target).then_some(target)
}

/// `VMOperationCommand.StartDiagnosisForm`: the ntol to offer when a VM
/// start broke the failover plan (`<= 0`: HA would have to be turned off).
pub fn ntol_for_vm_start(pool: &PoolRecord) -> i64 {
    (pool.ha_plan_exists_for - 1).min(pool.ha_host_failures_to_tolerate - 1)
}

/// Shared SRs a statefile could go on (`GetHeartbeatSRsAction`: shared and
/// not the tools SR), sorted by name.
pub fn heartbeat_candidates(model: &ConnectionModel) -> Vec<Ref<Sr>> {
    let mut srs: Vec<(&Ref<Sr>, &str)> = model
        .srs
        .iter()
        .filter(|(_, s)| s.shared && !objects::is_tools_sr(s))
        .map(|(r, s)| (r, s.name_label.as_str()))
        .collect();
    srs.sort_by(|a, b| objects::natural_cmp(a.1, b.1).then(a.0.cmp(b.0)));
    srs.into_iter().map(|(r, _)| r.clone()).collect()
}

/// `HAWizard.OnShown`: the broken shared SRs the wizard warns about.
pub fn broken_shared_srs(model: &ConnectionModel) -> Vec<String> {
    let mut names: Vec<String> = model
        .srs
        .values()
        .filter(|s| {
            !s.pbds.is_empty()
                && objects::sr_is_broken(model, s)
                && !objects::is_tools_sr(s)
                && s.shared
        })
        .map(|s| s.name_label.clone())
        .collect();
    names.sort_by(|a, b| objects::natural_cmp(a, b));
    names
}

/// Why the High Availability commands cannot run
/// (`HACommand.GetCantRunReasonCore`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaProblem {
    /// "a standalone server" (`HA_STANDALONE_SERVER`).
    Standalone,
    /// The coordinator is not known.
    NoCoordinator,
    /// HA is being turned on or off (by this client or another).
    Busy { enabling: bool },
    /// A pool secret rotation is pending (`ROTATE_POOL_SECRET_PENDING_HA`).
    SecretRotationPending,
}

/// Whether HA is being turned on or off (`HelpersGUI.FindActiveHaAction`,
/// and the pool's current operations for other clients).
pub fn busy(pool: &PoolRecord) -> Option<bool> {
    let ops = pool.current_operations.values();
    let mut found = None;
    for op in ops {
        match op {
            PoolAllowedOperations::HaEnable => found = Some(true),
            PoolAllowedOperations::HaDisable => found = found.or(Some(false)),
            _ => {}
        }
    }
    found
}

/// `HACommand.CanRunHACommand` (and its reason): HA can be configured on
/// a real pool with a coordinator, when HA is not being turned on or off.
/// `busy` is whether this client runs an HA operation on it
/// (`Some(true)`: enabling).
pub fn command_problem(model: &ConnectionModel, client_busy: Option<bool>) -> Option<HaProblem> {
    let (_, pool) = model.pool()?;
    if !objects::pool_is_visible(model) {
        return Some(HaProblem::Standalone);
    }
    if model.coordinator().is_none() {
        return Some(HaProblem::NoCoordinator);
    }
    if let Some(enabling) = client_busy.or_else(|| busy(pool)) {
        return Some(HaProblem::Busy { enabling });
    }
    None
}

/// `HAConfigureCommand.CanRun`: also refused while a pool secret
/// rotation is pending (unless the licence restricts rotation).
pub fn configure_problem(model: &ConnectionModel, client_busy: Option<bool>) -> Option<HaProblem> {
    if let Some(p) = command_problem(model, client_busy) {
        return Some(p);
    }
    let (_, pool) = model.pool()?;
    (!pool::feature_forbidden(model, Restriction::PoolSecretRotation) && pool.is_psr_pending)
        .then_some(HaProblem::SecretRotationPending)
}

/// `HADisableCommand.CanRun`.
pub fn disable_problem(model: &ConnectionModel, client_busy: Option<bool>) -> Option<HaProblem> {
    command_problem(model, client_busy)
}

/// `pool.ha_statefiles` cannot be resolved (the commands then show
/// `HA_CONFIGURE_NO_STATEFILE` / `HA_DISABLE_NO_STATEFILE`): true also for
/// none, as in the original.
pub fn statefiles_missing(model: &ConnectionModel, pool: &PoolRecord) -> bool {
    pool.ha_statefiles
        .iter()
        .all(|sf| !model.vdis.contains_key(&Ref::<Vdi>::new(sf.as_str())))
}

/// Live servers missing from the pool (`EditVmHaPrioritiesDialog` and the
/// VM page refuse to edit then).
pub fn dead_hosts(model: &ConnectionModel) -> Vec<String> {
    objects::sorted_hosts(model)
        .into_iter()
        .filter(|(_, h)| !objects::host_is_live(model, h))
        .map(|(_, h)| h.name_label.clone())
        .collect()
}

/// The HA wizard's finish page: VMs per priority
/// (`HAWizard.UpdateWizardContent`): always restart (high priority),
/// restart, best effort, do not restart.
pub fn priority_counts(priorities: impl IntoIterator<Item = RestartPriority>) -> [usize; 4] {
    let mut c = [0; 4];
    for p in priorities {
        match p {
            RestartPriority::AlwaysRestartHighPriority => c[0] += 1,
            RestartPriority::AlwaysRestart | RestartPriority::Restart => c[1] += 1,
            RestartPriority::BestEffort => c[2] += 1,
            RestartPriority::DoNotRestart => c[3] += 1,
        }
    }
    c
}

/// The startup options that changed (`AssignPriorities.GetChangedStartupOptions`):
/// a row with no priority yet counts as best effort.
pub fn changed_startup(
    model: &ConnectionModel,
    rows: impl IntoIterator<Item = (Ref<Vm>, Option<RestartPriority>, i64, i64)>,
) -> Vec<VmStartup> {
    rows.into_iter()
        .filter(|(vm, p, order, delay)| {
            model.vms.get(vm).is_none_or(|v| {
                v.order != *order || v.start_delay != *delay || Some(priority(v)) != *p
            })
        })
        .map(|(vm, p, order, start_delay)| VmStartup {
            vm,
            priority: p.unwrap_or(RestartPriority::BestEffort),
            order,
            start_delay,
        })
        .collect()
}

/// How one server sees one heartbeat target, on the HA tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heartbeat {
    /// "Healthy".
    Healthy,
    /// "Unhealthy" / "Not healthy".
    Unhealthy,
    /// "{n} of {m} servers".
    Some { seen: usize, of: usize },
    /// "Initializing..." during the first 30 seconds.
    Initializing,
}

/// One row of the HA tab's heartbeating table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatRow {
    pub host: Ref<Host>,
    pub name: String,
    pub network: Heartbeat,
    /// One per heartbeat SR, in [`heartbeat_srs`] order.
    pub srs: Vec<Heartbeat>,
}

/// `Pool.GetHAHeartbeatSRs`: the SRs of the pool's statefiles, by name.
pub fn heartbeat_srs(model: &ConnectionModel, pool: &PoolRecord) -> Vec<(Ref<Sr>, String)> {
    let mut srs: Vec<(Ref<Sr>, String)> = Vec::new();
    for sf in &pool.ha_statefiles {
        let Some(vdi) = model.vdis.get(&Ref::<Vdi>::new(sf.as_str())) else {
            continue;
        };
        if let Some(sr) = model.srs.get(&vdi.sr)
            && !srs.iter().any(|(r, _)| r == &vdi.sr)
        {
            srs.push((vdi.sr.clone(), sr.name_label.clone()));
        }
    }
    srs.sort_by(|a, b| objects::natural_cmp(&a.1, &b.1));
    srs
}

/// `HAPage.generatePoolHABox`'s heartbeating table. `initialized` is false
/// during the 30 seconds after HA was seen turned on.
pub fn heartbeat_rows(
    model: &ConnectionModel,
    pool: &PoolRecord,
    initialized: bool,
) -> Vec<HeartbeatRow> {
    let srs = heartbeat_srs(model, pool);
    let mut hosts: Vec<(&Ref<Host>, &HostRecord)> =
        model.hosts.iter().map(|(r, h)| (r, &**h)).collect();
    hosts.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label));
    let members = hosts.len();
    hosts
        .into_iter()
        .map(|(r, h)| {
            let peers = h.ha_network_peers.len();
            let network = if !initialized {
                Heartbeat::Initializing
            } else if peers == 0 {
                Heartbeat::Unhealthy
            } else if peers == members {
                Heartbeat::Healthy
            } else {
                Heartbeat::Some {
                    seen: peers,
                    of: members,
                }
            };
            let srs = srs
                .iter()
                .map(|(sr, _)| {
                    let ok = h.ha_statefiles.iter().any(|sf| {
                        model
                            .vdis
                            .get(&Ref::<Vdi>::new(sf.as_str()))
                            .is_some_and(|v| &v.sr == sr)
                    });
                    if ok {
                        Heartbeat::Healthy
                    } else {
                        Heartbeat::Unhealthy
                    }
                })
                .collect();
            HeartbeatRow {
                host: r.clone(),
                name: h.name_label.clone(),
                network,
                srs,
            }
        })
        .collect()
}

/// `HAPage.haStatusRed`: the pool cannot keep its configured promise.
pub fn overcommitted(pool: &PoolRecord) -> bool {
    pool.ha_host_failures_to_tolerate > pool.ha_plan_exists_for
}

/// The ntol control's state once the planner answered
/// (`HaNtolIndicator.LoadCalculationSucceededMode`): the value shown is the
/// pool's ntol when HA is on, else the maximum.
pub fn initial_ntol(pool: &PoolRecord, max: i64) -> i64 {
    if pool.ha_enabled {
        pool.ha_host_failures_to_tolerate
    } else {
        max
    }
}

/// RBAC calls the HA wizard needs (`HAWizard`'s blocking RBAC page).
pub const WIZARD_RBAC: &[&str] = &[
    "vm.set_ha_restart_priority",
    "vm.set_order",
    "vm.set_start_delay",
    "pool.sync_database",
    "pool.ha_compute_hypothetical_max_host_failures_to_tolerate",
    "pool.set_ha_host_failures_to_tolerate",
    "pool.enable_ha",
    "sr.assert_can_host_ha_statefile",
];

/// RBAC calls editing HA needs (`HAConfigureCommand.HA_PERMISSION_CHECKS`).
pub const CONFIGURE_RBAC: &[&str] = &[
    "pool.set_ha_host_failures_to_tolerate",
    "pool.sync_database",
    "vm.set_ha_restart_priority",
    "pool.ha_compute_hypothetical_max_host_failures_to_tolerate",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn priorities_round_trip_like_the_original() {
        for p in [
            RestartPriority::AlwaysRestartHighPriority,
            RestartPriority::AlwaysRestart,
            RestartPriority::Restart,
            RestartPriority::BestEffort,
            RestartPriority::DoNotRestart,
        ] {
            assert_eq!(RestartPriority::from_api(p.to_api()), p);
        }
        assert_eq!(
            RestartPriority::from_api("bogus"),
            RestartPriority::AlwaysRestart
        );
        assert!(RestartPriority::Restart.is_restart());
        assert!(!RestartPriority::AlwaysRestart.is_restart());
        let (a, b, c) = (Ref::new("a"), Ref::new("b"), Ref::new("c"));
        let api = priorities_for_api([
            (&a, RestartPriority::Restart),
            (&b, RestartPriority::BestEffort),
            (&c, RestartPriority::DoNotRestart),
        ]);
        assert_eq!(api, HashMap::from([(a, "restart".to_owned())]));
    }

    #[test]
    fn ntol_rules() {
        let pool = |other: serde_json::Value, ntol: i64, plan: i64| -> PoolRecord {
            serde_json::from_value(json!({"uuid": "p", "other_config": other,
                "ha_host_failures_to_tolerate": ntol, "ha_plan_exists_for": plan}))
            .unwrap()
        };
        assert_eq!(tolerable(None, 2), 2);
        assert_eq!(
            tolerable(Some(&pool(json!({HCI_LIMIT_KEY: "1"}), 0, 0)), 2),
            1
        );
        // A limit above what the server can do is ignored.
        assert_eq!(
            tolerable(Some(&pool(json!({HCI_LIMIT_KEY: "3"}), 0, 0)), 2),
            2
        );
        assert_eq!(
            tolerable(Some(&pool(json!({HCI_LIMIT_KEY: "x"}), 0, 0)), 2),
            2
        );
        // Before a server leaves: one less than the maximum, never below 0.
        assert_eq!(ntol_before_host_operation(2, 2), Some(1));
        assert_eq!(ntol_before_host_operation(1, 2), None);
        assert_eq!(ntol_before_host_operation(1, 0), Some(0));
        assert_eq!(ntol_before_host_operation(0, 0), None);
        assert_eq!(ntol_for_vm_start(&pool(json!({}), 2, 2)), 1);
        assert_eq!(ntol_for_vm_start(&pool(json!({}), 1, 1)), 0);
        assert!(overcommitted(&pool(json!({}), 2, 1)));
        assert_eq!(initial_ntol(&pool(json!({}), 1, 1), 2), 2);
        // Agility: unknown → proposed; a non-agile restart drops.
        assert_eq!(
            priority_after_agility(None, true),
            Some(RestartPriority::Restart)
        );
        assert_eq!(
            priority_after_agility(None, false),
            Some(RestartPriority::BestEffort)
        );
        assert_eq!(
            priority_after_agility(Some(RestartPriority::Restart), false),
            Some(RestartPriority::BestEffort)
        );
        assert_eq!(
            priority_after_agility(Some(RestartPriority::DoNotRestart), false),
            Some(RestartPriority::DoNotRestart)
        );
        assert_eq!(
            priority_counts([
                RestartPriority::Restart,
                RestartPriority::AlwaysRestart,
                RestartPriority::BestEffort,
                RestartPriority::DoNotRestart
            ]),
            [0, 2, 1, 1]
        );
    }
}
