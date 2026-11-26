//! F14a: a VM's memory settings — the Memory tab's Edit button, the
//! Memory Settings dialog and wizard (`BallooningDialog`,
//! `BallooningWizard`, `VMMemoryControlsBasic` / `Advanced`,
//! `MemorySpinner`) and `ChangeMemorySettingsAction`.
//!
//! **Mode.** Advanced (minimum, maximum and static maximum) when Dynamic
//! Memory Control is licensed and the first VM already balloons (its
//! dynamic maximum differs from its static maximum and it supports
//! ballooning); otherwise Basic: a fixed memory, or a range whose maximum
//! is also the static maximum, the range only when the VM supports
//! ballooning and DMC is licensed (else the reason is shown).
//!
//! **Spinners** ([`Spinner`]) hold MiB, shown in MB when the first VM's
//! static maximum is 1 GiB or less, else in GB with one decimal; an
//! untouched spinner keeps the exact bytes. Their ranges are rounded
//! inwards to that unit and a value outside its new range is clamped, as
//! WinForms' `NumericUpDown` does, which is what pushes the other values
//! along.
//!
//! **Limits.** The dynamic minimum stays at least the static minimum and
//! `ratio × static maximum` (the pool's `memory-ratio-hvm` /
//! `memory-ratio-pv`, 0.25 by default; raising the maximum raises the
//! minimum with it); while the VM runs, at most what its server can give
//! it (`maxDynMin`); the static maximum at most the template's
//! `memory-static-max` (1 TiB or more).
//!
//! **Apply** ([`MemoryEdit::apply`]): a static maximum unchanged at MiB
//! precision keeps its exact bytes (no reboot for rounding); when it
//! changes and a VM is not halted, one question is asked first. Each VM
//! then gets a [`MemoryChange`]: a running VM whose static limits stay
//! changes its dynamic range live (`VM.set_memory_dynamic_range`); a
//! halted one gets `VM.set_memory_limits`; one that must reboot is shut
//! down (cleanly if allowed), changed, and started again on its home
//! server even if the change failed.

use xapi::{Ref, Vm, VmOperations, VmPowerState, VmRecord};

use crate::devices::{DeviceRestriction, forbidden};
use crate::model::ConnectionModel;
use crate::objects;

/// One mebibyte.
pub const MIB: i64 = 1 << 20;
/// One gibibyte.
pub const GIB: i64 = 1 << 30;
/// `VM.DEFAULT_MEM_ALLOWED`: 1 TiB.
pub const DEFAULT_MEM_ALLOWED: i64 = 1 << 40;
/// The default `memory-ratio-*`.
pub const DEFAULT_RATIO: f64 = 0.25;

/// The unit every spinner of a dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemUnit {
    Mb,
    Gb,
}

impl MemUnit {
    /// MB when the static maximum is 1 GiB or less.
    pub fn for_static_max(static_max: i64) -> Self {
        if static_max <= GIB {
            Self::Mb
        } else {
            Self::Gb
        }
    }

    /// Decimals shown.
    pub fn decimals(self) -> usize {
        match self {
            Self::Mb => 0,
            Self::Gb => 1,
        }
    }

    /// MiB → shown value.
    pub fn show(self, mib: f64) -> f64 {
        match self {
            Self::Mb => mib.round(),
            Self::Gb => round1(mib / 1024.0),
        }
    }

    /// Typed value → MiB.
    pub fn mib(self, value: f64) -> f64 {
        match self {
            Self::Mb => value,
            Self::Gb => value * 1024.0,
        }
    }

    /// Bytes → MiB rounded up to the unit (`Initialize(…, RoundingBehavior.Up)`).
    fn ceil_mib(self, bytes: f64) -> f64 {
        match self {
            Self::Mb => (bytes / MIB as f64).ceil(),
            Self::Gb => (bytes / GIB as f64 * 10.0 - 1e-9).ceil() / 10.0 * 1024.0,
        }
    }
}

