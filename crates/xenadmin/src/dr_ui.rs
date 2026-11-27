//! Disaster recovery in the window (F12b), as the original's (see
//! [`xen_core::dr`] for the rules):
//!
//! * **Pool > Disaster Recovery > Configure...** (`DRConfigureDialog`): the
//!   pool's SRs that can hold its metadata, ticked when they do; at most 8;
//!   OK turns replication on and off for the SRs whose tick changed.
//! * **Pool > Disaster Recovery > Disaster Recovery Wizard...**
//!   (`DRFailoverWizard`): Permissions (blocking, for a restricted user),
//!   Welcome (Failover, Failback, Test Failover), Before You Start, Locate
//!   Mirrored SRs (the SRs that can hold metadata and those found by
//!   "Find Storage Repositories" on an iSCSI target or the HBAs, with
//!   whether VM configuration data was found; Next introduces the found
//!   ones with a `DR_task` and reads the metadata), Select vApps & VMs
//!   (per pool found, its vApps with their VMs and the VMs not in a vApp;
//!   this pool's own metadata is shown but cannot be chosen; the power
//!   state after the recovery), Pre-checks (grouped, with Resolve, Resolve
//!   All, Check Again and "Hide successful pre-checks"), Progress (one row
//!   per vApp and VM, then "Start VMs and vApps") and Summary (the report).
//!   Leaving the Progress page or cancelling cleans up, in order: the
//!   `DR_task`s are destroyed; after a test failover its VMs and vApps are
//!   deleted and the SRs it introduced detached and forgotten; what the
//!   pre-checks' fixes changed is changed back (HA turned on again).
//!
//! The wizard's operations run one at a time, each in the Events panel.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use egui::{Button, ComboBox, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Sr, Vm, VmAppliance};
use xen_core::dr::{
    self as rules, AfterRecovery, CheckGroup, CheckRow, CheckSubject, Chosen, ConfigureNote,
    DrItem, DrTaskSpec, PoolMetadata, Problem, Report, ScannedSr, StorageRow, Where, WizardType,
};
use xen_core::operations::{DrKind, OperationKind};
use xen_core::rbac::{self, RbacCheck};
use xen_core::storage::{DeviceConfig, IqnInfo, LunInfo, Secret, SrAction};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, OperationStatus,
    Precheck, PrecheckResult, VmAction,
};

use crate::controls::{ButtonRole, dialog_buttons};
use crate::dr_strings::dr_strings;
use crate::i18n::{self, fill};
use crate::ops_ui::{Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::window_frame::dialog_window;
use crate::xva_ui::page_list;

/// Precheck ids of this module.
const ID_BASE: u64 = 1 << 56;

// ---- menu ----------------------------------------------------------------------------------

/// Pool > Disaster Recovery (`DRCommand`): Configure... and Disaster
/// Recovery Wizard....
pub fn menu_entry(conn: &ConnectionView) -> Entry {
    let s = dr_strings();
    let usable = conn.status.is_connected();
    Entry::Submenu {
        label: s.menu.to_owned(),
        enabled: usable,
        entries: vec![
            Entry::Dialog {
                label: s.menu_configure,
                enabled: usable,
                dialog: DialogRequest::ConfigureDr,
            },
            Entry::Dialog {
                label: s.menu_wizard,
                enabled: usable,
                dialog: DialogRequest::DrWizard,
            },
        ],
    }
}

fn now_text() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    xen_core::format::datetime_utc(secs, true)
}

fn sr_type_name(conn: &ConnectionView, sr_type: &str) -> String {
    crate::tabs::general::sr_type_label(&conn.model, sr_type)
}

/// The newest operation after `floor` of `kind` on `conn`.
fn op_after(
    state: &AppState,
    conn: ConnectionId,
    floor: u64,
    pred: impl Fn(&DrKind) -> bool,
) -> Option<&xen_core::Operation> {
    state
        .operations
        .iter()
        .find(|o| {
            o.id > floor
                && o.connection == Some(conn)
                && matches!(&o.kind, OperationKind::Dr(k) if pred(k))
        })
        .map(|o| &**o)
}

fn floor_of(state: &AppState) -> u64 {
    state.operations.iter().map(|o| o.id).max().unwrap_or(0)
}

fn failure_text(op: &xen_core::Operation) -> Option<String> {
    match &op.status {
        OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
            Some(i18n::describe_xapi_error(e))
        }
        OperationStatus::Failed(other) => Some(format!("{other:?}")),
        OperationStatus::Cancelled => Some(crate::i18n::strings().op_cancelled.to_owned()),
        _ => None,
    }
}

// ---- Configure --------------------------------------------------------------------------------

/// The Configure dialog (`DRConfigureDialog`).
pub struct ConfigureDialog {
    connection: ConnectionId,
    pending: Option<u64>,
    /// The answer, until the rows are made from it.
    supported: Option<Vec<Ref<Sr>>>,
    /// The SRs that can hold the metadata, with their tick and whether
    /// replication was on.
    pub(crate) rows: Vec<(Ref<Sr>, bool, bool)>,
}

impl ConfigureDialog {
    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let s = dr_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        let m = &conn.model;
        if let Some(srs) = self.supported.take() {
            self.rows = srs
                .into_iter()
                .map(|sr| {
                    let on = rules::is_dr_enabled(m, &sr);
                    (sr, on, on)
                })
                .collect();
        }
        let originally = self.rows.iter().any(|(_, _, was)| *was);
        let checked = self.rows.iter().filter(|(_, on, _)| *on).count();
        let note = rules::configure_note(originally, checked);
        let mut outcome = Ok(None);
        window(fill(
            s.configure_title,
            &[("pool", &connection_name(&conn))],
        ))
        .show(ctx, |ui| {
            ui.label(s.configure_blurb1);
            ui.label(s.configure_blurb2);
            ui.add_space(4.0);
            if self.pending.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(s.checking_srs);
                });
            } else if self.rows.is_empty() {
                ui.label(RichText::new(s.no_srs).weak());
            }
            Grid::new("dr-configure")
                .num_columns(5)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.label("");
                    for h in [s.col_name, s.col_description, s.col_type, s.col_available] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for (sr, on, _) in &mut self.rows {
                        let Some(rec) = m.srs.get(sr) else { continue };
                        ui.checkbox(on, "");
                        ui.label(&rec.name_label);
                        ui.label(&rec.name_description);
                        ui.label(sr_type_name(&conn, &rec.r#type));
                        ui.label(xen_core::format::disk_size(
                            xen_core::provisioning::sr_free(rec),
                        ));
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            match note {
                ConfigureNote::Info => {
                    ui.label(s.enable_note);
                }
                ConfigureNote::Disabling => {
                    ui.colored_label(ui.visuals().warn_fg_color, s.disable_note1);
                    ui.colored_label(ui.visuals().warn_fg_color, s.disable_note2);
                }
                ConfigureNote::TooMany => {
                    ui.colored_label(ui.visuals().error_fg_color, s.limit_note1);
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        fill(s.limit_note2, &[("count", &rules::MAX_SRS.to_string())]),
                    );
                }
            }
            ui.separator();
            dialog_buttons(ui, |row| {
                let ok_enabled = self.pending.is_none() && note != ConfigureNote::TooMany;
                if row
                    .add_enabled(ButtonRole::Primary, ok_enabled, Button::new(s.ok))
                    .clicked()
                {
                    let enable: Vec<Ref<Sr>> = self
                        .rows
                        .iter()
                        .filter(|(_, on, was)| *on && !*was)
                        .map(|(r, _, _)| r.clone())
                        .collect();
                    let disable: Vec<Ref<Sr>> = self
                        .rows
                        .iter()
                        .filter(|(_, on, was)| !*on && *was)
                        .map(|(r, _, _)| r.clone())
                        .collect();
                    outcome = if enable.is_empty() && disable.is_empty() {
                        Err(())
                    } else {
                        Ok(Some(ActionRequest::ConfigureDr { enable, disable }))
                    };
                }
                if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                    outcome = Err(());
                }
            });
        });
        outcome
    }

    /// Whether the server answered (tests).
    #[cfg(test)]
    pub(crate) fn checked(&self) -> bool {
        self.pending.is_none()
    }

    /// The SRs listed (tests).
    #[cfg(test)]
    pub(crate) fn row_names(&self, conn: &ConnectionView) -> Vec<String> {
        let mut names: Vec<String> = self
            .supported
            .iter()
            .flatten()
            .chain(self.rows.iter().map(|(r, _, _)| r))
            .filter_map(|r| conn.model.srs.get(r).map(|s| s.name_label.clone()))
            .collect();
        names.sort();
        names
    }

    fn take(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if self.pending != Some(id) {
            return false;
        }
        self.pending = None;
        if let PrecheckResult::DrSupportedSrs(rows) = result {
            self.supported = Some(
                rows.iter()
                    .filter(|(_, ok)| *ok)
                    .map(|(sr, _)| sr.clone())
                    .collect(),
            );
        }
        true
    }
}

// ---- the iSCSI device dialog --------------------------------------------------------------

