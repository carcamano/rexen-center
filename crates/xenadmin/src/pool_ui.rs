//! Pools and server settings in the window: the Pool menu (New Pool, Add
//! Server, Remove Server, Change Server Password, Rotate Pool Secret,
//! Enable Certificate Verification, Make into standalone server,
//! Properties) and the server menu's entries (Add to Pool, Remove From
//! Pool, Back Up, Restore From Backup, Certificate, Control Domain Memory,
//! Password, Properties), the New Pool dialog (`NewPoolDialog`), Add
//! Server's checks and questions (`AddHostToPoolCommand`), Change Server
//! Password (`ChangeServerPasswordDialog`), Control Domain Memory
//! (`ControlDomainMemoryDialog`), Install Certificate
//! (`InstallCertificateDialog`) and Back Up / Restore. The Properties
//! dialog is [`crate::props_ui`].
//!
//! Operations of one connection go through [`OpsUi`] (confirmations, RBAC
//! check, role elevation); New Pool and Add Server, which involve several
//! connections, are sent as [`Command::NewPool`] / [`Command::JoinPool`].
//!
//! Files are chosen by typing their path (there is no native file dialog).

use std::collections::{HashSet, VecDeque};

use egui::{Align2, Button, Context, DragValue, Grid, RichText, TextEdit, Ui};
use xapi::{Host, HostRecord, PoolAllowedOperations, Ref};
use xen_core::objects;
use xen_core::operations::{OperationKind, PoolKind, PrecheckResult};
use xen_core::pool::{self, JoinReason, JoinWarning, Restriction, StandaloneProblem};
use xen_core::properties::PropertiesTarget;
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, HostAction,
    OperationStatus, Password, Precheck,
};

use crate::branding;
use crate::i18n::fill;
use crate::ops_ui::{Confirmation, Entry, OpsUi};
use crate::pool_strings::pool_strings;
use crate::props_ui::PropertiesDialog;
use crate::provision_ui::DialogRequest;

// ---- names and rules ------------------------------------------------------------

/// `Helpers.GetName(connection)`: the pool's name, or its coordinator's.
pub fn connection_name(conn: &ConnectionView) -> String {
    conn.model
        .pool()
        .map(|(_, p)| objects::pool_name(&conn.model, p))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| conn.server.clone())
}

/// `PoolJoinRules.ReasonMessage` (empty for Allowed).
pub fn reason_text(reason: JoinReason) -> &'static str {
    let s = pool_strings();
    match reason {
        JoinReason::WillBeCoordinator => s.reason_coordinator,
        JoinReason::Allowed => "",
        JoinReason::Connecting => s.reason_connecting,
        JoinReason::CoordinatorNotConnected => s.reason_coordinator_disconnected,
        JoinReason::CoordinatorConnecting => s.reason_coordinator_connecting,
        JoinReason::DifferentAdConfig => s.reason_ad,
        JoinReason::HasRunningVms => s.reason_running_vms,
        JoinReason::HasSharedStorage => s.reason_shared_storage,
        JoinReason::IsAPool => s.reason_is_a_pool,
        JoinReason::LicenseRestriction => s.reason_pooling_restricted,
        JoinReason::NotSameLinuxPack => s.reason_linux_pack,
        JoinReason::LicensedHostUnlicensedCoordinator => s.reason_licensed_host,
        JoinReason::UnlicensedHostLicensedCoordinator => s.reason_unlicensed_host,
        JoinReason::LicenseMismatch => s.reason_license_mismatch,
        JoinReason::CoordinatorPoolMaxNumberHostReached => s.reason_max_hosts,
        JoinReason::WillExceedPoolMaxSize => s.reason_exceed_max,
        JoinReason::DifferentServerVersion => s.reason_version,
        JoinReason::DifferentHomogeneousUpdatesFromCoordinator => s.reason_updates_coordinator,
        JoinReason::DifferentHomogeneousUpdatesFromPool => s.reason_updates_pool,
        JoinReason::DifferentCpus => s.reason_cpus,
        JoinReason::DifferentNetworkBackends => s.reason_network_backend,
        JoinReason::CoordinatorHasHa => s.reason_ha,
        JoinReason::NotPhysicalPif => s.reason_not_physical_pif,
        JoinReason::NonCompatibleManagementInterface => s.reason_management,
        JoinReason::WrongRoleOnCoordinator => s.reason_role_coordinator,
        JoinReason::WrongRoleOnSupporter => s.reason_role_supporter,
        JoinReason::HasClusteringEnabled => s.reason_clustering,
        JoinReason::WrongNumberOfIpsCluster => s.reason_ips_cluster,
        JoinReason::WrongNumberOfIpsBond => s.reason_ips_bond,
        JoinReason::NotConnected => s.reason_disconnected,
        JoinReason::TlsVerificationOnlyOnPool => s.reason_tls_pool,
        JoinReason::TlsVerificationOnlyOnPoolJoiner => s.reason_tls_pool_joiner,
        JoinReason::TlsVerificationOnlyOnCoordinator => s.reason_tls_coordinator,
        JoinReason::TlsVerificationOnlyOnCoordinatorJoiner => s.reason_tls_coordinator_joiner,
    }
}

/// The connections whose server is standalone (`Helpers.GetPool == null`)
/// and loaded.
fn standalone_servers(state: &AppState) -> Vec<&ConnectionView> {
    let mut v: Vec<&ConnectionView> = state
        .connections
        .iter()
        .map(|c| &**c)
        .filter(|c| {
            c.status.is_connected()
                && c.model.coordinator().is_some()
                && pool::visible_pool(&c.model).is_none()
        })
        .collect();
    v.sort_by(|a, b| objects::natural_cmp(&connection_name(a), &connection_name(b)));
    v
}

/// The connections of pools (`Helpers.GetPool != null`).
fn pools(state: &AppState) -> Vec<&ConnectionView> {
    let mut v: Vec<&ConnectionView> = state
        .connections
        .iter()
        .map(|c| &**c)
        .filter(|c| c.status.is_connected() && pool::visible_pool(&c.model).is_some())
        .collect();
    v.sort_by(|a, b| objects::natural_cmp(&connection_name(a), &connection_name(b)));
    v
}

fn rolling_upgrade(conn: &ConnectionView) -> bool {
    conn.model
        .pool()
        .is_some_and(|(_, p)| p.other_config.contains_key("rolling_upgrade_in_progress"))
}

fn ha_enabled(conn: &ConnectionView) -> bool {
    conn.model.pool().is_some_and(|(_, p)| p.ha_enabled)
}

/// `ChangeHostPasswordCommand.CanRun`: a local superuser session.
fn may_change_password(conn: &ConnectionView) -> bool {
    conn.status.is_connected() && conn.rbac.as_ref().is_none_or(|r| r.is_local_superuser)
}

/// `RotatePoolSecretCommand.CanRun`.
fn may_rotate_secret(conn: &ConnectionView) -> bool {
    let c = conn.model.coordinator().map(|h| &**h);
    conn.status.is_connected()
        && pool::stockholm_or_greater(c)
        && !pool::feature_forbidden(&conn.model, Restriction::PoolSecretRotation)
        && !ha_enabled(conn)
        && !rolling_upgrade(conn)
}

