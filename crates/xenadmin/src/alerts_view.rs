//! The Alerts view of the notifications area (`AlertSummaryPage`): the
//! XAPI messages of every connected server ([`xen_core::alerts`]), newest
//! first, with their severity, title and description, the server or pool,
//! the date, and Dismiss (one alert, the selected ones, or all), each
//! asked about first as in the original. Dismissing destroys the messages
//! on the server, through the operation machinery (RBAC check and role
//! elevation, Events entry).

use std::collections::HashSet;

use egui::{Color32, Context, Grid, RichText, ScrollArea, Ui};
use xapi::{Message, Ref};
use xen_core::alerts::{self, Alert, AlertObject, AlertPriority};
use xen_core::perfmon;
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionModel, PerfmonObjectRef,
};

use crate::i18n::fill;
use crate::locale::{MESSAGE_BODIES, MESSAGE_NAMES, TableInfo};
use crate::ops_ui::OpsUi;
use crate::perf_strings::perf_strings;

/// Width of the message column (title and description wrap inside it).
const MESSAGE_WIDTH: f32 = 440.0;

/// A pending "dismiss?" question.
struct Confirm {
    all: bool,
    targets: Vec<(ConnectionId, Vec<Ref<Message>>)>,
    focus: bool,
}

/// What a row asks the window for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertAction {
    /// "Alarm Settings": the alert settings of the alarm's object.
    EditAlerts(ConnectionId, PerfmonObjectRef),
}

/// State of the Alerts view.
#[derive(Default)]
pub struct AlertsView {
    selected: HashSet<(ConnectionId, Ref<Message>)>,
    confirm: Option<Confirm>,
}

/// A data table's text for `key`, in the active language.
fn lookup(table: &'static TableInfo, key: &str) -> Option<&'static str> {
    let pairs = table.pairs;
    let i = pairs.binary_search_by(|(k, _)| (*k).cmp(key)).ok()?;
    Some(crate::locale::translated(table, pairs[i].0).unwrap_or(pairs[i].1))
}

/// The friendly name of a message type (`Message.FriendlyName`), or the
/// name itself.
pub fn message_title(name: &str) -> String {
    lookup(&MESSAGE_NAMES, &name.to_lowercase())
        .map(str::to_owned)
        .unwrap_or_else(|| name.to_owned())
}

pub fn priority_label(p: AlertPriority) -> &'static str {
    let s = perf_strings();
    match p {
        AlertPriority::Critical => s.prio_critical,
        AlertPriority::Major => s.prio_major,
        AlertPriority::Warning => s.prio_warning,
        AlertPriority::Minor => s.prio_minor,
        AlertPriority::Information => s.prio_information,
        AlertPriority::Unknown => s.prio_unknown,
    }
}

fn priority_color(ui: &egui::Ui, p: AlertPriority) -> Color32 {
    use crate::theme::{Tone, tone};
    match p {
        AlertPriority::Critical => tone(ui, Tone::Error),
        AlertPriority::Major => tone(ui, Tone::Warning),
        AlertPriority::Warning => tone(ui, Tone::Caution),
        AlertPriority::Minor => tone(ui, Tone::Ok),
        AlertPriority::Information => tone(ui, Tone::Info),
        AlertPriority::Unknown => crate::theme::muted(ui.visuals()),
    }
}

