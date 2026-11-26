//! Scheduled snapshots (`VMSS`, F12b): the policies, their schedules and
//! run history, and the requests the window ends with. How they run is
//! [`crate::vmss_run`]'s.
//!
//! As the original:
//!
//! * **VM Snapshot Schedules** (`ScheduledSnapshotsDialog`, Pool menu):
//!   the pool's policies with their status, number of VMs, next run (this
//!   console's time and the server's) and last result; New, Properties,
//!   Enable / Disable, Run Now, Delete, and the run history of the
//!   selected policy (its `VMSS` messages).
//! * **New Snapshot Schedule** (`NewPolicyWizard`): Name, Virtual
//!   Machines, Snapshot Type, Snapshot Schedule (frequency, time and
//!   retention) and Finish (run it now). The policy is made enabled when
//!   it has VMs (`CreateVMPolicy`).
//! * **Properties** of a policy: General, Virtual Machines, Snapshot Type
//!   and Snapshot Schedule.
//! * **Assign to Snapshot Schedule** on VMs (`AssignGroupToolStripMenuItem
//!   <VMSS>`), and the VM's Snapshots tab line.
//!
//! **Times.** The console shows times in UTC, so "local" times are UTC
//! here. The schedule is stored in the server's local time: the time
//! chosen is turned into the server's (`ServerClientTimeZoneDiff`, from
//! `host.get_servertime` and `host.get_server_localtime` on the
//! coordinator) and rounded to the nearest quarter of an hour, as the
//! original does. Times are Unix-like second counts; a "local" one counts
//! seconds of that clock's wall time.

use std::collections::HashMap;
use std::sync::Arc;

use xapi::{
    Message, MessageRecord, Ref, Vm, VmOperations, VmRecord, Vmss, VmssFrequency, VmssRecord,
    VmssType,
};

use crate::model::ConnectionModel;
use crate::objects;
use crate::vapp::ChangeWarning;

/// The policies of a pool, sorted by name.
pub fn policies(model: &ConnectionModel) -> Vec<(&Ref<Vmss>, &Arc<VmssRecord>)> {
    let mut out: Vec<_> = model.vmss.iter().collect();
    out.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label).then(a.0.cmp(b.0)));
    out
}

/// The policy a VM is in.
pub fn policy_of<'a>(
    model: &'a ConnectionModel,
    vm: &VmRecord,
) -> Option<(&'a Ref<Vmss>, &'a Arc<VmssRecord>)> {
    model.vmss.get_key_value(&vm.snapshot_schedule)
}

/// `PolicyVmCount`: the policy's real VMs.
pub fn vm_count(model: &ConnectionModel, policy: &VmssRecord) -> usize {
    policy
        .vms
        .iter()
        .filter(|v| {
            model
                .vms
                .get(*v)
                .is_some_and(|vm| crate::vapp::is_real_vm(vm))
        })
        .count()
}

/// `RefreshPoolTitle`: (VMs in a schedule, real VMs shown).
pub fn protected_counts(model: &ConnectionModel, show_hidden: bool) -> (usize, usize) {
    let mut protected = 0;
    let mut real = 0;
    for vm in model.vms.values() {
        if crate::vapp::is_real_vm(vm) && objects::vm_is_shown(vm, show_hidden) {
            real += 1;
            if model.vmss.contains_key(&vm.snapshot_schedule) {
                protected += 1;
            }
        }
    }
    (protected, real)
}

/// The VMs of a policy, sorted as the console sorts VMs.
pub fn members<'a>(
    model: &'a ConnectionModel,
    policy: &VmssRecord,
) -> Vec<(&'a Ref<Vm>, &'a Arc<VmRecord>)> {
    let mut out: Vec<_> = policy
        .vms
        .iter()
        .filter_map(|r| model.vms.get_key_value(r))
        .collect();
    out.sort_by(|a, b| objects::vm_cmp((a.0, a.1), (b.0, b.1)));
    out
}

