//! The Performance tab of a host or VM (`PerformancePage`): the original's
//! default graphs (CPU, memory, network and, for VMs, disks) drawn with
//! `egui_plot` from the archives the backend keeps up to date
//! ([`xen_core::performance`]), the Zoom ranges (10 minutes to a year;
//! the archive used follows the range, as in the original), and the VM
//! lifecycle events as markers.
//!
//! Like consoles, a performance feed is opened when its tab is shown and
//! closed (its handle dropped) when the object is no longer selected.

use std::collections::HashMap;

use egui::{Color32, RichText, Ui};
use egui_plot::{GridMark, Line, Plot, PlotPoints, VLine};
use xapi::DataSourceRecord;
use xen_core::alerts;
use xen_core::format::{clock_utc, day_utc, memory_size};
use xen_core::performance::{self, GraphDef, GraphKind, PerfHandle, PerfStatus, PerfTarget, Unit};
use xen_core::{BackendHandle, Command, ConnectionId, ConnectionModel, ConnectionView};
use xen_rrd::Archives;

use crate::i18n::fill;
use crate::perf_strings::perf_strings;

/// Which performance feed: connection and object.
pub type PerfKey = (ConnectionId, PerfTarget);

/// The Zoom menu's ranges (`PerformancePage`), plus 2 hours: the span of
/// the minute archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    TenMinutes,
    Hour,
    TwoHours,
    Day,
    Week,
    Month,
    Year,
}

impl Zoom {
    pub const ALL: [Self; 7] = [
        Self::TenMinutes,
        Self::Hour,
        Self::TwoHours,
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Year,
    ];

    /// Width in seconds; one second less than the nominal range, as the
    /// original's `ZoomToRange` (so 10 minutes still uses the 5 s archive).
    pub fn seconds(self) -> i64 {
        let nominal = match self {
            Self::TenMinutes => 600,
            Self::Hour => 3600,
            Self::TwoHours => 7200,
            Self::Day => 86_400,
            Self::Week => 7 * 86_400,
            Self::Month => 30 * 86_400,
            Self::Year => 366 * 86_400,
        };
        nominal - 1
    }

    pub fn label(self) -> &'static str {
        let p = perf_strings();
        match self {
            Self::TenMinutes => p.zoom_10_minutes,
            Self::Hour => p.zoom_hour,
            Self::TwoHours => p.zoom_2_hours,
            Self::Day => p.zoom_day,
            Self::Week => p.zoom_week,
            Self::Month => p.zoom_month,
            Self::Year => p.zoom_year,
        }
    }
}

/// What the tab asks the window for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerfAction {
    /// Open the alert settings of the shown object.
    EditAlerts,
}

/// Open performance feeds and the zoom.
pub struct Performance {
    open: HashMap<PerfKey, PerfHandle>,
    pub zoom: Zoom,
}

impl Default for Performance {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
            zoom: Zoom::TenMinutes,
        }
    }
}

/// Height of a graph (`GraphList.GRAPH_HEIGHT`).
const GRAPH_HEIGHT: f32 = 150.0;
/// Width of a graph's key (`GraphList.KEY_WIDTH`).
const KEY_WIDTH: f32 = 200.0;

impl Performance {
    /// Closes the feeds that are not shown any more.
    pub fn retain(&mut self, shown: Option<&PerfKey>, alive: impl Fn(ConnectionId) -> bool) {
        self.open.retain(|k, _| Some(k) == shown && alive(k.0));
    }

    /// The feed of `key` (opened on first use).
    pub fn handle(&mut self, backend: &BackendHandle, key: &PerfKey) -> &PerfHandle {
        self.open.entry(key.clone()).or_insert_with(|| {
            let (handle, link) = performance::pair();
            backend.send(Command::OpenPerformance {
                connection: key.0,
                target: key.1.clone(),
                link,
            });
            handle
        })
    }

    /// Whether a feed is open for `key`.
    #[cfg(test)]
    pub fn is_open(&self, key: &PerfKey) -> bool {
        self.open.contains_key(key)
    }

