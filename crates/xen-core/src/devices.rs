//! F14a: GPUs and virtual GPUs, USB pass-through and vTPMs — the
//! original's rules over the cached model, and the requests that change
//! them (run by [`crate::devices_run`]).
//!
//! **GPU** (`GpuPage`, `GpuRow`, `GpuShinyBar`, `GpuConfiguration`,
//! `PoolGpuEditPage`, `GpuEditPage`, `AddVGPUDialog` and the
//! `VGPU_type` / `PGPU` / `GPU_group` / `VGPU` extensions):
//!
//! * a type is pass-through when its `model_name` is `"passthrough"`; its
//!   capacity is its first supporting PGPU's
//!   `supported_VGPU_max_capacities` entry; lists show the types
//!   pass-through first, then the fewest per GPU, then the highest
//!   resolution ([`cmp_vgpu_types`]);
//! * the GPU tab groups a pool's (or host's) physical GPUs with the same
//!   name, enabled and supported types ([`gpu_tab_groups`]); a GPU's bar
//!   has one slot per vGPU its resident type allows ([`gpu_bar`]);
//! * the allowed types of selected GPUs are set with
//!   `PGPU.set_enabled_VGPU_types` (a type in use cannot be disallowed);
//!   the placement policy with `GPU_group.set_allocation_algorithm` on
//!   every group; integrated GPU pass-through with
//!   `host.async_enable_display` / `disable_display` then
//!   `PGPU.async_enable_dom0_access` / `disable_dom0_access` of the
//!   server's system display device;
//! * a VM's vGPUs change only while it is halted and not protected by HA
//!   with "restart" ([`device_block`]); saving destroys the removed
//!   ones and creates the new ones with `VGPU.async_create(vm, group, "0",
//!   {}, type)` (no type when vGPU is not licensed).
//!
//! **USB** (`UsbPage`, `USBEditPage`, `AttachUsbDialog` and the USB
//! actions): a device is in use while its USB group has a VUSB; the host
//! tab toggles `PUSB.set_passthrough_enabled`; a halted VM not protected
//! by HA gets devices that are pass-through enabled and unused on the
//! servers `VM.get_possible_hosts` names: `VM.set_platform` with
//! `device-model = qemu-upstream-compat` first if it does not use it,
//! then `VUSB.create(vm, group, {})`; detaching unplugs first when the
//! VUSB is attached and may be unplugged, then destroys it.
//!
//! **vTPM** (`VtpmCommand`, `VtpmManagementDialog`, `VtpmAction`): a real
//! HVM VM booting with UEFI on XAPI 22.26 or later, when no server's
//! licence restricts vTPM; adding (`VTPM.create(vm, false)`) and removing
//! (`VTPM.destroy`) follow the VM's and the vTPM's allowed operations on
//! XAPI 23.11 or later, else one vTPM at most and a halted VM.
//!
//! Every licence check is the original's `BoolKeyPreferTrue`: a feature is
//! restricted unless the host says `"false"` for it, and forbidden when
//! any host of the connection restricts it.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use xapi::{
    AllocationAlgorithm, GpuGroup, GpuGroupRecord, Host, HostDisplay, HostRecord, PciDom0Access,
    Pgpu, PgpuRecord, Pusb, PusbRecord, Ref, Vgpu, VgpuType, VgpuTypeRecord, Vm, VmOperations,
    VmPowerState, VmRecord, Vtpm, VtpmOperations, Vusb,
};

use crate::model::ConnectionModel;
use crate::objects;
use crate::provisioning::recommendation;

// ---- licences and versions ------------------------------------------------------------------

/// A licence restriction of this phase (`Host.Restrict*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRestriction {
    /// `restrict_gpu`: GPU pass-through.
    Gpu,
    /// `restrict_vgpu`: virtual GPUs.
    Vgpu,
    /// `restrict_integrated_gpu_passthrough`.
    IntegratedGpu,
    /// `restrict_usb_passthrough`.
    Usb,
    /// `restrict_vtpm`.
    Vtpm,
    /// `restrict_dmc`: Dynamic Memory Control.
    Dmc,
}

impl DeviceRestriction {
    fn key(self) -> &'static str {
        match self {
            Self::Gpu => "restrict_gpu",
            Self::Vgpu => "restrict_vgpu",
            Self::IntegratedGpu => "restrict_integrated_gpu_passthrough",
            Self::Usb => "restrict_usb_passthrough",
            Self::Vtpm => "restrict_vtpm",
            Self::Dmc => "restrict_dmc",
        }
    }
}

/// Whether `host`'s licence restricts `what` (`BoolKeyPreferTrue`: unless
/// the key says "false").
pub fn host_restricts(host: &HostRecord, what: DeviceRestriction) -> bool {
    host.license_params
        .get(what.key())
        .is_none_or(|v| !v.trim().eq_ignore_ascii_case("false"))
}

/// `Helpers.FeatureForbidden`: some host of the connection restricts it.
pub fn forbidden(model: &ConnectionModel, what: DeviceRestriction) -> bool {
    model.hosts.values().any(|h| host_restricts(h, what))
}

/// `Helpers.XapiEqualOrGreater_22_26_0` (vTPM support).
pub fn xapi_22_26_or_greater(model: &ConnectionModel) -> bool {
    xapi_at_least(model, "22.26.0")
}

/// `Helpers.XapiEqualOrGreater_23_11_0` (vTPM allowed operations).
pub fn xapi_23_11_or_greater(model: &ConnectionModel) -> bool {
    xapi_at_least(model, "23.11.0")
}

fn xapi_at_least(model: &ConnectionModel, version: &str) -> bool {
    model.coordinator().is_none_or(|c| {
        crate::pool::xapi_version(c)
            .is_none_or(|v| crate::pool::version_cmp(Some(v), Some(version)) != Ordering::Less)
    })
}

/// The HA restart priority that keeps a VM from changing its devices.
pub const HA_RESTART: &str = "restart";

