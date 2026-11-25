//! The Properties dialog of a pool or a server: which pages it shows
//! (`PropertiesDialog.Build`), what each page reads and how it checks its
//! input, and the changes OK saves ([`PropertyChanges`], run by
//! [`crate::pool_run`]).
//!
//! Pages (in the original's order) and what they edit:
//!
//! * **General** (`GeneralEditPage`): name, description, tags, and for a
//!   server its iSCSI IQN;
//! * **Alerts** (server, `PerfmonAlertEditPage`): see [`crate::perfmon`];
//! * **Email Notifications** (pool or standalone server,
//!   `PerfmonAlertOptionsPage`): the pool's `mail-destination`,
//!   `ssmtp-mailhub` and `mail-language`;
//! * **Multipathing** (server, `HostMultipathPage`), only in maintenance
//!   mode: PBDs unplugged, `host.set_multipathing`, PBDs plugged again;
//! * **Log Destination** (server, `LogDestinationEditPage`): the remote
//!   syslog server (`logging["syslog_destination"]`), then
//!   `host.syslog_reconfigure`;
//! * **Power On** (pool and server, `HostPowerONEditPage`): each server's
//!   power-on mode (disabled, Wake-on-LAN, DRAC, a custom script) with its
//!   configuration; a password goes to a `secret`;
//! * **Auto-start on server boot** (server, `HostAutostartEditPage`): the
//!   pool's `other_config["auto_poweron"]`;
//! * **Live Patching** (pool or standalone server before XCP-ng 8.3,
//!   `LivePatchingEditPage`);
//! * **Network** (pool or standalone server on Open vSwitch,
//!   `NetworkOptionsEditPage`): IGMP snooping;
//! * **Clustering** (pool or standalone server, unless the licence forbids
//!   it, `ClusteringEditPage`);
//! * **Advanced** (pool, XCP-ng 8.3, `PoolAdvancedEditPage`): migration
//!   compression;
//! * **NRPE** (pool or standalone server, XAPI 23.27 or later, for the
//!   superuser or a pool administrator, `NRPEEditPage`): read with the
//!   `nrpe` plugin when the dialog opens.
//!
//! Left out: Custom Fields (later), GPU (no GPU support yet) and the
//! Security page (only for servers older than XCP-ng 8.2).

use std::collections::BTreeMap;

use xapi::{Host, HostRecord, Network, Pool, PoolRecord, Ref};

use crate::model::ConnectionModel;
use crate::objects;
use crate::pool::{self, Restriction};
use crate::rbac::SessionRbac;

/// The object a Properties dialog edits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PropertiesTarget {
    Pool(Ref<Pool>),
    Host(Ref<Host>),
}

/// The pages of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    General,
    Alerts,
    EmailOptions,
    Multipath,
    LogDestination,
    PowerOn,
    Autostart,
    LivePatching,
    NetworkOptions,
    Clustering,
    Advanced,
    Nrpe,
}

/// `isPoolOrStandalone`: a pool, or a server that is not in one.
pub fn is_pool_or_standalone(model: &ConnectionModel, target: &PropertiesTarget) -> bool {
    match target {
        PropertiesTarget::Pool(_) => true,
        PropertiesTarget::Host(_) => pool::visible_pool(model).is_none(),
    }
}

