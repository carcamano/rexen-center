//! One connection: an actor that owns a XAPI session for its lifetime.
//!
//! 1. **Log in** with the given or saved password. An untrusted certificate
//!    is shown to the user (unknown or changed) and pinned once accepted;
//!    `HOST_IS_SLAVE` is followed to the pool coordinator.
//! 2. **Load** every cached class after taking an `event.inject` token,
//!    refuse servers older than [`xapi::MIN_API_VERSION`] and refuse a pool
//!    another connection already has. The server is then saved (with its
//!    password in the keyring if asked).
//! 3. **Follow** `event.from` (server timeout shorter than the HTTP timeout),
//!    reloading in place on `EVENTS_LOST`, while a heartbeat calls
//!    `host.get_servertime` on the coordinator every
//!    [`BackendOptions::heartbeat_interval`]. `SESSION_INVALID`, a dead
//!    event stream or two failed heartbeats in a row mean the connection is
//!    lost.
//! 4. **Reconnect** with exponential backoff (back to step 1, re-reading the
//!    password from the keyring), until it works, the user disconnects, or a
//!    failure needs the user (wrong password, refused certificate…).
//!
//! The first connection attempt is not retried: a typo in the address should
//! fail at once.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio::time::Instant;
use xapi::error::codes;
use xapi::{ClientOptions, Endpoint, JsonRpcClient, Session};

use crate::backend::{
    BackendOptions, CertificatePrompt, ConnectionError, ConnectionId, ConnectionStatus,
    Notification, OperationId, Password, SessionSlot, Shared, event_client_timeout, logout_later,
    slot_set,
};
use crate::model::{ConnectionModel, EVENT_CLASSES, class_loaders};
use crate::operations::{OperationFailure, OperationKind, OperationStatus};
use crate::secrets::SecretError;
use crate::store::Store;

/// Concurrent `get_all_records` calls during the initial load.
const LOAD_CONCURRENCY: usize = 8;

/// Full reloads after `EVENTS_LOST` allowed in a row without a successful
/// `event.from` in between before the connection counts as lost.
const MAX_RELOADS_IN_A_ROW: u32 = 3;

/// `HOST_IS_SLAVE` redirects followed in one login.
const MAX_REDIRECTS: u32 = 3;

/// Failed heartbeats in a row that mean the connection is lost (the
/// original gives the server a second chance too).
const HEARTBEAT_FAILURES: u32 = 2;

/// Where the password comes from.
pub(crate) enum Secret {
    /// Typed by the user; kept in memory until it is saved in the keyring.
    Given(Password),
    /// Read from the keyring at each login.
    Saved,
}

/// What a connection connects to.
pub(crate) struct ConnectionSpec {
    pub id: ConnectionId,
    pub endpoint: Endpoint,
    /// The address as given (before any redirect).
    pub typed_address: String,
    pub username: String,
    pub secret: Secret,
    pub remember_password: bool,
}

impl ConnectionSpec {
    fn label(&self) -> String {
        self.endpoint.to_string()
    }
}

/// A logged-in, loaded connection.
struct Live {
    session: Session,
    model: ConnectionModel,
    token: String,
}

