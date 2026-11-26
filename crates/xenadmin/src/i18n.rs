//! User-interface strings.
//!
//! Every user-visible string lives in a [`Strings`] table so the UI can be
//! translated later by adding another table and selecting it in [`strings`].
//! Only English exists today. Templates use `{name}` placeholders filled by
//! [`fill`].

use std::time::Duration;

use xapi::error::codes;
use xen_core::operations::ProvisionKind;
use xen_core::rbac;
use xen_core::{
    ConnectionError, ConnectionModel, ConnectionStatus, ConnectionView, HostAction,
    OperationFailure, OperationKind, OperationStatus, OperationStep, VmAction,
};

use crate::friendly_errors::FRIENDLY_ERRORS;
use crate::ops_strings::ops_strings;

/// One language's UI strings.
pub struct Strings {
    pub menu_file: &'static str,
    pub menu_connect: &'static str,
    pub menu_disconnect: &'static str,
    pub menu_disconnect_all: &'static str,
    pub menu_quit: &'static str,
    pub menu_view: &'static str,
    pub menu_events: &'static str,
    pub menu_tools: &'static str,
    pub menu_options: &'static str,
    pub menu_vm: &'static str,
    pub menu_help: &'static str,
    pub menu_about: &'static str,

    pub action_start: &'static str,
    pub action_clean_shutdown: &'static str,

    pub tree_heading: &'static str,
    pub tree_empty: &'static str,
    pub tree_no_selection: &'static str,

    pub details_type_pool: &'static str,
    pub details_type_host: &'static str,
    pub details_type_vm: &'static str,
    pub details_busy: &'static str,

    pub power_running: &'static str,
    pub power_halted: &'static str,
    pub power_suspended: &'static str,
    pub power_paused: &'static str,
    pub power_unknown: &'static str,

    pub connect_title: &'static str,
    pub connect_server: &'static str,
    pub connect_server_hint: &'static str,
    pub connect_user: &'static str,
    pub connect_password: &'static str,
    pub connect_password_saved_hint: &'static str,
    pub connect_remember: &'static str,
    pub connect_button: &'static str,
    pub cancel: &'static str,

    pub cert_title: &'static str,
    /// `{server}`
    pub cert_unknown: &'static str,
    /// `{server}`
    pub cert_changed: &'static str,
    pub cert_fingerprint: &'static str,
    pub cert_previous: &'static str,
    pub cert_advice: &'static str,
    pub cert_trust: &'static str,

    pub about_title: &'static str,
    /// `{version}`
    pub about_version: &'static str,
    pub about_summary: &'static str,
    pub about_credits_heading: &'static str,
    pub about_credits: &'static str,
    /// `{license}`
    pub about_license_heading: &'static str,

    pub errors_title: &'static str,
    pub errors_clear: &'static str,

    pub status_disconnected: &'static str,
    /// `{server}`
    pub status_connecting: &'static str,
    /// `{server}`, `{version}`
    pub status_connected: &'static str,
    /// `{server}`, `{error}`
    pub status_failed: &'static str,
    /// `{server}`, `{seconds}`, `{attempt}`
    pub status_reconnect_wait: &'static str,
    /// `{server}`, `{attempt}`
    pub status_reconnecting: &'static str,
    /// `{count}`
    pub status_summary: &'static str,

    pub server_connecting: &'static str,
    pub server_connected: &'static str,
    pub server_reconnecting: &'static str,
    pub server_failed: &'static str,
    pub server_disconnected: &'static str,
    pub server_heading: &'static str,
    pub server_status: &'static str,
    pub server_user: &'static str,
    pub server_error: &'static str,
    pub server_connect: &'static str,
    pub server_enter_password: &'static str,
    pub server_retry_now: &'static str,
    pub server_disconnect: &'static str,
    pub server_forget: &'static str,
    pub server_dismiss: &'static str,

    pub events_title: &'static str,
    pub events_clear: &'static str,
    pub events_empty: &'static str,
    pub events_server_tasks: &'static str,
    pub events_cancel: &'static str,
    pub events_col_status: &'static str,
    pub events_col_description: &'static str,
    pub events_col_server: &'static str,
    pub events_col_progress: &'static str,
    pub events_col_time: &'static str,
    pub op_running: &'static str,
    pub op_succeeded: &'static str,
    pub op_cancelled: &'static str,
    pub op_failed: &'static str,
    /// `{server}`
    pub op_connect: &'static str,
    /// `{server}`
    pub op_reconnect: &'static str,
    /// `{task}`
    pub op_cancel_task: &'static str,
    pub op_not_connected: &'static str,
    pub op_disconnected: &'static str,
    /// `{time}`
    pub time_ago: &'static str,
    /// `{time}`
    pub time_took: &'static str,
    pub task_pending: &'static str,
    pub task_success: &'static str,
    pub task_failure: &'static str,
    pub task_cancelling: &'static str,
    pub task_cancelled: &'static str,

    pub options_title: &'static str,
    pub options_save_passwords: &'static str,
    pub options_restore: &'static str,
    pub options_settings_dir: &'static str,
    pub options_log_dir: &'static str,
    pub options_mode_user: &'static str,
    pub options_mode_portable: &'static str,
    pub options_mode_override: &'static str,
    pub options_mode_memory: &'static str,
    pub close: &'static str,

