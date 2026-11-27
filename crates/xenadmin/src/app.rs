//! Main window.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::collapsing_header::CollapsingState;
use egui::{
    Align2, Button, CentralPanel, Context, Grid, Key, Panel, RichText, ScrollArea, TextEdit, Ui,
};
use xapi::{Host, HostRecord, Pool, Ref, Sr, Vm, VmPowerState, VmRecord};
use xen_core::console::ConsoleTarget;
use xen_core::model::{self, ConnectionModel, TreeHost, TreeOptions, TreePool, TreeSr, TreeVm};
use xen_core::objects::{self, VmKind};
use xen_core::{
    AppState, BackendHandle, CertificatePrompt, Command, ConfigLocation, ConnectionError,
    ConnectionId, ConnectionStatus, ConnectionView, Notification, Preferences, SavedServer,
};

use crate::alerts_view::{AlertAction, AlertsView};
use crate::branding;
use crate::console_view::{ConsoleKey, Consoles, TabAction};
use crate::events;
use crate::i18n::{self, fill, strings};
use crate::icons::{self, HostState};
use crate::net_ui::NetUi;
use crate::ops_strings::ops_strings;
use crate::ops_ui::{self, Entry, OpsUi};
use crate::perf_strings::perf_strings;
use crate::perf_view::{PerfAction, PerfKey, Performance};
use crate::perfmon_ui::{self, PerfmonDialog};
use crate::pool_strings::pool_strings;
use crate::pool_ui::{self, PoolUi};
use crate::prov_strings::prov_strings;
use crate::provision_ui::{self, DialogRequest, ProvisionUi};
use crate::sr_strings::sr_strings;
use crate::storage_ui::{self, StorageUi};
use crate::tab_strings::tab_strings;
use crate::tabs::{self, Subject, Tab};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selection {
    /// A connection without a loaded pool (connecting, or failed).
    Connection(ConnectionId),
    /// A saved server that is not connected.
    Saved(String),
    Pool(ConnectionId, Ref<Pool>),
    Host(ConnectionId, Ref<Host>),
    Vm(ConnectionId, Ref<Vm>),
    Sr(ConnectionId, Ref<Sr>),
    /// F13: objects only the organization views show.
    Network(ConnectionId, Ref<xapi::Network>),
    Vdi(ConnectionId, Ref<xapi::Vdi>),
    VApp(ConnectionId, Ref<xapi::VmAppliance>),
    /// A folder, by path (folders are shared by every connection).
    Folder(String),
    /// A group of a view or saved search (a tag, a custom field value…).
    Group(Box<xen_core::search_eval::GroupTag>),
    /// The root of an organization view.
    ViewRoot(xen_core::search_eval::OrgView),
    /// The root of a saved search, by UUID.
    SavedSearch(String),
}

