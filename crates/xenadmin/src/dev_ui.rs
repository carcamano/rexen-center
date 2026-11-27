//! F14a in the window: the GPU tab (`GpuPage`: placement policy, one row
//! per group of identical GPUs with a bar per GPU and the allowed types,
//! Edit... with `GpuConfiguration`), the USB tab (`UsbPage`), the pool and
//! server Properties' GPU page (`PoolGpuEditPage`), the VM Properties'
//! GPU and USB pages (`GpuEditPage` with Add virtual GPU, `USBEditPage`
//! with Attach USB), the vTPM manager (`VtpmManagementDialog`), the
//! memory settings dialog and wizard (`BallooningDialog`,
//! `BallooningWizard`), and the external SSH and Remote Desktop consoles
//! with their options.
//!
//! The rules live in [`xen_core::devices`], [`xen_core::memory`] and
//! [`xen_core::consoles`]; this module draws them and sends the requests
//! through [`OpsUi`] (which asks the original's questions first).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::{
    Button, Color32, Context, CornerRadius, DragValue, Grid, Rect, RichText, ScrollArea, Sense, Ui,
    pos2, vec2,
};
use xapi::{Host, Pgpu, Pusb, Ref, VgpuType, Vm, VmRecord, Vtpm, Vusb};
use xen_core::consoles::{self, CommandLine, Platform, SshError};
use xen_core::devices::{
    self, DeviceRequest, IntegratedGpu, PageVgpu, PlacementPolicy, VgpuChoice,
};
use xen_core::memory::{MemoryChange, MemoryConfirm, MemoryEdit, MemoryField, MemoryMode};
use xen_core::operations::PrecheckResult;
use xen_core::properties::PropertiesTarget;
use xen_core::settings::{ExternalTools, SshConsole};
use xen_core::{ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView};

use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::dev_strings::{self, dev_strings};
use crate::i18n::fill;
use crate::ops_ui::{Confirmation, OpsUi};
use crate::pool_ui::window;

/// Ids of this module's prechecks.
fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    (1 << 53) + N.fetch_add(1, Ordering::Relaxed)
}

fn yes_no(b: bool) -> &'static str {
    if b {
        dev_strings().yes
    } else {
        dev_strings().no
    }
}

/// The question the original asks before an F14a request, if any.
pub fn confirmation(conn: &ConnectionView, request: &DeviceRequest) -> Option<Confirmation> {
    let d = dev_strings();
    let _ = conn;
    match request {
        DeviceRequest::SetUsbPassthrough { enabled, .. } => Some(Confirmation {
            title: branding::PRODUCT_NAME,
            text: if *enabled {
                d.usb_confirm_enable
            } else {
                d.usb_confirm_disable
            }
            .to_owned(),
            yes: if *enabled {
                d.usb_yes_enable
            } else {
                d.usb_yes_disable
            },
            no_default: false,
        }),
        DeviceRequest::DetachUsb { .. } => Some(Confirmation {
            title: d.usb_detach_title,
            text: d.usb_detach_confirm.to_owned(),
            yes: d.yes,
            no_default: true,
        }),
        DeviceRequest::RemoveVtpm { .. } => Some(Confirmation {
            title: branding::PRODUCT_NAME,
            text: d.vtpm_remove_warning.to_owned(),
            yes: d.yes,
            no_default: false,
        }),
        DeviceRequest::EnableRdp { .. } => Some(Confirmation {
            title: branding::PRODUCT_NAME,
            text: d.rdp_force_enable.to_owned(),
            yes: d.yes,
            no_default: false,
        }),
        _ => None,
    }
}

// ---- the GPU tab ----------------------------------------------------------------------------

pub(crate) const SEGMENT_COLORS: [Color32; 2] = [
    Color32::from_rgb(83, 39, 139),
    Color32::from_rgb(157, 115, 215),
];

