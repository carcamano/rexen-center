//! The VM and host operations of the console, and when each is offered.
//!
//! The rules follow the original console's commands (`XenAdmin/Commands`):
//! an operation is offered when the server lists it in the object's
//! `allowed_operations` and the original's extra conditions hold (a live
//! host, an enabled host to run on…). [`ActionRequest::rbac_methods`] lists
//! the API calls an operation makes, for the RBAC check made before it runs
//! (see [`crate::rbac`]).

use xapi::{
    Bond, DrTask, Host, HostAllowedOperations, HostRecord, Message, Network, Pif, Pool, Ref, Role,
    Sr, Subject, Vbd, Vdi, Vif, Vm, VmAppliance, VmOperations, VmPowerState, VmRecord, Vmss,
};

use crate::ha::VmStartup;

use crate::backend::Password;
use crate::properties::{ClusteringChange, PropertiesTarget, PropertyChanges};

use crate::model::{ConnectionModel, is_guest};
use crate::network::{
    IpChange, NetworkChanges, NewBondSpec, NewNetworkKind, NewNetworkSpec, VifSpec,
};
use crate::objects;
use crate::provisioning::{CopyMode, DiskSpec, NewVmSpec};
use crate::storage::{NewSrMode, NewSrSpec, SrAction, SrChanges};

/// VM operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VmAction {
    Start,
    CleanShutdown,
    CleanReboot,
    Suspend,
    Resume,
    Pause,
    Unpause,
    /// Force Shut Down.
    HardShutdown,
    /// Force Reboot.
    HardReboot,
    /// Start on a chosen server.
    StartOn(Ref<Host>),
    /// Resume on a chosen server.
    ResumeOn(Ref<Host>),
    /// Live migration to another server of the same pool.
    Migrate(Ref<Host>),
}

impl VmAction {
    /// The menu this action belongs to when it targets a chosen server.
    pub fn host_menu(&self) -> Option<HostMenu> {
        match self {
            Self::StartOn(_) => Some(HostMenu::StartOn),
            Self::ResumeOn(_) => Some(HostMenu::ResumeOn),
            Self::Migrate(_) => Some(HostMenu::Migrate),
            _ => None,
        }
    }

    /// The server this action targets, if any.
    pub fn target(&self) -> Option<&Ref<Host>> {
        match self {
            Self::StartOn(h) | Self::ResumeOn(h) | Self::Migrate(h) => Some(h),
            _ => None,
        }
    }
}

/// The VM submenus that list the pool's servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostMenu {
    StartOn,
    ResumeOn,
    Migrate,
}

impl HostMenu {
    /// The action for server `host`.
    pub fn action(self, host: Ref<Host>) -> VmAction {
        match self {
            Self::StartOn => VmAction::StartOn(host),
            Self::ResumeOn => VmAction::ResumeOn(host),
            Self::Migrate => VmAction::Migrate(host),
        }
    }
}

/// Host operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostAction {
    Reboot,
    Shutdown,
    PowerOn,
    /// Disable the host and migrate its VMs away (`host.evacuate`).
    EnterMaintenance,
    ExitMaintenance,
    RestartToolstack,
}

/// An object that can have performance alerts (`other_config["perfmon"]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PerfmonObjectRef {
    Vm(Ref<Vm>),
    Host(Ref<Host>),
    Sr(Ref<Sr>),
}

