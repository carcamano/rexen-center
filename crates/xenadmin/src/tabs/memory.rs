//! The Memory tab (`BallooningPage`): a bar and figures per host, and per
//! group of VMs with the same memory settings. Read-only: no Edit buttons.

use egui::{Color32, CornerRadius, Grid, Rect, RichText, Sense, Ui, pos2, vec2};
use xapi::{HostRecord, Ref, Vm, VmPowerState, VmRecord};
use xen_core::format::memory_size;
use xen_core::objects::{self, HostMemory};

use super::{Ctx, Subject, vm_display_name};
use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// One coloured part of a host's memory bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub name: String,
    pub bytes: i64,
    pub tooltip: String,
    pub color: Color32,
}

/// A row of the Memory tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryRow {
    Host {
        name: String,
        memory: HostMemory,
        segments: Vec<Segment>,
        /// Label/value pairs beside the bar.
        values: Vec<(&'static str, String)>,
        /// "(N% of total memory)".
        percent: String,
    },
    Vms {
        /// The VMs sharing these settings.
        names: Vec<String>,
        static_max: i64,
        /// Average current memory of the running ones.
        used: i64,
        /// Dynamic range markers, for VMs that balloon.
        dynamic: Option<(i64, i64)>,
        values: Vec<(&'static str, String)>,
        tooltip: String,
    },
}

const XEN_COLOR: Color32 = Color32::from_rgb(169, 169, 169);
const DOM0_COLOR: Color32 = Color32::from_rgb(105, 105, 105);
const VM_COLORS: [Color32; 2] = [
    Color32::from_rgb(25, 25, 112),
    Color32::from_rgb(70, 130, 180),
];
const USED_COLOR: Color32 = Color32::from_rgb(34, 139, 34);

/// The rows for `subject`: a host and the VMs whose home it is; every host
/// of a pool (no VM rows); a VM alone.
pub fn memory_rows(ctx: Ctx<'_>, subject: Subject<'_>) -> Vec<MemoryRow> {
    let m = ctx.model;
    match subject {
        Subject::Pool(..) => objects::sorted_hosts(m)
            .into_iter()
            .filter(|(_, h)| objects::host_is_live(m, h))
            .filter_map(|(_, h)| host_row(ctx, h))
            .collect(),
        Subject::Host(host_ref, host) => {
            let mut rows: Vec<MemoryRow> = host_row(ctx, host).into_iter().collect();
            let mut vms: Vec<(&Ref<Vm>, &VmRecord)> = m
                .vms
                .iter()
                .map(|(r, v)| (r, &**v))
                .filter(|(_, v)| {
                    objects::vm_kind(v) == objects::VmKind::Guest
                        && objects::vm_is_shown(v, ctx.show_hidden)
                        && objects::vm_home(m, v) == Some(host_ref)
                })
                .collect();
            vms.sort_by(|a, b| objects::vm_cmp(*a, *b));
            rows.extend(vm_groups(ctx, &vms));
            rows
        }
        Subject::Vm(r, vm) => vm_groups(ctx, &[(r, vm)]),
        Subject::Sr(..) => Vec::new(),
    }
}

fn host_row(ctx: Ctx<'_>, host: &HostRecord) -> Option<MemoryRow> {
    let t = tab_strings();
    let m = ctx.model;
    let memory = objects::host_memory(m, host)?;
    let mut segments = vec![
        Segment {
            name: t.mem_xen.to_owned(),
            bytes: memory.xen - memory.dom0,
            tooltip: format!(
                "{}\n{}",
                t.mem_xen,
                memory_size(memory.xen - memory.dom0, true)
            ),
            color: XEN_COLOR,
        },
        Segment {
            name: fill(t.mem_dom0_caption, &[("host", &host.name_label)]),
            bytes: memory.dom0,
            tooltip: format!(
                "{}\n{}",
                fill(t.mem_dom0_caption, &[("host", &host.name_label)]),
                memory_size(memory.dom0, true)
            ),
            color: DOM0_COLOR,
        },
    ];
    let mut resident: Vec<(&Ref<Vm>, &VmRecord)> = host
        .resident_vms
        .iter()
        .filter_map(|r| m.vms.get_key_value(r))
        .map(|(r, v)| (r, &**v))
        .filter(|(_, v)| !v.is_control_domain)
        .collect();
    resident.sort_by(|a, b| objects::vm_cmp(*a, *b));
    for (i, (_, vm)) in resident.iter().enumerate() {
        let actual = objects::vm_memory_actual(m, vm).unwrap_or(0);
        let mut tooltip = format!(
            "{}\n{}",
            vm.name_label,
            fill(t.mem_current, &[("memory", &memory_size(actual, true))])
        );
        if objects::vm_supports_ballooning(m, vm) {
            tooltip.push_str(&format!(
                "\n{}: {}\n{}: {}",
                t.mem_min,
                memory_size(vm.memory_dynamic_min, true),
                t.mem_max,
                memory_size(vm.memory_dynamic_max, true)
            ));
            if vm.memory_dynamic_max != vm.memory_static_max {
                tooltip.push_str(&format!(
                    "\n{}: {}",
                    t.mem_static_max,
                    memory_size(vm.memory_static_max, true)
                ));
            }
        }
        segments.push(Segment {
            name: vm.name_label.clone(),
            bytes: actual,
            tooltip,
            color: VM_COLORS[i % 2],
        });
    }
    segments.retain(|s| s.bytes >= 0);
    let total_max = memory.total_dynamic_max + memory.xen;
    let percent = if memory.total > 0 {
        (total_max as f64 / memory.total as f64 * 100.0).round() as i64
    } else {
        0
    };
    Some(MemoryRow::Host {
        name: host.name_label.clone(),
        values: vec![
            (t.mem_total, memory_size(memory.total, true)),
            (t.mem_used, memory_size(memory.total - memory.free, true)),
            (t.mem_dom0, memory_size(memory.dom0, true)),
            (t.mem_available, memory_size(memory.available, true)),
            (t.mem_total_max, memory_size(total_max, true)),
        ],
        percent: fill(t.mem_percent, &[("percent", &percent.to_string())]),
        memory,
        segments,
    })
}

/// Groups VMs with identical memory settings into one row each, running
/// ones first.
fn vm_groups(ctx: Ctx<'_>, vms: &[(&Ref<Vm>, &VmRecord)]) -> Vec<MemoryRow> {
    let t = tab_strings();
    let m = ctx.model;
    type Key = (bool, String, i64, i64, i64, i64);
    let key = |vm: &VmRecord| -> Key {
        let ballooning = objects::vm_supports_ballooning(m, vm);
        if ballooning {
            (
                true,
                vm.power_state.as_str().to_owned(),
                vm.memory_static_min,
                vm.memory_static_max,
                vm.memory_dynamic_min,
                vm.memory_dynamic_max,
            )
        } else {
            (
                false,
                vm.power_state.as_str().to_owned(),
                0,
                vm.memory_static_max,
                0,
                0,
            )
        }
    };
    let mut groups: Vec<(Key, Vec<&VmRecord>)> = Vec::new();
    for (_, vm) in vms {
        let k = key(vm);
        match groups.iter_mut().find(|(gk, _)| *gk == k) {
            Some((_, members)) => members.push(vm),
            None => groups.push((k, vec![vm])),
        }
    }
    let rank = |state: &str| match state {
        "Running" => 0,
        "Paused" => 1,
        "Suspended" => 2,
        "Halted" => 3,
        _ => 4,
    };
    groups.sort_by_key(|((_, state, ..), _)| rank(state));
    groups
        .into_iter()
        .map(|((ballooning, ..), members)| {
            let first = members[0];
            let live: Vec<i64> = members
                .iter()
                .filter(|v| matches!(v.power_state, VmPowerState::Running | VmPowerState::Paused))
                .filter_map(|v| objects::vm_memory_actual(m, v))
                .collect();
            let used = if live.is_empty() {
                0
            } else {
                live.iter().sum::<i64>() / live.len() as i64
            };
            let mut values = Vec::new();
            if ballooning {
                values.push((t.mem_min, memory_size(first.memory_dynamic_min, true)));
                values.push((t.mem_max, memory_size(first.memory_dynamic_max, true)));
                if first.memory_dynamic_max != first.memory_static_max {
                    values.push((t.mem_static_max, memory_size(first.memory_static_max, true)));
                }
            } else {
                values.push((t.mem_memory, memory_size(first.memory_static_max, true)));
            }
            let template = if members.len() > 1 {
                t.mem_current_per_vm
            } else {
                t.mem_current
            };
            MemoryRow::Vms {
                names: members.iter().map(|v| vm_display_name(v)).collect(),
                static_max: first.memory_static_max,
                used,
                dynamic: ballooning.then_some((first.memory_dynamic_min, first.memory_dynamic_max)),
                values,
                tooltip: fill(template, &[("memory", &memory_size(used, true))]),
            }
        })
        .collect()
}

// ---- drawing ---------------------------------------------------------------

/// Draws the Memory tab.
pub fn show(ui: &mut Ui, rows: &[MemoryRow]) {
    if rows.is_empty() {
        ui.label(RichText::new(tab_strings().empty_list).weak());
        return;
    }
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            ui.add_space(10.0);
            ui.separator();
        }
        match row {
            MemoryRow::Host {
                name,
                memory,
                segments,
                values,
                percent,
            } => {
                ui.strong(name);
                ui.horizontal_top(|ui| {
                    host_bar(ui, memory.total, segments);
                    ui.vertical(|ui| {
                        values_grid(ui, ("mem-host", name), values);
                        ui.label(percent);
                    });
                });
            }
            MemoryRow::Vms {
                names,
                static_max,
                used,
                dynamic,
                values,
                tooltip,
            } => {
                ui.strong(names.join(", "));
                ui.horizontal_top(|ui| {
                    vm_bar(ui, *static_max, *used, *dynamic, tooltip);
                    values_grid(ui, ("mem-vm", names.join(",")), values);
                });
            }
        }
    }
}