/// The pages shown for `target`, in order (`PropertiesDialog.Build`).
pub fn pages(
    model: &ConnectionModel,
    target: &PropertiesTarget,
    rbac: Option<&SessionRbac>,
) -> Vec<Page> {
    let is_host = matches!(target, PropertiesTarget::Host(_));
    let is_pool = !is_host;
    let pool_or_standalone = is_pool_or_standalone(model, target);
    let coordinator = model.coordinator().map(|h| &**h);
    let mut out = vec![Page::General];
    if is_host {
        out.push(Page::Alerts);
    }
    if pool_or_standalone {
        out.push(Page::EmailOptions);
    }
    if is_host {
        out.extend([Page::Multipath, Page::LogDestination]);
    }
    out.push(Page::PowerOn);
    if is_host {
        out.push(Page::Autostart);
    }
    if pool_or_standalone
        && !pool::feature_forbidden(model, Restriction::LivePatching)
        && !pool::cloud_or_greater(coordinator)
    {
        out.push(Page::LivePatching);
    }
    if pool_or_standalone
        && !pool::feature_forbidden(model, Restriction::IgmpSnooping)
        && coordinator.is_some_and(|c| {
            c.software_version
                .get("network_backend")
                .map(String::as_str)
                == Some("openvswitch")
        })
    {
        out.push(Page::NetworkOptions);
    }
    if pool_or_standalone && !pool::feature_forbidden(model, Restriction::Corosync) {
        out.push(Page::Clustering);
    }
    if is_pool && pool::cloud_or_greater(coordinator) && pool::xapi_22_33_or_greater(coordinator) {
        out.push(Page::Advanced);
    }
    let pool_admin =
        rbac.is_none_or(|r| r.is_local_superuser || r.permissions.contains("pool-admin"));
    if pool_or_standalone && pool::xapi_23_27_or_greater(coordinator) && pool_admin {
        out.push(Page::Nrpe);
    }
    out
}

// ---- General ------------------------------------------------------------------

/// `Helpers.ValidateIscsiIQN`:
/// `^iqn\.\d{4}-\d{2}\.([a-zA-Z0-9][-_a-zA-Z0-9]*(\.[a-zA-Z0-9][-_a-zA-Z0-9]*)*)(:.+)?$`.
pub fn is_valid_iqn(iqn: &str) -> bool {
    let Some(rest) = iqn.strip_prefix("iqn.") else {
        return false;
    };
    let b = rest.as_bytes();
    if b.len() < 8
        || !b[..4].iter().all(u8::is_ascii_digit)
        || b[4] != b'-'
        || !b[5..7].iter().all(u8::is_ascii_digit)
        || b[7] != b'.'
    {
        return false;
    }
    let rest = &rest[8..];
    let (name, suffix) = match rest.split_once(':') {
        Some((n, s)) => (n, Some(s)),
        None => (rest, None),
    };
    if suffix.is_some_and(str::is_empty) {
        return false;
    }
    !name.is_empty()
        && name.split('.').all(|label| {
            let mut chars = label.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// The server's iSCSI IQN (`Host.GetIscsiIqn`).
pub fn host_iqn(host: &HostRecord) -> &str {
    if host.iscsi_iqn.is_empty() {
        host.other_config
            .get("iscsi_iqn")
            .map(String::as_str)
            .unwrap_or_default()
    } else {
        &host.iscsi_iqn
    }
}

/// Splits the tags box ("a, b") into tags.
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for t in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !tags.iter().any(|x| x == t) {
            tags.push(t.to_owned());
        }
    }
    tags
}

// ---- Email Notifications --------------------------------------------------------

/// `Pool.MAIL_DESTINATION_KEY_NAME` and friends.
pub const MAIL_DESTINATION: &str = "mail-destination";
pub const SMTP_MAILHUB: &str = "ssmtp-mailhub";
pub const MAIL_LANGUAGE: &str = "mail-language";

/// The mail languages offered (`MAIL_LANGUAGE_*_CODE`), English first.
pub const MAIL_LANGUAGES: &[&str] = &["en-US", "zh-CN", "ja-JP"];

/// The pool's email settings (`None` fields: not set).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct EmailSettings {
    pub destination: Option<String>,
    /// `server:port`.
    pub mailhub: Option<String>,
    pub language: Option<String>,
}

/// The page's form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailForm {
    pub enabled: bool,
    pub address: String,
    pub server: String,
    pub port: String,
    /// Index in [`MAIL_LANGUAGES`].
    pub language: usize,
}

