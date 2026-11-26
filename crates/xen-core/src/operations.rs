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
use xapi::{Host, Ref, Session, Sr, Task, TaskStatusType, Vm, VmOperations};

use crate::actions::{self, ActionRequest, HostAction, PerfmonObjectRef, VmAction};
use crate::backend::{ConnectionError, ConnectionId, Notification, OperationId, Shared};
use crate::model::ConnectionModel;
use crate::objects;
use crate::provisioning::CopyMode;
use crate::storage::{
    DeviceConfig, IqnInfo, IscsiScan, LunInfo, NewSrMode, ProbeResult, Secret, SrAction,
};

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
    /// A VM creation, template, snapshot or disk operation.
    Provision(ProvisionKind),
    /// A storage repository operation.
    Storage(StorageKind),
    /// A networking operation.
    Network(NetworkKind),
    /// A pool, server settings or server maintenance operation (F10a).
    Pool(PoolKind),
    /// A high availability operation (F10b).
    Ha(HaKind),
    /// An Active Directory operation (F10b).
    Ad(AdKind),
    /// An XVA export or import (F11a).
    Transfer(TransferKind),
    /// A migration, copy or move of the F12a wizard.
    Migrate(MigrateKind),
    /// A vApp operation (F12a).
    VApp(VAppKind),
    /// A snapshot schedule operation (F12b).
    Vmss(VmssKind),
    /// A disaster recovery operation (F12b).
    Dr(DrKind),
    /// A folder, tag, custom field or saved search operation (F13).
    Org(OrgKind),
}

/// The history entry of a folder, tag, custom field or saved search
/// operation (F13), with the names its title shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgKind {
    /// `MOVE_OBJECT_TO_FOLDER` / `MOVE_OBJECTS_TO_FOLDER`.
    MoveToFolder {
        object: Option<String>,
        folder: String,
    },
    /// `DELETING_FOLDER` / `DELETING_FOLDERS` (also "Remove from folder").
    DeleteFromFolders { count: usize },
    /// `RENAMING_FOLDER`.
    RenameFolder { folder: String, name: String },
    /// `CREATE_NEW_FOLDER` / `CREATE_NEW_FOLDERS`.
    NewFolders { paths: Vec<String> },
    /// `ADD_TAG`.
    AddTag { tag: String },
    /// `DELETE_TAG` (untag).
    RemoveTag { tag: String },
    /// `RENAME_TAG`.
    RenameTag { old: String, new: String },
    /// `DELETE_TAG`, everywhere.
    DeleteTag { tag: String },
    /// `ACTION_SAVE_FOLDER_TAGS`.
    FolderAndTags { object: String },
    /// `ADD_CUSTOM_FIELD`.
    AddCustomField { name: String },
    /// `DELETE_CUSTOM_FIELD`.
    RemoveCustomField { name: String },
    /// `ACTION_SAVE_CUSTOM_FIELDS`.
    SaveCustomFields { object: String },
    /// Saving a search on the pool.
    SaveSearch { name: String },
    /// Deleting a saved search.
    DeleteSearch { name: String },
    /// The VM Properties dialog's name and description (`VM` properties).
    VmNames { name: String },
}

/// The history entry of a snapshot schedule operation (F12b), with the
/// names its title shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmssKind {
    /// `CREATE_POLICY` (`CREATING_VMSS`).
    Create { name: String },
    /// `CHANGE_POLICY_STATUS`.
    SetEnabled { name: String, enabled: bool },
    /// `RUN_POLICY`.
    RunNow { name: String },
    /// `DELETE_POLICIES`.
    Delete,
    /// `ASSIGN_VMSS_POLICY_NOAMP`.
    Assign { policy: String },
    /// `REMOVE_VM_FROM_VMSS` / `REMOVE_VMS_FROM_VMSS`.
    Remove { vm: Option<String>, policy: String },
    /// The Properties dialog.
    Properties { name: String },
}

/// The history entry of a disaster recovery operation (F12b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrKind {
    /// `ACTION_DR_CONFIGURING`.
    Configure,
    /// `ACTION_DR_TASK_CREATE_TITLE`.
    TaskCreate,
    /// `ACTION_DR_TASK_DESTROY_TITLE`.
    TaskDestroy { srs: String },
    /// `ACTION_DR_RECOVER_VM_TITLE` / `ACTION_DR_RECOVER_APPLIANCE_TITLE`.
    Recover { name: String, vapp: bool },
    /// `ACTION_START_VMS_AND_APPLIANCES_TITLE`.
    Start,
    /// `ACTION_SHUTDOWN_AND_DESTROY_VM(S)_TITLE`.
    DestroyVms { count: usize },
    /// `DELETE_VM_APPLIANCES`.
    DestroyVApps,
    /// `VM_APPLIANCE_SHUTTING_DOWN` (forced).
    HardShutDownVApp { name: String },
}

/// The history entry of a migration, copy or move (F12a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateKind {
    /// `ACTION_VM_MIGRATING_RESIDENT` / `ACTION_VM_MIGRATING_NON_RESIDENT`.
    Migrate {
        vm: String,
        from: Option<String>,
        to: String,
    },
    /// `ACTION_VM_CROSS_POOL_COPY_TITLE`.
    Copy {
        vm: String,
        pool: String,
        to: String,
    },
    /// `ACTION_VM_MOVING_HOST`.
    MoveHost {
        vm: String,
        from: String,
        to: String,
    },
    /// `ACTION_VM_MOVING_SR`.
    MoveSr { vm: String, sr: String },
    /// `ACTION_VM_MOVING`.
    Move { vm: String },
}

/// The history entry of a vApp operation (F12a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VAppKind {
    /// `CREATING_VM_APPLIANCE`.
    Create { name: String },
    /// The vApp Properties dialog.
    Properties { name: String },
    /// `ASSIGN_VM_TO_VAPP` / `ASSIGN_VMS_TO_VAPP` (`vm` when one).
    Assign { vm: Option<String>, vapp: String },
    /// `REMOVE_VM_FROM_APPLIANCE` / `REMOVE_VMS_FROM_APPLIANCE`.
    Remove { vm: Option<String>, vapp: String },
    /// `DELETE_VM_APPLIANCES` (one name when one).
    Delete { name: Option<String> },
    /// `VM_APPLIANCE_STARTING`.
    Start { name: String },
    /// `VM_APPLIANCE_SHUTTING_DOWN`.
    ShutDown { name: String },
}

/// The XVA transfers, with the names their titles show (the original's
/// action titles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferKind {
    /// "Export VM '{vm}' from '{pool}'".
    Export { vm: String, pool: String },
    /// "Import VM from '{file}' to '{host} in '{pool}''" (`in_pool`: a
    /// pool, else "on" a standalone server).
    Import {
        file: String,
        host: String,
        pool: String,
        in_pool: bool,
    },
    /// "Export OVF package '{name}' from '{pool}'" (or OVA).
    ExportOvf {
        name: String,
        pool: String,
        ova: bool,
    },
    /// "Import Appliance '{name}' to '{pool}'" (`file`: the package).
    ImportAppliance {
        name: String,
        file: String,
        pool: String,
    },
    /// "Import VM '{vm}' from Disk Image to '{pool}'" (`file`: the image).
    ImportImage {
        vm: String,
        file: String,
        pool: String,
    },
}

/// The HA operations, with the names their titles show (the original's
/// action titles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaKind {
    /// "Enabling HA on pool '{pool}'".
    Enable { pool: String },
    /// "Disabling HA on pool '{pool}'".
    Disable { pool: String },
    /// "Setting HA restart priorities".
    SetPriorities,
    /// "Changing the number of server failures to tolerate on '{pool}'".
    SetNtol { pool: String },
}

/// The Active Directory operations, with the names their titles show.
/// `pool` is `Helpers.GetName(connection)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdKind {
    /// "Enabling AD on '{pool}'".
    Enable { pool: String },
    /// "Disabling AD on '{pool}'".
    Disable { pool: String },
    /// "Adding/removing users on '{pool}'".
    AddRemove { pool: String },
    /// "Changing roles for {subjects}".
    Roles { subjects: Vec<String> },
    /// "Terminating session for {subjects}".
    Logout { subjects: Vec<String> },
}

