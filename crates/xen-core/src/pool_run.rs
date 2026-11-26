//! The pool and server operations of F10a (see [`crate::pool`] for when
//! each is offered and [`crate::properties`] for the Properties dialog).
//!
//! Each follows the original console's action, call for call:
//!
//! * **New Pool** (`CreatePoolAction`): the coordinator's pool of one gets
//!   the name and description; a default SR that is not shared is cleared
//!   (`ClearNonSharedSrs`: default, crash dump and suspend SRs); then each
//!   member joins with `pool.join` called on its own connection, given the
//!   coordinator's address and the credentials this console logged in
//!   with. A member that joined is dropped from the console's connections:
//!   from then on it is part of the pool's connection.
//!
//!   The original stops at the first member that cannot join and leaves
//!   the named pool behind. Here, when no member could join, the pool is
//!   made a standalone server again (name and description cleared, the
//!   SRs cleared before put back), so a failed New Pool leaves nothing.
//! * **Add Server** (`AddHostToPoolAction`): `pool.join` on the joining
//!   server's connection, then `ClearNonSharedSrs` on the pool. A failed
//!   join leaves both sides as they were (XAPI checks before it changes
//!   anything).
//! * **Remove Server** (`EjectHostFromPoolAction`): `pool.eject`; the
//!   server reboots as a standalone server, and the console connects to it
//!   once it answers again (`RemoveHostFromPoolCommand.WaitForReboot`: after
//!   30 s, every 15 s, for 5 minutes at most).
//! * **Make into standalone server** (`DestroyPoolAction`): the pool of
//!   one loses its name and description.
//! * **Properties** (`SaveChangesAction` and the pages' actions, in page
//!   order): see `save_properties`.
//! * **Control Domain Memory** (`ChangeControlDomainMemoryAction`):
//!   `VM.set_memory` on the control domain, then the server reboots as
//!   Reboot does.
//! * **Change Server Password** (`ChangeHostPasswordAction`):
//!   `session.change_password`; the console then logs in with the new one.
//! * **Rotate Pool Secret**, **Enable Certificate Verification**, **Reset
//!   Certificate**: one call each.
//! * **Install Certificate** (`InstallCertificateAction`): the files are
//!   read, `host.install_server_certificate`, then the console waits for
//!   the server to present the new certificate (a coordinator restarts its
//!   API; the connection asks about the new certificate when it
//!   reconnects).
//! * **Back Up / Restore** (`HostBackupRestoreAction`): a task is created
//!   and `GET /host_backup` (or `PUT /host_restore`) streams the file,
//!   then the task is followed to its end.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use xapi::error::codes;
use xapi::{Host, Network, Pool, Ref, Session, Sr};

use crate::actions::{ActionRequest, HostAction};
use crate::backend::{
    Command, ConnectionError, ConnectionId, Notification, OperationId, Password, Shared,
};
use crate::model::ConnectionModel;
use crate::objects;
use crate::operations::{
    OperationFailure, OperationKind, OperationStatus, OperationStep, PoolKind, Span, TaskOutcome,
    run_task,
};
use crate::pool;
use crate::properties::{
    ClusteringChange, EmailSettings, MAIL_DESTINATION, MAIL_LANGUAGE, NRPE_DEBUG_DISABLE,
    NRPE_DEBUG_ENABLE, NRPE_PLUGIN, NRPE_SSL_LOGGING_DISABLE, NRPE_SSL_LOGGING_ENABLE, NrpeConfig,
    POWER_ON_PASSWORD_SECRET, PowerOnMode, PropertiesTarget, PropertyChanges, SMTP_MAILHUB,
};
use crate::storage_run::{done, model, outcome, step};

/// The errors this module reports itself (worded by the UI).
pub mod errors {
    /// A server must join the pool's domain first and no domain account
    /// was given.
    pub const AD_CREDENTIALS: &str = "XENADMIN_AD_CREDENTIALS_REQUIRED";
    /// The password of the coordinator's connection could not be read.
    pub const NO_PASSWORD: &str = "XENADMIN_POOL_NO_PASSWORD";
    /// The server joined, but the console lost it before the pool listed
    /// it; params: the server's name.
    pub const JOIN_LOST: &str = "XENADMIN_POOL_JOIN_LOST";
    /// The current password given does not match (Change Server Password).
    pub const WRONG_PASSWORD: &str = "XENADMIN_WRONG_PASSWORD";
    /// The control domain memory is below its static minimum; params: the
    /// value asked for and the minimum, in bytes.
    pub const DOM0_MEMORY_TOO_LOW: &str = "XENADMIN_DOM0_MEMORY_TOO_LOW";
    /// A certificate file does not exist; params: which (`key`,
    /// `certificate`, `chain`) and the path.
    pub const FILE_MISSING: &str = "XENADMIN_FILE_MISSING";
    /// A certificate file could not be read; params: which and the path.
    pub const FILE_UNREADABLE: &str = "XENADMIN_FILE_UNREADABLE";
    /// The server is not live (Install Certificate).
    pub const HOST_UNREACHABLE: &str = "XENADMIN_HOST_UNREACHABLE";
    /// A local file of a backup or restore failed; params: the reason.
    pub const FILE_ERROR: &str = "XENADMIN_FILE_ERROR";
    /// The server did not present its new certificate in time.
    pub const CERTIFICATE_TIMEOUT: &str = "XENADMIN_CERTIFICATE_TIMEOUT";
}

