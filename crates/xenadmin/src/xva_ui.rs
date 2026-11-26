//! XVA export and import in the window (F11a), as the original's commands
//! and wizards:
//!
//! * **Export...** (`ExportCommand`, VM menu and the context menus of VMs,
//!   servers and pools) opens the Export wizard (`ExportApplianceWizard`)
//!   for the XVA format: Export File Details (name, folder, format; the
//!   folder must exist and be writable, the file must not), the blocking
//!   RBAC page for a restricted user, Virtual Machines (the halted or
//!   suspended VMs that may be exported, one ticked; the folder's free
//!   space checked against the disks' size) and Finish (summary, "Verify
//!   export on completion").
//! * **Export to File...** of a template and **Export Snapshot As
//!   Template...** of a snapshot (`ExportVMCommand`'s save dialog): the
//!   file and the verify option; too little free space is asked about.
//! * **Import...** (`ImportCommand`, File menu and the context menus of
//!   servers and pools) opens the Import wizard (`ImportWizard`) for an
//!   XVA: Import Source (an `.xva`, or an `.xva.gz` uncompressed next to it
//!   first, the compressed file then deleted as the original warns), Home
//!   Server (a pool or a server), the RBAC page for a restricted user,
//!   Storage (the SR; "Import >" starts the transfer, shown with its
//!   progress and Cancel, and the wizard moves on once the server made the
//!   VM), Networking (the imported VM's interfaces: MAC, network, add,
//!   delete) and Finish (summary, start the VM after import). Cancelling
//!   the wizard cancels a transfer in progress.
//!
//! The OVF/OVA format of the Export wizard (its EULA and Advanced Options
//! pages) and the Import wizard's OVF/OVA and disk image pages are F11b's
//! ([`crate::ovf_ui`]): the Import wizard hands such a source over to
//! [`crate::ovf_ui::OvfImportWizard`]. There is no native file dialog:
//! paths are typed, as in the Back Up dialog.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use egui::{Button, ComboBox, Context, Grid, ProgressBar, RichText, TextEdit, Ui};
use xapi::{Host, Network, Ref, Sr, Vm};
use xen_core::objects::{self, VmKind};
use xen_core::rbac::{self, RbacCheck};
use xen_core::xva::{self as rules};
use xen_core::xva_run::{ImportFinish, ImportVif};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, Operation,
    OperationKind, OperationStatus,
};

use crate::branding;
use crate::i18n::{self, fill};
use crate::ops_ui::{Entry, OpsUi};
use crate::pool_ui::{connection_name, window};
use crate::provision_ui::DialogRequest;
use crate::xva_strings::xva_strings;

/// `ExportVmAction.StaticRBACDependencies`.
const EXPORT_RBAC: &[&str] = &["task.create", "http/get_export", "task.destroy"];
/// `ImportVmAction.ConstantRBACRequirements` and `sr.scan` (CA-337323).
const IMPORT_RBAC: &[&str] = &[
    "vm.set_name_label",
    "network.destroy",
    "vif.create",
    "vif.destroy",
    "http/put_import",
    "sr.scan",
];

pub(crate) fn vm_name(conn: &ConnectionView, vm: &Ref<Vm>) -> String {
    conn.model
        .vms
        .get(vm)
        .map(|v| v.name_label.clone())
        .unwrap_or_default()
}

// ---- menu entries ----------------------------------------------------------------------

/// `ExportCommand` for the selection: `vms` (VMs selected) or a server or
/// pool (`host_ok`: the selected server is enabled and live, or the pool
/// has one).
pub fn export_entry(
    state: &AppState,
    conn: &ConnectionView,
    vms: &[Ref<Vm>],
    host_ok: bool,
) -> Entry {
    let x = xva_strings();
    let show_hidden = state.settings.preferences.show_hidden_objects;
    let usable = conn.status.is_connected();
    let (enabled, hint) = if vms.is_empty() {
        let any = !rules::exportable_vms(&conn.model, show_hidden, &conn.busy_vms).is_empty();
        (host_ok && any, (!any).then(|| x.no_halted_vms.to_owned()))
    } else {
        let any = vms.iter().any(|r| {
            conn.model
                .vms
                .get(r)
                .is_some_and(|v| rules::can_export_vm(v, conn.busy_vms.contains(r)))
        });
        let halted = vms.iter().any(|r| {
            conn.model.vms.get(r).is_some_and(|v| {
                matches!(
                    v.power_state,
                    xapi::VmPowerState::Halted | xapi::VmPowerState::Suspended
                )
            })
        });
        (
            any,
            (!any && !halted).then(|| x.shutdown_before_export.to_owned()),
        )
    };
    Entry::Named {
        label: x.menu_export.to_owned(),
        enabled: usable && enabled,
        hint,
        dialog: DialogRequest::Export(vms.to_vec()),
    }
}

/// `ImportCommand`, with `host` as the home server (`None`: the pool).
pub fn import_entry(conn: &ConnectionView, host: Option<Ref<Host>>) -> Entry {
    Entry::Named {
        label: xva_strings().menu_import.to_owned(),
        enabled: conn.status.is_connected(),
        hint: None,
        dialog: DialogRequest::Import(host),
    }
}

/// `ExportTemplateCommand` / `ExportSnapshotAsTemplateCommand`.
pub fn export_to_file_entry(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &xapi::VmRecord) -> Entry {
    let x = xva_strings();
    let label = if objects::vm_kind(vm) == VmKind::Snapshot {
        x.menu_export_snapshot
    } else {
        x.menu_export_template
    };
    Entry::Named {
        label: label.to_owned(),
        enabled: conn.status.is_connected()
            && rules::can_export_to_file(vm, conn.busy_vms.contains(vm_ref)),
        hint: None,
        dialog: DialogRequest::ExportToFile(vm_ref.clone()),
    }
}

/// `{folder}/{name}` with the `.xva` extension added when missing.
pub(crate) fn xva_path(folder: &str, name: &str) -> PathBuf {
    let mut file = name.to_owned();
    if !file.to_lowercase().ends_with(".xva") {
        file.push_str(".xva");
    }
    Path::new(folder).join(file)
}

/// `CheckPermissions`: a file can be made in `dir`.
pub(crate) fn writable(dir: &Path) -> bool {
    let touch = dir.join(format!("_xenadmin-rs-{}", std::process::id()));
    let ok = std::fs::File::create(&touch).is_ok();
    let _ = std::fs::remove_file(&touch);
    ok
}

/// The wizard pages' navigation buttons; returns (previous, next, cancel).
pub(crate) fn nav(
    ui: &mut Ui,
    can_previous: bool,
    can_next: bool,
    next: &str,
) -> (bool, bool, bool) {
    let n = crate::net_strings::net_strings();
    let mut out = (false, false, false);
    ui.horizontal(|ui| {
        out.0 = ui
            .add_enabled(can_previous, Button::new(n.previous))
            .clicked();
        out.1 = ui.add_enabled(can_next, Button::new(next)).clicked();
        out.2 = ui.button(n.cancel).clicked();
    });
    out
}

