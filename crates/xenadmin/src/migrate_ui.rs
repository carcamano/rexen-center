//! The Migrate, Move and Copy VM wizard in the window (F12a), as the
//! original's `CrossPoolMigrateWizard` (see [`xen_core::migrate`] for the
//! rules):
//!
//! * **Menus.** "Migrate VM wizard..." at the end of the Migrate to Server
//!   submenu (`CrossPoolMigrateCommand`); Move VM... / Migrate VM... of a
//!   halted or suspended VM (`MoveVMCommand`: the wizard when the VM may
//!   migrate, else the Move VM dialog); Copy VM... / Copy... of a halted VM
//!   or a template that may migrate (`CopyVMCommand`: the wizard in Copy
//!   mode, else the F7 Copy dialog).
//! * **Pages.** Copy mode only: Destination (Within pool / Cross-pool),
//!   then Name and Storage for a copy within the pool. Otherwise:
//!   Destination Pool (each connected pool or standalone server, and per
//!   VM its servers, each checked as `CrossPoolMigrateFilter` does, the
//!   refused ones shown with the reason), Permissions (when either
//!   connection's role falls short; blocking), Storage (all disks on one SR
//!   or an SR per disk, with the free space checks), Networking (to
//!   another pool: a network per interface), Migration Network (all but a
//!   move within the pool) and Finish (the summary; errors block Finish).
//! * **Finish** runs, per VM, `VM.migrate_send` (another pool or a storage
//!   motion; `copy=true` in Copy mode), a plain live migration within the
//!   pool, or the Move VM action; a copy within the pool runs the F7 clone
//!   or full copy.

use std::collections::{BTreeMap, HashMap};

use egui::{Button, ComboBox, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Host, Network, Ref, Sr, Vdi, Vm, VmRecord};
use xen_core::migrate::{
    self as rules, DiskRow, FinishRequest, LocalCheck, LocalRefusal, Refusal, ServerVerdict,
    VifRow, VmPlan, VmTarget, WizardMode,
};
use xen_core::objects::{self, VmKind};
use xen_core::provisioning::CopyMode;
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, Precheck,
    PrecheckResult, VmAction,
};