/// Draws one GPU's bar (`GpuShinyBar`).
fn gpu_bar(ui: &mut Ui, bar: &devices::GpuBar) {
    let width = (ui.available_width() - 20.0).clamp(200.0, 520.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let bar_rect = Rect::from_min_size(pos2(rect.min.x, rect.min.y + 12.0), vec2(width, 40.0));
    painter.rect_filled(bar_rect, CornerRadius::same(4), Color32::BLACK);
    let slot = width / bar.capacity.max(1) as f32;
    if bar.max_capacity > 1 {
        let mut x = bar_rect.min.x;
        while x <= bar_rect.max.x + 0.5 {
            painter.line_segment(
                [pos2(x, rect.min.y), pos2(x, bar_rect.center().y)],
                egui::Stroke::new(1.0, Color32::DARK_GRAY),
            );
            x += slot;
        }
    }
    let mut hovered = None;
    for (i, (vm, model)) in bar.segments.iter().enumerate() {
        let seg = Rect::from_min_size(
            pos2(bar_rect.min.x + slot * i as f32, bar_rect.min.y),
            vec2(slot, bar_rect.height()),
        );
        let fill = crate::theme::segment_fill(SEGMENT_COLORS[i % 2], Color32::BLACK);
        let ink = crate::theme::text_on(fill);
        painter.rect_filled(seg, CornerRadius::same(4), fill);
        let text = format!("{vm}\n{model}");
        let galley = painter.layout(
            text.clone(),
            egui::FontId::proportional(10.0),
            ink,
            slot - 4.0,
        );
        painter.galley(seg.center() - galley.size() / 2.0, galley, ink);
        if response.hover_pos().is_some_and(|p| seg.contains(p)) {
            hovered = Some(text);
        }
    }
    if let Some(t) = hovered {
        response.on_hover_text_at_pointer(t);
    }
}

/// A tick (allowed) or a red cross (not allowed), drawn.
fn mark(ui: &mut Ui, allowed: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let p = ui.painter_at(rect);
    let r = rect.shrink(2.0);
    if allowed {
        let stroke = egui::Stroke::new(2.0, Color32::from_rgb(34, 139, 34));
        p.line_segment(
            [pos2(r.min.x, r.center().y), pos2(r.min.x + 3.0, r.max.y)],
            stroke,
        );
        p.line_segment([pos2(r.min.x + 3.0, r.max.y), r.right_top()], stroke);
    } else {
        let stroke = egui::Stroke::new(2.0, Color32::from_rgb(200, 40, 40));
        p.line_segment([r.left_top(), r.right_bottom()], stroke);
        p.line_segment([r.right_top(), r.left_bottom()], stroke);
    }
}

/// The allowed-types editor (`GpuConfiguration`).
pub struct GpuConfigDialog {
    pub connection: ConnectionId,
    pub pgpus: Vec<Ref<Pgpu>>,
    /// Type, allowed when opened, allowed now, in use (cannot be cleared).
    pub rows: Vec<(Ref<VgpuType>, bool, bool, bool)>,
    title: String,
}

impl GpuConfigDialog {
    pub fn open(conn: &ConnectionView, pgpus: Vec<Ref<Pgpu>>) -> Option<Self> {
        let m = &conn.model;
        let first = m.get(pgpus.first()?)?;
        let d = dev_strings();
        let name = devices::pgpu_name(m, first);
        let title = if pgpus.len() == 1 {
            fill(d.gpu_title_one, &[("name", &name)])
        } else {
            fill(
                d.gpu_title_many,
                &[("name", &name), ("count", &pgpus.len().to_string())],
            )
        };
        let rows = devices::sorted_types_desc(m, &first.supported_vgpu_types)
            .into_iter()
            .map(|t| {
                let allowed = first.enabled_vgpu_types.contains(&t);
                let used = devices::type_in_use(m, &pgpus, &t);
                (t, allowed, allowed, used && allowed)
            })
            .collect();
        Some(Self {
            connection: conn.id,
            pgpus,
            rows,
            title,
        })
    }

    /// The request OK sends, if anything changed.
    pub fn request(&self, conn: &ConnectionView) -> Option<ActionRequest> {
        let changes: Vec<(Ref<VgpuType>, bool)> = self
            .rows
            .iter()
            .filter(|(_, was, now, _)| was != now)
            .map(|(t, _, now, _)| (t.clone(), *now))
            .collect();
        let plan = devices::allowed_types_plan(&conn.model, &self.pgpus, &changes);
        (!plan.is_empty()).then(|| {
            ActionRequest::Device(Box::new(DeviceRequest::SetAllowedVgpuTypes { pgpus: plan }))
        })
    }

    /// Draws it; whether it closes.
    fn window(
        &mut self,
        ctx: &Context,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let d = dev_strings();
        let m = &conn.model;
        let (mut ok, mut cancel) = (false, false);
        window(self.title.clone()).show(ctx, |ui| {
            ui.label(if self.pgpus.len() == 1 {
                d.gpu_rubric_one
            } else {
                d.gpu_rubric_many
            });
            ui.add_space(6.0);
            Grid::new("gpu-config")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("");
                    ui.strong(d.col_name);
                    ui.strong(d.col_per_gpu);
                    ui.strong(d.col_video_ram);
                    ui.end_row();
                    for (t, _, now, locked) in &mut self.rows {
                        let rec = m.get(&*t);
                        let pass = rec.is_some_and(|r| devices::is_passthrough(r));
                        let r = ui.add_enabled(!*locked, egui::Checkbox::without_text(now));
                        if *locked {
                            r.on_disabled_hover_text(d.gpu_type_in_use);
                        }
                        ui.label(if pass {
                            d.passthrough_whole.to_owned()
                        } else {
                            rec.map(|r| r.model_name.clone()).unwrap_or_default()
                        });
                        ui.label(if pass {
                            String::new()
                        } else {
                            rec.map(|r| devices::vgpu_type_capacity(m, t, r).to_string())
                                .unwrap_or_default()
                        });
                        ui.label(match rec.map(|r| r.framebuffer_size) {
                            Some(fb) if fb != 0 => xen_core::format::memory_size(fb, true),
                            _ => String::new(),
                        });
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
            dialog_buttons(ui, |row| {
                ok = row.button(ButtonRole::Primary, d.ok).clicked();
                cancel = row.button(ButtonRole::Cancel, d.cancel).clicked();
            });
        });
        if ok {
            if let Some(r) = self.request(conn) {
                ops.request(conn, r, backend);
            }
            return true;
        }
        cancel
    }
}

// ---- the pool / server Properties GPU page ------------------------------------------------

/// `PoolGpuEditPage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolGpuPage {
    pub show_placement: bool,
    pub show_integrated: Option<Ref<Host>>,
    pub placement: PlacementPolicy,
    placement_original: PlacementPolicy,
    integrated: IntegratedGpu,
    /// "This server will use the integrated GPU" after the next reboot.
    pub will_use: bool,
}

impl PoolGpuPage {
    pub fn new(conn: &ConnectionView, target: &PropertiesTarget) -> Self {
        let m = &conn.model;
        let show_placement =
            xen_core::properties::is_pool_or_standalone(m, target) && devices::vgpu_capability(m);
        let show_integrated = match target {
            PropertiesTarget::Host(h) if devices::can_change_integrated_gpu(m) => Some(h.clone()),
            _ => None,
        };
        let placement = devices::placement_policy(m);
        let integrated = show_integrated
            .as_ref()
            .map(|h| devices::integrated_gpu(m, h))
            .unwrap_or(IntegratedGpu {
                current: false,
                next: false,
            });
        Self {
            show_placement,
            show_integrated,
            placement,
            placement_original: placement,
            integrated,
            will_use: integrated.next,
        }
    }

    pub fn subtext(&self) -> String {
        let d = dev_strings();
        let mut parts = Vec::new();
        if self.show_placement {
            parts.push(match self.placement {
                PlacementPolicy::Density => d.sub_density,
                PlacementPolicy::Performance => d.sub_performance,
                PlacementPolicy::Mixed => d.sub_mixed,
            });
        }
        if self.show_integrated.is_some() {
            parts.push(if self.integrated.current {
                d.sub_integrated_using
            } else {
                d.sub_integrated_not_using
            });
        }
        parts.join("; ")
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let d = dev_strings();
        if self.show_placement {
            ui.group(|ui| {
                ui.strong(d.placement_group);
                ui.label(d.placement_rubric);
                ui.radio_value(
                    &mut self.placement,
                    PlacementPolicy::Density,
                    d.policy_density,
                );
                ui.radio_value(
                    &mut self.placement,
                    PlacementPolicy::Performance,
                    d.policy_performance,
                );
                if self.placement_original == PlacementPolicy::Mixed {
                    ui.radio_value(&mut self.placement, PlacementPolicy::Mixed, d.policy_mixed);
                }
            });
        }
        if self.show_integrated.is_some() {
            ui.group(|ui| {
                ui.strong(d.integrated_group);
                ui.label(d.integrated_rubric);
                ui.label(if self.integrated.current {
                    d.integrated_using
                } else {
                    d.integrated_not_using
                });
                ui.label(d.integrated_after_reboot);
                ui.radio_value(&mut self.will_use, true, d.integrated_will_use);
                ui.radio_value(&mut self.will_use, false, d.integrated_will_not_use);
            });
        }
    }

    /// What OK sends: the placement policy, then the integrated GPU.
    pub fn requests(&self) -> Vec<ActionRequest> {
        let mut out = Vec::new();
        if self.show_placement
            && self.placement != self.placement_original
            && let Some(algorithm) = self.placement.algorithm()
        {
            out.push(ActionRequest::Device(Box::new(
                DeviceRequest::SetPlacementPolicy { algorithm },
            )));
        }
        if let Some(host) = &self.show_integrated
            && self.will_use != self.integrated.next
        {
            out.push(ActionRequest::Device(Box::new(
                DeviceRequest::SetIntegratedGpu {
                    host: host.clone(),
                    enable: self.will_use,
                },
            )));
        }
        out
    }
}

// ---- the VM Properties' GPU page ----------------------------------------------------------

/// The Add virtual GPU dialog.
pub struct AddVgpuDialog {
    pub choices: Vec<VgpuChoice>,
    pub selected: Option<usize>,
}

/// `GpuEditPage`.
pub struct VmGpuPage {
    pub list: Vec<PageVgpu>,
    original: Vec<PageVgpu>,
    pub selected: Option<usize>,
    pub add: Option<AddVgpuDialog>,
}

impl VmGpuPage {
    /// The page, when the VM's dialog has it.
    pub fn new(conn: &ConnectionView, vm: &VmRecord) -> Option<Self> {
        let m = &conn.model;
        devices::vm_gpu_page_shown(m, vm).then(|| {
            let list = PageVgpu::of_vm(m, vm);
            Self {
                original: list.clone(),
                list,
                selected: None,
                add: None,
            }
        })
    }

    pub fn subtext(&self, conn: &ConnectionView) -> String {
        let d = dev_strings();
        match devices::gpu_page_summary(&conn.model, &self.list) {
            None => d.gpu_sub_unavailable.to_owned(),
            Some(names) if names.is_empty() => d.gpu_sub_none.to_owned(),
            Some(names) => names
                .iter()
                .map(dev_strings::page_vgpu_text)
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, conn: &ConnectionView, vm: &VmRecord) {
        let d = dev_strings();
        let m = &conn.model;
        let state = devices::gpu_page_state(m, vm, &self.list, &vm.ha_restart_priority);
        ui.strong(d.vm_gpu_title);
        ui.label(d.vm_gpu_rubric);
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            Grid::new("vm-gpus")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(d.col_pci_slot);
                    ui.strong(d.col_vgpu_type);
                    ui.strong(d.col_per_gpu);
                    ui.strong(d.col_video_ram);
                    ui.end_row();
                    for (i, v) in self.list.iter().enumerate() {
                        let (name, per, fb) = devices::page_vgpu_row(m, v);
                        if ui
                            .selectable_label(
                                self.selected == Some(i),
                                v.pci_slot().unwrap_or_else(|| "-".into()),
                            )
                            .clicked()
                        {
                            self.selected = Some(i);
                        }
                        ui.label(dev_strings::page_vgpu_text(&name));
                        ui.label(per.map(|p| p.to_string()).unwrap_or_default());
                        ui.label(if fb != 0 {
                            xen_core::format::memory_size(fb, true)
                        } else {
                            String::new()
                        });
                        ui.end_row();
                    }
                });
            ui.vertical(|ui| {
                if ui
                    .add_enabled(state.can_add, Button::new(d.gpu_add))
                    .clicked()
                {
                    let choices = devices::add_vgpu_choices(m, vm, &self.list);
                    let selected = devices::default_vgpu_choice(&choices);
                    self.add = Some(AddVgpuDialog { choices, selected });
                }
                let can_delete = state.block.is_none() && self.selected.is_some();
                if ui
                    .add_enabled(can_delete, Button::new(d.gpu_delete))
                    .clicked()
                    && let Some(i) = self.selected.take()
                    && i < self.list.len()
                {
                    self.list.remove(i);
                }
            });
        });
        ui.add_space(4.0);
        let warn = |ui: &mut Ui, text: &str| {
            ui.colored_label(ui.visuals().warn_fg_color, text);
        };
        let info = |ui: &mut Ui, text: &str| {
            ui.label(text);
        };
        if state.experimental {
            warn(ui, d.gpu_experimental);
        }
        if state.rdp_warning {
            warn(ui, d.gpu_rdp_warning);
        }
        if state.need_gpu {
            warn(ui, d.gpu_need_driver);
            info(ui, d.gpu_need_gpu);
        }
        if let Some(b) = state.block {
            info(ui, dev_strings::gpu_block_text(b));
        }
        if let Some(multiple) = state.multiple {
            info(
                ui,
                if multiple {
                    d.gpu_multiple
                } else {
                    d.gpu_single
                },
            );
        }
    }

    /// The Add virtual GPU dialog, if open.
    pub fn add_window(&mut self, ctx: &Context) {
        let d = dev_strings();
        let Some(add) = self.add.as_mut() else { return };
        let (mut ok, mut cancel) = (false, false);
        window(d.add_vgpu_title.to_owned()).show(ctx, |ui| {
            ui.label(d.add_vgpu_label);
            ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for (i, c) in add.choices.iter().enumerate() {
                    let text = dev_strings::choice_text(&c.text);
                    let text = if c.vgpu_type.is_none() && !c.selectable {
                        RichText::new(text).strong()
                    } else {
                        RichText::new(text)
                    };
                    if ui
                        .add_enabled(
                            c.selectable,
                            egui::Button::selectable(add.selected == Some(i), text),
                        )
                        .clicked()
                    {
                        add.selected = Some(i);
                    }
                }
            });
            dialog_buttons(ui, |row| {
                ok = row
                    .add_enabled(
                        ButtonRole::Primary,
                        add.selected.is_some(),
                        Button::new(d.add_vgpu_add),
                    )
                    .clicked();
                cancel = row.button(ButtonRole::Cancel, d.cancel).clicked();
            });
        });
        if ok && let Some(c) = add.selected.and_then(|i| add.choices.get(i)) {
            self.list.push(PageVgpu {
                existing: None,
                group: c.group.clone(),
                vgpu_type: c.vgpu_type.clone(),
                device: None,
            });
        }
        if ok || cancel {
            self.add = None;
        }
    }

    /// What OK sends when the list changed (`GpuAssignAction`).
    pub fn request(&self, vm: &Ref<Vm>) -> Option<ActionRequest> {
        (self.list != self.original).then(|| {
            ActionRequest::Device(Box::new(DeviceRequest::AssignVgpus {
                vm: vm.clone(),
                keep: self
                    .list
                    .iter()
                    .filter_map(|v| v.existing.clone())
                    .collect(),
                add: self
                    .list
                    .iter()
                    .filter(|v| v.existing.is_none())
                    .map(|v| (v.group.clone(), v.vgpu_type.clone()))
                    .collect(),
            }))
        })
    }
}

