//! The HA and Active Directory operations of F10b (see [`crate::ha`] and
//! [`crate::ad`] for when each is offered), call for call as the
//! original's actions:
//!
//! * **Configure HA** (`EnableHAAction`): each VM's restart priority,
//!   start order and delay, the pool's ntol, then `pool.enable_ha` with the
//!   heartbeat SR as a task. A missing statefile disk
//!   (`VDI_NOT_AVAILABLE`) is reported with the disk's UUID.
//! * **Disable HA** (`DisableHAAction`): `pool.disable_ha` as a task.
//! * **HA settings** (`SetHaPrioritiesAction`): VMs that stop being
//!   restarted first, then the ntol, then VMs that become restarted (so the
//!   plan holds at every step), then `pool.sync_database`.
//! * **Server operations of an HA pool** (`MaybeReduceNtolBeforeOp`): the
//!   ntol is lowered first, as the user accepted; after Exit Maintenance
//!   Mode the console asks whether to raise it again
//!   ([`Notification::HaNtolRaise`], `EnableNtolDialog`).
//! * **VM start of an HA pool**: "do not restart" first for a protected VM
//!   that is not agile (`HAUnprotectVMAction`), or a lower ntol after the
//!   start broke the failover plan (`HA_LOWERING_NTOL`).
//! * **Join a domain** (`EnableAdAction`): `pool.disable_external_auth`
//!   (its failure ignored), then `pool.enable_external_auth` with the
//!   domain and the credentials (`config["domain"|"user"|"pass"]`, type
//!   `AD`).
//! * **Leave the domain** (`DisableAdAction`): `pool.disable_external_auth`
//!   with the credentials, if given.
//! * **Add users** (`AddRemoveSubjectsAction`): each name is resolved
//!   (`auth.get_subject_identifier`, then
//!   `auth.get_subject_information_from_identifier`), then each resolved
//!   one gets a subject (`subject.create`), unless it already has access.
//!   The operation's `subjects` list shows each name's progress; it fails
//!   when any name failed, as the original's "completed with errors".
//! * **Remove users**: each subject's sessions are logged out
//!   (`session.logout_subject_identifier`), then `subject.destroy`; when the
//!   logged-in user removed their own access, the connection ends.
//! * **Change Role** (`AddRemoveRolesAction` per subject), then the
//!   logged-in subjects whose roles changed are logged out.
//! * **Log Out** (`session.logout_subject_identifier` per subject; logging
//!   oneself out ends the connection).

use std::collections::HashMap;

use xapi::{Pool, Ref, Session, Subject, Vm};

use crate::actions::{ActionRequest, HostAction};
use crate::ad::{self, SubjectEntry, SubjectProgress};
use crate::backend::{Command, ConnectionId, Notification, OperationId, Shared};
use crate::ha::{self, VmStartup};
use crate::model::ConnectionModel;
use crate::operations::{self, OperationStep, Span, TaskOutcome, run_task};

fn model_of(shared: &Shared, connection: ConnectionId) -> std::sync::Arc<ConnectionModel> {
    shared
        .state()
        .connection(connection)
        .map(|v| v.model.clone())
        .unwrap_or_default()
}

fn pool_ref(model: &ConnectionModel) -> Option<Ref<Pool>> {
    model.pool().map(|(r, _)| r.clone())
}

fn no_pool() -> TaskOutcome {
    TaskOutcome::Failed(xapi::Error::Api {
        code: "HANDLE_INVALID".to_owned(),
        params: vec!["pool".to_owned()],
    })
}

fn failed<T>(r: xapi::Result<T>) -> Option<TaskOutcome> {
    r.err().map(TaskOutcome::Failed)
}

