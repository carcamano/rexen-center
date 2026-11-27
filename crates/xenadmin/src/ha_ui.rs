//! High availability in the window: the Pool menu's High Availability
//! entries (`HACommand`, `HAConfigureCommand`, `HADisableCommand`), the
//! Configure HA wizard (`HAWizard`: prerequisites, the RBAC check, the
//! heartbeat SR, the restart priorities with the failure tolerance, the
//! summary), the HA settings dialog of a pool with HA on
//! (`EditVmHaPrioritiesDialog`) and the pool's HA tab (`HAPage`). The
//! questions HA adds to other operations are asked by [`OpsUi`]; the Users
//! tab is [`crate::ad_ui`].
//!
//! The server answers the wizard's questions through prechecks: which
//! shared SRs can hold the statefile (`SR.assert_can_host_ha_statefile`),
//! which VMs are agile (`VM.assert_agile`) and how many server failures
//! the pool can tolerate with the chosen priorities
//! (`pool.ha_compute_hypothetical_max_host_failures_to_tolerate`). None of
//! them changes anything.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use egui::{Button, Context, DragValue, Grid, RichText, Ui};
use xapi::{Ref, Sr, Vm};
use xen_core::ha::{self, HaProblem, RestartPriority};
use xen_core::operations::{HaKind, OperationKind};
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, Precheck,
    PrecheckResult,
};

use crate::controls::{ButtonRole, dialog_buttons};
use crate::ha_strings::{ha_strings, not_agile_reason, priority_name, vm_count};
use crate::i18n::{self, fill};
use crate::ops_ui::{Confirmation, Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::window_frame::dialog_window;

/// `HAPage.HB_INITIALIZATION_DELAY`.
const HEARTBEAT_DELAY: Duration = Duration::from_secs(30);

/// The question asked before an HA request, if any.
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let h = ha_strings();
    match request {
        ActionRequest::DisableHa => Some(Confirmation {
            title: h.disable_ha_title,
            text: fill(h.disable_query, &[("pool", &connection_name(conn))]),
            yes: crate::ops_strings::ops_strings().yes,
            no_default: false,
        }),
        _ => None,
    }
}

/// Whether this client is turning HA on (`Some(true)`) or off on `conn`
/// (`HelpersGUI.FindActiveHaAction`).
pub fn client_busy(state: &AppState, conn: ConnectionId) -> Option<bool> {
    state
        .operations
        .iter()
        .filter(|o| o.connection == Some(conn) && o.is_running())
        .find_map(|o| match &o.kind {
            OperationKind::Ha(HaKind::Enable { .. }) => Some(true),
            OperationKind::Ha(HaKind::Disable { .. }) => Some(false),
            _ => None,
        })
}

fn problem_text(conn: &ConnectionView, p: &HaProblem) -> String {
    let h = ha_strings();
    match p {
        HaProblem::Standalone => h.ha_standalone.to_owned(),
        HaProblem::NoCoordinator => crate::i18n::strings().op_not_connected.to_owned(),
        HaProblem::Busy { enabling } => fill(
            if *enabling {
                h.ha_page_enabling
            } else {
                h.ha_page_disabling
            },
            &[("pool", &connection_name(conn))],
        ),
        HaProblem::SecretRotationPending => h.psr_pending.to_owned(),
    }
}

/// The Pool menu's High Availability submenu (`HACommand`).
pub fn pool_entries(state: &AppState, conn: &ConnectionView) -> Vec<Entry> {
    let h = ha_strings();
    let usable = conn.status.is_connected();
    let busy = client_busy(state, conn.id);
    let configure = ha::configure_problem(&conn.model, busy);
    let disable = ha::disable_problem(&conn.model, busy);
    let ha_on = conn.model.pool().is_some_and(|(_, p)| p.ha_enabled);
    let any = usable && (configure.is_none() || (disable.is_none() && ha_on));
    vec![Entry::Submenu {
        label: h.menu_high_availability.to_owned(),
        enabled: any,
        entries: vec![
            Entry::Named {
                label: h.configure_ha.to_owned(),
                enabled: usable && configure.is_none(),
                hint: configure.as_ref().map(|p| problem_text(conn, p)),
                dialog: DialogRequest::ConfigureHa,
            },
            Entry::Named {
                label: h.disable_ha.to_owned(),
                enabled: usable && disable.is_none() && ha_on,
                hint: disable.as_ref().map(|p| problem_text(conn, p)),
                dialog: DialogRequest::DisableHa,
            },
        ],
    }]
}

// ---- the restart priorities and the failure tolerance ----------------------

/// One VM of the priorities grid (`VmWithSettingsRow`).
struct Row {
    vm: Ref<Vm>,
    name: String,
    priority: Option<RestartPriority>,
    order: i64,
    delay: i64,
    /// `None` while calculating; `Some(None)` agile; `Some(Some(code))`
    /// not agile, and why.
    agile: Option<Option<String>>,
    selected: bool,
}

/// The restart priorities page (`AssignPriorities`) with its failure
/// tolerance control (`HaNtolIndicator`).
struct Priorities {
    rows: Vec<Row>,
    changes_made: bool,
    agility: Option<u64>,
    planner: Option<u64>,
    /// The settings the planner was last asked about.
    asked: Option<BTreeMap<Ref<Vm>, String>>,
    /// The planner's answer: `None` while calculating.
    max: Option<Result<i64, String>>,
    /// The failures to tolerate (-1 until known).
    ntol: i64,
}

