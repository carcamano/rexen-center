//! Scheduled snapshots in the window (F12b), as the original's (see
//! [`xen_core::vmss`] for the rules):
//!
//! * **VM Snapshot Schedules...** (Pool menu, `ScheduledSnapshotsDialog`):
//!   the pool's policies (name, status, number of VMs, next snapshot time
//!   here and on the server, last result; Ctrl-click selects several),
//!   New..., Properties, Enable / Disable, Run Now, Delete (asked first,
//!   with the number of VMs), and the selected policy's run history (the
//!   last 10 messages, or those of the last 24 hours or 7 days; a failure
//!   opens to list the VMs that failed and why).
//! * **New Snapshot Schedule** (`NewPolicyWizard`): Permissions (a
//!   restricted user without `VMSS.create`; blocking), Schedule Name,
//!   Virtual Machines (VMs of another schedule are asked about), Snapshot
//!   Type, Snapshot Schedule (hourly at a quarter, daily or weekly at a
//!   time, with the number of snapshots kept; the next run here and on the
//!   server) and Finish (the summary, "run now"). The schedule is made
//!   enabled when it has VMs.
//! * **Properties** of a policy: General, Virtual Machines, Snapshot Type
//!   and Snapshot Schedule; OK saves what changed.
//! * **Assign to Snapshot Schedule** (VM menu): New Snapshot Schedule...
//!   and each policy, the VM's own ticked; choosing it again takes the VM
//!   out. The VM's Snapshots tab says which schedule it is in.
//!
//! Times: this console's are UTC (as everywhere in it); the server's are
//! its local time.

use std::collections::HashSet;

use egui::{Button, ComboBox, Context, DragValue, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Vm, Vmss, VmssFrequency, VmssRecord, VmssType};
use xen_core::rbac::{self, RbacCheck};
use xen_core::vapp::ChangeWarning;
use xen_core::vmss::{
    self as rules, NewPolicySpec, PolicyAlertType, PolicyChanges, ScheduleChoice, ServerTime,
};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionModel, ConnectionView,
    Precheck, PrecheckResult,
};

use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::{self, fill};
use crate::ops_ui::{Confirmation, Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::vapp_ui::{GroupKind, VmsPage};
use crate::vmss_strings::{day_name, vmss_strings};
use crate::window_frame::dialog_window;
use crate::xva_ui::{error_label, nav, page_list};

/// Precheck ids of this module.
const ID_BASE: u64 = 1 << 55;

// ---- menu entries ----------------------------------------------------------------------

/// "VM Snapshot Schedules..." (`VMGroup<VMSS>.ManageGroupsDialog`).
pub fn manage_entry(conn: &ConnectionView) -> Entry {
    Entry::Dialog {
        label: vmss_strings().menu_manage,
        enabled: conn.status.is_connected(),
        dialog: DialogRequest::ManagePolicies,
    }
}

/// Assign to Snapshot Schedule (`AssignGroupToolStripMenuItem<VMSS>`).
pub fn assign_entry(conn: &ConnectionView, vms: &[Ref<Vm>]) -> Entry {
    let s = vmss_strings();
    let m = &conn.model;
    let enabled = conn.status.is_connected()
        && vms.iter().any(|v| {
            m.vms
                .get(v)
                .is_some_and(|r| xen_core::vapp::can_assign(r, conn.busy_vms.contains(v)))
        });
    let mut entries = vec![Entry::Dialog {
        label: s.menu_new,
        enabled: true,
        dialog: DialogRequest::NewPolicy(vms.to_vec()),
    }];
    let current = match vms {
        [one] => m.vms.get(one).map(|v| v.snapshot_schedule.clone()),
        _ => None,
    };
    let policies = rules::policies(m);
    if !policies.is_empty() {
        entries.push(Entry::Separator);
    }
    for (i, (r, p)) in policies.into_iter().enumerate() {
        let mut label = if i < 9 {
            fill(
                s.menu_item,
                &[("index", &(i + 1).to_string()), ("name", &p.name_label)],
            )
        } else {
            p.name_label.clone()
        };
        if current.as_ref() == Some(r) {
            label = format!("\u{2022} {label}");
        }
        entries.push(Entry::Named {
            label,
            enabled: true,
            hint: None,
            dialog: DialogRequest::AssignPolicy {
                policy: r.clone(),
                vms: vms.to_vec(),
            },
        });
    }
    Entry::Submenu {
        label: s.menu_assign.to_owned(),
        enabled,
        entries,
    }
}

/// The question asked before assigning VMs of another schedule
/// (`ChangesOK`).
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let s = vmss_strings();
    let m = &conn.model;
    match request {
        ActionRequest::AssignPolicy { policy, vms } => {
            let name = m
                .vmss
                .get(policy)
                .map(|p| p.name_label.clone())
                .unwrap_or_default();
            change_text(rules::change_warning(m, vms, Some(policy), &name)).map(|text| {
                Confirmation {
                    title: s.change_title,
                    text,
                    yes: s.yes,
                    no_default: false,
                }
            })
        }
        _ => None,
    }
}

fn change_text(warning: Option<ChangeWarning>) -> Option<String> {
    let s = vmss_strings();
    Some(match warning? {
        ChangeWarning::One { vm, old, new } => {
            fill(s.change_one, &[("vm", &vm), ("old", &old), ("new", &new)])
        }
        ChangeWarning::Many { new } => fill(s.change_many, &[("new", &new)]),
    })
}

