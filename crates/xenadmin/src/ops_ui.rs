//! VM and host operations in the window: the menus and toolbar buttons
//! (offered by `allowed_operations`, see [`xen_core::actions`]), the
//! confirmation questions, the RBAC check made before an operation and the
//! role elevation dialog, the maintenance mode dialog, and the server
//! submenus of Start on / Resume on / Migrate to Server (each server
//! checked with `VM.assert_can_boot_here`, like the original).
//!
//! An operation goes through [`OpsUi::request`]: maintenance mode may open
//! its dialog, operations the original confirms ask first, then
//! [`OpsUi::dispatch`] checks the session's roles and either sends the
//! command or opens the role elevation dialog.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Align2, Button, Context, Grid, Key, RichText, TextEdit, Ui};
use xapi::{Host, HostRecord, Ref, Vm, VmRecord};
use xen_core::actions::{self, HostProblem};
use xen_core::operations::operation_kind;
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView,
    ElevationOutcome, HostAction, HostMenu, Notification, Precheck, PrecheckResult, VmAction,
};

use crate::branding;
use crate::i18n::{self, fill, strings};
use crate::ops_strings::ops_strings;

/// How long a server check of a submenu stays valid.
const CHECK_TTL: Duration = Duration::from_secs(15);

/// One entry of a VM or host menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Item {
        label: &'static str,
        enabled: bool,
        request: ActionRequest,
    },
    /// An item that opens a dialog (VM creation, snapshots, disks).
    Dialog {
        label: &'static str,
        enabled: bool,
        dialog: crate::provision_ui::DialogRequest,
    },
    /// A submenu listing the pool's servers.
    Servers {
        label: &'static str,
        enabled: bool,
        menu: HostMenu,
    },
    Separator,
}

/// The menu of a guest VM, in the original's order (`ContextMenuBuilder`
/// `SingleVM`), plus Pause / Unpause (toolbar-only in the original).
pub fn vm_entries(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<Entry> {
    let o = ops_strings();
    let usable = conn.status.is_connected();
    let busy = conn.busy_vms.contains(vm_ref);
    let item = |action: VmAction| Entry::Item {
        label: i18n::action_label(&action),
        enabled: usable && actions::vm_action_available(&conn.model, vm, &action, busy),
        request: ActionRequest::Vm {
            vm: vm_ref.clone(),
            action,
        },
    };
    let servers = |label, menu| Entry::Servers {
        label,
        enabled: usable && actions::host_menu_available(&conn.model, vm, menu, busy),
        menu,
    };
    vec![
        item(VmAction::CleanShutdown),
        item(VmAction::Start),
        item(VmAction::Resume),
        item(VmAction::Suspend),
        item(VmAction::CleanReboot),
        item(VmAction::Pause),
        item(VmAction::Unpause),
        Entry::Separator,
        item(VmAction::HardShutdown),
        item(VmAction::HardReboot),
        Entry::Separator,
        servers(o.start_on_server, HostMenu::StartOn),
        servers(o.resume_on_server, HostMenu::ResumeOn),
        servers(o.migrate_to_server, HostMenu::Migrate),
    ]
}

/// The menu of a host (`SingleAliveHostInPool` / `DeadHosts`).
pub fn host_entries(conn: &ConnectionView, host_ref: &Ref<Host>, host: &HostRecord) -> Vec<Entry> {
    let usable = conn.status.is_connected();
    let busy = conn.busy_hosts.contains(host_ref);
    let item = |action: HostAction| Entry::Item {
        label: i18n::host_action_label(action),
        enabled: usable && actions::host_action_available(&conn.model, host, action, busy),
        request: ActionRequest::Host {
            host: host_ref.clone(),
            action,
        },
    };
    let maintenance = if host.enabled {
        HostAction::EnterMaintenance
    } else {
        HostAction::ExitMaintenance
    };
    vec![
        item(HostAction::PowerOn),
        Entry::Separator,
        item(maintenance),
        Entry::Separator,
        item(HostAction::Reboot),
        item(HostAction::Shutdown),
        item(HostAction::RestartToolstack),
    ]
}

/// The toolbar buttons of a guest VM: Start or Shut Down, Reboot, Resume
/// or Suspend, Pause or Unpause, and the forced operations when the clean
/// ones are not offered (`ShowOnMainToolBar` in the original).
pub fn vm_toolbar(conn: &ConnectionView, vm_ref: &Ref<Vm>, vm: &VmRecord) -> Vec<Entry> {
    let entries = vm_entries(conn, vm_ref, vm);
    let find = |action: &VmAction| {
        entries
            .iter()
            .find(|e| matches!(e, Entry::Item { request: ActionRequest::Vm { action: a, .. }, .. } if a == action))
            .cloned()
            .expect("menu entry")
    };
    let enabled = |e: &Entry| matches!(e, Entry::Item { enabled: true, .. });
    let pick = |a: VmAction, b: VmAction| {
        let first = find(&a);
        if enabled(&first) { first } else { find(&b) }
    };
    let mut bar = vec![
        find(&VmAction::Start),
        find(&VmAction::CleanShutdown),
        find(&VmAction::CleanReboot),
        pick(VmAction::Resume, VmAction::Suspend),
        pick(VmAction::Unpause, VmAction::Pause),
    ];
    let clean = |a| enabled(&find(&a));
    for (forced, clean_one) in [
        (VmAction::HardShutdown, VmAction::CleanShutdown),
        (VmAction::HardReboot, VmAction::CleanReboot),
    ] {
        let entry = find(&forced);
        if enabled(&entry) && !clean(clean_one) {
            bar.push(entry);
        }
    }
    bar
}

/// The toolbar buttons of a host.
pub fn host_toolbar(conn: &ConnectionView, host_ref: &Ref<Host>, host: &HostRecord) -> Vec<Entry> {
    host_entries(conn, host_ref, host)
        .into_iter()
        .filter(|e| {
            matches!(e, Entry::Item { request: ActionRequest::Host { action, .. }, .. }
                if matches!(action, HostAction::PowerOn | HostAction::Reboot | HostAction::Shutdown))
        })
        .collect()
}

/// A confirmation question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: &'static str,
    pub text: String,
    pub yes: &'static str,
    /// "No" is the default button (host reboot and shut down).
    pub no_default: bool,
}