/// `IscsiDeviceConfigDialog`.
#[derive(Default)]
pub struct IscsiDialog {
    pub(crate) host: String,
    pub(crate) port: String,
    pub(crate) chap: bool,
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) iqns: Vec<IqnInfo>,
    pub(crate) iqn: Option<usize>,
    pub(crate) luns: Vec<LunInfo>,
    pub(crate) lun: Option<usize>,
    pending: Option<(u64, bool)>,
    error: Option<String>,
}

impl IscsiDialog {
    fn new() -> Self {
        Self {
            port: "3260".into(),
            ..Self::default()
        }
    }

    fn chap(&self) -> Option<(String, Secret)> {
        (self.chap && !self.user.is_empty())
            .then(|| (self.user.clone(), Secret(self.password.clone())))
    }

    fn device_config(&self) -> Option<DeviceConfig> {
        let iqn = self.iqns.get(self.iqn?)?;
        let lun = self.luns.get(self.lun?)?;
        let chap = self.chap();
        let dc = xen_core::storage::iscsi_device_config(
            iqn,
            lun,
            chap.as_ref().map(|(u, p)| (u.as_str(), p.0.as_str())),
        );
        Some(DeviceConfig(dc))
    }

    /// `Ok(Some(config))` when OK was clicked, `Err` when cancelled.
    fn show(
        &mut self,
        ctx: &Context,
        conn: ConnectionId,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> Result<Option<DeviceConfig>, ()> {
        let s = dr_strings();
        let mut outcome = Ok(None);
        let port: Option<u16> = self.port.trim().parse().ok();
        window(s.iscsi_title.to_owned()).show(ctx, |ui| {
            ui.label(s.iscsi_blurb);
            Grid::new("dr-iscsi").num_columns(2).show(ui, |ui| {
                let l = ui.label(s.iscsi_host);
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.host).desired_width(220.0))
                        .labelled_by(l.id);
                    ui.label(":");
                    ui.add(TextEdit::singleline(&mut self.port).desired_width(60.0));
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut self.chap, s.iscsi_chap);
                ui.end_row();
                if self.chap {
                    let l = ui.label(s.iscsi_user);
                    ui.add(TextEdit::singleline(&mut self.user).desired_width(220.0))
                        .labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label(s.iscsi_password);
                    ui.add(
                        TextEdit::singleline(&mut self.password)
                            .password(true)
                            .desired_width(220.0),
                    )
                    .labelled_by(l.id);
                    ui.end_row();
                }
                ui.label(s.iscsi_iqn);
                ui.horizontal(|ui| {
                    let text = self
                        .iqn
                        .and_then(|i| self.iqns.get(i))
                        .map(IqnInfo::label)
                        .unwrap_or_default();
                    ComboBox::from_id_salt("dr-iscsi-iqn")
                        .selected_text(text)
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for (i, q) in self.iqns.iter().enumerate() {
                                if ui
                                    .selectable_label(self.iqn == Some(i), q.label())
                                    .clicked()
                                {
                                    self.iqn = Some(i);
                                    self.luns.clear();
                                    self.lun = None;
                                }
                            }
                        });
                    let can =
                        !self.host.trim().is_empty() && port.is_some() && self.pending.is_none();
                    if ui
                        .add_enabled(can, Button::new(s.iscsi_discover_iqns))
                        .clicked()
                        && let Some(port) = port
                    {
                        *next_id += 1;
                        let id = ID_BASE + *next_id;
                        self.pending = Some((id, false));
                        self.error = None;
                        backend.send(Command::Precheck {
                            connection: conn,
                            id,
                            check: Precheck::IscsiIqns {
                                target: self.host.trim().to_owned(),
                                port,
                                chap: self.chap(),
                            },
                        });
                    }
                });
                ui.end_row();
                ui.label(s.iscsi_lun);
                ui.horizontal(|ui| {
                    let text = self
                        .lun
                        .and_then(|i| self.luns.get(i))
                        .map(xen_core::storage::lun_label)
                        .unwrap_or_default();
                    ComboBox::from_id_salt("dr-iscsi-lun")
                        .selected_text(text)
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for (i, l) in self.luns.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.lun,
                                    Some(i),
                                    xen_core::storage::lun_label(l),
                                );
                            }
                        });
                    let iqn = self.iqn.and_then(|i| self.iqns.get(i)).cloned();
                    if ui
                        .add_enabled(
                            iqn.is_some() && self.pending.is_none(),
                            Button::new(s.iscsi_discover_luns),
                        )
                        .clicked()
                        && let Some(iqn) = iqn
                    {
                        *next_id += 1;
                        let id = ID_BASE + *next_id;
                        self.pending = Some((id, true));
                        self.error = None;
                        backend.send(Command::Precheck {
                            connection: conn,
                            id,
                            check: Precheck::IscsiLuns {
                                target: iqn.address.clone(),
                                port: iqn.port,
                                iqn: iqn.target_iqn.clone(),
                                chap: self.chap(),
                            },
                        });
                    }
                });
                ui.end_row();
            });
            if self.pending.is_some() {
                ui.spinner();
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            dialog_buttons(ui, |row| {
                let config = self.device_config();
                if row
                    .add_enabled(ButtonRole::Primary, config.is_some(), Button::new(s.ok))
                    .clicked()
                {
                    outcome = Ok(config);
                }
                if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                    outcome = Err(());
                }
            });
        });
        outcome
    }

    fn take(&mut self, id: u64, result: &PrecheckResult) -> bool {
        let Some((pending, luns)) = self.pending else {
            return false;
        };
        if pending != id {
            return false;
        }
        self.pending = None;
        match (luns, result) {
            (false, PrecheckResult::IscsiIqns(Ok(iqns))) => {
                self.iqns = iqns.clone();
                self.iqn = (!self.iqns.is_empty()).then_some(0);
            }
            (true, PrecheckResult::IscsiLuns(Ok(luns))) => {
                self.luns = luns.clone();
                self.lun = (!self.luns.is_empty()).then_some(0);
            }
            (_, PrecheckResult::IscsiIqns(Err(e)) | PrecheckResult::IscsiLuns(Err(e))) => {
                self.error = Some(i18n::describe_xapi_error(e));
            }
            _ => {}
        }
        true
    }
}

// ---- the wizard ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Rbac,
    Welcome,
    First,
    Storage,
    VApps,
    Prechecks,
    Recover,
    Report,
}

/// A row of the Pre-checks page.
#[derive(Debug, Clone)]
pub(crate) enum PrecheckLine {
    Header(String),
    Check(CheckRow),
}