pub(crate) async fn run(
    shared: Arc<Shared>,
    mut spec: ConnectionSpec,
    slot: SessionSlot,
    wake: Arc<Notify>,
) {
    let opts = shared.options.clone();
    let connect_op = shared.start_operation(Some(spec.id), &spec.label(), OperationKind::Connect);
    let mut connected_once = false;
    let mut attempt = 0u32;
    let mut reconnect_op: Option<OperationId> = None;
    loop {
        let error = match establish(&shared, &mut spec, &slot).await {
            Ok(live) => {
                attempt = 0;
                let op = reconnect_op.take().unwrap_or(connect_op);
                shared.finish_operation(op, OperationStatus::Succeeded);
                connected_once = true;
                let lost = follow(&shared, &spec, live).await;
                if let Some(session) = slot_set(&slot, None) {
                    // Usually dead already; harmless when it is not.
                    logout_later(session);
                }
                tracing::warn!(server = %spec.label(), error = ?lost, "connection lost");
                lost
            }
            Err(e) => e,
        };
        let error = Arc::new(error);

        if let ConnectionError::AlreadyConnected { .. } = &*error {
            // The duplicate never appears in the list.
            let failure = OperationStatus::Failed(OperationFailure::Connection(error.clone()));
            shared.finish_operation(reconnect_op.unwrap_or(connect_op), failure);
            shared.remove_connection(spec.id);
            shared.notify(Notification::ConnectionRefused {
                server: spec.label(),
                error,
            });
            return;
        }
        if !connected_once || error.is_fatal() {
            tracing::warn!(server = %spec.label(), error = ?error, "connection failed");
            let failure = OperationStatus::Failed(OperationFailure::Connection(error.clone()));
            shared.finish_operation(reconnect_op.unwrap_or(connect_op), failure);
            shared.update_connection(spec.id, |v| {
                v.status = ConnectionStatus::Failed { error };
                v.busy_vms = Arc::default();
                v.busy_hosts = Arc::default();
                v.busy_srs = Arc::default();
            });
            return;
        }

        attempt += 1;
        if reconnect_op.is_none() {
            reconnect_op = Some(shared.start_operation(
                Some(spec.id),
                &spec.label(),
                OperationKind::Reconnect,
            ));
        }
        let delay = backoff(&opts, attempt);
        tracing::info!(server = %spec.label(), attempt, delay_ms = delay.as_millis() as u64, "reconnecting");
        let retry_at = std::time::Instant::now() + delay;
        let status_error = error.clone();
        shared.update_connection(spec.id, |v| {
            v.status = ConnectionStatus::Reconnecting {
                attempt,
                error: status_error,
                retry_at: Some(retry_at),
            };
            v.busy_vms = Arc::default();
            v.busy_hosts = Arc::default();
            v.busy_srs = Arc::default();
        });
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = wake.notified() => {}
        }
        shared.update_connection(spec.id, |v| {
            v.status = ConnectionStatus::Reconnecting {
                attempt,
                error,
                retry_at: None,
            };
        });
    }
}

/// Delay before reconnection attempt `attempt` (1-based): doubling from
/// `reconnect_delay`, capped at `max_reconnect_delay`.
pub(crate) fn backoff(opts: &BackendOptions, attempt: u32) -> Duration {
    let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(16));
    opts.reconnect_delay
        .saturating_mul(factor)
        .min(opts.max_reconnect_delay)
}

/// Logs in, loads the cache and publishes the connection as connected.
async fn establish(
    shared: &Shared,
    spec: &mut ConnectionSpec,
    slot: &SessionSlot,
) -> Result<Live, ConnectionError> {
    let read_from = spec.endpoint.clone();
    let password = match &spec.secret {
        Secret::Given(p) => p.clone(),
        Secret::Saved => {
            let (username, endpoint) = (spec.username.clone(), spec.endpoint.clone());
            match keyring(shared, move |store| store.password(&username, &endpoint)).await {
                Ok(Some(p)) => Password::new(p),
                Ok(None) => return Err(ConnectionError::PasswordRequired),
                Err(e) => return Err(ConnectionError::PasswordUnavailable(e)),
            }
        }
    };

    let session = login(shared, spec, &password).await?;
    slot_set(slot, Some(session.clone()));
    let abandon = |error: ConnectionError| {
        if let Some(session) = slot_set(slot, None) {
            logout_later(session);
        }
        error
    };

    let (model, token) = initial_load(&session)
        .await
        .map_err(|e| abandon(ConnectionError::Connect(e)))?;
    let api_version = model
        .coordinator()
        .map_or((0, 0), |h| (h.api_version_major, h.api_version_minor));
    if api_version < xapi::MIN_API_VERSION {
        let (major, minor) = api_version;
        return Err(abandon(ConnectionError::Unsupported(
            xapi::Error::UnsupportedApiVersion { major, minor },
        )));
    }
    if let Some((_, pool)) = model.pool()
        && let Err(existing) = shared.claim_pool(spec.id, &pool.uuid)
    {
        return Err(abandon(ConnectionError::AlreadyConnected { existing }));
    }

    remember(shared, spec, &read_from, &password).await;

    // What this user may do, for the checks made before each operation.
    let rights = match crate::rbac::fetch(&session).await {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            tracing::warn!(error = %e, "could not read the session's permissions");
            None
        }
    };

    let label = spec.label();
    tracing::info!(server = %label, objects = model.object_count(), "connected");
    let snapshot = Arc::new(model.clone());
    shared.update_connection(spec.id, |v| {
        v.status = ConnectionStatus::Connected { api_version };
        v.model = snapshot;
        v.rbac = rights;
    });
    Ok(Live {
        session,
        model,
        token,
    })
}

