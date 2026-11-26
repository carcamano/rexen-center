//! OVF/OVA export and import, and disk image import (F11b), call for call
//! as the original's `ExportApplianceAction`, `ImportApplianceAction` and
//! `ImportImageAction`.
//!
//! * **Export** ([`OvfExport`]): the package goes into `<folder>/<name>/`.
//!   Each disk of each VM is fetched with `GET /export_raw_vdi?…&vdi=<uuid>
//!   &format=vhd` (a task `export_raw_vdi_task` per disk) and written as
//!   `<vdi uuid>.vhd` (gzipped to `.vhd.gz` with "Compress"), hashed
//!   (SHA-256) on the way; then the descriptor `<name>.ovf` and, with
//!   "Create a manifest", `<name>.mf`. An OVA is written in one pass into
//!   `<name>.ova`: the descriptor first, each disk as it arrives, the
//!   manifest last (the OVF specification allows it at the end; the
//!   original copies the finished folder into the archive instead). A
//!   compressed OVA is that archive gzipped to `<name>.ova.gz`. "Verify
//!   export on completion" checks each disk's VHD structure afterwards. A
//!   cancelled or failed export removes every file it wrote (and the
//!   folder when it made it).
//! * **Import** ([`OvfImport`]): with "Verify digital signature" the
//!   certificate's signature of the manifest is checked, and with it or
//!   "Verify manifest content" every file is hashed against the manifest,
//!   before anything is made. Then, per virtual system: `VM.create` from
//!   the descriptor (`CreateVm`, hidden with `HideFromXenCenter` until it
//!   is complete), then its items in order: an interface (`VIF.create` on
//!   the network chosen), the first CD drive (an empty `VBD`), each disk
//!   (`VDI.create` in the SR chosen, `PUT /import_raw_vdi?…&vdi=<uuid>
//!   &format=vhd|raw` with a task `import_raw_vdi_task`, `VBD.create`).
//!   A dynamic VHD is sent as VHD; a fixed VHD, a raw image or an ISO as
//!   raw bytes (a fixed VHD's footer left out); a gzipped file is expanded
//!   next to the package first. VMDK, VHDX, qcow2 and WIM files are refused
//!   (no converter). A disk image import is the same with the envelope the
//!   wizard made of the image. Unlike the original, no vApp
//!   (`VM_appliance`) is made: vApps come with a later phase. A cancelled
//!   or failed import destroys the VMs and disks it made.
//!
//! Files stream [`crate::BackendOptions::transfer_buffer`] bytes at a time.

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use xapi::{
    DomainType, Host, Network, OnCrashBehaviour, OnNormalExit, OnSoftrebootBehavior, Ref, Session,
    Sr, Task, TaskStatusType, Vdi, VdiType, Vm, VmPowerState, VmRecord,
};

use crate::actions::{ActionRequest, VmAction};
use crate::backend::{Command, ConnectionId, OperationId, Shared};
use crate::disk_image::{self, Image, ImageFormat};
use crate::operations::{OperationStep, Span, TaskOutcome, run_task_result};
use crate::ova::TarWriter;
use crate::ovf::{self, Envelope, ExportedDisk, ExportedVm, Rasd};
use crate::ovf_package::{self, Package, PackageError};
use crate::storage_run::{model, step};
use crate::xva_run::CancelFlag;

/// The errors this module reports itself (worded by the UI).
pub mod errors {
    /// "Please shut down or suspend VM '{0}' before exporting."
    pub const OVF_VM_RUNNING: &str = "XENADMIN_OVF_VM_RUNNING";
    /// "Verification for disk '{0}' failed. See logs for details."
    pub const OVF_DISK_VERIFY: &str = "XENADMIN_OVF_DISK_VERIFY";
    /// The export failed; params: the reason.
    pub const OVF_EXPORT_FAILED: &str = "XENADMIN_OVF_EXPORT_FAILED";
    /// "Signature verification failed. {0}"
    pub const OVF_SIGNATURE: &str = "XENADMIN_OVF_SIGNATURE";
    /// "Manifest verification failed. {0}"
    pub const OVF_MANIFEST: &str = "XENADMIN_OVF_MANIFEST";
    /// The package could not be read; params: the reason.
    pub const OVF_PACKAGE: &str = "XENADMIN_OVF_PACKAGE";
    /// "File {0} not found"
    pub const OVF_FILE_MISSING: &str = "XENADMIN_OVF_FILE_MISSING";
    /// A disk format that needs converting; params: the file, the format.
    pub const OVF_UNSUPPORTED_DISK: &str = "XENADMIN_OVF_UNSUPPORTED_DISK";
    /// "SR '{0}' does not have {1} of free space to import virtual disk {2}."
    pub const OVF_SR_SPACE: &str = "XENADMIN_OVF_SR_SPACE";
    /// "The storage repository was not found and a default was not
    /// assigned."
    pub const OVF_NO_SR: &str = "XENADMIN_OVF_NO_SR";
    /// The disk could not be read; params: the file, the reason.
    pub const OVF_FILE_ERROR: &str = "XENADMIN_OVF_FILE_ERROR";
    /// The upload failed; params: the file, the reason.
    pub const OVF_IMPORT_FAILED: &str = "XENADMIN_OVF_IMPORT_FAILED";
    /// "The selected package is encrypted…"
    pub const OVF_ENCRYPTED: &str = "XENADMIN_OVF_ENCRYPTED";
}

fn client_error(code: &str, params: &[&str]) -> xapi::Error {
    crate::storage::client_error(code, params)
}

/// The Export wizard's OVF/OVA export.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OvfExport {
    pub vms: Vec<Ref<Vm>>,
    /// The folder the package folder goes into.
    pub folder: String,
    /// The package's name (`<folder>/<name>/<name>.ovf`).
    pub name: String,
    /// One `.ova` file.
    pub ova: bool,
    /// Gzip the disks (or the whole OVA).
    pub compress: bool,
    /// Write `<name>.mf`.
    pub manifest: bool,
    /// EULA texts.
    pub eulas: Vec<String>,
    /// Check each disk once written.
    pub verify: bool,
}

/// What the Import wizard chose for one virtual system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VmMapping {
    /// `VirtualSystem@id`.
    pub system: String,
    /// The VM's name.
    pub name: String,
    pub affinity: Option<Ref<Host>>,
    /// Disk item (`InstanceID`) → SR.
    pub srs: Vec<(String, Ref<Sr>)>,
    /// Interface item (`InstanceID`) → network.
    pub networks: Vec<(String, Ref<Network>)>,
}