/// Whether the pool runs HA and `priority` (the VM's, or the one picked
/// on the HA page and not saved yet) is "restart"
/// (`pool.ha_enabled && VM.HaPriorityIsRestart`).
pub fn ha_protected(model: &ConnectionModel, priority: &str) -> bool {
    crate::pool::visible_pool(model).is_some_and(|(_, p)| p.ha_enabled) && priority == HA_RESTART
}

/// A restriction of a VM's template (`VM.GetIntRestrictionValue`): a
/// template reads its own `recommendations`, another VM those of the
/// template with the same `reference_label`.
pub fn vm_restriction(model: &ConnectionModel, vm: &VmRecord, field: &str) -> Option<String> {
    if vm.is_a_template {
        return recommendation(vm, field);
    }
    if vm.reference_label.is_empty() {
        return None;
    }
    model
        .vms
        .values()
        .find(|t| t.is_a_template && t.reference_label == vm.reference_label)
        .and_then(|t| recommendation(t, field))
}

fn int_restriction(model: &ConnectionModel, vm: &VmRecord, field: &str, default: i64) -> i64 {
    vm_restriction(model, vm, field)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

// ---- GPU: names and order ---------------------------------------------------------------------

/// The model name of a pass-through type.
pub const PASSTHROUGH: &str = "passthrough";

/// `VGPU_type.IsPassthrough`.
pub fn is_passthrough(t: &VgpuTypeRecord) -> bool {
    t.model_name == PASSTHROUGH
}

/// `PGPU.Name`: its PCI device's name, else its UUID.
pub fn pgpu_name(model: &ConnectionModel, pgpu: &PgpuRecord) -> String {
    model
        .get(&pgpu.pci)
        .map(|p| p.device_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| pgpu.uuid.clone())
}

/// `VGPU_type.Capacity`: vGPUs per GPU, from its first supporting PGPU.
pub fn vgpu_type_capacity(model: &ConnectionModel, r: &Ref<VgpuType>, t: &VgpuTypeRecord) -> i64 {
    t.supported_on_pgpus
        .first()
        .and_then(|p| model.get(p))
        .and_then(|p| p.supported_vgpu_max_capacities.get(r).copied())
        .unwrap_or(0)
}

/// How a type is named (`VGPU_type.Name` / `Description`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VgpuTypeName {
    /// "Pass-through whole GPU".
    Passthrough,
    /// "{model} virtual GPU ({per} per GPU)", with ", multiple vGPU
    /// support" in the description when the type has compatible types.
    Virtual {
        model: String,
        per_gpu: i64,
        multiple: bool,
    },
}

/// The name of type `r`.
pub fn vgpu_type_name(model: &ConnectionModel, r: &Ref<VgpuType>) -> Option<VgpuTypeName> {
    let t = model.get(r)?;
    Some(if is_passthrough(t) {
        VgpuTypeName::Passthrough
    } else {
        VgpuTypeName::Virtual {
            model: t.model_name.clone(),
            per_gpu: vgpu_type_capacity(model, r, t),
            multiple: !t.compatible_types_in_vm.is_empty(),
        }
    })
}

/// `VGPU_type.CompareTo`: ascending means "smaller first" — pass-through
/// is the biggest, then the fewer per GPU the bigger, then the higher
/// resolution; ties by name and reference.
pub fn cmp_vgpu_types(model: &ConnectionModel, a: &Ref<VgpuType>, b: &Ref<VgpuType>) -> Ordering {
    let (Some(ta), Some(tb)) = (model.get(a), model.get(b)) else {
        return a.cmp(b);
    };
    match (is_passthrough(ta), is_passthrough(tb)) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    let (ca, cb) = (
        vgpu_type_capacity(model, a, ta),
        vgpu_type_capacity(model, b, tb),
    );
    if ca != cb {
        return cb.cmp(&ca);
    }
    let res = |t: &VgpuTypeRecord| t.max_resolution_x * t.max_resolution_y;
    res(ta)
        .cmp(&res(tb))
        .then_with(|| objects::natural_cmp(&ta.model_name, &tb.model_name))
        .then_with(|| a.cmp(b))
}

/// `refs` resolved and sorted as the original lists them (descending:
/// pass-through first).
pub fn sorted_types_desc(model: &ConnectionModel, refs: &[Ref<VgpuType>]) -> Vec<Ref<VgpuType>> {
    let mut out: Vec<Ref<VgpuType>> = refs
        .iter()
        .filter(|r| model.vgpu_types.contains_key(*r))
        .cloned()
        .collect();
    out.sort_by(|a, b| cmp_vgpu_types(model, b, a));
    out.dedup();
    out
}

/// `GPU_group.Name`: its label without a leading "Group of ".
pub fn gpu_group_name(g: &GpuGroupRecord) -> String {
    g.name_label
        .strip_prefix("Group of ")
        .unwrap_or(&g.name_label)
        .to_owned()
}

/// `PGPU.HasVGpu`: it supports a type that is not pass-through.
pub fn pgpu_has_vgpu(model: &ConnectionModel, p: &PgpuRecord) -> bool {
    p.supported_vgpu_types
        .iter()
        .filter_map(|t| model.get(t))
        .any(|t| !is_passthrough(t))
}

/// `GPU_group.HasVGpu`.
pub fn group_has_vgpu(model: &ConnectionModel, g: &GpuGroupRecord) -> bool {
    g.pgpus
        .iter()
        .filter_map(|p| model.get(p))
        .any(|p| pgpu_has_vgpu(model, p))
}

/// `GPU_group.HasPassthrough`.
pub fn group_has_passthrough(model: &ConnectionModel, g: &GpuGroupRecord) -> bool {
    g.supported_vgpu_types
        .iter()
        .filter_map(|t| model.get(t))
        .any(|t| is_passthrough(t))
}

/// The pass-through type a group supports, if any.
pub fn group_passthrough_type(
    model: &ConnectionModel,
    g: &GpuGroupRecord,
) -> Option<Ref<VgpuType>> {
    g.supported_vgpu_types
        .iter()
        .find(|t| model.get(*t).is_some_and(|t| is_passthrough(t)))
        .cloned()
}