/// `ChangesOK` for snapshot schedules: the VMs already in another policy
/// than `group` (`None`: a new one named `name`).
pub fn change_warning(
    model: &ConnectionModel,
    vms: &[Ref<Vm>],
    group: Option<&Ref<Vmss>>,
    name: &str,
) -> Option<ChangeWarning> {
    let moving: Vec<(&VmRecord, &VmssRecord)> = vms
        .iter()
        .filter_map(|r| model.vms.get(r))
        .filter_map(|vm| {
            let old = model.vmss.get(&vm.snapshot_schedule)?;
            (group != Some(&vm.snapshot_schedule)).then_some((&**vm, &**old))
        })
        .collect();
    match moving.as_slice() {
        [] => None,
        [(vm, old)] => Some(ChangeWarning::One {
            vm: vm.name_label.clone(),
            old: old.name_label.clone(),
            new: name.to_owned(),
        }),
        _ => Some(ChangeWarning::Many {
            new: name.to_owned(),
        }),
    }
}

/// The Enable / Disable button's state (`RefreshButtons`): enabled when
/// the policy has VMs or is enabled.
pub fn can_toggle(policy: &VmssRecord) -> bool {
    !policy.vms.is_empty() || policy.enabled
}

/// Run Now is offered for an enabled policy with VMs.
pub fn can_run_now(policy: &VmssRecord) -> bool {
    policy.enabled && !policy.vms.is_empty()
}

/// Whether the quiesce option applies to `vms`
/// (`ToggleQuiesceCheckBox`): each must allow `snapshot_with_quiesce`.
pub fn quiesce_possible(model: &ConnectionModel, vms: &[Ref<Vm>]) -> bool {
    vms.iter().all(|v| {
        model.vms.get(v).is_some_and(|vm| {
            vm.allowed_operations
                .contains(&VmOperations::SnapshotWithQuiesce)
        })
    })
}

// ---- time ---------------------------------------------------------------------------------

/// The server's clock as `GetServerLocalTimeAction` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerTime {
    /// The server's local wall time now.
    pub server_local: i64,
    /// `ServerClientTimeZoneDiff`: this console's time minus the server's
    /// local time (clock skew removed), in seconds.
    pub diff: i64,
}

impl ServerTime {
    /// From `host.get_servertime` (UTC) and `host.get_server_localtime`.
    pub fn new(server_utc: i64, server_local: i64) -> Self {
        Self {
            server_local,
            diff: server_utc - server_local,
        }
    }

    /// A time of this console as the server's local time.
    pub fn to_server(&self, client: i64) -> i64 {
        client - self.diff
    }

    /// A server local time as this console's time.
    pub fn to_client(&self, server: i64) -> i64 {
        server + self.diff
    }
}

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

/// Day of the week of `t` (0 = Sunday).
pub fn weekday(t: i64) -> u8 {
    (t.div_euclid(DAY) + 4).rem_euclid(7) as u8
}

/// `HelpersGUI.RoundToNearestQuarter`.
pub fn round_to_nearest_quarter(t: i64) -> i64 {
    let base = t - t.rem_euclid(HOUR);
    let into = t - base;
    let quarter = 15 * 60;
    let steps = (into * 2 + quarter) / (2 * quarter);
    base + steps * quarter
}

/// `VMSS.GetHourlyDate`.
pub fn hourly_date(now: i64, min: i64) -> i64 {
    let next = now - now.rem_euclid(HOUR) + min * 60;
    if now > next { next + HOUR } else { next }
}

/// `VMSS.GetDailyDate`.
pub fn daily_date(now: i64, min: i64, hour: i64) -> i64 {
    let next = now - now.rem_euclid(DAY) + hour * HOUR + min * 60;
    if now > next { next + DAY } else { next }
}

