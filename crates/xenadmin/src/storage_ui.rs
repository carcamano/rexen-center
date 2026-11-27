//! Storage repositories in the window: the Storage menu and the SR
//! context menu (`MainWindow`'s Storage menu, `ContextMenuBuilder` for an
//! SR), New SR on the pool and host menus, the Repair Storage Repository
//! dialog (`RepairSRDialog`), the SR Properties dialog (General, Alerts,
//! Read Caching pages of `PropertiesDialog`) and the SR Storage tab's
//! Rescan button. The New SR wizard is [`crate::new_sr`].
//!
//! Every operation ends in an [`ActionRequest`] sent through
//! [`OpsUi::request`] or [`OpsUi::dispatch`], so the confirmations, the
//! RBAC check and role elevation of the other operations apply.

use egui::{Align2, Button, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Sr, SrRecord};
use xen_core::objects;
use xen_core::operations::PrecheckResult;
use xen_core::storage::{self, SrAction, SrChanges, SrOp, SrOpProblem};
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionView, PerfmonObjectRef,
};

use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::fill;
use crate::new_sr::{NewSrWizard, WizardOutcome};
use crate::ops_ui::{Entry, OpsUi};
use crate::perfmon_ui::{self, PerfmonDialog};
use crate::provision_ui::DialogRequest;
use crate::sr_strings::sr_strings;
use crate::window_frame::{DialogWindow, dialog_window};

/// "New SR..." for a pool or host (`NewSRCommand`).
pub fn new_sr_entry(conn: &ConnectionView) -> Entry {
    Entry::Dialog {
        label: sr_strings().new_sr,
        enabled: conn.status.is_connected() && conn.model.pool().is_some(),
        dialog: DialogRequest::NewSr { reattach: None },
    }
}

/// The SR menu, in the original's order.
pub fn sr_entries(conn: &ConnectionView, sr_ref: &Ref<Sr>, sr: &SrRecord) -> Vec<Entry> {
    let s = sr_strings();
    let usable = conn.status.is_connected();
    let busy = conn.busy_srs.contains(sr_ref);
    let available = |op| usable && storage::sr_op_available(&conn.model, sr_ref, sr, op, busy);
    let item = |label, action: SrAction| Entry::Item {
        label,
        enabled: available(action.op()),
        request: ActionRequest::Sr {
            sr: sr_ref.clone(),
            action,
        },
    };
    let dialog = |label, op, dialog| Entry::Dialog {
        label,
        enabled: available(op),
        dialog,
    };
    vec![
        dialog(
            s.repair,
            SrOp::Repair,
            DialogRequest::RepairSr(sr_ref.clone()),
        ),
        item(s.set_default, SrAction::SetDefault),
        Entry::Separator,
        item(s.trim, SrAction::Trim),
        Entry::Separator,
        item(s.detach, SrAction::Detach),
        dialog(
            s.reattach,
            SrOp::Reattach,
            DialogRequest::NewSr {
                reattach: Some(sr_ref.clone()),
            },
        ),
        item(s.forget, SrAction::Forget),
        item(s.destroy, SrAction::Destroy),
        Entry::Separator,
        dialog(
            s.properties,
            SrOp::Properties,
            DialogRequest::SrProperties(sr_ref.clone()),
        ),
    ]
}

/// The original's tooltip on a disabled SR operation, if it says one.
pub fn problem_text(problem: SrOpProblem) -> String {
    let s = sr_strings();
    match problem {
        SrOpProblem::Detached => s.problem_detached.to_owned(),
        SrOpProblem::RunningVms => s.problem_running_vms.to_owned(),
        SrOpProblem::NotCreatedByConsole => fill(
            s.problem_not_by_console,
            &[("console", branding::PRODUCT_NAME)],
        ),
        SrOpProblem::InProgress => s.problem_in_progress.to_owned(),
        SrOpProblem::TrimUnsupported => s.problem_trim_unsupported.to_owned(),
        SrOpProblem::ScanInProgress => s.problem_scan_in_progress.to_owned(),
    }
}

/// A host's line in the Repair dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairRow {
    pub host: String,
    pub status: &'static str,
    pub ok: bool,
}

/// The hosts of `sr` and the state of their PBD (`RepairSRDialog.Build`).
pub fn repair_rows(conn: &ConnectionView, sr: &SrRecord) -> Vec<RepairRow> {
    let s = sr_strings();
    let m = &conn.model;
    storage::repair_hosts(m, sr)
        .into_iter()
        .map(|h| {
            let host = m
                .hosts
                .get(h)
                .map(|r| r.name_label.clone())
                .unwrap_or_default();
            let (status, ok) = match storage::pbd_of(m, sr, h) {
                None => (s.pbd_missing, false),
                Some((_, pbd)) if pbd.currently_attached => (s.pbd_connected, true),
                Some(_) => (s.pbd_unplugged, false),
            };
            RepairRow { host, status, ok }
        })
        .collect()
}

