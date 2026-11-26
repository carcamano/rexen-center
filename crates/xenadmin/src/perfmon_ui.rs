//! The performance alert settings of a VM, host or SR: the original's
//! "Alerts" page of the Properties dialog (`PerfmonAlertEditPage`), as a
//! window. Saving writes `other_config["perfmon"]` through the operation
//! machinery (RBAC check, role elevation, Events entry); nothing is sent
//! when nothing changed.

use egui::{Context, DragValue, Grid, RichText, Ui};
use xen_core::perfmon::{
    AlertGroup, INTERVAL_RANGE, MINUTES_RANGE, PerfmonConfig, PerfmonForm, PerfmonObject,
};
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionView, PerfmonObjectRef,
};

use crate::i18n::fill;
use crate::ops_ui::OpsUi;
use crate::perf_strings::perf_strings;

/// An open settings window.
pub struct PerfmonDialog {
    connection: ConnectionId,
    object: PerfmonObjectRef,
    name: String,
    original: PerfmonConfig,
    pub form: PerfmonForm,
}

impl PerfmonDialog {
    /// The window for `object`, filled from its current configuration.
    pub fn open(conn: &ConnectionView, object: PerfmonObjectRef) -> Option<Self> {
        let model = &conn.model;
        let (kind, name, other_config) = match &object {
            PerfmonObjectRef::Vm(r) => {
                let v = model.vms.get(r)?;
                (PerfmonObject::Vm, v.name_label.clone(), &v.other_config)
            }
            PerfmonObjectRef::Host(r) => {
                let h = model.hosts.get(r)?;
                (PerfmonObject::Host, h.name_label.clone(), &h.other_config)
            }
            PerfmonObjectRef::Sr(r) => {
                let s = model.srs.get(r)?;
                (PerfmonObject::Sr, s.name_label.clone(), &s.other_config)
            }
        };
        let original = PerfmonConfig::of(other_config);
        Some(Self {
            connection: conn.id,
            form: PerfmonForm::new(kind, &original),
            object,
            name,
            original,
        })
    }

    /// The request OK sends (`None`: nothing changed).
    pub fn request(&self) -> Option<ActionRequest> {
        let saved = self.form.apply(&self.original);
        if saved == self.original {
            return None;
        }
        Some(ActionRequest::SetPerfmon {
            object: self.object.clone(),
            config: (!saved.variables.is_empty()).then(|| saved.to_xml()),
        })
    }
}

fn group_texts(group: AlertGroup) -> (&'static str, &'static str, &'static str) {
    let p = perf_strings();
    match group {
        AlertGroup::Cpu => (p.pm_cpu_check, p.pm_cpu_level, p.pm_percent),
        AlertGroup::Network => (p.pm_net_check, p.pm_net_level, p.pm_kbs),
        AlertGroup::Disk => (p.pm_disk_check, p.pm_disk_level, p.pm_kbs),
        AlertGroup::Memory => (p.pm_memory_check, p.pm_memory_level, p.pm_mb),
        AlertGroup::Sr => (p.pm_sr_check, p.pm_sr_level, p.pm_kbs),
    }
}

/// Draws the page's controls (testable without a window).
pub fn form_ui(ui: &mut Ui, form: &mut PerfmonForm) {
    let p = perf_strings();
    ui.label(p.pm_rubric);
    ui.add_space(6.0);
    for (group, setting) in &mut form.groups {
        let (check, level, unit) = group_texts(*group);
        ui.checkbox(&mut setting.enabled, check);
        ui.add_enabled_ui(setting.enabled, |ui| {
            ui.indent(("perfmon-indent", *group as u8), |ui| {
                Grid::new(("perfmon-group", *group as u8))
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(level);
                        let (min, max) = group.level_range();
                        ui.horizontal(|ui| {
                            ui.add(
                                DragValue::new(&mut setting.level)
                                    .range(min..=max)
                                    .speed(1.0),
                            );
                            ui.label(unit);
                        });
                        ui.end_row();
                        ui.label(p.pm_longer);
                        ui.horizontal(|ui| {
                            ui.add(
                                DragValue::new(&mut setting.minutes)
                                    .range(MINUTES_RANGE.0..=MINUTES_RANGE.1)
                                    .speed(1.0)
                                    .fixed_decimals(0),
                            );
                            ui.label(p.pm_minutes);
                        });
                        ui.end_row();
                    });
            });
        });
        ui.add_space(4.0);
    }
    let any = form.groups.iter().any(|(_, s)| s.enabled);
    ui.add_enabled_ui(any, |ui| {
        ui.horizontal(|ui| {
            ui.label(p.pm_interval);
            ui.add(
                DragValue::new(&mut form.interval_minutes)
                    .range(INTERVAL_RANGE.0..=INTERVAL_RANGE.1)
                    .speed(5.0)
                    .fixed_decimals(0),
            );
            ui.label(p.pm_minutes);
        });
    });
    if !form.is_valid() {
        ui.colored_label(
            crate::theme::tone(ui, crate::theme::Tone::Error),
            p.pm_interval_multiple,
        );
    }
}