/// `VMSS.GetWeeklyDates`: the next run on each day, soonest first.
pub fn weekly_dates(now: i64, min: i64, hour: i64, days: &[u8]) -> Vec<i64> {
    let next = now - now.rem_euclid(DAY) + hour * HOUR + min * 60;
    let today = i64::from(weekday(next));
    let mut runs: Vec<i64> = days
        .iter()
        .map(|d| {
            let d = i64::from(*d);
            if today < d {
                next + (d - today) * DAY
            } else if d < today {
                next + (7 - (today - d)) * DAY
            } else if now < next {
                next
            } else {
                next + 7 * DAY
            }
        })
        .collect();
    runs.sort_unstable();
    runs
}

/// English day names XAPI's schedules use, Sunday first.
pub const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `VMSS.BackUpScheduleDays`: the days of a schedule (0 = Sunday), in the
/// order given.
pub fn schedule_days(schedule: &HashMap<String, String>) -> Vec<u8> {
    let Some(days) = schedule.get("days") else {
        return Vec::new();
    };
    days.split([',', ' '])
        .filter(|d| !d.is_empty())
        .filter_map(|d| {
            DAY_NAMES
                .iter()
                .position(|n| n.eq_ignore_ascii_case(d))
                .map(|i| i as u8)
        })
        .collect()
}

fn schedule_num(schedule: &HashMap<String, String>, key: &str) -> Option<i64> {
    schedule.get(key).and_then(|v| v.trim().parse().ok())
}

/// `VMSS.GetNextRunTime` in server local time.
pub fn next_run(policy: &VmssRecord, server_local: i64) -> Option<i64> {
    let min = schedule_num(&policy.schedule, "min")?;
    if policy.frequency == VmssFrequency::Hourly {
        return Some(hourly_date(server_local, min));
    }
    let hour = schedule_num(&policy.schedule, "hour")?;
    match &policy.frequency {
        VmssFrequency::Daily => Some(daily_date(server_local, min, hour)),
        VmssFrequency::Weekly => {
            weekly_dates(server_local, min, hour, &schedule_days(&policy.schedule))
                .first()
                .copied()
        }
        _ => None,
    }
}

/// A policy's next run: (this console's time, the server's), or `None`
/// without the server's time (`VMSS_HOST_NOT_LIVE`).
pub fn next_runs(policy: &VmssRecord, time: Option<ServerTime>) -> Option<(i64, i64)> {
    let time = time?;
    let server = next_run(policy, time.server_local)?;
    Some((round_to_nearest_quarter(time.to_client(server)), server))
}

// ---- the Snapshot Schedule page -------------------------------------------------------------

/// What the Snapshot Schedule page shows (`NewPolicySnapshotFrequencyPage`):
/// times are this console's (UTC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleChoice {
    pub frequency: VmssFrequency,
    /// Hourly: minutes past the hour (0, 15, 30 or 45).
    pub hourly_min: i64,
    /// Daily: (hour, minute).
    pub daily: (i64, i64),
    /// Weekly: (hour, minute) and the days (0 = Sunday).
    pub weekly: (i64, i64),
    pub days: Vec<u8>,
    /// Maximum number of scheduled snapshots (1–10).
    pub retention: i64,
}

impl Default for ScheduleChoice {
    /// The page's start: daily, 7 snapshots, 15 past the hour, Monday.
    fn default() -> Self {
        Self {
            frequency: VmssFrequency::Daily,
            hourly_min: 15,
            daily: (0, 0),
            weekly: (0, 0),
            days: vec![1],
            retention: 7,
        }
    }
}

/// `SetHourlyMinutes`: the quarter a minute falls in.
pub fn quarter_of(min: i64) -> i64 {
    (min.clamp(0, 59) / 15) * 15
}

/// The schedule the page makes, with the run times it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedSchedule {
    /// What is saved (server local time); empty without the server's time.
    pub schedule: HashMap<String, String>,
    /// The next run: (this console's, the server's).
    pub next_client: Option<i64>,
    pub next_server: Option<i64>,
}

