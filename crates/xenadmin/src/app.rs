//! Main window.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::collapsing_header::CollapsingState;
use egui::{
    Align2, Button, CentralPanel, Context, Grid, Key, Panel, RichText, ScrollArea, TextEdit, Ui,
};
use xapi::{Host, Pool, Ref, Vm, VmPowerState, VmRecord};
use xen_core::model::{self, ConnectionModel, TreePool, TreeVm};
use xen_core::{
    AppState, BackendHandle, CertificatePrompt, Command, ConfigLocation, ConnectionError,
    ConnectionId, ConnectionStatus, ConnectionView, Notification, Preferences, SavedServer,
    VmAction,
};

use crate::branding;
use crate::events;
use crate::i18n::{self, fill, strings};
use crate::icons;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    /// A connection without a loaded pool (connecting, or failed).
    Connection(ConnectionId),
    /// A saved server that is not connected.
    Saved(String),
    Pool(ConnectionId, Ref<Pool>),
    Host(ConnectionId, Ref<Host>),
    Vm(ConnectionId, Ref<Vm>),
}

impl Selection {
    fn connection(&self) -> Option<ConnectionId> {
        match self {
            Self::Connection(c) | Self::Pool(c, _) | Self::Host(c, _) | Self::Vm(c, _) => Some(*c),
            Self::Saved(_) => None,
        }
    }
}

struct ConnectForm {
    open: bool,
    server: String,
    username: String,
    password: String,
    remember: bool,
    focus: bool,
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self {
            open: false,
            server: String::new(),
            username: "root".to_owned(),
            password: String::new(),
            remember: true,
            focus: false,
        }
    }
}

/// Things the tree and detail views ask for while drawing.
enum Action {
    Send(Command),
    OpenConnect { server: String, username: String },
}

/// The application: UI state only. All XAPI work happens in the backend.
pub struct XenAdminApp {
    backend: BackendHandle,
    selection: Option<Selection>,
    connect: ConnectForm,
    cert_prompts: VecDeque<CertificatePrompt>,
    about_open: bool,
    options_open: bool,
    events_open: bool,
    errors: Vec<String>,
    /// Tree derived from each connection's last model snapshot (rebuilt only
    /// when it changes).
    tree_cache: HashMap<ConnectionId, (Arc<ConnectionModel>, Arc<Vec<TreePool>>)>,
    actions: Vec<Action>,
}

impl XenAdminApp {
    /// The connect dialog opens on start unless saved servers are being
    /// restored.
    pub fn new(backend: BackendHandle) -> Self {
        let state = backend.state();
        let restoring = state.settings.preferences.restore_connections
            && state.settings.servers.iter().any(|s| s.reconnect);
        let mut app = Self {
            backend,
            selection: None,
            connect: ConnectForm::default(),
            cert_prompts: VecDeque::new(),
            about_open: false,
            options_open: false,
            events_open: true,
            errors: Vec::new(),
            tree_cache: HashMap::new(),
            actions: Vec::new(),
        };
        if !restoring {
            app.open_connect_dialog(&state, None);
        }
        app
    }

    fn drain_notifications(&mut self) {
        while let Some(notification) = self.backend.try_recv() {
            match notification {
                Notification::CertificatePrompt(prompt) => self.cert_prompts.push_back(prompt),
                Notification::VmActionFailed {
                    vm_name,
                    action,
                    error,
                    ..
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
                Notification::ConnectionRefused { server, error } => {
                    self.errors.push(fill(
                        strings().err_connection_refused,
                        &[
                            ("server", &server),
                            ("error", &i18n::describe_connection_error(&error)),
                        ],
                    ));
                }
            }
        }
        // Prompts of connections that went away need no answer.
        self.cert_prompts.retain(|p| !p.is_stale());
    }

    fn apply_actions(&mut self, state: &AppState) {
        for action in std::mem::take(&mut self.actions) {
            match action {
                Action::Send(command) => {
                    if let Command::Disconnect { connection } = &command
                        && self.selection.as_ref().and_then(Selection::connection)
                            == Some(*connection)
                    {
                        self.selection = None;
                    }
                    self.backend.send(command);
                }
                Action::OpenConnect { server, username } => {
                    self.open_connect_dialog(state, Some((server, username)));
                }
            }
        }
    }

    fn tree_for(&mut self, conn: &ConnectionView) -> Arc<Vec<TreePool>> {
        match self.tree_cache.get(&conn.id) {
            Some((cached, tree)) if Arc::ptr_eq(cached, &conn.model) => tree.clone(),
            _ => {
                let tree = Arc::new(conn.model.tree());
                self.tree_cache
                    .insert(conn.id, (conn.model.clone(), tree.clone()));
                tree
            }
        }
    }

    fn open_connect_dialog(&mut self, state: &AppState, prefill: Option<(String, String)>) {
        let (server, username) = prefill.unwrap_or_else(|| (String::new(), "root".to_owned()));
        self.connect = ConnectForm {
            open: true,
            focus: true,
            server,
            username,
            password: String::new(),
            remember: state.settings.preferences.save_passwords,
        };
    }

    fn selected_vm<'a>(
        &self,
        state: &'a AppState,
    ) -> Option<(&'a ConnectionView, Ref<Vm>, &'a Arc<VmRecord>)> {
        match &self.selection {
            Some(Selection::Vm(c, r)) => {
                let conn = state.connection(*c)?;
                conn.model.vms.get(r).map(|rec| (&**conn, r.clone(), rec))
            }
            _ => None,
        }
    }

