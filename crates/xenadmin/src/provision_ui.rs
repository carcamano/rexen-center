//! VM creation, templates, snapshots and disks in the window: the menu
//! entries, the dialogs (Take Snapshot, revert, Copy VM, Delete VM, Add /
//! Attach / Move / resize a disk, the confirmations), the Snapshots tab,
//! and the VM Storage tab's disk buttons and DVD drives. The New VM wizard
//! is [`crate::new_vm`].
//!
//! Every dialog ends in an [`ActionRequest`] sent through
//! [`OpsUi::dispatch`], so the RBAC check and role elevation of the other
//! operations apply.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use egui::{
    Align2, Button, ComboBox, Context, DragValue, Grid, RichText, ScrollArea, TextEdit, Ui,
};
use xapi::{Host, Ref, Sr, Vbd, Vdi, Vm, VmRecord};
use xen_core::format::disk_size;
use xen_core::objects::{self, VmKind};
use xen_core::operations::ProvisionKind;
use xen_core::provisioning::{self as prov, CopyMode, DeletableDisk, DiskSpec};
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionView, OperationKind,
    OperationStatus,
};

use crate::branding;
use crate::i18n::fill;
use crate::new_vm::NewVmWizard;
use crate::ops_ui::{Entry, OpsUi};
use crate::prov_strings::prov_strings;

/// A dialog a menu, button or tab asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogRequest {
    /// The New VM wizard, with a template (or snapshot) and home server
    /// chosen beforehand.
    NewVm {
        template: Option<Ref<Vm>>,
        affinity: Option<Ref<Host>>,
    },
    CopyVm(Ref<Vm>),
    MakeTemplate(Ref<Vm>),
    DeleteVm(Ref<Vm>),
    TakeSnapshot(Ref<Vm>),
    Revert(Ref<Vm>),
    DeleteSnapshots(Vec<Ref<Vm>>),
    TemplateFromSnapshot(Ref<Vm>),
    NewDisk(Ref<Vm>),
    AttachDisk(Ref<Vm>),
    DetachDisk(Ref<Vm>, Ref<Vbd>),
    DeleteDisk(Ref<Vdi>),
    ResizeDisk(Ref<Vdi>),
    MoveDisk(Ref<Vdi>),
    NewCdDrive(Ref<Vm>),
    InstallTools(Ref<Vm>),
    /// The New SR wizard, or with `reattach` its Attach form for a
    /// detached SR (opened by [`crate::storage_ui::StorageUi`]).
    NewSr {
        reattach: Option<Ref<Sr>>,
    },
    /// The Repair Storage Repository dialog.
    RepairSr(Ref<Sr>),
    /// The SR Properties dialog.
    SrProperties(Ref<Sr>),
}

impl DialogRequest {
    /// A storage dialog ([`crate::storage_ui`]).
    pub fn is_storage(&self) -> bool {
        matches!(
            self,
            Self::NewSr { .. } | Self::RepairSr(_) | Self::SrProperties(_)
        )
    }
}

/// The F7 entries of a guest VM's menu (after the power operations).
pub fn vm_entries(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<Entry> {
    let p = prov_strings();
    let usable = conn.status.is_connected();
    let busy = conn.busy_vms.contains(vm_ref);
    let item = |label, enabled: bool, dialog| Entry::Dialog {
        label,
        enabled: usable && enabled,
        dialog,
    };
    vec![
        Entry::Separator,
        item(
            p.take_snapshot,
            prov::can_snapshot(vm, busy),
            DialogRequest::TakeSnapshot(vm_ref.clone()),
        ),
        item(
            p.copy_vm,
            prov::can_copy(vm, busy),
            DialogRequest::CopyVm(vm_ref.clone()),
        ),
        item(
            p.convert_to_template,
            prov::can_make_template(vm, busy),
            DialogRequest::MakeTemplate(vm_ref.clone()),
        ),
        item(
            install_tools_label(),
            prov::can_install_tools(&conn.model, vm, busy),
            DialogRequest::InstallTools(vm_ref.clone()),
        ),
        Entry::Separator,
        item(
            p.delete_vm,
            prov::can_delete(vm, busy),
            DialogRequest::DeleteVm(vm_ref.clone()),
        ),
    ]
}

/// "Install XCP-ng VM Tools...".
pub fn install_tools_label() -> &'static str {
    use std::sync::OnceLock;
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL.get_or_init(|| {
        let p = prov_strings();
        fill(p.install_tools, &[("tools", p.tools_name)])
    })
}