/// Runs a keyring call on the blocking pool: the OS store can block for a
/// long time (the macOS Keychain may ask the user to allow access).
async fn keyring<T: Send + 'static>(
    shared: &Shared,
    f: impl FnOnce(&Store) -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    let store = shared.options.store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .unwrap_or_else(|e| Err(SecretError(format!("keyring call failed: {e}"))))
}

/// Saves the server in the list, and its password in the keyring if asked.
/// Once saved, the password is no longer kept in memory.
async fn remember(
    shared: &Shared,
    spec: &mut ConnectionSpec,
    read_from: &Endpoint,
    password: &Password,
) {
    let address = spec.label();
    let settings =
        shared
            .store()
            .remember_server(Some(&spec.typed_address), &address, &spec.username);
    shared.publish_settings(settings);
    spec.typed_address = address;

    let given = matches!(spec.secret, Secret::Given(_));
    let moved = *read_from != spec.endpoint;
    let (username, endpoint, old) = (
        spec.username.clone(),
        spec.endpoint.clone(),
        read_from.clone(),
    );
    if spec.remember_password {
        if given || moved {
            let secret = password.clone();
            let delete_old = moved && !given;
            let saved = keyring(shared, move |store| {
                store.save_password(&username, &endpoint, secret.expose())?;
                if delete_old {
                    let _ = store.delete_password(&username, &old);
                }
                Ok(())
            })
            .await;
            match saved {
                Ok(()) => spec.secret = Secret::Saved,
                Err(e) => {
                    tracing::warn!(error = %e, "could not save the password in the keyring; keeping it in memory");
                    spec.secret = Secret::Given(password.clone());
                }
            }
        }
    } else if given {
        // The user chose not to keep it: drop any older saved copy.
        if let Err(e) = keyring(shared, move |store| {
            store.delete_password(&username, &endpoint)
        })
        .await
        {
            tracing::debug!(error = %e, "could not delete a saved password");
        }
    }
}