fn client_error(code: &str, params: &[&str]) -> xapi::Error {
    crate::storage::client_error(code, params)
}

/// Like `?` for plain API calls returning a [`TaskOutcome`] on error.
macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Err(TaskOutcome::Failed(e)),
        }
    };
}

/// Runs an F10a request made on one connection.
pub(crate) async fn run(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::DestroyPool { pool } => outcome(destroy_pool(shared, op, s, pool).await),
        ActionRequest::EjectHost { host } => {
            outcome(eject_host(shared, connection, op, s, host).await)
        }
        ActionRequest::EvacuateCoordinator {
            host,
            new_coordinator,
        } => {
            let name = model(shared, connection)
                .hosts
                .get(new_coordinator)
                .map(|h| h.name_label.clone())
                .unwrap_or_default();
            crate::operations::enter_maintenance(shared, op, s, host, Some((new_coordinator, name)))
                .await
        }
        ActionRequest::SetProperties { target, changes } => {
            outcome(save_properties(shared, connection, op, s, target, changes).await)
        }
        ActionRequest::SetDom0Memory { host, memory } => {
            outcome(set_dom0_memory(shared, connection, op, s, host, *memory).await)
        }
        ActionRequest::ChangePassword { old, new } => {
            outcome(change_password(shared, connection, s, old, new).await)
        }
        ActionRequest::RotatePoolSecret => {
            run_task(
                shared,
                op,
                s,
                xapi::pool::async_rotate_secret(s),
                Span::FULL,
                false,
            )
            .await
        }
        ActionRequest::EnableTlsVerification => {
            match xapi::pool::enable_tls_verification(s).await {
                Ok(()) => TaskOutcome::Succeeded,
                Err(e) => TaskOutcome::Failed(e),
            }
        }
        ActionRequest::InstallCertificate {
            host,
            key,
            certificate,
            chain,
        } => outcome(
            install_certificate(shared, connection, op, s, host, key, certificate, chain).await,
        ),
        ActionRequest::ResetCertificate { host } => {
            let coordinator = objects::is_coordinator(&model(shared, connection), host);
            let start = xapi::host::async_reset_server_certificate(s, host);
            run_task(shared, op, s, start, Span::FULL, coordinator).await
        }
        ActionRequest::BackupHost { host, path } => {
            outcome(backup(shared, connection, op, s, host, path).await)
        }
        ActionRequest::RestoreHost { host, path } => {
            outcome(restore(shared, connection, op, s, host, path).await)
        }
        _ => unreachable!("not a pool request"),
    }
}

// ---- pools ------------------------------------------------------------------------

/// `DestroyPoolAction`.
async fn destroy_pool(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    pool: &Ref<Pool>,
) -> Result<(), TaskOutcome> {
    step(shared, op, OperationStep::UndoingPool);
    tri!(xapi::pool::set_name_label(s, pool, "").await);
    tri!(xapi::pool::set_name_description(s, pool, "").await);
    Ok(())
}

/// The pool's default, crash dump and suspend SRs, as `ClearNonSharedSrs`
/// found them (to put back when a New Pool is undone).
type DefaultSrs = (Ref<Sr>, Ref<Sr>, Ref<Sr>);

/// `PoolAbstractAction.ClearNonSharedSrs`: a pool whose default SR is not
/// shared gets no default, crash dump and suspend SRs. Returns what was
/// cleared.
async fn clear_non_shared_srs(
    model: &ConnectionModel,
    s: &Session,
) -> Result<Option<DefaultSrs>, TaskOutcome> {
    let Some((pool_ref, pool)) = model.pool() else {
        return Ok(None);
    };
    let local = model.srs.get(&pool.default_sr).is_some_and(|sr| !sr.shared);
    if !local {
        return Ok(None);
    }
    let null = Ref::null();
    tri!(xapi::pool::set_default_sr(s, pool_ref, &null).await);
    tri!(xapi::pool::set_crash_dump_sr(s, pool_ref, &null).await);
    tri!(xapi::pool::set_suspend_image_sr(s, pool_ref, &null).await);
    Ok(Some((
        pool.default_sr.clone(),
        pool.crash_dump_sr.clone(),
        pool.suspend_image_sr.clone(),
    )))
}

/// A connection of the backend's loop: its id and session.
pub(crate) type Member = (ConnectionId, Session);

/// Starts New Pool: `coordinator`'s pool of one becomes pool `name`, and
/// `members` join it (`CreatePoolAction`).
pub(crate) fn start_new_pool(
    shared: &Arc<Shared>,
    coordinator: Option<Member>,
    members: Vec<Option<Member>>,
    name: String,
    description: String,
    ad: Option<(String, Password)>,
) {
    let (conn_id, session) = match coordinator {
        Some(c) => c,
        None => return,
    };
    let state = shared.state();
    let Some(view) = state.connection(conn_id).cloned() else {
        return;
    };
    let coordinator_name = view
        .model
        .coordinator()
        .map(|h| h.name_label.clone())
        .unwrap_or_default();
    let op = shared.start_operation(
        Some(conn_id),
        &view.server,
        OperationKind::Pool(PoolKind::CreatePool {
            name: name.clone(),
            coordinator: coordinator_name,
        }),
    );
    let shared = shared.clone();
    tokio::spawn(async move {
        let result = create_pool(
            &shared,
            conn_id,
            op,
            &session,
            members,
            (&name, &description),
            ad.as_ref(),
        )
        .await;
        finish(&shared, conn_id, op, result);
    });
}