    /// Draws the tab.
    pub fn tab(
        &mut self,
        ui: &mut Ui,
        backend: &BackendHandle,
        conn: &ConnectionView,
        key: &PerfKey,
    ) -> Option<PerfAction> {
        let p = perf_strings();
        let data = self.handle(backend, key).data();
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(p.hdr_performance).strong().size(15.0));
            ui.add_space(12.0);
            ui.menu_button(p.zoom, |ui| {
                for zoom in Zoom::ALL {
                    if ui
                        .selectable_label(self.zoom == zoom, zoom.label())
                        .clicked()
                    {
                        self.zoom = zoom;
                        ui.close();
                    }
                }
            });
            ui.label(fill(p.showing, &[("range", self.zoom.label())]));
            ui.add_space(12.0);
            if ui.button(p.alert_settings).clicked() {
                action = Some(PerfAction::EditAlerts);
            }
        });
        match &data.status {
            PerfStatus::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(p.graph_loading);
                });
            }
            PerfStatus::NotRunning => {
                ui.label(RichText::new(p.perf_not_running).weak());
            }
            PerfStatus::Disconnected => {
                ui.label(RichText::new(p.perf_disconnected).weak());
            }
            PerfStatus::Failed(e) => {
                ui.colored_label(
                    crate::theme::tone(ui, crate::theme::Tone::Error),
                    fill(p.perf_failed, &[("error", e)]),
                );
            }
            PerfStatus::Gone => {
                ui.label(p.perf_gone);
                return action;
            }
            PerfStatus::Live => {}
        }
        ui.separator();
        let model = &conn.model;
        let graphs = performance::default_graphs(model, &key.1);
        // The right edge: the newest point the server sent (its clock),
        // or now.
        let to = latest(&data.archives).unwrap_or_else(now);
        let from = to - self.zoom.seconds();
        let uuid = target_uuid(model, &key.1).unwrap_or_default();
        let events: Vec<(i64, String)> = alerts::graph_events(model, &uuid)
            .into_iter()
            .filter(|(t, _)| *t >= from && *t <= to)
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("perf-scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (i, graph) in graphs.iter().enumerate() {
                    graph_ui(ui, model, key, &data, graph, i, from, to, &events);
                    ui.add_space(10.0);
                }
                if !events.is_empty() {
                    ui.label(RichText::new(p.events_label).strong());
                    for (t, name) in events.iter().rev() {
                        ui.label(format!(
                            "{}  {}",
                            xen_core::format::datetime_utc(*t, true),
                            crate::alerts_view::message_title(name)
                        ));
                    }
                }
            });
        if data.status == PerfStatus::Live || data.status == PerfStatus::Loading {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        action
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn latest(archives: &Archives) -> Option<i64> {
    xen_rrd::Interval::ALL
        .into_iter()
        .filter_map(|i| archives.last_time(i))
        .max()
}

fn target_uuid(model: &ConnectionModel, target: &PerfTarget) -> Option<String> {
    match target {
        PerfTarget::Host(h) => model.hosts.get(h).map(|r| r.uuid.clone()),
        PerfTarget::Vm(v) => model.vms.get(v).map(|r| r.uuid.clone()),
    }
}

/// The title of a default graph.
pub fn graph_title(kind: GraphKind) -> &'static str {
    let p = perf_strings();
    match kind {
        GraphKind::Cpu => p.graph_cpu,
        GraphKind::Memory => p.graph_memory,
        GraphKind::Network => p.graph_network,
        GraphKind::Disk => p.graph_disk,
    }
}