/// The alert's title (`MessageAlert.Title` / `AlarmMessageAlert.Title`):
/// the friendly name, prefixed with the object's name unless it is the
/// pool (or the server of a pool of one).
pub fn alert_title(model: &ConnectionModel, alert: &Alert) -> String {
    let p = perf_strings();
    if let Some(alarm) = &alert.alarm {
        let title = match alarm.variable.as_str() {
            perfmon::CPU_USAGE => Some(p.alarm_cpu),
            perfmon::NETWORK_USAGE => Some(p.alarm_network),
            perfmon::DISK_USAGE => Some(p.alarm_disk),
            perfmon::MEMORY_FREE => Some(p.alarm_memory),
            perfmon::FS_USAGE => Some(p.alarm_fs),
            perfmon::LOG_FS_USAGE => Some(p.alarm_log_fs),
            perfmon::DOM0_MEMORY => Some(p.alarm_dom0_memory),
            v if v.starts_with("sr_io_throughput_total_") => Some(p.alarm_storage),
            _ => None,
        };
        if let Some(t) = title {
            return t.to_owned();
        }
    }
    if alert.record.name == "FAILED_LOGIN_ATTEMPTS"
        && let AlertObject::Pool(_) = &alert.object
    {
        return fill(p.failed_login_title, &[("pool", &alert.object_name)]);
    }
    let title = message_title(&alert.record.name);
    let prefixed = match &alert.object {
        AlertObject::Pool(_) => false,
        AlertObject::Host(_) => model.hosts.len() > 1,
        _ => true,
    };
    if prefixed && !alert.object_name.is_empty() {
        format!("{}: {title}", alert.object_name)
    } else {
        title
    }
}

fn percent(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

fn data_rate(v: f64) -> String {
    crate::perf_view::value_text(xen_core::performance::Unit::BytesPerSecond, v)
}

fn period_text(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    let p = perf_strings();
    let (template, count) = if s >= 3600 && s % 3600 == 0 {
        (p.period_hours, s / 3600)
    } else if s >= 60 {
        (p.period_minutes, s / 60)
    } else {
        (p.period_seconds, s)
    };
    fill(template, &[("count", &count.to_string())])
}

/// The alert's description (`Description`): perfmon alarms in words, the
/// friendly body of known messages with the object's name, else the body.
pub fn alert_description(model: &ConnectionModel, alert: &Alert) -> String {
    let p = perf_strings();
    let object = alert.object_name.as_str();
    if let Some(a) = &alert.alarm {
        let period = period_text(a.period);
        let o = ("object", object);
        let text = match a.variable.as_str() {
            perfmon::CPU_USAGE => Some(fill(
                p.alarm_cpu_desc,
                &[o, ("value", &percent(a.value)), ("period", &period)],
            )),
            perfmon::NETWORK_USAGE => Some(fill(
                p.alarm_network_desc,
                &[o, ("value", &data_rate(a.value)), ("period", &period)],
            )),
            perfmon::DISK_USAGE => Some(fill(
                p.alarm_disk_desc,
                &[o, ("value", &data_rate(a.value)), ("period", &period)],
            )),
            perfmon::MEMORY_FREE => Some(fill(
                p.alarm_memory_desc,
                &[
                    o,
                    (
                        "value",
                        &xen_core::format::memory_size((a.value * 1024.0) as i64, false),
                    ),
                    ("period", &period),
                ],
            )),
            perfmon::FS_USAGE => Some(fill(p.alarm_fs_desc, &[o, ("value", &percent(a.value))])),
            perfmon::LOG_FS_USAGE => Some(fill(
                p.alarm_log_fs_desc,
                &[o, ("value", &percent(a.value))],
            )),
            perfmon::DOM0_MEMORY => Some(fill(
                p.alarm_dom0_memory_desc,
                &[o, ("value", &percent(a.value))],
            )),
            v if v.starts_with("sr_io_throughput_total_") => {
                let short = v.trim_start_matches("sr_io_throughput_total_");
                let sr = model
                    .srs
                    .values()
                    .find(|s| s.uuid.starts_with(short))
                    .map(|s| s.name_label.clone())
                    .unwrap_or_default();
                Some(fill(
                    p.alarm_storage_desc,
                    &[
                        o,
                        ("sr", &sr),
                        ("value", &data_rate(a.value * 1024.0 * 1024.0)),
                        ("period", &period),
                    ],
                ))
            }
            _ => None,
        };
        if let Some(t) = text {
            return t;
        }
    }
    if alert.record.name == "FAILED_LOGIN_ATTEMPTS"
        && let Some(f) = alerts::FailedLogins::parse(&alert.record.body)
    {
        return failed_logins_text(&f);
    }
    if !matches!(alert.object, AlertObject::Other)
        && let Some(template) = lookup(&MESSAGE_BODIES, &alert.record.name.to_lowercase())
        && !template.contains("{2}")
    {
        let pool = model
            .pool()
            .map(|(_, p)| xen_core::objects::pool_name(model, p))
            .unwrap_or_default();
        return template.replace("{0}", object).replace("{1}", &pool);
    }
    alert.record.body.trim().to_owned()
}

/// `FailedLoginAttemptAlert.Description`.
fn failed_logins_text(f: &alerts::FailedLogins) -> String {
    let p = perf_strings();
    let n = |v: i64| v.to_string();
    if f.total == 0 && f.unknown > 0 {
        return fill(p.failed_login_all_unknown, &[("count", &n(f.unknown))]);
    }
    let mut lines = Vec::new();
    if f.total > 0 {
        lines.push(fill(p.failed_login_total, &[("count", &n(f.total))]));
    }
    if f.unknown > 0 {
        lines.push(fill(p.failed_login_unknown, &[("count", &n(f.unknown))]));
    }
    if !f.known.is_empty() {
        lines.push(p.failed_login_common.to_owned());
        for k in &f.known {
            let date = xapi::DateTime(k.date.clone())
                .unix_seconds()
                .map(|t| xen_core::format::datetime_utc(t, true))
                .unwrap_or_else(|| k.date.clone());
            let entries: Vec<String> = [
                (p.failed_login_ip, k.ip.clone()),
                (
                    p.failed_login_number,
                    if k.number > 0 {
                        n(k.number)
                    } else {
                        String::new()
                    },
                ),
                (p.failed_login_date, date),
                (p.failed_login_username, k.username.clone()),
                (p.failed_login_originator, k.originator.clone()),
                (p.failed_login_useragent, k.useragent.clone()),
            ]
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(t, v)| fill(t, &[("0", &v)]))
            .collect();
            lines.push(format!("- {}", entries.join(", ")));
        }
    }
    lines.join("\n")
}