impl EmailForm {
    /// `PerfmonAlertOptionsPage.Populate`.
    pub fn of(pool: &PoolRecord) -> Self {
        let get = |k: &str| pool.other_config.get(k).map(|v| v.trim().to_owned());
        let destination = get(MAIL_DESTINATION).filter(|v| !v.is_empty());
        let hub = get(SMTP_MAILHUB).filter(|v| !v.is_empty());
        let (server, port) = match &hub {
            Some(h) => match h.split_once(':') {
                Some((s, p)) => (s.to_owned(), p.split(':').next().unwrap_or("").to_owned()),
                None => (h.clone(), "25".to_owned()),
            },
            None => (String::new(), "25".to_owned()),
        };
        let language = get(MAIL_LANGUAGE)
            .and_then(|code| {
                MAIL_LANGUAGES
                    .iter()
                    .position(|l| l.eq_ignore_ascii_case(&code))
            })
            .unwrap_or(0);
        Self {
            enabled: destination.is_some() && hub.is_some(),
            address: destination.unwrap_or_default(),
            server,
            port,
            language,
        }
    }

    /// `ValidToSave`.
    pub fn is_valid(&self) -> bool {
        !self.enabled
            || (is_valid_email(&self.address)
                && is_valid_port(&self.port)
                && !self.server.trim().is_empty()
                && self.server.is_ascii())
    }

    /// What `PerfmonOptionsDefinitionAction` writes.
    pub fn settings(&self) -> EmailSettings {
        if !self.enabled {
            return EmailSettings::default();
        }
        EmailSettings {
            destination: Some(self.address.clone()),
            mailhub: Some(format!("{}:{}", self.server, self.port)),
            language: MAIL_LANGUAGES.get(self.language).map(|l| (*l).to_owned()),
        }
    }
}

/// `Util.IsValidPort`.
pub fn is_valid_port(s: &str) -> bool {
    s.parse::<i64>().is_ok_and(|p| p > 0 && p <= 65535)
}

/// The original's (unanchored) email check:
/// `\w+([-+.]\w+)*@\w+([-.]\w+)*\.\w+([-.]\w+)*`.
pub fn is_valid_email(s: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    s.char_indices().filter(|(_, c)| *c == '@').any(|(i, _)| {
        let before = s[..i].chars().next_back().is_some_and(word);
        let after = &s[i + 1..];
        let domain: String = after
            .chars()
            .take_while(|c| word(*c) || *c == '-' || *c == '.')
            .collect();
        let starts = domain.chars().next().is_some_and(word);
        let dotted = domain
            .char_indices()
            .any(|(j, c)| c == '.' && j > 0 && domain[j + 1..].chars().next().is_some_and(word));
        before && starts && dotted
    })
}

// ---- Log Destination ---------------------------------------------------------------

/// The remote syslog server, if any (`Host.GetSysLogDestination`).
pub fn syslog_destination(host: &HostRecord) -> Option<&str> {
    host.logging
        .get("syslog_destination")
        .map(String::as_str)
        .filter(|d| !d.trim().is_empty())
}

/// `LogDestinationEditPage`'s host name check:
/// `^[a-zA-Z0-9]([-a-zA-Z0-9]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([-a-zA-Z0-9]{0,61}[a-zA-Z0-9])?)*$`.
pub fn is_valid_log_server(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|label| {
            let b = label.as_bytes();
            !b.is_empty()
                && b.len() <= 63
                && b[0].is_ascii_alphanumeric()
                && b[b.len() - 1].is_ascii_alphanumeric()
                && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        })
}

// ---- Multipathing ----------------------------------------------------------------

/// `Host.MultipathEnabled`.
pub fn multipath_enabled(host: &HostRecord) -> bool {
    host.multipathing
        || host
            .other_config
            .get("multipathing")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// `HostMultipathPage.MaintenanceMode`: the setting can only change in
/// maintenance mode (or while the server is down).
pub fn multipath_editable(model: &ConnectionModel, host: &HostRecord) -> bool {
    objects::host_maintenance_flag(host) || !objects::host_is_live(model, host)
}

// ---- Auto-start -------------------------------------------------------------------

/// `Pool.GetVmAutostartEnabled`.
pub fn autostart_enabled(pool: &PoolRecord) -> bool {
    pool.other_config.get("auto_poweron").map(String::as_str) == Some("true")
}

// ---- Power On ---------------------------------------------------------------------

/// `power_on_config` keys.
pub const POWER_ON_IP: &str = "power_on_ip";
pub const POWER_ON_USER: &str = "power_on_user";
pub const POWER_ON_PASSWORD_SECRET: &str = "power_on_password_secret";

/// A server's power-on mode (`Host.PowerOnMode`). iLO is not offered:
/// XCP-ng 8.2 and later do not support it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PowerOnMode {
    Disabled,
    WakeOnLan,
    Drac {
        ip: String,
        user: String,
        /// The password as typed, saved in a `secret` on OK.
        password: String,
    },
    /// A custom script and its configuration (a `power_on_password_secret`
    /// value is a password, saved in a `secret`).
    Custom {
        mode: String,
        config: BTreeMap<String, String>,
    },
}