impl Priorities {
    fn new(conn: &ConnectionView, protect_by_default: bool, show_hidden: bool) -> Self {
        let vms = ha::protectable_vms(&conn.model, show_hidden);
        let first = protect_by_default && ha::first_time(&vms);
        let rows = vms
            .into_iter()
            .map(|(r, v)| Row {
                vm: r.clone(),
                name: v.name_label.clone(),
                priority: (!first).then(|| ha::priority(v)),
                order: v.order,
                delay: v.start_delay,
                agile: None,
                selected: false,
            })
            .collect();
        Self {
            rows,
            changes_made: false,
            agility: None,
            planner: None,
            asked: None,
            max: None,
            ntol: -1,
        }
    }

    fn settings(&self) -> BTreeMap<Ref<Vm>, String> {
        ha::priorities_for_api(
            self.rows
                .iter()
                .map(|r| (&r.vm, r.priority.unwrap_or(RestartPriority::BestEffort))),
        )
        .into_iter()
        .collect()
    }

    fn calculating(&self) -> bool {
        self.max.is_none()
    }

    fn overcommitted(&self) -> bool {
        matches!(self.max, Some(Ok(max)) if self.ntol > max)
    }

    /// `AssignPriorities.EnableNext`.
    fn ok(&self) -> bool {
        !self.calculating() && self.ntol >= 0 && !self.overcommitted()
    }

    /// Asks the server what it needs: each VM's agility once, the planner
    /// whenever the settings changed.
    fn ask(&mut self, conn: &ConnectionView, backend: &BackendHandle, ids: &mut IdSource) {
        if self.agility.is_none() && self.rows.iter().any(|r| r.agile.is_none()) {
            let id = ids.next(Pending::Agility);
            self.agility = Some(id);
            backend.send(Command::Precheck {
                connection: conn.id,
                id,
                check: Precheck::Agility {
                    vms: self.rows.iter().map(|r| r.vm.clone()).collect(),
                },
            });
        }
        // Rows whose priority is not known yet wait for their agility.
        if self.rows.iter().any(|r| r.priority.is_none()) {
            return;
        }
        let settings = self.settings();
        if self.asked.as_ref() != Some(&settings) {
            let id = ids.next(Pending::Planner);
            self.planner = Some(id);
            self.max = None;
            self.asked = Some(settings.clone());
            backend.send(Command::Precheck {
                connection: conn.id,
                id,
                check: Precheck::HaMaxFailures { restart: settings },
            });
        }
    }

    fn on_agility(&mut self, id: u64, result: &[(Ref<Vm>, Option<String>)]) -> bool {
        if self.agility != Some(id) {
            return false;
        }
        for (vm, why) in result {
            if let Some(row) = self.rows.iter_mut().find(|r| &r.vm == vm) {
                row.priority = ha::priority_after_agility(row.priority, why.is_none());
                row.agile = Some(why.clone());
            }
        }
        true
    }

    fn on_planner(
        &mut self,
        id: u64,
        result: &Result<i64, std::sync::Arc<xapi::Error>>,
        pool: Option<&xapi::PoolRecord>,
    ) -> bool {
        if self.planner != Some(id) {
            return false;
        }
        self.planner = None;
        match result {
            Ok(max) => {
                // `LoadCalculationSucceededMode`: the value is set once.
                if self.ntol == -1 {
                    self.ntol = pool.map_or(*max, |p| ha::initial_ntol(p, *max));
                }
                self.max = Some(Ok(*max));
            }
            Err(e) => {
                self.ntol = -1;
                self.max = Some(Err(i18n::describe_xapi_error(e)));
            }
        }
        true
    }

    /// The changed startup options (`GetChangedStartupOptions`).
    fn changed(&self, conn: &ConnectionView) -> Vec<ha::VmStartup> {
        ha::changed_startup(
            &conn.model,
            self.rows
                .iter()
                .map(|r| (r.vm.clone(), r.priority, r.order, r.delay)),
        )
    }