// ---- the USB tab and the VM Properties' USB page ---------------------------------------------

/// Device text: location and description.
fn pusb_line(p: &xapi::PusbRecord) -> String {
    format!(
        "{} {}",
        p.path,
        dev_strings::pusb_text(&devices::pusb_description(p))
    )
}

/// The Attach USB dialog.
pub struct AttachUsbDialog {
    pending: Option<u64>,
    /// `VM.get_possible_hosts`' answer (empty while waiting or on failure).
    pub hosts: Vec<Ref<Host>>,
    pub selected: Option<Ref<Pusb>>,
}

/// `USBEditPage`.
#[derive(Default)]
pub struct VmUsbPage {
    pub selected: Option<Ref<Vusb>>,
    pub attach: Option<AttachUsbDialog>,
}

impl VmUsbPage {
    pub fn new(conn: &ConnectionView, vm: &VmRecord) -> Option<Self> {
        devices::vm_usb_page_shown(&conn.model, vm).then(Self::default)
    }

    pub fn subtext(vm: &VmRecord) -> String {
        let d = dev_strings();
        match vm.vusbs.len() {
            0 => d.usb_sub_none.to_owned(),
            1 => d.usb_sub_one.to_owned(),
            n => fill(d.usb_sub_many, &[("count", &n.to_string())]),
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let d = dev_strings();
        let m = &conn.model;
        ui.strong(d.usb_page_title);
        Grid::new("vm-usbs")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.strong(d.col_location);
                ui.strong(d.col_description);
                ui.strong(d.col_attached);
                ui.end_row();
                for r in &vm.vusbs {
                    let Some(v) = m.get(r) else { continue };
                    let p = devices::vusb_device(m, r);
                    if ui
                        .selectable_label(
                            self.selected.as_ref() == Some(r),
                            p.map(|p| p.path.clone()).unwrap_or_default(),
                        )
                        .clicked()
                    {
                        self.selected = Some(r.clone());
                    }
                    ui.label(
                        p.map(|p| dev_strings::pusb_text(&devices::pusb_description(p)))
                            .unwrap_or_default(),
                    );
                    ui.label(yes_no(v.currently_attached));
                    ui.end_row();
                }
            });
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !vm.vusbs.contains(s))
        {
            self.selected = None;
        }
        let block = devices::device_block(m, vm, &vm.ha_restart_priority);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(block.is_none(), Button::new(d.usb_attach))
                .clicked()
            {
                let id = next_id();
                backend.send(Command::Precheck {
                    connection: conn.id,
                    id,
                    check: xen_core::Precheck::PossibleHosts { vm: vm_ref.clone() },
                });
                self.attach = Some(AttachUsbDialog {
                    pending: Some(id),
                    hosts: Vec::new(),
                    selected: None,
                });
            }
            if ui
                .add_enabled(self.selected.is_some(), Button::new(d.usb_detach))
                .clicked()
                && let Some(v) = self.selected.clone()
            {
                ops.request(
                    conn,
                    ActionRequest::Device(Box::new(DeviceRequest::detach_usb(m, vm_ref, &v))),
                    backend,
                );
            }
        });
        if let Some(b) = block {
            ui.label(dev_strings::usb_block_text(b));
        }
    }

    /// Takes the possible hosts' answer.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        match (&mut self.attach, result) {
            (Some(a), PrecheckResult::PossibleHosts(r)) if a.pending == Some(id) => {
                a.pending = None;
                a.hosts = r.clone().unwrap_or_default();
                true
            }
            _ => false,
        }
    }

    /// The Attach USB dialog, if open.
    pub fn attach_window(
        &mut self,
        ctx: &Context,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let d = dev_strings();
        let m = &conn.model;
        let Some(a) = self.attach.as_mut() else {
            return;
        };
        let (mut ok, mut cancel) = (false, false);
        window(d.attach_usb_title.to_owned()).show(ctx, |ui| {
            ui.label(d.attach_usb_note);
            let list = devices::attachable_pusbs(m, &a.hosts);
            ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                if list.is_empty() {
                    ui.label(RichText::new(d.attach_usb_none).weak());
                }
                for (h, pusbs) in &list {
                    ui.label(m.get(h).map(|h| h.name_label.clone()).unwrap_or_default());
                    for p in pusbs {
                        let Some(rec) = m.get(p) else { continue };
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            if ui
                                .selectable_label(a.selected.as_ref() == Some(p), pusb_line(rec))
                                .clicked()
                            {
                                a.selected = Some(p.clone());
                            }
                        });
                    }
                }
            });
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().warn_fg_color, d.attach_usb_warn1);
            ui.colored_label(ui.visuals().warn_fg_color, d.attach_usb_warn2);
            if !devices::uses_upstream_qemu(vm) {
                ui.colored_label(ui.visuals().warn_fg_color, d.attach_usb_warn3);
            }
            let can = a
                .selected
                .as_ref()
                .is_some_and(|p| list.iter().any(|(_, l)| l.contains(p)));
            dialog_buttons(ui, |row| {
                ok = row
                    .add_enabled(ButtonRole::Primary, can, Button::new(d.usb_attach))
                    .clicked();
                cancel = row.button(ButtonRole::Cancel, d.cancel).clicked();
            });
        });
        if ok && let Some(p) = a.selected.clone() {
            ops.request(
                conn,
                ActionRequest::Device(Box::new(DeviceRequest::attach_usb(m, vm_ref, &p))),
                backend,
            );
        }
        if ok || cancel {
            self.attach = None;
        }
    }
}