fn values_grid(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    values: &[(&'static str, String)],
) {
    Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (label, value) in values {
                ui.label(RichText::new(*label).weak());
                ui.label(value);
                ui.end_row();
            }
        });
}

fn bar_width(ui: &Ui) -> f32 {
    (ui.available_width() - 320.0).clamp(220.0, 900.0)
}

fn host_bar(ui: &mut Ui, total: i64, segments: &[Segment]) {
    let width = bar_width(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(5), ui.visuals().extreme_bg_color);
    let scale = if total > 0 { width / total as f32 } else { 0.0 };
    let mut x = rect.min.x;
    let mut hovered = None;
    for segment in segments {
        let w = segment.bytes as f32 * scale;
        let seg = Rect::from_min_size(pos2(x, rect.min.y), vec2(w, rect.height()));
        painter.rect_filled(seg, CornerRadius::ZERO, segment.color);
        let caption = format!("{}\n{}", segment.name, memory_size(segment.bytes, false));
        let galley = painter.layout(
            caption,
            egui::FontId::proportional(10.0),
            Color32::WHITE,
            w - 4.0,
        );
        if galley.size().x <= w - 4.0 && galley.size().y <= rect.height() {
            painter.galley(
                pos2(x + 2.0, seg.center().y - galley.size().y / 2.0),
                galley,
                Color32::WHITE,
            );
        }
        if let Some(pos) = response.hover_pos()
            && seg.contains(pos)
        {
            hovered = Some(segment.tooltip.clone());
        }
        x += w;
    }
    if let Some(text) = hovered {
        response.on_hover_text_at_pointer(text);
    }
}

