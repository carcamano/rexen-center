//! The operation history behind the Events panel, and the operations that
//! run as XAPI tasks.
//!
//! Every operation this client starts gets an [`Operation`] entry: VM and
//! host operations (run with their `Async.*` form so they have a server task
//! with progress that can be cancelled), task cancellations, and connection
//! events (connecting, reconnecting after a loss). Tasks started by other
//! clients are not copied here: the UI lists them straight from each
//! connection's cache (`ConnectionModel::tasks`, kept live by `event.from`).
//!
//! A running task is followed by polling `task.get_record` every
//! [`crate::BackendOptions::task_poll_interval`] (the original polls every
//! 900 ms); once it ends, the task is destroyed, as the original does.
//!
//! The host operations follow the original's actions:
//!
//! * **Reboot / Shut Down** (`RebootHostAction`, `ShutdownHostAction`):
//!   `host.disable`, then each running guest on the host is shut down
//!   (cleanly when allowed, forcibly otherwise), then `host.reboot` or
//!   `host.shutdown`. On failure the host is enabled again if it was.
//! * **Enter Maintenance Mode** (`EvacuateHostAction`): `host.disable`, the
//!   `MAINTENANCE_MODE` flag in `other_config`, then `host.evacuate`
//!   (migrates the VMs away). On failure the host is enabled again.
//! * **Exit Maintenance Mode** (`EnableHostAction`): the flag is removed and
//!   the host enabled.
//! * **Power On** (`host.power_on`) and **Restart Toolstack**
//!   (`host.restart_agent`).
//!
//! Rebooting, shutting down or restarting the toolstack of the coordinator
//! drops the connection; its reconnection logic takes over, and a task that
//! can no longer be followed then counts as done.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use xapi::error::codes;
use xapi::{Host, Ref, Session, Task, TaskStatusType, Vm, VmOperations};

use crate::actions::{self, ActionRequest, HostAction, PerfmonObjectRef, VmAction};
use crate::backend::{ConnectionError, ConnectionId, Notification, OperationId, Shared};
use crate::model::ConnectionModel;
use crate::objects;

/// `other_config` key marking a host put in maintenance mode by a console.
pub const MAINTENANCE_MODE: &str = "MAINTENANCE_MODE";

/// What an operation does (the UI words it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationKind {
    /// Connecting to a server.
    Connect,
    /// The connection was lost; reconnecting.
    Reconnect,
    /// A VM operation. `location` is the VM's home server (or the pool)
    /// and `from` the server it runs on; `target` names the server of
    /// Start on / Resume on / Migrate to.
    VmAction {
        vm_name: String,
        action: VmAction,
        location: String,
        from: Option<String>,
        target: Option<String>,
    },
    /// A host operation.
    HostAction {
        host_name: String,
        action: HostAction,
    },
    /// Cancelling a task (`task_name` is its `name_label`).
    CancelTask { task_name: String },
    /// Dismissing `count` alerts (`title`: the alert's, when only one).
    DismissAlerts { count: usize, title: Option<String> },
    /// Saving the performance alerts of `object_name`.
    SavePerfmon { object_name: String },
}

/// What a running operation is doing now (the original's action
/// `Description`), when there is more to say than its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStep {
    /// Reboot / Shut Down of a host: shutting down its guest `index` of
    /// `count` first.
    ShuttingDownVm {
        vm_name: String,
        index: usize,
        count: usize,
        for_reboot: bool,
    },
    /// Maintenance mode: disabling the host.
    Disabling,
    /// Maintenance mode: migrating the VMs away.
    Evacuating,
    /// Re-enabling a host after a failure.
    Reenabling,
}

/// How an operation ended (or that it has not).
#[derive(Debug, Clone)]
pub enum OperationStatus {
    Running,
    Succeeded,
    Cancelled,
    Failed(OperationFailure),
}

/// Why an operation failed.
#[derive(Debug, Clone)]
pub enum OperationFailure {
    Xapi(Arc<xapi::Error>),
    Connection(Arc<ConnectionError>),
    /// The server was not connected.
    NotConnected,
    /// The user closed the connection while it ran.
    Disconnected,
}

/// The object an operation works on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OperationObject {
    Vm(Ref<Vm>),
    Host(Ref<Host>),
}

