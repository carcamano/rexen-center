//! vApps in the window (F12a), as the original's (see
//! [`xen_core::vapp`] for the rules):
//!
//! * **Manage vApps...** (Pool menu and the pool's context menu,
//!   `VMAppliancesDialog`): the pool's vApps (name, description, number of
//!   VMs; Ctrl-click selects several) with the selected one's VMs, and
//!   New vApp..., Delete (asked first, with the number of VMs), Properties,
//!   Start, Shut down (asked first), Export (the Export wizard with the
//!   vApp's VMs) and Import.
//! * **New vApp...** (`NewVMApplianceWizard`): Permissions (a restricted
//!   user without `VM_appliance.create`; blocking), Name, Virtual Machines
//!   (the pool's VMs with their current vApp; VMs of another vApp are asked
//!   about), VM Startup Sequence (rows selected, then their start order and
//!   delay set) and Finish (the summary).
//! * **vApp Properties**: General (name, description), Virtual Machines
//!   and VM Startup Sequence; OK saves what changed.
//! * **VM menus**: Start vApp and Shut Down vApp (asked first) for a VM
//!   of a vApp, and Assign to vApp (New vApp... and each vApp, the VM's
//!   own ticked; choosing it again takes the VM out).

use std::collections::HashSet;

use egui::{Button, Context, DragValue, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Vm, VmAppliance};
use xen_core::rbac::{self, RbacCheck};
use xen_core::vapp::{self as rules, ChangeWarning, NewVAppSpec, StartupRow, VAppChanges};
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionModel, ConnectionView,
};