/// One operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionRequest {
    Vm {
        vm: Ref<Vm>,
        action: VmAction,
    },
    Host {
        host: Ref<Host>,
        action: HostAction,
    },
    /// Dismiss alerts: destroy their messages on the server
    /// (`DismissAlertsAction`).
    DismissAlerts {
        messages: Vec<Ref<Message>>,
    },
    /// Save the performance alerts of an object: `config` is the new
    /// `other_config["perfmon"]` XML, `None` removes the key
    /// (`PerfmonDefinitionAction`).
    SetPerfmon {
        object: PerfmonObjectRef,
        config: Option<String>,
    },
    /// The New VM wizard (`CreateVMAction`).
    NewVm(Box<NewVmSpec>),
    /// Copy VM / Copy Template (`VMCloneAction`, `VMCopyAction`).
    CopyVm {
        vm: Ref<Vm>,
        name: String,
        description: String,
        mode: CopyMode,
    },
    /// Create Template From Snapshot: a clone of the snapshot, marked
    /// `instant` (`NewTemplateFromSnapshotCommand`).
    TemplateFromSnapshot {
        snapshot: Ref<Vm>,
        name: String,
        description: String,
    },
    /// Convert to Template (`VMToTemplateAction`).
    MakeTemplate {
        vm: Ref<Vm>,
    },
    /// Delete VM or template with the chosen disks and snapshots
    /// (`VMDestroyAction`).
    DeleteVm {
        vm: Ref<Vm>,
        disks: Vec<Ref<Vdi>>,
        snapshots: Vec<Ref<Vm>>,
    },
    /// Take Snapshot (`VMSnapshotCreateAction`); `memory` takes a
    /// checkpoint.
    Snapshot {
        vm: Ref<Vm>,
        name: String,
        description: String,
        memory: bool,
    },
    /// Revert to a snapshot (`VMSnapshotRevertAction`). With
    /// `snapshot_first` (name, description), a snapshot of the VM's current
    /// state is taken first and the revert only runs if it worked (the
    /// confirmation's "Take a snapshot of the VM's current state and then
    /// revert").
    Revert {
        snapshot: Ref<Vm>,
        snapshot_first: Option<(String, String)>,
    },
    /// Delete snapshots with their own disks (`VMSnapshotDeleteAction`).
    DeleteSnapshots {
        snapshots: Vec<Ref<Vm>>,
    },
    /// Add Virtual Disk (`CreateDiskAction` + `VbdCreateAndPlugAction`).
    NewDisk {
        vm: Ref<Vm>,
        disk: DiskSpec,
    },
    /// Attach an existing disk (`AttachDiskDialog`).
    AttachDisk {
        vm: Ref<Vm>,
        vdi: Ref<Vdi>,
        read_only: bool,
    },
    /// Detach a disk, keeping it (`DetachVirtualDiskAction`).
    DetachDisk {
        vm: Ref<Vm>,
        vbd: Ref<Vbd>,
    },
    /// Delete a disk (`DestroyDiskAction`).
    DeleteDisk {
        vdi: Ref<Vdi>,
    },
    /// Resize a disk (the disk's Size and Location page); bytes.
    ResizeDisk {
        vdi: Ref<Vdi>,
        size: i64,
    },
    /// Move a disk to another SR (`MoveVirtualDiskAction`, or
    /// `MigrateVirtualDiskAction` while a running VM uses it).
    MoveDisk {
        vdi: Ref<Vdi>,
        sr: Ref<Sr>,
    },
    /// Load an ISO into a DVD drive, or empty it (`ChangeVMISOAction`).
    ChangeCd {
        vm: Ref<Vm>,
        vbd: Ref<Vbd>,
        iso: Option<Ref<Vdi>>,
    },
    /// Add a DVD drive to a VM (`CreateCdDriveAction`).
    NewCdDrive {
        vm: Ref<Vm>,
    },
    /// Insert the guest tools ISO (`InstallPVToolsAction`).
    InstallTools {
        vm: Ref<Vm>,
    },
    /// The New SR wizard: create, introduce or reattach an SR
    /// (`SrCreateAction`, `SrIntroduceAction`, `SrReattachAction`).
    NewSr(Box<NewSrSpec>),
    /// An SR operation of the Storage menu.
    Sr {
        sr: Ref<Sr>,
        action: SrAction,
    },
    /// The SR Properties dialog: the fields that changed.
    SetSrProperties {
        sr: Ref<Sr>,
        changes: SrChanges,
    },
    /// The New Network wizard: an external (VLAN), single-server or
    /// cross-server private network (`NetworkAction`, `CreateChinAction`).
    NewNetwork(Box<NewNetworkSpec>),
    /// A bonded network (`CreateBondAction`).
    NewBond(Box<NewBondSpec>),
    /// Remove Network (`NetworkAction` destroying a network).
    RemoveNetwork {
        network: Ref<Network>,
    },
    /// Delete Bond (`DestroyBondAction`): `bond` and its equivalents on the
    /// other hosts, and their network.
    DestroyBond {
        bond: Ref<Bond>,
    },
    /// The network Properties dialog: the fields that changed.
    SetNetworkProperties {
        network: Ref<Network>,
        changes: NetworkChanges,
    },
    /// Rescan on the NICs tab (`RescanPIFsAction`).
    RescanPifs {
        host: Ref<Host>,
    },
    /// Configure IP Addresses (`ChangeNetworkingAction`).
    ChangeNetworking(Box<IpChange>),
    /// Renaming secondary interfaces (`SetSecondaryManagementPurposeAction`);
    /// with `pool`, on every host's PIF of the same network.
    RenameInterfaces {
        pool: bool,
        pifs: Vec<(Ref<Pif>, String)>,
    },
    /// Add Interface (`CreateVIFAction`).
    NewVif {
        vm: Ref<Vm>,
        spec: VifSpec,
    },
    /// Remove Interface (`DeleteVIFAction`).
    DeleteVif {
        vif: Ref<Vif>,
    },
    /// The interface's Properties (`UpdateVIFAction`: deleted and made
    /// again).
    UpdateVif {
        vif: Ref<Vif>,
        spec: VifSpec,
    },
    /// Activate (`PlugVIFAction`).
    PlugVif {
        vif: Ref<Vif>,
    },
    /// Deactivate (`UnplugVIFAction`).
    UnplugVif {
        vif: Ref<Vif>,
    },
    /// Make into standalone server (`DestroyPoolAction`): the pool of one
    /// loses its name.
    DestroyPool {
        pool: Ref<Pool>,
    },
    /// Remove a server from the pool (`EjectHostFromPoolAction`); the
    /// console then connects to it once it is back.
    EjectHost {
        host: Ref<Host>,
    },
    /// Enter maintenance mode on the coordinator and hand the pool to
    /// another server (`EvacuateHostAction` with a new coordinator).
    EvacuateCoordinator {
        host: Ref<Host>,
        new_coordinator: Ref<Host>,
    },
    /// The pool or server Properties dialog: what changed.
    SetProperties {
        target: PropertiesTarget,
        changes: Box<PropertyChanges>,
    },
    /// Control Domain Memory (`ChangeControlDomainMemoryAction`, then the
    /// server reboots as `RebootHostAction`); bytes.
    SetDom0Memory {
        host: Ref<Host>,
        memory: i64,
    },
    /// Change Server Password (`ChangeHostPasswordAction`).
    ChangePassword {
        old: Password,
        new: Password,
    },
    /// Rotate Pool Secret (`pool.rotate_secret`).
    RotatePoolSecret,
    /// Enable Certificate Verification (`pool.enable_tls_verification`).
    EnableTlsVerification,
    /// Install Certificate (`InstallCertificateAction`): the files' paths.
    InstallCertificate {
        host: Ref<Host>,
        key: String,
        certificate: String,
        chain: Vec<String>,
    },
    /// Reset to Default Certificate (`host.reset_server_certificate`).
    ResetCertificate {
        host: Ref<Host>,
    },
    /// Back Up (`HostBackupRestoreAction`: `GET /host_backup` to `path`).
    BackupHost {
        host: Ref<Host>,
        path: String,
    },
    /// Restore From Backup (`PUT /host_restore` from `path`).
    RestoreHost {
        host: Ref<Host>,
        path: String,
    },
    /// The Configure HA wizard (`EnableHAAction`): the VMs' startup
    /// options, the ntol, then `pool.enable_ha` with the heartbeat SR.
    EnableHa {
        heartbeat_sr: Ref<Sr>,
        ntol: i64,
        startup: Vec<VmStartup>,
    },
    /// Disable HA (`DisableHAAction`).
    DisableHa,
    /// The HA settings dialog (`SetHaPrioritiesAction`).
    SetHaPriorities {
        startup: Vec<VmStartup>,
        ntol: i64,
    },
    /// Raise (or set) the pool's ntol, after the question asked once a
    /// server came out of maintenance mode (`EnableNtolDialog`).
    SetHaNtol {
        ntol: i64,
    },
    /// A server operation of an HA pool that first lowers the ntol to
    /// `ntol` (`MaybeReduceNtolBeforeOp`, after `NtolDialog`).
    HaHost {
        host: Ref<Host>,
        action: HostAction,
        ntol: i64,
    },
    /// Start or resume a VM of an HA pool after fixing what HA refused:
    /// `unprotect` sets "do not restart" first (`HAUnprotectVMAction`, the
    /// VM is protected but not agile); `ntol` lowers the pool's ntol first
    /// (`HA_LOWERING_NTOL`, the start broke the failover plan).
    HaVm {
        vm: Ref<Vm>,
        action: VmAction,
        unprotect: bool,
        ntol: Option<i64>,
    },
    /// Join a domain (`EnableAdAction`).
    EnableAd {
        domain: String,
        user: String,
        password: Password,
    },
    /// Leave the domain (`DisableAdAction`), with the domain credentials to
    /// remove the machine accounts, or without ("Skip").
    DisableAd {
        credentials: Option<(String, Password)>,
    },
    /// Add users and groups (`AddRemoveSubjectsAction` from the Add Users
    /// dialog): each name is resolved, then given access.
    AddSubjects {
        names: Vec<String>,
    },
    /// Remove access (`AddRemoveSubjectsAction`): their sessions are logged
    /// out and the subjects destroyed.
    RemoveSubjects {
        subjects: Vec<Ref<Subject>>,
    },
    /// Change Role (`AddRemoveRolesAction` per subject): each ends with
    /// exactly `roles`; then the logged-in ones in `log_out` are logged out.
    SetSubjectRoles {
        subjects: Vec<Ref<Subject>>,
        roles: Vec<Ref<Role>>,
        log_out: Vec<Ref<Subject>>,
    },
    /// Log Out (`Session.logout_subject_identifier` per subject).
    LogoutSubjects {
        subjects: Vec<Ref<Subject>>,
    },
    /// Export to an XVA file (`ExportVmAction`: `GET /export` into `path`,
    /// checked again afterwards with `verify`).
    ExportXva {
        vm: Ref<Vm>,
        path: String,
        verify: bool,
    },
    /// The Import wizard's XVA import (`ImportVmAction`: `PUT /import` of
    /// `path` into `sr`, `affinity` as the home server). The wizard ends it
    /// with [`crate::Command::FinishImport`].
    ImportXva {
        path: String,
        sr: Ref<Sr>,
        affinity: Option<Ref<Host>>,
    },
    /// The Export wizard's OVF/OVA export (`ExportApplianceAction`, F11b).
    ExportOvf(Box<crate::ovf_run::OvfExport>),
    /// The Import wizard's OVF/OVA or disk image import
    /// (`ImportApplianceAction` / `ImportImageAction`, F11b).
    ImportOvf(Box<crate::ovf_run::OvfImport>),
    /// The Migrate / Move / Copy wizard's `VM.migrate_send` to another pool
    /// or with storage motion (`VMCrossPoolMigrateAction`, F12a). The
    /// receiving server is asked on the connection `spec.target`.
    MigrateSend(Box<crate::migrate::MigrateSpec>),
    /// Move VM within the pool (`VMMoveAction`): each owned disk whose SR
    /// changes is copied to it, the old one destroyed and a drive made for
    /// the copy.
    MoveVm {
        vm: Ref<Vm>,
        host: Ref<Host>,
        storage: Vec<(Ref<Vdi>, Ref<Sr>)>,
    },
    /// New vApp (`CreateVMApplianceAction`, then
    /// `SetVMStartupOptionsAction`).
    NewVApp(Box<crate::vapp::NewVAppSpec>),
    /// vApp Properties: name and description, members
    /// (`AssignVMsToVMApplianceAction`) and startup sequence
    /// (`SetVMStartupOptionsAction`).
    SetVApp {
        vapp: Ref<VmAppliance>,
        changes: Box<crate::vapp::VAppChanges>,
    },
    /// Assign to vApp (`AssignVMsToVMApplianceAction`): `vms` become the
    /// vApp's members.
    AssignVApp {
        vapp: Ref<VmAppliance>,
        vms: Vec<Ref<Vm>>,
    },
    /// Leave a vApp (`RemoveVMsFromVMApplianceAction`).
    RemoveFromVApp {
        vapp: Ref<VmAppliance>,
        vms: Vec<Ref<Vm>>,
    },
    /// Delete vApps (`DestroyVMApplianceAction`); their VMs stay.
    DeleteVApps {
        vapps: Vec<Ref<VmAppliance>>,
    },
    /// Start vApp (`StartApplianceAction`): its VMs in start order.
    StartVApp {
        vapp: Ref<VmAppliance>,
    },
    /// Shut Down vApp (`ShutDownApplianceAction`).
    ShutDownVApp {
        vapp: Ref<VmAppliance>,
    },
    /// Force shut down a vApp (`HardShutDownApplianceAction`, a DR
    /// pre-check's fix).
    HardShutDownVApp {
        vapp: Ref<VmAppliance>,
    },
    /// New Snapshot Schedule (`CreateVMPolicy`, F12b).
    NewPolicy(Box<crate::vmss::NewPolicySpec>),
    /// Enable or Disable a snapshot schedule (`ChangePolicyEnabledAction`).
    SetPolicyEnabled {
        policy: Ref<Vmss>,
        enabled: bool,
    },
    /// Run Now (`RunPolicyNowAction`).
    RunPolicyNow {
        policy: Ref<Vmss>,
    },
    /// Delete snapshot schedules (`DestroyPolicyAction`); their VMs stay.
    DeletePolicies {
        policies: Vec<Ref<Vmss>>,
    },
    /// Assign to Snapshot Schedule (`AssignVMsToPolicyAction`): `vms`
    /// become the policy's VMs.
    AssignPolicy {
        policy: Ref<Vmss>,
        vms: Vec<Ref<Vm>>,
    },
    /// Leave a snapshot schedule (`RemoveVMsFromPolicyAction`).
    RemoveFromPolicy {
        policy: Ref<Vmss>,
        vms: Vec<Ref<Vm>>,
    },
    /// The snapshot schedule Properties dialog.
    SetPolicy {
        policy: Ref<Vmss>,
        changes: Box<crate::vmss::PolicyChanges>,
    },
    /// Disaster Recovery > Configure: replication turned on and off.
    ConfigureDr {
        enable: Vec<Ref<Sr>>,
        disable: Vec<Ref<Sr>>,
    },
    /// `DrTaskCreateAction`: the SRs of a device introduced for recovery;
    /// the `DR_task` made is the operation's `result`.
    DrTaskCreate(Box<crate::dr::DrTaskSpec>),
    /// `DR_task.destroy` (`srs`: the names of its SRs, for the title).
    DrTaskDestroy {
        task: Ref<DrTask>,
        srs: String,
    },
    /// `DrRecoverAction`: one VM or vApp of the metadata on `vdi`
    /// (references of that database; `name` for the title).
    DrRecover {
        vdi: Ref<Vdi>,
        item: crate::dr::DrItem,
        name: String,
    },
    /// `StartVMsAndAppliancesAction` after a recovery.
    DrStart {
        vms: Vec<Ref<Vm>>,
        vapps: Vec<Ref<VmAppliance>>,
        paused: bool,
    },
    /// `ShutdownAndDestroyVMsAction` (disks kept).
    DrDestroyVms {
        vms: Vec<Ref<Vm>>,
    },
    /// `ShutdownAndDestroyVmAppliancesAction`.
    DrDestroyVApps {
        vapps: Vec<Ref<VmAppliance>>,
    },
    /// Folders, tags, custom fields and saved searches (F13, run by
    /// [`crate::org_run`]).
    Org(Box<crate::org::OrgRequest>),
}

