//! Main window.

use std::sync::Arc;
use std::time::Duration;

use egui::collapsing_header::CollapsingState;
use egui::{
    Align2, Button, CentralPanel, Context, Grid, Key, Panel, RichText, ScrollArea, TextEdit, Ui,
};
use xapi::{Host, Pool, Ref, Vm, VmPowerState, VmRecord};
use xen_core::model::{self, ConnectionModel, TreePool, TreeVm};
use xen_core::{
    AppState, BackendHandle, CertificatePrompt, Command, ConnectionStatus, Notification, VmAction,
};

use crate::branding;
use crate::i18n::{self, fill, strings};
use crate::icons;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    Pool(Ref<Pool>),
    Host(Ref<Host>),
    Vm(Ref<Vm>),
}

struct ConnectForm {
    open: bool,
    server: String,
    username: String,
    password: String,
    focus: bool,
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self {
            open: false,
            server: String::new(),
            username: "root".to_owned(),
            password: String::new(),
            focus: false,
        }
    }
}

/// The application: UI state only. All XAPI work happens in the backend.
pub struct XenAdminApp {
    backend: BackendHandle,
    selection: Option<Selection>,
    connect: ConnectForm,
    cert_prompt: Option<CertificatePrompt>,
    about_open: bool,
    errors: Vec<String>,
    /// Tree derived from the last model snapshot (rebuilt only when it changes).
    tree_cache: Option<(Arc<ConnectionModel>, Arc<Vec<TreePool>>)>,
}

impl XenAdminApp {
    pub fn new(backend: BackendHandle) -> Self {
        Self {
            backend,
            selection: None,
            connect: ConnectForm {
                open: true,
                focus: true,
                ..ConnectForm::default()
            },
            cert_prompt: None,
            about_open: false,
            errors: Vec::new(),
            tree_cache: None,
        }
    }

    fn drain_notifications(&mut self) {
        while let Some(notification) = self.backend.try_recv() {
            match notification {
                Notification::CertificatePrompt(prompt) => {
                    // Only one connection attempt is live at a time; a newer
                    // prompt supersedes an unanswered one.
                    if let Some(old) = self.cert_prompt.replace(prompt) {
                        old.reject();
                    }
                }
                Notification::VmActionFailed {
                    vm_name,
                    action,
                    error,
                } => {
                    self.errors.push(fill(
                        strings().vm_action_failed,
                        &[
                            ("action", i18n::action_label(action)),
                            ("vm", &vm_name),
                            ("error", &i18n::describe_xapi_error(&error)),
                        ],
                    ));
                }
            }
        }
    }

    fn tree_for(&mut self, model: &Arc<ConnectionModel>) -> Arc<Vec<TreePool>> {
        match &self.tree_cache {
            Some((cached, tree)) if Arc::ptr_eq(cached, model) => tree.clone(),
            _ => {
                let tree = Arc::new(model.tree());
                self.tree_cache = Some((model.clone(), tree.clone()));
                tree
            }
        }
    }

    fn open_connect_dialog(&mut self) {
        self.connect.open = true;
        self.connect.focus = true;
    }