use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::{self, fill};
use crate::ops_ui::{Confirmation, Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::vapp_strings::vapp_strings;
use crate::window_frame::dialog_window;
use crate::xva_ui::{error_label, nav, page_list};

// ---- menu entries ----------------------------------------------------------------------

/// "Manage vApps..." (`VMGroup<VM_appliance>.ManageGroupsDialog`).
pub fn manage_entry(conn: &ConnectionView) -> Entry {
    Entry::Dialog {
        label: vapp_strings().menu_manage,
        enabled: conn.status.is_connected(),
        dialog: DialogRequest::ManageVApps,
    }
}

/// Start vApp and Shut Down vApp for VMs of one vApp
/// (`VappStartCommand`, `VappShutDownCommand`).
pub fn vm_entries(conn: &ConnectionView, vms: &[Ref<Vm>]) -> Vec<Entry> {
    let s = vapp_strings();
    let usable = conn.status.is_connected();
    let app = rules::common_vapp(&conn.model, vms);
    let (start, stop) = match app {
        Some((r, a)) => (
            (rules::can_start(a), Some(r.clone())),
            (rules::can_shut_down(a), Some(r.clone())),
        ),
        None => ((false, None), (false, None)),
    };
    let item = |label, (enabled, vapp): (bool, Option<Ref<VmAppliance>>), start: bool| {
        let vapp = vapp.unwrap_or_else(Ref::null);
        Entry::Item {
            label,
            enabled: usable && enabled,
            request: if start {
                ActionRequest::StartVApp { vapp }
            } else {
                ActionRequest::ShutDownVApp { vapp }
            },
        }
    };
    vec![
        item(s.menu_start, start, true),
        item(s.menu_shut_down, stop, false),
    ]
}

/// Assign to vApp (`AssignGroupToolStripMenuItem<VM_appliance>`).
pub fn assign_entry(conn: &ConnectionView, vms: &[Ref<Vm>]) -> Entry {
    let s = vapp_strings();
    let m = &conn.model;
    let enabled = conn.status.is_connected()
        && vms.iter().any(|v| {
            m.vms
                .get(v)
                .is_some_and(|r| rules::can_assign(r, conn.busy_vms.contains(v)))
        });
    let mut entries = vec![Entry::Dialog {
        label: s.menu_new,
        enabled: true,
        dialog: DialogRequest::NewVApp(vms.to_vec()),
    }];
    let current = match vms {
        [one] => m.vms.get(one).map(|v| v.appliance.clone()),
        _ => None,
    };
    let apps = rules::vapps(m);
    if !apps.is_empty() {
        entries.push(Entry::Separator);
    }
    for (i, (r, app)) in apps.into_iter().enumerate() {
        let mut label = if i < 9 {
            fill(
                s.menu_vapp_item,
                &[("index", &(i + 1).to_string()), ("name", &app.name_label)],
            )
        } else {
            app.name_label.clone()
        };
        if current.as_ref() == Some(r) {
            label = format!("\u{2022} {label}");
        }
        entries.push(Entry::Named {
            label,
            enabled: true,
            hint: None,
            dialog: DialogRequest::AssignVApp {
                vapp: r.clone(),
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

/// The questions asked before a vApp request (`VappShutDownCommand`,
/// `ChangesOK`).
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let s = vapp_strings();
    let m = &conn.model;
    match request {
        ActionRequest::ShutDownVApp { vapp } => {
            let name = m
                .vm_appliances
                .get(vapp)
                .map(|a| a.name_label.clone())
                .unwrap_or_default();
            Some(Confirmation {
                title: s.shut_down_title,
                text: fill(s.confirm_shut_down, &[("names", &name)]),
                yes: s.yes,
                no_default: false,
            })
        }
        ActionRequest::AssignVApp { vapp, vms } => {
            let name = m
                .vm_appliances
                .get(vapp)
                .map(|a| a.name_label.clone())
                .unwrap_or_default();
            change_text(rules::change_warning(m, vms, Some(vapp), &name)).map(|text| Confirmation {
                title: s.change_title,
                text,
                yes: s.yes,
                no_default: false,
            })
        }
        _ => None,
    }
}

pub(crate) fn change_text(warning: Option<ChangeWarning>) -> Option<String> {
    let s = vapp_strings();
    Some(match warning? {
        ChangeWarning::One { vm, old, new } => {
            fill(s.change_one, &[("vm", &vm), ("old", &old), ("new", &new)])
        }
        ChangeWarning::Many { new } => fill(s.change_many, &[("new", &new)]),
    })
}

fn in_pool(m: &ConnectionModel) -> bool {
    xen_core::objects::pool_is_visible(m)
}

// ---- shared pages ----------------------------------------------------------------------

/// Which group the Virtual Machines page fills (`NewVMGroupVMsPage<T>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GroupKind {
    #[default]
    VApp,
    /// A snapshot schedule (`VMSS`, F12b).
    Vmss,
}

/// The Virtual Machines page (`NewVMGroupVMsPage<T>`).
#[derive(Default)]
pub(crate) struct VmsPage {
    pub(crate) selected: Vec<Ref<Vm>>,
    pub(crate) search: String,
    pub(crate) kind: GroupKind,
}

impl VmsPage {
    pub(crate) fn new(kind: GroupKind, selected: Vec<Ref<Vm>>) -> Self {
        Self {
            selected,
            search: String::new(),
            kind,
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, conn: &ConnectionView, show_hidden: bool, id: &str) {
        let s = vapp_strings();
        let v = crate::vmss_strings::vmss_strings();
        let vmss = self.kind == GroupKind::Vmss;
        let m = &conn.model;
        ui.horizontal(|ui| {
            ui.label(fill(
                if in_pool(m) {
                    s.vms_in_pool
                } else {
                    s.vms_in_server
                },
                &[("pool", &connection_name(conn))],
            ));
            ui.add(
                TextEdit::singleline(&mut self.search)
                    .desired_width(180.0)
                    .hint_text(s.search_hint),
            );
        });
        ui.label(if vmss { v.vms_rubric } else { s.vms_rubric });
        let needle = self.search.to_lowercase();
        let mut rows: Vec<_> = rules::selectable_vms(m, show_hidden)
            .into_iter()
            .filter(|(_, v)| needle.is_empty() || v.name_label.to_lowercase().contains(&needle))
            .collect();
        // `ColumnCheckBox` ascending: the ticked first, then by name.
        rows.sort_by_key(|(r, _)| !self.selected.contains(r));
        egui::ScrollArea::vertical()
            .id_salt(format!("{id}-scroll"))
            .max_height(220.0)
            .show(ui, |ui| {
                Grid::new(format!("{id}-grid"))
                    .num_columns(3)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong(s.col_vm);
                        ui.strong(s.col_description);
                        ui.strong(if vmss { v.col_current } else { s.col_current });
                        ui.end_row();
                        for (r, v) in &rows {
                            let mut on = self.selected.contains(r);
                            if ui.checkbox(&mut on, &v.name_label).changed() {
                                if on {
                                    self.selected.push((*r).clone());
                                } else {
                                    self.selected.retain(|x| x != *r);
                                }
                            }
                            ui.label(&v.name_description);
                            let current = if vmss {
                                xen_core::vmss::policy_of(m, v).map(|(_, p)| p.name_label.clone())
                            } else {
                                rules::vapp_of(m, v).map(|(_, a)| a.name_label.clone())
                            };
                            ui.label(current.unwrap_or_else(|| s.none.to_owned()));
                            ui.end_row();
                        }
                    });
            });
        // VMs that went away drop out.
        self.selected.retain(|r| m.vms.contains_key(r));
        ui.horizontal(|ui| {
            let count = self.selected.len();
            ui.label(if count == 1 {
                s.one_vm_selected.to_owned()
            } else {
                fill(s.vms_selected, &[("count", &count.to_string())])
            });
            if ui
                .add_enabled(count < rows.len(), Button::new(s.select_all))
                .clicked()
            {
                for (r, _) in &rows {
                    if !self.selected.contains(r) {
                        self.selected.push((*r).clone());
                    }
                }
            }
            if ui
                .add_enabled(count > 0, Button::new(s.clear_all))
                .clicked()
            {
                self.selected.clear();
            }
        });
    }
}

/// The VM Startup Sequence page (`NewVMApplianceVMOrderAndDelaysPage`).
#[derive(Default)]
pub(crate) struct OrderPage {
    pub(crate) rows: Vec<StartupRow>,
    pub(crate) chosen: HashSet<Ref<Vm>>,
}

impl OrderPage {
    /// `SetSelectedVMs`: the rows of `vms`, keeping the edits made.
    fn set_vms(&mut self, m: &ConnectionModel, vms: &[Ref<Vm>]) {
        let mut rows = rules::startup_rows(m, vms);
        for row in &mut rows {
            if let Some(old) = self.rows.iter().find(|r| r.vm == row.vm) {
                row.order = old.order;
                row.start_delay = old.start_delay;
            }
        }
        self.rows = rows;
        self.chosen.retain(|v| vms.contains(v));
    }

    fn show(&mut self, ui: &mut Ui, conn: &ConnectionView, id: &str, blurb: bool) {
        let s = vapp_strings();
        let m = &conn.model;
        if blurb {
            ui.label(s.order_blurb);
            ui.add_space(4.0);
        }
        egui::ScrollArea::vertical()
            .id_salt(format!("{id}-scroll"))
            .max_height(200.0)
            .show(ui, |ui| {
                Grid::new(format!("{id}-grid"))
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong(s.col_vm);
                        ui.strong(s.col_description);
                        ui.strong(s.col_order);
                        ui.strong(s.col_delay);
                        ui.end_row();
                        for row in &self.rows {
                            let Some(vm) = m.vms.get(&row.vm) else {
                                continue;
                            };
                            let on = self.chosen.contains(&row.vm);
                            if ui.selectable_label(on, &vm.name_label).clicked() {
                                let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                                if !multi {
                                    self.chosen.clear();
                                }
                                if on && multi {
                                    self.chosen.remove(&row.vm);
                                } else {
                                    self.chosen.insert(row.vm.clone());
                                }
                            }
                            ui.label(&vm.name_description);
                            ui.label(row.order.to_string());
                            ui.label(row.start_delay.to_string());
                            ui.end_row();
                        }
                    });
            });
        ui.add_space(6.0);
        ui.strong(s.group_startup);
        let chosen: Vec<&StartupRow> = self
            .rows
            .iter()
            .filter(|r| self.chosen.contains(&r.vm))
            .collect();
        // `updateTextBoxes`: a value shared by every selected row.
        let shared = |f: fn(&StartupRow) -> i64| {
            let first = chosen.first().map(|r| f(r));
            first.filter(|v| chosen.iter().all(|r| f(r) == *v))
        };
        let mut order = shared(|r| r.order).unwrap_or(0);
        let mut delay = shared(|r| r.start_delay).unwrap_or(0);
        let enabled = !chosen.is_empty();
        let (mut set_order, mut set_delay) = (false, false);
        ui.add_enabled_ui(enabled, |ui| {
            Grid::new(format!("{id}-edit"))
                .num_columns(3)
                .show(ui, |ui| {
                    let l = ui.label(s.start_order);
                    set_order = ui
                        .add(DragValue::new(&mut order).range(0..=i64::MAX))
                        .labelled_by(l.id)
                        .changed();
                    ui.label("");
                    ui.end_row();
                    let l = ui.label(s.start_delay);
                    set_delay = ui
                        .add(DragValue::new(&mut delay).range(0..=i64::MAX))
                        .labelled_by(l.id)
                        .changed();
                    ui.label(s.seconds);
                    ui.end_row();
                });
        });
        for row in self.rows.iter_mut().filter(|r| self.chosen.contains(&r.vm)) {
            if set_order {
                row.order = order;
            }
            if set_delay {
                row.start_delay = delay;
            }
        }
    }
}

// ---- New vApp ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewPage {
    Rbac,
    Name,
    Vms,
    Order,
    Finish,
}

