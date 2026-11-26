//! How the disaster recovery operations and questions run (see
//! [`crate::dr`]), call for call as the original's actions:
//!
//! * **Configure** (`DRConfigureCommand`): `SR.disable_database_replication`
//!   for the SRs unticked, then `SR.enable_database_replication` for the
//!   SRs ticked.
//! * **`DrTaskCreateAction`**: `Async.DR_task.create(type, device_config,
//!   SR uuids)`; the task made is the operation's result.
//! * The wizard's clean-up: `DR_task.destroy`;
//!   **`ShutdownAndDestroyVMsAction`** (`VM.hard_shutdown` when running,
//!   paused or suspended, its snapshots destroyed first, drives and
//!   interfaces unplugged and destroyed, `VM.destroy`; disks kept);
//!   **`ShutdownAndDestroyVmAppliancesAction`**
//!   (`VM_appliance.hard_shutdown` when allowed, its VMs destroyed as above,
//!   `VM_appliance.destroy`).
//! * **`DrRecoverAction`** (after **`VdiOpenDatabaseAction`**):
//!   `VDI.open_database` gives a session on the metadata; on it
//!   `Async.VM.recover` / `Async.VM_appliance.recover` with this pool's
//!   session and `force`, followed as a task of this pool; the metadata
//!   session is logged out after.
//! * **`StartVMsAndAppliancesAction`**: each recovered VM started (or
//!   started paused), then each vApp.
//! * `HardShutDownApplianceAction`: `VM_appliance.hard_shutdown`.
//!
//! The questions (pre-checks): `SR.assert_supports_database_replication`
//! per SR; `SR.probe` with `metadata` (`SrProbeAction` of the Storage
//! page's scans); `GetMetadataVDIsAction` then `VdiLoadMetadataAction`
//! per metadata VDI (the pool, vApps and real VMs in it); and
//! `AssertCanBeRecoveredCheck` (`VM.assert_can_be_recovered`, and on
//! `VM_REQUIRES_SR` what the metadata says of the SRs required).

use std::collections::HashMap;

use xapi::{Ref, Session, Sr, Vdi, VdiType, Vm, VmAppliance, VmApplianceOperation, VmPowerState};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::dr::{DrItem, DrTaskSpec, MissingSr, PoolMetadata, Recoverable};
use crate::operations::{OperationStep, Span, TaskOutcome, run_task, run_task_result};
use crate::storage::DeviceConfig;

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return TaskOutcome::Failed(e),
        }
    };
}

/// Runs an F12b disaster recovery request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::ConfigureDr { enable, disable } => {
            let model = crate::storage_run::model(shared, connection);
            let count = (enable.len() + disable.len()).max(1) as f64;
            let mut done = 0.0;
            for (sr, on) in disable
                .iter()
                .map(|s| (s, false))
                .chain(enable.iter().map(|s| (s, true)))
            {
                let name = model
                    .srs
                    .get(sr)
                    .map(|x| x.name_label.clone())
                    .unwrap_or_default();
                shared.update_operation(op, |o| {
                    o.step = Some(if on {
                        OperationStep::EnablingDr { sr: name }
                    } else {
                        OperationStep::DisablingDr { sr: name }
                    });
                });
                if on {
                    tri!(xapi::sr::enable_database_replication(s, sr).await);
                } else {
                    tri!(xapi::sr::disable_database_replication(s, sr).await);
                }
                done += 1.0;
                shared.update_operation(op, |o| o.progress = Some(done / count));
            }
            TaskOutcome::Succeeded
        }
        ActionRequest::DrTaskCreate(spec) => create_task(shared, op, s, spec).await,
        ActionRequest::DrTaskDestroy { task, .. } => {
            tri!(xapi::dr_task::destroy(s, task).await);
            TaskOutcome::Succeeded
        }
        ActionRequest::DrRecover { vdi, item, .. } => recover(shared, op, s, vdi, item).await,
        ActionRequest::DrStart { vms, vapps, paused } => {
            start(shared, connection, op, s, vms, vapps, *paused).await
        }
        ActionRequest::DrDestroyVms { vms } => {
            let model = crate::storage_run::model(shared, connection);
            let count = vms.len().max(1) as f64;
            for (i, vm) in vms.iter().enumerate() {
                let running = model.vms.get(vm).is_some_and(|v| {
                    matches!(
                        v.power_state,
                        VmPowerState::Running | VmPowerState::Paused | VmPowerState::Suspended
                    )
                });
                if running {
                    tri!(xapi::vm::hard_shutdown(s, vm).await);
                }
                tri!(destroy_vm(s, &model, vm).await);
                let done = (i + 1) as f64 / count;
                shared.update_operation(op, |o| o.progress = Some(done));
            }
            TaskOutcome::Succeeded
        }
        ActionRequest::DrDestroyVApps { vapps } => {
            let model = crate::storage_run::model(shared, connection);
            let count = vapps.len().max(1) as f64;
            for (i, vapp) in vapps.iter().enumerate() {
                let Some(app) = model.vm_appliances.get(vapp) else {
                    continue;
                };
                if app
                    .allowed_operations
                    .contains(&VmApplianceOperation::HardShutdown)
                {
                    tri!(xapi::vm_appliance::hard_shutdown(s, vapp).await);
                }
                for vm in &app.vms {
                    tri!(destroy_vm(s, &model, vm).await);
                }
                match xapi::vm_appliance::destroy(s, vapp).await {
                    Ok(()) => {}
                    Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {}
                    Err(e) => return TaskOutcome::Failed(e),
                }
                let done = (i + 1) as f64 / count;
                shared.update_operation(op, |o| o.progress = Some(done));
            }
            TaskOutcome::Succeeded
        }
        ActionRequest::HardShutDownVApp { vapp } => {
            run_task(
                shared,
                op,
                s,
                xapi::vm_appliance::async_hard_shutdown(s, vapp),
                Span::FULL,
                false,
            )
            .await
        }
        _ => unreachable!("not a disaster recovery request"),
    }
}