/// The pool and server operations of F10a, with the names their titles
/// show (the original's action titles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolKind {
    /// "Creating new pool '{name}' with coordinator '{coordinator}'".
    CreatePool { name: String, coordinator: String },
    /// "Adding server '{host}' to pool '{pool}'".
    AddHost { host: String, pool: String },
    /// "Removing server '{host}' from pool '{pool}'".
    EjectHost { host: String, pool: String },
    /// "Making pool '{pool}' into standalone server".
    DestroyPool { pool: String },
    /// "Saving properties of {name}".
    Properties { name: String },
    /// "Changing control domain memory settings on '{host}'".
    Dom0Memory { host: String },
    /// "Changing administrator password on {pool}".
    ChangePassword { pool: String },
    /// "Rotating pool secret".
    RotateSecret,
    /// "Enabling certificate verification on {pool}...".
    EnableTls { pool: String },
    /// "Installing certificate on server {host}...".
    InstallCertificate { host: String },
    /// "Resetting certificate on server {host}...".
    ResetCertificate { host: String },
    /// "Backing up server '{host}'".
    Backup { host: String },
    /// "Restoring server '{host}'".
    Restore { host: String },
}

/// The networking operations, with the names their titles show (the
/// original's action titles). `pool` is the pool's name
/// (`Helpers.GetName(connection)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkKind {
    /// "Creating network '{name}' on {pool}".
    CreateNetwork { name: String, pool: String },
    /// "Removing network '{name}' from {pool}".
    RemoveNetwork { name: String, pool: String },
    /// "Updating network '{name}' on {pool}" (the Properties dialog).
    UpdateNetwork { name: String, pool: String },
    /// "Creating {bond}".
    CreateBond { name: String },
    /// "Destroying {bond}".
    DestroyBond { name: String },
    /// "Scanning NICs on server '{host}'".
    RescanPifs { host: String },
    /// "Reconfiguring server networking".
    ChangeNetworking,
    /// "Renaming server network interface".
    RenameInterfaces,
    /// "Creating new virtual network interface on VM {vm}".
    NewVif { vm: String },
    /// "Deleting virtual network interface '{network}' from VM '{vm}'".
    DeleteVif { network: String, vm: String },
    /// "Updating virtual network interface '{network}' on VM '{vm}'".
    UpdateVif { network: String, vm: String },
    /// "Activating virtual network interface on VM '{vm}'".
    PlugVif { vm: String },
    /// "Deactivating virtual network interface on VM '{vm}'".
    UnplugVif { vm: String },
}

/// The SR operations, with the names their titles show (the original's
/// action titles). `pool` is the pool's name (`Helpers.GetName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageKind {
    /// "Creating {type} SR '{name}' on '{pool}'".
    CreateSr {
        sr_type: String,
        name: String,
        pool: String,
    },
    /// "Attaching SR '{name}' to '{pool}'" (introduce or reattach).
    AttachSr { name: String, pool: String },
    /// An operation of the Storage menu on SR `name`.
    Sr {
        action: SrAction,
        name: String,
        pool: String,
    },
    /// Saving the SR Properties dialog.
    SrProperties { name: String },
}

/// The VM creation, template, snapshot and disk operations, with the names
/// their titles show (the original's action titles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionKind {
    /// "Create VM '{vm}'".
    CreateVm { vm: String, template: String },
    /// "Creating '{vm}' by cloning '{source}'".
    CloneVm { vm: String, source: String },
    /// "Copying VM '{source}' to '{vm}' on SR '{sr}'".
    CopyVm {
        vm: String,
        source: String,
        sr: String,
    },
    /// "Converting VM '{vm}' to template".
    MakeTemplate { vm: String },
    /// "Deleting VM '{vm}' from '{location}'".
    DeleteVm { vm: String, location: String },
    /// "Taking a snapshot of VM '{vm}'...".
    Snapshot { vm: String },
    /// "Revert snapshot '{snapshot}'".
    Revert { snapshot: String },
    /// "Deleting snapshot '{0}'..." / "Deleting snapshots".
    DeleteSnapshots { names: Vec<String> },
    /// "Creating disk '{disk}' on SR '{sr}'".
    NewDisk { disk: String, sr: String },
    /// "Attaching virtual disk '{disk}' to VM '{vm}'".
    AttachDisk { disk: String, vm: String },
    /// "Detaching disk '{disk}' from VM '{vm}'".
    DetachDisk { disk: String, vm: String },
    /// "Deleting disk '{disk}' from SR '{sr}'".
    DeleteDisk { disk: String, sr: String },
    /// "Changing disk size for '{disk}'...".
    ResizeDisk { disk: String },
    /// "Moving Virtual Disk '{disk}' from SR '{from}' to SR '{to}'".
    MoveDisk {
        disk: String,
        from: String,
        to: String,
    },
    /// "Loading ISO '{iso}' into the DVD Drive on VM {vm}" / "Setting DVD
    /// Drive on {vm} as empty.".
    ChangeCd { vm: String, iso: Option<String> },
    /// "Creating new DVD drive on VM {vm}".
    NewCdDrive { vm: String },
    /// "Installing {tools} on VM {vm}".
    InstallTools { vm: String },
}