/// The New vApp wizard (`NewVMApplianceWizard`).
pub struct NewVAppWizard {
    connection: ConnectionId,
    pub(crate) page: NewPage,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) vms: VmsPage,
    pub(crate) order: OrderPage,
    /// The `ChangesOK` question while leaving the VMs page.
    pub(crate) warning: Option<String>,
    error: Option<String>,
}

impl NewVAppWizard {
    pub(crate) fn new(conn: &ConnectionView, vms: Vec<Ref<Vm>>) -> Self {
        let rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let rbac_blocked =
            match rbac::check(&conn.model, conn.rbac.as_deref(), &["vm_appliance.create"]) {
                RbacCheck::Allowed => None,
                RbacCheck::Denied { .. } => Some(fill(
                    vapp_strings().rbac_create,
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
            vms: VmsPage::new(GroupKind::VApp, vms),
            order: OrderPage::default(),
            warning: None,
            error: None,
        }
    }

    fn pages(&self) -> Vec<NewPage> {
        let mut p = Vec::new();
        if self.rbac_needed {
            p.push(NewPage::Rbac);
        }
        p.extend([NewPage::Name, NewPage::Vms, NewPage::Order, NewPage::Finish]);
        p
    }

    fn page_name(p: NewPage) -> &'static str {
        let s = vapp_strings();
        match p {
            NewPage::Rbac => s.page_rbac,
            NewPage::Name => s.page_name,
            NewPage::Vms => s.page_vms,
            NewPage::Order => s.page_order,
            NewPage::Finish => s.page_finish,
        }
    }

    fn page_ok(&self) -> bool {
        match self.page {
            NewPage::Rbac => self.rbac_blocked.is_none(),
            NewPage::Name => !self.name.trim().is_empty(),
            _ => self.warning.is_none(),
        }
    }

    pub(crate) fn request(&self, m: &ConnectionModel) -> ActionRequest {
        ActionRequest::NewVApp(Box::new(NewVAppSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            vms: self.vms.selected.clone(),
            startup: rules::changed_startup(m, &self.order.rows),
        }))
    }