/// The Snapshots tab's line (`RefreshVMSSPanel`); true when "View snapshot
/// schedules" was clicked.
pub fn snapshots_line(ui: &mut Ui, conn: &ConnectionView, vm: &xapi::VmRecord) -> bool {
    let s = vmss_strings();
    let mut clicked = false;
    ui.horizontal(|ui| {
        match rules::policy_of(&conn.model, vm) {
            Some((_, p)) => ui.label(fill(s.tab_in, &[("name", &p.name_label)])),
            None => ui.label(s.tab_not_in),
        };
        clicked = ui.link(s.tab_view).clicked();
    });
    clicked
}

// ---- time ---------------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn civil(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month as usize, day)
}

/// `DATEFORMAT_HM` (`h:mm tt`) of a time of day.
pub fn clock_text(t: i64) -> String {
    let s = t.rem_euclid(86_400);
    let (h, m) = (s / 3600, (s % 3600) / 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ampm}")
}

/// `DATEFORMAT_DMY_HM` (`MMM d, yyyy h:mm tt`).
pub fn short_text(t: i64) -> String {
    let (y, mo, d) = civil(t.div_euclid(86_400));
    format!("{} {d}, {y} {}", &MONTHS[mo - 1][..3], clock_text(t))
}

/// `DATEFORMAT_WDMY_HM_LONG` (`dddd, MMMM d, yyyy h:mm tt`).
pub fn long_text(t: i64) -> String {
    let (y, mo, d) = civil(t.div_euclid(86_400));
    format!(
        "{}, {} {d}, {y} {}",
        day_name(rules::weekday(t), true),
        MONTHS[mo - 1],
        clock_text(t)
    )
}

/// The current time of this console (UTC).
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The server's clock, asked on the coordinator (`GetServerLocalTimeAction`).
#[derive(Default)]
struct Clock {
    pending: Option<u64>,
    time: Option<ServerTime>,
    asked: bool,
    failed: bool,
}

impl Clock {
    fn ask(&mut self, conn: &ConnectionView, backend: &BackendHandle, next_id: &mut u64) {
        let Some(host) = conn.model.pool().map(|(_, p)| p.master.clone()) else {
            self.failed = true;
            return;
        };
        *next_id += 1;
        let id = ID_BASE + *next_id;
        self.pending = Some(id);
        self.asked = true;
        backend.send(Command::Precheck {
            connection: conn.id,
            id,
            check: Precheck::ServerTime { host },
        });
    }

    fn take(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if self.pending != Some(id) {
            return false;
        }
        self.pending = None;
        match result {
            PrecheckResult::ServerTime(Ok(t)) => {
                self.time = Some(*t);
                self.failed = false;
            }
            _ => self.failed = true,
        }
        true
    }

    fn loading(&self) -> bool {
        self.pending.is_some()
    }
}

/// The text of a snapshot type (`BackupTypeToString`).
fn type_text(kind: &VmssType) -> &'static str {
    let s = vmss_strings();
    match kind {
        VmssType::Snapshot => s.disks_only,
        VmssType::Checkpoint => s.disks_and_memory,
        VmssType::SnapshotWithQuiesce => s.quiesced,
        _ => s.unknown,
    }
}

/// `FormattedSchedule`.
fn schedule_text(choice: &ScheduleChoice, long: bool) -> String {
    let s = vmss_strings();
    let at = |(h, m): (i64, i64)| clock_text(h * 3600 + m * 60);
    match choice.frequency {
        VmssFrequency::Hourly => fill(s.hourly_format, &[("min", &choice.hourly_min.to_string())]),
        VmssFrequency::Daily => fill(s.daily_format, &[("time", &at(choice.daily))]),
        VmssFrequency::Weekly if !choice.days.is_empty() => {
            let mut days = choice.days.clone();
            days.sort_unstable();
            days.dedup();
            let names: Vec<&str> = days.iter().map(|d| day_name(*d, long)).collect();
            fill(
                s.weekly_format,
                &[("time", &at(choice.weekly)), ("days", &names.join(", "))],
            )
        }
        _ => s.unknown.to_owned(),
    }
}

// ---- shared pages ------------------------------------------------------------------------

/// The Snapshot Type page (`NewPolicySnapshotTypePage`).
struct TypePage {
    kind: VmssType,
    /// Editing a policy whose type is `snapshot_with_quiesce` (VSS is
    /// gone from the servers this console supports, so the option shows
    /// only then).
    quiesce_shown: bool,
}

impl TypePage {
    fn show(&mut self, ui: &mut Ui) {
        let s = vmss_strings();
        ui.label(s.type_blurb);
        ui.add_space(6.0);
        let disk_only = matches!(
            self.kind,
            VmssType::Snapshot | VmssType::SnapshotWithQuiesce
        );
        if ui.radio(disk_only, s.disk_only).clicked() && !disk_only {
            self.kind = VmssType::Snapshot;
        }
        ui.label(RichText::new(s.disk_only_blurb).weak());
        if self.quiesce_shown {
            let mut q = self.kind == VmssType::SnapshotWithQuiesce;
            if ui.checkbox(&mut q, s.quiesce).changed() {
                self.kind = if q {
                    VmssType::SnapshotWithQuiesce
                } else {
                    VmssType::Snapshot
                };
            }
            if q {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    fill(s.quiesce_removed, &[("product", s.quiesce_product)]),
                );
            }
        }
        ui.add_space(6.0);
        let memory = self.kind == VmssType::Checkpoint;
        if ui.radio(memory, s.disk_and_memory).clicked() {
            self.kind = VmssType::Checkpoint;
        }
        ui.label(RichText::new(s.disk_and_memory_blurb).weak());
        ui.add_space(6.0);
        ui.colored_label(
            ui.visuals().warn_fg_color,
            fill(s.checkpoint_warning, &[("tools", s.vm_tools)]),
        );
    }
}