impl PowerOnMode {
    /// `PowerOnMode.Create` + `Load`; `secret` is the password behind
    /// `power_on_password_secret` when it could be read.
    pub fn of(host: &HostRecord, secret: Option<&str>) -> Self {
        let get = |k: &str| host.power_on_config.get(k).cloned().unwrap_or_default();
        match host.power_on_mode.as_str() {
            "" | "iLO" => Self::Disabled,
            "wake-on-lan" => Self::WakeOnLan,
            "DRAC" => Self::Drac {
                ip: get(POWER_ON_IP),
                user: get(POWER_ON_USER),
                password: secret.unwrap_or_default().to_owned(),
            },
            mode => Self::Custom {
                mode: mode.to_owned(),
                config: host
                    .power_on_config
                    .iter()
                    .map(|(k, v)| {
                        let v = if k == POWER_ON_PASSWORD_SECRET {
                            secret.unwrap_or_default().to_owned()
                        } else {
                            v.clone()
                        };
                        (k.clone(), v)
                    })
                    .collect(),
            },
        }
    }

    /// The `power_on_mode` value.
    pub fn mode_name(&self) -> &str {
        match self {
            Self::Disabled => "",
            Self::WakeOnLan => "wake-on-lan",
            Self::Drac { .. } => "DRAC",
            Self::Custom { mode, .. } => mode,
        }
    }

    /// The `power_on_config` value, with the password as typed.
    pub fn config(&self) -> BTreeMap<String, String> {
        match self {
            Self::Disabled | Self::WakeOnLan => BTreeMap::new(),
            Self::Drac { ip, user, password } => BTreeMap::from([
                (POWER_ON_IP.to_owned(), ip.clone()),
                (POWER_ON_USER.to_owned(), user.clone()),
                (POWER_ON_PASSWORD_SECRET.to_owned(), password.clone()),
            ]),
            Self::Custom { config, .. } => config.clone(),
        }
    }

    /// `HostPowerONEditPage.HasHostChanged` (a password is compared as
    /// read, so an unchanged mode with an unreadable secret counts as
    /// changed only when the user typed one).
    pub fn differs_from(&self, host: &HostRecord, secret: Option<&str>) -> bool {
        *self != Self::of(host, secret)
    }

    /// `ValidToSave`: DRAC needs an IP address, a custom mode a name.
    pub fn problem(&self) -> Option<PowerOnProblem> {
        match self {
            Self::Drac { ip, .. } if !crate::network::is_ip_address(ip) => {
                Some(PowerOnProblem::InvalidIp)
            }
            Self::Custom { mode, .. } if mode.trim().is_empty() => Some(PowerOnProblem::NoScript),
            _ => None,
        }
    }
}

/// Why the Power On page cannot be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerOnProblem {
    /// "IP address is invalid".
    InvalidIp,
    /// "Please specify a script name".
    NoScript,
}

// ---- Clustering -------------------------------------------------------------------

/// What the Clustering page changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClusteringChange {
    /// `EnableClusteringAction` on this network.
    Enable(Ref<Network>),
    /// `DisableClusteringAction`.
    Disable,
}

/// Why the Clustering page's controls are disabled
/// (`ClusteringEditPage.SetPage` / `LoadNetworks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusteringProblem {
    /// "Clustering cannot be enabled because there are no suitable networks."
    NoNetwork,
    /// "...thinly provisioned (GFS2) storage repositories attached..."
    Gfs2Attached,
    /// "You must disable HA before clustering can be enabled."
    HaEnabled,
    /// "You cannot enable clustering while a pool secret rotation is in
    /// progress."
    SecretRotationPending,
}

