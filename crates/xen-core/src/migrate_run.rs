//! How the Migrate / Move / Copy wizard's requests and checks run (see
//! [`crate::migrate`]), call for call as the original's actions:
//!
//! * **`VMCrossPoolMigrateAction`** ([`ActionRequest::MigrateSend`]):
//!   `host.migrate_receive` on the destination (with its own session) for
//!   the chosen server and migration network, then
//!   `Async.VM.migrate_send` here with `live`, the disk and interface maps
//!   and `copy=true` for a copy; the task's progress and Cancel are the
//!   operation's.
//! * **`VMMoveAction`** ([`ActionRequest::MoveVm`]): for each disk the VM
//!   owns whose SR changes, `Async.VDI.copy` to the new SR,
//!   `VDI.destroy` of the old disk (with its drive, as XAPI does) and
//!   `VBD.create` of a drive like the old one for the copy; then
//!   `VM.set_suspend_SR` to the first SR chosen. A disk that could not be
//!   destroyed fails the move at the end
//!   (`ACTION_VM_MOVING_VDI_DESTROY_FAILURE`).
//! * **`CrossPoolMigrateFilter.CanDoStorageMigration`**
//!   ([`crate::Precheck::Migrate`]): `host.migrate_receive` on the
//!   destination's management network, then `VM.assert_can_migrate` with
//!   every disk mapped to another SR that supports storage migration and
//!   (across pools) every interface to a network the server sees. A
//!   `VIF_NOT_IN_MAP` caused by snapshot interfaces is accepted. Within
//!   the pool in Migrate mode, a refusal falls back to
//!   `VM.assert_can_boot_here` (a plain live migration).

use std::collections::HashMap;
use std::sync::Arc;

use xapi::{Host, Network, Ref, Session, Sr, VbdRecord, Vdi, Vif, Vm};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, Notification, OperationId, Shared};
use crate::migrate::{self, MigrateSpec, ServerVerdict, WizardMode};
use crate::operations::{
    OperationStep, Precheck, PrecheckResult, Span, TaskOutcome, run_task, run_task_result,
};

/// Error codes of this console (not XAPI's).
pub mod errors {
    /// `CPM_WIZARD_ERROR_TARGET_DISCONNECTED`: the destination is not
    /// connected.
    pub const TARGET_DISCONNECTED: &str = "XENADMIN_CPM_TARGET_DISCONNECTED";
    /// `ACTION_VM_MOVING_VDI_DESTROY_FAILURE`: param 0 is the VM.
    pub const MOVE_DESTROY_FAILED: &str = "XENADMIN_MOVE_VDI_DESTROY_FAILURE";
    /// `CPM_WIZARD_VM_MISSING_ERROR`: a VM is gone.
    pub const VM_MISSING: &str = "XENADMIN_CPM_VM_MISSING";
}

fn client_error(code: &str, params: &[&str]) -> xapi::Error {
    xapi::Error::Api {
        code: code.to_owned(),
        params: params.iter().map(|p| (*p).to_owned()).collect(),
    }
}

/// Runs an F12a migration or move.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    target: Option<&Session>,
    request: &ActionRequest,
) -> TaskOutcome {
    match request {
        ActionRequest::MigrateSend(spec) => {
            // Within the pool the destination is this very connection.
            let target = if spec.target == connection {
                Some(session)
            } else {
                target
            };
            migrate_send(shared, op, session, target, spec).await
        }
        ActionRequest::MoveVm { vm, storage, .. } => {
            move_vm(shared, connection, op, session, vm, storage).await
        }
        _ => unreachable!("not a migration"),
    }
}

/// `VMCrossPoolMigrateAction.Run`.
async fn migrate_send(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    target: Option<&Session>,
    spec: &MigrateSpec,
) -> TaskOutcome {
    let Some(target) = target else {
        return TaskOutcome::Failed(client_error(errors::TARGET_DISCONNECTED, &[]));
    };
    shared.update_operation(op, |o| o.progress = Some(0.0));
    let none = HashMap::new();
    let dest = match xapi::host::migrate_receive(target, &spec.host, &spec.transfer_network, &none)
        .await
    {
        Ok(d) => d,
        Err(e) => return TaskOutcome::Failed(e),
    };
    shared.update_operation(op, |o| o.progress = Some(0.05));
    let vdi_map: HashMap<Ref<Vdi>, Ref<Sr>> = spec.vdi_map.iter().cloned().collect();
    let vif_map: HashMap<Ref<Vif>, Ref<Network>> = spec.vif_map.iter().cloned().collect();
    let options: HashMap<String, String> = if spec.copy {
        HashMap::from([("copy".to_owned(), "true".to_owned())])
    } else {
        HashMap::new()
    };
    let start = xapi::vm::async_migrate_send(
        session, &spec.vm, &dest, true, &vdi_map, &vif_map, &options, None,
    );
    run_task(shared, op, session, start, Span(0.05, 1.0), false).await
}