    fn selected_vm<'a>(&self, state: &'a AppState) -> Option<(Ref<Vm>, &'a Arc<VmRecord>)> {
        match &self.selection {
            Some(Selection::Vm(r)) => state.model.vms.get(r).map(|rec| (r.clone(), rec)),
            _ => None,
        }
    }

    // ---- panels -----------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(s.menu_file, |ui| {
                if ui.button(s.menu_connect).clicked() {
                    self.open_connect_dialog();
                    ui.close();
                }
                let can_disconnect = !matches!(state.status, ConnectionStatus::Disconnected);
                if ui
                    .add_enabled(can_disconnect, Button::new(s.menu_disconnect))
                    .clicked()
                {
                    self.backend.send(Command::Disconnect);
                    self.selection = None;
                    ui.close();
                }
                ui.separator();
                if ui.button(s.menu_quit).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(s.menu_vm, |ui| {
                let vm = self.selected_vm(state);
                if self.vm_action_buttons(ui, state, vm.as_ref().map(|(r, rec)| (r, rec.as_ref())))
                {
                    ui.close();
                }
            });
            ui.menu_button(s.menu_help, |ui| {
                if ui
                    .button(format!("{} {}", s.menu_about, branding::PRODUCT_NAME))
                    .clicked()
                {
                    self.about_open = true;
                    ui.close();
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.button(s.menu_connect).clicked() {
                self.open_connect_dialog();
            }
            ui.separator();
            let vm = self.selected_vm(state);
            self.vm_action_buttons(ui, state, vm.as_ref().map(|(r, rec)| (r, rec.as_ref())));
        });
        ui.add_space(2.0);
    }

    /// Start / Shut Down buttons for `vm`; returns whether one was clicked.
    fn vm_action_buttons(
        &self,
        ui: &mut Ui,
        state: &AppState,
        vm: Option<(&Ref<Vm>, &VmRecord)>,
    ) -> bool {
        let s = strings();
        let busy = vm.is_some_and(|(r, _)| state.busy_vms.contains(r));
        let can_start = !busy && vm.is_some_and(|(_, rec)| model::can_start(rec));
        let can_stop = !busy && vm.is_some_and(|(_, rec)| model::can_clean_shutdown(rec));
        let mut clicked = false;
        if ui
            .add_enabled(can_start, Button::new(s.action_start))
            .clicked()
            && let Some((r, _)) = vm
        {
            self.backend.send(Command::Vm {
                vm: r.clone(),
                action: VmAction::Start,
            });
            clicked = true;
        }
        if ui
            .add_enabled(can_stop, Button::new(s.action_clean_shutdown))
            .clicked()
            && let Some((r, _)) = vm
        {
            self.backend.send(Command::Vm {
                vm: r.clone(),
                action: VmAction::CleanShutdown,
            });
            clicked = true;
        }
        clicked
    }

    fn status_bar(&self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        ui.horizontal(|ui| match &state.status {
            ConnectionStatus::Disconnected => {
                ui.label(s.status_disconnected);
            }
            ConnectionStatus::Connecting { server } => {
                ui.spinner();
                ui.label(fill(s.status_connecting, &[("server", server)]));
            }
            ConnectionStatus::Connected {
                server,
                api_version,
            } => {
                let version = format!("{}.{}", api_version.0, api_version.1);
                ui.label(fill(
                    s.status_connected,
                    &[("server", server), ("version", &version)],
                ));
            }
            ConnectionStatus::Failed { server, error } => {
                let text = fill(
                    s.status_failed,
                    &[
                        ("server", server),
                        ("error", &i18n::describe_connection_error(error)),
                    ],
                );
                ui.colored_label(ui.visuals().error_fg_color, text);
            }
        });
    }

    fn tree(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        ui.add_space(4.0);
        ui.strong(s.tree_heading);
        ui.separator();
        if state.model.pools.is_empty() {
            ui.label(RichText::new(s.tree_empty).weak());
            return;
        }
        let tree = self.tree_for(&state.model);
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            for pool in tree.iter() {
                let id = ui.make_persistent_id(("pool", pool.pool_ref.as_str()));
                CollapsingState::load_with_default_open(ui.ctx(), id, true)
                    .show_header(ui, |ui| {
                        icons::pool(ui);
                        self.tree_label(ui, Selection::Pool(pool.pool_ref.clone()), &pool.name);
                    })
                    .body(|ui| {
                        for host in &pool.hosts {
                            let id = ui.make_persistent_id(("host", host.host_ref.as_str()));
                            CollapsingState::load_with_default_open(ui.ctx(), id, true)
                                .show_header(ui, |ui| {
                                    icons::host(ui, host.enabled);
                                    self.tree_label(
                                        ui,
                                        Selection::Host(host.host_ref.clone()),
                                        &host.name,
                                    );
                                })
                                .body(|ui| {
                                    for vm in &host.vms {
                                        self.vm_row(ui, state, vm);
                                    }
                                });
                        }
                        for vm in &pool.vms {
                            self.vm_row(ui, state, vm);
                        }
                    });
            }
        });
    }

    fn tree_label(&mut self, ui: &mut Ui, selection: Selection, name: &str) -> egui::Response {
        let selected = self.selection.as_ref() == Some(&selection);
        let response = ui.selectable_label(selected, name);
        if response.clicked() {
            self.selection = Some(selection);
        }
        response
    }

    fn vm_row(&mut self, ui: &mut Ui, state: &AppState, vm: &TreeVm) {
        ui.horizontal(|ui| {
            // Leaves line up with the labels of collapsible siblings.
            ui.add_space(ui.spacing().indent);
            icons::vm(ui, &vm.power_state);
            let response = self.tree_label(ui, Selection::Vm(vm.vm_ref.clone()), &vm.name);
            if let Some(record) = state.model.vms.get(&vm.vm_ref) {
                response.context_menu(|ui| {
                    if self.vm_action_buttons(ui, state, Some((&vm.vm_ref, record))) {
                        ui.close();
                    }
                });
            }
        });
    }

    fn details(&self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        let model = &state.model;
        match &self.selection {
            Some(Selection::Vm(r)) if model.vms.contains_key(r) => {
                let vm = &model.vms[r];
                ui.horizontal(|ui| {
                    icons::vm(ui, &vm.power_state);
                    ui.heading(&vm.name_label);
                });
                ui.label(RichText::new(s.details_type_vm).weak());
                ui.separator();
                Grid::new("vm-details")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(s.details_power_state);
                        ui.colored_label(
                            icons::power_color(&vm.power_state),
                            power_label(&vm.power_state),
                        );
                        ui.end_row();
                        if let Some(host) = model.hosts.get(&vm.resident_on) {
                            ui.label(s.details_type_host);
                            ui.label(&host.name_label);
                            ui.end_row();
                        }
                        row(ui, s.details_description, &vm.name_description);
                        row(ui, s.details_uuid, &vm.uuid);
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    self.vm_action_buttons(ui, state, Some((r, vm)));
                    if state.busy_vms.contains(r) {
                        ui.spinner();
                        ui.label(s.details_busy);
                    }
                });
            }
            Some(Selection::Host(r)) if model.hosts.contains_key(r) => {
                let host = &model.hosts[r];
                ui.horizontal(|ui| {
                    icons::host(ui, host.enabled);
                    ui.heading(&host.name_label);
                });
                ui.label(RichText::new(s.details_type_host).weak());
                ui.separator();
                let resident = host
                    .resident_vms
                    .iter()
                    .filter(|v| model.vms.get(*v).is_some_and(|rec| model::is_guest(rec)))
                    .count();
                Grid::new("host-details")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        row(ui, s.details_address, &host.address);
                        row(
                            ui,
                            s.details_enabled,
                            if host.enabled {
                                s.details_enabled
                            } else {
                                s.details_disabled
                            },
                        );
                        let version = [
                            host.software_version.get("product_brand"),
                            host.software_version.get("product_version"),
                        ]
                        .into_iter()
                        .flatten()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ");
                        row(ui, s.details_version, &version);
                        row(ui, s.details_vms, &resident.to_string());
                        row(ui, s.details_description, &host.name_description);
                        row(ui, s.details_uuid, &host.uuid);
                    });
            }
            Some(Selection::Pool(r)) if model.pools.contains_key(r) => {
                let pool = &model.pools[r];
                let name = model
                    .coordinator()
                    .map(|h| h.name_label.as_str())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    icons::pool(ui);
                    ui.heading(if pool.name_label.is_empty() {
                        name
                    } else {
                        &pool.name_label
                    });
                });
                ui.label(RichText::new(s.details_type_pool).weak());
                ui.separator();
                let guests = model.vms.values().filter(|v| model::is_guest(v)).count();
                Grid::new("pool-details")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        row(ui, s.details_coordinator, name);
                        row(ui, s.details_hosts, &model.hosts.len().to_string());
                        row(ui, s.details_vms, &guests.to_string());
                        row(ui, s.details_description, &pool.name_description);
                        row(ui, s.details_uuid, &pool.uuid);
                    });
            }
            _ => {
                ui.centered_and_justified(|ui| {
                    let hint = if model.pools.is_empty() {
                        s.tree_empty
                    } else {
                        s.tree_no_selection
                    };
                    ui.label(RichText::new(hint).weak());
                });
            }
        }
    }

    // ---- windows ----------------------------------------------------------

    fn connect_window(&mut self, ctx: &Context) {
        if !self.connect.open {
            return;
        }
        let s = strings();
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(s.connect_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                Grid::new("connect-form")
                    .num_columns(2)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        let label = ui.label(s.connect_server);
                        let server = ui
                            .add(
                                TextEdit::singleline(&mut self.connect.server)
                                    .hint_text(s.connect_server_hint)
                                    .desired_width(240.0),
                            )
                            .labelled_by(label.id);
                        if std::mem::take(&mut self.connect.focus) {
                            server.request_focus();
                        }
                        ui.end_row();
                        let label = ui.label(s.connect_user);
                        ui.add(
                            TextEdit::singleline(&mut self.connect.username).desired_width(240.0),
                        )
                        .labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label(s.connect_password);
                        let password = ui
                            .add(
                                TextEdit::singleline(&mut self.connect.password)
                                    .password(true)
                                    .desired_width(240.0),
                            )
                            .labelled_by(label.id);
                        if password.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            submit = true;
                        }
                        ui.end_row();
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ready = !self.connect.server.trim().is_empty()
                        && !self.connect.username.trim().is_empty();
                    if ui
                        .add_enabled(ready, Button::new(s.connect_button))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button(s.cancel).clicked() {
                        cancel = true;
                    }
                });
            });
        let ready =
            !self.connect.server.trim().is_empty() && !self.connect.username.trim().is_empty();
        if submit && ready {
            self.backend.send(Command::Connect {
                server: self.connect.server.trim().to_owned(),
                username: self.connect.username.trim().to_owned(),
                password: std::mem::take(&mut self.connect.password),
            });
            self.selection = None;
            self.connect.open = false;
        } else if cancel || !open {
            self.connect.password.clear();
            self.connect.open = false;
        }
    }

    fn certificate_window(&mut self, ctx: &Context) {
        let Some(prompt) = &self.cert_prompt else {
            return;
        };
        let s = strings();
        let mut decision = None;
        egui::Window::new(s.cert_title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                let template = if prompt.previous.is_some() {
                    s.cert_changed
                } else {
                    s.cert_unknown
                };
                let text = fill(template, &[("server", &prompt.server)]);
                if prompt.previous.is_some() {
                    ui.colored_label(ui.visuals().warn_fg_color, text);
                } else {
                    ui.label(text);
                }
                ui.add_space(8.0);
                ui.label(s.cert_fingerprint);
                ui.label(RichText::new(prompt.fingerprint.to_string()).monospace());
                if let Some(previous) = &prompt.previous {
                    ui.add_space(4.0);
                    ui.label(s.cert_previous);
                    ui.label(RichText::new(previous.to_string()).monospace().weak());
                }
                ui.add_space(8.0);
                ui.label(RichText::new(s.cert_advice).weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(s.cert_trust).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(s.cancel).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(accept) = decision
            && let Some(prompt) = self.cert_prompt.take()
        {
            if accept {
                prompt.accept();
            } else {
                prompt.reject();
            }
        }
    }

    fn about_window(&mut self, ctx: &Context) {
        let s = strings();
        let mut open = self.about_open;
        egui::Window::new(format!("{} {}", s.about_title, branding::PRODUCT_NAME))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.heading(branding::PRODUCT_NAME);
                ui.label(fill(s.about_version, &[("version", branding::VERSION)]));
                ui.label(s.about_summary);
                ui.add_space(8.0);
                ui.strong(s.about_credits_heading);
                ui.label(s.about_credits);
                ui.add_space(8.0);
                egui::CollapsingHeader::new(fill(
                    s.about_license_heading,
                    &[("license", branding::LICENSE_ID)],
                ))
                .default_open(false)
                .show(ui, |ui| {
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.label(RichText::new(branding::LICENSE_TEXT).monospace().small());
                    });
                });
            });
        self.about_open = open;
    }

    fn errors_window(&mut self, ctx: &Context) {
        if self.errors.is_empty() {
            return;
        }
        let s = strings();
        let mut clear = false;
        egui::Window::new(s.errors_title)
            .collapsible(true)
            .resizable(true)
            .anchor(Align2::RIGHT_BOTTOM, [-12.0, -36.0])
            .show(ctx, |ui| {
                ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    for error in &self.errors {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                });
                if ui.button(s.errors_clear).clicked() {
                    clear = true;
                }
            });
        if clear {
            self.errors.clear();
        }
    }
}