// ---- the vTPM manager -------------------------------------------------------------------------

/// `VtpmManagementDialog`.
pub struct VtpmDialog {
    pub connection: ConnectionId,
    pub vm: Ref<Vm>,
    pub selected: Option<Ref<Vtpm>>,
}

impl VtpmDialog {
    /// Draws it; whether it closes.
    fn window(
        &mut self,
        ctx: &Context,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let d = dev_strings();
        let m = &conn.model;
        let Some(vm) = m.get(&self.vm) else {
            return true;
        };
        let vtpms: Vec<Ref<Vtpm>> = vm
            .vtpms
            .iter()
            .filter(|r| m.vtpms.contains_key(*r))
            .cloned()
            .collect();
        if self.selected.as_ref().is_none_or(|s| !vtpms.contains(s)) {
            self.selected = vtpms.first().cloned();
        }
        let mut close = false;
        let name = crate::tabs::vm_display_name(vm);
        window(fill(d.vtpm_title, &[("vm", &ellipsis(&name, 50))])).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.label(fill(d.vtpm_blurb, &[("vm", &ellipsis(&name, 80))]));
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(220.0);
                    for t in &vtpms {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(self.selected.as_ref() == Some(t), d.vtpm_name)
                                .clicked()
                            {
                                self.selected = Some(t.clone());
                            }
                            let can = devices::can_remove_vtpm(m, t);
                            let r = ui.add_enabled(can.is_ok(), Button::new("X").small());
                            let r = match can {
                                Err(why) => {
                                    r.on_disabled_hover_text(dev_strings::vtpm_refusal_text(why))
                                }
                                Ok(()) => r.on_hover_text(d.vtpm_remove),
                            };
                            if r.clicked() {
                                ops.request(conn, remove_vtpm(&self.vm, t), backend);
                            }
                        });
                    }
                    let can_add = devices::can_add_vtpm(m, vm);
                    let r = ui.add_enabled(can_add.is_ok(), Button::new(d.vtpm_add));
                    let r = match can_add {
                        Err(why) => r.on_disabled_hover_text(dev_strings::vtpm_refusal_text(why)),
                        Ok(()) => r,
                    };
                    if r.clicked() {
                        ops.request(
                            conn,
                            ActionRequest::Device(Box::new(DeviceRequest::AddVtpm {
                                vm: self.vm.clone(),
                            })),
                            backend,
                        );
                    }
                });
                ui.add_space(12.0); // a separator would stretch to the screen height
                ui.vertical(|ui| {
                    match self
                        .selected
                        .as_ref()
                        .and_then(|t| m.get(t).map(|r| (t, r)))
                    {
                        None => {
                            ui.label(d.vtpm_none);
                        }
                        Some((t, rec)) => {
                            ui.group(|ui| {
                                ui.strong(d.vtpm_properties);
                                Grid::new("vtpm-props").num_columns(2).show(ui, |ui| {
                                    ui.label(d.vtpm_protected);
                                    ui.strong(yes_no(rec.is_protected));
                                    ui.end_row();
                                    ui.label("");
                                    ui.label(RichText::new(d.vtpm_protected_info).weak());
                                    ui.end_row();
                                    ui.label(d.vtpm_unique);
                                    ui.strong(yes_no(rec.is_unique));
                                    ui.end_row();
                                    ui.label("");
                                    ui.label(RichText::new(d.vtpm_unique_info).weak());
                                    ui.end_row();
                                });
                            });
                            let can = devices::can_remove_vtpm(m, t);
                            let r = ui.add_enabled(can.is_ok(), Button::new(d.vtpm_remove));
                            let r = match can {
                                Err(why) => {
                                    r.on_disabled_hover_text(dev_strings::vtpm_refusal_text(why))
                                }
                                Ok(()) => r,
                            };
                            if r.clicked() {
                                ops.request(conn, remove_vtpm(&self.vm, t), backend);
                            }
                        }
                    }
                });
            });
            ui.add_space(8.0);
            close = dialog_buttons(ui, |row| row.button(ButtonRole::Cancel, d.close).clicked());
        });
        close
    }
}

