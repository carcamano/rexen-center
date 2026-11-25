//! The storage repository operations (see [`crate::storage`] for the
//! requests and when each is offered) and the wizard's probes.
//!
//! Each follows the original console's action, call for call:
//!
//! * **New SR** (`SrCreateAction`): passwords in `device_config` become a
//!   `secret` first (`<key>_secret`); `SR.create` on the coordinator; a PBD
//!   the create could not plug is plugged by hand, and if that fails too
//!   (other than an offline host) the SR is forgotten again; then
//!   `other_config["auto-scan"]` (true for ISO libraries) and, for the
//!   pool's first shared SR for disks, the pool's default SR. The secret is
//!   destroyed once the PBDs copied it (CA-113396).
//!
//!   The original forgets the SR only when the manual plug fails with an
//!   error that is not an API failure, so a failed plug left a broken SR
//!   behind; here any API failure but `HOST_OFFLINE` / `HOST_STILL_BOOTING`
//!   rolls back, as its comment intends.
//! * **Introduce** an SR the probe found (`SrIntroduceAction`): an earlier
//!   record with that UUID is forgotten first, then `SR.introduce` and one
//!   PBD per host, created and plugged.
//! * **Reattach** a detached SR (`SrReattachAction`): one PBD per host,
//!   created and plugged, then its name and description. On failure the
//!   PBDs made are unplugged and destroyed (the wizard's
//!   `DetachSrAction(sr, destroyPbds: true)`).
//! * **Repair** (`SrRepairAction`): on each host that should see the SR
//!   (the coordinator first), a missing PBD is created from the first PBD's
//!   `device_config` and an unplugged one plugged; a failure is reported
//!   once every host was tried.
//! * **Detach** (`DetachSrAction`): every PBD unplugged, the coordinator's
//!   last (CA-176935). **Forget** / **Destroy**: `SR.forget` /
//!   `SR.destroy`. **Set as Default** (`SetSrAsDefaultAction`): the pool's
//!   default, suspend image and crash dump SR. **Rescan**
//!   (`SrRefreshAction`): `SR.scan`. **Reclaim freed space**
//!   (`SrTrimAction`): the `trim` plugin's `do_trim` on the first host the
//!   SR is plugged on.
//! * **Properties**: name, description, tags and read caching
//!   (`other_config["o_direct"]`).
//!
//! The probes run as prechecks (no history entry, like the original's
//! hidden probe actions): `SR.probe` (`SrProbeAction`), and the iSCSI IQN
//! and LUN scans, which call `SR.create` with too few parameters and read
//! the list from the error it fails with (`SR_BACKEND_FAILURE_96`, `_107` /
//! `_87`).

use std::collections::HashMap;

use xapi::error::codes;
use xapi::{Host, Pbd, PbdRecord, Ref, Session, Sr, TaskStatusType};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::model::ConnectionModel;
use crate::objects;
use crate::operations::{OperationStep, Span, TaskOutcome, run_task, run_task_result, task_error};
use crate::storage::{
    self, DeviceConfig, GUI_TEMP_PREFIX, IqnInfo, LunInfo, NewSrMode, NewSrSpec, ProbeResult,
    Secret, SrAction, SrChanges, errors,
};

/// Runs an F8 request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::NewSr(spec) => match spec.mode() {
            NewSrMode::Create => outcome(create_sr(shared, connection, op, s, spec).await),
            NewSrMode::Introduce => outcome(introduce_sr(shared, connection, op, s, spec).await),
            NewSrMode::Reattach => reattach_sr(shared, connection, op, s, spec).await,
        },
        ActionRequest::Sr { sr, action } => match action {
            SrAction::Repair => repair(shared, connection, op, s, sr).await,
            SrAction::Detach => outcome(detach(shared, connection, op, s, sr, false).await),
            SrAction::Forget => forget(shared, connection, op, s, sr).await,
            SrAction::Destroy => {
                run_task(
                    shared,
                    op,
                    s,
                    xapi::sr::async_destroy(s, sr),
                    Span(0.5, 1.0),
                    false,
                )
                .await
            }
            SrAction::SetDefault => {
                outcome(set_default(s, &pool_ref(shared, connection), sr).await)
            }
            SrAction::Rescan => {
                run_task(
                    shared,
                    op,
                    s,
                    xapi::sr::async_scan(s, sr),
                    Span::FULL,
                    false,
                )
                .await
            }
            SrAction::Trim => trim(shared, connection, op, s, sr).await,
        },
        ActionRequest::SetSrProperties { sr, changes } => {
            outcome(set_properties(s, sr, changes).await)
        }
        _ => unreachable!("not a storage request"),
    }
}