    fn ui(&mut self, ui: &mut Ui, conn: &ConnectionView, enabled: bool) {
        let h = ha_strings();
        ui.add_enabled_ui(enabled, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("ha-priorities")
                .max_height(170.0)
                .show(ui, |ui| {
                    Grid::new("ha-priorities-grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            for t in [
                                h.col_vm,
                                h.col_restart_priority,
                                h.col_start_order,
                                h.col_delay,
                                h.col_agile,
                            ] {
                                ui.strong(t);
                            }
                            ui.end_row();
                            let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                            let mut clicked = None;
                            for (i, row) in self.rows.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if let Some(vm) = conn.model.vms.get(&row.vm) {
                                        crate::icons::vm(ui, &vm.power_state);
                                    }
                                    if ui.selectable_label(row.selected, &row.name).clicked() {
                                        clicked = Some(i);
                                    }
                                });
                                ui.label(row.priority.map(priority_name).unwrap_or(""));
                                ui.label(row.order.to_string());
                                ui.label(fill(h.time_seconds, &[("n", &row.delay.to_string())]));
                                match &row.agile {
                                    None => ui.label(h.calculating_agility),
                                    Some(None) => ui.label(h.yes),
                                    Some(Some(code)) => {
                                        ui.label(h.no).on_hover_text(not_agile_reason(code))
                                    }
                                };
                                ui.end_row();
                            }
                            if let Some(i) = clicked {
                                if !multi {
                                    for r in &mut self.rows {
                                        r.selected = false;
                                    }
                                }
                                self.rows[i].selected = !self.rows[i].selected || !multi;
                            }
                        });
                });
            ui.add_space(4.0);
            self.selection_controls(ui);
        });
        ui.add_space(6.0);
        self.ntol_ui(ui, enabled);
    }

    /// `AssignPriorities.updateButtons` and the controls it drives.
    fn selection_controls(&mut self, ui: &mut Ui) {
        let h = ha_strings();
        let selected: Vec<usize> = (0..self.rows.len())
            .filter(|i| self.rows[*i].selected)
            .collect();
        let any = !selected.is_empty();
        let non_agile = selected
            .iter()
            .find_map(|i| self.rows[*i].agile.clone().flatten());
        let current: Vec<Option<RestartPriority>> =
            selected.iter().map(|i| self.rows[*i].priority).collect();
        let shown = match current.first() {
            Some(Some(p)) if current.iter().all(|c| c == &Some(*p)) => priority_name(*p),
            Some(_) if current.len() > 1 && current.iter().all(Option::is_some) => h.mixed_levels,
            _ => "",
        };
        let same = |f: fn(&Row) -> i64| -> Option<i64> {
            let mut values = selected.iter().map(|i| f(&self.rows[*i]));
            let first = values.next()?;
            values.all(|v| v == first).then_some(first)
        };
        let (mut order, mut delay) = (
            same(|r| r.order).unwrap_or(0),
            same(|r| r.delay).unwrap_or(0),
        );
        Grid::new("ha-selection-controls")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label(h.label_priority);
                ui.add_enabled_ui(any, |ui| {
                    egui::ComboBox::from_id_salt("ha-priority")
                        .selected_text(shown)
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for p in RestartPriority::AVAILABLE {
                                let blocked = p.is_restart() && non_agile.is_some();
                                let r = ui.add_enabled(
                                    !blocked,
                                    egui::Button::selectable(false, priority_name(p)),
                                );
                                let r = match &non_agile {
                                    Some(code) if blocked => {
                                        r.on_disabled_hover_text(not_agile_reason(code))
                                    }
                                    _ => r,
                                };
                                if r.clicked() {
                                    for i in &selected {
                                        if self.rows[*i].priority != Some(p) {
                                            self.rows[*i].priority = Some(p);
                                            self.changes_made = true;
                                        }
                                    }
                                }
                            }
                        });
                });
                ui.end_row();
                ui.label(h.label_start_order);
                ui.horizontal(|ui| {
                    let o = ui.add_enabled(any, DragValue::new(&mut order).range(0..=i64::MAX));
                    ui.label(h.label_start_delay);
                    let d = ui.add_enabled(any, DragValue::new(&mut delay).range(0..=i64::MAX));
                    ui.label(h.seconds);
                    if o.changed() {
                        for i in &selected {
                            self.rows[*i].order = order;
                        }
                        self.changes_made = true;
                    }
                    if d.changed() {
                        for i in &selected {
                            self.rows[*i].delay = delay;
                        }
                        self.changes_made = true;
                    }
                });
                ui.end_row();
            });
    }

    /// `HaNtolIndicator` and the status line of `AssignPriorities`.
    fn ntol_ui(&mut self, ui: &mut Ui, enabled: bool) {
        let h = ha_strings();
        ui.group(|ui| {
            ui.strong(h.ntol_group);
            ui.label(h.ntol_blurb);
            let calculating = self.calculating();
            ui.horizontal(|ui| {
                ui.label(h.ntol_label);
                let known = matches!(self.max, Some(Ok(_)));
                let mut v = self.ntol.max(0);
                let r = ui.add_enabled(
                    enabled && known && !calculating,
                    DragValue::new(&mut v).range(0..=i64::from(u16::MAX)),
                );
                if r.changed() {
                    self.ntol = v;
                }
                if let Some(Ok(max)) = &self.max {
                    ui.label(fill(h.ntol_max, &[("max", &max.to_string())]));
                }
                if calculating {
                    ui.spinner();
                    ui.label(h.ntol_calculating);
                }
            });
            if self.overcommitted() {
                ui.colored_label(ui.visuals().warn_fg_color, h.ntol_status);
            }
        });
        if self.calculating() {
            return;
        }
        let red = crate::theme::tone(ui, crate::theme::Tone::Error);
        match &self.max {
            Some(Err(_)) => {
                ui.colored_label(red, h.unable_to_calculate);
            }
            Some(Ok(max)) if self.overcommitted() || self.ntol == 0 => {
                let _ = max;
                ui.colored_label(red, h.overcommit_message);
            }
            Some(Ok(max)) => {
                ui.label(fill(h.guaranteed_message, &[("max", &max.to_string())]));
            }
            None => {}
        }
    }
}

// ---- prechecks ----------------------------------------------------------------------

