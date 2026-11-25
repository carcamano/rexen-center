//! The VM creation, template, snapshot and disk operations (see
//! [`crate::provisioning`] for the requests and when each is offered).
//!
//! Each follows the original console's action, call for call
//! (`CreateVMAction`, `VMCloneAction` / `VMCopyAction`,
//! `VMToTemplateAction`, `VMDestroyAction`, `VMSnapshotCreateAction`,
//! `VMSnapshotRevertAction`, `VMSnapshotDeleteAction`, `CreateDiskAction`
//! with `VbdCreateAndPlugAction`, `AttachDiskDialog`,
//! `DetachVirtualDiskAction`, `DestroyDiskAction`, the disk's Size and
//! Location page, `MoveVirtualDiskAction` / `MigrateVirtualDiskAction`,
//! `ChangeVMISOAction`, `CreateCdDriveAction`, `InstallPVToolsAction`):
//! the long steps run as `Async.*` tasks (progress and Cancel in the
//! Events panel), the field changes as plain calls.

use std::collections::HashMap;

use xapi::{
    Ref, Session, Sr, Vbd, VbdMode, VbdOperations, VbdRecord, VbdType, Vdi, VdiRecord, VdiType, Vm,
    VmPowerState,
};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::model::ConnectionModel;
use crate::operations::{
    OperationNotice, OperationStep, Span, TaskOutcome, run_task, run_task_result,
};
use crate::provisioning::{
    self, BootMode, CopyMode, DiskSource, DiskSpec, InstallMedia, NewVmSpec,
};

/// The prefix of the hidden name a new VM has until it is ready
/// (`Helpers.MakeHiddenName`).
pub const HIDDEN_PREFIX: &str = "__gui__";

/// Runs an F7 request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::NewVm(spec) => create_vm(shared, connection, op, s, spec).await,
        ActionRequest::CopyVm {
            vm,
            name,
            description,
            mode,
        } => copy_vm(shared, op, s, vm, name, description, mode).await,
        ActionRequest::TemplateFromSnapshot {
            snapshot,
            name,
            description,
        } => {
            let outcome =
                copy_vm(shared, op, s, snapshot, name, description, &CopyMode::Clone).await;
            if !matches!(outcome, TaskOutcome::Succeeded) {
                return outcome;
            }
            let Some(new) = created(shared, op) else {
                return outcome;
            };
            set_instant(s, &new).await
        }
        ActionRequest::MakeTemplate { vm } => {
            if let TaskOutcome::Failed(e) = set_instant(s, vm).await {
                return TaskOutcome::Failed(e);
            }
            match xapi::vm::set_is_a_template(s, vm, true).await {
                Ok(()) => TaskOutcome::Succeeded,
                Err(e) => TaskOutcome::Failed(e),
            }
        }
        ActionRequest::DeleteVm {
            vm,
            disks,
            snapshots,
        } => delete_vm(shared, connection, op, s, vm, disks, snapshots).await,
        ActionRequest::Snapshot {
            vm,
            name,
            description,
            memory,
        } => take_snapshot(shared, op, s, vm, name, description, *memory).await,
        ActionRequest::Revert {
            snapshot,
            snapshot_first,
        } => {
            if let Some((name, description)) = snapshot_first {
                let vm = match xapi::vm::get_snapshot_of(s, snapshot).await {
                    Ok(vm) => vm,
                    Err(e) => return TaskOutcome::Failed(e),
                };
                let taken = take_snapshot(shared, op, s, &vm, name, description, false).await;
                if !matches!(taken, TaskOutcome::Succeeded) {
                    return taken;
                }
                // The new snapshot is not what the revert made.
                shared.update_operation(op, |o| o.created = None);
            }
            revert(shared, connection, op, s, snapshot).await
        }
        ActionRequest::DeleteSnapshots { snapshots } => {
            let count = snapshots.len().max(1) as f64;
            let mut first_error = None;
            for (i, snapshot) in snapshots.iter().enumerate() {
                let span = Span(i as f64 / count, (i + 1) as f64 / count);
                match delete_snapshot(shared, op, s, snapshot, span).await {
                    Ok(()) => {}
                    Err(TaskOutcome::Cancelled) => return TaskOutcome::Cancelled,
                    Err(o) => {
                        first_error.get_or_insert(o);
                    }
                }
            }
            first_error.unwrap_or(TaskOutcome::Succeeded)
        }
        ActionRequest::NewDisk { vm, disk } => new_disk(shared, op, s, vm, disk).await,
        ActionRequest::AttachDisk { vm, vdi, read_only } => {
            attach_disk(shared, connection, op, s, vm, vdi, *read_only).await
        }
        ActionRequest::DetachDisk { vbd, .. } => detach(shared, op, s, vbd, Span::FULL).await,
        ActionRequest::DeleteDisk { vdi } => delete_disk(shared, op, s, vdi).await,
        ActionRequest::ResizeDisk { vdi, size } => resize_disk(shared, op, s, vdi, *size).await,
        ActionRequest::MoveDisk { vdi, sr } => move_disk(shared, connection, op, s, vdi, sr).await,
        ActionRequest::ChangeCd { vbd, iso, .. } => {
            change_cd(shared, op, s, vbd, iso.as_ref()).await
        }
        ActionRequest::NewCdDrive { vm } => new_cd_drive(shared, op, s, vm).await,
        ActionRequest::InstallTools { vm } => install_tools(shared, connection, op, s, vm).await,
        ActionRequest::Vm { .. }
        | ActionRequest::Host { .. }
        | ActionRequest::DismissAlerts { .. }
        | ActionRequest::SetPerfmon { .. }
        | ActionRequest::NewSr(_)
        | ActionRequest::Sr { .. }
        | ActionRequest::SetSrProperties { .. }
        | ActionRequest::NewNetwork(_)
        | ActionRequest::NewBond(_)
        | ActionRequest::RemoveNetwork { .. }
        | ActionRequest::DestroyBond { .. }
        | ActionRequest::SetNetworkProperties { .. }
        | ActionRequest::RescanPifs { .. }
        | ActionRequest::ChangeNetworking(_)
        | ActionRequest::RenameInterfaces { .. }
        | ActionRequest::NewVif { .. }
        | ActionRequest::DeleteVif { .. }
        | ActionRequest::UpdateVif { .. }
        | ActionRequest::PlugVif { .. }
        | ActionRequest::UnplugVif { .. }
        | ActionRequest::DestroyPool { .. }
        | ActionRequest::EjectHost { .. }
        | ActionRequest::EvacuateCoordinator { .. }
        | ActionRequest::SetProperties { .. }
        | ActionRequest::SetDom0Memory { .. }
        | ActionRequest::ChangePassword { .. }
        | ActionRequest::RotatePoolSecret
        | ActionRequest::EnableTlsVerification
        | ActionRequest::InstallCertificate { .. }
        | ActionRequest::ResetCertificate { .. }
        | ActionRequest::BackupHost { .. }
        | ActionRequest::RestoreHost { .. } => {
            unreachable!("run by crate::operations")
        }
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

