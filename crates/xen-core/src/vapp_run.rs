//! How the vApp operations run (see [`crate::vapp`]), call for call as the
//! original's actions:
//!
//! * **`CreateVMApplianceAction`**: `Async.VM_appliance.create` with the
//!   name and description, then `VM.set_appliance` for each chosen VM;
//!   the changed startup settings follow (`SetVMStartupOptionsAction`).
//! * **`SetVMStartupOptionsAction`**: `VM.set_order` and
//!   `VM.set_start_delay` per VM, then `Async.pool.sync_database` so every
//!   server has them.
//! * **`AssignVMsToVMApplianceAction`**: the members left out are
//!   released (`VM.set_appliance` to null), the new ones assigned.
//! * **`RemoveVMsFromVMApplianceAction`**: each VM released.
//! * **`DestroyVMApplianceAction`**: per vApp, its VMs released, then
//!   `VM_appliance.destroy` (one already gone is fine).
//! * **`StartApplianceAction`** / **`ShutDownApplianceAction`**:
//!   `VM_appliance.start` (not paused; nothing for an empty vApp) and
//!   `VM_appliance.shutdown` (when the server allows it), as tasks so
//!   they show progress and can be cancelled.
//! * The vApp Properties dialog: `VM_appliance.set_name_label` /
//!   `set_name_description`, then the two actions above as needed.

use xapi::{Ref, Session, Vm, VmAppliance, VmApplianceOperation, VmApplianceRecord};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::operations::{OperationStep, Span, TaskOutcome, run_task, run_task_result};
use crate::vapp::{NewVAppSpec, StartupRow, VAppChanges};

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return TaskOutcome::Failed(e),
        }
    };
}

/// Runs an F12a vApp request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::NewVApp(spec) => create(shared, connection, op, s, spec).await,
        ActionRequest::SetVApp { vapp, changes } => {
            set_properties(shared, connection, op, s, vapp, changes).await
        }
        ActionRequest::AssignVApp { vapp, vms } => assign(shared, connection, s, vapp, vms).await,
        ActionRequest::RemoveFromVApp { vms, .. } => {
            for vm in vms {
                tri!(xapi::vm::set_appliance(s, vm, &Ref::null()).await);
            }
            TaskOutcome::Succeeded
        }
        ActionRequest::DeleteVApps { vapps } => destroy(shared, connection, op, s, vapps).await,
        ActionRequest::StartVApp { vapp } => {
            let model = crate::storage_run::model(shared, connection);
            if model
                .vm_appliances
                .get(vapp)
                .is_some_and(|a| a.vms.is_empty())
            {
                return TaskOutcome::Succeeded;
            }
            let start = xapi::vm_appliance::async_start(s, vapp, false);
            run_task(shared, op, s, start, Span::FULL, false).await
        }
        ActionRequest::ShutDownVApp { vapp } => {
            let model = crate::storage_run::model(shared, connection);
            let allowed = model.vm_appliances.get(vapp).is_some_and(|a| {
                a.allowed_operations
                    .contains(&VmApplianceOperation::Shutdown)
            });
            if !allowed {
                return TaskOutcome::Succeeded;
            }
            let start = xapi::vm_appliance::async_shutdown(s, vapp);
            run_task(shared, op, s, start, Span::FULL, false).await
        }
        _ => unreachable!("not a vApp request"),
    }
}

/// `CreateVMApplianceAction`, then `SetVMStartupOptionsAction`.
async fn create(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewVAppSpec,
) -> TaskOutcome {
    let record = VmApplianceRecord {
        name_label: spec.name.clone(),
        name_description: spec.description.clone(),
        ..VmApplianceRecord::default()
    };
    let end = if spec.startup.is_empty() { 1.0 } else { 0.5 };
    let start = xapi::vm_appliance::async_create(s, &record);
    let vapp: Ref<VmAppliance> =
        match run_task_result(shared, op, s, start, Span(0.0, end * 0.8), false).await {
            (TaskOutcome::Succeeded, r) if r.starts_with("OpaqueRef:") => Ref::new(&r),
            (TaskOutcome::Succeeded, r) => {
                return TaskOutcome::Failed(xapi::Error::Protocol(format!(
                    "the task returned no vApp: {r:?}"
                )));
            }
            (other, _) => return other,
        };
    for vm in &spec.vms {
        tri!(xapi::vm::set_appliance(s, vm, &vapp).await);
    }
    shared.update_operation(op, |o| o.progress = Some(end));
    if spec.startup.is_empty() {
        return TaskOutcome::Succeeded;
    }
    set_startup(shared, connection, op, s, &spec.startup, Span(end, 1.0)).await
}