/// The Import wizard's OVF/OVA or disk image import.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OvfImport {
    /// The `.ovf`, `.ova` or disk image.
    pub path: String,
    /// A disk image import: the envelope made of the image.
    pub image: Option<Box<Envelope>>,
    pub vms: Vec<VmMapping>,
    pub verify_manifest: bool,
    pub verify_signature: bool,
    pub start: bool,
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Err(TaskOutcome::Failed(e)),
        }
    };
}

/// Runs an F11b request (registered with the transfer's cancel button by
/// [`crate::xva_run::run`]).
pub(crate) async fn run(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    cancel: &Arc<CancelFlag>,
    request: &ActionRequest,
) -> Result<(), TaskOutcome> {
    match request {
        ActionRequest::ExportOvf(spec) => {
            export(shared, connection, op, session, cancel, spec).await
        }
        ActionRequest::ImportOvf(spec) => {
            import(shared, connection, op, session, cancel, spec).await
        }
        _ => Err(TaskOutcome::Failed(xapi::Error::Protocol(
            "not an OVF request".into(),
        ))),
    }
}

/// Follows a transfer's task once the transfer ended (cancelling it when
/// it never started), then destroys it. `Err` is the server's failure.
async fn end_task(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    task: &Ref<Task>,
    transferred: bool,
    at: f64,
) -> TaskOutcome {
    if !transferred
        && matches!(
            xapi::task::get_status(s, task).await,
            Ok(TaskStatusType::Pending)
        )
    {
        let _ = xapi::task::cancel(s, task).await;
    }
    run_task_result(
        shared,
        op,
        s,
        async { Ok(task.clone()) },
        Span(at, at),
        false,
    )
    .await
    .0
}

// ---- export -----------------------------------------------------------------------------

/// Where an export writes, and what it wrote (removed on failure).
struct Output {
    dir: PathBuf,
    made_dir: bool,
    written: Vec<PathBuf>,
}

impl Output {
    fn remove(&self) {
        for f in &self.written {
            let _ = std::fs::remove_file(f);
        }
        if self.made_dir {
            let _ = std::fs::remove_dir(&self.dir);
        }
    }
}

/// A sink for a disk: a file of the folder, or the open entry of the OVA;
/// hashed (SHA-256) and optionally gzipped on the way.
enum Sink<'a> {
    File(std::fs::File),
    Gz(flate2::write::GzEncoder<std::fs::File>),
    Tar(&'a mut TarWriter<std::fs::File>),
}