    // ---- panels -----------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        let selected_conn = self
            .selection
            .as_ref()
            .and_then(Selection::connection)
            .filter(|c| state.connection(*c).is_some());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(s.menu_file, |ui| {
                if ui.button(s.menu_connect).clicked() {
                    self.actions.push(Action::OpenConnect {
                        server: String::new(),
                        username: "root".to_owned(),
                    });
                    ui.close();
                }
                if ui
                    .add_enabled(selected_conn.is_some(), Button::new(s.menu_disconnect))
                    .clicked()
                    && let Some(connection) = selected_conn
                {
                    self.actions
                        .push(Action::Send(Command::Disconnect { connection }));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !state.connections.is_empty(),
                        Button::new(s.menu_disconnect_all),
                    )
                    .clicked()
                {
                    for conn in &state.connections {
                        self.actions.push(Action::Send(Command::Disconnect {
                            connection: conn.id,
                        }));
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button(s.menu_quit).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(s.menu_view, |ui| {
                ui.checkbox(&mut self.events_open, s.menu_events);
            });
            ui.menu_button(s.menu_vm, |ui| {
                let vm = self.selected_vm(state);
                if self.vm_action_buttons(ui, vm.as_ref().map(|(c, r, rec)| (*c, r, rec.as_ref())))
                {
                    ui.close();
                }
            });
            ui.menu_button(s.menu_tools, |ui| {
                if ui.button(s.menu_options).clicked() {
                    self.options_open = true;
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
                self.actions.push(Action::OpenConnect {
                    server: String::new(),
                    username: "root".to_owned(),
                });
            }
            ui.separator();
            let vm = self.selected_vm(state);
            self.vm_action_buttons(ui, vm.as_ref().map(|(c, r, rec)| (*c, r, rec.as_ref())));
        });
        ui.add_space(2.0);
    }

    /// Start / Shut Down buttons for `vm`; returns whether one was clicked.
    fn vm_action_buttons(
        &mut self,
        ui: &mut Ui,
        vm: Option<(&ConnectionView, &Ref<Vm>, &VmRecord)>,
    ) -> bool {
        let s = strings();
        let usable = vm.filter(|(c, r, _)| c.status.is_connected() && !c.busy_vms.contains(*r));
        let can_start = usable.is_some_and(|(_, _, rec)| model::can_start(rec));
        let can_stop = usable.is_some_and(|(_, _, rec)| model::can_clean_shutdown(rec));
        let mut clicked = false;
        for (enabled, label, action) in [
            (can_start, s.action_start, VmAction::Start),
            (can_stop, s.action_clean_shutdown, VmAction::CleanShutdown),
        ] {
            if ui.add_enabled(enabled, Button::new(label)).clicked()
                && let Some((c, r, _)) = vm
            {
                self.actions.push(Action::Send(Command::Vm {
                    connection: c.id,
                    vm: r.clone(),
                    action,
                }));
                clicked = true;
            }
        }
        clicked
    }

    fn status_bar(&self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        ui.horizontal_wrapped(|ui| {
            if state.connections.is_empty() {
                ui.label(s.status_disconnected);
                return;
            }
            let connected: Vec<&Arc<ConnectionView>> = state
                .connections
                .iter()
                .filter(|c| c.status.is_connected())
                .collect();
            if let [only] = connected.as_slice()
                && state.connections.len() == 1
                && let ConnectionStatus::Connected { api_version } = &only.status
            {
                let version = format!("{}.{}", api_version.0, api_version.1);
                ui.label(fill(
                    s.status_connected,
                    &[("server", &only.server), ("version", &version)],
                ));
                return;
            }
            ui.label(fill(
                s.status_summary,
                &[("count", &connected.len().to_string())],
            ));
            for conn in state
                .connections
                .iter()
                .filter(|c| !c.status.is_connected())
            {
                ui.separator();
                status_line(ui, conn);
            }
        });
    }

    fn tree(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        ui.add_space(4.0);
        ui.strong(s.tree_heading);
        ui.separator();
        let saved: Vec<&SavedServer> = state
            .settings
            .servers
            .iter()
            .filter(|srv| state.connection_to(&srv.address).is_none())
            .collect();
        if state.connections.is_empty() && saved.is_empty() {
            ui.label(RichText::new(s.tree_empty).weak());
            return;
        }
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            for conn in &state.connections {
                if conn.model.pools.is_empty() {
                    self.connection_row(ui, conn);
                } else {
                    for pool in self.tree_for(conn).iter() {
                        self.pool_node(ui, state, conn, pool);
                    }
                }
            }
            for server in saved {
                self.saved_row(ui, server);
            }
        });
    }

    fn pool_node(&mut self, ui: &mut Ui, state: &AppState, conn: &ConnectionView, pool: &TreePool) {
        let id = ui.make_persistent_id(("pool", conn.id, pool.pool_ref.as_str()));
        CollapsingState::load_with_default_open(ui.ctx(), id, true)
            .show_header(ui, |ui| {
                icons::pool(ui);
                let response = self.tree_label(
                    ui,
                    Selection::Pool(conn.id, pool.pool_ref.clone()),
                    &pool.name,
                );
                if !conn.status.is_connected() {
                    ui.label(
                        RichText::new(i18n::connection_status_label(&conn.status))
                            .weak()
                            .italics(),
                    );
                }
                response.context_menu(|ui| self.connection_menu(ui, conn));
            })
            .body(|ui| {
                for host in &pool.hosts {
                    let id = ui.make_persistent_id(("host", conn.id, host.host_ref.as_str()));
                    CollapsingState::load_with_default_open(ui.ctx(), id, true)
                        .show_header(ui, |ui| {
                            icons::host(ui, host.enabled && conn.status.is_connected());
                            self.tree_label(
                                ui,
                                Selection::Host(conn.id, host.host_ref.clone()),
                                &host.name,
                            );
                        })
                        .body(|ui| {
                            for vm in &host.vms {
                                self.vm_row(ui, state, conn, vm);
                            }
                        });
                }
                for vm in &pool.vms {
                    self.vm_row(ui, state, conn, vm);
                }
            });
    }

    /// A connection with nothing loaded yet (connecting or failed).
    fn connection_row(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            icons::host(ui, false);
            let response = self.tree_label(ui, Selection::Connection(conn.id), &conn.server);
            if matches!(conn.status, ConnectionStatus::Connecting) {
                ui.spinner();
            }
            ui.label(
                RichText::new(i18n::connection_status_label(&conn.status))
                    .weak()
                    .italics(),
            );
            response.context_menu(|ui| self.connection_menu(ui, conn));
        });
    }

    /// A saved server that is not connected.
    fn saved_row(&mut self, ui: &mut Ui, server: &SavedServer) {
        let s = strings();
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            icons::host(ui, false);
            let response = self.tree_label(
                ui,
                Selection::Saved(server.address.clone()),
                &server.address,
            );
            ui.label(RichText::new(s.server_disconnected).weak().italics());
            if response.double_clicked() {
                self.actions.push(Action::Send(connect_saved(server)));
            }
            response.context_menu(|ui| {
                self.saved_buttons(ui, server);
            });
        });
    }

    fn connection_menu(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        if self.connection_buttons(ui, conn) {
            ui.close();
        }
    }

    /// Buttons for a connection's state; returns whether one was clicked.
    fn connection_buttons(&mut self, ui: &mut Ui, conn: &ConnectionView) -> bool {
        let s = strings();
        let mut clicked = false;
        match &conn.status {
            ConnectionStatus::Reconnecting { .. } => {
                if ui.button(s.server_retry_now).clicked() {
                    self.actions.push(Action::Send(Command::RetryNow {
                        connection: conn.id,
                    }));
                    clicked = true;
                }
            }
            ConnectionStatus::Failed { error } => {
                let needs_password = matches!(
                    **error,
                    ConnectionError::PasswordRequired | ConnectionError::PasswordUnavailable(_)
                ) || error
                    .xapi_error()
                    .is_some_and(|e| e.is_api(xapi::error::codes::SESSION_AUTHENTICATION_FAILED));
                if !needs_password && ui.button(s.server_connect).clicked() {
                    self.actions.push(Action::Send(Command::Connect {
                        server: conn.server.clone(),
                        username: conn.username.clone(),
                        password: None,
                        remember_password: true,
                    }));
                    clicked = true;
                }
                if ui.button(s.server_enter_password).clicked() {
                    self.actions.push(Action::OpenConnect {
                        server: conn.server.clone(),
                        username: conn.username.clone(),
                    });
                    clicked = true;
                }
            }
            _ => {}
        }
        let close_label = if conn.status.is_failed() {
            s.server_dismiss
        } else {
            s.server_disconnect
        };
        if ui.button(close_label).clicked() {
            self.actions.push(Action::Send(Command::Disconnect {
                connection: conn.id,
            }));
            clicked = true;
        }
        clicked
    }

    /// Buttons for a saved, disconnected server; returns whether one was
    /// clicked.
    fn saved_buttons(&mut self, ui: &mut Ui, server: &SavedServer) -> bool {
        let s = strings();
        let mut clicked = false;
        if ui.button(s.server_connect).clicked() {
            self.actions.push(Action::Send(connect_saved(server)));
            clicked = true;
        }
        if ui.button(s.server_enter_password).clicked() {
            self.actions.push(Action::OpenConnect {
                server: server.address.clone(),
                username: server.username.clone(),
            });
            clicked = true;
        }
        if ui.button(s.server_forget).clicked() {
            self.actions.push(Action::Send(Command::Forget {
                address: server.address.clone(),
            }));
            clicked = true;
        }
        if clicked {
            ui.close();
        }
        clicked
    }

    fn tree_label(&mut self, ui: &mut Ui, selection: Selection, name: &str) -> egui::Response {
        let selected = self.selection.as_ref() == Some(&selection);
        let response = ui.selectable_label(selected, name);
        if response.clicked() {
            self.selection = Some(selection);
            // The toolbar above was drawn with the old selection.
            ui.ctx().request_repaint();
        }
        response
    }

    fn vm_row(&mut self, ui: &mut Ui, _state: &AppState, conn: &ConnectionView, vm: &TreeVm) {
        ui.horizontal(|ui| {
            // Leaves line up with the labels of collapsible siblings.
            ui.add_space(ui.spacing().indent);
            icons::vm(ui, &vm.power_state);
            let response = self.tree_label(ui, Selection::Vm(conn.id, vm.vm_ref.clone()), &vm.name);
            if let Some(record) = conn.model.vms.get(&vm.vm_ref) {
                response.context_menu(|ui| {
                    if self.vm_action_buttons(ui, Some((conn, &vm.vm_ref, record))) {
                        ui.close();
                    }
                });
            }
        });
    }

    fn details(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        let selection = self.selection.clone();
        let conn = selection
            .as_ref()
            .and_then(Selection::connection)
            .and_then(|c| state.connection(c));
        if let (Some(conn), Some(Selection::Pool(..) | Selection::Host(..) | Selection::Vm(..))) =
            (conn, &selection)
            && !conn.status.is_connected()
        {
            ui.horizontal(|ui| {
                status_line(ui, conn);
            });
            ui.separator();
        }
        match (&selection, conn) {
            (Some(Selection::Vm(_, r)), Some(conn)) if conn.model.vms.contains_key(r) => {
                let vm = &conn.model.vms[r];
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
                        if let Some(host) = conn.model.hosts.get(&vm.resident_on) {
                            ui.label(s.details_type_host);
                            ui.label(&host.name_label);
                            ui.end_row();
                        }
                        row(ui, s.details_description, &vm.name_description);
                        row(ui, s.details_uuid, &vm.uuid);
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    self.vm_action_buttons(ui, Some((conn, r, vm)));
                    if conn.busy_vms.contains(r) {
                        ui.spinner();
                        ui.label(s.details_busy);
                    }
                });
            }
            (Some(Selection::Host(_, r)), Some(conn)) if conn.model.hosts.contains_key(r) => {
                host_details(ui, &conn.model, &conn.model.hosts[r]);
            }
            (Some(Selection::Pool(_, r)), Some(conn)) if conn.model.pools.contains_key(r) => {
                pool_details(ui, &conn.model, &conn.model.pools[r]);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    self.connection_buttons(ui, conn);
                });
            }
            (Some(Selection::Connection(_)), Some(conn)) => self.connection_details(ui, conn),
            (Some(Selection::Saved(address)), _) => {
                match state
                    .settings
                    .servers
                    .iter()
                    .find(|x| &x.address == address)
                {
                    Some(server) => {
                        let server = server.clone();
                        ui.horizontal(|ui| {
                            icons::host(ui, false);
                            ui.heading(&server.address);
                        });
                        ui.label(RichText::new(s.server_heading).weak());
                        ui.separator();
                        Grid::new("saved-details")
                            .num_columns(2)
                            .spacing([16.0, 6.0])
                            .show(ui, |ui| {
                                row(ui, s.server_status, s.server_disconnected);
                                row(ui, s.server_user, &server.username);
                            });
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            self.saved_buttons(ui, &server);
                        });
                    }
                    None => no_selection(ui, state),
                }
            }
            _ => no_selection(ui, state),
        }
    }

    fn connection_details(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = strings();
        ui.horizontal(|ui| {
            icons::host(ui, conn.status.is_connected());
            ui.heading(&conn.server);
        });
        ui.label(RichText::new(s.server_heading).weak());
        ui.separator();
        Grid::new("server-details")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                row(
                    ui,
                    s.server_status,
                    i18n::connection_status_label(&conn.status),
                );
                row(ui, s.server_user, &conn.username);
                match &conn.status {
                    ConnectionStatus::Failed { error }
                    | ConnectionStatus::Reconnecting { error, .. } => {
                        ui.label(s.server_error);
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            i18n::describe_connection_error(error),
                        );
                        ui.end_row();
                    }
                    _ => {}
                }
            });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if matches!(conn.status, ConnectionStatus::Connecting) {
                ui.spinner();
            }
            self.connection_buttons(ui, conn);
        });
    }

    // ---- windows ----------------------------------------------------------

    fn connect_window(&mut self, ctx: &Context, state: &AppState) {
        if !self.connect.open {
            return;
        }
        let s = strings();
        let saved = state
            .settings
            .server(self.connect.server.trim())
            .is_some_and(|srv| srv.username == self.connect.username.trim());
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
                        let mut field = TextEdit::singleline(&mut self.connect.password)
                            .password(true)
                            .desired_width(240.0);
                        if saved {
                            field = field.hint_text(s.connect_password_saved_hint);
                        }
                        let password = ui.add(field).labelled_by(label.id);
                        if password.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            submit = true;
                        }
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut self.connect.remember, s.connect_remember);
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
            let password = std::mem::take(&mut self.connect.password);
            let password = if password.is_empty() && saved {
                None
            } else {
                Some(password.into())
            };
            self.backend.send(Command::Connect {
                server: self.connect.server.trim().to_owned(),
                username: self.connect.username.trim().to_owned(),
                password,
                remember_password: self.connect.remember,
            });
            self.connect.open = false;
        } else if cancel || !open {
            self.connect.password.clear();
            self.connect.open = false;
        }
    }

    fn certificate_window(&mut self, ctx: &Context) {
        let Some(prompt) = self.cert_prompts.front() else {
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
            && let Some(prompt) = self.cert_prompts.pop_front()
        {
            if accept {
                prompt.accept();
            } else {
                prompt.reject();
            }
        }
    }

    fn options_window(&mut self, ctx: &Context, state: &AppState) {
        if !self.options_open {
            return;
        }
        let s = strings();
        let mut open = true;
        let mut close = false;
        let mut preferences = state.settings.preferences.clone();
        egui::Window::new(s.options_title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.checkbox(&mut preferences.save_passwords, s.options_save_passwords);
                ui.checkbox(&mut preferences.restore_connections, s.options_restore);
                ui.add_space(8.0);
                ui.separator();
                match &state.config {
                    Some(paths) => {
                        let mode = match paths.location {
                            ConfigLocation::User => s.options_mode_user,
                            ConfigLocation::Portable => s.options_mode_portable,
                            ConfigLocation::Override => s.options_mode_override,
                        };
                        Grid::new("options-paths")
                            .num_columns(2)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(s.options_settings_dir);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(paths.dir.display().to_string()).monospace(),
                                    );
                                    ui.label(RichText::new(mode).weak());
                                });
                                ui.end_row();
                                ui.label(s.options_log_dir);
                                ui.label(
                                    RichText::new(paths.log_dir().display().to_string())
                                        .monospace(),
                                );
                                ui.end_row();
                            });
                    }
                    None => {
                        ui.label(RichText::new(s.options_mode_memory).weak());
                    }
                }
                ui.add_space(8.0);
                if ui.button(s.close).clicked() {
                    close = true;
                }
            });
        if preferences != state.settings.preferences {
            self.backend
                .send(Command::SetPreferences(Preferences { ..preferences }));
        }
        if close || !open {
            self.options_open = false;
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
            .anchor(Align2::RIGHT_TOP, [-12.0, 60.0])
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
        self.tree_cache
            .retain(|id, _| state.connection(*id).is_some());
        let ctx = ui.ctx().clone();
        Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &state));
        Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &state));
        let mut moving = false;
        if self.events_open {
            Panel::bottom("events")
                .resizable(true)
                .default_size(190.0)
                .min_size(80.0)
                .show(ui, |ui| moving = events::show(ui, &state, &self.backend));
        }
        Panel::left("tree")
            .resizable(true)
            .default_size(280.0)
            .min_size(180.0)
            .show(ui, |ui| self.tree(ui, &state));
        CentralPanel::default().show(ui, |ui| self.details(ui, &state));
        self.connect_window(&ctx, &state);
        self.certificate_window(&ctx);
        self.options_window(&ctx, &state);
        self.about_window(&ctx);
        self.errors_window(&ctx);
        self.apply_actions(&state);

        // Countdowns and relative times move without backend updates.
        let reconnecting = state
            .connections
            .iter()
            .any(|c| matches!(c.status, ConnectionStatus::Reconnecting { .. }));
        if moving || reconnecting {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for prompt in self.cert_prompts.drain(..) {
            prompt.reject();
        }
        self.backend.shutdown_blocking(Duration::from_secs(3));
    }
}