/// The menu of a template or snapshot.
pub fn template_entries(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<Entry> {
    let p = prov_strings();
    let usable = conn.status.is_connected();
    let busy = conn.busy_vms.contains(vm_ref);
    let item = |label, enabled: bool, dialog| Entry::Dialog {
        label,
        enabled: usable && enabled,
        dialog,
    };
    let new_vm = DialogRequest::NewVm {
        template: Some(vm_ref.clone()),
        affinity: None,
    };
    match objects::vm_kind(vm) {
        VmKind::Snapshot => vec![
            item(p.new_vm_from_snapshot, prov::can_create_from(vm), new_vm),
            item(
                p.template_from_snapshot,
                prov::can_create_from(vm),
                DialogRequest::TemplateFromSnapshot(vm_ref.clone()),
            ),
            Entry::Separator,
            item(
                p.revert_to_snapshot,
                prov::can_revert(&conn.model, vm),
                DialogRequest::Revert(vm_ref.clone()),
            ),
            item(
                p.delete_snapshot,
                prov::can_delete_snapshot(vm),
                DialogRequest::DeleteSnapshots(vec![vm_ref.clone()]),
            ),
        ],
        VmKind::UserTemplate | VmKind::DefaultTemplate => vec![
            item(p.new_vm_from_template, prov::can_create_from(vm), new_vm),
            item(
                p.copy_template,
                prov::can_copy(vm, busy),
                DialogRequest::CopyVm(vm_ref.clone()),
            ),
            Entry::Separator,
            item(
                p.delete_template,
                prov::can_delete(vm, busy),
                DialogRequest::DeleteVm(vm_ref.clone()),
            ),
        ],
        _ => Vec::new(),
    }
}

/// "New VM..." for a pool or host (the host as home server).
pub fn new_vm_entry(conn: &ConnectionView, affinity: Option<Ref<Host>>) -> Entry {
    let p = prov_strings();
    let any_enabled = conn
        .model
        .hosts
        .values()
        .any(|h| h.enabled && objects::host_is_live(&conn.model, h));
    Entry::Dialog {
        label: p.new_vm,
        enabled: conn.status.is_connected() && any_enabled,
        dialog: DialogRequest::NewVm {
            template: None,
            affinity,
        },
    }
}

/// A yes/no question ending in a request.
struct Confirm {
    title: String,
    text: String,
    yes: String,
    request: ActionRequest,
}

struct TakeSnapshot {
    vm: Ref<Vm>,
    name: String,
    description: String,
    memory: bool,
    disks_ok: bool,
    memory_ok: bool,
    /// Revert to this snapshot once the new one is taken.
    then_revert: Option<Ref<Vm>>,
}

struct Revert {
    snapshot: Ref<Vm>,
    vm: Ref<Vm>,
    name: String,
    take_first: bool,
    can_snapshot: bool,
}

struct CopyVm {
    vm: Ref<Vm>,
    template: bool,
    name: String,
    description: String,
    full: bool,
    can_fast: bool,
    can_full: bool,
    sr: Option<Ref<Sr>>,
}

struct DeleteVm {
    vm: Ref<Vm>,
    title: String,
    disks: Vec<DeletableDisk>,
    snapshots: Vec<(Ref<Vm>, String, bool)>,
}

struct TemplateFromSnapshot {
    snapshot: Ref<Vm>,
    name: String,
    description: String,
}

struct NewDisk {
    vm: Ref<Vm>,
    name: String,
    description: String,
    size_gib: f64,
    sr: Option<Ref<Sr>>,
}

struct AttachDisk {
    vm: Ref<Vm>,
    vdi: Option<Ref<Vdi>>,
    read_only: bool,
}

struct Resize {
    vdi: Ref<Vdi>,
    name: String,
    current: i64,
    size_gib: f64,
    allowed: bool,
}

struct MoveDisk {
    vdi: Ref<Vdi>,
    name: String,
    sr: Option<Ref<Sr>>,
}

enum Kind {
    Confirm(Confirm),
    TakeSnapshot(TakeSnapshot),
    Revert(Revert),
    CopyVm(CopyVm),
    DeleteVm(DeleteVm),
    TemplateFromSnapshot(TemplateFromSnapshot),
    NewDisk(NewDisk),
    AttachDisk(AttachDisk),
    Resize(Resize),
    MoveDisk(MoveDisk),
}

struct Dialog {
    connection: ConnectionId,
    kind: Kind,
}

const GIB: f64 = (1u64 << 30) as f64;

/// The F7 part of the window.
#[derive(Default)]
pub struct ProvisionUi {
    dialog: Option<Dialog>,
    /// The New VM wizard, when open.
    pub wizard: Option<NewVmWizard>,
    /// The drive selected on the VM Storage tab.
    disk_selection: Option<(ConnectionId, Ref<Vbd>)>,
    /// The snapshots selected on the Snapshots tab.
    snapshot_selection: HashSet<(ConnectionId, Ref<Vm>)>,
    /// A VM this window asked to create: selected once made.
    pending_select: Option<(ConnectionId, String, Instant)>,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
}

impl ProvisionUi {
    /// Opens the dialog `request` asks for.
    pub fn open(&mut self, conn: &ConnectionView, request: DialogRequest) {
        let p = prov_strings();
        let m = &conn.model;
        let vm_name = |r: &Ref<Vm>| {
            m.vms
                .get(r)
                .map(|v| v.name_label.clone())
                .unwrap_or_default()
        };
        let confirm = |title: &str, text: String, yes: &str, request| {
            Kind::Confirm(Confirm {
                title: title.to_owned(),
                text,
                yes: yes.to_owned(),
                request,
            })
        };
        let kind = match request {
            DialogRequest::NewVm { template, affinity } => {
                self.wizard = Some(NewVmWizard::new(conn, template, affinity));
                return;
            }
            DialogRequest::TakeSnapshot(vm) => {
                let Some(record) = m.vms.get(&vm) else { return };
                if !prov::can_snapshot(record, false) {
                    self.errors.push(p.take_snapshot_error.to_owned());
                    return;
                }
                Kind::TakeSnapshot(take_snapshot_form(record, vm, None))
            }
            DialogRequest::Revert(snapshot) => {
                let Some(snap) = m.vms.get(&snapshot) else {
                    return;
                };
                let vm = snap.snapshot_of.clone();
                let can_snapshot = m
                    .vms
                    .get(&vm)
                    .is_some_and(|v| v.allowed_operations.contains(&xapi::VmOperations::Snapshot));
                Kind::Revert(Revert {
                    snapshot,
                    vm,
                    name: snap.name_label.clone(),
                    take_first: can_snapshot,
                    can_snapshot,
                })
            }
            DialogRequest::DeleteSnapshots(snapshots) => {
                let what = match snapshots.as_slice() {
                    [one] => fill(p.remove_snapshot_one, &[("name", &vm_name(one))]),
                    many => fill(
                        p.remove_snapshot_many,
                        &[("count", &many.len().to_string())],
                    ),
                };
                confirm(
                    branding::PRODUCT_NAME,
                    fill(p.remove_snapshot_text, &[("what", &what)]),
                    p.yes,
                    ActionRequest::DeleteSnapshots { snapshots },
                )
            }
            DialogRequest::TemplateFromSnapshot(snapshot) => {
                let name = vm_name(&snapshot);
                Kind::TemplateFromSnapshot(TemplateFromSnapshot {
                    name: prov::unique_name(
                        &fill(p.template_from_snapshot_name, &[("name", &name)]),
                        prov::vm_names(m),
                    ),
                    description: fill(
                        p.template_from_snapshot_description,
                        &[("product", branding::PRODUCT_NAME), ("name", &name)],
                    ),
                    snapshot,
                })
            }
            DialogRequest::CopyVm(vm) => {
                let Some(record) = m.vms.get(&vm) else { return };
                let template = record.is_a_template;
                let has_disk = !prov::vm_disks(m, record).is_empty();
                let allow_copy = !template
                    || record
                        .allowed_operations
                        .contains(&xapi::VmOperations::Copy);
                let can_full = allow_copy && has_disk;
                let can_fast = !allow_copy
                    || !has_disk
                    || record
                        .allowed_operations
                        .contains(&xapi::VmOperations::Clone);
                Kind::CopyVm(CopyVm {
                    name: prov::unique_name(
                        &fill(p.copy_of, &[("name", &record.name_label)]),
                        prov::vm_names(m),
                    ),
                    description: record.name_description.clone(),
                    full: !can_fast,
                    can_fast,
                    can_full,
                    sr: prov::vm_disks(m, record)
                        .first()
                        .map(|(_, _, _, v)| v.sr.clone())
                        .or_else(|| m.pool().map(|(_, p)| p.default_sr.clone())),
                    template,
                    vm,
                })
            }
            DialogRequest::MakeTemplate(vm) => {
                let name = ellipsis(&vm_name(&vm), 25);
                confirm(
                    p.convert_title,
                    fill(p.convert_text, &[("vm", &name)]),
                    p.convert_button,
                    ActionRequest::MakeTemplate { vm },
                )
            }
            DialogRequest::DeleteVm(vm) => {
                let Some(record) = m.vms.get(&vm) else { return };
                let (disks, snapshots) = prov::deletable_items(m, &vm, record);
                let what = if record.is_a_template {
                    p.what_template
                } else {
                    p.what_vm
                };
                Kind::DeleteVm(DeleteVm {
                    title: fill(p.delete_title, &[("what", what)]),
                    disks,
                    snapshots: snapshots.into_iter().map(|(r, n)| (r, n, false)).collect(),
                    vm,
                })
            }
            DialogRequest::NewDisk(vm) => {
                let Some(record) = m.vms.get(&vm) else { return };
                let srs = prov::disk_srs(m, Some(record));
                let default = m.pool().map(|(_, p)| p.default_sr.clone());
                let sr = srs
                    .iter()
                    .find(|(r, _)| Some(*r) == default.as_ref())
                    .or_else(|| srs.first())
                    .map(|(r, _)| (*r).clone());
                let names: Vec<&str> = m.vdis.values().map(|v| v.name_label.as_str()).collect();
                Kind::NewDisk(NewDisk {
                    vm,
                    name: prov::unique_name(p.new_disk_name, names),
                    description: String::new(),
                    size_gib: 1.0,
                    sr,
                })
            }
            DialogRequest::AttachDisk(vm) => Kind::AttachDisk(AttachDisk {
                vm,
                vdi: None,
                read_only: false,
            }),
            DialogRequest::DetachDisk(vm, vbd) => {
                let system = m.get(&vbd).is_some_and(|b| {
                    m.get(&b.vdi)
                        .is_some_and(|v| prov::is_system_disk(v, Some(b)))
                });
                let (title, text) = if system {
                    (p.detach_system_title, p.detach_system_text)
                } else {
                    (p.detach_disk_title, p.detach_disk_text)
                };
                confirm(
                    title,
                    text.to_owned(),
                    p.yes,
                    ActionRequest::DetachDisk { vm, vbd },
                )
            }
            DialogRequest::DeleteDisk(vdi) => {
                let system = m.get(&vdi).is_some_and(|v| {
                    let vbd = v.vbds.iter().filter_map(|b| m.get(b)).next();
                    prov::is_system_disk(v, vbd.map(|b| &**b))
                });
                let (title, text) = if system {
                    (p.delete_system_title, p.delete_system_text)
                } else {
                    (p.delete_disk_title, p.delete_disk_text)
                };
                confirm(
                    title,
                    text.to_owned(),
                    p.yes,
                    ActionRequest::DeleteDisk { vdi },
                )
            }
            DialogRequest::ResizeDisk(vdi) => {
                let Some(record) = m.get(&vdi) else { return };
                Kind::Resize(Resize {
                    name: record.name_label.clone(),
                    current: record.virtual_size,
                    size_gib: record.virtual_size as f64 / GIB,
                    allowed: record.allowed_operations.is_empty()
                        || record
                            .allowed_operations
                            .contains(&xapi::VdiOperations::Resize)
                        || record
                            .allowed_operations
                            .contains(&xapi::VdiOperations::ResizeOnline),
                    vdi,
                })
            }
            DialogRequest::MoveDisk(vdi) => {
                let Some(record) = m.get(&vdi) else { return };
                Kind::MoveDisk(MoveDisk {
                    name: record.name_label.clone(),
                    sr: prov::move_targets(m, record)
                        .first()
                        .map(|(r, _)| (*r).clone()),
                    vdi,
                })
            }
            DialogRequest::NewCdDrive(vm) => {
                let hvm = m.vms.get(&vm).is_some_and(|v| prov::is_hvm(v));
                let request = ActionRequest::NewCdDrive { vm: vm.clone() };
                if !hvm {
                    self.dispatch_later(conn.id, request);
                    return;
                }
                confirm(
                    p.new_dvd_title,
                    fill(p.new_dvd_text, &[("vm", &vm_name(&vm))]),
                    p.new_dvd_yes,
                    request,
                )
            }
            DialogRequest::NewSr { .. }
            | DialogRequest::RepairSr(_)
            | DialogRequest::SrProperties(_) => {
                return;
            }
            DialogRequest::InstallTools(vm) => confirm(
                &fill(p.install_tools_title, &[("tools", p.tools_name)]),
                fill(
                    p.install_tools_text,
                    &[("product", branding::PRODUCT_NAME), ("tools", p.tools_name)],
                ),
                &fill(p.install_tools_button, &[("tools", p.tools_name)]),
                ActionRequest::InstallTools { vm },
            ),
        };
        self.dialog = Some(Dialog {
            connection: conn.id,
            kind,
        });
    }

    /// Sends a request with no dialog on the next draw.
    fn dispatch_later(&mut self, connection: ConnectionId, request: ActionRequest) {
        self.dialog = Some(Dialog {
            connection,
            kind: Kind::Confirm(Confirm {
                title: String::new(),
                text: String::new(),
                yes: String::new(),
                request,
            }),
        });
    }

    /// Sets the size field of an open resize dialog (tests: a drag value
    /// is awkward to type into).
    #[cfg(test)]
    pub fn set_resize_gib(&mut self, gib: f64) {
        if let Some(Dialog {
            kind: Kind::Resize(r),
            ..
        }) = self.dialog.as_mut()
        {
            r.size_gib = gib;
        }
    }

    /// Sets the name and SR of an open dialog that has them (tests: the
    /// real-host runs need exact names and the test SR).
    #[cfg(test)]
    pub fn set_dialog_fields(&mut self, name: Option<&str>, sr: Option<Ref<Sr>>) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        match &mut dialog.kind {
            Kind::NewDisk(d) => {
                if let Some(n) = name {
                    d.name = n.to_owned();
                }
                if sr.is_some() {
                    d.sr = sr;
                }
            }
            Kind::CopyVm(d) => {
                if let Some(n) = name {
                    d.name = n.to_owned();
                }
                if sr.is_some() {
                    d.sr = sr;
                }
            }
            Kind::TemplateFromSnapshot(d) => {
                if let Some(n) = name {
                    d.name = n.to_owned();
                }
            }
            Kind::TakeSnapshot(d) => {
                if let Some(n) = name {
                    d.name = n.to_owned();
                }
            }
            _ => {}
        }
    }

    /// The VM this window asked to create, once made (to select it).
    pub fn take_created(&mut self, state: &AppState) -> Option<(ConnectionId, Ref<Vm>)> {
        let (conn, name, asked) = self.pending_select.as_ref()?;
        if asked.elapsed() > Duration::from_secs(3600) {
            self.pending_select = None;
            return None;
        }
        let view = state.connection(*conn)?;
        let made = state
            .operations
            .iter()
            .rev()
            .find_map(|o| match (&o.kind, &o.status) {
                (
                    OperationKind::Provision(ProvisionKind::CreateVm { vm, .. }),
                    OperationStatus::Succeeded,
                ) if vm == name
                    && o.connection == Some(*conn)
                    && o.started
                        .elapsed()
                        .is_ok_and(|e| e <= asked.elapsed() + Duration::from_secs(1)) =>
                {
                    o.created.clone()
                }
                _ => None,
            })?;
        let conn = *conn;
        if !view.model.vms.contains_key(&made) {
            return None;
        }
        self.pending_select = None;
        Some((conn, made))
    }

    /// Draws the open dialog and the wizard.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(wizard) = self.wizard.as_mut() {
            match wizard.show(ctx, state) {
                crate::new_vm::WizardOutcome::Open => {}
                crate::new_vm::WizardOutcome::Cancelled => self.wizard = None,
                crate::new_vm::WizardOutcome::Create(connection, spec) => {
                    self.wizard = None;
                    if let Some(conn) = state.connection(connection) {
                        self.pending_select = Some((connection, spec.name.clone(), Instant::now()));
                        ops.dispatch(conn, ActionRequest::NewVm(spec), backend);
                    }
                }
            }
        }
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(dialog.connection) else {
            self.dialog = None;
            return;
        };
        let (close, request, next) = show_dialog(ctx, conn, &mut dialog.kind);
        if close {
            self.dialog = None;
        }
        if let Some(request) = request {
            ops.dispatch(conn, request, backend);
        }
        if let Some(next) = next {
            let connection = conn.id;
            self.dialog = Some(Dialog {
                connection,
                kind: next,
            });
        }
    }

    // ---- the Snapshots tab ---------------------------------------------------

    /// The Snapshots tab of a guest VM (`SnapshotsPage`, list view).
    pub fn snapshots_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
    ) {
        let p = prov_strings();
        let m = &conn.model;
        let snapshots = prov::snapshots(m, vm);
        self.snapshot_selection
            .retain(|(c, r)| *c != conn.id || snapshots.iter().any(|(s, _)| *s == r));
        let selected: Vec<Ref<Vm>> = snapshots
            .iter()
            .filter(|(r, _)| self.snapshot_selection.contains(&(conn.id, (*r).clone())))
            .map(|(r, _)| (*r).clone())
            .collect();
        let usable = conn.status.is_connected();
        let busy = conn.busy_vms.contains(vm_ref);
        let mut open = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    usable && prov::can_snapshot(vm, busy),
                    Button::new(p.take_snapshot_button),
                )
                .clicked()
            {
                open = Some(DialogRequest::TakeSnapshot(vm_ref.clone()));
            }
            let one = match selected.as_slice() {
                [one] => m.vms.get(one).map(|s| (one.clone(), s.clone())),
                _ => None,
            };
            let can_revert = usable
                && vm.current_operations.is_empty()
                && one.as_ref().is_some_and(|(_, s)| prov::can_revert(m, s));
            if ui
                .add_enabled(can_revert, Button::new(p.revert_button))
                .clicked()
                && let Some((r, _)) = &one
            {
                open = Some(DialogRequest::Revert(r.clone()));
            }
            ui.add_enabled_ui(usable && one.is_some(), |ui| {
                ui.menu_button(p.snapshot_actions, |ui| {
                    if let Some((r, s)) = &one {
                        if ui
                            .add_enabled(
                                prov::can_create_from(s),
                                Button::new(p.new_vm_from_snapshot),
                            )
                            .clicked()
                        {
                            open = Some(DialogRequest::NewVm {
                                template: Some(r.clone()),
                                affinity: None,
                            });
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                prov::can_create_from(s),
                                Button::new(p.template_from_snapshot),
                            )
                            .clicked()
                        {
                            open = Some(DialogRequest::TemplateFromSnapshot(r.clone()));
                            ui.close();
                        }
                    }
                });
            });
            let can_delete = usable
                && !selected.is_empty()
                && selected
                    .iter()
                    .all(|r| m.vms.get(r).is_some_and(|s| prov::can_delete_snapshot(s)));
            if ui.add_enabled(can_delete, Button::new(p.delete)).clicked() {
                open = Some(DialogRequest::DeleteSnapshots(selected.clone()));
            }
            if selected.len() > 1 {
                ui.label(
                    RichText::new(fill(
                        p.snapshots_selected,
                        &[("count", &selected.len().to_string())],
                    ))
                    .weak(),
                );
            }
        });
        ui.add_space(6.0);
        if snapshots.is_empty() {
            ui.label(p.snapshots_empty);
        } else {
            ScrollArea::vertical().id_salt("snapshots").show(ui, |ui| {
                Grid::new("snapshots-grid")
                    .striped(true)
                    .num_columns(4)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for header in [p.col_type, p.col_name, p.col_created, p.col_description] {
                            ui.strong(header);
                        }
                        ui.end_row();
                        for (r, s) in &snapshots {
                            let key = (conn.id, (*r).clone());
                            let mut checked = self.snapshot_selection.contains(&key);
                            let kind = if prov::snapshot_has_memory(s) {
                                p.disks_and_memory
                            } else {
                                p.disks_only
                            };
                            if ui.checkbox(&mut checked, kind).changed() {
                                if checked {
                                    self.snapshot_selection.insert(key.clone());
                                } else {
                                    self.snapshot_selection.remove(&key);
                                }
                            }
                            let label = ui.selectable_label(checked, &s.name_label);
                            if label.clicked() {
                                // A click selects this one alone, as a list does.
                                self.snapshot_selection.retain(|(c, _)| *c != conn.id);
                                self.snapshot_selection.insert(key.clone());
                            }
                            label.context_menu(|ui| {
                                for entry in template_entries(conn, r, s) {
                                    if let Entry::Dialog {
                                        label,
                                        enabled,
                                        dialog,
                                    } = entry
                                        && ui.add_enabled(enabled, Button::new(label)).clicked()
                                    {
                                        open = Some(dialog);
                                        ui.close();
                                    }
                                }
                            });
                            ui.label(
                                s.snapshot_time
                                    .unix_seconds()
                                    .map(|t| xen_core::format::datetime_utc(t, true))
                                    .unwrap_or_default(),
                            );
                            ui.label(if s.name_description.is_empty() {
                                p.none
                            } else {
                                &s.name_description
                            });
                            ui.end_row();
                        }
                    });
            });
        }
        if let Some(request) = open {
            self.open(conn, request);
        }
    }

    // ---- the VM Storage tab ----------------------------------------------------

    /// The VM Storage tab: its DVD drives, then its disks with the
    /// original's buttons (`VMStoragePage`).
    #[allow(clippy::too_many_arguments)]
    pub fn vm_storage_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
        ctx: crate::tabs::Ctx<'_>,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let p = prov_strings();
        let m = &conn.model;
        let usable = conn.status.is_connected();
        let busy = conn.busy_vms.contains(vm_ref);
        let mut open = None;
        if objects::vm_kind(vm) == VmKind::Guest {
            if let Some(request) = dvd_drives(ui, conn, vm_ref, vm, &mut open) {
                ops.dispatch(conn, request, backend);
            }
            ui.add_space(8.0);
        }
        let disks: Vec<_> = prov::vm_disks(m, vm)
            .into_iter()
            .filter(|(_, b, _, v)| crate::tabs::storage::vm_disk_shown(ctx, b, v))
            .collect();
        if self
            .disk_selection
            .as_ref()
            .is_some_and(|(c, r)| *c != conn.id || !disks.iter().any(|(b, ..)| *b == r))
        {
            self.disk_selection = None;
        }
        let selected = self
            .disk_selection
            .as_ref()
            .and_then(|(_, r)| disks.iter().find(|(b, ..)| *b == r))
            .cloned();
        let editable = usable && !vm.is_a_snapshot && !busy;
        ui.horizontal(|ui| {
            if ui.add_enabled(editable, Button::new(p.add_disk)).clicked() {
                open = Some(DialogRequest::NewDisk(vm_ref.clone()));
            }
            if ui
                .add_enabled(editable && !vm.is_a_template, Button::new(p.attach_disk))
                .clicked()
            {
                open = Some(DialogRequest::AttachDisk(vm_ref.clone()));
            }
            let sel = selected.as_ref();
            if ui
                .add_enabled(usable && sel.is_some(), Button::new(p.disk_properties))
                .clicked()
                && let Some((_, _, vdi, _)) = sel
            {
                open = Some(DialogRequest::ResizeDisk((*vdi).clone()));
            }
            let can_detach = usable && sel.is_some_and(|(_, b, _, _)| prov::can_detach(vm, b));
            if ui
                .add_enabled(can_detach, Button::new(p.detach_disk))
                .clicked()
                && let Some((vbd, ..)) = sel
            {
                open = Some(DialogRequest::DetachDisk(vm_ref.clone(), (*vbd).clone()));
            }
            let can_move = usable
                && sel.is_some_and(|(_, _, _, v)| {
                    !v.is_a_snapshot && !prov::move_targets(m, v).is_empty()
                });
            if ui.add_enabled(can_move, Button::new(p.move_disk)).clicked()
                && let Some((_, _, vdi, _)) = sel
            {
                open = Some(DialogRequest::MoveDisk((*vdi).clone()));
            }
            let can_delete = usable && sel.is_some_and(|(_, _, _, v)| prov::can_delete_disk(m, v));
            if ui
                .add_enabled(can_delete, Button::new(p.delete_disk))
                .clicked()
                && let Some((_, _, vdi, _)) = sel
            {
                open = Some(DialogRequest::DeleteDisk((*vdi).clone()));
            }
        });
        ui.add_space(4.0);
        let columns = crate::tabs::storage::vm_disk_columns();
        Grid::new("vm-disks-grid")
            .striped(true)
            .num_columns(columns.len())
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for header in &columns {
                    ui.strong(*header);
                }
                ui.end_row();
                for (vbd_ref, vbd, _, vdi) in &disks {
                    let cells = crate::tabs::storage::vm_disk_row(ctx, vbd, vdi);
                    let is_selected = selected.as_ref().is_some_and(|(b, ..)| b == vbd_ref);
                    // Any cell selects the row, as a list view does.
                    for cell in &cells {
                        if ui.selectable_label(is_selected, cell).clicked() {
                            self.disk_selection = Some((conn.id, (*vbd_ref).clone()));
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some(request) = open {
            self.open(conn, request);
        }
    }
}

/// The DVD drives of a VM (`MultipleDvdIsoList`): one ISO list per drive
/// with Eject, or the link that creates a drive. Changing the ISO needs no
/// confirmation; it returns the request to run.
fn dvd_drives(
    ui: &mut Ui,
    conn: &ConnectionView,
    vm_ref: &Ref<Vm>,
    vm: &VmRecord,
    open: &mut Option<DialogRequest>,
) -> Option<ActionRequest> {
    let p = prov_strings();
    let m = &conn.model;
    let drives = prov::cd_drives(m, vm);
    let usable = conn.status.is_connected() && !conn.busy_vms.contains(vm_ref);
    if drives.is_empty() {
        if ui
            .add_enabled(
                usable && !vm.is_a_snapshot,
                egui::Link::new(p.create_dvd_drive),
            )
            .clicked()
        {
            *open = Some(DialogRequest::NewCdDrive(vm_ref.clone()));
        }
        return None;
    }
    let groups = prov::iso_groups(m, Some(vm));
    let mut request = None;
    for (i, (vbd_ref, vbd)) in drives.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(fill(p.dvd_drive, &[("n", &(i + 1).to_string())]));
            let current = (!vbd.empty).then(|| vbd.vdi.clone());
            let current_name = current
                .as_ref()
                .and_then(|v| m.get(v))
                .map(|v| v.name_label.clone())
                .unwrap_or_else(|| p.empty_iso.to_owned());
            let enabled = usable && prov::can_change_cd(vm, vbd);
            let mut chosen: Option<Option<Ref<Vdi>>> = None;
            ui.add_enabled_ui(enabled, |ui| {
                ComboBox::from_id_salt(("dvd", vbd_ref.as_str()))
                    .selected_text(&current_name)
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(current.is_none(), p.empty_iso)
                            .clicked()
                        {
                            chosen = Some(None);
                        }
                        for group in &groups {
                            ui.label(RichText::new(&group.sr_name).strong());
                            for (iso, name) in &group.isos {
                                if ui
                                    .selectable_label(
                                        current.as_ref() == Some(iso),
                                        format!("    {name}"),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(Some(iso.clone()));
                                }
                            }
                        }
                    });
                if ui
                    .add_enabled(current.is_some(), Button::new(p.eject))
                    .clicked()
                {
                    chosen = Some(None);
                }
            });
            if let Some(iso) = chosen
                && iso != current
            {
                request = Some(ActionRequest::ChangeCd {
                    vm: vm_ref.clone(),
                    vbd: (*vbd_ref).clone(),
                    iso,
                });
            }
        });
    }
    request
}