/// Runs an HA or AD request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let model = model_of(shared, connection);
    let s = session;
    match request {
        ActionRequest::EnableHa {
            heartbeat_sr,
            ntol,
            startup,
        } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            if let Some(f) = set_startup(shared, op, s, &model, startup, Span(0.0, 0.1)).await {
                return f;
            }
            if let Some(f) =
                failed(xapi::pool::set_ha_host_failures_to_tolerate(s, &pool, *ntol).await)
            {
                return f;
            }
            let no_config = HashMap::new();
            let start =
                xapi::pool::async_enable_ha(s, std::slice::from_ref(heartbeat_sr), &no_config);
            match run_task(shared, op, s, start, Span(0.15, 1.0), false).await {
                TaskOutcome::Failed(xapi::Error::Api { code, params })
                    if code == "VDI_NOT_AVAILABLE" && !params.is_empty() =>
                {
                    // `FriendlyErrorNames.VDI_NOT_AVAILABLE` names the disk
                    // by its UUID.
                    let uuid = model
                        .vdis
                        .get(&Ref::new(params[0].as_str()))
                        .map(|v| v.uuid.clone())
                        .unwrap_or_else(|| params[0].clone());
                    TaskOutcome::Failed(xapi::Error::Api {
                        code,
                        params: vec![uuid],
                    })
                }
                other => other,
            }
        }
        ActionRequest::DisableHa => {
            run_task(
                shared,
                op,
                s,
                xapi::pool::async_disable_ha(s),
                Span::FULL,
                false,
            )
            .await
        }
        ActionRequest::SetHaPriorities { startup, ntol } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            let (restart, others): (Vec<VmStartup>, Vec<VmStartup>) = startup
                .iter()
                .cloned()
                .partition(|v| v.priority.is_restart());
            if let Some(f) = set_startup(shared, op, s, &model, &others, Span(0.0, 0.3)).await {
                return f;
            }
            if let Some(f) =
                failed(xapi::pool::set_ha_host_failures_to_tolerate(s, &pool, *ntol).await)
            {
                return f;
            }
            if let Some(f) = set_startup(shared, op, s, &model, &restart, Span(0.3, 0.6)).await {
                return f;
            }
            shared.update_operation(op, |o| o.step = None);
            run_task(
                shared,
                op,
                s,
                xapi::pool::async_sync_database(s),
                Span(0.6, 1.0),
                false,
            )
            .await
        }
        ActionRequest::SetHaNtol { ntol } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            failed(xapi::pool::set_ha_host_failures_to_tolerate(s, &pool, *ntol).await)
                .unwrap_or(TaskOutcome::Succeeded)
        }
        ActionRequest::HaHost { host, action, ntol } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            shared.update_operation(op, |o| o.step = Some(OperationStep::LoweringNtol));
            if let Some(f) =
                failed(xapi::pool::set_ha_host_failures_to_tolerate(s, &pool, *ntol).await)
            {
                return f;
            }
            shared.update_operation(op, |o| o.step = None);
            operations::run_host_action(shared, connection, op, s, host, *action).await
        }
        ActionRequest::HaVm {
            vm,
            action,
            unprotect,
            ntol,
        } => {
            if *unprotect {
                let name = model
                    .vms
                    .get(vm)
                    .map(|v| v.name_label.clone())
                    .unwrap_or_default();
                shared.update_operation(op, |o| {
                    o.step = Some(OperationStep::UnprotectingVm { vm: name });
                });
                if let Some(f) = failed(
                    xapi::vm::set_ha_restart_priority(
                        s,
                        vm,
                        ha::RestartPriority::DoNotRestart.to_api(),
                    )
                    .await,
                ) {
                    return f;
                }
            }
            if let Some(ntol) = ntol {
                let Some(pool) = pool_ref(&model) else {
                    return no_pool();
                };
                shared.update_operation(op, |o| o.step = Some(OperationStep::LoweringNtol));
                if let Some(f) =
                    failed(xapi::pool::set_ha_host_failures_to_tolerate(s, &pool, *ntol).await)
                {
                    return f;
                }
            }
            shared.update_operation(op, |o| o.step = None);
            operations::run_vm_action(shared, connection, op, s, vm, action).await
        }
        ActionRequest::EnableAd {
            domain,
            user,
            password,
        } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            enable_ad(s, &pool, domain, user, password.expose()).await
        }
        ActionRequest::DisableAd { credentials } => {
            let Some(pool) = pool_ref(&model) else {
                return no_pool();
            };
            let config: HashMap<String, String> = credentials
                .iter()
                .flat_map(|(u, p)| {
                    [
                        ("user".to_owned(), u.clone()),
                        ("pass".to_owned(), p.expose().to_owned()),
                    ]
                })
                .collect();
            failed(xapi::pool::disable_external_auth(s, &pool, Some(&config)).await)
                .unwrap_or(TaskOutcome::Succeeded)
        }
        ActionRequest::AddSubjects { names } => add_subjects(shared, op, s, &model, names).await,
        ActionRequest::RemoveSubjects { subjects } => {
            remove_subjects(shared, connection, op, s, &model, subjects).await
        }
        ActionRequest::SetSubjectRoles {
            subjects,
            roles,
            log_out,
        } => {
            let count = subjects.len().max(1);
            for (i, subject) in subjects.iter().enumerate() {
                let rec = match subject_record(s, &model, subject).await {
                    Ok(r) => r,
                    Err(e) => return TaskOutcome::Failed(e),
                };
                let (add, remove) = ad::role_changes(&model, &rec, roles);
                for r in &add {
                    if let Some(f) = failed(xapi::subject::add_to_roles(s, subject, r).await) {
                        return f;
                    }
                }
                for r in &remove {
                    if let Some(f) = failed(xapi::subject::remove_from_roles(s, subject, r).await) {
                        return f;
                    }
                }
                let p = (i + 1) as f64 / count as f64;
                shared.update_operation(op, |o| o.progress = Some(p * 0.8));
            }
            logout(shared, connection, s, &model, log_out).await
        }
        ActionRequest::LogoutSubjects { subjects } => {
            logout(shared, connection, s, &model, subjects).await
        }
        _ => TaskOutcome::Failed(xapi::Error::Protocol("not an HA or AD request".into())),
    }
}