pub(crate) fn page_list(ui: &mut Ui, names: &[&str], current: usize) {
    ui.vertical(|ui| {
        ui.set_width(130.0);
        for (i, name) in names.iter().enumerate() {
            let t = RichText::new(*name);
            ui.label(if i == current { t.strong() } else { t.weak() });
        }
    });
}

pub(crate) fn summary(ui: &mut Ui, id: &str, rows: &[(String, String)]) {
    Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for (k, v) in rows {
                ui.label(RichText::new(k).strong());
                ui.label(v);
                ui.end_row();
            }
        });
}

pub(crate) fn error_label(ui: &mut Ui, error: &Option<String>) {
    if let Some(e) = error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
}

// ---- the Export wizard -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportPage {
    Appliance,
    Rbac,
    Vms,
    /// OVF/OVA only.
    Eula,
    /// OVF/OVA only.
    Options,
    Finish,
}

/// The Export wizard's format (`ExportAppliancePage`): OVF/OVA first, as
/// the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Ovf,
    Xva,
}

/// The Export wizard (`ExportApplianceWizard`).
pub struct ExportWizard {
    connection: ConnectionId,
    pub(crate) page: ExportPage,
    pub(crate) name: String,
    pub(crate) folder: String,
    pub(crate) format: ExportFormat,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    rbac_blocked_ovf: Option<String>,
    pub(crate) ticked: Vec<Ref<Vm>>,
    search: String,
    pub(crate) verify: bool,
    pub(crate) error: Option<String>,
    /// OVF/OVA: the EULA files, the path being typed, the selected one and
    /// the text shown by View.
    pub(crate) eulas: Vec<String>,
    pub(crate) eula_input: String,
    pub(crate) eula_selected: Option<usize>,
    pub(crate) eula_view: Option<String>,
    pub(crate) manifest: bool,
    pub(crate) ova: bool,
    pub(crate) compress: bool,
}