/// `EnableTlsVerificationCommand.CanRun`.
fn may_enable_tls(conn: &ConnectionView) -> bool {
    let c = conn.model.coordinator().map(|h| &**h);
    let Some((_, p)) = conn.model.pool() else {
        return false;
    };
    let busy = p.current_operations.values().any(|o| {
        matches!(
            o,
            PoolAllowedOperations::HaEnable
                | PoolAllowedOperations::HaDisable
                | PoolAllowedOperations::ClusterCreate
                | PoolAllowedOperations::DesignateNewMaster
        )
    });
    conn.status.is_connected()
        && pool::cloud_or_greater(c)
        && pool::xapi_1_290_or_greater(c)
        && !pool::feature_forbidden(&conn.model, Restriction::CertificateVerification)
        && !p.tls_verification_enabled
        && !p.ha_enabled
        && !busy
        && !rolling_upgrade(conn)
}

// ---- menus ---------------------------------------------------------------------------

/// The Pool menu's entries for the pool of `conn`, in the original's order.
pub fn pool_entries(state: &AppState, conn: &ConnectionView) -> Vec<Entry> {
    let s = pool_strings();
    let usable = conn.status.is_connected();
    let in_pool = usable && pool::visible_pool(&conn.model).is_some();
    let mut add = Vec::new();
    if in_pool {
        for other in standalone_servers(state) {
            let host = other.model.coordinator().map(|h| &**h);
            let restricted = host.is_some_and(|h| pool::host_restricts(h, Restriction::Pooling));
            let name = connection_name(other);
            add.push(Entry::Named {
                label: if restricted {
                    fill(s.add_server_restricted, &[("name", &name)])
                } else {
                    name
                },
                enabled: !restricted,
                hint: None,
                dialog: DialogRequest::AddToPool {
                    pool: conn.id,
                    hosts: vec![other.id],
                    confirm: true,
                },
            });
        }
        add.push(Entry::Separator);
        add.push(Entry::Named {
            label: s.add_new_server.to_owned(),
            enabled: true,
            hint: None,
            dialog: DialogRequest::AddNewServer { pool: conn.id },
        });
    }
    let removable: Vec<Entry> = pool::removable_hosts(&conn.model)
        .into_iter()
        .map(|(r, h)| Entry::Item {
            label: leak(h.name_label.clone()),
            enabled: true,
            request: ActionRequest::EjectHost { host: r.clone() },
        })
        .collect();
    let standalone = pool::standalone_problem(&conn.model);
    let mut entries = vec![
        Entry::Dialog {
            label: s.new_pool,
            enabled: true,
            dialog: DialogRequest::NewPool { servers: vec![] },
        },
        Entry::Separator,
        Entry::Submenu {
            label: s.add_server.to_owned(),
            enabled: in_pool,
            entries: add,
        },
        Entry::Submenu {
            label: s.remove_server.to_owned(),
            enabled: in_pool && !removable.is_empty(),
            entries: removable,
        },
        Entry::Separator,
    ];
    // `highAvailabilityToolStripMenuItem`, after the vApp entries.
    entries.push(crate::vapp_ui::manage_entry(conn));
    entries.extend(crate::ha_ui::pool_entries(state, conn));
    entries.push(crate::dr_ui::menu_entry(conn));
    entries.push(crate::vmss_ui::manage_entry(conn));
    entries.push(Entry::Separator);
    entries.extend([
        Entry::Named {
            label: s.change_password.to_owned(),
            enabled: may_change_password(conn),
            hint: (!may_change_password(conn) && usable).then(|| s.password_ad.to_owned()),
            dialog: DialogRequest::ChangePassword,
        },
        Entry::Named {
            label: s.rotate_secret.to_owned(),
            enabled: may_rotate_secret(conn),
            hint: rotate_hint(conn),
            dialog: DialogRequest::Run(ActionRequest::RotatePoolSecret),
        },
        Entry::Named {
            label: s.enable_tls.to_owned(),
            enabled: may_enable_tls(conn),
            hint: ha_enabled(conn).then(|| s.tls_ha.to_owned()),
            dialog: DialogRequest::Run(ActionRequest::EnableTlsVerification),
        },
        Entry::Separator,
        Entry::Dialog {
            label: s.make_standalone,
            enabled: usable
                && !matches!(
                    standalone,
                    Some(StandaloneProblem::NotAPool | StandaloneProblem::SeveralHosts)
                ),
            dialog: DialogRequest::MakeStandalone,
        },
        Entry::Separator,
        Entry::Dialog {
            label: s.properties,
            enabled: usable && conn.model.pool().is_some(),
            dialog: DialogRequest::Properties(PropertiesTarget::Pool(
                conn.model
                    .pool()
                    .map(|(r, _)| r.clone())
                    .unwrap_or_default(),
            )),
        },
    ]);
    entries
}

fn rotate_hint(conn: &ConnectionView) -> Option<String> {
    let s = pool_strings();
    if ha_enabled(conn) {
        Some(s.rotate_ha.to_owned())
    } else if rolling_upgrade(conn) {
        Some(s.rotate_rpu.to_owned())
    } else {
        None
    }
}