    fn summary(&self, m: &ConnectionModel) -> String {
        let s = vapp_strings();
        let vms: Vec<String> = self
            .vms
            .selected
            .iter()
            .filter_map(|v| m.vms.get(v))
            .map(|v| v.name_label.clone())
            .collect();
        fill(s.summary, &[("name", &self.name), ("vms", &vms.join(", "))])
    }

    /// Moves on from the current page (`UpdateWizardContent`).
    fn advance(&mut self, m: &ConnectionModel) {
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        if self.page == NewPage::Vms {
            self.order.set_vms(m, &self.vms.selected);
        }
        self.page = pages[(index + 1).min(pages.len() - 1)];
    }

    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let s = vapp_strings();
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
            .id(egui::Id::unique("new-vapp-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([760.0, 460.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(600.0);
                        egui::ScrollArea::vertical()
                            .id_salt("new-vapp-page")
                            .max_height(370.0)
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
                                    ui.label(s.name_blurb);
                                    ui.add_space(6.0);
                                    name_grid(
                                        ui,
                                        "new-vapp-name",
                                        &mut self.name,
                                        &mut self.description,
                                    );
                                }
                                NewPage::Vms => {
                                    ui.heading(s.vms_title);
                                    self.vms.show(ui, &conn, show_hidden, "new-vapp-vms");
                                    if let Some(text) = self.warning.clone() {
                                        ui.add_space(6.0);
                                        ui.colored_label(ui.visuals().warn_fg_color, text);
                                        ui.horizontal(|ui| {
                                            if ui.button(s.yes).clicked() {
                                                self.warning = None;
                                                self.advance(&conn.model);
                                            }
                                            if ui.button(s.no).clicked() {
                                                self.warning = None;
                                            }
                                        });
                                    }
                                }
                                NewPage::Order => {
                                    ui.heading(s.order_title);
                                    self.order.show(ui, &conn, "new-vapp-order", true);
                                }
                                NewPage::Finish => {
                                    ui.heading(s.finish_title);
                                    ui.label(s.finish_intro);
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(self.summary(&conn.model)).monospace());
                                }
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.separator();
                let last = self.page == NewPage::Finish;
                let (previous, next, cancel) = nav(
                    ui,
                    index > 0,
                    self.page_ok(),
                    if last { n.finish } else { n.next },
                );
                if previous {
                    self.warning = None;
                    self.page = pages[index - 1];
                }
                if next {
                    if last {
                        outcome = Ok(Some(self.request(&conn.model)));
                    } else if self.page == NewPage::Vms {
                        // `userAcceptsWarning`.
                        self.warning = change_text(rules::change_warning(
                            &conn.model,
                            &self.vms.selected,
                            None,
                            &self.name,
                        ));
                        if self.warning.is_none() {
                            self.advance(&conn.model);
                        }
                    } else {
                        self.advance(&conn.model);
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

fn name_grid(ui: &mut Ui, id: &str, name: &mut String, description: &mut String) {
    let s = vapp_strings();
    Grid::new(id).num_columns(2).show(ui, |ui| {
        let l = ui.label(s.name_label);
        ui.add(TextEdit::singleline(name).desired_width(380.0))
            .labelled_by(l.id);
        ui.end_row();
        let l = ui.label(s.description_label);
        ui.add(
            TextEdit::multiline(description)
                .desired_rows(3)
                .desired_width(380.0),
        )
        .labelled_by(l.id);
        ui.end_row();
    });
}

// ---- Properties ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropTab {
    General,
    Vms,
    Order,
}

/// The vApp Properties dialog (`PropertiesDialog` of a `VM_appliance`).
pub struct VAppProperties {
    connection: ConnectionId,
    pub(crate) vapp: Ref<VmAppliance>,
    pub(crate) tab: PropTab,
    pub(crate) name: String,
    pub(crate) description: String,
    original_name: String,
    original_description: String,
    original_members: Vec<Ref<Vm>>,
    pub(crate) vms: VmsPage,
    pub(crate) order: OrderPage,
    pub(crate) warning: Option<String>,
}

impl VAppProperties {
    pub(crate) fn new(conn: &ConnectionView, vapp: Ref<VmAppliance>) -> Option<Self> {
        let app = conn.model.vm_appliances.get(&vapp)?;
        let members = app.vms.clone();
        let mut order = OrderPage::default();
        order.set_vms(&conn.model, &members);
        Some(Self {
            connection: conn.id,
            vapp,
            tab: PropTab::General,
            name: app.name_label.clone(),
            description: app.name_description.clone(),
            original_name: app.name_label.clone(),
            original_description: app.name_description.clone(),
            original_members: members.clone(),
            vms: VmsPage::new(GroupKind::VApp, members),
            order,
            warning: None,
        })
    }

    pub(crate) fn changes(&self, m: &ConnectionModel) -> VAppChanges {
        let mut members = self.vms.selected.clone();
        members.sort();
        let mut original = self.original_members.clone();
        original.sort();
        VAppChanges {
            name: (self.name.trim() != self.original_name).then(|| self.name.trim().to_owned()),
            description: (self.description != self.original_description)
                .then(|| self.description.clone()),
            members: (members != original).then(|| self.vms.selected.clone()),
            startup: rules::changed_startup(m, &self.order.rows),
        }
    }

    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let s = vapp_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        if !conn.model.vm_appliances.contains_key(&self.vapp) {
            return Err(());
        }
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let mut outcome = Ok(None);
        let mut open = true;
        dialog_window(fill(s.properties_title, &[("name", &self.original_name)]))
            .id(egui::Id::unique("vapp-properties"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([760.0, 440.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for (tab, label) in [
                            (PropTab::General, s.page_general),
                            (PropTab::Vms, s.page_vms),
                            (PropTab::Order, s.page_order),
                        ] {
                            if ui.selectable_label(self.tab == tab, label).clicked() {
                                if tab == PropTab::Order {
                                    self.order.set_vms(&conn.model, &self.vms.selected);
                                }
                                self.tab = tab;
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(580.0);
                        egui::ScrollArea::vertical()
                            .id_salt("vapp-properties-page")
                            .max_height(350.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.tab {
                                PropTab::General => {
                                    ui.heading(s.general_title);
                                    name_grid(
                                        ui,
                                        "vapp-properties-name",
                                        &mut self.name,
                                        &mut self.description,
                                    );
                                }
                                PropTab::Vms => {
                                    ui.heading(s.vms_title);
                                    self.vms.show(ui, &conn, show_hidden, "vapp-properties-vms");
                                }
                                PropTab::Order => {
                                    ui.heading(s.order_title);
                                    self.order.show(ui, &conn, "vapp-properties-order", true);
                                }
                            });
                        if let Some(text) = self.warning.clone() {
                            ui.colored_label(ui.visuals().warn_fg_color, text);
                            ui.horizontal(|ui| {
                                if ui.button(s.yes).clicked() {
                                    self.warning = None;
                                    outcome = Ok(Some(self.save(&conn.model)));
                                }
                                if ui.button(s.no).clicked() {
                                    self.warning = None;
                                }
                            });
                        }
                    });
                });
                ui.separator();
                dialog_buttons(ui, |row| {
                    let valid = !self.name.trim().is_empty() && self.warning.is_none();
                    if row
                        .add_enabled(ButtonRole::Primary, valid, Button::new(s.ok))
                        .clicked()
                    {
                        self.order.set_vms(&conn.model, &self.vms.selected);
                        let changes = self.changes(&conn.model);
                        if changes.is_empty() {
                            outcome = Err(());
                        } else if changes.members.is_some()
                            && let Some(text) = change_text(rules::change_warning(
                                &conn.model,
                                &self.vms.selected,
                                Some(&self.vapp),
                                self.name.trim(),
                            ))
                        {
                            // `ValidToSave`: `userAcceptsWarning`.
                            self.warning = Some(text);
                        } else {
                            outcome = Ok(Some(self.save(&conn.model)));
                        }
                    }
                    if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                        outcome = Err(());
                    }
                });
            });
        if !open {
            return Err(());
        }
        outcome
    }