impl Selection {
    fn connection(&self) -> Option<ConnectionId> {
        match self {
            Self::Connection(c)
            | Self::Pool(c, _)
            | Self::Host(c, _)
            | Self::Vm(c, _)
            | Self::Sr(c, _)
            | Self::Network(c, _)
            | Self::Vdi(c, _)
            | Self::VApp(c, _) => Some(*c),
            Self::Saved(_)
            | Self::Folder(_)
            | Self::Group(_)
            | Self::ViewRoot(_)
            | Self::SavedSearch(_) => None,
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
    about: crate::about::About,
    options_open: bool,
    events_open: bool,
    errors: Vec<String>,
    /// Tree derived from each connection's last model snapshot and the view
    /// options (rebuilt only when either changes).
    tree_cache: HashMap<ConnectionId, TreeCacheEntry>,
    /// The tree's name filter.
    tree_filter: String,
    /// The detail tab last chosen; kept while moving between objects that
    /// have it, like the original.
    tab: Tab,
    actions: Vec<Action>,
    /// VM and host operations: menus, confirmations, role elevation.
    ops: OpsUi,
    /// Open consoles (docked, undocked, full screen) and their options.
    consoles: Consoles,
    /// VMs started or resumed from this window: the Console tab is shown
    /// once they run, if still selected.
    console_on_start: Vec<(ConnectionId, Ref<Vm>)>,
    /// Open performance feeds and the graphs' zoom.
    performance: Performance,
    /// The Alerts view of the notifications area.
    alerts: AlertsView,
    /// Which view the notifications area shows.
    notifications: Notifications,
    /// The performance alert settings window.
    perfmon: Option<PerfmonDialog>,
    /// VM creation, templates, snapshots and disks: the wizard, dialogs
    /// and tab selections.
    provision: ProvisionUi,
    /// Storage repositories: the New SR wizard and the SR dialogs.
    storage: StorageUi,
    /// Networking: the New Network wizard, the networking dialogs and the
    /// selections of the Networking and NICs tabs.
    network: NetUi,
    /// Pools and server settings: New Pool, Add Server, the Properties
    /// dialog and the server dialogs.
    pool: PoolUi,
    /// High availability and Active Directory: the HA wizard and settings,
    /// the HA and Users tabs.
    ha: crate::ha_ui::HaUi,
    /// XVA export and import: the wizards and the Export to File dialog.
    xva: crate::xva_ui::XvaUi,
    migrate: crate::migrate_ui::MigrateUi,
    vapp: crate::vapp_ui::VAppUi,
    vmss: crate::vmss_ui::VmssUi,
    dr: crate::dr_ui::DrUi,
    /// Navigation views, the Search tab, folders, tags, custom fields and
    /// saved searches (F13).
    org: crate::org_ui::OrgUi,
    /// GPUs, USB devices, vTPMs, memory settings and the external consoles
    /// (F14a).
    dev: crate::dev_ui::DevUi,
    /// The selection of the last frame (an imported search is dropped when
    /// it changes).
    last_selection: Option<Selection>,
    /// The theme and language last applied from the preferences.
    applied_theme: Option<xen_core::ThemeSetting>,
    applied_language: Option<String>,
}

/// The views of the notifications area (the original's Alerts and Events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notifications {
    Alerts,
    Events,
}

type TreeCacheEntry = (Arc<ConnectionModel>, TreeOptions, Arc<Vec<TreePool>>);

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
            about: crate::about::About::default(),
            options_open: false,
            events_open: true,
            errors: Vec::new(),
            tree_cache: HashMap::new(),
            tree_filter: String::new(),
            tab: Tab::General,
            actions: Vec::new(),
            ops: OpsUi::default(),
            consoles: Consoles::new(),
            console_on_start: Vec::new(),
            performance: Performance::default(),
            alerts: AlertsView::default(),
            notifications: Notifications::Events,
            perfmon: None,
            provision: ProvisionUi::default(),
            storage: StorageUi::default(),
            network: NetUi::default(),
            pool: PoolUi::default(),
            ha: crate::ha_ui::HaUi::default(),
            xva: crate::xva_ui::XvaUi::default(),
            migrate: crate::migrate_ui::MigrateUi::default(),
            vapp: crate::vapp_ui::VAppUi::default(),
            vmss: crate::vmss_ui::VmssUi::default(),
            dr: crate::dr_ui::DrUi::default(),
            org: crate::org_ui::OrgUi::default(),
            dev: crate::dev_ui::DevUi::default(),
            last_selection: None,
            applied_theme: None,
            applied_language: None,
        };
        if !restoring {
            app.open_connect_dialog(&state, None);
        }
        app
    }

    fn drain_notifications(&mut self) {
        let state = self.backend.state();
        while let Some(notification) = self.backend.try_recv() {
            let Some(notification) = self
                .ops
                .on_notification(notification, &state, &self.backend)
            else {
                continue;
            };
            match notification {
                Notification::CertificatePrompt(prompt) => self.cert_prompts.push_back(prompt),
                Notification::ConnectionRefused { server, error } => {
                    self.errors.push(fill(
                        strings().err_connection_refused,
                        &[
                            ("server", &server),
                            ("error", &i18n::describe_connection_error(&error)),
                        ],
                    ));
                }
                // The New SR wizard's probes, the pool dialogs' questions.
                Notification::Precheck { id, result, .. } => {
                    if !self.dev.on_precheck(&state, id, &result)
                        && !self.org.on_device_precheck(id, &result)
                        && !self.ha.on_precheck(&state, id, &result)
                        && !self.pool.on_precheck(id, &result)
                        && !self.migrate.on_precheck(id, &result)
                        && !self.vmss.on_precheck(id, &result)
                        && !self.dr.on_precheck(id, &result)
                    {
                        self.storage.on_precheck(&state, id, result);
                    }
                }
                Notification::ReconnectFailed { server } => {
                    self.errors.push(fill(
                        pool_strings().reconnect_failed,
                        &[("server", &server)],
                    ));
                }
                // Taken by `OpsUi`.
                Notification::ActionFailed { .. }
                | Notification::Elevation { .. }
                | Notification::HaNtolRaise { .. } => {}
            }
        }
        self.errors.append(&mut self.ops.errors);
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

    /// The console of the selected VM or host, if it has one.
    fn selected_console(&self, state: &AppState) -> Option<ConsoleKey> {
        match self.selection.as_ref()? {
            Selection::Vm(c, r) => {
                let vm = state.connection(*c)?.model.get(r)?;
                (objects::vm_kind(vm) == VmKind::Guest).then(|| (*c, ConsoleTarget::Vm(r.clone())))
            }
            Selection::Host(c, r) => Some((*c, ConsoleTarget::Host(r.clone()))),
            _ => None,
        }
    }

    /// Closes consoles no longer shown, and shows the Console tab of a VM
    /// started from this window once it runs (if it is still selected).
    fn update_consoles(&mut self, state: &AppState) {
        let selected = self.selected_console(state);
        self.consoles
            .retain(selected.as_ref(), |c| state.connection(c).is_some());
        self.console_on_start.append(&mut self.ops.started);
        let running = |c: ConnectionId, r: &Ref<Vm>| {
            state
                .connection(c)
                .and_then(|v| v.model.get(r))
                .map(|vm| vm.power_state == VmPowerState::Running)
        };
        let mut switch = false;
        self.console_on_start.retain(|(c, r)| match running(*c, r) {
            Some(true) => {
                switch |= self.selection == Some(Selection::Vm(*c, r.clone()));
                false
            }
            Some(false) => true,
            None => false,
        });
        if switch {
            self.tab = Tab::Console;
        }
    }

    /// The performance feed of the selected host or VM, if it can have one.
    fn selected_perf(&self, state: &AppState) -> Option<PerfKey> {
        match self.selection.as_ref()? {
            Selection::Vm(c, r) => {
                let vm = state.connection(*c)?.model.get(r)?;
                (objects::vm_kind(vm) == VmKind::Guest)
                    .then(|| (*c, xen_core::PerfTarget::Vm(r.clone())))
            }
            Selection::Host(c, r) => Some((*c, xen_core::PerfTarget::Host(r.clone()))),
            _ => None,
        }
    }

    /// Opens the performance alert settings of `object`.
    fn edit_alerts(
        &mut self,
        state: &AppState,
        connection: ConnectionId,
        object: xen_core::PerfmonObjectRef,
    ) {
        if let Some(conn) = state.connection(connection) {
            self.perfmon = PerfmonDialog::open(conn, object);
        }
    }

    /// The notifications area: Alerts and Events.
    fn notifications(&mut self, ui: &mut Ui, state: &AppState) -> bool {
        let p = perf_strings();
        let count = AlertsView::count(state);
        let alerts_title = if count > 0 {
            fill(p.alerts_unread, &[("count", &count.to_string())])
        } else {
            p.alerts_title.to_owned()
        };
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.notifications == Notifications::Alerts, alerts_title)
                .clicked()
            {
                self.notifications = Notifications::Alerts;
            }
            if ui
                .selectable_label(
                    self.notifications == Notifications::Events,
                    strings().events_title,
                )
                .clicked()
            {
                self.notifications = Notifications::Events;
            }
        });
        match self.notifications {
            Notifications::Events => events::show(ui, state, &self.backend),
            Notifications::Alerts => {
                if let Some(AlertAction::EditAlerts(c, object)) = self.alerts.show(ui, state) {
                    self.edit_alerts(state, c, object);
                }
                false
            }
        }
    }

    fn tree_for(&mut self, conn: &ConnectionView, options: &TreeOptions) -> Arc<Vec<TreePool>> {
        match self.tree_cache.get(&conn.id) {
            Some((cached, cached_options, tree))
                if Arc::ptr_eq(cached, &conn.model) && cached_options == options =>
            {
                tree.clone()
            }
            _ => {
                let tree = Arc::new(conn.model.tree(options));
                self.tree_cache
                    .insert(conn.id, (conn.model.clone(), options.clone(), tree.clone()));
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

    fn selected_host<'a>(
        &self,
        state: &'a AppState,
    ) -> Option<(&'a ConnectionView, Ref<Host>, &'a Arc<HostRecord>)> {
        match &self.selection {
            Some(Selection::Host(c, r)) => {
                let conn = state.connection(*c)?;
                conn.model.hosts.get(r).map(|rec| (&**conn, r.clone(), rec))
            }
            _ => None,
        }
    }

    /// Buttons for operation entries (toolbar and details header).
    fn op_buttons(&mut self, ui: &mut Ui, conn: &ConnectionView, entries: Vec<Entry>) {
        for entry in entries {
            if let Entry::Item {
                label,
                enabled,
                request,
            } = entry
                && ui.add_enabled(enabled, Button::new(label)).clicked()
            {
                self.ops.request(conn, request, &self.backend);
            }
        }
    }

    /// The selected connection, if connected.
    fn selected_connection<'a>(&self, state: &'a AppState) -> Option<&'a ConnectionView> {
        let c = self.selection.as_ref().and_then(Selection::connection)?;
        state.connection(c).map(|c| &**c)
    }

    /// What New VM starts from: the selected template or snapshot, and the
    /// selected host as home server.
    fn new_vm_request(&self, state: &AppState) -> DialogRequest {
        let template = self
            .selected_vm(state)
            .and_then(|(_, r, vm)| xen_core::provisioning::can_create_from(vm).then_some(r));
        let affinity = self.selected_host(state).map(|(_, r, _)| r);
        DialogRequest::NewVm { template, affinity }
    }

    /// The toolbar buttons of the selected VM or host.
    fn selection_buttons(&mut self, ui: &mut Ui, state: &AppState) {
        if let Some(conn) = self.selected_connection(state) {
            let entry = provision_ui::new_vm_entry(conn, None);
            let enabled = matches!(entry, Entry::Dialog { enabled: true, .. });
            if ui
                .add_enabled(enabled, Button::new(prov_strings().new_vm_toolbar))
                .clicked()
            {
                let request = self.new_vm_request(state);
                self.provision.open(conn, request);
            }
            let entry = storage_ui::new_sr_entry(conn);
            let enabled = matches!(entry, Entry::Dialog { enabled: true, .. });
            if ui
                .add_enabled(enabled, Button::new(sr_strings().new_storage_toolbar))
                .clicked()
            {
                self.storage
                    .open(conn, DialogRequest::NewSr { reattach: None });
            }
            ui.separator();
        }
        if let Some((conn, r, vm)) = self.selected_vm(state)
            && model::is_guest(vm)
        {
            let entries = ops_ui::vm_toolbar(conn, &r, vm);
            self.op_buttons(ui, conn, entries);
        } else if let Some((conn, r, host)) = self.selected_host(state) {
            let entries = ops_ui::host_toolbar(conn, &r, host);
            self.op_buttons(ui, conn, entries);
        } else {
            // Nothing to act on: the VM buttons, disabled.
            let s = strings();
            for label in [s.action_start, s.action_clean_shutdown] {
                ui.add_enabled(false, Button::new(label));
            }
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
                let any_connected = state.connections.iter().any(|c| c.status.is_connected());
                if ui
                    .add_enabled(
                        any_connected,
                        Button::new(crate::xva_strings::xva_strings().menu_import),
                    )
                    .clicked()
                {
                    // `ImportCommand` with the selection as the home server.
                    let target = match &self.selection {
                        Some(Selection::Host(c, h)) => Some((*c, Some(h.clone()))),
                        _ => selected_conn.map(|c| (c, None)),
                    };
                    self.xva.open_import(target);
                    ui.close();
                }
                ui.separator();
                if ui.button(s.menu_quit).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(s.menu_view, |ui| {
                ui.checkbox(&mut self.events_open, s.menu_events);
                ui.separator();
                let t = tab_strings();
                let mut prefs = state.settings.preferences.clone();
                ui.checkbox(&mut prefs.show_user_templates, t.view_custom_templates);
                ui.checkbox(&mut prefs.show_default_templates, t.view_default_templates);
                ui.checkbox(&mut prefs.show_local_storage, t.view_local_storage);
                ui.checkbox(&mut prefs.show_snapshots, t.view_snapshots);
                ui.checkbox(&mut prefs.show_hidden_objects, t.view_hidden);
                if prefs != state.settings.preferences {
                    self.actions
                        .push(Action::Send(Command::SetPreferences(prefs)));
                }
            });
            ui.menu_button(pool_strings().menu_pool, |ui| {
                let conn = self
                    .selected_connection(state)
                    .or_else(|| state.connections.first().map(|c| &**c));
                let Some(conn) = conn else {
                    ui.add_enabled(false, Button::new(ops_strings().host_menu_empty));
                    return;
                };
                let entries = pool_ui::pool_entries(state, conn);
                self.ops.menu(ui, conn, None, entries, false, &self.backend);
            });
            ui.menu_button(s.menu_vm, |ui| {
                let Some(conn) = self.selected_connection(state) else {
                    ui.add_enabled(false, Button::new(ops_strings().host_menu_empty));
                    return;
                };
                let mut entries = vec![provision_ui::new_vm_entry(conn, None)];
                let mut vm_arg = None;
                if let Some((_, r, vm)) = self.selected_vm(state) {
                    entries.push(Entry::Separator);
                    if model::is_guest(vm) {
                        entries.extend(ops_ui::vm_entries(conn, &r, vm));
                        entries.extend(provision_ui::vm_entries(conn, &r, vm));
                        entries.push(crate::xva_ui::export_entry(
                            state,
                            conn,
                            std::slice::from_ref(&r),
                            false,
                        ));
                        if xen_core::devices::vtpm_supported(&conn.model) {
                            let can = xen_core::devices::vtpm_command(&conn.model, vm);
                            entries.push(Entry::Separator);
                            entries.push(Entry::Named {
                                label: crate::dev_strings::dev_strings().vtpm_menu.to_owned(),
                                enabled: can.is_ok(),
                                hint: can
                                    .err()
                                    .and_then(crate::dev_strings::vtpm_unavailable_text)
                                    .map(str::to_owned),
                                dialog: crate::provision_ui::DialogRequest::ManageVtpm(r.clone()),
                            });
                        }
                    } else {
                        entries.extend(provision_ui::template_entries(conn, &r, vm));
                    }
                    vm_arg = Some((r, vm.clone()));
                }
                let vm_arg = vm_arg.as_ref().map(|(r, v)| (r, &**v));
                self.ops
                    .menu(ui, conn, vm_arg, entries, false, &self.backend);
            });
            ui.menu_button(ops_strings().menu_server, |ui| {
                match self.selected_host(state) {
                    Some((conn, r, host)) => {
                        let mut entries = ops_ui::host_entries(conn, &r, host);
                        entries.extend(pool_ui::host_entries(state, conn, &r, host));
                        self.ops.menu(ui, conn, None, entries, false, &self.backend);
                    }
                    None => {
                        ui.add_enabled(false, Button::new(ops_strings().host_menu_empty));
                    }
                }
            });
            ui.menu_button(sr_strings().menu_storage, |ui| {
                let Some(conn) = self.selected_connection(state) else {
                    ui.add_enabled(false, Button::new(ops_strings().host_menu_empty));
                    return;
                };
                let mut entries = vec![storage_ui::new_sr_entry(conn)];
                if let Some(Selection::Sr(_, r)) = &self.selection
                    && let Some(sr) = conn.model.srs.get(r)
                {
                    entries.push(Entry::Separator);
                    entries.extend(storage_ui::sr_entries(conn, r, sr));
                }
                self.ops.menu(ui, conn, None, entries, false, &self.backend);
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
                    self.about.open = true;
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
            self.selection_buttons(ui, state);
        });
        ui.add_space(2.0);
    }

    fn status_bar(&self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        if let Some(hint) = &self.org.drop_hint
            && ui.ctx().dragged_id().is_some()
        {
            ui.label(hint);
            return;
        }
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
        let t = tab_strings();
        Panel::bottom("nav-buttons").show(ui, |ui| {
            if self.org.nav_buttons(ui, state) {
                self.selection = None;
                self.org.imported = None;
            }
        });
        ui.add_space(4.0);
        ui.strong(s.tree_heading);
        ui.horizontal(|ui| {
            let label = ui.label(RichText::new(t.tree_filter_label).weak());
            let clear = !self.tree_filter.is_empty();
            let width = ui.available_width() - if clear { 28.0 } else { 0.0 };
            ui.add(
                TextEdit::singleline(&mut self.tree_filter)
                    .hint_text(t.tree_filter)
                    .desired_width(width),
            )
            .labelled_by(label.id);
            if clear
                && ui
                    .small_button("x")
                    .on_hover_text(t.tree_filter_clear)
                    .clicked()
            {
                self.tree_filter.clear();
            }
        });
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
        if self.org.mode != crate::org_ui::NavMode::Infrastructure {
            let filter = self.tree_filter.clone();
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                self.org.tree(
                    ui,
                    state,
                    &mut self.selection,
                    &filter,
                    &mut self.ops,
                    &self.backend,
                );
            });
            return;
        }
        let options = state.settings.preferences.tree_options(&self.tree_filter);
        let needle = self.tree_filter.trim().to_lowercase();
        let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
        let filtering = !needle.is_empty();
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let mut shown = 0;
            for conn in &state.connections {
                if conn.model.pools.is_empty() {
                    if matches(&conn.server) {
                        self.connection_row(ui, conn);
                        shown += 1;
                    }
                } else {
                    for pool in self.tree_for(conn, &options).iter() {
                        self.pool_node(ui, state, conn, pool, filtering);
                        shown += 1;
                    }
                }
            }
            for server in saved.into_iter().filter(|srv| matches(&srv.address)) {
                self.saved_row(ui, server);
                shown += 1;
            }
            if shown == 0 {
                ui.label(RichText::new(t.tree_no_match).weak());
            }
        });
    }

    fn pool_node(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        pool: &TreePool,
        filtering: bool,
    ) {
        let id = ui.make_persistent_id(("pool", conn.id, pool.pool_ref.as_str()));
        let mut node = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if filtering {
            node.set_open(true);
        }
        node.show_header(ui, |ui| {
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
            response.context_menu(|ui| {
                if conn.status.is_connected() {
                    // `Helpers.PoolHasEnabledHosts`.
                    let host_ok = conn
                        .model
                        .hosts
                        .values()
                        .any(|h| h.enabled && objects::host_is_live(&conn.model, h));
                    let mut entries = vec![
                        provision_ui::new_vm_entry(conn, None),
                        storage_ui::new_sr_entry(conn),
                        crate::xva_ui::import_entry(conn, None),
                        crate::xva_ui::export_entry(state, conn, &[], host_ok),
                        Entry::Separator,
                    ];
                    entries.extend(pool_ui::pool_entries(state, conn));
                    self.ops.menu(ui, conn, None, entries, true, &self.backend);
                    ui.separator();
                }
                self.connection_menu(ui, conn);
            });
        })
        .body(|ui| {
            for host in &pool.hosts {
                self.host_node(ui, state, conn, host, filtering);
            }
            for vm in &pool.vms {
                self.vm_node(ui, state, conn, vm, filtering);
            }
            for sr in &pool.srs {
                self.sr_row(ui, conn, sr);
            }
            for vm in &pool.templates {
                self.vm_node(ui, state, conn, vm, filtering);
            }
        });
    }

    fn host_node(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        host: &TreeHost,
        filtering: bool,
    ) {
        let icon = if !conn.status.is_connected() || !host.live {
            HostState::Disconnected
        } else if host.maintenance {
            HostState::Maintenance
        } else {
            HostState::Connected
        };
        let selection = Selection::Host(conn.id, host.host_ref.clone());
        if host.vms.is_empty() && host.srs.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                icons::host_state(ui, icon);
                let response = self.tree_label(ui, selection, &host.name);
                self.host_drop(ui, &response, state, conn, &host.host_ref);
                self.host_context_menu(&response, conn, &host.host_ref);
            });
            return;
        }
        let id = ui.make_persistent_id(("host", conn.id, host.host_ref.as_str()));
        let mut node = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if filtering {
            node.set_open(true);
        }
        node.show_header(ui, |ui| {
            icons::host_state(ui, icon);
            let response = self.tree_label(ui, selection, &host.name);
            self.host_drop(ui, &response, state, conn, &host.host_ref);
            self.host_context_menu(&response, conn, &host.host_ref);
        })
        .body(|ui| {
            for vm in &host.vms {
                self.vm_node(ui, state, conn, vm, filtering);
            }
            for sr in &host.srs {
                self.sr_row(ui, conn, sr);
            }
        });
    }

    fn sr_row(&mut self, ui: &mut Ui, conn: &ConnectionView, sr: &TreeSr) {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            icons::sr(ui, sr.icon);
            let response = self.tree_label(ui, Selection::Sr(conn.id, sr.sr_ref.clone()), &sr.name);
            if let Some(record) = conn.model.srs.get(&sr.sr_ref) {
                response.context_menu(|ui| {
                    let entries = storage_ui::sr_entries(conn, &sr.sr_ref, record);
                    self.ops.menu(ui, conn, None, entries, true, &self.backend);
                });
            }
        });
    }

    /// A VM, template or snapshot; a VM with shown snapshots can be expanded.
    fn vm_node(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        vm: &TreeVm,
        filtering: bool,
    ) {
        if vm.snapshots.is_empty() {
            self.vm_row(ui, state, conn, vm);
            return;
        }
        let id = ui.make_persistent_id(("vm", conn.id, vm.vm_ref.as_str()));
        let mut node = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if filtering {
            node.set_open(true);
        }
        node.show_header(ui, |ui| {
            icons::vm_state(ui, vm.icon);
            let response = self.tree_label(ui, Selection::Vm(conn.id, vm.vm_ref.clone()), &vm.name);
            self.vm_drag(ui, &response, state, conn, vm);
            self.vm_context_menu(&response, conn, vm);
        })
        .body(|ui| {
            for snapshot in &vm.snapshots {
                self.vm_row(ui, state, conn, snapshot);
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

    fn vm_row(&mut self, ui: &mut Ui, state: &AppState, conn: &ConnectionView, vm: &TreeVm) {
        ui.horizontal(|ui| {
            // Leaves line up with the labels of collapsible siblings.
            ui.add_space(ui.spacing().indent);
            icons::vm_state(ui, vm.icon);
            let response = self.tree_label(ui, Selection::Vm(conn.id, vm.vm_ref.clone()), &vm.name);
            self.vm_drag(ui, &response, state, conn, vm);
            self.vm_context_menu(&response, conn, vm);
        });
    }

    /// A VM row can be dragged onto a server (`DragDropMigrateVMCommand`).
    fn vm_drag(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        state: &AppState,
        conn: &ConnectionView,
        vm: &TreeVm,
    ) {
        let dragged = xen_core::search_eval::ObjKey::Vm(conn.id, vm.vm_ref.clone());
        crate::org_ui::dnd(
            ui,
            response,
            state,
            Some(dragged),
            None,
            &mut self.ops,
            &self.backend,
            &mut self.org.drop_hint,
        );
    }

    /// A server row takes VMs dropped on it (live migration in the pool).
    fn host_drop(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        state: &AppState,
        conn: &ConnectionView,
        host: &Ref<Host>,
    ) {
        let target = crate::org_ui::DropTarget::Host(conn.id, host.clone());
        crate::org_ui::dnd(
            ui,
            response,
            state,
            None,
            Some(target),
            &mut self.ops,
            &self.backend,
            &mut self.org.drop_hint,
        );
    }

    /// The operations a guest offers now, on its tree row.
    fn vm_context_menu(&mut self, response: &egui::Response, conn: &ConnectionView, vm: &TreeVm) {
        if let Some(record) = conn.model.vms.get(&vm.vm_ref) {
            response.context_menu(|ui| {
                let entries = if model::is_guest(record) {
                    let mut e = ops_ui::vm_entries(conn, &vm.vm_ref, record);
                    e.extend(provision_ui::vm_entries(conn, &vm.vm_ref, record));
                    e.push(crate::xva_ui::export_entry(
                        &self.backend.state(),
                        conn,
                        std::slice::from_ref(&vm.vm_ref),
                        false,
                    ));
                    e
                } else {
                    provision_ui::template_entries(conn, &vm.vm_ref, record)
                };
                self.ops.menu(
                    ui,
                    conn,
                    Some((&vm.vm_ref, record)),
                    entries,
                    true,
                    &self.backend,
                );
                ui.separator();
                if ui
                    .button(crate::org_strings::org_strings().properties)
                    .clicked()
                {
                    self.org.vm_properties =
                        crate::org_ui::VmPropertiesDialog::open(conn, vm.vm_ref.clone());
                    ui.close();
                }
            });
        }
    }

    /// The operations a host offers now, on its tree row.
    fn host_context_menu(
        &mut self,
        response: &egui::Response,
        conn: &ConnectionView,
        host: &Ref<Host>,
    ) {
        if let Some(record) = conn.model.hosts.get(host) {
            response.context_menu(|ui| {
                let state = self.backend.state();
                let host_ok = record.enabled && objects::host_is_live(&conn.model, record);
                let mut entries = vec![
                    provision_ui::new_vm_entry(conn, Some(host.clone())),
                    storage_ui::new_sr_entry(conn),
                    crate::xva_ui::import_entry(conn, Some(host.clone())),
                    crate::xva_ui::export_entry(&state, conn, &[], host_ok),
                    Entry::Separator,
                ];
                entries.extend(ops_ui::host_entries(conn, host, record));
                entries.extend(pool_ui::host_entries(
                    &self.backend.state(),
                    conn,
                    host,
                    record,
                ));
                self.ops.menu(ui, conn, None, entries, true, &self.backend);
            });
        }
    }

    fn details(&mut self, ui: &mut Ui, state: &AppState) {
        let selection = self.selection.clone();
        let conn = selection
            .as_ref()
            .and_then(Selection::connection)
            .and_then(|c| state.connection(c));
        let object = matches!(
            selection,
            Some(Selection::Pool(..) | Selection::Host(..) | Selection::Vm(..) | Selection::Sr(..))
        );
        if let Some(conn) = conn
            && object
            && !conn.status.is_connected()
        {
            ui.horizontal(|ui| {
                status_line(ui, conn);
            });
            ui.separator();
        }
        let model = conn.map(|c| c.model.clone());
        let subject = match (&selection, &model) {
            (Some(Selection::Vm(_, r)), Some(m)) => m.vms.get(r).map(|v| Subject::Vm(r, v)),
            (Some(Selection::Host(_, r)), Some(m)) => m.hosts.get(r).map(|h| Subject::Host(r, h)),
            (Some(Selection::Pool(_, r)), Some(m)) => m.pools.get(r).map(|p| Subject::Pool(p)),
            (Some(Selection::Sr(_, r)), Some(m)) => m.srs.get(r).map(|x| Subject::Sr(r, x)),
            _ => None,
        };
        if let Some(sel) = &selection
            && matches!(
                sel,
                Selection::Network(..)
                    | Selection::Vdi(..)
                    | Selection::VApp(..)
                    | Selection::Folder(_)
                    | Selection::Group(_)
                    | Selection::ViewRoot(_)
                    | Selection::SavedSearch(_)
            )
        {
            if let Some(next) = self.org.details(ui, state, sel, &self.backend) {
                self.tab = Tab::General;
                self.selection = Some(next);
            }
            return;
        }
        match (&selection, conn, subject) {
            (_, Some(conn), Some(subject)) => self.object_details(ui, state, conn, subject),
            (Some(Selection::Connection(_)), Some(conn), _) => self.connection_details(ui, conn),
            (Some(Selection::Saved(address)), _, _) => self.saved_details(ui, state, address),
            _ => self.welcome(ui, state),
        }
    }

    /// The home page, shown when nothing is selected: a native page in place
    /// of the original's web page (`TabPages/HomePage`), whose one action
    /// was "Add a Server".
    fn welcome(&mut self, ui: &mut Ui, state: &AppState) {
        let s = strings();
        let mut add = false;
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.25).clamp(16.0, 160.0));
            ui.label(
                RichText::new(branding::PRODUCT_NAME)
                    .heading()
                    .size(28.0)
                    .strong(),
            );
            ui.label(RichText::new(branding::SUBTITLE).italics());
            ui.add_space(4.0);
            ui.label(fill(
                s.welcome_blurb,
                &[("platform", branding::PLATFORM_NAME)],
            ));
            ui.add_space(18.0);
            add = ui
                .add(
                    egui::Button::new(RichText::new(s.welcome_add_server).size(16.0))
                        .min_size(egui::vec2(180.0, 36.0)),
                )
                .clicked();
            ui.add_space(18.0);
            let saved = state.settings.servers.len();
            if saved > 0 {
                let connected = state
                    .settings
                    .servers
                    .iter()
                    .filter(|server| {
                        state
                            .connections
                            .iter()
                            .any(|c| c.server == server.address && c.status.is_connected())
                    })
                    .count();
                ui.label(fill(
                    s.welcome_servers,
                    &[
                        ("connected", &connected.to_string()),
                        ("saved", &saved.to_string()),
                    ],
                ));
            }
            // The tree says how to connect when there is nothing yet.
            if saved > 0 || !state.connections.is_empty() {
                ui.label(RichText::new(s.tree_no_selection).weak());
            }
        });
        if add {
            self.open_connect_dialog(state, None);
        }
    }

    fn saved_details(&mut self, ui: &mut Ui, state: &AppState, address: &str) {
        let s = strings();
        let Some(server) = state
            .settings
            .servers
            .iter()
            .find(|x| x.address == address)
            .cloned()
        else {
            self.welcome(ui, state);
            return;
        };
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

    /// Header, actions and tabs of a pool, host, VM or SR.
    fn object_details(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        subject: Subject<'_>,
    ) {
        let s = strings();
        let t = tab_strings();
        let model = &conn.model;
        ui.horizontal(|ui| match subject {
            Subject::Pool(pool) => {
                icons::pool(ui);
                ui.heading(tabs::pool_display_name(model, pool));
            }
            Subject::Host(_, host) => {
                let state = if !conn.status.is_connected() || !objects::host_is_live(model, host) {
                    HostState::Disconnected
                } else if objects::host_in_maintenance(host) {
                    HostState::Maintenance
                } else {
                    HostState::Connected
                };
                icons::host_state(ui, state);
                ui.heading(&host.name_label);
            }
            Subject::Vm(_, vm) => {
                icons::vm_state(ui, objects::vm_icon(vm));
                ui.heading(tabs::vm_display_name(vm));
            }
            Subject::Sr(r, sr) => {
                icons::sr(ui, objects::sr_icon(model, r, sr));
                ui.heading(&sr.name_label);
            }
        });
        ui.horizontal(|ui| {
            match subject {
                Subject::Pool(..) => {
                    ui.label(RichText::new(s.details_type_pool).weak());
                }
                Subject::Host(..) => {
                    ui.label(RichText::new(s.details_type_host).weak());
                }
                Subject::Sr(..) => {
                    ui.label(RichText::new(t.type_sr).weak());
                }
                Subject::Vm(_, vm) => {
                    let kind = match objects::vm_kind(vm) {
                        VmKind::Snapshot => t.type_snapshot,
                        VmKind::UserTemplate | VmKind::DefaultTemplate => t.type_template,
                        _ => s.details_type_vm,
                    };
                    ui.label(RichText::new(kind).weak());
                    if !vm.is_a_template {
                        ui.label(RichText::new("·").weak());
                        ui.colored_label(
                            icons::power_color(ui, &vm.power_state),
                            power_label(&vm.power_state),
                        );
                    }
                }
            }
            ui.add_space(16.0);
            match subject {
                Subject::Vm(r, vm) if model::is_guest(vm) => {
                    let entries = ops_ui::vm_toolbar(conn, r, vm);
                    self.op_buttons(ui, conn, entries);
                    if ui
                        .button(crate::org_strings::org_strings().properties)
                        .clicked()
                    {
                        self.org.vm_properties =
                            crate::org_ui::VmPropertiesDialog::open(conn, r.clone());
                    }
                    if conn.busy_vms.contains(r) {
                        ui.spinner();
                        ui.label(s.details_busy);
                    }
                }
                Subject::Host(r, host) => {
                    let entries = ops_ui::host_toolbar(conn, r, host);
                    self.op_buttons(ui, conn, entries);
                    if conn.busy_hosts.contains(r) {
                        ui.spinner();
                        ui.label(s.details_busy);
                    }
                }
                Subject::Pool(..) => {
                    self.connection_buttons(ui, conn);
                }
                Subject::Sr(r, _) => {
                    if ui.button(perf_strings().alert_settings).clicked() {
                        self.perfmon =
                            PerfmonDialog::open(conn, xen_core::PerfmonObjectRef::Sr(r.clone()));
                    }
                    if ui.button(sr_strings().properties).clicked() {
                        self.storage
                            .open(conn, DialogRequest::SrProperties(r.clone()));
                    }
                    if conn.busy_srs.contains(r) {
                        ui.spinner();
                        ui.label(s.details_busy);
                    }
                }
                _ => {}
            }
        });
        ui.separator();

        let tabs = tabs::tabs_for(model, subject);
        if !tabs.contains(&self.tab) {
            self.tab = Tab::General;
        }
        ui.horizontal(|ui| {
            for tab in &tabs {
                if ui.selectable_label(self.tab == *tab, tab.title()).clicked() {
                    self.tab = *tab;
                }
            }
        });
        ui.separator();
        if self.tab == Tab::Performance {
            let key = match subject {
                Subject::Vm(r, _) => (conn.id, xen_core::PerfTarget::Vm(r.clone())),
                Subject::Host(r, _) => (conn.id, xen_core::PerfTarget::Host(r.clone())),
                _ => return,
            };
            if let Some(PerfAction::EditAlerts) =
                self.performance.tab(ui, &self.backend, conn, &key)
            {
                let object = match &key.1 {
                    xen_core::PerfTarget::Vm(r) => xen_core::PerfmonObjectRef::Vm(r.clone()),
                    xen_core::PerfTarget::Host(r) => xen_core::PerfmonObjectRef::Host(r.clone()),
                };
                self.perfmon = PerfmonDialog::open(conn, object);
            }
            return;
        }
        if self.tab == Tab::Search {
            let w = crate::search_ui::world(state);
            let object = match subject {
                Subject::Pool(_) => model
                    .pool()
                    .map(|(r, _)| xen_core::search_eval::ObjKey::Pool(conn.id, r.clone())),
                Subject::Host(r, _) => {
                    Some(xen_core::search_eval::ObjKey::Host(conn.id, r.clone()))
                }
                _ => None,
            };
            let (base, title) = xen_core::search_eval::search_for(&w, object.as_ref());
            let title = match title {
                xen_core::search_eval::SearchTitle::ObjectOverview(n) => fill(
                    crate::org_strings::org_strings().search_object_overview,
                    &[("name", &n)],
                ),
                xen_core::search_eval::SearchTitle::Folder(n) => n,
                xen_core::search_eval::SearchTitle::Overview => {
                    crate::org_strings::org_strings().search_overview.to_owned()
                }
            };
            let (base, title) = match self.org.imported.clone() {
                Some(imported) => {
                    let name = imported.name.clone();
                    (imported, name)
                }
                None => (base, title),
            };
            drop(w);
            let action = self
                .org
                .search
                .show(ui, state, &self.backend, &base, &title);
            if let Some(next) = self.org.on_search_action(action) {
                self.tab = Tab::General;
                self.selection = Some(next);
            }
            return;
        }
        if self.tab == Tab::Snapshots {
            if let Subject::Vm(r, vm) = subject {
                ui.label(
                    RichText::new(tabs::page_title(self.tab, subject))
                        .strong()
                        .size(15.0),
                );
                ui.add_space(4.0);
                self.provision.snapshots_tab(ui, conn, r, vm);
            }
            return;
        }
        if self.tab == Tab::Ha {
            let hidden = state.settings.preferences.show_hidden_objects;
            ScrollArea::vertical()
                .id_salt("ha-tab-scroll")
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.ha
                        .ha_tab(ui, state, conn, &mut self.ops, &self.backend, hidden);
                });
            return;
        }
        if self.tab == Tab::Users {
            ScrollArea::vertical()
                .id_salt("users-tab-scroll")
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.ha.ad.users_tab(ui, state, conn, &self.backend);
                });
            return;
        }
        if matches!(self.tab, Tab::Gpu | Tab::Usb) {
            ui.label(
                RichText::new(tabs::page_title(self.tab, subject))
                    .strong()
                    .size(15.0),
            );
            ui.add_space(4.0);
            ScrollArea::vertical()
                .id_salt("devices-tab-scroll")
                .auto_shrink(false)
                .show(ui, |ui| match (self.tab, subject) {
                    (Tab::Gpu, Subject::Host(r, _)) => self.dev.gpu_tab(ui, conn, Some(r)),
                    (Tab::Gpu, _) => self.dev.gpu_tab(ui, conn, None),
                    (Tab::Usb, Subject::Host(_, host)) => {
                        self.dev
                            .usb_tab(ui, conn, host, &mut self.ops, &self.backend)
                    }
                    _ => {}
                });
            return;
        }
        if self.tab == Tab::Console {
            let external = ui
                .horizontal(|ui| {
                    let r = match subject {
                        Subject::Vm(_, vm) => {
                            crate::dev_ui::console_buttons(ui, conn, Some(vm), None)
                        }
                        Subject::Host(r, _) => {
                            crate::dev_ui::console_buttons(ui, conn, None, Some(r))
                        }
                        _ => None,
                    };
                    if let Some(note) = &self.dev.console_note {
                        ui.label(RichText::new(note).weak());
                    }
                    r
                })
                .inner;
            if let Some(request) = external {
                let vm = match subject {
                    Subject::Vm(r, _) => Some(r),
                    _ => None,
                };
                self.dev
                    .console_request(state, conn, vm, request, &mut self.ops, &self.backend);
            }
            let (key, title) = match subject {
                Subject::Vm(r, vm) => (
                    (conn.id, ConsoleTarget::Vm(r.clone())),
                    tabs::vm_display_name(vm),
                ),
                Subject::Host(r, host) => (
                    (conn.id, ConsoleTarget::Host(r.clone())),
                    host.name_label.clone(),
                ),
                _ => return,
            };
            if let Some(TabAction::Vm(action)) =
                self.consoles.tab(ui, &self.backend, conn, &key, &title)
                && let ConsoleTarget::Vm(vm) = &key.1
            {
                let request = xen_core::ActionRequest::Vm {
                    vm: vm.clone(),
                    action,
                };
                self.ops.request(conn, request, &self.backend);
            }
            return;
        }
        ui.label(
            RichText::new(tabs::page_title(self.tab, subject))
                .strong()
                .size(15.0),
        );
        ui.add_space(4.0);

        let ctx = tabs::Ctx {
            model,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            show_hidden: state.settings.preferences.show_hidden_objects,
        };
        if let (Tab::VmStorage, Subject::Vm(r, vm)) = (self.tab, subject) {
            ScrollArea::vertical()
                .id_salt("vm-storage-scroll")
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.provision.vm_storage_tab(
                        ui,
                        conn,
                        r,
                        vm,
                        ctx,
                        &mut self.ops,
                        &self.backend,
                    );
                });
            return;
        }
        let kind = match subject {
            Subject::Pool(..) => "pool",
            Subject::Host(..) => "host",
            Subject::Vm(..) => "vm",
            Subject::Sr(..) => "sr",
        };
        ScrollArea::vertical()
            .id_salt("details-scroll")
            .auto_shrink(false)
            .show(ui, |ui| match (self.tab, subject) {
                (Tab::General, Subject::Vm(_, vm)) => {
                    let secs = tabs::general::with_custom_fields(
                        tabs::general::vm_sections(ctx, vm),
                        model,
                        &vm.other_config,
                    );
                    tabs::render::sections(ui, kind, &secs);
                }
                (Tab::General, Subject::Host(r, host)) => {
                    let secs = tabs::general::with_custom_fields(
                        tabs::general::host_sections(ctx, r, host),
                        model,
                        &host.other_config,
                    );
                    tabs::render::sections(ui, kind, &secs);
                }
                (Tab::General, Subject::Pool(pool)) => {
                    let secs = tabs::general::with_custom_fields(
                        tabs::general::pool_sections(ctx, pool),
                        model,
                        &pool.other_config,
                    );
                    tabs::render::sections(ui, kind, &secs);
                }
                (Tab::General, Subject::Sr(_, sr)) => {
                    let secs = tabs::general::with_custom_fields(
                        tabs::general::sr_sections(ctx, sr),
                        model,
                        &sr.other_config,
                    );
                    tabs::render::sections(ui, kind, &secs);
                }
                (Tab::Memory, _) => {
                    if let Some(vms) =
                        tabs::memory::show(ui, &tabs::memory::memory_rows(ctx, subject))
                    {
                        self.dev.open(conn, crate::dev_ui::memory_request(vms));
                    }
                }
                (Tab::SrStorage, Subject::Sr(r, sr)) => {
                    ui.horizontal(|ui| {
                        self.storage
                            .rescan_button(ui, conn, r, sr, &mut self.ops, &self.backend);
                    });
                    ui.add_space(4.0);
                    tabs::render::table(ui, "sr-disks", &tabs::storage::sr_disks(ctx, sr));
                }
                (Tab::PhysicalStorage, Subject::Host(r, _)) => {
                    tabs::render::table(ui, "srs", &tabs::storage::physical_storage(ctx, Some(r)));
                }
                (Tab::PhysicalStorage, _) => {
                    tabs::render::table(ui, "srs", &tabs::storage::physical_storage(ctx, None));
                }
                (Tab::Networking, Subject::Vm(r, vm)) => {
                    self.network
                        .vm_tab(ui, conn, r, vm, ctx, &mut self.ops, &self.backend);
                }
                (Tab::Networking, subject) => {
                    let host = match subject {
                        Subject::Host(r, _) => Some(r),
                        _ => None,
                    };
                    self.network
                        .host_tab(ui, conn, host, ctx, &mut self.ops, &self.backend);
                }
                (Tab::Nics, Subject::Host(r, _)) => {
                    self.network
                        .nics_tab(ui, conn, r, ctx, &mut self.ops, &self.backend);
                }
                _ => {}
            });
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

    /// Applies the theme and language of the preferences when they change.
    fn apply_appearance(&mut self, ctx: &Context, preferences: &Preferences) {
        if self.applied_theme != Some(preferences.theme) {
            ctx.set_theme(crate::theme::preference(preferences.theme));
            self.applied_theme = Some(preferences.theme);
        }
        if self.applied_language.as_deref() != Some(preferences.language.as_str()) {
            crate::locale::apply_preference(&preferences.language);
            self.applied_language = Some(preferences.language.clone());
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
                appearance_options(ui, &mut preferences);
                ui.add_space(8.0);
                ui.separator();
                crate::dev_ui::external_tools_options(ui, &mut preferences.external_tools);
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

/// Options > Appearance: the theme and the language.
fn appearance_options(ui: &mut Ui, preferences: &mut Preferences) {
    use xen_core::ThemeSetting;
    let s = strings();
    ui.label(RichText::new(s.options_appearance).strong());
    Grid::new("options-appearance")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(s.options_theme);
            ui.horizontal(|ui| {
                for (value, label) in [
                    (ThemeSetting::System, s.options_theme_system),
                    (ThemeSetting::Light, s.options_theme_light),
                    (ThemeSetting::Dark, s.options_theme_dark),
                ] {
                    ui.radio_value(&mut preferences.theme, value, label);
                }
            });
            ui.end_row();
            let label = ui.label(s.options_language);
            let system = crate::locale::resolve("", crate::locale::system_languages());
            let system_name = crate::locale::LOCALES
                .iter()
                .find(|l| l.tag == system)
                .map_or(system, |l| l.name);
            let follow = fill(s.options_language_system, &[("language", system_name)]);
            let selected = crate::locale::LOCALES
                .iter()
                .find(|l| l.tag == preferences.language)
                .map_or(follow.clone(), |l| l.name.to_owned());
            egui::ComboBox::from_id_salt("options-language")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut preferences.language, String::new(), follow);
                    for l in crate::locale::LOCALES {
                        ui.selectable_value(&mut preferences.language, l.tag.to_owned(), l.name);
                    }
                })
                .response
                .labelled_by(label.id);
            ui.end_row();
        });
}