/// Rounds to one decimal, halves away from zero.
fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// `Util.CorrectRoundingErrors`: bytes within one of a round tenth of a
/// GiB snap to it.
fn correct_rounding(bytes: i64) -> f64 {
    let gib = round1(bytes as f64 / GIB as f64) * GIB as f64;
    if (bytes as f64 - gib).abs() <= 1.0 {
        gib
    } else {
        bytes as f64
    }
}

/// A memory spinner: its value and range in MiB (the range already
/// rounded inwards to the unit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spinner {
    pub mib: f64,
    pub min: f64,
    pub max: f64,
}

impl Spinner {
    fn new(bytes: i64) -> Self {
        Self {
            mib: correct_rounding(bytes) / MIB as f64,
            min: 0.0,
            max: f64::MAX,
        }
    }

    /// The value in bytes (truncated, as `(long)`).
    pub fn bytes(&self) -> i64 {
        (self.mib * MIB as f64) as i64
    }

    fn bytes_f(&self) -> f64 {
        self.mib * MIB as f64
    }

    /// `SetRange`: rounded inwards (unrounded if that inverts it),
    /// ignored when `min > max`; the value is clamped into it. Whether the
    /// value moved.
    fn set_range(&mut self, unit: MemUnit, min: f64, max: f64) -> bool {
        if min > max {
            return false;
        }
        let (rmin, rmax) = match unit {
            MemUnit::Mb => ((min / MIB as f64).ceil(), (max / MIB as f64).floor()),
            MemUnit::Gb => (
                (min / GIB as f64 * 10.0 - 1e-9).ceil() / 10.0 * 1024.0,
                (max / GIB as f64 * 10.0 + 1e-9).floor() / 10.0 * 1024.0,
            ),
        };
        let (lo, hi) = if rmin <= rmax {
            (rmin, rmax)
        } else {
            (min / MIB as f64, max / MIB as f64)
        };
        self.min = lo;
        self.max = hi;
        self.clamp()
    }

    fn clamp(&mut self) -> bool {
        let v = self.mib.clamp(self.min, self.max);
        let moved = v != self.mib;
        self.mib = v;
        moved
    }
}

/// Basic or Advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMode {
    Basic,
    Advanced,
}

/// Why a memory range cannot be set (Basic mode's label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmcUnavailable {
    /// "…not available due to license restrictions on the server."
    License,
    /// "A memory range cannot be set for this template."
    Template,
    /// "…these VMs." (their tools differ).
    Vms,
    /// The OS does not support it (tools with I/O drivers installed).
    NotSupported { plural: bool },
    /// Windows without I/O drivers.
    NoIo { plural: bool },
    /// No guest tools.
    NoTools { plural: bool },
}

impl DmcUnavailable {
    /// Whether the "Install tools" text goes with it.
    pub fn tools_link(self) -> bool {
        !matches!(self, Self::License | Self::Template)
    }
}

/// Which spinner the user changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryField {
    DynMin,
    DynMax,
    StaticMax,
    Fixed,
}

/// The question asked before a static maximum change reboots a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryConfirm {
    /// `CONFIRM_CHANGE_STATIC_MAX_*` (Advanced).
    StaticMax { plural: bool },
    /// `CONFIRM_CHANGE_MEMORY_MAX_*` (Basic, the VM balloons).
    MemoryMax { plural: bool },
    /// `CONFIRM_CHANGE_MEMORY_*` (forcibly rebooted).
    Memory { plural: bool },
}

/// What one VM gets (`ChangeMemorySettingsAction`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemoryChange {
    pub vm: Ref<Vm>,
    pub static_min: i64,
    pub static_max: i64,
    pub dynamic_min: i64,
    pub dynamic_max: i64,
    /// The calls it will make (decided when it is made, from the VM's
    /// state then: [`MemoryChange::rbac_methods`]).
    pub methods: Vec<&'static str>,
}