    pub err_cert_rejected: &'static str,
    pub err_password_required: &'static str,
    /// `{error}`
    pub err_password_unavailable: &'static str,
    /// `{existing}`
    pub err_already_connected: &'static str,
    pub err_too_many_redirects: &'static str,
    /// `{server}`, `{error}`
    pub err_connection_refused: &'static str,
    /// `{error}`
    pub err_connect: &'static str,
    /// `{error}`
    pub err_lost: &'static str,
    pub err_auth_failed: &'static str,
    /// `{coordinator}`
    pub err_host_is_member: &'static str,
    pub err_host_booting: &'static str,
    pub err_session_invalid: &'static str,
    /// `{error}`
    pub err_network: &'static str,
}

/// English strings.
pub static EN: Strings = Strings {
    menu_file: "File",
    menu_connect: "Connect…",
    menu_disconnect: "Disconnect",
    menu_disconnect_all: "Disconnect All",
    menu_quit: "Quit",
    menu_view: "View",
    menu_events: "Events",
    menu_tools: "Tools",
    menu_options: "Options…",
    menu_vm: "VM",
    menu_help: "Help",
    menu_about: "About",

    action_start: "Start",
    action_clean_shutdown: "Shut Down",

    tree_heading: "Infrastructure",
    tree_empty: "Not connected. Use File > Connect… to add a server.",
    tree_no_selection: "Select a pool, host or VM in the tree.",

    details_type_pool: "Pool",
    details_type_host: "Host",
    details_type_vm: "Virtual machine",
    details_busy: "Operation in progress…",

    power_running: "Running",
    power_halted: "Halted",
    power_suspended: "Suspended",
    power_paused: "Paused",
    power_unknown: "Unknown",

    connect_title: "Connect to Server",
    connect_server: "Server:",
    connect_server_hint: "host name or IP address",
    connect_user: "User name:",
    connect_password: "Password:",
    connect_password_saved_hint: "leave empty to use the saved password",
    connect_remember: "Remember password",
    connect_button: "Connect",
    cancel: "Cancel",

    cert_title: "Security Certificate",
    cert_unknown: "The server {server} presented a certificate that is not trusted yet. \
                   XCP-ng hosts use self-signed certificates by default.",
    cert_changed: "WARNING: the certificate presented by {server} has CHANGED since you last trusted it. \
                   This can mean the host was reinstalled or its certificate renewed, or that someone \
                   is intercepting the connection.",
    cert_fingerprint: "SHA-256 fingerprint:",
    cert_previous: "Previously trusted fingerprint:",
    cert_advice: "Compare the fingerprint with the one shown on the host console before trusting it.",
    cert_trust: "Trust and Connect",

    about_title: "About",
    about_version: "Version {version}",
    about_summary: "A cross-platform management console for XCP-ng pools.",
    about_credits_heading: "Credits",
    about_credits: "Based on XCP-ng Center, maintained by the XCP-ng community, which derives from \
                    XenAdmin (Citrix XenCenter) by Citrix Systems, Inc. and Cloud Software Group, Inc. \
                    Thanks to the XenCenter and XCP-ng Center contributors listed in the original \
                    project's CREDITS.md.",
    about_license_heading: "License ({license})",

    errors_title: "Errors",
    errors_clear: "Clear",

    status_disconnected: "Not connected",
    status_connecting: "Connecting to {server}…",
    status_connected: "Connected to {server} (API {version})",
    status_failed: "{server}: {error}",
    status_reconnect_wait: "{server}: connection lost; retrying in {seconds} s (attempt {attempt})",
    status_reconnecting: "{server}: reconnecting (attempt {attempt})…",
    status_summary: "{count} connected",

    server_connecting: "Connecting…",
    server_connected: "Connected",
    server_reconnecting: "Reconnecting…",
    server_failed: "Not connected",
    server_disconnected: "Disconnected",
    server_heading: "Server",
    server_status: "Status",
    server_user: "User name",
    server_error: "Last error",
    server_connect: "Connect",
    server_enter_password: "Enter Password…",
    server_retry_now: "Retry Now",
    server_disconnect: "Disconnect",
    server_forget: "Forget Server",
    server_dismiss: "Dismiss",

    events_title: "Events",
    events_clear: "Clear Finished",
    events_empty: "No events yet.",
    events_server_tasks: "Server tasks",
    events_cancel: "Cancel",
    events_col_status: "Status",
    events_col_description: "Description",
    events_col_server: "Server",
    events_col_progress: "Progress",
    events_col_time: "Time",
    op_running: "In progress",
    op_succeeded: "Completed",
    op_cancelled: "Cancelled",
    op_failed: "Failed",
    op_connect: "Connect to {server}",
    op_reconnect: "Reconnect to {server}",
    op_cancel_task: "Cancel task '{task}'",
    op_not_connected: "the server is not connected",
    op_disconnected: "disconnected while running",
    time_ago: "{time} ago",
    time_took: "took {time}",
    task_pending: "In progress",
    task_success: "Completed",
    task_failure: "Failed",
    task_cancelling: "Cancelling…",
    task_cancelled: "Cancelled",

    options_title: "Options",
    options_save_passwords: "Remember passwords in the system keyring by default",
    options_restore: "Reconnect to the servers that were connected when the app last closed",
    options_settings_dir: "Settings folder",
    options_log_dir: "Log folder",
    options_mode_user: "per-user settings",
    options_mode_portable: "portable mode (next to the program)",
    options_mode_override: "set by XENADMIN_CONFIG_DIR",
    options_mode_memory: "Settings are kept in memory only (no configuration folder).",
    close: "Close",

    err_cert_rejected: "Connection cancelled: certificate not trusted.",
    err_password_required: "No saved password for this server. Enter the password to connect.",
    err_password_unavailable: "Could not read the saved password from the system keyring: {error}",
    err_already_connected: "Already connected to this server or pool (as {existing}).",
    err_too_many_redirects: "The pool members kept redirecting to each other; no pool coordinator was found.",
    err_connection_refused: "{server}: {error}",
    err_connect: "Could not connect: {error}",
    err_lost: "Connection lost: {error}",
    err_auth_failed: "Authentication failed. Check the user name and password.",
    err_host_is_member: "This host is a pool member. Connect to the pool coordinator ({coordinator}) instead.",
    err_host_booting: "The host is still booting. Try again shortly.",
    err_session_invalid: "The session has expired or was closed on the server.",
    err_network: "network error ({error})",
};