/// What a pending precheck is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Srs,
    Agility,
    Planner,
}

#[derive(Default)]
struct IdSource {
    next: u64,
    pending: HashMap<u64, Pending>,
}

impl IdSource {
    fn next(&mut self, what: Pending) -> u64 {
        self.next += 1;
        let id = (1 << 52) + self.next;
        self.pending.insert(id, what);
        id
    }
}

// ---- the wizard ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Intro,
    Rbac,
    Sr,
    Priorities,
    Finish,
}

/// One SR of the heartbeat page (`SRWrapper`).
struct SrRow {
    sr: Ref<Sr>,
    name: String,
    description: String,
    comment: String,
    enabled: bool,
}

/// The Configure HA wizard (`HAWizard`).
pub struct HaWizard {
    connection: ConnectionId,
    page: Page,
    /// `Helpers.ConnectionRequiresRbac`: the RBAC page is shown.
    rbac_needed: bool,
    /// The blocking RBAC check failed: why.
    rbac_blocked: Option<String>,
    /// `None` while scanning.
    srs: Option<Vec<SrRow>>,
    scan: Option<u64>,
    sr: Option<Ref<Sr>>,
    priorities: Option<Priorities>,
}

impl HaWizard {
    fn open(conn: &ConnectionView) -> Self {
        let rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), ha::WIZARD_RBAC) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                ha_strings().rbac_enable_warning,
                &[("role", &i18n::session_roles_text(conn))],
            )),
        };
        Self {
            connection: conn.id,
            page: Page::Intro,
            rbac_needed,
            rbac_blocked,
            srs: None,
            scan: None,
            sr: None,
            priorities: None,
        }
    }

    fn pages(&self) -> Vec<Page> {
        let mut p = vec![Page::Intro];
        if self.rbac_needed {
            p.push(Page::Rbac);
        }
        p.extend([Page::Sr, Page::Priorities, Page::Finish]);
        p
    }

    fn page_name(page: Page) -> &'static str {
        let h = ha_strings();
        match page {
            Page::Intro => h.page_intro,
            Page::Rbac => h.page_rbac,
            Page::Sr => h.page_sr,
            Page::Priorities => h.page_priorities,
            Page::Finish => h.page_finish,
        }
    }

    fn page_ok(&self) -> bool {
        match self.page {
            Page::Intro => true,
            Page::Rbac => self.rbac_blocked.is_none(),
            Page::Sr => self.sr.is_some(),
            Page::Priorities => self.priorities.as_ref().is_some_and(Priorities::ok),
            Page::Finish => true,
        }
    }

    /// `HAWizard.RunNextPagePrecheck`: the heartbeat SRs are scanned when
    /// leaving the introduction (or the RBAC page).
    fn scan(&mut self, conn: &ConnectionView, backend: &BackendHandle, ids: &mut IdSource) {
        let id = ids.next(Pending::Srs);
        self.scan = Some(id);
        self.srs = None;
        backend.send(Command::Precheck {
            connection: conn.id,
            id,
            check: Precheck::HeartbeatSrs {
                srs: ha::heartbeat_candidates(&conn.model),
            },
        });
    }

    fn on_srs(
        &mut self,
        conn: &ConnectionView,
        id: u64,
        result: &[(Ref<Sr>, Option<std::sync::Arc<xapi::Error>>)],
    ) -> bool {
        if self.scan != Some(id) {
            return false;
        }
        self.scan = None;
        let h = ha_strings();
        let mut rows: Vec<SrRow> = result
            .iter()
            .filter_map(|(r, e)| {
                let sr = conn.model.srs.get(r)?;
                let comment = match e.as_deref() {
                    None => String::new(),
                    Some(e) if e.is_api("SR_HAS_NO_PBDS") => h.sr_detached.to_owned(),
                    Some(e) => i18n::describe_xapi_error_in(Some(conn), e)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                };
                Some(SrRow {
                    sr: r.clone(),
                    name: sr.name_label.clone(),
                    description: sr.name_description.clone(),
                    comment,
                    enabled: e.is_none(),
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then_with(|| xen_core::objects::natural_cmp(&a.name, &b.name))
        });
        if let Some(sel) = &self.sr
            && !rows.iter().any(|r| &r.sr == sel && r.enabled)
        {
            self.sr = None;
        }
        self.srs = Some(rows);
        true
    }

    fn next(
        &mut self,
        conn: &ConnectionView,
        show_hidden: bool,
        backend: &BackendHandle,
        ids: &mut IdSource,
    ) -> Option<ActionRequest> {
        let pages = self.pages();
        let i = pages.iter().position(|p| *p == self.page)?;
        if self.page == Page::Finish {
            let p = self.priorities.as_ref()?;
            return Some(ActionRequest::EnableHa {
                heartbeat_sr: self.sr.clone()?,
                ntol: p.ntol,
                startup: p.changed(conn),
            });
        }
        if matches!(self.page, Page::Intro | Page::Rbac) && pages[i + 1] == Page::Sr {
            self.scan(conn, backend, ids);
        }
        self.page = pages[i + 1];
        if self.page == Page::Priorities && self.priorities.is_none() {
            self.priorities = Some(Priorities::new(conn, true, show_hidden));
        }
        None
    }

    fn previous(&mut self) {
        let pages = self.pages();
        if let Some(i) = pages.iter().position(|p| *p == self.page)
            && i > 0
        {
            self.page = pages[i - 1];
        }
    }

    fn sr_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let h = ha_strings();
        ui.heading(h.page_sr_title);
        ui.label(h.sr_blurb);
        ui.add_space(6.0);
        let Some(rows) = &self.srs else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(fill(h.scanning_srs, &[("pool", &connection_name(conn))]));
            });
            return;
        };
        if rows.is_empty() {
            ui.label(RichText::new(h.no_shared_srs).italics());
            return;
        }
        Grid::new("ha-srs")
            .num_columns(3)
            .striped(true)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.strong(h.col_sr);
                ui.strong(h.col_description);
                ui.strong(h.col_comment);
                ui.end_row();
                for row in rows {
                    ui.horizontal(|ui| {
                        if let Some(sr) = conn.model.srs.get(&row.sr) {
                            crate::icons::sr(
                                ui,
                                xen_core::objects::sr_icon(&conn.model, &row.sr, sr),
                            );
                        }
                        let selected = self.sr.as_ref() == Some(&row.sr);
                        if ui
                            .add_enabled(row.enabled, Button::selectable(selected, &row.name))
                            .clicked()
                        {
                            self.sr = Some(row.sr.clone());
                        }
                    });
                    ui.label(&row.description);
                    ui.label(&row.comment);
                    ui.end_row();
                }
            });
    }

    fn finish_page(&self, ui: &mut Ui, conn: &ConnectionView) {
        let h = ha_strings();
        ui.heading(h.page_finish_title);
        ui.label(h.finish_blurb);
        ui.add_space(6.0);
        let Some(p) = &self.priorities else { return };
        let counts = ha::priority_counts(
            p.rows
                .iter()
                .map(|r| r.priority.unwrap_or(RestartPriority::BestEffort)),
        );
        let sr_name = self
            .sr
            .as_ref()
            .and_then(|r| conn.model.srs.get(r))
            .map(|s| s.name_label.clone())
            .unwrap_or_default();
        ui.group(|ui| {
            ui.strong(h.finish_summary);
            Grid::new("ha-finish")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        (h.finish_sr, sr_name),
                        (h.finish_ntol, p.ntol.to_string()),
                        (h.finish_restart, vm_count(counts[1])),
                        (h.finish_best_effort, vm_count(counts[2])),
                        (h.finish_do_not_restart, vm_count(counts[3])),
                    ] {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
        });
        let warn = ui.visuals().warn_fg_color;
        if counts[0] + counts[1] + counts[2] == 0 && counts[3] > 0 {
            ui.colored_label(warn, h.finish_no_vms_protected);
        }
        if p.ntol == 0 {
            ui.colored_label(warn, h.finish_no_ha_guaranteed);
        }
    }

    /// Draws the wizard; returns the request to run, or `Err(())` once
    /// cancelled.
    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
        ids: &mut IdSource,
    ) -> Result<Option<ActionRequest>, ()> {
        let h = ha_strings();
        let n = crate::net_strings::net_strings();
        let Some(conn) = state.connection(self.connection) else {
            return Err(());
        };
        let show_hidden = state.settings.preferences.show_hidden_objects;
        if let Some(p) = self.priorities.as_mut()
            && self.page == Page::Priorities
        {
            p.ask(conn, backend, ids);
        }
        let mut open = true;
        let mut outcome = Ok(None);
        dialog_window(h.wizard_title)
            .id(egui::Id::unique("ha-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([780.0, 470.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(120.0);
                        for page in self.pages() {
                            let t = RichText::new(Self::page_name(page));
                            ui.label(if page == self.page {
                                t.strong()
                            } else {
                                t.weak()
                            });
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(620.0);
                        egui::ScrollArea::vertical()
                            .id_salt("ha-wizard-page")
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                Page::Intro => {
                                    ui.label(RichText::new(h.page_intro_title).weak());
                                    ui.heading(h.intro_heading);
                                    ui.label(h.intro_blurb);
                                }
                                Page::Rbac => {
                                    ui.heading(h.page_rbac);
                                    ui.label(h.rbac_description);
                                    ui.add_space(6.0);
                                    match &self.rbac_blocked {
                                        Some(why) => {
                                            ui.colored_label(ui.visuals().error_fg_color, why);
                                        }
                                        None => {
                                            ui.label(h.rbac_click_next);
                                        }
                                    }
                                }
                                Page::Sr => self.sr_page(ui, conn),
                                Page::Priorities => {
                                    ui.heading(h.page_priorities_title);
                                    if let Some(p) = self.priorities.as_mut() {
                                        p.ui(ui, conn, true);
                                    }
                                }
                                Page::Finish => self.finish_page(ui, conn),
                            });
                    });
                });
                ui.add_space(8.0);
                dialog_buttons(ui, |row| {
                    if row
                        .add_enabled(
                            ButtonRole::Back,
                            self.page != Page::Intro,
                            Button::new(n.previous),
                        )
                        .clicked()
                    {
                        self.previous();
                    }
                    let last = self.page == Page::Finish;
                    if row
                        .add_enabled(
                            ButtonRole::Primary,
                            self.page_ok(),
                            Button::new(if last { n.finish } else { n.next }),
                        )
                        .clicked()
                        && let Some(r) = self.next(conn, show_hidden, backend, ids)
                    {
                        outcome = Ok(Some(r));
                    }
                    if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                        outcome = Err(());
                    }
                });
            });
        if !open {
            return Err(());
        }
        outcome
    }
}