/// The question the original asks before `request`, if any.
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let o = ops_strings();
    let model = &conn.model;
    let plain = |title, text: &str| Confirmation {
        title,
        text: text.to_owned(),
        yes: o.yes,
        no_default: false,
    };
    match request {
        ActionRequest::Vm { vm, action } => {
            let record = model.vms.get(vm)?;
            let ha = actions::vm_ha_protected(model, record);
            let tasks = conn.busy_vms.contains(vm);
            Some(match action {
                VmAction::CleanShutdown => plain(
                    o.confirm_shutdown_vm_title,
                    if ha {
                        o.ha_confirm_shutdown_vm
                    } else {
                        o.confirm_shutdown_vm
                    },
                ),
                VmAction::CleanReboot => plain(o.confirm_reboot_vm_title, o.confirm_reboot_vm),
                VmAction::Suspend => plain(
                    o.confirm_suspend_vm_title,
                    if ha {
                        o.ha_confirm_suspend_vm
                    } else {
                        o.confirm_suspend_vm
                    },
                ),
                VmAction::HardShutdown => plain(
                    o.confirm_force_shutdown_vm_title,
                    match (ha, tasks) {
                        (true, true) => o.ha_confirm_force_shutdown_vm,
                        (true, false) => o.ha_confirm_force_shutdown_vm_no_tasks,
                        (false, true) => o.confirm_force_shutdown_vm,
                        (false, false) => o.confirm_force_shutdown_vm_no_tasks,
                    },
                ),
                VmAction::HardReboot => plain(
                    o.confirm_force_reboot_vm_title,
                    if tasks {
                        o.confirm_force_reboot_vm
                    } else {
                        o.confirm_force_reboot_vm_no_tasks
                    },
                ),
                VmAction::Migrate(h) => plain(
                    o.confirm_migrate_title,
                    &fill(
                        o.confirm_migrate,
                        &[
                            ("vm", &record.name_label),
                            (
                                "host",
                                &model
                                    .hosts
                                    .get(h)
                                    .map(|x| x.name_label.clone())
                                    .unwrap_or_default(),
                            ),
                        ],
                    ),
                ),
                _ => return None,
            })
        }
        // Asked by the Alerts view and the F7 dialogs themselves; saving
        // alerts is not confirmed.
        ActionRequest::Host { host, action } => {
            let record = model.hosts.get(host)?;
            let name = ("host", record.name_label.as_str());
            match action {
                HostAction::Reboot => Some(Confirmation {
                    title: o.confirm_reboot_host_title,
                    text: fill(o.confirm_reboot_host, &[name]),
                    yes: o.confirm_reboot_host_yes,
                    no_default: true,
                }),
                HostAction::Shutdown => {
                    let mut text = fill(o.confirm_shutdown_host, &[name]);
                    if xen_core::objects::is_coordinator(model, host) && model.hosts.len() > 1 {
                        text.push_str("\n\n");
                        text.push_str(&fill(o.shutdown_coordinator, &[name]));
                    }
                    Some(Confirmation {
                        title: o.confirm_shutdown_host_title,
                        text,
                        yes: o.confirm_shutdown_host_yes,
                        no_default: true,
                    })
                }
                HostAction::RestartToolstack => Some(plain(
                    o.confirm_restart_toolstack_title,
                    &fill(
                        o.confirm_restart_toolstack,
                        &[name, ("product", branding::PRODUCT_NAME)],
                    ),
                )),
                _ => None,
            }
        }
        ActionRequest::Sr { sr, action } => {
            use xen_core::storage::SrAction;
            let s = crate::sr_strings::sr_strings();
            let name = model
                .srs
                .get(sr)
                .map(|r| r.name_label.clone())
                .unwrap_or_default();
            let question = |title, text: String, yes, no_default| Confirmation {
                title,
                text,
                yes,
                no_default,
            };
            match action {
                SrAction::Detach => Some(question(
                    s.detach_title,
                    s.detach_text.to_owned(),
                    s.yes,
                    false,
                )),
                SrAction::Forget => Some(question(
                    s.forget_title,
                    fill(s.forget_text, &[("name", &name)]),
                    s.forget_yes,
                    true,
                )),
                SrAction::Destroy => Some(question(
                    s.destroy_title,
                    fill(s.destroy_text, &[("name", &name)]),
                    s.destroy_yes,
                    true,
                )),
                SrAction::Trim => {
                    Some(question(s.trim_title, s.trim_text.to_owned(), s.yes, false))
                }
                _ => None,
            }
        }
        request => crate::net_ui::confirmation(conn, request),
    }
}

