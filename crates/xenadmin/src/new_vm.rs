//! The New VM wizard (`NewVMWizard`): Template, Name, Installation Media
//! (with the boot mode and vTPM), Home Server, CPU & Memory, Storage,
//! Networking and Finish, as the original lays them out. The pages the
//! original only shows for Citrix features (vGPU, cloud-config, LUN per
//! VDI, copying BIOS strings) are left out. Finishing returns a
//! [`NewVmSpec`] that [`xen_core`] runs as `CreateVMAction` does.

use std::sync::Arc;

use egui::{Button, ComboBox, Context, DragValue, Grid, RichText, ScrollArea, TextEdit, Ui};
use xapi::{Host, Network, Ref, Sr, Vdi, Vm, VmRecord};
use xen_core::format::{disk_size, memory_size};
use xen_core::objects::{self, VmKind};
use xen_core::provisioning::{
    self as prov, BootMode, DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif,
};
use xen_core::{AppState, ConnectionId, ConnectionModel, ConnectionView};

use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::fill;
use crate::prov_strings::prov_strings;
use crate::window_frame::dialog_window;

const GIB: f64 = (1u64 << 30) as f64;
const MIB: f64 = (1u64 << 20) as f64;
/// The original's limit on networks preset from a default template.
const MAX_DEFAULT_NETWORKS: usize = 4;

/// The wizard's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Template,
    Name,
    Media,
    Home,
    CpuMem,
    Storage,
    Networking,
    Finish,
}

const PAGES: [Page; 8] = [
    Page::Template,
    Page::Name,
    Page::Media,
    Page::Home,
    Page::CpuMem,
    Page::Storage,
    Page::Networking,
    Page::Finish,
];

impl Page {
    fn name(self) -> &'static str {
        let p = prov_strings();
        match self {
            Page::Template => p.page_template,
            Page::Name => p.page_name,
            Page::Media => p.page_media,
            Page::Home => p.page_home,
            Page::CpuMem => p.page_cpu_mem,
            Page::Storage => p.page_storage,
            Page::Networking => p.page_networking,
            Page::Finish => p.page_finish,
        }
    }

    fn title(self) -> &'static str {
        let p = prov_strings();
        match self {
            Page::Template => p.title_template,
            Page::Name => p.title_name,
            Page::Media => p.title_media,
            Page::Home => p.title_home,
            Page::CpuMem => p.title_cpu_mem,
            Page::Storage => p.title_storage,
            Page::Networking => p.title_networking,
            Page::Finish => p.title_finish,
        }
    }
}

/// What the installation media page offers for a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaOptions {
    /// The whole section is off (no media needed).
    pub disabled: bool,
    pub cd: bool,
    pub network: bool,
    /// A default template that installs from media: an ISO is required.
    pub iso_required: bool,
    /// The CD label: "Install from ISO library or DVD drive:" (default
    /// templates with install methods) or "DVD drive:".
    pub install_label: bool,
}

/// `Page_InstallationMedia`'s analysis of a template.
pub fn media_options(model: &ConnectionModel, template: &VmRecord) -> MediaOptions {
    let user = !template.other_config.contains_key("default_template");
    let hvm = prov::is_hvm(template);
    let methods = template
        .other_config
        .get("install-methods")
        .cloned()
        .unwrap_or_default();
    let has_cd_drive = template
        .vbds
        .iter()
        .filter_map(|b| model.get(b))
        .any(|b| b.r#type == xapi::VbdType::Cd);
    let order = template
        .hvm_boot_params
        .get("order")
        .cloned()
        .unwrap_or_default()
        .to_uppercase();
    if (user && !has_cd_drive) || (!user && methods.is_empty()) {
        return MediaOptions {
            disabled: true,
            ..Default::default()
        };
    }
    let cd = !(user && order.starts_with('N'))
        && (user || methods.is_empty() || (methods.contains("cdrom") && hvm));
    let network = hvm && ((user && order.starts_with('N')) || (!user && !methods.is_empty()));
    MediaOptions {
        disabled: false,
        cd,
        network,
        iso_required: !user && !methods.is_empty(),
        install_label: !user && !methods.is_empty(),
    }
}

/// The template list's category (`Page_Template`'s type column).
pub fn template_category(vm: &VmRecord) -> &'static str {
    let p = prov_strings();
    match objects::vm_kind(vm) {
        VmKind::Snapshot => return p.template_type_snapshots,
        VmKind::UserTemplate => return p.template_type_custom,
        _ => {}
    }
    let name = vm.name_label.to_lowercase();
    for (prefix, label) in [
        ("debian", p.template_type_debian),
        ("ubuntu", p.template_type_ubuntu),
        ("windows", p.template_type_windows),
        ("red hat", p.template_type_redhat),
        ("centos", p.template_type_centos),
        ("rocky", p.template_type_rocky),
        ("oracle", p.template_type_oracle),
        ("suse", p.template_type_suse),
        ("sles", p.template_type_suse),
    ] {
        if name.starts_with(prefix) {
            return label;
        }
    }
    p.template_type_misc
}

/// The templates (and snapshots) the wizard lists: hidden and
/// half-made (`__gui__`) ones left out, by category then name.
pub fn template_list(model: &ConnectionModel, filter: &str) -> Vec<(Ref<Vm>, Arc<VmRecord>)> {
    let filter = filter.trim().to_lowercase();
    let vtpm_ok = prov::vtpm_allowed(model);
    let mut out: Vec<(Ref<Vm>, Arc<VmRecord>)> = model
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && !v.is_control_domain)
        .filter(|(_, v)| objects::vm_is_shown(v, false))
        .filter(|(_, v)| {
            !v.name_label
                .starts_with(xen_core::provision_run::HIDDEN_PREFIX)
        })
        .filter(|(_, v)| vtpm_ok || !prov::template_wants_vtpm(v))
        .filter(|(_, v)| prov::can_create_from(v))
        .filter(|(_, v)| filter.is_empty() || v.name_label.to_lowercase().contains(&filter))
        .map(|(r, v)| (r.clone(), v.clone()))
        .collect();
    let rank = |v: &VmRecord| match objects::vm_kind(v) {
        VmKind::UserTemplate => 0,
        VmKind::DefaultTemplate => 1,
        _ => 2,
    };
    out.sort_by(|(ra, a), (rb, b)| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| template_category(a).cmp(template_category(b)))
            .then_with(|| objects::natural_cmp(&a.name_label, &b.name_label))
            .then_with(|| ra.cmp(rb))
    });
    out
}