/// `DrTaskCreateAction`.
async fn create_task(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    spec: &DrTaskSpec,
) -> TaskOutcome {
    let dconf = spec.device_config.map();
    let start = xapi::dr_task::async_create(s, &spec.sr_type, &dconf, &spec.srs);
    match run_task_result(shared, op, s, start, Span::FULL, false).await {
        (TaskOutcome::Succeeded, r) => {
            let task = crate::storage::task_string_result(&r);
            shared.update_operation(op, |o| o.result = Some(task));
            TaskOutcome::Succeeded
        }
        (other, _) => other,
    }
}

/// `ShutdownAndDestroyVMsAction.DestroyVM`: snapshots first, then drives
/// and interfaces, then the VM (an object already gone is fine).
async fn destroy_vm(s: &Session, model: &crate::ConnectionModel, vm: &Ref<Vm>) -> xapi::Result<()> {
    let Some(rec) = model.vms.get(vm) else {
        return Ok(());
    };
    for snap in &rec.snapshots {
        Box::pin(destroy_vm(s, model, snap)).await?;
    }
    for vbd in &rec.vbds {
        if let Some(b) = model.vbds.get(vbd) {
            if b.currently_attached && b.allowed_operations.contains(&xapi::VbdOperations::Unplug) {
                xapi::vbd::unplug(s, vbd).await?;
            } else if !b.currently_attached {
                gone_ok(xapi::vbd::destroy(s, vbd).await)?;
            }
        }
    }
    for vif in &rec.vifs {
        if let Some(v) = model.vifs.get(vif) {
            if v.currently_attached && v.allowed_operations.contains(&xapi::VifOperations::Unplug) {
                xapi::vif::unplug(s, vif).await?;
            } else if !v.currently_attached {
                gone_ok(xapi::vif::destroy(s, vif).await)?;
            }
        }
    }
    gone_ok(xapi::vm::destroy(s, vm).await)
}

fn gone_ok(r: xapi::Result<()>) -> xapi::Result<()> {
    match r {
        Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => Ok(()),
        other => other,
    }
}

/// A session on the metadata of `vdi` (`VdiOpenDatabaseAction`).
async fn open_database(s: &Session, vdi: &Ref<Vdi>) -> xapi::Result<Session> {
    let meta = xapi::vdi::open_database(s, vdi).await?;
    Ok(Session::from_ref(s.client().clone(), meta))
}

/// `DrRecoverAction`, with its own metadata session.
async fn recover(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    vdi: &Ref<Vdi>,
    item: &DrItem,
) -> TaskOutcome {
    let meta = tri!(open_database(s, vdi).await);
    let me = s.session_ref().clone();
    let outcome = match item {
        DrItem::Vm(vm) => {
            run_task(
                shared,
                op,
                s,
                xapi::vm::async_recover(&meta, vm, &me, true),
                Span::FULL,
                false,
            )
            .await
        }
        DrItem::VApp(app) => {
            run_task(
                shared,
                op,
                s,
                xapi::vm_appliance::async_recover(&meta, app, &me, true),
                Span::FULL,
                false,
            )
            .await
        }
    };
    if let Err(e) = meta.logout().await {
        tracing::debug!(error = %e, "metadata session logout failed");
    }
    outcome
}