/// Menu labels must be `'static`; server names are few and long-lived.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// The server menu's F10a entries for `host`, in the original's order
/// (appended after the power and maintenance operations).
pub fn host_entries(
    state: &AppState,
    conn: &ConnectionView,
    host_ref: &Ref<Host>,
    host: &HostRecord,
) -> Vec<Entry> {
    let s = pool_strings();
    let usable = conn.status.is_connected();
    let live = usable && objects::host_is_live(&conn.model, host);
    let standalone = pool::visible_pool(&conn.model).is_none();
    let restricted = pool::host_restricts(host, Restriction::Pooling);
    let mut add = Vec::new();
    for p in pools(state) {
        add.push(Entry::Named {
            label: connection_name(p),
            enabled: true,
            hint: None,
            dialog: DialogRequest::AddToPool {
                pool: p.id,
                hosts: vec![conn.id],
                confirm: true,
            },
        });
    }
    add.push(Entry::Separator);
    add.push(Entry::Dialog {
        label: s.new_pool,
        enabled: true,
        dialog: DialogRequest::NewPool {
            servers: vec![conn.id],
        },
    });
    let coordinator = conn.model.coordinator().map(|h| &**h);
    let install_ok = pool::stockholm_or_greater(Some(host)) && !ha_enabled(conn);
    let reset_ok = pool::cloud_or_greater(Some(host)) && pool::xapi_1_290_or_greater(Some(host));
    let _ = coordinator;
    let remove = pool::remove_problem(&conn.model, host_ref);
    vec![
        Entry::Separator,
        Entry::Submenu {
            label: if restricted && standalone {
                s.add_to_pool_restricted.to_owned()
            } else {
                s.add_to_pool.to_owned()
            },
            enabled: usable && standalone && !restricted,
            entries: add,
        },
        Entry::Named {
            label: s.remove_from_pool.to_owned(),
            enabled: usable && remove.is_none(),
            hint: remove.and_then(|p| match p {
                pool::RemoveProblem::Coordinator => Some(s.remove_coordinator.to_owned()),
                pool::RemoveProblem::Unreachable => Some(s.host_unreachable.to_owned()),
                pool::RemoveProblem::RunningVms => Some(s.reason_running_vms.to_owned()),
                pool::RemoveProblem::NotInPool => None,
            }),
            dialog: DialogRequest::Run(ActionRequest::EjectHost {
                host: host_ref.clone(),
            }),
        },
        Entry::Separator,
        Entry::Dialog {
            label: s.back_up,
            enabled: live,
            dialog: DialogRequest::Backup(host_ref.clone()),
        },
        Entry::Dialog {
            label: s.restore,
            enabled: live,
            dialog: DialogRequest::Restore(host_ref.clone()),
        },
        Entry::Separator,
        Entry::Submenu {
            label: s.certificate.to_owned(),
            enabled: usable && (install_ok || reset_ok),
            entries: vec![
                Entry::Named {
                    label: s.install_certificate.to_owned(),
                    enabled: install_ok,
                    hint: ha_enabled(conn).then(|| s.install_ha.to_owned()),
                    dialog: DialogRequest::InstallCertificate(host_ref.clone()),
                },
                Entry::Named {
                    label: s.reset_certificate.to_owned(),
                    enabled: reset_ok,
                    hint: None,
                    dialog: DialogRequest::Run(ActionRequest::ResetCertificate {
                        host: host_ref.clone(),
                    }),
                },
            ],
        },
        Entry::Dialog {
            label: s.dom0_memory,
            enabled: live,
            dialog: DialogRequest::Dom0Memory(host_ref.clone()),
        },
        Entry::Submenu {
            label: s.password.to_owned(),
            enabled: usable,
            entries: vec![Entry::Named {
                label: s.change_password.to_owned(),
                enabled: may_change_password(conn) && live,
                hint: (!may_change_password(conn)).then(|| s.password_ad.to_owned()),
                dialog: DialogRequest::ChangePassword,
            }],
        },
        Entry::Separator,
        Entry::Dialog {
            label: s.properties,
            enabled: usable,
            dialog: DialogRequest::Properties(PropertiesTarget::Host(host_ref.clone())),
        },
    ]
}

/// The question the original asks before an F10a request, if any.
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let s = pool_strings();
    let o = crate::ops_strings::ops_strings();
    let model = &conn.model;
    match request {
        ActionRequest::EjectHost { host } => {
            let name = model.hosts.get(host)?.name_label.clone();
            let pool = connection_name(conn);
            Some(Confirmation {
                title: s.remove_title,
                text: fill(s.remove_text, &[("host", &name), ("pool", &pool)]),
                yes: s.remove_yes,
                no_default: true,
            })
        }
        ActionRequest::ResetCertificate { .. } => Some(Confirmation {
            title: s.reset_certificate.trim_end_matches("..."),
            text: s.reset_warning.to_owned(),
            yes: o.yes,
            no_default: false,
        }),
        ActionRequest::EnableTlsVerification => Some(Confirmation {
            title: s.enable_tls,
            text: s.tls_warning.to_owned(),
            yes: s.tls_yes,
            no_default: true,
        }),
        ActionRequest::RotatePoolSecret => Some(Confirmation {
            title: s.rotate_secret,
            text: s.rotate_remind.to_owned(),
            yes: s.ok,
            no_default: false,
        }),
        ActionRequest::SetDom0Memory { host, .. } => {
            let name = model.hosts.get(host)?.name_label.clone();
            Some(Confirmation {
                title: leak(fill(s.dom0_title, &[("host", &name)])),
                text: s.dom0_confirm.to_owned(),
                yes: o.yes,
                no_default: false,
            })
        }
        _ => None,
    }
}

// ---- the window's part ------------------------------------------------------------------

/// A question of a multi-step flow (Yes goes on, No stops).
struct Question {
    title: String,
    text: String,
    yes: String,
}

/// What a finished flow does.
enum FlowEnd {
    Send(Command),
}

/// A chain of questions, then an action (`AddHostToPoolCommand`,
/// `NewPoolDialog.createPool`).
struct Flow {
    questions: VecDeque<Question>,
    end: Option<FlowEnd>,
    /// Servers must join the pool's domain first: its account is asked
    /// last (`AddHostToPoolCommand.GetAdPrompt`).
    ad: Option<AdPrompt>,
}

/// `AdPasswordPrompt` for the pool's domain (the domain is not asked).
struct AdPrompt {
    domain: String,
    username: String,
    password: String,
    /// Why it is asked again (the account was refused).
    error: Option<String>,
}

impl AdPrompt {
    fn new(domain: String) -> Self {
        Self {
            domain,
            username: String::new(),
            password: String::new(),
            error: None,
        }
    }
}

/// The pool's domain, when some of `members` must join it before joining
/// the pool (`PoolJoinRules.CompatibleAdConfig`, as `FixAd` checks).
fn ad_domain(members: &[&ConnectionView], coordinator: &ConnectionView) -> Option<String> {
    let c = coordinator.model.coordinator()?;
    if c.external_auth_type.is_empty() {
        return None;
    }
    members
        .iter()
        .any(|m| {
            m.model
                .coordinator()
                .is_some_and(|h| !pool::compatible_ad_config(h, c, false))
        })
        .then(|| c.external_auth_service_name.clone())
}

/// Why the sessions involved may not run `command` (New Pool, Add
/// Server), worded as the RBAC denial with the roles that could.
fn join_rbac_problem(state: &AppState, command: &Command) -> Option<String> {
    use xen_core::rbac::{self, RbacCheck};
    const POOL: &[&str] = &[
        "pool.set_name_label",
        "pool.set_name_description",
        "pool.set_default_sr",
        "pool.set_suspend_image_sr",
        "pool.set_crash_dump_sr",
    ];
    const JOIN: &[&str] = &["pool.join"];
    let (coordinator, members): (ConnectionId, &[ConnectionId]) = match command {
        Command::NewPool {
            coordinator,
            members,
            ..
        } => (*coordinator, members),
        Command::JoinPool { pool, hosts, .. } => (*pool, hosts),
        _ => return None,
    };
    let checks = std::iter::once((coordinator, POOL)).chain(members.iter().map(|m| (*m, JOIN)));
    for (c, methods) in checks {
        let Some(conn) = state.connection(c) else {
            continue;
        };
        if let RbacCheck::Denied { authorized } =
            rbac::check(&conn.model, conn.rbac.as_deref(), methods)
        {
            let authorized = authorized
                .iter()
                .map(|r| crate::i18n::role_name(&r.name_label))
                .collect::<Vec<_>>()
                .join(", ");
            return Some(format!(
                "{}: {}",
                connection_name(conn),
                fill(
                    crate::ops_strings::ops_strings().rbac_denied,
                    &[
                        ("roles", &crate::i18n::session_roles_text(conn)),
                        ("authorized", &authorized),
                    ],
                )
            ));
        }
    }
    None
}