    fn save(&self, m: &ConnectionModel) -> ActionRequest {
        ActionRequest::SetVApp {
            vapp: self.vapp.clone(),
            changes: Box::new(self.changes(m)),
        }
    }
}

// ---- Manage vApps -------------------------------------------------------------------------

/// A question of the Manage vApps dialog.
enum ManageQuestion {
    Delete(Vec<Ref<VmAppliance>>, String),
    ShutDown(Ref<VmAppliance>),
}

/// The Manage vApps dialog (`VMAppliancesDialog`).
pub struct ManageVApps {
    connection: ConnectionId,
    pub(crate) selected: Vec<Ref<VmAppliance>>,
    question: Option<ManageQuestion>,
}

/// What the Manage vApps dialog asks the window for.
pub(crate) enum ManageAction {
    Request(ActionRequest),
    Dialog(DialogRequest),
}

impl ManageVApps {
    fn delete_text(&self, m: &ConnectionModel) -> String {
        let s = vapp_strings();
        let apps: Vec<_> = self
            .selected
            .iter()
            .filter_map(|r| m.vm_appliances.get(r))
            .collect();
        let count: usize = apps.iter().map(|a| a.vms.len()).sum();
        let c = ("count", count.to_string());
        match apps.as_slice() {
            [one] => {
                let name = ("name", one.name_label.clone());
                match count {
                    0 => fill(s.delete_one_0, &[(name.0, &name.1)]),
                    1 => fill(s.delete_one_1, &[(name.0, &name.1), (c.0, &c.1)]),
                    _ => fill(s.delete_one_n, &[(name.0, &name.1), (c.0, &c.1)]),
                }
            }
            _ => match count {
                0 => s.delete_many_0.to_owned(),
                1 => fill(s.delete_many_1, &[(c.0, &c.1)]),
                _ => fill(s.delete_many_n, &[(c.0, &c.1)]),
            },
        }
    }

    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Vec<ManageAction>, ()> {
        let s = vapp_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return Err(());
        };
        let m = &conn.model;
        let apps = rules::vapps(m);
        self.selected.retain(|r| m.vm_appliances.contains_key(r));
        let current = match self.selected.as_slice() {
            [one] => m.vm_appliances.get_key_value(one),
            _ => None,
        };
        let mut actions = Vec::new();
        let mut close = false;
        let pool = connection_name(&conn);
        window(fill(s.manage_title, &[("pool", &pool)])).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(s.button_new).clicked() {
                    actions.push(ManageAction::Dialog(DialogRequest::NewVApp(Vec::new())));
                }
                if ui
                    .add_enabled(!self.selected.is_empty(), Button::new(s.button_delete))
                    .clicked()
                {
                    self.question = Some(ManageQuestion::Delete(
                        self.selected.clone(),
                        self.delete_text(m),
                    ));
                }
                if ui
                    .add_enabled(current.is_some(), Button::new(s.button_properties))
                    .clicked()
                    && let Some((r, _)) = current
                {
                    actions.push(ManageAction::Dialog(DialogRequest::VAppProperties(
                        r.clone(),
                    )));
                }
                ui.separator();
                let can_start = current.is_some_and(|(_, a)| rules::can_start(a));
                if ui
                    .add_enabled(can_start, Button::new(s.button_start))
                    .clicked()
                    && let Some((r, _)) = current
                {
                    actions.push(ManageAction::Request(ActionRequest::StartVApp {
                        vapp: r.clone(),
                    }));
                }
                let can_stop = current.is_some_and(|(_, a)| rules::can_shut_down(a));
                if ui
                    .add_enabled(can_stop, Button::new(s.button_shut_down))
                    .clicked()
                    && let Some((r, _)) = current
                {
                    self.question = Some(ManageQuestion::ShutDown(r.clone()));
                }
                ui.separator();
                let exportable = current.is_some_and(|(_, a)| {
                    a.vms.iter().all(|v| {
                        m.vms.get(v).is_some_and(|vm| {
                            !vm.is_a_template
                                && !conn.busy_vms.contains(v)
                                && vm.allowed_operations.contains(&xapi::VmOperations::Export)
                        })
                    })
                });
                if ui
                    .add_enabled(exportable, Button::new(s.button_export))
                    .clicked()
                    && let Some((_, a)) = current
                {
                    actions.push(ManageAction::Dialog(DialogRequest::Export(a.vms.clone())));
                }
                if ui.button(s.button_import).clicked() {
                    actions.push(ManageAction::Dialog(DialogRequest::Import(None)));
                }
            });
            ui.separator();
            ui.label(fill(
                if in_pool(m) {
                    s.defined_in_pool
                } else {
                    s.defined_in_server
                },
                &[("pool", &pool)],
            ));
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(440.0);
                    Grid::new("vapps-grid")
                        .num_columns(3)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            ui.strong(s.col_name);
                            ui.strong(s.col_description);
                            ui.strong(s.col_vms);
                            ui.end_row();
                            for (r, app) in &apps {
                                let on = self.selected.contains(r);
                                if ui.selectable_label(on, &app.name_label).clicked() {
                                    let multi =
                                        ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                                    if !multi {
                                        self.selected.clear();
                                    }
                                    if on && multi {
                                        self.selected.retain(|x| x != *r);
                                    } else {
                                        self.selected.push((*r).clone());
                                    }
                                }
                                ui.label(&app.name_description);
                                ui.label(app.vms.len().to_string());
                                ui.end_row();
                            }
                        });
                    if apps.is_empty() {
                        ui.label(RichText::new(s.no_vapps).weak());
                    }
                });
                if let Some((_, app)) = current {
                    // No separator: in a window sized by its content it
                    // would stretch to the screen's height.
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_min_width(200.0);
                        ui.strong(&app.name_label);
                        for (_, vm) in rules::members(m, app) {
                            ui.horizontal(|ui| {
                                crate::icons::vm_state(ui, xen_core::objects::vm_icon(vm));
                                ui.label(&vm.name_label);
                            });
                        }
                    });
                }
            });
            if let Some(q) = &self.question {
                ui.separator();
                let (title, text) = match q {
                    ManageQuestion::Delete(_, text) => (s.delete_title, text.clone()),
                    ManageQuestion::ShutDown(_) => {
                        (s.shut_down_title, s.confirm_shut_down_one.to_owned())
                    }
                };
                ui.strong(title);
                ui.colored_label(ui.visuals().warn_fg_color, text);
                ui.horizontal(|ui| {
                    if ui.button(s.yes).clicked() {
                        match self.question.take() {
                            Some(ManageQuestion::Delete(vapps, _)) => {
                                actions.push(ManageAction::Request(ActionRequest::DeleteVApps {
                                    vapps,
                                }));
                                self.selected.clear();
                            }
                            Some(ManageQuestion::ShutDown(vapp)) => actions
                                .push(ManageAction::Request(ActionRequest::ShutDownVApp { vapp })),
                            None => {}
                        }
                    }
                    if ui.button(s.no).clicked() {
                        self.question = None;
                    }
                });
            }
            ui.separator();
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