impl MemoryChange {
    /// The static limits change.
    pub fn static_changed(&self, vm: &VmRecord) -> bool {
        self.static_min != vm.memory_static_min || self.static_max != vm.memory_static_max
    }

    /// The VM is shut down and started again around the change.
    pub fn needs_reboot(&self, vm: &VmRecord) -> bool {
        if self.static_changed(vm) {
            vm.power_state != VmPowerState::Halted
        } else {
            !matches!(vm.power_state, VmPowerState::Halted | VmPowerState::Running)
        }
    }

    /// `ApiMethodsToRoleCheck`.
    pub fn rbac_methods(&self, model: &ConnectionModel) -> Vec<&'static str> {
        let Some(vm) = model.get(&self.vm) else {
            return vec!["vm.set_memory_limits"];
        };
        let mut m = vec![if self.static_changed(vm) {
            "vm.set_memory_limits"
        } else {
            "vm.set_memory_dynamic_range"
        }];
        if self.needs_reboot(vm) {
            m.push(
                if vm.allowed_operations.contains(&VmOperations::CleanShutdown) {
                    "vm.clean_shutdown"
                } else {
                    "vm.hard_shutdown"
                },
            );
            m.push(if objects::vm_home(model, vm).is_some() {
                "vm.start_on"
            } else {
                "vm.start"
            });
            m.push("task.destroy");
        }
        m
    }
}

/// Whether the tools status of a running VM is known (no Unknown flag):
/// metrics, and the guest agent reporting or two minutes since boot.
fn tools_known(model: &ConnectionModel, vm: &VmRecord, now: i64) -> bool {
    let Some(metrics) = model.get(&vm.metrics) else {
        return false;
    };
    if model.get(&vm.guest_metrics).is_some() {
        return true;
    }
    metrics
        .start_time
        .unix_seconds()
        .is_some_and(|t| now - t >= 120)
}

/// Whether a Memory tab row of `vms` has an Edit button: each is halted,
/// or running with a known tools status (`VMMemoryControlsNoEdit`).
pub fn can_edit(model: &ConnectionModel, vms: &[&VmRecord], now: i64) -> bool {
    !vms.is_empty()
        && vms.iter().all(|vm| match vm.power_state {
            VmPowerState::Halted => true,
            VmPowerState::Running => tools_known(model, vm, now),
            _ => false,
        })
}

/// `VM.UsesBallooning`.
pub fn uses_ballooning(model: &ConnectionModel, vm: &VmRecord) -> bool {
    vm.memory_dynamic_max != vm.memory_static_max && objects::vm_supports_ballooning(model, vm)
}

/// `VM.MaxMemAllowed`: the template's `memory-static-max`, else the most
/// any template allows, at least 1 TiB.
pub fn max_mem_allowed(model: &ConnectionModel, vm: &VmRecord) -> i64 {
    let parse = |v: Option<String>| v.and_then(|v| v.trim().parse::<i64>().ok());
    if let Some(v) = parse(crate::devices::vm_restriction(
        model,
        vm,
        "memory-static-max",
    )) {
        return v;
    }
    model
        .vms
        .values()
        .filter(|t| t.is_a_template)
        .filter_map(|t| parse(crate::provisioning::recommendation(t, "memory-static-max")))
        .fold(DEFAULT_MEM_ALLOWED, i64::max)
}

/// The pool's dynamic-minimum ratio for these VMs: HVM's, PV's, or the
/// larger when mixed.
pub fn memory_ratio(model: &ConnectionModel, vms: &[&VmRecord]) -> f64 {
    let read = |key: &str| {
        model
            .pool()
            .and_then(|(_, p)| p.other_config.get(key))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| (0.0..=1.0).contains(v))
            .unwrap_or(DEFAULT_RATIO)
    };
    let hvm = vms.iter().filter(|v| objects::vm_is_hvm(v)).count();
    if hvm == vms.len() {
        read("memory-ratio-hvm")
    } else if hvm == 0 {
        read("memory-ratio-pv")
    } else {
        read("memory-ratio-hvm").max(read("memory-ratio-pv"))
    }
}