/// Something the user must know once an operation succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationNotice {
    /// The new disk could not be plugged into the running VM: "You need to
    /// shut down and then restart the VM before it can access the new
    /// disk."
    RestartForDisk,
    /// Same for a new DVD drive.
    RestartForDrive,
    /// A new or changed interface could not be plugged into the running VM
    /// (`VIF_HOTPLUG_FAILED_MESSAGE`).
    RestartForVif,
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
    /// New VM: "Cloning template '{template}'" / copying it to an SR.
    CloningTemplate {
        template: String,
        sr: Option<String>,
    },
    /// New VM: "Provisioning VM".
    Provisioning,
    /// New VM: "Setting VM properties".
    SettingProperties,
    /// New VM: "Creating CD drive".
    CreatingCdDrive,
    /// New VM: "Creating disks".
    CreatingDisks,
    /// New VM: "Creating networks".
    CreatingNetworks,
    /// New VM: "Attaching a new vTPM".
    AttachingVtpm,
    /// New VM: "Starting VM".
    StartingVm,
    /// New VM: undoing what was made after a failure.
    RollingBack,
    /// Revert: "Reverting the power state of {vm}".
    RevertingPowerState { vm: String },
    /// New SR: "Creating SR".
    CreatingSr,
    /// New SR: "Attaching SR".
    AttachingSr,
    /// Attach / repair: "Creating PBD for {host}".
    CreatingPbd { host: String },
    /// Attach / repair: "Plugging PBD for {host}".
    PluggingPbd { host: String },
    /// Trim: "Reclaiming freed space".
    ReclaimingSpace,
    /// New SR / reattach: undoing what was made after a failure.
    UndoingSr,
    /// "Creating network".
    CreatingNetwork,
    /// "Removing network".
    RemovingNetwork,
    /// "Updating network".
    UpdatingNetwork,
    /// "Configuring IP address on {pif}...".
    BringingUp { pif: String },
    /// "Deconfiguring management facility on {pif}...".
    Depurposing { pif: String },
    /// "Configuring management interface on {pif}...".
    ManagementReconfiguring { pif: String },
    /// "Removing IP address from {pif}...".
    BringingDown { pif: String },
    /// "Waiting for other pool members to recover...".
    WaitingForMembers,
    /// "Scanning...".
    ScanningNics,
    /// "Refreshing networks...".
    RefreshingNetworks,
    /// New network or bond: undoing what was made after a failure.
    UndoingNetwork,
    /// New Pool: "Creating pool".
    CreatingPool,
    /// New Pool: "Adding members...".
    AddingMembers,
    /// Add Server: "Adding to Pool".
    Joining,
    /// Remove Server: "Removing from pool".
    Ejecting,
    /// New Pool: nobody could join; the pool of one is a standalone server
    /// again.
    UndoingPool,
    /// "Nominating server {host} as new coordinator...".
    NewCoordinator { host: String },
    /// Properties: "Saving settings...".
    SavingSettings,
    /// "Backing up server '{host}' ({received} received)...".
    BackingUp { received: i64 },
    /// Install Certificate: waiting for the server to present the new
    /// certificate.
    WaitingForCertificate,
    /// HA: "Setting HA restart priority on {vm}".
    SettingHaPriority { vm: String },
    /// HA: "Lowering the number of server failures to tolerate".
    LoweringNtol,
    /// Add Server: joining the pool's domain first.
    JoiningDomain,
    /// HA: "Setting HA restart priority on {vm} to 'Do not restart'".
    UnprotectingVm { vm: String },
    /// Export: "Exporting..." ({received} bytes so far).
    Exporting { received: i64 },
    /// Export: "Verifying Export...".
    VerifyingExport,
    /// Import: "Uploading '{file}' to '{pool}' ({percent}% done)...".
    Uploading {
        file: String,
        pool: String,
        percent: u32,
    },
    /// Import: "Updating VM..." (or template).
    UpdatingImported { template: bool },
    /// Import: "Waiting for Import VM wizard to complete..." (or template).
    WaitingForImportWizard { template: bool },
    /// Import: "Updating VM networking..." (or template).
    UpdatingImportNetworks { template: bool },
    /// Import: "Import VM complete" (or template).
    ImportComplete { template: bool },
    /// A failed import: removing what it left.
    RemovingImported,
    /// OVF export: "Exporting disk '{disk}' to '{file}' ({index} of
    /// {count})...".
    ExportingDisk {
        disk: String,
        file: String,
        index: usize,
        count: usize,
    },
    /// OVF export: "Verifying disk '{disk}'...".
    VerifyingDisk { disk: String },
    /// OVF export: "Calculating checksum for file '{file}'...".
    CalculatingChecksum { file: String },
    /// OVF export: "Creating manifest file...".
    CreatingManifest,
    /// OVF export: "Archiving appliance files into single package file
    /// '{file}'...".
    ArchivingPackage { file: String },
    /// OVF import: "Verifying Digital Signature...".
    VerifyingSignature,
    /// OVF import: "Verifying Manifest Content...".
    VerifyingManifest,
    /// OVF import: "Creating VM '{vm}'...".
    CreatingVm { vm: String },
    /// OVF import: "Attached CD/DVD Drive.".
    AttachedCd,
    /// OVF import: "Expanding {file}...".
    ExpandingFile { file: String },
    /// OVF import: "Preparing VDI for '{disk}'...".
    PreparingVdi { disk: String },
    /// OVF import: "Importing '{file}' ({index} of {count})...".
    ImportingDisk {
        file: String,
        index: usize,
        count: usize,
    },
    /// Move VM: `ACTION_MOVING_VDI_TO_SR`.
    MovingDisk {
        disk: String,
        from: String,
        to: String,
    },
    /// `SETTING_VM_STARTUP_OPTIONS_ON_X`.
    SettingStartupOptions { vm: String },
    /// `DELETING_VM_APPLIANCE`.
    DeletingVApp { name: String },
    /// OVF import: `IMPORT_CREATING_APPLIANCE`.
    CreatingVApp { name: String },
    /// `DELETING_VMSS`.
    DeletingPolicy { name: String },
    /// `ACTION_DR_ENABLING_ON`.
    EnablingDr { sr: String },
    /// `ACTION_DR_DISABLING_ON`.
    DisablingDr { sr: String },
    /// Starting a recovered VM.
    StartingRecoveredVm { vm: String },
    /// Starting a recovered vApp.
    StartingVApp { name: String },
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
    Sr(Ref<Sr>),
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
    /// The VM, template or snapshot it made (New VM, Copy, Take Snapshot),
    /// once known.
    pub created: Option<Ref<Vm>>,
    /// What the user must know after it succeeded.
    pub notice: Option<OperationNotice>,
    /// Add Users: each entered name and what became of it.
    pub subjects: Vec<crate::ad::SubjectEntry>,
    /// What it produced that the window needs next (the `DR_task` a
    /// `DR_task.create` made).
    pub result: Option<String>,
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
        ActionRequest::Host { host, action } | ActionRequest::HaHost { host, action, .. } => {
            OperationKind::HostAction {
                host_name: host_name(host),
                action: *action,
            }
        }
        ActionRequest::HaVm { vm, action, .. } => operation_kind(
            model,
            &ActionRequest::Vm {
                vm: vm.clone(),
                action: action.clone(),
            },
        ),
        request if request.is_ha() => OperationKind::Ha(ha_kind(model, request)),
        request if request.is_ad() => OperationKind::Ad(ad_kind(model, request)),
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
        ActionRequest::NewSr(_)
        | ActionRequest::Sr { .. }
        | ActionRequest::SetSrProperties { .. } => {
            OperationKind::Storage(storage_kind(model, request))
        }
        request if request.is_network() => OperationKind::Network(network_kind(model, request)),
        ActionRequest::EvacuateCoordinator { host, .. } => OperationKind::HostAction {
            host_name: host_name(host),
            action: HostAction::EnterMaintenance,
        },
        request if request.is_pool() => OperationKind::Pool(pool_kind(model, request)),
        request if request.is_transfer() => OperationKind::Transfer(transfer_kind(model, request)),
        request if request.is_migrate() => OperationKind::Migrate(migrate_kind(model, request)),
        request if request.is_vapp() => OperationKind::VApp(vapp_kind(model, request)),
        request if request.is_vmss() => OperationKind::Vmss(vmss_kind(model, request)),
        request if request.is_dr() => OperationKind::Dr(dr_kind(model, request)),
        ActionRequest::Org(r) => OperationKind::Org(org_kind(model, r)),
        request => OperationKind::Provision(provision_kind(model, request)),
    }
}

/// The history entry of an F12a migration or move.
fn migrate_kind(model: &ConnectionModel, request: &ActionRequest) -> MigrateKind {
    let vm_name = |r: &Ref<Vm>| {
        model
            .vms
            .get(r)
            .map(|v| v.name_label.clone())
            .unwrap_or_else(|| r.to_string())
    };
    let host_name = |h: &Ref<Host>| model.hosts.get(h).map(|h| h.name_label.clone());
    match request {
        ActionRequest::MigrateSend(spec) => {
            let vm = vm_name(&spec.vm);
            let to = spec.host_name.clone();
            if spec.copy {
                let pool = model
                    .pool()
                    .map(|(_, p)| objects::pool_name(model, p))
                    .unwrap_or_default();
                MigrateKind::Copy { vm, pool, to }
            } else {
                let from = model
                    .vms
                    .get(&spec.vm)
                    .and_then(|v| v.resident_on.non_null())
                    .and_then(host_name);
                MigrateKind::Migrate { vm, from, to }
            }
        }
        ActionRequest::MoveVm { vm, host, storage } => {
            let name = vm_name(vm);
            let from = model
                .vms
                .get(vm)
                .and_then(|v| objects::vm_home(model, v))
                .filter(|h| *h != host)
                .and_then(host_name);
            let to = host_name(host).unwrap_or_default();
            match (from, storage.as_slice()) {
                (Some(from), _) => MigrateKind::MoveHost { vm: name, from, to },
                (None, [(_, sr)]) => MigrateKind::MoveSr {
                    vm: name,
                    sr: model
                        .srs
                        .get(sr)
                        .map(|s| s.name_label.clone())
                        .unwrap_or_default(),
                },
                (None, _) => MigrateKind::Move { vm: name },
            }
        }
        _ => unreachable!("not a migration"),
    }
}