/// The F12a vApp dialogs of the window.
#[derive(Default)]
pub struct VAppUi {
    pub(crate) manage: Option<ManageVApps>,
    pub(crate) wizard: Option<NewVAppWizard>,
    pub(crate) properties: Option<VAppProperties>,
    /// Dialogs of other modules asked for (Export, Import).
    pub forwarded: Vec<(ConnectionId, DialogRequest)>,
    pub errors: Vec<String>,
}

impl VAppUi {
    /// Opens the dialog of `request`; assigning to a vApp goes straight to
    /// the operations.
    pub fn open(
        &mut self,
        conn: &ConnectionView,
        request: DialogRequest,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        match request {
            DialogRequest::ManageVApps => {
                self.manage = Some(ManageVApps {
                    connection: conn.id,
                    selected: Vec::new(),
                    question: None,
                });
            }
            DialogRequest::NewVApp(vms) => self.wizard = Some(NewVAppWizard::new(conn, vms)),
            DialogRequest::VAppProperties(vapp) => {
                self.properties = VAppProperties::new(conn, vapp);
            }
            DialogRequest::AssignVApp { vapp, vms } => {
                let Some(app) = conn.model.vm_appliances.get(&vapp) else {
                    return;
                };
                // `AssignGroupToVMCommand.RunCore`: choosing a VM's own
                // vApp takes it out.
                if let [one] = vms.as_slice()
                    && app.vms.contains(one)
                {
                    ops.request(
                        conn,
                        ActionRequest::RemoveFromVApp {
                            vapp,
                            vms: vec![one.clone()],
                        },
                        backend,
                    );
                    return;
                }
                let mut members = vms.clone();
                for v in &app.vms {
                    if !members.contains(v) {
                        members.push(v.clone());
                    }
                }
                ops.request(
                    conn,
                    ActionRequest::AssignVApp { vapp, vms: members },
                    backend,
                );
            }
            _ => {}
        }
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
            match d.show(ctx, state) {
                Ok(actions) => {
                    for action in actions {
                        match action {
                            ManageAction::Request(request) => {
                                if let Some(conn) = state.connection(id) {
                                    // Asked in the dialog already.
                                    ops.dispatch(conn, request, backend);
                                }
                            }
                            ManageAction::Dialog(DialogRequest::Export(vms)) => {
                                self.forwarded.push((id, DialogRequest::Export(vms)));
                            }
                            ManageAction::Dialog(DialogRequest::Import(host)) => {
                                self.forwarded.push((id, DialogRequest::Import(host)));
                            }
                            ManageAction::Dialog(request) => {
                                if let Some(conn) = state.connection(id) {
                                    self.open(conn, request, ops, backend);
                                }
                            }
                        }
                    }
                }
                Err(()) => self.manage = None,
            }
        }
        if let Some(w) = self.wizard.as_mut() {
            let id = w.connection;
            match w.show(ctx, state) {
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