/// `Helpers.GpuCapability`: GPUs licensed and present.
pub fn gpu_capability(model: &ConnectionModel) -> bool {
    !forbidden(model, DeviceRestriction::Gpu) && model.pool().is_some() && !model.pgpus.is_empty()
}

/// `Helpers.VGpuCapability`: vGPUs licensed and some GPU supports one.
pub fn vgpu_capability(model: &ConnectionModel) -> bool {
    !forbidden(model, DeviceRestriction::Vgpu)
        && model.pool().is_some()
        && model.pgpus.values().any(|p| pgpu_has_vgpu(model, p))
}

/// `Helpers.GpusAvailable`: a GPU group with GPUs and supported types.
pub fn gpus_available(model: &ConnectionModel) -> bool {
    model
        .gpu_groups
        .values()
        .any(|g| !g.pgpus.is_empty() && !g.supported_vgpu_types.is_empty())
}

/// `VM.CanHaveGpu`: HVM, and its template allows GPU pass-through.
pub fn vm_can_have_gpu(model: &ConnectionModel, vm: &VmRecord) -> bool {
    objects::vm_is_hvm(vm) && int_restriction(model, vm, "allow-gpu-passthrough", 1) != 0
}

/// `VM.CanHaveVGpu`.
pub fn vm_can_have_vgpu(model: &ConnectionModel, vm: &VmRecord) -> bool {
    vm_can_have_gpu(model, vm) && int_restriction(model, vm, "allow-vgpu", 1) != 0
}

/// `VGPU.IsPassthrough`: its type is pass-through or unknown.
pub fn vgpu_is_passthrough(model: &ConnectionModel, v: &Ref<Vgpu>) -> bool {
    model
        .get(v)
        .is_none_or(|v| model.get(&v.r#type).is_none_or(|t| is_passthrough(t)))
}

/// `VM.HasGPUPassthrough`.
pub fn vm_has_gpu_passthrough(model: &ConnectionModel, vm: &VmRecord) -> bool {
    vm.vgpus
        .iter()
        .filter(|v| model.vgpus.contains_key(*v))
        .any(|v| vgpu_is_passthrough(model, v))
}

// ---- GPU tab ----------------------------------------------------------------------------------

/// Whether the GPU tab shows for a pool or live host (licence only: a
/// pool without GPUs shows the "no GPUs" text).
pub fn gpu_tab_shown(model: &ConnectionModel) -> bool {
    !forbidden(model, DeviceRestriction::Gpu)
}

/// The GPU tab's rows: physical GPUs (with supported types) of the pool
/// or of `host`, ordered by server then name, grouped by identical name,
/// enabled and supported types (`GpuSettings`), first seen first.
pub fn gpu_tab_groups(model: &ConnectionModel, host: Option<&Ref<Host>>) -> Vec<Vec<Ref<Pgpu>>> {
    let host_rank: Vec<&Ref<Host>> = objects::sorted_hosts(model)
        .into_iter()
        .map(|(r, _)| r)
        .collect();
    let rank = |h: &Ref<Host>| host_rank.iter().position(|x| *x == h).unwrap_or(usize::MAX);
    let mut pgpus: Vec<(&Ref<Pgpu>, &PgpuRecord, String)> = model
        .pgpus
        .iter()
        .filter(|(_, p)| !p.supported_vgpu_types.is_empty())
        .filter(|(_, p)| host.is_none_or(|h| &p.host == h))
        .map(|(r, p)| (r, &**p, pgpu_name(model, p)))
        .collect();
    pgpus.sort_by(|a, b| {
        rank(&a.1.host)
            .cmp(&rank(&b.1.host))
            .then_with(|| objects::natural_cmp(&a.2, &b.2))
            .then_with(|| a.0.cmp(b.0))
    });
    type Key = (String, Vec<Ref<VgpuType>>, Vec<Ref<VgpuType>>);
    let mut groups: Vec<(Key, Vec<Ref<Pgpu>>)> = Vec::new();
    for (r, p, name) in pgpus {
        let mut enabled = p.enabled_vgpu_types.clone();
        enabled.sort();
        let mut supported = p.supported_vgpu_types.clone();
        supported.sort();
        let key = (name, enabled, supported);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(r.clone()),
            None => groups.push((key, vec![r.clone()])),
        }
    }
    groups.into_iter().map(|(_, l)| l).collect()
}

/// The GPU placement policy of the pool (`GpuPlacementPolicyPanel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementPolicy {
    /// `depth_first`: "Maximum density".
    Density,
    /// `breadth_first`: "Maximum performance".
    Performance,
    /// Groups differ (or none): "Mixed".
    Mixed,
}

impl PlacementPolicy {
    /// The algorithm to set, `None` for Mixed.
    pub fn algorithm(self) -> Option<AllocationAlgorithm> {
        match self {
            Self::Density => Some(AllocationAlgorithm::DepthFirst),
            Self::Performance => Some(AllocationAlgorithm::BreadthFirst),
            Self::Mixed => None,
        }
    }
}

/// Every GPU group's algorithm, if they all agree.
pub fn placement_policy(model: &ConnectionModel) -> PlacementPolicy {
    let mut algs = model.gpu_groups.values().map(|g| &g.allocation_algorithm);
    let Some(first) = algs.next() else {
        return PlacementPolicy::Mixed;
    };
    if algs.any(|a| a != first) {
        return PlacementPolicy::Mixed;
    }
    match first {
        AllocationAlgorithm::DepthFirst => PlacementPolicy::Density,
        AllocationAlgorithm::BreadthFirst => PlacementPolicy::Performance,
        AllocationAlgorithm::Unknown(_) => PlacementPolicy::Mixed,
    }
}

/// One GPU's bar (`GpuShinyBar`): `capacity` slots, one per resident
/// vGPU (VM name, type model name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuBar {
    pub capacity: i64,
    /// Above 1, the bar shows slot ticks.
    pub max_capacity: i64,
    pub segments: Vec<(String, String)>,
}