/// The Snapshot Schedule page (`NewPolicySnapshotFrequencyPage`).
struct SchedulePage {
    choice: ScheduleChoice,
    /// Filled from the policy once the server's time is known (editing).
    from: Option<VmssRecord>,
}

impl SchedulePage {
    fn show(&mut self, ui: &mut Ui, clock: &Clock, id: &str) {
        let s = vmss_strings();
        if clock.loading() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(s.loading);
            });
            return;
        }
        if let (Some(policy), Some(time)) = (self.from.take(), clock.time) {
            self.choice = ScheduleChoice::from_policy(&policy, time);
        }
        ui.label(s.schedule_blurb);
        ui.add_space(4.0);
        let c = &mut self.choice;
        for (f, label) in [
            (VmssFrequency::Hourly, s.hourly),
            (VmssFrequency::Daily, s.daily),
            (VmssFrequency::Weekly, s.weekly),
        ] {
            let on = c.frequency == f;
            if ui.radio(on, label).clicked() && !on {
                c.set_frequency(f);
            }
        }
        ui.add_space(4.0);
        let quarter = |ui: &mut Ui, id: String, min: &mut i64| {
            ComboBox::from_id_salt(id)
                .selected_text(min.to_string())
                .width(60.0)
                .show_ui(ui, |ui| {
                    for q in [0, 15, 30, 45] {
                        ui.selectable_value(min, q, q.to_string());
                    }
                });
        };
        let time = |ui: &mut Ui, id: String, hm: &mut (i64, i64)| {
            ui.add(DragValue::new(&mut hm.0).range(0..=23).suffix(" h"));
            ComboBox::from_id_salt(id)
                .selected_text(format!("{:02}", hm.1))
                .width(60.0)
                .show_ui(ui, |ui| {
                    for q in [0, 15, 30, 45] {
                        ui.selectable_value(&mut hm.1, q, format!("{q:02}"));
                    }
                });
            ui.label(clock_text(hm.0 * 3600 + hm.1 * 60));
        };
        match c.frequency {
            VmssFrequency::Hourly => {
                ui.horizontal(|ui| {
                    ui.label(s.recur_at);
                    quarter(ui, format!("{id}-min"), &mut c.hourly_min);
                    ui.label(s.minutes_past);
                });
            }
            VmssFrequency::Daily => {
                ui.horizontal(|ui| {
                    ui.label(s.recur_daily);
                    time(ui, format!("{id}-daily"), &mut c.daily);
                });
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.label(s.recur_weekly);
                    time(ui, format!("{id}-weekly"), &mut c.weekly);
                });
                ui.horizontal_wrapped(|ui| {
                    for d in 0..7u8 {
                        let mut on = c.days.contains(&d);
                        if ui.checkbox(&mut on, day_name(d, true)).changed() {
                            if on {
                                c.days.push(d);
                            } else {
                                c.days.retain(|x| *x != d);
                            }
                        }
                    }
                });
            }
        }
        ui.add_space(6.0);
        ui.label(s.retention_blurb);
        ui.horizontal(|ui| {
            let l = ui.label(s.retention);
            ui.add(DragValue::new(&mut c.retention).range(1..=10))
                .labelled_by(l.id);
        });
        ui.add_space(6.0);
        let computed = c.compute(now(), clock.time);
        let unknown = vmss_strings().unknown.to_owned();
        ui.label(fill(
            s.next_client,
            &[(
                "time",
                &computed
                    .next_client
                    .map(long_text)
                    .unwrap_or(unknown.clone()),
            )],
        ));
        ui.label(fill(
            s.next_server,
            &[(
                "time",
                &computed.next_server.map(long_text).unwrap_or(unknown),
            )],
        ));
        if !c.valid() {
            ui.colored_label(ui.visuals().error_fg_color, s.invalid_schedule);
        }
    }
}

fn name_grid(ui: &mut Ui, id: &str, name: &mut String, description: &mut String) {
    let s = vmss_strings();
    Grid::new(id).num_columns(2).show(ui, |ui| {
        let l = ui.label(s.name_label);
        ui.add(TextEdit::singleline(name).desired_width(380.0))
            .labelled_by(l.id);
        ui.end_row();
        let l = ui.label(s.description_label);
        ui.add(
            TextEdit::multiline(description)
                .desired_width(380.0)
                .desired_rows(3),
        )
        .labelled_by(l.id);
        ui.end_row();
    });
}

// ---- New Snapshot Schedule --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewPage {
    Rbac,
    Name,
    Vms,
    Type,
    Schedule,
    Finish,
}

/// The New Snapshot Schedule wizard (`NewPolicyWizard`).
pub struct NewPolicyWizard {
    connection: ConnectionId,
    pub(crate) page: NewPage,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) vms: VmsPage,
    kind: TypePage,
    pub(crate) schedule: ScheduleChoice,
    clock: Clock,
    pub(crate) run_now: bool,
    /// The `ChangesOK` question while leaving the VMs page.
    pub(crate) warning: Option<String>,
    error: Option<String>,
}