/// A row of the Progress page.
#[derive(Debug, Clone)]
pub(crate) struct RecoverRow {
    pub(crate) title: String,
    /// `None` for the "Start VMs and vApps" row.
    item: Option<(usize, DrItem)>,
    pub(crate) state: RowState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowState {
    Pending,
    Working,
    Done,
    Failed(String),
}

/// What the wizard waits for.
#[derive(Debug, Clone)]
enum Waiting {
    Nothing,
    /// The SRs that can hold metadata (Storage page).
    Srs(u64),
    /// A scan of a device (iSCSI or HBA).
    Scan(u64, String, DeviceConfig),
    /// `DR_task.create`s for the found SRs, then the metadata.
    Introducing {
        queue: VecDeque<DrTaskSpec>,
        floor: Option<u64>,
    },
    Metadata(u64),
    /// The server's pre-checks, per selected metadata.
    Checks(BTreeMap<u64, usize>),
    /// A pre-check fix running (then the checks run again).
    Fix {
        floor: u64,
        revert: Option<ActionRequest>,
        task: bool,
    },
    /// The recovery: the row running, sent after `floor`.
    Recovering {
        row: usize,
        floor: Option<u64>,
    },
}

/// The Disaster Recovery wizard (`DRFailoverWizard`).
pub struct DrWizard {
    connection: ConnectionId,
    pub(crate) page: Page,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    pub(crate) kind: Option<WizardType>,
    // Storage.
    supported: Vec<Ref<Sr>>,
    scanned: Vec<ScannedSr>,
    pub(crate) selected_srs: Vec<String>,
    pub(crate) iscsi: Option<IscsiDialog>,
    notice: Option<String>,
    // vApps & VMs.
    pub(crate) metadata: Vec<PoolMetadata>,
    pub(crate) chosen: Vec<Chosen>,
    pub(crate) after: AfterRecovery,
    // Pre-checks.
    pub(crate) checks: Vec<PrecheckLine>,
    pub(crate) hide_successful: bool,
    checks_done: bool,
    /// The question of a fix that deletes VMs.
    pub(crate) question: Option<(String, ConnectionId, ActionRequest)>,
    // Recover.
    pub(crate) rows: Vec<RecoverRow>,
    recovered_vms: Vec<String>,
    recovered_vapps: Vec<String>,
    recovery_done: bool,
    completed: Option<String>,
    // Clean-up.
    dr_tasks: Vec<Ref<xapi::DrTask>>,
    introduced: Vec<String>,
    reverts: Vec<ActionRequest>,
    warnings: Vec<String>,
    pub(crate) report: Report,
    waiting: Waiting,
    error: Option<String>,
    /// The server's storage check per selected metadata (its index).
    answers: BTreeMap<usize, Result<Vec<(DrItem, rules::Recoverable)>, String>>,
    /// Resolve All: the fixes still to run.
    fix_queue: VecDeque<Fix>,
    /// The fix waiting for its question's answer.
    pending_fix: Option<Fix>,
    /// Check Again (asked on the next frame).
    pub(crate) recheck: bool,
    /// Find Hardware HBA SRs (asked on the next frame).
    pub(crate) hba_scan: bool,
    /// This pool's uuid (its own metadata cannot be chosen).
    my_pool_uuid: Option<String>,
    /// `DoFinalCleanup` running after the Progress page.
    cleanup: Option<Cleanup>,
}

impl DrWizard {
    fn new(conn: &ConnectionView) -> Self {
        let rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), &["dr_task.create"])
        {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                dr_strings().rbac_message,
                &[("role", &i18n::session_roles_text(conn))],
            )),
        };
        Self {
            connection: conn.id,
            page: if rbac_needed {
                Page::Rbac
            } else {
                Page::Welcome
            },
            rbac_needed,
            rbac_blocked,
            kind: None,
            supported: Vec::new(),
            scanned: Vec::new(),
            selected_srs: Vec::new(),
            iscsi: None,
            notice: None,
            metadata: Vec::new(),
            chosen: Vec::new(),
            after: AfterRecovery::Start,
            checks: Vec::new(),
            hide_successful: true,
            checks_done: false,
            question: None,
            rows: Vec::new(),
            recovered_vms: Vec::new(),
            recovered_vapps: Vec::new(),
            recovery_done: false,
            completed: None,
            dr_tasks: Vec::new(),
            introduced: Vec::new(),
            reverts: Vec::new(),
            warnings: Vec::new(),
            report: Report::default(),
            waiting: Waiting::Nothing,
            error: None,
            answers: BTreeMap::new(),
            fix_queue: VecDeque::new(),
            pending_fix: None,
            recheck: false,
            hba_scan: false,
            my_pool_uuid: conn.model.pool().map(|(_, p)| p.uuid.clone()),
            cleanup: None,
        }
    }

    fn pages(&self) -> Vec<Page> {
        let mut p = Vec::new();
        if self.rbac_needed {
            p.push(Page::Rbac);
        }
        p.extend([
            Page::Welcome,
            Page::First,
            Page::Storage,
            Page::VApps,
            Page::Prechecks,
            Page::Recover,
            Page::Report,
        ]);
        p
    }

    fn page_name(p: Page) -> &'static str {
        let s = dr_strings();
        match p {
            Page::Rbac => s.page_rbac,
            Page::Welcome => s.page_welcome,
            Page::First => s.page_first,
            Page::Storage => s.page_storage,
            Page::VApps => s.page_vapps,
            Page::Prechecks => s.page_prechecks,
            Page::Recover => s.page_recover,
            Page::Report => s.page_report,
        }
    }

    pub(crate) fn title(&self, pool: &str) -> String {
        let s = dr_strings();
        match self.kind {
            Some(WizardType::Failover) => fill(s.title_failover, &[("pool", pool)]),
            Some(WizardType::Failback) => fill(s.title_failback, &[("pool", pool)]),
            Some(WizardType::Dryrun) => fill(s.title_dryrun, &[("pool", pool)]),
            None => s.wizard_title.to_owned(),
        }
    }

    fn busy(&self) -> bool {
        !matches!(self.waiting, Waiting::Nothing)
    }

    /// Nothing asked of the server is pending (tests).
    #[cfg(test)]
    pub(crate) fn idle(&self) -> bool {
        !self.busy()
    }

    /// The Storage page's SRs (tests).
    #[cfg(test)]
    pub(crate) fn row_names(&self, conn: &ConnectionView) -> Vec<String> {
        self.storage_rows(conn)
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    fn problems(&self) -> impl Iterator<Item = &Problem> {
        self.checks.iter().filter_map(|l| match l {
            PrecheckLine::Check(c) => c.problem.as_ref(),
            PrecheckLine::Header(_) => None,
        })
    }

    fn page_ok(&self) -> bool {
        if self.busy() {
            return false;
        }
        match self.page {
            Page::Rbac => self.rbac_blocked.is_none(),
            Page::Welcome => self.kind.is_some(),
            Page::Storage => !self.selected_srs.is_empty(),
            Page::VApps => self.chosen.iter().any(|c| !c.is_empty()),
            Page::Prechecks => self.checks_done && self.problems().all(Problem::is_warning),
            Page::Recover => self.recovery_done,
            _ => true,
        }
    }

    fn storage_rows(&self, conn: &ConnectionView) -> Vec<StorageRow> {
        rules::storage_rows(&conn.model, &self.supported, &self.scanned)
    }

    fn ask(&mut self, backend: &BackendHandle, next_id: &mut u64, check: Precheck) -> u64 {
        *next_id += 1;
        let id = ID_BASE + *next_id;
        backend.send(Command::Precheck {
            connection: self.connection,
            id,
            check,
        });
        id
    }

    /// Entering a page going forward (`PageLoadedCore` / `PopulatePage`).
    fn enter(&mut self, conn: &ConnectionView, backend: &BackendHandle, next_id: &mut u64) {
        match self.page {
            Page::Storage => {
                let id = self.ask(
                    backend,
                    next_id,
                    Precheck::DrSupportedSrs {
                        srs: rules::candidate_srs(&conn.model),
                    },
                );
                self.waiting = Waiting::Srs(id);
            }
            Page::VApps => {
                self.chosen = vec![Chosen::default(); self.metadata.len()];
                self.after = if self.kind == Some(WizardType::Dryrun) {
                    AfterRecovery::StartPaused
                } else {
                    AfterRecovery::Start
                };
            }
            Page::Prechecks => self.run_checks(backend, next_id),
            Page::Recover => self.start_recovery(conn),
            _ => {}
        }
    }

    /// The selected metadata with what was chosen in it.
    fn selected(&self) -> Vec<(usize, PoolMetadata, Chosen)> {
        self.metadata
            .iter()
            .zip(&self.chosen)
            .enumerate()
            .filter(|(_, (_, c))| !c.is_empty())
            .map(|(i, (m, c))| (i, m.clone(), c.clone()))
            .collect()
    }

    /// `RefreshRechecks`: the server's checks are asked; the cache ones
    /// are made when the answers are in.
    fn run_checks(&mut self, backend: &BackendHandle, next_id: &mut u64) {
        self.checks_done = false;
        self.checks.clear();
        self.answers.clear();
        let pending: BTreeMap<u64, usize> = self
            .selected()
            .into_iter()
            .map(|(i, meta, chosen)| {
                let id = self.ask(
                    backend,
                    next_id,
                    Precheck::DrRecoverable {
                        vdi: meta.vdi.clone(),
                        items: chosen.items(&meta),
                    },
                );
                (id, i)
            })
            .collect();
        self.waiting = Waiting::Checks(pending);
    }

    /// The Pre-checks page's rows once the server answered (`GenerateChecks`).
    fn finish_checks(&mut self, state: &AppState, conn: &ConnectionView) {
        let Some(kind) = self.kind else { return };
        let sources: BTreeMap<String, (ConnectionId, Arc<xen_core::ConnectionModel>)> = state
            .connections
            .iter()
            .filter(|c| c.id != conn.id && c.status.is_connected())
            .filter_map(|c| {
                let (_, p) = c.model.pool()?;
                Some((p.uuid.clone(), (c.id, c.model.clone())))
            })
            .collect();
        let selected: Vec<(PoolMetadata, Chosen)> = self
            .selected()
            .into_iter()
            .map(|(_, m, c)| (m, c))
            .collect();
        let mut groups = rules::cache_checks(kind, &conn.model, &selected, &sources);
        let mut rows = Vec::new();
        for (i, meta, chosen) in self.selected() {
            match self.answers.get(&i) {
                Some(Ok(answers)) => {
                    for item in chosen.items(&meta) {
                        let problem = answers
                            .iter()
                            .find(|(x, _)| *x == item)
                            .and_then(|(_, a)| rules::recoverable_problem(a));
                        rows.push(CheckRow {
                            subject: subject_of(&meta, &item),
                            problem,
                        });
                    }
                }
                Some(Err(e)) => self.error = Some(e.clone()),
                None => {}
            }
        }
        groups.push((
            CheckGroup::Recoverable {
                pool: connection_name(conn),
            },
            rows,
        ));
        let s = dr_strings();
        let count = groups.len();
        self.checks.clear();
        for (i, (group, rows)) in groups.into_iter().enumerate() {
            let name = match &group {
                CheckGroup::HaStatus => s.group_ha.to_owned(),
                CheckGroup::SourcePowerState => s.group_power.to_owned(),
                CheckGroup::Existing { pool } => fill(s.group_existing, &[("pool", pool)]),
                CheckGroup::Recoverable { pool } => fill(s.group_storage, &[("pool", pool)]),
            };
            self.checks.push(PrecheckLine::Header(fill(
                s.header,
                &[
                    ("index", &(i + 1).to_string()),
                    ("count", &count.to_string()),
                    ("group", &name),
                ],
            )));
            self.checks
                .extend(rows.into_iter().map(PrecheckLine::Check));
        }
        self.warnings = self
            .checks
            .iter()
            .filter_map(|l| match l {
                PrecheckLine::Check(c) if c.problem.as_ref().is_some_and(Problem::is_warning) => {
                    Some(check_text(c))
                }
                _ => None,
            })
            .collect();
        self.checks_done = true;
    }

    /// The report's start (`StartSummaryReport`) and the Progress page's
    /// rows.
    fn start_recovery(&mut self, conn: &ConnectionView) {
        let s = dr_strings();
        let pool = connection_name(conn);
        self.report = Report::default();
        self.report.line(&self.title(&pool), 0, None);
        self.report.line(s.report_selected_srs, 0, None);
        for row in self.storage_rows(conn) {
            if self.selected_srs.contains(&row.uuid) {
                self.report.line(&row.name, 1, None);
            }
        }
        self.report.line(s.report_selected, 0, None);
        for (_, meta, chosen) in self.selected() {
            self.report.line(&meta.pool_name, 1, None);
            for (r, app) in &meta.vm_appliances {
                if !chosen.vapps.contains(r) {
                    continue;
                }
                self.report.line(&app.name_label, 2, None);
                for vm in &app.vms {
                    if let Some(v) = meta.vm(vm) {
                        self.report.line(&v.name_label, 3, None);
                    }
                }
            }
            for (r, vm) in meta.loose_vms() {
                if chosen.vms.contains(r) {
                    self.report.line(&vm.name_label, 2, None);
                }
            }
        }
        self.report.line(
            &fill(s.report_power, &[("state", self.power_text(self.after))]),
            0,
            None,
        );
        let names = self.introduced_names(conn);
        if names.is_empty() {
            self.report.line(s.report_introduced_none, 0, None);
        } else {
            self.report.line(
                &fill(s.report_introduced, &[("srs", &names.join(", "))]),
                0,
                None,
            );
        }
        if self.warnings.is_empty() {
            self.report.line(s.report_warnings_none, 0, None);
        } else {
            self.report.line(s.report_warnings, 0, None);
            for w in self.warnings.clone() {
                self.report.line(&w, 1, None);
            }
        }
        self.report.line("", 0, None);
        self.report.line(s.report_started, 0, Some(&now_text()));
        self.rows.clear();
        for (i, meta, chosen) in self.selected() {
            for item in chosen.items(&meta) {
                let name = rules::item_name(&meta, &item);
                let title = match item {
                    DrItem::Vm(_) => fill(s.op_recover_vm, &[("name", &name)]),
                    DrItem::VApp(_) => fill(s.op_recover_vapp, &[("name", &name)]),
                };
                self.rows.push(RecoverRow {
                    title,
                    item: Some((i, item)),
                    state: RowState::Pending,
                });
            }
        }
        if self.after != AfterRecovery::DoNotStart {
            self.rows.push(RecoverRow {
                title: s.op_start.to_owned(),
                item: None,
                state: RowState::Pending,
            });
        }
        self.recovered_vms.clear();
        self.recovered_vapps.clear();
        self.recovery_done = false;
        self.completed = None;
        self.waiting = Waiting::Recovering {
            row: 0,
            floor: None,
        };
    }

    fn introduced_names(&self, conn: &ConnectionView) -> Vec<String> {
        self.introduced
            .iter()
            .map(|u| {
                conn.model
                    .srs
                    .values()
                    .find(|s| s.uuid == *u)
                    .map(|s| s.name_label.clone())
                    .unwrap_or_else(|| u.clone())
            })
            .collect()
    }

    fn power_text(&self, after: AfterRecovery) -> &'static str {
        let s = dr_strings();
        let back = self.kind == Some(WizardType::Failback);
        match (after, back) {
            (AfterRecovery::Start, false) => s.failover_start,
            (AfterRecovery::Start, true) => s.failback_start,
            (AfterRecovery::StartPaused, false) => s.failover_paused,
            (AfterRecovery::StartPaused, true) => s.failback_paused,
            (AfterRecovery::DoNotStart, false) => s.failover_none,
            (AfterRecovery::DoNotStart, true) => s.failback_none,
        }
    }

    /// `DoFinalCleanup`'s steps.
    fn cleanup(&mut self, conn: &ConnectionView) -> Cleanup {
        let s = dr_strings();
        let mut steps = VecDeque::new();
        steps.push_back(Step::Line(String::new()));
        if !self.dr_tasks.is_empty() {
            steps.push_back(Step::Line(s.report_dr_cleanup.to_owned()));
            let names = self.introduced_names(conn).join(", ");
            for task in std::mem::take(&mut self.dr_tasks) {
                steps.push_back(Step::Run(ActionRequest::DrTaskDestroy {
                    task,
                    srs: names.clone(),
                }));
            }
        }
        if self.kind == Some(WizardType::Dryrun) {
            steps.push_back(Step::Line(s.report_dryrun_cleanup.to_owned()));
            if !self.recovered_vms.is_empty() {
                steps.push_back(Step::DestroyVms(self.recovered_vms.clone()));
            }
            if !self.recovered_vapps.is_empty() {
                steps.push_back(Step::DestroyVApps(self.recovered_vapps.clone()));
            }
            for sr in &self.introduced {
                steps.push_back(Step::Sr(sr.clone(), SrAction::Detach));
                steps.push_back(Step::Sr(sr.clone(), SrAction::Forget));
            }
        }
        if !self.reverts.is_empty() {
            steps.push_back(Step::Line(s.report_revert.to_owned()));
            for r in std::mem::take(&mut self.reverts) {
                steps.push_back(Step::Run(r));
            }
        }
        Cleanup {
            connection: self.connection,
            steps,
            floor: None,
            report: Report::default(),
            waiting_since: None,
        }
    }

    /// What a problem's Resolve does (`GetSolutionAction`): the connection,
    /// the request, what changes it back, and the question asked first.
    fn fix(&self, conn: &ConnectionView, problem: &Problem) -> Option<Fix> {
        let s = dr_strings();
        let at = |w: &Where| match w {
            Where::Here => conn.id,
            Where::Source(c) => *c,
        };
        Some(match problem {
            Problem::HaEnabled { heartbeat_sr, ntol } => Fix {
                connection: conn.id,
                request: ActionRequest::DisableHa,
                revert: heartbeat_sr.clone().map(|sr| ActionRequest::EnableHa {
                    heartbeat_sr: sr,
                    ntol: *ntol,
                    startup: Vec::new(),
                }),
                question: None,
            },
            Problem::RunningVm {
                at: w,
                vm,
                hard,
                clean,
                ..
            } => Fix {
                connection: at(w),
                request: ActionRequest::Vm {
                    vm: vm.clone(),
                    action: if !*hard && *clean {
                        VmAction::CleanShutdown
                    } else {
                        VmAction::HardShutdown
                    },
                },
                revert: None,
                question: None,
            },
            Problem::RunningVApp {
                at: w, vapp, hard, ..
            } => Fix {
                connection: at(w),
                request: if *hard {
                    ActionRequest::HardShutDownVApp { vapp: vapp.clone() }
                } else {
                    ActionRequest::ShutDownVApp { vapp: vapp.clone() }
                },
                revert: None,
                question: None,
            },
            Problem::ExistingVm { vm, name, pool } => Fix {
                connection: conn.id,
                request: ActionRequest::DrDestroyVms {
                    vms: vec![vm.clone()],
                },
                revert: None,
                question: Some(fill(s.confirm_delete_vm, &[("vm", name), ("pool", pool)])),
            },
            Problem::ExistingVApp { vms, pool } => Fix {
                connection: conn.id,
                request: ActionRequest::DrDestroyVms {
                    vms: vms.iter().map(|(r, _)| r.clone()).collect(),
                },
                revert: None,
                question: Some(fill(
                    s.confirm_delete_vms,
                    &[
                        (
                            "vms",
                            &vms.iter()
                                .map(|(_, n)| n.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                        ),
                        ("pool", pool),
                    ],
                )),
            },
            Problem::MissingSr {
                fix: Some(spec),
                sr: Some(sr),
            } if sr.shared => Fix {
                connection: conn.id,
                request: ActionRequest::DrTaskCreate(Box::new(spec.clone())),
                revert: None,
                question: None,
            },
            Problem::MissingSrs { fixes } if !fixes.is_empty() => Fix {
                connection: conn.id,
                request: ActionRequest::DrTaskCreate(Box::new(fixes[0].clone())),
                revert: None,
                question: None,
            },
            _ => return None,
        })
    }
}