/// The bar of `pgpu`.
pub fn gpu_bar(model: &ConnectionModel, pgpu: &PgpuRecord) -> GpuBar {
    let mut vgpus: Vec<(&Ref<Vgpu>, &xapi::VgpuRecord)> = pgpu
        .resident_vgpus
        .iter()
        .filter_map(|r| model.vgpus.get_key_value(r).map(|(r, v)| (r, &**v)))
        .collect();
    vgpus.sort_by(|a, b| a.0.cmp(b.0));
    let mut max_capacity = 1;
    if !forbidden(model, DeviceRestriction::Vgpu) && pgpu_has_vgpu(model, pgpu) {
        max_capacity = pgpu
            .supported_vgpu_max_capacities
            .values()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
    }
    let capacity = vgpus
        .first()
        .and_then(|(_, v)| pgpu.supported_vgpu_max_capacities.get(&v.r#type))
        .copied()
        .unwrap_or(max_capacity)
        .max(1);
    let segments = vgpus
        .iter()
        .filter_map(|(_, v)| {
            let vm = model.get(&v.vm)?;
            let model_name = model
                .get(&v.r#type)
                .map(|t| t.model_name.clone())
                .unwrap_or_default();
            Some((vm.name_label.clone(), model_name))
        })
        .collect();
    GpuBar {
        capacity,
        max_capacity,
        segments,
    }
}

/// Whether a type runs on any of `pgpus` (`GpuConfiguration`'s
/// `isInUse`): an allowed type in use cannot be disallowed.
pub fn type_in_use(model: &ConnectionModel, pgpus: &[Ref<Pgpu>], t: &Ref<VgpuType>) -> bool {
    pgpus
        .iter()
        .filter_map(|p| model.get(p))
        .flat_map(|p| &p.resident_vgpus)
        .filter_map(|v| model.get(v))
        .any(|v| &v.r#type == t)
}

/// The new allowed lists of `pgpus` after `changes` (type, now allowed),
/// for the types whose box changed (`GpuConfiguration.OK`): each GPU's
/// list with the disallowed types removed and the allowed ones added
/// (once — the original could add a type twice).
pub fn allowed_types_plan(
    model: &ConnectionModel,
    pgpus: &[Ref<Pgpu>],
    changes: &[(Ref<VgpuType>, bool)],
) -> Vec<(Ref<Pgpu>, Vec<Ref<VgpuType>>)> {
    if changes.is_empty() {
        return Vec::new();
    }
    pgpus
        .iter()
        .filter_map(|r| model.get(r).map(|p| (r, p)))
        .map(|(r, p)| {
            let mut list = p.enabled_vgpu_types.clone();
            for (t, allowed) in changes {
                if *allowed {
                    if !list.contains(t) {
                        list.push(t.clone());
                    }
                } else {
                    list.retain(|x| x != t);
                }
            }
            (r.clone(), list)
        })
        .collect()
}

/// A server's integrated GPU state (`PoolGpuEditPage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegratedGpu {
    /// The server uses its integrated GPU now.
    pub current: bool,
    /// It will after the next reboot.
    pub next: bool,
}

/// `host.SystemDisplayDevice`.
pub fn system_display_device<'a>(
    model: &'a ConnectionModel,
    host: &Ref<Host>,
) -> Option<(&'a Ref<Pgpu>, &'a PgpuRecord)> {
    let mut list: Vec<(&Ref<Pgpu>, &PgpuRecord)> = model
        .pgpus
        .iter()
        .filter(|(_, p)| &p.host == host && p.is_system_display_device)
        .map(|(r, p)| (r, &**p))
        .collect();
    list.sort_by(|a, b| a.0.cmp(b.0));
    list.into_iter().next()
}

/// The state of `host`'s integrated GPU.
pub fn integrated_gpu(model: &ConnectionModel, host: &Ref<Host>) -> IntegratedGpu {
    let d = model
        .get(host)
        .map(|h| h.display.clone())
        .unwrap_or(HostDisplay::Unknown(String::new()));
    let a = system_display_device(model, host)
        .map(|(_, p)| p.dom0_access.clone())
        .unwrap_or(PciDom0Access::Unknown(String::new()));
    let host_now = matches!(d, HostDisplay::Enabled | HostDisplay::DisableOnReboot);
    let host_next = matches!(d, HostDisplay::Enabled | HostDisplay::EnableOnReboot);
    let gpu_now = matches!(a, PciDom0Access::Enabled | PciDom0Access::DisableOnReboot);
    let gpu_next = matches!(a, PciDom0Access::Enabled | PciDom0Access::EnableOnReboot);
    IntegratedGpu {
        current: host_now && gpu_now,
        next: host_next && gpu_next,
    }
}

/// `Host.CanEnableDisableIntegratedGpu`.
pub fn can_change_integrated_gpu(model: &ConnectionModel) -> bool {
    gpu_capability(model) && !forbidden(model, DeviceRestriction::IntegratedGpu)
}

// ---- GPU: the VM's page -----------------------------------------------------------------------

/// Whether a VM's Properties dialog has the GPU page: it can have a GPU,
/// GPUs are available and the licence allows GPUs (the original only
/// asserts the last).
pub fn vm_gpu_page_shown(model: &ConnectionModel, vm: &VmRecord) -> bool {
    !vm.is_a_snapshot
        && vm_can_have_gpu(model, vm)
        && gpus_available(model)
        && !forbidden(model, DeviceRestriction::Gpu)
}

/// Why a VM's vGPUs (or USB devices) cannot change now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBlock {
    /// The VM is not shut down.
    NotHalted,
    /// The VM is protected by HA with "restart".
    Ha,
}

/// `GpuEditPage.ShowHideWarnings` / `USBEditPage.ShowHideWarnings`:
/// `priority` is the HA priority the HA page shows (saved or not).
pub fn device_block(model: &ConnectionModel, vm: &VmRecord, priority: &str) -> Option<DeviceBlock> {
    if vm.power_state != VmPowerState::Halted {
        Some(DeviceBlock::NotHalted)
    } else if ha_protected(model, priority) {
        Some(DeviceBlock::Ha)
    } else {
        None
    }
}

/// A vGPU listed on the VM's GPU page: an existing one, or a new one to
/// create on `group` with `vgpu_type` (none: the group's default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageVgpu {
    pub existing: Option<Ref<Vgpu>>,
    pub group: Ref<GpuGroup>,
    pub vgpu_type: Option<Ref<VgpuType>>,
    /// `device` of an existing vGPU.
    pub device: Option<String>,
}