/// Hashes what goes into the inner writer.
struct Hashing<W: Write> {
    inner: W,
    hash: ring::digest::Context,
    bytes: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.hash.update(&b[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Write for Sink<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File(f) => f.write(b),
            Self::Gz(g) => g.write(b),
            Self::Tar(t) => {
                t.write(b)?;
                Ok(b.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Gz(g) => g.flush(),
            Self::Tar(_) => Ok(()),
        }
    }
}

fn is_disk_full(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::StorageFull || e.raw_os_error() == Some(28)
}

/// A failed export's outcome.
fn export_error(why: impl std::fmt::Display) -> TaskOutcome {
    TaskOutcome::Failed(client_error(errors::OVF_EXPORT_FAILED, &[&why.to_string()]))
}

fn file_error(e: std::io::Error) -> TaskOutcome {
    if is_disk_full(&e) {
        TaskOutcome::Failed(client_error(crate::xva_run::errors::EXPORT_DISK_FULL, &[]))
    } else {
        export_error(e)
    }
}

/// `Export`: what the descriptor says of `vm` (its disks' files named
/// `<vdi uuid>.vhd[.gz]`).
fn describe_vm(m: &crate::model::ConnectionModel, vm: &VmRecord, compress: bool) -> ExportedVm {
    let mut out = ExportedVm {
        id: ovf::new_guid(),
        name: vm.name_label.clone(),
        description: vm.name_description.clone(),
        hvm: !vm.hvm_boot_policy.is_empty(),
        arch: vm.other_config.get("domarch").cloned().unwrap_or_default(),
        vcpus_at_startup: vm.vcpus_at_startup,
        vcpus_max: vm.vcpus_max,
        memory_mib: vm.memory_dynamic_max / (1 << 20),
        order: vm.order,
        start_delay: vm.start_delay,
        shutdown_delay: vm.shutdown_delay,
        ..Default::default()
    };
    if let Some(gm) = m.get(&vm.guest_metrics)
        && let Some(name) = gm.os_version.get("name")
    {
        out.os_description = name.split('|').next().unwrap_or_default().to_owned();
        out.os_info = match (gm.os_version.get("major"), gm.os_version.get("minor")) {
            (Some(a), Some(b)) => format!("Version {a}.{b}"),
            _ => "Guest Operating System".to_owned(),
        };
    }
    let mut vifs: Vec<_> = vm.vifs.iter().filter_map(|v| m.vifs.get(v)).collect();
    vifs.sort_by(|a, b| crate::objects::natural_cmp(&a.device, &b.device));
    for vif in vifs {
        if let Some(n) = m.networks.get(&vif.network) {
            out.nics.push((
                n.uuid.clone(),
                n.name_label.clone(),
                n.name_description.clone(),
                vif.mac.clone(),
            ));
        }
    }
    let mut vbds: Vec<_> = vm.vbds.iter().filter_map(|b| m.vbds.get(b)).collect();
    vbds.sort_by(|a, b| crate::objects::natural_cmp(&a.userdevice, &b.userdevice));
    let mut n = 0;
    for vbd in vbds {
        if vbd.r#type == xapi::VbdType::Cd {
            out.cds.push((vbd.uuid.clone(), vbd.userdevice.clone()));
            continue;
        }
        let Some(vdi) = m.vdis.get(&vbd.vdi) else {
            continue;
        };
        n += 1;
        out.disks.push(ExportedDisk {
            disk_id: ovf::new_guid(),
            file: format!("{}.vhd{}", vdi.uuid, if compress { ".gz" } else { "" }),
            file_size: None,
            compressed: compress,
            name: if vdi.name_label.is_empty() {
                format!("Hard Disk Image {n}")
            } else {
                vdi.name_label.clone()
            },
            description: vdi.name_description.clone(),
            capacity: vdi.virtual_size.max(0) as u64,
            bootable: vbd.bootable,
            userdevice: vbd.userdevice.clone(),
        });
    }
    let info = |what: &str| format!("{what} Specific Information");
    let pairs = |m: &HashMap<String, String>| ovf::join_pairs(m);
    let oc = &mut out.other_config;
    if !vm.hvm_boot_params.is_empty() {
        oc.push((
            "HVM_boot_params".into(),
            pairs(&vm.hvm_boot_params),
            "HVM Boot Parameters".into(),
        ));
    }
    if !vm.hvm_boot_policy.is_empty() {
        oc.push((
            "HVM_boot_policy".into(),
            vm.hvm_boot_policy.clone(),
            "Boot Order: CD/DVD - Network - Disk".into(),
        ));
    }
    if (vm.hvm_shadow_multiplier - 1.0).abs() > f64::EPSILON && vm.hvm_shadow_multiplier > 0.0 {
        oc.push((
            "HVM_shadow_multiplier".into(),
            vm.hvm_shadow_multiplier.to_string(),
            info("Xen"),
        ));
    }
    if !vm.platform.is_empty() {
        oc.push((
            "platform".into(),
            pairs(&vm.platform),
            "Xen Platform Information".into(),
        ));
    }
    if !vm.nvram.is_empty() {
        oc.push((
            "NVRAM".into(),
            pairs(&vm.nvram),
            "Non-Volatile Random-Access Memory".into(),
        ));
    }
    for (k, v) in [
        ("PV_args", &vm.pv_args),
        ("PV_bootloader", &vm.pv_bootloader),
        ("PV_bootloader_args", &vm.pv_bootloader_args),
        ("PV_kernel", &vm.pv_kernel),
        ("PV_legacy_args", &vm.pv_legacy_args),
        ("PV_ramdisk", &vm.pv_ramdisk),
    ] {
        if !v.is_empty() {
            oc.push((k.into(), v.clone(), info("Xen")));
        }
    }
    if vm.hardware_platform_version >= 0 {
        oc.push((
            "hardware_platform_version".into(),
            vm.hardware_platform_version.to_string(),
            info("Xen"),
        ));
    }
    if !vm.recommendations.is_empty() {
        oc.push((
            "recommendations".into(),
            vm.recommendations.clone(),
            info("Xen"),
        ));
    }
    if !vm.reference_label.is_empty() {
        oc.push((
            "reference_label".into(),
            vm.reference_label.clone(),
            info("Xen"),
        ));
    }
    if vm.has_vendor_device {
        oc.push(("VM_has_vendor_device".into(), "True".into(), info("Xen")));
    }
    out
}

/// One disk fetched into `sink`: its task, the download, the task's end.
/// Returns the SHA-256 of what was written and its size.
#[allow(clippy::too_many_arguments)]
async fn fetch_disk(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    cancel: &CancelFlag,
    client: &xapi::http::HttpTransfer,
    vdi_uuid: &str,
    file: &str,
    mut sink: Sink<'_>,
    progress: (f64, f64, u64),
) -> Result<(Vec<u8>, u64), TaskOutcome> {
    let task = tri!(
        xapi::task::create(
            s,
            "export_raw_vdi_task",
            &format!("Exporting VDI {vdi_uuid} to {file}"),
        )
        .await
    );
    let session_ref = s.session_ref().as_str().to_owned();
    let (from, to, expected) = progress;
    let limit = shared.options.transfer_faults.disk_full_after;
    let mut w = Hashing {
        inner: &mut sink,
        hash: ring::digest::Context::new(&ring::digest::SHA256),
        bytes: 0,
    };
    let mut received = 0u64;
    let fetched: Result<(), TaskOutcome> = async {
        let mut download = client
            .get(
                "export_raw_vdi",
                &[
                    ("task_id", task.as_str()),
                    ("session_id", session_ref.as_str()),
                    ("vdi", vdi_uuid),
                    ("format", "vhd"),
                ],
            )
            .await
            .map_err(|e| TaskOutcome::Failed(client_error(errors::OVF_EXPORT_FAILED, &[&e.to_string()])))?;
        loop {
            let chunk = tokio::select! {
                c = download.chunk() => c.map_err(|e| TaskOutcome::Failed(client_error(errors::OVF_EXPORT_FAILED, &[&e.to_string()])))?,
                () = cancel.wait() => return Err(TaskOutcome::Cancelled),
            };
            let Some(chunk) = chunk else { break };
            if let Some(limit) = limit
                && received + chunk.len() as u64 > limit
            {
                return Err(file_error(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "no space left on device",
                )));
            }
            w.write_all(&chunk).map_err(file_error)?;
            received += chunk.len() as u64;
            let fraction = (received as f64 / expected.max(1) as f64).min(0.99);
            shared.update_operation(op, |o| o.progress = Some(from + (to - from) * fraction));
        }
        w.flush().map_err(file_error)?;
        Ok(())
    }
    .await;
    let Hashing {
        hash,
        bytes: written,
        ..
    } = w;
    let hash = hash.finish();
    if let Sink::Gz(g) = sink {
        g.finish().map_err(file_error)?;
    }
    let followed = end_task(shared, op, s, &task, fetched.is_ok(), to).await;
    if cancel.is_set() {
        return Err(TaskOutcome::Cancelled);
    }
    match (fetched, followed) {
        (Ok(()), TaskOutcome::Succeeded) => Ok((hash.as_ref().to_vec(), written)),
        (Ok(()), TaskOutcome::Cancelled) | (Err(TaskOutcome::Cancelled), _) => {
            Err(TaskOutcome::Cancelled)
        }
        // What the server says is the reason, when it says something.
        (_, TaskOutcome::Failed(e)) if e.is_api("VDI_IN_USE") => Err(TaskOutcome::Failed(
            client_error(crate::xva_run::errors::EXPORT_VDI_IN_USE, &[]),
        )),
        (Err(TaskOutcome::Failed(c)), TaskOutcome::Failed(server))
            if c.is_api(errors::OVF_EXPORT_FAILED) =>
        {
            Err(TaskOutcome::Failed(server))
        }
        (Err(o), _) => Err(o),
        (Ok(()), TaskOutcome::Failed(e)) => Err(TaskOutcome::Failed(e)),
    }
}