impl ExportWizard {
    fn new(conn: &ConnectionView, vms: Vec<Ref<Vm>>) -> Self {
        let rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), EXPORT_RBAC) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                xva_strings().rbac_export,
                &[("role", &i18n::session_roles_text(conn))],
            )),
        };
        let rbac_blocked_ovf = match rbac::check(
            &conn.model,
            conn.rbac.as_deref(),
            crate::ovf_ui::EXPORT_OVF_RBAC,
        ) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                crate::ovf_strings::ovf_strings().rbac_export_appliance,
                &[("role", &i18n::session_roles_text(conn))],
            )),
        };
        let name = match vms.as_slice() {
            [one] => vm_name(conn, one),
            _ => String::new(),
        };
        Self {
            connection: conn.id,
            page: ExportPage::Appliance,
            name,
            folder: rules::downloads_folder()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            format: ExportFormat::Ovf,
            rbac_needed,
            rbac_blocked,
            rbac_blocked_ovf,
            ticked: vms,
            search: String::new(),
            verify: false,
            error: None,
            eulas: Vec::new(),
            eula_input: String::new(),
            eula_selected: None,
            eula_view: None,
            manifest: false,
            ova: false,
            compress: false,
        }
    }

    /// The RBAC page's refusal for the chosen format.
    fn blocked(&self) -> Option<&String> {
        match self.format {
            ExportFormat::Ovf => self.rbac_blocked_ovf.as_ref(),
            ExportFormat::Xva => self.rbac_blocked.as_ref(),
        }
    }

    fn pages(&self) -> Vec<ExportPage> {
        let mut p = vec![ExportPage::Appliance];
        if self.rbac_needed {
            p.push(ExportPage::Rbac);
        }
        p.push(ExportPage::Vms);
        if self.format == ExportFormat::Ovf {
            p.extend([ExportPage::Eula, ExportPage::Options]);
        }
        p.push(ExportPage::Finish);
        p
    }

    fn page_name(p: ExportPage) -> &'static str {
        let x = xva_strings();
        match p {
            ExportPage::Appliance => x.page_appliance,
            ExportPage::Rbac => x.page_rbac,
            ExportPage::Vms => x.page_vms,
            ExportPage::Eula => crate::ovf_strings::ovf_strings().page_eula,
            ExportPage::Options => crate::ovf_strings::ovf_strings().page_options,
            ExportPage::Finish => x.page_finish,
        }
    }

    /// `CheckPathValid`, without a message for an empty field.
    fn path_error(&self) -> Option<Option<&'static str>> {
        let x = xva_strings();
        let (name, folder) = (self.name.trim(), self.folder.trim());
        if name.is_empty() || folder.is_empty() {
            return Some(None);
        }
        if !rules::valid_file_name(name) {
            return Some(Some(x.error_invalid_name));
        }
        if folder.chars().any(char::is_control) {
            return Some(Some(x.error_invalid_dir));
        }
        None
    }

    /// `PageLeaveCore` of the first page: the folder exists, the file does
    /// not, the folder is writable.
    fn leave_appliance(&mut self) -> bool {
        let x = xva_strings();
        self.name = self.name.trim().to_owned();
        self.folder = self.folder.trim().to_owned();
        let dir = Path::new(&self.folder);
        self.error = if !dir.is_dir() {
            Some(x.error_dir_missing.to_owned())
        } else if self.format == ExportFormat::Ovf
            && xen_core::ovf_rules::appliance_exists(dir, &self.name)
        {
            Some(
                crate::ovf_strings::ovf_strings()
                    .error_appliance_exists
                    .to_owned(),
            )
        } else if self.format == ExportFormat::Xva && xva_path(&self.folder, &self.name).exists() {
            Some(x.error_exists.to_owned())
        } else if !writable(dir) {
            Some(x.error_permissions.to_owned())
        } else {
            None
        };
        self.error.is_none()
    }

    /// `CheckSpaceRequirements` when leaving the VMs page (and, for an
    /// OVF, no disk over 2 TiB).
    fn leave_vms(&mut self, conn: &ConnectionView) -> bool {
        if self.format == ExportFormat::Ovf
            && self
                .ticked
                .iter()
                .filter_map(|r| conn.model.vms.get(r))
                .any(|v| xen_core::ovf_rules::too_large_for_ovf(&conn.model, v))
        {
            self.error = Some(fill(
                crate::ovf_strings::ovf_strings().ovf_disk_too_large,
                &[(
                    "size",
                    &xen_core::format::disk_size(xen_core::ovf_rules::OVF_DISK_MAX),
                )],
            ));
            return false;
        }
        let needed: i64 = self
            .ticked
            .iter()
            .filter_map(|r| conn.model.vms.get(r))
            .map(|v| rules::export_space(&conn.model, v))
            .sum();
        self.error = match rules::free_space(Path::new(&self.folder)) {
            Some(free) if needed as u64 > free => Some(fill(
                xva_strings().error_space,
                &[
                    ("free", &xen_core::format::disk_size(free as i64)),
                    ("needed", &xen_core::format::disk_size(needed)),
                ],
            )),
            _ => None,
        };
        self.error.is_none()
    }

    fn page_ok(&self) -> bool {
        match self.page {
            ExportPage::Appliance => self.path_error().is_none(),
            ExportPage::Rbac => self.blocked().is_none(),
            ExportPage::Vms => match self.format {
                ExportFormat::Xva => self.ticked.len() == 1,
                ExportFormat::Ovf => !self.ticked.is_empty(),
            },
            ExportPage::Eula | ExportPage::Options | ExportPage::Finish => true,
        }
    }

    fn appliance_page(&mut self, ui: &mut Ui) {
        let x = xva_strings();
        ui.heading(x.page_appliance_title);
        ui.label(x.appliance_intro);
        ui.add_space(8.0);
        Grid::new("export-appliance")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                let l = ui.label(x.label_name);
                ui.add(TextEdit::singleline(&mut self.name).desired_width(420.0))
                    .labelled_by(l.id);
                ui.end_row();
                let l = ui.label(x.label_location);
                ui.add(
                    TextEdit::singleline(&mut self.folder)
                        .desired_width(420.0)
                        .hint_text(x.location_hint),
                )
                .labelled_by(l.id);
                ui.end_row();
                ui.label(x.label_format);
                let o = crate::ovf_strings::ovf_strings();
                let shown = match self.format {
                    ExportFormat::Ovf => o.format_ovf,
                    ExportFormat::Xva => x.format_xva,
                };
                ComboBox::from_id_salt("export-format")
                    .selected_text(shown)
                    .width(420.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.format, ExportFormat::Ovf, o.format_ovf);
                        ui.selectable_value(&mut self.format, ExportFormat::Xva, x.format_xva);
                    });
                ui.end_row();
            });
        if let Some(Some(e)) = self.path_error() {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }

    fn vms_page(&mut self, ui: &mut Ui, conn: &ConnectionView, show_hidden: bool) {
        let x = xva_strings();
        ui.heading(match self.format {
            ExportFormat::Xva => x.page_vms_title,
            ExportFormat::Ovf => crate::ovf_strings::ovf_strings().page_vms_title_ovf,
        });
        ui.label(x.vms_intro);
        ui.add_space(6.0);
        let pool = connection_name(conn);
        let in_pool = conn.model.hosts.len() > 1
            || conn
                .model
                .pool()
                .is_some_and(|(_, p)| !p.name_label.is_empty());
        ui.horizontal(|ui| {
            ui.label(fill(
                if in_pool {
                    x.vms_in_pool
                } else {
                    x.vms_in_server
                },
                &[("pool", &pool)],
            ));
            ui.add(
                TextEdit::singleline(&mut self.search)
                    .desired_width(180.0)
                    .hint_text(x.search_hint),
            );
        });
        let vms = rules::exportable_vms(&conn.model, show_hidden, &conn.busy_vms);
        let search = self.search.to_lowercase();
        let mut rows: Vec<_> = vms
            .iter()
            .filter(|(_, v)| search.is_empty() || v.name_label.to_lowercase().contains(&search))
            .collect();
        // `columnTick` descending: the ticked first.
        rows.sort_by_key(|(r, _)| !self.ticked.contains(r));
        egui::ScrollArea::vertical()
            .id_salt("export-vms")
            .max_height(200.0)
            .show(ui, |ui| {
                Grid::new("export-vm-grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong(x.col_vm);
                        ui.strong(x.col_description);
                        ui.strong(x.col_size);
                        ui.strong(x.col_vapp);
                        ui.end_row();
                        for (r, v) in rows {
                            let mut on = self.ticked.contains(r);
                            if ui.checkbox(&mut on, &v.name_label).changed() {
                                if on {
                                    self.ticked.push((*r).clone());
                                } else {
                                    self.ticked.retain(|t| t != *r);
                                }
                                self.error = None;
                            }
                            ui.label(&v.name_description);
                            ui.label(xen_core::format::disk_size(rules::export_space(
                                &conn.model,
                                v,
                            )));
                            ui.label(
                                xen_core::vapp::vapp_of(&conn.model, v)
                                    .map(|(_, a)| a.name_label.as_str())
                                    .unwrap_or(x.vapp_none),
                            );
                            ui.end_row();
                        }
                    });
            });
        // Ticked VMs no longer exportable drop out.
        let exportable: HashSet<&Ref<Vm>> = vms.iter().map(|(r, _)| *r).collect();
        self.ticked.retain(|r| exportable.contains(r));
        ui.horizontal(|ui| {
            let count = self.ticked.len();
            ui.label(if count == 1 {
                x.one_vm_selected.to_owned()
            } else {
                fill(x.vms_selected, &[("count", &count.to_string())])
            });
            if ui
                .add_enabled(count < vms.len(), Button::new(x.select_all))
                .clicked()
            {
                self.ticked = vms.iter().map(|(r, _)| (*r).clone()).collect();
            }
            if ui
                .add_enabled(count > 0, Button::new(x.clear_all))
                .clicked()
            {
                self.ticked.clear();
            }
        });
        if self.ticked.len() > 1 && self.format == ExportFormat::Xva {
            ui.label(RichText::new(x.only_one_xva).italics());
        }
        if self.format == ExportFormat::Ovf
            && self
                .ticked
                .iter()
                .filter_map(|r| conn.model.vms.get(r))
                .any(|v| xen_core::ovf_rules::has_vtpm(v))
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                crate::ovf_strings::ovf_strings().vtpm_warning,
            );
        }
    }

    fn finish_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        if self.format == ExportFormat::Ovf {
            self.ovf_finish_page(ui, conn);
            return;
        }
        let x = xva_strings();
        ui.heading(x.page_finish_export_title);
        ui.label(x.finish_export_intro);
        ui.add_space(6.0);
        let vms: Vec<String> = self.ticked.iter().map(|r| vm_name(conn, r)).collect();
        summary(
            ui,
            "export-summary",
            &[
                (x.summary_package.to_owned(), self.name.clone()),
                (x.summary_destination.to_owned(), self.folder.clone()),
                (x.summary_vms.to_owned(), vms.join("\n")),
            ],
        );
        ui.add_space(8.0);
        ui.checkbox(&mut self.verify, x.verify);
        if self.verify {
            ui.colored_label(ui.visuals().warn_fg_color, x.verify_warning);
        }
    }

    /// Draws the wizard; `Ok(Some)` once finished, `Err` once cancelled.
    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let x = xva_strings();
        let n = crate::net_strings::net_strings();
        let Some(conn) = state.connection(self.connection) else {
            return Err(());
        };
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let mut open = true;
        let mut outcome = Ok(None);
        let title = match self.format {
            ExportFormat::Ovf => crate::ovf_strings::ovf_strings().export_ovf_title,
            ExportFormat::Xva => x.export_wizard_title,
        };
        egui::Window::new(title)
            .id(egui::Id::unique("export-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([800.0, 470.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(640.0);
                        egui::ScrollArea::vertical()
                            .id_salt("export-wizard-page")
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                ExportPage::Appliance => self.appliance_page(ui),
                                ExportPage::Rbac => {
                                    ui.heading(x.page_rbac);
                                    ui.label(x.rbac_description);
                                    ui.add_space(6.0);
                                    match self.blocked() {
                                        Some(why) => {
                                            ui.colored_label(ui.visuals().error_fg_color, why);
                                        }
                                        None => {
                                            ui.label(x.rbac_click_next);
                                        }
                                    }
                                }
                                ExportPage::Vms => self.vms_page(ui, conn, show_hidden),
                                ExportPage::Eula => self.eula_page(ui),
                                ExportPage::Options => self.options_page(ui),
                                ExportPage::Finish => self.finish_page(ui, conn),
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.separator();
                let last = self.page == ExportPage::Finish;
                let (previous, next, cancel) = nav(
                    ui,
                    index > 0,
                    self.page_ok(),
                    if last { n.finish } else { n.next },
                );
                if previous {
                    self.error = None;
                    self.page = pages[index - 1];
                }
                if next {
                    if last && self.format == ExportFormat::Ovf {
                        outcome = Ok(self.ovf_request());
                    } else if last {
                        let vm = self.ticked.first().cloned();
                        outcome = match vm {
                            Some(vm) => Ok(Some(ActionRequest::ExportXva {
                                vm,
                                path: xva_path(&self.folder, &self.name).display().to_string(),
                                verify: self.verify,
                            })),
                            None => Ok(None),
                        };
                    } else {
                        let ok = match self.page {
                            ExportPage::Appliance => self.leave_appliance(),
                            ExportPage::Vms => self.leave_vms(conn),
                            _ => true,
                        };
                        if ok {
                            self.error = None;
                            // The format may have changed the pages.
                            let pages = self.pages();
                            let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
                            self.page = pages[(index + 1).min(pages.len() - 1)];
                        }
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

// ---- Export to File (templates and snapshots) -----------------------------------------------

/// `ExportVMDialog` with `ExportVMCommand`'s free space question.
pub struct ExportFileDialog {
    connection: ConnectionId,
    vm: Ref<Vm>,
    pub(crate) path: String,
    pub(crate) verify: bool,
    error: Option<String>,
    /// The free space question is showing: its text.
    question: Option<String>,
}

impl ExportFileDialog {
    fn new(conn: &ConnectionView, vm: Ref<Vm>) -> Self {
        let folder = rules::downloads_folder().unwrap_or_default();
        Self {
            connection: conn.id,
            path: xva_path(&folder.display().to_string(), &vm_name(conn, &vm))
                .display()
                .to_string(),
            vm,
            verify: true,
            error: None,
            question: None,
        }
    }

    fn request(&self) -> ActionRequest {
        let path = PathBuf::from(self.path.trim());
        let path = if path.extension().is_none() {
            path.with_extension("xva")
        } else {
            path
        };
        ActionRequest::ExportXva {
            vm: self.vm.clone(),
            path: path.display().to_string(),
            verify: self.verify,
        }
    }

    /// Draws the dialog; `Ok(Some)` to export, `Err` once closed.
    fn show(&mut self, ctx: &Context, state: &AppState) -> Result<Option<ActionRequest>, ()> {
        let x = xva_strings();
        let n = crate::net_strings::net_strings();
        let Some(conn) = state.connection(self.connection) else {
            return Err(());
        };
        let Some(record) = conn.model.vms.get(&self.vm) else {
            return Err(());
        };
        if let Some(text) = self.question.clone() {
            let mut answer = None;
            window(x.export_file_title.to_owned()).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(x.continue_export).clicked() {
                        answer = Some(0);
                    }
                    if ui.button(x.choose_destination).clicked() {
                        answer = Some(1);
                    }
                    if ui.button(n.cancel).clicked() {
                        answer = Some(2);
                    }
                });
            });
            return match answer {
                Some(0) => Ok(Some(self.request())),
                Some(1) => {
                    self.question = None;
                    Ok(None)
                }
                Some(_) => Err(()),
                None => Ok(None),
            };
        }
        let mut go = false;
        let mut cancel = false;
        window(x.export_file_title.to_owned()).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(RichText::new(&record.name_label).strong());
            let l = ui.label(x.file_name);
            ui.add(TextEdit::singleline(&mut self.path).desired_width(480.0))
                .labelled_by(l.id);
            ui.checkbox(&mut self.verify, x.verify);
            error_label(ui, &self.error);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                go = ui
                    .add_enabled(!self.path.trim().is_empty(), Button::new(x.export_button))
                    .clicked();
                cancel = ui.button(n.cancel).clicked();
            });
        });
        if cancel {
            return Err(());
        }
        if !go {
            return Ok(None);
        }
        let request = self.request();
        let ActionRequest::ExportXva { path, .. } = &request else {
            return Ok(None);
        };
        let path = Path::new(path);
        let folder = path.parent().unwrap_or(Path::new("."));
        if !folder.is_dir() {
            self.error = Some(x.error_dir_missing.to_owned());
            return Ok(None);
        }
        // CA-12975: warn when the disks may not fit (100 MiB kept free).
        let needed = rules::export_space(&conn.model, record);
        if let Some(free) = rules::free_space(folder)
            && needed as u64 >= free.saturating_sub(100 << 20)
        {
            self.question = Some(fill(
                x.not_enough_space,
                &[
                    ("needed", &xen_core::format::disk_size(needed)),
                    ("free", &xen_core::format::disk_size(free as i64)),
                    ("vm", &record.name_label),
                ],
            ));
            return Ok(None);
        }
        Ok(Some(request))
    }
}