impl PageVgpu {
    /// The VM's vGPUs, in its order.
    pub fn of_vm(model: &ConnectionModel, vm: &VmRecord) -> Vec<PageVgpu> {
        vm.vgpus
            .iter()
            .filter_map(|r| model.get(r).map(|v| (r, v)))
            .map(|(r, v)| PageVgpu {
                existing: Some(r.clone()),
                group: v.gpu_group.clone(),
                vgpu_type: Some(v.r#type.clone()),
                device: Some(v.device.clone()),
            })
            .collect()
    }

    /// The PCI slot column: `device + 11` (XAPI keeps 0–10), "-" for a new
    /// vGPU.
    pub fn pci_slot(&self) -> Option<String> {
        let d = self.device.as_deref()?;
        Some(match d.trim().parse::<i64>() {
            Ok(n) => (n + 11).to_string(),
            Err(_) => d.to_owned(),
        })
    }

    /// Whether its type is pass-through (or unknown).
    pub fn is_passthrough(&self, model: &ConnectionModel) -> bool {
        self.vgpu_type
            .as_ref()
            .and_then(|t| model.get(t))
            .is_none_or(|t| is_passthrough(t))
    }

    /// Whether its type is a Tech Preview.
    pub fn is_experimental(&self, model: &ConnectionModel) -> bool {
        self.vgpu_type
            .as_ref()
            .and_then(|t| model.get(t))
            .is_some_and(|t| t.experimental)
    }
}

/// How the page names a vGPU (`VGpuDetailRow`): a pass-through type is
/// "Pass-through whole GPU" when its group has vGPUs, else the group's
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageVgpuName {
    Passthrough,
    Group(String),
    Model(String),
}

/// The row of `v`: name, vGPUs per GPU and video RAM (bytes).
pub fn page_vgpu_row(model: &ConnectionModel, v: &PageVgpu) -> (PageVgpuName, Option<i64>, i64) {
    let group = model.get(&v.group);
    let t = v
        .vgpu_type
        .as_ref()
        .and_then(|r| model.get(r).map(|t| (r, t)));
    match t {
        Some((r, t)) if !is_passthrough(t) => (
            PageVgpuName::Model(t.model_name.clone()),
            Some(vgpu_type_capacity(model, r, t)),
            t.framebuffer_size,
        ),
        _ => {
            let name = match group {
                Some(g) if !group_has_vgpu(model, g) => PageVgpuName::Group(gpu_group_name(g)),
                _ => PageVgpuName::Passthrough,
            };
            (name, None, t.map_or(0, |(_, t)| t.framebuffer_size))
        }
    }
}

/// Whether every listed vGPU's type has compatible types (vacuously true).
pub fn multiple_vgpu_support(model: &ConnectionModel, list: &[PageVgpu]) -> bool {
    list.iter().all(|v| {
        v.vgpu_type
            .as_ref()
            .and_then(|t| model.get(t))
            .is_some_and(|t| !t.compatible_types_in_vm.is_empty())
    })
}

/// The page's state (`GpuEditPage.ShowHideWarnings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GpuPageState {
    pub block: Option<DeviceBlock>,
    pub can_add: bool,
    pub experimental: bool,
    /// `Some(true)`: "supports multiple instances"; `Some(false)`: "does
    /// not"; `None`: no vGPU.
    pub multiple: Option<bool>,
    /// The Remote Desktop warning (a first pass-through GPU).
    pub rdp_warning: bool,
    /// "Needs drivers" and "needs a GPU at boot".
    pub need_gpu: bool,
}

/// The state of the GPU page of `vm` listing `list`.
pub fn gpu_page_state(
    model: &ConnectionModel,
    vm: &VmRecord,
    list: &[PageVgpu],
    priority: &str,
) -> GpuPageState {
    let mut s = GpuPageState {
        experimental: list.iter().any(|v| v.is_experimental(model)),
        block: device_block(model, vm, priority),
        ..Default::default()
    };
    if s.block.is_some() {
        return s;
    }
    let multiple = multiple_vgpu_support(model, list);
    s.can_add = gpus_available(model) && multiple;
    s.multiple = (!list.is_empty()).then_some(multiple);
    let current = PageVgpu::of_vm(model, vm);
    s.rdp_warning = list != current.as_slice()
        && current.is_empty()
        && list.iter().any(|v| v.is_passthrough(model));
    s.need_gpu = !list.is_empty();
    s
}

/// An entry of the Add vGPU list (`GpuTuple`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgpuChoice {
    pub group: Ref<GpuGroup>,
    /// `None` on a group header (or a group without a known pass-through
    /// type in pass-through-only mode).
    pub vgpu_type: Option<Ref<VgpuType>>,
    pub text: VgpuChoiceText,
    /// Headers and types not allowed on the group cannot be picked.
    pub selectable: bool,
}

/// What an Add vGPU entry says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VgpuChoiceText {
    /// A group (bold header, or a pass-through-only group).
    Group(String),
    /// A type under its group, indented.
    Type(VgpuTypeName),
}