/// One entry of the history.
#[derive(Debug, Clone)]
pub struct Operation {
    pub id: OperationId,
    pub connection: Option<ConnectionId>,
    /// Server it ran against.
    pub server: String,
    pub kind: OperationKind,
    pub status: OperationStatus,
    /// 0.0–1.0 while a task reports progress.
    pub progress: Option<f64>,
    /// The XAPI task behind it, once started (kept after it ends so the UI
    /// can tell it apart from other clients' tasks).
    pub task: Option<Ref<Task>>,
    /// The object it works on.
    pub object: Option<OperationObject>,
    /// What it is doing now.
    pub step: Option<OperationStep>,
    /// Run with the session of another user (role elevation).
    pub elevated_as: Option<String>,
    pub started: SystemTime,
    pub finished: Option<SystemTime>,
}

impl Operation {
    pub fn is_running(&self) -> bool {
        matches!(self.status, OperationStatus::Running)
    }

    /// Whether it can be cancelled now (a running server task).
    pub fn can_cancel(&self) -> bool {
        self.is_running() && self.task.is_some()
    }
}

/// The history entry kind for `request` (names resolved from `model`).
pub fn operation_kind(model: &ConnectionModel, request: &ActionRequest) -> OperationKind {
    let host_name = |h: &Ref<Host>| {
        model
            .hosts
            .get(h)
            .map(|r| r.name_label.clone())
            .unwrap_or_else(|| h.to_string())
    };
    match request {
        ActionRequest::Vm { vm, action } => {
            let record = model.vms.get(vm);
            let vm_name = record
                .map(|r| r.name_label.clone())
                .unwrap_or_else(|| vm.to_string());
            let pool_name = model
                .pool()
                .map(|(_, p)| objects::pool_name(model, p))
                .unwrap_or_default();
            let location = record
                .and_then(|r| objects::vm_home(model, r))
                .map(host_name)
                .unwrap_or(pool_name);
            let from = record
                .and_then(|r| r.resident_on.non_null())
                .filter(|h| model.hosts.contains_key(*h))
                .map(host_name);
            OperationKind::VmAction {
                vm_name,
                action: action.clone(),
                location,
                from,
                target: action.target().map(host_name),
            }
        }
        ActionRequest::Host { host, action } => OperationKind::HostAction {
            host_name: host_name(host),
            action: *action,
        },
        ActionRequest::DismissAlerts { messages } => OperationKind::DismissAlerts {
            count: messages.len(),
            title: match messages.as_slice() {
                [one] => model.messages.get(one).map(|m| m.name.clone()),
                _ => None,
            },
        },
        ActionRequest::SetPerfmon { object, .. } => OperationKind::SavePerfmon {
            object_name: match object {
                PerfmonObjectRef::Vm(r) => model.vms.get(r).map(|v| v.name_label.clone()),
                PerfmonObjectRef::Host(r) => model.hosts.get(r).map(|h| h.name_label.clone()),
                PerfmonObjectRef::Sr(r) => model.srs.get(r).map(|x| x.name_label.clone()),
            }
            .unwrap_or_default(),
        },
    }
}

fn kind_of(shared: &Shared, connection: ConnectionId, request: &ActionRequest) -> OperationKind {
    let state = shared.state();
    let model = state
        .connection(connection)
        .map(|v| v.model.clone())
        .unwrap_or_default();
    operation_kind(&model, request)
}