struct ConfirmDialog {
    connection: ConnectionId,
    request: ActionRequest,
    question: Confirmation,
    focus: bool,
}

/// The role elevation dialog (`RoleElevationDialog`).
pub struct ElevationDialog {
    connection: ConnectionId,
    request: ActionRequest,
    action_title: String,
    server: String,
    is_pool: bool,
    current_user: String,
    current_roles: String,
    authorized: String,
    username: String,
    password: String,
    /// The request id while the credentials are being checked.
    pending: Option<u64>,
    error: Option<String>,
    focus: bool,
}

/// One VM in the maintenance mode dialog.
struct EvacuatedVm {
    vm: Ref<Vm>,
    name: String,
}

/// The maintenance mode dialog (`EvacuateHostDialog`, simplified).
struct MaintenanceDialog {
    connection: ConnectionId,
    host: Ref<Host>,
    host_name: String,
    coordinator: bool,
    vms: Vec<EvacuatedVm>,
    /// Precheck id while asking.
    pending: Option<u64>,
    result: Option<Result<HashMap<Ref<Vm>, String>, String>>,
}

/// The server check of one submenu.
struct HostChecks {
    requested: Instant,
    /// `None` while asking; per host, why it cannot be used (if it cannot).
    result: Option<HashMap<Ref<Host>, Option<String>>>,
}

type ChecksKey = (ConnectionId, Ref<Vm>, HostMenu);

/// What a pending precheck is for.
enum PendingCheck {
    Submenu(ChecksKey),
    Maintenance,
}

/// Operation state of the window.
#[derive(Default)]
pub struct OpsUi {
    confirm: Option<ConfirmDialog>,
    pub elevation: Option<ElevationDialog>,
    maintenance: Option<MaintenanceDialog>,
    checks: HashMap<ChecksKey, HostChecks>,
    pending: HashMap<u64, PendingCheck>,
    next_id: u64,
    /// Error messages for the window's error list.
    pub errors: Vec<String>,
    /// VMs this window just asked to start or resume (the window shows
    /// their Console tab once they run).
    pub started: Vec<(xen_core::ConnectionId, Ref<Vm>)>,
    /// Dialogs a menu asked for (opened by the window).
    pub dialog_requests: Vec<(ConnectionId, crate::provision_ui::DialogRequest)>,
}

impl OpsUi {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Asks for `request`: maintenance mode may open its dialog, and the
    /// operations the original confirms ask first.
    pub fn request(
        &mut self,
        conn: &ConnectionView,
        request: ActionRequest,
        backend: &BackendHandle,
    ) {
        if let ActionRequest::Host {
            host,
            action: HostAction::EnterMaintenance,
        } = &request
        {
            self.enter_maintenance(conn, host, backend);
            return;
        }
        match confirmation(conn, &request) {
            Some(question) => {
                self.confirm = Some(ConfirmDialog {
                    connection: conn.id,
                    request,
                    question,
                    focus: true,
                });
            }
            None => self.dispatch(conn, request, backend),
        }
    }