/// The history entry of an F12a vApp operation.
fn vapp_kind(model: &ConnectionModel, request: &ActionRequest) -> VAppKind {
    let app_name = |r: &Ref<xapi::VmAppliance>| {
        model
            .vm_appliances
            .get(r)
            .map(|a| a.name_label.clone())
            .unwrap_or_default()
    };
    let one_vm = |vms: &[Ref<Vm>]| match vms {
        [one] => model.vms.get(one).map(|v| v.name_label.clone()),
        _ => None,
    };
    match request {
        ActionRequest::NewVApp(spec) => VAppKind::Create {
            name: spec.name.clone(),
        },
        ActionRequest::SetVApp { vapp, changes } => VAppKind::Properties {
            name: changes.name.clone().unwrap_or_else(|| app_name(vapp)),
        },
        ActionRequest::AssignVApp { vapp, vms } => VAppKind::Assign {
            vm: one_vm(vms),
            vapp: app_name(vapp),
        },
        ActionRequest::RemoveFromVApp { vapp, vms } => VAppKind::Remove {
            vm: one_vm(vms),
            vapp: app_name(vapp),
        },
        ActionRequest::DeleteVApps { vapps } => VAppKind::Delete {
            name: match vapps.as_slice() {
                [one] => Some(app_name(one)),
                _ => None,
            },
        },
        ActionRequest::StartVApp { vapp } => VAppKind::Start {
            name: app_name(vapp),
        },
        ActionRequest::ShutDownVApp { vapp } => VAppKind::ShutDown {
            name: app_name(vapp),
        },
        _ => unreachable!("not a vApp request"),
    }
}

/// The history entry of an F13 operation.
fn org_kind(model: &ConnectionModel, request: &crate::org::OrgRequest) -> OrgKind {
    use crate::org::{FolderItem, OrgRequest};
    let name = |o: &crate::org::OrgObject| o.name(model).unwrap_or_default().to_owned();
    match request {
        OrgRequest::MoveToFolder { items, folder } => OrgKind::MoveToFolder {
            object: match items.as_slice() {
                [FolderItem::Object(o)] => Some(name(o)),
                [FolderItem::Folder(f)] => Some(crate::org::folder_name(f)),
                _ => None,
            },
            folder: crate::org::folder_name(folder),
        },
        OrgRequest::DeleteFromFolders { items } => {
            OrgKind::DeleteFromFolders { count: items.len() }
        }
        OrgRequest::RenameFolder { folder, name } => OrgKind::RenameFolder {
            folder: crate::org::folder_name(folder),
            name: name.clone(),
        },
        OrgRequest::NewFolders { paths } => OrgKind::NewFolders {
            paths: paths.clone(),
        },
        OrgRequest::AddTag { tag, .. } => OrgKind::AddTag { tag: tag.clone() },
        OrgRequest::RemoveTag { tag, .. } => OrgKind::RemoveTag { tag: tag.clone() },
        OrgRequest::RenameTag { old, new } => OrgKind::RenameTag {
            old: old.clone(),
            new: new.clone(),
        },
        OrgRequest::DeleteTag { tag } => OrgKind::DeleteTag { tag: tag.clone() },
        OrgRequest::FolderAndTags { object, .. } => OrgKind::FolderAndTags {
            object: name(object),
        },
        OrgRequest::AddCustomField(d) => OrgKind::AddCustomField {
            name: d.name.clone(),
        },
        OrgRequest::RemoveCustomField(d) => OrgKind::RemoveCustomField {
            name: d.name.clone(),
        },
        OrgRequest::SaveCustomFields { object, .. } => OrgKind::SaveCustomFields {
            object: name(object),
        },
        OrgRequest::SaveSearch { name, .. } => OrgKind::SaveSearch { name: name.clone() },
        OrgRequest::DeleteSearch { name, .. } => OrgKind::DeleteSearch { name: name.clone() },
        OrgRequest::VmNames { vm, .. } => OrgKind::VmNames {
            name: model
                .vms
                .get(vm)
                .map(|v| v.name_label.clone())
                .unwrap_or_default(),
        },
    }
}

/// The history entry of an F12b snapshot schedule operation.
fn vmss_kind(model: &ConnectionModel, request: &ActionRequest) -> VmssKind {
    let name = |r: &Ref<xapi::Vmss>| {
        model
            .vmss
            .get(r)
            .map(|p| p.name_label.clone())
            .unwrap_or_default()
    };
    match request {
        ActionRequest::NewPolicy(spec) => VmssKind::Create {
            name: spec.name.clone(),
        },
        ActionRequest::SetPolicyEnabled { policy, enabled } => VmssKind::SetEnabled {
            name: name(policy),
            enabled: *enabled,
        },
        ActionRequest::RunPolicyNow { policy } => VmssKind::RunNow { name: name(policy) },
        ActionRequest::DeletePolicies { .. } => VmssKind::Delete,
        ActionRequest::AssignPolicy { policy, .. } => VmssKind::Assign {
            policy: name(policy),
        },
        ActionRequest::RemoveFromPolicy { policy, vms } => VmssKind::Remove {
            vm: match vms.as_slice() {
                [one] => model.vms.get(one).map(|v| v.name_label.clone()),
                _ => None,
            },
            policy: name(policy),
        },
        ActionRequest::SetPolicy { policy, changes } => VmssKind::Properties {
            name: changes.name.clone().unwrap_or_else(|| name(policy)),
        },
        _ => unreachable!("not a snapshot schedule request"),
    }
}

/// The history entry of an F12b disaster recovery operation.
fn dr_kind(model: &ConnectionModel, request: &ActionRequest) -> DrKind {
    match request {
        ActionRequest::ConfigureDr { .. } => DrKind::Configure,
        ActionRequest::DrTaskCreate(_) => DrKind::TaskCreate,
        ActionRequest::DrTaskDestroy { srs, .. } => DrKind::TaskDestroy { srs: srs.clone() },
        ActionRequest::DrRecover { item, name, .. } => DrKind::Recover {
            name: name.clone(),
            vapp: matches!(item, crate::dr::DrItem::VApp(_)),
        },
        ActionRequest::DrStart { .. } => DrKind::Start,
        ActionRequest::DrDestroyVms { vms } => DrKind::DestroyVms { count: vms.len() },
        ActionRequest::DrDestroyVApps { .. } => DrKind::DestroyVApps,
        ActionRequest::HardShutDownVApp { vapp } => DrKind::HardShutDownVApp {
            name: model
                .vm_appliances
                .get(vapp)
                .map(|a| a.name_label.clone())
                .unwrap_or_default(),
        },
        _ => unreachable!("not a disaster recovery request"),
    }
}

/// The history entry of an XVA transfer.
fn transfer_kind(model: &ConnectionModel, request: &ActionRequest) -> TransferKind {
    let pool = model.pool();
    let pool_name = pool
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    match request {
        ActionRequest::ExportXva { vm, .. } => TransferKind::Export {
            vm: model
                .vms
                .get(vm)
                .map(|v| v.name_label.clone())
                .unwrap_or_else(|| vm.to_string()),
            pool: pool_name,
        },
        ActionRequest::ImportXva { path, affinity, .. } => {
            let host = affinity
                .clone()
                .or_else(|| pool.map(|(_, p)| p.master.clone()))
                .and_then(|h| model.hosts.get(&h).map(|h| h.name_label.clone()))
                .unwrap_or_default();
            TransferKind::Import {
                file: path.clone(),
                host,
                pool: pool_name,
                in_pool: pool.is_some_and(|(_, p)| !p.name_label.is_empty())
                    || model.hosts.len() > 1,
            }
        }
        ActionRequest::ExportOvf(spec) => TransferKind::ExportOvf {
            name: spec.name.clone(),
            pool: pool_name,
            ova: spec.ova,
        },
        ActionRequest::ImportOvf(spec) => match &spec.image {
            Some(_) => TransferKind::ImportImage {
                vm: spec.vms.first().map(|v| v.name.clone()).unwrap_or_default(),
                file: spec.path.clone(),
                pool: pool_name,
            },
            None => TransferKind::ImportAppliance {
                name: std::path::Path::new(&spec.path)
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file: spec.path.clone(),
                pool: pool_name,
            },
        },
        _ => unreachable!("not a transfer"),
    }
}

/// The history entry of an HA request.
fn ha_kind(model: &ConnectionModel, request: &ActionRequest) -> HaKind {
    let pool = model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    match request {
        ActionRequest::EnableHa { .. } => HaKind::Enable { pool },
        ActionRequest::DisableHa => HaKind::Disable { pool },
        ActionRequest::SetHaPriorities { .. } => HaKind::SetPriorities,
        _ => HaKind::SetNtol { pool },
    }
}