/// Logs in, asking the UI about untrusted certificates and following
/// `HOST_IS_SLAVE` to the coordinator (which updates `spec.endpoint`).
async fn login(
    shared: &Shared,
    spec: &mut ConnectionSpec,
    password: &Password,
) -> Result<Session, ConnectionError> {
    let store = shared.store();
    let call_options = ClientOptions {
        timeout: shared.options.call_timeout,
        ..ClientOptions::default()
    };
    let mut redirects = 0;
    loop {
        let client = Arc::new(
            JsonRpcClient::new(&spec.endpoint, store.trust(), &call_options)
                .map_err(ConnectionError::Connect)?,
        );
        match Session::login_with_password(
            client,
            &spec.username,
            password.expose(),
            &shared.options.originator,
        )
        .await
        {
            Ok(session) => return Ok(session),
            Err(xapi::Error::UntrustedCertificate { host, fingerprint }) => {
                let previous = store.trust().get(&host);
                tracing::info!(server = %spec.label(), %fingerprint, changed = previous.is_some(), "asking about the certificate");
                let (reply, answer) = oneshot::channel();
                shared.notify(Notification::CertificatePrompt(CertificatePrompt {
                    connection: spec.id,
                    server: spec.label(),
                    fingerprint,
                    previous,
                    reply,
                }));
                if answer.await == Ok(true) {
                    store.trust_certificate(&host, fingerprint);
                } else {
                    return Err(ConnectionError::CertificateRejected);
                }
            }
            Err(e) if e.is_api(codes::HOST_IS_SLAVE) => {
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return Err(ConnectionError::TooManyRedirects);
                }
                let coordinator = match &e {
                    xapi::Error::Api { params, .. } => params.first().cloned().unwrap_or_default(),
                    _ => String::new(),
                };
                let target = spec
                    .endpoint
                    .redirect(&coordinator)
                    .map_err(|_| ConnectionError::Connect(e))?;
                tracing::info!(member = %spec.label(), coordinator = %target, "pool member; redirecting to the coordinator");
                shared
                    .claim_endpoint(spec.id, &target)
                    .map_err(|existing| ConnectionError::AlreadyConnected { existing })?;
                spec.endpoint = target;
            }
            Err(e) => return Err(ConnectionError::Connect(e)),
        }
    }
}

/// Follows events and runs the heartbeat until the connection is lost.
async fn follow(shared: &Shared, spec: &ConnectionSpec, live: Live) -> ConnectionError {
    let opts = &shared.options;
    let trust = shared.store().trust();
    // event.from long-polls on its own HTTP client so it never delays regular
    // calls; its timeout exceeds the server-side one. The heartbeat has its
    // own short-timeout client too.
    let event_options = ClientOptions {
        timeout: event_client_timeout(opts.event_timeout),
        ..ClientOptions::default()
    };
    let beat_options = ClientOptions {
        timeout: opts.heartbeat_timeout,
        connect_timeout: opts.heartbeat_timeout,
        ..ClientOptions::default()
    };
    let clients = JsonRpcClient::new(&spec.endpoint, trust, &event_options).and_then(|events| {
        JsonRpcClient::new(&spec.endpoint, trust, &beat_options).map(|beat| (events, beat))
    });
    let (event_client, beat_client) = match clients {
        Ok(c) => c,
        Err(e) => return ConnectionError::Lost(e),
    };
    let events = live.session.with_client(Arc::new(event_client));
    let beat = live.session.with_client(Arc::new(beat_client));
    tokio::select! {
        lost = event_loop(shared, spec.id, &live.session, &events, live.model, live.token) => lost,
        lost = heartbeat(shared, spec.id, &beat) => lost,
    }
}

async fn event_loop(
    shared: &Shared,
    id: ConnectionId,
    session: &Session,
    events: &Session,
    mut model: ConnectionModel,
    mut token: String,
) -> ConnectionError {
    let opts = &shared.options;
    let mut reloads_in_a_row = 0u32;
    loop {
        match events
            .event_from(EVENT_CLASSES, &token, opts.event_timeout)
            .await
        {
            Ok(batch) => {
                reloads_in_a_row = 0;
                token = batch.token;
                if model.apply_events(&batch.events) {
                    let snapshot = Arc::new(model.clone());
                    shared.update_connection(id, |v| v.model = snapshot);
                    // Coalesce event storms: at most one snapshot per interval.
                    tokio::time::sleep_until(Instant::now() + opts.publish_interval).await;
                }
            }
            Err(e) if e.is_api(codes::EVENTS_LOST) => {
                reloads_in_a_row += 1;
                if reloads_in_a_row > MAX_RELOADS_IN_A_ROW {
                    return ConnectionError::Lost(e);
                }
                tracing::warn!(error = %e, "events lost; reloading the cache");
                match initial_load(session).await {
                    Ok((reloaded, new_token)) => {
                        model = reloaded;
                        token = new_token;
                        let snapshot = Arc::new(model.clone());
                        shared.update_connection(id, |v| v.model = snapshot);
                        tracing::info!(objects = model.object_count(), "cache reloaded");
                    }
                    Err(e) => return ConnectionError::Lost(e),
                }
            }
            Err(e) => return ConnectionError::Lost(e),
        }
    }
}