    /// Runs `request` if the session may; otherwise opens the role
    /// elevation dialog.
    pub fn dispatch(
        &mut self,
        conn: &ConnectionView,
        request: ActionRequest,
        backend: &BackendHandle,
    ) {
        if let ActionRequest::Vm { vm, action } = &request
            && matches!(
                action,
                VmAction::Start | VmAction::StartOn(_) | VmAction::Resume | VmAction::ResumeOn(_)
            )
        {
            self.started.push((conn.id, vm.clone()));
        }
        match rbac::check(&conn.model, conn.rbac.as_deref(), &request.rbac_methods()) {
            RbacCheck::Allowed => backend.send(match request {
                ActionRequest::Vm { vm, action } => Command::Vm {
                    connection: conn.id,
                    vm,
                    action,
                },
                ActionRequest::Host { host, action } => Command::Host {
                    connection: conn.id,
                    host,
                    action,
                },
                request => Command::Action {
                    connection: conn.id,
                    request,
                },
            }),
            RbacCheck::Denied { authorized } => {
                tracing::info!(
                    "the session's roles do not allow this operation; asking for other credentials"
                );
                let kind = operation_kind(&conn.model, &request);
                let pool = conn.model.pool();
                let is_pool = conn.model.hosts.len() > 1;
                let server = pool
                    .map(|(_, p)| xen_core::objects::pool_name(&conn.model, p))
                    .unwrap_or_else(|| conn.server.clone());
                self.elevation = Some(ElevationDialog {
                    connection: conn.id,
                    action_title: i18n::operation_title(&kind, &conn.server),
                    request,
                    server,
                    is_pool,
                    current_user: conn.username.clone(),
                    current_roles: i18n::session_roles_text(conn),
                    authorized: authorized
                        .iter()
                        .map(|r| i18n::role_name(&r.name_label))
                        .collect::<Vec<_>>()
                        .join(", "),
                    username: String::new(),
                    password: String::new(),
                    pending: None,
                    error: None,
                    focus: true,
                });
            }
        }
    }

    fn enter_maintenance(
        &mut self,
        conn: &ConnectionView,
        host: &Ref<Host>,
        backend: &BackendHandle,
    ) {
        let o = ops_strings();
        let model = &conn.model;
        let Some(record) = model.hosts.get(host) else {
            return;
        };
        let coordinator = xen_core::objects::is_coordinator(model, host);
        if coordinator && model.pool().is_some_and(|(_, p)| p.ha_enabled) {
            self.errors.push(fill(
                o.ha_cannot_evacuate_coordinator,
                &[("host", &record.name_label)],
            ));
            return;
        }
        let vms: Vec<EvacuatedVm> = actions::running_guests(model, record)
            .into_iter()
            .map(|(r, v)| EvacuatedVm {
                vm: r.clone(),
                name: v.name_label.clone(),
            })
            .collect();
        let request = ActionRequest::Host {
            host: host.clone(),
            action: HostAction::EnterMaintenance,
        };
        // Nothing to move: no dialog (`HostMaintenanceModeCommand`).
        if vms.is_empty() && (model.hosts.len() == 1 || !coordinator) {
            self.dispatch(conn, request, backend);
            return;
        }
        let mut dialog = MaintenanceDialog {
            connection: conn.id,
            host: host.clone(),
            host_name: record.name_label.clone(),
            coordinator: coordinator && model.hosts.len() > 1,
            vms,
            pending: None,
            result: None,
        };
        self.check_evacuation(&mut dialog, backend);
        self.maintenance = Some(dialog);
    }

    fn check_evacuation(&mut self, dialog: &mut MaintenanceDialog, backend: &BackendHandle) {
        let id = self.next_id();
        self.pending.insert(id, PendingCheck::Maintenance);
        dialog.pending = Some(id);
        dialog.result = None;
        backend.send(Command::Precheck {
            connection: dialog.connection,
            id,
            check: Precheck::Evacuation {
                host: dialog.host.clone(),
            },
        });
    }

