//! The write tools: VM lifecycle and snapshots, through the same
//! operations the window runs ([`xen_core::ActionRequest`]).
//!
//! Every check happens before anything reaches a server, in this order,
//! and each failure is a refusal:
//!
//! 1. write tools are enabled in `mcp.toml` (`[writes] enabled = true`);
//! 2. the call says `confirm: true`;
//! 3. the server is connected and the VM (or snapshot) is found in the
//!    cache;
//! 4. the VM (for a snapshot, the VM it belongs to) is in the server's
//!    allow-list (`[writes.allow]`), by UUID or by a name only that VM has;
//! 5. the operation is offered now, by the same rules as the window's menus
//!    (`allowed_operations`, power state, a host to run on, not busy);
//! 6. HA: starting or resuming a VM the pool's HA protects needs the
//!    window (it asks the HA planner first), so it is refused here;
//! 7. RBAC: the session's roles allow every call the operation makes (no
//!    role elevation over MCP).
//!
//! Then the operation runs as an entry of the operation history and the
//! tool waits for its end (and for the cache to show it).

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;
use xapi::{Ref, Vm, VmPowerState, VmRecord};
use xen_core::actions::vm_action_available;
use xen_core::objects::{self, VmKind};
use xen_core::rbac::{self, RbacCheck};
use xen_core::{
    ActionRequest, Command, ConnectionView, OperationFailure, OperationStatus, VmAction,
    provisioning,
};

use crate::server::{Args, Call, McpServer, Slot, ToolError, ToolResult, resolve};
use crate::tools::{WRITE_TOOLS, tool};

/// The write tools' definitions.
pub fn definitions() -> Vec<Value> {
    let vm = json!({"type": "string", "description": "VM UUID or name."});
    let snapshot = json!({"type": "string", "description": "Snapshot UUID or name."});
    let confirm =
        json!({"type": "boolean", "description": "Must be true: the user agreed to this change."});
    let force = json!({"type": "boolean", "description": "Force (hard) instead of a clean operation through the guest (default false)."});
    vec![
        tool(
            "vm_start",
            "Start VM",
            "Start a halted VM. Only VMs in the server's write allow-list.",
            json!({"vm": vm, "confirm": confirm}),
            &["vm", "confirm"],
            true,
        ),
        tool(
            "vm_shutdown",
            "Shut down VM",
            "Shut a VM down: cleanly through the guest, or forced (power off) with force: true. Only VMs in the server's write allow-list.",
            json!({"vm": vm, "force": force, "confirm": confirm}),
            &["vm", "confirm"],
            true,
        ),
        tool(
            "vm_reboot",
            "Reboot VM",
            "Reboot a running VM: cleanly, or forced with force: true. Only VMs in the server's write allow-list.",
            json!({"vm": vm, "force": force, "confirm": confirm}),
            &["vm", "confirm"],
            true,
        ),
        tool(
            "vm_suspend",
            "Suspend VM",
            "Suspend a running VM to disk. Only VMs in the server's write allow-list.",
            json!({"vm": vm, "confirm": confirm}),
            &["vm", "confirm"],
            true,
        ),
        tool(
            "vm_resume",
            "Resume VM",
            "Resume a suspended VM. Only VMs in the server's write allow-list.",
            json!({"vm": vm, "confirm": confirm}),
            &["vm", "confirm"],
            true,
        ),
        tool(
            "vm_snapshot",
            "Take snapshot",
            "Take a snapshot of a VM (with memory: true, a checkpoint of a running VM). Only VMs in the server's write allow-list.",
            json!({
                "vm": vm,
                "name": {"type": "string", "description": "The snapshot's name."},
                "description": {"type": "string"},
                "memory": {"type": "boolean", "description": "Include the memory (running VMs only; default false)."},
                "confirm": confirm
            }),
            &["vm", "name", "confirm"],
            true,
        ),
        tool(
            "vm_snapshot_revert",
            "Revert to snapshot",
            "Revert a VM to one of its snapshots; its current state is lost. Only snapshots of VMs in the server's write allow-list.",
            json!({"snapshot": snapshot, "confirm": confirm}),
            &["snapshot", "confirm"],
            true,
        ),
        tool(
            "vm_snapshot_delete",
            "Delete snapshot",
            "Delete a snapshot with its own disks. Only snapshots of VMs in the server's write allow-list.",
            json!({"snapshot": snapshot, "confirm": confirm}),
            &["snapshot", "confirm"],
            true,
        ),
    ]
}

/// What a write tool does to its VM.
enum Plan {
    Lifecycle(VmAction),
    Snapshot {
        name: String,
        description: String,
        memory: bool,
    },
    Revert,
    DeleteSnapshot,
}