/// `maxDynMin`: what the running VM's server leaves for the edited VMs,
/// shared between them; `None` when not running.
fn max_dyn_min(model: &ConnectionModel, vms: &[Ref<Vm>]) -> Option<i64> {
    let vm0 = model.get(vms.first()?)?;
    if !matches!(
        vm0.power_state,
        VmPowerState::Running | VmPowerState::Paused
    ) {
        return None;
    }
    let host = model.get(&vm0.resident_on)?;
    let metrics = model.get(&host.metrics)?;
    let mut sum = host.memory_overhead;
    for r in &host.resident_vms {
        let Some(vm) = model.get(r) else { continue };
        sum += vm.memory_overhead;
        if vm.is_control_domain {
            sum += model.get(&vm.metrics).map_or(0, |m| m.memory_actual);
        } else if !vms.contains(r) {
            sum += vm.memory_dynamic_min;
        }
    }
    Some((metrics.memory_total - sum).max(0) / vms.len() as i64)
}

/// I/O drivers detected on a running VM (`IoDriversInstalled`).
fn io_drivers(model: &ConnectionModel, vm: &VmRecord) -> Option<bool> {
    if vm.power_state != VmPowerState::Running || model.get(&vm.metrics).is_none() {
        return None;
    }
    let gm = model.get(&vm.guest_metrics)?;
    Some(gm.pv_drivers_detected || gm.pv_drivers_version.contains_key("major"))
}

/// The Memory Settings dialog or wizard's state.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEdit {
    pub vms: Vec<Ref<Vm>>,
    pub mode: MemoryMode,
    pub unit: MemUnit,
    /// The first VM is a template (the rubric says so).
    pub template: bool,
    /// Basic: the range is chosen (else the fixed memory).
    pub dynamic: bool,
    /// Basic: why the range cannot be chosen.
    pub unavailable: Option<DmcUnavailable>,
    pub dyn_min: Spinner,
    pub dyn_max: Spinner,
    pub static_max: Spinner,
    pub fixed: Spinner,
    has_ballooning: bool,
    licensed: bool,
    orig_static_max: i64,
    static_min: i64,
    ratio: f64,
    max_dyn_min: Option<i64>,
    spinner_max: i64,
}

impl MemoryEdit {
    /// The dialog (one VM) or wizard page (several) for `vms`.
    pub fn new(model: &ConnectionModel, vms: &[Ref<Vm>]) -> Option<Self> {
        let recs: Vec<&VmRecord> = vms
            .iter()
            .filter_map(|r| model.get(r).map(|v| &**v))
            .collect();
        let vm0 = *recs.first()?;
        let licensed = !forbidden(model, DeviceRestriction::Dmc);
        let has_ballooning = objects::vm_supports_ballooning(model, vm0);
        let mode = if licensed && uses_ballooning(model, vm0) {
            MemoryMode::Advanced
        } else {
            MemoryMode::Basic
        };
        let max_allowed = recs
            .iter()
            .map(|v| max_mem_allowed(model, v))
            .min()
            .unwrap_or(DEFAULT_MEM_ALLOWED);
        let mut unavailable = None;
        let mut dynamic = false;
        if mode == MemoryMode::Basic {
            if has_ballooning && licensed {
                dynamic = vm0.memory_dynamic_min != vm0.memory_static_max;
            } else {
                unavailable = Some(Self::reason(model, &recs, licensed));
            }
        }
        let mut edit = Self {
            vms: vms.to_vec(),
            mode,
            unit: MemUnit::for_static_max(vm0.memory_static_max),
            template: vm0.is_a_template,
            dynamic,
            unavailable,
            dyn_min: Spinner::new(vm0.memory_dynamic_min),
            dyn_max: Spinner::new(vm0.memory_dynamic_max),
            static_max: Spinner::new(vm0.memory_static_max),
            fixed: Spinner::new(vm0.memory_static_max),
            has_ballooning,
            licensed,
            orig_static_max: vm0.memory_static_max,
            static_min: vm0.memory_static_min,
            ratio: memory_ratio(model, &recs),
            max_dyn_min: max_dyn_min(model, vms),
            spinner_max: vm0.memory_static_max.max(max_allowed),
        };
        edit.settle(None);
        Some(edit)
    }