/// The Add vGPU dialog's list for a VM whose page lists `list`
/// (`AddVGPUDialog.PopulateComboBox`). Without vGPU (licence or
/// template), each group with pass-through offers it under the group's
/// name — picking it adds the group's pass-through type (the original
/// added nothing, a bug).
pub fn add_vgpu_choices(
    model: &ConnectionModel,
    vm: &VmRecord,
    list: &[PageVgpu],
) -> Vec<VgpuChoice> {
    let mut groups: Vec<(&Ref<GpuGroup>, &GpuGroupRecord)> = model
        .gpu_groups
        .iter()
        .filter(|(_, g)| !g.pgpus.is_empty() && !g.supported_vgpu_types.is_empty())
        .map(|(r, g)| (r, &**g))
        .collect();
    groups.sort_by(|a, b| {
        objects::natural_cmp(&gpu_group_name(a.1), &gpu_group_name(b.1)).then_with(|| a.0.cmp(b.0))
    });
    let passthrough_only =
        forbidden(model, DeviceRestriction::Vgpu) || !vm_can_have_vgpu(model, vm);
    let mut out = Vec::new();
    for (r, g) in groups {
        if passthrough_only {
            if group_has_passthrough(model, g) {
                out.push(VgpuChoice {
                    group: r.clone(),
                    vgpu_type: group_passthrough_type(model, g),
                    text: VgpuChoiceText::Group(gpu_group_name(g)),
                    selectable: true,
                });
            }
            continue;
        }
        let mut common: BTreeSet<Ref<VgpuType>> = g.supported_vgpu_types.iter().cloned().collect();
        for v in list {
            if let Some(t) = v.vgpu_type.as_ref().and_then(|t| model.get(t)) {
                let compatible: BTreeSet<Ref<VgpuType>> =
                    t.compatible_types_in_vm.iter().cloned().collect();
                common = common.intersection(&compatible).cloned().collect();
            }
        }
        let common: Vec<Ref<VgpuType>> = common.into_iter().collect();
        let types = sorted_types_desc(model, &common);
        let has_vgpu = group_has_vgpu(model, g);
        if has_vgpu && !types.is_empty() {
            out.push(VgpuChoice {
                group: r.clone(),
                vgpu_type: None,
                text: VgpuChoiceText::Group(gpu_group_name(g)),
                selectable: false,
            });
        }
        for t in types {
            let enabled = g.enabled_vgpu_types.contains(&t);
            let text = if has_vgpu {
                VgpuChoiceText::Type(vgpu_type_name(model, &t).unwrap_or(VgpuTypeName::Passthrough))
            } else {
                VgpuChoiceText::Group(gpu_group_name(g))
            };
            out.push(VgpuChoice {
                group: r.clone(),
                vgpu_type: Some(t),
                text,
                selectable: enabled,
            });
        }
    }
    out
}

/// The entry the Add vGPU dialog selects first: the only selectable one
/// when the list offers a single type.
pub fn default_vgpu_choice(choices: &[VgpuChoice]) -> Option<usize> {
    let mut selectable = choices.iter().enumerate().filter(|(_, c)| c.selectable);
    let first = selectable.next()?;
    selectable.next().is_none().then_some(first.0)
}

/// The GPU page's summary in the page list (`GpuEditPage.SubText`):
/// `None` when no GPU is available, else the vGPUs' descriptions.
pub fn gpu_page_summary(model: &ConnectionModel, list: &[PageVgpu]) -> Option<Vec<PageVgpuName>> {
    gpus_available(model).then(|| list.iter().map(|v| page_vgpu_row(model, v).0).collect())
}

// ---- USB ----------------------------------------------------------------------------------------

/// How a device is described (`PUSB.Description`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PusbDescription {
    Text(String),
    /// "Vendor: {vendor}; Product: {product}".
    Ids {
        vendor: String,
        product: String,
    },
}

/// The description of `p`.
pub fn pusb_description(p: &PusbRecord) -> PusbDescription {
    if p.description.is_empty() {
        PusbDescription::Ids {
            vendor: p.vendor_id.clone(),
            product: p.product_id.clone(),
        }
    } else {
        PusbDescription::Text(p.description.clone())
    }
}

/// The VM using a device: the VM of its group's first VUSB
/// (`UsbPage`'s "Virtual machine").
pub fn pusb_vm<'a>(model: &'a ConnectionModel, p: &PusbRecord) -> Option<&'a VmRecord> {
    let group = model.get(&p.usb_group)?;
    let vusb = model.get(group.vusbs.first()?)?;
    model.get(&vusb.vm).map(|v| &**v)
}

/// Whether a device is in use: its USB group has a VUSB (on any VM,
/// running or not).
pub fn pusb_in_use(model: &ConnectionModel, p: &PusbRecord) -> bool {
    model.get(&p.usb_group).is_some_and(|g| !g.vusbs.is_empty())
}

/// Whether a live host shows the USB tab: it has devices and no server's
/// licence restricts USB pass-through.
pub fn usb_tab_shown(model: &ConnectionModel, host: &HostRecord) -> bool {
    !host.pusbs.is_empty() && !forbidden(model, DeviceRestriction::Usb)
}

/// A host's devices, in its order.
pub fn host_pusbs<'a>(
    model: &'a ConnectionModel,
    host: &'a HostRecord,
) -> Vec<(&'a Ref<Pusb>, &'a PusbRecord)> {
    host.pusbs
        .iter()
        .filter_map(|r| model.pusbs.get_key_value(r).map(|(r, p)| (r, &**p)))
        .collect()
}

/// Whether a VM's Properties dialog has the USB page: an HVM VM (not a
/// template or snapshot), USB pass-through licensed, and some server
/// with a device.
pub fn vm_usb_page_shown(model: &ConnectionModel, vm: &VmRecord) -> bool {
    !vm.is_a_snapshot
        && !vm.is_a_template
        && objects::vm_is_hvm(vm)
        && !forbidden(model, DeviceRestriction::Usb)
        && model.hosts.values().any(|h| !h.pusbs.is_empty())
}

/// The device behind a VUSB (its group's first PUSB).
pub fn vusb_device<'a>(model: &'a ConnectionModel, vusb: &Ref<Vusb>) -> Option<&'a PusbRecord> {
    let v = model.get(vusb)?;
    let g = model.get(&v.usb_group)?;
    model.get(g.pusbs.first()?).map(|p| &**p)
}

/// The device model USB pass-through needs.
pub const UPSTREAM_QEMU: &str = "qemu-upstream-compat";

/// `VM.UsingUpstreamQemu`.
pub fn uses_upstream_qemu(vm: &VmRecord) -> bool {
    vm.platform.get("device-model").map(String::as_str) == Some(UPSTREAM_QEMU)
}