/// `ExportApplianceAction`.
async fn export(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    cancel: &CancelFlag,
    spec: &OvfExport,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let mut vms = Vec::new();
    for r in &spec.vms {
        let Some(v) = m.vms.get(r) else {
            return Err(TaskOutcome::Failed(client_error(
                "HANDLE_INVALID",
                &["VM", r.as_str()],
            )));
        };
        if !matches!(
            v.power_state,
            VmPowerState::Halted | VmPowerState::Suspended
        ) {
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_VM_RUNNING,
                &[&v.name_label],
            )));
        }
        vms.push(v.clone());
    }
    let coordinator = m.pool().map(|(_, p)| p.master.clone()).unwrap_or_default();
    let (client, _) = tri!(crate::pool_run::transfer_client(
        shared,
        connection,
        &coordinator,
        shared.options.transfer_timeout,
    ));
    let dir = Path::new(&spec.folder).join(&spec.name);
    let mut out = Output {
        made_dir: !dir.exists(),
        dir: dir.clone(),
        written: Vec::new(),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(file_error(e));
    }
    let result = write_package(shared, op, s, cancel, &client, spec, &m, &vms, &mut out).await;
    if let Err(outcome) = result {
        tracing::info!("export failed or cancelled; removing what it wrote");
        out.remove();
        return Err(outcome);
    }
    shared.update_operation(op, |o| o.progress = Some(1.0));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_package(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    cancel: &CancelFlag,
    client: &xapi::http::HttpTransfer,
    spec: &OvfExport,
    m: &crate::model::ConnectionModel,
    vms: &[Arc<VmRecord>],
    out: &mut Output,
) -> Result<(), TaskOutcome> {
    let gz_disks = spec.compress && !spec.ova;
    let mut described: Vec<ExportedVm> = vms.iter().map(|v| describe_vm(m, v, gz_disks)).collect();
    // The disks to fetch: (vm index, disk index, vdi uuid, expected bytes).
    let mut disks = Vec::new();
    for (i, v) in vms.iter().enumerate() {
        let mut vbds: Vec<_> = v.vbds.iter().filter_map(|b| m.vbds.get(b)).collect();
        vbds.sort_by(|a, b| crate::objects::natural_cmp(&a.userdevice, &b.userdevice));
        let mut j = 0;
        for vbd in vbds.iter().filter(|b| b.r#type != xapi::VbdType::Cd) {
            if let Some(vdi) = m.vdis.get(&vbd.vdi) {
                disks.push((
                    i,
                    j,
                    vdi.uuid.clone(),
                    vdi.physical_utilisation.max(1 << 20) as u64,
                    vdi.name_label.clone(),
                ));
                j += 1;
            }
        }
    }
    let total: u64 = disks.iter().map(|d| d.3).sum::<u64>().max(1);
    let descriptor_name = format!("{}.ovf", spec.name);
    let mut manifest: Vec<(String, Vec<u8>)> = Vec::new();
    let sha = |b: &[u8]| {
        ring::digest::digest(&ring::digest::SHA256, b)
            .as_ref()
            .to_vec()
    };
    let mut done = 0u64;
    let count = disks.len();
    if spec.ova {
        let ova_path = out.dir.join(format!("{}.ova", spec.name));
        let file = std::fs::File::create(&ova_path).map_err(file_error)?;
        out.written.push(ova_path.clone());
        let mut tar = TarWriter::new(file);
        // The descriptor first: its files' sizes are left out.
        let xml = ovf::export_descriptor(&spec.name, &described, &spec.eulas);
        tar.entry(&descriptor_name, xml.as_bytes())
            .map_err(file_error)?;
        manifest.push((descriptor_name.clone(), sha(xml.as_bytes())));
        for (n, (i, j, uuid, expected, label)) in disks.iter().enumerate() {
            let file = described[*i].disks[*j].file.clone();
            step(
                shared,
                op,
                OperationStep::ExportingDisk {
                    disk: label.clone(),
                    file: file.clone(),
                    index: n + 1,
                    count,
                },
            );
            tar.begin(&file).map_err(file_error)?;
            let from = 0.8 * done as f64 / total as f64;
            done += expected;
            let to = 0.8 * done as f64 / total as f64;
            let (hash, _) = fetch_disk(
                shared,
                op,
                s,
                cancel,
                client,
                uuid,
                &file,
                Sink::Tar(&mut tar),
                (from, to, *expected),
            )
            .await?;
            tar.end().map_err(file_error)?;
            manifest.push((file, hash));
        }
        if spec.manifest {
            step(shared, op, OperationStep::CreatingManifest);
            let text = manifest_text(&manifest);
            tar.entry(&format!("{}.mf", spec.name), text.as_bytes())
                .map_err(file_error)?;
        }
        let file = tar.finish().map_err(file_error)?;
        file.sync_all().map_err(file_error)?;
        drop(file);
        if spec.verify {
            verify_ova(shared, op, cancel, &ova_path, &described)?;
        }
        if spec.compress {
            step(
                shared,
                op,
                OperationStep::ArchivingPackage {
                    file: format!("{}.ova.gz", spec.name),
                },
            );
            let gz_path = out.dir.join(format!("{}.ova.gz", spec.name));
            out.written.push(gz_path.clone());
            gzip_file(&ova_path, &gz_path, cancel).map_err(|e| match e {
                Some(e) => file_error(e),
                None => TaskOutcome::Cancelled,
            })?;
            let _ = std::fs::remove_file(&ova_path);
        }
        return Ok(());
    }
    // A folder: the disks, then the descriptor with their sizes.
    for (n, (i, j, uuid, expected, label)) in disks.iter().enumerate() {
        let file = described[*i].disks[*j].file.clone();
        step(
            shared,
            op,
            OperationStep::ExportingDisk {
                disk: label.clone(),
                file: file.clone(),
                index: n + 1,
                count,
            },
        );
        let path = out.dir.join(&file);
        let f = std::fs::File::create(&path).map_err(file_error)?;
        out.written.push(path.clone());
        let sink = if gz_disks {
            Sink::Gz(flate2::write::GzEncoder::new(
                f,
                flate2::Compression::default(),
            ))
        } else {
            Sink::File(f)
        };
        let from = 0.8 * done as f64 / total as f64;
        done += expected;
        let to = 0.8 * done as f64 / total as f64;
        fetch_disk(
            shared,
            op,
            s,
            cancel,
            client,
            uuid,
            &file,
            sink,
            (from, to, *expected),
        )
        .await?;
        let size = std::fs::metadata(&path).map_err(file_error)?.len();
        described[*i].disks[*j].file_size = Some(size);
        if spec.verify {
            step(
                shared,
                op,
                OperationStep::VerifyingDisk {
                    disk: label.clone(),
                },
            );
            verify_disk(&path, 0, size, gz_disks, label)?;
        }
    }
    let xml = ovf::export_descriptor(&spec.name, &described, &spec.eulas);
    let ovf_path = out.dir.join(&descriptor_name);
    std::fs::write(&ovf_path, &xml).map_err(file_error)?;
    out.written.push(ovf_path);
    shared.update_operation(op, |o| o.progress = Some(0.85));
    if spec.manifest {
        // The files hashed again from disk, as the original does.
        let mut lines = vec![(descriptor_name, sha(xml.as_bytes()))];
        for v in &described {
            for d in &v.disks {
                if cancel.is_set() {
                    return Err(TaskOutcome::Cancelled);
                }
                step(
                    shared,
                    op,
                    OperationStep::CalculatingChecksum {
                        file: d.file.clone(),
                    },
                );
                let hash = hash_file(&out.dir.join(&d.file)).map_err(file_error)?;
                lines.push((d.file.clone(), hash));
            }
        }
        step(shared, op, OperationStep::CreatingManifest);
        let mf = out.dir.join(format!("{}.mf", spec.name));
        std::fs::write(&mf, manifest_text(&lines)).map_err(file_error)?;
        out.written.push(mf);
    }
    Ok(())
}