impl ActionRequest {
    /// A migration or move of the F12a wizard (run by
    /// [`crate::migrate_run`]).
    pub fn is_migrate(&self) -> bool {
        matches!(self, Self::MigrateSend(_) | Self::MoveVm { .. })
    }

    /// A vApp operation (run by [`crate::vapp_run`]).
    pub fn is_vapp(&self) -> bool {
        matches!(
            self,
            Self::NewVApp(_)
                | Self::SetVApp { .. }
                | Self::AssignVApp { .. }
                | Self::RemoveFromVApp { .. }
                | Self::DeleteVApps { .. }
                | Self::StartVApp { .. }
                | Self::ShutDownVApp { .. }
        )
    }

    /// A snapshot schedule operation (run by [`crate::vmss_run`]).
    pub fn is_vmss(&self) -> bool {
        matches!(
            self,
            Self::NewPolicy(_)
                | Self::SetPolicyEnabled { .. }
                | Self::RunPolicyNow { .. }
                | Self::DeletePolicies { .. }
                | Self::AssignPolicy { .. }
                | Self::RemoveFromPolicy { .. }
                | Self::SetPolicy { .. }
        )
    }

    /// A folder, tag, custom field or saved search operation (run by
    /// [`crate::org_run`]).
    pub fn is_org(&self) -> bool {
        matches!(self, Self::Org(_))
    }

    /// A disaster recovery operation (run by [`crate::dr_run`]).
    pub fn is_dr(&self) -> bool {
        matches!(
            self,
            Self::ConfigureDr { .. }
                | Self::DrTaskCreate(_)
                | Self::DrTaskDestroy { .. }
                | Self::DrRecover { .. }
                | Self::DrStart { .. }
                | Self::DrDestroyVms { .. }
                | Self::DrDestroyVApps { .. }
                | Self::HardShutDownVApp { .. }
        )
    }

    /// The other connection an operation needs (the destination of a
    /// migration), if any.
    pub fn target_connection(&self) -> Option<crate::backend::ConnectionId> {
        match self {
            Self::MigrateSend(spec) => Some(spec.target),
            _ => None,
        }
    }

    /// An XVA export or import (run by [`crate::xva_run`]).
    pub fn is_transfer(&self) -> bool {
        matches!(
            self,
            Self::ExportXva { .. }
                | Self::ImportXva { .. }
                | Self::ExportOvf(_)
                | Self::ImportOvf(_)
        )
    }

    /// An HA request (run by [`crate::ha_run`]).
    pub fn is_ha(&self) -> bool {
        matches!(
            self,
            Self::EnableHa { .. }
                | Self::DisableHa
                | Self::SetHaPriorities { .. }
                | Self::SetHaNtol { .. }
                | Self::HaHost { .. }
                | Self::HaVm { .. }
        )
    }

    /// An Active Directory request (run by [`crate::ha_run`]).
    pub fn is_ad(&self) -> bool {
        matches!(
            self,
            Self::EnableAd { .. }
                | Self::DisableAd { .. }
                | Self::AddSubjects { .. }
                | Self::RemoveSubjects { .. }
                | Self::SetSubjectRoles { .. }
                | Self::LogoutSubjects { .. }
        )
    }