impl eframe::App for XenAdminApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.drain_notifications();
        let state = self.backend.state();
        self.tree_cache
            .retain(|id, _| state.connection(*id).is_some());
        let ctx = ui.ctx().clone();
        self.apply_appearance(&ctx, &state.settings.preferences);
        self.update_consoles(&state);
        let shown_perf = self.selected_perf(&state);
        self.performance
            .retain(shown_perf.as_ref(), |c| state.connection(c).is_some());
        let titles = |key: &ConsoleKey| console_title(&state, key);
        self.consoles.windows(&ctx, titles);
        if self.consoles.fullscreen_ui(ui) {
            self.ops.windows(&ctx, &state, &self.backend);
            self.apply_actions(&state);
            return;
        }
        if self.selection != self.last_selection {
            self.org.imported = None;
            self.last_selection = self.selection.clone();
        }
        Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &state));
        Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &state));
        let mut moving = false;
        if self.events_open {
            Panel::bottom("events")
                .resizable(true)
                .default_size(190.0)
                .min_size(80.0)
                .show(ui, |ui| moving = self.notifications(ui, &state));
        }
        Panel::left("tree")
            .resizable(true)
            .default_size(280.0)
            .min_size(180.0)
            .show(ui, |ui| self.tree(ui, &state));
        CentralPanel::default().show(ui, |ui| self.details(ui, &state));
        // Uptimes on the General tab move on their own.
        ctx.request_repaint_after(Duration::from_secs(30));
        self.connect_window(&ctx, &state);
        self.certificate_window(&ctx);
        self.options_window(&ctx, &state);
        self.about.show(&ctx);
        self.alerts
            .window(&ctx, &state, &mut self.ops, &self.backend);
        perfmon_ui::window(
            &mut self.perfmon,
            &ctx,
            &state,
            &mut self.ops,
            &self.backend,
        );
        self.ops.windows(&ctx, &state, &self.backend);
        for (c, request) in std::mem::take(&mut self.ops.dialog_requests) {
            if let Some(conn) = state.connection(c) {
                if crate::migrate_ui::takes(conn, &request) {
                    self.migrate.open(conn, request);
                } else if request.is_device() {
                    self.dev.open(conn, request);
                } else if request.is_vapp() {
                    self.vapp.open(conn, request, &mut self.ops, &self.backend);
                } else if request.is_vmss() {
                    self.vmss.open(conn, request, &mut self.ops, &self.backend);
                } else if request.is_dr() {
                    self.dr.open(conn, request, &self.backend);
                } else if request.is_ha() {
                    let hidden = state.settings.preferences.show_hidden_objects;
                    self.ha
                        .open(conn, request, &mut self.ops, &self.backend, hidden);
                } else if request.is_pool() {
                    self.pool.open(&state, conn, request, &self.backend);
                } else if request.is_storage() {
                    self.storage.open(conn, request);
                } else if request.is_transfer() {
                    self.xva.open(conn, request);
                } else {
                    self.provision.open(conn, request);
                }
            }
        }
        self.provision
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.provision.errors);
        self.storage
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.storage.errors);
        self.network
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.network.errors);
        self.pool
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.pool.errors);
        self.ha.windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.ha.errors);
        for (c, request) in std::mem::take(&mut self.provision.forwarded) {
            if let Some(conn) = state.connection(c) {
                if request.is_vmss() {
                    self.vmss.open(conn, request, &mut self.ops, &self.backend);
                } else {
                    self.xva.open(conn, request);
                }
            }
        }
        self.migrate
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.migrate.errors);
        self.vapp
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.vapp.errors);
        self.vmss
            .windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.vmss.errors);
        self.dr.windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.dr.errors);
        for (c, request) in std::mem::take(&mut self.vapp.forwarded) {
            if let Some(conn) = state.connection(c) {
                self.xva.open(conn, request);
            }
        }
        self.xva.windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.xva.errors);
        if let Some(d) = self
            .pool
            .take_org_dialog()
            .or_else(|| self.storage.take_org_dialog())
        {
            self.org.dialog = Some(d);
        }
        self.org.windows(&ctx, &state, &mut self.ops, &self.backend);
        self.org.search.end_frame();
        self.dev.windows(&ctx, &state, &mut self.ops, &self.backend);
        self.errors.append(&mut self.dev.errors);
        if let Some((c, target)) = self.dev.open_gpu_properties.take()
            && let Some(conn) = state.connection(c)
        {
            self.pool.open(
                &state,
                conn,
                crate::provision_ui::DialogRequest::Properties(target),
                &self.backend,
            );
            self.pool
                .select_properties_page(xen_core::properties::Page::Gpu);
        }
        if std::mem::take(&mut self.dev.open_options) {
            self.options_open = true;
        }
        if std::mem::take(&mut self.pool.open_connect) {
            self.open_connect_dialog(&state, None);
        }
        if let Some((c, vm)) = self.provision.take_created(&state) {
            self.selection = Some(Selection::Vm(c, vm));
        }
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
        // Consoles close with their handles.
        self.consoles = Consoles::new();
        for prompt in self.cert_prompts.drain(..) {
            prompt.reject();
        }
        self.backend.shutdown_blocking(Duration::from_secs(3));
    }
}