impl ScheduleChoice {
    /// Choosing a frequency resets the retention to its default (hourly
    /// 10, daily 7, weekly 4), as the radio buttons do.
    pub fn set_frequency(&mut self, frequency: VmssFrequency) {
        if self.frequency != frequency {
            self.retention = match frequency {
                VmssFrequency::Hourly => 10,
                VmssFrequency::Weekly => 4,
                _ => 7,
            };
            self.frequency = frequency;
        }
    }

    /// `PopulateTab`: the page for an existing policy, its server times
    /// turned into this console's.
    pub fn from_policy(policy: &VmssRecord, time: ServerTime) -> Self {
        let mut out = Self {
            frequency: policy.frequency.clone(),
            retention: policy.retained_snapshots.clamp(1, 10),
            ..Self::default()
        };
        let Some(server) = next_run(policy, time.server_local) else {
            return out;
        };
        let client = round_to_nearest_quarter(time.to_client(server));
        let t = client.rem_euclid(DAY);
        let (hour, min) = (t / HOUR, (t % HOUR) / 60);
        match &policy.frequency {
            VmssFrequency::Hourly => out.hourly_min = quarter_of(min),
            VmssFrequency::Daily => out.daily = (hour, min),
            VmssFrequency::Weekly => {
                out.weekly = (hour, min);
                let days = schedule_days(&policy.schedule);
                if !days.is_empty() {
                    out.days = days;
                }
            }
            _ => {}
        }
        out
    }

    /// `EnableNext` / `ValidToSave`.
    pub fn valid(&self) -> bool {
        self.frequency != VmssFrequency::Weekly || !self.days.is_empty()
    }

    /// `RecalculateSchedule` at this console's time `now`.
    pub fn compute(&self, now: i64, time: Option<ServerTime>) -> ComputedSchedule {
        let mut schedule = HashMap::new();
        let mut next_client = None;
        let mut next_server = None;
        let to_server = |c: i64| time.map(|t| round_to_nearest_quarter(t.to_server(c)));
        let split = |t: i64| {
            let s = t.rem_euclid(DAY);
            ((s / HOUR).to_string(), ((s % HOUR) / 60).to_string())
        };
        match self.frequency {
            VmssFrequency::Hourly => {
                let c = hourly_date(now, self.hourly_min);
                next_client = Some(c);
                if let Some(s) = to_server(c) {
                    schedule.insert("min".to_owned(), split(s).1);
                    next_server = Some(s);
                }
            }
            VmssFrequency::Daily => {
                let c = daily_date(now, self.daily.1, self.daily.0);
                next_client = Some(c);
                if let Some(s) = to_server(c) {
                    let (h, m) = split(s);
                    schedule.insert("hour".to_owned(), h);
                    schedule.insert("min".to_owned(), m);
                    next_server = Some(s);
                }
            }
            VmssFrequency::Weekly if !self.days.is_empty() => {
                let mut days = self.days.clone();
                days.sort_unstable();
                days.dedup();
                let runs = weekly_dates(now, self.weekly.1, self.weekly.0, &days);
                if let (Some(t), Some(first)) = (time, runs.first()) {
                    let server: Vec<i64> = runs
                        .iter()
                        .map(|c| round_to_nearest_quarter(t.to_server(*c)))
                        .collect();
                    next_client = Some(*first);
                    next_server = server.first().copied();
                    let (h, m) = split(server[0]);
                    schedule.insert("hour".to_owned(), h);
                    schedule.insert("min".to_owned(), m);
                    let names: Vec<String> = server
                        .iter()
                        .map(|s| DAY_NAMES[usize::from(weekday(*s))].to_lowercase())
                        .collect();
                    schedule.insert("days".to_owned(), names.join(","));
                }
            }
            _ => {}
        }
        ComputedSchedule {
            schedule,
            next_client,
            next_server,
        }
    }
}

// ---- run history ----------------------------------------------------------------------------

/// `PolicyAlertType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PolicyAlertType {
    Error,
    Warn,
    Info,
}