use crate::cpm_strings::cpm_strings;
use crate::i18n::{self, fill};
use crate::ops_ui::{Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::xva_ui::{error_label, nav, page_list, summary};

/// `VMCrossPoolMigrateAction.StaticRBACDependencies`.
const CPM_RBAC: &[&str] = &[
    "host.migrate_receive",
    "vm.migrate_send",
    "vm.assert_can_migrate",
    "task.destroy",
];

/// The copy within the pool (`VMCopyAction`, `VMCloneAction`, `SrRefreshAction`).
const INTRA_COPY_RBAC: &[&str] = &["vm.copy", "vm.clone", "vm.set_name_description", "sr.scan"];

/// Precheck ids of this wizard (apart from the other dialogs').
const ID_BASE: u64 = 1 << 54;

// ---- menu entries ----------------------------------------------------------------------

/// "Migrate VM wizard..." (`CrossPoolMigrateCommand` without a server).
pub fn migrate_wizard_entry(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Entry {
    let busy = conn.busy_vms.contains(vm_ref);
    let (enabled, hint) = match rules::can_migrate_send(&conn.model, vm) {
        Ok(()) => (rules::can_open_migrate(&conn.model, vm, busy), None),
        Err(why) => (false, Some(cant_migrate_text(vm, why))),
    };
    Entry::Named {
        label: cpm_strings().menu_cpm.to_owned(),
        enabled: conn.status.is_connected() && enabled,
        hint,
        dialog: DialogRequest::Migrate {
            vms: vec![vm_ref.clone()],
            mode: WizardMode::Migrate,
        },
    }
}

/// Move VM... / Migrate VM... (`MoveVMCommand`).
pub fn move_entry(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Entry {
    let c = cpm_strings();
    let busy = conn.busy_vms.contains(vm_ref);
    Entry::Named {
        label: match rules::move_mode(vm) {
            WizardMode::Migrate => c.menu_migrate_vm,
            _ => c.menu_move_vm,
        }
        .to_owned(),
        enabled: conn.status.is_connected() && rules::can_move(&conn.model, vm, busy),
        hint: None,
        dialog: DialogRequest::MoveVm(vm_ref.clone()),
    }
}

/// `MIGRATION_NOT_ALLOWED` / `MIGRATION_NOT_ALLOWED_USUPPORTED_SR`.
fn cant_migrate_text(vm: &VmRecord, why: rules::CantMigrate) -> String {
    let c = cpm_strings();
    match why {
        rules::CantMigrate::NotAllowed => fill(
            c.migration_not_allowed,
            &[
                ("vm", &vm.name_label),
                ("tools", crate::prov_strings::prov_strings().tools_name),
            ],
        ),
        rules::CantMigrate::UnsupportedSr => {
            fill(c.migration_unsupported_sr, &[("vm", &vm.name_label)])
        }
    }
}

/// Whether this module opens `request` (a Copy of a VM that may migrate
/// opens the wizard instead of the F7 dialog).
pub fn takes(conn: &ConnectionView, request: &DialogRequest) -> bool {
    match request {
        DialogRequest::Migrate { .. } | DialogRequest::MoveVm(_) => true,
        DialogRequest::CopyVm(vm) => conn
            .model
            .vms
            .get(vm)
            .is_some_and(|v| rules::copy_uses_wizard(&conn.model, v)),
        _ => false,
    }
}

// ---- the wizard --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    CopyMode,
    IntraCopy,
    Destination,
    Rbac,
    Storage,
    Network,
    Transfer,
    Finish,
}

/// A server check: asked, or answered (`None`: fine).
#[derive(Debug, Clone)]
enum Verdict {
    Asking,
    Done(Option<Refusal>),
}

/// How a destination or server shows in a list.
#[derive(Debug, Clone)]
pub(crate) enum ItemState {
    Checking,
    Ok,
    Refused(String),
}

/// The copy within the pool (`IntraPoolCopyPage`).
pub(crate) struct IntraCopy {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) full: bool,
    can_fast: bool,
    can_full: bool,
    /// A template that neither fast clones nor copies: "Clone" only.
    slow: bool,
    pub(crate) sr: Option<Ref<Sr>>,
}

/// The wizard (`CrossPoolMigrateWizard`).
pub struct MigrateWizard {
    pub(crate) source: ConnectionId,
    pub(crate) vms: Vec<Ref<Vm>>,
    pub(crate) mode: WizardMode,
    templates_only: bool,
    pub(crate) page: Page,
    /// Copy mode: Within pool chosen.
    pub(crate) intra: bool,
    pub(crate) copy: IntraCopy,
    /// The destination pool (its connection).
    pub(crate) destination: Option<ConnectionId>,
    /// Per VM, its server (`None`: "None selected").
    pub(crate) hosts: BTreeMap<Ref<Vm>, Option<Ref<Host>>>,
    verdicts: HashMap<(ConnectionId, Ref<Vm>, Ref<Host>), Verdict>,
    /// The checks asked, by precheck id: their destination.
    pending: HashMap<u64, ConnectionId>,
    next_id: u64,
    pub(crate) all_same: bool,
    pub(crate) same_sr: Option<Ref<Sr>>,
    pub(crate) disk_srs: HashMap<Ref<Vdi>, Option<Ref<Sr>>>,
    /// Per VM and MAC: the destination network.
    pub(crate) networks: HashMap<(Ref<Vm>, String), Option<Ref<Network>>>,
    pub(crate) transfer: Option<Ref<Network>>,
    pub(crate) error: Option<String>,
}

impl MigrateWizard {
    pub(crate) fn new(conn: &ConnectionView, vms: Vec<Ref<Vm>>, mode: WizardMode) -> Self {
        let m = &conn.model;
        let templates_only = !vms.is_empty()
            && vms
                .iter()
                .all(|v| m.vms.get(v).is_some_and(|r| r.is_a_template));
        let c = cpm_strings();
        let first = vms.first().and_then(|v| m.vms.get(v));
        let copy = match first {
            Some(vm) => {
                let template = vm.is_a_template;
                let has_disk = !rules::copy_disks(m, vm).is_empty();
                let allow_copy =
                    !template || vm.allowed_operations.contains(&xapi::VmOperations::Copy);
                let any_fast = vm.allowed_operations.contains(&xapi::VmOperations::Clone);
                let can_full = allow_copy && has_disk;
                let can_fast = !allow_copy || any_fast || !has_disk;
                IntraCopy {
                    name: rules::default_copy_name(m, vm, c.copy_default_name),
                    description: vm.name_description.clone(),
                    full: !can_fast,
                    can_fast,
                    can_full,
                    slow: template && !(any_fast || allow_copy),
                    sr: xen_core::provisioning::vm_disks(m, vm)
                        .first()
                        .map(|(_, _, _, v)| v.sr.clone())
                        .or_else(|| m.pool().map(|(_, p)| p.default_sr.clone())),
                }
            }
            None => IntraCopy {
                name: String::new(),
                description: String::new(),
                full: false,
                can_fast: true,
                can_full: false,
                slow: false,
                sr: None,
            },
        };
        Self {
            source: conn.id,
            vms,
            mode,
            templates_only,
            page: if mode == WizardMode::Copy {
                Page::CopyMode
            } else {
                Page::Destination
            },
            intra: true,
            copy,
            destination: None,
            hosts: BTreeMap::new(),
            verdicts: HashMap::new(),
            pending: HashMap::new(),
            next_id: 0,
            all_same: true,
            same_sr: None,
            disk_srs: HashMap::new(),
            networks: HashMap::new(),
            transfer: None,
            error: None,
        }
    }

    fn same(&self) -> bool {
        self.destination == Some(self.source)
    }

    fn is_template(&self, state: &AppState) -> bool {
        state.connection(self.source).is_some_and(|c| {
            self.vms
                .iter()
                .any(|v| c.model.vms.get(v).is_some_and(|r| r.is_a_template))
        })
    }

    /// The pages, as `UpdateWizardContent` adds and removes them.
    pub(crate) fn pages(&self, state: &AppState) -> Vec<Page> {
        let mut p = Vec::new();
        if self.mode == WizardMode::Copy {
            p.push(Page::CopyMode);
            if self.intra {
                if self.rbac_needed(state) {
                    p.push(Page::Rbac);
                }
                p.push(Page::IntraCopy);
                return p;
            }
        }
        p.push(Page::Destination);
        if self.destination.is_some() && self.rbac_needed(state) {
            p.push(Page::Rbac);
        }
        p.push(Page::Storage);
        if self.destination.is_some() && !self.same() {
            p.push(Page::Network);
        }
        if !(self.mode == WizardMode::Move && self.same()) {
            p.push(Page::Transfer);
        }
        p.push(Page::Finish);
        p
    }

    fn page_name(p: Page) -> &'static str {
        let c = cpm_strings();
        match p {
            Page::CopyMode => c.page_copy_mode,
            Page::IntraCopy => c.page_intra_copy,
            Page::Destination => c.page_destination,
            Page::Rbac => c.page_rbac,
            Page::Storage => c.page_storage,
            Page::Network => c.page_network,
            Page::Transfer => c.page_transfer,
            Page::Finish => c.page_finish,
        }
    }

    /// `Helpers.ConnectionRequiresRbac` of the source or the destination.
    fn rbac_needed(&self, state: &AppState) -> bool {
        let needs = |c: ConnectionId| {
            state
                .connection(c)
                .and_then(|v| v.rbac.as_ref())
                .is_some_and(|r| !r.is_local_superuser)
        };
        needs(self.source)
            || (!(self.mode == WizardMode::Copy && self.intra)
                && self.destination.is_some_and(needs))
    }

    /// The blocking RBAC messages (one per connection that falls short).
    fn rbac_blocked(&self, state: &AppState) -> Vec<String> {
        let c = cpm_strings();
        let template = self.is_template(state);
        let (text, methods, conns) = if self.mode == WizardMode::Copy && self.intra {
            (
                if template {
                    c.rbac_intra_copy_template
                } else {
                    c.rbac_intra_copy_vm
                },
                INTRA_COPY_RBAC,
                vec![self.source],
            )
        } else {
            let text = match (self.mode, template) {
                (WizardMode::Copy, true) => c.rbac_copy_template,
                (WizardMode::Copy, false) => c.rbac_copy_vm,
                (_, true) => c.rbac_migrate_template,
                (_, false) => c.rbac_migrate_vm,
            };
            let mut conns = vec![self.source];
            if let Some(d) = self.destination
                && d != self.source
            {
                conns.push(d);
            }
            (text, CPM_RBAC, conns)
        };
        conns
            .into_iter()
            .filter_map(|id| state.connection(id))
            .filter_map(
                |conn| match rbac::check(&conn.model, conn.rbac.as_deref(), methods) {
                    RbacCheck::Allowed => None,
                    RbacCheck::Denied { .. } => Some(fill(
                        c.rbac_on_server,
                        &[
                            ("server", &connection_name(conn)),
                            (
                                "text",
                                &fill(text, &[("role", &i18n::session_roles_text(conn))]),
                            ),
                        ],
                    )),
                },
            )
            .collect()
    }

    // ---- destination checks ----------------------------------------------------------

    /// The state of a destination (`host: None`: a pool) for `vms`,
    /// asking the servers what is not known yet.
    fn item_state(
        &mut self,
        state: &AppState,
        backend: &BackendHandle,
        dest: ConnectionId,
        host: Option<&Ref<Host>>,
        vms: &[Ref<Vm>],
    ) -> ItemState {
        let (Some(src), Some(dst)) = (state.connection(self.source), state.connection(dest)) else {
            return ItemState::Refused(cpm_strings().target_disconnected.to_owned());
        };
        let same = dest == self.source;
        let mut ask: Vec<(Ref<Vm>, Ref<Host>)> = Vec::new();
        let mut result = ItemState::Ok;
        for vm_ref in vms {
            let Some(vm) = src.model.vms.get(vm_ref) else {
                continue;
            };
            if rules::is_current_server(&src.model, vm, &dst.model, host, same) {
                return ItemState::Refused(cpm_strings().current_server.to_owned());
            }
            if self.mode == WizardMode::Move && same {
                continue;
            }
            let mut ok = false;
            let mut asking = false;
            let mut refusal: Option<String> = None;
            let mut decided = false;
            for h in rules::collate_hosts(&dst.model, host) {
                match rules::local_check(&src.model, vm, &dst.model, &h, self.mode, same) {
                    LocalCheck::Skip => {}
                    LocalCheck::Refused(r) => {
                        decided = true;
                        refusal = Some(local_text(&r));
                    }
                    LocalCheck::Ask => {
                        decided = true;
                        match self.verdicts.get(&(dest, vm_ref.clone(), h.clone())) {
                            None => {
                                ask.push((vm_ref.clone(), h.clone()));
                                asking = true;
                            }
                            Some(Verdict::Asking) => asking = true,
                            Some(Verdict::Done(None)) => {
                                ok = true;
                                break;
                            }
                            Some(Verdict::Done(Some(r))) => {
                                refusal = Some(refusal_text(Some(src), r));
                            }
                        }
                    }
                }
            }
            if ok || !decided {
                continue;
            }
            if asking {
                result = ItemState::Checking;
            } else if let Some(why) = refusal {
                if !ask.is_empty() {
                    self.send_checks(backend, dest, ask);
                }
                return ItemState::Refused(why);
            }
        }
        if !ask.is_empty() {
            self.send_checks(backend, dest, ask);
        }
        result
    }

    fn send_checks(
        &mut self,
        backend: &BackendHandle,
        dest: ConnectionId,
        checks: Vec<(Ref<Vm>, Ref<Host>)>,
    ) {
        for (vm, host) in &checks {
            self.verdicts
                .insert((dest, vm.clone(), host.clone()), Verdict::Asking);
        }
        self.next_id += 1;
        let id = ID_BASE + self.next_id;
        self.pending.insert(id, dest);
        backend.send(Command::Precheck {
            connection: self.source,
            id,
            check: Precheck::Migrate {
                target: dest,
                mode: self.mode,
                checks,
            },
        });
    }

    /// Takes the answer to a check of this wizard.
    pub(crate) fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        let Some(dest) = self.pending.remove(&id) else {
            return false;
        };
        if let PrecheckResult::Migrate(rows) = result {
            for (vm, host, verdict) in rows {
                let refusal = match verdict {
                    ServerVerdict::Ok => None,
                    ServerVerdict::Refused { error, boot } => Some(Refusal::Server {
                        error: error.clone(),
                        boot: boot.clone(),
                    }),
                };
                self.verdicts
                    .insert((dest, vm.clone(), host.clone()), Verdict::Done(refusal));
            }
        }
        true
    }

    // ---- pages ----------------------------------------------------------------------------

    fn copy_mode_page(&mut self, ui: &mut Ui, state: &AppState) {
        let c = cpm_strings();
        ui.heading(c.copy_mode_title);
        ui.label(if self.templates_only && self.vms.len() == 1 {
            c.copy_rubric_template
        } else {
            c.copy_rubric_vm
        });
        ui.add_space(8.0);
        let restricted = state
            .connection(self.source)
            .is_some_and(|conn| rules::cross_pool_restricted(&conn.model));
        if restricted {
            self.intra = true;
        }
        if ui.radio(self.intra, c.within_pool).clicked() {
            self.intra = true;
        }
        ui.label(RichText::new(c.within_pool_text).weak());
        ui.add_enabled_ui(!restricted, |ui| {
            if ui.radio(!self.intra, c.cross_pool).clicked() {
                self.intra = false;
            }
            ui.label(RichText::new(c.cross_pool_text).weak());
        });
        if restricted {
            ui.colored_label(ui.visuals().warn_fg_color, c.cross_pool_licence);
        }
    }

    fn intra_copy_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let c = cpm_strings();
        let template = self.templates_only;
        ui.heading(c.intra_title);
        ui.label(if template {
            c.intra_rubric_template
        } else {
            c.intra_rubric_vm
        });
        ui.add_space(6.0);
        Grid::new("cpm-copy-form").num_columns(2).show(ui, |ui| {
            let l = ui.label(c.copy_name);
            ui.add(TextEdit::singleline(&mut self.copy.name).desired_width(360.0))
                .labelled_by(l.id);
            ui.end_row();
            let l = ui.label(c.copy_description);
            ui.add(
                TextEdit::multiline(&mut self.copy.description)
                    .desired_rows(2)
                    .desired_width(360.0),
            )
            .labelled_by(l.id);
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.strong(c.copy_mode_group);
        let can_fast = self.copy.can_fast;
        let (clone, clone_text) = if self.copy.slow {
            (c.slow_clone, c.slow_clone_text)
        } else if template {
            (c.fast_clone, c.fast_clone_template)
        } else {
            (c.fast_clone, c.fast_clone_vm)
        };
        ui.add_enabled_ui(can_fast, |ui| {
            if ui.radio(!self.copy.full, clone).clicked() {
                self.copy.full = false;
            }
        })
        .response
        .on_disabled_hover_text(c.fast_clone_unavailable);
        ui.label(RichText::new(clone_text).weak());
        ui.add_enabled_ui(self.copy.can_full, |ui| {
            if ui.radio(self.copy.full, c.full_copy).clicked() {
                self.copy.full = true;
            }
        });
        ui.label(
            RichText::new(if template {
                c.copy_select_sr_template
            } else {
                c.copy_select_sr_vm
            })
            .weak(),
        );
        let srs = self
            .vms
            .first()
            .and_then(|v| conn.model.vms.get(v))
            .map(|v| xen_core::provisioning::disk_srs(&conn.model, Some(v)))
            .unwrap_or_default();
        ui.add_enabled_ui(self.copy.full, |ui| {
            crate::provision_ui::sr_combo(ui, "cpm-copy-sr", &srs, &mut self.copy.sr);
        });
    }

    fn destination_page(&mut self, ui: &mut Ui, state: &AppState, backend: &BackendHandle) {
        let c = cpm_strings();
        ui.heading(c.destination_title);
        ui.label(match (self.templates_only, self.mode) {
            (true, _) => c.destination_intro_copy_template,
            (false, WizardMode::Copy) => c.destination_intro_copy,
            (false, WizardMode::Move) => c.destination_intro_move,
            (false, WizardMode::Migrate) => c.destination_intro_migrate,
        });
        ui.add_space(6.0);
        // `PopulateComboBox`: every connected pool or standalone server
        // (not the source in Copy mode).
        let candidates: Vec<(ConnectionId, Option<Ref<Host>>, String)> = state
            .connections
            .iter()
            .filter(|c| c.status.is_connected())
            .filter(|c| !(self.mode == WizardMode::Copy && c.id == self.source))
            .filter_map(|c| {
                if objects::pool_is_visible(&c.model) {
                    Some((c.id, None, connection_name(c)))
                } else {
                    let coordinator = c.model.pool().map(|(_, p)| p.master.clone())?;
                    let name = c.model.hosts.get(&coordinator)?.name_label.clone();
                    Some((c.id, Some(coordinator), name))
                }
            })
            .collect();
        let vms = self.vms.clone();
        let mut items: Vec<(ConnectionId, String, ItemState)> = Vec::new();
        for (id, host, name) in candidates {
            let s = self.item_state(state, backend, id, host.as_ref(), &vms);
            items.push((id, name, s));
        }
        // A destination that became refused is chosen no more.
        if let Some(d) = self.destination
            && !items
                .iter()
                .any(|(id, _, s)| *id == d && !matches!(s, ItemState::Refused(_)))
        {
            self.destination = None;
            self.hosts.clear();
        }
        let shown = self
            .destination
            .and_then(|d| items.iter().find(|(id, _, _)| *id == d))
            .map(|(_, n, _)| n.clone())
            .unwrap_or_else(|| c.destination_choose.to_owned());
        let mut chosen = None;
        ui.horizontal(|ui| {
            ui.label(c.destination_label);
            ComboBox::from_id_salt("cpm-destination")
                .selected_text(shown)
                .width(420.0)
                .show_ui(ui, |ui| {
                    for (id, name, s) in &items {
                        let (text, enabled) = item_text(name, s);
                        let on = self.destination == Some(*id);
                        if ui
                            .add_enabled(enabled, Button::selectable(on, text))
                            .clicked()
                        {
                            chosen = Some(*id);
                        }
                    }
                });
        });
        if items.is_empty() {
            ui.label(RichText::new(c.no_destinations).italics());
        }
        ui.label(RichText::new(c.add_server_hint).weak());
        if let Some(id) = chosen
            && self.destination != Some(id)
        {
            self.destination = Some(id);
            self.hosts.clear();
            self.reset_after_destination();
        }
        if items
            .iter()
            .any(|(_, _, s)| matches!(s, ItemState::Checking))
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        let Some(dest) = self.destination else {
            return;
        };
        ui.add_space(8.0);
        ui.label(match (self.templates_only, self.mode) {
            (true, _) => c.table_intro_templates,
            (false, WizardMode::Copy) => c.table_intro_copy,
            (false, WizardMode::Move) => c.table_intro_move,
            (false, WizardMode::Migrate) => c.table_intro_migrate,
        });
        let (Some(src), Some(dst)) = (state.connection(self.source), state.connection(dest)) else {
            return;
        };
        let src = src.clone();
        let dst = dst.clone();
        let shared = has_pool_shared_storage(&dst.model);
        let hosts: Vec<(Ref<Host>, String)> = objects::sorted_hosts(&dst.model)
            .into_iter()
            .map(|(r, h)| (r.clone(), h.name_label.clone()))
            .collect();
        let standalone_host = (!objects::pool_is_visible(&dst.model))
            .then(|| dst.model.pool().map(|(_, p)| p.master.clone()))
            .flatten();
        Grid::new("cpm-targets")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.strong(if self.templates_only {
                    c.col_template
                } else {
                    c.col_vm
                });
                ui.strong(c.col_server);
                ui.end_row();
                for vm_ref in vms.clone() {
                    let name = src
                        .model
                        .vms
                        .get(&vm_ref)
                        .map(|v| v.name_label.clone())
                        .unwrap_or_default();
                    ui.label(&name);
                    let mut options: Vec<(Option<Ref<Host>>, String, ItemState)> = Vec::new();
                    if shared {
                        options.push((None, c.none_selected.to_owned(), ItemState::Ok));
                    }
                    for (h, hname) in &hosts {
                        let s = self.item_state(
                            state,
                            backend,
                            dest,
                            Some(h),
                            std::slice::from_ref(&vm_ref),
                        );
                        options.push((Some(h.clone()), hname.clone(), s));
                    }
                    // `SetComboBoxPreSelection`: the standalone server, else
                    // the first usable choice.
                    let current = self.hosts.get(&vm_ref).cloned();
                    let usable = |h: &Option<Ref<Host>>| {
                        options
                            .iter()
                            .any(|(o, _, s)| o == h && matches!(s, ItemState::Ok))
                    };
                    let current = match current {
                        Some(h) if usable(&h) => Some(h),
                        _ => {
                            let preferred = standalone_host
                                .clone()
                                .map(Some)
                                .filter(|h| usable(h))
                                .or_else(|| {
                                    options
                                        .iter()
                                        .find(|(_, _, s)| matches!(s, ItemState::Ok))
                                        .map(|(h, _, _)| h.clone())
                                });
                            match preferred {
                                Some(p) => {
                                    self.hosts.insert(vm_ref.clone(), p.clone());
                                    Some(p)
                                }
                                None => {
                                    self.hosts.remove(&vm_ref);
                                    None
                                }
                            }
                        }
                    };
                    let shown = current
                        .as_ref()
                        .and_then(|h| options.iter().find(|(o, _, _)| o == h))
                        .map(|(_, n, _)| n.clone())
                        .unwrap_or_default();
                    ComboBox::from_id_salt(("cpm-target", vm_ref.as_str()))
                        .selected_text(shown)
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for (h, n, s) in &options {
                                let (text, enabled) = item_text(n, s);
                                let on = current.as_ref() == Some(h);
                                if ui
                                    .add_enabled(enabled, Button::selectable(on, text))
                                    .clicked()
                                {
                                    self.hosts.insert(vm_ref.clone(), h.clone());
                                }
                            }
                        });
                    ui.end_row();
                }
            });
    }

    /// `NotifyNextPagesOfChange`: the later pages start over.
    fn reset_after_destination(&mut self) {
        self.all_same = true;
        self.same_sr = None;
        self.disk_srs.clear();
        self.networks.clear();
        self.transfer = None;
    }

    fn vm_host(&self, vm: &Ref<Vm>) -> Option<Ref<Host>> {
        self.hosts.get(vm).cloned().flatten()
    }

    /// The Storage page's rows with their offered SRs.
    fn disk_rows(&self, src: &ConnectionView) -> Vec<DiskRow> {
        self.vms
            .iter()
            .filter_map(|v| src.model.vms.get_key_value(v))
            .flat_map(|(r, vm)| rules::disk_rows(&src.model, r, vm))
            .collect()
    }

    fn storage_page(
        &mut self,
        ui: &mut Ui,
        src: &ConnectionView,
        dst: &ConnectionView,
        show_hidden: bool,
    ) {
        let c = cpm_strings();
        ui.heading(c.storage_title);
        ui.label(c.storage_intro);
        ui.add_space(6.0);
        let same = self.same();
        let rows = self.disk_rows(src);
        let hosts: Vec<Option<Ref<Host>>> = self.vms.iter().map(|v| self.vm_host(v)).collect();
        let common = rules::common_srs(
            &src.model,
            &dst.model,
            &rows,
            &hosts,
            self.mode,
            same,
            show_hidden,
        );
        if common.is_empty() {
            self.all_same = false;
        }
        if self.same_sr.is_none() {
            // `DetermineDefaultSrPositionInComboBox`.
            self.same_sr = common
                .iter()
                .find(|(r, _, ok)| *ok && xen_core::storage::is_default_sr(&dst.model, r))
                .or_else(|| common.first())
                .map(|(r, _, _)| (*r).clone());
        }
        let sr_text = |sr: &xapi::SrRecord| {
            fill(
                c.sr_item,
                &[
                    ("sr", &sr.name_label),
                    (
                        "free",
                        &xen_core::format::disk_size(xen_core::provisioning::sr_free(sr)),
                    ),
                ],
            )
        };
        ui.add_enabled_ui(!common.is_empty(), |ui| {
            if ui.radio(self.all_same, c.all_on_same_sr).clicked() {
                self.all_same = true;
            }
        });
        ui.add_enabled_ui(self.all_same && !common.is_empty(), |ui| {
            let shown = self
                .same_sr
                .as_ref()
                .and_then(|r| common.iter().find(|(x, _, _)| *x == r))
                .map(|(_, s, ok)| {
                    if *ok {
                        sr_text(s)
                    } else {
                        fill(c.sr_not_enough, &[("sr", &sr_text(s))])
                    }
                })
                .unwrap_or_else(|| c.no_common_sr.to_owned());
            ComboBox::from_id_salt("cpm-same-sr")
                .selected_text(shown)
                .width(420.0)
                .show_ui(ui, |ui| {
                    for (r, s, ok) in &common {
                        let text = if *ok {
                            sr_text(s)
                        } else {
                            fill(c.sr_not_enough, &[("sr", &sr_text(s))])
                        };
                        if ui
                            .selectable_label(self.same_sr.as_ref() == Some(*r), text)
                            .clicked()
                        {
                            self.same_sr = Some((*r).clone());
                        }
                    }
                });
        });
        if ui.radio(!self.all_same, c.specific_srs).clicked() {
            self.all_same = false;
        }
        let names: HashMap<Ref<Vm>, String> = self
            .vms
            .iter()
            .filter_map(|v| {
                src.model
                    .vms
                    .get(v)
                    .map(|r| (v.clone(), r.name_label.clone()))
            })
            .collect();
        ui.add_enabled_ui(!self.all_same, |ui| {
            Grid::new("cpm-disks")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.strong(if self.templates_only {
                        c.col_disk_template
                    } else {
                        c.col_disk_vm
                    });
                    ui.strong(c.col_sr);
                    ui.end_row();
                    let mut index: HashMap<Ref<Vm>, usize> = HashMap::new();
                    for row in &rows {
                        let vm_name = names.get(&row.vm).cloned().unwrap_or_default();
                        let i = index.entry(row.vm.clone()).or_insert(0);
                        ui.label(if row.name.is_empty() {
                            fill(
                                c.disk_unnamed,
                                &[("vm", &vm_name), ("index", &i.to_string())],
                            )
                        } else {
                            fill(c.disk_label, &[("vm", &vm_name), ("disk", &row.name)])
                        });
                        *i += 1;
                        let host = self.vm_host(&row.vm);
                        let srs = rules::target_srs(
                            &src.model,
                            &dst.model,
                            row,
                            host.as_ref(),
                            self.mode,
                            same,
                            show_hidden,
                        );
                        if srs.is_empty() {
                            ui.label(RichText::new(c.no_sr).italics());
                            self.disk_srs.insert(row.vdi.clone(), None);
                            ui.end_row();
                            continue;
                        }
                        let entry = self.disk_srs.entry(row.vdi.clone()).or_insert(None);
                        if entry
                            .as_ref()
                            .is_none_or(|r| !srs.iter().any(|(x, _)| *x == r))
                        {
                            *entry = rules::default_sr(&dst.model, &srs).cloned();
                        }
                        let shown = entry
                            .as_ref()
                            .and_then(|r| srs.iter().find(|(x, _)| *x == r))
                            .map(|(_, s)| sr_text(s))
                            .unwrap_or_default();
                        ComboBox::from_id_salt(("cpm-disk-sr", row.vdi.as_str()))
                            .selected_text(shown)
                            .width(360.0)
                            .show_ui(ui, |ui| {
                                for (r, s) in &srs {
                                    if ui
                                        .selectable_label(entry.as_ref() == Some(*r), sr_text(s))
                                        .clicked()
                                    {
                                        *entry = Some((*r).clone());
                                    }
                                }
                            });
                        ui.end_row();
                    }
                });
        });
        if let Some(e) = self.storage_error(src, dst, show_hidden) {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }

    /// `CheckStorageRequirements` (and the rows without any SR).
    fn storage_error(
        &self,
        src: &ConnectionView,
        dst: &ConnectionView,
        show_hidden: bool,
    ) -> Option<String> {
        let c = cpm_strings();
        let rows = self.disk_rows(src);
        let same = self.same();
        if self.all_same {
            let hosts: Vec<Option<Ref<Host>>> = self.vms.iter().map(|v| self.vm_host(v)).collect();
            let common = rules::common_srs(
                &src.model,
                &dst.model,
                &rows,
                &hosts,
                self.mode,
                same,
                show_hidden,
            );
            let chosen = self
                .same_sr
                .as_ref()
                .and_then(|r| common.iter().find(|(x, _, _)| *x == r));
            return match chosen {
                None => Some(c.no_common_sr.to_owned()),
                Some((_, s, false)) => {
                    Some(fill(c.space_limitation_common, &[("sr", &s.name_label)]))
                }
                Some(_) => None,
            };
        }
        let choices: Vec<Option<Ref<Sr>>> = rows
            .iter()
            .map(|r| self.disk_srs.get(&r.vdi).cloned().flatten())
            .collect();
        if choices.iter().any(Option::is_none) {
            return Some(c.no_sr.to_owned());
        }
        rules::short_of_space(&dst.model, &rows, &choices, self.mode)
            .map(|sr| fill(c.space_limitation, &[("sr", &sr.name_label)]))
    }

    /// The VM's disks and the SR each goes to (`VmMappings` of the Storage
    /// page, with `AddAdditionalMappings`).
    fn storage_plan(&self, src: &ConnectionView, vm_ref: &Ref<Vm>) -> BTreeMap<Ref<Vdi>, Ref<Sr>> {
        let mut out = BTreeMap::new();
        let Some(vm) = src.model.vms.get(vm_ref) else {
            return out;
        };
        for row in rules::disk_rows(&src.model, vm_ref, vm) {
            let sr = if self.all_same {
                self.same_sr.clone()
            } else {
                self.disk_srs.get(&row.vdi).cloned().flatten()
            };
            if let Some(sr) = sr {
                out.insert(row.vdi, sr);
            }
        }
        rules::additional_mappings(&src.model, vm, &mut out, self.same());
        out
    }

    fn vif_rows(&self, src: &ConnectionView) -> Vec<VifRow> {
        self.vms
            .iter()
            .filter_map(|v| src.model.vms.get_key_value(v))
            .flat_map(|(r, vm)| rules::vif_rows(&src.model, r, vm))
            .collect()
    }

    fn network_page(
        &mut self,
        ui: &mut Ui,
        src: &ConnectionView,
        dst: &ConnectionView,
        show_hidden: bool,
    ) {
        let c = cpm_strings();
        ui.heading(c.network_title);
        ui.label(match (self.templates_only, self.vms.len() > 1) {
            (true, true) => c.network_intro_template,
            (true, false) => c.network_intro_template_single,
            (false, true) => c.network_intro,
            (false, false) => c.network_intro_single,
        });
        ui.add_space(6.0);
        ui.label(c.network_table_intro);
        let rows = self.vif_rows(src);
        Grid::new("cpm-vifs")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.strong(if self.templates_only {
                    c.col_vif_template
                } else {
                    c.col_vif_vm
                });
                ui.strong(c.col_target_network);
                ui.end_row();
                for row in &rows {
                    ui.label(format!(
                        "{} - {} ({})",
                        row.owner_name, row.network_name, row.mac
                    ));
                    let host = self.vm_host(&row.vm);
                    let nets = rules::target_networks(&dst.model, host.as_ref(), show_hidden);
                    let key = (row.vm.clone(), row.mac.clone());
                    if nets.is_empty() {
                        ui.label(RichText::new(c.no_networks).italics());
                        self.networks.insert(key, None);
                        ui.end_row();
                        continue;
                    }
                    let entry = self.networks.entry(key).or_insert(None);
                    if entry
                        .as_ref()
                        .is_none_or(|r| !nets.iter().any(|(x, _)| *x == r))
                    {
                        *entry =
                            rules::default_network(&dst.model, &nets, &row.network_name).cloned();
                    }
                    let name = |n: &xapi::NetworkRecord| objects::network_name(&dst.model, n);
                    let shown = entry
                        .as_ref()
                        .and_then(|r| nets.iter().find(|(x, _)| *x == r))
                        .map(|(_, n)| name(n))
                        .unwrap_or_default();
                    ComboBox::from_id_salt(("cpm-vif", row.vif.as_str()))
                        .selected_text(shown)
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for (r, n) in &nets {
                                if ui
                                    .selectable_label(entry.as_ref() == Some(*r), name(n))
                                    .clicked()
                                {
                                    *entry = Some((*r).clone());
                                }
                            }
                        });
                    ui.end_row();
                }
            });
    }

    fn transfer_page(&mut self, ui: &mut Ui, dst: &ConnectionView) {
        let c = cpm_strings();
        ui.heading(c.transfer_title);
        ui.label(if self.templates_only {
            c.transfer_blurb_template
        } else {
            c.transfer_blurb_vm
        });
        ui.add_space(8.0);
        let nets = rules::transfer_networks(&dst.model);
        if self
            .transfer
            .as_ref()
            .is_none_or(|t| !nets.iter().any(|(r, _, _)| *r == t))
        {
            self.transfer = nets
                .iter()
                .find(|(_, _, m)| *m)
                .or_else(|| nets.first())
                .map(|(r, _, _)| (*r).clone());
        }
        let pool = connection_name(dst);
        let label = |n: &xapi::NetworkRecord| {
            fill(
                c.transfer_item,
                &[
                    ("network", &objects::network_name(&dst.model, n)),
                    ("pool", &pool),
                ],
            )
        };
        if nets.is_empty() {
            ui.label(RichText::new(c.no_transfer_networks).italics());
            return;
        }
        ui.horizontal(|ui| {
            ui.label(c.transfer_label);
            let shown = self
                .transfer
                .as_ref()
                .and_then(|t| nets.iter().find(|(r, _, _)| *r == t))
                .map(|(_, n, _)| label(n))
                .unwrap_or_default();
            ComboBox::from_id_salt("cpm-transfer")
                .selected_text(shown)
                .width(380.0)
                .show_ui(ui, |ui| {
                    for (r, n, _) in &nets {
                        if ui
                            .selectable_label(self.transfer.as_ref() == Some(*r), label(n))
                            .clicked()
                        {
                            self.transfer = Some((*r).clone());
                        }
                    }
                });
        });
    }

    /// The plan of each VM (`m_vmMappings`).
    pub(crate) fn plans(&self, src: &ConnectionView) -> Vec<VmPlan> {
        let Some(dest) = self.destination else {
            return Vec::new();
        };
        let same = self.same();
        self.vms
            .iter()
            .map(|vm| VmPlan {
                vm: vm.clone(),
                target: VmTarget {
                    connection: dest,
                    host: self.vm_host(vm),
                },
                storage: self.storage_plan(src, vm),
                // Within the pool XAPI keeps the networks
                // (`ClearAllNetworkMappings`).
                networks: if same {
                    BTreeMap::new()
                } else {
                    self.networks
                        .iter()
                        .filter(|((v, _), _)| v == vm)
                        .filter_map(|((_, mac), n)| n.clone().map(|n| (mac.clone(), n)))
                        .collect()
                },
            })
            .collect()
    }

    /// `GetVMMappingSummary`: the rows and whether any is an error.
    fn summary_rows(
        &self,
        src: &ConnectionView,
        dst: &ConnectionView,
    ) -> (Vec<(String, String)>, bool) {
        let c = cpm_strings();
        let mut rows = Vec::new();
        let mut errors = false;
        let transfer_shown = self.pages_without_state().contains(&Page::Transfer);
        for plan in self.plans(src) {
            let vm_name = src
                .model
                .vms
                .get(&plan.vm)
                .map(|v| v.name_label.clone())
                .unwrap_or_default();
            rows.push((
                if self.templates_only {
                    c.summary_template
                } else {
                    c.summary_vm
                }
                .to_owned(),
                vm_name,
            ));
            // `DestinationPoolSummary`.
            let destination = match &plan.target.host {
                Some(h) => match dst.model.hosts.get(h) {
                    None => c.summary_unknown.to_owned(),
                    Some(host) if objects::pool_is_visible(&dst.model) => {
                        let _ = host;
                        connection_name(dst)
                    }
                    Some(host) => host.name_label.clone(),
                },
                None => connection_name(dst),
            };
            rows.push((c.summary_destination.to_owned(), destination));
            if transfer_shown
                && let Some(t) = self
                    .transfer
                    .as_ref()
                    .and_then(|t| dst.model.networks.get(t))
            {
                rows.push((
                    c.summary_transfer.to_owned(),
                    objects::network_name(&dst.model, t),
                ));
            }
            let mut first = true;
            for (vdi, sr) in &plan.storage {
                let Some(v) = src.model.vdis.get(vdi) else {
                    continue;
                };
                if v.is_a_snapshot {
                    continue;
                }
                let sr_name = dst
                    .model
                    .srs
                    .get(sr)
                    .map(|s| s.name_label.clone())
                    .unwrap_or_default();
                rows.push((
                    if std::mem::take(&mut first) {
                        c.summary_storage.to_owned()
                    } else {
                        String::new()
                    },
                    format!("{} -> {}", v.name_label, sr_name),
                ));
            }
            let vifs = rules::vif_rows(
                &src.model,
                &plan.vm,
                src.model
                    .vms
                    .get(&plan.vm)
                    .map(|v| &**v)
                    .unwrap_or(&VmRecord::default()),
            );
            let mut first = true;
            for (mac, net) in &plan.networks {
                let source = vifs.iter().find(|r| &r.mac == mac);
                let missing = source.is_none_or(|r| !src.model.networks.contains_key(&r.network));
                errors |= missing;
                let from = match source {
                    Some(r) if !missing => r.network_name.clone(),
                    _ => c.summary_network_not_found.to_owned(),
                };
                let to = dst
                    .model
                    .networks
                    .get(net)
                    .map(|n| objects::network_name(&dst.model, n))
                    .unwrap_or_default();
                rows.push((
                    if std::mem::take(&mut first) {
                        c.summary_network.to_owned()
                    } else {
                        String::new()
                    },
                    format!("{from} -> {to}"),
                ));
            }
            rows.push((String::new(), String::new()));
        }
        (rows, errors)
    }

    /// The pages as last computed (for the summary's Migration Network row).
    fn pages_without_state(&self) -> Vec<Page> {
        let mut p = vec![Page::Destination, Page::Storage];
        if !(self.mode == WizardMode::Move && self.same()) {
            p.push(Page::Transfer);
        }
        p
    }

    fn finish_page(&mut self, ui: &mut Ui, src: &ConnectionView, dst: &ConnectionView) {
        let c = cpm_strings();
        ui.heading(c.finish_title);
        let many = self.vms.len() > 1;
        ui.label(
            match (self.templates_only, self.mode == WizardMode::Copy, many) {
                (true, _, true) => c.finish_intro_copy_template,
                (true, _, false) => c.finish_intro_copy_single_template,
                (false, true, true) => c.finish_intro_copy,
                (false, true, false) => c.finish_intro_copy_single,
                (false, false, true) => c.finish_intro,
                (false, false, false) => c.finish_intro_single,
            },
        );
        ui.add_space(6.0);
        let (rows, errors) = self.summary_rows(src, dst);
        summary(ui, "cpm-summary", &rows);
        if errors {
            ui.colored_label(ui.visuals().error_fg_color, c.finish_errors);
        }
    }

    /// Whether Next (or Finish) is allowed on the current page.
    fn page_ok(&self, state: &AppState) -> bool {
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let src = state.connection(self.source);
        let dst = self.destination.and_then(|d| state.connection(d));
        match self.page {
            Page::CopyMode => true,
            Page::IntraCopy => {
                !self.copy.name.trim().is_empty() && (!self.copy.full || self.copy.sr.is_some())
            }
            Page::Destination => {
                self.destination.is_some() && self.vms.iter().all(|v| self.hosts.contains_key(v))
            }
            Page::Rbac => self.rbac_blocked(state).is_empty(),
            Page::Storage => match (src, dst) {
                (Some(s), Some(d)) => self.storage_error(s, d, show_hidden).is_none(),
                _ => false,
            },
            Page::Network => self.networks.values().all(Option::is_some),
            Page::Transfer => self.transfer.is_some(),
            Page::Finish => match (src, dst) {
                (Some(s), Some(d)) => !self.summary_rows(s, d).1,
                _ => false,
            },
        }
    }

    /// The window title (`UpdateWindowTitle`).
    fn title(&self, state: &AppState) -> String {
        let c = cpm_strings();
        let target = self
            .vms
            .first()
            .and_then(|v| self.hosts.get(v).cloned())
            .and_then(|h| {
                let d = state.connection(self.destination?)?;
                Some(match h {
                    Some(h) => d.model.hosts.get(&h)?.name_label.clone(),
                    None => connection_name(d),
                })
            });
        let template = self.is_template(state);
        match (self.mode, target) {
            (WizardMode::Migrate, Some(t)) => fill(c.title_migrate_to, &[("target", &t)]),
            (WizardMode::Migrate, None) => c.title_migrate.to_owned(),
            (WizardMode::Move, Some(t)) => fill(c.title_move_to, &[("target", &t)]),
            (WizardMode::Move, None) => c.title_move.to_owned(),
            (WizardMode::Copy, Some(t)) if template => {
                fill(c.title_copy_template_to, &[("target", &t)])
            }
            (WizardMode::Copy, Some(t)) => fill(c.title_copy_vm_to, &[("target", &t)]),
            (WizardMode::Copy, None) if template => c.title_copy_template.to_owned(),
            (WizardMode::Copy, None) => c.title_copy_vm.to_owned(),
        }
    }

    /// The requests Finish makes (`FinishWizard`).
    pub(crate) fn finish_requests(&self, state: &AppState) -> Result<Vec<ActionRequest>, String> {
        let c = cpm_strings();
        let src = state
            .connection(self.source)
            .ok_or_else(|| c.target_disconnected.to_owned())?;
        if !rules::all_available(&src.model, &self.vms) {
            return Err(fill(
                c.vm_missing,
                &[("console", crate::branding::PRODUCT_NAME)],
            ));
        }
        if self.mode == WizardMode::Copy && self.intra {
            let vm = self.vms.first().cloned().ok_or_else(String::new)?;
            let mode = match (self.copy.full, &self.copy.sr) {
                (true, Some(sr)) => CopyMode::Copy(sr.clone()),
                _ => CopyMode::Clone,
            };
            return Ok(vec![ActionRequest::CopyVm {
                vm,
                name: self.copy.name.trim().to_owned(),
                description: self.copy.description.trim().to_owned(),
                mode,
            }]);
        }
        let dst = self
            .destination
            .and_then(|d| state.connection(d))
            .filter(|d| d.status.is_connected())
            .ok_or_else(|| c.target_disconnected.to_owned())?;
        let mut out = Vec::new();
        for plan in self.plans(src) {
            let request = rules::finish_request(
                &src.model,
                src.id,
                &dst.model,
                &plan,
                self.mode,
                self.transfer.as_ref(),
            )
            .ok_or_else(|| c.target_disconnected.to_owned())?;
            match request {
                FinishRequest::MigrateSend(spec) => {
                    out.push(ActionRequest::MigrateSend(Box::new(spec)));
                }
                FinishRequest::PoolMigrate { vm, host } => out.push(ActionRequest::Vm {
                    vm,
                    action: VmAction::Migrate(host),
                }),
                FinishRequest::Move { vm, host, storage } => {
                    out.push(ActionRequest::MoveVm { vm, host, storage });
                }
                FinishRequest::Nothing => {}
            }
        }
        Ok(out)
    }

    /// `PageLeaveCore` checks: the VMs are still there.
    fn leave(&mut self, state: &AppState) -> bool {
        let c = cpm_strings();
        let ok = state
            .connection(self.source)
            .is_some_and(|s| rules::all_available(&s.model, &self.vms));
        if !ok {
            self.error = Some(fill(
                c.vm_missing,
                &[("console", crate::branding::PRODUCT_NAME)],
            ));
            return false;
        }
        if self.page == Page::Destination
            && self
                .destination
                .and_then(|d| state.connection(d))
                .is_none_or(|d| !d.status.is_connected())
        {
            self.error = Some(c.target_disconnected.to_owned());
            return false;
        }
        true
    }

    /// Draws the wizard; `Ok(Some)` with the requests once finished, `Err`
    /// once cancelled.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
    ) -> Result<Option<Vec<ActionRequest>>, ()> {
        let c = cpm_strings();
        let n = crate::net_strings::net_strings();
        let Some(src) = state.connection(self.source).cloned() else {
            return Err(());
        };
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let pages = self.pages(state);
        if !pages.contains(&self.page) {
            self.page = pages[0];
        }
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let dst = self.destination.and_then(|d| state.connection(d)).cloned();
        let mut open = true;
        let mut outcome = Ok(None);
        let title = self.title(state);
        egui::Window::new(title)
            .id(egui::Id::unique("cpm-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([820.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(660.0);
                        egui::ScrollArea::vertical()
                            .id_salt("cpm-wizard-page")
                            .max_height(390.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match (self.page, dst.as_deref()) {
                                (Page::CopyMode, _) => self.copy_mode_page(ui, state),
                                (Page::IntraCopy, _) => self.intra_copy_page(ui, &src),
                                (Page::Destination, _) => {
                                    self.destination_page(ui, state, backend);
                                }
                                (Page::Rbac, _) => {
                                    ui.heading(c.rbac_title);
                                    ui.label(c.rbac_intro);
                                    ui.add_space(6.0);
                                    let blocked = self.rbac_blocked(state);
                                    if blocked.is_empty() {
                                        ui.label(c.rbac_click_next);
                                    }
                                    for why in blocked {
                                        ui.colored_label(ui.visuals().error_fg_color, why);
                                    }
                                }
                                (Page::Storage, Some(d)) => {
                                    self.storage_page(ui, &src, d, show_hidden);
                                }
                                (Page::Network, Some(d)) => {
                                    self.network_page(ui, &src, d, show_hidden);
                                }
                                (Page::Transfer, Some(d)) => self.transfer_page(ui, d),
                                (Page::Finish, Some(d)) => self.finish_page(ui, &src, d),
                                (_, None) => {
                                    ui.label(c.target_disconnected);
                                }
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.separator();
                let last = index + 1 == pages.len();
                let (previous, next, cancel) = nav(
                    ui,
                    index > 0,
                    self.page_ok(state),
                    if last { n.finish } else { n.next },
                );
                if previous {
                    self.error = None;
                    self.page = pages[index - 1];
                }
                if next && self.leave(state) {
                    self.error = None;
                    if last {
                        match self.finish_requests(state) {
                            Ok(requests) => outcome = Ok(Some(requests)),
                            Err(e) => self.error = Some(e),
                        }
                    } else {
                        // The choice may have changed the pages.
                        let pages = self.pages(state);
                        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
                        self.page = pages[(index + 1).min(pages.len() - 1)];
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

/// `HasPoolSharedStorage`: an attached shared SR that can make disks.
fn has_pool_shared_storage(model: &xen_core::ConnectionModel) -> bool {
    model.pbds.values().any(|pbd| {
        model
            .srs
            .get(&pbd.sr)
            .is_some_and(|sr| sr.shared && xen_core::storage::supports_vdi_create(model, sr))
    })
}

/// An item's text and whether it can be chosen.
fn item_text(name: &str, state: &ItemState) -> (String, bool) {
    let c = cpm_strings();
    match state {
        ItemState::Ok => (name.to_owned(), true),
        ItemState::Checking => (fill(c.checking, &[("target", name)]), false),
        ItemState::Refused(why) => (
            fill(c.with_reason, &[("target", name), ("reason", why)]),
            false,
        ),
    }
}

fn local_text(r: &LocalRefusal) -> String {
    let c = cpm_strings();
    match r {
        LocalRefusal::CurrentServer => c.current_server,
        LocalRefusal::OlderServer => c.older_server,
        LocalRefusal::DynamicMemory => c.dynamic_memory,
    }
    .to_owned()
}

/// `CanDoStorageMigration`'s reason: the first line of a permission error,
/// the dynamic memory text, else the friendly error; within the pool, why
/// a live migration is refused too.
fn refusal_text(conn: Option<&ConnectionView>, r: &Refusal) -> String {
    match r {
        Refusal::Local(l) => local_text(l),
        Refusal::Server { error, boot } => {
            let text = match &**error {
                xapi::Error::Api { code, params }
                    if params
                        .first()
                        .is_some_and(|p| p.contains("DYNAMIC_MEMORY_CONTROL_UNAVAILABLE"))
                        || code == "DYNAMIC_MEMORY_CONTROL_UNAVAILABLE" =>
                {
                    cpm_strings().dynamic_memory.to_owned()
                }
                e => i18n::describe_xapi_error_in(conn, e)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            };
            match boot {
                Some(b) => format!(
                    "{text} - {}",
                    i18n::describe_xapi_error_in(conn, b)
                        .lines()
                        .next()
                        .unwrap_or_default()
                ),
                None => text,
            }
        }
    }
}

// ---- the Move VM dialog ----------------------------------------------------------------

/// `MoveVMDialog`: the SR the VM's disks move to (when the wizard cannot
/// run).
pub struct MoveDialog {
    connection: ConnectionId,
    vm: Ref<Vm>,
    sr: Option<Ref<Sr>>,
}

impl MoveDialog {
    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let c = cpm_strings();
        let p = crate::prov_strings::prov_strings();
        let Some(conn) = state.connection(self.connection) else {
            return Err(());
        };
        let Some(vm) = conn.model.vms.get(&self.vm) else {
            return Err(());
        };
        let srs = xen_core::provisioning::disk_srs(&conn.model, Some(vm));
        let mut outcome = Ok(None);
        window(c.move_dialog_title.to_owned()).show(ctx, |ui| {
            ui.label(fill(c.move_dialog_intro, &[("vm", &vm.name_label)]));
            crate::provision_ui::sr_combo(ui, "move-vm-sr", &srs, &mut self.sr);
            ui.horizontal(|ui| {
                let home = objects::vm_home(&conn.model, vm)
                    .cloned()
                    .or_else(|| conn.model.pool().map(|(_, p)| p.master.clone()));
                if ui
                    .add_enabled(
                        self.sr.is_some() && home.is_some(),
                        Button::new(c.move_button),
                    )
                    .clicked()
                    && let (Some(sr), Some(host)) = (&self.sr, home)
                {
                    let storage = xen_core::provisioning::vm_disks(&conn.model, vm)
                        .into_iter()
                        .map(|(_, _, vdi, _)| (vdi.clone(), sr.clone()))
                        .collect();
                    outcome = Ok(Some(ActionRequest::MoveVm {
                        vm: self.vm.clone(),
                        host,
                        storage,
                    }));
                }
                if ui.button(p.cancel).clicked() {
                    outcome = Err(());
                }
            });
        });
        outcome
    }
}

// ---- the dialogs together ----------------------------------------------------------------

/// The F12a migration dialogs of the window.
#[derive(Default)]
pub struct MigrateUi {
    pub(crate) wizard: Option<MigrateWizard>,
    pub(crate) move_dialog: Option<MoveDialog>,
    pub errors: Vec<String>,
}

impl MigrateUi {
    /// Opens the wizard (or the Move VM dialog) of `request`.
    pub fn open(&mut self, conn: &ConnectionView, request: DialogRequest) {
        match request {
            DialogRequest::Migrate { vms, mode } => {
                self.wizard = Some(MigrateWizard::new(conn, vms, mode));
            }
            DialogRequest::CopyVm(vm) => {
                self.wizard = Some(MigrateWizard::new(conn, vec![vm], WizardMode::Copy));
            }
            DialogRequest::MoveVm(vm) => {
                let Some(record) = conn.model.vms.get(&vm) else {
                    return;
                };
                let busy = conn.busy_vms.contains(&vm);
                let restricted = rules::cross_pool_restricted(&conn.model);
                if rules::move_uses_wizard(&conn.model, record, busy) && !restricted {
                    self.wizard =
                        Some(MigrateWizard::new(conn, vec![vm], rules::move_mode(record)));
                } else if objects::vm_kind(record) == VmKind::Guest {
                    let sr = xen_core::provisioning::vm_disks(&conn.model, record)
                        .first()
                        .map(|(_, _, _, v)| v.sr.clone());
                    self.move_dialog = Some(MoveDialog {
                        connection: conn.id,
                        vm,
                        sr,
                    });
                }
            }
            _ => {}
        }
    }

    /// Takes the answer to a check of the wizard.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        self.wizard
            .as_mut()
            .is_some_and(|w| w.on_precheck(id, result))
    }

    /// Draws the open dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(w) = self.wizard.as_mut() {
            let source = w.source;
            match w.show(ctx, state, backend) {
                Ok(Some(requests)) => {
                    if let Some(conn) = state.connection(source) {
                        for request in requests {
                            // The wizard asked its questions already.
                            ops.dispatch(conn, request, backend);
                        }
                    }
                    self.wizard = None;
                }
                Ok(None) => {}
                Err(()) => self.wizard = None,
            }
        }
        if let Some(d) = self.move_dialog.as_mut() {
            let source = d.connection;
            match d.show(ctx, state) {
                Ok(Some(request)) => {
                    if let Some(conn) = state.connection(source) {
                        ops.dispatch(conn, request, backend);
                    }
                    self.move_dialog = None;
                }
                Ok(None) => {}
                Err(()) => self.move_dialog = None,
            }
        }
    }
}