/// Starts `request` as XAPI task(s), recorded in the history. With
/// `elevated_as`, `session` belongs to that other user and is logged out
/// when the operation ends.
pub(crate) fn start_action(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    session: Option<Session>,
    request: ActionRequest,
    elevated_as: Option<String>,
) -> OperationId {
    let server = shared
        .state()
        .connection(connection)
        .map(|v| v.server.clone())
        .unwrap_or_default();
    let kind = kind_of(shared, connection, &request);
    let object = match &request {
        ActionRequest::Vm { vm, .. } => Some(OperationObject::Vm(vm.clone())),
        ActionRequest::Host { host, .. } => Some(OperationObject::Host(host.clone())),
        ActionRequest::DismissAlerts { .. } | ActionRequest::SetPerfmon { .. } => None,
    };
    let op = shared.start_operation(Some(connection), &server, kind.clone());
    let elevated = elevated_as.is_some();
    shared.update_operation(op, |o| {
        o.object = object.clone();
        o.elevated_as = elevated_as;
    });
    let Some(session) = session else {
        shared.finish_operation(op, OperationStatus::Failed(OperationFailure::NotConnected));
        return op;
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        if let Some(object) = &object {
            set_busy(&shared, connection, object, true);
        }
        let outcome = match &request {
            ActionRequest::Vm { vm, action } => {
                run_vm_action(&shared, connection, op, &session, vm, action).await
            }
            ActionRequest::Host { host, action } => {
                run_host_action(&shared, connection, op, &session, host, *action).await
            }
            ActionRequest::DismissAlerts { messages } => {
                dismiss_alerts(&shared, op, &session, messages).await
            }
            ActionRequest::SetPerfmon { object, config } => {
                set_perfmon(&shared, connection, &session, object, config.as_deref()).await
            }
        };
        if let Some(object) = &object {
            set_busy(&shared, connection, object, false);
        }
        shared.update_operation(op, |o| o.step = None);
        if elevated {
            crate::backend::logout_later(session);
        }
        match outcome {
            TaskOutcome::Succeeded => shared.finish_operation(op, OperationStatus::Succeeded),
            TaskOutcome::Cancelled => shared.finish_operation(op, OperationStatus::Cancelled),
            TaskOutcome::Failed(error) => {
                tracing::warn!(error = %error, "operation failed");
                let error = Arc::new(error);
                shared.finish_operation(
                    op,
                    OperationStatus::Failed(OperationFailure::Xapi(error.clone())),
                );
                if shared.state().connection(connection).is_some() {
                    shared.notify(Notification::ActionFailed {
                        connection,
                        operation: op,
                        kind,
                        error,
                    });
                }
            }
        }
    });
    op
}

async fn run_vm_action(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    action: &VmAction,
) -> TaskOutcome {
    if matches!(action, VmAction::HardShutdown | VmAction::HardReboot) {
        // `CancelAllTasks`: what this client still runs on the VM goes first.
        cancel_running_on(shared, connection, session, op, vm).await;
    }
    let s = session;
    let start = async {
        match action {
            VmAction::Start => xapi::vm::async_start(s, vm, false, false).await,
            VmAction::CleanShutdown => xapi::vm::async_clean_shutdown(s, vm).await,
            VmAction::CleanReboot => xapi::vm::async_clean_reboot(s, vm).await,
            VmAction::Suspend => xapi::vm::async_suspend(s, vm).await,
            VmAction::Resume => xapi::vm::async_resume(s, vm, false, false).await,
            VmAction::Pause => xapi::vm::async_pause(s, vm).await,
            VmAction::Unpause => xapi::vm::async_unpause(s, vm).await,
            VmAction::HardShutdown => xapi::vm::async_hard_shutdown(s, vm).await,
            VmAction::HardReboot => xapi::vm::async_hard_reboot(s, vm).await,
            VmAction::StartOn(h) => xapi::vm::async_start_on(s, vm, h, false, false).await,
            VmAction::ResumeOn(h) => xapi::vm::async_resume_on(s, vm, h, false, false).await,
            VmAction::Migrate(h) => {
                let options = HashMap::from([("live".to_owned(), "true".to_owned())]);
                xapi::vm::async_pool_migrate(s, vm, h, &options).await
            }
        }
    };
    run_task(shared, op, session, start, Span::FULL, false).await
}

/// `DismissAlertsAction`: destroys each message; one already gone
/// (`HANDLE_INVALID`, dismissed elsewhere) is fine.
async fn dismiss_alerts(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    messages: &[Ref<xapi::Message>],
) -> TaskOutcome {
    let count = messages.len().max(1) as f64;
    for (i, message) in messages.iter().enumerate() {
        match xapi::message::destroy(session, message).await {
            Ok(()) => {}
            Err(e) if e.is_api(codes::HANDLE_INVALID) => {
                tracing::debug!("alert already dismissed");
            }
            Err(e) => return TaskOutcome::Failed(e),
        }
        shared.update_operation(op, |o| o.progress = Some((i + 1) as f64 / count));
    }
    TaskOutcome::Succeeded
}