    /// The RBAC calls of an HA or AD request (the original's
    /// `ApiMethodsToRoleCheck`).
    fn ha_ad_rbac_methods(&self) -> Vec<&'static str> {
        const PRIORITIES: [&str; 3] = [
            "vm.set_ha_restart_priority",
            "vm.set_order",
            "vm.set_start_delay",
        ];
        const NTOL: &str = "pool.set_ha_host_failures_to_tolerate";
        let mut m: Vec<&'static str> = match self {
            Self::EnableHa { .. } => {
                let mut m = PRIORITIES.to_vec();
                m.extend([NTOL, "pool.enable_ha"]);
                m
            }
            Self::DisableHa => vec!["pool.disable_ha"],
            Self::SetHaPriorities { .. } => {
                let mut m = PRIORITIES.to_vec();
                m.extend([NTOL, "pool.sync_database"]);
                m
            }
            Self::SetHaNtol { .. } => vec![NTOL],
            Self::HaHost { host, action, .. } => {
                let mut m = Self::Host {
                    host: host.clone(),
                    action: *action,
                }
                .rbac_methods();
                m.push(NTOL);
                return m;
            }
            Self::HaVm {
                vm,
                action,
                unprotect,
                ntol,
            } => {
                let mut m = Self::Vm {
                    vm: vm.clone(),
                    action: action.clone(),
                }
                .rbac_methods();
                if *unprotect {
                    m.push("vm.set_ha_restart_priority");
                }
                if ntol.is_some() {
                    m.push(NTOL);
                }
                return m;
            }
            // The original lists no call for joining; joining replaces any
            // earlier configuration first.
            Self::EnableAd { .. } => {
                return vec!["pool.disable_external_auth", "pool.enable_external_auth"];
            }
            Self::DisableAd { .. } => return crate::ad::rbac_calls::LEAVE.to_vec(),
            Self::AddSubjects { .. } => return crate::ad::rbac_calls::ADD.to_vec(),
            Self::RemoveSubjects { .. } => return crate::ad::rbac_calls::REMOVE.to_vec(),
            Self::SetSubjectRoles { log_out, .. } => {
                let mut m = vec!["subject.add_to_roles", "subject.remove_from_roles"];
                if !log_out.is_empty() {
                    m.extend(crate::ad::rbac_calls::LOGOUT);
                }
                return m;
            }
            Self::LogoutSubjects { .. } => return crate::ad::rbac_calls::LOGOUT.to_vec(),
            _ => return Vec::new(),
        };
        m.push("task.destroy");
        m
    }

    /// An F10a pool or properties request (run by [`crate::pool_run`]).
    pub fn is_pool(&self) -> bool {
        matches!(
            self,
            Self::DestroyPool { .. }
                | Self::EjectHost { .. }
                | Self::EvacuateCoordinator { .. }
                | Self::SetProperties { .. }
                | Self::SetDom0Memory { .. }
                | Self::ChangePassword { .. }
                | Self::RotatePoolSecret
                | Self::EnableTlsVerification
                | Self::InstallCertificate { .. }
                | Self::ResetCertificate { .. }
                | Self::BackupHost { .. }
                | Self::RestoreHost { .. }
        )
    }

    /// The RBAC calls of an F10a request.
    fn pool_rbac_methods(&self) -> Vec<&'static str> {
        match self {
            Self::DestroyPool { .. } => vec!["pool.set_name_label", "pool.set_name_description"],
            Self::EjectHost { .. } => vec!["pool.eject"],
            Self::EvacuateCoordinator { .. } => vec![
                "host.disable",
                "host.enable",
                "host.evacuate",
                "host.add_to_other_config",
                "host.remove_from_other_config",
                "pool.designate_new_master",
            ],
            Self::SetProperties { target, changes } => properties_rbac(target, changes),
            Self::SetDom0Memory { .. } => vec![
                "vm.set_memory",
                "host.disable",
                "host.enable",
                "vm.clean_shutdown",
                "vm.hard_shutdown",
                "host.reboot",
            ],
            // Every user may change their own password (the original does
            // not check, and only offers it to the superuser).
            Self::ChangePassword { .. } => vec![],
            Self::RotatePoolSecret => vec!["pool.rotate_secret"],
            Self::EnableTlsVerification => vec!["pool.enable_tls_verification"],
            Self::InstallCertificate { .. } => vec!["host.install_server_certificate"],
            Self::ResetCertificate { .. } => vec!["host.reset_server_certificate"],
            Self::BackupHost { .. } => vec!["http/get_host_backup"],
            Self::RestoreHost { .. } => vec!["http/put_host_restore"],
            _ => Vec::new(),
        }
    }
}

/// The calls the Properties dialog's save makes (`SaveChangesAction` and
/// the pages' actions).
fn properties_rbac(target: &PropertiesTarget, c: &PropertyChanges) -> Vec<&'static str> {
    let host = matches!(target, PropertiesTarget::Host(_));
    let mut m = Vec::new();
    if c.name.is_some() {
        m.push(if host {
            "host.set_name_label"
        } else {
            "pool.set_name_label"
        });
    }
    if c.description.is_some() {
        m.push(if host {
            "host.set_name_description"
        } else {
            "pool.set_name_description"
        });
    }
    if c.tags.is_some() {
        m.push(if host {
            "host.set_tags"
        } else {
            "pool.set_tags"
        });
    }
    if c.iqn.is_some() {
        m.push("host.set_iscsi_iqn");
    }
    if let Some(config) = &c.perfmon {
        m.push("host.remove_from_other_config/key:perfmon");
        if config.is_some() {
            m.push("host.add_to_other_config/key:perfmon");
        }
    }
    if c.email.is_some() {
        m.extend(["pool.remove_from_other_config", "pool.add_to_other_config"]);
    }
    if c.multipath.is_some() {
        m.extend([
            "pbd.plug",
            "pbd.unplug",
            "host.set_multipathing",
            "host.remove_from_other_config/key:multipathhandle",
            "host.add_to_other_config/key:multipathhandle",
        ]);
    }
    if c.syslog.is_some() {
        m.extend(["host.set_logging", "host.syslog_reconfigure"]);
    }
    if !c.power_on.is_empty() {
        m.push("host.set_power_on_mode");
        if c.power_on.iter().any(|(_, mode)| {
            mode.config()
                .contains_key(crate::properties::POWER_ON_PASSWORD_SECRET)
        }) {
            m.extend(["secret.create", "secret.get_by_uuid", "secret.destroy"]);
        }
    }
    if c.autostart.is_some() {
        m.extend([
            "pool.remove_from_other_config/key:auto_poweron",
            "pool.add_to_other_config/key:auto_poweron",
        ]);
    }
    if c.live_patching_disabled.is_some() {
        m.push("pool.set_live_patching_disabled");
    }
    if c.igmp_snooping.is_some() {
        m.push("pool.set_igmp_snooping_enabled");
    }
    match &c.clustering {
        Some(ClusteringChange::Enable(_)) => {
            m.extend(["cluster.pool_create", "pif.set_disallow_unplug"])
        }
        Some(ClusteringChange::Disable) => {
            m.extend(["cluster.pool_destroy", "pif.set_disallow_unplug"])
        }
        None => {}
    }
    if c.migration_compression.is_some() {
        m.push("pool.set_migration_compression");
    }
    if c.nrpe.is_some() {
        m.push("host.call_plugin");
    }
    m
}

