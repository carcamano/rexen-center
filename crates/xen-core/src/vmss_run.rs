//! How the scheduled-snapshot operations run (see [`crate::vmss`]), call
//! for call as the original's actions:
//!
//! * **`CreateVMPolicy`**: `Async.VMSS.create` with the record, then
//!   `VM.set_snapshot_schedule` for each chosen VM, then `VMSS.snapshot_now`
//!   when "run now" was ticked.
//! * **`ChangePolicyEnabledAction`**: `VMSS.set_enabled`.
//! * **`RunPolicyNowAction`**: `VMSS.snapshot_now`.
//! * **`DestroyPolicyAction`**: per policy, its VMs released
//!   (`VM.set_snapshot_schedule` to null), then `VMSS.destroy` (one already
//!   gone is fine).
//! * **`AssignVMsToPolicyAction`** / **`RemoveVMsFromPolicyAction`**.
//! * The policy Properties dialog: the changed fields with their setters
//!   (`VMSS.SaveChanges` order: name, description, type, retention,
//!   frequency, schedule), then the members.

use xapi::{Ref, Session, Vm, Vmss, VmssRecord};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::operations::{OperationStep, Span, TaskOutcome, run_task_result};
use crate::vmss::{NewPolicySpec, PolicyChanges};

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return TaskOutcome::Failed(e),
        }
    };
}

/// Runs an F12b snapshot schedule request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::NewPolicy(spec) => create(shared, op, s, spec).await,
        ActionRequest::SetPolicyEnabled { policy, enabled } => {
            tri!(xapi::vmss::set_enabled(s, policy, *enabled).await);
            TaskOutcome::Succeeded
        }
        ActionRequest::RunPolicyNow { policy } => {
            tri!(xapi::vmss::snapshot_now(s, policy).await);
            TaskOutcome::Succeeded
        }
        ActionRequest::DeletePolicies { policies } => {
            destroy(shared, connection, op, s, policies).await
        }
        ActionRequest::AssignPolicy { policy, vms } => {
            assign(shared, connection, s, policy, vms).await
        }
        ActionRequest::RemoveFromPolicy { vms, .. } => {
            for vm in vms {
                tri!(xapi::vm::set_snapshot_schedule(s, vm, &Ref::null()).await);
            }
            TaskOutcome::Succeeded
        }
        ActionRequest::SetPolicy { policy, changes } => {
            set_properties(shared, connection, s, policy, changes).await
        }
        _ => unreachable!("not a snapshot schedule request"),
    }
}

/// `CreateVMPolicy`.
async fn create(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    spec: &NewPolicySpec,
) -> TaskOutcome {
    let record = VmssRecord {
        name_label: spec.name.clone(),
        name_description: spec.description.clone(),
        enabled: spec.enabled,
        r#type: spec.kind.clone(),
        retained_snapshots: spec.retained,
        frequency: spec.frequency.clone(),
        schedule: spec.schedule.clone(),
        // XAPI's "never".
        last_run_time: xapi::DateTime("19700101T00:00:00Z".to_owned()),
        ..VmssRecord::default()
    };
    let start = xapi::vmss::async_create(s, &record);
    let policy: Ref<Vmss> = match run_task_result(shared, op, s, start, Span(0.0, 0.5), false).await
    {
        (TaskOutcome::Succeeded, r) if r.starts_with("OpaqueRef:") => Ref::new(&r),
        (TaskOutcome::Succeeded, r) => {
            return TaskOutcome::Failed(xapi::Error::Protocol(format!(
                "the task returned no snapshot schedule: {r:?}"
            )));
        }
        (other, _) => return other,
    };
    for vm in &spec.vms {
        tri!(xapi::vm::set_snapshot_schedule(s, vm, &policy).await);
    }
    shared.update_operation(op, |o| o.progress = Some(0.6));
    if spec.run_now {
        tri!(xapi::vmss::snapshot_now(s, &policy).await);
    }
    TaskOutcome::Succeeded
}

/// `AssignVMsToPolicyAction`: `vms` become the members.
async fn assign(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    policy: &Ref<Vmss>,
    vms: &[Ref<Vm>],
) -> TaskOutcome {
    let model = crate::storage_run::model(shared, connection);
    let current: Vec<Ref<Vm>> = model
        .vmss
        .get(policy)
        .map(|p| p.vms.clone())
        .unwrap_or_default();
    for vm in current.iter().filter(|v| !vms.contains(v)) {
        tri!(xapi::vm::set_snapshot_schedule(s, vm, &Ref::null()).await);
    }
    for vm in vms {
        let already = model
            .vms
            .get(vm)
            .is_some_and(|v| &v.snapshot_schedule == policy);
        if !already {
            tri!(xapi::vm::set_snapshot_schedule(s, vm, policy).await);
        }
    }
    TaskOutcome::Succeeded
}

/// The policy Properties dialog's save.
async fn set_properties(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    policy: &Ref<Vmss>,
    c: &PolicyChanges,
) -> TaskOutcome {
    if let Some(name) = &c.name {
        tri!(xapi::vmss::set_name_label(s, policy, name).await);
    }
    if let Some(description) = &c.description {
        tri!(xapi::vmss::set_name_description(s, policy, description).await);
    }
    if let Some(kind) = &c.kind {
        tri!(xapi::vmss::set_type(s, policy, kind).await);
    }
    if let Some(retained) = c.retained {
        tri!(xapi::vmss::set_retained_snapshots(s, policy, retained).await);
    }
    if let Some(frequency) = &c.frequency {
        tri!(xapi::vmss::set_frequency(s, policy, frequency).await);
    }
    if let Some(schedule) = &c.schedule {
        tri!(xapi::vmss::set_schedule(s, policy, schedule).await);
    }
    if let Some(members) = &c.members {
        return assign(shared, connection, s, policy, members).await;
    }
    TaskOutcome::Succeeded
}

/// `DestroyPolicyAction`.
async fn destroy(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    policies: &[Ref<Vmss>],
) -> TaskOutcome {
    let model = crate::storage_run::model(shared, connection);
    let count = policies.len().max(1) as f64;
    for (i, policy) in policies.iter().enumerate() {
        let Some(p) = model.vmss.get(policy) else {
            continue;
        };
        let name = p.name_label.clone();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::DeletingPolicy { name });
        });
        for vm in &p.vms {
            tri!(xapi::vm::set_snapshot_schedule(s, vm, &Ref::null()).await);
        }
        match xapi::vmss::destroy(s, policy).await {
            Ok(()) => {}
            Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {
                tracing::debug!("snapshot schedule already deleted");
            }
            Err(e) => return TaskOutcome::Failed(e),
        }
        let done = (i + 1) as f64 / count;
        shared.update_operation(op, |o| o.progress = Some(done));
    }
    TaskOutcome::Succeeded
}