impl NewPolicyWizard {
    pub(crate) fn new(conn: &ConnectionView, vms: Vec<Ref<Vm>>) -> Self {
        let rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), &["vmss.create"]) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                vmss_strings().rbac_create,
                &[("role", &i18n::session_roles_text(conn))],
            )),
        };
        Self {
            connection: conn.id,
            page: if rbac_needed {
                NewPage::Rbac
            } else {
                NewPage::Name
            },
            rbac_needed,
            rbac_blocked,
            name: String::new(),
            description: String::new(),
            vms: VmsPage::new(GroupKind::Vmss, vms),
            kind: TypePage {
                kind: VmssType::Snapshot,
                quiesce_shown: false,
            },
            schedule: ScheduleChoice::default(),
            clock: Clock::default(),
            run_now: false,
            warning: None,
            error: None,
        }
    }

    fn pages(&self) -> Vec<NewPage> {
        let mut p = Vec::new();
        if self.rbac_needed {
            p.push(NewPage::Rbac);
        }
        p.extend([
            NewPage::Name,
            NewPage::Vms,
            NewPage::Type,
            NewPage::Schedule,
            NewPage::Finish,
        ]);
        p
    }

    fn page_name(p: NewPage) -> &'static str {
        let s = vmss_strings();
        match p {
            NewPage::Rbac => s.page_rbac,
            NewPage::Name => s.page_name,
            NewPage::Vms => s.page_vms,
            NewPage::Type => s.page_type,
            NewPage::Schedule => s.page_schedule,
            NewPage::Finish => s.page_finish,
        }
    }

    fn page_ok(&self) -> bool {
        match self.page {
            NewPage::Rbac => self.rbac_blocked.is_none(),
            NewPage::Name => !self.name.trim().is_empty(),
            NewPage::Vms => self.warning.is_none(),
            NewPage::Schedule => !self.clock.loading() && self.schedule.valid(),
            _ => true,
        }
    }

    /// `FinishWizard`: enabled when it has VMs.
    pub(crate) fn request(&self) -> ActionRequest {
        let computed = self.schedule.compute(now(), self.clock.time);
        ActionRequest::NewPolicy(Box::new(NewPolicySpec {
            name: self.name.clone(),
            description: self.description.clone(),
            kind: self.kind.kind.clone(),
            frequency: self.schedule.frequency.clone(),
            schedule: computed.schedule,
            retained: self.schedule.retention,
            enabled: !self.vms.selected.is_empty(),
            vms: self.vms.selected.clone(),
            run_now: self.run_now && !self.vms.selected.is_empty(),
        }))
    }

    fn summary(&self, m: &ConnectionModel) -> String {
        let s = vmss_strings();
        let vms: Vec<String> = self
            .vms
            .selected
            .iter()
            .filter_map(|v| m.vms.get(v))
            .map(|v| v.name_label.clone())
            .collect();
        fill(
            s.summary,
            &[
                ("name", &self.name),
                ("vms", &vms.join(", ")),
                ("type", type_text(&self.kind.kind)),
                ("schedule", &schedule_text(&self.schedule, true)),
            ],
        )
    }

    fn advance(&mut self, conn: &ConnectionView, backend: &BackendHandle, next_id: &mut u64) {
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        self.page = pages[(index + 1).min(pages.len() - 1)];
        if self.page == NewPage::Schedule && !self.clock.asked {
            self.clock.ask(conn, backend, next_id);
        }
    }

    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> Result<Option<ActionRequest>, ()> {
        let s = vmss_strings();
        let n = crate::net_strings::net_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let mut open = true;
        let mut outcome = Ok(None);
        dialog_window(s.wizard_title)
            .id(egui::Id::unique("new-policy-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([780.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(620.0);
                        egui::ScrollArea::vertical()
                            .id_salt("new-policy-page")
                            .max_height(390.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                NewPage::Rbac => {
                                    ui.heading(s.page_rbac);
                                    match &self.rbac_blocked {
                                        Some(why) => {
                                            ui.colored_label(ui.visuals().error_fg_color, why);
                                        }
                                        None => {
                                            ui.label(s.rbac_click_next);
                                        }
                                    }
                                }
                                NewPage::Name => {
                                    ui.heading(s.name_title);
                                    ui.label(s.name_wizard_blurb);
                                    ui.label(s.name_blurb);
                                    ui.add_space(6.0);
                                    name_grid(
                                        ui,
                                        "new-policy-name",
                                        &mut self.name,
                                        &mut self.description,
                                    );
                                }
                                NewPage::Vms => {
                                    ui.heading(s.vms_title);
                                    self.vms.show(ui, &conn, show_hidden, "new-policy-vms");
                                    if let Some(text) = self.warning.clone() {
                                        ui.add_space(6.0);
                                        ui.colored_label(ui.visuals().warn_fg_color, text);
                                        ui.horizontal(|ui| {
                                            if ui.button(s.yes).clicked() {
                                                self.warning = None;
                                                self.advance(&conn, backend, next_id);
                                            }
                                            if ui.button(s.no).clicked() {
                                                self.warning = None;
                                            }
                                        });
                                    }
                                }
                                NewPage::Type => {
                                    ui.heading(s.type_title);
                                    self.kind.show(ui);
                                }
                                NewPage::Schedule => {
                                    ui.heading(s.schedule_title);
                                    let mut page = SchedulePage {
                                        choice: self.schedule.clone(),
                                        from: None,
                                    };
                                    page.show(ui, &self.clock, "new-policy-schedule");
                                    self.schedule = page.choice;
                                }
                                NewPage::Finish => {
                                    ui.heading(s.finish_title);
                                    ui.label(s.finish_blurb);
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(self.summary(&conn.model)).monospace());
                                    ui.add_space(6.0);
                                    ui.add_enabled(
                                        !self.vms.selected.is_empty(),
                                        egui::Checkbox::new(&mut self.run_now, s.run_now),
                                    );
                                }
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.add_space(8.0);
                let last = self.page == NewPage::Finish;
                let (previous, next, cancel) = nav(
                    ui,
                    index > 0,
                    self.page_ok(),
                    if last { n.finish } else { n.next },
                );
                if previous && index > 0 {
                    self.warning = None;
                    self.page = pages[index - 1];
                }
                if next {
                    if last {
                        outcome = Ok(Some(self.request()));
                    } else if self.page == NewPage::Vms {
                        self.warning = change_text(rules::change_warning(
                            &conn.model,
                            &self.vms.selected,
                            None,
                            &self.name,
                        ));
                        if self.warning.is_none() {
                            self.advance(&conn, backend, next_id);
                        }
                    } else {
                        self.advance(&conn, backend, next_id);
                    }
                }
                if cancel {
                    outcome = Err(());
                }
            });
        if !open {
            return Err(());
        }
        outcome
    }
}