fn ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn take_snapshot_form(
    vm: &VmRecord,
    vm_ref: Ref<Vm>,
    then_revert: Option<Ref<Vm>>,
) -> TakeSnapshot {
    let disks_ok = vm
        .allowed_operations
        .contains(&xapi::VmOperations::Snapshot);
    let memory_ok = prov::can_checkpoint(vm);
    TakeSnapshot {
        vm: vm_ref,
        name: String::new(),
        description: String::new(),
        memory: !disks_ok && memory_ok,
        disks_ok,
        memory_ok,
        then_revert,
    }
}

fn sr_label(sr: &xapi::SrRecord) -> String {
    format!(
        "{} ({})",
        sr.name_label,
        fill(
            prov_strings().sr_free,
            &[
                ("free", &disk_size(prov::sr_free(sr))),
                ("total", &disk_size(sr.physical_size))
            ]
        )
    )
}

fn sr_combo(
    ui: &mut Ui,
    id: &str,
    srs: &[(&Ref<Sr>, &std::sync::Arc<xapi::SrRecord>)],
    sr: &mut Option<Ref<Sr>>,
) {
    let text = sr
        .as_ref()
        .and_then(|r| srs.iter().find(|(x, _)| *x == r))
        .map(|(_, s)| sr_label(s))
        .unwrap_or_else(|| prov_strings().none.to_owned());
    ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(320.0)
        .show_ui(ui, |ui| {
            for (r, s) in srs {
                if ui
                    .selectable_label(sr.as_ref() == Some(*r), sr_label(s))
                    .clicked()
                {
                    *sr = Some((*r).clone());
                }
            }
        });
}