fn remove_vtpm(vm: &Ref<Vm>, t: &Ref<Vtpm>) -> ActionRequest {
    ActionRequest::Device(Box::new(DeviceRequest::RemoveVtpm {
        vm: vm.clone(),
        vtpm: t.clone(),
    }))
}

/// `name` cut to `max` characters with an ellipsis.
fn ellipsis(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        name.to_owned()
    } else {
        let mut s: String = name.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

// ---- memory settings --------------------------------------------------------------------------

/// The wizard's VMs (ticked or not) and page (0: choose, 1: settings).
pub type MemoryWizard = (Vec<(Ref<Vm>, bool)>, usize);

/// The Memory Settings dialog (one VM) or wizard (several).
pub struct MemoryDialog {
    pub connection: ConnectionId,
    /// The wizard's VMs (ticked or not) and page (0: choose, 1: settings).
    pub wizard: Option<MemoryWizard>,
    pub edit: Option<MemoryEdit>,
    /// The question asked before applying, with what applying sends.
    pub confirm: Option<(MemoryConfirm, Vec<MemoryChange>)>,
}

impl MemoryDialog {
    pub fn open(conn: &ConnectionView, vms: Vec<Ref<Vm>>) -> Option<Self> {
        if vms.len() == 1 {
            Some(Self {
                connection: conn.id,
                wizard: None,
                edit: Some(MemoryEdit::new(&conn.model, &vms)?),
                confirm: None,
            })
        } else {
            Some(Self {
                connection: conn.id,
                wizard: Some((vms.into_iter().map(|v| (v, true)).collect(), 0)),
                edit: None,
                confirm: None,
            })
        }
    }

    /// Apply: ask first when needed, else send. Whether it is done.
    pub fn apply(
        &mut self,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let Some(edit) = &self.edit else { return false };
        let (confirm, changes) = edit.apply(&conn.model);
        match confirm {
            Some(c) => {
                self.confirm = Some((c, changes));
                false
            }
            None => {
                send_memory(conn, changes, ops, backend);
                true
            }
        }
    }

    /// Draws it; whether it closes.
    fn window(
        &mut self,
        ctx: &Context,
        conn: &ConnectionView,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let d = dev_strings();
        let mut close = false;
        if let Some((c, changes)) = &self.confirm {
            let (mut yes, mut no) = (false, false);
            window(branding::PRODUCT_NAME.to_owned()).show(ctx, |ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    dev_strings::memory_confirm_text(*c),
                );
                dialog_buttons(ui, |row| {
                    yes = row.button(ButtonRole::Primary, d.yes).clicked();
                    no = row.button(ButtonRole::Cancel, d.no).clicked();
                });
            });
            if yes {
                send_memory(conn, changes.clone(), ops, backend);
                return true;
            }
            if no {
                self.confirm = None;
            }
            return false;
        }
        let model = &conn.model;
        window(d.mem_title.to_owned()).show(ctx, |ui| {
            ui.set_width(520.0);
            if let Some((vms, page)) = &mut self.wizard {
                if *page == 0 {
                    ui.strong(d.wiz_choose_vms_title);
                    ui.label(d.wiz_choose_vms_text);
                    for (r, ticked) in vms.iter_mut() {
                        let name = model
                            .get(&*r)
                            .map(|v| crate::tabs::vm_display_name(v))
                            .unwrap_or_default();
                        ui.checkbox(ticked, name);
                    }
                    let n = vms.iter().filter(|(_, t)| *t).count();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(n < vms.len(), Button::new(d.gpu_select_all))
                            .clicked()
                        {
                            vms.iter_mut().for_each(|v| v.1 = true);
                        }
                        if ui
                            .add_enabled(n > 0, Button::new(d.gpu_clear_all))
                            .clicked()
                        {
                            vms.iter_mut().for_each(|v| v.1 = false);
                        }
                    });
                    ui.add_space(8.0);
                    dialog_buttons(ui, |row| {
                        if row
                            .add_enabled(ButtonRole::Primary, n > 0, Button::new(d.next))
                            .clicked()
                        {
                            let chosen: Vec<Ref<Vm>> = vms
                                .iter()
                                .filter(|(_, t)| *t)
                                .map(|(r, _)| r.clone())
                                .collect();
                            let changed = self.edit.as_ref().is_none_or(|e| e.vms != chosen);
                            if changed {
                                self.edit = MemoryEdit::new(model, &chosen);
                            }
                            *page = 1;
                        }
                        close |= row.button(ButtonRole::Cancel, d.cancel).clicked();
                    });
                    return;
                }
                ui.strong(d.wiz_memory_title);
                ui.label(d.wiz_memory_text);
            } else if let Some(e) = &self.edit {
                ui.label(match (e.mode, e.template) {
                    (MemoryMode::Advanced, false) => d.mem_rubric_advanced,
                    (MemoryMode::Advanced, true) => d.mem_rubric_advanced_template,
                    (MemoryMode::Basic, false) => d.mem_rubric,
                    (MemoryMode::Basic, true) => d.mem_rubric_template,
                });
            }
            ui.add_space(6.0);
            if let Some(e) = self.edit.as_mut() {
                memory_controls(ui, e);
            }
            ui.add_space(8.0);
            let mut apply = false;
            dialog_buttons(ui, |row| {
                if let Some((_, page)) = &mut self.wizard {
                    if row.button(ButtonRole::Back, d.back).clicked() {
                        *page = 0;
                    }
                    apply = row.button(ButtonRole::Primary, d.finish).clicked();
                } else {
                    apply = row.button(ButtonRole::Primary, d.ok).clicked();
                }
                close |= row.button(ButtonRole::Cancel, d.cancel).clicked();
            });
            if apply && self.apply(conn, ops, backend) {
                close = true;
            }
        });
        close
    }
}