    fn reason(model: &ConnectionModel, recs: &[&VmRecord], licensed: bool) -> DmcUnavailable {
        let vm0 = recs[0];
        let windows = crate::consoles::vm_is_windows(model, vm0);
        if !licensed {
            DmcUnavailable::License
        } else if recs.len() > 1 {
            let io: Vec<Option<bool>> = recs.iter().map(|v| io_drivers(model, v)).collect();
            if io.iter().any(|s| *s != io[0]) {
                DmcUnavailable::Vms
            } else if io[0] == Some(true) {
                DmcUnavailable::NotSupported { plural: true }
            } else if windows {
                DmcUnavailable::NoIo { plural: true }
            } else {
                DmcUnavailable::NoTools { plural: true }
            }
        } else if vm0.is_a_template {
            DmcUnavailable::Template
        } else if io_drivers(model, vm0) == Some(true) {
            DmcUnavailable::NotSupported { plural: false }
        } else if windows {
            DmcUnavailable::NoIo { plural: false }
        } else {
            DmcUnavailable::NoTools { plural: false }
        }
    }

    /// Basic: the range can be chosen.
    pub fn dynamic_allowed(&self) -> bool {
        self.mode == MemoryMode::Basic && self.unavailable.is_none()
    }

    /// The static maximum the controls give now (bytes).
    fn cur_static_max(&self) -> f64 {
        match self.mode {
            MemoryMode::Advanced => self.static_max.bytes_f(),
            MemoryMode::Basic if self.dynamic => self.dyn_max.bytes_f(),
            MemoryMode::Basic => self.fixed.bytes_f(),
        }
    }

    /// `DynMinSpinnerMax`.
    fn dyn_min_max(&self) -> f64 {
        let mut m = self.dyn_max.bytes_f();
        if let Some(limit) = self.max_dyn_min
            && (limit as f64) < m
        {
            m = (limit.max(self.static_min)) as f64;
        }
        m.max(self.dyn_min.bytes_f())
    }

    /// `DynMinSpinnerMin`.
    fn dyn_min_min(&self) -> f64 {
        let limit = (self.cur_static_max() * self.ratio).trunc();
        (self.static_min as f64)
            .max(limit)
            .min(self.dyn_min.bytes_f())
    }

    /// `StatMaxSpinnerMax`.
    fn stat_max_max(&self) -> f64 {
        let spinner_max = self.spinner_max as f64;
        let sm = match self.max_dyn_min {
            Some(limit) if spinner_max * self.ratio > limit as f64 => limit as f64 / self.ratio,
            _ => spinner_max,
        };
        sm.max(self.cur_static_max())
    }

    /// `SetSpinnerRanges`; whether a value was clamped.
    fn set_ranges(&mut self) -> bool {
        let unit = self.unit;
        let mut moved = false;
        match self.mode {
            MemoryMode::Basic => {
                let spinner_max = self.spinner_max as f64;
                let max_fixed = match self.max_dyn_min {
                    Some(l) if (l as f64) <= spinner_max => l as f64,
                    _ => spinner_max,
                };
                moved |= self
                    .fixed
                    .set_range(unit, (self.static_min.max(MIB)) as f64, max_fixed);
                if !self.has_ballooning {
                    return moved;
                }
                let (lo, hi) = (self.dyn_min_min(), self.dyn_min_max());
                moved |= self.dyn_min.set_range(unit, lo, hi);
                let hi = self.stat_max_max();
                moved |= self
                    .dyn_max
                    .set_range(unit, self.dyn_min.bytes_f().max(MIB as f64), hi);
            }
            MemoryMode::Advanced => {
                let (lo, hi) = (self.dyn_min_min(), self.dyn_min_max());
                moved |= self.dyn_min.set_range(unit, lo, hi);
                let (lo, hi) = (self.dyn_min.bytes_f(), self.static_max.bytes_f());
                moved |= self.dyn_max.set_range(unit, lo, hi);
                let hi = self.stat_max_max();
                moved |=
                    self.static_max
                        .set_range(unit, self.dyn_max.bytes_f().max(MIB as f64), hi);
            }
        }
        moved
    }