/// A pre-check fix to run.
#[derive(Debug, Clone)]
struct Fix {
    connection: ConnectionId,
    request: ActionRequest,
    revert: Option<ActionRequest>,
    question: Option<String>,
}

fn subject_of(meta: &PoolMetadata, item: &DrItem) -> CheckSubject {
    let name = rules::item_name(meta, item);
    match item {
        DrItem::Vm(_) => CheckSubject::Vm { name },
        DrItem::VApp(_) => CheckSubject::VApp { name },
    }
}

fn subject_text(subject: &CheckSubject) -> String {
    let s = dr_strings();
    match subject {
        CheckSubject::Ha { pool } => fill(s.check_ha, &[("pool", pool)]),
        CheckSubject::Vm { name } => fill(s.check_vm, &[("name", name)]),
        CheckSubject::VApp { name } => fill(s.check_vapp, &[("name", name)]),
    }
}

/// A problem's `Description`.
pub(crate) fn problem_text(problem: &Problem) -> String {
    let s = dr_strings();
    match problem {
        Problem::HaEnabled { .. } => s.problem_ha.to_owned(),
        Problem::RunningVm { pool, .. } => fill(s.problem_running_vm, &[("pool", pool)]),
        Problem::RunningVApp { pool, .. } => fill(s.problem_running_vapp, &[("pool", pool)]),
        Problem::ExistingVm { pool, .. } => fill(s.problem_existing_vm, &[("pool", pool)]),
        Problem::ExistingVApp { pool, .. } => fill(s.problem_existing_vapp, &[("pool", pool)]),
        Problem::ExistingWarning { pool } => fill(s.warning_existing, &[("pool", pool)]),
        Problem::MissingSr { sr: None, .. } => s.problem_missing_sr_no_info.to_owned(),
        Problem::MissingSr { sr: Some(sr), .. } => fill(
            if sr.shared {
                s.problem_missing_sr
            } else {
                s.problem_local_sr
            },
            &[("name", &sr.name)],
        ),
        Problem::MissingSrs { fixes } => fill(
            s.problem_missing_srs,
            &[("count", &fixes.len().to_string())],
        ),
    }
}