async fn create_pool(
    shared: &Arc<Shared>,
    conn_id: ConnectionId,
    op: OperationId,
    s: &Session,
    members: Vec<Option<Member>>,
    (name, description): (&str, &str),
    ad: Option<&(String, Password)>,
) -> TaskOutcome {
    let m = model(shared, conn_id);
    let Some((pool_ref, _)) = m.pool() else {
        return TaskOutcome::Failed(client_error(codes::HANDLE_INVALID, &["pool"]));
    };
    let pool_ref = pool_ref.clone();
    step(shared, op, OperationStep::CreatingPool);
    if let Err(e) = xapi::pool::set_name_label(s, &pool_ref, name).await {
        return TaskOutcome::Failed(e);
    }
    let undo = |cleared: Option<DefaultSrs>| {
        let pool_ref = pool_ref.clone();
        async move {
            step(shared, op, OperationStep::UndoingPool);
            let _ = xapi::pool::set_name_label(s, &pool_ref, "").await;
            let _ = xapi::pool::set_name_description(s, &pool_ref, "").await;
            if let Some((default, crash, suspend)) = cleared {
                let _ = xapi::pool::set_default_sr(s, &pool_ref, &default).await;
                let _ = xapi::pool::set_crash_dump_sr(s, &pool_ref, &crash).await;
                let _ = xapi::pool::set_suspend_image_sr(s, &pool_ref, &suspend).await;
            }
        }
    };
    if let Err(e) = xapi::pool::set_name_description(s, &pool_ref, description).await {
        undo(None).await;
        return TaskOutcome::Failed(e);
    }
    let cleared = match clear_non_shared_srs(&m, s).await {
        Ok(c) => c,
        Err(o) => {
            undo(None).await;
            return o;
        }
    };
    step(shared, op, OperationStep::AddingMembers);
    let count = members.len().max(1) as f64;
    let mut joined = 0;
    for (i, member) in members.into_iter().enumerate() {
        let span = Span(
            0.1 + 0.9 * i as f64 / count,
            0.1 + 0.9 * (i + 1) as f64 / count,
        );
        let result = match member {
            Some(member) => join(shared, conn_id, op, member, span, ad).await,
            None => Err(TaskOutcome::Failed(xapi::Error::Protocol(
                "a member is no longer connected".to_owned(),
            ))),
        };
        match result {
            Ok(()) => joined += 1,
            Err(o) => {
                if joined == 0 {
                    undo(cleared).await;
                }
                return o;
            }
        }
    }
    TaskOutcome::Succeeded
}

/// Starts Add Server: each of `hosts` joins the pool of `pool`
/// (`AddHostToPoolAction`, one history entry per server).
pub(crate) fn start_join(
    shared: &Arc<Shared>,
    pool: Option<Member>,
    hosts: Vec<Option<Member>>,
    ad: Option<(String, Password)>,
) {
    let Some((pool_conn, pool_session)) = pool else {
        return;
    };
    let state = shared.state();
    let Some(view) = state.connection(pool_conn).cloned() else {
        return;
    };
    let pool_name = view
        .model
        .pool()
        .map(|(_, p)| objects::pool_name(&view.model, p))
        .unwrap_or_default();
    for member in hosts.into_iter().flatten() {
        let host_name = state
            .connection(member.0)
            .and_then(|c| c.model.coordinator().map(|h| h.name_label.clone()))
            .unwrap_or_default();
        let op = shared.start_operation(
            Some(pool_conn),
            &view.server,
            OperationKind::Pool(PoolKind::AddHost {
                host: host_name,
                pool: pool_name.clone(),
            }),
        );
        let shared = shared.clone();
        let pool_session = pool_session.clone();
        let ad = ad.clone();
        tokio::spawn(async move {
            let result = async {
                step(&shared, op, OperationStep::Joining);
                join(&shared, pool_conn, op, member, Span::FULL, ad.as_ref()).await?;
                let m = model(&shared, pool_conn);
                clear_non_shared_srs(&m, &pool_session).await?;
                Ok(())
            }
            .await;
            finish(&shared, pool_conn, op, outcome(result));
        });
    }
}