/// The history entry of an AD request.
fn ad_kind(model: &ConnectionModel, request: &ActionRequest) -> AdKind {
    let pool = model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    let names = |subjects: &[Ref<xapi::Subject>]| -> Vec<String> {
        subjects
            .iter()
            .filter_map(|s| model.subjects.get(s))
            .map(|s| crate::ad::shown_name(s).to_owned())
            .collect()
    };
    match request {
        ActionRequest::EnableAd { .. } => AdKind::Enable { pool },
        ActionRequest::DisableAd { .. } => AdKind::Disable { pool },
        ActionRequest::SetSubjectRoles { subjects, .. } => AdKind::Roles {
            subjects: names(subjects),
        },
        ActionRequest::LogoutSubjects { subjects } => AdKind::Logout {
            subjects: names(subjects),
        },
        _ => AdKind::AddRemove { pool },
    }
}

/// The history entry of an F10a request.
fn pool_kind(model: &ConnectionModel, request: &ActionRequest) -> PoolKind {
    let pool = model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    let host = |h: &Ref<Host>| {
        model
            .hosts
            .get(h)
            .map(|r| r.name_label.clone())
            .unwrap_or_default()
    };
    match request {
        ActionRequest::DestroyPool { .. } => PoolKind::DestroyPool { pool },
        ActionRequest::EjectHost { host: h } => PoolKind::EjectHost {
            host: host(h),
            pool,
        },
        ActionRequest::SetProperties { target, .. } => PoolKind::Properties {
            name: match target {
                crate::properties::PropertiesTarget::Pool(_) => pool,
                crate::properties::PropertiesTarget::Host(h) => host(h),
            },
        },
        ActionRequest::SetDom0Memory { host: h, .. } => PoolKind::Dom0Memory { host: host(h) },
        ActionRequest::ChangePassword { .. } => PoolKind::ChangePassword { pool },
        ActionRequest::RotatePoolSecret => PoolKind::RotateSecret,
        ActionRequest::EnableTlsVerification => PoolKind::EnableTls { pool },
        ActionRequest::InstallCertificate { host: h, .. } => {
            PoolKind::InstallCertificate { host: host(h) }
        }
        ActionRequest::ResetCertificate { host: h } => PoolKind::ResetCertificate { host: host(h) },
        ActionRequest::BackupHost { host: h, .. } => PoolKind::Backup { host: host(h) },
        ActionRequest::RestoreHost { host: h, .. } => PoolKind::Restore { host: host(h) },
        _ => unreachable!("not a pool request"),
    }
}

/// The history entry of an F9 request.
fn network_kind(model: &ConnectionModel, request: &ActionRequest) -> NetworkKind {
    let pool = model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    let network_name = |r: &Ref<xapi::Network>| {
        model
            .networks
            .get(r)
            .map(|n| objects::network_name(model, n))
            .unwrap_or_default()
    };
    let vm_name = |r: &Ref<Vm>| {
        model
            .vms
            .get(r)
            .map(|v| v.name_label.clone())
            .unwrap_or_default()
    };
    let vif_vm = |r: &Ref<xapi::Vif>| model.get(r).map(|v| vm_name(&v.vm)).unwrap_or_default();
    let vif_network = |r: &Ref<xapi::Vif>| {
        model
            .get(r)
            .map(|v| network_name(&v.network))
            .unwrap_or_default()
    };
    match request {
        ActionRequest::NewNetwork(spec) => NetworkKind::CreateNetwork {
            name: spec.name.clone(),
            pool,
        },
        ActionRequest::NewBond(spec) => NetworkKind::CreateBond {
            name: spec.name.clone(),
        },
        ActionRequest::RemoveNetwork { network } => NetworkKind::RemoveNetwork {
            name: network_name(network),
            pool,
        },
        ActionRequest::DestroyBond { bond } => NetworkKind::DestroyBond {
            name: model
                .get(bond)
                .and_then(|b| model.get(&b.master))
                .map(|p| objects::pif_name(model, p))
                .unwrap_or_default(),
        },
        ActionRequest::SetNetworkProperties { network, .. } => NetworkKind::UpdateNetwork {
            name: network_name(network),
            pool,
        },
        ActionRequest::RescanPifs { host } => NetworkKind::RescanPifs {
            host: model
                .hosts
                .get(host)
                .map(|h| h.name_label.clone())
                .unwrap_or_default(),
        },
        ActionRequest::ChangeNetworking(_) => NetworkKind::ChangeNetworking,
        ActionRequest::RenameInterfaces { .. } => NetworkKind::RenameInterfaces,
        ActionRequest::NewVif { vm, .. } => NetworkKind::NewVif { vm: vm_name(vm) },
        ActionRequest::DeleteVif { vif } => NetworkKind::DeleteVif {
            network: vif_network(vif),
            vm: vif_vm(vif),
        },
        ActionRequest::UpdateVif { vif, .. } => NetworkKind::UpdateVif {
            network: vif_network(vif),
            vm: vif_vm(vif),
        },
        ActionRequest::PlugVif { vif } => NetworkKind::PlugVif { vm: vif_vm(vif) },
        ActionRequest::UnplugVif { vif } => NetworkKind::UnplugVif { vm: vif_vm(vif) },
        _ => unreachable!("not a networking request"),
    }
}

/// The history entry of an F8 request.
fn storage_kind(model: &ConnectionModel, request: &ActionRequest) -> StorageKind {
    let pool = model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default();
    let sr_name = |r: &Ref<Sr>| {
        model
            .srs
            .get(r)
            .map(|v| v.name_label.clone())
            .unwrap_or_else(|| r.to_string())
    };
    match request {
        ActionRequest::NewSr(spec) => match spec.mode() {
            NewSrMode::Create => StorageKind::CreateSr {
                sr_type: spec.kind.sr_type().to_owned(),
                name: spec.name.clone(),
                pool,
            },
            NewSrMode::Introduce | NewSrMode::Reattach => StorageKind::AttachSr {
                name: spec.name.clone(),
                pool,
            },
        },
        ActionRequest::Sr { sr, action } => StorageKind::Sr {
            action: *action,
            name: sr_name(sr),
            pool,
        },
        ActionRequest::SetSrProperties { sr, .. } => {
            StorageKind::SrProperties { name: sr_name(sr) }
        }
        _ => unreachable!("not a storage request"),
    }
}