/// The active string table.
pub fn strings() -> &'static Strings {
    &EN
}

/// Replaces `{name}` placeholders in `template`.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Human-readable description of a XAPI error.
pub fn describe_xapi_error(error: &xapi::Error) -> String {
    describe_xapi_error_in(None, error)
}

/// The original console's description of error `code` (its
/// `FriendlyErrorNames`), with `{0}`, `{1}`… replaced by `params`.
pub fn friendly_error(code: &str, params: &[String]) -> Option<String> {
    let i = FRIENDLY_ERRORS
        .binary_search_by(|(k, _)| k.as_bytes().cmp(code.as_bytes()))
        .ok()?;
    let mut text = FRIENDLY_ERRORS[i].1.to_owned();
    for n in 0..10 {
        let placeholder = format!("{{{n}}}");
        if text.contains(&placeholder) {
            let value = params.get(n).map(String::as_str).unwrap_or_default();
            text = text.replace(&placeholder, value);
        }
    }
    Some(text.trim().to_owned())
}

/// The short form of a friendly error (menus), else its full form.
pub fn short_error(error: &xapi::Error) -> String {
    match error {
        xapi::Error::Api { code, params } => friendly_error(&format!("{code}-SHORT"), params)
            .or_else(|| friendly_error(code, params))
            .unwrap_or_else(|| error.to_string()),
        other => describe_xapi_error(other),
    }
}

/// The name of the cached object `opaque_ref` refers to, if any.
fn name_of_ref(model: &ConnectionModel, opaque_ref: &str) -> Option<String> {
    if !opaque_ref.starts_with("OpaqueRef:") {
        return None;
    }
    let name = |label: Option<&String>| label.cloned();
    name(
        model
            .vms
            .get(&xapi::Ref::new(opaque_ref))
            .map(|v| &v.name_label),
    )
    .or_else(|| {
        name(
            model
                .hosts
                .get(&xapi::Ref::new(opaque_ref))
                .map(|h| &h.name_label),
        )
    })
    .or_else(|| {
        name(
            model
                .srs
                .get(&xapi::Ref::new(opaque_ref))
                .map(|x| &x.name_label),
        )
    })
    .or_else(|| {
        name(
            model
                .networks
                .get(&xapi::Ref::new(opaque_ref))
                .map(|x| &x.name_label),
        )
    })
    .or_else(|| {
        name(
            model
                .vdis
                .get(&xapi::Ref::new(opaque_ref))
                .map(|x| &x.name_label),
        )
    })
    .or_else(|| {
        model
            .pools
            .get(&xapi::Ref::new(opaque_ref))
            .map(|p| xen_core::objects::pool_name(model, p))
    })
}

/// Friendly name of an RBAC role (`Role.FriendlyName`).
pub fn role_name(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "pool-admin" => "Pool Admin".to_owned(),
        "pool-operator" => "Pool Operator".to_owned(),
        "vm-power-admin" => "VM Power Admin".to_owned(),
        "vm-admin" => "VM Admin".to_owned(),
        "vm-operator" => "VM Operator".to_owned(),
        "read-only" => "Read Only".to_owned(),
        _ => name.to_owned(),
    }
}

/// The roles of a connection's session, as the role elevation dialog and
/// the permission errors word them.
pub fn session_roles_text(conn: &ConnectionView) -> String {
    match conn.rbac.as_deref() {
        Some(r) if r.is_local_superuser => ops_strings().superuser_role.to_owned(),
        Some(r) => rbac::session_roles(&conn.model, r)
            .iter()
            .map(|r| role_name(&r.name_label))
            .collect::<Vec<_>>()
            .join(", "),
        None => String::new(),
    }
}