/// Runs write tool `name`; `None` if it is not one.
pub async fn call_write(
    server: &McpServer,
    call: &mut Call,
    name: &str,
    args: &Args<'_>,
) -> Option<ToolResult> {
    if !WRITE_TOOLS.contains(&name) {
        return None;
    }
    Some(run(server, call, name, args).await)
}

async fn run(server: &McpServer, call: &mut Call, name: &str, args: &Args<'_>) -> ToolResult {
    // 1. The switch, before anything else is looked at.
    if !server.writes_enabled() {
        return Err(ToolError::Refused(
            "write tools are disabled; they need `[writes] enabled = true` in mcp.toml and the VM in `[writes.allow]`"
                .into(),
        ));
    }
    let (known, plan): (&[&str], Plan) = match name {
        "vm_start" => (
            &["server", "vm", "confirm"],
            Plan::Lifecycle(VmAction::Start),
        ),
        "vm_suspend" => (
            &["server", "vm", "confirm"],
            Plan::Lifecycle(VmAction::Suspend),
        ),
        "vm_resume" => (
            &["server", "vm", "confirm"],
            Plan::Lifecycle(VmAction::Resume),
        ),
        "vm_shutdown" | "vm_reboot" => {
            let force = args.bool("force")?.unwrap_or(false);
            let action = match (name, force) {
                ("vm_shutdown", false) => VmAction::CleanShutdown,
                ("vm_shutdown", true) => VmAction::HardShutdown,
                (_, false) => VmAction::CleanReboot,
                (_, true) => VmAction::HardReboot,
            };
            (
                &["server", "vm", "force", "confirm"],
                Plan::Lifecycle(action),
            )
        }
        "vm_snapshot" => (
            &["server", "vm", "name", "description", "memory", "confirm"],
            Plan::Snapshot {
                name: args.req_str("name")?.to_owned(),
                description: args.str("description")?.unwrap_or_default().to_owned(),
                memory: args.bool("memory")?.unwrap_or(false),
            },
        ),
        "vm_snapshot_revert" => (&["server", "snapshot", "confirm"], Plan::Revert),
        "vm_snapshot_delete" => (&["server", "snapshot", "confirm"], Plan::DeleteSnapshot),
        other => return Err(ToolError::Invalid(format!("unknown tool {other}"))),
    };
    args.only(known)?;
    // 2. Explicit confirmation.
    if args.bool("confirm")? != Some(true) {
        return Err(ToolError::Refused(
            "this tool changes the VM: ask the user, then call it again with confirm: true".into(),
        ));
    }
    // 3. The server and the object, from the cache.
    let (slot, conn) = server.connection(call, args.str("server")?).await?;
    let model = &conn.model;
    let (object, vm_ref): (Ref<Vm>, Ref<Vm>) = match plan {
        Plan::Revert | Plan::DeleteSnapshot => {
            let s = resolve(
                model
                    .vms
                    .iter()
                    .filter(|(_, v)| v.is_a_snapshot)
                    .map(|(r, v)| (r, v.uuid.as_str(), v.name_label.as_str())),
                args.req_str("snapshot")?,
                "snapshot",
            )?;
            (s.clone(), model.vms[s].snapshot_of.clone())
        }
        _ => {
            let v = resolve(
                model
                    .vms
                    .iter()
                    .filter(|(_, v)| objects::vm_kind(v) == VmKind::Guest)
                    .map(|(r, v)| (r, v.uuid.as_str(), v.name_label.as_str())),
                args.req_str("vm")?,
                "VM",
            )?;
            (v.clone(), v.clone())
        }
    };
    let record = model.vms[&object].clone();
    let mut target = json!({"kind": if record.is_a_snapshot { "snapshot" } else { "vm" }, "uuid": record.uuid, "name": record.name_label});
    let vm = model.vms.get(&vm_ref).cloned();
    if record.is_a_snapshot {
        target["vm"] = vm.as_ref().map_or(
            Value::Null,
            |v| json!({"uuid": v.uuid, "name": v.name_label}),
        );
    }
    call.target = Some(target);
    // 4. The allow-list.
    let Some(vm) = vm else {
        return Err(ToolError::Refused(
            "the snapshot's VM is not in the cache, so the allow-list cannot be checked".into(),
        ));
    };
    check_allowed(server, slot, &conn, &vm)?;
    // 5. Offered now, as in the window.
    let busy = conn.busy_vms.contains(&vm_ref);
    let request = match plan {
        Plan::Lifecycle(action) => {
            if !vm_action_available(model, &vm, &action, busy) {
                return Err(not_offered(&vm, action_name(&action), busy));
            }
            ActionRequest::Vm {
                vm: vm_ref.clone(),
                action,
            }
        }
        Plan::Snapshot {
            name,
            description,
            memory,
        } => {
            if !provisioning::can_snapshot(&vm, busy) {
                return Err(not_offered(&vm, "snapshot", busy));
            }
            if memory && !provisioning::can_checkpoint(&vm) {
                return Err(ToolError::Refused(
                    "a snapshot with memory needs a running VM that allows checkpoints".into(),
                ));
            }
            ActionRequest::Snapshot {
                vm: vm_ref.clone(),
                name,
                description,
                memory,
            }
        }
        Plan::Revert => {
            if busy || !provisioning::can_revert(model, &record) {
                return Err(not_offered(&record, "revert", busy));
            }
            ActionRequest::Revert {
                snapshot: object.clone(),
                snapshot_first: None,
            }
        }
        Plan::DeleteSnapshot => {
            if !provisioning::can_delete_snapshot(&record) {
                return Err(not_offered(&record, "destroy", false));
            }
            ActionRequest::DeleteSnapshots {
                snapshots: vec![object.clone()],
            }
        }
    };
    // 6. HA-protected starts go through the window's HA planner.
    if let ActionRequest::Vm {
        action: VmAction::Start | VmAction::Resume,
        ..
    } = &request
        && model.pool().is_some_and(|(_, p)| p.ha_enabled)
        && xen_core::ha::priority(&vm).is_restart()
    {
        return Err(ToolError::Refused(
            "the pool's HA protects this VM; start or resume it from the window, which checks the HA plan first".into(),
        ));
    }
    // 7. RBAC.
    if let RbacCheck::Denied { authorized } =
        rbac::check(model, conn.rbac.as_deref(), &request.rbac_methods())
    {
        let roles: Vec<&str> = authorized.iter().map(|r| r.name_label.as_str()).collect();
        return Err(ToolError::Refused(format!(
            "the user {} is not allowed to do this (roles that are: {})",
            conn.username,
            if roles.is_empty() {
                "none".to_owned()
            } else {
                roles.join(", ")
            }
        )));
    }
    // Run it.
    let (reply, wait) = oneshot::channel();
    server.backend.send(Command::ActionTracked {
        connection: conn.id,
        request: request.clone(),
        reply,
    });
    let op = wait
        .await
        .map_err(|_| ToolError::Failed("the backend stopped".into()))?;
    tracing::info!(tool = name, vm = %vm.uuid, op, "write operation started");
    let finished = wait_operation(server, op).await?;
    call.detail = Some(format!("operation {op}: {}", status_name(&finished.status)));
    if let OperationStatus::Failed(f) = &finished.status {
        return Err(ToolError::Failed(format!(
            "the operation failed: {}",
            describe_failure(f)
        )));
    }
    if matches!(finished.status, OperationStatus::Cancelled) {
        return Err(ToolError::Failed("the operation was cancelled".into()));
    }
    settle(server, &conn, &request, &vm_ref, finished.created.as_ref()).await
}