// ---- the HA settings dialog -----------------------------------------------------------

/// `EditVmHaPrioritiesDialog`.
pub struct EditHaDialog {
    connection: ConnectionId,
    priorities: Priorities,
    original_ntol: i64,
    /// The "failure tolerance would be 0" question is showing.
    ask_zero: bool,
    /// OK was pressed: the dialog closes.
    closing: bool,
}

impl EditHaDialog {
    /// Draws the dialog; returns what to run, `Err(())` when closed.
    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
        ids: &mut IdSource,
        errors: &mut Vec<String>,
    ) -> Result<Option<ActionRequest>, ()> {
        let h = ha_strings();
        let Some(conn) = state.connection(self.connection) else {
            return Err(());
        };
        let pool_name = connection_name(conn);
        // `pool_PropertyChanged`: HA turned off while the dialog is open.
        if !conn.model.pool().is_some_and(|(_, p)| p.ha_enabled) {
            errors.push(fill(h.ha_was_disabled, &[("pool", &pool_name)]));
            return Err(());
        }
        let dead = !ha::dead_hosts(&conn.model).is_empty();
        if !dead {
            self.priorities.ask(conn, backend, ids);
        }
        let mut result = Ok(None);
        let mut open = true;
        dialog_window(fill(h.edit_title, &[("pool", &pool_name)]))
            .id(egui::Id::unique("ha-edit"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([640.0, 430.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                if dead {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        fill(h.edit_dead_hosts, &[("pool", &pool_name)]),
                    );
                }
                self.priorities.ui(ui, conn, !dead);
                ui.separator();
                if self.ask_zero {
                    ui.label(h.ntol_zero_query);
                    dialog_buttons(ui, |row| {
                        if row
                            .button(ButtonRole::Primary, crate::ops_strings::ops_strings().yes)
                            .clicked()
                        {
                            self.ask_zero = false;
                            result = Ok(self.save(conn));
                        }
                        if row
                            .button(ButtonRole::Cancel, crate::ops_strings::ops_strings().no)
                            .clicked()
                        {
                            self.ask_zero = false;
                        }
                    });
                    return;
                }
                dialog_buttons(ui, |row| {
                    if row
                        .add_enabled(
                            ButtonRole::Primary,
                            !dead && self.priorities.ok(),
                            Button::new(h.ok),
                        )
                        .clicked()
                    {
                        if self.priorities.ntol == 0 {
                            self.ask_zero = true;
                        } else {
                            result = Ok(self.save(conn));
                        }
                    }
                    if row.button(ButtonRole::Cancel, h.cancel).clicked() {
                        result = Err(());
                    }
                });
            });
        if !open {
            return Err(());
        }
        // A save closes the dialog, with or without a request.
        match result {
            Ok(Some(r)) => Ok(Some(r)),
            Ok(None) if self.closing => Err(()),
            other => other,
        }
    }

    /// `buttonOk_Click`: runs only when something changed.
    fn save(&mut self, conn: &ConnectionView) -> Option<ActionRequest> {
        self.closing = true;
        let p = &self.priorities;
        (p.changes_made || p.ntol != self.original_ntol).then(|| ActionRequest::SetHaPriorities {
            startup: p.changed(conn),
            ntol: p.ntol,
        })
    }
}