impl ActionRequest {
    /// An F9 networking request.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::NewNetwork(_)
                | Self::NewBond(_)
                | Self::RemoveNetwork { .. }
                | Self::DestroyBond { .. }
                | Self::SetNetworkProperties { .. }
                | Self::RescanPifs { .. }
                | Self::ChangeNetworking(_)
                | Self::RenameInterfaces { .. }
                | Self::NewVif { .. }
                | Self::DeleteVif { .. }
                | Self::UpdateVif { .. }
                | Self::PlugVif { .. }
                | Self::UnplugVif { .. }
        )
    }

    /// The RBAC calls of an F9 request.
    fn network_rbac_methods(&self) -> Vec<&'static str> {
        match self {
            Self::NewNetwork(spec) => match &spec.kind {
                NewNetworkKind::External { .. } => {
                    vec![
                        "network.create",
                        "pool.create_vlan_from_pif",
                        "network.destroy",
                    ]
                }
                NewNetworkKind::Internal => vec!["network.create"],
                NewNetworkKind::CrossServer { .. } => vec![
                    "network.create",
                    "tunnel.create",
                    "tunnel.destroy",
                    "network.destroy",
                ],
            },
            Self::NewBond(_) => vec![
                "host.management_reconfigure",
                "network.create",
                "network.destroy",
                "network.remove_from_other_config",
                "pif.reconfigure_ip",
                "pif.plug",
                "pif.set_other_config",
                "bond.create",
                "bond.destroy",
            ],
            Self::RemoveNetwork { .. } => vec!["network.destroy", "vlan.destroy", "tunnel.destroy"],
            Self::DestroyBond { .. } => vec![
                "host.management_reconfigure",
                "network.destroy",
                "pif.reconfigure_ip",
                "pif.plug",
                "pif.set_other_config",
                "bond.destroy",
            ],
            Self::SetNetworkProperties { changes, .. } => {
                let mut m = Vec::new();
                if changes.name.is_some() {
                    m.push("network.set_name_label");
                }
                if changes.description.is_some() {
                    m.push("network.set_name_description");
                }
                if changes.tags.is_some() {
                    m.push("network.set_tags");
                }
                if changes.auto_plug.is_some() {
                    m.extend([
                        "network.remove_from_other_config",
                        "network.add_to_other_config",
                    ]);
                }
                if changes.mtu.is_some() {
                    m.push("network.set_mtu");
                }
                if changes.bond_mode.is_some() {
                    m.push("bond.set_mode");
                }
                if changes.hashing.is_some() {
                    m.push("bond.set_property");
                }
                if let Some(pifs) = &changes.pifs {
                    m.extend(["vlan.destroy", "tunnel.destroy"]);
                    if matches!(pifs, crate::network::PifChange::External { .. }) {
                        m.push("pool.create_vlan_from_pif");
                    }
                } else if changes.replug() {
                    m.extend(["pif.unplug", "pif.plug", "vif.unplug", "vif.plug"]);
                }
                m
            }
            Self::RescanPifs { .. } => vec!["pif.scan"],
            Self::ChangeNetworking(_) => vec![
                "host.management_reconfigure",
                "pool.management_reconfigure",
                "pif.reconfigure_ip",
                "pif.plug",
                "pif.set_disallow_unplug",
                "pif.set_other_config",
            ],
            Self::RenameInterfaces { .. } => vec!["pif.set_other_config"],
            Self::NewVif { .. } => vec!["vif.create", "vif.plug"],
            Self::DeleteVif { .. } => vec!["vif.unplug", "vif.destroy"],
            Self::UpdateVif { .. } => vec!["vif.unplug", "vif.destroy", "vif.create", "vif.plug"],
            Self::PlugVif { .. } => vec!["vif.plug"],
            Self::UnplugVif { .. } => vec!["vif.unplug"],
            _ => Vec::new(),
        }
    }
}

/// `task.destroy`, called once a task has been followed to its end.
const TASK: &[&str] = &["task.destroy"];
/// `SetVMStartupOptionsAction`.
const STARTUP: &[&str] = &["vm.set_order", "vm.set_start_delay", "pool.sync_database"];