/// The friendly name of a data source (`Helpers.GetFriendlyDataSourceName`
/// for the default graphs' sources).
pub fn source_name(model: &ConnectionModel, target: &PerfTarget, name: &str) -> String {
    let p = perf_strings();
    let num = |rest: &str| rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty();
    if let Some(n) = name.strip_prefix("cpu").filter(|r| num(r)) {
        return fill(p.ds_cpu, &[("0", n)]);
    }
    match name {
        "cpu_avg" => return p.ds_cpu_avg.to_owned(),
        "cpu_usage" => return p.ds_cpu_usage.to_owned(),
        "memory_free_kib" | "memory_internal_free" => return p.ds_used_memory.to_owned(),
        "memory_total_kib" | "memory" => return p.ds_total_memory.to_owned(),
        "loadavg" => return p.ds_loadavg.to_owned(),
        _ => {}
    }
    let split = |prefix: &str| -> Option<(String, bool)> {
        let rest = name.strip_prefix(prefix)?;
        if let Some(d) = rest.strip_suffix("_rx") {
            Some((d.to_owned(), true))
        } else {
            rest.strip_suffix("_tx").map(|d| (d.to_owned(), false))
        }
    };
    if let Some((device, rx)) = split("pif_") {
        let n = device.strip_prefix("eth").unwrap_or(&device).to_owned();
        return fill(if rx { p.ds_nic_rx } else { p.ds_nic_tx }, &[("0", &n)]);
    }
    if let Some((device, rx)) = split("vif_") {
        // `{network} Receive`, as the original names a VIF by its network.
        let network = match target {
            PerfTarget::Vm(v) => model.vms.get(v).and_then(|vm| {
                vm.vifs
                    .iter()
                    .filter_map(|r| model.get(r))
                    .find(|vif| vif.device == device)
                    .and_then(|vif| model.get(&vif.network))
                    .map(|n| n.name_label.clone())
            }),
            PerfTarget::Host(_) => None,
        };
        let label = network.unwrap_or_else(|| fill(p.ds_vif_default, &[("device", &device)]));
        return fill(if rx { p.ds_vif_rx } else { p.ds_vif_tx }, &[("0", &label)]);
    }
    if let Some(rest) = name.strip_prefix("vbd_") {
        let (device, read) = match (rest.strip_suffix("_read"), rest.strip_suffix("_write")) {
            (Some(d), _) => (d, true),
            (_, Some(d)) => (d, false),
            _ => return name.to_owned(),
        };
        // `Disk {userdevice}`, as the original.
        let user = match target {
            PerfTarget::Vm(v) => model.vms.get(v).and_then(|vm| {
                vm.vbds
                    .iter()
                    .filter_map(|r| model.get(r))
                    .find(|vbd| vbd.device == device)
                    .map(|vbd| vbd.userdevice.clone())
            }),
            PerfTarget::Host(_) => None,
        };
        let label = user.unwrap_or_else(|| device.to_owned());
        return fill(
            if read { p.ds_vbd_read } else { p.ds_vbd_write },
            &[("0", &label)],
        );
    }
    name.to_owned()
}

/// A value with its unit, as the axis and the hover label show it.
pub fn value_text(unit: Unit, v: f64) -> String {
    match unit {
        Unit::Percent => format!("{v:.0}%"),
        Unit::Bytes => memory_size(v.max(0.0) as i64, true),
        Unit::BytesPerSecond => format!("{}/s", rate(v)),
        Unit::Plain => {
            if v.abs() >= 100.0 {
                format!("{v:.0}")
            } else {
                format!("{v:.2}")
            }
        }
    }
}

fn rate(v: f64) -> String {
    let v = v.max(0.0);
    if v >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", v / (1024.0 * 1024.0 * 1024.0))
    } else if v >= 1024.0 * 1024.0 {
        format!("{:.1} MB", v / (1024.0 * 1024.0))
    } else if v >= 1024.0 {
        format!("{:.0} kB", v / 1024.0)
    } else {
        format!("{v:.0} B")
    }
}

/// The unit a graph's axis uses (its first drawn source's).
fn graph_unit(graph: &GraphDef, sources: &[DataSourceRecord]) -> Unit {
    graph
        .sources
        .iter()
        .find(|s| performance::is_drawn(s))
        .map(|s| performance::unit_of(s, sources).0)
        .unwrap_or(Unit::Plain)
}

/// One line of a graph: its name, colour and the segments between gaps.
type Series = (String, Color32, Vec<Vec<[f64; 2]>>);