/// The history entry of an F7 request.
fn provision_kind(model: &ConnectionModel, request: &ActionRequest) -> ProvisionKind {
    let vm_name = |r: &Ref<Vm>| {
        model
            .vms
            .get(r)
            .map(|v| v.name_label.clone())
            .unwrap_or_else(|| r.to_string())
    };
    let sr_name = |r: &Ref<xapi::Sr>| {
        model
            .srs
            .get(r)
            .map(|v| v.name_label.clone())
            .unwrap_or_default()
    };
    let vdi = |r: &Ref<xapi::Vdi>| model.vdis.get(r);
    let vdi_name = |r: &Ref<xapi::Vdi>| vdi(r).map(|v| v.name_label.clone()).unwrap_or_default();
    let vdi_sr = |r: &Ref<xapi::Vdi>| vdi(r).map(|v| sr_name(&v.sr)).unwrap_or_default();
    match request {
        ActionRequest::NewVm(spec) => ProvisionKind::CreateVm {
            vm: spec.name.clone(),
            template: vm_name(&spec.template),
        },
        ActionRequest::CopyVm { vm, name, mode, .. } => match mode {
            CopyMode::Clone => ProvisionKind::CloneVm {
                vm: name.clone(),
                source: vm_name(vm),
            },
            CopyMode::Copy(sr) => ProvisionKind::CopyVm {
                vm: name.clone(),
                source: vm_name(vm),
                sr: sr_name(sr),
            },
        },
        ActionRequest::TemplateFromSnapshot { snapshot, name, .. } => ProvisionKind::CloneVm {
            vm: name.clone(),
            source: vm_name(snapshot),
        },
        ActionRequest::MakeTemplate { vm } => ProvisionKind::MakeTemplate { vm: vm_name(vm) },
        ActionRequest::DeleteVm { vm, .. } => {
            let pool = model
                .pool()
                .map(|(_, p)| objects::pool_name(model, p))
                .unwrap_or_default();
            let location = model
                .vms
                .get(vm)
                .and_then(|r| objects::vm_home(model, r))
                .and_then(|h| model.hosts.get(h))
                .map(|h| h.name_label.clone())
                .unwrap_or(pool);
            ProvisionKind::DeleteVm {
                vm: vm_name(vm),
                location,
            }
        }
        ActionRequest::Snapshot { vm, .. } => ProvisionKind::Snapshot { vm: vm_name(vm) },
        ActionRequest::Revert { snapshot, .. } => ProvisionKind::Revert {
            snapshot: vm_name(snapshot),
        },
        ActionRequest::DeleteSnapshots { snapshots } => ProvisionKind::DeleteSnapshots {
            names: snapshots.iter().map(vm_name).collect(),
        },
        ActionRequest::NewDisk { disk, .. } => ProvisionKind::NewDisk {
            disk: disk.name.clone(),
            sr: sr_name(&disk.sr),
        },
        ActionRequest::AttachDisk { vm, vdi, .. } => ProvisionKind::AttachDisk {
            disk: vdi_name(vdi),
            vm: vm_name(vm),
        },
        ActionRequest::DetachDisk { vm, vbd } => ProvisionKind::DetachDisk {
            disk: model.get(vbd).map(|b| vdi_name(&b.vdi)).unwrap_or_default(),
            vm: vm_name(vm),
        },
        ActionRequest::DeleteDisk { vdi } => ProvisionKind::DeleteDisk {
            disk: vdi_name(vdi),
            sr: vdi_sr(vdi),
        },
        ActionRequest::ResizeDisk { vdi, .. } => ProvisionKind::ResizeDisk {
            disk: vdi_name(vdi),
        },
        ActionRequest::MoveDisk { vdi, sr } => ProvisionKind::MoveDisk {
            disk: vdi_name(vdi),
            from: vdi_sr(vdi),
            to: sr_name(sr),
        },
        ActionRequest::ChangeCd { vm, iso, .. } => ProvisionKind::ChangeCd {
            vm: vm_name(vm),
            iso: iso.as_ref().map(vdi_name),
        },
        ActionRequest::NewCdDrive { vm } => ProvisionKind::NewCdDrive { vm: vm_name(vm) },
        ActionRequest::InstallTools { vm } => ProvisionKind::InstallTools { vm: vm_name(vm) },
        _ => unreachable!("not a provisioning request"),
    }
}

/// The VM an operation works on (busy while it runs).
fn request_object(model: &ConnectionModel, request: &ActionRequest) -> Option<OperationObject> {
    let vm_of_vdi = |vdi: &Ref<xapi::Vdi>| {
        model
            .vdis
            .get(vdi)?
            .vbds
            .iter()
            .filter_map(|b| model.get(b))
            .map(|b| b.vm.clone())
            .find(|v| model.vms.contains_key(v))
    };
    let vm = match request {
        ActionRequest::Vm { vm, .. } | ActionRequest::HaVm { vm, .. } => {
            return Some(OperationObject::Vm(vm.clone()));
        }
        ActionRequest::EnableHa { .. }
        | ActionRequest::DisableHa
        | ActionRequest::SetHaPriorities { .. }
        | ActionRequest::SetHaNtol { .. }
        | ActionRequest::EnableAd { .. }
        | ActionRequest::DisableAd { .. }
        | ActionRequest::AddSubjects { .. }
        | ActionRequest::RemoveSubjects { .. }
        | ActionRequest::SetSubjectRoles { .. }
        | ActionRequest::LogoutSubjects { .. } => None,
        ActionRequest::Host { host, .. }
        | ActionRequest::HaHost { host, .. }
        | ActionRequest::EjectHost { host }
        | ActionRequest::EvacuateCoordinator { host, .. }
        | ActionRequest::SetDom0Memory { host, .. }
        | ActionRequest::InstallCertificate { host, .. }
        | ActionRequest::ResetCertificate { host }
        | ActionRequest::BackupHost { host, .. }
        | ActionRequest::RestoreHost { host, .. } => {
            return Some(OperationObject::Host(host.clone()));
        }
        ActionRequest::ExportXva { vm, .. } => return Some(OperationObject::Vm(vm.clone())),
        ActionRequest::MigrateSend(spec) => return Some(OperationObject::Vm(spec.vm.clone())),
        ActionRequest::MoveVm { vm, .. } => return Some(OperationObject::Vm(vm.clone())),
        ActionRequest::NewVApp(_)
        | ActionRequest::SetVApp { .. }
        | ActionRequest::AssignVApp { .. }
        | ActionRequest::RemoveFromVApp { .. }
        | ActionRequest::DeleteVApps { .. }
        | ActionRequest::StartVApp { .. }
        | ActionRequest::ShutDownVApp { .. }
        | ActionRequest::HardShutDownVApp { .. }
        | ActionRequest::NewPolicy(_)
        | ActionRequest::SetPolicyEnabled { .. }
        | ActionRequest::RunPolicyNow { .. }
        | ActionRequest::DeletePolicies { .. }
        | ActionRequest::AssignPolicy { .. }
        | ActionRequest::RemoveFromPolicy { .. }
        | ActionRequest::SetPolicy { .. }
        | ActionRequest::ConfigureDr { .. }
        | ActionRequest::DrTaskCreate(_)
        | ActionRequest::DrTaskDestroy { .. }
        | ActionRequest::DrRecover { .. }
        | ActionRequest::DrStart { .. }
        | ActionRequest::DrDestroyVms { .. }
        | ActionRequest::DrDestroyVApps { .. }
        | ActionRequest::Org(_) => None,
        ActionRequest::ImportXva { .. }
        | ActionRequest::ExportOvf(_)
        | ActionRequest::ImportOvf(_) => None,
        ActionRequest::SetProperties {
            target: crate::properties::PropertiesTarget::Host(host),
            ..
        } => return Some(OperationObject::Host(host.clone())),
        ActionRequest::DestroyPool { .. }
        | ActionRequest::SetProperties { .. }
        | ActionRequest::ChangePassword { .. }
        | ActionRequest::RotatePoolSecret
        | ActionRequest::EnableTlsVerification => None,
        ActionRequest::Sr { sr, .. } | ActionRequest::SetSrProperties { sr, .. } => {
            return Some(OperationObject::Sr(sr.clone()));
        }
        ActionRequest::NewSr(spec) => {
            return spec.reattach.clone().map(OperationObject::Sr);
        }
        ActionRequest::DismissAlerts { .. }
        | ActionRequest::SetPerfmon { .. }
        | ActionRequest::NewVm(_)
        | ActionRequest::DeleteSnapshots { .. } => None,
        ActionRequest::NewVif { vm, .. } => Some(vm.clone()),
        ActionRequest::DeleteVif { vif }
        | ActionRequest::UpdateVif { vif, .. }
        | ActionRequest::PlugVif { vif }
        | ActionRequest::UnplugVif { vif } => model.get(vif).map(|v| v.vm.clone()),
        ActionRequest::NewNetwork(_)
        | ActionRequest::NewBond(_)
        | ActionRequest::RemoveNetwork { .. }
        | ActionRequest::DestroyBond { .. }
        | ActionRequest::SetNetworkProperties { .. }
        | ActionRequest::RescanPifs { .. }
        | ActionRequest::ChangeNetworking(_)
        | ActionRequest::RenameInterfaces { .. } => None,
        ActionRequest::CopyVm { vm, .. }
        | ActionRequest::MakeTemplate { vm }
        | ActionRequest::DeleteVm { vm, .. }
        | ActionRequest::Snapshot { vm, .. }
        | ActionRequest::NewDisk { vm, .. }
        | ActionRequest::AttachDisk { vm, .. }
        | ActionRequest::DetachDisk { vm, .. }
        | ActionRequest::ChangeCd { vm, .. }
        | ActionRequest::NewCdDrive { vm }
        | ActionRequest::InstallTools { vm } => Some(vm.clone()),
        ActionRequest::TemplateFromSnapshot { snapshot, .. } => Some(snapshot.clone()),
        ActionRequest::Revert { snapshot, .. } => {
            model.vms.get(snapshot).map(|s| s.snapshot_of.clone())
        }
        ActionRequest::DeleteDisk { vdi }
        | ActionRequest::ResizeDisk { vdi, .. }
        | ActionRequest::MoveDisk { vdi, .. } => vm_of_vdi(vdi),
    };
    vm.map(OperationObject::Vm)
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
    start_action_with(shared, connection, session, None, request, elevated_as)
}