/// The networks clustering can use: the pool's networks whose PIFs have an
/// IP address (`ExcludeNetworksWithoutIpAddresses`), by name.
pub fn cluster_networks(model: &ConnectionModel) -> Vec<(&Ref<Network>, String)> {
    let mut v: Vec<(&Ref<Network>, String)> = model
        .networks
        .iter()
        .filter(|(_, n)| objects::network_is_shown(model, n, false))
        .filter(|(_, n)| {
            n.pifs
                .iter()
                .filter_map(|p| model.pifs.get(p))
                .any(|p| objects::pif_friendly_ip(p).is_some())
        })
        .map(|(r, n)| (r, objects::network_name(model, n)))
        .collect();
    v.sort_by(|a, b| objects::natural_cmp(&a.1, &b.1));
    v
}

/// The page's state: whether clustering is on, and what keeps the
/// controls disabled.
pub fn clustering_problem(model: &ConnectionModel) -> Option<ClusteringProblem> {
    let (_, pool) = model.pool()?;
    if !model.clusters.is_empty() {
        let gfs2 = model
            .srs
            .values()
            .any(|sr| sr.r#type == "gfs2" && !objects::sr_is_detached(model, sr));
        return gfs2.then_some(ClusteringProblem::Gfs2Attached);
    }
    if cluster_networks(model).is_empty() {
        return Some(ClusteringProblem::NoNetwork);
    }
    if pool.ha_enabled {
        return Some(ClusteringProblem::HaEnabled);
    }
    if !pool::feature_forbidden(model, Restriction::PoolSecretRotation) && pool.is_psr_pending {
        return Some(ClusteringProblem::SecretRotationPending);
    }
    None
}

// ---- NRPE ----------------------------------------------------------------------------

/// `NRPEHostConfiguration` plugin calls.
pub const NRPE_PLUGIN: &str = "nrpe";

/// A check and its label kind, in the page's order
/// (`NRPEEditPage` constructor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NrpeCheckKind {
    /// Warning below critical.
    Plain,
    /// Free space: warning above critical.
    Free,
    /// Three comma-separated numbers (load averages).
    Dom0Load,
}

/// The NRPE checks: name and kind.
pub const NRPE_CHECKS: &[(&str, NrpeCheckKind)] = &[
    ("check_host_load", NrpeCheckKind::Plain),
    ("check_host_cpu", NrpeCheckKind::Plain),
    ("check_host_memory", NrpeCheckKind::Plain),
    ("check_vgpu", NrpeCheckKind::Plain),
    ("check_vgpu_memory", NrpeCheckKind::Plain),
    ("check_load", NrpeCheckKind::Dom0Load),
    ("check_cpu", NrpeCheckKind::Plain),
    ("check_memory", NrpeCheckKind::Plain),
    ("check_swap", NrpeCheckKind::Free),
    ("check_disk_root", NrpeCheckKind::Free),
    ("check_disk_log", NrpeCheckKind::Free),
];

/// A server's NRPE configuration (`NRPEHostConfiguration`). `allow_hosts`
/// is empty when none are set (the page shows its placeholder).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NrpeConfig {
    pub enabled: bool,
    pub allow_hosts: String,
    pub debug: bool,
    pub ssl_logging: bool,
    /// Check name → (warning, critical).
    pub thresholds: BTreeMap<String, (String, String)>,
}

/// `NRPEHostConfiguration.DEBUG_ENABLE` and friends.
pub const NRPE_DEBUG_ENABLE: &str = "1";
pub const NRPE_DEBUG_DISABLE: &str = "0";
pub const NRPE_SSL_LOGGING_ENABLE: &str = "0x2f";
pub const NRPE_SSL_LOGGING_DISABLE: &str = "0x00";

/// `AllowHostsWithoutLocalAddress`: drops the local addresses the plugin
/// always adds.
fn without_local_addresses(hosts: &str) -> String {
    hosts
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty() && *h != "127.0.0.1" && *h != "::1")
        .collect::<Vec<_>>()
        .join(",")
}