/// Human-readable description of a XAPI error raised on connection
/// `conn`: object references become names, and a permission error says
/// which roles could do it (`Failure.ParseRBACFailure`).
pub fn describe_xapi_error_in(conn: Option<&ConnectionView>, error: &xapi::Error) -> String {
    let s = strings();
    if let xapi::Error::Api { code, params } = error
        && let Some(text) = crate::sr_strings::storage_error(code, params)
            .or_else(|| crate::net_strings::network_error(code))
            .or_else(|| crate::pool_strings::pool_error(code, params))
            .or_else(|| crate::ha_strings::ha_error(code, params))
    {
        return text;
    }
    match error {
        xapi::Error::Api { code, params } => match code.as_str() {
            codes::SESSION_AUTHENTICATION_FAILED => s.err_auth_failed.to_owned(),
            codes::HOST_IS_SLAVE => fill(
                s.err_host_is_member,
                &[(
                    "coordinator",
                    params.first().map(String::as_str).unwrap_or("?"),
                )],
            ),
            codes::HOST_STILL_BOOTING => s.err_host_booting.to_owned(),
            codes::SESSION_INVALID => s.err_session_invalid.to_owned(),
            codes::RBAC_PERMISSION_DENIED if conn.is_some_and(|c| c.rbac.is_some()) => {
                let conn = conn.expect("checked");
                let method = params
                    .first()
                    .map(|m| rbac::normalize(m))
                    .unwrap_or_default();
                let authorized = rbac::authorized_roles(&conn.model, &[method])
                    .iter()
                    .map(|r| role_name(&r.name_label))
                    .collect::<Vec<_>>()
                    .join(", ");
                fill(
                    ops_strings().rbac_denied,
                    &[
                        ("roles", &session_roles_text(conn)),
                        ("authorized", &authorized),
                    ],
                )
            }
            _ => {
                let params: Vec<String> = params
                    .iter()
                    .map(|p| {
                        conn.and_then(|c| name_of_ref(&c.model, p))
                            .unwrap_or_else(|| p.clone())
                    })
                    .collect();
                friendly_error(code, &params).unwrap_or_else(|| {
                    xapi::Error::Api {
                        code: code.clone(),
                        params,
                    }
                    .to_string()
                })
            }
        },
        xapi::Error::Transport(e) => {
            let mut detail = e.to_string();
            let mut source = std::error::Error::source(e);
            while let Some(inner) = source {
                detail = format!("{detail}: {inner}");
                source = inner.source();
            }
            fill(s.err_network, &[("error", &detail)])
        }
        other => other.to_string(),
    }
}

/// Human-readable description of a connection failure.
pub fn describe_connection_error(error: &ConnectionError) -> String {
    let s = strings();
    match error {
        ConnectionError::CertificateRejected => s.err_cert_rejected.to_owned(),
        ConnectionError::PasswordRequired => s.err_password_required.to_owned(),
        ConnectionError::PasswordUnavailable(e) => {
            fill(s.err_password_unavailable, &[("error", &e.to_string())])
        }
        ConnectionError::AlreadyConnected { existing } => {
            fill(s.err_already_connected, &[("existing", existing)])
        }
        ConnectionError::TooManyRedirects => s.err_too_many_redirects.to_owned(),
        ConnectionError::Connect(e) | ConnectionError::Unsupported(e) => {
            fill(s.err_connect, &[("error", &describe_xapi_error(e))])
        }
        ConnectionError::Lost(e) => fill(s.err_lost, &[("error", &describe_xapi_error(e))]),
    }
}

/// Short status word for a connection.
pub fn connection_status_label(status: &ConnectionStatus) -> &'static str {
    let s = strings();
    match status {
        ConnectionStatus::Connecting => s.server_connecting,
        ConnectionStatus::Connected { .. } => s.server_connected,
        ConnectionStatus::Reconnecting { .. } => s.server_reconnecting,
        ConnectionStatus::Failed { .. } => s.server_failed,
    }
}