fn send_memory(
    conn: &ConnectionView,
    changes: Vec<MemoryChange>,
    ops: &mut OpsUi,
    backend: &BackendHandle,
) {
    for c in changes {
        ops.request(
            conn,
            ActionRequest::Device(Box::new(DeviceRequest::SetMemory(c))),
            backend,
        );
    }
}

/// The spinners (`VMMemoryControlsBasic` / `Advanced`).
fn memory_controls(ui: &mut Ui, e: &mut MemoryEdit) {
    let d = dev_strings();
    let unit = match e.unit {
        xen_core::memory::MemUnit::Mb => d.mem_mb,
        xen_core::memory::MemUnit::Gb => d.mem_gb,
    };
    let spinner = |ui: &mut Ui, e: &mut MemoryEdit, field: MemoryField, enabled: bool| {
        let mut v = e.shown(field);
        let (lo, hi) = e.shown_range(field);
        let r = ui.add_enabled(
            enabled,
            DragValue::new(&mut v)
                .range(lo..=hi)
                .speed(e.step(field))
                .fixed_decimals(e.unit.decimals()),
        );
        ui.label(unit);
        if r.changed() {
            e.set(field, v);
        }
    };
    match e.mode {
        MemoryMode::Advanced => {
            Grid::new("mem-adv").num_columns(3).show(ui, |ui| {
                for (label, field) in [
                    (d.mem_min, MemoryField::DynMin),
                    (d.mem_max, MemoryField::DynMax),
                    (d.mem_static_max, MemoryField::StaticMax),
                ] {
                    ui.label(label);
                    ui.horizontal(|ui| spinner(ui, e, field, true));
                    ui.end_row();
                }
            });
        }
        MemoryMode::Basic => {
            ui.horizontal(|ui| {
                if ui.radio(!e.dynamic, d.mem_fixed).clicked() {
                    e.set_dynamic(false);
                }
                spinner(ui, e, MemoryField::Fixed, true);
            });
            let allowed = e.dynamic_allowed();
            if ui
                .add_enabled(allowed, egui::RadioButton::new(e.dynamic, d.mem_dynamic))
                .clicked()
            {
                e.set_dynamic(true);
            }
            ui.add_enabled_ui(allowed, |ui| {
                Grid::new("mem-basic").num_columns(2).show(ui, |ui| {
                    for (label, field) in [
                        (d.mem_min, MemoryField::DynMin),
                        (d.mem_max, MemoryField::DynMax),
                    ] {
                        ui.label(label);
                        ui.horizontal(|ui| spinner(ui, e, field, allowed));
                        ui.end_row();
                    }
                });
            });
            if let Some(u) = e.unavailable {
                ui.colored_label(ui.visuals().warn_fg_color, dev_strings::dmc_text(u));
            }
        }
    }
}

// ---- external consoles ------------------------------------------------------------------------

/// What the Console tab's external console buttons asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleRequest {
    /// Launch SSH Console to this address.
    Ssh(String),
    /// Open Remote Desktop (probe the guest's addresses first).
    Rdp,
    /// Turn on Remote Desktop in the guest (asked first).
    EnableRdp,
}

/// Options > External Tools. The two groups fill the dialog's width and
/// each path's Browse... button sits at the right end of its row, so the
/// fields and the buttons line up.
pub fn external_tools_options(ui: &mut Ui, t: &mut ExternalTools) {
    use crate::file_dialog::{Pick, path_row};
    let d = dev_strings();
    ui.strong(d.tools_heading);
    ui.label(fill(d.tools_info, &[("console", branding::PRODUCT_NAME)]));
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.label(d.tools_ssh_group);
        ui.radio_value(&mut t.ssh_console, SshConsole::System, d.tools_system);
        // The two programs' paths start together, after the longer name.
        egui::Grid::new("options-ssh-console")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.radio_value(&mut t.ssh_console, SshConsole::Putty, d.tools_putty);
                if path_row(ui, &mut t.putty_location, Pick::Open(Vec::new()), |e| e).changed() {
                    t.ssh_console = SshConsole::Putty;
                }
                ui.end_row();
                ui.radio_value(&mut t.ssh_console, SshConsole::OpenSsh, d.tools_openssh);
                if path_row(ui, &mut t.openssh_location, Pick::Open(Vec::new()), |e| e).changed() {
                    t.ssh_console = SshConsole::OpenSsh;
                }
                ui.end_row();
            });
    });
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.label(d.tools_rdp_group);
        path_row(ui, &mut t.rdp_client, Pick::Open(Vec::new()), |e| e);
        ui.label(RichText::new(d.tools_rdp_hint).weak());
    });
}

// ---- the state of this module in the window ---------------------------------------------------

/// F14a's dialogs and selections.
#[derive(Default)]
pub struct DevUi {
    /// The GPU tab's ticked GPUs (all ticked by default).
    gpu_unticked: HashMap<(ConnectionId, Ref<Pgpu>), ()>,
    pub gpu_config: Option<GpuConfigDialog>,
    /// The USB tab's selected device.
    pub usb_selected: Option<(ConnectionId, Ref<Pusb>)>,
    pub vtpm: Option<VtpmDialog>,
    pub memory: Option<MemoryDialog>,
    /// The Properties dialog to open at its GPU page (the placement
    /// policy's Edit...).
    pub open_gpu_properties: Option<(ConnectionId, PropertiesTarget)>,
    /// External consoles: only record the command lines, never start them
    /// (tests and the real-host run's dry run).
    pub dry_run: bool,
    /// Every command line launched (or recorded), in order.
    pub launched: Vec<String>,
    /// The last launch, shown on the Console tab.
    pub console_note: Option<String>,
    /// The SSH console's problem (with "Configure SSH Console").
    pub ssh_problem: Option<String>,
    /// Open Options at External Tools.
    pub open_options: bool,
    /// The Remote Desktop address being probed: (id, connection, VM).
    pub rdp_pending: Option<(u64, ConnectionId, Ref<Vm>)>,
    /// The port probed for Remote Desktop (0: 3389; tests use a local
    /// listener's).
    pub rdp_port: u16,
    pub errors: Vec<String>,
}