// ---- the Import wizard ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportPage {
    Source,
    Home,
    Rbac,
    Storage,
    Network,
    Finish,
}

/// Uncompressing an `.xva.gz` in the background.
struct Uncompress {
    file: String,
    out: PathBuf,
    done_bytes: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
}

/// One interface of the network page (`VifRow`).
#[derive(Debug, Clone)]
pub(crate) struct VifRow {
    pub(crate) device: String,
    /// What is typed (empty: generated).
    pub(crate) mac: String,
    pub(crate) network: Ref<Network>,
}

/// The Import wizard (`ImportWizard`, XVA).
pub struct ImportWizard {
    pub(crate) page: ImportPage,
    pub(crate) path: String,
    error: Option<String>,
    /// The disks' size, once the source is read.
    capacity: Option<u64>,
    /// The `.gz` question is showing.
    ask_uncompress: bool,
    uncompress: Option<Uncompress>,
    /// The connection and home server (`None`: the pool) chosen.
    pub(crate) target: Option<(ConnectionId, Option<Ref<Host>>)>,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    /// The session may not set a home server (`m_ignoreAffinitySet`).
    rbac_affinity: Option<String>,
    pub(crate) sr: Option<Ref<Sr>>,
    /// The import started from the storage page: newest operation before
    /// it, and the operation once seen.
    started_after: Option<u64>,
    pub(crate) operation: Option<Operation>,
    pub(crate) vm: Option<Ref<Vm>>,
    pub(crate) vifs: Vec<VifRow>,
    selected_vif: Option<usize>,
    pub(crate) start: bool,
    /// An OVF/OVA package or a disk image: the wizard that takes over.
    pub(crate) handoff: Option<Box<crate::ovf_ui::OvfImportWizard>>,
}