fn manifest_text(lines: &[(String, Vec<u8>)]) -> String {
    lines
        .iter()
        .map(|(n, h)| format!("{}\n", ovf_package::manifest_line(n, h)))
        .collect()
}

fn hash_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().to_vec())
}

/// Gzips `from` into `to` (`Err(None)`: cancelled).
fn gzip_file(from: &Path, to: &Path, cancel: &CancelFlag) -> Result<(), Option<std::io::Error>> {
    let mut input = std::fs::File::open(from).map_err(Some)?;
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(to).map_err(Some)?,
        flate2::Compression::default(),
    );
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.is_set() {
            return Err(None);
        }
        let n = input.read(&mut buf).map_err(Some)?;
        if n == 0 {
            break;
        }
        gz.write_all(&buf[..n]).map_err(Some)?;
    }
    gz.finish().map_err(Some)?.sync_all().map_err(Some)
}

/// "Verify export on completion" of one disk file (or archive entry).
fn verify_disk(
    path: &Path,
    offset: u64,
    len: u64,
    gz: bool,
    label: &str,
) -> Result<(), TaskOutcome> {
    let failed = |why: String| {
        tracing::warn!(disk = label, error = %why, "disk verification failed");
        TaskOutcome::Failed(client_error(errors::OVF_DISK_VERIFY, &[label]))
    };
    let mut f = std::fs::File::open(path).map_err(file_error)?;
    f.seek(std::io::SeekFrom::Start(offset))
        .map_err(file_error)?;
    let r = f.take(len);
    if gz {
        // The expanded size is not known: read it all once to learn it.
        let mut count = flate2::read::GzDecoder::new(r);
        let size =
            std::io::copy(&mut count, &mut std::io::sink()).map_err(|e| failed(e.to_string()))?;
        let mut f = std::fs::File::open(path).map_err(file_error)?;
        f.seek(std::io::SeekFrom::Start(offset))
            .map_err(file_error)?;
        let mut d = flate2::read::GzDecoder::new(f.take(len));
        disk_image::check_vhd(&mut d, size).map_err(failed)?;
    } else {
        let mut r = r;
        disk_image::check_vhd(&mut r, len).map_err(failed)?;
    }
    Ok(())
}

fn verify_ova(
    shared: &Shared,
    op: OperationId,
    cancel: &CancelFlag,
    path: &Path,
    described: &[ExportedVm],
) -> Result<(), TaskOutcome> {
    let entries = crate::ova::index(path).map_err(export_error)?;
    for d in described.iter().flat_map(|v| v.disks.iter()) {
        if cancel.is_set() {
            return Err(TaskOutcome::Cancelled);
        }
        step(
            shared,
            op,
            OperationStep::VerifyingDisk {
                disk: d.name.clone(),
            },
        );
        let Some(e) = entries.iter().find(|e| e.name == d.file) else {
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_DISK_VERIFY,
                &[&d.name],
            )));
        };
        verify_disk(path, e.offset, e.size, false, &d.name)?;
    }
    Ok(())
}

// ---- import -----------------------------------------------------------------------------

/// What an import made (removed if it fails).
#[derive(Default)]
struct Made {
    vms: Vec<Ref<Vm>>,
    vdis: Vec<Ref<Vdi>>,
}

async fn remove_made(shared: &Shared, op: OperationId, s: &Session, made: &Made) {
    if made.vms.is_empty() && made.vdis.is_empty() {
        return;
    }
    step(shared, op, OperationStep::RemovingImported);
    for vm in &made.vms {
        if let Err(e) = xapi::vm::destroy(s, vm).await {
            tracing::warn!(error = %e, "VM.destroy of a half-imported VM failed");
        }
    }
    for vdi in &made.vdis {
        // The disk may still be busy with the cut upload for a moment.
        for attempt in 0..10 {
            match xapi::vdi::destroy(s, vdi).await {
                Ok(()) => break,
                Err(e) if e.is_api("HANDLE_INVALID") => break,
                Err(e) => {
                    if attempt == 9 {
                        tracing::warn!(error = %e, "VDI.destroy of a half-imported disk failed");
                    }
                    tokio::time::sleep(shared.options.task_poll_interval).await;
                }
            }
        }
    }
}

/// `ImportApplianceAction` / `ImportImageAction`.
async fn import(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    cancel: &Arc<CancelFlag>,
    spec: &OvfImport,
) -> Result<(), TaskOutcome> {
    let path = Path::new(&spec.path);
    let package = match &spec.image {
        Some(env) => Package::image(path, (**env).clone()),
        None => match Package::open(path) {
            Ok(p) => p,
            Err(e) => {
                return Err(TaskOutcome::Failed(client_error(
                    errors::OVF_PACKAGE,
                    &[&e.to_string()],
                )));
            }
        },
    };
    if package.envelope.encrypted {
        return Err(TaskOutcome::Failed(client_error(
            errors::OVF_ENCRYPTED,
            &[],
        )));
    }
    let verify = spec.verify_manifest || spec.verify_signature;
    if spec.verify_signature {
        step(shared, op, OperationStep::VerifyingSignature);
        match package.verify_signature() {
            Ok(sig) => tracing::info!(
                subject = %sig.subject,
                algorithm = %sig.algorithm,
                "the package signature matches its certificate (trust not checked)"
            ),
            Err(e) => {
                return Err(TaskOutcome::Failed(client_error(
                    errors::OVF_SIGNATURE,
                    &[&e.to_string()],
                )));
            }
        }
    }
    if verify && package.has_manifest() {
        step(shared, op, OperationStep::VerifyingManifest);
        let p = package.clone();
        let (sh, c) = (shared.clone(), cancel.clone());
        let checked = tokio::task::spawn_blocking(move || {
            p.verify_manifest(
                &|done, total| {
                    let f = 0.2 * done as f64 / total.max(1) as f64;
                    sh.update_operation(op, |o| o.progress = Some(f));
                },
                &|| c.is_set(),
            )
        })
        .await
        .unwrap_or(Err(PackageError::Cancelled));
        match checked {
            Ok(()) => {}
            Err(PackageError::Cancelled) => return Err(TaskOutcome::Cancelled),
            Err(e) => {
                return Err(TaskOutcome::Failed(client_error(
                    errors::OVF_MANIFEST,
                    &[&e.to_string()],
                )));
            }
        }
    }
    let mut made = Made::default();
    let result = import_systems(shared, connection, op, s, cancel, spec, &package, &mut made).await;
    if let Err(outcome) = result {
        let outcome = if cancel.is_set() {
            TaskOutcome::Cancelled
        } else {
            outcome
        };
        remove_made(shared, op, s, &made).await;
        shared.update_operation(op, |o| o.created = None);
        return Err(outcome);
    }
    step(
        shared,
        op,
        OperationStep::ImportComplete { template: false },
    );
    shared.update_operation(op, |o| o.progress = Some(1.0));
    if spec.start {
        for vm in &made.vms {
            shared.send(Command::Action {
                connection,
                request: ActionRequest::Vm {
                    vm: vm.clone(),
                    action: VmAction::Start,
                },
            });
        }
    }
    Ok(())
}