/// A problem's `HelpMessage` (the Resolve link), if it has one.
fn help_text(problem: &Problem) -> Option<&'static str> {
    let s = dr_strings();
    if !problem.is_fixable() {
        return None;
    }
    Some(match problem {
        Problem::HaEnabled { .. } => s.problem_ha_fix,
        Problem::RunningVm { .. } => s.problem_running_vm_fix,
        Problem::RunningVApp { .. } => s.problem_running_vapp_fix,
        Problem::ExistingVm { .. } => s.problem_existing_vm_fix,
        Problem::ExistingVApp { .. } => s.problem_existing_vapp_fix,
        Problem::MissingSr { .. } => s.problem_missing_sr_fix,
        Problem::MissingSrs { .. } => s.problem_missing_srs_fix,
        Problem::ExistingWarning { .. } => return None,
    })
}

/// A row's text: "{check}: OK" or "{check}: {problem}".
pub(crate) fn check_text(row: &CheckRow) -> String {
    let s = dr_strings();
    let check = subject_text(&row.subject);
    match &row.problem {
        None => fill(s.check_ok, &[("check", &check)]),
        Some(p) => fill(
            s.check_problem,
            &[("check", &check), ("problem", &problem_text(p))],
        ),
    }
}

// ---- clean-up -------------------------------------------------------------------------------

/// One step of `DoFinalCleanup`.
#[derive(Debug, Clone)]
enum Step {
    Line(String),
    Run(ActionRequest),
    /// `ShutdownAndDestroyVMs` of the VMs with these uuids.
    DestroyVms(Vec<String>),
    /// `ShutdownAndDestroyAppliances` of the vApps with these uuids.
    DestroyVApps(Vec<String>),
    /// `DetachAndForgetSRs`: the SR with this uuid.
    Sr(String, SrAction),
}

/// The clean-up running, one operation at a time; it goes on after the
/// wizard closes.
#[derive(Debug)]
pub(crate) struct Cleanup {
    connection: ConnectionId,
    steps: VecDeque<Step>,
    floor: Option<(u64, String)>,
    report: Report,
    /// Since when a Forget waits for the cache to show the SR detached.
    waiting_since: Option<std::time::Instant>,
}

impl Cleanup {
    /// Moves on; true once done.
    fn step(&mut self, state: &AppState, ops: &mut OpsUi, backend: &BackendHandle) -> bool {
        let s = dr_strings();
        let Some(conn) = state.connection(self.connection) else {
            return true;
        };
        if let Some((floor, title)) = &self.floor {
            let ended = state
                .operations
                .iter()
                .find(|o| o.id > *floor && o.connection == Some(self.connection));
            match ended {
                Some(op) if !op.is_running() => {
                    let line = match failure_text(op) {
                        None => fill(s.report_succeeded, &[("title", title)]),
                        Some(e) => fill(s.report_failed, &[("title", title), ("error", &e)]),
                    };
                    self.report.line(&line, 1, None);
                    self.floor = None;
                }
                _ => return false,
            }
        }
        while let Some(step) = self.steps.pop_front() {
            let m = &conn.model;
            let request = match step {
                Step::Line(text) => {
                    self.report.line(&text, 0, None);
                    continue;
                }
                Step::Run(r) => r,
                Step::DestroyVms(uuids) => {
                    let vms: Vec<Ref<Vm>> = m
                        .vms
                        .iter()
                        .filter(|(_, v)| uuids.contains(&v.uuid))
                        .map(|(r, _)| r.clone())
                        .collect();
                    if vms.is_empty() {
                        continue;
                    }
                    ActionRequest::DrDestroyVms { vms }
                }
                Step::DestroyVApps(uuids) => {
                    let vapps: Vec<Ref<VmAppliance>> = m
                        .vm_appliances
                        .iter()
                        .filter(|(_, a)| uuids.contains(&a.uuid))
                        .map(|(r, _)| r.clone())
                        .collect();
                    if vapps.is_empty() {
                        continue;
                    }
                    ActionRequest::DrDestroyVApps { vapps }
                }
                Step::Sr(uuid, action) => {
                    let Some((sr, rec)) = m.srs.iter().find(|(_, x)| x.uuid == uuid) else {
                        continue;
                    };
                    // Forget once the cache shows the SR detached (a few
                    // seconds at most).
                    let detached = rec
                        .allowed_operations
                        .contains(&xapi::StorageOperations::Forget);
                    if action == SrAction::Forget && !detached {
                        let since = *self
                            .waiting_since
                            .get_or_insert_with(std::time::Instant::now);
                        if since.elapsed() < std::time::Duration::from_secs(10) {
                            self.steps.push_front(Step::Sr(uuid, action));
                            return false;
                        }
                    }
                    self.waiting_since = None;
                    ActionRequest::Sr {
                        sr: sr.clone(),
                        action,
                    }
                }
            };
            let title = i18n::operation_title(
                &xen_core::operations::operation_kind(m, &request),
                &conn.server,
            );
            self.floor = Some((floor_of(state), title));
            ops.dispatch(conn, request, backend);
            return false;
        }
        true
    }
}

// ---- drawing the wizard ----------------------------------------------------------------------

impl DrWizard {
    /// Follows what the wizard waits for (each frame).
    fn follow(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) {
        let s = dr_strings();
        match std::mem::replace(&mut self.waiting, Waiting::Nothing) {
            Waiting::Introducing { mut queue, floor } => {
                if let Some(floor) = floor {
                    match op_after(state, conn.id, floor, |k| *k == DrKind::TaskCreate) {
                        Some(op) if !op.is_running() => {
                            if let Some(task) = &op.result {
                                self.dr_tasks.push(Ref::new(task));
                            }
                            if let Some(e) = failure_text(op) {
                                self.error = Some(e);
                                queue.clear();
                            }
                        }
                        _ => {
                            self.waiting = Waiting::Introducing {
                                queue,
                                floor: Some(floor),
                            };
                            return;
                        }
                    }
                }
                if let Some(spec) = queue.pop_front() {
                    self.introduced.extend(spec.srs.iter().cloned());
                    let floor = floor_of(state);
                    ops.dispatch(conn, ActionRequest::DrTaskCreate(Box::new(spec)), backend);
                    self.waiting = Waiting::Introducing {
                        queue,
                        floor: Some(floor),
                    };
                } else {
                    let id = self.ask(
                        backend,
                        next_id,
                        Precheck::DrMetadata {
                            srs: self.selected_srs.clone(),
                        },
                    );
                    self.waiting = Waiting::Metadata(id);
                }
            }
            Waiting::Checks(pending) if pending.is_empty() => self.finish_checks(state, conn),
            Waiting::Fix {
                floor,
                revert,
                task,
            } => {
                let done = state
                    .operations
                    .iter()
                    .find(|o| o.id > floor)
                    .filter(|o| !o.is_running());
                match done {
                    Some(op) => {
                        match failure_text(op) {
                            None => {
                                self.reverts.extend(revert);
                                if task && let Some(t) = &op.result {
                                    self.dr_tasks.push(Ref::new(t));
                                }
                            }
                            Some(e) => self.error = Some(e),
                        }
                        if let Some(next) = self.fix_queue.pop_front() {
                            self.start_fix(state, conn, next, ops, backend);
                        } else {
                            self.run_checks(backend, next_id);
                        }
                    }
                    None => {
                        self.waiting = Waiting::Fix {
                            floor,
                            revert,
                            task,
                        }
                    }
                }
            }
            Waiting::Recovering { row, floor } => {
                self.follow_recovery(state, conn, ops, backend, row, floor)
            }
            other => self.waiting = other,
        }
        let _ = s;
    }

    fn start_fix(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        fix: Fix,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(q) = fix.question.clone() {
            self.question = Some((q, fix.connection, fix.request.clone()));
            self.pending_fix = Some(fix);
            return;
        }
        self.dispatch_fix(state, conn, fix, ops, backend);
    }