// ---- the window's HA state ---------------------------------------------------------------

/// The HA and AD state of the window.
#[derive(Default)]
pub struct HaUi {
    wizard: Option<HaWizard>,
    edit: Option<EditHaDialog>,
    ids: IdSource,
    /// When HA was seen turned on, per connection (the tab's heartbeat
    /// initialisation delay).
    ha_since: HashMap<ConnectionId, Instant>,
    /// Warnings shown before a dialog (broken SRs).
    notices: VecDeque<String>,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
    /// The Users tab and its dialogs.
    pub ad: crate::ad_ui::AdUi,
}

impl HaUi {
    /// Opens what `request` asks for.
    pub fn open(
        &mut self,
        conn: &ConnectionView,
        request: DialogRequest,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        show_hidden: bool,
    ) {
        let h = ha_strings();
        let Some((_, pool)) = conn.model.pool() else {
            return;
        };
        let name = connection_name(conn);
        match request {
            DialogRequest::ConfigureHa if pool.ha_enabled => {
                if ha::statefiles_missing(&conn.model, pool) {
                    self.errors
                        .push(fill(h.configure_no_statefile, &[("pool", &name)]));
                } else if let RbacCheck::Denied { authorized } =
                    rbac::check(&conn.model, conn.rbac.as_deref(), ha::CONFIGURE_RBAC)
                {
                    let roles = authorized
                        .iter()
                        .map(|r| i18n::role_name(&r.name_label))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.errors.push(fill(
                        h.rbac_configure_warning,
                        &[
                            ("roles", &roles),
                            ("current", &i18n::session_roles_text(conn)),
                        ],
                    ));
                } else {
                    self.edit = Some(EditHaDialog {
                        connection: conn.id,
                        priorities: Priorities::new(conn, false, show_hidden),
                        original_ntol: pool.ha_host_failures_to_tolerate,
                        ask_zero: false,
                        closing: false,
                    });
                }
            }
            DialogRequest::ConfigureHa => {
                let broken = ha::broken_shared_srs(&conn.model);
                if !broken.is_empty() {
                    self.notices
                        .push_back(fill(h.srs_broken_warning, &[("srs", &broken.join("\n"))]));
                }
                self.wizard = Some(HaWizard::open(conn));
            }
            DialogRequest::DisableHa => {
                if ha::statefiles_missing(&conn.model, pool) {
                    self.errors
                        .push(fill(h.disable_no_statefile, &[("pool", &name)]));
                } else {
                    ops.request(conn, ActionRequest::DisableHa, backend);
                }
            }
            _ => {}
        }
    }