// ---- Properties ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropTab {
    General,
    Vms,
    Type,
    Schedule,
}

/// The Properties dialog of a policy.
pub struct PolicyProperties {
    connection: ConnectionId,
    pub(crate) policy: Ref<Vmss>,
    pub(crate) tab: PropTab,
    original: VmssRecord,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) vms: VmsPage,
    kind: TypePage,
    schedule: SchedulePage,
    clock: Clock,
    pub(crate) warning: Option<String>,
}

impl PolicyProperties {
    fn new(
        conn: &ConnectionView,
        policy: Ref<Vmss>,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> Option<Self> {
        let p = (**conn.model.vmss.get(&policy)?).clone();
        let mut clock = Clock::default();
        clock.ask(conn, backend, next_id);
        Some(Self {
            connection: conn.id,
            policy,
            tab: PropTab::General,
            name: p.name_label.clone(),
            description: p.name_description.clone(),
            vms: VmsPage::new(GroupKind::Vmss, p.vms.clone()),
            kind: TypePage {
                kind: p.r#type.clone(),
                quiesce_shown: p.r#type == VmssType::SnapshotWithQuiesce,
            },
            schedule: SchedulePage {
                choice: ScheduleChoice {
                    frequency: p.frequency.clone(),
                    retention: p.retained_snapshots,
                    ..ScheduleChoice::default()
                },
                from: Some(p.clone()),
            },
            clock,
            original: p,
            warning: None,
        })
    }

    /// Whether the server's time is known (the schedule tab is filled).
    #[cfg(test)]
    pub(crate) fn has_time(&self) -> bool {
        self.clock.time.is_some() && self.schedule.from.is_none()
    }

    /// Changes the Snapshot Schedule tab's values (tests).
    #[cfg(test)]
    pub(crate) fn set_schedule(&mut self, f: impl FnOnce(&mut ScheduleChoice)) {
        f(&mut self.schedule.choice);
    }

    pub(crate) fn changes(&self) -> PolicyChanges {
        let computed = self.schedule.choice.compute(now(), self.clock.time);
        // Without the server's time the schedule cannot be computed: it is
        // left as it was.
        let schedule = if self.clock.time.is_some() {
            computed.schedule
        } else {
            self.original.schedule.clone()
        };
        let frequency = if self.clock.time.is_some() {
            self.schedule.choice.frequency.clone()
        } else {
            self.original.frequency.clone()
        };
        PolicyChanges::between(
            &self.original,
            &self.name,
            &self.description,
            self.kind.kind.clone(),
            frequency,
            &schedule,
            self.schedule.choice.retention,
            &self.vms.selected,
        )
    }

    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let s = vmss_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        if !conn.model.vmss.contains_key(&self.policy) {
            return Err(());
        }
        // `PopulateTab` once the server's time is known, whichever tab shows.
        if let (Some(time), Some(policy)) = (self.clock.time, self.schedule.from.take()) {
            self.schedule.choice = ScheduleChoice::from_policy(&policy, time);
        }
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let mut outcome = Ok(None);
        window(fill(
            s.properties_title,
            &[("name", &self.original.name_label)],
        ))
        .show(ctx, |ui| {
            let mut divider = 0.0;
            let row = ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    let pages = [
                        (PropTab::General, s.tab_general, ""),
                        (PropTab::Vms, s.page_vms, ""),
                        (PropTab::Type, s.page_type, ""),
                        (PropTab::Schedule, s.page_schedule, ""),
                    ];
                    if let Some(tab) = crate::controls::page_list(ui, &self.tab, pages) {
                        self.tab = tab;
                    }
                });
                divider = crate::controls::divider_gap(ui);
                ui.vertical(|ui| {
                    ui.set_min_width(600.0);
                    egui::ScrollArea::vertical()
                        .id_salt("policy-props")
                        .max_height(380.0)
                        .show(ui, |ui| match self.tab {
                            PropTab::General => {
                                ui.label(s.general_blurb);
                                name_grid(
                                    ui,
                                    "policy-props-name",
                                    &mut self.name,
                                    &mut self.description,
                                );
                            }
                            PropTab::Vms => {
                                self.vms.show(ui, &conn, show_hidden, "policy-props-vms")
                            }
                            PropTab::Type => self.kind.show(ui),
                            PropTab::Schedule => {
                                self.schedule.show(ui, &self.clock, "policy-props-schedule");
                            }
                        });
                });
            });
            crate::controls::paint_divider(ui, divider, row.response.rect);
            if let Some(text) = self.warning.clone() {
                ui.colored_label(ui.visuals().warn_fg_color, text);
                ui.horizontal(|ui| {
                    if ui.button(s.yes).clicked() {
                        self.warning = None;
                        outcome = Ok(Some(ActionRequest::SetPolicy {
                            policy: self.policy.clone(),
                            changes: Box::new(self.changes()),
                        }));
                    }
                    if ui.button(s.no).clicked() {
                        self.warning = None;
                    }
                });
            }
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                let valid = !self.name.trim().is_empty() && self.schedule.choice.valid();
                if row
                    .add_enabled(ButtonRole::Primary, valid, Button::new(s.ok))
                    .clicked()
                {
                    let changes = self.changes();
                    if changes.is_empty() {
                        outcome = Err(());
                    } else {
                        let warning = changes.members.as_ref().and_then(|members| {
                            change_text(rules::change_warning(
                                &conn.model,
                                members,
                                Some(&self.policy),
                                &self.name,
                            ))
                        });
                        match warning {
                            Some(text) => self.warning = Some(text),
                            None => {
                                outcome = Ok(Some(ActionRequest::SetPolicy {
                                    policy: self.policy.clone(),
                                    changes: Box::new(changes),
                                }));
                            }
                        }
                    }
                }
                if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                    outcome = Err(());
                }
            });
        });
        outcome
    }
}