/// `PerfmonDefinitionAction`: replaces (or removes) `other_config["perfmon"]`,
/// then asks perfmon on the hosts concerned to read it again (the host
/// itself, the VM's home, the SR's hosts); without that refresh perfmon
/// picks the change up within half an hour. A failed refresh is logged
/// only, as in the original.
async fn set_perfmon(
    shared: &Shared,
    connection: ConnectionId,
    session: &Session,
    object: &PerfmonObjectRef,
    config: Option<&str>,
) -> TaskOutcome {
    use crate::perfmon::PERFMON_KEY;
    let s = session;
    let written = async {
        match object {
            PerfmonObjectRef::Vm(r) => {
                xapi::vm::remove_from_other_config(s, r, PERFMON_KEY).await?;
                if let Some(xml) = config {
                    xapi::vm::add_to_other_config(s, r, PERFMON_KEY, xml).await?;
                }
            }
            PerfmonObjectRef::Host(r) => {
                xapi::host::remove_from_other_config(s, r, PERFMON_KEY).await?;
                if let Some(xml) = config {
                    xapi::host::add_to_other_config(s, r, PERFMON_KEY, xml).await?;
                }
            }
            PerfmonObjectRef::Sr(r) => {
                xapi::sr::remove_from_other_config(s, r, PERFMON_KEY).await?;
                if let Some(xml) = config {
                    xapi::sr::add_to_other_config(s, r, PERFMON_KEY, xml).await?;
                }
            }
        }
        Ok::<(), xapi::Error>(())
    }
    .await;
    if let Err(e) = written {
        return TaskOutcome::Failed(e);
    }
    if !shared.options.perfmon_refresh {
        return TaskOutcome::Succeeded;
    }
    let hosts: Vec<Ref<Host>> = {
        let state = shared.state();
        let model = state
            .connection(connection)
            .map(|v| v.model.clone())
            .unwrap_or_default();
        match object {
            PerfmonObjectRef::Host(h) => vec![h.clone()],
            PerfmonObjectRef::Vm(v) => model
                .vms
                .get(v)
                .and_then(|vm| objects::vm_home(&model, vm))
                .cloned()
                .into_iter()
                .collect(),
            PerfmonObjectRef::Sr(sr) => model
                .srs
                .get(sr)
                .map(|rec| {
                    rec.pbds
                        .iter()
                        .filter_map(|p| model.get(p))
                        .map(|p| p.host.clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    };
    let none = HashMap::new();
    for host in hosts {
        let refresh = xapi::host::call_plugin(s, &host, "perfmon", "refresh", &none).await;
        if let Err(e) = refresh {
            let not_running = matches!(&e, xapi::Error::Api { params, .. }
                if params.iter().any(|p| p.contains("not running")));
            if not_running {
                let started = async {
                    xapi::host::call_plugin(s, &host, "perfmon", "start", &none).await?;
                    xapi::host::call_plugin(s, &host, "perfmon", "refresh", &none).await
                }
                .await;
                if let Err(e) = started {
                    tracing::debug!(error = %e, "perfmon refresh failed; alerts start within half an hour");
                }
            } else {
                tracing::debug!(error = %e, "perfmon refresh failed; alerts start within half an hour");
            }
        }
    }
    TaskOutcome::Succeeded
}

/// Cancels the tasks of this client's other running operations on `vm`.
async fn cancel_running_on(
    shared: &Shared,
    connection: ConnectionId,
    session: &Session,
    op: OperationId,
    vm: &Ref<Vm>,
) {
    let object = OperationObject::Vm(vm.clone());
    let tasks: Vec<Ref<Task>> = shared
        .state()
        .operations
        .iter()
        .filter(|o| {
            o.id != op
                && o.connection == Some(connection)
                && o.object.as_ref() == Some(&object)
                && o.can_cancel()
        })
        .filter_map(|o| o.task.clone())
        .collect();
    for task in tasks {
        if let Err(e) = xapi::task::cancel(session, &task).await {
            tracing::debug!(error = %e, "cancelling a task before a forced operation failed");
        }
    }
}

async fn run_host_action(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    host: &Ref<Host>,
    action: HostAction,
) -> TaskOutcome {
    let s = session;
    match action {
        HostAction::Reboot | HostAction::Shutdown => {
            reboot_or_shutdown(shared, connection, op, session, host, action).await
        }
        HostAction::PowerOn => {
            let start = xapi::host::async_power_on(s, host);
            run_task(shared, op, session, start, Span::FULL, false).await
        }
        HostAction::RestartToolstack => {
            let coordinator = is_coordinator(shared, connection, host);
            let start = xapi::host::async_restart_agent(s, host);
            run_task(shared, op, session, start, Span::FULL, coordinator).await
        }
        HostAction::EnterMaintenance => {
            shared.update_operation(op, |o| o.step = Some(OperationStep::Disabling));
            let disabled = run_task(
                shared,
                op,
                session,
                xapi::host::async_disable(s, host),
                Span(0.0, 0.2),
                false,
            )
            .await;
            if !matches!(disabled, TaskOutcome::Succeeded) {
                return disabled;
            }
            let flagged = async {
                let _ = xapi::host::remove_from_other_config(s, host, MAINTENANCE_MODE).await;
                xapi::host::add_to_other_config(s, host, MAINTENANCE_MODE, "true").await
            }
            .await;
            let outcome = match flagged {
                Err(e) => TaskOutcome::Failed(e),
                Ok(()) => {
                    shared.update_operation(op, |o| o.step = Some(OperationStep::Evacuating));
                    let start = xapi::host::async_evacuate(s, host, None, None);
                    run_task(shared, op, session, start, Span(0.2, 1.0), false).await
                }
            };
            if !matches!(outcome, TaskOutcome::Succeeded) {
                shared.update_operation(op, |o| o.step = Some(OperationStep::Reenabling));
                if let TaskOutcome::Failed(e) = enable(shared, op, session, host).await {
                    tracing::warn!(error = %e, "could not take the host out of maintenance mode again");
                }
            }
            outcome
        }
        HostAction::ExitMaintenance => enable(shared, op, session, host).await,
    }
}

/// `HostAbstractAction.Enable`: clears the maintenance flag and enables the
/// host.
async fn enable(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    host: &Ref<Host>,
) -> TaskOutcome {
    if let Err(e) = xapi::host::remove_from_other_config(session, host, MAINTENANCE_MODE).await {
        return TaskOutcome::Failed(e);
    }
    let start = xapi::host::async_enable(session, host);
    run_task(shared, op, session, start, Span::FULL, false).await
}

async fn reboot_or_shutdown(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    host: &Ref<Host>,
    action: HostAction,
) -> TaskOutcome {
    let for_reboot = action == HostAction::Reboot;
    let (was_enabled, guests) = {
        let state = shared.state();
        let model = state
            .connection(connection)
            .map(|v| v.model.clone())
            .unwrap_or_default();
        match model.hosts.get(host) {
            Some(rec) => (
                rec.enabled,
                actions::running_guests(&model, rec)
                    .into_iter()
                    .map(|(r, v)| {
                        let clean = v.allowed_operations.contains(&VmOperations::CleanShutdown);
                        (r.clone(), v.name_label.clone(), clean)
                    })
                    .collect::<Vec<_>>(),
            ),
            None => (true, Vec::new()),
        }
    };
    let reenable = || async {
        if was_enabled {
            shared.update_operation(op, |o| o.step = Some(OperationStep::Reenabling));
            if let Err(e) = xapi::host::enable(session, host).await {
                tracing::warn!(error = %e, "could not enable the host again");
            }
        }
    };
    if let Err(e) = xapi::host::disable(session, host).await {
        return TaskOutcome::Failed(e);
    }
    let count = guests.len();
    let share = 0.94 / count.max(1) as f64;
    for (index, (vm, vm_name, clean)) in guests.into_iter().enumerate() {
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::ShuttingDownVm {
                vm_name,
                index: index + 1,
                count,
                for_reboot,
            });
        });
        let start = async {
            if clean {
                xapi::vm::async_clean_shutdown(session, &vm).await
            } else {
                xapi::vm::async_hard_shutdown(session, &vm).await
            }
        };
        let from = 0.01 + share * index as f64;
        let outcome = run_task(shared, op, session, start, Span(from, from + share), false).await;
        if !matches!(outcome, TaskOutcome::Succeeded) {
            reenable().await;
            return outcome;
        }
    }
    shared.update_operation(op, |o| o.step = None);
    let coordinator = is_coordinator(shared, connection, host);
    let start = async {
        if for_reboot {
            xapi::host::async_reboot(session, host).await
        } else {
            xapi::host::async_shutdown(session, host).await
        }
    };
    let outcome = run_task(shared, op, session, start, Span(0.95, 1.0), coordinator).await;
    if !matches!(outcome, TaskOutcome::Succeeded) {
        reenable().await;
    }
    outcome
}

fn is_coordinator(shared: &Shared, connection: ConnectionId, host: &Ref<Host>) -> bool {
    shared
        .state()
        .connection(connection)
        .is_some_and(|v| objects::is_coordinator(&v.model, host))
}

/// Cancels `task`. With `record`, the cancellation itself is an entry of
/// the history (a task the user picked in the Events panel); otherwise it
/// is the cancel button of an operation that already has one.
pub(crate) fn cancel_task(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    session: Session,
    task: Ref<Task>,
    record: bool,
) {
    let op = record.then(|| {
        let state = shared.state();
        let view = state.connection(connection);
        let task_name = view
            .and_then(|v| v.model.tasks.get(&task))
            .map(|t| t.name_label.clone())
            .unwrap_or_else(|| task.to_string());
        let server = view.map(|v| v.server.clone()).unwrap_or_default();
        shared.start_operation(
            Some(connection),
            &server,
            OperationKind::CancelTask { task_name },
        )
    });
    let shared = shared.clone();
    tokio::spawn(async move {
        let result = xapi::task::cancel(&session, &task).await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "task cancel failed");
        }
        if let Some(op) = op {
            let status = match result {
                Ok(()) => OperationStatus::Succeeded,
                Err(e) => OperationStatus::Failed(OperationFailure::Xapi(Arc::new(e))),
            };
            shared.finish_operation(op, status);
        }
    });
}