/// `Helpers.DefaultVMName`: "{template} (1)", "(2)"… the first unused.
pub fn default_vm_name(model: &ConnectionModel, template: &str) -> String {
    let taken: std::collections::HashSet<&str> = prov::vm_names(model).collect();
    (1..)
        .map(|n| format!("{template} ({n})"))
        .find(|n| {
            !taken.contains(n.as_str())
                && !taken
                    .contains(format!("{}{n}", xen_core::provision_run::HIDDEN_PREFIX).as_str())
        })
        .unwrap_or_else(|| template.to_owned())
}

/// The installation media choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaChoice {
    Cd(Option<Ref<Vdi>>),
    Network,
}

/// The disk being added or edited on the Storage page.
struct DiskForm {
    index: Option<usize>,
    disk: NewVmDisk,
    size_gib: f64,
    min_size: i64,
    resizable: bool,
}

/// The interface being added on the Networking page.
struct VifForm {
    network: Option<Ref<Network>>,
    mac: String,
}

/// What became of the wizard this frame.
pub enum WizardOutcome {
    Open,
    Cancelled,
    Create(ConnectionId, Box<NewVmSpec>),
}

/// The wizard's state.
pub struct NewVmWizard {
    connection: ConnectionId,
    pub page: Page,
    pub template: Option<Ref<Vm>>,
    /// The template the later pages were filled from.
    applied: Option<Ref<Vm>>,
    pub search: String,
    pub name: String,
    pub description: String,
    pub media: MediaChoice,
    pub boot_mode: Option<BootMode>,
    pub vtpm: bool,
    pub affinity: Option<Ref<Host>>,
    pub vcpus_max: i64,
    pub vcpus_at_startup: i64,
    pub cores_per_socket: i64,
    pub memory_static_max_mib: f64,
    pub memory_dynamic_min_mib: f64,
    pub memory_dynamic_max_mib: f64,
    pub disks: Vec<NewVmDisk>,
    /// Template disks' sizes (the minimum of each row).
    min_sizes: Vec<i64>,
    pub diskless: bool,
    pub fast_clone: bool,
    pub vifs: Vec<NewVmVif>,
    pub start: bool,
    disk_form: Option<DiskForm>,
    vif_form: Option<VifForm>,
    /// The disk rows were made for this VM name (provisioned disks are
    /// named after the VM).
    disks_named_for: String,
}

impl NewVmWizard {
    /// A wizard for `conn`, starting on the template page (or with
    /// `template` chosen).
    pub fn new(
        conn: &ConnectionView,
        template: Option<Ref<Vm>>,
        affinity: Option<Ref<Host>>,
    ) -> Self {
        let mut wizard = Self {
            connection: conn.id,
            page: Page::Template,
            template: None,
            applied: None,
            search: String::new(),
            name: String::new(),
            description: String::new(),
            media: MediaChoice::Cd(None),
            boot_mode: None,
            vtpm: false,
            affinity: affinity.clone(),
            vcpus_max: 1,
            vcpus_at_startup: 1,
            cores_per_socket: 1,
            memory_static_max_mib: 1024.0,
            memory_dynamic_min_mib: 1024.0,
            memory_dynamic_max_mib: 1024.0,
            disks: Vec::new(),
            min_sizes: Vec::new(),
            diskless: false,
            fast_clone: true,
            vifs: Vec::new(),
            start: true,
            disk_form: None,
            vif_form: None,
            disks_named_for: String::new(),
        };
        if let Some(t) = template.filter(|t| conn.model.vms.contains_key(t)) {
            wizard.template = Some(t);
            wizard.apply_template(&conn.model, affinity);
        }
        wizard
    }