fn action_name(action: &VmAction) -> &'static str {
    match action {
        VmAction::Start => "start",
        VmAction::CleanShutdown => "clean shutdown",
        VmAction::HardShutdown => "forced shutdown",
        VmAction::CleanReboot => "clean reboot",
        VmAction::HardReboot => "forced reboot",
        VmAction::Suspend => "suspend",
        VmAction::Resume => "resume",
        _ => "this operation",
    }
}

fn not_offered(vm: &VmRecord, what: &str, busy: bool) -> ToolError {
    let allowed: Vec<&str> = vm.allowed_operations.iter().map(|o| o.as_str()).collect();
    ToolError::Refused(format!(
        "{what} is not possible for '{}' now (power state {}{}; the server allows: {})",
        vm.name_label,
        vm.power_state.as_str(),
        if busy {
            ", another operation of this client is running on it"
        } else {
            ""
        },
        if allowed.is_empty() {
            "nothing".to_owned()
        } else {
            allowed.join(", ")
        }
    ))
}

/// Refuses unless `vm` is in the allow-list of the slot's server.
fn check_allowed(
    server: &McpServer,
    slot: &Slot,
    conn: &ConnectionView,
    vm: &VmRecord,
) -> Result<(), ToolError> {
    let Some(entries) = server
        .config
        .allowed_for(&[slot.address.as_str(), conn.server.as_str()])
    else {
        return Err(ToolError::Refused(format!(
            "server {} has no write allow-list (`[writes.allow]` in mcp.toml)",
            slot.address
        )));
    };
    let guests_named = |name: &str| {
        conn.model
            .vms
            .values()
            .filter(|v| objects::vm_kind(v) == VmKind::Guest && v.name_label == name)
            .count()
    };
    let mut ambiguous = false;
    for entry in entries {
        let entry = entry.trim();
        if entry.eq_ignore_ascii_case(&vm.uuid) {
            return Ok(());
        }
        if entry == vm.name_label {
            if guests_named(entry) == 1 {
                return Ok(());
            }
            ambiguous = true;
        }
    }
    Err(ToolError::Refused(format!(
        "VM '{}' ({}) is not in the write allow-list of server {}{}",
        vm.name_label,
        vm.uuid,
        slot.address,
        if ambiguous {
            " (its name is listed, but several VMs share it: list the UUID instead)"
        } else {
            ""
        }
    )))
}