/// The Properties dialog's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertiesTab {
    General,
    CustomFields,
    Alerts,
    ReadCaching,
}

/// The SR Properties dialog.
pub struct PropertiesDialog {
    sr: Ref<Sr>,
    pub tab: PropertiesTab,
    pub name: String,
    pub description: String,
    /// Comma-separated.
    pub tags: String,
    /// `None` when the SR does not support read caching.
    pub read_caching: Option<bool>,
    original: (String, String, Vec<String>, Option<bool>),
    pub perfmon: Option<PerfmonDialog>,
    /// The General tab's folder and the Custom Fields tab (F13).
    pub org: crate::org_ui::OrgEditor,
    pub org_dialog: Option<crate::org_ui::OrgDialog>,
}

impl PropertiesDialog {
    fn new(conn: &ConnectionView, sr: Ref<Sr>) -> Option<Self> {
        let rec = conn.model.srs.get(&sr)?;
        let read_caching = storage::supports_read_caching(&conn.model, rec)
            .then(|| storage::read_caching_enabled(&conn.model, rec));
        Some(Self {
            tab: PropertiesTab::General,
            name: rec.name_label.clone(),
            description: rec.name_description.clone(),
            tags: rec.tags.join(", "),
            read_caching,
            original: (
                rec.name_label.clone(),
                rec.name_description.clone(),
                rec.tags.clone(),
                read_caching,
            ),
            perfmon: PerfmonDialog::open(conn, PerfmonObjectRef::Sr(sr.clone())),
            org: {
                let mut e =
                    crate::org_ui::OrgEditor::new(conn, xen_core::org::OrgObject::Sr(sr.clone()));
                e.edit_tags = false;
                e
            },
            org_dialog: None,
            sr,
        })
    }

    /// What changed.
    pub fn changes(&self) -> SrChanges {
        let tags: Vec<String> = self
            .tags
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let (name, description, old_tags, caching) = &self.original;
        SrChanges {
            name: (self.name.trim() != name && !self.name.trim().is_empty())
                .then(|| self.name.trim().to_owned()),
            description: (&self.description != description).then(|| self.description.clone()),
            tags: (&tags != old_tags).then_some(tags),
            read_caching: self.read_caching.filter(|v| Some(*v) != *caching),
        }
    }
}

/// The Repair dialog.
pub struct RepairDialog {
    sr: Ref<Sr>,
    /// Repair was pressed.
    pub started: bool,
}

enum Dialog {
    Repair(RepairDialog),
    Properties(Box<PropertiesDialog>),
}

/// The storage part of the window.
pub struct StorageUi {
    /// The New SR wizard, when open.
    pub wizard: Option<NewSrWizard>,
    dialog: Option<(ConnectionId, Dialog)>,
    /// Numbers the wizard's prechecks (above the operation menus' ids).
    next_id: u64,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
}

impl Default for StorageUi {
    fn default() -> Self {
        Self {
            wizard: None,
            dialog: None,
            next_id: 1 << 40,
            errors: Vec::new(),
        }
    }
}

impl StorageUi {
    /// The open SR Properties dialog (tests).
    #[cfg(test)]
    pub fn sr_properties_mut(&mut self) -> Option<&mut PropertiesDialog> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Properties(d))) => Some(d),
            _ => None,
        }
    }

    /// A custom field dialog the SR Properties dialog asked for.
    pub fn take_org_dialog(&mut self) -> Option<crate::org_ui::OrgDialog> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Properties(d))) => d.org_dialog.take(),
            _ => None,
        }
    }
}

impl StorageUi {
    /// Opens the dialog `request` asks for.
    pub fn open(&mut self, conn: &ConnectionView, request: DialogRequest) {
        match request {
            DialogRequest::NewSr { reattach } => {
                // `RunFinalAction`: an SR with PBDs is in use.
                if let Some(sr) = reattach.as_ref().and_then(|r| conn.model.srs.get(r))
                    && storage::has_pbds(sr)
                {
                    let pool = conn
                        .model
                        .pool()
                        .map(|(_, p)| objects::pool_name(&conn.model, p))
                        .unwrap_or_default();
                    self.errors.push(fill(
                        sr_strings().err_sr_in_use,
                        &[("name", &sr.name_label), ("pool", &pool)],
                    ));
                    return;
                }
                self.wizard = Some(NewSrWizard::new(conn, reattach));
            }
            DialogRequest::RepairSr(sr) => {
                self.dialog = Some((conn.id, Dialog::Repair(RepairDialog { sr, started: false })));
            }
            DialogRequest::SrProperties(sr) => {
                if let Some(d) = PropertiesDialog::new(conn, sr) {
                    self.dialog = Some((conn.id, Dialog::Properties(Box::new(d))));
                }
            }
            _ => {}
        }
    }