/// A subject's record: the cached one, else the server's (a subject just
/// added may not have reached the cache yet).
async fn subject_record(
    s: &Session,
    model: &ConnectionModel,
    subject: &Ref<Subject>,
) -> xapi::Result<xapi::SubjectRecord> {
    match model.subjects.get(subject) {
        Some(r) => Ok((**r).clone()),
        None => xapi::subject::get_record(s, subject).await,
    }
}

/// Sets each VM's restart priority, order and delay (`EnableHAAction`,
/// `SetHaPrioritiesAction`).
async fn set_startup(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    model: &ConnectionModel,
    startup: &[VmStartup],
    span: Span,
) -> Option<TaskOutcome> {
    let count = startup.len().max(1);
    for (i, v) in startup.iter().enumerate() {
        let name = model
            .vms
            .get(&v.vm)
            .map(|r| r.name_label.clone())
            .unwrap_or_default();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::SettingHaPriority { vm: name });
        });
        if let Some(f) =
            failed(xapi::vm::set_ha_restart_priority(s, &v.vm, v.priority.to_api()).await)
        {
            return Some(f);
        }
        if let Some(f) = failed(xapi::vm::set_order(s, &v.vm, v.order).await) {
            return Some(f);
        }
        if let Some(f) = failed(xapi::vm::set_start_delay(s, &v.vm, v.start_delay).await) {
            return Some(f);
        }
        let at = span.0 + (span.1 - span.0) * (i + 1) as f64 / count as f64;
        shared.update_operation(op, |o| o.progress = Some(at));
    }
    shared.update_operation(op, |o| o.step = None);
    None
}

/// `EnableAdAction.Run`.
async fn enable_ad(
    s: &Session,
    pool: &Ref<Pool>,
    domain: &str,
    user: &str,
    password: &str,
) -> TaskOutcome {
    if let Err(e) = xapi::pool::disable_external_auth(s, pool, Some(&HashMap::new())).await {
        tracing::debug!(error = %e, "disabling external authentication before joining failed");
    }
    let config = HashMap::from([
        ("domain".to_owned(), domain.to_owned()),
        ("user".to_owned(), user.to_owned()),
        ("pass".to_owned(), password.to_owned()),
    ]);
    failed(xapi::pool::enable_external_auth(s, pool, &config, domain, ad::AUTH_TYPE_AD).await)
        .unwrap_or(TaskOutcome::Succeeded)
}

/// Joins the server of `session` (its pool `pool`) to `domain`, for Add
/// Server and New Pool (`PoolAbstractAction.FixAd`).
pub(crate) async fn join_domain(
    session: &Session,
    pool: &Ref<Pool>,
    domain: &str,
    user: &str,
    password: &str,
) -> Result<(), xapi::Error> {
    match enable_ad(session, pool, domain, user, password).await {
        TaskOutcome::Failed(e) => Err(e),
        _ => Ok(()),
    }
}