/// [`start_action`] for an operation that also needs the session of
/// another connection (`target`: the destination of a migration).
pub(crate) fn start_action_with(
    shared: &Arc<Shared>,
    connection: ConnectionId,
    session: Option<Session>,
    target: Option<Session>,
    request: ActionRequest,
    elevated_as: Option<String>,
) -> OperationId {
    let server = shared
        .state()
        .connection(connection)
        .map(|v| v.server.clone())
        .unwrap_or_default();
    let kind = kind_of(shared, connection, &request);
    let object = {
        let state = shared.state();
        let model = state
            .connection(connection)
            .map(|v| v.model.clone())
            .unwrap_or_default();
        request_object(&model, &request)
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
            request @ (ActionRequest::NewSr(_)
            | ActionRequest::Sr { .. }
            | ActionRequest::SetSrProperties { .. }) => {
                crate::storage_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_network() => {
                crate::network_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_pool() => {
                crate::pool_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_ha() || request.is_ad() => {
                crate::ha_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_transfer() => {
                crate::xva_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_migrate() => {
                crate::migrate_run::run(&shared, connection, op, &session, target.as_ref(), request)
                    .await
            }
            request if request.is_vapp() => {
                crate::vapp_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_vmss() => {
                crate::vmss_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_dr() => {
                crate::dr_run::run(&shared, connection, op, &session, request).await
            }
            request if request.is_org() => {
                crate::org_run::run(&shared, connection, op, &session, request).await
            }
            request => crate::provision_run::run(&shared, connection, op, &session, request).await,
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

pub(crate) async fn run_vm_action(
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
pub(crate) async fn set_perfmon(
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

pub(crate) async fn run_host_action(
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
        HostAction::EnterMaintenance => enter_maintenance(shared, op, session, host, None).await,
        HostAction::ExitMaintenance => {
            let outcome = enable(shared, op, session, host).await;
            if matches!(outcome, TaskOutcome::Succeeded) {
                crate::ha_run::offer_ntol_raise(shared, connection, session, host).await;
            }
            outcome
        }
    }
}

/// `EvacuateHostAction`: disables the host, flags it, migrates its VMs
/// away and, for the coordinator with `new_coordinator` (its ref and name),
/// hands the pool to that server (`pool.designate_new_master`; the
/// connection then follows the new coordinator). On failure the host is
/// enabled again.
pub(crate) async fn enter_maintenance(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    host: &Ref<Host>,
    new_coordinator: Option<(&Ref<Host>, String)>,
) -> TaskOutcome {
    let s = session;
    let end = if new_coordinator.is_some() { 0.8 } else { 1.0 };
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
    let mut outcome = match flagged {
        Err(e) => TaskOutcome::Failed(e),
        Ok(()) => {
            shared.update_operation(op, |o| o.step = Some(OperationStep::Evacuating));
            let start = xapi::host::async_evacuate(s, host, None, None);
            run_task(shared, op, session, start, Span(0.2, end), false).await
        }
    };
    if let (TaskOutcome::Succeeded, Some((coordinator, name))) = (&outcome, new_coordinator) {
        shared.update_operation(op, |o| {
            o.step = Some(OperationStep::NewCoordinator { host: name });
        });
        let start = xapi::pool::async_designate_new_master(s, coordinator);
        // The old coordinator stops answering as such: losing it is fine.
        outcome = run_task(shared, op, session, start, Span(0.8, 1.0), true).await;
    }
    if !matches!(outcome, TaskOutcome::Succeeded) {
        shared.update_operation(op, |o| o.step = Some(OperationStep::Reenabling));
        if let TaskOutcome::Failed(e) = enable(shared, op, session, host).await {
            tracing::warn!(error = %e, "could not take the host out of maintenance mode again");
        }
    }
    outcome
}

/// `HostAbstractAction.Enable`: clears the maintenance flag and enables the
/// host.
pub(crate) async fn enable(
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

pub(crate) async fn reboot_or_shutdown(
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
        OperationObject::Sr(sr) => toggle(&mut v.busy_srs, sr, busy),
    });
}

pub(crate) enum TaskOutcome {
    Succeeded,
    Cancelled,
    Failed(xapi::Error),
}

/// The share of an operation's progress bar a task fills.
#[derive(Clone, Copy)]
pub(crate) struct Span(pub(crate) f64, pub(crate) f64);

impl Span {
    pub(crate) const FULL: Span = Span(0.0, 1.0);

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
pub(crate) async fn run_task(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl Future<Output = xapi::Result<Ref<Task>>>,
    span: Span,
    goes_away: bool,
) -> TaskOutcome {
    run_task_result(shared, op, session, start, span, goes_away)
        .await
        .0
}

/// Like [`run_task`], also returning the task's `result` once it succeeded
/// (the value the call returns, e.g. the new object of `Async.VM.clone`).
pub(crate) async fn run_task_result(
    shared: &Shared,
    op: OperationId,
    session: &Session,
    start: impl Future<Output = xapi::Result<Ref<Task>>>,
    span: Span,
    goes_away: bool,
) -> (TaskOutcome, String) {
    let task = match start.await {
        Ok(t) => t,
        Err(e) => return (TaskOutcome::Failed(e), String::new()),
    };
    let mut result = String::new();
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
                return (TaskOutcome::Succeeded, String::new());
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
            TaskStatusType::Success => {
                result = task_result_value(&record.result);
                break TaskOutcome::Succeeded;
            }
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
    (outcome, result)
}

/// The value in a task's `result`: XAPI writes it as XML-RPC
/// (`<value>OpaqueRef:…</value>`), sometimes as a bare string.
pub fn task_result_value(result: &str) -> String {
    let trimmed = result.trim();
    let inner = trimmed
        .strip_prefix("<value>")
        .and_then(|r| r.strip_suffix("</value>"))
        .unwrap_or(trimmed);
    let inner = inner
        .strip_prefix("<string>")
        .and_then(|r| r.strip_suffix("</string>"))
        .unwrap_or(inner);
    inner.trim().to_owned()
}

/// `HANDLE_INVALID` on the task itself.
fn is_task_gone(e: &xapi::Error) -> bool {
    matches!(e, xapi::Error::Api { code, params }
        if code == "HANDLE_INVALID" && params.first().is_some_and(|c| c.eq_ignore_ascii_case("task")))
}

/// A failed task's `error_info` as an API error.
pub(crate) fn task_error(error_info: Vec<String>) -> xapi::Error {
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
    /// `SR.probe` on the coordinator (`SrProbeAction`: the NFS and SMB
    /// Scan buttons, the iSCSI page's check of the chosen LUN).
    SrProbe {
        sr_type: String,
        device_config: DeviceConfig,
    },
    /// The IQNs of an iSCSI target (`ISCSIPopulateIQNsAction`).
    IscsiIqns {
        target: String,
        port: u16,
        chap: Option<(String, Secret)>,
    },
    /// The LUNs of a target IQN (`ISCSIPopulateLunsAction`).
    IscsiLuns {
        target: String,
        port: u16,
        iqn: String,
        chap: Option<(String, Secret)>,
    },
    /// The NRPE configuration of a server (`NRPERetrieveAction`: the `nrpe`
    /// plugin's `status`, `get-config` and `get-threshold`).
    Nrpe { host: Ref<Host> },
    /// The power-on passwords of these servers (`PowerOnMode.GetSecret`).
    PowerOnSecrets { hosts: Vec<Ref<Host>> },
    /// The network of the pool's cluster (`Cluster.get_network`).
    ClusterNetwork { cluster: Ref<xapi::Cluster> },
    /// Whether `password` is the one this connection logged in with
    /// (Change Server Password's check of the current password).
    CurrentPassword { password: crate::backend::Password },
    /// Which of `srs` can hold an HA statefile
    /// (`SR.assert_can_host_ha_statefile`, `GetHeartbeatSRsAction`).
    HeartbeatSrs { srs: Vec<Ref<Sr>> },
    /// The most server failures the pool can tolerate if `restart` VMs are
    /// restarted (`pool.ha_compute_hypothetical_max_host_failures_to_tolerate`,
    /// then the pool's HCI limit: `Pool.GetMaximumTolerableHostFailures`).
    HaMaxFailures {
        restart: std::collections::BTreeMap<Ref<Vm>, String>,
    },
    /// Whether each VM is agile (`VM.assert_agile`).
    Agility { vms: Vec<Ref<Vm>> },
    /// The SIDs of the users logged in now
    /// (`session.get_all_subject_identifiers`).
    LoggedInSubjects,
    /// `CrossPoolMigrateFilter`'s questions for each VM and destination
    /// server: `host.migrate_receive` on connection `target`, then
    /// `VM.assert_can_migrate` here; within the pool in Migrate mode, a
    /// refused storage migration falls back to `VM.assert_can_boot_here`.
    Migrate {
        target: ConnectionId,
        mode: crate::migrate::WizardMode,
        checks: Vec<(Ref<Vm>, Ref<Host>)>,
    },
    /// The server's clock (`GetServerLocalTimeAction`:
    /// `host.get_servertime` and `host.get_server_localtime` of `host`).
    ServerTime { host: Ref<Host> },
    /// `SR.assert_supports_database_replication` for each SR (F12b).
    DrSupportedSrs { srs: Vec<Ref<Sr>> },
    /// `SR.probe` with `metadata = true` (the DR Storage page's scans).
    DrProbe {
        sr_type: String,
        device_config: DeviceConfig,
    },
    /// The metadata on these SRs (uuids): `GetMetadataVDIsAction`, then
    /// `VdiLoadMetadataAction` per metadata VDI.
    DrMetadata { srs: Vec<String> },
    /// `AssertCanBeRecoveredCheck` for items of the metadata on `vdi`.
    DrRecoverable {
        vdi: Ref<xapi::Vdi>,
        items: Vec<crate::dr::DrItem>,
    },
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
    /// What `SR.probe` found.
    SrProbe(Result<ProbeResult, Arc<xapi::Error>>),
    IscsiIqns(IscsiScan<IqnInfo>),
    IscsiLuns(IscsiScan<LunInfo>),
    Nrpe(Result<crate::properties::NrpeConfig, Arc<xapi::Error>>),
    /// Server and password, for those whose secret could be read.
    PowerOnSecrets(Vec<(Ref<Host>, String)>),
    ClusterNetwork(Option<Ref<xapi::Network>>),
    CurrentPassword(bool),
    /// Per SR: `None` if it can hold the statefile, else why not.
    HeartbeatSrs(Vec<(Ref<Sr>, Option<Arc<xapi::Error>>)>),
    HaMaxFailures(Result<i64, Arc<xapi::Error>>),
    /// Per VM: `None` if agile, else the error code that says why not.
    Agility(Vec<(Ref<Vm>, Option<String>)>),
    LoggedInSubjects(Result<Vec<String>, Arc<xapi::Error>>),
    /// Per VM and server: what the servers said.
    Migrate(Vec<(Ref<Vm>, Ref<Host>, crate::migrate::ServerVerdict)>),
    ServerTime(Result<crate::vmss::ServerTime, Arc<xapi::Error>>),
    /// Per SR: whether it can hold the pool's metadata.
    DrSupportedSrs(Vec<(Ref<Sr>, bool)>),
    DrProbe(Result<ProbeResult, Arc<xapi::Error>>),
    DrMetadata(Result<Vec<crate::dr::PoolMetadata>, Arc<xapi::Error>>),
    DrRecoverable(Result<Vec<(crate::dr::DrItem, crate::dr::Recoverable)>, Arc<xapi::Error>>),
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
            Precheck::SrProbe {
                sr_type,
                device_config,
            } => PrecheckResult::SrProbe(
                crate::storage_run::probe(&shared, &session, connection, &sr_type, &device_config)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::IscsiIqns { target, port, chap } => PrecheckResult::IscsiIqns(
                crate::storage_run::scan_iqns(&shared, &session, connection, &target, port, chap)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::IscsiLuns {
                target,
                port,
                iqn,
                chap,
            } => PrecheckResult::IscsiLuns(
                crate::storage_run::scan_luns(
                    &shared, &session, connection, &target, port, &iqn, chap,
                )
                .await
                .map_err(Arc::new),
            ),
            Precheck::Nrpe { host } => PrecheckResult::Nrpe(
                crate::pool_run::read_nrpe(&session, &host)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::PowerOnSecrets { hosts } => {
                let model = crate::storage_run::model(&shared, connection);
                PrecheckResult::PowerOnSecrets(
                    crate::pool_run::read_power_on_secrets(&session, &model, &hosts).await,
                )
            }
            Precheck::ClusterNetwork { cluster } => PrecheckResult::ClusterNetwork(
                xapi::cluster::get_network(&session, &cluster).await.ok(),
            ),
            Precheck::CurrentPassword { password } => PrecheckResult::CurrentPassword(
                shared
                    .connection_password(connection)
                    .await
                    .is_ok_and(|p| p == password),
            ),
            Precheck::HeartbeatSrs { srs } => {
                let mut out = Vec::new();
                for sr in srs {
                    // The plain call: the check needs no task.
                    let error = xapi::sr::assert_can_host_ha_statefile(&session, &sr)
                        .await
                        .err()
                        .map(Arc::new);
                    out.push((sr, error));
                }
                PrecheckResult::HeartbeatSrs(out)
            }
            Precheck::HaMaxFailures { restart } => {
                let config: HashMap<Ref<Vm>, String> = restart.into_iter().collect();
                let model = crate::storage_run::model(&shared, connection);
                PrecheckResult::HaMaxFailures(
                    xapi::pool::ha_compute_hypothetical_max_host_failures_to_tolerate(
                        &session, &config,
                    )
                    .await
                    .map(|max| crate::ha::tolerable(model.pool().map(|(_, p)| &**p), max))
                    .map_err(Arc::new),
                )
            }
            Precheck::Agility { vms } => {
                let mut out = Vec::new();
                for vm in vms {
                    let why = match xapi::vm::assert_agile(&session, &vm).await {
                        Ok(()) => None,
                        Err(xapi::Error::Api { code, .. }) => Some(code),
                        Err(e) => Some(e.to_string()),
                    };
                    out.push((vm, why));
                }
                PrecheckResult::Agility(out)
            }
            Precheck::LoggedInSubjects => PrecheckResult::LoggedInSubjects(
                xapi::session::get_all_subject_identifiers(&session)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::Migrate { .. } => {
                tracing::debug!("migration checks need the destination; see migrate_run");
                PrecheckResult::Migrate(Vec::new())
            }
            Precheck::ServerTime { host } => PrecheckResult::ServerTime(
                crate::dr_run::server_time(&session, &host)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::DrSupportedSrs { srs } => {
                PrecheckResult::DrSupportedSrs(crate::dr_run::supported_srs(&session, srs).await)
            }
            Precheck::DrProbe {
                sr_type,
                device_config,
            } => PrecheckResult::DrProbe(
                crate::dr_run::probe_metadata(
                    &shared,
                    &session,
                    connection,
                    &sr_type,
                    &device_config,
                )
                .await
                .map_err(Arc::new),
            ),
            Precheck::DrMetadata { srs } => PrecheckResult::DrMetadata(
                crate::dr_run::load_metadata(&session, &srs)
                    .await
                    .map_err(Arc::new),
            ),
            Precheck::DrRecoverable { vdi, items } => PrecheckResult::DrRecoverable(
                crate::dr_run::recoverable(&session, &vdi, items)
                    .await
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