// ---- VM Snapshot Schedules ---------------------------------------------------------------------

/// The run history's time span (`comboBoxTimeSpan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Span {
    Last10,
    Day,
    Week,
}

/// The VM Snapshot Schedules dialog (`ScheduledSnapshotsDialog`).
pub struct ManagePolicies {
    connection: ConnectionId,
    pub(crate) selected: Vec<Ref<Vmss>>,
    clock: Clock,
    /// What the policies were when the server's time was last asked
    /// (`VMSSCollectionChanged` asks again).
    seen: Vec<(Ref<Vmss>, VmssRecord)>,
    /// Rows busy with an operation: the history floor when it was sent.
    busy: Vec<(Ref<Vmss>, u64)>,
    pub(crate) history: bool,
    pub(crate) span: Span,
    expanded: HashSet<Ref<xapi::Message>>,
    pub(crate) question: Option<(Vec<Ref<Vmss>>, String)>,
}

/// What the dialog asks the window for.
pub(crate) enum ManageAction {
    Request(ActionRequest),
    Dialog(DialogRequest),
}

impl ManagePolicies {
    fn delete_text(&self, m: &ConnectionModel) -> String {
        let s = vmss_strings();
        let policies: Vec<_> = self.selected.iter().filter_map(|r| m.vmss.get(r)).collect();
        let count: usize = policies.iter().map(|p| p.vms.len()).sum();
        let c = count.to_string();
        match policies.as_slice() {
            [one] if count == 0 => fill(s.delete_one_0, &[("name", &one.name_label)]),
            [one] => fill(s.delete_one, &[("name", &one.name_label), ("count", &c)]),
            _ if count == 0 => s.delete_many_0.to_owned(),
            _ => fill(s.delete_many, &[("count", &c)]),
        }
    }

    fn last_result(m: &ConnectionModel, p: &VmssRecord) -> Option<PolicyAlertType> {
        rules::policy_messages(m, p)
            .first()
            .map(|(_, msg)| PolicyAlertType::from_priority(msg.priority))
    }