    fn free_ranges(&mut self) {
        let max = self.spinner_max as f64 / MIB as f64;
        for s in [
            &mut self.dyn_min,
            &mut self.dyn_max,
            &mut self.static_max,
            &mut self.fixed,
        ] {
            s.min = 0.0;
            s.max = max.max(s.mib);
        }
    }

    /// The ratio rule after the maximum (Basic) or static maximum
    /// (Advanced) changed: the minimum goes up to `ratio × static max`.
    fn push_min(&mut self) {
        let min = (self.cur_static_max() * self.ratio).trunc();
        if self.dyn_min.bytes_f() < min {
            self.free_ranges();
            self.dyn_min.mib = self.unit.ceil_mib(min);
        }
    }

    /// The ranges again, and the change logic again for whatever their
    /// clamping moved, until nothing moves.
    fn settle(&mut self, changed: Option<MemoryField>) {
        let mut changed = changed;
        for _ in 0..8 {
            let upper = match self.mode {
                MemoryMode::Basic => MemoryField::DynMax,
                MemoryMode::Advanced => MemoryField::StaticMax,
            };
            if changed == Some(upper) {
                self.push_min();
            }
            let before = (self.dyn_max.mib, self.static_max.mib);
            if !self.set_ranges() {
                return;
            }
            changed = if before.1 != self.static_max.mib {
                Some(MemoryField::StaticMax)
            } else if before.0 != self.dyn_max.mib {
                Some(MemoryField::DynMax)
            } else {
                Some(MemoryField::DynMin)
            };
        }
    }

    /// The spinner of `field`.
    pub fn spinner(&self, field: MemoryField) -> &Spinner {
        match field {
            MemoryField::DynMin => &self.dyn_min,
            MemoryField::DynMax => &self.dyn_max,
            MemoryField::StaticMax => &self.static_max,
            MemoryField::Fixed => &self.fixed,
        }
    }

    /// The value `field` shows, in the unit.
    pub fn shown(&self, field: MemoryField) -> f64 {
        self.unit.show(self.spinner(field).mib)
    }

    /// The range `field` accepts, in the unit.
    pub fn shown_range(&self, field: MemoryField) -> (f64, f64) {
        let s = self.spinner(field);
        (self.unit.show(s.min), self.unit.show(s.max))
    }

    /// The spinner step, in the unit (`MemorySpinner.SetIncrement`).
    pub fn step(&self, field: MemoryField) -> f64 {
        match self.unit {
            MemUnit::Mb => {
                let static_max = self.cur_static_max() / MIB as f64;
                let mut i = 1.0;
                while i < static_max / 8.0 && i < 128.0 {
                    i *= 2.0;
                }
                i
            }
            MemUnit::Gb => {
                if self.spinner(field).mib < 10.0 * 1024.0 {
                    0.1
                } else {
                    1.0
                }
            }
        }
    }

    /// The user typed `value` (in the unit) into `field`: clamped into
    /// its range, then the original's change logic.
    pub fn set(&mut self, field: MemoryField, value: f64) {
        let mib = self.unit.mib(match self.unit {
            MemUnit::Mb => value.round(),
            MemUnit::Gb => round1(value),
        });
        {
            let s = match field {
                MemoryField::DynMin => &mut self.dyn_min,
                MemoryField::DynMax => &mut self.dyn_max,
                MemoryField::StaticMax => &mut self.static_max,
                MemoryField::Fixed => &mut self.fixed,
            };
            if (self.unit.show(s.mib) - value).abs() < 1e-9 {
                return;
            }
            s.mib = mib;
            s.clamp();
        }
        if self.mode == MemoryMode::Basic {
            match field {
                MemoryField::DynMin | MemoryField::DynMax if self.dynamic_allowed() => {
                    self.dynamic = true;
                }
                MemoryField::Fixed => self.dynamic = false,
                _ => {}
            }
        }
        self.settle(Some(field));
    }