    /// An answer to a precheck this part asked.
    pub fn on_precheck(&mut self, state: &AppState, id: u64, result: &PrecheckResult) -> bool {
        let Some(what) = self.ids.pending.remove(&id) else {
            return self.ad.on_precheck(id, result);
        };
        let pool_of = |c: ConnectionId| {
            state
                .connection(c)
                .and_then(|v| v.model.pool().map(|(_, p)| (**p).clone()))
        };
        let mut lists: Vec<(ConnectionId, &mut Priorities)> = Vec::new();
        if let Some(w) = self.wizard.as_mut()
            && let Some(p) = w.priorities.as_mut()
        {
            lists.push((w.connection, p));
        }
        if let Some(e) = self.edit.as_mut() {
            lists.push((e.connection, &mut e.priorities));
        }
        match (what, result) {
            (Pending::Srs, PrecheckResult::HeartbeatSrs(r)) => {
                if let Some(w) = self.wizard.as_mut()
                    && let Some(conn) = state.connection(w.connection)
                {
                    w.on_srs(conn, id, r);
                }
            }
            (Pending::Agility, PrecheckResult::Agility(r)) => {
                for (_, p) in lists {
                    p.on_agility(id, r);
                }
            }
            (Pending::Planner, PrecheckResult::HaMaxFailures(r)) => {
                for (c, p) in lists {
                    p.on_planner(id, r, pool_of(c).as_ref());
                }
            }
            _ => {}
        }
        true
    }