impl ImportWizard {
    fn new(target: Option<(ConnectionId, Option<Ref<Host>>)>) -> Self {
        Self {
            page: ImportPage::Source,
            path: String::new(),
            error: None,
            capacity: None,
            ask_uncompress: false,
            uncompress: None,
            target,
            rbac_needed: false,
            rbac_blocked: None,
            rbac_affinity: None,
            sr: None,
            started_after: None,
            operation: None,
            vm: None,
            vifs: Vec::new(),
            selected_vif: None,
            start: true,
            handoff: None,
        }
    }

    /// The wizard back on its source page with `path` (Previous from the
    /// OVF/OVA or disk image pages).
    pub(crate) fn back_to_source(
        path: String,
        target: Option<(ConnectionId, Option<Ref<Host>>)>,
    ) -> Self {
        let mut w = Self::new(target);
        w.path = path;
        w
    }

    fn pages(&self) -> Vec<ImportPage> {
        let mut p = vec![ImportPage::Source, ImportPage::Home];
        if self.rbac_needed {
            p.push(ImportPage::Rbac);
        }
        p.extend([ImportPage::Storage, ImportPage::Network, ImportPage::Finish]);
        p
    }

    fn page_name(p: ImportPage) -> &'static str {
        let x = xva_strings();
        match p {
            ImportPage::Source => x.page_source,
            ImportPage::Home => x.page_home,
            ImportPage::Rbac => x.page_rbac,
            ImportPage::Storage => x.page_storage,
            ImportPage::Network => x.page_network,
            ImportPage::Finish => x.page_finish,
        }
    }

    fn running(&self) -> bool {
        self.operation.as_ref().is_some_and(Operation::is_running)
            || (self.started_after.is_some() && self.operation.is_none())
    }

    /// `CheckIsSupportedType` and `CheckPathExists`; an OVF/OVA package or
    /// a disk image is handed over to [`crate::ovf_ui::OvfImportWizard`].
    fn leave_source(&mut self) -> bool {
        use xen_core::ovf_rules::{SourceKind, source_kind};
        let x = xva_strings();
        let o = crate::ovf_strings::ovf_strings();
        self.path = self.path.trim().to_owned();
        let kind = source_kind(&self.path);
        self.error = if kind == SourceKind::Unknown {
            Some(o.error_unknown_type.to_owned())
        } else if !Path::new(&self.path).is_file() {
            Some(x.error_path_missing.to_owned())
        } else {
            None
        };
        if self.error.is_some() {
            return false;
        }
        let compressed = matches!(
            kind,
            SourceKind::Xva { compressed: true }
                | SourceKind::Ovf {
                    compressed: true,
                    ..
                }
        );
        let dir = Path::new(&self.path).parent().unwrap_or(Path::new("."));
        if compressed {
            if !writable(dir) {
                self.error = Some(x.error_source_readonly.to_owned());
                return false;
            }
            self.ask_uncompress = true;
            return false;
        }
        match kind {
            SourceKind::Xva { .. } => {
                self.capacity = Some(rules::disk_capacity(Path::new(&self.path)));
                true
            }
            SourceKind::Ovf { ova, .. } => {
                if ova && !writable(dir) {
                    self.error = Some(o.error_ova_readonly.to_owned());
                    return false;
                }
                match crate::ovf_ui::OvfImportWizard::package(&self.path, self.target.clone()) {
                    Ok(w) => self.handoff = Some(Box::new(w)),
                    Err(e) => self.error = Some(e),
                }
                false
            }
            SourceKind::Image => {
                match crate::ovf_ui::OvfImportWizard::image(&self.path, self.target.clone()) {
                    Ok(w) => self.handoff = Some(Box::new(w)),
                    Err(e) => self.error = Some(e),
                }
                false
            }
            SourceKind::Unknown => false,
        }
    }

    fn start_uncompress(&mut self) {
        let from = PathBuf::from(&self.path);
        let out = rules::uncompressed_path(&from);
        let done_bytes = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let (d, c, r, o, f) = (
            done_bytes.clone(),
            cancel.clone(),
            result.clone(),
            out.clone(),
            from.clone(),
        );
        std::thread::spawn(move || {
            let outcome = rules::uncompress(&f, &o, &|n| d.store(n, Ordering::Relaxed), &|| {
                c.load(Ordering::Relaxed)
            });
            let outcome = outcome.map_err(|e| e.to_string()).map(|()| {
                // The original deletes the compressed file once done.
                if let Err(e) = std::fs::remove_file(&f) {
                    tracing::warn!(error = %e, "could not delete the compressed file");
                }
            });
            *r.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        });
        self.uncompress = Some(Uncompress {
            file: from
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            out,
            done_bytes,
            cancel,
            result,
        });
    }

    /// The uncompression ended: the wizard goes on with the `.xva`.
    fn poll_uncompress(&mut self) -> bool {
        let Some(u) = &self.uncompress else {
            return false;
        };
        let done = u
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(done) = done else { return false };
        let u = self.uncompress.take().expect("checked");
        match done {
            Ok(()) => {
                self.path = u.out.display().to_string();
                // An `.ova.gz` became an `.ova`: on to its own pages.
                self.leave_source()
            }
            Err(e) => {
                tracing::warn!(error = %e, "uncompressing failed");
                self.error = Some(fill(xva_strings().uncompress_failed, &[("file", &u.file)]));
                false
            }
        }
    }

    /// `ConfigureRbacPage` for the chosen connection.
    fn configure_rbac(&mut self, conn: &ConnectionView) {
        let x = xva_strings();
        self.rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        let role = i18n::session_roles_text(conn);
        self.rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), IMPORT_RBAC) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(x.rbac_import, &[("role", &role)])),
        };
        self.rbac_affinity =
            match rbac::check(&conn.model, conn.rbac.as_deref(), &["vm.set_affinity"]) {
                RbacCheck::Allowed => None,
                RbacCheck::Denied { .. } => Some(fill(x.rbac_affinity, &[("role", &role)])),
            };
    }

    fn page_ok(&self, state: &AppState) -> bool {
        match self.page {
            ImportPage::Source => {
                !self.path.trim().is_empty() && self.uncompress.is_none() && !self.ask_uncompress
            }
            ImportPage::Home => self
                .target
                .as_ref()
                .is_some_and(|(c, _)| state.connection(*c).is_some()),
            ImportPage::Rbac => self.rbac_blocked.is_none(),
            ImportPage::Storage => self.sr.is_some() && !self.running(),
            ImportPage::Network => self
                .vifs
                .iter()
                .all(|v| v.mac.trim().is_empty() || crate::new_vm::valid_mac(v.mac.trim())),
            ImportPage::Finish => true,
        }
    }

    fn source_page(&mut self, ui: &mut Ui) {
        let x = xva_strings();
        ui.heading(x.page_source_title);
        ui.label(x.source_intro);
        ui.add_space(8.0);
        ui.add_enabled_ui(self.uncompress.is_none(), |ui| {
            let l = ui.label(x.label_filename);
            if ui
                .add(
                    TextEdit::singleline(&mut self.path)
                        .desired_width(560.0)
                        .hint_text(x.path_hint),
                )
                .labelled_by(l.id)
                .changed()
            {
                self.error = None;
                self.capacity = None;
            }
        });
        if let Some(u) = &self.uncompress {
            ui.add_space(6.0);
            ui.label(fill(
                x.uncompressing,
                &[
                    ("file", &u.file),
                    (
                        "done",
                        &xen_core::format::disk_size(u.done_bytes.load(Ordering::Relaxed) as i64),
                    ),
                ],
            ));
            ui.add(ProgressBar::new(0.0).animate(true));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        if self.ask_uncompress {
            let from = PathBuf::from(&self.path);
            ui.add_space(6.0);
            ui.label(fill(
                x.uncompress_question,
                &[
                    ("console", branding::PRODUCT_NAME),
                    (
                        "out",
                        &rules::uncompressed_path(&from)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ),
                    (
                        "file",
                        &from
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ),
                ],
            ));
            let o = crate::ops_strings::ops_strings();
            ui.horizontal(|ui| {
                if ui.button(o.yes).clicked() {
                    self.ask_uncompress = false;
                    self.start_uncompress();
                }
                if ui.button(o.no).clicked() {
                    self.ask_uncompress = false;
                }
            });
        }
        if let Some(c) = self.capacity {
            ui.label(fill(
                x.disk_capacity,
                &[("size", &xen_core::format::disk_size(c as i64))],
            ));
        }
    }

    fn home_page(&mut self, ui: &mut Ui, state: &AppState) {
        let x = xva_strings();
        ui.heading(x.page_home_title);
        ui.label(x.home_blurb);
        ui.add_space(6.0);
        if target_picker(ui, state, &mut self.target) {
            // `NotifyNextPagesOfChange`: the SR is chosen again.
            self.sr = None;
        }
    }

    fn storage_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let x = xva_strings();
        ui.heading(x.page_storage_title);
        ui.label(x.storage_hint);
        ui.add_space(6.0);
        let host = self.target.as_ref().and_then(|(_, h)| h.clone());
        let srs = rules::import_srs(&conn.model, host.as_ref());
        if self.sr.is_none() {
            self.sr = srs.first().map(|(r, _)| (*r).clone());
        }
        let running = self.running();
        if srs.is_empty() {
            ui.label(RichText::new(x.no_srs).italics());
        }
        ui.add_enabled_ui(!running, |ui| {
            Grid::new("import-srs")
                .num_columns(3)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.strong(x.col_sr);
                    ui.strong(x.col_sr_description);
                    ui.strong(x.col_sr_free);
                    ui.end_row();
                    for (r, sr) in &srs {
                        let on = self.sr.as_ref() == Some(*r);
                        if ui.selectable_label(on, &sr.name_label).clicked() {
                            self.sr = Some((*r).clone());
                        }
                        ui.label(&sr.name_description);
                        ui.label(fill(
                            x.sr_size_free,
                            &[
                                (
                                    "free",
                                    &xen_core::format::disk_size(
                                        sr.physical_size - sr.physical_utilisation,
                                    ),
                                ),
                                ("size", &xen_core::format::disk_size(sr.physical_size)),
                            ],
                        ));
                        ui.end_row();
                    }
                });
        });
        if let Some(op) = &self.operation {
            ui.add_space(8.0);
            let title = i18n::operation_title(&op.kind, &op.server);
            ui.label(RichText::new(title).strong());
            if let Some(step) = &op.step {
                ui.label(i18n::operation_step(step));
            }
            if op.is_running() {
                ui.add(ProgressBar::new(op.progress.unwrap_or(0.0) as f32).show_percentage());
            }
        }
    }

    fn network_page(&mut self, ui: &mut Ui, conn: &ConnectionView, show_hidden: bool) {
        let x = xva_strings();
        ui.heading(x.page_network_title);
        ui.label(x.network_blurb);
        ui.add_space(6.0);
        ui.label(x.network_label);
        let affinity = self.target.as_ref().and_then(|(_, h)| h.clone());
        let networks = rules::import_networks(&conn.model, affinity.as_ref(), show_hidden);
        let name = |r: &Ref<Network>| {
            conn.model
                .networks
                .get(r)
                .map(|n| objects::network_name(&conn.model, n))
                .unwrap_or_default()
        };
        Grid::new("import-vifs")
            .num_columns(3)
            .striped(true)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.strong(x.col_name);
                ui.strong(x.col_mac);
                ui.strong(x.col_network);
                ui.end_row();
                for (i, row) in self.vifs.iter_mut().enumerate() {
                    let hidden = conn
                        .model
                        .networks
                        .get(&row.network)
                        .is_some_and(|n| objects::is_guest_installer_network(n));
                    if hidden && !show_hidden {
                        continue;
                    }
                    let label = fill(x.interface, &[("device", &row.device)]);
                    if ui
                        .selectable_label(self.selected_vif == Some(i), &label)
                        .clicked()
                    {
                        self.selected_vif = Some(i);
                    }
                    ui.add(
                        TextEdit::singleline(&mut row.mac)
                            .desired_width(160.0)
                            .hint_text(x.auto_mac),
                    );
                    ComboBox::from_id_salt(format!("import-vif-net-{i}"))
                        .selected_text(name(&row.network))
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for (r, n) in &networks {
                                if ui
                                    .selectable_label(
                                        &row.network == *r,
                                        objects::network_name(&conn.model, n),
                                    )
                                    .clicked()
                                {
                                    row.network = (*r).clone();
                                }
                            }
                        });
                    ui.end_row();
                }
            });
        let max = self
            .vm
            .as_ref()
            .and_then(|v| conn.model.vms.get(v))
            .map_or(7, |v| rules::max_vifs(v));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!networks.is_empty(), Button::new(x.add))
                .clicked()
            {
                if self.vifs.len() >= max {
                    self.error = Some(x.max_vifs.to_owned());
                } else {
                    let mut device = 0;
                    while self.vifs.iter().any(|v| v.device == device.to_string()) {
                        device += 1;
                    }
                    self.vifs.push(VifRow {
                        device: device.to_string(),
                        mac: String::new(),
                        network: networks[0].0.clone(),
                    });
                }
            }
            if ui
                .add_enabled(self.selected_vif.is_some(), Button::new(x.delete))
                .clicked()
                && let Some(i) = self.selected_vif.take()
                && i < self.vifs.len()
            {
                self.vifs.remove(i);
                self.error = None;
            }
        });
        if !self
            .vifs
            .iter()
            .all(|v| v.mac.trim().is_empty() || crate::new_vm::valid_mac(v.mac.trim()))
        {
            ui.colored_label(
                ui.visuals().error_fg_color,
                crate::prov_strings::prov_strings().invalid_mac,
            );
        }
    }

    fn finish_page(&mut self, ui: &mut Ui, state: &AppState, conn: &ConnectionView) {
        let x = xva_strings();
        ui.heading(x.page_finish_import_title);
        ui.label(x.finish_import_intro);
        ui.add_space(6.0);
        let vm = self.vm.as_ref().and_then(|v| conn.model.vms.get(v));
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let mut rows = vec![(
            x.summary_vm_name.to_owned(),
            vm.map(|v| v.name_label.clone()).unwrap_or_default(),
        )];
        let home = match self.target.as_ref().and_then(|(_, h)| h.as_ref()) {
            Some(h) => conn
                .model
                .hosts
                .get(h)
                .map(|h| h.name_label.clone())
                .unwrap_or_default(),
            None => connection_name(conn),
        };
        rows.push((x.summary_home.to_owned(), home));
        rows.push((
            x.summary_storage.to_owned(),
            self.sr
                .as_ref()
                .and_then(|s| conn.model.srs.get(s))
                .map(|s| s.name_label.clone())
                .unwrap_or_default(),
        ));
        let mut first = true;
        for v in &self.vifs {
            let Some(n) = conn.model.networks.get(&v.network) else {
                continue;
            };
            if objects::is_guest_installer_network(n) && !show_hidden {
                continue;
            }
            rows.push((
                if first {
                    x.summary_network.to_owned()
                } else {
                    String::new()
                },
                objects::network_name(&conn.model, n),
            ));
            first = false;
        }
        summary(ui, "import-summary", &rows);
        if vm.is_some_and(|v| !v.is_a_template) {
            ui.add_space(8.0);
            ui.checkbox(&mut self.start, x.start_vms);
        }
    }

    /// Follows the transfer started from the storage page.
    fn watch(&mut self, state: &AppState, show_hidden: bool) {
        let Some(after) = self.started_after else {
            return;
        };
        let Some((c, _)) = &self.target else { return };
        let op = state
            .operations
            .iter()
            .find(|o| {
                o.id > after
                    && o.connection == Some(*c)
                    && matches!(&o.kind, OperationKind::Transfer(xen_core::operations::TransferKind::Import { file, .. }) if *file == self.path)
            })
            .map(|o| (**o).clone());
        let Some(op) = op else { return };
        if !op.is_running() && self.page != ImportPage::Finish {
            match &op.status {
                OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
                    self.error = Some(i18n::describe_xapi_error_in(
                        state.connection(*c).map(|c| &**c),
                        e,
                    ));
                }
                OperationStatus::Failed(f) => {
                    self.error = Some(i18n::describe_operation_failure(
                        state.connection(*c).map(|c| &**c),
                        f,
                    ));
                }
                OperationStatus::Cancelled => {
                    self.error = Some(i18n::operation_status_label(&op.status).to_owned());
                }
                _ => {}
            }
            if !matches!(op.status, OperationStatus::Succeeded) {
                // `m_importXvaAction_Completed`: try again from the storage page.
                self.page = ImportPage::Storage;
                self.started_after = None;
                self.operation = None;
                self.vm = None;
                return;
            }
        }
        if self.page == ImportPage::Storage
            && self.vm.is_none()
            && let Some(vm) = op.created.clone()
            && let Some(conn) = state.connection(*c)
            && let Some(record) = conn.model.vms.get(&vm)
        {
            // `ImportVmCompleted`: on to the networks of the imported VM.
            let affinity = self.target.as_ref().and_then(|(_, h)| h.clone());
            let offered = rules::import_networks(&conn.model, affinity.as_ref(), show_hidden);
            self.vifs = rules::default_import_vifs(&conn.model, record, &offered)
                .into_iter()
                .map(|(device, mac, network)| VifRow {
                    device,
                    mac,
                    network,
                })
                .collect();
            self.vm = Some(vm);
            self.page = ImportPage::Network;
            self.error = None;
        }
        self.operation = Some(op);
    }

    fn finish(&self) -> Option<ImportFinish> {
        Some(ImportFinish {
            start: self.start,
            vifs: Some(
                self.vifs
                    .iter()
                    .map(|v| ImportVif {
                        device: v.device.clone(),
                        network: v.network.clone(),
                        mac: v.mac.trim().to_lowercase(),
                    })
                    .collect(),
            ),
        })
    }

    /// Draws the wizard; `Err` once it closed (finished or cancelled), with
    /// what to send.
    fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> Result<(), ()> {
        let x = xva_strings();
        let n = crate::net_strings::net_strings();
        let show_hidden = state.settings.preferences.show_hidden_objects;
        self.watch(state, show_hidden);
        if self.poll_uncompress() {
            self.page = ImportPage::Home;
        }
        if self.running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let conn = self
            .target
            .as_ref()
            .and_then(|(c, _)| state.connection(*c))
            .cloned();
        let mut open = true;
        let mut close = false;
        let mut cancelled = false;
        let title = if self.page == ImportPage::Source {
            x.import_wizard_title
        } else {
            x.import_xva_title
        };
        egui::Window::new(title)
            .id(egui::Id::unique("import-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([800.0, 470.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let names: Vec<&str> = pages.iter().map(|p| Self::page_name(*p)).collect();
                    page_list(ui, &names, index);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(640.0);
                        egui::ScrollArea::vertical()
                            .id_salt("import-wizard-page")
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match (self.page, conn.as_deref()) {
                                (ImportPage::Source, _) => self.source_page(ui),
                                (ImportPage::Home, _) => self.home_page(ui, state),
                                (ImportPage::Rbac, _) => {
                                    ui.heading(x.page_rbac);
                                    ui.label(x.rbac_description);
                                    ui.add_space(6.0);
                                    if let Some(why) = &self.rbac_blocked {
                                        ui.colored_label(ui.visuals().error_fg_color, why);
                                    } else {
                                        if let Some(w) = &self.rbac_affinity {
                                            ui.colored_label(ui.visuals().warn_fg_color, w);
                                        }
                                        ui.label(x.rbac_click_next);
                                    }
                                }
                                (ImportPage::Storage, Some(c)) => self.storage_page(ui, c),
                                (ImportPage::Network, Some(c)) => {
                                    self.network_page(ui, c, show_hidden)
                                }
                                (ImportPage::Finish, Some(c)) => self.finish_page(ui, state, c),
                                _ => {}
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.separator();
                let last = self.page == ImportPage::Finish;
                let next_label = match self.page {
                    ImportPage::Storage => x.import_button,
                    _ if last => n.finish,
                    _ => n.next,
                };
                let can_previous = index > 0
                    && !self.running()
                    && !matches!(self.page, ImportPage::Network)
                    && self.uncompress.is_none();
                let (previous, next, cancel) =
                    nav(ui, can_previous, self.page_ok(state), next_label);
                if previous {
                    self.error = None;
                    self.page = pages[index - 1];
                }
                if next {
                    match self.page {
                        ImportPage::Source => {
                            if self.leave_source() {
                                self.error = None;
                                self.page = pages[index + 1];
                            }
                        }
                        ImportPage::Home => {
                            if let Some(c) = &conn {
                                self.configure_rbac(c);
                            }
                            self.page = self.pages()[index + 1];
                        }
                        ImportPage::Storage => {
                            // `PageLeaveCore`: the transfer starts here.
                            if let (Some(c), Some(sr)) = (&conn, self.sr.clone()) {
                                self.error = None;
                                self.started_after =
                                    Some(state.operations.iter().map(|o| o.id).max().unwrap_or(0));
                                self.operation = None;
                                let affinity = if self.rbac_affinity.is_some() {
                                    None
                                } else {
                                    self.target.as_ref().and_then(|(_, h)| h.clone())
                                };
                                ops.request(
                                    c,
                                    ActionRequest::ImportXva {
                                        path: self.path.clone(),
                                        sr,
                                        affinity,
                                    },
                                    backend,
                                );
                            }
                        }
                        ImportPage::Finish => {
                            if let Some(op) = &self.operation {
                                backend.send(Command::FinishImport {
                                    operation: op.id,
                                    finish: self.finish(),
                                });
                            }
                            close = true;
                        }
                        _ => {
                            self.error = None;
                            self.page = pages[index + 1];
                        }
                    }
                }
                if cancel {
                    cancelled = true;
                }
            });
        if !open || cancelled {
            // `OnCancel`: EndWizard(false, null), then Cancel.
            if let Some(op) = &self.operation
                && op.is_running()
            {
                backend.send(Command::FinishImport {
                    operation: op.id,
                    finish: None,
                });
                backend.send(Command::CancelOperation { operation: op.id });
            }
            if let Some(u) = &self.uncompress {
                u.cancel.store(true, Ordering::Relaxed);
            }
            return Err(());
        }
        if close {
            return Err(());
        }
        Ok(())
    }
}