/// The name shown on an undocked console's window.
fn console_title(state: &AppState, key: &ConsoleKey) -> String {
    let Some(conn) = state.connection(key.0) else {
        return String::new();
    };
    match &key.1 {
        ConsoleTarget::Vm(r) => conn
            .model
            .get(r)
            .map(|vm| tabs::vm_display_name(vm))
            .unwrap_or_default(),
        ConsoleTarget::Host(r) => conn
            .model
            .get(r)
            .map(|h| h.name_label.clone())
            .unwrap_or_default(),
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

    // F5: the Console tab (`app/tests/console.rs`).
    mod console;
    // F6: the Performance tab, the Alerts view and the alert settings
    // (`app/tests/performance.rs`).
    mod performance;
    // F7: VM creation, templates, snapshots and disks
    // (`app/tests/provisioning.rs`), and on a real host
    // (`app/tests/provisioning_real.rs`).
    mod provisioning;
    mod provisioning_real;
    // F8: storage repositories (`app/tests/storage.rs`), and the real
    // host's SRs read only (`app/tests/storage_real.rs`).
    mod storage;
    mod storage_real;
    // F9: networking (`app/tests/network.rs`), and the real host's networks
    // read only plus one VM's interfaces (`app/tests/network_real.rs`).
    mod network;
    mod network_real;
    // F10a: pools, the pool and server Properties and the server dialogs
    // (`app/tests/pool.rs`), and the real host's pool and server settings
    // read only (`app/tests/pool_real.rs`).
    mod pool;
    mod pool_real;
    // F10b: HA and AD/RBAC (`app/tests/ha.rs`), and the real host's HA and
    // Users tabs read only (`app/tests/ha_real.rs`).
    mod ha;
    mod ha_real;
    // F11a: XVA export and import (`app/tests/xva.rs`), and the round trip
    // of a disposable test VM on the real host (`app/tests/xva_real.rs`).
    mod xva;
    mod xva_real;
    // F11b: OVF/OVA export and import and disk image import
    // (`app/tests/ovf.rs`), and on a real host (`app/tests/ovf_real.rs`).
    mod ovf;
    mod ovf_real;
    // F12a: the Migrate / Move / Copy wizard between two mock pools and
    // vApps (`app/tests/migrate.rs`), and the real host's vApp and Copy
    // wizard on a disposable test VM (`app/tests/migrate_real.rs`).
    mod migrate;
    mod migrate_real;
    // F12b: scheduled snapshots and disaster recovery between two mock
    // pools (`app/tests/vmss_dr.rs`), and the real host's snapshot
    // schedule on a disposable test VM with DR read only
    // (`app/tests/vmss_dr_real.rs`).
    mod vmss_dr;
    mod vmss_dr_real;
    // F13: views, search, folders, tags, custom fields, drag and drop.
    mod org;
    mod org_real;
    // F14a: GPUs, USB, vTPMs, memory settings and the external consoles
    // (`app/tests/devices.rs`), and on a real host
    // (`app/tests/devices_real.rs`).
    mod devices;
    mod devices_real;
    // F14b: themes and the message catalogue on a real host
    // (`app/tests/appearance_real.rs`).
    mod appearance_real;
    // The README screenshots, from the mock (`app/tests/screenshots.rs`;
    // feature `readme-screenshots`).
    #[cfg(feature = "readme-screenshots")]
    mod screenshots;

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
                    perf_interval: Duration::from_millis(200),
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
        // The original asks first.
        let o = ops_strings();
        step_until(&mut harness, "confirmation", |h| {
            h.query_by_label(o.confirm_shutdown_vm).is_some()
        });
        harness.get_by_label(o.yes).click();
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
        assert_eq!(about_item, "About ReXen Center");
        harness.get_by_label(branding::PRODUCT_NAME);
        harness.get_by_label("XCP-ng Center, born again in Rust");
        harness.get_by_label(&crate::about::version_line());
        harness.get_by_label(s.about_disclaimer);
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

    /// Connects the real window code to a real host, read-only: types the
    /// address and credentials into the connect dialog, trusts the
    /// certificate only if it has the expected fingerprint and waits for the
    /// cache to load. Production timings (heartbeat every 15 s, event.from
    /// 60 s).
    fn real_host_harness(runtime: &tokio::runtime::Runtime) -> Harness<'static, XenAdminApp> {
        real_host_harness_with(runtime, |o| o)
    }

    /// [`real_host_harness`] with changed backend options.
    fn real_host_harness_with(
        runtime: &tokio::runtime::Runtime,
        tweak: impl FnOnce(BackendOptions) -> BackendOptions + Send + 'static,
    ) -> Harness<'static, XenAdminApp> {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let (host, user, password, fingerprint) = (
            var("XENADMIN_REAL_HOST"),
            var("XENADMIN_REAL_USER"),
            var("XENADMIN_REAL_PASSWORD"),
            var("XENADMIN_REAL_FINGERPRINT"),
        );
        let handle = runtime.handle().clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 800.0))
            .build_eframe(move |cc| {
                let ctx = cc.egui_ctx.clone();
                let options = tweak(BackendOptions {
                    originator: "xenadmin-ui-test".into(),
                    ..Default::default()
                });
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

        // A real host may take longer than the mock to load its cache.
        let connected = step_for(&mut harness, Duration::from_secs(90), |h| {
            h.state()
                .backend
                .state()
                .connections
                .first()
                .is_some_and(|c| c.status.is_connected() && c.model.object_count() > 0)
        });
        assert!(connected, "timed out waiting for connected");
        harness
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
    /// It waits for the tree and for live `event.from` polling, and checks
    /// the tree against the backend's cache. It clicks no operation button
    /// and changes nothing on the host. Only counts are printed (no names).
    #[test]
    #[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables)"]
    fn real_host_tree_read_only() {
        let runtime = runtime();
        let mut harness = real_host_harness(&runtime);
        let state = harness.state().backend.state();
        let tree = state.connections[0]
            .model
            .tree(&xen_core::TreeOptions::default());
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

    fn sections_json(sections: &[tabs::Section]) -> serde_json::Value {
        serde_json::Value::Object(
            sections
                .iter()
                .map(|sec| {
                    let rows = sec
                        .rows
                        .iter()
                        .map(|r| (r.label.clone(), serde_json::json!(r.value)))
                        .collect();
                    (sec.title.to_owned(), serde_json::Value::Object(rows))
                })
                .collect(),
        )
    }

    fn table_json(table: &tabs::Table) -> serde_json::Value {
        serde_json::Value::Array(
            table
                .rows
                .iter()
                .map(|r| {
                    serde_json::Value::Object(
                        table
                            .columns
                            .iter()
                            .zip(&r.cells)
                            .map(|(c, v)| ((*c).to_owned(), serde_json::json!(v)))
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    fn memory_json(rows: &[tabs::memory::MemoryRow]) -> serde_json::Value {
        use tabs::memory::MemoryRow;
        serde_json::Value::Array(
            rows.iter()
                .map(|row| match row {
                    MemoryRow::Host {
                        name,
                        values,
                        percent,
                        ..
                    } => serde_json::json!({
                        "host": name,
                        "values": values.iter().map(|(l, v)| (l.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
                        "percent": percent,
                    }),
                    MemoryRow::Vms { names, values, .. } => serde_json::json!({
                        "vms": names,
                        "values": values.iter().map(|(l, v)| (l.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
                    }),
                })
                .collect(),
        )
    }

    /// Read-only acceptance check of the detail tabs against a real host:
    /// selects the pool, its coordinator, one VM (`XENADMIN_REAL_VM`, a
    /// UUID, or the first running guest) and one SR (`XENADMIN_REAL_SR`, or
    /// the pool's default SR), draws every tab of each in the real window,
    /// and writes what the tabs show to the JSON file `XENADMIN_REAL_DUMP`
    /// for comparison with `xe *-param-list`. It clicks no operation button
    /// and changes nothing on the host; nothing is printed but counts.
    #[test]
    #[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables)"]
    fn real_host_tabs_read_only() {
        let dump_path = std::env::var("XENADMIN_REAL_DUMP").expect("set XENADMIN_REAL_DUMP");
        let runtime = runtime();
        let mut harness = real_host_harness(&runtime);
        let state = harness.state().backend.state();
        let conn = state.connections[0].clone();
        let m = conn.model.clone();
        let (pool_ref, pool) = m.pool().map(|(r, p)| (r.clone(), p.clone())).unwrap();
        let host_ref = pool.master.clone();
        let host = m.hosts[&host_ref].clone();
        let vm_ref = match std::env::var("XENADMIN_REAL_VM") {
            Ok(uuid) => m
                .vms
                .iter()
                .find(|(_, v)| v.uuid == uuid)
                .map(|(r, _)| r.clone()),
            Err(_) => {
                let mut running: Vec<_> = m
                    .vms
                    .iter()
                    .filter(|(_, v)| model::is_guest(v) && v.power_state == VmPowerState::Running)
                    .collect();
                running.sort_by(|a, b| a.1.uuid.cmp(&b.1.uuid));
                running.first().map(|(r, _)| (*r).clone())
            }
        }
        .expect("a VM");
        let vm = m.vms[&vm_ref].clone();
        let sr_ref = match std::env::var("XENADMIN_REAL_SR") {
            Ok(uuid) => m
                .srs
                .iter()
                .find(|(_, x)| x.uuid == uuid)
                .map(|(r, _)| r.clone()),
            Err(_) => pool.default_sr.non_null().cloned(),
        }
        .expect("an SR");
        let sr = m.srs[&sr_ref].clone();

        // Draw every tab of each object in the real window.
        let objects = [
            Selection::Pool(conn.id, pool_ref.clone()),
            Selection::Host(conn.id, host_ref.clone()),
            Selection::Vm(conn.id, vm_ref.clone()),
            Selection::Sr(conn.id, sr_ref.clone()),
        ];
        let mut drawn = 0;
        for selection in objects {
            let subject = match &selection {
                Selection::Pool(..) => Subject::Pool(&pool),
                Selection::Host(_, r) => Subject::Host(r, &host),
                Selection::Vm(_, r) => Subject::Vm(r, &vm),
                Selection::Sr(_, r) => Subject::Sr(r, &sr),
                _ => unreachable!(),
            };
            for tab in tabs::tabs_for(&m, subject) {
                harness.state_mut().selection = Some(selection.clone());
                harness.state_mut().tab = tab;
                harness.step();
                harness.step();
                assert!(
                    harness
                        .query_all_by_label(tabs::page_title(tab, subject))
                        .next()
                        .is_some(),
                    "{tab:?} not drawn"
                );
                drawn += 1;
            }
        }

        let ctx = tabs::Ctx {
            model: &m,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            show_hidden: false,
        };
        let dump = serde_json::json!({
            "now": ctx.now,
            "pool": {
                "uuid": pool.uuid,
                "general": sections_json(&tabs::general::pool_sections(ctx, &pool)),
                "memory": memory_json(&tabs::memory::memory_rows(ctx, Subject::Pool(&pool))),
                "storage": table_json(&tabs::storage::physical_storage(ctx, None)),
                "networks": table_json(&tabs::network::networks(ctx, None)),
            },
            "host": {
                "uuid": host.uuid,
                "general": sections_json(&tabs::general::host_sections(ctx, &host_ref, &host)),
                "memory": memory_json(&tabs::memory::memory_rows(ctx, Subject::Host(&host_ref, &host))),
                "storage": table_json(&tabs::storage::physical_storage(ctx, Some(&host_ref))),
                "networks": table_json(&tabs::network::networks(ctx, Some(&host_ref))),
                "ip_configuration": table_json(&tabs::network::ip_configuration(ctx, Some(&host_ref))),
                "nics": table_json(&tabs::network::nics(ctx, &host_ref)),
            },
            "vm": {
                "uuid": vm.uuid,
                "power_state": power_label(&vm.power_state),
                "general": sections_json(&tabs::general::vm_sections(ctx, &vm)),
                "memory": memory_json(&tabs::memory::memory_rows(ctx, Subject::Vm(&vm_ref, &vm))),
                "storage": table_json(&tabs::storage::vm_disks(ctx, &vm)),
                "networking": table_json(&tabs::network::vm_interfaces(ctx, &vm)),
            },
            "sr": {
                "uuid": sr.uuid,
                "general": sections_json(&tabs::general::sr_sections(ctx, &sr)),
                "storage": table_json(&tabs::storage::sr_disks(ctx, &sr)),
            },
        });
        std::fs::write(&dump_path, serde_json::to_string_pretty(&dump).unwrap()).unwrap();
        harness
            .state()
            .backend
            .shutdown_blocking(Duration::from_secs(5));
        println!(
            "real host: {drawn} tabs drawn for a pool, a host, a VM and an SR; dump written; logged out"
        );
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
        let title = fill(ops_strings().op_starting, &[("vm", "db-01")]);
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
        // "Cancelled" already shows on the VM start's row (the operation and
        // the task status share the word): wait for the server to have
        // cancelled the task and for its own row to say so.
        step_until(&mut harness, "task cancelled", |h| {
            server.record("task", &task).unwrap()["status"] == "cancelled"
                && h.query_all_by_label(s.task_cancelled).count() == 2
        });
        assert_eq!(s.op_cancelled, s.task_cancelled);

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

    /// Nothing selected: the home page, whose "Add a Server" opens the
    /// connect dialog, as the original's.
    #[test]
    fn the_home_page_adds_a_server() {
        let runtime = runtime();
        let mut harness = app_harness(&runtime);
        let s = strings();
        harness.step();
        harness.get_by_label(s.cancel).click();
        step_until(&mut harness, "home page", |h| {
            h.query_by_label(s.welcome_add_server).is_some()
                && h.query_by_label(s.connect_title).is_none()
        });
        // The product's name and subtitle head the page.
        assert_eq!(branding::PRODUCT_NAME, "ReXen Center");
        harness.get_by_label("ReXen Center");
        harness.get_by_label(branding::SUBTITLE);
        harness.get_by_label(s.welcome_add_server).click();
        step_until(&mut harness, "connect dialog", |h| {
            h.query_by_label(s.connect_title).is_some()
        });
    }

    /// Options > Appearance: the theme switches at once and is saved; the
    /// language list follows the catalogue, and choosing a language
    /// re-labels the window (the test-only pseudo-language here).
    #[test]
    fn options_switch_theme_and_language() {
        use xen_core::ThemeSetting;
        let runtime = runtime();
        let mut harness = app_harness(&runtime);
        let s = strings();
        let theme =
            |h: &Harness<'_, XenAdminApp>| h.state().backend.state().settings.preferences.theme;
        harness.step();
        harness.get_by_label(s.cancel).click();
        harness.step();
        // A new profile follows the system.
        assert_eq!(theme(&harness), ThemeSetting::System);
        assert_eq!(
            harness.ctx.options(|o| o.theme_preference),
            egui::ThemePreference::System
        );
        harness.get_by_label(s.menu_tools).click();
        step_until(&mut harness, "tools menu", |h| {
            h.query_by_label(s.menu_options).is_some()
        });
        harness.get_by_label(s.menu_options).click();
        step_until(&mut harness, "options", |h| {
            h.query_by_label(s.options_theme_dark).is_some()
        });

        harness.get_by_label(s.options_theme_dark).click();
        step_until(&mut harness, "dark saved and shown", |h| {
            theme(h) == ThemeSetting::Dark && (h.ctx.theme() == egui::Theme::Dark)
        });
        harness.get_by_label(s.options_theme_light).click();
        step_until(&mut harness, "light saved and shown", |h| {
            theme(h) == ThemeSetting::Light && !(h.ctx.theme() == egui::Theme::Dark)
        });
        harness.get_by_label(s.options_theme_system).click();
        step_until(&mut harness, "system saved", |h| {
            theme(h) == ThemeSetting::System
                && h.ctx.options(|o| o.theme_preference) == egui::ThemePreference::System
        });

        // Language: "System default (English)", then every shipped language.
        let follow = fill(s.options_language_system, &[("language", "English")]);
        harness.get_by_value(&follow);
        harness.get_by_label(s.options_language).click();
        step_until(&mut harness, "language list", |h| {
            h.query_by_label("Pseudo").is_some()
        });
        harness.get_by_label("Pseudo").click();
        step_until(&mut harness, "pseudo-language", |h| {
            h.state().backend.state().settings.preferences.language == "en-XA"
                && h.query_by_label("[[File]]").is_some()
                && h.query_by_label("[[Theme:]]").is_some()
        });
        assert!(harness.query_by_label(s.menu_file).is_none());
        harness.get_by_label("[[Language:]]").click();
        step_until(&mut harness, "language list again", |h| {
            h.query_by_label("English").is_some()
        });
        harness.get_by_label("English").click();
        step_until(&mut harness, "English again", |h| {
            h.state().backend.state().settings.preferences.language == "en"
                && h.query_by_label(s.menu_file).is_some()
        });
        crate::locale::set_active_for_this_thread(None);
    }

    /// Clicks the first widget labelled `label` (the tree and the tab bar
    /// come before the details that may repeat a name).
    fn click_first(harness: &mut Harness<'_, XenAdminApp>, label: &str) {
        harness
            .get_all_by_label(label)
            .next()
            .unwrap_or_else(|| panic!("no {label:?}"))
            .click();
        harness.step();
    }

    fn connect_to_mock(
        runtime: &tokio::runtime::Runtime,
    ) -> (MockServer, Harness<'static, XenAdminApp>) {
        let server = runtime.block_on(MockServer::start(false));
        let mut harness = app_harness(runtime);
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
        });
        (server, harness)
    }

    /// Every tab of a VM, a host, an SR and the pool shows the fixture's
    /// data, and changes made on the server show up in the open tab through
    /// `event.from` alone.
    #[test]
    fn detail_tabs_show_and_follow_the_cache() {
        let runtime = runtime();
        let (server, mut harness) = connect_to_mock(&runtime);
        let t = tab_strings();

        // VM: General, Storage, Networking, Memory.
        click_first(&mut harness, "web-01");
        step_until(&mut harness, "VM general", |h| {
            h.query_by_label("Debian GNU/Linux 12 (bookworm)").is_some()
        });
        harness.get_by_label(t.hdr_vm_general);
        harness.get_by_label("Optimized (version 8.3 installed)");
        harness.get_by_label("production, web");
        click_tab(&mut harness, t.tab_storage);
        step_until(&mut harness, "VM disks", |h| {
            h.query_by_label("/dev/xvdb").is_some()
        });
        harness.get_by_label("web-01 data");
        harness.get_by_label("7 (Highest)");
        // Renamed elsewhere: the open tab follows.
        server.set_field(
            "vdi",
            "OpaqueRef:vdi-web-data",
            "name_label",
            serde_json::json!("web-01 content"),
        );
        step_until(&mut harness, "renamed disk", |h| {
            h.query_by_label("web-01 content").is_some()
        });
        click_first(&mut harness, t.tab_networking);
        step_until(&mut harness, "VIFs", |h| {
            h.query_by_label("192.0.2.101, 2001:db8::101").is_some()
        });
        harness.get_by_label("Guests VLAN 20");
        click_first(&mut harness, t.tab_memory);
        step_until(&mut harness, "VM memory", |h| {
            h.query_by_label(t.mem_static_max).is_some()
        });
        harness.get_by_label("4.0 GB");

        // The chosen tab sticks when another object has it too.
        click_first(&mut harness, "xcp-host-01");
        step_until(&mut harness, "host memory", |h| {
            h.query_by_label(t.mem_total).is_some()
        });
        harness.get_by_label("32.0 GB");
        click_first(&mut harness, t.tab_nics);
        step_until(&mut harness, "NICs", |h| {
            h.query_by_label("0000:01:00.1").is_some()
        });
        assert_eq!(harness.get_all_by_label("10000 Mbit/s").count(), 2);

        // Host in maintenance mode, then taken out of it elsewhere.
        click_first(&mut harness, "xcp-host-02");
        click_first(&mut harness, t.tab_general);
        step_until(&mut harness, "maintenance", |h| {
            h.query_by_label(t.host_maintenance).is_some()
        });
        server.set_field(
            "host",
            "OpaqueRef:host-0002",
            "enabled",
            serde_json::json!(true),
        );
        step_until(&mut harness, "enabled", |h| {
            h.query_by_label(t.host_maintenance).is_none()
                && h.query_by_label(t.hdr_host_general).is_some()
        });

        // Broken SR, repaired elsewhere.
        click_first(&mut harness, "ISO library");
        // Only "General" starts expanded, as in the original.
        step_until(&mut harness, "SR page", |h| {
            h.query_by_label(t.hdr_sr_general).is_some()
        });
        assert!(harness.query_by_label(t.sr_broken).is_none());
        // "Status" is also an Events column; the section header comes last.
        harness
            .get_all_by_label(t.sec_status)
            .last()
            .unwrap()
            .click();
        harness.step();
        step_until(&mut harness, "broken SR", |h| {
            h.query_by_label(t.sr_broken).is_some()
        });
        harness.get_by_label(t.hdr_sr_general);
        server.set_field(
            "pbd",
            "OpaqueRef:pbd-iso-02",
            "currently_attached",
            serde_json::json!(true),
        );
        step_until(&mut harness, "repaired SR", |h| {
            h.query_by_label(t.sr_ok).is_some()
        });
        click_tab(&mut harness, t.tab_storage);
        step_until(&mut harness, "SR disks", |h| {
            h.query_by_label("debian-12.iso").is_some()
        });

        // Pool: physical storage and networks.
        click_first(&mut harness, "Lab Pool");
        click_tab(&mut harness, t.tab_storage);
        step_until(&mut harness, "pool storage", |h| {
            h.query_by_label("29% (600 GB used)").is_some()
        });
        click_first(&mut harness, t.tab_networking);
        step_until(&mut harness, "pool networks", |h| {
            h.query_by_label(t.link_partial).is_some()
        });
        harness.get_by_label(t.ip_config_label);
    }

    /// View options add templates, snapshots and storage to the tree (and are
    /// saved); the filter keeps matching names and their parents.
    #[test]
    fn view_options_and_name_filter_shape_the_tree() {
        let runtime = runtime();
        let (server, mut harness) = connect_to_mock(&runtime);
        let s = strings();
        let t = tab_strings();
        harness.get_by_label("Web server template");
        harness.get_by_label("NFS VM storage");
        assert!(harness.query_by_label("Debian Bookworm 12").is_none());
        assert!(harness.query_by_label("web-01 before upgrade").is_none());

        for option in [t.view_default_templates, t.view_snapshots] {
            harness.get_by_label(s.menu_view).click();
            step_until(&mut harness, "view menu", |h| {
                h.query_by_label(option).is_some()
            });
            harness.get_by_label(option).click();
            harness.step();
        }
        step_until(&mut harness, "templates and snapshots", |h| {
            h.query_by_label("Debian Bookworm 12").is_some()
                && h.query_by_label("web-01 before upgrade").is_some()
        });
        let prefs = harness.state().backend.state().settings.preferences.clone();
        assert!(prefs.show_default_templates && prefs.show_snapshots);

        // A snapshot opens on its own General page.
        click_first(&mut harness, "web-01 before upgrade");
        step_until(&mut harness, "snapshot page", |h| {
            h.query_by_label(t.hdr_snapshot_general).is_some()
        });

        harness.get_by_label(s.menu_view).click();
        step_until(&mut harness, "view menu", |h| {
            h.query_by_label(t.view_local_storage).is_some()
        });
        harness.get_by_label(t.view_local_storage).click();
        step_until(&mut harness, "no local storage", |h| {
            h.query_all_by_label("Local storage").next().is_none()
        });

        // Filter by name (with the pool selected, so the details repeat no
        // VM or host name).
        let count = |h: &Harness<'_, XenAdminApp>, label: &str| h.query_all_by_label(label).count();
        click_first(&mut harness, "Lab Pool");
        harness
            .get_by_role_and_label(Role::TextInput, t.tree_filter_label)
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::TextInput, t.tree_filter_label)
            .type_text("DB-");
        step_until(&mut harness, "filtered", |h| {
            count(h, "db-01") == 1 && count(h, "web-01") == 0 && count(h, "xcp-host-01") == 0
        });
        assert_eq!(count(&harness, "xcp-host-02"), 1);
        assert!(count(&harness, "Lab Pool") >= 1);
        // A new match appears live.
        server.set_name_label("vm", "OpaqueRef:vm-build", "db-migrator");
        step_until(&mut harness, "live match", |h| count(h, "db-migrator") == 1);
        harness
            .get_by_role_and_label(Role::TextInput, t.tree_filter_label)
            .type_text("nothing-like-this");
        step_until(&mut harness, "no match", |h| {
            h.query_by_label(t.tree_no_match).is_some()
        });
        harness.get_by_label("x").click();
        step_until(&mut harness, "cleared", |h| count(h, "web-01") == 1);
    }

    // ---- F4: operations ------------------------------------------------------

    /// Lets windows that changed size settle (an anchored window re-centers
    /// over a few frames; a click aims at the last frame's position).
    fn settle(harness: &mut Harness<'_, XenAdminApp>) {
        for _ in 0..4 {
            harness.step();
        }
    }

    /// Clicks the detail tab titled `title` (the Storage menu of the menu
    /// bar has the Storage tab's label too).
    fn click_tab(harness: &mut Harness<'_, XenAdminApp>, title: &str) {
        settle(harness);
        harness
            .get_all_by_label(title)
            .find(|n| {
                n.accesskit_node()
                    .bounding_box()
                    .is_some_and(|b| b.y0 > 60.0)
            })
            .unwrap_or_else(|| panic!("no tab {title:?}"))
            .click();
        harness.step();
    }

    /// Right-clicks the first node labelled `label` and draws its menu.
    fn right_click(harness: &mut Harness<'_, XenAdminApp>, label: &str) {
        settle(harness);
        harness
            .get_all_by_label(label)
            .next()
            .unwrap_or_else(|| panic!("no {label:?}"))
            .click_secondary();
        harness.step();
        harness.step();
    }

    /// Clicks the last node labelled `label` (menus are drawn above the
    /// panels, so their items come last).
    fn click_last(harness: &mut Harness<'_, XenAdminApp>, label: &str) {
        settle(harness);
        harness
            .get_all_by_label(label)
            .last()
            .unwrap_or_else(|| panic!("no {label:?}"))
            .click();
        harness.step();
    }

    /// Clicks the right-most node labelled `label` (a submenu item, beside
    /// the tree row of the same name).
    fn click_rightmost(harness: &mut Harness<'_, XenAdminApp>, label: &str) {
        settle(harness);
        harness
            .get_all_by_label(label)
            .max_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x))
            .unwrap_or_else(|| panic!("no {label:?}"))
            .click();
        harness.step();
    }

    /// Whether the right-most node labelled `label` is enabled.
    fn rightmost_enabled(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
        h.query_all_by_label(label).count() >= 2
            && h.query_all_by_label(label)
                .max_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x))
                .is_some_and(|n| !n.accesskit_node().is_disabled())
    }

    fn vm_rec(h: &Harness<'_, XenAdminApp>, r: &str) -> Arc<VmRecord> {
        first_conn(h).unwrap().model.vms[&Ref::new(r)].clone()
    }

    fn op_done(h: &Harness<'_, XenAdminApp>, title: &str) -> bool {
        h.state()
            .backend
            .state()
            .operations
            .iter()
            .any(|o| !o.is_running() && i18n::operation_title(&o.kind, &o.server) == title)
    }

    /// Guest context menu (only what is offered now), the original's
    /// confirmations, a host context menu, and the Migrate to Server submenu
    /// with each server checked by the server.
    #[test]
    fn context_menus_confirmations_and_the_migrate_submenu() {
        let runtime = runtime();
        let (server, mut harness) = connect_to_mock(&runtime);
        let o = ops_strings();

        // A running guest offers what it allows, and nothing else.
        right_click(&mut harness, "web-01");
        for label in [
            o.reboot,
            o.suspend,
            o.pause,
            o.force_shut_down,
            o.migrate_to_server,
        ] {
            assert!(harness.query_by_label(label).is_some(), "{label}");
        }
        for label in [o.resume, o.unpause, o.start_on_server] {
            assert!(harness.query_by_label(label).is_none(), "{label}");
        }
        click_last(&mut harness, o.reboot);
        step_until(&mut harness, "confirmation", |h| {
            h.query_by_label(o.confirm_reboot_vm).is_some()
        });
        harness.get_by_label(o.confirm_reboot_vm_title);
        click_last(&mut harness, o.yes);
        let title = "Rebooting VM 'web-01' on 'xcp-host-01'";
        step_until(&mut harness, "reboot done", |h| op_done(h, title));
        assert!(server.calls().iter().any(|c| c == "Async.VM.clean_reboot"));
        harness.get_by_label(title);

        // Pause asks nothing; the tree shows the new state.
        step_until(&mut harness, "pause offered again", |h| {
            vm_rec(h, "OpaqueRef:vm-web")
                .allowed_operations
                .contains(&xapi::VmOperations::Pause)
        });
        right_click(&mut harness, "web-01");
        click_last(&mut harness, o.pause);
        step_until(&mut harness, "paused", |h| {
            vm_rec(h, "OpaqueRef:vm-web").power_state == VmPowerState::Paused
        });
        right_click(&mut harness, "web-01");
        assert!(harness.query_by_label(o.reboot).is_none());
        click_last(&mut harness, o.unpause);
        step_until(&mut harness, "running", |h| {
            vm_rec(h, "OpaqueRef:vm-web").power_state == VmPowerState::Running
        });

        // Host 2 leaves maintenance mode from its context menu.
        right_click(&mut harness, "xcp-host-02");
        assert!(harness.query_by_label(o.enter_maintenance).is_none());
        click_last(&mut harness, o.exit_maintenance);
        step_until(&mut harness, "host 2 enabled", |h| {
            first_conn(h).unwrap().model.hosts[&Ref::new("OpaqueRef:host-0002")].enabled
        });

        // Migrate to Server: the current server is not offered, host 2 is
        // once the server agrees.
        right_click(&mut harness, "web-01");
        click_last(&mut harness, o.migrate_to_server);
        let current = fill(
            o.host_with_reason,
            &[("host", "xcp-host-01"), ("reason", o.reason_current_server)],
        );
        step_until(&mut harness, "servers listed", |h| {
            h.query_by_label(&current).is_some() && rightmost_enabled(h, "xcp-host-02")
        });
        assert!(
            server
                .calls()
                .iter()
                .any(|c| c == "VM.assert_can_boot_here")
        );
        click_rightmost(&mut harness, "xcp-host-02");
        step_until(&mut harness, "migrate confirmation", |h| {
            h.query_by_label("Do you want to migrate the VM web-01 to the server xcp-host-02?")
                .is_some()
        });
        click_last(&mut harness, o.yes);
        step_until(&mut harness, "migrated", |h| {
            vm_rec(h, "OpaqueRef:vm-web").resident_on.as_str() == "OpaqueRef:host-0002"
        });
        step_until(&mut harness, "migration listed", |h| {
            op_done(
                h,
                "Migrating VM 'web-01' from 'xcp-host-01' to 'xcp-host-02'",
            )
        });
    }

    /// Server menu, toolbar and host context menu: maintenance mode with
    /// its dialog, reboot and shut down with their confirmations, power on,
    /// toolstack restart.
    #[test]
    fn host_operations_through_the_window() {
        let runtime = runtime();
        let (server, mut harness) = connect_to_mock(&runtime);
        let o = ops_strings();
        let h1 = Ref::<Host>::new("OpaqueRef:host-0001");
        let h2 = Ref::<Host>::new("OpaqueRef:host-0002");

        // The coordinator runs web-01 and host 2 is in maintenance: the
        // dialog says web-01 cannot be moved, and will not go on.
        right_click(&mut harness, "xcp-host-01");
        click_last(&mut harness, o.enter_maintenance);
        let title = fill(o.evacuate_title, &[("host", "xcp-host-01")]);
        let blocked = "There were no servers available to complete the specified operation.";
        step_until(&mut harness, "evacuation check", |h| {
            h.query_by_label(&title).is_some() && h.query_by_label(blocked).is_some()
        });
        assert!(!is_enabled(&harness, o.evacuate_button));
        click_last(&mut harness, strings().cancel);
        step_until(&mut harness, "dialog closed", |h| {
            h.query_by_label(&title).is_none()
        });

        // Host 2 out of maintenance through the Server menu.
        click_first(&mut harness, "xcp-host-02");
        harness
            .get_all_by_label(o.menu_server)
            .next()
            .unwrap()
            .click();
        harness.step();
        click_last(&mut harness, o.exit_maintenance);
        step_until(&mut harness, "host 2 enabled", |h| {
            first_conn(h).unwrap().model.hosts[&h2].enabled
        });

        // Now web-01 can move: Check Again, then enter maintenance mode.
        right_click(&mut harness, "xcp-host-01");
        click_last(&mut harness, o.enter_maintenance);
        step_until(&mut harness, "web-01 movable", |h| {
            h.query_by_label(o.evacuate_ok).is_some() && is_enabled(h, o.evacuate_button)
        });
        click_last(&mut harness, o.evacuate_button);
        step_until(&mut harness, "host 1 in maintenance", |h| {
            let c = first_conn(h).unwrap();
            !c.model.hosts[&h1].enabled
                && c.model.vms[&Ref::new("OpaqueRef:vm-web")].resident_on == h2
        });
        assert!(server.calls().iter().any(|c| c == "Async.host.evacuate"));
        // As the original, the coordinator hands the pool to the server
        // chosen in the dialog (the only other one).
        step_until(&mut harness, "host 2 coordinator", |h| {
            first_conn(h)
                .unwrap()
                .model
                .pool()
                .is_some_and(|(_, p)| p.master == h2)
        });
        assert!(
            server
                .calls()
                .iter()
                .any(|c| c == "Async.pool.designate_new_master")
        );
        // The rest of this test drives host 1 as the coordinator again.
        server.set_field(
            "pool",
            "OpaqueRef:pool-0001",
            "master",
            serde_json::json!(h1.as_str()),
        );
        step_until(&mut harness, "host 1 coordinator again", |h| {
            first_conn(h)
                .unwrap()
                .model
                .pool()
                .is_some_and(|(_, p)| p.master == h1)
        });
        right_click(&mut harness, "xcp-host-01");
        click_last(&mut harness, o.exit_maintenance);
        step_until(&mut harness, "host 1 enabled", |h| {
            first_conn(h).unwrap().model.hosts[&h1].enabled
        });

        // Reboot host 2 from the toolbar: "No" is the default; its guest is
        // shut down first.
        click_first(&mut harness, "xcp-host-02");
        click_first(&mut harness, o.reboot);
        step_until(&mut harness, "reboot confirmation", |h| {
            h.query_by_label(o.confirm_reboot_host_yes).is_some()
        });
        harness.get_by_label("Are you sure you want to reboot 'xcp-host-02'?");
        assert!(harness.get_all_by_label(o.no).last().unwrap().is_focused());
        click_last(&mut harness, o.confirm_reboot_host_yes);
        step_until(&mut harness, "rebooted", |h| {
            op_done(h, "Rebooting server 'xcp-host-02'")
                && vm_rec(h, "OpaqueRef:vm-web").power_state == VmPowerState::Halted
        });

        // Shut host 2 down, then power it on from its context menu.
        click_first(&mut harness, o.shut_down);
        step_until(&mut harness, "shutdown confirmation", |h| {
            h.query_by_label(o.confirm_shutdown_host_yes).is_some()
        });
        click_last(&mut harness, o.confirm_shutdown_host_yes);
        step_until(&mut harness, "host 2 off", |h| {
            let c = first_conn(h).unwrap();
            !xen_core::objects::host_is_live(&c.model, &c.model.hosts[&h2])
        });
        right_click(&mut harness, "xcp-host-02");
        // A host that is off offers Power On only (the toolbar's Reboot is
        // there, disabled).
        assert!(
            harness
                .query_all_by_label(o.reboot)
                .all(|n| n.accesskit_node().is_disabled())
        );
        click_last(&mut harness, o.power_on);
        step_until(&mut harness, "host 2 on", |h| {
            let c = first_conn(h).unwrap();
            xen_core::objects::host_is_live(&c.model, &c.model.hosts[&h2])
        });

        // Restart the coordinator's toolstack: the window reconnects.
        let logins = server.call_count("session.login_with_password");
        right_click(&mut harness, "xcp-host-01");
        click_last(&mut harness, o.restart_toolstack);
        let question = fill(
            o.confirm_restart_toolstack,
            &[("host", "xcp-host-01"), ("product", branding::PRODUCT_NAME)],
        );
        step_until(&mut harness, "toolstack confirmation", |h| {
            h.query_by_label(&question).is_some()
        });
        click_last(&mut harness, o.yes);
        step_until(&mut harness, "logged in again", |h| {
            server.call_count("session.login_with_password") > logins
                && first_conn(h).is_some_and(|c| c.status.is_connected())
        });
    }

    /// A VM Operator migrating a VM gets the role elevation dialog; a wrong
    /// password is refused there; a VM Power Admin's credentials run it.
    /// A failing operation shows the original's friendly error.
    #[test]
    fn role_elevation_and_friendly_errors() {
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start(false));
        server.set_field(
            "host",
            "OpaqueRef:host-0002",
            "enabled",
            serde_json::json!(true),
        );
        let mut harness = app_harness(&runtime);
        harness.state_mut().connect.username = "vm-operator-user".into();
        connect_through_dialog_as(&mut harness, &server.address(), "vm-operator-password");
        step_until(&mut harness, "tree", |h| {
            h.query_by_label("Lab Pool").is_some()
                && first_conn(h).is_some_and(|c| c.rbac.is_some())
        });
        let o = ops_strings();

        right_click(&mut harness, "web-01");
        click_last(&mut harness, o.migrate_to_server);
        step_until(&mut harness, "host 2 offered", |h| {
            rightmost_enabled(h, "xcp-host-02")
        });
        click_rightmost(&mut harness, "xcp-host-02");
        step_until(&mut harness, "confirmation", |h| {
            h.query_by_label("Do you want to migrate the VM web-01 to the server xcp-host-02?")
                .is_some()
        });
        click_last(&mut harness, o.yes);
        step_until(&mut harness, "elevation dialog", |h| {
            h.query_by_label(o.elevation_blurb).is_some()
        });
        harness.get_by_label("VM Operator");
        harness.get_by_label("Pool Admin, Pool Operator, VM Power Admin");
        harness.get_by_label("Migrating VM 'web-01' from 'xcp-host-01' to 'xcp-host-02'");
        assert!(!server.calls().iter().any(|c| c == "Async.VM.pool_migrate"));

        // A wrong password: the dialog stays, with the reason.
        harness
            .get_by_label(o.elevation_username)
            .type_text("vm-power-admin-user");
        harness.step();
        harness
            .get_by_role_and_label(Role::PasswordInput, o.elevation_password)
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::PasswordInput, o.elevation_password)
            .type_text("wrong");
        harness.step();
        harness.get_by_label(o.elevation_authorize).click();
        let refused = fill(
            o.user_authorization_failed,
            &[
                ("product", branding::PRODUCT_NAME),
                ("user", "vm-power-admin-user"),
                ("error", strings().err_auth_failed),
            ],
        );
        step_until(&mut harness, "refused", |h| {
            h.query_by_label(&refused).is_some()
        });

        // The right one runs it as that user.
        harness
            .get_by_role_and_label(Role::PasswordInput, o.elevation_password)
            .focus();
        harness.step();
        harness
            .get_by_role_and_label(Role::PasswordInput, o.elevation_password)
            .type_text("vm-power-admin-password");
        harness.step();
        harness.get_by_label(o.elevation_authorize).click();
        step_until(&mut harness, "migrated as the power admin", |h| {
            h.query_by_label(o.elevation_blurb).is_none()
                && vm_rec(h, "OpaqueRef:vm-web").resident_on.as_str() == "OpaqueRef:host-0002"
        });
        step_until(&mut harness, "listed with the user", |h| {
            h.query_by_label(
                "Migrating VM 'web-01' from 'xcp-host-01' to 'xcp-host-02' (as vm-power-admin-user)",
            )
            .is_some()
        });

        // A failure shows the original's wording, with names, not refs.
        server.fail_method("VM.clean_reboot", Some(("VM_MISSING_PV_DRIVERS", &[])));
        right_click(&mut harness, "web-01");
        click_last(&mut harness, o.reboot);
        step_until(&mut harness, "confirmation", |h| {
            h.query_by_label(o.confirm_reboot_vm).is_some()
        });
        click_last(&mut harness, o.yes);
        let error = "Rebooting VM 'web-01' on 'xcp-host-02': You attempted an operation on a VM which requires PV drivers to be installed but the drivers were not detected.";
        step_until(&mut harness, "friendly error", |h| {
            h.query_by_label(error).is_some()
        });
    }

    /// Like [`connect_through_dialog`] with the user name already set in
    /// the form.
    fn connect_through_dialog_as(
        harness: &mut Harness<'_, XenAdminApp>,
        server: &str,
        password: &str,
    ) {
        let s = strings();
        harness.step();
        harness.get_by_label(s.connect_server).type_text(server);
        harness.step();
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

    // ---- F4: real host, one named VM ------------------------------------------

    /// The variable naming the only VM the named-VM real-host tests may
    /// operate on (its exact name label).
    const NAMED_VM_VAR: &str = "XENADMIN_REAL_NAMED_VM";

    /// The name used when [`NAMED_VM_VAR`] is unset or empty: a neutral
    /// label, so a real run refuses to act unless the host has exactly one
    /// VM so named.
    const NAMED_VM_DEFAULT: &str = "rexen-test-vm";

    /// The only VM the named-VM real-host tests may operate on, from
    /// [`NAMED_VM_VAR`] (default [`NAMED_VM_DEFAULT`]). The mock rehearsals
    /// rename a mock guest to the same name.
    fn named_vm() -> &'static str {
        static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        NAME.get_or_init(|| {
            std::env::var(NAMED_VM_VAR)
                .ok()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| NAMED_VM_DEFAULT.to_owned())
        })
    }

    /// Steps until `done` or `limit`; `false` on timeout (no panic: the
    /// real-host run must always reach its cleanup).
    fn step_for(
        harness: &mut Harness<'_, XenAdminApp>,
        limit: Duration,
        done: impl Fn(&Harness<'_, XenAdminApp>) -> bool,
    ) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            harness.step();
            if done(harness) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Every operation this client ran targets `vm` (never a host).
    fn only_on(h: &Harness<'_, XenAdminApp>, vm: &Ref<Vm>) {
        for op in h.state().backend.state().operations.iter() {
            match &op.object {
                None | Some(xen_core::OperationObject::Vm(_)) => {}
                Some(other) => panic!("an operation on another object: {other:?}"),
            }
            if let Some(xen_core::OperationObject::Vm(r)) = &op.object {
                assert_eq!(r, vm, "an operation on another VM");
            }
        }
    }

    /// Operations of one name on the named VM, through the real window, with
    /// the state shown in the window after each. Ignored unless asked for:
    ///
    /// ```text
    /// XENADMIN_REAL_HOST=… XENADMIN_REAL_USER=root XENADMIN_REAL_PASSWORD=… \
    /// XENADMIN_REAL_FINGERPRINT=AA:BB:… XENADMIN_REAL_LOG=/path/outside/repo.txt \
    ///   cargo test -p xenadmin real_host_named_vm -- --ignored --nocapture
    /// ```
    ///
    /// It refuses to act unless exactly one VM is named [`named_vm`] and it is
    /// halted; it never touches another VM or a host; it leaves the VM
    /// halted (forcing a shut down if a step failed). The log gets each
    /// step's outcome (operation titles, statuses, durations, and whether
    /// the VM's guest agent offered the clean operations).
    #[test]
    #[ignore = "operates only the real VM named by XENADMIN_REAL_NAMED_VM (XENADMIN_REAL_* variables)"]
    fn real_host_named_vm_lifecycle() {
        let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
        let runtime = runtime();
        let mut harness = real_host_harness(&runtime);
        named_vm_lifecycle(&mut harness, Duration::from_secs(240), Some(&log_path));
    }

    /// The same sequence against the mock (the sanitized capture of the real
    /// host, one halted guest renamed [`named_vm`]), so the real run cannot
    /// fail on a test mistake.
    #[test]
    fn named_vm_lifecycle_on_the_mock() {
        let vm_label = named_vm();
        let runtime = runtime();
        let server = runtime.block_on(MockServer::start_with_records(
            true,
            xapi_mock::capture_records(),
        ));
        server.set_name_label("vm", "OpaqueRef:vm-0010", vm_label);
        let mut harness = app_harness(&runtime);
        connect_through_dialog(&mut harness, &server.address(), xapi_mock::PASSWORD);
        step_until(&mut harness, "certificate prompt", |h| {
            h.query_by_label(strings().cert_trust).is_some()
        });
        harness.get_by_label(strings().cert_trust).click();
        step_until(&mut harness, "connected", |h| {
            first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
        });
        let log = named_vm_lifecycle(&mut harness, Duration::from_secs(20), None);
        for step in [
            "Start:",
            "Pause:",
            "Unpause:",
            "Suspend:",
            "Resume:",
            "Reboot:",
            "Force Reboot:",
            "Shut Down:",
            "Start on Server:",
            "Force Shut Down:",
        ] {
            assert!(log.contains(step), "{step} missing from\n{log}");
        }
        assert!(log.contains("end state: Halted"), "{log}");
        for call in [
            "Async.VM.start",
            "Async.VM.pause",
            "Async.VM.unpause",
            "Async.VM.suspend",
            "Async.VM.resume",
            "Async.VM.clean_reboot",
            "Async.VM.hard_reboot",
            "Async.VM.clean_shutdown",
            "Async.VM.start_on",
            "Async.VM.hard_shutdown",
        ] {
            assert!(server.calls().iter().any(|c| c == call), "{call}");
        }
        // Nothing but the named VM was touched.
        assert!(
            !server
                .calls()
                .iter()
                .any(|c| c.starts_with("Async.host.") || c.starts_with("host.disable"))
        );
    }

    /// Runs every VM operation on [`named_vm`] through the window; returns the
    /// log. Panics (after leaving the VM halted) if a step fails.
    fn named_vm_lifecycle(
        harness: &mut Harness<'static, XenAdminApp>,
        long: Duration,
        log_path: Option<&str>,
    ) -> String {
        let vm_label = named_vm();
        use std::fmt::Write as _;
        use xapi::VmOperations;
        let state = harness.state().backend.state();
        let conn = state.connections[0].clone();
        let named: Vec<(Ref<Vm>, Arc<VmRecord>)> = conn
            .model
            .vms
            .iter()
            .filter(|(_, v)| v.name_label == vm_label)
            .map(|(r, v)| (r.clone(), v.clone()))
            .collect();
        assert_eq!(
            named.len(),
            1,
            "refusing: {} VMs are named {vm_label:?}",
            named.len()
        );
        let (vm_ref, record) = named.into_iter().next().unwrap();
        assert!(
            model::is_guest(&record),
            "refusing: {vm_label:?} is not a guest VM"
        );
        assert_eq!(
            record.power_state,
            VmPowerState::Halted,
            "refusing: {vm_label:?} is not halted as expected"
        );
        let host_names: Vec<String> = conn
            .model
            .hosts
            .values()
            .map(|h| h.name_label.clone())
            .collect();
        assert_eq!(host_names.len(), 1, "expected the single-host lab pool");
        let host_name = host_names[0].clone();
        let mut log = String::new();
        let _ = writeln!(log, "VM {vm_label:?} found once, halted; host count 1");
        let _ = writeln!(
            log,
            "connected as root; RBAC: {:?}",
            conn.rbac.as_ref().map(|r| r.is_local_superuser)
        );

        let s = strings();
        let o = ops_strings();
        let power = |h: &Harness<'_, XenAdminApp>| vm_rec(h, vm_ref.as_str()).power_state.clone();
        let allows = |h: &Harness<'_, XenAdminApp>, op: VmOperations| {
            vm_rec(h, vm_ref.as_str()).allowed_operations.contains(&op)
        };
        let shown = |h: &Harness<'_, XenAdminApp>, label: &str| {
            h.query_all_by_label(label).next().is_some()
        };

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Select the VM in the tree.
            harness.state_mut().selection = Some(Selection::Vm(conn.id, vm_ref.clone()));
            settle(harness);
            assert!(shown(harness, s.power_halted), "Halted not shown");

            // One toolbar step: click `label`, answer a confirmation if one
            // comes, wait for `expected`.
            let step = |harness: &mut Harness<'_, XenAdminApp>,
                        what: &str,
                        click: &dyn Fn(&mut Harness<'_, XenAdminApp>),
                        confirm: Option<&str>,
                        expected: VmPowerState,
                        expected_label: &str,
                        log: &mut String| {
                let started = Instant::now();
                let before = harness.state().backend.state().operations.len();
                click(harness);
                if let Some(text) = confirm {
                    assert!(
                        step_for(harness, Duration::from_secs(30), |h| shown(h, text)),
                        "{what}: no confirmation"
                    );
                    click_last(harness, o.yes);
                }
                let done = step_for(harness, long, |h| {
                    let st = h.state().backend.state();
                    st.operations.len() > before
                        && st.operations[before..].iter().all(|op| !op.is_running())
                });
                let st = harness.state().backend.state();
                let ops: Vec<String> = st.operations[before.min(st.operations.len())..]
                    .iter()
                    .map(|op| {
                        let status = match &op.status {
                            xen_core::OperationStatus::Failed(f) => format!(
                                "Failed: {}",
                                i18n::describe_operation_failure(Some(&st.connections[0]), f)
                            ),
                            other => i18n::operation_status_label(other).to_owned(),
                        };
                        format!("{} -> {status}", i18n::operation_title(&op.kind, "host"))
                    })
                    .collect();
                let reached = step_for(harness, long, |h| {
                    power(h) == expected && shown(h, expected_label)
                });
                let _ = writeln!(
                    log,
                    "{what}: ops [{}]; finished={done}; power {:?} shown as {expected_label:?}={reached}; {:.1} s",
                    ops.join("; ").replace(&host_name, "<host>"),
                    power(harness),
                    started.elapsed().as_secs_f64()
                );
                only_on(harness, &vm_ref);
                assert!(reached, "{what}: the VM did not reach {expected:?}");
            };
            // The server may list an operation a moment after the power
            // state changed: wait until the button is offered.
            let toolbar = |label: &'static str| {
                move |h: &mut Harness<'_, XenAdminApp>| {
                    let offered = step_for(h, Duration::from_secs(60), |h| {
                        h.query_all_by_label(label)
                            .next()
                            .is_some_and(|n| !n.accesskit_node().is_disabled())
                    });
                    assert!(
                        offered,
                        "{label} is not offered; allowed {:?}",
                        first_conn(h).map(|c| c
                            .model
                            .vms
                            .values()
                            .find(|v| v.name_label == vm_label)
                            .map(|v| v.allowed_operations.clone()))
                    );
                    settle(h);
                    h.get_all_by_label(label).next().unwrap().click();
                    h.step();
                }
            };
            // A context menu lists only what is offered: reopen it until
            // `label` is there. The VM is unselected meanwhile so the
            // toolbar and details header (which may show the same button)
            // are not in the way; it is selected again after the click.
            let selected = Selection::Vm(conn.id, vm_ref.clone());
            let context = move |label: &'static str| {
                let selected = selected.clone();
                move |h: &mut Harness<'_, XenAdminApp>| {
                    h.state_mut().selection = None;
                    let deadline = Instant::now() + Duration::from_secs(60);
                    loop {
                        right_click(h, vm_label);
                        if h.query_all_by_label(label).next().is_some() {
                            break;
                        }
                        h.key_press(egui::Key::Escape);
                        h.step();
                        assert!(Instant::now() < deadline, "{label} is not offered");
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    click_last(h, label);
                    h.state_mut().selection = Some(selected.clone());
                    h.step();
                }
            };

            step(
                harness,
                "Start",
                &toolbar(s.action_start),
                None,
                VmPowerState::Running,
                s.power_running,
                &mut log,
            );
            // The guest agent, if any, reports after boot: wait for the clean
            // operations for a while.
            let tools = step_for(harness, Duration::from_secs(150), |h| {
                allows(h, VmOperations::CleanShutdown) && allows(h, VmOperations::CleanReboot)
            });
            let _ = writeln!(
                log,
                "guest agent: clean shut down/reboot offered = {tools}; allowed now {:?}",
                vm_rec(harness, vm_ref.as_str()).allowed_operations
            );
            step(
                harness,
                "Pause",
                &toolbar(o.pause),
                None,
                VmPowerState::Paused,
                s.power_paused,
                &mut log,
            );
            step(
                harness,
                "Unpause",
                &toolbar(o.unpause),
                None,
                VmPowerState::Running,
                s.power_running,
                &mut log,
            );
            if allows(harness, VmOperations::Suspend) {
                step(
                    harness,
                    "Suspend",
                    &toolbar(o.suspend),
                    Some(o.confirm_suspend_vm),
                    VmPowerState::Suspended,
                    s.power_suspended,
                    &mut log,
                );
                step(
                    harness,
                    "Resume",
                    &toolbar(o.resume),
                    None,
                    VmPowerState::Running,
                    s.power_running,
                    &mut log,
                );
                if tools {
                    let _ = step_for(harness, Duration::from_secs(150), |h| {
                        allows(h, VmOperations::CleanReboot)
                    });
                }
            } else {
                let _ = writeln!(
                    log,
                    "Suspend: NOT OFFERED by the server (allowed_operations has no suspend)"
                );
            }
            if allows(harness, VmOperations::CleanReboot) {
                step(
                    harness,
                    "Reboot",
                    &toolbar(o.reboot),
                    Some(o.confirm_reboot_vm),
                    VmPowerState::Running,
                    s.power_running,
                    &mut log,
                );
            } else {
                let _ = writeln!(
                    log,
                    "Reboot (clean): NOT OFFERED by the server (no guest agent)"
                );
            }
            step(
                harness,
                "Force Reboot",
                &context(o.force_reboot),
                Some(o.confirm_force_reboot_vm_no_tasks),
                VmPowerState::Running,
                s.power_running,
                &mut log,
            );
            if tools {
                let _ = step_for(harness, Duration::from_secs(150), |h| {
                    allows(h, VmOperations::CleanShutdown)
                });
            }
            if allows(harness, VmOperations::CleanShutdown) {
                step(
                    harness,
                    "Shut Down",
                    &toolbar(s.action_clean_shutdown),
                    Some(o.confirm_shutdown_vm),
                    VmPowerState::Halted,
                    s.power_halted,
                    &mut log,
                );
            } else {
                let _ = writeln!(
                    log,
                    "Shut Down (clean): NOT OFFERED by the server (no guest agent)"
                );
                step(
                    harness,
                    "Force Shut Down",
                    &context(o.force_shut_down),
                    Some(o.confirm_force_shutdown_vm_no_tasks),
                    VmPowerState::Halted,
                    s.power_halted,
                    &mut log,
                );
            }
            // Start on Server: the submenu asks the server about the host.
            let host = host_name.clone();
            let open_submenu = context(o.start_on_server);
            let start_on = move |h: &mut Harness<'_, XenAdminApp>| {
                open_submenu(h);
                assert!(
                    step_for(h, Duration::from_secs(20), |h| rightmost_enabled(h, &host)),
                    "the host is not offered in Start on Server"
                );
                click_rightmost(h, &host);
            };
            step(
                harness,
                "Start on Server",
                &start_on,
                None,
                VmPowerState::Running,
                s.power_running,
                &mut log,
            );
            step(
                harness,
                "Force Shut Down",
                &context(o.force_shut_down),
                Some(o.confirm_force_shutdown_vm_no_tasks),
                VmPowerState::Halted,
                s.power_halted,
                &mut log,
            );
        }));
        match &outcome {
            Ok(()) => {}
            Err(e) => {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                let _ = writeln!(
                    log,
                    "STEP FAILED: {}",
                    why.lines()
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .replace(&host_name, "<host>")
                );
            }
        }

        // Cleanup: leave the VM halted, as found.
        if power(harness) != VmPowerState::Halted {
            let _ = writeln!(log, "cleanup: forcing a shut down of {vm_label:?}");
            harness.state().backend.send(Command::Vm {
                connection: conn.id,
                vm: vm_ref.clone(),
                action: xen_core::VmAction::HardShutdown,
            });
            let _ = step_for(harness, long, |h| power(h) == VmPowerState::Halted);
        }
        let end = power(harness);
        let _ = writeln!(log, "end state: {end:?}");
        only_on(harness, &vm_ref);
        let total = harness
            .state()
            .backend
            .state()
            .operations
            .iter()
            .filter(|o| o.object.is_some())
            .count();
        let _ = writeln!(log, "operations run: {total}, all on {vm_label:?}");
        harness
            .state()
            .backend
            .shutdown_blocking(Duration::from_secs(5));
        let _ = writeln!(log, "logged out");
        println!("{log}");
        if let Some(path) = log_path {
            std::fs::write(path, &log).unwrap();
        }
        assert_eq!(end, VmPowerState::Halted, "the VM was not left halted");
        if let Err(e) = outcome {
            std::panic::resume_unwind(e);
        }
        log
    }

    /// A real XAPI error in the window: Unpause sent to the halted VM named
    /// [`named_vm`] (the window would not offer it; the command goes straight
    /// to the backend). The server refuses it without changing anything, and
    /// the window shows the original's friendly message. Ignored unless
    /// asked for (same variables as [`real_host_named_vm_lifecycle`]).
    #[test]
    #[ignore = "sends a refused call to the real VM named by XENADMIN_REAL_NAMED_VM (XENADMIN_REAL_* variables)"]
    fn real_host_named_vm_friendly_error() {
        let vm_label = named_vm();
        let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
        let runtime = runtime();
        let mut harness = real_host_harness(&runtime);
        let conn = first_conn(&harness).unwrap();
        let named: Vec<Ref<Vm>> = conn
            .model
            .vms
            .iter()
            .filter(|(_, v)| v.name_label == vm_label)
            .map(|(r, _)| r.clone())
            .collect();
        assert_eq!(
            named.len(),
            1,
            "refusing: {} VMs are named {vm_label:?}",
            named.len()
        );
        let vm = named[0].clone();
        assert_eq!(
            conn.model.vms[&vm].power_state,
            VmPowerState::Halted,
            "refusing: {vm_label:?} is not halted"
        );
        harness.state().backend.send(Command::Vm {
            connection: conn.id,
            vm: vm.clone(),
            action: xen_core::VmAction::Unpause,
        });
        let prefix = format!("Unpausing VM '{vm_label}': ");
        let shown = step_for(&mut harness, Duration::from_secs(60), |h| {
            h.state().errors.iter().any(|e| e.starts_with(&prefix))
        });
        let error = harness
            .state()
            .errors
            .iter()
            .find(|e| e.starts_with(&prefix))
            .cloned()
            .unwrap_or_default();
        settle(&mut harness);
        let on_screen = harness.query_all_by_label(&error).next().is_some();
        let still_halted = vm_rec(&harness, vm.as_str()).power_state == VmPowerState::Halted;
        only_on(&harness, &vm);
        harness
            .state()
            .backend
            .shutdown_blocking(Duration::from_secs(5));
        let log = format!(
            "error shown={shown} on screen={on_screen}: {error}\nVM still halted: {still_halted}\nlogged out\n"
        );
        println!("{log}");
        std::fs::write(&log_path, &log).unwrap();
        assert!(shown && on_screen && still_halted, "{log}");
        assert!(error.contains("needed it to be in state"), "{log}");
    }
}