impl eframe::App for XenAdminApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.drain_notifications();
        let state = self.backend.state();
        let ctx = ui.ctx().clone();
        Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &state));
        Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &state));
        Panel::left("tree")
            .resizable(true)
            .default_size(280.0)
            .min_size(180.0)
            .show(ui, |ui| self.tree(ui, &state));
        CentralPanel::default().show(ui, |ui| self.details(ui, &state));
        self.connect_window(&ctx);
        self.certificate_window(&ctx);
        self.about_window(&ctx);
        self.errors_window(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(prompt) = self.cert_prompt.take() {
            prompt.reject();
        }
        self.backend.shutdown_blocking(Duration::from_secs(3));
    }
}

fn row(ui: &mut Ui, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    ui.label(label);
    ui.label(value);
    ui.end_row();
}

fn power_label(state: &VmPowerState) -> &'static str {
    let s = strings();
    match state {
        VmPowerState::Running => s.power_running,
        VmPowerState::Halted => s.power_halted,
        VmPowerState::Suspended => s.power_suspended,
        VmPowerState::Paused => s.power_paused,
        VmPowerState::Unknown(_) => s.power_unknown,
    }
}

#[cfg(test)]
mod tests {
    //! Drives the real window code headlessly (egui_kittest + AccessKit)
    //! against the mock XAPI server: connect dialog, certificate prompt, live
    //! tree, Start / Shut Down buttons and About window.