    fn result_text(t: Option<PolicyAlertType>) -> &'static str {
        let s = vmss_strings();
        match t {
            None => s.not_yet_run,
            Some(PolicyAlertType::Error) => s.failed,
            Some(PolicyAlertType::Warn) => s.warning,
            Some(PolicyAlertType::Info) => s.succeeded,
        }
    }

    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> Result<Vec<ManageAction>, ()> {
        let s = vmss_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        let m = &conn.model;
        let policies: Vec<(Ref<Vmss>, VmssRecord)> = rules::policies(m)
            .into_iter()
            .map(|(r, p)| (r.clone(), (**p).clone()))
            .collect();
        if policies != self.seen && !self.clock.loading() {
            self.seen = policies.clone();
            self.clock.ask(&conn, backend, next_id);
        }
        self.selected.retain(|r| m.vmss.contains_key(r));
        self.busy.retain(|(_, floor)| {
            !state
                .operations
                .iter()
                .any(|o| o.id > *floor && !o.is_running())
        });
        let busy: HashSet<Ref<Vmss>> = self.busy.iter().map(|(r, _)| r.clone()).collect();
        let current = match self.selected.as_slice() {
            [one] => policies.iter().find(|(r, _)| r == one).cloned(),
            _ => None,
        };
        let floor = state.operations.iter().map(|o| o.id).max().unwrap_or(0);
        let mut actions = Vec::new();
        let mut close = false;
        let pool = connection_name(&conn);
        let (protected, total) =
            rules::protected_counts(m, state.settings.preferences.show_hidden_objects);
        window(s.dialog_title.to_owned()).show(ctx, |ui| {
            ui.label(s.top_blurb);
            ui.label(fill(
                if xen_core::objects::pool_is_visible(m) {
                    s.defined_in_pool
                } else {
                    s.defined_in_server
                },
                &[
                    ("pool", &pool),
                    ("protected", &protected.to_string()),
                    ("total", &total.to_string()),
                ],
            ));
            ui.horizontal(|ui| {
                if ui.button(s.button_new).clicked() {
                    actions.push(ManageAction::Dialog(DialogRequest::NewPolicy(Vec::new())));
                }
                let one = current.as_ref().filter(|(r, _)| !busy.contains(r));
                if ui
                    .add_enabled(one.is_some(), Button::new(s.button_properties))
                    .clicked()
                    && let Some((r, _)) = one
                {
                    actions.push(ManageAction::Dialog(DialogRequest::PolicyProperties(
                        r.clone(),
                    )));
                }
                let label = match &current {
                    Some((_, p)) if p.enabled => s.button_disable,
                    _ => s.button_enable,
                };
                if ui
                    .add_enabled(
                        one.is_some_and(|(_, p)| rules::can_toggle(p)),
                        Button::new(label),
                    )
                    .clicked()
                    && let Some((r, p)) = one
                {
                    self.busy.push((r.clone(), floor));
                    actions.push(ManageAction::Request(ActionRequest::SetPolicyEnabled {
                        policy: r.clone(),
                        enabled: !p.enabled,
                    }));
                }
                if ui
                    .add_enabled(
                        one.is_some_and(|(_, p)| rules::can_run_now(p)),
                        Button::new(s.button_run_now),
                    )
                    .clicked()
                    && let Some((r, _)) = one
                {
                    self.busy.push((r.clone(), floor));
                    actions.push(ManageAction::Request(ActionRequest::RunPolicyNow {
                        policy: r.clone(),
                    }));
                }
                let deletable = self.selected.iter().any(|r| !busy.contains(r));
                if ui
                    .add_enabled(deletable, Button::new(s.button_delete))
                    .clicked()
                {
                    self.question = Some((self.selected.clone(), self.delete_text(m)));
                }
            });
            ui.separator();
            if self.clock.loading() && self.clock.time.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(s.loading);
                });
            }
            Grid::new("policies-grid")
                .num_columns(6)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        s.col_name,
                        s.col_status,
                        s.col_vms,
                        s.col_next,
                        s.col_server_time,
                        s.col_last,
                    ] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for (r, p) in &policies {
                        let on = self.selected.contains(r);
                        if ui.selectable_label(on, &p.name_label).clicked() {
                            let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                            if !multi {
                                self.selected.clear();
                            }
                            if on && multi {
                                self.selected.retain(|x| x != r);
                            } else {
                                self.selected.push(r.clone());
                            }
                        }
                        ui.label(if p.enabled { s.enabled } else { s.disabled });
                        ui.label(rules::vm_count(m, p).to_string());
                        match rules::next_runs(p, self.clock.time) {
                            Some((client, server)) => {
                                ui.label(fill(s.time_utc, &[("time", &short_text(client))]));
                                ui.label(short_text(server));
                            }
                            None => {
                                ui.label(s.host_not_live);
                                ui.label(s.host_not_live);
                            }
                        }
                        ui.label(Self::result_text(Self::last_result(m, p)));
                        ui.end_row();
                    }
                });
            if policies.is_empty() {
                ui.label(RichText::new(s.no_policies).weak());
            }
            ui.add_space(4.0);
            let toggle = if self.history {
                s.hide_history
            } else {
                s.show_history
            };
            if ui.button(toggle).clicked() {
                self.history = !self.history;
            }
            if self.history
                && let Some((_, p)) = &current
            {
                ui.horizontal(|ui| {
                    ui.label(fill(s.history_for, &[("name", &p.name_label)]));
                    ui.label(s.show);
                    ComboBox::from_id_salt("policy-history-span")
                        .selected_text(match self.span {
                            Span::Last10 => s.span_last_10,
                            Span::Day => s.span_24h,
                            Span::Week => s.span_7d,
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.span, Span::Last10, s.span_last_10);
                            ui.selectable_value(&mut self.span, Span::Day, s.span_24h);
                            ui.selectable_value(&mut self.span, Span::Week, s.span_7d);
                        });
                });
                let messages = rules::policy_messages(m, p);
                let hours = match self.span {
                    Span::Last10 => None,
                    Span::Day => Some(24),
                    Span::Week => Some(7 * 24),
                };
                let reference = self
                    .clock
                    .time
                    .map(|t| t.server_local + t.diff)
                    .unwrap_or_else(now);
                Grid::new("policy-history")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("");
                        ui.strong(s.col_date);
                        ui.strong(s.col_description);
                        ui.end_row();
                        for (r, msg) in rules::history(&messages, hours, reference) {
                            let kind = PolicyAlertType::from_priority(msg.priority);
                            ui.label(Self::result_text(Some(kind)));
                            ui.label(
                                msg.timestamp
                                    .unix_seconds()
                                    .map(|t| fill(s.time_utc, &[("time", &short_text(t))]))
                                    .unwrap_or_default(),
                            );
                            let title = crate::alerts_view::message_title(&msg.name);
                            let failures = rules::vm_failures(&msg.body);
                            if kind == PolicyAlertType::Error && !failures.is_empty() {
                                let open = self.expanded.contains(r);
                                let head = fill(
                                    s.alert_failed_count,
                                    &[("title", &title), ("count", &failures.len().to_string())],
                                );
                                ui.vertical(|ui| {
                                    if ui
                                        .selectable_label(
                                            open,
                                            format!("{} {head}", if open { "[-]" } else { "[+]" }),
                                        )
                                        .clicked()
                                    {
                                        if open {
                                            self.expanded.remove(r);
                                        } else {
                                            self.expanded.insert(r.clone());
                                        }
                                    }
                                    if open {
                                        for (vm, code) in &failures {
                                            let error = i18n::friendly_error(code, &[])
                                                .unwrap_or_else(|| code.clone());
                                            ui.label(fill(
                                                s.alert_vm_error,
                                                &[("vm", vm), ("error", &error)],
                                            ));
                                        }
                                    }
                                });
                            } else {
                                ui.label(title);
                            }
                            ui.end_row();
                        }
                    });
            }
            if let Some((_, text)) = &self.question {
                ui.separator();
                ui.strong(s.delete_title);
                ui.colored_label(ui.visuals().warn_fg_color, text.clone());
                ui.horizontal(|ui| {
                    if ui.button(s.yes).clicked()
                        && let Some((policies, _)) = self.question.take()
                    {
                        for p in &policies {
                            self.busy.push((p.clone(), floor));
                        }
                        actions.push(ManageAction::Request(ActionRequest::DeletePolicies {
                            policies,
                        }));
                    }
                    if ui.button(s.no).clicked() {
                        self.question = None;
                    }
                });
            }
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                if row.button(ButtonRole::Cancel, s.button_close).clicked() {
                    close = true;
                }
            });
        });
        if close {
            return Err(());
        }
        Ok(actions)
    }
}