/// The server or pool an alert belongs to (`AppliesTo`).
fn applies_to(model: &ConnectionModel, server: &str) -> String {
    model
        .pool()
        .map(|(_, p)| xen_core::objects::pool_name(model, p))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| server.to_owned())
}

/// Alerts of every connection, newest first.
pub fn all_alerts(state: &AppState) -> Vec<(ConnectionId, Alert)> {
    let mut out: Vec<(ConnectionId, Alert)> = state
        .connections
        .iter()
        .flat_map(|c| alerts::alerts(&c.model).into_iter().map(move |a| (c.id, a)))
        .collect();
    out.sort_by(|a, b| {
        b.1.record
            .timestamp
            .unix_seconds()
            .cmp(&a.1.record.timestamp.unix_seconds())
    });
    out
}

/// The perfmon object an alarm's alert opens the settings of.
fn alarm_object(alert: &Alert) -> Option<PerfmonObjectRef> {
    alert.alarm.as_ref()?;
    match &alert.object {
        AlertObject::Vm(v) => Some(PerfmonObjectRef::Vm(v.clone())),
        AlertObject::Host(h) => Some(PerfmonObjectRef::Host(h.clone())),
        AlertObject::Sr(s) => Some(PerfmonObjectRef::Sr(s.clone())),
        _ => None,
    }
}

impl AlertsView {
    /// The selected alerts.
    #[cfg(test)]
    pub fn selected(&self) -> Vec<(ConnectionId, Ref<Message>)> {
        self.selected.iter().cloned().collect()
    }

    /// Whether the "dismiss?" question is open (and for all alerts).
    #[cfg(test)]
    pub fn asking(&self) -> Option<bool> {
        self.confirm.as_ref().map(|c| c.all)
    }

    /// Number of alerts (for the view's tab title).
    pub fn count(state: &AppState) -> usize {
        state
            .connections
            .iter()
            .map(|c| alerts::alerts(&c.model).len())
            .sum()
    }