impl ActionRequest {
    /// The API calls the operation makes, as RBAC permission names
    /// (lowercase `class.method`, without `async_`). The original lists the
    /// same calls in each action's `ApiMethodsToRoleCheck`.
    pub fn rbac_methods(&self) -> Vec<&'static str> {
        if self.is_ha() || self.is_ad() {
            return self.ha_ad_rbac_methods();
        }
        if self.is_pool() {
            let mut m = self.pool_rbac_methods();
            m.push("task.destroy");
            return m;
        }
        if self.is_network() {
            let mut m = self.network_rbac_methods();
            m.push("task.destroy");
            return m;
        }
        let own: &[&str] = match self {
            Self::DismissAlerts { .. } => return vec!["message.destroy"],
            Self::Org(request) => return request.rbac_methods(),
            // `VMCrossPoolMigrateAction.StaticRBACDependencies` on the
            // source (`host.migrate_receive` is the destination's).
            Self::MigrateSend(_) => &["vm.migrate_send", "vm.assert_can_migrate", "task.destroy"],
            // The calls `VMMoveAction` makes.
            Self::MoveVm { .. } => &[
                "vdi.copy",
                "vdi.destroy",
                "vbd.create",
                "vm.set_suspend_sr",
                "task.destroy",
            ],
            Self::NewVApp(spec) => {
                let mut m = vec!["vm_appliance.create", "vm.set_appliance"];
                if !spec.startup.is_empty() {
                    m.extend(STARTUP);
                }
                m.extend(TASK);
                return m;
            }
            Self::SetVApp { changes, .. } => {
                let mut m = Vec::new();
                if changes.name.is_some() {
                    m.push("vm_appliance.set_name_label");
                }
                if changes.description.is_some() {
                    m.push("vm_appliance.set_name_description");
                }
                if changes.members.is_some() {
                    m.push("vm.set_appliance");
                }
                if !changes.startup.is_empty() {
                    m.extend(STARTUP);
                    m.extend(TASK);
                }
                return m;
            }
            Self::AssignVApp { .. } | Self::RemoveFromVApp { .. } => &["vm.set_appliance"],
            Self::DeleteVApps { .. } => &["vm.set_appliance", "vm_appliance.destroy"],
            Self::StartVApp { .. } => &["vm_appliance.start", "task.destroy"],
            Self::ShutDownVApp { .. } => &["vm_appliance.shutdown", "task.destroy"],
            Self::HardShutDownVApp { .. } => &["vm_appliance.hard_shutdown", "task.destroy"],
            // `CreateVMPolicy`.
            Self::NewPolicy(spec) => {
                let mut m = vec!["vmss.create", "task.destroy"];
                if !spec.vms.is_empty() {
                    m.push("vm.set_snapshot_schedule");
                }
                if spec.run_now {
                    m.push("vmss.snapshot_now");
                }
                return m;
            }
            Self::SetPolicyEnabled { .. } => &["vmss.set_enabled"],
            Self::RunPolicyNow { .. } => &["vmss.snapshot_now"],
            Self::DeletePolicies { .. } => &["vm.set_snapshot_schedule", "vmss.destroy"],
            Self::AssignPolicy { .. } | Self::RemoveFromPolicy { .. } => {
                &["vm.set_snapshot_schedule"]
            }
            Self::SetPolicy { changes, .. } => {
                let c = &**changes;
                let mut m = Vec::new();
                for (on, name) in [
                    (c.name.is_some(), "vmss.set_name_label"),
                    (c.description.is_some(), "vmss.set_name_description"),
                    (c.kind.is_some(), "vmss.set_type"),
                    (c.retained.is_some(), "vmss.set_retained_snapshots"),
                    (c.frequency.is_some(), "vmss.set_frequency"),
                    (c.schedule.is_some(), "vmss.set_schedule"),
                    (c.members.is_some(), "vm.set_snapshot_schedule"),
                ] {
                    if on {
                        m.push(name);
                    }
                }
                return m;
            }
            Self::ConfigureDr { enable, disable } => {
                let mut m = Vec::new();
                if !disable.is_empty() {
                    m.push("sr.disable_database_replication");
                }
                if !enable.is_empty() {
                    m.push("sr.enable_database_replication");
                }
                return m;
            }
            Self::DrTaskCreate(_) => &["dr_task.create", "task.destroy"],
            Self::DrTaskDestroy { .. } => &["dr_task.destroy"],
            Self::DrRecover { item, .. } => match item {
                crate::dr::DrItem::Vm(_) => &["vdi.open_database", "vm.recover", "task.destroy"],
                crate::dr::DrItem::VApp(_) => {
                    &["vdi.open_database", "vm_appliance.recover", "task.destroy"]
                }
            },
            Self::DrStart { vms, vapps, .. } => {
                let mut m = vec!["task.destroy"];
                if !vms.is_empty() {
                    m.push("vm.start");
                }
                if !vapps.is_empty() {
                    m.push("vm_appliance.start");
                }
                return m;
            }
            Self::DrDestroyVms { .. } => &[
                "vm.hard_shutdown",
                "vm.destroy",
                "vbd.destroy",
                "vif.destroy",
            ],
            Self::DrDestroyVApps { .. } => &[
                "vm_appliance.hard_shutdown",
                "vm_appliance.destroy",
                "vm.destroy",
                "vbd.unplug",
                "vbd.destroy",
                "vif.unplug",
                "vif.destroy",
            ],
            // `ExportVmAction` / `ImportVmAction.ConstantRBACRequirements`.
            Self::ExportXva { .. } => &["task.create", "http/get_export"],
            // `ApplianceAction.StaticRBACDependencies` and the transfers.
            Self::ExportOvf(_) => &["task.create", "http/get_export_raw_vdi"],
            Self::ImportOvf(spec) => {
                let mut m = vec![
                    "vm.create",
                    "vm.destroy",
                    "vm.remove_from_other_config",
                    "vdi.create",
                    "vdi.destroy",
                    "vbd.create",
                    "vif.create",
                    "task.create",
                    "http/put_import_raw_vdi",
                ];
                if spec.start {
                    m.push("vm.start");
                }
                m.extend(TASK);
                return m;
            }
            Self::ImportXva { affinity, .. } => {
                let mut m = vec![
                    "task.create",
                    "vm.set_name_label",
                    "network.destroy",
                    "vif.create",
                    "vif.destroy",
                    "vif.move",
                    "http/put_import",
                ];
                if affinity.is_some() {
                    m.push("vm.set_affinity");
                }
                m.extend(TASK);
                return m;
            }
            Self::SetPerfmon { object, config } => {
                let (remove, add) = match object {
                    PerfmonObjectRef::Vm(_) => (
                        "vm.remove_from_other_config/key:perfmon",
                        "vm.add_to_other_config/key:perfmon",
                    ),
                    PerfmonObjectRef::Host(_) => (
                        "host.remove_from_other_config/key:perfmon",
                        "host.add_to_other_config/key:perfmon",
                    ),
                    PerfmonObjectRef::Sr(_) => (
                        "sr.remove_from_other_config/key:perfmon",
                        "sr.add_to_other_config/key:perfmon",
                    ),
                };
                return match config {
                    Some(_) => vec![remove, add],
                    None => vec![remove],
                };
            }
            Self::NewVm(spec) => {
                let mut m = vec![
                    "vm.clone",
                    "vm.provision",
                    "vm.set_other_config",
                    "vm.set_name_label",
                    "vm.set_name_description",
                    "vm.set_vcpus_max",
                    "vm.set_vcpus_at_startup",
                    "vm.set_platform",
                    "vm.set_hvm_boot_params",
                    "vm.set_memory_limits",
                    "vm.set_affinity",
                    "vm.set_suspend_sr",
                    "vbd.create",
                    "vbd.destroy",
                    "vbd.eject",
                    "vbd.insert",
                    "vdi.create",
                    "vdi.destroy",
                    "vdi.copy",
                    "vdi.set_sm_config",
                    "vif.create",
                    "vif.destroy",
                    "vm.destroy",
                ];
                if spec.full_copy_sr.is_some() {
                    m.push("vm.copy");
                }
                if spec.vtpm {
                    m.push("vtpm.create");
                }
                if spec.start {
                    m.push("vm.start");
                }
                m.push("task.destroy");
                return m;
            }
            Self::CopyVm { mode, .. } => match mode {
                CopyMode::Clone => &["vm.clone", "vm.set_name_description"],
                CopyMode::Copy(_) => &["vm.copy", "vm.set_name_description"],
            },
            Self::TemplateFromSnapshot { .. } => &[
                "vm.clone",
                "vm.set_name_description",
                "vm.add_to_other_config",
                "vm.remove_from_other_config",
            ],
            Self::MakeTemplate { .. } => &[
                "vm.add_to_other_config",
                "vm.remove_from_other_config",
                "vm.set_is_a_template",
            ],
            Self::DeleteVm { .. } => &["vm.destroy", "vm.hard_shutdown", "vdi.destroy"],
            Self::Snapshot { memory, .. } => {
                if *memory {
                    &["vm.checkpoint", "vm.set_name_description"]
                } else {
                    &["vm.snapshot", "vm.set_name_description"]
                }
            }
            Self::Revert { snapshot_first, .. } => {
                let mut m = vec![
                    "vm.revert",
                    "vm.start",
                    "vm.start_on",
                    "vm.resume",
                    "vm.resume_on",
                    "vm.assert_can_boot_here",
                ];
                if snapshot_first.is_some() {
                    m.extend(["vm.snapshot", "vm.set_name_description"]);
                }
                m.push("task.destroy");
                return m;
            }
            Self::DeleteSnapshots { .. } => &["vm.destroy", "vm.hard_shutdown", "vdi.destroy"],
            Self::NewDisk { .. } => &["vdi.create", "vbd.create", "vbd.plug"],
            Self::AttachDisk { .. } => &["vbd.create", "vbd.plug"],
            Self::DetachDisk { .. } => &["vbd.unplug", "vbd.destroy"],
            Self::DeleteDisk { .. } => &["vbd.unplug", "vbd.destroy", "vdi.destroy"],
            Self::ResizeDisk { .. } => &["vdi.resize"],
            Self::MoveDisk { .. } => &[
                "vdi.pool_migrate",
                "vdi.copy",
                "vdi.destroy",
                "vbd.create",
                "vbd.destroy",
                "vbd.unplug",
            ],
            Self::NewSr(spec) => {
                let mut m = match spec.mode() {
                    NewSrMode::Create => vec![
                        "sr.create",
                        "sr.set_other_config",
                        "sr.forget",
                        "pbd.plug",
                        "pbd.unplug",
                    ],
                    NewSrMode::Introduce => {
                        vec!["sr.forget", "sr.introduce", "pbd.create", "pbd.plug"]
                    }
                    NewSrMode::Reattach => vec![
                        "sr.set_name_label",
                        "sr.set_name_description",
                        "pbd.create",
                        "pbd.plug",
                        "pbd.unplug",
                        "pbd.destroy",
                    ],
                };
                if spec.mode() == NewSrMode::Create
                    && spec
                        .device_config
                        .iter()
                        .any(|(k, _)| crate::storage::is_secret_key(k))
                {
                    m.extend(["secret.create", "secret.destroy"]);
                }
                if spec.mode() != NewSrMode::Reattach && spec.kind.shared() && !spec.kind.is_iso() {
                    // The pool's first shared SR becomes its default.
                    m.extend([
                        "pool.set_default_sr",
                        "pool.set_suspend_image_sr",
                        "pool.set_crash_dump_sr",
                    ]);
                }
                m.push("task.destroy");
                return m;
            }
            Self::Sr { action, .. } => match action {
                SrAction::Repair => &["pbd.create", "pbd.plug"],
                SrAction::Detach => &["pbd.unplug"],
                SrAction::Forget => &["sr.forget"],
                SrAction::Destroy => &["sr.destroy"],
                SrAction::SetDefault => &[
                    "pool.set_default_sr",
                    "pool.set_suspend_image_sr",
                    "pool.set_crash_dump_sr",
                ],
                SrAction::Rescan => &["sr.scan"],
                SrAction::Trim => &["host.call_plugin"],
            },
            Self::SetSrProperties { changes, .. } => {
                let mut m = Vec::new();
                if changes.name.is_some() {
                    m.push("sr.set_name_label");
                }
                if changes.description.is_some() {
                    m.push("sr.set_name_description");
                }
                if changes.tags.is_some() {
                    m.push("sr.set_tags");
                }
                if changes.read_caching.is_some() {
                    m.extend([
                        "sr.remove_from_other_config/key:o_direct",
                        "sr.add_to_other_config/key:o_direct",
                    ]);
                }
                return m;
            }
            Self::NewNetwork(_)
            | Self::NewBond(_)
            | Self::RemoveNetwork { .. }
            | Self::DestroyBond { .. }
            | Self::SetNetworkProperties { .. }
            | Self::RescanPifs { .. }
            | Self::ChangeNetworking(_)
            | Self::RenameInterfaces { .. }
            | Self::NewVif { .. }
            | Self::DeleteVif { .. }
            | Self::UpdateVif { .. }
            | Self::PlugVif { .. }
            | Self::UnplugVif { .. } => &[],
            Self::DestroyPool { .. }
            | Self::EjectHost { .. }
            | Self::EvacuateCoordinator { .. }
            | Self::SetProperties { .. }
            | Self::SetDom0Memory { .. }
            | Self::ChangePassword { .. }
            | Self::RotatePoolSecret
            | Self::EnableTlsVerification
            | Self::InstallCertificate { .. }
            | Self::ResetCertificate { .. }
            | Self::BackupHost { .. }
            | Self::RestoreHost { .. } => &[],
            Self::EnableHa { .. }
            | Self::DisableHa
            | Self::SetHaPriorities { .. }
            | Self::SetHaNtol { .. }
            | Self::HaHost { .. }
            | Self::HaVm { .. }
            | Self::EnableAd { .. }
            | Self::DisableAd { .. }
            | Self::AddSubjects { .. }
            | Self::RemoveSubjects { .. }
            | Self::SetSubjectRoles { .. }
            | Self::LogoutSubjects { .. } => &[],
            Self::ChangeCd { .. } | Self::InstallTools { .. } => &["vbd.eject", "vbd.insert"],
            Self::NewCdDrive { .. } => &["vbd.create", "vbd.plug"],
            Self::Vm { action, .. } => match action {
                VmAction::Start => &["vm.start"],
                VmAction::CleanShutdown => &["vm.clean_shutdown"],
                VmAction::CleanReboot => &["vm.clean_reboot"],
                VmAction::Suspend => &["vm.suspend"],
                VmAction::Resume => &["vm.resume"],
                VmAction::Pause => &["vm.pause"],
                VmAction::Unpause => &["vm.unpause"],
                VmAction::HardShutdown => &["vm.hard_shutdown"],
                VmAction::HardReboot => &["vm.hard_reboot"],
                VmAction::StartOn(_) => &["vm.start_on"],
                VmAction::ResumeOn(_) => &["vm.resume_on"],
                VmAction::Migrate(_) => &["vm.pool_migrate"],
            },
            Self::Host { action, .. } => match action {
                HostAction::Reboot => &[
                    "host.disable",
                    "host.enable",
                    "vm.clean_shutdown",
                    "vm.hard_shutdown",
                    "host.reboot",
                ],
                HostAction::Shutdown => &[
                    "host.disable",
                    "host.enable",
                    "vm.clean_shutdown",
                    "vm.hard_shutdown",
                    "host.shutdown",
                ],
                HostAction::PowerOn => &["host.power_on"],
                HostAction::EnterMaintenance => &[
                    "host.disable",
                    "host.enable",
                    "host.evacuate",
                    "host.add_to_other_config",
                    "host.remove_from_other_config",
                ],
                HostAction::ExitMaintenance => &["host.enable", "host.remove_from_other_config"],
                HostAction::RestartToolstack => &["host.restart_agent"],
            },
        };
        own.iter().chain(TASK).copied().collect()
    }
}