    fn dispatch_fix(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        fix: Fix,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let task = matches!(fix.request, ActionRequest::DrTaskCreate(_));
        if let ActionRequest::DrTaskCreate(spec) = &fix.request {
            self.introduced.extend(spec.srs.iter().cloned());
        }
        let floor = floor_of(state);
        let target: &ConnectionView = state.connection(fix.connection).map_or(conn, |c| &**c);
        ops.dispatch(target, fix.request, backend);
        self.waiting = Waiting::Fix {
            floor,
            revert: fix.revert,
            task,
        };
    }

    fn follow_recovery(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        row: usize,
        floor: Option<u64>,
    ) {
        let s = dr_strings();
        let Some(current) = self.rows.get(row).cloned() else {
            // All done (`SetCompletedMessages`).
            let pool = connection_name(conn);
            let done = match self.kind {
                Some(WizardType::Failback) => fill(s.recover_done_failback, &[("pool", &pool)]),
                Some(WizardType::Dryrun) => fill(s.recover_done_dryrun, &[("pool", &pool)]),
                _ => fill(
                    s.recover_done_failover,
                    &[("product", crate::branding::PLATFORM_NAME)],
                ),
            };
            self.report.line(&done, 0, Some(&now_text()));
            self.completed = Some(done);
            self.recovery_done = true;
            return;
        };
        if let Some(floor) = floor {
            let op = state
                .operations
                .iter()
                .find(|o| o.id > floor && o.connection == Some(conn.id));
            match op {
                Some(op) if !op.is_running() => {
                    let line = match failure_text(op) {
                        None => {
                            self.rows[row].state = RowState::Done;
                            if let Some((i, item)) = &current.item {
                                let uuid = rules::item_uuid(&self.metadata[*i], item);
                                match item {
                                    DrItem::Vm(_) => self.recovered_vms.push(uuid),
                                    DrItem::VApp(_) => {
                                        self.recovered_vapps.push(uuid);
                                    }
                                }
                            }
                            fill(s.report_succeeded, &[("title", &current.title)])
                        }
                        Some(e) => {
                            self.rows[row].state = RowState::Failed(e.clone());
                            fill(s.report_failed, &[("title", &current.title), ("error", &e)])
                        }
                    };
                    self.report.line(&line, 1, None);
                    self.waiting = Waiting::Recovering {
                        row: row + 1,
                        floor: None,
                    };
                }
                _ => {
                    self.waiting = Waiting::Recovering {
                        row,
                        floor: Some(floor),
                    }
                }
            }
            return;
        }
        let request = match &current.item {
            Some((i, item)) => ActionRequest::DrRecover {
                vdi: self.metadata[*i].vdi.clone(),
                item: item.clone(),
                name: rules::item_name(&self.metadata[*i], item),
            },
            None => {
                // The recovered objects must be in the cache first.
                let m = &conn.model;
                let vms: Vec<Ref<Vm>> = m
                    .vms
                    .iter()
                    .filter(|(_, v)| self.recovered_vms.contains(&v.uuid))
                    .map(|(r, _)| r.clone())
                    .collect();
                let vapps: Vec<Ref<VmAppliance>> = m
                    .vm_appliances
                    .iter()
                    .filter(|(_, a)| self.recovered_vapps.contains(&a.uuid))
                    .map(|(r, _)| r.clone())
                    .collect();
                if vms.len() < self.recovered_vms.len() || vapps.len() < self.recovered_vapps.len()
                {
                    self.waiting = Waiting::Recovering { row, floor: None };
                    return;
                }
                ActionRequest::DrStart {
                    vms,
                    vapps,
                    paused: self.after == AfterRecovery::StartPaused,
                }
            }
        };
        self.rows[row].state = RowState::Working;
        let floor = floor_of(state);
        ops.dispatch(conn, request, backend);
        self.waiting = Waiting::Recovering {
            row,
            floor: Some(floor),
        };
    }