/// `pool.join` on `member`'s connection with the coordinator's address and
/// credentials; once the pool lists the server, the member's connection is
/// dropped (`ConnectionsManager.ClearCacheAndRemoveConnection`).
async fn join(
    shared: &Arc<Shared>,
    pool_conn: ConnectionId,
    op: OperationId,
    (member, member_session): Member,
    span: Span,
    ad: Option<&(String, Password)>,
) -> Result<(), TaskOutcome> {
    let state = shared.state();
    let (Some(pool_view), Some(member_view)) = (
        state.connection(pool_conn).cloned(),
        state.connection(member).cloned(),
    ) else {
        return Err(TaskOutcome::Failed(xapi::Error::Protocol(
            "the connection went away".to_owned(),
        )));
    };
    let (uuid, name) = member_view
        .model
        .coordinator()
        .map(|h| (h.uuid.clone(), h.name_label.clone()))
        .unwrap_or_default();
    let password = match shared.connection_password(pool_conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = ?e, "pool join: no password for the coordinator");
            return Err(TaskOutcome::Failed(client_error(errors::NO_PASSWORD, &[])));
        }
    };
    // `PoolAbstractAction.FixAd`: a server without the coordinator's AD
    // configuration joins its domain first.
    if let (Some(c), Some(h)) = (
        pool_view.model.coordinator(),
        member_view.model.coordinator(),
    ) && !pool::compatible_ad_config(h, c, false)
        && !c.external_auth_type.is_empty()
    {
        let Some((user, password)) = ad else {
            return Err(TaskOutcome::Failed(client_error(
                errors::AD_CREDENTIALS,
                &[],
            )));
        };
        let Some((member_pool, _)) = member_view.model.pool() else {
            return Err(TaskOutcome::Failed(client_error(
                codes::HANDLE_INVALID,
                &["pool"],
            )));
        };
        step(shared, op, OperationStep::JoiningDomain);
        tracing::info!(member = %member_view.server, "joining the pool's domain first");
        crate::ha_run::join_domain(
            &member_session,
            member_pool,
            &c.external_auth_service_name,
            user,
            password.expose(),
        )
        .await
        .map_err(TaskOutcome::Failed)?;
        step(shared, op, OperationStep::Joining);
    }
    let address = pool::pool_join_address(&pool_view);
    tracing::info!(member = %member_view.server, "joining the pool");
    let start = xapi::pool::async_join(
        &member_session,
        &address,
        &pool_view.username,
        password.expose(),
    );
    // The joining server restarts its API once it joined: losing it then
    // is expected; the pool listing it decides.
    done(run_task(shared, op, &member_session, start, span, true).await)?;
    if !wait_for_host(shared, pool_conn, &uuid).await {
        return Err(TaskOutcome::Failed(client_error(
            errors::JOIN_LOST,
            &[&name],
        )));
    }
    if let Some(view) = shared.state().connection(member) {
        tracing::info!(member = %view.server, "joined; dropping its connection");
        shared.send(Command::Forget {
            address: view.server.clone(),
        });
    }
    Ok(())
}