fn window(title: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_owned())
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
}

/// Draws a dialog. Returns whether it closes, the request to run, and a
/// dialog to open next.
fn show_dialog(
    ctx: &Context,
    conn: &ConnectionView,
    kind: &mut Kind,
) -> (bool, Option<ActionRequest>, Option<Kind>) {
    let p = prov_strings();
    let m = &conn.model;
    let mut close = false;
    let mut request = None;
    let mut next = None;
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    match kind {
        Kind::Confirm(c) if c.title.is_empty() => {
            // A request with no question (`dispatch_later`).
            return (true, Some(c.request.clone()), None);
        }
        Kind::Confirm(c) => {
            window(&c.title).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(&c.text);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(&c.yes).clicked() {
                        request = Some(c.request.clone());
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::TakeSnapshot(d) => {
            window(p.take_snapshot_title).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(p.take_snapshot_blurb);
                ui.add_space(6.0);
                Grid::new("snap-form").num_columns(2).show(ui, |ui| {
                    let label = ui.label(p.name_label);
                    ui.add(TextEdit::singleline(&mut d.name).id_salt("snapshot-name"))
                        .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label(p.description_label);
                    ui.add(TextEdit::multiline(&mut d.description).desired_rows(2))
                        .labelled_by(label.id);
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.add_enabled_ui(d.disks_ok, |ui| {
                    ui.radio_value(&mut d.memory, false, p.snapshot_disks)
                });
                if !d.disks_ok {
                    ui.label(RichText::new(p.snapshot_disks_unavailable).weak());
                }
                ui.add_enabled_ui(d.memory_ok, |ui| {
                    ui.radio_value(&mut d.memory, true, p.snapshot_memory)
                });
                if !d.memory_ok {
                    ui.label(RichText::new(p.snapshot_memory_needs_running).weak());
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ok = !d.name.trim().is_empty() && (d.disks_ok || d.memory_ok);
                    if ui
                        .add_enabled(ok, Button::new(p.take_snapshot_ok))
                        .clicked()
                    {
                        request = Some(match &d.then_revert {
                            Some(snapshot) => ActionRequest::Revert {
                                snapshot: snapshot.clone(),
                                snapshot_first: Some((
                                    d.name.trim().to_owned(),
                                    d.description.trim().to_owned(),
                                )),
                            },
                            None => ActionRequest::Snapshot {
                                vm: d.vm.clone(),
                                name: d.name.trim().to_owned(),
                                description: d.description.trim().to_owned(),
                                memory: d.memory,
                            },
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::Revert(d) => {
            window(p.revert_title).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(fill(p.revert_blurb, &[("name", &d.name)]));
                ui.add_space(6.0);
                if d.can_snapshot {
                    ui.checkbox(&mut d.take_first, p.revert_new_snapshot);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(p.yes).clicked() {
                        close = true;
                        if d.take_first && d.can_snapshot {
                            if let Some(vm) = m.vms.get(&d.vm) {
                                next = Some(Kind::TakeSnapshot(take_snapshot_form(
                                    vm,
                                    d.vm.clone(),
                                    Some(d.snapshot.clone()),
                                )));
                            }
                        } else {
                            request = Some(ActionRequest::Revert {
                                snapshot: d.snapshot.clone(),
                                snapshot_first: None,
                            });
                        }
                    }
                    if ui.button(p.no).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::TemplateFromSnapshot(d) => {
            window(p.save_as_template).show(ctx, |ui| {
                let label = ui.label(p.template_name_prompt);
                ui.add(TextEdit::singleline(&mut d.name).desired_width(380.0))
                    .labelled_by(label.id);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!d.name.trim().is_empty(), Button::new(p.ok))
                        .clicked()
                    {
                        request = Some(ActionRequest::TemplateFromSnapshot {
                            snapshot: d.snapshot.clone(),
                            name: d.name.trim().to_owned(),
                            description: d.description.clone(),
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::CopyVm(d) => {
            let title = if d.template {
                p.copy_template_title
            } else {
                p.copy_vm_title
            };
            let srs = m
                .vms
                .get(&d.vm)
                .map(|v| prov::disk_srs(m, Some(v)))
                .unwrap_or_default();
            window(title).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label(if d.template {
                    p.copy_template_rubric
                } else {
                    p.copy_vm_rubric
                });
                ui.add_space(6.0);
                Grid::new("copy-form").num_columns(2).show(ui, |ui| {
                    let label = ui.label(p.name_label);
                    ui.add(TextEdit::singleline(&mut d.name).desired_width(320.0))
                        .labelled_by(label.id);
                    ui.end_row();
                    ui.label(p.description_label);
                    ui.add(
                        TextEdit::multiline(&mut d.description)
                            .desired_rows(2)
                            .desired_width(320.0),
                    );
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.add_enabled_ui(d.can_fast, |ui| {
                    ui.radio_value(&mut d.full, false, p.fast_clone)
                });
                ui.label(
                    RichText::new(if d.template {
                        p.fast_clone_template
                    } else {
                        p.fast_clone_vm
                    })
                    .weak(),
                );
                ui.add_enabled_ui(d.can_full, |ui| {
                    ui.radio_value(&mut d.full, true, p.full_copy)
                });
                ui.label(
                    RichText::new(if d.template {
                        p.full_copy_template
                    } else {
                        p.full_copy_vm
                    })
                    .weak(),
                );
                ui.add_enabled_ui(d.full, |ui| sr_combo(ui, "copy-sr", &srs, &mut d.sr));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ok = !d.name.trim().is_empty() && (!d.full || d.sr.is_some());
                    if ui.add_enabled(ok, Button::new(p.copy_button)).clicked() {
                        let mode = match (&d.full, &d.sr) {
                            (true, Some(sr)) => CopyMode::Copy(sr.clone()),
                            _ => CopyMode::Clone,
                        };
                        request = Some(ActionRequest::CopyVm {
                            vm: d.vm.clone(),
                            name: d.name.trim().to_owned(),
                            description: d.description.trim().to_owned(),
                            mode,
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::DeleteVm(d) => {
            window(&d.title.clone()).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label(p.delete_blurb);
                ui.add_space(4.0);
                if d.disks.is_empty() && d.snapshots.is_empty() {
                    ui.label(RichText::new(p.delete_nothing).weak());
                }
                if !d.disks.is_empty() {
                    ui.strong(p.attached_disks);
                    for disk in &mut d.disks {
                        ui.checkbox(&mut disk.checked, &disk.name);
                    }
                }
                if !d.snapshots.is_empty() {
                    ui.strong(p.snapshots_group);
                    for (_, name, checked) in &mut d.snapshots {
                        ui.checkbox(checked, name.as_str());
                    }
                }
                ui.horizontal(|ui| {
                    if ui.small_button(p.select_all).clicked() {
                        d.disks.iter_mut().for_each(|x| x.checked = true);
                        d.snapshots.iter_mut().for_each(|x| x.2 = true);
                    }
                    if ui.small_button(p.clear_all).clicked() {
                        d.disks.iter_mut().for_each(|x| x.checked = false);
                        d.snapshots.iter_mut().for_each(|x| x.2 = false);
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new(p.delete_footer).weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(p.delete_button).clicked() {
                        request = Some(ActionRequest::DeleteVm {
                            vm: d.vm.clone(),
                            disks: d
                                .disks
                                .iter()
                                .filter(|x| x.checked)
                                .map(|x| x.vdi.clone())
                                .collect(),
                            snapshots: d
                                .snapshots
                                .iter()
                                .filter(|x| x.2)
                                .map(|x| x.0.clone())
                                .collect(),
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::NewDisk(d) => {
            let srs = m
                .vms
                .get(&d.vm)
                .map(|v| prov::disk_srs(m, Some(v)))
                .unwrap_or_default();
            window(p.add_disk_title).show(ctx, |ui| {
                ui.set_max_width(480.0);
                Grid::new("new-disk").num_columns(2).show(ui, |ui| {
                    let label = ui.label(p.name_label);
                    ui.add(TextEdit::singleline(&mut d.name).desired_width(320.0))
                        .labelled_by(label.id);
                    ui.end_row();
                    ui.label(p.description_label);
                    ui.add(
                        TextEdit::multiline(&mut d.description)
                            .desired_rows(2)
                            .desired_width(320.0),
                    );
                    ui.end_row();
                    ui.label(p.size_label);
                    ui.horizontal(|ui| {
                        ui.add(
                            DragValue::new(&mut d.size_gib)
                                .range(0.0..=1_048_576.0)
                                .speed(0.25)
                                .max_decimals(3),
                        );
                        ui.label(p.gib);
                    });
                    ui.end_row();
                    ui.label(p.location_label);
                    sr_combo(ui, "new-disk-sr", &srs, &mut d.sr);
                    ui.end_row();
                });
                let size = (d.size_gib * GIB) as i64;
                let error = if srs.is_empty() {
                    Some(p.no_sr_for_disk.to_owned())
                } else if size < 1 << 20 {
                    Some(fill(p.disk_too_small, &[("size", &disk_size(1 << 20))]))
                } else {
                    None
                };
                if let Some(e) = &error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ok = error.is_none() && d.sr.is_some() && !d.name.trim().is_empty();
                    if ui.add_enabled(ok, Button::new(p.create)).clicked()
                        && let Some(sr) = &d.sr
                    {
                        request = Some(ActionRequest::NewDisk {
                            vm: d.vm.clone(),
                            disk: DiskSpec {
                                name: d.name.trim().to_owned(),
                                description: d.description.trim().to_owned(),
                                sr: sr.clone(),
                                size,
                            },
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::AttachDisk(d) => {
            let candidates = m
                .vms
                .get(&d.vm)
                .map(|v| prov::attachable_vdis(m, &d.vm, v))
                .unwrap_or_default();
            let hvm = m.vms.get(&d.vm).is_some_and(|v| prov::is_hvm(v));
            window(p.attach_title).show(ctx, |ui| {
                ui.set_max_width(480.0);
                if candidates.is_empty() {
                    ui.label(p.no_disks_to_attach);
                }
                ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    let mut last_sr = None;
                    for (r, v) in &candidates {
                        if last_sr != Some(&v.sr) {
                            last_sr = Some(&v.sr);
                            ui.strong(
                                m.get(&v.sr)
                                    .map(|s| s.name_label.as_str())
                                    .unwrap_or_default(),
                            );
                        }
                        let text = format!("    {} ({})", v.name_label, disk_size(v.virtual_size));
                        if ui
                            .selectable_label(d.vdi.as_ref() == Some(*r), text)
                            .clicked()
                        {
                            d.vdi = Some((*r).clone());
                            d.read_only = v.read_only;
                        }
                    }
                });
                let forced = d
                    .vdi
                    .as_ref()
                    .and_then(|r| m.get(r))
                    .is_some_and(|v| v.read_only);
                ui.add_enabled_ui(!hvm && !forced, |ui| {
                    ui.checkbox(&mut d.read_only, p.attach_read_only)
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(d.vdi.is_some(), Button::new(p.attach_button))
                        .clicked()
                        && let Some(vdi) = &d.vdi
                    {
                        request = Some(ActionRequest::AttachDisk {
                            vm: d.vm.clone(),
                            vdi: vdi.clone(),
                            read_only: d.read_only && !hvm,
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::Resize(d) => {
            window(p.resize_title).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(fill(p.resize_blurb, &[("disk", &d.name)]));
                ui.add_space(6.0);
                let min = d.current as f64 / GIB;
                ui.add_enabled_ui(d.allowed, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(p.size_label);
                        ui.add(
                            DragValue::new(&mut d.size_gib)
                                .range(min..=1_048_576.0)
                                .speed(0.25)
                                .max_decimals(3),
                        );
                        ui.label(p.gib);
                    });
                });
                if !d.allowed {
                    ui.label(RichText::new(p.resize_not_allowed).weak());
                }
                let size = (d.size_gib * GIB) as i64;
                // Less than 10 MiB more is no change (`VDISizeLocationPage`).
                let changed = size - d.current > 10 << 20;
                let sr_room = m
                    .get(&d.vdi)
                    .and_then(|v| m.get(&v.sr))
                    .map(|sr| prov::sr_free(sr))
                    .unwrap_or(i64::MAX);
                let room = size - d.current <= sr_room;
                if changed && !room {
                    ui.colored_label(ui.visuals().error_fg_color, p.not_enough_space);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(d.allowed && changed && room, Button::new(p.ok))
                        .clicked()
                    {
                        request = Some(ActionRequest::ResizeDisk {
                            vdi: d.vdi.clone(),
                            size,
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
        Kind::MoveDisk(d) => {
            let targets = m
                .get(&d.vdi)
                .map(|v| prov::move_targets(m, v))
                .unwrap_or_default();
            window(p.move_title).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(RichText::new(&d.name).strong());
                ui.label(p.move_blurb);
                if targets.is_empty() {
                    ui.label(RichText::new(p.no_move_target).weak());
                }
                sr_combo(ui, "move-sr", &targets, &mut d.sr);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(d.sr.is_some(), Button::new(p.move_button))
                        .clicked()
                        && let Some(sr) = &d.sr
                    {
                        request = Some(ActionRequest::MoveDisk {
                            vdi: d.vdi.clone(),
                            sr: sr.clone(),
                        });
                        close = true;
                    }
                    if ui.button(p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        }
    }
    if escape {
        close = true;
    }
    (close, request, next)
}