impl NrpeConfig {
    /// `NRPERetrieveAction`: parses the answers of the plugin's `status`,
    /// `get-config` and `get-threshold`.
    pub fn parse(status: &str, config: &str, thresholds: &str) -> Self {
        let mut out = Self {
            enabled: status.trim() == "active enabled",
            ..Self::default()
        };
        for line in config.lines().map(str::trim) {
            if let Some(v) = line.strip_prefix("allowed_hosts:") {
                out.allow_hosts = without_local_addresses(v.trim());
            } else if let Some(v) = line.strip_prefix("debug:") {
                out.debug = v.trim() == NRPE_DEBUG_ENABLE;
            } else if let Some(v) = line.strip_prefix("ssl_logging:") {
                out.ssl_logging = v.trim() == NRPE_SSL_LOGGING_ENABLE;
            }
        }
        for line in thresholds.lines() {
            // The original takes words 0 (name), 4 (warning) and 8
            // (critical): "check_cpu warning threshold is: 80 critical
            // threshold is: 90".
            let words: Vec<&str> = line.split(' ').collect();
            if let (Some(name), Some(w), Some(c)) = (words.first(), words.get(4), words.get(8))
                && !name.is_empty()
            {
                out.thresholds
                    .insert((*name).to_owned(), ((*w).to_owned(), (*c).to_owned()));
            }
        }
        out
    }

    /// `set-config`'s `allowed_hosts` argument.
    pub fn allowed_hosts_arg(&self) -> String {
        format!("127.0.0.1,::1,{}", self.allow_hosts)
    }
}

/// Why the NRPE page cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NrpeProblem {
    /// "Monitoring servers should not be empty."
    AllowHostsEmpty,
    /// "Monitoring servers should be comma separated IP address or domain
    /// list…"
    AllowHostsFormat,
    /// "Please remove duplicate addresses"
    AllowHostsDuplicate,
    /// A threshold of check `name` is wrong.
    Threshold { name: String, error: ThresholdError },
}

/// What is wrong with a threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdError {
    Empty,
    NotANumber,
    OutOfRange,
    NotThreeNumbers,
    WarningNotBelowCritical,
    WarningNotAboveCritical,
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

fn is_ipv4_cidr(s: &str) -> bool {
    let (ip, bits) = match s.split_once('/') {
        Some((ip, bits)) => (ip, Some(bits)),
        None => (s, None),
    };
    let ip_ok = {
        let parts: Vec<&str> = ip.split('.').collect();
        parts.len() == 4
            && parts
                .iter()
                .all(|p| (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
    };
    ip_ok
        && bits
            .is_none_or(|b| !b.is_empty() && b.len() <= 2 && b.parse::<u8>().is_ok_and(|n| n <= 32))
}

/// `NRPEEditPage.REGEX_DOMAIN`, lower-case names.
fn is_nrpe_domain(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    let n = labels.len();
    labels.iter().enumerate().all(|(i, label)| {
        let body = label
            .strip_prefix("xn--")
            .or_else(|| label.strip_prefix('_'))
            .unwrap_or(label);
        let ok_chars = body
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let last = i + 1 == n;
        !body.is_empty()
            && ok_chars
            && !body.starts_with('-')
            && (last || !body.ends_with('-'))
            && body.len() <= 63
    })
}

/// `NRPEEditPage.IsAllowHostsValid`.
pub fn allow_hosts_problem(text: &str) -> Option<NrpeProblem> {
    if text.trim().is_empty() {
        return Some(NrpeProblem::AllowHostsEmpty);
    }
    let hosts: Vec<&str> = text.split(',').map(str::trim).collect();
    for (i, h) in hosts.iter().enumerate() {
        if !is_ipv4(h) && !is_ipv4_cidr(h) && !is_nrpe_domain(h) {
            return Some(NrpeProblem::AllowHostsFormat);
        }
        if hosts[..i].contains(h) {
            return Some(NrpeProblem::AllowHostsDuplicate);
        }
    }
    None
}

fn number(s: &str) -> Result<f64, ThresholdError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ThresholdError::Empty);
    }
    s.parse::<f64>().map_err(|_| ThresholdError::NotANumber)
}