fn created(shared: &Shared, op: OperationId) -> Option<Ref<Vm>> {
    shared.state().operation(op).and_then(|o| o.created.clone())
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

/// Runs a task to its end.
async fn task(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl std::future::Future<Output = xapi::Result<Ref<xapi::Task>>>,
    span: Span,
) -> Result<(), TaskOutcome> {
    done(run_task(shared, op, session, start, span, false).await)
}

/// `SetVMOtherConfigAction(vm, "instant", "true")`.
async fn set_instant(s: &Session, vm: &Ref<Vm>) -> TaskOutcome {
    let result = async {
        xapi::vm::remove_from_other_config(s, vm, "instant").await?;
        xapi::vm::add_to_other_config(s, vm, "instant", "true").await
    }
    .await;
    match result {
        Ok(()) => TaskOutcome::Succeeded,
        Err(e) => TaskOutcome::Failed(e),
    }
}

/// The record of a new VDI (`VDI.create` takes a whole record).
fn vdi_record(
    name: &str,
    description: &str,
    sr: &Ref<Sr>,
    size: i64,
    kind: VdiType,
    vmhint: &str,
) -> VdiRecord {
    VdiRecord {
        name_label: name.to_owned(),
        name_description: description.to_owned(),
        sr: sr.clone(),
        virtual_size: size,
        r#type: kind,
        sharable: false,
        read_only: false,
        sm_config: HashMap::from([("vmhint".to_owned(), vmhint.to_owned())]),
        // XAPI rejects empty enum and date values in records
        // ("Unknown tag/contents").
        on_boot: xapi::OnBoot::Persist,
        snapshot_time: xapi::DateTime(NEVER.to_owned()),
        ..Default::default()
    }
}

/// XAPI's "never" date.
const NEVER: &str = "19700101T00:00:00Z";

/// The record of a new VIF (`""` MAC: generated by the server).
fn vif_record(
    vm: &Ref<Vm>,
    network: &Ref<xapi::Network>,
    device: String,
    mac: &str,
    mtu: i64,
) -> xapi::VifRecord {
    xapi::VifRecord {
        device,
        network: network.clone(),
        vm: vm.clone(),
        mac: mac.to_owned(),
        mtu,
        // Empty enum values are refused by XAPI.
        locking_mode: xapi::VifLockingMode::NetworkDefault,
        ipv4_configuration_mode: xapi::VifIpv4ConfigurationMode::None,
        ipv6_configuration_mode: xapi::VifIpv6ConfigurationMode::None,
        ..Default::default()
    }
}

/// The record of a new VBD (`other_config["owner"]` when the VM owns the
/// disk).
fn vbd_record(
    vm: &Ref<Vm>,
    vdi: Option<&Ref<Vdi>>,
    userdevice: &str,
    kind: VbdType,
    read_only: bool,
    bootable: bool,
    owner: bool,
) -> VbdRecord {
    let mut other_config = HashMap::new();
    if owner {
        other_config.insert("owner".to_owned(), "true".to_owned());
    }
    VbdRecord {
        vm: vm.clone(),
        vdi: vdi.cloned().unwrap_or_else(Ref::null),
        userdevice: userdevice.to_owned(),
        bootable,
        mode: if read_only || kind == VbdType::Cd {
            VbdMode::Ro
        } else {
            VbdMode::Rw
        },
        r#type: kind,
        unpluggable: true,
        empty: vdi.is_none(),
        other_config,
        ..Default::default()
    }
}

/// `VM.get_allowed_VBD_devices`: `wanted` if free, else the first free
/// position.
async fn position(session: &Session, vm: &Ref<Vm>, wanted: Option<&str>) -> xapi::Result<String> {
    let mut allowed = xapi::vm::get_allowed_vbd_devices(session, vm).await?;
    if let Some(w) = wanted
        && allowed.iter().any(|a| a == w)
    {
        return Ok(w.to_owned());
    }
    allowed.sort_by(|a, b| crate::objects::natural_cmp(a, b));
    allowed.into_iter().next().ok_or_else(|| xapi::Error::Api {
        code: "VM_TOO_MANY_VBDS".into(),
        params: vec![vm.to_string()],
    })
}

/// `VbdCreateAndPlugAction`: `VBD.create`, then `VBD.async_plug` when the
/// drive allows it; when it does not and the VM is not halted, the user
/// must restart the VM (`notice`).
async fn create_and_plug(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    record: &VbdRecord,
    notice: OperationNotice,
    span: Span,
) -> Result<Ref<Vbd>, TaskOutcome> {
    let s = session;
    let vbd = tri!(xapi::vbd::create(s, record).await);
    let ops = tri!(xapi::vbd::get_allowed_operations(s, &vbd).await);
    if ops.contains(&VbdOperations::Plug) {
        task(shared, op, s, xapi::vbd::async_plug(s, &vbd), span).await?;
    } else if tri!(xapi::vm::get_power_state(s, &record.vm).await) != VmPowerState::Halted {
        shared.update_operation(op, |o| o.notice = Some(notice));
    }
    Ok(vbd)
}

// ---- New VM -------------------------------------------------------------------

/// `CreateVMAction`. Until the networks are made, a failure destroys what
/// was made (the VM and the disks it owns); after that the VM stays.
async fn create_vm(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    spec: &NewVmSpec,
) -> TaskOutcome {
    let m = model(shared, connection);
    let built = build_vm(shared, &m, op, session, spec).await;
    match built {
        Ok(vm) => outcome(finish_vm(shared, op, session, spec, &vm).await),
        Err((Some(vm), failure)) => {
            step(shared, op, OperationStep::RollingBack);
            if let Err(e) = destroy_with_owned_disks(session, &vm).await {
                tracing::warn!(error = %e, "could not remove the half-made VM");
            }
            shared.update_operation(op, |o| o.created = None);
            failure
        }
        Err((None, failure)) => failure,
    }
}

/// Everything up to the point of no return: the VM made, its settings,
/// DVD drive, disks and networks.
async fn build_vm(
    shared: &Shared,
    m: &ConnectionModel,
    op: OperationId,
    session: &Session,
    spec: &NewVmSpec,
) -> Result<Ref<Vm>, (Option<Ref<Vm>>, TaskOutcome)> {
    let s = session;
    let missing = || {
        TaskOutcome::Failed(xapi::Error::Api {
            code: "HANDLE_INVALID".into(),
            params: vec!["VM".into(), spec.template.to_string()],
        })
    };
    let template = m
        .vms
        .get(&spec.template)
        .cloned()
        .ok_or_else(|| (None, missing()))?;

    // 1. Clone (or copy) under a hidden name.
    let hidden = format!("{HIDDEN_PREFIX}{}", spec.name);
    step(
        shared,
        op,
        OperationStep::CloningTemplate {
            template: template.name_label.clone(),
            sr: spec
                .full_copy_sr
                .as_ref()
                .and_then(|r| m.srs.get(r))
                .map(|sr| sr.name_label.clone()),
        },
    );
    let start = async {
        match &spec.full_copy_sr {
            Some(sr) => xapi::vm::async_copy(s, &spec.template, &hidden, sr).await,
            None => xapi::vm::async_clone(s, &spec.template, &hidden).await,
        }
    };
    let vm: Ref<Vm> = made(shared, op, s, start, Span(0.0, 0.1))
        .await
        .map_err(|o| (None, o))?;
    shared.update_operation(op, |o| o.created = Some(vm.clone()));
    let fail = |o: TaskOutcome| (Some(vm.clone()), o);
    configure_vm(shared, m, op, s, spec, &template, &vm)
        .await
        .map_err(fail)?;
    Ok(vm)
}

async fn configure_vm(
    shared: &Shared,
    m: &ConnectionModel,
    op: OperationId,
    s: &Session,
    spec: &NewVmSpec,
    template: &xapi::VmRecord,
    vm: &Ref<Vm>,
) -> Result<(), TaskOutcome> {
    // 2. The vendor device the template recommends.
    let vendor =
        provisioning::recommendation(template, "has-vendor-device").as_deref() == Some("true");
    let no_vendor_policy = m.pool().is_some_and(|(_, p)| p.policy_no_vendor_device);
    if vendor && !no_vendor_policy {
        tri!(xapi::vm::set_has_vendor_device(s, vm, true).await);
    }

    // 3. Provision with the disks removed from the instructions: every disk
    // is made below, with the sizes and SRs chosen.
    step(shared, op, OperationStep::Provisioning);
    let mut other_config = tri!(xapi::vm::get_other_config(s, vm).await);
    if other_config.remove("disks").is_some() {
        tri!(xapi::vm::set_other_config(s, vm, &other_config).await);
    }
    task(
        shared,
        op,
        s,
        xapi::vm::async_provision(s, vm),
        Span(0.1, 0.6),
    )
    .await?;
    // XAPI's provision makes it a VM; make sure of it.
    if tri!(xapi::vm::get_is_a_template(s, vm).await) {
        tri!(xapi::vm::set_is_a_template(s, vm, false).await);
    }

    // 4. Name, description, vCPUs (keeping at_startup ≤ max), topology,
    // home server, memory.
    step(shared, op, OperationStep::SettingProperties);
    tri!(xapi::vm::set_name_label(s, vm, &spec.name).await);
    tri!(xapi::vm::set_name_description(s, vm, &spec.description).await);
    let current = tri!(xapi::vm::get_record(s, vm).await);
    if current.vcpus_at_startup > spec.vcpus_at_startup {
        tri!(xapi::vm::set_vcpus_at_startup(s, vm, spec.vcpus_at_startup).await);
        tri!(xapi::vm::set_vcpus_max(s, vm, spec.vcpus_max).await);
    } else {
        tri!(xapi::vm::set_vcpus_max(s, vm, spec.vcpus_max).await);
        tri!(xapi::vm::set_vcpus_at_startup(s, vm, spec.vcpus_at_startup).await);
    }
    let mut platform = current.platform.clone();
    if let Some(cores) = spec.cores_per_socket {
        platform.insert("cores-per-socket".into(), cores.to_string());
        tri!(xapi::vm::set_platform(s, vm, &platform).await);
    }
    let affinity = spec.affinity.clone().unwrap_or_else(Ref::null);
    if affinity != template.affinity {
        tri!(xapi::vm::set_affinity(s, vm, &affinity).await);
    }
    if spec.memory_dynamic_min != template.memory_dynamic_min
        || spec.memory_dynamic_max != template.memory_dynamic_max
        || spec.memory_static_max != template.memory_static_max
    {
        tri!(
            xapi::vm::set_memory_limits(
                s,
                vm,
                template.memory_static_min,
                spec.memory_static_max,
                spec.memory_dynamic_min,
                spec.memory_dynamic_max,
            )
            .await
        );
    }

    // 5. Boot: the network first when it installs from there (or has no
    // disk); the firmware and Secure Boot.
    if provisioning::is_hvm(template) {
        let mut params = tri!(xapi::vm::get_hvm_boot_params(s, vm).await);
        if spec.disks.is_empty() || spec.media == InstallMedia::Network {
            let order = params.get("order").cloned().unwrap_or_default();
            params.insert("order".into(), network_first(&order));
        }
        if let Some(mode) = spec.boot_mode {
            params.insert(
                "firmware".into(),
                if mode == BootMode::Bios {
                    "bios"
                } else {
                    "uefi"
                }
                .into(),
            );
        }
        tri!(xapi::vm::set_hvm_boot_params(s, vm, &params).await);
        if let Some(mode) = spec.boot_mode {
            let mut platform = tri!(xapi::vm::get_platform(s, vm).await);
            platform.insert(
                "secureboot".into(),
                (mode == BootMode::UefiSecure).to_string(),
            );
            tri!(xapi::vm::set_platform(s, vm, &platform).await);
        }
    }

    // 6. The DVD drive (not for custom templates without one).
    let custom = !template.other_config.contains_key("default_template");
    let template_has_cd = template
        .vbds
        .iter()
        .filter_map(|b| m.get(b))
        .any(|b| b.r#type == VbdType::Cd);
    if !(custom && !template_has_cd) {
        step(shared, op, OperationStep::CreatingCdDrive);
        let mut cd = None;
        for vbd in tri!(xapi::vm::get_vbds(s, vm).await) {
            let rec = tri!(xapi::vbd::get_record(s, &vbd).await);
            if rec.r#type == VbdType::Cd && ["0", "1", "2", "3"].contains(&rec.userdevice.as_str())
            {
                cd = Some((vbd, rec.empty));
                break;
            }
        }
        let (cd, empty) = match cd {
            Some(found) => found,
            None => {
                let at = tri!(position(s, vm, Some("3")).await);
                let iso = matches!(spec.media, InstallMedia::Iso(_));
                let record = vbd_record(vm, None, &at, VbdType::Cd, true, iso, false);
                let vbd = made(
                    shared,
                    op,
                    s,
                    xapi::vbd::async_create(s, &record),
                    Span(0.6, 0.65),
                )
                .await?;
                (vbd, true)
            }
        };
        if !empty {
            task(
                shared,
                op,
                s,
                xapi::vbd::async_eject(s, &cd),
                Span(0.65, 0.67),
            )
            .await?;
        }
        if let InstallMedia::Iso(iso) = &spec.media {
            task(
                shared,
                op,
                s,
                xapi::vbd::async_insert(s, &cd, iso),
                Span(0.67, 0.7),
            )
            .await?;
        }
    }

    // 7. Disks.
    step(shared, op, OperationStep::CreatingDisks);
    let vbds = tri!(xapi::vm::get_vbds(s, vm).await);
    let mut by_position: HashMap<String, (Ref<Vbd>, VbdRecord)> = HashMap::new();
    for vbd in vbds {
        let rec = tri!(xapi::vbd::get_record(s, &vbd).await);
        if rec.r#type != VbdType::Cd {
            by_position.insert(rec.userdevice.clone(), (vbd, rec));
        }
    }
    let count = spec.disks.len().max(1) as f64;
    let mut suspend_sr = None;
    for (i, disk) in spec.disks.iter().enumerate() {
        let span = Span(
            0.7 + 0.2 * i as f64 / count,
            0.7 + 0.2 * (i + 1) as f64 / count,
        );
        suspend_sr.get_or_insert_with(|| disk.sr.clone());
        let bootable_new = disk.userdevice == "0" && !matches!(spec.media, InstallMedia::Iso(_));
        let vdi = match by_position.get(&disk.userdevice) {
            // A cloned disk already where it should be.
            Some((_, rec)) if tri!(xapi::vdi::get_sr(s, &rec.vdi).await) == disk.sr => {
                rec.vdi.clone()
            }
            // A cloned disk on another SR: copied there.
            Some((vbd, rec)) => {
                let new: Ref<Vdi> = made(
                    shared,
                    op,
                    s,
                    xapi::vdi::async_copy(s, &rec.vdi, Some(&disk.sr), None, None),
                    span,
                )
                .await?;
                add_vm_hint(s, &new, vm).await?;
                tri!(xapi::vbd::destroy(s, vbd).await);
                tri!(xapi::vdi::destroy(s, &rec.vdi).await);
                let record = vbd_record(
                    vm,
                    Some(&new),
                    &disk.userdevice,
                    VbdType::Disk,
                    false,
                    disk.userdevice == "0",
                    true,
                );
                let at = tri!(position(s, vm, Some(&disk.userdevice)).await);
                tri!(
                    xapi::vbd::create(
                        s,
                        &VbdRecord {
                            userdevice: at,
                            ..record
                        }
                    )
                    .await
                );
                new
            }
            None => {
                let kind = if disk.system && disk.source == DiskSource::Provision {
                    VdiType::System
                } else {
                    VdiType::User
                };
                let record = vdi_record(
                    &disk.name,
                    &disk.description,
                    &disk.sr,
                    disk.size,
                    kind,
                    &vm.to_string(),
                );
                let new: Ref<Vdi> =
                    made(shared, op, s, xapi::vdi::async_create(s, &record), span).await?;
                let at = tri!(position(s, vm, Some(&disk.userdevice)).await);
                let record = vbd_record(
                    vm,
                    Some(&new),
                    &at,
                    VbdType::Disk,
                    false,
                    bootable_new,
                    true,
                );
                tri!(xapi::vbd::create(s, &record).await);
                new
            }
        };
        let rec = tri!(xapi::vdi::get_record(s, &vdi).await);
        if rec.name_description != disk.description {
            tri!(xapi::vdi::set_name_description(s, &vdi, &disk.description).await);
        }
        if rec.name_label != disk.name {
            tri!(xapi::vdi::set_name_label(s, &vdi, &disk.name).await);
        }
    }
    let suspend_sr = suspend_sr.unwrap_or_else(Ref::null);
    tri!(xapi::vm::set_suspend_sr(s, vm, &suspend_sr).await);

    // 8. Networks: the template's interfaces are replaced by the chosen
    // ones.
    step(shared, op, OperationStep::CreatingNetworks);
    for vif in tri!(xapi::vm::get_vifs(s, vm).await) {
        task(
            shared,
            op,
            s,
            xapi::vif::async_destroy(s, &vif),
            Span(0.9, 0.92),
        )
        .await?;
    }
    for (index, vif) in spec.vifs.iter().enumerate() {
        let allowed = tri!(xapi::vm::get_allowed_vif_devices(s, vm).await);
        let wanted = index.to_string();
        let device = if allowed.contains(&wanted) {
            wanted
        } else {
            match allowed.first() {
                Some(d) => d.clone(),
                None => {
                    tracing::warn!("no free interface position left on the new VM");
                    break;
                }
            }
        };
        let mtu = m.networks.get(&vif.network).map_or(1500, |n| n.mtu);
        let record = vif_record(vm, &vif.network, device, &vif.mac, mtu);
        made::<xapi::Vif>(
            shared,
            op,
            s,
            xapi::vif::async_create(s, &record),
            Span(0.92, 0.95),
        )
        .await?;
    }
    Ok(())
}

/// After the point of no return: the vTPM, then the start.
async fn finish_vm(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    spec: &NewVmSpec,
    vm: &Ref<Vm>,
) -> Result<(), TaskOutcome> {
    if spec.vtpm {
        step(shared, op, OperationStep::AttachingVtpm);
        let record = tri!(xapi::vm::get_record(s, vm).await);
        if record.vtpms.is_empty() {
            tri!(xapi::vtpm::create(s, vm, false).await);
        }
    }
    if spec.start {
        step(shared, op, OperationStep::StartingVm);
        task(
            shared,
            op,
            s,
            xapi::vm::async_start(s, vm, false, false),
            Span(0.95, 1.0),
        )
        .await?;
    }
    Ok(())
}

/// `VM.GetBootOrderNetworkFirst`: `n` moved (or added) to the front.
pub fn network_first(order: &str) -> String {
    let order = order.to_lowercase();
    if order.is_empty() {
        return "ncd".to_owned();
    }
    let rest: String = order.chars().filter(|c| *c != 'n').collect();
    format!("n{rest}")
}

/// `AddVMHint`: `sm_config["vmhint"]` = the VM.
async fn add_vm_hint(s: &Session, vdi: &Ref<Vdi>, vm: &Ref<Vm>) -> Result<(), TaskOutcome> {
    let mut sm = tri!(xapi::vdi::get_sm_config(s, vdi).await);
    sm.insert("vmhint".into(), vm.to_string());
    tri!(xapi::vdi::set_sm_config(s, vdi, &sm).await);
    Ok(())
}

/// `VMDestroyAction.DestroyVM(vm, deleteAllOwnerDisks: true)`: the VM,
/// then the disks its drives own and its suspend disk.
async fn destroy_with_owned_disks(s: &Session, vm: &Ref<Vm>) -> xapi::Result<()> {
    let record = xapi::vm::get_record(s, vm).await?;
    let mut disks = Vec::new();
    for vbd in &record.vbds {
        let rec = xapi::vbd::get_record(s, vbd).await?;
        if provisioning::vbd_is_owner(&rec) && !rec.vdi.is_null() {
            disks.push(rec.vdi);
        }
    }
    if !record.suspend_vdi.is_null() {
        disks.push(record.suspend_vdi.clone());
    }
    xapi::vm::destroy(s, vm).await?;
    let mut first = None;
    for vdi in disks {
        match xapi::vdi::destroy(s, &vdi).await {
            Ok(()) => {}
            Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {}
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    first.map_or(Ok(()), Err)
}

// ---- VMs and templates -------------------------------------------------------------

/// `VMCloneAction` / `VMCopyAction`: `VM.clone` or `VM.copy`, then the
/// description.
async fn copy_vm(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    name: &str,
    description: &str,
    mode: &CopyMode,
) -> TaskOutcome {
    let s = session;
    let start = async {
        match mode {
            CopyMode::Clone => xapi::vm::async_clone(s, vm, name).await,
            CopyMode::Copy(sr) => xapi::vm::async_copy(s, vm, name, sr).await,
        }
    };
    let new: Ref<Vm> = match made(shared, op, s, start, Span(0.0, 0.95)).await {
        Ok(r) => r,
        Err(o) => return o,
    };
    shared.update_operation(op, |o| o.created = Some(new.clone()));
    match xapi::vm::set_name_description(s, &new, description).await {
        Ok(()) => TaskOutcome::Succeeded,
        Err(e) => TaskOutcome::Failed(e),
    }
}

/// `VMDestroyAction`: the chosen snapshots (each with the disks it owns),
/// then the VM, then the chosen disks and the suspend disk. Errors after
/// the VM is gone are collected; the first is reported.
async fn delete_vm(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    disks: &[Ref<Vdi>],
    snapshots: &[Ref<Vm>],
) -> TaskOutcome {
    let s = session;
    let m = model(shared, connection);
    let total = (snapshots.len() + 1 + disks.len()) as f64;
    let span = |i: usize| Span(i as f64 / total, (i + 1) as f64 / total);
    let mut first_error = None;
    for (i, snapshot) in snapshots.iter().enumerate() {
        if let Err(o) = delete_snapshot(shared, op, s, snapshot, span(i)).await {
            first_error.get_or_insert(o);
        }
    }
    let mut destroy: Vec<Ref<Vdi>> = disks.to_vec();
    if let Some(suspend) = m.vms.get(vm).map(|r| r.suspend_vdi.clone())
        && !suspend.is_null()
    {
        destroy.push(suspend);
    }
    if let Err(e) = xapi::vm::destroy(s, vm).await {
        return TaskOutcome::Failed(e);
    }
    let base = snapshots.len() + 1;
    for (i, vdi) in destroy.iter().enumerate() {
        match run_task(
            shared,
            op,
            s,
            xapi::vdi::async_destroy(s, vdi),
            span(base + i),
            false,
        )
        .await
        {
            TaskOutcome::Failed(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {}
            TaskOutcome::Succeeded => {}
            other => {
                first_error.get_or_insert(other);
            }
        }
    }
    first_error.unwrap_or(TaskOutcome::Succeeded)
}

// ---- snapshots ------------------------------------------------------------------

/// `VMSnapshotCreateAction`: `VM.snapshot` (disks only) or
/// `VM.checkpoint` (disks and memory), then the description.
async fn take_snapshot(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    name: &str,
    description: &str,
    memory: bool,
) -> TaskOutcome {
    let s = session;
    let start = async {
        if memory {
            xapi::vm::async_checkpoint(s, vm, name).await
        } else {
            xapi::vm::async_snapshot(s, vm, name, None).await
        }
    };
    let snapshot: Ref<Vm> = match made(shared, op, s, start, Span(0.0, 0.95)).await {
        Ok(r) => r,
        Err(o) => return o,
    };
    shared.update_operation(op, |o| o.created = Some(snapshot.clone()));
    match xapi::vm::set_name_description(s, &snapshot, description).await {
        Ok(()) => TaskOutcome::Succeeded,
        Err(e) => TaskOutcome::Failed(e),
    }
}

/// `VMSnapshotRevertAction`: `VM.revert`; when the VM was running at the
/// snapshot, it is started again (or resumed, for a snapshot with memory)
/// on the server it ran on if that server can take it. Failures of that
/// last step are only logged, as in the original.
async fn revert(
    shared: &Shared,
    _connection: ConnectionId,
    op: OperationId,
    session: &Session,
    snapshot: &Ref<Vm>,
) -> TaskOutcome {
    let s = session;
    // Read from the server: the cache may not have a snapshot just taken.
    let snap = match xapi::vm::get_record(s, snapshot).await {
        Ok(r) => r,
        Err(e) => return TaskOutcome::Failed(e),
    };
    let vm = snap.snapshot_of.clone();
    let was_running = snap
        .snapshot_info
        .get("power-state-at-snapshot")
        .map(String::as_str)
        == Some("Running");
    let (vm_name, previous_host) = match xapi::vm::get_record(s, &vm).await {
        Ok(v) => (v.name_label, Some(v.resident_on).filter(|h| !h.is_null())),
        Err(_) => (String::new(), None),
    };
    if let Err(o) = task(
        shared,
        op,
        s,
        xapi::vm::async_revert(s, snapshot),
        Span(0.0, 0.9),
    )
    .await
    {
        return o;
    }
    if !was_running || vm.is_null() {
        return TaskOutcome::Succeeded;
    }
    step(
        shared,
        op,
        OperationStep::RevertingPowerState { vm: vm_name },
    );
    let state = match xapi::vm::get_power_state(s, &vm).await {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the VM's state after reverting");
            return TaskOutcome::Succeeded;
        }
    };
    let stopped = matches!(state, VmPowerState::Halted | VmPowerState::Suspended);
    let host = match &previous_host {
        Some(h) if xapi::vm::assert_can_boot_here(s, &vm, h).await.is_ok() => Some(h.clone()),
        _ => None,
    };
    let start = async {
        match (state, &host) {
            (VmPowerState::Suspended, Some(h)) => {
                xapi::vm::async_resume_on(s, &vm, h, false, false).await
            }
            (VmPowerState::Suspended, None) => xapi::vm::async_resume(s, &vm, false, false).await,
            (_, Some(h)) => xapi::vm::async_start_on(s, &vm, h, false, false).await,
            (_, None) => xapi::vm::async_start(s, &vm, false, false).await,
        }
    };
    if stopped
        && let TaskOutcome::Failed(e) = run_task(shared, op, s, start, Span(0.9, 1.0), false).await
    {
        tracing::warn!(error = %e, "could not restore the VM's power state after reverting");
    }
    TaskOutcome::Succeeded
}

/// `VMSnapshotDeleteAction`: a snapshot with memory is shut down first,
/// then destroyed, then the disks its drives own and its suspend disk.
async fn delete_snapshot(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    snapshot: &Ref<Vm>,
    span: Span,
) -> Result<(), TaskOutcome> {
    let s = session;
    // Read from the server: the cache may not have a snapshot just taken.
    let record = match xapi::vm::get_record(s, snapshot).await {
        Ok(r) => r,
        Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => return Ok(()),
        Err(e) => return Err(TaskOutcome::Failed(e)),
    };
    let mut disks = Vec::new();
    for vbd in &record.vbds {
        let b = tri!(xapi::vbd::get_record(s, vbd).await);
        if provisioning::vbd_is_owner(&b) && !b.vdi.is_null() && b.r#type != VbdType::Cd {
            disks.push(b.vdi);
        }
    }
    if !record.suspend_vdi.is_null() {
        disks.push(record.suspend_vdi.clone());
    }
    if record.power_state == VmPowerState::Suspended {
        tri!(xapi::vm::hard_shutdown(s, snapshot).await);
    }
    let mid = span.0 + (span.1 - span.0) / 2.0;
    task(
        shared,
        op,
        s,
        xapi::vm::async_destroy(s, snapshot),
        Span(span.0, mid),
    )
    .await?;
    let mut first = None;
    for vdi in disks {
        match xapi::vdi::destroy(s, &vdi).await {
            Ok(()) => {}
            Err(e) if e.is_api(xapi::error::codes::HANDLE_INVALID) => {}
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    shared.update_operation(op, |o| o.progress = Some(span.1));
    match first {
        Some(e) => Err(TaskOutcome::Failed(e)),
        None => Ok(()),
    }
}

// ---- disks ------------------------------------------------------------------------------

/// `CreateDiskAction` + `VbdCreateAndPlugAction`: `VDI.create` with the
/// VM as `vmhint`, then a drive owning it at the first free position
/// (bootable when it is position 0 and the VM has no other bootable
/// disk), plugged in when the VM runs.
async fn new_disk(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    disk: &DiskSpec,
) -> TaskOutcome {
    let s = session;
    outcome(
        async {
            let at = tri!(position(s, vm, None).await);
            let uuid = tri!(xapi::vm::get_uuid(s, vm).await);
            let record = vdi_record(
                &disk.name,
                &disk.description,
                &disk.sr,
                disk.size,
                VdiType::User,
                &uuid,
            );
            let vdi = tri!(xapi::vdi::create(s, &record).await);
            shared.update_operation(op, |o| o.progress = Some(0.5));
            let bootable = at == "0" && !has_bootable_disk(s, vm).await?;
            let record = vbd_record(vm, Some(&vdi), &at, VbdType::Disk, false, bootable, true);
            create_and_plug(
                shared,
                op,
                s,
                &record,
                OperationNotice::RestartForDisk,
                Span(0.5, 1.0),
            )
            .await?;
            Ok(())
        }
        .await,
    )
}

async fn has_bootable_disk(s: &Session, vm: &Ref<Vm>) -> Result<bool, TaskOutcome> {
    for vbd in tri!(xapi::vm::get_vbds(s, vm).await) {
        let rec = tri!(xapi::vbd::get_record(s, &vbd).await);
        if rec.bootable && rec.r#type == VbdType::Disk {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `AttachDiskDialog`: a drive at the first free position (bootable at
/// position 0; owning the disk when nothing else uses it), plugged in when
/// the VM runs.
async fn attach_disk(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
    vdi: &Ref<Vdi>,
    read_only: bool,
) -> TaskOutcome {
    let s = session;
    let owner = model(shared, connection)
        .vdis
        .get(vdi)
        .is_none_or(|v| v.vbds.is_empty());
    outcome(
        async {
            let at = tri!(position(s, vm, None).await);
            let record = vbd_record(
                vm,
                Some(vdi),
                &at,
                VbdType::Disk,
                read_only,
                at == "0",
                owner,
            );
            create_and_plug(
                shared,
                op,
                s,
                &record,
                OperationNotice::RestartForDisk,
                Span::FULL,
            )
            .await?;
            Ok(())
        }
        .await,
    )
}

/// `DetachVirtualDiskAction`: unplug when attached (and allowed), then
/// destroy the drive (also tried when the unplug failed). The disk stays.
async fn detach(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vbd: &Ref<Vbd>,
    span: Span,
) -> TaskOutcome {
    let s = session;
    let mid = span.0 + (span.1 - span.0) / 2.0;
    let rec = match xapi::vbd::get_record(s, vbd).await {
        Ok(r) => r,
        Err(e) => return TaskOutcome::Failed(e),
    };
    let mut unplug_error = None;
    if rec.currently_attached && rec.allowed_operations.contains(&VbdOperations::Unplug) {
        match run_task(
            shared,
            op,
            s,
            xapi::vbd::async_unplug(s, vbd),
            Span(span.0, mid),
            false,
        )
        .await
        {
            TaskOutcome::Succeeded => {}
            TaskOutcome::Cancelled => return TaskOutcome::Cancelled,
            failed => unplug_error = Some(failed),
        }
    }
    match run_task(
        shared,
        op,
        s,
        xapi::vbd::async_destroy(s, vbd),
        Span(mid, span.1),
        false,
    )
    .await
    {
        TaskOutcome::Succeeded => TaskOutcome::Succeeded,
        other => unplug_error.unwrap_or(other),
    }
}

/// `DestroyDiskAction`: each drive is detached, then the disk destroyed.
async fn delete_disk(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vdi: &Ref<Vdi>,
) -> TaskOutcome {
    let s = session;
    let vbds = match xapi::vdi::get_vbds(s, vdi).await {
        Ok(v) => v,
        Err(e) => return TaskOutcome::Failed(e),
    };
    for vbd in &vbds {
        if let o @ (TaskOutcome::Failed(_) | TaskOutcome::Cancelled) =
            detach(shared, op, s, vbd, Span(0.0, 0.5)).await
        {
            return o;
        }
    }
    run_task(
        shared,
        op,
        s,
        xapi::vdi::async_destroy(s, vdi),
        Span(0.5, 1.0),
        false,
    )
    .await
}

/// The disk's Size and Location page: `VDI.resize`. (The original falls
/// back to `VDI.resize_online`, which XAPI 25 no longer has: `resize`
/// handles attached disks itself.)
async fn resize_disk(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vdi: &Ref<Vdi>,
    size: i64,
) -> TaskOutcome {
    let s = session;
    run_task(
        shared,
        op,
        s,
        xapi::vdi::async_resize(s, vdi, size),
        Span::FULL,
        false,
    )
    .await
}

/// `MigrateVirtualDiskAction` while a running VM uses the disk
/// (`VDI.pool_migrate`); otherwise `MoveVirtualDiskAction`: `VDI.copy` to
/// the SR, the old drives destroyed, the old disk destroyed, and the
/// drives made again on the copy (same position, mode, boot flag and
/// owner flag).
async fn move_disk(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vdi: &Ref<Vdi>,
    sr: &Ref<Sr>,
) -> TaskOutcome {
    let s = session;
    let _ = connection;
    let mut live = false;
    for vbd in match xapi::vdi::get_vbds(s, vdi).await {
        Ok(v) => v,
        Err(e) => return TaskOutcome::Failed(e),
    } {
        live |= matches!(xapi::vbd::get_currently_attached(s, &vbd).await, Ok(true));
    }
    if live {
        let none = HashMap::new();
        let start = xapi::vdi::async_pool_migrate(s, vdi, sr, &none);
        return match made::<Vdi>(shared, op, s, start, Span::FULL).await {
            Ok(_) => TaskOutcome::Succeeded,
            Err(o) => o,
        };
    }
    outcome(
        async {
            let old = tri!(xapi::vdi::get_record(s, vdi).await);
            let copy: Ref<Vdi> = made(
                shared,
                op,
                s,
                xapi::vdi::async_copy(s, vdi, Some(sr), None, None),
                Span(0.0, 0.9),
            )
            .await?;
            if old.r#type == VdiType::Suspend {
                for vbd in &old.vbds {
                    let rec = tri!(xapi::vbd::get_record(s, vbd).await);
                    tri!(xapi::vm::set_suspend_vdi(s, &rec.vm, &copy).await);
                }
            }
            let mut again = Vec::new();
            for vbd in &old.vbds {
                let rec = tri!(xapi::vbd::get_record(s, vbd).await);
                let mut record = vbd_record(
                    &rec.vm,
                    Some(&copy),
                    &rec.userdevice,
                    rec.r#type.clone(),
                    rec.mode == VbdMode::Ro,
                    rec.bootable,
                    false,
                );
                record.unpluggable = rec.unpluggable;
                record.other_config = rec.other_config.clone();
                if rec.currently_attached {
                    if rec.allowed_operations.contains(&VbdOperations::Unplug) {
                        tri!(xapi::vbd::unplug(s, vbd).await);
                    }
                } else {
                    tri!(xapi::vbd::destroy(s, vbd).await);
                }
                again.push(record);
            }
            tri!(xapi::vdi::destroy(s, vdi).await);
            for record in again {
                tri!(xapi::vbd::create(s, &record).await);
            }
            Ok(())
        }
        .await,
    )
}

/// `ChangeVMISOAction`: eject what the drive holds, then insert the ISO
/// (`None` only ejects).
async fn change_cd(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vbd: &Ref<Vbd>,
    iso: Option<&Ref<Vdi>>,
) -> TaskOutcome {
    let s = session;
    outcome(
        async {
            let rec = tri!(xapi::vbd::get_record(s, vbd).await);
            if !rec.empty {
                task(
                    shared,
                    op,
                    s,
                    xapi::vbd::async_eject(s, vbd),
                    Span(0.0, 0.5),
                )
                .await?;
            }
            if let Some(iso) = iso {
                task(
                    shared,
                    op,
                    s,
                    xapi::vbd::async_insert(s, vbd, iso),
                    Span(0.5, 1.0),
                )
                .await?;
            }
            Ok(())
        }
        .await,
    )
}

/// `CreateCdDriveAction`: an empty read-only drive at position 3 (or the
/// first free one), plugged in when allowed.
async fn new_cd_drive(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
) -> TaskOutcome {
    let s = session;
    outcome(
        async {
            let at = tri!(position(s, vm, Some("3")).await);
            let record = vbd_record(vm, None, &at, VbdType::Cd, true, false, false);
            create_and_plug(
                shared,
                op,
                s,
                &record,
                OperationNotice::RestartForDrive,
                Span::FULL,
            )
            .await?;
            Ok(())
        }
        .await,
    )
}

/// `InstallPVToolsAction`: the guest tools ISO into the VM's first DVD
/// drive (what it held is ejected).
async fn install_tools(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    vm: &Ref<Vm>,
) -> TaskOutcome {
    let m = model(shared, connection);
    let Some(iso) = provisioning::tools_iso(&m).cloned() else {
        return TaskOutcome::Failed(xapi::Error::Api {
            code: "VDI_MISSING".into(),
            params: vec!["guest-tools.iso".into()],
        });
    };
    let drive = m.vms.get(vm).and_then(|v| {
        provisioning::cd_drives(&m, v)
            .first()
            .map(|(r, _)| (*r).clone())
    });
    let Some(drive) = drive else {
        return TaskOutcome::Failed(xapi::Error::Api {
            code: "VM_NO_CD_DRIVE".into(),
            params: vec![vm.to_string()],
        });
    };
    change_cd(shared, op, session, &drive, Some(&iso)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_order_network_first() {
        assert_eq!(network_first("cd"), "ncd");
        assert_eq!(network_first("cdn"), "ncd");
        assert_eq!(network_first("DCN"), "ndc");
        assert_eq!(network_first(""), "ncd");
    }
}