async fn wait_operation(
    server: &McpServer,
    op: xen_core::OperationId,
) -> Result<std::sync::Arc<xen_core::Operation>, ToolError> {
    let deadline = Instant::now() + server.timings.operation_timeout;
    loop {
        if let Some(o) = server.backend.state().operation(op)
            && !o.is_running()
        {
            return Ok(o.clone());
        }
        if Instant::now() >= deadline {
            return Err(ToolError::Failed(format!(
                "the operation is still running after {} s; it continues on the server",
                server.timings.operation_timeout.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn status_name(s: &OperationStatus) -> String {
    match s {
        OperationStatus::Running => "running".into(),
        OperationStatus::Succeeded => "succeeded".into(),
        OperationStatus::Cancelled => "cancelled".into(),
        OperationStatus::Failed(f) => format!("failed ({})", describe_failure(f)),
    }
}

fn describe_failure(f: &OperationFailure) -> String {
    match f {
        OperationFailure::Xapi(e) => e.to_string(),
        OperationFailure::Connection(e) => crate::server::describe_connection_error(e),
        OperationFailure::NotConnected => "the server is not connected".into(),
        OperationFailure::Disconnected => "the connection was closed".into(),
    }
}

/// After success: waits (briefly) for the cache to show the effect, and
/// answers with the VM's or snapshot's state.
async fn settle(
    server: &McpServer,
    conn: &ConnectionView,
    request: &ActionRequest,
    vm: &Ref<Vm>,
    created: Option<&Ref<Vm>>,
) -> ToolResult {
    let expected = match request {
        ActionRequest::Vm { action, .. } => match action {
            VmAction::Start | VmAction::Resume | VmAction::CleanReboot | VmAction::HardReboot => {
                Some(VmPowerState::Running)
            }
            VmAction::CleanShutdown | VmAction::HardShutdown => Some(VmPowerState::Halted),
            VmAction::Suspend => Some(VmPowerState::Suspended),
            _ => None,
        },
        _ => None,
    };
    let deadline = Instant::now() + server.timings.settle_wait;
    loop {
        let state = server.backend.state();
        let model = state.connection(conn.id).map(|c| c.model.clone());
        // The VM is settled once its task has left `current_operations`:
        // the server updates `allowed_operations` in the same change, so
        // the next call sees what is offered now (a reboot keeps the power
        // state Running throughout).
        let idle = |m: &xen_core::ConnectionModel| {
            m.vms
                .get(vm)
                .is_none_or(|v| v.current_operations.is_empty())
        };
        let done = model.as_ref().is_some_and(|m| {
            idle(m)
                && match request {
                    ActionRequest::Vm { .. } => m
                        .vms
                        .get(vm)
                        .is_some_and(|v| Some(&v.power_state) == expected.as_ref()),
                    ActionRequest::Snapshot { .. } => {
                        created.is_some_and(|c| m.vms.contains_key(c))
                    }
                    ActionRequest::DeleteSnapshots { snapshots } => {
                        snapshots.iter().all(|s| !m.vms.contains_key(s))
                    }
                    _ => true,
                }
        });
        if done || Instant::now() >= deadline {
            let model = model.unwrap_or_default();
            let mut out = json!({"status": "succeeded"});
            if let Some(v) = model.vms.get(vm) {
                out["vm"] = json!({"uuid": v.uuid, "name": v.name_label, "power_state": v.power_state.as_str()});
            }
            if let Some(s) = created.and_then(|c| model.vms.get(c)) {
                out["snapshot"] = crate::views::snapshot_summary(s);
            }
            if let ActionRequest::DeleteSnapshots { snapshots } = request {
                out["deleted"] = json!(snapshots.iter().all(|s| !model.vms.contains_key(s)));
            }
            if !done {
                out["note"] = json!("the change is not in the cache yet");
            }
            return Ok(out);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