/// `StartVMsAndAppliancesAction`.
async fn start(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vms: &[Ref<Vm>],
    vapps: &[Ref<VmAppliance>],
    paused: bool,
) -> TaskOutcome {
    if vms.is_empty() && vapps.is_empty() {
        return TaskOutcome::Failed(crate::storage::client_error(
            crate::dr::NOTHING_TO_START,
            &[],
        ));
    }
    let model = crate::storage_run::model(shared, connection);
    let count = (vms.len() + vapps.len()) as f64;
    let mut i = 0.0;
    for vm in vms {
        let name = model
            .vms
            .get(vm)
            .map(|v| v.name_label.clone())
            .unwrap_or_default();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::StartingRecoveredVm { vm: name });
        });
        let span = Span(i / count, (i + 1.0) / count);
        let outcome = run_task(
            shared,
            op,
            s,
            xapi::vm::async_start(s, vm, paused, false),
            span,
            false,
        )
        .await;
        if !matches!(outcome, TaskOutcome::Succeeded) {
            return outcome;
        }
        i += 1.0;
    }
    for vapp in vapps {
        let name = model
            .vm_appliances
            .get(vapp)
            .map(|a| a.name_label.clone())
            .unwrap_or_default();
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::StartingVApp { name });
        });
        let span = Span(i / count, (i + 1.0) / count);
        let outcome = run_task(
            shared,
            op,
            s,
            xapi::vm_appliance::async_start(s, vapp, paused),
            span,
            false,
        )
        .await;
        if !matches!(outcome, TaskOutcome::Succeeded) {
            return outcome;
        }
        i += 1.0;
    }
    TaskOutcome::Succeeded
}

// ---- questions --------------------------------------------------------------------------------

/// `SR.SupportsDatabaseReplication` for each SR.
pub(crate) async fn supported_srs(s: &Session, srs: Vec<Ref<Sr>>) -> Vec<(Ref<Sr>, bool)> {
    let mut out = Vec::new();
    for sr in srs {
        let ok = xapi::sr::assert_supports_database_replication(s, &sr)
            .await
            .is_ok();
        out.push((sr, ok));
    }
    out
}

/// `SrProbeAction` with `metadata = true` on the coordinator.
pub(crate) async fn probe_metadata(
    shared: &Shared,
    s: &Session,
    connection: ConnectionId,
    sr_type: &str,
    device_config: &DeviceConfig,
) -> xapi::Result<crate::storage::ProbeResult> {
    crate::storage_run::probe_with(
        shared,
        s,
        connection,
        sr_type,
        device_config,
        &HashMap::from([("metadata".to_owned(), "true".to_owned())]),
    )
    .await
}

/// `GetMetadataVDIsAction` on the chosen SRs (by uuid, read from the
/// server so SRs a `DR_task` just introduced are found), then
/// `VdiLoadMetadataAction` on each metadata VDI not in use.
pub(crate) async fn load_metadata(
    s: &Session,
    sr_uuids: &[String],
) -> xapi::Result<Vec<PoolMetadata>> {
    let mut out = Vec::new();
    for uuid in sr_uuids {
        let sr = match xapi::sr::get_by_uuid(s, uuid).await {
            Ok(sr) => sr,
            Err(e) if e.is_api("UUID_INVALID") => continue,
            Err(e) => return Err(e),
        };
        let rec = xapi::sr::get_record(s, &sr).await?;
        let iso = rec.content_type == "iso" || rec.r#type == "iso";
        if !rec.shared || rec.is_tools_sr || iso {
            continue;
        }
        for vdi in &rec.vdis {
            let Ok(v) = xapi::vdi::get_record(s, vdi).await else {
                continue;
            };
            if v.r#type != VdiType::Metadata || !v.metadata_latest {
                continue;
            }
            // In use: this pool's own, being written.
            let mut in_use = false;
            for vbd in &v.vbds {
                if xapi::vbd::get_currently_attached(s, vbd)
                    .await
                    .unwrap_or(false)
                {
                    in_use = true;
                }
            }
            if in_use {
                tracing::debug!("metadata VDI in use: not loaded");
                continue;
            }
            if let Some(meta) = load_pool_metadata(s, vdi, &v.name_label, &rec.name_label).await {
                out.push(meta);
            }
        }
    }
    Ok(out)
}