/// `CheckGroup.CheckValue` for one check.
pub fn threshold_problem(
    kind: NrpeCheckKind,
    warning: &str,
    critical: &str,
) -> Option<ThresholdError> {
    if kind == NrpeCheckKind::Dom0Load {
        let parse = |s: &str| -> Option<Vec<f64>> {
            let v: Vec<&str> = s.split(',').collect();
            (v.len() == 3)
                .then(|| {
                    v.iter()
                        .map(|x| x.trim().parse::<f64>().ok())
                        .collect::<Option<Vec<_>>>()
                })
                .flatten()
        };
        let (Some(w), Some(c)) = (parse(warning), parse(critical)) else {
            return Some(ThresholdError::NotThreeNumbers);
        };
        return w
            .iter()
            .zip(&c)
            .any(|(w, c)| w > c)
            .then_some(ThresholdError::WarningNotBelowCritical);
    }
    let range = |v: f64| (0.01..=100.0).contains(&v);
    let w = match number(warning) {
        Ok(v) if range(v) => v,
        Ok(_) => return Some(ThresholdError::OutOfRange),
        Err(e) => return Some(e),
    };
    let c = match number(critical) {
        Ok(v) if range(v) => v,
        Ok(_) => return Some(ThresholdError::OutOfRange),
        Err(e) => return Some(e),
    };
    match kind {
        NrpeCheckKind::Free if w <= c => Some(ThresholdError::WarningNotAboveCritical),
        NrpeCheckKind::Plain if w >= c => Some(ThresholdError::WarningNotBelowCritical),
        _ => None,
    }
}

/// `NRPEEditPage.ValidToSave`: the first problem, if any.
pub fn nrpe_problem(config: &NrpeConfig) -> Option<NrpeProblem> {
    if !config.enabled {
        return None;
    }
    if let Some(p) = allow_hosts_problem(&config.allow_hosts) {
        return Some(p);
    }
    NRPE_CHECKS.iter().find_map(|(name, kind)| {
        let (w, c) = config.thresholds.get(*name).cloned().unwrap_or_default();
        threshold_problem(*kind, &w, &c).map(|error| NrpeProblem::Threshold {
            name: (*name).to_owned(),
            error,
        })
    })
}

// ---- the changes ------------------------------------------------------------------------

/// What OK saves: only the fields that changed are set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PropertyChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// A server's iSCSI IQN.
    pub iqn: Option<String>,
    /// The server's performance alerts: the new `other_config["perfmon"]`
    /// XML (`Some(None)` removes it).
    pub perfmon: Option<Option<String>>,
    pub email: Option<EmailSettings>,
    pub multipath: Option<bool>,
    /// The remote syslog server (`Some(None)`: local only).
    pub syslog: Option<Option<String>>,
    pub power_on: Vec<(Ref<Host>, PowerOnMode)>,
    pub autostart: Option<bool>,
    pub live_patching_disabled: Option<bool>,
    pub igmp_snooping: Option<bool>,
    pub clustering: Option<ClusteringChange>,
    pub migration_compression: Option<bool>,
    /// The NRPE configuration as edited, and as read when the dialog
    /// opened (only what differs is sent).
    pub nrpe: Option<(NrpeConfig, NrpeConfig)>,
}

impl PropertyChanges {
    /// Nothing to save.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The servers whose Power On settings the page edits (the server, or every
/// server of the pool), by name.
pub fn power_on_hosts<'a>(
    model: &'a ConnectionModel,
    target: &PropertiesTarget,
) -> Vec<(&'a Ref<Host>, &'a HostRecord)> {
    match target {
        PropertiesTarget::Host(h) => model
            .hosts
            .get_key_value(h)
            .map(|(r, h)| vec![(r, &**h)])
            .unwrap_or_default(),
        PropertiesTarget::Pool(_) => objects::sorted_hosts(model)
            .into_iter()
            .map(|(r, h)| (r, &**h))
            .collect(),
    }
}

// ---- Control domain memory -----------------------------------------------------------------

/// `ControlDomainMemoryDialog.MAXIMUM_DOM0_MEMORY_GB`.
pub const MAX_DOM0_MEMORY: i64 = 256 * 1024 * 1024 * 1024;