/// Description of an entry of the operation history.
pub fn operation_title(kind: &OperationKind, server: &str) -> String {
    let s = strings();
    match kind {
        OperationKind::Connect => fill(s.op_connect, &[("server", server)]),
        OperationKind::Reconnect => fill(s.op_reconnect, &[("server", server)]),
        OperationKind::VmAction {
            vm_name,
            action,
            location,
            from,
            target,
        } => {
            let o = ops_strings();
            let target = target.as_deref().unwrap_or_default();
            let vm = ("vm", vm_name.as_str());
            match action {
                VmAction::Start => fill(o.op_starting, &[vm]),
                VmAction::StartOn(_) => fill(o.op_starting_on, &[vm, ("host", target)]),
                VmAction::CleanShutdown | VmAction::HardShutdown => {
                    fill(o.op_shutting_down_on, &[vm, ("host", location)])
                }
                VmAction::CleanReboot | VmAction::HardReboot => {
                    fill(o.op_rebooting_on, &[vm, ("host", location)])
                }
                VmAction::Suspend => fill(o.op_suspending, &[vm]),
                VmAction::Resume => fill(o.op_resuming, &[vm]),
                VmAction::ResumeOn(_) => fill(o.op_resuming_on, &[vm, ("host", target)]),
                VmAction::Pause => fill(o.op_pausing, &[vm]),
                VmAction::Unpause => fill(o.op_unpausing, &[vm]),
                VmAction::Migrate(_) => match from {
                    Some(from) => fill(
                        o.op_migrating_from,
                        &[vm, ("from", from.as_str()), ("to", target)],
                    ),
                    None => fill(o.op_migrating, &[vm, ("to", target)]),
                },
            }
        }
        OperationKind::HostAction { host_name, action } => {
            let o = ops_strings();
            let template = match action {
                HostAction::Reboot => o.op_host_rebooting,
                HostAction::Shutdown => o.op_host_shutting_down,
                HostAction::PowerOn => o.op_host_power_on,
                HostAction::EnterMaintenance => o.op_host_evacuating,
                HostAction::ExitMaintenance => o.op_host_exiting_maintenance,
                HostAction::RestartToolstack => o.op_toolstack_restart,
            };
            fill(template, &[("host", host_name)])
        }
        OperationKind::CancelTask { task_name } => fill(s.op_cancel_task, &[("task", task_name)]),
        OperationKind::DismissAlerts { count, title } => {
            let p = crate::perf_strings::perf_strings();
            match title {
                Some(t) if *count == 1 => fill(
                    p.op_dismiss_one,
                    &[("title", &crate::alerts_view::message_title(t))],
                ),
                _ => fill(p.op_dismiss_many, &[("count", &count.to_string())]),
            }
        }
        OperationKind::SavePerfmon { object_name } => fill(
            crate::perf_strings::perf_strings().op_save_alerts,
            &[("object", object_name)],
        ),
        OperationKind::Provision(kind) => provision_title(kind),
        OperationKind::Storage(kind) => storage_title(kind),
        OperationKind::Network(kind) => network_title(kind),
        OperationKind::Pool(kind) => pool_title(kind),
        OperationKind::Ha(kind) => crate::ha_strings::ha_title(kind),
        OperationKind::Ad(kind) => crate::ha_strings::ad_title(kind),
    }
}

/// The title of a pool or server settings operation.
fn pool_title(kind: &xen_core::operations::PoolKind) -> String {
    use xen_core::operations::PoolKind;
    let p = crate::pool_strings::pool_strings();
    match kind {
        PoolKind::CreatePool { name, coordinator } => fill(
            p.op_create_pool,
            &[("name", name), ("coordinator", coordinator)],
        ),
        PoolKind::AddHost { host, pool } => fill(p.op_add_host, &[("host", host), ("pool", pool)]),
        PoolKind::EjectHost { host, pool } => {
            fill(p.op_eject_host, &[("host", host), ("pool", pool)])
        }
        PoolKind::DestroyPool { pool } => fill(p.op_destroy_pool, &[("pool", pool)]),
        PoolKind::Properties { name } => fill(p.op_properties, &[("name", name)]),
        PoolKind::Dom0Memory { host } => fill(p.op_dom0_memory, &[("host", host)]),
        PoolKind::ChangePassword { pool } => fill(p.op_change_password, &[("pool", pool)]),
        PoolKind::RotateSecret => p.op_rotate_secret.to_owned(),
        PoolKind::EnableTls { pool } => fill(p.op_enable_tls, &[("pool", pool)]),
        PoolKind::InstallCertificate { host } => fill(p.op_install_certificate, &[("host", host)]),
        PoolKind::ResetCertificate { host } => fill(p.op_reset_certificate, &[("host", host)]),
        PoolKind::Backup { host } => fill(p.op_backup, &[("host", host)]),
        PoolKind::Restore { host } => fill(p.op_restore, &[("host", host)]),
    }
}

/// The title of a networking operation.
fn network_title(kind: &xen_core::operations::NetworkKind) -> String {
    use xen_core::operations::NetworkKind;
    let n = crate::net_strings::net_strings();
    match kind {
        NetworkKind::CreateNetwork { name, pool } => {
            fill(n.op_create_network, &[("name", name), ("pool", pool)])
        }
        NetworkKind::RemoveNetwork { name, pool } => {
            fill(n.op_remove_network, &[("name", name), ("pool", pool)])
        }
        NetworkKind::UpdateNetwork { name, pool } => {
            fill(n.op_update_network, &[("name", name), ("pool", pool)])
        }
        NetworkKind::CreateBond { name } => fill(n.op_create_bond, &[("bond", name)]),
        NetworkKind::DestroyBond { name } => fill(n.op_destroy_bond, &[("bond", name)]),
        NetworkKind::RescanPifs { host } => fill(n.op_rescan, &[("host", host)]),
        NetworkKind::ChangeNetworking => n.op_change_networking.to_owned(),
        NetworkKind::RenameInterfaces => n.op_rename_interfaces.to_owned(),
        NetworkKind::NewVif { vm } => fill(n.op_new_vif, &[("vm", vm)]),
        NetworkKind::DeleteVif { network, vm } => {
            fill(n.op_delete_vif, &[("network", network), ("vm", vm)])
        }
        NetworkKind::UpdateVif { network, vm } => {
            fill(n.op_update_vif, &[("network", network), ("vm", vm)])
        }
        NetworkKind::PlugVif { vm } => fill(n.op_plug_vif, &[("vm", vm)]),
        NetworkKind::UnplugVif { vm } => fill(n.op_unplug_vif, &[("vm", vm)]),
    }
}