/// `VMMoveAction.Run`.
async fn move_vm(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    storage: &[(Ref<Vdi>, Ref<Sr>)],
) -> TaskOutcome {
    let s = session;
    let model = crate::storage_run::model(shared, connection);
    let Some(record) = model.vms.get(vm).cloned() else {
        return TaskOutcome::Failed(client_error(errors::VM_MISSING, &[]));
    };
    let map: HashMap<&Ref<Vdi>, &Ref<Sr>> = storage.iter().map(|(v, s)| (v, s)).collect();
    let sr_name = |r: &Ref<Sr>| {
        model
            .srs
            .get(r)
            .map(|x| x.name_label.clone())
            .unwrap_or_default()
    };
    let mut progress = 0.1;
    shared.update_operation(op, |o| o.progress = Some(progress));
    let half = 0.9 / (record.vbds.len().max(1) * 2) as f64;
    let mut failures = 0;
    for vbd_ref in &record.vbds {
        let Some(old) = model.vbds.get(vbd_ref) else {
            continue;
        };
        if !crate::provisioning::vbd_is_owner(old) {
            continue;
        }
        let Some(sr) = map.get(&old.vdi) else {
            continue;
        };
        let Some(current) = model.vdis.get(&old.vdi) else {
            continue;
        };
        if &current.sr == *sr {
            continue;
        }
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::MovingDisk {
                disk: current.name_label.clone(),
                from: sr_name(&current.sr),
                to: sr_name(sr),
            });
        });
        let start = xapi::vdi::async_copy(s, &old.vdi, Some(*sr), None, None);
        let copy: Ref<Vdi> =
            match run_task_result(shared, op, s, start, Span(progress, progress + half), false)
                .await
            {
                (TaskOutcome::Succeeded, r) if r.starts_with("OpaqueRef:") => Ref::new(&r),
                (TaskOutcome::Succeeded, r) => {
                    return TaskOutcome::Failed(xapi::Error::Protocol(format!(
                        "the copy returned no disk: {r:?}"
                    )));
                }
                (other, _) => return other,
            };
        let new_vbd = VbdRecord {
            vm: vm.clone(),
            vdi: copy,
            userdevice: old.userdevice.clone(),
            bootable: old.bootable,
            mode: old.mode.clone(),
            r#type: old.r#type.clone(),
            unpluggable: old.unpluggable,
            other_config: old.other_config.clone(),
            empty: false,
            ..VbdRecord::default()
        };
        if let Err(e) = xapi::vdi::destroy(s, &old.vdi).await {
            tracing::error!(error = %e, "failed to destroy the old disk of a moved VM");
            failures += 1;
        }
        if let Err(e) = xapi::vbd::create(s, &new_vbd).await {
            return TaskOutcome::Failed(e);
        }
        progress += 2.0 * half;
        shared.update_operation(op, |o| o.progress = Some(progress.min(1.0)));
    }
    shared.update_operation(op, |o| o.step = None);
    if let Some((_, sr)) = storage.first()
        && let Err(e) = xapi::vm::set_suspend_sr(s, vm, sr).await
    {
        return TaskOutcome::Failed(e);
    }
    if failures > 0 {
        return TaskOutcome::Failed(client_error(
            errors::MOVE_DESTROY_FAILED,
            &[&record.name_label],
        ));
    }
    TaskOutcome::Succeeded
}

/// Runs [`Precheck::Migrate`] with the source's `session` and the
/// destination's `target`.
pub(crate) fn run_precheck(
    shared: &Arc<Shared>,
    session: Session,
    target: Option<Session>,
    connection: ConnectionId,
    id: u64,
    check: Precheck,
) {
    let Precheck::Migrate {
        target: target_id,
        mode,
        checks,
    } = check
    else {
        return;
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        let mut out = Vec::new();
        for (vm, host) in checks {
            let verdict = match &target {
                Some(t) => {
                    can_migrate(
                        &shared, &session, t, connection, target_id, mode, &vm, &host,
                    )
                    .await
                }
                None => ServerVerdict::Refused {
                    error: Arc::new(client_error(errors::TARGET_DISCONNECTED, &[])),
                    boot: None,
                },
            };
            out.push((vm, host, verdict));
        }
        shared.notify(Notification::Precheck {
            connection,
            id,
            result: PrecheckResult::Migrate(out),
        });
    });
}

/// `CanDoStorageMigration`, then the in-pool fallback.
#[allow(clippy::too_many_arguments)]
async fn can_migrate(
    shared: &Shared,
    session: &Session,
    target: &Session,
    connection: ConnectionId,
    target_id: ConnectionId,
    mode: WizardMode,
    vm: &Ref<Vm>,
    host: &Ref<Host>,
) -> ServerVerdict {
    let source = crate::storage_run::model(shared, connection);
    let dest = crate::storage_run::model(shared, target_id);
    let same = connection == target_id;
    let Some(record) = source.vms.get(vm).cloned() else {
        return ServerVerdict::Refused {
            error: Arc::new(client_error(errors::VM_MISSING, &[])),
            boot: None,
        };
    };
    let storage = async {
        let network = migrate::management_network(&dest)
            .ok_or_else(|| client_error("HANDLE_INVALID", &["network"]))?;
        let none = HashMap::new();
        let receive = xapi::host::migrate_receive(target, host, &network, &none).await?;
        let vdi_map: HashMap<Ref<Vdi>, Ref<Sr>> = migrate::probe_vdi_map(&source, &record, &dest)
            .into_iter()
            .collect();
        let vif_map: HashMap<Ref<Vif>, Ref<Network>> =
            migrate::probe_vif_map(&source, &record, &dest, host, same)
                .into_iter()
                .collect();
        xapi::vm::assert_can_migrate(session, vm, &receive, true, &vdi_map, &vif_map, &none, None)
            .await
    }
    .await;
    let error = match storage {
        Ok(()) => return ServerVerdict::Ok,
        Err(e)
            if e.is_api("VIF_NOT_IN_MAP")
                && migrate::snapshots_have_extra_vifs(&source, &record) =>
        {
            return ServerVerdict::Ok;
        }
        Err(e) => e,
    };
    tracing::info!(error = %error, "VM cannot be migrated to that server");
    let mut boot = None;
    if mode == WizardMode::Migrate && same {
        match xapi::vm::assert_can_boot_here(session, vm, host).await {
            Ok(()) => return ServerVerdict::Ok,
            Err(e) => boot = Some(Arc::new(e)),
        }
    }
    ServerVerdict::Refused {
        error: Arc::new(error),
        boot,
    }
}