/// `SetVMStartupOptionsAction`: order and delay per VM (60% of its
/// share), then `pool.sync_database`.
async fn set_startup(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    rows: &[StartupRow],
    span: Span,
) -> TaskOutcome {
    let model = crate::storage_run::model(shared, connection);
    let count = rows.len().max(1) as f64;
    let at = |f: f64| span.0 + (span.1 - span.0) * f;
    for (i, row) in rows.iter().enumerate() {
        let vm = model
            .vms
            .get(&row.vm)
            .map(|v| v.name_label.clone())
            .unwrap_or_default();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::SettingStartupOptions { vm });
        });
        tri!(xapi::vm::set_order(s, &row.vm, row.order).await);
        tri!(xapi::vm::set_start_delay(s, &row.vm, row.start_delay).await);
        let done = at(0.6 * (i + 1) as f64 / count);
        shared.update_operation(op, |o| o.progress = Some(done));
    }
    shared.update_operation(op, |o| o.step = None);
    let start = xapi::pool::async_sync_database(s);
    run_task(shared, op, s, start, Span(at(0.6), span.1), false).await
}

/// `AssignVMsToVMApplianceAction`: `vms` become the members.
async fn assign(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    vapp: &Ref<VmAppliance>,
    vms: &[Ref<Vm>],
) -> TaskOutcome {
    let model = crate::storage_run::model(shared, connection);
    let current: Vec<Ref<Vm>> = model
        .vm_appliances
        .get(vapp)
        .map(|a| a.vms.clone())
        .unwrap_or_default();
    for vm in current.iter().filter(|v| !vms.contains(v)) {
        tri!(xapi::vm::set_appliance(s, vm, &Ref::null()).await);
    }
    for vm in vms {
        let already = model.vms.get(vm).is_some_and(|v| &v.appliance == vapp);
        if !already {
            tri!(xapi::vm::set_appliance(s, vm, vapp).await);
        }
    }
    TaskOutcome::Succeeded
}

/// The vApp Properties dialog's save.
async fn set_properties(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vapp: &Ref<VmAppliance>,
    changes: &VAppChanges,
) -> TaskOutcome {
    if let Some(name) = &changes.name {
        tri!(xapi::vm_appliance::set_name_label(s, vapp, name).await);
    }
    if let Some(description) = &changes.description {
        tri!(xapi::vm_appliance::set_name_description(s, vapp, description).await);
    }
    if let Some(members) = &changes.members {
        let outcome = assign(shared, connection, s, vapp, members).await;
        if !matches!(outcome, TaskOutcome::Succeeded) {
            return outcome;
        }
    }
    if changes.startup.is_empty() {
        return TaskOutcome::Succeeded;
    }
    set_startup(shared, connection, op, s, &changes.startup, Span(0.3, 1.0)).await
}

/// `DestroyVMApplianceAction`.
async fn destroy(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vapps: &[Ref<VmAppliance>],
) -> TaskOutcome {
    let model = crate::storage_run::model(shared, connection);
    let count = vapps.len().max(1) as f64;
    for (i, vapp) in vapps.iter().enumerate() {
        let Some(app) = model.vm_appliances.get(vapp) else {
            continue;
        };
        let name = app.name_label.clone();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::DeletingVApp { name });
        });
        for vm in &app.vms {
            tri!(xapi::vm::set_appliance(s, vm, &Ref::null()).await);
        }
        match xapi::vm_appliance::destroy(s, vapp).await {
            Ok(()) => {}
            Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {
                tracing::debug!("vApp already deleted");
            }
            Err(e) => return TaskOutcome::Failed(e),
        }
        let done = (i + 1) as f64 / count;
        shared.update_operation(op, |o| o.progress = Some(done));
    }
    TaskOutcome::Succeeded
}