/// The title of a storage repository operation.
fn storage_title(kind: &xen_core::operations::StorageKind) -> String {
    use xen_core::operations::StorageKind;
    use xen_core::storage::SrAction;
    let s = crate::sr_strings::sr_strings();
    match kind {
        StorageKind::CreateSr {
            sr_type,
            name,
            pool,
        } => {
            let type_name =
                crate::tabs::general::sr_type_label(&xen_core::ConnectionModel::default(), sr_type);
            fill(
                s.op_create,
                &[("type", &type_name), ("name", name), ("pool", pool)],
            )
        }
        StorageKind::AttachSr { name, pool } => {
            fill(s.op_attach, &[("name", name), ("pool", pool)])
        }
        StorageKind::Sr { action, name, pool } => {
            let template = match action {
                SrAction::Repair => s.op_repair,
                SrAction::Detach => s.op_detach,
                SrAction::Forget => s.op_forget,
                SrAction::Destroy => s.op_destroy,
                SrAction::SetDefault => s.op_set_default,
                SrAction::Rescan => s.op_rescan,
                SrAction::Trim => s.op_trim,
            };
            fill(template, &[("name", name), ("pool", pool)])
        }
        StorageKind::SrProperties { name } => fill(s.op_properties, &[("name", name)]),
    }
}

/// The title of a VM creation, template, snapshot or disk operation.
fn provision_title(kind: &ProvisionKind) -> String {
    let p = crate::prov_strings::prov_strings();
    match kind {
        ProvisionKind::CreateVm { vm, .. } => fill(p.op_create_vm, &[("vm", vm)]),
        ProvisionKind::CloneVm { vm, source } => {
            fill(p.op_clone_vm, &[("vm", vm), ("source", source)])
        }
        ProvisionKind::CopyVm { vm, source, sr } => {
            fill(p.op_copy_vm, &[("vm", vm), ("source", source), ("sr", sr)])
        }
        ProvisionKind::MakeTemplate { vm } => fill(p.op_make_template, &[("vm", vm)]),
        ProvisionKind::DeleteVm { vm, location } => {
            fill(p.op_delete_vm, &[("vm", vm), ("location", location)])
        }
        ProvisionKind::Snapshot { vm } => fill(p.op_snapshot, &[("vm", vm)]),
        ProvisionKind::Revert { snapshot } => fill(p.op_revert, &[("snapshot", snapshot)]),
        ProvisionKind::DeleteSnapshots { names } => match names.as_slice() {
            [one] => fill(p.op_delete_snapshot, &[("snapshot", one)]),
            _ => p.op_delete_snapshots.to_owned(),
        },
        ProvisionKind::NewDisk { disk, sr } => fill(p.op_new_disk, &[("disk", disk), ("sr", sr)]),
        ProvisionKind::AttachDisk { disk, vm } => {
            fill(p.op_attach_disk, &[("disk", disk), ("vm", vm)])
        }
        ProvisionKind::DetachDisk { disk, vm } => {
            fill(p.op_detach_disk, &[("disk", disk), ("vm", vm)])
        }
        ProvisionKind::DeleteDisk { disk, sr } => {
            fill(p.op_delete_disk, &[("disk", disk), ("sr", sr)])
        }
        ProvisionKind::ResizeDisk { disk } => fill(p.op_resize_disk, &[("disk", disk)]),
        ProvisionKind::MoveDisk { disk, from, to } => fill(
            p.op_move_disk,
            &[("disk", disk), ("from", from), ("to", to)],
        ),
        ProvisionKind::ChangeCd { vm, iso } => match iso {
            Some(iso) => fill(p.op_load_iso, &[("iso", iso), ("vm", vm)]),
            None => fill(p.op_eject_iso, &[("vm", vm)]),
        },
        ProvisionKind::NewCdDrive { vm } => fill(p.op_new_dvd, &[("vm", vm)]),
        ProvisionKind::InstallTools { vm } => {
            fill(p.op_install_tools, &[("tools", p.tools_name), ("vm", vm)])
        }
    }
}

/// What a running operation is doing now.
pub fn operation_step(step: &OperationStep) -> String {
    let o = ops_strings();
    match step {
        OperationStep::ShuttingDownVm {
            vm_name,
            index,
            count,
            for_reboot,
        } => fill(
            if *for_reboot {
                o.step_reboot_vm_shutdown
            } else {
                o.step_shutdown_vm_shutdown
            },
            &[
                ("vm", vm_name),
                ("index", &index.to_string()),
                ("count", &count.to_string()),
            ],
        ),
        OperationStep::Disabling => o.step_disabling.to_owned(),
        OperationStep::Evacuating => o.step_evacuating.to_owned(),
        OperationStep::Reenabling => o.step_reenabling.to_owned(),
        step => provision_step(step),
    }
}