fn set_busy(shared: &Shared, connection: ConnectionId, object: &OperationObject, busy: bool) {
    fn toggle<T: Clone + Eq + std::hash::Hash>(set: &mut Arc<HashSet<T>>, item: &T, busy: bool) {
        let mut next = HashSet::clone(set);
        if busy {
            next.insert(item.clone());
        } else {
            next.remove(item);
        }
        *set = Arc::new(next);
    }
    shared.update_connection(connection, |v| match object {
        OperationObject::Vm(vm) => toggle(&mut v.busy_vms, vm, busy),
        OperationObject::Host(host) => toggle(&mut v.busy_hosts, host, busy),
    });
}

enum TaskOutcome {
    Succeeded,
    Cancelled,
    Failed(xapi::Error),
}

/// The share of an operation's progress bar a task fills.
#[derive(Clone, Copy)]
struct Span(f64, f64);

impl Span {
    const FULL: Span = Span(0.0, 1.0);

    fn at(self, progress: f64) -> f64 {
        self.0 + (self.1 - self.0) * progress.clamp(0.0, 1.0)
    }
}

/// Starts a task with `start`, follows it until it ends (publishing its
/// progress on operation `op`, mapped to `span`) and destroys it.
///
/// With `goes_away` (the operation restarts or stops the server this
/// connection talks to), losing the server while following the task means
/// it did its job.
async fn run_task(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl Future<Output = xapi::Result<Ref<Task>>>,
    span: Span,
    goes_away: bool,
) -> TaskOutcome {
    let task = match start.await {
        Ok(t) => t,
        Err(e) => return TaskOutcome::Failed(e),
    };
    shared.update_operation(op, |o| {
        o.task = Some(task.clone());
        o.progress = Some(span.at(0.0));
    });
    let outcome = loop {
        tokio::time::sleep(shared.options.task_poll_interval).await;
        let record = match xapi::task::get_record(session, &task).await {
            Ok(r) => r,
            Err(e) if goes_away && (e.is_unreachable() || e.is_api(codes::SESSION_INVALID)) => {
                tracing::info!(error = %e, "the server went away as asked");
                return TaskOutcome::Succeeded;
            }
            // `AsyncAction.Poll`: a task that vanished has finished.
            Err(e) if is_task_gone(&e) => break TaskOutcome::Succeeded,
            Err(e) => break TaskOutcome::Failed(e),
        };
        match record.status {
            TaskStatusType::Pending | TaskStatusType::Cancelling => {
                let progress = span.at(record.progress);
                shared.update_operation(op, |o| o.progress = Some(progress));
            }
            TaskStatusType::Success => break TaskOutcome::Succeeded,
            TaskStatusType::Cancelled => break TaskOutcome::Cancelled,
            TaskStatusType::Failure => {
                let error = task_error(record.error_info);
                if is_task_gone(&error) {
                    break TaskOutcome::Succeeded;
                }
                if error.is_api(codes::TASK_CANCELLED) {
                    break TaskOutcome::Cancelled;
                }
                break TaskOutcome::Failed(error);
            }
            TaskStatusType::Unknown(status) => {
                break TaskOutcome::Failed(xapi::Error::Protocol(format!(
                    "unknown task status {status}"
                )));
            }
        }
    };
    shared.update_operation(op, |o| o.progress = Some(span.at(1.0)));
    if let Err(e) = xapi::task::destroy(session, &task).await {
        tracing::debug!(error = %e, "task.destroy failed");
    }
    outcome
}