    use std::time::{Duration, Instant};

    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use xapi_mock::MockServer;
    use xen_core::BackendOptions;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// Steps frames until `done` holds (the backend works on other threads).
    fn step_until(
        harness: &mut Harness<'_, XenAdminApp>,
        what: &str,
        done: impl Fn(&Harness<'_, XenAdminApp>) -> bool,
    ) {
        let deadline = Instant::now() + WAIT;
        loop {
            harness.step();
            if done(harness) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn is_enabled(harness: &Harness<'_, XenAdminApp>, label: &str) -> bool {
        harness
            .get_all_by_label(label)
            .next()
            .is_some_and(|n| !n.accesskit_node().is_disabled())
    }

    #[test]
    fn connect_trust_and_operate_through_the_ui() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(MockServer::start(true));
        let handle = runtime.handle().clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_eframe(|cc| {
                let ctx = cc.egui_ctx.clone();
                let options = BackendOptions {
                    originator: "xenadmin-ui-test".into(),
                    event_timeout: 5.0,
                    ..Default::default()
                };
                XenAdminApp::new(BackendHandle::spawn(
                    &handle,
                    options,
                    Arc::new(move || ctx.request_repaint()),
                ))
            });
        let s = strings();

        // The connect dialog opens on start with the server field focused.
        harness.step();
        harness.get_by_label(s.connect_title);
        let server_field = harness.get_by_label(s.connect_server);
        assert!(server_field.is_focused());
        server_field.type_text(&server.address());
        harness.step();
        assert_eq!(
            harness.get_by_label(s.connect_user).value().as_deref(),
            Some("root")
        );
        harness
            .get_by_role_and_label(Role::PasswordInput, s.connect_password)
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::PasswordInput, s.connect_password)
            .type_text(xapi_mock::PASSWORD);
        harness.step();
        harness.get_by_label(s.connect_button).click();

        // Trust-on-first-use prompt shows the fingerprint.
        step_until(&mut harness, "certificate prompt", |h| {
            h.query_by_label(s.cert_trust).is_some()
        });
        let fingerprint = xapi::Fingerprint::of_der(&xapi_mock::certificate_der()).to_string();
        harness.get_by_label(&fingerprint);
        harness.get_by_label(s.cert_trust).click();

        // Tree: pool -> hosts -> VMs; no templates, snapshots or control domains.
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
        });
        for name in [
            "xcp-host-01",
            "xcp-host-02",
            "web-01",
            "db-01",
            "build-runner",
            "legacy-app",
        ] {
            harness.get_by_label(name);
        }
        for hidden in [
            "Debian Bookworm 12",
            "web-01 before upgrade",
            "Control domain on host: xcp-host-01",
        ] {
            assert!(
                harness.query_by_label(hidden).is_none(),
                "{hidden} must not be in the tree"
            );
        }