// ---- when an operation is offered ------------------------------------------

fn allowed(vm: &VmRecord, op: VmOperations) -> bool {
    vm.allowed_operations.contains(&op)
}

/// `Helpers.EnabledTargetExists`: the VM's host is enabled, or (when it has
/// none) some host of the pool is.
fn enabled_target_exists(model: &ConnectionModel, host: Option<&Ref<Host>>) -> bool {
    match host.and_then(|h| model.hosts.get(h)) {
        Some(h) => h.enabled,
        None => model.hosts.values().any(|h| h.enabled),
    }
}

/// `VM.GetStorageHost`: the host whose local storage holds one of the VM's
/// disks, if any (a halted VM can only start there).
pub fn storage_host<'a>(model: &'a ConnectionModel, vm: &VmRecord) -> Option<&'a Ref<Host>> {
    vm.vbds
        .iter()
        .filter_map(|r| model.get(r))
        .filter(|vbd| vbd.r#type != xapi::VbdType::Cd)
        .filter_map(|vbd| model.get(&vbd.vdi).and_then(|vdi| model.get(&vdi.sr)))
        .find(|sr| objects::is_local_sr(sr))
        .and_then(|sr| objects::sr_home(model, sr))
}

/// Whether `action` is offered for `vm`. `busy` means this client has an
/// operation running on it: everything but the forced operations waits
/// (`VM.Locked` in the original); the forced ones are then offered even
/// when the server does not list them (CA-16960 in the original).
pub fn vm_action_available(
    model: &ConnectionModel,
    vm: &VmRecord,
    action: &VmAction,
    busy: bool,
) -> bool {
    if !is_guest(vm) {
        return false;
    }
    let home = objects::vm_home(model, vm);
    let running = vm.power_state == VmPowerState::Running;
    match action {
        VmAction::HardShutdown => (running && busy) || allowed(vm, VmOperations::HardShutdown),
        VmAction::HardReboot => {
            (running && busy)
                || (allowed(vm, VmOperations::HardReboot) && enabled_target_exists(model, home))
        }
        _ if busy => false,
        VmAction::Start => {
            vm.power_state == VmPowerState::Halted
                && allowed(vm, VmOperations::Start)
                && enabled_target_exists(model, storage_host(model, vm))
        }
        VmAction::CleanShutdown => allowed(vm, VmOperations::CleanShutdown),
        VmAction::CleanReboot => {
            allowed(vm, VmOperations::CleanReboot) && enabled_target_exists(model, home)
        }
        VmAction::Suspend => running && allowed(vm, VmOperations::Suspend),
        VmAction::Resume => {
            vm.power_state == VmPowerState::Suspended
                && allowed(vm, VmOperations::Resume)
                && enabled_target_exists(model, home)
        }
        VmAction::Pause => allowed(vm, VmOperations::Pause),
        VmAction::Unpause => allowed(vm, VmOperations::Unpause),
        VmAction::StartOn(h) | VmAction::ResumeOn(h) | VmAction::Migrate(h) => {
            let menu = action.host_menu().expect("host menu action");
            host_menu_available(model, vm, menu, busy)
                && model
                    .hosts
                    .get(h)
                    .is_some_and(|host| target_host_problem(model, vm, h, host, menu).is_none())
        }
    }
}

/// Whether the "Start on Server", "Resume on Server" or "Migrate to Server"
/// submenu is offered for `vm`.
///
/// The original offers Start/Resume on Server only in pools of two or more
/// servers; here one server is enough (choosing it is harmless and lets a
/// single-host pool start a VM on a named server).
pub fn host_menu_available(
    model: &ConnectionModel,
    vm: &VmRecord,
    menu: HostMenu,
    busy: bool,
) -> bool {
    if !is_guest(vm) || busy || model.hosts.is_empty() {
        return false;
    }
    let home = objects::vm_home(model, vm);
    match menu {
        HostMenu::StartOn => {
            allowed(vm, VmOperations::Start)
                && enabled_target_exists(model, storage_host(model, vm))
        }
        HostMenu::ResumeOn => {
            !allowed(vm, VmOperations::Suspend)
                && allowed(vm, VmOperations::Resume)
                && enabled_target_exists(model, home)
        }
        HostMenu::Migrate => allowed(vm, VmOperations::PoolMigrate),
    }
}

/// Why `host` cannot take `vm` for `menu`, known without asking the server
/// (`VMOperationHostCommand.VmCanBootOnHost` before its
/// `VM.assert_can_boot_here` call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostProblem {
    /// The VM already runs there.
    CurrentServer,
    /// The host is not live.
    Offline,
    /// The host is disabled (maintenance mode).
    Disabled,
    /// The host runs an older product version than the VM's host.
    OlderThanCurrent,
    /// The VM's CPU vendor differs from the host's.
    IncompatibleCpu,
}