/// What to send again when the domain account was refused.
#[derive(Clone)]
enum AdRetry {
    Join {
        pool: ConnectionId,
        hosts: Vec<ConnectionId>,
    },
    NewPool {
        coordinator: ConnectionId,
        members: Vec<ConnectionId>,
        name: String,
        description: String,
    },
}

/// What "Add New Server" waits for: a new connection to come up.
enum Awaiting {
    NewPool,
    AddToPool(ConnectionId),
}

/// The New Pool dialog.
pub struct NewPoolDialog {
    pub name: String,
    pub description: String,
    pub coordinator: Option<ConnectionId>,
    pub members: HashSet<ConnectionId>,
}

impl NewPoolDialog {
    /// The servers and why each can or cannot join, sorted as the
    /// original (`ConnectionWrapperWithMoreStuff.SameLevelSortOrder`).
    pub fn rows<'a>(&self, state: &'a AppState) -> Vec<(&'a ConnectionView, JoinReason)> {
        let coordinator = self
            .coordinator
            .and_then(|c| state.connection(c))
            .map(|c| &**c);
        let mut rows: Vec<(&ConnectionView, JoinReason)> = state
            .connections
            .iter()
            .filter(|c| c.status.is_connected())
            .map(|c| {
                (
                    &**c,
                    pool::can_join_pool(Some(c), coordinator, true, true, 1),
                )
            })
            .collect();
        rows.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| objects::natural_cmp(&connection_name(a.0), &connection_name(b.0)))
        });
        rows
    }

    /// The servers that can be the coordinator.
    pub fn coordinators<'a>(&self, state: &'a AppState) -> Vec<&'a ConnectionView> {
        let mut v: Vec<&ConnectionView> = state
            .connections
            .iter()
            .filter(|c| c.status.is_connected())
            .filter(|c| pool::can_join_pool(Some(c), Some(c), true, true, 1).can_be_coordinator())
            .map(|c| &**c)
            .collect();
        v.sort_by(|a, b| objects::natural_cmp(&connection_name(a), &connection_name(b)));
        v
    }

    /// The members that will join: checked and allowed.
    fn supporters(&self, state: &AppState) -> Vec<ConnectionId> {
        self.rows(state)
            .into_iter()
            .filter(|(c, r)| *r == JoinReason::Allowed && self.members.contains(&c.id))
            .map(|(c, _)| c.id)
            .collect()
    }

    /// Why Create Pool is disabled (`validToClose`).
    pub fn invalid(&self, state: &AppState) -> Option<&'static str> {
        let s = pool_strings();
        if self.name.trim().is_empty() {
            return Some(s.pool_name_empty);
        }
        let Some(c) = self.coordinator.and_then(|c| state.connection(c)) else {
            return Some(s.no_eligible_coordinator);
        };
        let n = self.supporters(state).len();
        if pool::feature_forbidden(&c.model, Restriction::PoolSize) && c.model.hosts.len() + n > 3 {
            return Some(s.will_exceed_max);
        }
        None
    }
}

/// Change Server Password.
pub struct PasswordDialog {
    connection: ConnectionId,
    pub current: String,
    pub new: String,
    pub confirm: String,
    pub error: Option<&'static str>,
    pending: Option<u64>,
}

/// Control Domain Memory.
pub struct Dom0Dialog {
    connection: ConnectionId,
    host: Ref<Host>,
    /// MiB.
    pub memory: i64,
    original: i64,
}

/// Install Certificate.
pub struct InstallDialog {
    connection: ConnectionId,
    host: Ref<Host>,
    pub key: String,
    pub certificate: String,
    pub chain: Vec<String>,
    pub chain_entry: String,
    pub errors: (Option<String>, Option<String>, Option<String>),
}

/// Back Up / Restore.
pub struct TransferDialog {
    connection: ConnectionId,
    host: Ref<Host>,
    pub restore: bool,
    pub path: String,
    pub error: Option<String>,
}

/// The pool part of the window.
#[derive(Default)]
pub struct PoolUi {
    pub new_pool: Option<NewPoolDialog>,
    flow: Option<Flow>,
    awaiting: Option<(Awaiting, HashSet<ConnectionId>)>,
    pub password: Option<PasswordDialog>,
    pub dom0: Option<Dom0Dialog>,
    pub install: Option<InstallDialog>,
    pub transfer: Option<TransferDialog>,
    pub properties: Option<Box<PropertiesDialog>>,
    /// Requests to run through the operations (with their questions).
    queued: Vec<(ConnectionId, ActionRequest)>,
    /// Information to show (the restore's next steps).
    notices: VecDeque<String>,
    /// Operations already looked at for notices.
    seen_ops: u64,
    next_id: u64,
    /// The window should open its connect dialog (Add New Server).
    pub open_connect: bool,
    /// The last Add Server / New Pool sent with a domain account: the
    /// newest operation id then, what was sent and the domain. A refused
    /// account asks again (`PoolAbstractAction.FixAd`).
    ad_sent: Option<(xen_core::OperationId, AdRetry, String)>,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
}