        // Buttons follow power_state / allowed_operations.
        harness.get_by_label("db-01").click();
        harness.step();
        assert!(is_enabled(&harness, s.action_start));
        assert!(!is_enabled(&harness, s.action_clean_shutdown));
        harness
            .get_all_by_label(s.action_start)
            .next()
            .unwrap()
            .click();
        step_until(&mut harness, "db-01 running", |h| {
            h.query_by_label(s.power_running).is_some() && is_enabled(h, s.action_clean_shutdown)
        });
        assert_eq!(
            server.record("vm", "OpaqueRef:vm-db").unwrap()["power_state"],
            "Running"
        );
        assert!(!is_enabled(&harness, s.action_start));

        harness
            .get_all_by_label(s.action_clean_shutdown)
            .next()
            .unwrap()
            .click();
        step_until(&mut harness, "db-01 halted", |h| {
            h.query_by_label(s.power_halted).is_some()
        });

        // A change made elsewhere shows up without any refresh.
        server.set_name_label("vm", "OpaqueRef:vm-build", "build-runner-renamed");
        step_until(&mut harness, "rename", |h| {
            h.query_by_label("build-runner-renamed").is_some()
        });

        // About window.
        let about_item = format!("{} {}", s.menu_about, branding::PRODUCT_NAME);
        harness.get_by_label(s.menu_help).click();
        step_until(&mut harness, "help menu", |h| {
            h.query_by_label(&about_item).is_some()
        });
        harness.get_by_label(&about_item).click();
        step_until(&mut harness, "about window", |h| {
            h.query_by_label(s.about_credits).is_some()
        });
        harness.get_by_label(branding::PRODUCT_NAME);
        harness.get_by_label(&fill(s.about_version, &[("version", branding::VERSION)]));
        harness.get_by_label(s.about_credits);
    }
}