#[allow(clippy::too_many_arguments)]
fn graph_ui(
    ui: &mut Ui,
    model: &ConnectionModel,
    key: &PerfKey,
    data: &xen_core::PerfData,
    graph: &GraphDef,
    index: usize,
    from: i64,
    to: i64,
    events: &[(i64, String)],
) {
    let p = perf_strings();
    ui.label(RichText::new(graph_title(graph.kind)).strong());
    let unit = graph_unit(graph, &data.sources);
    // A VM without its guest agent reports no free memory.
    let needs_tools = graph.kind == GraphKind::Memory
        && matches!(key.1, PerfTarget::Vm(_))
        && data.status != PerfStatus::Loading
        && data
            .archives
            .series(xen_rrd::Interval::FiveSeconds, "memory_internal_free")
            .is_empty()
        && data
            .archives
            .series(xen_rrd::Interval::OneDay, "memory_internal_free")
            .is_empty();
    if needs_tools {
        ui.label(RichText::new(p.graph_needs_tools).weak());
        return;
    }
    let mut lines: Vec<Series> = Vec::new();
    let mut max_y: f64 = 0.0;
    for (i, source) in graph
        .sources
        .iter()
        .filter(|s| performance::is_drawn(s))
        .enumerate()
    {
        let points = performance::plot_points(&data.archives, &data.sources, source, from, to);
        let mut segments: Vec<Vec<[f64; 2]>> = vec![Vec::new()];
        for (t, v) in points {
            if v.is_finite() && v >= 0.0 {
                max_y = max_y.max(v);
                segments.last_mut().expect("a segment").push([t as f64, v]);
            } else if !segments.last().is_some_and(Vec::is_empty) {
                segments.push(Vec::new());
            }
        }
        segments.retain(|s| !s.is_empty());
        lines.push((
            source_name(model, &key.1, source),
            crate::theme::series(ui.visuals(), i),
            segments,
        ));
    }
    let empty = lines.iter().all(|l| l.2.is_empty());
    // The memory graph spans the whole memory, as the original's
    // (`DataSet.GetMemoryMax`): the newest total.
    let total = match graph.kind {
        GraphKind::Memory => graph
            .sources
            .iter()
            .find(|s| !performance::is_drawn(s))
            .and_then(|s| {
                let (_, factor) = performance::unit_of(s, &data.sources);
                data.archives
                    .points(s, from, to)
                    .iter()
                    .rev()
                    .find(|p| p.1.is_finite())
                    .map(|p| p.1 * factor)
            }),
        _ => None,
    };
    let span = to - from;
    let id = format!("perf-graph-{}-{index}", key.0);
    let mut plot = Plot::new(id)
        .height(GRAPH_HEIGHT)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .include_x(from as f64)
        .include_x(to as f64)
        .include_y(0.0)
        .x_grid_spacer(time_grid)
        .x_axis_formatter(move |mark: GridMark, _| axis_time(mark.value as i64, span))
        .y_axis_formatter(move |mark: GridMark, _| value_text(unit, mark.value))
        .label_formatter(move |hover| match hover {
            egui_plot::HoverPosition::NearDataPoint {
                plot_name,
                position,
                ..
            } => Some(format!(
                "{plot_name}\n{}\n{}",
                xen_core::format::datetime_utc(position.x as i64, true),
                value_text(unit, position.y)
            )),
            egui_plot::HoverPosition::Elsewhere { .. } => None,
        });
    plot = match (unit, total) {
        (Unit::Percent, _) => plot.include_y(100.0),
        (_, Some(total)) => plot.include_y(total.max(max_y)),
        _ => plot.include_y(max_y * 1.05).include_y(1.0),
    };
    // The plot, and its key on the right (`DataKey`, 200 px wide).
    let key_width = KEY_WIDTH.min(ui.available_width() / 3.0);
    let plot_width = (ui.available_width() - key_width - 12.0).max(200.0);
    let keys: Vec<(String, Color32)> = lines.iter().map(|l| (l.0.clone(), l.1)).collect();
    ui.horizontal_top(|ui| {
        let event_color = crate::theme::muted(ui.visuals());
        plot.width(plot_width).show(ui, |plot_ui| {
            for (name, color, segments) in lines {
                for segment in segments {
                    plot_ui.line(
                        Line::new(name.clone(), PlotPoints::new(segment))
                            .color(color)
                            .width(1.5),
                    );
                }
            }
            for (t, name) in events {
                plot_ui.vline(
                    VLine::new(crate::alerts_view::message_title(name), *t as f64)
                        .color(event_color)
                        .width(1.0),
                );
            }
        });
        ui.vertical(|ui| {
            ui.set_width(key_width);
            egui::ScrollArea::vertical()
                .id_salt(("perf-key", key.0, index))
                .max_height(GRAPH_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (name, color) in keys {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, color);
                            ui.add(egui::Label::new(name.as_str()).truncate())
                                .on_hover_text(name.as_str());
                        });
                    }
                });
        });
    });
    if empty && data.status != PerfStatus::Loading {
        ui.label(RichText::new(p.graph_no_data).weak());
    }
}

/// Time-friendly grid steps (seconds), finest first.
const TIME_STEPS: [i64; 20] = [
    5,
    10,
    15,
    30,
    60,
    120,
    300,
    600,
    900,
    1800,
    3600,
    7200,
    10_800,
    21_600,
    43_200,
    86_400,
    172_800,
    7 * 86_400,
    14 * 86_400,
    30 * 86_400,
];