/// `GlobalSelectHost`: each connected pool (a pool of one shows its
/// server only) and its live servers; true when the choice changed.
pub(crate) fn target_picker(
    ui: &mut Ui,
    state: &AppState,
    target: &mut Option<(ConnectionId, Option<Ref<Host>>)>,
) -> bool {
    let mut chosen = None;
    for conn in state.connections.iter().filter(|c| c.status.is_connected()) {
        let m = &conn.model;
        let pool_row = m.hosts.len() > 1 || m.pool().is_some_and(|(_, p)| !p.name_label.is_empty());
        let mut hosts: Vec<_> = m.hosts.iter().collect();
        hosts.sort_by(|a, b| objects::natural_cmp(&a.1.name_label, &b.1.name_label));
        if pool_row {
            let on = target.as_ref() == Some(&(conn.id, None));
            if ui.selectable_label(on, connection_name(conn)).clicked() {
                chosen = Some((conn.id, None));
            }
        }
        for (r, h) in hosts {
            let live = objects::host_is_live(m, h);
            let on = target.as_ref() == Some(&(conn.id, Some(r.clone())));
            ui.horizontal(|ui| {
                if pool_row {
                    ui.add_space(20.0);
                }
                if ui
                    .add_enabled(live, egui::Button::selectable(on, &h.name_label))
                    .clicked()
                {
                    chosen = Some((conn.id, Some(r.clone())));
                }
            });
        }
    }
    match chosen {
        Some(c) if target.as_ref() != Some(&c) => {
            *target = Some(c);
            true
        }
        _ => false,
    }
}