fn provision_step(step: &OperationStep) -> String {
    let p = crate::prov_strings::prov_strings();
    match step {
        OperationStep::CloningTemplate { template, sr } => match sr {
            Some(sr) => fill(p.step_copying, &[("template", template), ("sr", sr)]),
            None => fill(p.step_cloning, &[("template", template)]),
        },
        OperationStep::Provisioning => p.step_provisioning.to_owned(),
        OperationStep::SettingProperties => p.step_properties.to_owned(),
        OperationStep::CreatingCdDrive => p.step_cd_drive.to_owned(),
        OperationStep::CreatingDisks => p.step_disks.to_owned(),
        OperationStep::CreatingNetworks => p.step_networks.to_owned(),
        OperationStep::AttachingVtpm => p.step_vtpm.to_owned(),
        OperationStep::StartingVm => p.step_starting.to_owned(),
        OperationStep::RollingBack => p.step_rolling_back.to_owned(),
        OperationStep::RevertingPowerState { vm } => fill(p.step_revert_power, &[("vm", vm)]),
        step => storage_step(step),
    }
}

fn storage_step(step: &OperationStep) -> String {
    let s = crate::sr_strings::sr_strings();
    match step {
        OperationStep::CreatingSr => s.step_creating.to_owned(),
        OperationStep::AttachingSr => s.step_attaching.to_owned(),
        OperationStep::CreatingPbd { host } => fill(s.step_create_pbd, &[("host", host)]),
        OperationStep::PluggingPbd { host } => fill(s.step_plug_pbd, &[("host", host)]),
        OperationStep::ReclaimingSpace => s.step_trim.to_owned(),
        OperationStep::UndoingSr => s.step_undo.to_owned(),
        step => network_step(step),
    }
}

fn network_step(step: &OperationStep) -> String {
    let n = crate::net_strings::net_strings();
    match step {
        OperationStep::CreatingNetwork => n.step_creating.to_owned(),
        OperationStep::RemovingNetwork => n.step_removing.to_owned(),
        OperationStep::UpdatingNetwork => n.step_updating.to_owned(),
        OperationStep::BringingUp { pif } => fill(n.step_bringing_up, &[("pif", pif)]),
        OperationStep::Depurposing { pif } => fill(n.step_depurposing, &[("pif", pif)]),
        OperationStep::ManagementReconfiguring { pif } => fill(n.step_management, &[("pif", pif)]),
        OperationStep::BringingDown { pif } => fill(n.step_bringing_down, &[("pif", pif)]),
        OperationStep::WaitingForMembers => n.step_waiting_members.to_owned(),
        OperationStep::ScanningNics => n.step_scanning.to_owned(),
        OperationStep::RefreshingNetworks => n.step_refreshing.to_owned(),
        OperationStep::UndoingNetwork => n.step_undo.to_owned(),
        step => pool_step(step),
    }
}

fn pool_step(step: &OperationStep) -> String {
    let p = crate::pool_strings::pool_strings();
    match step {
        OperationStep::CreatingPool => p.step_creating_pool.to_owned(),
        OperationStep::AddingMembers => p.step_adding_members.to_owned(),
        OperationStep::Joining => p.step_joining.to_owned(),
        OperationStep::Ejecting => p.step_ejecting.to_owned(),
        OperationStep::UndoingPool => p.step_undoing_pool.to_owned(),
        OperationStep::NewCoordinator { host } => fill(p.step_new_coordinator, &[("host", host)]),
        OperationStep::SavingSettings => p.step_saving.to_owned(),
        OperationStep::BackingUp { received } => fill(
            p.step_backing_up,
            &[("received", &xen_core::format::memory_size(*received, false))],
        ),
        OperationStep::WaitingForCertificate => p.step_certificate.to_owned(),
        OperationStep::SettingHaPriority { vm } => fill(
            crate::ha_strings::ha_strings().step_priority_on,
            &[("vm", vm)],
        ),
        OperationStep::LoweringNtol => crate::ha_strings::ha_strings()
            .step_lowering_ntol
            .to_owned(),
        OperationStep::UnprotectingVm { vm } => fill(
            crate::ha_strings::ha_strings().step_unprotect,
            &[
                ("vm", vm),
                (
                    "priority",
                    crate::ha_strings::priority_name(xen_core::ha::RestartPriority::DoNotRestart),
                ),
            ],
        ),
        OperationStep::JoiningDomain => crate::ha_strings::ha_strings()
            .step_joining_domain
            .to_owned(),
        _ => String::new(),
    }
}

/// What the user must know after an operation succeeded.
pub fn operation_notice(notice: xen_core::operations::OperationNotice) -> &'static str {
    let p = crate::prov_strings::prov_strings();
    match notice {
        xen_core::operations::OperationNotice::RestartForDisk => p.restart_for_disk,
        xen_core::operations::OperationNotice::RestartForDrive => p.restart_for_drive,
        xen_core::operations::OperationNotice::RestartForVif => {
            crate::net_strings::net_strings().hotplug_text
        }
    }
}

/// Status word of an operation.
pub fn operation_status_label(status: &OperationStatus) -> &'static str {
    let s = strings();
    match status {
        OperationStatus::Running => s.op_running,
        OperationStatus::Succeeded => s.op_succeeded,
        OperationStatus::Cancelled => s.op_cancelled,
        OperationStatus::Failed(_) => s.op_failed,
    }
}