impl DevUi {
    /// The GPU tab of a pool (`host` none) or server.
    pub fn gpu_tab(&mut self, ui: &mut Ui, conn: &ConnectionView, host: Option<&Ref<Host>>) {
        let d = dev_strings();
        let m = &conn.model;
        if devices::vgpu_capability(m) {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let policy = match devices::placement_policy(m) {
                        PlacementPolicy::Density => d.policy_density,
                        PlacementPolicy::Performance => d.policy_performance,
                        PlacementPolicy::Mixed => d.policy_mixed,
                    };
                    ui.label(fill(d.gpu_placement, &[("policy", policy)]));
                    if ui.button(d.gpu_edit).clicked() {
                        let target = match xen_core::pool::visible_pool(m) {
                            Some((p, _)) => Some(PropertiesTarget::Pool(p.clone())),
                            None => host
                                .cloned()
                                .or_else(|| m.hosts.keys().next().cloned())
                                .map(PropertiesTarget::Host),
                        };
                        if let Some(t) = target {
                            self.open_gpu_properties = Some((conn.id, t));
                        }
                    }
                });
            });
        }
        let groups = devices::gpu_tab_groups(m, host);
        if groups.is_empty() {
            ui.label(if host.is_some() {
                d.gpu_none_host
            } else {
                d.gpu_none_pool
            });
            return;
        }
        let show_hosts = host.is_none() && m.hosts.len() > 1;
        for (gi, group) in groups.iter().enumerate() {
            let Some(first) = m.get(&group[0]) else {
                continue;
            };
            let vgpu = !devices::forbidden(m, devices::DeviceRestriction::Vgpu)
                && devices::pgpu_has_vgpu(m, first);
            ui.group(|ui| {
                ui.strong(devices::pgpu_name(m, first));
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width((ui.available_width() * 0.6).max(260.0));
                        let mut last_host = None;
                        for p in group {
                            let Some(rec) = m.get(p) else { continue };
                            if show_hosts && last_host != Some(&rec.host) {
                                last_host = Some(&rec.host);
                                ui.label(fill(
                                    d.gpu_on_host,
                                    &[(
                                        "host",
                                        &m.get(&rec.host)
                                            .map(|h| h.name_label.clone())
                                            .unwrap_or_default(),
                                    )],
                                ));
                            }
                            ui.horizontal(|ui| {
                                if group.len() > 1 && vgpu {
                                    let key = (conn.id, p.clone());
                                    let mut ticked = !self.gpu_unticked.contains_key(&key);
                                    if ui.checkbox(&mut ticked, "").changed() {
                                        if ticked {
                                            self.gpu_unticked.remove(&key);
                                        } else {
                                            self.gpu_unticked.insert(key, ());
                                        }
                                    }
                                }
                                gpu_bar(ui, &devices::gpu_bar(m, rec));
                            });
                        }
                    });
                    ui.vertical(|ui| {
                        ui.label(d.gpu_allowed_types);
                        for t in devices::sorted_types_desc(m, &first.supported_vgpu_types) {
                            let allowed = first.enabled_vgpu_types.contains(&t);
                            ui.horizontal(|ui| {
                                mark(ui, allowed);
                                ui.label(
                                    devices::vgpu_type_name(m, &t)
                                        .map(|n| dev_strings::vgpu_type_text(&n, false))
                                        .unwrap_or_default(),
                                );
                            });
                        }
                        if vgpu {
                            let ticked: Vec<Ref<Pgpu>> = group
                                .iter()
                                .filter(|p| {
                                    !self.gpu_unticked.contains_key(&(conn.id, (*p).clone()))
                                })
                                .cloned()
                                .collect();
                            if group.len() > 1 {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            ticked.len() < group.len(),
                                            Button::new(d.gpu_select_all),
                                        )
                                        .clicked()
                                    {
                                        for p in group {
                                            self.gpu_unticked.remove(&(conn.id, p.clone()));
                                        }
                                    }
                                    if ui
                                        .add_enabled(
                                            !ticked.is_empty(),
                                            Button::new(d.gpu_clear_all),
                                        )
                                        .clicked()
                                    {
                                        for p in group {
                                            self.gpu_unticked.insert((conn.id, p.clone()), ());
                                        }
                                    }
                                });
                            }
                            let label = if group.len() > 1 {
                                d.gpu_edit_selected
                            } else {
                                d.gpu_edit
                            };
                            if ui
                                .add_enabled(!ticked.is_empty(), Button::new(label))
                                .clicked()
                            {
                                self.gpu_config = GpuConfigDialog::open(conn, ticked);
                            }
                        }
                    });
                });
            });
            if gi + 1 < groups.len() {
                ui.add_space(5.0);
            }
        }
    }

    /// The USB tab of a server.
    pub fn usb_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        host: &xapi::HostRecord,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let d = dev_strings();
        let m = &conn.model;
        ui.label(RichText::new(d.usb_devices).size(15.0));
        let list = devices::host_pusbs(m, host);
        if self
            .usb_selected
            .as_ref()
            .is_some_and(|(c, p)| *c != conn.id || !list.iter().any(|(r, _)| *r == p))
        {
            self.usb_selected = None;
        }
        Grid::new("usb-devices")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.strong(d.col_location);
                ui.strong(d.col_description);
                ui.strong(d.col_passthrough);
                ui.strong(d.col_vm);
                ui.end_row();
                for (r, p) in &list {
                    let selected = self.usb_selected.as_ref().is_some_and(|(_, s)| s == *r);
                    if ui.selectable_label(selected, &p.path).clicked() {
                        self.usb_selected = Some((conn.id, (*r).clone()));
                    }
                    ui.label(dev_strings::pusb_text(&devices::pusb_description(p)));
                    ui.label(if p.passthrough_enabled {
                        d.enabled
                    } else {
                        d.disabled
                    });
                    ui.label(
                        devices::pusb_vm(m, p)
                            .map(|v| v.name_label.clone())
                            .unwrap_or_default(),
                    );
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        let selected = self
            .usb_selected
            .as_ref()
            .and_then(|(_, r)| m.get(r).map(|p| (r.clone(), p)));
        let (label, enabled) = match &selected {
            None => (d.usb_enable, false),
            Some((_, p)) if p.passthrough_enabled => (d.usb_disable, !devices::pusb_in_use(m, p)),
            Some(_) => (d.usb_enable, true),
        };
        if ui.add_enabled(enabled, Button::new(label)).clicked()
            && let Some((r, p)) = selected
        {
            ops.request(
                conn,
                ActionRequest::Device(Box::new(DeviceRequest::SetUsbPassthrough {
                    pusb: r,
                    enabled: !p.passthrough_enabled,
                })),
                backend,
            );
        }
    }

    /// Opens a vTPM or memory dialog.
    pub fn open(&mut self, conn: &ConnectionView, request: crate::provision_ui::DialogRequest) {
        use crate::provision_ui::DialogRequest;
        match request {
            DialogRequest::ManageVtpm(vm) => {
                self.vtpm = Some(VtpmDialog {
                    connection: conn.id,
                    vm,
                    selected: None,
                });
            }
            DialogRequest::MemorySettings(vms) => {
                self.memory = MemoryDialog::open(conn, vms);
            }
            _ => {}
        }
    }

    /// Takes a precheck answer this module asked for.
    pub fn on_precheck(&mut self, state: &AppState, id: u64, result: &PrecheckResult) -> bool {
        let Some((pending, c, vm)) = self.rdp_pending.clone() else {
            return false;
        };
        let PrecheckResult::RdpAddress(address) = result else {
            return false;
        };
        if pending != id {
            return false;
        }
        self.rdp_pending = None;
        let d = dev_strings();
        match address {
            None => self.errors.push(d.rdp_no_address.to_owned()),
            Some(a) => {
                let uuid = state
                    .connection(c)
                    .and_then(|conn| conn.model.get(&vm).map(|v| v.uuid.clone()))
                    .unwrap_or_default();
                let cmd = consoles::rdp_command(
                    &state.settings.preferences.external_tools,
                    Platform::current(),
                    a,
                    &uuid,
                    &std::env::temp_dir(),
                );
                if let Err(e) = self.launch(&cmd) {
                    self.errors
                        .push(fill(d.rdp_failed, &[("error", &e.to_string())]));
                }
            }
        }
        true
    }

    /// Starts `cmd` (or only records it in a dry run).
    fn launch(&mut self, cmd: &CommandLine) -> std::io::Result<()> {
        let line = cmd.to_string();
        tracing::info!(command = %line, dry_run = self.dry_run, "external console");
        if !self.dry_run {
            cmd.launch()?;
        }
        self.console_note = Some(fill(dev_strings().console_launched, &[("command", &line)]));
        self.launched.push(line);
        Ok(())
    }

    /// The Console tab's external console request.
    pub fn console_request(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        vm: Option<&Ref<Vm>>,
        request: ConsoleRequest,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let d = dev_strings();
        match request {
            ConsoleRequest::Ssh(address) => {
                let tools = &state.settings.preferences.external_tools;
                match consoles::ssh_command(tools, Platform::current(), "root", &address, |p| {
                    p.exists()
                }) {
                    Ok(cmd) => {
                        if self.launch(&cmd).is_err() {
                            self.ssh_problem = Some(d.ssh_failed.to_owned());
                        }
                    }
                    Err(SshError::NotConfigured) => {
                        self.ssh_problem = Some(d.ssh_not_configured.to_owned());
                    }
                    Err(SshError::Missing(_)) => self.ssh_problem = Some(d.ssh_missing.to_owned()),
                }
            }
            ConsoleRequest::Rdp => {
                let Some(vm) = vm.and_then(|r| conn.model.get(r).map(|v| (r, v))) else {
                    return;
                };
                let candidates = consoles::rdp_candidates(&conn.model, vm.1);
                let id = next_id();
                self.rdp_pending = Some((id, conn.id, vm.0.clone()));
                self.console_note = Some(d.rdp_looking.to_owned());
                backend.send(Command::Precheck {
                    connection: conn.id,
                    id,
                    check: xen_core::Precheck::RdpAddress {
                        candidates,
                        port: if self.rdp_port == 0 {
                            consoles::RDP_PORT
                        } else {
                            self.rdp_port
                        },
                    },
                });
            }
            ConsoleRequest::EnableRdp => {
                if let Some(vm) = vm {
                    ops.request(
                        conn,
                        ActionRequest::Device(Box::new(DeviceRequest::EnableRdp {
                            vm: vm.clone(),
                        })),
                        backend,
                    );
                }
            }
        }
    }

    /// Draws the open dialogs.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let conn_of = |c: ConnectionId| state.connection(c).cloned();
        if let Some(dlg) = self.gpu_config.as_mut() {
            match conn_of(dlg.connection) {
                Some(conn) if !dlg.window(ctx, &conn, ops, backend) => {}
                _ => self.gpu_config = None,
            }
        }
        if let Some(dlg) = self.vtpm.as_mut() {
            match conn_of(dlg.connection) {
                Some(conn) if !dlg.window(ctx, &conn, ops, backend) => {}
                _ => self.vtpm = None,
            }
        }
        if let Some(dlg) = self.memory.as_mut() {
            match conn_of(dlg.connection) {
                Some(conn) if !dlg.window(ctx, &conn, ops, backend) => {}
                _ => self.memory = None,
            }
        }
        if let Some(problem) = self.ssh_problem.clone() {
            let d = dev_strings();
            let (mut configure, mut cancel) = (false, false);
            window(branding::PRODUCT_NAME.to_owned()).show(ctx, |ui| {
                ui.colored_label(ui.visuals().warn_fg_color, problem);
                dialog_buttons(ui, |row| {
                    configure = row.button(ButtonRole::Primary, d.ssh_configure).clicked();
                    cancel = row.button(ButtonRole::Cancel, d.cancel).clicked();
                });
            });
            if configure {
                self.open_options = true;
            }
            if configure || cancel {
                self.ssh_problem = None;
            }
        }
    }
}