/// Waits until the pool's connection lists the server with `uuid`.
async fn wait_for_host(shared: &Shared, conn: ConnectionId, uuid: &str) -> bool {
    let deadline = tokio::time::Instant::now() + shared.options.join_wait;
    loop {
        if model(shared, conn).hosts.values().any(|h| h.uuid == uuid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(shared.options.task_poll_interval).await;
    }
}

/// Ends a multi-connection operation and reports a failure.
fn finish(shared: &Shared, connection: ConnectionId, op: OperationId, outcome: TaskOutcome) {
    shared.update_operation(op, |o| o.step = None);
    match outcome {
        TaskOutcome::Succeeded => shared.finish_operation(op, OperationStatus::Succeeded),
        TaskOutcome::Cancelled => shared.finish_operation(op, OperationStatus::Cancelled),
        TaskOutcome::Failed(error) => {
            tracing::warn!(error = %error, "pool operation failed");
            let error = Arc::new(error);
            shared.finish_operation(
                op,
                OperationStatus::Failed(OperationFailure::Xapi(error.clone())),
            );
            let kind = shared.state().operation(op).map(|o| o.kind.clone());
            if let Some(kind) = kind
                && shared.state().connection(connection).is_some()
            {
                shared.notify(Notification::ActionFailed {
                    connection,
                    operation: op,
                    kind,
                    error,
                });
            }
        }
    }
}

/// `EjectHostFromPoolAction`, then `WaitForReboot` in the background.
async fn eject_host(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    host: &Ref<Host>,
) -> Result<(), TaskOutcome> {
    let state = shared.state();
    let view = state.connection(connection).cloned();
    let target = view.as_ref().and_then(|v| {
        let connected = xapi::Endpoint::parse(&v.server).ok()?;
        v.model.hosts.get(host)?;
        let endpoint = crate::performance::host_endpoint(&v.model, &connected, host);
        Some((endpoint.to_string(), v.username.clone()))
    });
    let password = shared.connection_password(connection).await.ok();
    step(shared, op, OperationStep::Ejecting);
    done(
        run_task(
            shared,
            op,
            s,
            xapi::pool::async_eject(s, host),
            Span::FULL,
            false,
        )
        .await,
    )?;
    match (target, password) {
        (Some((server, username)), Some(password)) => {
            tokio::spawn(reconnect_when_back(
                shared.clone(),
                server,
                username,
                password,
            ));
        }
        _ => tracing::info!("ejected; not reconnecting (no address or password)"),
    }
    Ok(())
}

/// `RemoveHostFromPoolCommand.WaitForReboot`: waits, then tries to reach
/// the server every so often and connects once it answers.
async fn reconnect_when_back(
    shared: Arc<Shared>,
    server: String,
    username: String,
    password: crate::backend::Password,
) {
    let opts = shared.options.clone();
    tokio::time::sleep(opts.eject_reconnect_delay).await;
    let Ok(endpoint) = xapi::Endpoint::parse(&server) else {
        return;
    };
    for _ in 0..=opts.eject_reconnect_attempts {
        let reachable = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        if reachable {
            tracing::info!(%server, "the removed server is back; connecting");
            shared.send(Command::Connect {
                server,
                username,
                password: Some(password),
                remember_password: true,
            });
            return;
        }
        tokio::time::sleep(opts.eject_reconnect_poll).await;
    }
    shared.notify(Notification::ReconnectFailed { server });
}

// ---- Properties ---------------------------------------------------------------------

/// The Properties dialog's save: `SaveChangesAction` (name, description,
/// IQN, log destination field), then the pages' actions in page order
/// (tags, alerts, email, multipathing, `syslog_reconfigure`, power on,
/// auto-start, live patching, IGMP snooping, clustering, migration
/// compression, NRPE).
pub(crate) async fn save_properties(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    target: &PropertiesTarget,
    c: &PropertyChanges,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some((pool_ref, pool_rec)) = m.pool().map(|(r, p)| (r.clone(), p.clone())) else {
        return Err(TaskOutcome::Failed(client_error(
            codes::HANDLE_INVALID,
            &["pool"],
        )));
    };
    step(shared, op, OperationStep::SavingSettings);
    match target {
        PropertiesTarget::Pool(p) => {
            if let Some(name) = &c.name {
                tri!(xapi::pool::set_name_label(s, p, name).await);
            }
            if let Some(d) = &c.description {
                tri!(xapi::pool::set_name_description(s, p, d).await);
            }
        }
        PropertiesTarget::Host(h) => {
            if let Some(name) = &c.name {
                tri!(xapi::host::set_name_label(s, h, name).await);
            }
            if let Some(d) = &c.description {
                tri!(xapi::host::set_name_description(s, h, d).await);
            }
            if let Some(iqn) = &c.iqn {
                tri!(xapi::host::set_iscsi_iqn(s, h, iqn).await);
            }
            if let Some(dest) = &c.syslog {
                let mut logging = m
                    .hosts
                    .get(h)
                    .map(|r| r.logging.clone())
                    .unwrap_or_default();
                match dest {
                    Some(d) => {
                        logging.insert("syslog_destination".to_owned(), d.clone());
                    }
                    None => {
                        logging.remove("syslog_destination");
                    }
                }
                tri!(xapi::host::set_logging(s, h, &logging).await);
            }
        }
    }
    if let Some(tags) = &c.tags {
        match target {
            PropertiesTarget::Pool(p) => tri!(xapi::pool::set_tags(s, p, tags).await),
            PropertiesTarget::Host(h) => tri!(xapi::host::set_tags(s, h, tags).await),
        }
    }
    if let (Some(config), PropertiesTarget::Host(h)) = (&c.perfmon, target) {
        let object = crate::actions::PerfmonObjectRef::Host(h.clone());
        done(
            crate::operations::set_perfmon(shared, connection, s, &object, config.as_deref()).await,
        )?;
    }
    if let Some(email) = &c.email {
        save_email(s, &pool_ref, email).await?;
    }
    if let (Some(on), PropertiesTarget::Host(h)) = (c.multipath, target) {
        set_multipath(&m, s, h, on).await?;
    }
    if let (Some(_), PropertiesTarget::Host(h)) = (&c.syslog, target) {
        tri!(xapi::host::syslog_reconfigure(s, h).await);
    }
    for (host, mode) in &c.power_on {
        save_power_on(s, host, mode).await?;
    }
    if let Some(on) = c.autostart {
        let _ = xapi::pool::remove_from_other_config(s, &pool_ref, "auto_poweron").await;
        tri!(
            xapi::pool::add_to_other_config(
                s,
                &pool_ref,
                "auto_poweron",
                if on { "true" } else { "false" }
            )
            .await
        );
    }
    if let Some(disabled) = c.live_patching_disabled {
        tri!(xapi::pool::set_live_patching_disabled(s, &pool_ref, disabled).await);
    }
    if let Some(on) = c.igmp_snooping {
        tri!(xapi::pool::set_igmp_snooping_enabled(s, &pool_ref, on).await);
    }
    match &c.clustering {
        Some(ClusteringChange::Enable(network)) => {
            enable_clustering(shared, op, &m, s, network).await?;
        }
        Some(ClusteringChange::Disable) => disable_clustering(shared, op, &m, s).await?,
        None => {}
    }
    if let Some(on) = c.migration_compression {
        tri!(xapi::pool::set_migration_compression(s, &pool_ref, on).await);
    }
    if let Some((new, old)) = &c.nrpe {
        let hosts: Vec<Ref<Host>> = match target {
            PropertiesTarget::Host(h) => vec![h.clone()],
            PropertiesTarget::Pool(_) => m.hosts.keys().cloned().collect(),
        };
        for host in hosts {
            update_nrpe(s, &host, new, old).await?;
        }
    }
    let _ = pool_rec;
    Ok(())
}

/// `PerfmonOptionsDefinitionAction`.
async fn save_email(
    s: &Session,
    pool: &Ref<Pool>,
    email: &EmailSettings,
) -> Result<(), TaskOutcome> {
    for (key, value) in [
        (MAIL_DESTINATION, &email.destination),
        (SMTP_MAILHUB, &email.mailhub),
        (MAIL_LANGUAGE, &email.language),
    ] {
        tri!(xapi::pool::remove_from_other_config(s, pool, key).await);
        if let Some(v) = value {
            tri!(xapi::pool::add_to_other_config(s, pool, key, v).await);
        }
    }
    Ok(())
}

/// `EditMultipathAction`: the plugged PBDs are unplugged, the setting
/// changed, then the PBDs plugged again (every one tried; the first error
/// counts).
async fn set_multipath(
    m: &ConnectionModel,
    s: &Session,
    host: &Ref<Host>,
    on: bool,
) -> Result<(), TaskOutcome> {
    const HANDLE: &str = "multipathhandle";
    let plugged: Vec<Ref<xapi::Pbd>> = m
        .hosts
        .get(host)
        .map(|h| {
            h.pbds
                .iter()
                .filter(|p| m.pbds.get(*p).is_some_and(|p| p.currently_attached))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut unplugged = Vec::new();
    let change = async {
        for pbd in &plugged {
            xapi::pbd::unplug(s, pbd).await?;
            unplugged.push(pbd.clone());
        }
        xapi::host::set_multipathing(s, host, on).await?;
        xapi::host::remove_from_other_config(s, host, HANDLE).await?;
        if on {
            xapi::host::add_to_other_config(s, host, HANDLE, "dmp").await?;
        }
        Ok::<(), xapi::Error>(())
    }
    .await;
    let mut plug_error = None;
    for pbd in &unplugged {
        if let Err(e) = xapi::pbd::plug(s, pbd).await {
            tracing::debug!(error = %e, "replugging a PBD failed");
            plug_error.get_or_insert(e);
        }
    }
    match (change, plug_error) {
        (Err(e), _) | (Ok(()), Some(e)) => Err(TaskOutcome::Failed(e)),
        (Ok(()), None) => Ok(()),
    }
}

/// `SavePowerOnSettingsAction.SaveConfig`: a password goes to a new
/// `secret` (destroyed again if the mode cannot be set).
async fn save_power_on(
    s: &Session,
    host: &Ref<Host>,
    mode: &PowerOnMode,
) -> Result<(), TaskOutcome> {
    let mut config: HashMap<String, String> = mode.config().into_iter().collect();
    let mut secret = None;
    if !matches!(mode, PowerOnMode::Disabled | PowerOnMode::WakeOnLan)
        && let Some(password) = config.get(POWER_ON_PASSWORD_SECRET).cloned()
    {
        let record = xapi::SecretRecord {
            value: password,
            ..Default::default()
        };
        let r = tri!(xapi::secret::create(s, &record).await);
        let uuid = tri!(xapi::secret::get_uuid(s, &r).await);
        config.insert(POWER_ON_PASSWORD_SECRET.to_owned(), uuid);
        secret = Some(r);
    }
    if let Err(e) = xapi::host::set_power_on_mode(s, host, mode.mode_name(), &config).await {
        if let Some(r) = secret {
            let _ = xapi::secret::destroy(s, &r).await;
        }
        return Err(TaskOutcome::Failed(e));
    }
    Ok(())
}

/// `EnableClusteringAction`: the network's PIFs may no longer be unplugged,
/// then `Cluster.pool_create` with corosync.
async fn enable_clustering(
    shared: &Shared,
    op: OperationId,
    m: &ConnectionModel,
    s: &Session,
    network: &Ref<Network>,
) -> Result<(), TaskOutcome> {
    let pifs = m
        .networks
        .get(network)
        .map(|n| n.pifs.clone())
        .unwrap_or_default();
    for pif in &pifs {
        tri!(xapi::pif::set_disallow_unplug(s, pif, true).await);
    }
    let start = xapi::cluster::async_pool_create(s, network, "corosync", None, None);
    done(run_task(shared, op, s, start, Span(0.5, 1.0), false).await)
}

/// `DisableClusteringAction`.
async fn disable_clustering(
    shared: &Shared,
    op: OperationId,
    m: &ConnectionModel,
    s: &Session,
) -> Result<(), TaskOutcome> {
    let Some((cluster, record)) = m.clusters.iter().next() else {
        return Ok(());
    };
    let start = xapi::cluster::async_pool_destroy(s, cluster);
    done(run_task(shared, op, s, start, Span(0.5, 1.0), false).await)?;
    for ch in record
        .cluster_hosts
        .iter()
        .filter_map(|c| m.cluster_hosts.get(c))
    {
        tri!(xapi::pif::set_disallow_unplug(s, &ch.pif, false).await);
    }
    Ok(())
}

/// `NRPEUpdateAction.SetNRPEConfigureForHost`.
async fn update_nrpe(
    s: &Session,
    host: &Ref<Host>,
    new: &NrpeConfig,
    old: &NrpeConfig,
) -> Result<(), TaskOutcome> {
    let call = |f: &'static str, args: HashMap<String, String>| async move {
        xapi::host::call_plugin(s, host, NRPE_PLUGIN, f, &args).await
    };
    if new.enabled != old.enabled {
        if new.enabled {
            let args = HashMap::from([("enable".to_owned(), "true".to_owned())]);
            if let Err(e) = call("control", args).await {
                tracing::warn!(error = %e, "nrpe control failed");
            }
        }
        tri!(
            call(
                if new.enabled { "enable" } else { "disable" },
                HashMap::new()
            )
            .await
        );
    }
    if !new.enabled {
        return Ok(());
    }
    if new.allow_hosts != old.allow_hosts
        || new.debug != old.debug
        || new.ssl_logging != old.ssl_logging
    {
        let args = HashMap::from([
            ("allowed_hosts".to_owned(), new.allowed_hosts_arg()),
            (
                "debug".to_owned(),
                if new.debug {
                    NRPE_DEBUG_ENABLE
                } else {
                    NRPE_DEBUG_DISABLE
                }
                .to_owned(),
            ),
            (
                "ssl_logging".to_owned(),
                if new.ssl_logging {
                    NRPE_SSL_LOGGING_ENABLE
                } else {
                    NRPE_SSL_LOGGING_DISABLE
                }
                .to_owned(),
            ),
        ]);
        tri!(call("set-config", args).await);
    }
    for (name, (w, c)) in &new.thresholds {
        if let Some((ow, oc)) = old.thresholds.get(name)
            && (ow != w || oc != c)
        {
            let args = HashMap::from([
                (name.clone(), String::new()),
                ("w".to_owned(), w.clone()),
                ("c".to_owned(), c.clone()),
            ]);
            tri!(call("set-threshold", args).await);
        }
    }
    tri!(call("restart", HashMap::new()).await);
    Ok(())
}

/// `NRPERetrieveAction` (the NRPE page reads it when it opens).
pub(crate) async fn read_nrpe(s: &Session, host: &Ref<Host>) -> xapi::Result<NrpeConfig> {
    let none = HashMap::new();
    let status = xapi::host::call_plugin(s, host, NRPE_PLUGIN, "status", &none).await?;
    let config = xapi::host::call_plugin(s, host, NRPE_PLUGIN, "get-config", &none).await?;
    let thresholds = xapi::host::call_plugin(s, host, NRPE_PLUGIN, "get-threshold", &none).await?;
    Ok(NrpeConfig::parse(&status, &config, &thresholds))
}

/// `PowerOnMode.GetSecret` for each server with a `power_on_password_secret`
/// (read when the Power On page opens); unreadable ones are left out.
pub(crate) async fn read_power_on_secrets(
    s: &Session,
    model: &ConnectionModel,
    hosts: &[Ref<Host>],
) -> Vec<(Ref<Host>, String)> {
    let mut out = Vec::new();
    for h in hosts {
        let Some(uuid) = model
            .hosts
            .get(h)
            .and_then(|r| r.power_on_config.get(POWER_ON_PASSWORD_SECRET))
        else {
            continue;
        };
        let value = async {
            let r = xapi::secret::get_by_uuid(s, uuid).await?;
            xapi::secret::get_value(s, &r).await
        }
        .await;
        match value {
            Ok(v) => out.push((h.clone(), v)),
            Err(e) => tracing::debug!(error = %e, "power-on secret unreadable"),
        }
    }
    out
}

// ---- server settings -------------------------------------------------------------------

/// `ChangeControlDomainMemoryAction`, then the server reboots.
async fn set_dom0_memory(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    host: &Ref<Host>,
    memory: i64,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some(dom0) = m.hosts.get(host).map(|h| h.control_domain.clone()) else {
        return Err(TaskOutcome::Failed(client_error(
            codes::HANDLE_INVALID,
            &["host"],
        )));
    };
    if let Err(e) = xapi::vm::set_memory(s, &dom0, memory).await {
        let min = m.vms.get(&dom0).map_or(0, |v| v.memory_static_min);
        if e.is_api("MEMORY_CONSTRAINT_VIOLATION") && memory < min {
            return Err(TaskOutcome::Failed(client_error(
                errors::DOM0_MEMORY_TOO_LOW,
                &[&memory.to_string(), &min.to_string()],
            )));
        }
        return Err(TaskOutcome::Failed(e));
    }
    done(
        crate::operations::reboot_or_shutdown(shared, connection, op, s, host, HostAction::Reboot)
            .await,
    )
}

/// `ChangeHostPasswordAction` (the dialog checked the current password;
/// it is checked again here).
async fn change_password(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    old: &crate::backend::Password,
    new: &crate::backend::Password,
) -> Result<(), TaskOutcome> {
    match shared.connection_password(connection).await {
        Ok(current) if current == *old => {}
        Ok(_) => {
            return Err(TaskOutcome::Failed(client_error(
                errors::WRONG_PASSWORD,
                &[],
            )));
        }
        Err(ConnectionError::PasswordRequired) | Err(_) => {
            return Err(TaskOutcome::Failed(client_error(errors::NO_PASSWORD, &[])));
        }
    }
    tri!(xapi::session::change_password(s, old.expose(), new.expose()).await);
    shared.set_connection_password(connection, new).await;
    Ok(())
}

/// Reads a certificate file (`InstallCertificateAction.CollectFileContents`).
async fn read_file(which: &str, path: &str) -> Result<String, TaskOutcome> {
    let p = PathBuf::from(path);
    if !p.is_file() {
        return Err(TaskOutcome::Failed(client_error(
            errors::FILE_MISSING,
            &[which, path],
        )));
    }
    let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(p)).await;
    match read {
        Ok(Ok(text)) => Ok(text),
        _ => Err(TaskOutcome::Failed(client_error(
            errors::FILE_UNREADABLE,
            &[which, path],
        ))),
    }
}

/// `InstallCertificateAction`.
#[allow(clippy::too_many_arguments)]
async fn install_certificate(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    host: &Ref<Host>,
    key: &str,
    certificate: &str,
    chain: &[String],
) -> Result<(), TaskOutcome> {
    let private_key = read_file("key", key).await?;
    let cert = read_file("certificate", certificate).await?;
    let mut parts = Vec::new();
    for c in chain {
        parts.push(read_file("chain", c).await?);
    }
    let chain = parts.join("\n");
    let m = model(shared, connection);
    let Some(rec) = m.hosts.get(host) else {
        return Err(TaskOutcome::Failed(client_error(
            errors::HOST_UNREACHABLE,
            &[],
        )));
    };
    if !objects::host_is_live(&m, rec) {
        return Err(TaskOutcome::Failed(client_error(
            errors::HOST_UNREACHABLE,
            &[],
        )));
    }
    let coordinator = objects::is_coordinator(&m, host);
    let old_uuid = rec
        .certificates
        .first()
        .and_then(|c| m.certificates.get(c))
        .map(|c| c.uuid.clone());
    let start = xapi::host::async_install_server_certificate(
        s,
        host,
        &cert,
        &private_key,
        (!chain.is_empty()).then_some(chain.as_str()),
    );
    done(run_task(shared, op, s, start, Span(0.3, 0.5), coordinator).await)?;
    if coordinator {
        // The coordinator restarted its API with the new certificate; the
        // connection asks about it when it reconnects.
        return Ok(());
    }
    step(shared, op, OperationStep::WaitingForCertificate);
    let deadline = tokio::time::Instant::now() + shared.options.certificate_wait;
    loop {
        let current = async {
            let h = xapi::host::get_record(s, host).await?;
            match h.certificates.first() {
                Some(c) => Ok(Some(xapi::certificate::get_record(s, c).await?.uuid)),
                None => Ok::<_, xapi::Error>(None),
            }
        }
        .await;
        if let Ok(uuid) = current
            && uuid.is_some()
            && uuid != old_uuid
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(TaskOutcome::Failed(client_error(
                errors::CERTIFICATE_TIMEOUT,
                &[],
            )));
        }
        tokio::time::sleep(shared.options.task_poll_interval).await;
    }
}