    #[allow(clippy::too_many_lines)]
    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> WizardOutcome {
        let s = dr_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return WizardOutcome::Close;
        };
        if let Some(c) = self.cleanup.as_mut() {
            if c.step(state, ops, backend) {
                let report = std::mem::take(&mut c.report);
                for line in report.lines() {
                    self.report.push_raw(line);
                }
                self.cleanup = None;
                self.page = Page::Report;
            }
        } else {
            self.follow(state, &conn, ops, backend, next_id);
        }
        let pool = connection_name(&conn);
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let mut open = true;
        let mut outcome = WizardOutcome::Stay;
        let iscsi_outcome = self
            .iscsi
            .as_mut()
            .map(|d| d.show(ctx, conn.id, backend, next_id));
        match iscsi_outcome {
            Some(Ok(Some(config))) => {
                self.iscsi = None;
                let id = self.ask(
                    backend,
                    next_id,
                    Precheck::DrProbe {
                        sr_type: "lvmoiscsi".into(),
                        device_config: config.clone(),
                    },
                );
                self.waiting = Waiting::Scan(id, "lvmoiscsi".into(), config);
            }
            Some(Err(())) => self.iscsi = None,
            _ => {}
        }
        dialog_window(self.title(&pool))
            .id(egui::Id::unique("dr-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([820.0, 500.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(650.0);
                        egui::ScrollArea::vertical()
                            .id_salt("dr-page")
                            .max_height(400.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.page_ui(ui, &conn, &pool, state, ops, backend));
                        if let Some(e) = &self.error {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                    });
                });
                ui.separator();
                dialog_buttons(ui, |row| {
                    let first = index == 0;
                    let can_back = !first
                        && !self.busy()
                        && self.cleanup.is_none()
                        && !matches!(self.page, Page::Recover | Page::Report);
                    if row
                        .add_enabled(ButtonRole::Back, can_back, Button::new(s.back))
                        .clicked()
                    {
                        self.error = None;
                        self.page = pages[index - 1];
                    }
                    let next_label = match (self.page, self.kind) {
                        (Page::Prechecks, Some(WizardType::Failback)) => s.next_failback,
                        (Page::Prechecks, _) => s.next_failover,
                        (Page::Report, _) => s.finish,
                        _ => s.next,
                    };
                    let can_next = self.page_ok() && self.cleanup.is_none();
                    if row
                        .add_enabled(ButtonRole::Primary, can_next, Button::new(next_label))
                        .clicked()
                    {
                        self.error = None;
                        self.next(&conn, state, backend, next_id);
                        if self.page == Page::Report
                            && self.cleanup.is_none()
                            && next_label == s.finish
                        {
                            outcome = WizardOutcome::Close;
                        }
                    }
                    let can_cancel = self.page != Page::Report && self.cleanup.is_none();
                    if row
                        .add_enabled(ButtonRole::Cancel, can_cancel, Button::new(s.cancel))
                        .clicked()
                    {
                        outcome = WizardOutcome::Cancel;
                    }
                });
            });
        if !open && outcome == WizardOutcome::Stay {
            outcome = if self.page == Page::Report {
                WizardOutcome::Close
            } else {
                WizardOutcome::Cancel
            };
        }
        if outcome == WizardOutcome::Cancel {
            // `OnCancel`: the clean-up goes on without the wizard (the one
            // already running, if the window was closed during it).
            let cleanup = match self.cleanup.take() {
                Some(c) => c,
                None => self.cleanup(&conn),
            };
            return WizardOutcome::Orphan(cleanup);
        }
        outcome
    }

    /// Next (`UpdateWizardContent` and `PageLeaveCore`).
    fn next(
        &mut self,
        conn: &ConnectionView,
        state: &AppState,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) {
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        match self.page {
            Page::Storage => {
                // Introduce the found SRs, then read the metadata.
                let queue: VecDeque<DrTaskSpec> =
                    rules::devices_to_introduce(&self.storage_rows(conn), &self.selected_srs)
                        .into();
                self.waiting = Waiting::Introducing { queue, floor: None };
                return;
            }
            Page::Recover => {
                self.cleanup = Some(self.cleanup(conn));
                return;
            }
            Page::Report => return,
            _ => {}
        }
        let _ = state;
        self.page = pages[(index + 1).min(pages.len() - 1)];
        self.enter(conn, backend, next_id);
    }

    fn page_ui(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        pool: &str,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let s = dr_strings();
        match self.page {
            Page::Rbac => {
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
            Page::Welcome => {
                ui.heading(s.welcome_title);
                ui.label(s.welcome_blurb);
                ui.add_space(6.0);
                for (k, label, blurb) in [
                    (
                        WizardType::Failover,
                        s.failover,
                        fill(s.failover_blurb, &[("pool", pool)]),
                    ),
                    (
                        WizardType::Failback,
                        s.failback,
                        s.failback_blurb.to_owned(),
                    ),
                    (
                        WizardType::Dryrun,
                        s.dryrun,
                        fill(s.dryrun_blurb, &[("pool", pool)]),
                    ),
                ] {
                    if ui.radio(self.kind == Some(k), label).clicked() {
                        self.kind = Some(k);
                    }
                    ui.label(RichText::new(blurb).weak());
                    ui.add_space(4.0);
                }
            }
            Page::First => {
                let (title, lines): (&str, Vec<String>) = match self.kind {
                    Some(WizardType::Failback) => (
                        s.first_title_failback,
                        vec![
                            s.failback_line1.into(),
                            s.failback_line2.into(),
                            s.failback_line3.into(),
                        ],
                    ),
                    Some(WizardType::Dryrun) => (
                        s.first_title_dryrun,
                        vec![
                            fill(s.dryrun_line1, &[("pool", pool)]),
                            s.dryrun_line2.into(),
                            s.dryrun_line3.into(),
                        ],
                    ),
                    _ => (
                        s.first_title_failover,
                        vec![
                            s.failover_line1.into(),
                            s.failover_line2.into(),
                            s.failover_line3.into(),
                        ],
                    ),
                };
                ui.heading(title);
                for l in lines {
                    ui.label(l);
                    ui.add_space(4.0);
                }
                let warning = match self.kind {
                    Some(WizardType::Failover) => Some(s.failover_line4),
                    Some(WizardType::Failback) => Some(s.failback_line4),
                    _ => None,
                };
                if let Some(w) = warning {
                    ui.colored_label(ui.visuals().warn_fg_color, s.important);
                    ui.colored_label(ui.visuals().warn_fg_color, w);
                }
            }
            Page::Storage => self.storage_ui(ui, conn),
            Page::VApps => self.vapps_ui(ui, pool),
            Page::Prechecks => self.prechecks_ui(ui, conn, state, ops, backend),
            Page::Recover => self.recover_ui(ui, pool),
            Page::Report => {
                let title = match self.kind {
                    Some(WizardType::Failback) => s.report_title_failback,
                    Some(WizardType::Dryrun) => s.report_title_dryrun,
                    _ => s.report_title_failover,
                };
                ui.heading(fill(title, &[("pool", pool)]));
                let mut text = self.report.text();
                ui.add(
                    TextEdit::multiline(&mut text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(18)
                        .interactive(false)
                        .font(egui::TextStyle::Monospace),
                );
            }
        }
    }

    fn storage_ui(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = dr_strings();
        ui.heading(s.storage_title);
        ui.label(if self.kind == Some(WizardType::Failback) {
            s.storage_failback
        } else {
            s.storage_failover
        });
        ui.add_space(4.0);
        let busy = self.busy();
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(match self.waiting {
                    Waiting::Introducing { .. } => s.preparing,
                    Waiting::Metadata(_) => s.loading_metadata,
                    _ => "",
                });
            });
        }
        let rows = self.storage_rows(conn);
        Grid::new("dr-storage")
            .num_columns(5)
            .striped(true)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.label("");
                for h in [s.col_name, s.col_description, s.col_type, s.col_metadata] {
                    ui.strong(h);
                }
                ui.end_row();
                for row in &rows {
                    let mut on = self.selected_srs.contains(&row.uuid);
                    if ui
                        .add_enabled(!busy, egui::Checkbox::new(&mut on, ""))
                        .changed()
                    {
                        if on {
                            self.selected_srs.push(row.uuid.clone());
                        } else {
                            self.selected_srs.retain(|u| *u != row.uuid);
                        }
                    }
                    ui.label(&row.name);
                    ui.label(&row.description);
                    ui.label(sr_type_name(conn, &row.sr_type));
                    ui.label(if row.metadata { s.yes } else { s.no });
                    ui.end_row();
                }
            });
        if let Some(n) = &self.notice {
            ui.label(RichText::new(n).weak());
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.menu_button(s.find_srs, |ui| {
                    if ui.button(s.find_hba).clicked() {
                        self.hba_scan = true;
                        ui.close();
                    }
                    if ui.button(s.find_iscsi).clicked() {
                        self.iscsi = Some(IscsiDialog::new());
                        ui.close();
                    }
                });
            });
            let count = self.selected_srs.len();
            if ui
                .add_enabled(!busy && count < rows.len(), Button::new(s.select_all))
                .clicked()
            {
                self.selected_srs = rows.iter().map(|r| r.uuid.clone()).collect();
            }
            if ui
                .add_enabled(!busy && count > 0, Button::new(s.clear_all))
                .clicked()
            {
                self.selected_srs.clear();
            }
        });
    }

    fn vapps_ui(&mut self, ui: &mut Ui, pool: &str) {
        let s = dr_strings();
        let back = self.kind == Some(WizardType::Failback);
        ui.heading(if back {
            s.vapps_title_failback
        } else {
            s.vapps_title_failover
        });
        ui.label(if back {
            fill(s.vapps_failback, &[("pool", pool)])
        } else {
            s.vapps_failover.to_owned()
        });
        ui.add_space(4.0);
        ui.label(s.vapps_found);
        if self.metadata.is_empty() {
            ui.label(RichText::new(s.no_metadata).weak());
        }
        let my_pool = self.my_pool_uuid.clone();
        for (i, meta) in self.metadata.iter().enumerate() {
            let current = Some(&meta.pool.uuid) == my_pool.as_ref();
            ui.horizontal(|ui| {
                ui.strong(&meta.pool_name);
                if current {
                    ui.label(RichText::new(s.current_pool).weak());
                }
            });
            let chosen = &mut self.chosen[i];
            ui.add_enabled_ui(!current, |ui| {
                ui.indent(("dr-meta", i), |ui| {
                    for (r, app) in &meta.vm_appliances {
                        let mut on = chosen.vapps.contains(r);
                        if ui.checkbox(&mut on, &app.name_label).changed() {
                            if on {
                                chosen.vapps.push(r.clone());
                            } else {
                                chosen.vapps.retain(|x| x != r);
                            }
                        }
                        ui.indent(("dr-vapp", i, r.to_string()), |ui| {
                            for vm in &app.vms {
                                if let Some(v) = meta.vm(vm) {
                                    ui.label(&v.name_label);
                                }
                            }
                        });
                    }
                    for (r, vm) in meta.loose_vms() {
                        let mut on = chosen.vms.contains(r);
                        if ui.checkbox(&mut on, &vm.name_label).changed() {
                            if on {
                                chosen.vms.push(r.clone());
                            } else {
                                chosen.vms.retain(|x| x != r);
                            }
                        }
                    }
                });
            });
        }
        ui.horizontal(|ui| {
            let any = self.chosen.iter().any(|c| !c.is_empty());
            if ui.button(s.select_all).clicked() {
                for (i, meta) in self.metadata.iter().enumerate() {
                    if Some(&meta.pool.uuid) == my_pool.as_ref() {
                        continue;
                    }
                    self.chosen[i] = Chosen {
                        vapps: meta.vm_appliances.iter().map(|(r, _)| r.clone()).collect(),
                        vms: meta
                            .loose_vms()
                            .into_iter()
                            .map(|(r, _)| r.clone())
                            .collect(),
                    };
                }
            }
            if ui.add_enabled(any, Button::new(s.clear_all)).clicked() {
                for c in &mut self.chosen {
                    *c = Chosen::default();
                }
            }
        });
        ui.add_space(6.0);
        ui.strong(s.power_state);
        let options: Vec<AfterRecovery> = if self.kind == Some(WizardType::Dryrun) {
            vec![AfterRecovery::StartPaused, AfterRecovery::DoNotStart]
        } else {
            vec![AfterRecovery::Start, AfterRecovery::DoNotStart]
        };
        for o in options {
            if ui.radio(self.after == o, self.power_text(o)).clicked() {
                self.after = o;
            }
        }
    }

    fn prechecks_ui(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let s = dr_strings();
        let back = self.kind == Some(WizardType::Failback);
        ui.heading(if back {
            s.prechecks_title_failback
        } else {
            s.prechecks_title_failover
        });
        ui.label(if back {
            s.prechecks_failback
        } else {
            s.prechecks_failover
        });
        if !self.checks_done {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(s.running_prechecks);
            });
        }
        ui.checkbox(&mut self.hide_successful, s.hide_successful);
        let mut fix = None;
        Grid::new("dr-prechecks")
            .num_columns(2)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                for line in &self.checks {
                    match line {
                        PrecheckLine::Header(text) => {
                            ui.strong(text);
                            ui.label("");
                            ui.end_row();
                        }
                        PrecheckLine::Check(row) => {
                            let ok = row.problem.is_none();
                            if ok && self.hide_successful {
                                continue;
                            }
                            let text = check_text(row);
                            match &row.problem {
                                None => ui.label(text),
                                Some(p) if p.is_warning() => {
                                    ui.colored_label(ui.visuals().warn_fg_color, text)
                                }
                                Some(_) => ui.colored_label(ui.visuals().error_fg_color, text),
                            };
                            match row.problem.as_ref().and_then(help_text) {
                                Some(help) if !self.busy() => {
                                    if ui.link(help).clicked() {
                                        fix = row.problem.clone();
                                    }
                                }
                                _ => {
                                    ui.label("");
                                }
                            }
                            ui.end_row();
                        }
                    }
                }
            });
        let problems: Vec<Problem> = self
            .problems()
            .filter(|p| !p.is_warning())
            .cloned()
            .collect();
        if self.checks_done && !problems.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, s.must_resolve);
        }
        if let Some((text, _, _)) = self.question.clone() {
            ui.colored_label(ui.visuals().warn_fg_color, text);
            ui.horizontal(|ui| {
                if ui.button(s.yes).clicked() {
                    self.question = None;
                    if let Some(f) = self.pending_fix.take() {
                        self.dispatch_fix(state, conn, f, ops, backend);
                    }
                }
                if ui.button(s.no).clicked() {
                    self.question = None;
                    self.pending_fix = None;
                    match self.fix_queue.pop_front() {
                        Some(next) => self.start_fix(state, conn, next, ops, backend),
                        None => self.recheck = true,
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            let idle = self.checks_done && !self.busy() && self.question.is_none();
            if ui.add_enabled(idle, Button::new(s.check_again)).clicked() {
                self.recheck = true;
            }
            let fixable = !problems.is_empty() && problems.iter().all(Problem::is_fixable);
            if ui
                .add_enabled(idle && fixable, Button::new(s.resolve_all))
                .clicked()
            {
                let fixes: Vec<Fix> = problems.iter().filter_map(|p| self.fix(conn, p)).collect();
                self.fix_queue = fixes.into();
                if let Some(first) = self.fix_queue.pop_front() {
                    self.start_fix(state, conn, first, ops, backend);
                }
            }
        });
        if let Some(p) = fix
            && let Some(f) = self.fix(conn, &p)
        {
            self.start_fix(state, conn, f, ops, backend);
        }
        if self.checks_done && self.problems().all(Problem::is_warning) {
            ui.label(match self.kind {
                Some(WizardType::Failback) => s.continue_failback,
                Some(WizardType::Dryrun) => s.continue_dryrun,
                _ => s.continue_failover,
            });
        }
    }

    fn recover_ui(&mut self, ui: &mut Ui, pool: &str) {
        let s = dr_strings();
        let product = crate::branding::PLATFORM_NAME;
        let (title, blurb) = match self.kind {
            Some(WizardType::Failback) => (
                fill(s.recover_title_failback, &[("pool", pool)]),
                s.recover_blurb_failback.to_owned(),
            ),
            Some(WizardType::Dryrun) => (
                fill(s.recover_title_dryrun, &[("pool", pool)]),
                s.recover_blurb_dryrun.to_owned(),
            ),
            _ => (
                fill(s.recover_title_failover, &[("product", product)]),
                fill(s.recover_blurb_failover, &[("product", product)]),
            ),
        };
        ui.heading(title);
        ui.label(blurb);
        ui.add_space(4.0);
        Grid::new("dr-recover")
            .num_columns(2)
            .striped(true)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.strong(s.col_task);
                ui.strong(s.col_status);
                ui.end_row();
                for row in &self.rows {
                    ui.label(&row.title);
                    match &row.state {
                        RowState::Pending => ui.label(s.status_pending),
                        RowState::Working => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(s.status_working)
                            })
                            .inner
                        }
                        RowState::Done => ui.label(s.status_completed),
                        RowState::Failed(e) => ui
                            .colored_label(ui.visuals().error_fg_color, s.status_failed)
                            .on_hover_text(e),
                    };
                    ui.end_row();
                }
            });
        let done = self
            .rows
            .iter()
            .filter(|r| matches!(r.state, RowState::Done | RowState::Failed(_)))
            .count();
        ui.label(fill(
            s.overall,
            &[
                ("done", &done.to_string()),
                ("count", &self.rows.len().to_string()),
            ],
        ));
        if let Some(c) = &self.completed {
            ui.strong(c);
        }
        if self.cleanup.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(s.report_dr_cleanup);
            });
        }
    }

    fn take(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if let Some(d) = self.iscsi.as_mut()
            && d.take(id, result)
        {
            return true;
        }
        let s = dr_strings();
        match (&mut self.waiting, result) {
            (Waiting::Srs(p), PrecheckResult::DrSupportedSrs(rows)) if *p == id => {
                self.supported = rows
                    .iter()
                    .filter(|(_, ok)| *ok)
                    .map(|(r, _)| r.clone())
                    .collect();
                self.waiting = Waiting::Nothing;
            }
            (Waiting::Scan(p, sr_type, config), PrecheckResult::DrProbe(answer)) if *p == id => {
                match answer {
                    Ok(found) if !found.srs.is_empty() => {
                        for sr in &found.srs {
                            if self.scanned.iter().any(|x| x.uuid == sr.uuid) {
                                continue;
                            }
                            self.scanned.push(ScannedSr {
                                uuid: sr.uuid.clone(),
                                name_label: sr.name_label.clone(),
                                name_description: sr.name_description.clone(),
                                pool_metadata_detected: sr.pool_metadata_detected,
                                sr_type: sr_type.clone(),
                                device_config: config.clone(),
                            });
                            // `AddScanResultsToDataGridView`: ticked when it
                            // holds metadata.
                            if sr.pool_metadata_detected && !self.selected_srs.contains(&sr.uuid) {
                                self.selected_srs.push(sr.uuid.clone());
                            }
                        }
                        self.notice = None;
                    }
                    Ok(_) => self.notice = Some(s.scan_none.to_owned()),
                    Err(e) => self.error = Some(i18n::describe_xapi_error(e)),
                }
                self.waiting = Waiting::Nothing;
            }
            (Waiting::Metadata(p), PrecheckResult::DrMetadata(answer)) if *p == id => {
                match answer {
                    Ok(m) => self.metadata = m.clone(),
                    Err(e) => {
                        self.error = Some(i18n::describe_xapi_error(e));
                        self.metadata.clear();
                    }
                }
                self.waiting = Waiting::Nothing;
                self.page = Page::VApps;
                self.chosen = vec![Chosen::default(); self.metadata.len()];
                self.after = if self.kind == Some(WizardType::Dryrun) {
                    AfterRecovery::StartPaused
                } else {
                    AfterRecovery::Start
                };
            }
            (Waiting::Checks(pending), PrecheckResult::DrRecoverable(answer))
                if pending.contains_key(&id) =>
            {
                if let Some(i) = pending.remove(&id) {
                    self.answers
                        .insert(i, answer.clone().map_err(|e| i18n::describe_xapi_error(&e)));
                }
            }
            _ => return false,
        }
        true
    }
}