/// Connects to a saved server with its saved password.
fn connect_saved(server: &SavedServer) -> Command {
    Command::Connect {
        server: server.address.clone(),
        username: server.username.clone(),
        password: None,
        remember_password: true,
    }
}

/// One line describing a connection that is not (fully) connected.
fn status_line(ui: &mut Ui, conn: &ConnectionView) {
    let s = strings();
    match &conn.status {
        ConnectionStatus::Connecting => {
            ui.spinner();
            ui.label(fill(s.status_connecting, &[("server", &conn.server)]));
        }
        ConnectionStatus::Connected { api_version } => {
            let version = format!("{}.{}", api_version.0, api_version.1);
            ui.label(fill(
                s.status_connected,
                &[("server", &conn.server), ("version", &version)],
            ));
        }
        ConnectionStatus::Reconnecting {
            attempt, retry_at, ..
        } => {
            let attempt = attempt.to_string();
            let text = match retry_at {
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    fill(
                        s.status_reconnect_wait,
                        &[
                            ("server", &conn.server),
                            ("seconds", &left.as_secs().max(1).to_string()),
                            ("attempt", &attempt),
                        ],
                    )
                }
                None => {
                    ui.spinner();
                    fill(
                        s.status_reconnecting,
                        &[("server", &conn.server), ("attempt", &attempt)],
                    )
                }
            };
            ui.colored_label(ui.visuals().warn_fg_color, text);
        }
        ConnectionStatus::Failed { error } => {
            let text = fill(
                s.status_failed,
                &[
                    ("server", &conn.server),
                    ("error", &i18n::describe_connection_error(error)),
                ],
            );
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
    }
}