impl PoolUi {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        (1 << 50) + self.next_id
    }

    /// Opens what `request` asks for (`conn`: the menu's connection).
    pub fn open(
        &mut self,
        state: &AppState,
        conn: &ConnectionView,
        request: DialogRequest,
        backend: &BackendHandle,
    ) {
        let restore_requested = matches!(request, DialogRequest::Restore(_));
        match request {
            DialogRequest::NewPool { servers } => {
                let mut d = NewPoolDialog {
                    name: String::new(),
                    description: String::new(),
                    coordinator: None,
                    members: HashSet::new(),
                };
                let coordinators = d.coordinators(state);
                // `selectCoordinator` / `selectSupporters`.
                d.coordinator = servers
                    .first()
                    .copied()
                    .filter(|c| coordinators.iter().any(|v| v.id == *c))
                    .or_else(|| coordinators.first().map(|c| c.id));
                d.members = servers.iter().skip(1).copied().collect();
                self.new_pool = Some(d);
            }
            DialogRequest::AddToPool {
                pool,
                hosts,
                confirm,
            } => self.add_to_pool(state, pool, &hosts, confirm),
            DialogRequest::AddNewServer { pool } => {
                self.awaiting = Some((
                    Awaiting::AddToPool(pool),
                    state.connections.iter().map(|c| c.id).collect(),
                ));
                self.open_connect = true;
            }
            DialogRequest::MakeStandalone => {
                let s = pool_strings();
                let name = connection_name(conn);
                match pool::standalone_problem(&conn.model) {
                    Some(StandaloneProblem::HaEnabled) => {
                        self.errors
                            .push(fill(s.standalone_ha_enabled, &[("pool", &name)]));
                    }
                    Some(StandaloneProblem::HaEnabling) => {
                        self.errors
                            .push(fill(s.standalone_ha_enabling, &[("pool", &name)]));
                    }
                    Some(StandaloneProblem::SeveralHosts) => {
                        self.errors.push(s.standalone_members.to_owned());
                    }
                    Some(StandaloneProblem::NotAPool) => {}
                    None => {
                        if let Some((r, _)) = conn.model.pool() {
                            self.queued
                                .push((conn.id, ActionRequest::DestroyPool { pool: r.clone() }));
                        }
                    }
                }
            }
            DialogRequest::Run(request) => self.queued.push((conn.id, request)),
            DialogRequest::ChangePassword => {
                self.password = Some(PasswordDialog {
                    connection: conn.id,
                    current: String::new(),
                    new: String::new(),
                    confirm: String::new(),
                    error: None,
                    pending: None,
                });
            }
            DialogRequest::Dom0Memory(host) => {
                if let Some(rec) = conn.model.hosts.get(&host)
                    && let Some((_, _, value)) =
                        xen_core::properties::dom0_memory_range(&conn.model, rec)
                {
                    let mib = value / (1024 * 1024);
                    self.dom0 = Some(Dom0Dialog {
                        connection: conn.id,
                        host,
                        memory: mib,
                        original: mib,
                    });
                }
            }
            DialogRequest::InstallCertificate(host) => {
                self.install = Some(InstallDialog {
                    connection: conn.id,
                    host,
                    key: String::new(),
                    certificate: String::new(),
                    chain: Vec::new(),
                    chain_entry: String::new(),
                    errors: (None, None, None),
                });
            }
            DialogRequest::Backup(host) | DialogRequest::Restore(host) => {
                let restore = restore_requested;
                self.transfer = Some(TransferDialog {
                    connection: conn.id,
                    host,
                    restore,
                    path: String::new(),
                    error: None,
                });
            }
            DialogRequest::Properties(target) => {
                self.properties =
                    PropertiesDialog::open(conn, target, backend, &mut self.next_id).map(Box::new);
            }
            _ => {}
        }
    }

    /// `AddHostToPoolCommand.RunCore`: refuses the servers that cannot
    /// join (listing why), asks to confirm, asks the licensing, CPU and AD
    /// questions, then joins.
    fn add_to_pool(
        &mut self,
        state: &AppState,
        pool: ConnectionId,
        hosts: &[ConnectionId],
        confirm: bool,
    ) {
        let s = pool_strings();
        let Some(pool_view) = state.connection(pool).filter(|c| c.status.is_connected()) else {
            let host = hosts
                .first()
                .and_then(|h| state.connection(*h))
                .map(|c| connection_name(c))
                .unwrap_or_default();
            let pool_name = state
                .connection(pool)
                .map(|c| connection_name(c))
                .unwrap_or_default();
            self.errors.push(fill(
                s.pool_disconnected,
                &[("host", &host), ("pool", &pool_name)],
            ));
            return;
        };
        let pool_name = connection_name(pool_view);
        let members: Vec<&ConnectionView> = hosts
            .iter()
            .filter_map(|h| state.connection(*h))
            .map(|c| &**c)
            .collect();
        let refused: Vec<String> = members
            .iter()
            .filter_map(|m| {
                let reason =
                    pool::can_join_pool(Some(m), Some(pool_view), true, true, members.len());
                (reason != JoinReason::Allowed)
                    .then(|| format!("{}: {}", connection_name(m), reason_text(reason)))
            })
            .collect();
        if !refused.is_empty() {
            self.errors.push(format!(
                "{}\n{}\n{}",
                s.add_error_title,
                fill(s.add_error_text, &[("pool", &pool_name)]),
                refused.join("\n")
            ));
            return;
        }
        let mut questions = VecDeque::new();
        if confirm {
            let text = match members.as_slice() {
                [one] => fill(
                    s.confirm_move,
                    &[("host", &connection_name(one)), ("pool", &pool_name)],
                ),
                _ => fill(s.confirm_move_multiple, &[("pool", &pool_name)]),
            };
            questions.push_back(Question {
                title: s.adding_title.to_owned(),
                text,
                yes: s.yes.to_owned(),
            });
        }
        questions.extend(warning_questions(&members, pool_view, false));
        self.flow = Some(Flow {
            questions,
            end: Some(FlowEnd::Send(Command::JoinPool {
                pool,
                hosts: hosts.to_vec(),
                ad: None,
            })),
            ad: ad_domain(&members, pool_view).map(AdPrompt::new),
        });
    }

    /// An answer to a precheck this part asked.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        if let Some(d) = self.password.as_mut()
            && d.pending == Some(id)
        {
            d.pending = None;
            if let PrecheckResult::CurrentPassword(ok) = result {
                if *ok {
                    d.pending = Some(u64::MAX); // accepted: dispatched by `windows`
                } else {
                    d.error = Some(pool_strings().password_wrong);
                }
            }
            return true;
        }
        if let Some(p) = self.properties.as_mut() {
            return p.on_precheck(id, result);
        }
        false
    }

    /// Draws the dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        for (c, request) in std::mem::take(&mut self.queued) {
            if let Some(conn) = state.connection(c) {
                ops.request(conn, request, backend);
            }
        }
        self.watch_new_connections(state);
        self.watch_operations(state);
        self.new_pool_window(ctx, state);
        self.flow_window(ctx, state, backend);
        self.password_window(ctx, state, ops, backend);
        self.dom0_window(ctx, state, ops, backend);
        self.install_window(ctx, state, ops, backend);
        self.transfer_window(ctx, state, ops, backend);
        if let Some(p) = self.properties.as_mut() {
            let close = p.window(ctx, state, ops, backend);
            if close {
                self.properties = None;
            }
        }
        self.notice_window(ctx);
    }

    /// Add New Server: once the new connection is up, it joins (or is
    /// ticked in the New Pool dialog).
    fn watch_new_connections(&mut self, state: &AppState) {
        let Some((what, known)) = &self.awaiting else {
            return;
        };
        let fresh = state.connections.iter().find(|c| {
            !known.contains(&c.id) && c.status.is_connected() && c.model.coordinator().is_some()
        });
        let Some(fresh) = fresh else { return };
        let fresh_id = fresh.id;
        match what {
            Awaiting::NewPool => {
                if let Some(d) = self.new_pool.as_mut() {
                    d.members.insert(fresh_id);
                }
            }
            Awaiting::AddToPool(pool) => {
                let pool = *pool;
                if let Some(other) = pool::visible_pool(&fresh.model)
                    && let Some(p) = state.connection(pool)
                {
                    let s = pool_strings();
                    let host = fresh
                        .model
                        .coordinator()
                        .map(|h| h.name_label.clone())
                        .unwrap_or_default();
                    self.errors.push(format!(
                        "{}: {}",
                        s.join_impossible,
                        fill(
                            s.host_already_in_pool,
                            &[
                                ("host", &host),
                                ("pool", &connection_name(p)),
                                ("other", &other.1.name_label),
                            ],
                        )
                    ));
                } else {
                    self.add_to_pool(state, pool, &[fresh_id], false);
                }
            }
        }
        self.awaiting = None;
    }

    /// The restore's next steps once it worked (`RestoreAction_Completed`);
    /// a refused domain account is asked again (`FixAd`).
    fn watch_operations(&mut self, state: &AppState) {
        if let Some((floor, retry, domain)) = self.ad_sent.clone() {
            let refused = state.operations.iter().find(|o| {
                o.id > floor
                    && matches!(
                        o.kind,
                        OperationKind::Pool(PoolKind::AddHost { .. } | PoolKind::CreatePool { .. })
                    )
                    && matches!(&o.status, OperationStatus::Failed(xen_core::OperationFailure::Xapi(e))
                        if xen_core::ad::is_credentials_failure(e))
            });
            let all_done = state
                .operations
                .iter()
                .filter(|o| o.id > floor)
                .all(|o| !o.is_running());
            if let Some(op) = refused
                && all_done
                && self.flow.is_none()
            {
                let error = match &op.status {
                    OperationStatus::Failed(xen_core::OperationFailure::Xapi(e)) => {
                        crate::i18n::describe_xapi_error(e)
                    }
                    _ => String::new(),
                };
                // Only the servers still on their own are sent again.
                let standalone = |c: &ConnectionId| {
                    state
                        .connection(*c)
                        .is_some_and(|v| pool::visible_pool(&v.model).is_none())
                };
                let command = match retry {
                    AdRetry::Join { pool, hosts } => Command::JoinPool {
                        pool,
                        hosts: hosts.into_iter().filter(standalone).collect(),
                        ad: None,
                    },
                    AdRetry::NewPool {
                        coordinator,
                        members,
                        name,
                        description,
                    } => Command::NewPool {
                        coordinator,
                        members: members.into_iter().filter(standalone).collect(),
                        name,
                        description,
                        ad: None,
                    },
                };
                let mut prompt = AdPrompt::new(domain);
                prompt.error = Some(error);
                self.flow = Some(Flow {
                    questions: VecDeque::new(),
                    end: Some(FlowEnd::Send(command)),
                    ad: Some(prompt),
                });
                self.ad_sent = None;
            } else if all_done && refused.is_none() && state.operations.iter().any(|o| o.id > floor)
            {
                self.ad_sent = None;
            }
        }
        let s = pool_strings();
        for op in state.operations.iter().filter(|o| o.id > self.seen_ops) {
            if op.is_running() {
                continue;
            }
            if let (OperationKind::Pool(PoolKind::Restore { host }), OperationStatus::Succeeded) =
                (&op.kind, &op.status)
            {
                self.notices.push_back(fill(
                    s.restore_done,
                    &[("host", host), ("product", branding::PLATFORM_NAME)],
                ));
            }
        }
        if let Some(max) = state
            .operations
            .iter()
            .take_while(|o| !o.is_running())
            .map(|o| o.id)
            .max()
        {
            self.seen_ops = self.seen_ops.max(max);
        }
    }

    fn notice_window(&mut self, ctx: &Context) {
        let Some(text) = self.notices.front().cloned() else {
            return;
        };
        let mut ok = false;
        window(branding::PRODUCT_NAME.to_owned()).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.label(text);
            ui.add_space(8.0);
            ok = ui.button(pool_strings().ok).clicked();
        });
        if ok {
            self.notices.pop_front();
        }
    }

    fn flow_window(&mut self, ctx: &Context, state: &AppState, backend: &BackendHandle) {
        let Some(flow) = self.flow.as_mut() else {
            return;
        };
        let Some(q) = flow.questions.front() else {
            let mut account = None;
            if let Some(p) = flow.ad.as_mut() {
                let h = crate::ha_strings::ha_strings();
                let (mut ok, mut cancel) = (false, false);
                window(h.join_title.to_owned()).show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(fill(h.joining_ad, &[("domain", &p.domain)]));
                    if let Some(e) = &p.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    ui.add_space(6.0);
                    Grid::new("pool-ad-prompt")
                        .num_columns(2)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            let l = ui.label(h.username);
                            ui.text_edit_singleline(&mut p.username).labelled_by(l.id);
                            ui.end_row();
                            let l = ui.label(h.password);
                            ui.add(TextEdit::singleline(&mut p.password).password(true))
                                .labelled_by(l.id);
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ok = ui
                            .add_enabled(!p.username.trim().is_empty(), Button::new(h.ok))
                            .clicked();
                        cancel = ui.button(h.cancel).clicked();
                    });
                });
                if cancel {
                    self.flow = None;
                    return;
                }
                if !ok {
                    return;
                }
                account = Some((
                    p.username.trim().to_owned(),
                    Password::new(p.password.clone()),
                    p.domain.clone(),
                ));
            }
            if let Some(FlowEnd::Send(mut command)) = flow.end.take() {
                // The roles each server's session needs (`AddHostToPoolAction`,
                // `CreatePoolAction`): refused with the roles that could.
                if let Some(text) = join_rbac_problem(state, &command) {
                    self.errors.push(text);
                    self.flow = None;
                    return;
                }
                if let Some((user, password, domain)) = account {
                    let floor = state.operations.iter().map(|o| o.id).max().unwrap_or(0);
                    let retry = match &mut command {
                        Command::JoinPool { pool, hosts, ad } => {
                            *ad = Some((user, password));
                            Some(AdRetry::Join {
                                pool: *pool,
                                hosts: hosts.clone(),
                            })
                        }
                        Command::NewPool {
                            coordinator,
                            members,
                            name,
                            description,
                            ad,
                        } => {
                            *ad = Some((user, password));
                            Some(AdRetry::NewPool {
                                coordinator: *coordinator,
                                members: members.clone(),
                                name: name.clone(),
                                description: description.clone(),
                            })
                        }
                        _ => None,
                    };
                    self.ad_sent = retry.map(|r| (floor, r, domain));
                }
                backend.send(command);
            }
            self.flow = None;
            return;
        };
        let s = pool_strings();
        let (mut yes, mut no) = (false, false);
        window(q.title.clone()).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(&q.text);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                yes = ui.button(&q.yes).clicked();
                no = ui.button(s.no).clicked();
            });
        });
        if yes {
            flow.questions.pop_front();
        } else if no {
            self.flow = None;
        }
    }

    fn new_pool_window(&mut self, ctx: &Context, state: &AppState) {
        let Some(d) = self.new_pool.as_mut() else {
            return;
        };
        let s = pool_strings();
        let mut create = false;
        let mut cancel = false;
        let mut add_new = false;
        window(s.new_pool_title.to_owned()).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(s.new_pool_blurb);
            ui.add_space(6.0);
            Grid::new("new-pool").num_columns(2).show(ui, |ui| {
                let label = ui.label(s.name_label);
                ui.add(TextEdit::singleline(&mut d.name).desired_width(300.0))
                    .labelled_by(label.id);
                ui.end_row();
                ui.label(s.description_label);
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut d.description).desired_width(220.0));
                    ui.label(RichText::new(s.optional).weak());
                });
                ui.end_row();
                ui.label(s.coordinator_label);
                let coordinators = d.coordinators(state);
                let shown = d
                    .coordinator
                    .and_then(|c| state.connection(c))
                    .map(|c| connection_name(c))
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("new-pool-coordinator")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for c in coordinators {
                            ui.selectable_value(&mut d.coordinator, Some(c.id), connection_name(c));
                        }
                    });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.label(s.members_label);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(480.0);
                for (c, reason) in d.rows(state) {
                    let mut on =
                        d.members.contains(&c.id) || reason == JoinReason::WillBeCoordinator;
                    let enabled = reason == JoinReason::Allowed;
                    ui.horizontal(|ui| {
                        ui.add_enabled(enabled, egui::Checkbox::new(&mut on, connection_name(c)));
                        let text = reason_text(reason);
                        if !text.is_empty() {
                            ui.label(RichText::new(text).weak().italics());
                        }
                    });
                    if enabled {
                        if on {
                            d.members.insert(c.id);
                        } else {
                            d.members.remove(&c.id);
                        }
                    }
                }
            });
            if ui
                .button(s.add_new_server.trim_end_matches("...").to_owned() + "...")
                .clicked()
            {
                add_new = true;
            }
            ui.add_space(8.0);
            let invalid = d.invalid(state);
            ui.horizontal(|ui| {
                let mut r = ui.add_enabled(invalid.is_none(), Button::new(s.create_pool));
                if let Some(why) = invalid {
                    r = r.on_disabled_hover_text(why);
                }
                create = r.clicked();
                cancel = ui.button(s.cancel).clicked();
            });
        });
        if add_new {
            self.awaiting = Some((
                Awaiting::NewPool,
                state.connections.iter().map(|c| c.id).collect(),
            ));
            self.open_connect = true;
        }
        if cancel {
            self.new_pool = None;
            return;
        }
        if create {
            let d = self.new_pool.take().expect("open");
            let Some(coordinator) = d.coordinator.and_then(|c| state.connection(c)) else {
                return;
            };
            let members = d.supporters(state);
            let views: Vec<&ConnectionView> = members
                .iter()
                .filter_map(|m| state.connection(*m))
                .map(|c| &**c)
                .collect();
            self.flow = Some(Flow {
                questions: warning_questions(&views, coordinator, true)
                    .into_iter()
                    .collect(),
                ad: ad_domain(&views, coordinator).map(AdPrompt::new),
                end: Some(FlowEnd::Send(Command::NewPool {
                    coordinator: coordinator.id,
                    members,
                    name: d.name.trim().to_owned(),
                    description: d.description.clone(),
                    ad: None,
                })),
            });
        }
    }

    fn password_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(d) = self.password.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.password = None;
            return;
        };
        let s = pool_strings();
        if d.pending == Some(u64::MAX) {
            let request = ActionRequest::ChangePassword {
                old: Password::new(d.current.clone()),
                new: Password::new(d.new.clone()),
            };
            ops.dispatch(conn, request, backend);
            self.password = None;
            return;
        }
        let mut ok = false;
        let mut cancel = false;
        let rotate_hint = pool::stockholm_or_greater(conn.model.coordinator().map(|h| &**h))
            && !pool::feature_forbidden(&conn.model, Restriction::PoolSecretRotation);
        window(fill(s.password_title, &[("pool", &connection_name(conn))])).show(ctx, |ui| {
            ui.set_max_width(440.0);
            Grid::new("change-password").num_columns(2).show(ui, |ui| {
                let label = ui.label(s.password_current);
                if ui
                    .add(TextEdit::singleline(&mut d.current).password(true))
                    .labelled_by(label.id)
                    .changed()
                {
                    d.error = None;
                }
                ui.end_row();
                let label = ui.label(s.password_new);
                if ui
                    .add(TextEdit::singleline(&mut d.new).password(true))
                    .labelled_by(label.id)
                    .changed()
                {
                    d.error = None;
                }
                ui.end_row();
                let label = ui.label(s.password_confirm);
                if ui
                    .add(TextEdit::singleline(&mut d.confirm).password(true))
                    .labelled_by(label.id)
                    .changed()
                {
                    d.error = None;
                }
                ui.end_row();
            });
            if let Some(e) = d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.label(RichText::new(s.password_note).weak());
            if rotate_hint {
                ui.label(RichText::new(s.password_rotate_hint).weak());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !d.current.is_empty()
                    && !d.new.is_empty()
                    && !d.confirm.is_empty()
                    && d.pending.is_none();
                ok = ui.add_enabled(ready, Button::new(s.ok)).clicked();
                cancel = ui.button(s.cancel).clicked();
                if d.pending.is_some() {
                    ui.spinner();
                }
            });
        });
        if cancel {
            self.password = None;
            return;
        }
        if ok {
            if d.new != d.confirm {
                d.error = Some(s.passwords_dont_match);
                return;
            }
            let id = self.next_id();
            let d = self.password.as_mut().expect("open");
            d.pending = Some(id);
            backend.send(Command::Precheck {
                connection: d.connection,
                id,
                check: Precheck::CurrentPassword {
                    password: Password::new(d.current.clone()),
                },
            });
        }
    }

    fn dom0_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(d) = self.dom0.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.dom0 = None;
            return;
        };
        let Some(host) = conn.model.hosts.get(&d.host).cloned() else {
            self.dom0 = None;
            return;
        };
        let s = pool_strings();
        let editable = xen_core::properties::multipath_editable(&conn.model, &host);
        let range = xen_core::properties::dom0_memory_range(&conn.model, &host);
        let (mut ok, mut cancel, mut maintenance) = (false, false, false);
        window(fill(s.dom0_title, &[("host", &host.name_label)])).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.label(s.dom0_blurb);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(s.dom0_label);
                let (min, max) = range
                    .map(|(a, b, _)| (a / (1024 * 1024), b / (1024 * 1024)))
                    .unwrap_or((d.memory, d.memory));
                ui.add_enabled(
                    editable,
                    DragValue::new(&mut d.memory).range(min..=max).suffix(" MB"),
                );
            });
            ui.add_space(4.0);
            if editable {
                ui.colored_label(ui.visuals().warn_fg_color, s.dom0_reboot);
            } else {
                ui.colored_label(ui.visuals().warn_fg_color, s.dom0_maintenance);
                maintenance = ui.link(s.dom0_enter_maintenance).clicked();
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ok = ui.button(s.ok).clicked();
                cancel = ui.button(s.cancel).clicked();
            });
        });
        if maintenance {
            ops.request(
                conn,
                ActionRequest::Host {
                    host: d.host.clone(),
                    action: HostAction::EnterMaintenance,
                },
                backend,
            );
        }
        if cancel || (ok && d.memory == d.original) {
            self.dom0 = None;
            return;
        }
        if ok {
            let request = ActionRequest::SetDom0Memory {
                host: d.host.clone(),
                memory: d.memory * 1024 * 1024,
            };
            ops.request(conn, request, backend);
            self.dom0 = None;
        }
    }

    fn install_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(d) = self.install.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.install = None;
            return;
        };
        let name = conn
            .model
            .hosts
            .get(&d.host)
            .map(|h| h.name_label.clone())
            .unwrap_or_default();
        let s = pool_strings();
        let (mut install, mut cancel) = (false, false);
        window(fill(s.install_title, &[("host", &name)])).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(s.install_blurb);
            ui.add_space(6.0);
            let error = |ui: &mut Ui, e: &Option<String>| {
                if let Some(e) = e {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            };
            ui.label(s.install_key);
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut d.key)
                        .desired_width(400.0)
                        .hint_text(s.path_hint),
                );
                crate::file_dialog::browse_button(
                    ui,
                    &mut d.key,
                    crate::file_dialog::Pick::Open(crate::file_dialog::key_filters()),
                );
            });
            error(ui, &d.errors.0);
            ui.label(s.install_cert);
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut d.certificate)
                        .desired_width(400.0)
                        .hint_text(s.path_hint),
                );
                crate::file_dialog::browse_button(
                    ui,
                    &mut d.certificate,
                    crate::file_dialog::Pick::Open(crate::file_dialog::certificate_filters()),
                );
            });
            error(ui, &d.errors.1);
            ui.label(s.install_chain);
            let mut remove = None;
            for (i, c) in d.chain.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(c);
                    if ui.small_button(s.install_remove).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                d.chain.remove(i);
            }
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut d.chain_entry).desired_width(320.0));
                crate::file_dialog::browse_button(
                    ui,
                    &mut d.chain_entry,
                    crate::file_dialog::Pick::Open(crate::file_dialog::certificate_filters()),
                );
                if ui.button(s.install_add).clicked() && !d.chain_entry.trim().is_empty() {
                    d.chain.push(d.chain_entry.trim().to_owned());
                    d.chain_entry.clear();
                }
            });
            error(ui, &d.errors.2);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !d.key.trim().is_empty() && !d.certificate.trim().is_empty();
                install = ui
                    .add_enabled(ready, Button::new(s.install_button))
                    .clicked();
                cancel = ui.button(s.cancel).clicked();
            });
        });
        if cancel {
            self.install = None;
            return;
        }
        if install {
            let missing = |p: &str| !std::path::Path::new(p.trim()).is_file();
            d.errors = (
                missing(&d.key).then(|| s.path_missing.to_owned()),
                missing(&d.certificate).then(|| s.path_missing.to_owned()),
                d.chain
                    .iter()
                    .find(|c| missing(c))
                    .map(|c| fill(s.path_missing_named, &[("path", c)])),
            );
            if d.errors.0.is_some() || d.errors.1.is_some() || d.errors.2.is_some() {
                return;
            }
            let request = ActionRequest::InstallCertificate {
                host: d.host.clone(),
                key: d.key.trim().to_owned(),
                certificate: d.certificate.trim().to_owned(),
                chain: d.chain.clone(),
            };
            ops.request(conn, request, backend);
            self.install = None;
        }
    }

    fn transfer_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(d) = self.transfer.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.transfer = None;
            return;
        };
        let name = conn
            .model
            .hosts
            .get(&d.host)
            .map(|h| h.name_label.clone())
            .unwrap_or_default();
        let s = pool_strings();
        let (mut go, mut cancel) = (false, false);
        let title = if d.restore {
            s.restore_title
        } else {
            s.backup_title
        };
        window(fill(title, &[("host", &name)])).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.label(fill(
                s.backup_file,
                &[
                    ("product", branding::PLATFORM_NAME),
                    ("ext", BACKUP_EXTENSION),
                ],
            ));
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut d.path)
                        .desired_width(360.0)
                        .hint_text(s.path_hint),
                );
                let filters = crate::file_dialog::backup_filters();
                let pick = if d.restore {
                    crate::file_dialog::Pick::Open(filters)
                } else {
                    crate::file_dialog::Pick::Save(filters)
                };
                crate::file_dialog::browse_button(ui, &mut d.path, pick);
            });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if d.restore {
                    s.restore_button
                } else {
                    s.backup_button
                };
                go = ui
                    .add_enabled(!d.path.trim().is_empty(), Button::new(label))
                    .clicked();
                cancel = ui.button(s.cancel).clicked();
            });
        });
        if cancel {
            self.transfer = None;
            return;
        }
        if go {
            let mut path = d.path.trim().to_owned();
            if d.restore {
                if !std::path::Path::new(&path).is_file() {
                    d.error = Some(s.path_missing.to_owned());
                    return;
                }
            } else if std::path::Path::new(&path).extension().is_none() {
                // `SaveFileDialog.AddExtension`.
                path.push('.');
                path.push_str(BACKUP_EXTENSION);
            }
            let request = if d.restore {
                ActionRequest::RestoreHost {
                    host: d.host.clone(),
                    path,
                }
            } else {
                ActionRequest::BackupHost {
                    host: d.host.clone(),
                    path,
                }
            };
            ops.request(conn, request, backend);
            self.transfer = None;
        }
    }
}