/// `VdiLoadMetadataAction`: `None` when the database could not be read
/// or holds no vApp and no VM.
async fn load_pool_metadata(
    s: &Session,
    vdi: &Ref<Vdi>,
    vdi_name: &str,
    sr_name: &str,
) -> Option<PoolMetadata> {
    let meta = match open_database(s, vdi).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(error = %e, "metadata could not be opened");
            return None;
        }
    };
    let read = async {
        let pools = xapi::pool::get_all_records(&meta).await.ok()?;
        let (_, pool) = pools.into_iter().next()?;
        let coordinator = if pool.name_label.is_empty() && !pool.master.is_null() {
            xapi::host::get_record(&meta, &pool.master).await.ok()
        } else {
            None
        };
        let pool_name = crate::dr::metadata_pool_name(&pool, coordinator.as_ref());
        let mut vm_appliances: Vec<_> = xapi::vm_appliance::get_all_records(&meta)
            .await
            .ok()?
            .into_iter()
            .collect();
        vm_appliances.sort_by(|a, b| crate::objects::natural_cmp(&a.1.name_label, &b.1.name_label));
        let mut vms: Vec<_> = xapi::vm::get_all_records(&meta)
            .await
            .ok()?
            .into_iter()
            .filter(|(_, v)| crate::vapp::is_real_vm(v))
            .collect();
        vms.sort_by(|a, b| crate::objects::natural_cmp(&a.1.name_label, &b.1.name_label));
        Some(PoolMetadata {
            vdi: vdi.clone(),
            vdi_name: vdi_name.to_owned(),
            sr_name: sr_name.to_owned(),
            pool,
            pool_name,
            vm_appliances,
            vms,
        })
    };
    let result = read.await;
    if let Err(e) = meta.logout().await {
        tracing::debug!(error = %e, "metadata session logout failed");
    }
    result.filter(|m| !m.vm_appliances.is_empty() || !m.vms.is_empty())
}

/// `AssertCanBeRecoveredCheck` for each item of one metadata VDI.
pub(crate) async fn recoverable(
    s: &Session,
    vdi: &Ref<Vdi>,
    items: Vec<DrItem>,
) -> xapi::Result<Vec<(DrItem, Recoverable)>> {
    let meta = open_database(s, vdi).await?;
    let me = s.session_ref().clone();
    let mut out = Vec::new();
    for item in items {
        let answer = match &item {
            DrItem::Vm(vm) => xapi::vm::assert_can_be_recovered(&meta, vm, &me).await,
            DrItem::VApp(app) => xapi::vm_appliance::assert_can_be_recovered(&meta, app, &me).await,
        };
        let verdict = match answer {
            Err(xapi::Error::Api { code, params })
                if code == "VM_REQUIRES_SR" && params.len() > 1 =>
            {
                let required = match &item {
                    DrItem::Vm(vm) => xapi::vm::get_srs_required_for_recovery(&meta, vm, &me).await,
                    DrItem::VApp(app) => {
                        xapi::vm_appliance::get_srs_required_for_recovery(&meta, app, &me).await
                    }
                }
                .unwrap_or_default();
                let mut infos = Vec::new();
                for sr in &required {
                    if let Some(info) = missing_sr(&meta, sr).await {
                        infos.push(info);
                    }
                }
                let named: Ref<Sr> = Ref::new(&params[1]);
                Recoverable::NeedsSrs {
                    sr: missing_sr(&meta, &named).await,
                    required: infos,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "assert_can_be_recovered failed");
                Recoverable::Yes
            }
            Ok(()) => Recoverable::Yes,
        };
        out.push((item, verdict));
    }
    if let Err(e) = meta.logout().await {
        tracing::debug!(error = %e, "metadata session logout failed");
    }
    Ok(out)
}

/// What the metadata says of an SR (`SR.get_record`, the first PBD's
/// `device_config`).
async fn missing_sr(meta: &Session, sr: &Ref<Sr>) -> Option<MissingSr> {
    let rec = xapi::sr::get_record(meta, sr).await.ok()?;
    let device_config = match rec.pbds.first() {
        Some(pbd) => xapi::pbd::get_device_config(meta, pbd).await.ok().map(|m| {
            let mut v: Vec<(String, String)> = m.into_iter().collect();
            v.sort();
            DeviceConfig(v)
        }),
        None => None,
    };
    Some(MissingSr {
        uuid: rec.uuid,
        name: rec.name_label,
        shared: rec.shared,
        sr_type: rec.r#type,
        device_config,
    })
}

/// The server's clock (`GetServerLocalTimeAction`).
pub(crate) async fn server_time(
    s: &Session,
    host: &Ref<xapi::Host>,
) -> xapi::Result<crate::vmss::ServerTime> {
    let utc = xapi::host::get_servertime(s, host).await?;
    let local = xapi::host::get_server_localtime(s, host).await?;
    let parse = |d: &xapi::DateTime| {
        d.unix_seconds().ok_or_else(|| {
            xapi::Error::Protocol(format!("unreadable server time {:?}", d.as_str()))
        })
    };
    Ok(crate::vmss::ServerTime::new(parse(&utc)?, parse(&local)?))
}