async fn heartbeat(shared: &Shared, id: ConnectionId, beat: &Session) -> ConnectionError {
    let opts = &shared.options;
    let mut failures = 0u32;
    loop {
        tokio::time::sleep(opts.heartbeat_interval).await;
        let coordinator = shared
            .state()
            .connection(id)
            .and_then(|c| c.model.pool().map(|(_, p)| p.master.clone()));
        let Some(coordinator) = coordinator else {
            continue;
        };
        match xapi::host::get_servertime(beat, &coordinator).await {
            Ok(_) => failures = 0,
            Err(e)
                if e.is_api(codes::SESSION_INVALID)
                    || matches!(e, xapi::Error::UntrustedCertificate { .. }) =>
            {
                // Forgotten session, or the server now presents another
                // certificate: log in again (which asks about it).
                return ConnectionError::Lost(e);
            }
            Err(e) if e.is_unreachable() => {
                failures += 1;
                tracing::debug!(error = %e, failures, "heartbeat failed");
                if failures >= HEARTBEAT_FAILURES {
                    return ConnectionError::Lost(e);
                }
            }
            Err(e) => {
                // The server answered: the link is alive.
                tracing::debug!(error = %e, "heartbeat call refused");
                failures = 0;
            }
        }
    }
}

/// Takes an event token with `event.inject` on the pool, then loads every
/// cached class with `get_all_records`.
///
/// This is the order the Xen Orchestra client uses: a token taken *before*
/// the load means no change made during the load is lost (it is replayed by
/// the first `event.from`, and applying it again is harmless).
pub async fn initial_load(session: &Session) -> xapi::Result<(ConnectionModel, String)> {
    let pool = xapi::pool::get_all(session)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| xapi::Error::Protocol("the server has no pool object".to_owned()))?;
    let token = xapi::event::inject(session, "pool", pool.as_str()).await?;
    let model = load_all(session).await?;
    Ok((model, token))
}

/// Loads every cached class concurrently. A class the server does not know
/// (older XAPI) or does not let this user list is left empty.
async fn load_all(session: &Session) -> xapi::Result<ConnectionModel> {
    let permits = Arc::new(Semaphore::new(LOAD_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for loader in class_loaders(session) {
        let permits = permits.clone();
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await;
            (loader.class, loader.load.await)
        });
    }
    let mut model = ConnectionModel::default();
    while let Some(joined) = tasks.join_next().await {
        let (class, result) =
            joined.map_err(|e| xapi::Error::Protocol(format!("loading task failed: {e}")))?;
        match result {
            Ok(apply) => apply(&mut model),
            Err(e)
                if e.is_api(codes::MESSAGE_METHOD_UNKNOWN)
                    || e.is_api(codes::RBAC_PERMISSION_DENIED) =>
            {
                tracing::debug!(class, error = %e, "class not loaded");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let opts = BackendOptions {
            reconnect_delay: Duration::from_secs(2),
            max_reconnect_delay: Duration::from_secs(60),
            ..BackendOptions::default()
        };
        let delays: Vec<u64> = (1..=7).map(|a| backoff(&opts, a).as_secs()).collect();
        assert_eq!(delays, [2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(&opts, 1000).as_secs(), 60);
    }

    #[test]
    fn event_poll_timeout_is_shorter_than_the_http_timeout() {
        let opts = BackendOptions::default();
        assert_eq!(opts.event_timeout, 60.0);
        for server in [0.5, 2.0, 30.0, 60.0, 300.0] {
            assert!(event_client_timeout(server).as_secs_f64() > server + 5.0);
        }
    }
}