/// `BrandManager.ExtensionBackup`.
pub const BACKUP_EXTENSION: &str = "xbk";

/// The licensing, CPU masking and AD questions (`new_pool`: New Pool's
/// wording, else Add Server's).
fn warning_questions(
    members: &[&ConnectionView],
    coordinator: &ConnectionView,
    new_pool: bool,
) -> Vec<Question> {
    let s = pool_strings();
    pool::join_warnings(members, coordinator)
        .into_iter()
        .map(|w| {
            let (template, hosts) = match &w {
                JoinWarning::License(h) => (
                    if new_pool {
                        s.warn_license_new
                    } else {
                        s.warn_license_add
                    },
                    h,
                ),
                JoinWarning::CpuMasking(h) => (
                    if new_pool {
                        s.warn_cpu_new
                    } else {
                        s.warn_cpu_add
                    },
                    h,
                ),
                JoinWarning::ActiveDirectory(h) => (
                    if new_pool {
                        s.warn_ad_new
                    } else {
                        s.warn_ad_add
                    },
                    h,
                ),
            };
            Question {
                title: s.adding_title.to_owned(),
                text: fill(
                    template,
                    &[
                        ("hosts", &hosts.join("\n")),
                        ("product", branding::PLATFORM_NAME),
                    ],
                ),
                yes: s.yes.to_owned(),
            }
        })
        .collect()
}

pub(crate) fn window(title: String) -> egui::Window<'static> {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
}

impl PoolUi {
    /// A custom field dialog the Properties dialog asked for.
    pub fn take_org_dialog(&mut self) -> Option<crate::org_ui::OrgDialog> {
        self.properties.as_mut().and_then(|p| p.org_dialog.take())
    }

    /// Shows `page` of the open Properties dialog, if it has it.
    pub fn select_properties_page(&mut self, page: xen_core::properties::Page) {
        if let Some(p) = self.properties.as_mut()
            && p.pages.contains(&page)
        {
            p.page = page;
        }
    }
}

/// The open Properties dialog (tests).
#[cfg(test)]
impl PoolUi {
    pub fn props(&mut self) -> Option<&mut PropertiesDialog> {
        self.properties.as_deref_mut()
    }
}
