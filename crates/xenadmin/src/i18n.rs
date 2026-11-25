//! User-interface strings.
//!
//! Every user-visible string lives in a [`Strings`] table so the UI can be
//! translated later by adding another table and selecting it in [`strings`].
//! Only English exists today. Templates use `{name}` placeholders filled by
//! [`fill`].

use xapi::error::codes;
use xen_core::{ConnectionError, VmAction};

/// One language's UI strings.
pub struct Strings {
    pub menu_file: &'static str,
    pub menu_connect: &'static str,
    pub menu_disconnect: &'static str,
    pub menu_quit: &'static str,
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
    pub details_description: &'static str,
    pub details_uuid: &'static str,
    pub details_power_state: &'static str,
    pub details_address: &'static str,
    pub details_version: &'static str,
    pub details_coordinator: &'static str,
    pub details_enabled: &'static str,
    pub details_disabled: &'static str,
    pub details_busy: &'static str,
    pub details_hosts: &'static str,
    pub details_vms: &'static str,

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

    /// `{action}`, `{vm}`, `{error}`
    pub vm_action_failed: &'static str,

    pub err_cert_rejected: &'static str,
    /// `{error}`
    pub err_connect: &'static str,
    /// `{error}`
    pub err_lost: &'static str,
    pub err_auth_failed: &'static str,
    /// `{coordinator}`
    pub err_host_is_member: &'static str,
    pub err_permission_denied: &'static str,
    pub err_host_booting: &'static str,
    pub err_bad_power_state: &'static str,
    pub err_session_invalid: &'static str,
    /// `{error}`
    pub err_network: &'static str,
}

/// English strings.
pub static EN: Strings = Strings {
    menu_file: "File",
    menu_connect: "Connect…",
    menu_disconnect: "Disconnect",
    menu_quit: "Quit",
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
    details_description: "Description",
    details_uuid: "UUID",
    details_power_state: "Power state",
    details_address: "Address",
    details_version: "Version",
    details_coordinator: "Coordinator",
    details_enabled: "Enabled",
    details_disabled: "Disabled (maintenance)",
    details_busy: "Operation in progress…",
    details_hosts: "Hosts",
    details_vms: "VMs",

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

    vm_action_failed: "{action} of {vm} failed: {error}",

    err_cert_rejected: "Connection cancelled: certificate not trusted.",
    err_connect: "Could not connect: {error}",
    err_lost: "Connection lost: {error}",
    err_auth_failed: "Authentication failed. Check the user name and password.",
    err_host_is_member: "This host is a pool member. Connect to the pool coordinator ({coordinator}) instead.",
    err_permission_denied: "You do not have permission to perform this operation.",
    err_host_booting: "The host is still booting. Try again shortly.",
    err_bad_power_state: "The VM is not in the right power state for this operation.",
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
    let s = strings();
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
            codes::RBAC_PERMISSION_DENIED => s.err_permission_denied.to_owned(),
            codes::HOST_STILL_BOOTING => s.err_host_booting.to_owned(),
            codes::VM_BAD_POWER_STATE => s.err_bad_power_state.to_owned(),
            codes::SESSION_INVALID => s.err_session_invalid.to_owned(),
            _ => error.to_string(),
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
        ConnectionError::Connect(e) | ConnectionError::Unsupported(e) => {
            fill(s.err_connect, &[("error", &describe_xapi_error(e))])
        }
        ConnectionError::Lost(e) => fill(s.err_lost, &[("error", &describe_xapi_error(e))]),
    }
}

/// Label of a VM action.
pub fn action_label(action: VmAction) -> &'static str {
    match action {
        VmAction::Start => strings().action_start,
        VmAction::CleanShutdown => strings().action_clean_shutdown,
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
    }
}