// ---- the dialogs together ----------------------------------------------------------------

/// The F11a dialogs of the window.
#[derive(Default)]
pub struct XvaUi {
    pub(crate) export: Option<ExportWizard>,
    pub(crate) export_file: Option<ExportFileDialog>,
    pub(crate) import: Option<ImportWizard>,
    /// The Import wizard on an OVF/OVA package or a disk image (F11b).
    pub(crate) import_ovf: Option<crate::ovf_ui::OvfImportWizard>,
    pub errors: Vec<String>,
}

impl XvaUi {
    /// Opens the dialog of `request` for `conn`.
    pub fn open(&mut self, conn: &ConnectionView, request: DialogRequest) {
        match request {
            DialogRequest::Export(vms) => self.export = Some(ExportWizard::new(conn, vms)),
            DialogRequest::ExportToFile(vm) => {
                self.export_file = Some(ExportFileDialog::new(conn, vm));
            }
            DialogRequest::Import(host) => {
                self.import_ovf = None;
                self.import = Some(ImportWizard::new(Some((conn.id, host))));
            }
            _ => {}
        }
    }

    /// Opens the Import wizard with nothing chosen (the File menu).
    pub fn open_import(&mut self, target: Option<(ConnectionId, Option<Ref<Host>>)>) {
        self.import_ovf = None;
        self.import = Some(ImportWizard::new(target));
    }