    /// Takes the notifications meant for operations; returns the others.
    pub fn on_notification(
        &mut self,
        notification: Notification,
        state: &AppState,
    ) -> Option<Notification> {
        match notification {
            Notification::ActionFailed {
                connection,
                kind,
                error,
                ..
            } => {
                let conn = state.connection(connection).map(|c| &**c);
                let server = conn.map(|c| c.server.as_str()).unwrap_or_default();
                self.errors.push(fill(
                    ops_strings().operation_failed,
                    &[
                        ("operation", &i18n::operation_title(&kind, server)),
                        ("error", &i18n::describe_xapi_error_in(conn, &error)),
                    ],
                ));
                None
            }
            Notification::Elevation {
                request_id,
                outcome,
            } => {
                let o = ops_strings();
                if let Some(dialog) = self
                    .elevation
                    .as_mut()
                    .filter(|d| d.pending == Some(request_id))
                {
                    dialog.pending = None;
                    match outcome {
                        ElevationOutcome::Authorized { .. } => self.elevation = None,
                        ElevationOutcome::NotAuthorized => {
                            dialog.error = Some(format!(
                                "{}: {}",
                                o.permission_denied, o.user_not_authorized
                            ));
                            dialog.password.clear();
                        }
                        ElevationOutcome::LoginFailed(e) => {
                            let conn = state.connection(dialog.connection).map(|c| &**c);
                            dialog.error = Some(fill(
                                o.user_authorization_failed,
                                &[
                                    ("product", branding::PRODUCT_NAME),
                                    ("user", dialog.username.trim()),
                                    ("error", &i18n::describe_xapi_error_in(conn, &e)),
                                ],
                            ));
                            dialog.password.clear();
                        }
                        ElevationOutcome::NotConnected => {
                            dialog.error = Some(strings().op_not_connected.to_owned());
                        }
                    }
                }
                None
            }
            Notification::Precheck {
                connection,
                id,
                result,
            } => {
                // Another part of the window asked (the New SR wizard).
                let Some(pending) = self.pending.remove(&id) else {
                    return Some(Notification::Precheck {
                        connection,
                        id,
                        result,
                    });
                };
                match (Some(pending), result) {
                    (Some(PendingCheck::Submenu(key)), PrecheckResult::BootHosts(hosts)) => {
                        let conn = state.connection(key.0).map(|c| &**c);
                        let map = hosts
                            .into_iter()
                            .map(|(h, e)| {
                                let why = e.map(|e| match conn {
                                    Some(_) => i18n::short_error(&e),
                                    None => i18n::describe_xapi_error(&e),
                                });
                                (h, why)
                            })
                            .collect();
                        if let Some(c) = self.checks.get_mut(&key) {
                            c.result = Some(map);
                        }
                    }
                    (Some(PendingCheck::Maintenance), PrecheckResult::Evacuation(result)) => {
                        if let Some(d) = self.maintenance.as_mut().filter(|d| d.pending == Some(id))
                        {
                            d.pending = None;
                            let conn = state.connection(d.connection).map(|c| &**c);
                            d.result = Some(
                                result
                                    .map(|blocked| {
                                        blocked
                                            .into_iter()
                                            .map(|(vm, mut reason)| {
                                                let code = if reason.is_empty() {
                                                    String::new()
                                                } else {
                                                    reason.remove(0)
                                                };
                                                let e = xapi::Error::Api {
                                                    code,
                                                    params: reason,
                                                };
                                                (vm, i18n::describe_xapi_error_in(conn, &e))
                                            })
                                            .collect()
                                    })
                                    .map_err(|e| i18n::describe_xapi_error_in(conn, &e)),
                            );
                        }
                    }
                    _ => {}
                }
                None
            }
            other => Some(other),
        }
    }

    // ---- menus -------------------------------------------------------------

    /// Draws `entries` as menu items; `only_enabled` leaves out the items
    /// that cannot run (context menus, like the original's `AddIfEnabled`).
    /// Returns whether an item was chosen.
    #[allow(clippy::too_many_arguments)]
    pub fn menu(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm: Option<(&Ref<Vm>, &VmRecord)>,
        entries: Vec<Entry>,
        only_enabled: bool,
        backend: &BackendHandle,
    ) -> bool {
        let mut chosen = None;
        let mut dialog = None;
        let mut pending_separator = false;
        let mut drawn = 0;
        for entry in entries {
            match entry {
                Entry::Separator => pending_separator = drawn > 0,
                Entry::Dialog {
                    label,
                    enabled,
                    dialog: request,
                } => {
                    if only_enabled && !enabled {
                        continue;
                    }
                    if std::mem::take(&mut pending_separator) {
                        ui.separator();
                    }
                    drawn += 1;
                    if ui.add_enabled(enabled, Button::new(label)).clicked() {
                        dialog = Some(request);
                    }
                }
                Entry::Item {
                    label,
                    enabled,
                    request,
                } => {
                    if only_enabled && !enabled {
                        continue;
                    }
                    if std::mem::take(&mut pending_separator) {
                        ui.separator();
                    }
                    drawn += 1;
                    if ui.add_enabled(enabled, Button::new(label)).clicked() {
                        chosen = Some(request);
                    }
                }
                Entry::Servers {
                    label,
                    enabled,
                    menu,
                } => {
                    if only_enabled && !enabled {
                        continue;
                    }
                    if std::mem::take(&mut pending_separator) {
                        ui.separator();
                    }
                    drawn += 1;
                    let Some((vm_ref, vm)) = vm else { continue };
                    if !enabled {
                        ui.add_enabled(false, Button::new(label));
                        continue;
                    }
                    ui.menu_button(label, |ui| {
                        if let Some(request) =
                            self.servers_menu(ui, conn, vm_ref, vm, menu, backend)
                        {
                            chosen = Some(request);
                        }
                    });
                }
            }
        }
        if drawn == 0 {
            ui.add_enabled(false, Button::new(ops_strings().host_menu_empty));
        }
        if let Some(dialog) = dialog {
            self.dialog_requests.push((conn.id, dialog));
            ui.close();
            return true;
        }
        match chosen {
            Some(request) => {
                self.request(conn, request, backend);
                ui.close();
                true
            }
            None => false,
        }
    }