/// The control domain memory dialog's range and value
/// (`ControlDomainMemoryDialog.Populate`): `(min, max, value)` in bytes.
pub fn dom0_memory_range(model: &ConnectionModel, host: &HostRecord) -> Option<(i64, i64, i64)> {
    let dom0 = objects::control_domain(model, host)?;
    if dom0.memory_dynamic_max < dom0.memory_dynamic_min {
        return None;
    }
    let available = objects::host_memory(model, host).map_or(0, |m| m.available.max(0));
    let mut min = dom0.memory_static_min;
    let value = dom0.memory_dynamic_min;
    let mut max = (value + available).min(MAX_DOM0_MEMORY);
    if value > max {
        max = value;
    }
    if value < min {
        min = value;
    }
    Some((min, max, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iqn_and_host_names() {
        assert!(is_valid_iqn(
            "iqn.2007-11.com.example.subdomain:optional-string"
        ));
        assert!(is_valid_iqn("iqn.2025-01.org.example:host"));
        assert!(!is_valid_iqn("iqn.25-01.org.example"));
        assert!(!is_valid_iqn("eui.02004567A425678D"));
        assert!(!is_valid_iqn("iqn.2025-01.org.example:"));
        assert!(is_valid_log_server("syslog.example.com"));
        assert!(is_valid_log_server("192.0.2.10"));
        assert!(!is_valid_log_server("-bad.example"));
        assert!(!is_valid_log_server("bad host"));
    }

    #[test]
    fn email_checks() {
        assert!(is_valid_email("ops@example.com"));
        assert!(is_valid_email("first.last+tag@mail.example.org"));
        assert!(!is_valid_email("ops@localhost"));
        assert!(!is_valid_email("not an address"));
        assert!(is_valid_port("25") && !is_valid_port("0") && !is_valid_port("70000"));
    }

    #[test]
    fn nrpe_parsing_and_checks() {
        let config = NrpeConfig::parse(
            "active enabled\n",
            "allowed_hosts: 127.0.0.1,::1,192.0.2.5\ndebug: 0\nssl_logging: 0x2f\n",
            "check_cpu warning threshold is: 80 critical threshold is: 90\n\
             check_swap warning threshold is: 25 critical threshold is: 15",
        );
        assert!(config.enabled && !config.debug && config.ssl_logging);
        assert_eq!(config.allow_hosts, "192.0.2.5");
        assert_eq!(
            config.thresholds.get("check_cpu"),
            Some(&("80".to_owned(), "90".to_owned()))
        );
        assert_eq!(allow_hosts_problem(""), Some(NrpeProblem::AllowHostsEmpty));
        assert_eq!(
            allow_hosts_problem("192.0.2.1, nagios.example.com, 10.0.0.0/8"),
            None
        );
        assert_eq!(
            allow_hosts_problem("192.0.2.1,192.0.2.1"),
            Some(NrpeProblem::AllowHostsDuplicate)
        );
        assert_eq!(
            allow_hosts_problem("bad host!"),
            Some(NrpeProblem::AllowHostsFormat)
        );
        use NrpeCheckKind::*;
        assert_eq!(threshold_problem(Plain, "80", "90"), None);
        assert_eq!(
            threshold_problem(Plain, "95", "90"),
            Some(ThresholdError::WarningNotBelowCritical)
        );
        assert_eq!(threshold_problem(Free, "25", "15"), None);
        assert_eq!(
            threshold_problem(Plain, "", "90"),
            Some(ThresholdError::Empty)
        );
        assert_eq!(
            threshold_problem(Plain, "x", "90"),
            Some(ThresholdError::NotANumber)
        );
        assert_eq!(
            threshold_problem(Plain, "0", "90"),
            Some(ThresholdError::OutOfRange)
        );
        assert_eq!(
            threshold_problem(Dom0Load, "2.7,2.6,2.5", "3.2,3.1,3"),
            None
        );
        assert_eq!(
            threshold_problem(Dom0Load, "2,2", "3,3,3"),
            Some(ThresholdError::NotThreeNumbers)
        );
    }

    #[test]
    fn tags_are_split_and_deduplicated() {
        assert_eq!(parse_tags(" a, b ,a,, c"), vec!["a", "b", "c"]);
    }
}