/// Why an operation failed (`conn`: the connection it ran on, if still
/// there, for object names and roles).
pub fn describe_operation_failure(
    conn: Option<&ConnectionView>,
    failure: &OperationFailure,
) -> String {
    let s = strings();
    match failure {
        OperationFailure::Xapi(e) => describe_xapi_error_in(conn, e),
        OperationFailure::Connection(e) => describe_connection_error(e),
        OperationFailure::NotConnected => s.op_not_connected.to_owned(),
        OperationFailure::Disconnected => s.op_disconnected.to_owned(),
    }
}

/// Status word of a XAPI task.
pub fn task_status_label(status: &xapi::TaskStatusType) -> &str {
    let s = strings();
    match status {
        xapi::TaskStatusType::Pending => s.task_pending,
        xapi::TaskStatusType::Success => s.task_success,
        xapi::TaskStatusType::Failure => s.task_failure,
        xapi::TaskStatusType::Cancelling => s.task_cancelling,
        xapi::TaskStatusType::Cancelled => s.task_cancelled,
        xapi::TaskStatusType::Unknown(other) => other,
    }
}

/// A short duration: `5 s`, `3 min`, `2 h`, `4 d`.
pub fn short_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86400 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// Label of a VM action (its menu item).
pub fn action_label(action: &VmAction) -> &'static str {
    let o = ops_strings();
    match action {
        VmAction::Start => strings().action_start,
        VmAction::CleanShutdown => strings().action_clean_shutdown,
        VmAction::CleanReboot => o.reboot,
        VmAction::Suspend => o.suspend,
        VmAction::Resume => o.resume,
        VmAction::Pause => o.pause,
        VmAction::Unpause => o.unpause,
        VmAction::HardShutdown => o.force_shut_down,
        VmAction::HardReboot => o.force_reboot,
        VmAction::StartOn(_) => o.start_on_server,
        VmAction::ResumeOn(_) => o.resume_on_server,
        VmAction::Migrate(_) => o.migrate_to_server,
    }
}

/// Label of a host action (its menu item).
pub fn host_action_label(action: HostAction) -> &'static str {
    let o = ops_strings();
    match action {
        HostAction::Reboot => o.reboot,
        HostAction::Shutdown => o.shut_down,
        HostAction::PowerOn => o.power_on,
        HostAction::EnterMaintenance => o.enter_maintenance,
        HostAction::ExitMaintenance => o.exit_maintenance,
        HostAction::RestartToolstack => o.restart_toolstack,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders() {
        assert_eq!(
            fill("{a} and {b}, {a}", &[("a", "x"), ("b", "y")]),
            "x and y, x"
        );
    }

    #[test]
    fn friendly_api_errors() {
        let e = xapi::Error::Api {
            code: codes::HOST_IS_SLAVE.into(),
            params: vec!["10.0.0.9".into()],
        };
        assert!(describe_xapi_error(&e).contains("10.0.0.9"));
        let e = xapi::Error::Api {
            code: "SOMETHING_NEW".into(),
            params: vec!["p".into()],
        };
        assert_eq!(describe_xapi_error(&e), "SOMETHING_NEW (p)");
        // The original's wording, parameters filled in.
        let e = xapi::Error::Api {
            code: codes::VM_BAD_POWER_STATE.into(),
            params: vec!["OpaqueRef:vm".into(), "running".into(), "halted".into()],
        };
        assert_eq!(
            describe_xapi_error(&e),
            "You attempted an operation on a VM that needed it to be in state 'running' but was in state 'halted'."
        );
        let e = xapi::Error::Api {
            code: "HOST_DISABLED".into(),
            params: vec!["OpaqueRef:h".into()],
        };
        assert_eq!(short_error(&e), "Disabled");
        // Every entry is reachable by binary search.
        for (code, _) in FRIENDLY_ERRORS {
            assert!(friendly_error(code, &[]).is_some(), "{code}");
        }
    }

    #[test]
    fn durations_and_titles() {
        assert_eq!(short_duration(Duration::from_secs(5)), "5 s");
        assert_eq!(short_duration(Duration::from_secs(185)), "3 min");
        assert_eq!(short_duration(Duration::from_secs(7300)), "2 h");
        let kind = OperationKind::VmAction {
            vm_name: "web".into(),
            action: VmAction::Start,
            location: "h1".into(),
            from: None,
            target: None,
        };
        assert_eq!(operation_title(&kind, "h"), "Starting VM 'web'");
        let kind = OperationKind::VmAction {
            vm_name: "web".into(),
            action: VmAction::Migrate(xapi::Ref::new("OpaqueRef:h2")),
            location: "h1".into(),
            from: Some("h1".into()),
            target: Some("h2".into()),
        };
        assert_eq!(
            operation_title(&kind, "h"),
            "Migrating VM 'web' from 'h1' to 'h2'"
        );
        assert!(
            describe_connection_error(&ConnectionError::AlreadyConnected {
                existing: "10.0.0.1".into()
            })
            .contains("10.0.0.1")
        );
    }
}