    /// The servers of a Start on / Resume on / Migrate to submenu: the home
    /// server first, then every host; each enabled once the server agrees.
    fn servers_menu(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
        menu: HostMenu,
        backend: &BackendHandle,
    ) -> Option<ActionRequest> {
        let o = ops_strings();
        let model = &conn.model;
        let hosts: Vec<(Ref<Host>, std::sync::Arc<HostRecord>)> =
            xen_core::objects::sorted_hosts(model)
                .into_iter()
                .map(|(r, h)| (r.clone(), h.clone()))
                .collect();
        let key = (conn.id, vm_ref.clone(), menu);
        let stale = self
            .checks
            .get(&key)
            .is_none_or(|c| c.requested.elapsed() > CHECK_TTL);
        if stale {
            // Only hosts without a known problem are asked about.
            let ask: Vec<Ref<Host>> = hosts
                .iter()
                .filter(|(r, h)| actions::target_host_problem(model, vm, r, h, menu).is_none())
                .map(|(r, _)| r.clone())
                .collect();
            let id = self.next_id();
            self.pending.insert(id, PendingCheck::Submenu(key.clone()));
            self.checks.insert(
                key.clone(),
                HostChecks {
                    requested: Instant::now(),
                    result: None,
                },
            );
            backend.send(Command::Precheck {
                connection: conn.id,
                id,
                check: Precheck::BootHosts {
                    vm: vm_ref.clone(),
                    hosts: ask,
                },
            });
        }
        let checks = self.checks.get(&key).and_then(|c| c.result.as_ref());
        // `None`: still asking; `Some(None)`: usable; `Some(Some(why))`: not.
        let verdict = |r: &Ref<Host>, h: &HostRecord| -> Option<Option<String>> {
            if let Some(problem) = actions::target_host_problem(model, vm, r, h, menu) {
                return Some(Some(problem_text(problem).to_owned()));
            }
            let checks = checks?;
            Some(checks.get(r).cloned().unwrap_or(None))
        };
        let mut chosen = None;
        let mut item = |ui: &mut Ui, label: &str, target: Option<&Ref<Host>>| {
            let host = target.and_then(|r| hosts.iter().find(|(h, _)| h == r));
            let (text, enabled) = match host {
                None => (
                    fill(
                        o.host_with_reason,
                        &[("host", label), ("reason", o.reason_no_home)],
                    ),
                    false,
                ),
                Some((r, h)) => match verdict(r, h) {
                    None => (fill(o.host_checking, &[("host", label)]), false),
                    Some(None) => (label.to_owned(), true),
                    Some(Some(why)) => (
                        fill(o.host_with_reason, &[("host", label), ("reason", &why)]),
                        false,
                    ),
                },
            };
            if ui.add_enabled(enabled, Button::new(text)).clicked()
                && let Some((r, _)) = host
            {
                chosen = Some(ActionRequest::Vm {
                    vm: vm_ref.clone(),
                    action: menu.action(r.clone()),
                });
            }
        };
        item(ui, o.home_server, vm.affinity.non_null());
        for (r, h) in &hosts {
            item(ui, &h.name_label, Some(r));
        }
        if checks.is_none() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        chosen
    }

    // ---- windows -----------------------------------------------------------

    /// Draws the open dialogs.
    pub fn windows(&mut self, ctx: &Context, state: &AppState, backend: &BackendHandle) {
        self.confirm_window(ctx, state, backend);
        self.elevation_window(ctx, state, backend);
        self.maintenance_window(ctx, state, backend);
        // Checks of connections that went away are dropped.
        self.checks.retain(|k, _| state.connection(k.0).is_some());
    }