/// The Console tab's external console buttons for `vm` (or a server's
/// console when `vm` is none, with `host`): SSH, and Remote Desktop for a
/// VM; the pass-through GPU warning.
pub fn console_buttons(
    ui: &mut Ui,
    conn: &ConnectionView,
    vm: Option<&VmRecord>,
    host: Option<&Ref<Host>>,
) -> Option<ConsoleRequest> {
    let d = dev_strings();
    let m = &conn.model;
    let mut out = None;
    let ssh = match (vm, host) {
        (Some(vm), _) => consoles::vm_ssh(m, vm),
        (None, Some(h)) => consoles::host_ssh(m, h),
        _ => return None,
    };
    if ssh.visible
        && ui
            .add_enabled(ssh.address.is_some(), Button::new(d.ssh_button))
            .clicked()
        && let Some(a) = ssh.address
    {
        out = Some(ConsoleRequest::Ssh(a));
    }
    if let Some(vm) = vm {
        let rdp = consoles::rdp_state(m, vm);
        // The toggle's visibility: Linux HVM guests have one console.
        let shown = !(xen_core::objects::vm_is_hvm(vm) && !rdp.has_rdp);
        if shown && vm.power_state == xapi::VmPowerState::Running {
            let (label, enabled) = if rdp.can_enable() {
                (d.rdp_turn_on, true)
            } else {
                (d.rdp_open, rdp.usable)
            };
            let r = ui.add_enabled(enabled, Button::new(label));
            let r = if rdp.enabled || rdp.control {
                r
            } else {
                r.on_disabled_hover_text(d.rdp_not_enabled)
            };
            if r.clicked() {
                out = Some(if rdp.can_enable() {
                    ConsoleRequest::EnableRdp
                } else {
                    ConsoleRequest::Rdp
                });
            }
        }
        if vm.power_state == xapi::VmPowerState::Running && devices::vm_has_gpu_passthrough(m, vm) {
            ui.colored_label(ui.visuals().warn_fg_color, d.gpu_passthrough_console);
        }
    }
    out
}

/// The Memory tab's Edit... opens the dialog or wizard for these VMs.
pub fn memory_request(vms: Vec<Ref<Vm>>) -> crate::provision_ui::DialogRequest {
    crate::provision_ui::DialogRequest::MemorySettings(vms)
}