    /// Draws the view.
    pub fn show(&mut self, ui: &mut Ui, state: &AppState) -> Option<AlertAction> {
        let p = perf_strings();
        let all = all_alerts(state);
        // Selections of alerts that went away are dropped.
        let present: HashSet<(ConnectionId, Ref<Message>)> =
            all.iter().map(|(c, a)| (*c, a.message.clone())).collect();
        self.selected.retain(|k| present.contains(k));
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.selected.is_empty(),
                    egui::Button::new(p.alerts_dismiss_selected),
                )
                .clicked()
            {
                let targets = group(self.selected.iter().cloned());
                self.confirm = Some(Confirm {
                    all: false,
                    targets,
                    focus: true,
                });
            }
            if ui
                .add_enabled(!all.is_empty(), egui::Button::new(p.alerts_dismiss_all))
                .clicked()
            {
                let targets = group(all.iter().map(|(c, a)| (*c, a.message.clone())));
                self.confirm = Some(Confirm {
                    all: true,
                    targets,
                    focus: true,
                });
            }
        });
        ui.separator();
        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("alerts-scroll")
            .show(ui, |ui| {
                if all.is_empty() {
                    ui.label(RichText::new(p.alerts_empty).weak());
                    return;
                }
                Grid::new("alerts-grid")
                    .num_columns(5)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for title in [
                            p.alerts_col_severity,
                            p.alerts_col_message,
                            p.alerts_col_location,
                            p.alerts_col_date,
                            p.alerts_col_actions,
                        ] {
                            ui.label(RichText::new(title).strong());
                        }
                        ui.end_row();
                        for (conn_id, alert) in &all {
                            let Some(conn) = state.connection(*conn_id) else {
                                continue;
                            };
                            let model = &conn.model;
                            let key = (*conn_id, alert.message.clone());
                            ui.colored_label(
                                priority_color(ui, alert.priority),
                                priority_label(alert.priority),
                            );
                            ui.vertical(|ui| {
                                ui.set_width(MESSAGE_WIDTH);
                                let title = alert_title(model, alert);
                                let mut checked = self.selected.contains(&key);
                                if ui
                                    .checkbox(&mut checked, RichText::new(title).strong())
                                    .changed()
                                {
                                    if checked {
                                        self.selected.insert(key.clone());
                                    } else {
                                        self.selected.remove(&key);
                                    }
                                }
                                ui.add(egui::Label::new(alert_description(model, alert)).wrap());
                            });
                            ui.label(applies_to(model, &conn.server));
                            ui.label(
                                alert
                                    .record
                                    .timestamp
                                    .unix_seconds()
                                    .map(|t| xen_core::format::datetime_utc(t, true))
                                    .unwrap_or_default(),
                            );
                            ui.horizontal(|ui| {
                                if ui.button(p.alerts_dismiss).clicked() {
                                    self.confirm = Some(Confirm {
                                        all: false,
                                        targets: vec![(*conn_id, vec![alert.message.clone()])],
                                        focus: true,
                                    });
                                }
                                if let Some(object) = alarm_object(alert)
                                    && ui.button(p.alerts_alarm_action).clicked()
                                {
                                    action = Some(AlertAction::EditAlerts(*conn_id, object));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        action
    }

    /// The "dismiss?" question; Yes dismisses through `ops` (RBAC check).
    pub fn window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let p = perf_strings();
        let Some(confirm) = self.confirm.as_mut() else {
            return;
        };
        let mut answer = None;
        egui::Window::new(p.alerts_confirm_title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(if confirm.all {
                    p.alerts_confirm_all
                } else {
                    p.alerts_confirm_selected
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let yes = ui.button(if confirm.all {
                        p.alerts_confirm_yes_all
                    } else {
                        p.alerts_confirm_yes
                    });
                    if confirm.focus {
                        yes.request_focus();
                        confirm.focus = false;
                    }
                    if yes.clicked() {
                        answer = Some(true);
                    }
                    if ui.button(p.alerts_confirm_no).clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                let confirm = self.confirm.take().expect("a question");
                for (conn_id, messages) in confirm.targets {
                    if let Some(conn) = state.connection(conn_id) {
                        for m in &messages {
                            self.selected.remove(&(conn_id, m.clone()));
                        }
                        ops.dispatch(conn, ActionRequest::DismissAlerts { messages }, backend);
                    }
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

/// Groups alerts by connection.
fn group(
    items: impl Iterator<Item = (ConnectionId, Ref<Message>)>,
) -> Vec<(ConnectionId, Vec<Ref<Message>>)> {
    let mut out: Vec<(ConnectionId, Vec<Ref<Message>>)> = Vec::new();
    for (c, m) in items {
        match out.iter_mut().find(|(id, _)| *id == c) {
            Some((_, list)) => list.push(m),
            None => out.push((c, vec![m])),
        }
    }
    for (_, list) in &mut out {
        list.sort();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> ConnectionModel {
        let text = include_str!("../../../tests/fixtures/xcp-ng-8.3-capture.json");
        ConnectionModel::from_database(&serde_json::from_str(text).unwrap())
    }

    #[test]
    fn message_tables_are_sorted_for_lookup() {
        for table in [MESSAGE_NAMES.pairs, MESSAGE_BODIES.pairs] {
            assert!(table.windows(2).all(|w| w[0].0 < w[1].0));
        }
        assert_eq!(message_title("VM_STARTED"), "VM Started");
        assert_eq!(
            message_title("LICENSE_EXPIRES_SOON"),
            "License expires soon"
        );
        assert_eq!(message_title("SOMETHING_NEW"), "SOMETHING_NEW");
    }

    #[test]
    fn titles_and_descriptions_follow_the_original() {
        let model = capture();
        let all = alerts::alerts(&model);
        let login = all
            .iter()
            .find(|a| a.record.name == "FAILED_LOGIN_ATTEMPTS")
            .unwrap();
        // Failed logins: the original's title and the body in words.
        assert!(
            alert_title(&model, login)
                .starts_with("There has been a number of failed attempts to login to '")
        );
        let text = alert_description(&model, login);
        assert!(text.starts_with("There has been a total of "), "{text}");
        assert!(!text.contains('<'), "{text}");

        let (vm_ref, vm) = model
            .vms
            .iter()
            .find(|(_, v)| v.name_label == "guest-10")
            .unwrap();
        let alarm = Alert {
            message: Ref::new("OpaqueRef:m"),
            record: std::sync::Arc::new(xapi::MessageRecord {
                name: "ALARM".into(),
                priority: 3,
                cls: xapi::Cls::Vm,
                obj_uuid: vm.uuid.clone(),
                body: "value: 0.953\nconfig:\n<variable>\n<name value=\"cpu_usage\"/>\n<alarm_trigger_level value=\"0.9\"/>\n<alarm_trigger_period value=\"60\"/>\n</variable>".into(),
                ..Default::default()
            }),
            priority: AlertPriority::Warning,
            object: AlertObject::Vm(vm_ref.clone()),
            object_name: vm.name_label.clone(),
            alarm: xen_core::perfmon::Alarm::parse("value: 0.953\nconfig:\n<variable><name value=\"cpu_usage\"/><alarm_trigger_level value=\"0.9\"/><alarm_trigger_period value=\"60\"/></variable>"),
        };
        assert_eq!(alert_title(&model, &alarm), "CPU Usage Alarm");
        assert_eq!(
            alert_description(&model, &alarm),
            "CPU usage on guest-10 has been on average 95% for the last 1 minute(s)."
        );
        assert_eq!(
            alarm_object(&alarm),
            Some(PerfmonObjectRef::Vm(vm_ref.clone()))
        );
        // A VM message: prefixed with the VM, friendly body.
        let started = Alert {
            record: std::sync::Arc::new(xapi::MessageRecord {
                name: "VM_SECURE_BOOT_FAILED".into(),
                cls: xapi::Cls::Vm,
                obj_uuid: vm.uuid.clone(),
                ..Default::default()
            }),
            alarm: None,
            ..alarm
        };
        assert!(alert_title(&model, &started).starts_with("guest-10: "));
        assert!(alert_description(&model, &started).contains("guest-10"));
    }
}
