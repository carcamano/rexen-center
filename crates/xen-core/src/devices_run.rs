//! How the F14a requests run ([`crate::devices::DeviceRequest`]), call for
//! call as the original's actions; the long calls run as tasks followed
//! to their end (the original left `VGPU.async_create`'s tasks unfollowed).

use xapi::{AllocationAlgorithm, Ref, Session, Vm, VmOperations, VmPowerState, VusbOperations};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::devices::{DeviceRequest, DeviceRestriction, UPSTREAM_QEMU, forbidden};
use crate::memory::MemoryChange;
use crate::operations::{Span, TaskOutcome, run_task};
use crate::storage_run::model;

/// Runs an F14a request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let ActionRequest::Device(request) = request else {
        unreachable!("not a device request")
    };
    let s = session;
    let failed = TaskOutcome::Failed;
    let progress = |p: f64| shared.update_operation(op, |o| o.progress = Some(p));
    match &**request {
        DeviceRequest::SetAllowedVgpuTypes { pgpus } => {
            let n = pgpus.len().max(1) as f64;
            for (i, (pgpu, types)) in pgpus.iter().enumerate() {
                if let Err(e) = xapi::pgpu::set_enabled_vgpu_types(s, pgpu, types).await {
                    return failed(e);
                }
                progress((i + 1) as f64 / n);
            }
            TaskOutcome::Succeeded
        }
        DeviceRequest::SetPlacementPolicy { algorithm } => {
            set_placement(shared, connection, op, s, algorithm).await
        }
        DeviceRequest::SetIntegratedGpu { host, enable } => {
            let start = async {
                if *enable {
                    xapi::host::async_enable_display(s, host).await
                } else {
                    xapi::host::async_disable_display(s, host).await
                }
            };
            let outcome = run_task(shared, op, s, start, Span(0.0, 0.5), false).await;
            if !matches!(outcome, TaskOutcome::Succeeded) {
                return outcome;
            }
            let m = model(shared, connection);
            let Some((pgpu, _)) = crate::devices::system_display_device(&m, host) else {
                return TaskOutcome::Succeeded;
            };
            let pgpu = pgpu.clone();
            let start = async {
                if *enable {
                    xapi::pgpu::async_enable_dom0_access(s, &pgpu).await
                } else {
                    xapi::pgpu::async_disable_dom0_access(s, &pgpu).await
                }
            };
            run_task(shared, op, s, start, Span(0.5, 1.0), false).await
        }
        DeviceRequest::AssignVgpus { vm, keep, add } => {
            assign_vgpus(shared, connection, op, s, vm, keep, add).await
        }
        DeviceRequest::SetUsbPassthrough { pusb, enabled } => {
            match xapi::pusb::set_passthrough_enabled(s, pusb, *enabled).await {
                Ok(()) => TaskOutcome::Succeeded,
                Err(e) => failed(e),
            }
        }
        DeviceRequest::AttachUsb { vm, pusb, .. } => {
            let m = model(shared, connection);
            let (Some(vm_rec), Some(p)) = (m.get(vm), m.get(pusb)) else {
                return failed(gone("PUSB", pusb.as_str()));
            };
            if !crate::devices::uses_upstream_qemu(vm_rec) {
                let mut platform = vm_rec.platform.clone();
                platform.insert("device-model".into(), UPSTREAM_QEMU.into());
                if let Err(e) = xapi::vm::set_platform(s, vm, &platform).await {
                    return failed(e);
                }
                progress(0.5);
            }
            let empty = std::collections::HashMap::new();
            match xapi::vusb::create(s, vm, &p.usb_group, Some(&empty)).await {
                Ok(_) => TaskOutcome::Succeeded,
                Err(e) => failed(e),
            }
        }
        DeviceRequest::DetachUsb { vusb, .. } => {
            let m = model(shared, connection);
            let attached = m.get(vusb).is_some_and(|v| v.currently_attached);
            let mut unplug_error = None;
            if attached {
                match xapi::vusb::get_allowed_operations(s, vusb).await {
                    Ok(ops) if ops.contains(&VusbOperations::Unplug) => {
                        let start = xapi::vusb::async_unplug(s, vusb);
                        if let TaskOutcome::Failed(e) =
                            run_task(shared, op, s, start, Span(0.0, 0.5), false).await
                        {
                            unplug_error = Some(e);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => unplug_error = Some(e),
                }
            }
            // Destroyed even when the unplug failed, as the original's
            // `finally`; the unplug's error is the one reported.
            let start = xapi::vusb::async_destroy(s, vusb);
            let outcome = run_task(shared, op, s, start, Span(0.5, 1.0), false).await;
            match unplug_error {
                Some(e) => failed(e),
                None => outcome,
            }
        }
        DeviceRequest::AddVtpm { vm } => match xapi::vtpm::create(s, vm, false).await {
            Ok(_) => TaskOutcome::Succeeded,
            Err(e) => failed(e),
        },
        DeviceRequest::RemoveVtpm { vtpm, .. } => match xapi::vtpm::destroy(s, vtpm).await {
            Ok(()) => TaskOutcome::Succeeded,
            Err(e) => failed(e),
        },
        DeviceRequest::SetMemory(change) => set_memory(shared, connection, op, s, change).await,
        DeviceRequest::EnableRdp { vm } => {
            let args = std::collections::HashMap::new();
            match xapi::vm::call_plugin(s, vm, "guest-agent-operation", "request-rdp-on", &args)
                .await
            {
                Ok(_) => TaskOutcome::Succeeded,
                Err(e) => failed(e),
            }
        }
    }
}

fn gone(class: &str, r: &str) -> xapi::Error {
    xapi::Error::Api {
        code: "HANDLE_INVALID".into(),
        params: vec![class.into(), r.into()],
    }
}

/// `SetGpuPlacementPolicyAction`: every GPU group.
async fn set_placement(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    algorithm: &AllocationAlgorithm,
) -> TaskOutcome {
    let m = model(shared, connection);
    let mut groups: Vec<_> = m.gpu_groups.keys().cloned().collect();
    groups.sort();
    let n = groups.len().max(1) as f64;
    for (i, g) in groups.iter().enumerate() {
        if let Err(e) = xapi::gpu_group::set_allocation_algorithm(s, g, algorithm).await {
            return TaskOutcome::Failed(e);
        }
        shared.update_operation(op, |o| o.progress = Some((i + 1) as f64 / n));
    }
    TaskOutcome::Succeeded
}

/// `GpuAssignAction`: the removed vGPUs destroyed, then the new ones
/// created on device "0" (the first free slot), each task followed.
async fn assign_vgpus(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vm: &Ref<Vm>,
    keep: &[Ref<xapi::Vgpu>],
    add: &[(Ref<xapi::GpuGroup>, Option<Ref<xapi::VgpuType>>)],
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(rec) = m.get(vm) else {
        return TaskOutcome::Failed(gone("VM", vm.as_str()));
    };
    let remove: Vec<_> = rec
        .vgpus
        .iter()
        .filter(|v| m.vgpus.contains_key(*v) && !keep.contains(v))
        .cloned()
        .collect();
    let vgpu_restricted = forbidden(&m, DeviceRestriction::Vgpu);
    let steps = (remove.len() + add.len()).max(1) as f64;
    let mut done = 0.0;
    for v in &remove {
        if let Err(e) = xapi::vgpu::destroy(s, v).await {
            return TaskOutcome::Failed(e);
        }
        done += 1.0;
        shared.update_operation(op, |o| o.progress = Some(done / steps));
    }
    let empty = std::collections::HashMap::new();
    for (group, vgpu_type) in add {
        if !m.gpu_groups.contains_key(group) {
            done += 1.0;
            continue;
        }
        let vgpu_type = vgpu_type
            .as_ref()
            .filter(|t| !vgpu_restricted && m.vgpu_types.contains_key(*t));
        let start = xapi::vgpu::async_create(s, vm, group, Some("0"), Some(&empty), vgpu_type);
        let span = Span(done / steps, (done + 1.0) / steps);
        let outcome = run_task(shared, op, s, start, span, false).await;
        if !matches!(outcome, TaskOutcome::Succeeded) {
            return outcome;
        }
        done += 1.0;
    }
    TaskOutcome::Succeeded
}

/// `ChangeMemorySettingsAction`.
async fn set_memory(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    c: &MemoryChange,
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(rec) = m.get(&c.vm) else {
        return TaskOutcome::Failed(gone("VM", c.vm.as_str()));
    };
    let home = crate::objects::vm_home(&m, rec).cloned();
    let static_changed = c.static_changed(rec);
    let reboot = c.needs_reboot(rec);
    if reboot {
        crate::storage_run::step(shared, op, crate::operations::OperationStep::MemoryShutdown);
        let clean = rec
            .allowed_operations
            .contains(&VmOperations::CleanShutdown);
        let start = async {
            if clean {
                xapi::vm::async_clean_shutdown(s, &c.vm).await
            } else {
                xapi::vm::async_hard_shutdown(s, &c.vm).await
            }
        };
        if let TaskOutcome::Failed(e) = run_task(shared, op, s, start, Span(0.0, 0.4), false).await
        {
            // Fine if it is down already or going down.
            let m = model(shared, connection);
            let going = m.get(&c.vm).is_some_and(|v| {
                v.power_state == VmPowerState::Halted
                    || v.current_operations.values().any(|o| {
                        matches!(
                            o,
                            VmOperations::CleanShutdown
                                | VmOperations::HardShutdown
                                | VmOperations::Shutdown
                        )
                    })
            });
            if !going {
                return TaskOutcome::Failed(e);
            }
        }
        if !wait_halted(shared, connection, &c.vm).await {
            return TaskOutcome::Failed(xapi::Error::Api {
                code: "VM_BAD_POWER_STATE".into(),
                params: vec![c.vm.to_string(), "halted".into(), "running".into()],
            });
        }
    }
    crate::storage_run::step(shared, op, crate::operations::OperationStep::MemorySet);
    let set = if static_changed {
        xapi::vm::set_memory_limits(
            s,
            &c.vm,
            c.static_min,
            c.static_max,
            c.dynamic_min,
            c.dynamic_max,
        )
        .await
    } else {
        xapi::vm::set_memory_dynamic_range(s, &c.vm, c.dynamic_min, c.dynamic_max).await
    };
    shared.update_operation(op, |o| o.progress = Some(if reboot { 0.5 } else { 1.0 }));
    if reboot {
        // Started again even when the change failed (`finally`).
        crate::storage_run::step(shared, op, crate::operations::OperationStep::MemoryStart);
        let start = async {
            match &home {
                Some(h) => xapi::vm::async_start_on(s, &c.vm, h, false, false).await,
                None => xapi::vm::async_start(s, &c.vm, false, false).await,
            }
        };
        let started = run_task(shared, op, s, start, Span(0.5, 1.0), false).await;
        if let Err(e) = set {
            return TaskOutcome::Failed(e);
        }
        return started;
    }
    match set {
        Ok(()) => TaskOutcome::Succeeded,
        Err(e) => TaskOutcome::Failed(e),
    }
}

/// Waits until the cache shows the VM halted (cancellable by the server
/// going away: gives up after the join wait).
async fn wait_halted(shared: &Shared, connection: ConnectionId, vm: &Ref<Vm>) -> bool {
    let deadline = tokio::time::Instant::now() + shared.options.join_wait;
    loop {
        let m = model(shared, connection);
        match m.get(vm) {
            Some(v) if v.power_state == VmPowerState::Halted => return true,
            None => return false,
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(shared.options.task_poll_interval).await;
    }
}