/// A disk to upload: where its bytes are, how many, in which format.
struct Upload {
    file: PathBuf,
    offset: u64,
    length: u64,
    format: ImageFormat,
    virtual_size: u64,
    /// A temporary expanded copy, removed afterwards.
    temporary: bool,
}

/// Opens the disk file `href` of the package, expanding a gzipped one
/// first, and looks at what it holds.
fn prepare_disk(
    package: &Package,
    href: &str,
    gz: bool,
    cancel: &CancelFlag,
) -> Result<Upload, TaskOutcome> {
    let missing = || TaskOutcome::Failed(client_error(errors::OVF_FILE_MISSING, &[href]));
    let file_error = |e: &dyn std::fmt::Display| {
        TaskOutcome::Failed(client_error(
            errors::OVF_FILE_ERROR,
            &[href, &e.to_string()],
        ))
    };
    let pf = package.open_file(href).map_err(|_| missing())?;
    let (file, offset, len, temporary) = if gz {
        let dir = match &package.layout {
            ovf_package::Layout::Folder(d) => d.clone(),
            ovf_package::Layout::Ova(p, _) => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
        };
        let name = href.rsplit('/').next().unwrap_or(href);
        let out = dir.join(format!("unc_{}", name.trim_end_matches(".gz")));
        let mut d = flate2::read::GzDecoder::new(pf.reader().map_err(|e| file_error(&e))?);
        let mut w = std::fs::File::create(&out).map_err(|e| file_error(&e))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if cancel.is_set() {
                let _ = std::fs::remove_file(&out);
                return Err(TaskOutcome::Cancelled);
            }
            let n = match d.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&out);
                    return Err(file_error(&e));
                }
            };
            if n == 0 {
                break;
            }
            if let Err(e) = w.write_all(&buf[..n]) {
                let _ = std::fs::remove_file(&out);
                return Err(file_error(&e));
            }
        }
        let len = std::fs::metadata(&out).map_err(|e| file_error(&e))?.len();
        (out, 0, len, true)
    } else {
        let p = match &package.layout {
            ovf_package::Layout::Ova(p, _) => p.clone(),
            ovf_package::Layout::Folder(d) => {
                if Path::new(href).is_absolute() {
                    PathBuf::from(href)
                } else {
                    d.join(href)
                }
            }
        };
        (p, pf.offset, pf.len, false)
    };
    let image = std::fs::File::open(&file)
        .and_then(|mut f| disk_image::inspect_at(&mut f, offset, len))
        .map_err(|e| file_error(&e))?;
    match image {
        Image::Supported {
            format,
            virtual_size,
            file_size,
        } => Ok(Upload {
            file,
            offset,
            length: format.upload_length(virtual_size, file_size),
            format,
            virtual_size,
            temporary,
        }),
        Image::Unsupported(kind) => {
            if temporary {
                let _ = std::fs::remove_file(&file);
            }
            Err(TaskOutcome::Failed(client_error(
                errors::OVF_UNSUPPORTED_DISK,
                &[href, kind.name()],
            )))
        }
    }
}