// ---- the dialogs together ----------------------------------------------------------------

/// The F12b snapshot schedule dialogs of the window.
#[derive(Default)]
pub struct VmssUi {
    pub(crate) manage: Option<ManagePolicies>,
    pub(crate) wizard: Option<NewPolicyWizard>,
    pub(crate) properties: Option<PolicyProperties>,
    next_id: u64,
    pub errors: Vec<String>,
}

impl VmssUi {
    /// Opens the dialog of `request`; assigning goes straight to the
    /// operations.
    pub fn open(
        &mut self,
        conn: &ConnectionView,
        request: DialogRequest,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        match request {
            DialogRequest::ManagePolicies => {
                self.manage = Some(ManagePolicies {
                    connection: conn.id,
                    selected: rules::policies(&conn.model)
                        .first()
                        .map(|(r, _)| vec![(*r).clone()])
                        .unwrap_or_default(),
                    clock: Clock::default(),
                    seen: Vec::new(),
                    busy: Vec::new(),
                    history: false,
                    span: Span::Last10,
                    expanded: HashSet::new(),
                    question: None,
                });
            }
            DialogRequest::NewPolicy(vms) => self.wizard = Some(NewPolicyWizard::new(conn, vms)),
            DialogRequest::PolicyProperties(policy) => {
                self.properties = PolicyProperties::new(conn, policy, backend, &mut self.next_id);
            }
            DialogRequest::AssignPolicy { policy, vms } => {
                let Some(p) = conn.model.vmss.get(&policy) else {
                    return;
                };
                // Choosing a VM's own schedule takes it out.
                if let [one] = vms.as_slice()
                    && p.vms.contains(one)
                {
                    ops.request(
                        conn,
                        ActionRequest::RemoveFromPolicy {
                            policy,
                            vms: vec![one.clone()],
                        },
                        backend,
                    );
                    return;
                }
                let mut members = vms.clone();
                for v in &p.vms {
                    if !members.contains(v) {
                        members.push(v.clone());
                    }
                }
                ops.request(
                    conn,
                    ActionRequest::AssignPolicy {
                        policy,
                        vms: members,
                    },
                    backend,
                );
            }
            _ => {}
        }
    }

    /// Takes the answer to a question of these dialogs.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if let Some(w) = self.wizard.as_mut()
            && w.clock.take(id, result)
        {
            return true;
        }
        if let Some(p) = self.properties.as_mut()
            && p.clock.take(id, result)
        {
            return true;
        }
        if let Some(d) = self.manage.as_mut()
            && d.clock.take(id, result)
        {
            return true;
        }
        false
    }

    /// Draws the open dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(d) = self.manage.as_mut() {
            let id = d.connection;
            match d.show(ctx, state, backend, &mut self.next_id) {
                Ok(actions) => {
                    for action in actions {
                        let Some(conn) = state.connection(id) else {
                            continue;
                        };
                        match action {
                            // Asked in the dialog already.
                            ManageAction::Request(request) => ops.dispatch(conn, request, backend),
                            ManageAction::Dialog(request) => self.open(conn, request, ops, backend),
                        }
                    }
                }
                Err(()) => self.manage = None,
            }
        }
        if let Some(w) = self.wizard.as_mut() {
            let id = w.connection;
            match w.show(ctx, state, backend, &mut self.next_id) {
                Ok(Some(request)) => {
                    if let Some(conn) = state.connection(id) {
                        ops.dispatch(conn, request, backend);
                    }
                    self.wizard = None;
                }
                Ok(None) => {}
                Err(()) => self.wizard = None,
            }
        }
        if let Some(p) = self.properties.as_mut() {
            let id = p.connection;
            match p.show(ctx, state) {
                Ok(Some(request)) => {
                    if let Some(conn) = state.connection(id) {
                        ops.dispatch(conn, request, backend);
                    }
                    self.properties = None;
                }
                Ok(None) => {}
                Err(()) => self.properties = None,
            }
        }
    }
}