/// Grid marks on round times: a labelled step giving about six marks, and
/// the next finer step as minor lines.
fn time_grid(input: egui_plot::GridInput) -> Vec<GridMark> {
    let (min, max) = input.bounds;
    let span = (max - min).max(1.0);
    let major_i = TIME_STEPS
        .iter()
        .position(|s| span / *s as f64 <= 7.0)
        .unwrap_or(TIME_STEPS.len() - 1);
    let mut marks = Vec::new();
    for (i, step) in [
        (major_i, TIME_STEPS[major_i]),
        (
            major_i.saturating_sub(1),
            TIME_STEPS[major_i.saturating_sub(1)],
        ),
    ] {
        if i != major_i && step == TIME_STEPS[major_i] {
            continue;
        }
        let mut t = (min / step as f64).ceil() as i64 * step;
        while (t as f64) <= max && marks.len() < 400 {
            let major = i == major_i;
            if major || t % TIME_STEPS[major_i] != 0 {
                marks.push(GridMark {
                    value: t as f64,
                    step_size: step as f64,
                });
            }
            t += step;
        }
    }
    marks
}

/// An axis time label: the time of day for spans up to two days, the date
/// beyond.
fn axis_time(t: i64, span: i64) -> String {
    if span <= 2 * 86_400 {
        clock_utc(t, span < 600)
    } else {
        day_utc(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xapi::{Host, Ref, Vm};

    fn lab() -> ConnectionModel {
        let text = include_str!("../../../tests/fixtures/records.json");
        ConnectionModel::from_database(&serde_json::from_str(text).unwrap())
    }

    #[test]
    fn data_source_names_follow_the_original() {
        let model = lab();
        let host = PerfTarget::Host(Ref::<Host>::new("OpaqueRef:host-0001"));
        assert_eq!(source_name(&model, &host, "cpu3"), "CPU 3");
        assert_eq!(source_name(&model, &host, "memory_free_kib"), "Used Memory");
        assert_eq!(source_name(&model, &host, "pif_eth0_rx"), "NIC 0 Receive");
        assert_eq!(source_name(&model, &host, "pif_eth1_tx"), "NIC 1 Send");
        let vm = PerfTarget::Vm(Ref::<Vm>::new("OpaqueRef:vm-web"));
        assert_eq!(
            source_name(&model, &vm, "memory_internal_free"),
            "Used Memory"
        );
        let names: Vec<String> = performance::default_graphs(&model, &vm)
            .iter()
            .flat_map(|g| g.sources.clone())
            .map(|s| source_name(&model, &vm, &s))
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("Disk ") && n.ends_with(" Read")),
            "{names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.ends_with(" Send") && !n.starts_with("VIF")),
            "{names:?}"
        );
        assert_eq!(source_name(&model, &vm, "something_else"), "something_else");
    }

    #[test]
    fn time_grid_marks_round_times() {
        let marks = time_grid(egui_plot::GridInput {
            bounds: (1_795_628_000.0, 1_795_628_599.0),
            base_step_size: 1.0,
        });
        let major: Vec<i64> = marks
            .iter()
            .filter(|m| m.step_size == 120.0)
            .map(|m| m.value as i64)
            .collect();
        assert!(!major.is_empty() && major.len() <= 7, "{major:?}");
        assert!(major.iter().all(|t| t % 120 == 0));
        let day = time_grid(egui_plot::GridInput {
            bounds: (0.0, 86_399.0),
            base_step_size: 1.0,
        });
        assert!(day.iter().any(|m| m.step_size == 21_600.0));
    }

    #[test]
    fn values_and_ranges() {
        assert_eq!(value_text(Unit::Percent, 42.4), "42%");
        assert_eq!(
            value_text(Unit::Bytes, 4.0 * 1024.0 * 1024.0 * 1024.0),
            "4.0 GB"
        );
        assert_eq!(value_text(Unit::BytesPerSecond, 2048.0), "2 kB/s");
        assert_eq!(value_text(Unit::Plain, 0.5), "0.50");
        assert_eq!(Zoom::TenMinutes.seconds(), 599);
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::TenMinutes.seconds()),
            xen_rrd::Interval::FiveSeconds
        );
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::Hour.seconds()),
            xen_rrd::Interval::OneMinute
        );
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::TwoHours.seconds()),
            xen_rrd::Interval::OneMinute
        );
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::Day.seconds()),
            xen_rrd::Interval::OneHour
        );
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::Week.seconds()),
            xen_rrd::Interval::OneHour
        );
        assert_eq!(
            xen_rrd::Interval::for_width(Zoom::Year.seconds()),
            xen_rrd::Interval::OneDay
        );
        assert_eq!(axis_time(1_795_628_969, 599), "17:49:29");
        assert_eq!(axis_time(1_795_628_969, 7199), "17:49");
        assert_eq!(axis_time(1_795_628_969, 86_400 * 7), "Nov 25");
    }
}