/// The Attach USB dialog's devices: per server `VM.get_possible_hosts`
/// named (in its order), the pass-through enabled devices nobody uses;
/// servers without one are left out.
pub fn attachable_pusbs(
    model: &ConnectionModel,
    possible_hosts: &[Ref<Host>],
) -> Vec<(Ref<Host>, Vec<Ref<Pusb>>)> {
    possible_hosts
        .iter()
        .filter_map(|h| model.get(h).map(|rec| (h, rec)))
        .filter_map(|(h, rec)| {
            let list: Vec<Ref<Pusb>> = host_pusbs(model, rec)
                .into_iter()
                .filter(|(_, p)| p.passthrough_enabled && !pusb_in_use(model, p))
                .map(|(r, _)| r.clone())
                .collect();
            (!list.is_empty()).then(|| (h.clone(), list))
        })
        .collect()
}

// ---- vTPM ---------------------------------------------------------------------------------------

/// Why Manage vTPMs... is not available for a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtpmUnavailable {
    /// Not a single real VM, licence, or XAPI too old: no reason shown.
    Hidden,
    /// "TPM is supported only for VMs using the UEFI or UEFI secure boot
    /// modes."
    NotUefi,
}

/// `VM.IsDefaultBootModeUefi`.
pub fn boots_uefi(vm: &VmRecord) -> bool {
    vm.hvm_boot_params
        .get("firmware")
        .is_some_and(|f| f.trim().eq_ignore_ascii_case("uefi"))
}

/// Whether vTPMs are offered on this connection at all (the VM menu item's
/// visibility): XAPI 22.26 or later and no licence restriction.
pub fn vtpm_supported(model: &ConnectionModel) -> bool {
    xapi_22_26_or_greater(model) && !forbidden(model, DeviceRestriction::Vtpm)
}

/// `VtpmCommand.CanRun` and its reason.
pub fn vtpm_command(model: &ConnectionModel, vm: &VmRecord) -> Result<(), VtpmUnavailable> {
    if objects::vm_kind(vm) != objects::VmKind::Guest || !vtpm_supported(model) {
        return Err(VtpmUnavailable::Hidden);
    }
    if !(objects::vm_is_hvm(vm) && boots_uefi(vm)) {
        return Err(VtpmUnavailable::NotUefi);
    }
    Ok(())
}

/// Why a vTPM cannot be added or removed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtpmRefusal {
    /// "Attaching a new vTPM to this VM is currently not allowed."
    AddNotAllowed,
    /// "The maximum number of vTPMs that can be attached to this VM is 1."
    MaxReached,
    /// "You need to shut down this VM before you can attach a new vTPM to
    /// it."
    AddNotHalted,
    /// "Removing this vTPM from the VM is currently not allowed."
    RemoveNotAllowed,
    /// "You need to shut down the VM before you can remove this vTPM."
    RemoveNotHalted,
}

/// `VM.MAX_ALLOWED_VTPMS`.
pub const MAX_VTPMS: usize = 1;

/// `VM.CanAddVtpm`.
pub fn can_add_vtpm(model: &ConnectionModel, vm: &VmRecord) -> Result<(), VtpmRefusal> {
    if xapi_23_11_or_greater(model) {
        return if vm.allowed_operations.contains(&VmOperations::CreateVtpm) {
            Ok(())
        } else {
            Err(VtpmRefusal::AddNotAllowed)
        };
    }
    if vm.vtpms.len() >= MAX_VTPMS {
        Err(VtpmRefusal::MaxReached)
    } else if vm.power_state != VmPowerState::Halted {
        Err(VtpmRefusal::AddNotHalted)
    } else {
        Ok(())
    }
}

/// Whether `vtpm` can be removed (`VtpmManagementPage.CanRemoveVtpm`).
pub fn can_remove_vtpm(model: &ConnectionModel, vtpm: &Ref<Vtpm>) -> Result<(), VtpmRefusal> {
    let Some(t) = model.get(vtpm) else {
        return Err(VtpmRefusal::RemoveNotAllowed);
    };
    if xapi_23_11_or_greater(model) {
        return if t.allowed_operations.contains(&VtpmOperations::Destroy) {
            Ok(())
        } else {
            Err(VtpmRefusal::RemoveNotAllowed)
        };
    }
    match model.get(&t.vm) {
        Some(vm) if vm.power_state == VmPowerState::Halted => Ok(()),
        Some(_) => Err(VtpmRefusal::RemoveNotHalted),
        None => Err(VtpmRefusal::RemoveNotAllowed),
    }
}

/// The General tab's "Device Security" row: the VM's vTPM count, when
/// vTPMs are supported and it has one.
pub fn vtpm_count_shown(model: &ConnectionModel, vm: &VmRecord) -> Option<usize> {
    (vtpm_supported(model) && !vm.vtpms.is_empty()).then_some(vm.vtpms.len())
}

// ---- requests -----------------------------------------------------------------------------------

/// An F14a change (run by [`crate::devices_run`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceRequest {
    /// `VgpuConfigurationAction`: each GPU's new allowed types.
    SetAllowedVgpuTypes {
        pgpus: Vec<(Ref<Pgpu>, Vec<Ref<VgpuType>>)>,
    },
    /// `SetGpuPlacementPolicyAction`: every GPU group.
    SetPlacementPolicy { algorithm: AllocationAlgorithm },
    /// `UpdateIntegratedGpuPassthroughAction`.
    SetIntegratedGpu { host: Ref<Host>, enable: bool },
    /// `GpuAssignAction`: the VM keeps `keep`, loses its other vGPUs and
    /// gets the new ones (group, type) in order.
    AssignVgpus {
        vm: Ref<Vm>,
        keep: Vec<Ref<Vgpu>>,
        add: Vec<(Ref<GpuGroup>, Option<Ref<VgpuType>>)>,
    },
    /// `SetUsbPassthroughAction`.
    SetUsbPassthrough { pusb: Ref<Pusb>, enabled: bool },
    /// `CreateVUSBAction`; `set_platform`: the VM does not use the
    /// upstream device model yet (decided when the request is made, as the
    /// original's constructor does).
    AttachUsb {
        vm: Ref<Vm>,
        pusb: Ref<Pusb>,
        set_platform: bool,
    },
    /// `DeleteVUSBAction`; `attached`: the VUSB was plugged in when asked.
    DetachUsb {
        vm: Ref<Vm>,
        vusb: Ref<Vusb>,
        attached: bool,
    },
    /// `NewVtpmAction`.
    AddVtpm { vm: Ref<Vm> },
    /// `RemoveVtpmAction`.
    RemoveVtpm { vm: Ref<Vm>, vtpm: Ref<Vtpm> },
    /// `ChangeMemorySettingsAction` (see [`crate::memory`]).
    SetMemory(crate::memory::MemoryChange),
    /// "Turn on Remote Desktop": the guest agent's `request-rdp-on`.
    EnableRdp { vm: Ref<Vm> },
}