    fn confirm_window(&mut self, ctx: &Context, state: &AppState, backend: &BackendHandle) {
        let Some(dialog) = self.confirm.as_mut() else {
            return;
        };
        let o = ops_strings();
        let mut answer = None;
        egui::Window::new(dialog.question.title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(&dialog.question.text);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let yes = ui.button(dialog.question.yes);
                    let no = ui.button(o.no);
                    if std::mem::take(&mut dialog.focus) {
                        if dialog.question.no_default {
                            no.request_focus();
                        } else {
                            yes.request_focus();
                        }
                    }
                    if yes.clicked() {
                        answer = Some(true);
                    }
                    if no.clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                        answer = Some(false);
                    }
                });
            });
        if let Some(yes) = answer {
            let dialog = self.confirm.take().expect("open dialog");
            if yes && let Some(conn) = state.connection(dialog.connection) {
                self.dispatch(conn, dialog.request, backend);
            }
        }
    }

    fn elevation_window(&mut self, ctx: &Context, state: &AppState, backend: &BackendHandle) {
        let Some(dialog) = self.elevation.as_mut() else {
            return;
        };
        if state.connection(dialog.connection).is_none() {
            self.elevation = None;
            return;
        }
        let o = ops_strings();
        let s = strings();
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(branding::PRODUCT_NAME)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.strong(o.elevation_blurb);
                ui.add_space(6.0);
                Grid::new("elevation-info")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        let rows = [
                            (o.elevation_action, dialog.action_title.as_str()),
                            (
                                if dialog.is_pool {
                                    o.elevation_pool
                                } else {
                                    o.elevation_server
                                },
                                dialog.server.as_str(),
                            ),
                            (o.elevation_user, dialog.current_user.as_str()),
                            (o.elevation_role, dialog.current_roles.as_str()),
                            (o.elevation_authorized, dialog.authorized.as_str()),
                        ];
                        for (label, value) in rows {
                            ui.label(label);
                            ui.label(value);
                            ui.end_row();
                        }
                    });
                ui.separator();
                let busy = dialog.pending.is_some();
                Grid::new("elevation-form")
                    .num_columns(2)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        let label = ui.label(o.elevation_username);
                        let user = ui
                            .add_enabled(
                                !busy,
                                TextEdit::singleline(&mut dialog.username).desired_width(220.0),
                            )
                            .labelled_by(label.id);
                        if std::mem::take(&mut dialog.focus) {
                            user.request_focus();
                        }
                        ui.end_row();
                        let label = ui.label(o.elevation_password);
                        let password = ui
                            .add_enabled(
                                !busy,
                                TextEdit::singleline(&mut dialog.password)
                                    .password(true)
                                    .desired_width(220.0),
                            )
                            .labelled_by(label.id);
                        if password.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            submit = true;
                        }
                        ui.end_row();
                    });
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(o.elevation_checking);
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready =
                        !busy && !dialog.username.trim().is_empty() && !dialog.password.is_empty();
                    if ui
                        .add_enabled(ready, Button::new(o.elevation_authorize))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button(s.cancel).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.elevation = None;
            return;
        }
        let ready = dialog.pending.is_none()
            && !dialog.username.trim().is_empty()
            && !dialog.password.is_empty();
        if submit && ready {
            self.next_id += 1;
            let id = self.next_id;
            let dialog = self.elevation.as_mut().expect("open dialog");
            dialog.pending = Some(id);
            dialog.error = None;
            backend.send(Command::RunElevated {
                connection: dialog.connection,
                request: dialog.request.clone(),
                username: dialog.username.trim().to_owned(),
                password: std::mem::take(&mut dialog.password).into(),
                request_id: id,
            });
        }
    }

    fn maintenance_window(&mut self, ctx: &Context, state: &AppState, backend: &BackendHandle) {
        let Some(mut dialog) = self.maintenance.take() else {
            return;
        };
        let Some(conn) = state.connection(dialog.connection).cloned() else {
            return;
        };
        let o = ops_strings();
        let s = strings();
        let mut evacuate = false;
        let mut check_again = false;
        let mut cancel = false;
        egui::Window::new(fill(o.evacuate_title, &[("host", &dialog.host_name)]))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.label(o.evacuate_blurb);
                if dialog.coordinator {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        fill(
                            o.evacuate_coordinator,
                            &[("product", branding::PRODUCT_NAME)],
                        ),
                    );
                }
                ui.add_space(6.0);
                ui.strong(o.evacuate_vms);
                if dialog.vms.is_empty() {
                    ui.label(RichText::new(o.evacuate_no_vms).weak());
                }
                Grid::new("evacuate-vms")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for vm in &dialog.vms {
                            ui.label(&vm.name);
                            match &dialog.result {
                                None => {
                                    ui.label(RichText::new(o.evacuate_checking).weak());
                                }
                                Some(Err(_)) => {
                                    ui.label("");
                                }
                                Some(Ok(blocked)) => match blocked.get(&vm.vm) {
                                    Some(why) => {
                                        ui.colored_label(ui.visuals().error_fg_color, why);
                                    }
                                    None => {
                                        ui.label(o.evacuate_ok);
                                    }
                                },
                            }
                            ui.end_row();
                        }
                    });
                if let Some(Err(e)) = &dialog.result {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        fill(o.evacuate_check_failed, &[("error", e)]),
                    );
                }
                if dialog.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(o.evacuate_checking);
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ready = matches!(&dialog.result, Some(Ok(b)) if b.is_empty())
                        && conn.status.is_connected();
                    if ui
                        .add_enabled(ready, Button::new(o.evacuate_button))
                        .clicked()
                    {
                        evacuate = true;
                    }
                    if ui
                        .add_enabled(
                            dialog.pending.is_none(),
                            Button::new(o.evacuate_check_again),
                        )
                        .clicked()
                    {
                        check_again = true;
                    }
                    if ui.button(s.cancel).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            return;
        }
        if evacuate {
            let request = ActionRequest::Host {
                host: dialog.host.clone(),
                action: HostAction::EnterMaintenance,
            };
            self.dispatch(&conn, request, backend);
            return;
        }
        if check_again {
            self.check_evacuation(&mut dialog, backend);
        }
        self.maintenance = Some(dialog);
    }
}