    fn template_record<'a>(&self, model: &'a ConnectionModel) -> Option<&'a Arc<VmRecord>> {
        self.template.as_ref().and_then(|t| model.vms.get(t))
    }

    /// Fills every later page from the chosen template.
    fn apply_template(&mut self, model: &ConnectionModel, affinity: Option<Ref<Host>>) {
        let Some(template) = self.template_record(model).cloned() else {
            return;
        };
        self.applied = self.template.clone();
        self.name = default_vm_name(model, &template.name_label);
        self.description = String::new();
        let options = media_options(model, &template);
        let template_iso = template
            .vbds
            .iter()
            .filter_map(|b| model.get(b))
            .find(|b| b.r#type == xapi::VbdType::Cd && !b.empty)
            .map(|b| b.vdi.clone());
        self.media = if options.cd || !options.network {
            MediaChoice::Cd(template_iso)
        } else {
            MediaChoice::Network
        };
        self.boot_mode = prov::default_boot_mode(&template);
        self.vtpm = prov::template_wants_vtpm(&template);
        self.affinity = affinity.or_else(|| {
            crate_storage_host(model, &template).filter(|h| model.hosts.contains_key(h))
        });
        self.vcpus_max = template.vcpus_max.max(1);
        self.vcpus_at_startup = template.vcpus_at_startup.clamp(1, self.vcpus_max);
        self.cores_per_socket = template
            .platform
            .get("cores-per-socket")
            .and_then(|c| c.parse().ok())
            .unwrap_or(1);
        self.memory_static_max_mib = template.memory_static_max as f64 / MIB;
        self.memory_dynamic_min_mib = template.memory_dynamic_min as f64 / MIB;
        self.memory_dynamic_max_mib = template.memory_dynamic_max as f64 / MIB;
        self.fast_clone = true;
        self.start = true;
        self.make_disks(model, &template);
        self.diskless = self.disks.is_empty() && self.media == MediaChoice::Network;
        self.vifs = default_vifs(model, &template);
    }

    /// The Storage page's rows (`Page_Storage.LoadDisks`).
    fn make_disks(&mut self, model: &ConnectionModel, template: &VmRecord) {
        let p = prov_strings();
        self.disks.clear();
        self.min_sizes.clear();
        let default_sr = model.pool().map(|(_, p)| p.default_sr.clone());
        let usable: Vec<Ref<Sr>> = prov::disk_srs(model, None)
            .into_iter()
            .map(|(r, _)| r.clone())
            .collect();
        let best = |suggested: Option<Ref<Sr>>| -> Ref<Sr> {
            suggested
                .filter(|s| usable.contains(s))
                .or_else(|| default_sr.clone().filter(|s| usable.contains(s)))
                .or_else(|| usable.first().cloned())
                .unwrap_or_else(Ref::null)
        };
        let provision = template
            .other_config
            .get("disks")
            .map(|x| prov::parse_provision(x))
            .unwrap_or_default();
        if !provision.is_empty() {
            for disk in provision {
                let sr = model
                    .srs
                    .iter()
                    .find(|(_, s)| !disk.sr.is_empty() && s.uuid == disk.sr)
                    .map(|(r, _)| r.clone());
                self.min_sizes.push(disk.size);
                self.disks.push(NewVmDisk {
                    source: DiskSource::Provision,
                    system: disk.kind == "system",
                    name: fill(
                        p.disk_default_name,
                        &[("vm", &self.name), ("n", &disk.device)],
                    ),
                    userdevice: disk.device,
                    description: p.provision_disk_description.to_owned(),
                    size: disk.size,
                    sr: best(sr),
                    bootable: disk.bootable,
                });
            }
        } else {
            for (_, vbd, vdi_ref, vdi) in prov::vm_disks(model, template) {
                self.min_sizes.push(vdi.virtual_size);
                self.disks.push(NewVmDisk {
                    source: DiskSource::Template(vdi_ref.clone()),
                    system: vdi.r#type == xapi::VdiType::System,
                    userdevice: vbd.userdevice.clone(),
                    name: vdi.name_label.clone(),
                    description: vdi.name_description.clone(),
                    size: vdi.virtual_size,
                    sr: best(Some(vdi.sr.clone())),
                    bootable: vbd.bootable,
                });
            }
        }
        self.disks_named_for = self.name.clone();
    }

    /// Provisioned disks follow a VM name changed after they were made.
    fn rename_disks(&mut self) {
        if self.disks_named_for == self.name {
            return;
        }
        let old = self.disks_named_for.clone();
        for disk in &mut self.disks {
            if disk.source == DiskSource::Provision
                && let Some(rest) = disk.name.strip_prefix(&old)
            {
                disk.name = format!("{}{rest}", self.name);
            }
        }
        self.disks_named_for = self.name.clone();
    }

    fn is_custom(&self, model: &ConnectionModel) -> bool {
        self.template_record(model)
            .is_some_and(|t| !t.other_config.contains_key("default_template"))
    }

    /// The SR a custom template is copied to when fast clone is off
    /// (`_fullCopySR`): the one SR every template disk goes to.
    fn full_copy_sr(&self, model: &ConnectionModel) -> Option<Ref<Sr>> {
        if !self.is_custom(model) || self.fast_clone {
            return None;
        }
        let targets: Vec<&Ref<Sr>> = self
            .disks
            .iter()
            .filter(|d| matches!(d.source, DiskSource::Template(_)))
            .map(|d| &d.sr)
            .collect();
        let first = targets.first()?;
        targets.iter().all(|t| t == first).then(|| (*first).clone())
    }

    /// Whether the page's answers let the wizard go on.
    pub fn page_ok(&self, model: &ConnectionModel, page: Page) -> bool {
        match page {
            Page::Template => self.template_record(model).is_some(),
            Page::Name => !self.name.trim().is_empty(),
            Page::Media => {
                let Some(t) = self.template_record(model) else {
                    return false;
                };
                let options = media_options(model, t);
                match &self.media {
                    _ if options.disabled => true,
                    MediaChoice::Cd(iso) => options.cd && (!options.iso_required || iso.is_some()),
                    MediaChoice::Network => options.network,
                }
            }
            Page::Home => true,
            Page::CpuMem => {
                self.vcpus_at_startup >= 1
                    && self.vcpus_at_startup <= self.vcpus_max
                    && self.memory_dynamic_min_mib <= self.memory_dynamic_max_mib
                    && self.memory_dynamic_max_mib <= self.memory_static_max_mib
                    && self.memory_dynamic_min_mib > 0.0
            }
            Page::Storage => {
                self.diskless
                    || self
                        .disks
                        .iter()
                        .all(|d| !d.sr.is_null() && model.srs.contains_key(&d.sr))
            }
            Page::Networking | Page::Finish => true,
        }
    }

    /// The spec the wizard's answers make.
    pub fn spec(&self, model: &ConnectionModel) -> Option<NewVmSpec> {
        let template = self.template_record(model)?;
        let options = media_options(model, template);
        let media = match &self.media {
            _ if options.disabled => InstallMedia::None,
            MediaChoice::Cd(Some(iso)) => InstallMedia::Iso(iso.clone()),
            MediaChoice::Cd(None) => InstallMedia::None,
            MediaChoice::Network => InstallMedia::Network,
        };
        // `AssignVtpm`: templates that want a vTPM bring their own.
        let vtpm = self.vtpm
            && self.boot_mode.is_some_and(|m| m != BootMode::Bios)
            && template.vtpms.is_empty()
            && !prov::template_wants_vtpm(template);
        Some(NewVmSpec {
            template: self.template.clone()?,
            name: self.name.trim().to_owned(),
            description: self.description.trim().to_owned(),
            media,
            affinity: self.affinity.clone(),
            vcpus_max: self.vcpus_max,
            vcpus_at_startup: self.vcpus_at_startup,
            cores_per_socket: Some(self.cores_per_socket),
            memory_static_max: (self.memory_static_max_mib * MIB) as i64,
            memory_dynamic_min: (self.memory_dynamic_min_mib * MIB) as i64,
            memory_dynamic_max: (self.memory_dynamic_max_mib * MIB) as i64,
            disks: if self.diskless {
                Vec::new()
            } else {
                self.disks.clone()
            },
            full_copy_sr: self.full_copy_sr(model),
            vifs: self.vifs.clone(),
            boot_mode: self.boot_mode,
            vtpm,
            start: self.start && self.can_start(model),
        })
    }

    /// `Page_CpuMem.CanStartVm`: some host has the CPUs and the memory.
    pub fn can_start(&self, model: &ConnectionModel) -> bool {
        let (cpus, total, _) = host_limits(model);
        cpus > 0 && self.vcpus_max <= cpus && (self.memory_dynamic_min_mib * MIB) as i64 <= total
    }

    /// Draws the wizard.
    pub fn show(&mut self, ctx: &Context, state: &AppState) -> WizardOutcome {
        let p = prov_strings();
        let Some(conn) = state.connection(self.connection) else {
            return WizardOutcome::Cancelled;
        };
        let model = conn.model.clone();
        let pool = model
            .pool()
            .map(|(_, p)| objects::pool_name(&model, p))
            .unwrap_or_else(|| conn.server.clone());
        let mut outcome = WizardOutcome::Open;
        let mut open = true;
        dialog_window(fill(p.wizard_title, &[("pool", &pool)]))
            .id(egui::Id::unique("new-vm-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([800.0, 500.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for page in PAGES {
                            let text = RichText::new(page.name());
                            ui.label(if page == self.page {
                                text.strong()
                            } else {
                                text.weak()
                            });
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.heading(self.page.title());
                        ui.add_space(6.0);
                        ui.set_min_width(600.0);
                        ScrollArea::vertical()
                            .id_salt("wizard-page")
                            .max_height(360.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                Page::Template => self.template_page(ui, &model),
                                Page::Name => self.name_page(ui),
                                Page::Media => self.media_page(ui, &model),
                                Page::Home => self.home_page(ui, &model),
                                Page::CpuMem => self.cpu_page(ui, &model),
                                Page::Storage => self.storage_page(ui, &model),
                                Page::Networking => self.network_page(ui, &model),
                                Page::Finish => self.finish_page(ui, &model),
                            });
                    });
                });
                ui.separator();
                dialog_buttons(ui, |row| {
                    let index = PAGES.iter().position(|x| *x == self.page).unwrap_or(0);
                    if row
                        .add_enabled(ButtonRole::Back, index > 0, Button::new(p.back))
                        .clicked()
                    {
                        self.page = PAGES[index - 1];
                    }
                    let ok = self.page_ok(&model, self.page);
                    if self.page == Page::Finish {
                        if row
                            .add_enabled(ButtonRole::Primary, ok, Button::new(p.create_now))
                            .clicked()
                            && let Some(spec) = self.spec(&model)
                        {
                            outcome = WizardOutcome::Create(self.connection, Box::new(spec));
                        }
                    } else if row
                        .add_enabled(ButtonRole::Primary, ok, Button::new(p.next))
                        .clicked()
                    {
                        if self.page == Page::Template && self.applied != self.template {
                            self.apply_template(&model, self.affinity.clone());
                        }
                        if self.page == Page::Name {
                            self.rename_disks();
                        }
                        self.page = PAGES[index + 1];
                    }
                    if row.button(ButtonRole::Cancel, p.cancel).clicked() {
                        outcome = WizardOutcome::Cancelled;
                    }
                });
            });
        self.disk_window(ctx, &model);
        self.vif_window(ctx, &model);
        if !open {
            return WizardOutcome::Cancelled;
        }
        outcome
    }

    fn template_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        ui.horizontal(|ui| {
            let label = ui.label(p.template_search);
            ui.add(TextEdit::singleline(&mut self.search).id_salt("template-search"))
                .labelled_by(label.id);
        });
        ui.add_space(4.0);
        Grid::new("templates")
            .striped(true)
            .num_columns(2)
            .show(ui, |ui| {
                for (r, t) in template_list(model, &self.search) {
                    if ui
                        .selectable_label(self.template.as_ref() == Some(&r), &t.name_label)
                        .clicked()
                    {
                        self.template = Some(r.clone());
                    }
                    ui.label(RichText::new(template_category(&t)).weak());
                    ui.end_row();
                }
            });
    }

    fn name_page(&mut self, ui: &mut Ui) {
        let p = prov_strings();
        ui.label(p.name_rubric);
        ui.add_space(6.0);
        Grid::new("vm-name").num_columns(2).show(ui, |ui| {
            ui.label(p.name_label);
            ui.add(
                TextEdit::singleline(&mut self.name)
                    .id_salt("vm-name")
                    .desired_width(380.0),
            );
            ui.end_row();
            ui.label(p.description_label);
            ui.add(
                TextEdit::multiline(&mut self.description)
                    .desired_rows(3)
                    .desired_width(380.0),
            );
            ui.end_row();
        });
    }

    fn media_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        let Some(template) = self.template_record(model).cloned() else {
            return;
        };
        let options = media_options(model, &template);
        if options.disabled {
            ui.label(p.media_none_needed);
        } else {
            let label = if options.install_label {
                p.media_iso
            } else {
                p.media_dvd
            };
            let is_cd = matches!(self.media, MediaChoice::Cd(_));
            if ui
                .add_enabled(options.cd, egui::RadioButton::new(is_cd, label))
                .clicked()
            {
                self.media = MediaChoice::Cd(None);
            }
            let groups = prov::iso_groups(model, None);
            ui.add_enabled_ui(options.cd && is_cd, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    let current = match &self.media {
                        MediaChoice::Cd(Some(v)) => model.get(v).map(|x| x.name_label.clone()),
                        _ => None,
                    }
                    .unwrap_or_else(|| p.empty_iso.to_owned());
                    ComboBox::from_id_salt("wizard-iso")
                        .selected_text(current)
                        .width(340.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, p.empty_iso).clicked() {
                                self.media = MediaChoice::Cd(None);
                            }
                            for group in &groups {
                                ui.label(RichText::new(&group.sr_name).strong());
                                for (iso, name) in &group.isos {
                                    let selected = self.media == MediaChoice::Cd(Some(iso.clone()));
                                    if ui
                                        .selectable_label(selected, format!("    {name}"))
                                        .clicked()
                                    {
                                        self.media = MediaChoice::Cd(Some(iso.clone()));
                                    }
                                }
                            }
                        });
                });
            });
            let is_net = self.media == MediaChoice::Network;
            if ui
                .add_enabled(
                    options.network,
                    egui::RadioButton::new(is_net, p.media_network),
                )
                .clicked()
            {
                self.media = MediaChoice::Network;
            }
        }
        let modes = prov::supported_boot_modes(&template);
        if !modes.is_empty() {
            ui.add_space(10.0);
            ui.strong(p.boot_mode);
            for (mode, label) in [
                (BootMode::Bios, p.boot_bios),
                (BootMode::Uefi, p.boot_uefi),
                (BootMode::UefiSecure, p.boot_secure),
            ] {
                ui.horizontal(|ui| {
                    let supported = modes.contains(&mode);
                    if ui
                        .add_enabled(
                            supported,
                            egui::RadioButton::new(self.boot_mode == Some(mode), label),
                        )
                        .clicked()
                    {
                        self.boot_mode = Some(mode);
                    }
                    if !supported {
                        ui.label(RichText::new(p.boot_unsupported).weak());
                    }
                });
            }
            if prov::vtpm_allowed(model) {
                let vtpm_template = prov::template_wants_vtpm(&template);
                let bios = self.boot_mode == Some(BootMode::Bios);
                if bios {
                    self.vtpm = false;
                }
                ui.add_enabled_ui(!bios && !vtpm_template && template.vtpms.is_empty(), |ui| {
                    ui.checkbox(&mut self.vtpm, p.vtpm_check);
                });
                if bios {
                    ui.label(RichText::new(p.vtpm_bios).weak());
                }
            }
        }
    }

    fn home_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        if ui.radio(self.affinity.is_none(), p.home_none).clicked() {
            self.affinity = None;
        }
        let hosts: Vec<(Ref<Host>, String, bool)> = objects::sorted_hosts(model)
            .into_iter()
            .map(|(r, h)| {
                (
                    r.clone(),
                    h.name_label.clone(),
                    h.enabled && objects::host_is_live(model, h),
                )
            })
            .collect();
        let first_usable = hosts.iter().find(|h| h.2).map(|h| h.0.clone());
        if ui.radio(self.affinity.is_some(), p.home_server).clicked() && self.affinity.is_none() {
            self.affinity = first_usable;
        }
        ui.add_enabled_ui(self.affinity.is_some(), |ui| {
            for (r, name, usable) in &hosts {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    let selected = self.affinity.as_ref() == Some(r);
                    if ui
                        .add_enabled(*usable, Button::selectable(selected, name.as_str()))
                        .clicked()
                    {
                        self.affinity = Some(r.clone());
                    }
                });
            }
        });
    }

    fn cpu_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        let Some(template) = self.template_record(model).cloned() else {
            return;
        };
        let (max_vcpus, max_memory) = prov::template_limits(&template);
        ui.label(p.cpu_rubric);
        ui.add_space(6.0);
        let ballooning = template.memory_dynamic_min != template.memory_static_max;
        let max_mib = (max_memory.max(template.memory_static_max)) as f64 / MIB;
        let min_mib = (template.memory_static_min as f64 / MIB).max(1.0);
        Grid::new("cpu-mem")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(p.vcpus_max);
                ui.add(DragValue::new(&mut self.vcpus_max).range(1..=max_vcpus));
                ui.end_row();
                ui.label(p.vcpus_initial);
                ui.add(DragValue::new(&mut self.vcpus_at_startup).range(1..=self.vcpus_max));
                ui.end_row();
                ui.label(p.topology);
                let options = topologies(self.vcpus_max);
                if !options.contains(&self.cores_per_socket) {
                    self.cores_per_socket = *options.last().unwrap_or(&1);
                }
                ComboBox::from_id_salt("topology")
                    .selected_text(topology_text(self.vcpus_max, self.cores_per_socket))
                    .show_ui(ui, |ui| {
                        for cores in options {
                            ui.selectable_value(
                                &mut self.cores_per_socket,
                                cores,
                                topology_text(self.vcpus_max, cores),
                            );
                        }
                    });
                ui.end_row();
                if ballooning {
                    ui.label(p.memory_static_max);
                    ui.add(
                        DragValue::new(&mut self.memory_static_max_mib)
                            .range(min_mib..=max_mib)
                            .suffix(" MB")
                            .speed(64.0),
                    );
                    ui.end_row();
                    ui.label(p.memory_dynamic_min);
                    ui.add(
                        DragValue::new(&mut self.memory_dynamic_min_mib)
                            .range(min_mib..=self.memory_dynamic_max_mib)
                            .suffix(" MB")
                            .speed(64.0),
                    );
                    ui.end_row();
                    ui.label(p.memory_dynamic_max);
                    ui.add(
                        DragValue::new(&mut self.memory_dynamic_max_mib)
                            .range(self.memory_dynamic_min_mib..=self.memory_static_max_mib)
                            .suffix(" MB")
                            .speed(64.0),
                    );
                    ui.end_row();
                } else {
                    ui.label(p.memory);
                    ui.add(
                        DragValue::new(&mut self.memory_static_max_mib)
                            .range(min_mib..=max_mib)
                            .suffix(" MB")
                            .speed(64.0),
                    );
                    self.memory_dynamic_min_mib = self.memory_static_max_mib;
                    self.memory_dynamic_max_mib = self.memory_static_max_mib;
                    ui.end_row();
                }
            });
        self.vcpus_at_startup = self.vcpus_at_startup.min(self.vcpus_max);
        let (cpus, total, _) = host_limits(model);
        if cpus > 0 && self.vcpus_max > cpus {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                fill(
                    p.vcpus_warn,
                    &[
                        ("n", &self.vcpus_max.to_string()),
                        ("max", &cpus.to_string()),
                    ],
                ),
            );
        }
        if total > 0 && (self.memory_dynamic_min_mib * MIB) as i64 > total {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                fill(p.memory_warn, &[("size", &memory_size(total, false))]),
            );
        }
    }

    fn storage_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        let Some(template) = self.template_record(model).cloned() else {
            return;
        };
        let can_diskless = prov::is_hvm(&template) && self.media == MediaChoice::Network;
        if ui.radio(!self.diskless, p.storage_disks).clicked() {
            self.diskless = false;
        }
        let custom = self.is_custom(model);
        ui.add_enabled_ui(!self.diskless, |ui| {
            Grid::new("wizard-disks")
                .striped(true)
                .num_columns(4)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in [p.col_position, p.col_location, p.col_size, p.col_name] {
                        ui.strong(h);
                    }
                    ui.strong("");
                    ui.end_row();
                    let mut edit = None;
                    let mut delete = None;
                    for (i, disk) in self.disks.iter().enumerate() {
                        ui.label(&disk.userdevice);
                        let sr = model.srs.get(&disk.sr);
                        match sr {
                            Some(sr) => ui.label(&sr.name_label),
                            None => ui.colored_label(ui.visuals().error_fg_color, p.no_storage),
                        };
                        ui.label(disk_size(disk.size));
                        ui.label(&disk.name);
                        ui.horizontal(|ui| {
                            if ui.small_button(p.edit).clicked() {
                                edit = Some(i);
                            }
                            let deletable = disk.source == DiskSource::New
                                || (disk.source == DiskSource::Provision && !disk.system);
                            if ui
                                .add_enabled(deletable, Button::new(p.delete).small())
                                .clicked()
                            {
                                delete = Some(i);
                            }
                        });
                        ui.end_row();
                    }
                    if let Some(i) = edit {
                        let disk = self.disks[i].clone();
                        self.disk_form = Some(DiskForm {
                            index: Some(i),
                            size_gib: disk.size as f64 / GIB,
                            min_size: self.min_sizes.get(i).copied().unwrap_or(1 << 20),
                            resizable: !matches!(disk.source, DiskSource::Template(_)),
                            disk,
                        });
                    }
                    if let Some(i) = delete {
                        self.disks.remove(i);
                        if i < self.min_sizes.len() {
                            self.min_sizes.remove(i);
                        }
                    }
                });
            if ui.button(p.add).clicked() {
                let names: Vec<&str> = model.vdis.values().map(|v| v.name_label.as_str()).collect();
                let used: Vec<i64> = self
                    .disks
                    .iter()
                    .filter_map(|d| d.userdevice.parse().ok())
                    .collect();
                let position = (0..).find(|n| !used.contains(n) && *n != 3).unwrap_or(0);
                let sr = self
                    .disks
                    .first()
                    .map(|d| d.sr.clone())
                    .or_else(|| model.pool().map(|(_, p)| p.default_sr.clone()))
                    .unwrap_or_else(Ref::null);
                self.disk_form = Some(DiskForm {
                    index: None,
                    size_gib: 1.0,
                    min_size: 1 << 20,
                    resizable: true,
                    disk: NewVmDisk {
                        source: DiskSource::New,
                        system: false,
                        userdevice: position.to_string(),
                        name: prov::unique_name(p.new_disk_name, names),
                        description: String::new(),
                        size: 1 << 30,
                        sr,
                        bootable: false,
                    },
                });
            }
            if custom {
                ui.checkbox(&mut self.fast_clone, p.fast_clone_check);
            }
        });
        if ui
            .add_enabled(
                can_diskless,
                egui::RadioButton::new(self.diskless, p.storage_diskless),
            )
            .clicked()
        {
            self.diskless = true;
        }
        // Overcommitted SRs (a warning only).
        let mut per_sr: std::collections::BTreeMap<&Ref<Sr>, i64> = Default::default();
        for d in &self.disks {
            *per_sr.entry(&d.sr).or_default() += d.size;
        }
        for (sr, needed) in per_sr {
            if let Some(rec) = model.srs.get(sr)
                && prov::sr_free(rec) < needed
            {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    fill(
                        p.sr_overcommit,
                        &[
                            ("sr", &rec.name_label),
                            ("free", &disk_size(prov::sr_free(rec))),
                            ("needed", &disk_size(needed)),
                        ],
                    ),
                );
            }
        }
    }

    fn disk_window(&mut self, ctx: &Context, model: &ConnectionModel) {
        let p = prov_strings();
        let Some(form) = self.disk_form.as_mut() else {
            return;
        };
        let srs = prov::disk_srs(model, None);
        let mut close = false;
        let mut save = false;
        let title = if form.index.is_some() {
            p.edit_disk_title
        } else {
            p.add_disk_title
        };
        dialog_window(title)
            .id(egui::Id::unique("wizard-disk"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                Grid::new("wizard-disk-form").num_columns(2).show(ui, |ui| {
                    let label = ui.label(p.name_label);
                    ui.add(TextEdit::singleline(&mut form.disk.name).desired_width(300.0))
                        .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label(p.description_label);
                    ui.add(TextEdit::singleline(&mut form.disk.description).desired_width(300.0))
                        .labelled_by(label.id);
                    ui.end_row();
                    ui.label(p.size_label);
                    let min = form.min_size as f64 / GIB;
                    ui.add_enabled_ui(form.resizable, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                DragValue::new(&mut form.size_gib)
                                    .range(min..=1_048_576.0)
                                    .speed(0.25)
                                    .max_decimals(3),
                            );
                            ui.label(p.gib);
                        });
                    });
                    ui.end_row();
                    ui.label(p.location_label);
                    let mut sr = (!form.disk.sr.is_null()).then(|| form.disk.sr.clone());
                    let text = sr
                        .as_ref()
                        .and_then(|r| model.srs.get(r))
                        .map(|s| s.name_label.clone())
                        .unwrap_or_else(|| p.no_storage.to_owned());
                    ComboBox::from_id_salt("wizard-disk-sr")
                        .selected_text(text)
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for (r, s) in &srs {
                                let label = format!(
                                    "{} ({})",
                                    s.name_label,
                                    fill(
                                        p.sr_free,
                                        &[
                                            ("free", &disk_size(prov::sr_free(s))),
                                            ("total", &disk_size(s.physical_size))
                                        ]
                                    )
                                );
                                if ui
                                    .selectable_label(sr.as_ref() == Some(*r), label)
                                    .clicked()
                                {
                                    sr = Some((*r).clone());
                                }
                            }
                        });
                    if let Some(sr) = sr {
                        form.disk.sr = sr;
                    }
                    ui.end_row();
                });
                ui.add_space(8.0);
                dialog_buttons(ui, |row| {
                    let ok = !form.disk.name.trim().is_empty() && !form.disk.sr.is_null();
                    if row
                        .add_enabled(ButtonRole::Primary, ok, Button::new(p.ok))
                        .clicked()
                    {
                        save = true;
                        close = true;
                    }
                    if row.button(ButtonRole::Cancel, p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        if save {
            let form = self.disk_form.as_ref().expect("form");
            let mut disk = form.disk.clone();
            if form.resizable {
                disk.size = ((form.size_gib * GIB) as i64).max(form.min_size);
            }
            match form.index {
                Some(i) => self.disks[i] = disk,
                None => {
                    self.min_sizes.push(1 << 20);
                    self.disks.push(disk);
                }
            }
        }
        if close {
            self.disk_form = None;
        }
    }

    fn network_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        ui.label(p.networks_on);
        ui.add_space(4.0);
        let mut delete = None;
        Grid::new("wizard-vifs")
            .striped(true)
            .num_columns(3)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.strong(p.col_mac);
                ui.strong(p.col_network);
                ui.strong("");
                ui.end_row();
                for (i, vif) in self.vifs.iter().enumerate() {
                    ui.label(if vif.mac.is_empty() {
                        p.autogen_mac
                    } else {
                        &vif.mac
                    });
                    ui.label(
                        model
                            .networks
                            .get(&vif.network)
                            .map(|n| objects::network_name(model, n))
                            .unwrap_or_default(),
                    );
                    if ui.small_button(p.remove_interface).clicked() {
                        delete = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = delete {
            self.vifs.remove(i);
        }
        let max = self
            .template_record(model)
            .and_then(|t| prov::recommendation(t, "number-of-vifs"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(7usize);
        if ui
            .add_enabled(self.vifs.len() < max, Button::new(p.add_interface))
            .clicked()
        {
            self.vif_form = Some(VifForm {
                network: objects::shown_networks(model, false)
                    .first()
                    .map(|(r, _)| (*r).clone()),
                mac: String::new(),
            });
        }
    }

    fn vif_window(&mut self, ctx: &Context, model: &ConnectionModel) {
        let p = prov_strings();
        let Some(form) = self.vif_form.as_mut() else {
            return;
        };
        let networks = objects::shown_networks(model, false);
        let mut close = false;
        let mut save = false;
        dialog_window(p.add_interface)
            .id(egui::Id::unique("wizard-vif"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let text = form
                    .network
                    .as_ref()
                    .and_then(|r| model.networks.get(r))
                    .map(|n| objects::network_name(model, n))
                    .unwrap_or_default();
                ComboBox::from_id_salt("wizard-vif-net")
                    .selected_text(text)
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for (r, n) in &networks {
                            if ui
                                .selectable_label(
                                    form.network.as_ref() == Some(*r),
                                    objects::network_name(model, n),
                                )
                                .clicked()
                            {
                                form.network = Some((*r).clone());
                            }
                        }
                    });
                let label = ui.label(p.mac_label);
                ui.add(TextEdit::singleline(&mut form.mac).desired_width(200.0))
                    .labelled_by(label.id);
                let mac_ok = form.mac.trim().is_empty() || valid_mac(form.mac.trim());
                if !mac_ok {
                    ui.colored_label(ui.visuals().error_fg_color, p.invalid_mac);
                }
                dialog_buttons(ui, |row| {
                    if row
                        .add_enabled(
                            ButtonRole::Primary,
                            mac_ok && form.network.is_some(),
                            Button::new(p.ok),
                        )
                        .clicked()
                    {
                        save = true;
                        close = true;
                    }
                    if row.button(ButtonRole::Cancel, p.cancel).clicked() {
                        close = true;
                    }
                });
            });
        if save
            && let Some(form) = &self.vif_form
            && let Some(network) = &form.network
        {
            self.vifs.push(NewVmVif {
                network: network.clone(),
                mac: form.mac.trim().to_lowercase(),
            });
        }
        if close {
            self.vif_form = None;
        }
    }

    fn finish_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let p = prov_strings();
        ui.label(p.finish_blurb);
        ui.add_space(6.0);
        let Some(template) = self.template_record(model).cloned() else {
            return;
        };
        Grid::new("wizard-summary")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).strong());
                    ui.label(v);
                    ui.end_row();
                };
                row(p.summary_template, template.name_label.clone());
                row(p.summary_name, self.name.clone());
                row(
                    p.summary_media,
                    match &self.media {
                        MediaChoice::Cd(Some(v)) => format!(
                            "{}: {}",
                            p.media_kind_cd,
                            model
                                .get(v)
                                .map(|x| x.name_label.clone())
                                .unwrap_or_default()
                        ),
                        MediaChoice::Cd(None) => p.home_none_short.to_owned(),
                        MediaChoice::Network => p.media_kind_network.to_owned(),
                    },
                );
                if let Some(mode) = self.boot_mode {
                    row(
                        p.summary_boot,
                        match mode {
                            BootMode::Bios => p.boot_bios,
                            BootMode::Uefi => p.boot_uefi,
                            BootMode::UefiSecure => p.boot_secure,
                        }
                        .to_owned(),
                    );
                }
                row(
                    p.summary_home,
                    self.affinity
                        .as_ref()
                        .and_then(|h| model.hosts.get(h))
                        .map(|h| h.name_label.clone())
                        .unwrap_or_else(|| p.home_none_short.to_owned()),
                );
                row(p.summary_vcpus, self.vcpus_max.to_string());
                row(
                    p.summary_memory,
                    memory_size((self.memory_static_max_mib * MIB) as i64, false),
                );
                row(
                    p.summary_disks,
                    if self.diskless {
                        p.home_none_short.to_owned()
                    } else {
                        self.disks
                            .iter()
                            .map(|d| {
                                format!(
                                    "{} ({})",
                                    disk_size(d.size),
                                    model
                                        .srs
                                        .get(&d.sr)
                                        .map(|s| s.name_label.as_str())
                                        .unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                );
                row(
                    p.summary_networks,
                    self.vifs
                        .iter()
                        .filter_map(|v| model.networks.get(&v.network))
                        .map(|n| objects::network_name(model, n))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            });
        ui.add_space(8.0);
        let can_start = self.can_start(model);
        if !can_start {
            self.start = false;
        }
        ui.add_enabled_ui(can_start, |ui| {
            ui.checkbox(&mut self.start, p.start_automatically)
        });
    }
}

/// `VM.GetStorageHost`, for the suggested home server.
fn crate_storage_host(model: &ConnectionModel, template: &VmRecord) -> Option<Ref<Host>> {
    xen_core::actions::storage_host(model, template).cloned()
}

/// The interfaces a new VM starts with (`Page_Networking`): a custom
/// template's own (MACs regenerated), else one per shown network that
/// plugs automatically, at most four.
fn default_vifs(model: &ConnectionModel, template: &VmRecord) -> Vec<NewVmVif> {
    let custom = !template.other_config.contains_key("default_template");
    if custom {
        let mut vifs: Vec<_> = template.vifs.iter().filter_map(|v| model.get(v)).collect();
        vifs.sort_by(|a, b| objects::natural_cmp(&a.device, &b.device));
        return vifs
            .into_iter()
            .map(|v| NewVmVif {
                network: v.network.clone(),
                mac: String::new(),
            })
            .collect();
    }
    objects::shown_networks(model, false)
        .into_iter()
        .filter(|(_, n)| n.other_config.get("automatic").map(String::as_str) != Some("false"))
        .filter(|(_, n)| !objects::is_guest_installer_network(n))
        .take(MAX_DEFAULT_NETWORKS)
        .map(|(r, _)| NewVmVif {
            network: r.clone(),
            mac: String::new(),
        })
        .collect()
}

/// The largest host: (physical CPUs, total memory, free memory).
fn host_limits(model: &ConnectionModel) -> (i64, i64, i64) {
    let mut out = (0, 0, 0);
    for host in model.hosts.values() {
        let cpus = host
            .cpu_info
            .get("cpu_count")
            .and_then(|c| c.parse().ok())
            .unwrap_or_else(|| host.host_cpus.len() as i64);
        out.0 = out.0.max(cpus);
        if let Some(mem) = objects::host_memory(model, host) {
            out.1 = out.1.max(mem.total);
            out.2 = out.2.max(mem.free);
        }
    }
    out
}

/// Cores-per-socket choices for `vcpus` (`CPUTopologyComboBox`): divisors
/// leaving at most 16 sockets.
pub fn topologies(vcpus: i64) -> Vec<i64> {
    let v: Vec<i64> = (1..=vcpus.max(1))
        .filter(|c| vcpus % c == 0 && vcpus / c <= 16)
        .collect();
    if v.is_empty() { vec![vcpus.max(1)] } else { v }
}

fn topology_text(vcpus: i64, cores: i64) -> String {
    fill(
        prov_strings().topology_text,
        &[
            ("sockets", &(vcpus / cores.max(1)).to_string()),
            ("cores", &cores.to_string()),
        ],
    )
}

/// `aa:bb:cc:dd:ee:ff` (or with dashes).
pub fn valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split([':', '-']).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    #[test]
    fn templates_names_media_and_disks() {
        let m = model();
        let list = template_list(&m, "");
        let names: Vec<&str> = list.iter().map(|(_, t)| t.name_label.as_str()).collect();
        assert_eq!(
            names,
            [
                "Web server template",
                "Debian Bookworm 12",
                "web-01 before upgrade"
            ]
        );
        assert_eq!(template_list(&m, "debian").len(), 1);
        assert_eq!(
            default_vm_name(&m, "Debian Bookworm 12"),
            "Debian Bookworm 12 (1)"
        );
        let debian = &m.vms[&Ref::new("OpaqueRef:vm-template-debian")];
        let o = media_options(&m, debian);
        assert!(o.cd && o.network && o.iso_required && !o.disabled);
        let user = &m.vms[&Ref::new("OpaqueRef:vm-template-user")];
        assert!(media_options(&m, user).disabled);
        assert_eq!(template_category(debian), "Debian");
        assert_eq!(topologies(4), vec![1, 2, 4]);
        assert_eq!(topologies(32), vec![2, 4, 8, 16, 32]);
        assert!(valid_mac("02:12:34:56:78:9a"));
        assert!(!valid_mac("02:12:34:56:78"));
    }
}