impl PolicyAlertType {
    /// `PolicyAlert.FromPriority`.
    pub fn from_priority(priority: i64) -> Self {
        if priority < 3 {
            Self::Error
        } else if priority == 3 {
            Self::Warn
        } else {
            Self::Info
        }
    }
}

/// The messages of a policy (class `VMSS`, its uuid), newest first.
pub fn policy_messages<'a>(
    model: &'a ConnectionModel,
    policy: &VmssRecord,
) -> Vec<(&'a Ref<Message>, &'a Arc<MessageRecord>)> {
    let mut out: Vec<_> = model
        .messages
        .iter()
        .filter(|(_, m)| m.cls == xapi::Cls::Vmss && m.obj_uuid == policy.uuid)
        .collect();
    out.sort_by(|a, b| {
        b.1.timestamp
            .unix_seconds()
            .cmp(&a.1.timestamp.unix_seconds())
            .then(b.0.cmp(a.0))
    });
    out
}

/// `PolicyAlert`'s reading of a failure's body: per line
/// `VM:<name>UUID:<uuid>Error:['<code>', …]`, the VM's name and the error
/// code.
pub fn vm_failures(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("VM:")?;
            let (name, rest) = rest.split_once("UUID:")?;
            let (_, error) = rest.split_once("Error:")?;
            let inner = error.trim().trim_end_matches(',').strip_prefix('[')?;
            let inner = inner.strip_suffix(']')?;
            let code = inner
                .split(',')
                .map(|s| s.trim().trim_matches('\''))
                .find(|s| !s.is_empty())?;
            Some((name.trim().to_owned(), code.to_owned()))
        })
        .collect()
}

/// The run history's time span choice (`RunHistoryTimeSpan`): the last 10
/// messages, or those of the last `hours` hours.
pub fn history<'a>(
    messages: &[(&'a Ref<Message>, &'a Arc<MessageRecord>)],
    hours: Option<i64>,
    now: i64,
) -> Vec<(&'a Ref<Message>, &'a Arc<MessageRecord>)> {
    match hours {
        None => messages.iter().take(10).copied().collect(),
        Some(h) => messages
            .iter()
            .take_while(|(_, m)| {
                m.timestamp
                    .unix_seconds()
                    .is_some_and(|t| t >= now - h * HOUR)
            })
            .copied()
            .collect(),
    }
}

// ---- requests -------------------------------------------------------------------------------

/// The New Snapshot Schedule wizard's request (`CreateVMPolicy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPolicySpec {
    pub name: String,
    pub description: String,
    pub kind: VmssType,
    pub frequency: VmssFrequency,
    pub schedule: HashMap<String, String>,
    pub retained: i64,
    /// `enabled = SelectedVMs.Count != 0`.
    pub enabled: bool,
    pub vms: Vec<Ref<Vm>>,
    /// "Run the new snapshot schedule job when I click Finish".
    pub run_now: bool,
}

impl std::hash::Hash for NewPolicySpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.vms.hash(state);
    }
}

/// The policy Properties dialog's changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    pub kind: Option<VmssType>,
    pub frequency: Option<VmssFrequency>,
    pub schedule: Option<HashMap<String, String>>,
    pub retained: Option<i64>,
    /// The new members (`AssignVMsToPolicyAction`), when changed.
    pub members: Option<Vec<Ref<Vm>>>,
}

impl std::hash::Hash for PolicyChanges {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.members.hash(state);
    }
}