// ---- backup and restore ---------------------------------------------------------------

/// The HTTP client for `host`'s handlers (the server itself, not the
/// coordinator: `HostBackupRestoreAction` uses `Host.address`).
fn transfer_client(
    shared: &Shared,
    connection: ConnectionId,
    host: &Ref<Host>,
) -> xapi::Result<(xapi::http::HttpTransfer, String)> {
    let state = shared.state();
    let view = state
        .connection(connection)
        .ok_or_else(|| xapi::Error::Protocol("not connected".to_owned()))?;
    let connected = xapi::Endpoint::parse(&view.server)?;
    let endpoint = crate::performance::host_endpoint(&view.model, &connected, host);
    let tls = if endpoint.tls {
        let accepted = crate::performance::accepted_fingerprints(
            &view.model,
            host,
            &endpoint,
            shared.store().trust(),
        );
        Some(xapi::pinned_client_config(accepted)?)
    } else {
        None
    };
    let address = view
        .model
        .hosts
        .get(host)
        .map(|h| h.address.clone())
        .unwrap_or_default();
    Ok((
        xapi::http::HttpTransfer::new(&endpoint.base_url(), tls, shared.options.call_timeout)?,
        address,
    ))
}

fn file_error(e: &xapi::Error) -> TaskOutcome {
    match e {
        xapi::Error::Io(io) => {
            TaskOutcome::Failed(client_error(errors::FILE_ERROR, &[&io.to_string()]))
        }
        xapi::Error::Cancelled => TaskOutcome::Cancelled,
        _ => TaskOutcome::Failed(xapi::Error::Protocol(e.to_string())),
    }
}

