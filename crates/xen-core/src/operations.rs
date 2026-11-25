//! The operation history behind the Events panel, and the operations that
//! run as XAPI tasks.
//!
//! Every operation this client starts gets an [`Operation`] entry: VM power
//! operations (run with their `Async.*` form so they have a server task with
//! progress that can be cancelled), task cancellations, and connection
//! events (connecting, reconnecting after a loss). Tasks started by other
//! clients are not copied here: the UI lists them straight from each
//! connection's cache (`ConnectionModel::tasks`, kept live by `event.from`).
//!
//! A running task is followed by polling `task.get_record` every
//! [`crate::BackendOptions::task_poll_interval`] (the original polls every
//! 900 ms); once it ends, the task is destroyed, as the original does.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use xapi::{Ref, Session, Task, TaskStatusType, Vm};

use crate::backend::{ConnectionError, ConnectionId, Notification, OperationId, Shared, VmAction};

/// What an operation does (the UI words it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationKind {
    /// Connecting to a server.
    Connect,
    /// The connection was lost; reconnecting.
    Reconnect,
    /// A VM power operation.
    VmAction { vm_name: String, action: VmAction },
    /// Cancelling a task (`task_name` is its `name_label`).
    CancelTask { task_name: String },
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

/// Starts a VM power operation as a XAPI task, recorded in the history.
pub(crate) fn start_vm_action(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    session: Option<Session>,
    vm: Ref<Vm>,
    action: VmAction,
) {
    let state = shared.state();
    let view = state.connection(connection);
    let server = view.map(|v| v.server.clone()).unwrap_or_default();
    let vm_name = view
        .and_then(|v| v.model.vms.get(&vm))
        .map(|r| r.name_label.clone())
        .unwrap_or_else(|| vm.to_string());
    let op = shared.start_operation(
        Some(connection),
        &server,
        OperationKind::VmAction {
            vm_name: vm_name.clone(),
            action,
        },
    );
    let Some(session) = session else {
        shared.finish_operation(op, OperationStatus::Failed(OperationFailure::NotConnected));
        return;
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        set_busy(&shared, connection, &vm, true);
        let start = {
            let (session, vm) = (session.clone(), vm.clone());
            async move {
                match action {
                    VmAction::Start => xapi::vm::async_start(&session, &vm, false, false).await,
                    VmAction::CleanShutdown => xapi::vm::async_clean_shutdown(&session, &vm).await,
                }
            }
        };
        let outcome = run_task(&shared, op, &session, start).await;
        set_busy(&shared, connection, &vm, false);
        match outcome {
            TaskOutcome::Succeeded => shared.finish_operation(op, OperationStatus::Succeeded),
            TaskOutcome::Cancelled => shared.finish_operation(op, OperationStatus::Cancelled),
            TaskOutcome::Failed(error) => {
                let error = Arc::new(error);
                shared.finish_operation(
                    op,
                    OperationStatus::Failed(OperationFailure::Xapi(error.clone())),
                );
                if shared.state().connection(connection).is_some() {
                    shared.notify(Notification::VmActionFailed {
                        connection,
                        vm_name,
                        action,
                        error,
                    });
                }
            }
        }
    });
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

fn set_busy(shared: &Shared, connection: ConnectionId, vm: &Ref<Vm>, busy: bool) {
    shared.update_connection(connection, |v| {
        let mut set = HashSet::clone(&v.busy_vms);
        if busy {
            set.insert(vm.clone());
        } else {
            set.remove(vm);
        }
        v.busy_vms = Arc::new(set);
    });
}

enum TaskOutcome {
    Succeeded,
    Cancelled,
    Failed(xapi::Error),
}

/// Starts a task with `start`, follows it until it ends (publishing its
/// progress on operation `op`) and destroys it.
async fn run_task(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl Future<Output = xapi::Result<Ref<Task>>>,
) -> TaskOutcome {
    let task = match start.await {
        Ok(t) => t,
        Err(e) => return TaskOutcome::Failed(e),
    };
    shared.update_operation(op, |o| {
        o.task = Some(task.clone());
        o.progress = Some(0.0);
    });
    let outcome = loop {
        tokio::time::sleep(shared.options.task_poll_interval).await;
        let record = match xapi::task::get_record(session, &task).await {
            Ok(r) => r,
            Err(e) => break TaskOutcome::Failed(e),
        };
        match record.status {
            TaskStatusType::Pending | TaskStatusType::Cancelling => {
                let progress = record.progress.clamp(0.0, 1.0);
                shared.update_operation(op, |o| o.progress = Some(progress));
            }
            TaskStatusType::Success => break TaskOutcome::Succeeded,
            TaskStatusType::Cancelled => break TaskOutcome::Cancelled,
            TaskStatusType::Failure => break TaskOutcome::Failed(task_error(record.error_info)),
            TaskStatusType::Unknown(status) => {
                break TaskOutcome::Failed(xapi::Error::Protocol(format!(
                    "unknown task status {status}"
                )));
            }
        }
    };
    if let Err(e) = xapi::task::destroy(session, &task).await {
        tracing::debug!(error = %e, "task.destroy failed");
    }
    outcome
}

/// A failed task's `error_info` as an API error.
fn task_error(error_info: Vec<String>) -> xapi::Error {
    let mut info = error_info.into_iter();
    xapi::Error::Api {
        code: info.next().unwrap_or_else(|| "UNKNOWN_ERROR".to_owned()),
        params: info.collect(),
    }
}