/// The model of `connection` now.
fn model(shared: &Shared, connection: ConnectionId) -> std::sync::Arc<ConnectionModel> {
    shared
        .state()
        .connection(connection)
        .map(|v| v.model.clone())
        .unwrap_or_default()
}

fn step(shared: &Shared, op: OperationId, step: OperationStep) {
    shared.update_operation(op, |o| o.step = Some(step));
}

fn outcome(result: Result<(), TaskOutcome>) -> TaskOutcome {
    match result {
        Ok(()) => TaskOutcome::Succeeded,
        Err(o) => o,
    }
}

fn done(outcome: TaskOutcome) -> Result<(), TaskOutcome> {
    match outcome {
        TaskOutcome::Succeeded => Ok(()),
        other => Err(other),
    }
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

/// Runs a task and returns the object it made (its result).
async fn made<T>(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl std::future::Future<Output = xapi::Result<Ref<xapi::Task>>>,
    span: Span,
) -> Result<Ref<T>, TaskOutcome> {
    match run_task_result(shared, op, session, start, span, false).await {
        (TaskOutcome::Succeeded, r) if r.starts_with("OpaqueRef:") => Ok(Ref::new(&r)),
        (TaskOutcome::Succeeded, r) => Err(TaskOutcome::Failed(xapi::Error::Protocol(format!(
            "the task returned no object: {r:?}"
        )))),
        (other, _) => Err(other),
    }
}

/// The pool (`Helpers.GetPoolOfOne`).
fn pool_ref(shared: &Shared, connection: ConnectionId) -> Option<Ref<xapi::Pool>> {
    model(shared, connection).pool().map(|(r, _)| r.clone())
}

/// The coordinator, or the "connection lost" error of the wizard.
fn coordinator(model: &ConnectionModel) -> Result<Ref<Host>, TaskOutcome> {
    model
        .pool()
        .map(|(_, p)| p.master.clone())
        .filter(|h| model.hosts.contains_key(h))
        .ok_or_else(|| TaskOutcome::Failed(storage::client_error(errors::NO_COORDINATOR, &[])))
}

fn host_name(model: &ConnectionModel, host: &Ref<Host>) -> String {
    model
        .hosts
        .get(host)
        .map(|h| h.name_label.clone())
        .unwrap_or_else(|| host.to_string())
}

// ---- New SR ----------------------------------------------------------------

/// `SrCreateAction.CreateSecret`: the first password key becomes a secret;
/// returns the secret's uuid.
async fn make_secret(
    s: &Session,
    dconf: &mut HashMap<String, String>,
) -> Result<Option<String>, TaskOutcome> {
    let Some(key) = ["cifspassword", "password", "chappassword"]
        .into_iter()
        .find(|k| dconf.contains_key(*k))
    else {
        return Ok(None);
    };
    let value = dconf.remove(key).unwrap_or_default();
    let record = xapi::SecretRecord {
        value,
        ..Default::default()
    };
    let secret = tri!(xapi::secret::create(s, &record).await);
    let uuid = tri!(xapi::secret::get_uuid(s, &secret).await);
    dconf.insert(format!("{key}_secret"), uuid.clone());
    Ok(Some(uuid))
}

/// Destroys the secret `uuid`, best effort (CA-145254).
async fn drop_secret(s: &Session, uuid: Option<&str>) {
    let Some(uuid) = uuid else { return };
    let gone = async {
        let r = xapi::secret::get_by_uuid(s, uuid).await?;
        xapi::secret::destroy(s, &r).await
    }
    .await;
    if let Err(e) = gone {
        tracing::debug!(error = %e, "the SR's secret could not be destroyed");
    }
}

async fn create_sr(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewSrSpec,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let host = coordinator(&m)?;
    let first_shared = storage::first_shared_disk_sr(&m, spec.kind, None);
    let mut dconf = spec.device_config_map();
    let secret = make_secret(s, &mut dconf).await?;
    step(shared, op, OperationStep::CreatingSr);
    let smconf = spec.sm_config_map();
    let start = xapi::sr::async_create(
        s,
        &host,
        &dconf,
        0,
        &spec.name,
        &spec.description,
        spec.kind.sr_type(),
        spec.kind.content_type(),
        spec.kind.shared(),
        Some(&smconf),
    );
    let created = made::<Sr>(shared, op, s, start, Span(0.0, 0.8)).await;
    // The PBDs copied the secret; it is not needed any more either way.
    drop_secret(s, secret.as_deref()).await;
    let sr = created?;

    // "Checking that SR.create() actually succeeded".
    for pbd in tri!(xapi::sr::get_pbds(s, &sr).await) {
        if tri!(xapi::pbd::get_currently_attached(s, &pbd).await) {
            continue;
        }
        if let Err(e) = xapi::pbd::plug(s, &pbd).await {
            if e.is_api("HOST_OFFLINE") || e.is_api(codes::HOST_STILL_BOOTING) {
                tracing::warn!(error = %e, "could not check the new SR on an offline host");
                continue;
            }
            step(shared, op, OperationStep::UndoingSr);
            forget_failed(s, &sr).await;
            return Err(TaskOutcome::Failed(e));
        }
    }
    shared.update_operation(op, |o| o.progress = Some(0.9));
    let auto_scan = if spec.kind.is_iso() { "true" } else { "false" };
    let other_config = HashMap::from([("auto-scan".to_owned(), auto_scan.to_owned())]);
    tri!(xapi::sr::set_other_config(s, &sr, &other_config).await);
    if first_shared {
        set_default(s, &m.pool().map(|(r, _)| r.clone()), &sr).await?;
    }
    Ok(())
}

/// `ForgetFailedSR`: unplugs what is plugged and forgets the SR; never
/// fails.
async fn forget_failed(s: &Session, sr: &Ref<Sr>) {
    let forgotten = async {
        for pbd in xapi::sr::get_pbds(s, sr).await? {
            if xapi::pbd::get_currently_attached(s, &pbd).await? {
                xapi::pbd::unplug(s, &pbd).await?;
            }
        }
        xapi::sr::forget(s, sr).await
    }
    .await;
    if let Err(e) = forgotten {
        tracing::debug!(error = %e, "SR.forget() failed; continuing anyway");
    }
}

/// `SetSrAsDefaultAction`.
async fn set_default(
    s: &Session,
    pool: &Option<Ref<xapi::Pool>>,
    sr: &Ref<Sr>,
) -> Result<(), TaskOutcome> {
    let Some(pool) = pool else {
        return Ok(());
    };
    tri!(xapi::pool::set_default_sr(s, pool, sr).await);
    tri!(xapi::pool::set_suspend_image_sr(s, pool, sr).await);
    tri!(xapi::pool::set_crash_dump_sr(s, pool, sr).await);
    Ok(())
}

/// Creates and plugs a PBD for each host (`SrIntroduceAction`,
/// `SrReattachAction`).
async fn attach_pbds(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    m: &ConnectionModel,
    sr: &Ref<Sr>,
    dconf: &HashMap<String, String>,
    span: Span,
) -> Result<(), TaskOutcome> {
    let hosts: Vec<Ref<Host>> = objects::sorted_hosts(m)
        .into_iter()
        .map(|(r, _)| r.clone())
        .collect();
    let share = (span.1 - span.0) / (hosts.len().max(1) * 2) as f64;
    for (i, host) in hosts.iter().enumerate() {
        let from = span.0 + share * (2 * i) as f64;
        let name = host_name(m, host);
        step(
            shared,
            op,
            OperationStep::CreatingPbd { host: name.clone() },
        );
        let record = PbdRecord {
            host: host.clone(),
            sr: sr.clone(),
            device_config: dconf.clone(),
            currently_attached: false,
            ..Default::default()
        };
        let pbd = made::<Pbd>(
            shared,
            op,
            s,
            xapi::pbd::async_create(s, &record),
            Span(from, from + share),
        )
        .await?;
        step(shared, op, OperationStep::PluggingPbd { host: name });
        done(
            run_task(
                shared,
                op,
                s,
                xapi::pbd::async_plug(s, &pbd),
                Span(from + share, from + 2.0 * share),
                false,
            )
            .await,
        )?;
    }
    Ok(())
}

async fn introduce_sr(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewSrSpec,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let uuid = spec.uuid.clone().unwrap_or_default();
    step(shared, op, OperationStep::AttachingSr);
    // "If SR is already attached, forget it (it may be in a broken
    // invisible state with no PBDs)".
    if let Ok(old) = xapi::sr::get_by_uuid(s, &uuid).await {
        let forgotten = run_task(
            shared,
            op,
            s,
            xapi::sr::async_forget(s, &old),
            Span(0.0, 0.05),
            false,
        )
        .await;
        if let TaskOutcome::Failed(e) = forgotten {
            tracing::debug!(error = %e, "the pre-emptive SR.forget failed (allowed)");
        }
    }
    let first_shared = storage::first_shared_disk_sr(&m, spec.kind, None);
    let sr = made::<Sr>(
        shared,
        op,
        s,
        xapi::sr::async_introduce(
            s,
            &uuid,
            &spec.name,
            &spec.description,
            spec.kind.sr_type(),
            spec.kind.content_type(),
            spec.kind.shared(),
            Some(&HashMap::new()),
        ),
        Span(0.05, 0.1),
    )
    .await?;
    attach_pbds(
        shared,
        op,
        s,
        &m,
        &sr,
        &spec.device_config_map(),
        Span(0.1, 1.0),
    )
    .await?;
    if first_shared {
        set_default(s, &m.pool().map(|(r, _)| r.clone()), &sr).await?;
    }
    Ok(())
}

async fn reattach_sr(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewSrSpec,
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(sr) = spec.reattach.clone() else {
        return TaskOutcome::Succeeded;
    };
    if m.srs.get(&sr).is_some_and(|r| storage::has_pbds(r)) {
        let name = m
            .srs
            .get(&sr)
            .map(|r| r.name_label.clone())
            .unwrap_or_default();
        let pool = m
            .pool()
            .map(|(_, p)| objects::pool_name(&m, p))
            .unwrap_or_default();
        return TaskOutcome::Failed(storage::client_error(errors::SR_IN_USE, &[&name, &pool]));
    }
    step(shared, op, OperationStep::AttachingSr);
    let attached = async {
        attach_pbds(
            shared,
            op,
            s,
            &m,
            &sr,
            &spec.device_config_map(),
            Span::FULL,
        )
        .await?;
        tri!(xapi::sr::set_name_label(s, &sr, &spec.name).await);
        tri!(xapi::sr::set_name_description(s, &sr, &spec.description).await);
        Ok(())
    }
    .await;
    match attached {
        Ok(()) => TaskOutcome::Succeeded,
        Err(failure) => {
            // The wizard's `DetachSrAction(sr, destroyPbds: true)`.
            step(shared, op, OperationStep::UndoingSr);
            let pbds = xapi::sr::get_pbds(s, &sr).await.unwrap_or_default();
            for pbd in &pbds {
                if xapi::pbd::get_currently_attached(s, pbd)
                    .await
                    .unwrap_or(false)
                    && let Err(e) = xapi::pbd::unplug(s, pbd).await
                {
                    tracing::warn!(error = %e, "could not unplug a PBD after a failed reattach");
                }
            }
            for pbd in &pbds {
                if let Err(e) = xapi::pbd::destroy(s, pbd).await {
                    tracing::warn!(error = %e, "could not destroy a PBD after a failed reattach");
                }
            }
            failure
        }
    }
}

// ---- SR operations ---------------------------------------------------------

async fn repair(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    sr_ref: &Ref<Sr>,
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(sr) = m.srs.get(sr_ref).cloned() else {
        return TaskOutcome::Failed(xapi::Error::Api {
            code: codes::HANDLE_INVALID.to_owned(),
            params: vec!["SR".to_owned(), sr_ref.to_string()],
        });
    };
    let hosts: Vec<Ref<Host>> = storage::repair_hosts(&m, &sr)
        .into_iter()
        .cloned()
        .collect();
    if hosts.is_empty() {
        return TaskOutcome::Succeeded;
    }
    let template = sr
        .pbds
        .first()
        .and_then(|p| m.get(p))
        .map(|p| p.device_config.clone());
    let share = 1.0 / (hosts.len() * 2) as f64;
    let mut failure = None;
    for (i, host) in hosts.iter().enumerate() {
        let from = share * (2 * i) as f64;
        let name = host_name(&m, host);
        let mut pbd =
            storage::pbd_of(&m, &sr, host).map(|(r, p)| (r.clone(), p.currently_attached));
        if pbd.is_none()
            && sr.shared
            && let Some(device_config) = &template
        {
            step(
                shared,
                op,
                OperationStep::CreatingPbd { host: name.clone() },
            );
            let record = PbdRecord {
                host: host.clone(),
                sr: sr_ref.clone(),
                device_config: device_config.clone(),
                currently_attached: false,
                ..Default::default()
            };
            match made::<Pbd>(
                shared,
                op,
                s,
                xapi::pbd::async_create(s, &record),
                Span(from, from + share),
            )
            .await
            {
                Ok(r) => pbd = Some((r, false)),
                Err(TaskOutcome::Cancelled) => return TaskOutcome::Cancelled,
                Err(o) => failure = Some(o),
            }
        }
        if let Some((pbd, false)) = pbd {
            step(shared, op, OperationStep::PluggingPbd { host: name });
            match run_task(
                shared,
                op,
                s,
                xapi::pbd::async_plug(s, &pbd),
                Span(from + share, from + 2.0 * share),
                false,
            )
            .await
            {
                TaskOutcome::Succeeded => {}
                TaskOutcome::Cancelled => return TaskOutcome::Cancelled,
                o => failure = Some(o),
            }
        }
    }
    failure.unwrap_or(TaskOutcome::Succeeded)
}

/// `DetachSrAction`: unplugs every PBD, the coordinator's last; with
/// `destroy`, destroys them too.
async fn detach(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    sr_ref: &Ref<Sr>,
    destroy: bool,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some(sr) = m.srs.get(sr_ref) else {
        return Ok(());
    };
    let (mut others, mut coordinator): (Vec<Ref<Pbd>>, Vec<Ref<Pbd>>) = (Vec::new(), Vec::new());
    for p in &sr.pbds {
        match m.get(p) {
            Some(pbd) if objects::is_coordinator(&m, &pbd.host) => coordinator.push(p.clone()),
            Some(_) => others.push(p.clone()),
            None => {}
        }
    }
    others.append(&mut coordinator);
    let count = (others.len() * if destroy { 2 } else { 1 }).max(1) as f64;
    for (i, pbd) in others.iter().enumerate() {
        let span = Span(i as f64 / count, (i + 1) as f64 / count);
        done(run_task(shared, op, s, xapi::pbd::async_unplug(s, pbd), span, false).await)?;
    }
    if destroy {
        let n = others.len();
        for (i, pbd) in others.iter().enumerate() {
            let span = Span((n + i) as f64 / count, (n + i + 1) as f64 / count);
            done(run_task(shared, op, s, xapi::pbd::async_destroy(s, pbd), span, false).await)?;
        }
    }
    Ok(())
}

/// `ForgetSrAction`: only when the server allows it (the original then
/// does nothing and reports no error).
async fn forget(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    sr: &Ref<Sr>,
) -> TaskOutcome {
    let allowed = model(shared, connection).srs.get(sr).is_none_or(|r| {
        r.allowed_operations
            .contains(&xapi::StorageOperations::Forget)
    });
    if !allowed {
        tracing::info!("the server does not allow forgetting this SR now; nothing done");
        return TaskOutcome::Succeeded;
    }
    run_task(
        shared,
        op,
        s,
        xapi::sr::async_forget(s, sr),
        Span::FULL,
        false,
    )
    .await
}

/// `SrTrimAction`.
async fn trim(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    sr_ref: &Ref<Sr>,
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(sr) = m.srs.get(sr_ref) else {
        return TaskOutcome::Succeeded;
    };
    step(shared, op, OperationStep::ReclaimingSpace);
    let Some(host) = storage::first_attached_host(&m, sr) else {
        return TaskOutcome::Failed(storage::client_error(errors::TRIM_DETACHED, &[]));
    };
    let args = HashMap::from([("sr_uuid".to_owned(), sr.uuid.clone())]);
    match xapi::host::call_plugin(s, host, "trim", "do_trim", &args).await {
        Ok(result) if result.trim().eq_ignore_ascii_case("true") => TaskOutcome::Succeeded,
        Ok(result) => {
            tracing::warn!(result = %result, "trim.do_trim failed");
            let (code, message) = storage::parse_trim_error(&result).unwrap_or_default();
            TaskOutcome::Failed(storage::client_error(
                errors::TRIM_FAILED,
                &[&code, &message],
            ))
        }
        Err(e) => TaskOutcome::Failed(e),
    }
}

/// The SR Properties dialog: the changed fields only.
async fn set_properties(s: &Session, sr: &Ref<Sr>, changes: &SrChanges) -> Result<(), TaskOutcome> {
    if let Some(name) = &changes.name {
        tri!(xapi::sr::set_name_label(s, sr, name).await);
    }
    if let Some(description) = &changes.description {
        tri!(xapi::sr::set_name_description(s, sr, description).await);
    }
    if let Some(tags) = &changes.tags {
        tri!(xapi::sr::set_tags(s, sr, tags).await);
    }
    if let Some(on) = changes.read_caching {
        // `SR.SetReadCachingEnabled`: on removes `o_direct`, off sets it.
        tri!(xapi::sr::remove_from_other_config(s, sr, "o_direct").await);
        if !on {
            tri!(xapi::sr::add_to_other_config(s, sr, "o_direct", "true").await);
        }
    }
    Ok(())
}

// ---- probes ----------------------------------------------------------------

/// Starts a task and waits for it, outside the history; returns its
/// `result`.
async fn await_task(
    shared: &Shared,
    s: &Session,
    start: impl std::future::Future<Output = xapi::Result<Ref<xapi::Task>>>,
) -> xapi::Result<String> {
    let task = start.await?;
    let result = loop {
        tokio::time::sleep(shared.options.task_poll_interval).await;
        let record = match xapi::task::get_record(s, &task).await {
            Ok(r) => r,
            Err(e) => break Err(e),
        };
        match record.status {
            TaskStatusType::Pending | TaskStatusType::Cancelling => {}
            TaskStatusType::Success => break Ok(record.result),
            TaskStatusType::Failure => break Err(task_error(record.error_info)),
            TaskStatusType::Cancelled => {
                break Err(xapi::Error::Api {
                    code: codes::TASK_CANCELLED.to_owned(),
                    params: vec![task.to_string()],
                });
            }
            TaskStatusType::Unknown(status) => {
                break Err(xapi::Error::Protocol(format!(
                    "unknown task status {status}"
                )));
            }
        }
    };
    if let Err(e) = xapi::task::destroy(s, &task).await {
        tracing::debug!(error = %e, "task.destroy failed");
    }
    result
}

/// `SrProbeAction`: `SR.probe` on the coordinator.
pub(crate) async fn probe(
    shared: &Shared,
    s: &Session,
    connection: ConnectionId,
    sr_type: &str,
    device_config: &DeviceConfig,
) -> xapi::Result<ProbeResult> {
    let host = coordinator(&model(shared, connection)).map_err(no_coordinator)?;
    let dconf = device_config.map();
    let none = HashMap::new();
    let result = await_task(
        shared,
        s,
        xapi::sr::async_probe(s, &host, &dconf, sr_type, Some(&none)),
    )
    .await?;
    storage::parse_probe(&result)
        .ok_or_else(|| xapi::Error::Protocol(format!("unreadable SR.probe answer: {result:.200}")))
}

fn no_coordinator(_: TaskOutcome) -> xapi::Error {
    storage::client_error(errors::NO_COORDINATOR, &[])
}

/// The iSCSI scans: `SR.create` of an `lvmoiscsi` SR with too few
/// parameters, which fails with the list in its fourth error field.
async fn iscsi_scan(
    shared: &Shared,
    s: &Session,
    connection: ConnectionId,
    dconf: &HashMap<String, String>,
    expected: &[&str],
) -> xapi::Result<Option<String>> {
    let host = coordinator(&model(shared, connection)).map_err(no_coordinator)?;
    let none = HashMap::new();
    let created = await_task(
        shared,
        s,
        xapi::sr::async_create(
            s,
            &host,
            dconf,
            0,
            GUI_TEMP_PREFIX,
            "SHOULD NEVER BE CREATED",
            "lvmoiscsi",
            "user",
            true,
            Some(&none),
        ),
    )
    .await;
    match created {
        Ok(result) => {
            // "Create should always fail and never get here": do not leave
            // the SR behind.
            let sr = crate::operations::task_result_value(&result);
            if sr.starts_with("OpaqueRef:") {
                forget_failed(s, &Ref::new(&sr)).await;
            }
            Err(storage::client_error(errors::SCAN_CREATED, &[]))
        }
        Err(xapi::Error::Api { code, params }) if expected.contains(&code.as_str()) => {
            match params.get(2) {
                Some(xml) if !xml.is_empty() => Ok(Some(xml.clone())),
                _ => Ok(None),
            }
        }
        Err(e) => Err(e),
    }
}

fn chap_ref(chap: &Option<(String, Secret)>) -> Option<(&str, &str)> {
    chap.as_ref().map(|(u, p)| (u.as_str(), p.0.as_str()))
}

/// `ISCSIPopulateIQNsAction`.
pub(crate) async fn scan_iqns(
    shared: &Shared,
    s: &Session,
    connection: ConnectionId,
    target: &str,
    port: u16,
    chap: Option<(String, Secret)>,
) -> xapi::Result<Vec<IqnInfo>> {
    let dconf = storage::iscsi_scan_config(target, port, None, chap_ref(&chap));
    let xml = iscsi_scan(shared, s, connection, &dconf, &["SR_BACKEND_FAILURE_96"]).await?;
    let Some(xml) = xml else {
        return Err(storage::client_error(errors::NO_IQNS, &[target]));
    };
    let iqns = storage::parse_iqns(&xml)
        .ok_or_else(|| storage::client_error(errors::BAD_RESPONSE, &[target]))?;
    if iqns.is_empty() {
        return Err(storage::client_error(errors::NO_IQNS, &[target]));
    }
    Ok(iqns)
}

/// `ISCSIPopulateLunsAction`.
pub(crate) async fn scan_luns(
    shared: &Shared,
    s: &Session,
    connection: ConnectionId,
    target: &str,
    port: u16,
    iqn: &str,
    chap: Option<(String, Secret)>,
) -> xapi::Result<Vec<LunInfo>> {
    let dconf = storage::iscsi_scan_config(target, port, Some(iqn), chap_ref(&chap));
    let xml = iscsi_scan(
        shared,
        s,
        connection,
        &dconf,
        &["SR_BACKEND_FAILURE_107", "SR_BACKEND_FAILURE_87"],
    )
    .await?;
    let Some(xml) = xml else {
        return Err(storage::client_error(errors::NO_LUNS, &[target]));
    };
    let luns = storage::parse_luns(&xml)
        .ok_or_else(|| storage::client_error(errors::BAD_RESPONSE, &[target]))?;
    if luns.is_empty() {
        return Err(storage::client_error(errors::NO_LUNS, &[target]));
    }
    Ok(luns)
}