/// The local reason why `host` cannot take `vm`, if any.
pub fn target_host_problem(
    model: &ConnectionModel,
    vm: &VmRecord,
    host_ref: &Ref<Host>,
    host: &HostRecord,
    menu: HostMenu,
) -> Option<HostProblem> {
    if vm.power_state == VmPowerState::Running
        && let Some(resident) = model.hosts.get(&vm.resident_on)
    {
        if &vm.resident_on == host_ref {
            return Some(HostProblem::CurrentServer);
        }
        if version_cmp(product_version(host), product_version(resident)).is_lt() {
            return Some(HostProblem::OlderThanCurrent);
        }
    }
    if !objects::host_is_live(model, host) {
        return Some(HostProblem::Offline);
    }
    if !host.enabled {
        return Some(HostProblem::Disabled);
    }
    if matches!(menu, HostMenu::Migrate | HostMenu::ResumeOn)
        && matches!(
            vm.power_state,
            VmPowerState::Running | VmPowerState::Suspended
        )
        && let (Some(a), Some(b)) = (
            vm.last_boot_cpu_flags.get("vendor"),
            host.cpu_info.get("vendor"),
        )
        && a != b
    {
        return Some(HostProblem::IncompatibleCpu);
    }
    None
}

fn product_version(host: &HostRecord) -> &str {
    host.software_version
        .get("product_version")
        .map(String::as_str)
        .unwrap_or_default()
}

/// Compares dotted versions numerically (`8.10` > `8.9`); missing parts
/// compare as equal so an unknown version never blocks.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    if a.is_empty() || b.is_empty() {
        return std::cmp::Ordering::Equal;
    }
    let parts = |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a).cmp(&parts(b))
}

/// Whether `action` is offered for `host`. `busy` means this client has an
/// operation running on it.
pub fn host_action_available(
    model: &ConnectionModel,
    host: &HostRecord,
    action: HostAction,
    busy: bool,
) -> bool {
    let live = objects::host_is_live(model, host);
    match action {
        HostAction::Reboot | HostAction::RestartToolstack => live,
        HostAction::Shutdown => live && !busy,
        HostAction::PowerOn => {
            !live
                && host
                    .allowed_operations
                    .contains(&HostAllowedOperations::PowerOn)
                && !host.power_on_mode.is_empty()
                && !busy
        }
        HostAction::EnterMaintenance => host.enabled && model.coordinator().is_some(),
        HostAction::ExitMaintenance => !host.enabled && model.coordinator().is_some(),
    }
}

/// `VM.HAIsProtected`: HA is on in the pool and the VM has a restart
/// priority (any but "do not restart", which is the empty string).
pub fn vm_ha_protected(model: &ConnectionModel, vm: &VmRecord) -> bool {
    model.pool().is_some_and(|(_, p)| p.ha_enabled) && !vm.ha_restart_priority.is_empty()
}

/// The running guests on `host` (what Reboot and Shut Down stop first and
/// what maintenance mode migrates away).
pub fn running_guests<'a>(
    model: &'a ConnectionModel,
    host: &'a HostRecord,
) -> Vec<(&'a Ref<Vm>, &'a VmRecord)> {
    host.resident_vms
        .iter()
        .filter_map(|r| model.vms.get(r).map(|v| (r, &**v)))
        .filter(|(_, v)| !v.is_control_domain && v.power_state == VmPowerState::Running)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ConnectionModel {
        ConnectionModel::from_database(
            &serde_json::from_str(include_str!("../../../tests/fixtures/records.json")).unwrap(),
        )
    }

    fn vm(m: &ConnectionModel, r: &str) -> VmRecord {
        (*m.vms[&Ref::new(r)]).clone()
    }

    #[test]
    fn vm_actions_follow_allowed_operations_and_power_state() {
        let m = model();
        let web = vm(&m, "OpaqueRef:vm-web"); // running
        let db = vm(&m, "OpaqueRef:vm-db"); // halted
        let build = vm(&m, "OpaqueRef:vm-build"); // halted, no local disk
        let legacy = vm(&m, "OpaqueRef:vm-legacy"); // suspended
        for a in [
            VmAction::CleanShutdown,
            VmAction::CleanReboot,
            VmAction::Suspend,
            VmAction::Pause,
            VmAction::HardShutdown,
            VmAction::HardReboot,
        ] {
            assert!(vm_action_available(&m, &web, &a, false), "{a:?}");
            assert!(!vm_action_available(&m, &db, &a, false), "{a:?}");
        }
        assert!(vm_action_available(&m, &build, &VmAction::Start, false));
        assert!(!vm_action_available(&m, &web, &VmAction::Start, false));
        // db-01's home server (host 2) is in maintenance mode, but its
        // disks are on shared storage: it can start elsewhere.
        assert!(vm_action_available(&m, &db, &VmAction::Start, false));
        assert!(vm_action_available(&m, &legacy, &VmAction::Resume, false));
        assert!(!vm_action_available(&m, &legacy, &VmAction::Suspend, false));
        assert!(!vm_action_available(&m, &web, &VmAction::Unpause, false));

        // Busy: only the forced operations stay.
        assert!(!vm_action_available(&m, &web, &VmAction::CleanReboot, true));
        assert!(vm_action_available(&m, &web, &VmAction::HardShutdown, true));
        let mut stuck = web.clone();
        stuck.allowed_operations.clear();
        assert!(vm_action_available(&m, &stuck, &VmAction::HardReboot, true));
        assert!(!vm_action_available(
            &m,
            &stuck,
            &VmAction::HardReboot,
            false
        ));

        // Never on templates, snapshots or control domains.
        for r in ["OpaqueRef:vm-template-debian", "OpaqueRef:vm-dom0-01"] {
            assert!(!vm_action_available(
                &m,
                &vm(&m, r),
                &VmAction::HardShutdown,
                false
            ));
        }
    }

    #[test]
    fn host_menus_and_targets() {
        let m = model();
        let web = vm(&m, "OpaqueRef:vm-web");
        let db = vm(&m, "OpaqueRef:vm-build");
        let (h1, h2) = (
            Ref::new("OpaqueRef:host-0001"),
            Ref::new("OpaqueRef:host-0002"),
        );
        assert!(host_menu_available(&m, &web, HostMenu::Migrate, false));
        assert!(!host_menu_available(&m, &web, HostMenu::StartOn, false));
        assert!(host_menu_available(&m, &db, HostMenu::StartOn, false));
        assert!(!host_menu_available(&m, &db, HostMenu::StartOn, true));
        // web-01 runs on host 1; host 2 is in maintenance.
        assert_eq!(
            target_host_problem(&m, &web, &h1, &m.hosts[&h1], HostMenu::Migrate),
            Some(HostProblem::CurrentServer)
        );
        assert_eq!(
            target_host_problem(&m, &web, &h2, &m.hosts[&h2], HostMenu::Migrate),
            Some(HostProblem::Disabled)
        );
        assert!(vm_action_available(
            &m,
            &db,
            &VmAction::StartOn(h1.clone()),
            false
        ));
        assert!(!vm_action_available(&m, &db, &VmAction::StartOn(h2), false));
    }

    #[test]
    fn host_actions() {
        let m = model();
        let h1 = &m.hosts[&Ref::new("OpaqueRef:host-0001")];
        let h2 = &m.hosts[&Ref::new("OpaqueRef:host-0002")];
        assert!(host_action_available(&m, h1, HostAction::Reboot, false));
        assert!(host_action_available(
            &m,
            h1,
            HostAction::EnterMaintenance,
            false
        ));
        assert!(!host_action_available(
            &m,
            h1,
            HostAction::ExitMaintenance,
            false
        ));
        assert!(host_action_available(
            &m,
            h2,
            HostAction::ExitMaintenance,
            false
        ));
        assert!(!host_action_available(&m, h1, HostAction::PowerOn, false));
        assert!(!host_action_available(&m, h1, HostAction::Shutdown, true));
        assert_eq!(running_guests(&m, h1).len(), 1);
        assert!(version_cmp("8.10.0", "8.9.1").is_gt());
    }

    #[test]
    fn rbac_methods_include_the_task_cleanup() {
        let r = ActionRequest::Vm {
            vm: Ref::new("OpaqueRef:x"),
            action: VmAction::Migrate(Ref::new("OpaqueRef:h")),
        };
        assert_eq!(r.rbac_methods(), vec!["vm.pool_migrate", "task.destroy"]);
    }
}