    /// Basic: choose the fixed memory or the range.
    pub fn set_dynamic(&mut self, dynamic: bool) {
        if !dynamic || self.dynamic_allowed() {
            self.dynamic = dynamic;
            self.settle(None);
        }
    }

    /// The (dynamic min, dynamic max, static max) the controls give.
    pub fn values(&self) -> (i64, i64, i64) {
        match self.mode {
            MemoryMode::Advanced => (
                self.dyn_min.bytes(),
                self.dyn_max.bytes(),
                self.static_max.bytes(),
            ),
            MemoryMode::Basic if self.dynamic => (
                self.dyn_min.bytes(),
                self.dyn_max.bytes(),
                self.dyn_max.bytes(),
            ),
            MemoryMode::Basic => (self.fixed.bytes(), self.fixed.bytes(), self.fixed.bytes()),
        }
    }

    /// `ChangeMemorySettings`: the question to ask first (if any) and one
    /// change per VM.
    pub fn apply(&self, model: &ConnectionModel) -> (Option<MemoryConfirm>, Vec<MemoryChange>) {
        let (dmin, dmax, smax) = self.values();
        let (mut dynamic_min, mut dynamic_max, mut static_max) = if self.has_ballooning {
            (dmin, dmax, smax)
        } else {
            (smax, smax, smax)
        };
        let mut confirm = None;
        if self.orig_static_max / MIB == static_max / MIB {
            if dynamic_min == static_max {
                dynamic_min = self.orig_static_max;
            }
            if dynamic_max == static_max {
                dynamic_max = self.orig_static_max;
            }
            static_max = self.orig_static_max;
        } else if let Some(vm) = self
            .vms
            .iter()
            .filter_map(|r| model.get(r))
            .find(|v| v.power_state != VmPowerState::Halted)
        {
            let plural = self.vms.len() > 1;
            confirm = Some(if self.mode == MemoryMode::Advanced {
                MemoryConfirm::StaticMax { plural }
            } else if objects::vm_supports_ballooning(model, vm) && self.licensed {
                MemoryConfirm::MemoryMax { plural }
            } else {
                MemoryConfirm::Memory { plural }
            });
        }
        let changes = self
            .vms
            .iter()
            .filter_map(|r| model.get(r).map(|v| (r, v)))
            .map(|(r, v)| {
                let mut c = MemoryChange {
                    vm: r.clone(),
                    static_min: v.memory_static_min,
                    static_max,
                    dynamic_min,
                    dynamic_max,
                    methods: Vec::new(),
                };
                c.methods = c.rbac_methods(model);
                c
            })
            .collect();
        (confirm, changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_rounding() {
        assert_eq!(MemUnit::for_static_max(GIB), MemUnit::Mb);
        assert_eq!(MemUnit::for_static_max(GIB + 1), MemUnit::Gb);
        assert_eq!(MemUnit::Gb.show(1536.0), 1.5);
        assert_eq!(MemUnit::Gb.ceil_mib(1.01 * GIB as f64), 1.1 * 1024.0);
        assert_eq!(MemUnit::Mb.ceil_mib(1.5 * MIB as f64), 2.0);
        assert_eq!(correct_rounding(GIB + 1), GIB as f64);
        let mut s = Spinner::new(3 * GIB);
        assert!(s.set_range(MemUnit::Gb, 0.33 * GIB as f64, 2.06 * GIB as f64));
        assert_eq!(
            (s.min, s.max, s.mib),
            (0.4 * 1024.0, 2.0 * 1024.0, 2.0 * 1024.0)
        );
    }
}