/// `CreateVm`: the record of the VM a virtual system describes.
fn vm_record(
    env: &Envelope,
    system: &ovf::VirtualSystem,
    mapping: &VmMapping,
    image: bool,
) -> VmRecord {
    let hw = ovf::hardware(system).cloned().unwrap_or_default();
    let h = ovf::vm_hardware(&hw);
    let mut r = VmRecord {
        name_label: if mapping.name.is_empty() {
            "Imported VM".into()
        } else {
            mapping.name.clone()
        },
        name_description: if hw.vssd.description.is_empty() {
            "Imported from an OVF/OVA package.".into()
        } else {
            hw.vssd.description.clone()
        },
        user_version: 1,
        power_state: VmPowerState::Halted,
        memory_target: h.memory,
        memory_static_max: h.memory,
        memory_dynamic_max: h.memory,
        memory_dynamic_min: h.memory,
        memory_static_min: h.memory,
        vcpus_max: h.vcpus_max,
        vcpus_at_startup: h.vcpus_at_startup,
        actions_after_softreboot: OnSoftrebootBehavior::SoftReboot,
        actions_after_shutdown: OnNormalExit::Destroy,
        actions_after_reboot: OnNormalExit::Restart,
        actions_after_crash: OnCrashBehaviour::Restart,
        hvm_shadow_multiplier: 1.0,
        other_config: HashMap::from([("HideFromXenCenter".to_owned(), "true".to_owned())]),
        affinity: mapping.affinity.clone().unwrap_or_else(Ref::null),
        snapshot_time: xapi::DateTime("19700101T00:00:00Z".to_owned()),
        ..Default::default()
    };
    let _ = image;
    if h.other_config.is_empty() {
        r.hvm_boot_policy = "BIOS order".into();
        r.hvm_boot_params = HashMap::from([("order".to_owned(), "dc".to_owned())]);
        r.platform = ovf::split_pairs("nx=true;acpi=true;apic=true;pae=true;stdvga=0;");
    }
    for (k, v) in &h.other_config {
        match k.as_str() {
            "hvm_boot_policy" => r.hvm_boot_policy = v.clone(),
            "hvm_boot_params" => r.hvm_boot_params = ovf::split_pairs(v),
            "platform" => {
                r.platform = ovf::split_pairs(v);
                r.platform
                    .entry("nx".to_owned())
                    .or_insert_with(|| "true".to_owned());
            }
            "nvram" => r.nvram = ovf::split_pairs(v),
            "pv_args" => r.pv_args = v.clone(),
            "pv_bootloader" => r.pv_bootloader = v.clone(),
            "pv_bootloader_args" => r.pv_bootloader_args = v.clone(),
            "pv_kernel" => r.pv_kernel = v.clone(),
            "pv_legacy_args" => r.pv_legacy_args = v.clone(),
            "pv_ramdisk" => r.pv_ramdisk = v.clone(),
            "hvm_shadow_multiplier" => {
                r.hvm_shadow_multiplier = v.parse().unwrap_or(1.0);
            }
            "hardware_platform_version" => {
                r.hardware_platform_version = v.parse().unwrap_or(0);
            }
            "recommendations" => r.recommendations = v.clone(),
            "reference_label" => r.reference_label = v.clone(),
            "vm_has_vendor_device" | "has_vendor_device" => {
                r.has_vendor_device = v.eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }
    r.domain_type = if r.hvm_boot_policy.is_empty() {
        DomainType::Pv
    } else {
        DomainType::Hvm
    };
    if let Some(start) = env.startup.iter().find(|i| i.id == system.id) {
        r.order = start.order;
        r.start_delay = start.start_delay;
        r.shutdown_delay = start.stop_delay;
    }
    r
}

/// The device for an item: the one it asks for when free, else the first
/// free one (`VerifyUserDevice`).
async fn free_device(
    s: &Session,
    vm: &Ref<Vm>,
    wanted: Option<String>,
) -> xapi::Result<Option<String>> {
    let allowed = xapi::vm::get_allowed_vbd_devices(s, vm).await?;
    Ok(match wanted {
        Some(w) if allowed.contains(&w) => Some(w),
        _ => allowed.first().cloned(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn import_systems(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    cancel: &Arc<CancelFlag>,
    spec: &OvfImport,
    package: &Package,
    made: &mut Made,
) -> Result<(), TaskOutcome> {
    let env = &package.envelope;
    let m = model(shared, connection);
    // The bytes to upload, for the progress.
    let mut total = 0u64;
    for mapping in &spec.vms {
        if let Some(sys) = env.system(&mapping.system)
            && let Some(hw) = ovf::hardware(sys)
        {
            for r in hw.items.iter().filter(|r| r.is_disk()) {
                if let Some((d, f)) = env.disk_of(r) {
                    total += f.size.unwrap_or(d.capacity).max(1);
                }
            }
        }
    }
    let total = total.max(1);
    let base = if spec.verify_manifest || spec.verify_signature {
        0.2
    } else {
        0.0
    };
    let mut done = 0u64;
    let default_sr = m.pool().map(|(_, p)| p.default_sr.clone());
    let disk_count: usize = spec
        .vms
        .iter()
        .filter_map(|v| env.system(&v.system))
        .filter_map(ovf::hardware)
        .map(|h| h.items.iter().filter(|r| r.is_disk()).count())
        .sum();
    let mut disk_index = 0;
    for mapping in &spec.vms {
        if cancel.is_set() {
            return Err(TaskOutcome::Cancelled);
        }
        let Some(system) = env.system(&mapping.system) else {
            continue;
        };
        step(
            shared,
            op,
            OperationStep::CreatingVm {
                vm: mapping.name.clone(),
            },
        );
        let record = vm_record(env, system, mapping, spec.image.is_some());
        let vm = tri!(xapi::vm::create(s, &record).await);
        made.vms.push(vm.clone());
        if made.vms.len() == 1 {
            shared.update_operation(op, |o| o.created = Some(vm.clone()));
        }
        let hw = ovf::hardware(system).cloned().unwrap_or_default();
        let mut vif_device = 0;
        let mut has_cd = false;
        for item in &hw.items {
            if cancel.is_set() {
                return Err(TaskOutcome::Cancelled);
            }
            match item.resource_type {
                ovf::resource::NIC => {
                    let Some(net) = mapping
                        .networks
                        .iter()
                        .find(|(id, _)| *id == item.instance_id)
                        .map(|(_, n)| n.clone())
                    else {
                        continue;
                    };
                    let mtu = m.networks.get(&net).map_or(1500, |n| n.mtu);
                    let mac = ovf::mac_with_colons(&item.address);
                    let mac = if valid_mac(&mac) { mac } else { String::new() };
                    let rec = crate::provision_run::vif_record(
                        &vm,
                        &net,
                        vif_device.to_string(),
                        &mac,
                        mtu,
                    );
                    vif_device += 1;
                    tri!(xapi::vif::create(s, &rec).await);
                }
                _ if item.is_cd() => {
                    if has_cd {
                        continue;
                    }
                    has_cd = true;
                    let wanted = ovf::wanted_device(item).or(Some("3".to_owned()));
                    let Some(device) = tri!(free_device(s, &vm, wanted).await) else {
                        tracing::warn!("no device left for the CD drive");
                        continue;
                    };
                    step(shared, op, OperationStep::AttachedCd);
                    let rec = cd_record(&vm, &device);
                    tri!(xapi::vbd::create(s, &rec).await);
                }
                _ if item.is_disk() => {
                    let Some((disk, file)) = env.disk_of(item) else {
                        continue;
                    };
                    disk_index += 1;
                    let sr = mapping
                        .srs
                        .iter()
                        .find(|(id, _)| *id == item.instance_id)
                        .map(|(_, sr)| sr.clone())
                        .or_else(|| default_sr.clone().filter(|r| m.srs.contains_key(r)));
                    let Some(sr) = sr else {
                        return Err(TaskOutcome::Failed(client_error(errors::OVF_NO_SR, &[])));
                    };
                    let gz = file
                        .compression
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case("gzip"));
                    if gz {
                        step(
                            shared,
                            op,
                            OperationStep::ExpandingFile {
                                file: file.href.clone(),
                            },
                        );
                    }
                    let up = {
                        let (p, h, c) = (package.clone(), file.href.clone(), cancel.clone());
                        tokio::task::spawn_blocking(move || prepare_disk(&p, &h, gz, &c))
                            .await
                            .unwrap_or(Err(TaskOutcome::Cancelled))?
                    };
                    let expected = file.size.unwrap_or(disk.capacity).max(1);
                    let from = base + (1.0 - base) * done as f64 / total as f64;
                    done += expected;
                    let to = base + (1.0 - base) * done as f64 / total as f64;
                    let uploaded = upload_disk(
                        shared,
                        connection,
                        op,
                        s,
                        cancel,
                        &vm,
                        &mapping.name,
                        item,
                        disk,
                        &up,
                        &sr,
                        ovf::disk_bootable(env, item),
                        (disk_index, disk_count, from, to),
                        made,
                    )
                    .await;
                    if up.temporary {
                        let _ = std::fs::remove_file(&up.file);
                    }
                    uploaded?;
                }
                _ => {}
            }
        }
        tri!(xapi::vm::remove_from_other_config(s, &vm, "HideFromXenCenter").await);
    }
    Ok(())
}

fn cd_record(vm: &Ref<Vm>, device: &str) -> xapi::VbdRecord {
    xapi::VbdRecord {
        vm: vm.clone(),
        vdi: Ref::null(),
        userdevice: device.to_owned(),
        bootable: false,
        mode: xapi::VbdMode::Ro,
        r#type: xapi::VbdType::Cd,
        unpluggable: true,
        empty: true,
        other_config: HashMap::from([("owner".to_owned(), "true".to_owned())]),
        ..Default::default()
    }
}

/// One disk: the VDI, the upload, the drive.
#[allow(clippy::too_many_arguments)]
async fn upload_disk(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    cancel: &Arc<CancelFlag>,
    vm: &Ref<Vm>,
    vm_name: &str,
    item: &Rasd,
    disk: &ovf::DiskDef,
    up: &Upload,
    sr: &Ref<Sr>,
    bootable: bool,
    (index, count, from, to): (usize, usize, f64, f64),
    made: &mut Made,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let device = ovf::wanted_device(item);
    let label = if item.element_name.is_empty() {
        format!("{vm_name}_{}", device.clone().unwrap_or_default())
    } else {
        item.element_name.clone()
    };
    let size = disk_image::round_up_mib(disk.capacity.max(up.virtual_size));
    step(
        shared,
        op,
        OperationStep::PreparingVdi {
            disk: label.clone(),
        },
    );
    if let Some(r) = m.srs.get(sr) {
        let free = (r.physical_size - r.physical_utilisation).max(0) as u64;
        if r.physical_size > 0 && size > free {
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_SR_SPACE,
                &[
                    &r.name_label,
                    &crate::format::disk_size(size as i64),
                    &label,
                ],
            )));
        }
    }
    let rec = xapi::VdiRecord {
        name_label: label.clone(),
        name_description: item.description.clone(),
        sr: sr.clone(),
        virtual_size: size as i64,
        r#type: VdiType::User,
        sharable: false,
        read_only: false,
        managed: true,
        on_boot: xapi::OnBoot::Persist,
        snapshot_time: xapi::DateTime("19700101T00:00:00Z".to_owned()),
        ..Default::default()
    };
    let vdi = tri!(xapi::vdi::create(s, &rec).await);
    made.vdis.push(vdi.clone());
    let uuid = tri!(xapi::vdi::get_uuid(s, &vdi).await);
    // `SR.GetStorageHost`, else the coordinator.
    let host = m
        .srs
        .get(sr)
        .and_then(|r| crate::objects::sr_home(&m, r))
        .cloned()
        .or_else(|| m.pool().map(|(_, p)| p.master.clone()))
        .unwrap_or_default();
    let (client, _) = tri!(crate::pool_run::transfer_client(
        shared,
        connection,
        &host,
        shared.options.transfer_timeout,
    ));
    let task = tri!(
        xapi::task::create(
            s,
            "import_raw_vdi_task",
            &format!("Importing disk {} to VDI {uuid}", up.file.display()),
        )
        .await
    );
    let file_name = up
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    step(
        shared,
        op,
        OperationStep::ImportingDisk {
            file: file_name.clone(),
            index,
            count,
        },
    );
    let mut file = match tokio::fs::File::open(&up.file).await {
        Ok(f) => f,
        Err(e) => {
            let _ = xapi::task::destroy(s, &task).await;
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_FILE_ERROR,
                &[&file_name, &e.to_string()],
            )));
        }
    };
    if let Err(e) =
        tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(up.offset)).await
    {
        let _ = xapi::task::destroy(s, &task).await;
        return Err(TaskOutcome::Failed(client_error(
            errors::OVF_FILE_ERROR,
            &[&file_name, &e.to_string()],
        )));
    }
    let length = up.length;
    let reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
        match shared.options.transfer_faults.short_read_after {
            Some(n) => Box::new(file.take(n.min(length))),
            None => Box::new(file.take(length)),
        };
    let sent = {
        let shared = shared.clone();
        Arc::new(move |bytes: u64| {
            let f = (bytes as f64 / length.max(1) as f64).min(1.0);
            shared.update_operation(op, |o| o.progress = Some(from + (to - from) * f));
        }) as Arc<dyn Fn(u64) + Send + Sync>
    };
    let cancelled = {
        let cancel = cancel.clone();
        Arc::new(move || cancel.is_set()) as Arc<dyn Fn() -> bool + Send + Sync>
    };
    let session_ref = s.session_ref().as_str().to_owned();
    let params = [
        ("session_id", session_ref.as_str()),
        ("task_id", task.as_str()),
        ("vdi", uuid.as_str()),
        ("format", up.format.transfer_format()),
    ];
    let buffer = shared.options.transfer_buffer.max(1);
    let uploaded = tokio::select! {
        r = client.put_reader("import_raw_vdi", &params, reader, length, buffer, sent, cancelled) => r,
        () = cancel.wait() => Err(xapi::Error::Cancelled),
    };
    let followed = end_task(shared, op, s, &task, uploaded.is_ok(), to).await;
    if cancel.is_set() {
        return Err(TaskOutcome::Cancelled);
    }
    match (uploaded, followed) {
        (Ok(()), TaskOutcome::Succeeded) => {}
        (Ok(()), other) => return Err(other),
        (Err(xapi::Error::Cancelled), _) => return Err(TaskOutcome::Cancelled),
        (Err(xapi::Error::Io(e)), _) => {
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_FILE_ERROR,
                &[&file_name, &e.to_string()],
            )));
        }
        (Err(_), TaskOutcome::Failed(server)) => return Err(TaskOutcome::Failed(server)),
        (Err(e), _) => {
            return Err(TaskOutcome::Failed(client_error(
                errors::OVF_IMPORT_FAILED,
                &[&file_name, &e.to_string()],
            )));
        }
    }
    let Some(device) = tri!(free_device(s, vm, device).await) else {
        tracing::warn!("no device left for the disk; it is not attached");
        return Ok(());
    };
    let bootable = bootable || device == "0";
    let rec = xapi::VbdRecord {
        vm: vm.clone(),
        vdi: vdi.clone(),
        userdevice: device,
        bootable,
        mode: xapi::VbdMode::Rw,
        r#type: xapi::VbdType::Disk,
        unpluggable: true,
        empty: false,
        other_config: HashMap::from([("owner".to_owned(), "true".to_owned())]),
        ..Default::default()
    };
    tri!(xapi::vbd::create(s, &rec).await);
    Ok(())
}

/// `aa:bb:cc:dd:ee:ff`.
fn valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}