fn error_parts(e: &xapi::Error) -> Vec<String> {
    match e {
        xapi::Error::Api { code, params } => std::iter::once(code.clone())
            .chain(params.iter().cloned())
            .collect(),
        other => vec![other.to_string()],
    }
}

/// `AddRemoveSubjectsAction` with names to add.
async fn add_subjects(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    model: &ConnectionModel,
    names: &[String],
) -> TaskOutcome {
    let mut entries: Vec<SubjectEntry> = names
        .iter()
        .map(|n| SubjectEntry {
            name: n.clone(),
            progress: SubjectProgress::Resolving,
        })
        .collect();
    let publish = |entries: &Vec<SubjectEntry>, done: usize| {
        let entries = entries.clone();
        let steps = (names.len() * 2).max(1);
        shared.update_operation(op, |o| {
            o.subjects = entries;
            o.progress = Some(done as f64 / steps as f64);
        });
    };
    publish(&entries, 0);
    let mut done = 0;
    let mut first_error: Option<xapi::Error> = None;
    // Resolve every name first.
    let mut resolved: Vec<(usize, String, HashMap<String, String>)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let r = async {
            let sid = xapi::auth::get_subject_identifier(s, name).await?;
            let info = xapi::auth::get_subject_information_from_identifier(s, &sid).await?;
            Ok::<_, xapi::Error>((sid, info))
        }
        .await;
        match r {
            Ok((sid, info)) => {
                let shown = info.get(ad::SUBJECT_NAME_KEY).cloned().unwrap_or_default();
                entries[i].progress = SubjectProgress::Adding { resolved: shown };
                resolved.push((i, sid, info));
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve an AD name");
                entries[i].progress = SubjectProgress::ResolveFailed {
                    error: error_parts(&e),
                };
                first_error.get_or_insert(e);
            }
        }
        done += 1;
        publish(&entries, done);
    }
    // Then give each resolved one access.
    for (i, sid, info) in resolved {
        let shown = info
            .get(ad::SUBJECT_DISPLAYNAME_KEY)
            .or_else(|| info.get(ad::SUBJECT_NAME_KEY))
            .cloned()
            .unwrap_or_default();
        let name = info.get(ad::SUBJECT_NAME_KEY).cloned().unwrap_or_default();
        if model.subjects.values().any(|x| x.subject_identifier == sid) {
            entries[i].progress = SubjectProgress::AlreadyHasAccess { resolved: shown };
            first_error.get_or_insert(xapi::Error::Api {
                code: "SUBJECT_ALREADY_EXISTS".to_owned(),
                params: vec![],
            });
        } else {
            let record = xapi::SubjectRecord {
                subject_identifier: sid.clone(),
                other_config: info,
                ..Default::default()
            };
            match xapi::subject::create(s, &record).await {
                Ok(_) => entries[i].progress = SubjectProgress::Added { resolved: name },
                Err(e) if e.is_api("SUBJECT_ALREADY_EXISTS") => {
                    entries[i].progress = SubjectProgress::AlreadyHasAccess { resolved: shown };
                    first_error.get_or_insert(e);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not add an AD subject");
                    entries[i].progress = SubjectProgress::AddFailed {
                        resolved: name,
                        error: error_parts(&e),
                    };
                    first_error.get_or_insert(e);
                }
            }
        }
        done += 1;
        publish(&entries, done);
    }
    shared.update_operation(op, |o| o.progress = Some(1.0));
    match first_error {
        Some(e) => TaskOutcome::Failed(e),
        None => TaskOutcome::Succeeded,
    }
}

/// The SID the session of `connection` logged in as, when it is a domain
/// user.
fn own_sid(shared: &Shared, connection: ConnectionId) -> Option<String> {
    shared
        .state()
        .connection(connection)
        .and_then(|c| c.rbac.as_ref().and_then(|r| r.user_sid.clone()))
}

/// The SID of the subject that gave this session access (the user's own
/// or one of their groups'), when it is a domain user.
fn session_subject_sid(
    shared: &Shared,
    connection: ConnectionId,
    model: &ConnectionModel,
) -> Option<String> {
    let state = shared.state();
    let subject = state
        .connection(connection)
        .and_then(|c| c.rbac.as_ref().and_then(|r| r.subject.clone()))?;
    model
        .subjects
        .get(&subject)
        .map(|s| s.subject_identifier.clone())
}