/// What the wizard tells the window.
#[derive(Debug, PartialEq)]
enum WizardOutcome {
    Stay,
    Close,
    Cancel,
    Orphan(Cleanup),
}

impl PartialEq for Cleanup {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

// ---- the dialogs together ------------------------------------------------------------------

/// The F12b disaster recovery dialogs of the window.
#[derive(Default)]
pub struct DrUi {
    pub(crate) configure: Option<ConfigureDialog>,
    pub(crate) wizard: Option<DrWizard>,
    /// Clean-ups of cancelled wizards still running.
    orphans: Vec<Cleanup>,
    next_id: u64,
    pub errors: Vec<String>,
}

impl DrUi {
    pub fn open(&mut self, conn: &ConnectionView, request: DialogRequest, backend: &BackendHandle) {
        match request {
            DialogRequest::ConfigureDr => {
                self.next_id += 1;
                let id = ID_BASE + self.next_id;
                backend.send(Command::Precheck {
                    connection: conn.id,
                    id,
                    check: Precheck::DrSupportedSrs {
                        srs: rules::candidate_srs(&conn.model),
                    },
                });
                self.configure = Some(ConfigureDialog {
                    connection: conn.id,
                    pending: Some(id),
                    supported: None,
                    rows: Vec::new(),
                });
            }
            DialogRequest::DrWizard => {
                self.wizard = Some(DrWizard::new(conn));
            }
            _ => {}
        }
    }

    /// Takes the answer to a question of these dialogs.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if let Some(d) = self.configure.as_mut()
            && d.take(id, result)
        {
            return true;
        }
        if let Some(w) = self.wizard.as_mut() {
            return w.take(id, result);
        }
        false
    }

    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(d) = self.configure.as_mut() {
            let id = d.connection;
            match d.show(ctx, state) {
                Ok(Some(request)) => {
                    if let Some(conn) = state.connection(id) {
                        ops.dispatch(conn, request, backend);
                    }
                    self.configure = None;
                }
                Ok(None) => {}
                Err(()) => self.configure = None,
            }
        }
        if let Some(w) = self.wizard.as_mut() {
            if std::mem::take(&mut w.recheck) {
                w.run_checks(backend, &mut self.next_id);
            }
            if std::mem::take(&mut w.hba_scan) {
                let id = w.ask(
                    backend,
                    &mut self.next_id,
                    Precheck::DrProbe {
                        sr_type: "lvmohba".into(),
                        device_config: DeviceConfig::default(),
                    },
                );
                w.waiting = Waiting::Scan(id, "lvmohba".into(), DeviceConfig::default());
            }
            match w.show(ctx, state, ops, backend, &mut self.next_id) {
                WizardOutcome::Stay => {}
                WizardOutcome::Close | WizardOutcome::Cancel => self.wizard = None,
                WizardOutcome::Orphan(cleanup) => {
                    self.orphans.push(cleanup);
                    self.wizard = None;
                }
            }
        }
        self.orphans.retain_mut(|c| !c.step(state, ops, backend));
    }
}