    /// An answer to a precheck this part asked.
    pub fn on_precheck(&mut self, state: &AppState, id: u64, result: PrecheckResult) {
        if let Some(wizard) = self.wizard.as_mut()
            && wizard.waits_for(id)
        {
            let model = state
                .connection(wizard.connection_id())
                .map(|c| c.model.clone())
                .unwrap_or_default();
            wizard.on_precheck(id, result, &model);
        }
    }

    /// The open Properties dialog (tests).
    #[cfg(test)]
    pub fn properties(&mut self) -> Option<&mut PropertiesDialog> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Properties(d))) => Some(d),
            _ => None,
        }
    }

    /// Draws the wizard and the open dialog.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(wizard) = self.wizard.as_mut() {
            match wizard.show(ctx, state, backend, &mut self.next_id) {
                WizardOutcome::Open => {}
                WizardOutcome::Cancelled => self.wizard = None,
                WizardOutcome::Finish(connection, spec) => {
                    self.wizard = None;
                    if let Some(conn) = state.connection(connection) {
                        ops.dispatch(conn, ActionRequest::NewSr(spec), backend);
                    }
                }
            }
        }
        let Some((connection, dialog)) = self.dialog.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(*connection) else {
            self.dialog = None;
            return;
        };
        let close = match dialog {
            Dialog::Repair(d) => repair_window(ctx, conn, d, ops, backend),
            Dialog::Properties(d) => properties_window(ctx, conn, d, ops, backend),
        };
        if close {
            self.dialog = None;
        }
    }

    // ---- the SR Storage tab ------------------------------------------------

    /// The SR Storage tab's Rescan button (`SrStoragePage.buttonRescan`).
    pub fn rescan_button(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        sr_ref: &Ref<Sr>,
        sr: &SrRecord,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let s = sr_strings();
        let busy = conn.busy_srs.contains(sr_ref);
        let enabled = conn.status.is_connected()
            && storage::sr_op_available(&conn.model, sr_ref, sr, SrOp::Rescan, busy);
        let mut response = ui.add_enabled(enabled, Button::new(s.rescan));
        if let Some(problem) = storage::sr_op_problem(&conn.model, sr, SrOp::Rescan, busy) {
            response = response.on_disabled_hover_text(problem_text(problem));
        }
        if response.clicked() {
            ops.request(
                conn,
                ActionRequest::Sr {
                    sr: sr_ref.clone(),
                    action: SrAction::Rescan,
                },
                backend,
            );
        }
        if busy {
            ui.spinner();
        }
    }
}

fn window(title: &str) -> DialogWindow<'static> {
    dialog_window(title.to_owned())
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
}

/// Returns whether the dialog closes.
fn repair_window(
    ctx: &Context,
    conn: &ConnectionView,
    d: &mut RepairDialog,
    ops: &mut OpsUi,
    backend: &BackendHandle,
) -> bool {
    let s = sr_strings();
    let Some(sr) = conn.model.srs.get(&d.sr).cloned() else {
        return true;
    };
    let mut close = false;
    let broken =
        objects::sr_is_broken(&conn.model, &sr) || !objects::sr_multipath_ok(&conn.model, &sr);
    let running = conn.busy_srs.contains(&d.sr);
    let rows = repair_rows(conn, &sr);
    window(&fill(s.repair_title, &[("name", &sr.name_label)])).show(ctx, |ui| {
        ui.set_max_width(480.0);
        ui.label(fill(s.repair_blurb, &[("console", branding::PRODUCT_NAME)]));
        if !objects::sr_multipath_ok(&conn.model, &sr) {
            ui.add_space(4.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                fill(s.multipath_failed, &[("product", "XCP-ng")]),
            );
        }
        ui.add_space(6.0);
        ui.label(RichText::new(s.repair_status).strong());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(440.0);
            ui.label(RichText::new(&sr.name_label).strong());
            Grid::new("repair-hosts").num_columns(2).show(ui, |ui| {
                for row in &rows {
                    ui.label(format!("    {}", row.host));
                    let color = if row.ok {
                        crate::theme::tone(ui, crate::theme::Tone::Ok)
                    } else {
                        ui.visuals().error_fg_color
                    };
                    let text = RichText::new(row.status).color(color);
                    ui.label(if row.ok { text } else { text.strong() });
                    ui.end_row();
                }
            });
        });
        ui.add_space(8.0);
        dialog_buttons(ui, |row| {
            if running {
                row.add_leading(egui::Spinner::new());
            }
            let can = broken && !rows.is_empty() && !running && conn.status.is_connected();
            if row
                .add_enabled(ButtonRole::Primary, can, Button::new(s.repair_button))
                .clicked()
            {
                d.started = true;
                ops.dispatch(
                    conn,
                    ActionRequest::Sr {
                        sr: d.sr.clone(),
                        action: SrAction::Repair,
                    },
                    backend,
                );
            }
            let label = if d.started { s.close } else { s.cancel };
            if row.button(ButtonRole::Cancel, label).clicked() {
                close = true;
            }
        });
    });
    close
}