fn problem_text(problem: HostProblem) -> &'static str {
    let o = ops_strings();
    match problem {
        HostProblem::CurrentServer => o.reason_current_server,
        HostProblem::Offline => o.reason_offline,
        HostProblem::Disabled => o.reason_disabled,
        HostProblem::OlderThanCurrent => o.reason_older,
        HostProblem::IncompatibleCpu => o.reason_cpu,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use xen_core::{ConnectionModel, ConnectionStatus, SessionRbac};

    fn conn() -> ConnectionView {
        let model = ConnectionModel::from_database(&xapi_mock::fixture_records());
        ConnectionView {
            id: 1,
            server: "s".into(),
            username: "root".into(),
            status: ConnectionStatus::Connected {
                api_version: (2, 21),
            },
            model: Arc::new(model),
            busy_vms: Arc::default(),
            busy_hosts: Arc::default(),
            busy_srs: Arc::default(),
            rbac: Some(Arc::new(SessionRbac::superuser())),
            pool_uuid: None,
            authority: "s".into(),
        }
    }

    fn labels(entries: &[Entry]) -> Vec<(&'static str, bool)> {
        entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item { label, enabled, .. }
                | Entry::Servers { label, enabled, .. }
                | Entry::Dialog { label, enabled, .. } => Some((*label, *enabled)),
                Entry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn running_vm_menu_and_toolbar() {
        let c = conn();
        let r = Ref::new("OpaqueRef:vm-web");
        let vm = c.model.vms[&r].clone();
        let enabled: Vec<_> = labels(&vm_entries(&c, &r, &vm))
            .into_iter()
            .filter(|(_, e)| *e)
            .map(|(l, _)| l)
            .collect();
        assert_eq!(
            enabled,
            [
                "Shut Down",
                "Suspend",
                "Reboot",
                "Pause",
                "Force Shut Down",
                "Force Reboot",
                "Migrate to Server"
            ]
        );
        let bar = labels(&vm_toolbar(&c, &r, &vm));
        assert_eq!(
            bar,
            [
                ("Start", false),
                ("Shut Down", true),
                ("Reboot", true),
                ("Suspend", true),
                ("Pause", true)
            ]
        );
    }

    #[test]
    fn confirmations_follow_the_original() {
        let c = conn();
        let vm = Ref::new("OpaqueRef:vm-web");
        let ask = |action| {
            confirmation(
                &c,
                &ActionRequest::Vm {
                    vm: vm.clone(),
                    action,
                },
            )
        };
        assert_eq!(ask(VmAction::CleanReboot).unwrap().title, "Reboot VM");
        assert!(ask(VmAction::Pause).is_none());
        assert!(ask(VmAction::Resume).is_none());
        assert!(
            ask(VmAction::HardShutdown)
                .unwrap()
                .text
                .ends_with("This operation can result in data loss.")
        );
        let host = Ref::new("OpaqueRef:host-0001");
        let q = confirmation(
            &c,
            &ActionRequest::Host {
                host,
                action: HostAction::Shutdown,
            },
        )
        .unwrap();
        assert_eq!(q.yes, "Yes, Shut Down");
        assert!(q.no_default);
        assert!(q.text.contains("is the pool coordinator"));
    }

    #[test]
    fn host_menu_by_state() {
        let c = conn();
        let r = Ref::new("OpaqueRef:host-0002");
        let host = c.model.hosts[&r].clone();
        let enabled: Vec<_> = labels(&host_entries(&c, &r, &host))
            .into_iter()
            .filter(|(_, e)| *e)
            .map(|(l, _)| l)
            .collect();
        assert_eq!(
            enabled,
            [
                "Exit Maintenance Mode",
                "Reboot",
                "Shut Down",
                "Restart Toolstack"
            ]
        );
    }
}