impl PolicyChanges {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What changed between `policy` and the dialog's values
    /// (`HasChanged` of each page).
    #[allow(clippy::too_many_arguments)]
    pub fn between(
        policy: &VmssRecord,
        name: &str,
        description: &str,
        kind: VmssType,
        frequency: VmssFrequency,
        schedule: &HashMap<String, String>,
        retained: i64,
        members: &[Ref<Vm>],
    ) -> Self {
        let mut same_members =
            members.len() == policy.vms.len() && members.iter().all(|m| policy.vms.contains(m));
        if members.is_empty() && policy.vms.is_empty() {
            same_members = true;
        }
        let frequency_changed = frequency != policy.frequency;
        Self {
            name: (name != policy.name_label).then(|| name.to_owned()),
            description: (description != policy.name_description).then(|| description.to_owned()),
            kind: (kind != policy.r#type).then_some(kind),
            frequency: frequency_changed.then_some(frequency),
            // XAPI clears the schedule when the frequency changes: it is
            // set again after.
            schedule: (frequency_changed || *schedule != policy.schedule)
                .then(|| schedule.clone())
                .filter(|s| !s.is_empty()),
            retained: (retained != policy.retained_snapshots).then_some(retained),
            members: (!same_members).then(|| members.to_vec()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WED_NOON: i64 = 1_795_608_000;

    #[test]
    fn dates_follow_the_original() {
        assert_eq!(weekday(WED_NOON), 3);
        assert_eq!(
            hourly_date(WED_NOON + 20 * 60, 15),
            WED_NOON + HOUR + 15 * 60
        );
        assert_eq!(hourly_date(WED_NOON + 10 * 60, 15), WED_NOON + 15 * 60);
        assert_eq!(daily_date(WED_NOON, 30, 3), WED_NOON + 15 * HOUR + 30 * 60);
        // Monday and Friday from Wednesday noon, at 03:00.
        let runs = weekly_dates(WED_NOON, 0, 3, &[1, 5]);
        assert_eq!(weekday(runs[0]), 5);
        assert_eq!(weekday(runs[1]), 1);
        assert_eq!(round_to_nearest_quarter(WED_NOON + 7 * 60), WED_NOON);
        assert_eq!(
            round_to_nearest_quarter(WED_NOON + 8 * 60),
            WED_NOON + 15 * 60
        );
        assert_eq!(
            round_to_nearest_quarter(WED_NOON + 53 * 60),
            WED_NOON + HOUR
        );
    }

    #[test]
    fn the_schedule_goes_to_server_time() {
        // A server two hours ahead of UTC.
        let time = ServerTime::new(WED_NOON, WED_NOON + 2 * HOUR);
        let choice = ScheduleChoice {
            frequency: VmssFrequency::Weekly,
            weekly: (23, 0),
            days: vec![2],
            ..ScheduleChoice::default()
        };
        let c = choice.compute(WED_NOON, Some(time));
        // Tuesday 23:00 here is Wednesday 01:00 there.
        assert_eq!(c.schedule.get("hour").map(String::as_str), Some("1"));
        assert_eq!(
            c.schedule.get("days").map(String::as_str),
            Some("wednesday")
        );
        let policy = VmssRecord {
            frequency: VmssFrequency::Weekly,
            schedule: c.schedule.clone(),
            retained_snapshots: 4,
            ..Default::default()
        };
        let back = ScheduleChoice::from_policy(&policy, time);
        assert_eq!(back.weekly, (23, 0));
        // As the original (`PopulateTab`), the days shown are the server's.
        assert_eq!(back.days, vec![3]);
        assert!(c.next_client.is_some());
        assert_eq!(next_runs(&policy, Some(time)).map(|r| r.0), c.next_client);
        assert!(
            ScheduleChoice::default()
                .compute(WED_NOON, None)
                .schedule
                .is_empty()
        );
    }

    #[test]
    fn failures_are_read_from_the_body() {
        let body = "VM:web UUID:1234Error:['VM_BAD_POWER_STATE', 'x']\nnoise\nVM:db UUID:5Error:['SR_FULL']";
        assert_eq!(
            vm_failures(body),
            vec![
                ("web".to_owned(), "VM_BAD_POWER_STATE".to_owned()),
                ("db".to_owned(), "SR_FULL".to_owned())
            ]
        );
        assert_eq!(PolicyAlertType::from_priority(2), PolicyAlertType::Error);
        assert_eq!(PolicyAlertType::from_priority(3), PolicyAlertType::Warn);
        assert_eq!(PolicyAlertType::from_priority(5), PolicyAlertType::Info);
    }
}