/// `HostBackupRestoreAction` backup.
async fn backup(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    host: &Ref<Host>,
    path: &str,
) -> Result<(), TaskOutcome> {
    let (client, address) = tri!(transfer_client(shared, connection, host));
    let task = tri!(xapi::task::create(s, "get_host_backup_task", &address).await);
    shared.update_operation(op, |o| {
        o.task = Some(task.clone());
        o.step = Some(OperationStep::BackingUp { received: 0 });
    });
    let received = |bytes: i64| {
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::BackingUp { received: bytes })
        });
    };
    let got = client
        .get_to_file(
            "host_backup",
            &[
                ("session_id", s.session_ref().as_str()),
                ("task_id", task.as_str()),
            ],
            std::path::Path::new(path),
            &received,
            &|| false,
        )
        .await;
    let followed = run_task(shared, op, s, async { Ok(task.clone()) }, Span::FULL, false).await;
    if let Err(e) = got {
        let _ = std::fs::remove_file(path);
        return Err(match followed {
            TaskOutcome::Failed(task_error) => TaskOutcome::Failed(task_error),
            _ => file_error(&e),
        });
    }
    done(followed)
}

/// `HostBackupRestoreAction` restore.
async fn restore(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    host: &Ref<Host>,
    path: &str,
) -> Result<(), TaskOutcome> {
    if !PathBuf::from(path).is_file() {
        return Err(TaskOutcome::Failed(client_error(
            errors::FILE_MISSING,
            &["backup", path],
        )));
    }
    let (client, address) = tri!(transfer_client(shared, connection, host));
    let task = tri!(xapi::task::create(s, "put_host_restore_task", &address).await);
    shared.update_operation(op, |o| o.task = Some(task.clone()));
    let sent = client
        .put_file(
            "host_restore",
            &[
                ("session_id", s.session_ref().as_str()),
                ("task_id", task.as_str()),
            ],
            std::path::Path::new(path),
        )
        .await;
    let followed = run_task(shared, op, s, async { Ok(task.clone()) }, Span::FULL, false).await;
    if let Err(e) = sent {
        return Err(match followed {
            TaskOutcome::Failed(task_error) => TaskOutcome::Failed(task_error),
            _ => file_error(&e),
        });
    }
    done(followed)
}