fn vm_bar(ui: &mut Ui, static_max: i64, used: i64, dynamic: Option<(i64, i64)>, tooltip: &str) {
    let width = bar_width(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let bar = Rect::from_min_size(rect.min, vec2(width, 20.0));
    painter.rect_filled(bar, CornerRadius::same(4), ui.visuals().extreme_bg_color);
    let scale = if static_max > 0 {
        width / static_max as f32
    } else {
        0.0
    };
    let used_w = (used as f32 * scale).min(width);
    if used_w > 0.0 {
        let seg = Rect::from_min_size(bar.min, vec2(used_w, bar.height()));
        painter.rect_filled(seg, CornerRadius::same(4), USED_COLOR);
        let caption = memory_size(used, false);
        let galley =
            painter.layout_no_wrap(caption, egui::FontId::proportional(10.0), Color32::WHITE);
        if galley.size().x + 4.0 <= used_w {
            painter.galley(
                pos2(
                    seg.max.x - galley.size().x - 3.0,
                    seg.center().y - galley.size().y / 2.0,
                ),
                galley,
                Color32::WHITE,
            );
        }
    }
    if let Some((min, max)) = dynamic {
        let marker = ui.visuals().text_color();
        for bytes in [min, max] {
            let x = rect.min.x + (bytes as f32 * scale).min(width);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x, bar.max.y),
                    pos2(x - 4.0, rect.max.y),
                    pos2(x + 4.0, rect.max.y),
                ],
                marker,
                egui::Stroke::NONE,
            ));
        }
    }
    response.on_hover_text_at_pointer(tooltip);
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn host_rows_and_vm_groups() {
        let m = fixture();
        let t = tab_strings();
        let hr = Ref::new("OpaqueRef:host-0001");
        let rows = memory_rows(ctx(&m), Subject::Host(&hr, &m.hosts[&hr]));
        // The host, then web-01 (running), then legacy-app (suspended).
        assert_eq!(rows.len(), 3);
        let MemoryRow::Host {
            values,
            segments,
            percent,
            ..
        } = &rows[0]
        else {
            panic!("host row first");
        };
        let value = |label: &str| values.iter().find(|(l, _)| *l == label).unwrap().1.clone();
        assert_eq!(value(t.mem_total), "32.0 GB");
        assert_eq!(value(t.mem_used), "4.1 GB");
        assert_eq!(value(t.mem_dom0), "2.0 GB");
        assert_eq!(value(t.mem_available), "28.9 GB");
        // web-01's dynamic max 2 GiB + Xen 2.125 GiB.
        assert_eq!(value(t.mem_total_max), "4.1 GB");
        assert_eq!(percent, "(13% of total memory)");
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["XCP-ng", "Control domain on host xcp-host-01", "web-01"]
        );
        let MemoryRow::Vms {
            names,
            used,
            dynamic,
            values,
            ..
        } = &rows[1]
        else {
            panic!("VM row");
        };
        assert_eq!(names, &["web-01"]);
        assert_eq!(*used, 2 << 30);
        assert_eq!(*dynamic, Some((1 << 30, 2 << 30)));
        assert_eq!(
            values,
            &[
                (t.mem_min, "1.0 GB".to_owned()),
                (t.mem_max, "2.0 GB".to_owned()),
                (t.mem_static_max, "4.0 GB".to_owned())
            ]
        );
        let MemoryRow::Vms { names, values, .. } = &rows[2] else {
            panic!("VM row");
        };
        assert_eq!(names, &["legacy-app"]);
        assert_eq!(values, &[(t.mem_memory, "1.0 GB".to_owned())]);

        // A pool shows only its hosts.
        let (_, p) = m.pool().unwrap();
        let rows = memory_rows(ctx(&m), Subject::Pool(p));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| matches!(r, MemoryRow::Host { .. })));
    }

    #[test]
    fn identical_vms_share_a_row() {
        let mut m = fixture();
        let db = m.vms[&Ref::new("OpaqueRef:vm-db")].clone();
        let mut twin = (*db).clone();
        twin.name_label = "db-02".into();
        m.vms
            .insert(Ref::new("OpaqueRef:vm-db2"), std::sync::Arc::new(twin));
        let hr = Ref::new("OpaqueRef:host-0002");
        let rows = memory_rows(ctx(&m), Subject::Host(&hr, &m.hosts[&hr]));
        let MemoryRow::Vms { names, tooltip, .. } = &rows[1] else {
            panic!("VM row");
        };
        assert_eq!(names, &["db-01", "db-02"]);
        assert_eq!(tooltip, "Current memory usage: 0 per VM");
    }

    #[test]
    fn real_capture_memory_rows_build() {
        let m = capture();
        let (hr, h) = m.hosts.iter().next().unwrap();
        let rows = memory_rows(ctx(&m), Subject::Host(hr, h));
        assert!(rows.len() > 1);
    }
}