/// The original's summary of the page (`SubText`), e.g. "When CPU usage
/// exceeds 50% for 1 min(s)".
pub fn summary(form: &PerfmonForm) -> String {
    let p = perf_strings();
    let parts: Vec<String> = form
        .groups
        .iter()
        .filter(|(_, s)| s.enabled)
        .map(|(g, s)| {
            let template = match g {
                AlertGroup::Cpu => p.pm_sub_cpu,
                AlertGroup::Network => p.pm_sub_net,
                AlertGroup::Disk => p.pm_sub_disk,
                AlertGroup::Memory => p.pm_sub_memory,
                AlertGroup::Sr => p.pm_sub_sr,
            };
            fill(
                template,
                &[
                    ("level", &format!("{}", s.level)),
                    ("minutes", &format!("{}", s.minutes)),
                ],
            )
        })
        .collect();
    if parts.is_empty() {
        p.pm_none.to_owned()
    } else {
        parts.join("; ")
    }
}

/// Draws the open window, if any; OK saves through `ops`.
pub fn window(
    dialog: &mut Option<PerfmonDialog>,
    ctx: &Context,
    state: &AppState,
    ops: &mut OpsUi,
    backend: &BackendHandle,
) {
    let p = perf_strings();
    let Some(d) = dialog.as_mut() else {
        return;
    };
    let Some(conn) = state.connection(d.connection) else {
        *dialog = None;
        return;
    };
    let mut close = false;
    let mut save = false;
    egui::Window::new(fill(p.pm_title, &[("object", &d.name)]))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(460.0);
            form_ui(ui, &mut d.form);
            ui.add_space(4.0);
            ui.label(RichText::new(summary(&d.form)).weak());
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(d.form.is_valid(), egui::Button::new(p.pm_ok))
                    .clicked()
                {
                    save = true;
                }
                if ui.button(p.pm_cancel).clicked() {
                    close = true;
                }
            });
        });
    if save {
        if let Some(request) = d.request() {
            ops.dispatch(conn, request, backend);
        }
        close = true;
    }
    if close {
        *dialog = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xen_core::perfmon::CPU_USAGE;

    #[test]
    fn nothing_changed_sends_nothing_and_clearing_removes_the_key() {
        let model = xen_core::ConnectionModel::from_database(
            &serde_json::from_str(include_str!("../../../tests/fixtures/records.json")).unwrap(),
        );
        let conn = ConnectionView {
            id: 1,
            server: "s".into(),
            username: "root".into(),
            status: xen_core::ConnectionStatus::Connected {
                api_version: (2, 21),
            },
            model: std::sync::Arc::new(model),
            busy_vms: Default::default(),
            busy_hosts: Default::default(),
            busy_srs: Default::default(),
            rbac: None,
            pool_uuid: None,
            authority: "s:443".into(),
        };
        let vm = PerfmonObjectRef::Vm(xapi::Ref::new("OpaqueRef:vm-web"));
        let mut d = PerfmonDialog::open(&conn, vm.clone()).unwrap();
        assert!(d.request().is_none());
        assert_eq!(summary(&d.form), "None defined");
        d.form.setting_mut(AlertGroup::Cpu).unwrap().enabled = true;
        match d.request() {
            Some(ActionRequest::SetPerfmon {
                object,
                config: Some(xml),
            }) => {
                assert_eq!(object, vm);
                assert_eq!(
                    PerfmonConfig::parse(&xml).get(CPU_USAGE).unwrap().level,
                    Some(0.5)
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(summary(&d.form), "When CPU usage exceeds 50% for 1 min(s)");
        // From an existing configuration, turning everything off removes it.
        d.original = d.form.apply(&PerfmonConfig::default());
        d.form.setting_mut(AlertGroup::Cpu).unwrap().enabled = false;
        assert_eq!(
            d.request(),
            Some(ActionRequest::SetPerfmon {
                object: vm,
                config: None
            })
        );
    }
}