/// `AddRemoveSubjectsAction` with subjects to remove: the session's own
/// subject is not logged out by name, the connection ends instead.
async fn remove_subjects(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    model: &ConnectionModel,
    subjects: &[Ref<Subject>],
) -> TaskOutcome {
    let me = session_subject_sid(shared, connection, model);
    let mut log_self_out = false;
    let mut first_error = None;
    let count = subjects.len().max(1);
    for (i, subject) in subjects.iter().enumerate() {
        let rec = match subject_record(s, model, subject).await {
            Ok(r) => r,
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        let sid = rec.subject_identifier.clone();
        let r = async {
            if me.as_deref() == Some(sid.as_str()) {
                log_self_out = true;
            } else {
                xapi::session::logout_subject_identifier(s, &sid).await?;
            }
            xapi::subject::destroy(s, subject).await
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "could not remove an AD subject");
            first_error.get_or_insert(e);
        }
        let p = (i + 1) as f64 / count as f64;
        shared.update_operation(op, |o| o.progress = Some(p));
    }
    if log_self_out {
        shared.send(Command::Disconnect { connection });
    }
    match first_error {
        Some(e) => TaskOutcome::Failed(e),
        None => TaskOutcome::Succeeded,
    }
}

/// Logs `subjects` out; the logged-in user's own subject last, by ending
/// the connection (`AdPage.LogoutSubjects`).
async fn logout(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    model: &ConnectionModel,
    subjects: &[Ref<Subject>],
) -> TaskOutcome {
    let me = own_sid(shared, connection);
    let mut myself = false;
    for subject in subjects {
        let rec = match subject_record(s, model, subject).await {
            Ok(r) => r,
            Err(e) => return TaskOutcome::Failed(e),
        };
        if me.as_deref() == Some(rec.subject_identifier.as_str()) {
            myself = true;
            continue;
        }
        if let Some(f) =
            failed(xapi::session::logout_subject_identifier(s, &rec.subject_identifier).await)
        {
            return f;
        }
    }
    if myself {
        if let Some(sid) = &me
            && let Err(e) = xapi::session::logout_subject_identifier(s, sid).await
        {
            tracing::debug!(error = %e, "logging out the other sessions of this user failed");
        }
        shared.send(Command::Disconnect { connection });
    }
    TaskOutcome::Succeeded
}

/// After Exit Maintenance Mode on an HA pool: asks the UI whether to raise
/// the ntol to what the planner now allows (`HostAbstractAction.Enable`).
pub(crate) async fn offer_ntol_raise(
    shared: &Shared,
    connection: ConnectionId,
    session: &Session,
    host: &Ref<xapi::Host>,
) {
    let model = model_of(shared, connection);
    let Some((_, pool)) = model.pool() else {
        return;
    };
    if !pool.ha_enabled {
        return;
    }
    let config = ha::current_priorities(&model);
    let max =
        match xapi::pool::ha_compute_hypothetical_max_host_failures_to_tolerate(session, &config)
            .await
        {
            Ok(max) => ha::tolerable(Some(pool), max),
            Err(e) => {
                tracing::warn!(error = %e, "could not compute the HA plan");
                return;
            }
        };
    let current = pool.ha_host_failures_to_tolerate;
    if current < max {
        shared.notify(Notification::HaNtolRaise {
            connection,
            host: host.clone(),
            current,
            max,
        });
    }
}

/// The request a refused VM start becomes once the user accepted a lower
/// ntol, if HA allows one.
pub fn vm_start_with_lower_ntol(
    model: &ConnectionModel,
    vm: &Ref<Vm>,
    action: &crate::actions::VmAction,
) -> Option<ActionRequest> {
    let (_, pool) = model.pool()?;
    let ntol = ha::ntol_for_vm_start(pool);
    (ntol > 0).then(|| ActionRequest::HaVm {
        vm: vm.clone(),
        action: action.clone(),
        unprotect: false,
        ntol: Some(ntol),
    })
}

/// Whether `action` takes a server away (the ntol question applies).
pub fn takes_host_away(action: HostAction) -> bool {
    matches!(
        action,
        HostAction::Reboot | HostAction::Shutdown | HostAction::EnterMaintenance
    )
}