    /// Draws the open dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(w) = self.export.as_mut() {
            let conn = state.connection(w.connection).cloned();
            match w.show(ctx, state) {
                Ok(Some(request)) => {
                    if let Some(c) = conn {
                        ops.request(&c, request, backend);
                    }
                    self.export = None;
                }
                Ok(None) => {}
                Err(()) => self.export = None,
            }
        }
        if let Some(d) = self.export_file.as_mut() {
            let conn = state.connection(d.connection).cloned();
            match d.show(ctx, state) {
                Ok(Some(request)) => {
                    if let Some(c) = conn {
                        ops.request(&c, request, backend);
                    }
                    self.export_file = None;
                }
                Ok(None) => {}
                Err(()) => self.export_file = None,
            }
        }
        if let Some(w) = self.import.as_mut()
            && w.show(ctx, state, ops, backend).is_err()
        {
            self.import = None;
        }
        if let Some(w) = self.import.as_mut()
            && let Some(next) = w.handoff.take()
        {
            self.import = None;
            self.import_ovf = Some(*next);
        }
        if let Some(w) = self.import_ovf.as_mut() {
            use crate::ovf_ui::Shown;
            match w.show(ctx, state, ops, backend) {
                Shown::Open => {}
                Shown::Closed => self.import_ovf = None,
                Shown::Back { path, target } => {
                    self.import_ovf = None;
                    self.import = Some(ImportWizard::back_to_source(path, target));
                }
            }
        }
    }
}