/// Returns whether the dialog closes.
fn properties_window(
    ctx: &Context,
    conn: &ConnectionView,
    d: &mut PropertiesDialog,
    ops: &mut OpsUi,
    backend: &BackendHandle,
) -> bool {
    let s = sr_strings();
    let Some(sr) = conn.model.srs.get(&d.sr).cloned() else {
        return true;
    };
    let mut close = false;
    let mut save = false;
    window(&fill(s.properties_title, &[("name", &sr.name_label)])).show(ctx, |ui| {
        ui.set_min_width(560.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(120.0);
                let mut tabs = vec![
                    (PropertiesTab::General, s.tab_general),
                    (
                        PropertiesTab::CustomFields,
                        crate::org_strings::org_strings().page_custom_fields,
                    ),
                ];
                if d.perfmon.is_some() {
                    tabs.push((PropertiesTab::Alerts, s.tab_alerts));
                }
                if d.read_caching.is_some() {
                    tabs.push((PropertiesTab::ReadCaching, s.tab_read_caching));
                }
                for (tab, label) in tabs {
                    if ui.selectable_label(d.tab == tab, label).clicked() {
                        d.tab = tab;
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_min_width(400.0);
                match d.tab {
                    PropertiesTab::General => {
                        Grid::new("sr-general").num_columns(2).show(ui, |ui| {
                            let label = ui.label(s.name_label);
                            ui.add(TextEdit::singleline(&mut d.name).desired_width(320.0))
                                .labelled_by(label.id);
                            ui.end_row();
                            let label = ui.label(s.description_label);
                            ui.add(
                                TextEdit::multiline(&mut d.description)
                                    .desired_rows(3)
                                    .desired_width(320.0),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                            let label = ui.label(s.tags_label);
                            ui.add(
                                TextEdit::singleline(&mut d.tags)
                                    .hint_text(s.tags_hint)
                                    .desired_width(320.0),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                            d.org.general_rows(ui, conn);
                        });
                    }
                    PropertiesTab::CustomFields => {
                        d.org.custom_fields_page(ui, conn, &mut d.org_dialog);
                    }
                    PropertiesTab::Alerts => {
                        if let Some(p) = d.perfmon.as_mut() {
                            perfmon_ui::form_ui(ui, &mut p.form);
                        }
                    }
                    PropertiesTab::ReadCaching => {
                        ui.label(s.read_caching_blurb);
                        ui.add_space(6.0);
                        if let Some(on) = d.read_caching.as_mut() {
                            ui.checkbox(on, s.read_caching_enable);
                        }
                        ui.add_space(6.0);
                        for text in [
                            s.read_caching_change,
                            s.read_caching_vdis,
                            s.read_caching_memory,
                        ] {
                            ui.label(RichText::new(text).weak());
                        }
                    }
                }
            });
        });
        ui.separator();
        dialog_buttons(ui, |row| {
            if row
                .add_enabled(
                    ButtonRole::Primary,
                    !d.name.trim().is_empty() && d.org.valid(),
                    Button::new(s.ok),
                )
                .clicked()
            {
                save = true;
                close = true;
            }
            if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                close = true;
            }
        });
    });
    if save {
        let changes = d.changes();
        if !changes.is_empty() {
            ops.dispatch(
                conn,
                ActionRequest::SetSrProperties {
                    sr: d.sr.clone(),
                    changes,
                },
                backend,
            );
        }
        if let Some(request) = d.perfmon.as_ref().and_then(PerfmonDialog::request) {
            ops.dispatch(conn, request, backend);
        }
        for request in d.org.requests(conn) {
            ops.request(conn, request, backend);
        }
    }
    close
}