/// `HANDLE_INVALID` on the task itself.
fn is_task_gone(e: &xapi::Error) -> bool {
    matches!(e, xapi::Error::Api { code, params }
        if code == "HANDLE_INVALID" && params.first().is_some_and(|c| c.eq_ignore_ascii_case("task")))
}

/// A failed task's `error_info` as an API error.
fn task_error(error_info: Vec<String>) -> xapi::Error {
    let mut info = error_info.into_iter();
    xapi::Error::Api {
        code: info.next().unwrap_or_else(|| "UNKNOWN_ERROR".to_owned()),
        params: info.collect(),
    }
}

/// A read-only question asked before an operation, whose answer the UI
/// shows next to the choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precheck {
    /// `VM.assert_can_boot_here` on each host (the Start on / Resume on /
    /// Migrate to Server submenus).
    BootHosts { vm: Ref<Vm>, hosts: Vec<Ref<Host>> },
    /// `host.get_vms_which_prevent_evacuation` (Enter Maintenance Mode).
    Evacuation { host: Ref<Host> },
}

/// A VM that keeps a host from entering maintenance mode, and why (an
/// error code and its parameters).
pub type EvacuationBlocker = (Ref<Vm>, Vec<String>);

/// The answer to a [`Precheck`].
#[derive(Debug, Clone)]
pub enum PrecheckResult {
    /// Per host: `None` if the VM can boot there, else why not.
    BootHosts(Vec<(Ref<Host>, Option<Arc<xapi::Error>>)>),
    /// The VMs that cannot be migrated away, with the server's reason
    /// (an error code and its parameters).
    Evacuation(Result<Vec<EvacuationBlocker>, Arc<xapi::Error>>),
}

pub(crate) fn run_precheck(
    shared: &Arc<Shared>,
    session: Session,
    connection: ConnectionId,
    id: u64,
    check: Precheck,
) {
    let shared = shared.clone();
    tokio::spawn(async move {
        let result = match check {
            Precheck::BootHosts { vm, hosts } => {
                let mut out = Vec::new();
                for host in hosts {
                    let error = xapi::vm::assert_can_boot_here(&session, &vm, &host)
                        .await
                        .err()
                        .map(Arc::new);
                    out.push((host, error));
                }
                PrecheckResult::BootHosts(out)
            }
            Precheck::Evacuation { host } => PrecheckResult::Evacuation(
                xapi::host::get_vms_which_prevent_evacuation(&session, &host)
                    .await
                    .map(|m| {
                        let mut v: Vec<_> = m.into_iter().collect();
                        v.sort_by(|a, b| a.0.cmp(&b.0));
                        v
                    })
                    .map_err(Arc::new),
            ),
        };
        shared.notify(Notification::Precheck {
            connection,
            id,
            result,
        });
    });
}