    /// Draws the wizard and dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(text) = self.notices.front().cloned() {
            let mut ok = false;
            window(ha_strings().high_availability.to_owned()).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(text);
                ui.add_space(8.0);
                dialog_buttons(ui, |row| {
                    ok = row.button(ButtonRole::Primary, ha_strings().ok).clicked();
                });
            });
            if ok {
                self.notices.pop_front();
            }
            return;
        }
        if let Some(w) = self.wizard.as_mut() {
            match w.show(ctx, state, backend, &mut self.ids) {
                Err(()) => self.wizard = None,
                Ok(Some(request)) => {
                    let c = w.connection;
                    self.wizard = None;
                    if let Some(conn) = state.connection(c) {
                        ops.dispatch(conn, request, backend);
                    }
                }
                Ok(None) => {}
            }
        }
        if let Some(d) = self.edit.as_mut() {
            match d.show(ctx, state, backend, &mut self.ids, &mut self.errors) {
                Err(()) => self.edit = None,
                Ok(Some(request)) => {
                    let c = d.connection;
                    self.edit = None;
                    if let Some(conn) = state.connection(c) {
                        ops.dispatch(conn, request, backend);
                    }
                }
                Ok(None) => {}
            }
        }
        self.ad.windows(ctx, state, ops, backend);
        self.errors.append(&mut self.ad.errors);
    }

    /// The pool's HA tab (`HAPage`).
    pub fn ha_tab(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        show_hidden: bool,
    ) {
        let h = ha_strings();
        let Some((_, pool)) = conn.model.pool() else {
            return;
        };
        let name = connection_name(conn);
        ui.label(RichText::new(h.high_availability).strong().size(15.0));
        ui.add_space(4.0);
        let busy = client_busy(state, conn.id).or_else(|| ha::busy(pool));
        if let Some(enabling) = busy {
            ui.label(fill(
                if enabling {
                    h.ha_page_enabling
                } else {
                    h.ha_page_disabling
                },
                &[("pool", &name)],
            ));
            return;
        }
        if !conn.status.is_connected() {
            return;
        }
        ui.label(fill(
            if pool.ha_enabled {
                h.ha_tab_configured_blurb
            } else {
                h.ha_panel_blurb
            },
            &[("pool", &name)],
        ));
        ui.add_space(6.0);
        let configure = ha::configure_problem(&conn.model, client_busy(state, conn.id));
        let disable = ha::disable_problem(&conn.model, client_busy(state, conn.id));
        let mut open = None;
        ui.horizontal(|ui| {
            let r = ui.add_enabled(configure.is_none(), Button::new(h.configure_ha));
            if let Some(p) = &configure {
                r.clone().on_disabled_hover_text(problem_text(conn, p));
            }
            if r.clicked() {
                open = Some(DialogRequest::ConfigureHa);
            }
            if pool.ha_enabled
                && ui
                    .add_enabled(disable.is_none(), Button::new(h.disable_ha))
                    .clicked()
            {
                open = Some(DialogRequest::DisableHa);
            }
        });
        if let Some(request) = open {
            self.open(conn, request, ops, backend, show_hidden);
        }
        if !pool.ha_enabled {
            self.ha_since.remove(&conn.id);
            return;
        }
        let since = *self.ha_since.entry(conn.id).or_insert_with(Instant::now);
        let initialized = since.elapsed() >= HEARTBEAT_DELAY;
        if !initialized {
            ui.ctx()
                .request_repaint_after(HEARTBEAT_DELAY.saturating_sub(since.elapsed()));
        }
        ui.add_space(10.0);
        let red = crate::theme::tone(ui, crate::theme::Tone::Error);
        ui.label(RichText::new(h.configuration_title).strong());
        Grid::new("ha-config")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label(format!("{}: ", h.pool_ha_enabled));
                ui.label(crate::ops_strings::ops_strings().yes);
                ui.end_row();
                ui.label(h.configured_capacity);
                let ntol = pool.ha_host_failures_to_tolerate;
                if ntol > 0 {
                    ui.label(ntol.to_string());
                } else {
                    ui.colored_label(red, RichText::new(h.ntol_zero_hapage).strong());
                }
                ui.end_row();
                ui.label(h.current_capacity);
                let plan = pool.ha_plan_exists_for;
                let text = if plan == ntol {
                    plan.to_string()
                } else {
                    fill(h.overcommitted_ntol, &[("n", &plan.to_string())])
                };
                if ha::overcommitted(pool) {
                    ui.colored_label(red, RichText::new(text).strong());
                } else {
                    ui.label(text);
                }
                ui.end_row();
            });
        ui.add_space(10.0);
        ui.label(RichText::new(h.heartbeating_status).strong());
        let srs = ha::heartbeat_srs(&conn.model, pool);
        let green = crate::theme::tone(ui, crate::theme::Tone::Ok);
        Grid::new("ha-heartbeats")
            .num_columns(2 + srs.len())
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label("");
                ui.strong(h.network);
                for (_, n) in &srs {
                    ui.strong(n);
                }
                ui.end_row();
                for row in ha::heartbeat_rows(&conn.model, pool, initialized) {
                    ui.label(&row.name);
                    for (i, beat) in std::iter::once(row.network)
                        .chain(row.srs.iter().copied())
                        .enumerate()
                    {
                        let (text, healthy) = match beat {
                            ha::Heartbeat::Healthy => (h.heartbeat_healthy.to_owned(), true),
                            ha::Heartbeat::Unhealthy => (h.heartbeat_unhealthy.to_owned(), false),
                            ha::Heartbeat::Some { seen, of } => (
                                fill(
                                    h.heartbeat_servers,
                                    &[("n", &seen.to_string()), ("m", &of.to_string())],
                                ),
                                false,
                            ),
                            ha::Heartbeat::Initializing => {
                                (h.heartbeat_initialising.to_owned(), false)
                            }
                        };
                        // The network column is green only once initialised.
                        let _ = i;
                        if healthy {
                            ui.colored_label(green, text);
                        } else {
                            ui.label(text);
                        }
                    }
                    ui.end_row();
                }
            });
    }

    /// The wizard is open (tests).
    #[cfg(test)]
    pub fn wizard_open(&self) -> bool {
        self.wizard.is_some()
    }

    /// The settings dialog is open (tests).
    #[cfg(test)]
    pub fn edit_open(&self) -> bool {
        self.edit.is_some()
    }

    /// The wizard's heartbeat SRs once scanned: name, usable, comment
    /// (tests).
    #[cfg(test)]
    pub fn wizard_srs(&self) -> Option<Vec<(String, bool, String)>> {
        let rows = self.wizard.as_ref()?.srs.as_ref()?;
        Some(
            rows.iter()
                .map(|r| (r.name.clone(), r.enabled, r.comment.clone()))
                .collect(),
        )
    }

    /// The wizard's priorities page: VMs, agile, not agile, still
    /// calculating, the planner's answer and the ntol (tests).
    #[cfg(test)]
    pub fn wizard_plan(&self) -> Option<WizardPlan> {
        let p = self.wizard.as_ref()?.priorities.as_ref()?;
        let count = |f: fn(&Row) -> bool| p.rows.iter().filter(|r| f(r)).count();
        Some(WizardPlan {
            vms: p.rows.len(),
            agile: count(|r| matches!(r.agile, Some(None))),
            not_agile: count(|r| matches!(r.agile, Some(Some(_)))),
            calculating: count(|r| r.agile.is_none()),
            max: p.max.clone(),
            ntol: p.ntol,
        })
    }
}

/// What the wizard's priorities page shows (tests).
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct WizardPlan {
    pub vms: usize,
    pub agile: usize,
    pub not_agile: usize,
    pub calculating: usize,
    pub max: Option<Result<i64, String>>,
    pub ntol: i64,
}

#[cfg(test)]
impl HaUi {
    /// A dialog of the HA part is open (tests).
    pub fn any_open(&self) -> bool {
        self.wizard.is_some() || self.edit.is_some() || !self.notices.is_empty()
    }
}