impl DeviceRequest {
    /// The VM it concerns, if any (the history's object).
    pub fn vm(&self) -> Option<&Ref<Vm>> {
        match self {
            Self::AssignVgpus { vm, .. }
            | Self::AttachUsb { vm, .. }
            | Self::DetachUsb { vm, .. }
            | Self::AddVtpm { vm }
            | Self::RemoveVtpm { vm, .. }
            | Self::EnableRdp { vm } => Some(vm),
            Self::SetMemory(c) => Some(&c.vm),
            _ => None,
        }
    }

    /// The calls it makes, as RBAC permission names (each action's
    /// `ApiMethodsToRoleCheck`; the detach's are VUSB calls — the original
    /// listed VBD ones by mistake).
    pub fn rbac_methods(&self) -> Vec<&'static str> {
        match self {
            Self::SetAllowedVgpuTypes { .. } => vec!["pgpu.set_enabled_vgpu_types"],
            Self::SetPlacementPolicy { .. } => vec!["gpu_group.set_allocation_algorithm"],
            Self::SetIntegratedGpu { enable: true, .. } => {
                vec![
                    "host.enable_display",
                    "pgpu.enable_dom0_access",
                    "task.destroy",
                ]
            }
            Self::SetIntegratedGpu { enable: false, .. } => {
                vec![
                    "host.disable_display",
                    "pgpu.disable_dom0_access",
                    "task.destroy",
                ]
            }
            Self::AssignVgpus { .. } => vec!["vgpu.destroy", "vgpu.create", "task.destroy"],
            Self::SetUsbPassthrough { .. } => vec!["pusb.set_passthrough_enabled"],
            Self::AttachUsb { set_platform, .. } => {
                let mut m = Vec::new();
                if *set_platform {
                    m.push("vm.set_platform");
                }
                m.push("vusb.create");
                m
            }
            Self::DetachUsb { attached, .. } => {
                let mut m = Vec::new();
                if *attached {
                    m.extend(["vusb.get_allowed_operations", "vusb.unplug"]);
                }
                m.extend(["vusb.destroy", "task.destroy"]);
                m
            }
            Self::AddVtpm { .. } => vec!["vtpm.create"],
            Self::RemoveVtpm { .. } => vec!["vtpm.destroy"],
            Self::SetMemory(c) => c.methods.clone(),
            Self::EnableRdp { .. } => vec!["vm.call_plugin"],
        }
    }

    /// Attach `pusb` to `vm`.
    pub fn attach_usb(model: &ConnectionModel, vm: &Ref<Vm>, pusb: &Ref<Pusb>) -> Self {
        Self::AttachUsb {
            vm: vm.clone(),
            pusb: pusb.clone(),
            set_platform: model.get(vm).is_none_or(|v| !uses_upstream_qemu(v)),
        }
    }

    /// Detach `vusb` from `vm`.
    pub fn detach_usb(model: &ConnectionModel, vm: &Ref<Vm>, vusb: &Ref<Vusb>) -> Self {
        Self::DetachUsb {
            vm: vm.clone(),
            vusb: vusb.clone(),
            attached: model.get(vusb).is_some_and(|v| v.currently_attached),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lic(host: &mut HostRecord, key: &str, value: &str) {
        host.license_params.insert(key.into(), value.into());
    }

    #[test]
    fn licence_keys_prefer_restriction() {
        let mut h = HostRecord::default();
        assert!(host_restricts(&h, DeviceRestriction::Gpu));
        lic(&mut h, "restrict_gpu", "false");
        assert!(!host_restricts(&h, DeviceRestriction::Gpu));
        lic(&mut h, "restrict_gpu", "true");
        assert!(host_restricts(&h, DeviceRestriction::Gpu));
        lic(&mut h, "restrict_usb_passthrough", "true");
        assert!(host_restricts(&h, DeviceRestriction::Usb));
    }

    #[test]
    fn pci_slots_and_group_names() {
        let v = PageVgpu {
            existing: None,
            group: Ref::new("OpaqueRef:g"),
            vgpu_type: None,
            device: Some("0".into()),
        };
        assert_eq!(v.pci_slot().as_deref(), Some("11"));
        let new = PageVgpu { device: None, ..v };
        assert_eq!(new.pci_slot(), None);
        let g = GpuGroupRecord {
            name_label: "Group of NVIDIA Corporation GK104GL [GRID K2] GPUs".into(),
            ..Default::default()
        };
        assert_eq!(
            gpu_group_name(&g),
            "NVIDIA Corporation GK104GL [GRID K2] GPUs"
        );
    }

    #[test]
    fn usb_description_falls_back_to_ids() {
        let mut p = PusbRecord {
            vendor_id: "0781".into(),
            product_id: "5567".into(),
            ..Default::default()
        };
        assert_eq!(
            pusb_description(&p),
            PusbDescription::Ids {
                vendor: "0781".into(),
                product: "5567".into()
            }
        );
        p.description = "SanDisk Cruzer Blade".into();
        assert_eq!(
            pusb_description(&p),
            PusbDescription::Text("SanDisk Cruzer Blade".into())
        );
    }

    #[test]
    fn uefi_boot_mode() {
        let mut vm = VmRecord::default();
        assert!(!boots_uefi(&vm));
        vm.hvm_boot_params
            .insert("firmware".into(), " UEFI ".into());
        assert!(boots_uefi(&vm));
    }
}