fn no_selection(ui: &mut Ui, state: &AppState) {
    let s = strings();
    ui.centered_and_justified(|ui| {
        let hint = if state.connections.is_empty() && state.settings.servers.is_empty() {
            s.tree_empty
        } else {
            s.tree_no_selection
        };
        ui.label(RichText::new(hint).weak());
    });
}

fn host_details(ui: &mut Ui, model: &ConnectionModel, host: &xapi::HostRecord) {
    let s = strings();
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

fn pool_details(ui: &mut Ui, model: &ConnectionModel, pool: &xapi::PoolRecord) {
    let s = strings();
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
    use xen_core::{BackendOptions, MemorySecretStore, SecretStore, Settings, Store};

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

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn app_harness(runtime: &tokio::runtime::Runtime) -> Harness<'static, XenAdminApp> {
        app_harness_with(runtime, Arc::new(Store::default()))
    }

    /// The app over an in-memory store (no files, no OS keyring), with short
    /// timeouts.
    fn app_harness_with(
        runtime: &tokio::runtime::Runtime,
        store: Arc<Store>,
    ) -> Harness<'static, XenAdminApp> {
        let handle = runtime.handle().clone();
        Harness::builder()
            .with_size(egui::vec2(1100.0, 800.0))
            .build_eframe(move |cc| {
                let ctx = cc.egui_ctx.clone();
                let options = BackendOptions {
                    originator: "xenadmin-ui-test".into(),
                    store,
                    event_timeout: 5.0,
                    heartbeat_interval: Duration::from_millis(300),
                    heartbeat_timeout: Duration::from_millis(800),
                    reconnect_delay: Duration::from_millis(200),
                    max_reconnect_delay: Duration::from_millis(500),
                    task_poll_interval: Duration::from_millis(50),
                    ..Default::default()
                };
                XenAdminApp::new(BackendHandle::spawn(
                    &handle,
                    options,
                    Arc::new(move || ctx.request_repaint()),
                ))
            })
    }

    /// Fills in and submits the connect dialog that opens on start (user
    /// `root`, the dialog's default).
    fn connect_through_dialog(
        harness: &mut Harness<'_, XenAdminApp>,
        server: &str,
        password: &str,
    ) {
        let s = strings();
        harness.step();
        harness.get_by_label(s.connect_title);
        let server_field = harness.get_by_label(s.connect_server);
        assert!(server_field.is_focused());
        server_field.type_text(server);
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
            .type_text(password);
        harness.step();
        harness.get_by_label(s.connect_button).click();
    }

    #[test]
    fn connect_trust_and_operate_through_the_ui() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start(true));
        let mut harness = app_harness(&runtime);
        let s = strings();

        // The connect dialog opens on start with the server field focused.
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);

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

    /// The full real-shaped database (sanitized XCP-ng 8.3 capture): every
    /// class is loaded and the tree shows the pool, its host and its 10 guests.
    #[test]
    fn real_shaped_database_shows_the_tree() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start_with_records(
            false,
            xapi_mock::capture_records(),
        ));
        let mut harness = app_harness(&runtime);
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
        });
        harness.get_by_label("xcp-host-01");
        for i in 1..=10 {
            harness.get_by_label(&format!("guest-{i:02}"));
        }
        let state = harness.state().backend.state();
        let model = &state.connections[0].model;
        assert_eq!(model.srs.len(), 6);
        assert_eq!(model.vm_guest_metrics.len(), 10);
        assert_eq!(model.object_count(), 241);
    }

    /// Read-only check of the real window code against a real host. Ignored
    /// unless asked for:
    ///
    /// ```text
    /// XENADMIN_REAL_HOST=… XENADMIN_REAL_USER=root XENADMIN_REAL_PASSWORD=… \
    /// XENADMIN_REAL_FINGERPRINT=AA:BB:… \
    ///   cargo test -p xenadmin real_host -- --ignored --nocapture
    /// ```
    ///
    /// It types the address and credentials into the connect dialog, trusts
    /// the certificate only if it has the expected fingerprint, waits for the
    /// tree and for live `event.from` polling, and checks the tree against
    /// the backend's cache. It clicks no operation button and changes nothing
    /// on the host. Only counts are printed (no names).
    #[test]
    #[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables)"]
    fn real_host_tree_read_only() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let (host, user, password, fingerprint) = (
            var("XENADMIN_REAL_HOST"),
            var("XENADMIN_REAL_USER"),
            var("XENADMIN_REAL_PASSWORD"),
            var("XENADMIN_REAL_FINGERPRINT"),
        );
        let runtime = runtime();
        // Production timings (heartbeat every 15 s, event.from 60 s).
        let handle = runtime.handle().clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 800.0))
            .build_eframe(move |cc| {
                let ctx = cc.egui_ctx.clone();
                let options = BackendOptions {
                    originator: "xenadmin-ui-test".into(),
                    ..Default::default()
                };
                XenAdminApp::new(BackendHandle::spawn(
                    &handle,
                    options,
                    Arc::new(move || ctx.request_repaint()),
                ))
            });
        let s = strings();
        assert_eq!(
            user, "root",
            "the test types only the password; the dialog's user is root"
        );
        connect_through_dialog(&mut harness, &host, &password);
        drop(password);

        step_until(&mut harness, "certificate prompt", |h| {
            h.query_by_label(s.cert_trust).is_some()
        });
        // Trust only the expected certificate.
        harness.get_by_label(&fingerprint.to_uppercase());
        harness.get_by_label(s.cert_trust).click();

        step_until(&mut harness, "connected", |h| {
            h.state()
                .backend
                .state()
                .connections
                .first()
                .is_some_and(|c| c.status.is_connected() && c.model.object_count() > 0)
        });
        let state = harness.state().backend.state();
        let tree = state.connections[0].model.tree();
        assert_eq!(tree.len(), 1);
        harness.get_by_label(&tree[0].name);
        let mut guests = 0;
        for h in &tree[0].hosts {
            harness.get_by_label(&h.name);
            for vm in &h.vms {
                assert!(harness.query_by_label(&vm.name).is_some());
                guests += 1;
            }
        }
        for vm in &tree[0].vms {
            assert!(harness.query_by_label(&vm.name).is_some());
            guests += 1;
        }
        // Keep stepping for a while: the event loop must stay connected and
        // the heartbeat (host.get_servertime, every 15 s) must succeed.
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            harness.step();
            std::thread::sleep(Duration::from_millis(50));
        }
        let state = harness.state().backend.state();
        let conn = &state.connections[0];
        assert!(conn.status.is_connected(), "{:?}", conn.status);
        assert!(
            !state
                .operations
                .iter()
                .any(|o| o.kind == xen_core::OperationKind::Reconnect),
            "the connection dropped during the run"
        );
        let tasks = conn.model.tasks.len();
        println!(
            "real host: {} pool, {} host(s), {guests} guest VM(s) in the tree; {} cached objects; {tasks} server task(s) listed in Events; stayed connected 20 s (heartbeat included)",
            conn.model.pools.len(),
            conn.model.hosts.len(),
            conn.model.object_count()
        );
        for (class, count) in conn.model.class_counts() {
            if count > 0 {
                println!("  {class}: {count}");
            }
        }
        // Log out (the harness does not run `on_exit`).
        harness
            .state()
            .backend
            .shutdown_blocking(Duration::from_secs(5));
        println!("logged out");
    }

    fn first_conn(h: &Harness<'_, XenAdminApp>) -> Option<Arc<xen_core::ConnectionView>> {
        h.state().backend.state().connections.first().cloned()
    }

    /// A slow VM start shows in the Events panel with its progress and is
    /// cancelled from there; a server task of another client shows too.
    #[test]
    fn events_panel_follows_and_cancels_operations() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start(false));
        server.set_task_timing(100, Duration::from_millis(100));
        let mut harness = app_harness(&runtime);
        let s = strings();
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
        });
        harness.get_by_label(s.events_title);
        harness.get_by_label(&fill(s.op_connect, &[("server", &server.address())]));

        harness.get_by_label("db-01").click();
        harness.step();
        harness
            .get_all_by_label(s.action_start)
            .next()
            .unwrap()
            .click();
        let title = fill(
            s.op_vm_action,
            &[("action", s.action_start), ("vm", "db-01")],
        );
        step_until(&mut harness, "operation with cancel", |h| {
            h.query_by_label(&title).is_some() && h.query_by_label(s.events_cancel).is_some()
        });
        harness.get_by_label(s.events_cancel).click();
        step_until(&mut harness, "cancelled", |h| {
            h.query_by_label(s.op_cancelled).is_some()
        });
        assert_eq!(
            server.record("vm", "OpaqueRef:vm-db").unwrap()["power_state"],
            "Halted"
        );

        // Another client's task: listed with a Cancel button.
        let task = server.start_task("Async.VM.export");
        step_until(&mut harness, "server task", |h| {
            h.query_by_label("Async.VM.export").is_some()
                && h.query_by_label(s.events_server_tasks).is_some()
        });
        harness.get_by_label(s.events_cancel).click();
        step_until(&mut harness, "task cancelled", |h| {
            h.query_by_label(s.task_cancelled).is_some()
        });
        assert_eq!(server.record("task", &task).unwrap()["status"], "cancelled");

        // Clear Finished empties the operation list.
        harness.get_by_label(s.events_clear).click();
        step_until(&mut harness, "cleared", |h| {
            h.state().backend.state().operations.is_empty()
        });
    }

    /// The server restarts: the window shows the reconnection and then the
    /// live tree again, without any user action.
    #[test]
    fn reconnection_after_a_server_restart_shows_in_the_window() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start(false));
        let mut harness = app_harness(&runtime);
        let s = strings();
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
        });

        server.stop();
        step_until(&mut harness, "reconnecting", |h| {
            first_conn(h).is_some_and(|c| {
                matches!(c.status, xen_core::ConnectionStatus::Reconnecting { .. })
            }) && h.query_by_label(s.server_reconnecting).is_some()
        });
        // The last tree stays visible meanwhile.
        harness.get_by_label("Lab Pool");
        harness.get_by_label(&fill(s.op_reconnect, &[("server", &server.address())]));

        runtime.block_on(server.restart());
        step_until(&mut harness, "reconnected", |h| {
            first_conn(h).is_some_and(|c| c.status.is_connected())
                && h.query_by_label(s.server_reconnecting).is_none()
        });
        server.set_name_label("vm", "OpaqueRef:vm-build", "after-restart");
        step_until(&mut harness, "live again", |h| {
            h.query_by_label("after-restart").is_some()
        });
    }

    /// A saved server with a saved password reconnects on start: no connect
    /// dialog, no password typed. Options change the preferences.
    #[test]
    fn saved_servers_reconnect_on_start_without_a_password() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start(false));
        let secrets = Arc::new(MemorySecretStore::new());
        secrets
            .set(&format!("root@{}", server.address()), xapi_mock::PASSWORD)
            .unwrap();
        let settings = Settings {
            servers: vec![xen_core::SavedServer {
                address: server.address(),
                username: "root".into(),
                reconnect: true,
            }],
            ..Settings::default()
        };
        let store = Arc::new(Store::in_memory_with(settings, secrets));
        let mut harness = app_harness_with(&runtime, store);
        let s = strings();
        harness.step();
        assert!(harness.query_by_label(s.connect_title).is_none());
        step_until(&mut harness, "restored", |h| {
            h.query_by_label("Lab Pool").is_some()
        });

        // Disconnect: the server stays in the tree as a saved server.
        harness.get_by_label("Lab Pool").click();
        harness.step();
        harness.get_by_label(s.server_disconnect).click();
        step_until(&mut harness, "saved row", |h| {
            h.query_by_label(s.server_disconnected).is_some()
                && h.state().backend.state().connections.is_empty()
        });
        assert!(!harness.state().backend.state().settings.servers[0].reconnect);

        // Options: the preference is saved through the backend.
        harness.get_by_label(s.menu_tools).click();
        step_until(&mut harness, "tools menu", |h| {
            h.query_by_label(s.menu_options).is_some()
        });
        harness.get_by_label(s.menu_options).click();
        step_until(&mut harness, "options", |h| {
            h.query_by_label(s.options_restore).is_some()
        });
        harness.get_by_label(s.options_restore).click();
        step_until(&mut harness, "preference saved", |h| {
            !h.state()
                .backend
                .state()
                .settings
                .preferences
                .restore_connections
        });
        harness.get_by_label(s.options_mode_memory);
    }
}
