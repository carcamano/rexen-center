//! OVF/OVA and disk image wizards in the window (F11b), as the original's
//! `ExportApplianceWizard` (OVF/OVA) and `ImportWizard` (OVF and VHD
//! types):
//!
//! * the **Export wizard**'s OVF/OVA format ([`crate::xva_ui::ExportWizard`]
//!   with these pages): several VMs, the EULAs page (text files added by
//!   path, removed, viewed; at most 25, none empty), Advanced Options
//!   (create a manifest, sign — not available, said so —, create an OVA,
//!   compress) and Finish with its summary and "Verify export on
//!   completion";
//! * the **Import wizard** once its source is an OVF/OVA package or a disk
//!   image ([`OvfImportWizard`]):
//!   - a package: EULAs (when it has any; accepted to go on), Location (the
//!     pool or server), the RBAC page for a restricted user, Storage (every
//!     disk on one SR, or each on its own), Networking (each interface's
//!     network), Security (when the package has a manifest or a signature:
//!     verify it) and Finish (summary, start the VMs after import);
//!   - a disk image: VM Definition (name, CPUs, memory), Location, RBAC,
//!     Storage, Networking, Boot options (BIOS, UEFI, UEFI Secure Boot) and
//!     Finish.
//!
//!   VMDK, VHDX, qcow2 and WIM disks are refused on the source page (no
//!   converter, plan decision 12). The OS Fixup page is left out: its ISO
//!   is not shipped. Finish starts the import, followed in the Events
//!   panel with its progress and Cancel, and closes the wizard, as the
//!   original.

use std::path::Path;

use egui::{Button, ComboBox, Context, DragValue, Grid, RichText, ScrollArea, TextEdit, Ui};
use xapi::{Host, Network, Ref, Sr};
use xen_core::disk_image::Image;
use xen_core::ovf::{self, Envelope, Finding};
use xen_core::ovf_package::{self, Package};
use xen_core::ovf_rules::{self, DiskRow, ImageBoot, NicRow};
use xen_core::ovf_run::{OvfExport, OvfImport, VmMapping};
use xen_core::rbac::{self, RbacCheck};
use xen_core::xva as rules;
use xen_core::{ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionView};

use crate::branding;
use crate::i18n::{self, fill};
use crate::ops_ui::OpsUi;
use crate::ovf_strings::ovf_strings;
use crate::pool_ui::connection_name;
use crate::xva_strings::xva_strings;
use crate::xva_ui::{ExportWizard, error_label, nav, page_list, summary, target_picker, vm_name};

/// `ExportApplianceAction`'s transfers.
pub(crate) const EXPORT_OVF_RBAC: &[&str] = &["task.create", "http/get_export_raw_vdi"];
/// `ApplianceAction.StaticRBACDependencies` and the upload.
pub(crate) const IMPORT_OVF_RBAC: &[&str] = &[
    "vm.add_to_other_config",
    "vm.create",
    "vm.destroy",
    "vm.hard_shutdown",
    "vm.remove_from_other_config",
    "vm.set_hvm_boot_params",
    "vm.start",
    "vdi.create",
    "vdi.destroy",
    "vbd.create",
    "vbd.eject",
    "vif.create",
    "http/put_import_raw_vdi",
];
/// The most EULAs a package carries.
const MAX_EULAS: usize = 25;

fn yes_no(b: bool) -> String {
    let o = ovf_strings();
    if b { o.yes } else { o.no }.to_owned()
}

// ---- the Export wizard's OVF/OVA pages -----------------------------------------------------

impl ExportWizard {
    /// `ExportEulaPage`.
    pub(crate) fn eula_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_export_eula_title);
        ui.label(o.export_eula_intro);
        ui.add_space(6.0);
        ui.label(o.eula_files);
        ScrollArea::vertical()
            .id_salt("export-eulas")
            .max_height(110.0)
            .show(ui, |ui| {
                for (i, e) in self.eulas.iter().enumerate() {
                    if ui
                        .selectable_label(self.eula_selected == Some(i), e)
                        .clicked()
                    {
                        self.eula_selected = Some(i);
                    }
                }
            });
        let full = self.eulas.len() >= MAX_EULAS;
        ui.horizontal(|ui| {
            let l = ui.label(o.eula_path_hint);
            ui.add(TextEdit::singleline(&mut self.eula_input).desired_width(300.0))
                .labelled_by(l.id);
            let add = ui
                .add_enabled(
                    !full && !self.eula_input.trim().is_empty(),
                    Button::new(o.eula_add),
                )
                .on_disabled_hover_text(fill(o.eula_max, &[("max", &MAX_EULAS.to_string())]));
            if add.clicked() {
                let path = self.eula_input.trim().to_owned();
                match std::fs::read_to_string(&path) {
                    Ok(t) if !t.trim().is_empty() => {
                        if !self.eulas.contains(&path) {
                            self.eulas.push(path);
                        }
                        self.eula_input.clear();
                        self.error = None;
                    }
                    _ => self.error = Some(fill(o.eula_invalid, &[("file", &path)])),
                }
            }
        });
        ui.horizontal(|ui| {
            let selected = self.eula_selected.filter(|i| *i < self.eulas.len());
            if ui
                .add_enabled(selected.is_some(), Button::new(o.eula_remove))
                .clicked()
                && let Some(i) = selected
            {
                self.eulas.remove(i);
                self.eula_selected = None;
                self.eula_view = None;
            }
            if ui
                .add_enabled(selected.is_some(), Button::new(o.eula_view))
                .clicked()
                && let Some(i) = selected
            {
                self.eula_view = std::fs::read_to_string(&self.eulas[i]).ok();
            }
        });
        if let Some(text) = &self.eula_view {
            ScrollArea::vertical()
                .id_salt("export-eula-view")
                .max_height(120.0)
                .show(ui, |ui| ui.label(text.as_str()));
        }
    }

    /// `ExportOptionsPage`.
    pub(crate) fn options_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_options_title);
        ui.add_space(6.0);
        ui.label(RichText::new(o.group_manifest).strong());
        ui.checkbox(&mut self.manifest, o.create_manifest);
        let mut sign = false;
        ui.add_enabled(false, egui::Checkbox::new(&mut sign, o.sign_package));
        ui.label(RichText::new(o.sign_unavailable).weak());
        ui.add_space(8.0);
        ui.label(RichText::new(o.group_output).strong());
        ui.checkbox(&mut self.ova, o.create_ova);
        ui.checkbox(&mut self.compress, o.compress);
        if self.compress {
            ui.label(if self.ova {
                o.compress_ova_hint
            } else {
                o.compress_ovf_hint
            });
        }
    }

    /// `ExportFinishPage` for an OVF/OVA package.
    pub(crate) fn ovf_finish_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let x = xva_strings();
        let o = ovf_strings();
        ui.heading(x.page_finish_export_title);
        ui.label(o.finish_export_ovf_intro);
        ui.add_space(6.0);
        let mut rows = vec![
            (x.summary_package.to_owned(), self.name.clone()),
            (x.summary_destination.to_owned(), self.folder.clone()),
        ];
        for (i, r) in self.ticked.iter().enumerate() {
            rows.push((
                if i == 0 {
                    x.summary_vms.to_owned()
                } else {
                    String::new()
                },
                vm_name(conn, r),
            ));
        }
        rows.push((o.summary_eulas.to_owned(), self.eulas.join("\n")));
        rows.push((o.summary_manifest.to_owned(), yes_no(self.manifest)));
        rows.push((o.summary_signed.to_owned(), yes_no(false)));
        rows.push((o.summary_ova.to_owned(), yes_no(self.ova)));
        rows.push((o.summary_compressed.to_owned(), yes_no(self.compress)));
        summary(ui, "export-ovf-summary", &rows);
        ui.add_space(8.0);
        ui.checkbox(&mut self.verify, x.verify);
    }

    /// The export the wizard asks for (`None`, with the error shown, when an
    /// EULA file cannot be read any more).
    pub(crate) fn ovf_request(&mut self) -> Option<ActionRequest> {
        let mut texts = Vec::new();
        for path in &self.eulas {
            match std::fs::read_to_string(path) {
                Ok(t) if !t.trim().is_empty() => texts.push(t),
                _ => {
                    self.error = Some(fill(ovf_strings().eula_invalid, &[("file", path)]));
                    return None;
                }
            }
        }
        Some(ActionRequest::ExportOvf(Box::new(OvfExport {
            vms: self.ticked.clone(),
            folder: self.folder.clone(),
            name: self.name.clone(),
            ova: self.ova,
            compress: self.compress,
            manifest: self.manifest,
            eulas: texts,
            verify: self.verify,
        })))
    }
}

// ---- the Import wizard on a package or an image ---------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OvfPage {
    VmDefinition,
    Eula,
    Location,
    Rbac,
    Storage,
    Network,
    Security,
    Boot,
    Finish,
}

/// What is imported.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    Package(Box<Package>),
    /// A disk image: its format, the disk's size and the file's size.
    Image {
        capacity: u64,
        file_size: u64,
    },
}

/// What the window does with the wizard after a frame.
pub enum Shown {
    Open,
    Closed,
    /// Previous on the first page: back to the source page.
    Back {
        path: String,
        target: Option<(ConnectionId, Option<Ref<Host>>)>,
    },
}

/// The Import wizard's OVF/OVA and disk image pages.
pub struct OvfImportWizard {
    pub(crate) source: Source,
    pub(crate) path: String,
    pub(crate) page: OvfPage,
    /// The validation warnings of the package (shown on the first page).
    pub(crate) warnings: Vec<String>,
    pub(crate) eula_accepted: bool,
    // The disk image's VM.
    pub(crate) vm_name: String,
    pub(crate) cpus: i64,
    pub(crate) memory_mb: i64,
    pub(crate) boot: ImageBoot,
    pub(crate) target: Option<(ConnectionId, Option<Ref<Host>>)>,
    rbac_needed: bool,
    rbac_blocked: Option<String>,
    rbac_affinity: Option<String>,
    /// The virtual systems: (id, VM name).
    pub(crate) systems: Vec<(String, String)>,
    pub(crate) all_on_one: bool,
    pub(crate) sr: Option<Ref<Sr>>,
    pub(crate) disks: Vec<(DiskRow, Option<Ref<Sr>>)>,
    pub(crate) nics: Vec<(NicRow, Option<Ref<Network>>)>,
    /// The connection the rows were made for.
    rows_for: Option<ConnectionId>,
    pub(crate) verify: bool,
    certificate: Option<Result<String, String>>,
    pub(crate) start: bool,
    pub(crate) error: Option<String>,
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn finding_text(f: &Finding) -> String {
    let o = ovf_strings();
    match f {
        Finding::NoDescriptor => o.error_no_ovf.to_owned(),
        Finding::MissingFile(n) => fill(o.error_file_missing_in_package, &[("file", n)]),
        Finding::NoVersion => o.warn_no_version.to_owned(),
        Finding::UnsupportedVersion(v) => fill(o.warn_version, &[("version", v)]),
        Finding::UnsupportedExtension(f) => fill(o.warn_extension, &[("file", f)]),
        Finding::UnexpectedHardware(t) => fill(o.warn_hardware, &[("type", t)]),
        Finding::CpuInvalid => o.warn_cpu.to_owned(),
        Finding::CpuLimit(q, l) => fill(
            o.warn_cpu_limit,
            &[("count", &q.to_string()), ("limit", &l.to_string())],
        ),
        Finding::MemoryNoInstanceId => o.warn_memory_id.to_owned(),
        Finding::MemoryInvalid => o.warn_memory.to_owned(),
        Finding::MemoryUnits => o.warn_memory_units.to_owned(),
        Finding::NetworkNotConnected => o.warn_network.to_owned(),
        Finding::UnknownRequired(id, name) => fill(o.warn_required, &[("id", id), ("name", name)]),
        Finding::NoDiskLink(n) => fill(o.warn_disk_link, &[("name", n)]),
    }
}

impl OvfImportWizard {
    fn new(source: Source, path: &str, target: Option<(ConnectionId, Option<Ref<Host>>)>) -> Self {
        Self {
            source,
            path: path.to_owned(),
            page: OvfPage::Location,
            warnings: Vec::new(),
            eula_accepted: false,
            vm_name: String::new(),
            cpus: 1,
            memory_mb: ovf_rules::IMAGE_MEMORY_MIN_MB,
            boot: ImageBoot::Bios,
            target,
            rbac_needed: false,
            rbac_blocked: None,
            rbac_affinity: None,
            systems: Vec::new(),
            all_on_one: true,
            sr: None,
            disks: Vec::new(),
            nics: Vec::new(),
            rows_for: None,
            verify: false,
            certificate: None,
            start: true,
            error: None,
        }
    }

    /// `LoadAppliance`: opens and checks the package at `path`; the error
    /// to show on the source page when it cannot be imported.
    pub fn package(
        path: &str,
        target: Option<(ConnectionId, Option<Ref<Host>>)>,
    ) -> Result<Self, String> {
        let o = ovf_strings();
        let console = branding::PRODUCT_NAME;
        let package = Package::open(Path::new(path)).map_err(|e| {
            tracing::warn!(error = %e, "the package cannot be read");
            o.error_no_ovf.to_owned()
        })?;
        if package.envelope.encrypted {
            return Err(fill(o.error_encrypted, &[("console", console)]));
        }
        let findings = ovf::validate(&package.envelope, &|f| package.has_file(f));
        if let Some(hard) = findings.iter().rev().find(|f| f.is_error()) {
            return Err(finding_text(hard));
        }
        if package.envelope.systems.is_empty() {
            return Err(o.error_no_ovf.to_owned());
        }
        if let Some((file, format)) = ovf_rules::unconvertible_disk(&package) {
            return Err(fill(
                o.error_vmdk_in_package,
                &[("file", &file), ("format", format), ("console", console)],
            ));
        }
        let stem = Path::new(path)
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let env = &package.envelope;
        let systems = env
            .systems
            .iter()
            .map(|s| (s.id.clone(), ovf::vm_name(env, s, &stem)))
            .collect();
        let has_eula = !env.all_eulas().is_empty();
        let mut w = Self::new(Source::Package(Box::new(package)), path, target);
        w.systems = systems;
        w.warnings = findings.iter().map(finding_text).collect();
        w.page = if has_eula {
            OvfPage::Eula
        } else {
            OvfPage::Location
        };
        Ok(w)
    }

    /// `GetDiskCapacityImage`: looks at the disk image at `path`.
    pub fn image(
        path: &str,
        target: Option<(ConnectionId, Option<Ref<Host>>)>,
    ) -> Result<Self, String> {
        let o = ovf_strings();
        let found = ovf_rules::inspect_image(Path::new(path)).map_err(|e| {
            tracing::warn!(error = %e, "the image cannot be read");
            o.error_not_valid_image.to_owned()
        })?;
        match found {
            Ok(Image::Supported {
                format: _,
                virtual_size,
                file_size,
            }) if virtual_size > 0 => {
                let mut w = Self::new(
                    Source::Image {
                        capacity: virtual_size,
                        file_size,
                    },
                    path,
                    target,
                );
                w.page = OvfPage::VmDefinition;
                Ok(w)
            }
            Ok(_) => Err(o.error_not_valid_image.to_owned()),
            Err(kind) => Err(fill(
                o.error_needs_conversion,
                &[("format", kind.name()), ("console", branding::PRODUCT_NAME)],
            )),
        }
    }

    fn package_ref(&self) -> Option<&Package> {
        match &self.source {
            Source::Package(p) => Some(p),
            Source::Image { .. } => None,
        }
    }

    fn is_image(&self) -> bool {
        matches!(self.source, Source::Image { .. })
    }

    fn pages(&self) -> Vec<OvfPage> {
        let mut p = Vec::new();
        match &self.source {
            Source::Image { .. } => p.push(OvfPage::VmDefinition),
            Source::Package(pkg) => {
                if !pkg.envelope.all_eulas().is_empty() {
                    p.push(OvfPage::Eula);
                }
            }
        }
        p.push(OvfPage::Location);
        if self.rbac_needed {
            p.push(OvfPage::Rbac);
        }
        p.extend([OvfPage::Storage, OvfPage::Network]);
        match &self.source {
            Source::Image { .. } => p.push(OvfPage::Boot),
            Source::Package(pkg) => {
                if pkg.has_manifest() || pkg.has_signature() {
                    p.push(OvfPage::Security);
                }
            }
        }
        p.push(OvfPage::Finish);
        p
    }

    fn page_name(p: OvfPage) -> &'static str {
        let o = ovf_strings();
        let x = xva_strings();
        match p {
            OvfPage::VmDefinition => o.page_vm_definition,
            OvfPage::Eula => o.page_eula,
            OvfPage::Location => o.page_location,
            OvfPage::Rbac => x.page_rbac,
            OvfPage::Storage => x.page_storage,
            OvfPage::Network => x.page_network,
            OvfPage::Security => o.page_security,
            OvfPage::Boot => o.page_boot,
            OvfPage::Finish => x.page_finish,
        }
    }

    /// The image's envelope, made of the wizard's choices (`None` for a
    /// package).
    fn image_envelope(&self) -> Option<Envelope> {
        let Source::Image {
            capacity,
            file_size,
            ..
        } = &self.source
        else {
            return None;
        };
        let (params, platform) = self.boot.settings();
        let path = std::fs::canonicalize(&self.path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| self.path.clone());
        Some(ovf::image_envelope(
            self.vm_name.trim(),
            self.cpus,
            self.memory_mb,
            params,
            platform,
            *capacity,
            &path,
            *file_size,
        ))
    }

    /// The disk and interface rows, made once a connection is chosen (an
    /// image's one disk and one interface).
    fn make_rows(&mut self, conn: &ConnectionView, show_hidden: bool) {
        if self.rows_for == Some(conn.id) && !self.disks.is_empty() {
            return;
        }
        let (env, systems) = match &self.source {
            Source::Package(p) => (p.envelope.clone(), self.systems.clone()),
            Source::Image { .. } => {
                let env = self.image_envelope().expect("an image");
                let systems = vec![(env.systems[0].id.clone(), self.vm_name.trim().to_owned())];
                (env, systems)
            }
        };
        let host = self.target.as_ref().and_then(|(_, h)| h.clone());
        let srs = rules::import_srs(&conn.model, host.as_ref());
        let default_sr = ovf_rules::default_sr(&conn.model, &srs);
        let nets = rules::import_networks(&conn.model, host.as_ref(), show_hidden);
        self.sr = default_sr.clone();
        self.disks = ovf_rules::disk_rows(&env, &systems)
            .into_iter()
            .map(|r| (r, default_sr.clone()))
            .collect();
        self.nics = ovf_rules::nic_rows(&env, &systems)
            .into_iter()
            .map(|r| {
                let n = ovf_rules::default_network(&r, &nets);
                (r, n)
            })
            .collect();
        self.rows_for = Some(conn.id);
    }

    fn configure_rbac(&mut self, conn: &ConnectionView) {
        let o = ovf_strings();
        let role = i18n::session_roles_text(conn);
        self.rbac_needed = conn.rbac.as_ref().is_some_and(|r| !r.is_local_superuser);
        self.rbac_blocked = match rbac::check(&conn.model, conn.rbac.as_deref(), IMPORT_OVF_RBAC) {
            RbacCheck::Allowed => None,
            RbacCheck::Denied { .. } => Some(fill(
                if self.is_image() {
                    o.rbac_import_image
                } else {
                    o.rbac_import_appliance
                },
                &[("role", &role)],
            )),
        };
        self.rbac_affinity =
            match rbac::check(&conn.model, conn.rbac.as_deref(), &["vm.set_affinity"]) {
                RbacCheck::Allowed => None,
                RbacCheck::Denied { .. } => {
                    Some(fill(xva_strings().rbac_affinity, &[("role", &role)]))
                }
            };
    }

    /// Each SR has room for the disks put on it.
    fn storage_error(&self, conn: &ConnectionView) -> Option<String> {
        let mut need: std::collections::HashMap<Ref<Sr>, u64> = std::collections::HashMap::new();
        for (row, sr) in &self.disks {
            let sr = if self.all_on_one {
                self.sr.clone()
            } else {
                sr.clone()
            };
            let Some(sr) = sr else { continue };
            let n = need.entry(sr.clone()).or_default();
            *n += row.size;
            if let Some(rec) = conn.model.srs.get(&sr)
                && rec.physical_size > 0
                && *n > ovf_rules::sr_free(rec)
            {
                return Some(fill(
                    ovf_strings().sr_too_small,
                    &[
                        ("sr", &rec.name_label),
                        ("size", &xen_core::format::disk_size(row.size as i64)),
                        ("disk", &row.label),
                    ],
                ));
            }
        }
        None
    }

    fn page_ok(&self, state: &AppState, conn: Option<&ConnectionView>) -> bool {
        match self.page {
            OvfPage::VmDefinition => {
                let n = self.vm_name.trim();
                !n.is_empty() && rules::valid_file_name(n)
            }
            OvfPage::Eula => {
                self.eula_accepted
                    && self
                        .package_ref()
                        .is_some_and(|p| p.envelope.all_eulas().len() <= MAX_EULAS)
            }
            OvfPage::Location => self
                .target
                .as_ref()
                .is_some_and(|(c, _)| state.connection(*c).is_some()),
            OvfPage::Rbac => self.rbac_blocked.is_none(),
            OvfPage::Storage => {
                let chosen = if self.all_on_one {
                    self.sr.is_some()
                } else {
                    self.disks.iter().all(|(_, s)| s.is_some())
                };
                chosen && conn.is_some_and(|c| self.storage_error(c).is_none())
            }
            OvfPage::Network => self.nics.iter().all(|(_, n)| n.is_some()),
            OvfPage::Security => {
                !(self.verify
                    && self.package_ref().is_some_and(Package::has_signature)
                    && matches!(self.certificate, Some(Err(_))))
            }
            OvfPage::Boot | OvfPage::Finish => true,
        }
    }

    // ---- pages ---------------------------------------------------------------------------

    fn vm_definition_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_vm_definition_title);
        ui.label(o.vm_definition_intro);
        ui.add_space(8.0);
        Grid::new("image-vm")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                let l = ui.label(o.label_vm_name);
                ui.add(TextEdit::singleline(&mut self.vm_name).desired_width(320.0))
                    .labelled_by(l.id);
                ui.end_row();
                ui.label(o.label_cpus);
                ui.add(DragValue::new(&mut self.cpus).range(1..=ovf_rules::IMAGE_CPUS_MAX));
                ui.end_row();
                ui.label(o.label_memory);
                ui.horizontal(|ui| {
                    ui.add(
                        DragValue::new(&mut self.memory_mb)
                            .range(ovf_rules::IMAGE_MEMORY_MIN_MB..=ovf_rules::IMAGE_MEMORY_MAX_MB)
                            .speed(ovf_rules::IMAGE_MEMORY_MIN_MB as f64),
                    );
                    ui.label(o.mb);
                });
                ui.end_row();
            });
        let n = self.vm_name.trim();
        if !n.is_empty() && !rules::valid_file_name(n) {
            ui.colored_label(
                ui.visuals().error_fg_color,
                xva_strings().error_invalid_name,
            );
        }
        if let Source::Image { capacity, .. } = &self.source {
            ui.add_space(6.0);
            ui.label(fill(
                xva_strings().disk_capacity,
                &[("size", &xen_core::format::disk_size(*capacity as i64))],
            ));
        }
    }

    fn eula_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_import_eula_title);
        ui.label(o.import_eula_intro);
        ui.add_space(6.0);
        let eulas: Vec<String> = self
            .package_ref()
            .map(|p| {
                p.envelope
                    .all_eulas()
                    .iter()
                    .map(|e| (*e).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        if eulas.len() > MAX_EULAS {
            ui.colored_label(ui.visuals().error_fg_color, o.eula_too_many);
            return;
        }
        ScrollArea::vertical()
            .id_salt("import-eulas")
            .max_height(240.0)
            .show(ui, |ui| {
                for e in &eulas {
                    ui.label(RichText::new(o.eula_tab).strong());
                    ui.label(e);
                    ui.separator();
                }
            });
        ui.checkbox(&mut self.eula_accepted, o.eula_accept);
    }

    fn location_page(&mut self, ui: &mut Ui, state: &AppState) {
        let o = ovf_strings();
        ui.heading(o.page_location_title);
        ui.label(o.location_intro);
        ui.add_space(6.0);
        if target_picker(ui, state, &mut self.target) {
            self.rows_for = None;
            self.disks.clear();
        }
        if !self.warnings.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(o.validation_warnings).strong());
            for w in &self.warnings {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
        }
    }

    fn storage_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let o = ovf_strings();
        let x = xva_strings();
        ui.heading(o.page_storage_title);
        ui.label(o.storage_intro);
        ui.add_space(6.0);
        let host = self.target.as_ref().and_then(|(_, h)| h.clone());
        let srs = rules::import_srs(&conn.model, host.as_ref());
        let label = |r: &Ref<Sr>| {
            conn.model
                .srs
                .get(r)
                .map(|s| {
                    fill(
                        "{name} ({free})",
                        &[
                            ("name", &s.name_label),
                            (
                                "free",
                                &fill(
                                    x.sr_size_free,
                                    &[
                                        (
                                            "free",
                                            &xen_core::format::disk_size(
                                                ovf_rules::sr_free(s) as i64
                                            ),
                                        ),
                                        ("size", &xen_core::format::disk_size(s.physical_size)),
                                    ],
                                ),
                            ),
                        ],
                    )
                })
                .unwrap_or_default()
        };
        if srs.is_empty() {
            ui.label(RichText::new(x.no_srs).italics());
        }
        ui.radio_value(&mut self.all_on_one, true, o.all_on_one);
        ui.add_enabled_ui(self.all_on_one, |ui| {
            ComboBox::from_id_salt("ovf-sr-all")
                .selected_text(self.sr.as_ref().map(label).unwrap_or_default())
                .width(420.0)
                .show_ui(ui, |ui| {
                    for (r, _) in &srs {
                        ui.selectable_value(&mut self.sr, Some((*r).clone()), label(r));
                    }
                });
        });
        ui.radio_value(&mut self.all_on_one, false, o.each_on_own);
        ui.add_enabled_ui(!self.all_on_one, |ui| {
            Grid::new("ovf-disks")
                .num_columns(2)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.strong(o.col_vm_disk);
                    ui.strong(o.col_target_sr);
                    ui.end_row();
                    for (i, (row, sr)) in self.disks.iter_mut().enumerate() {
                        ui.label(format!("{} - {}", row.vm, row.label));
                        ComboBox::from_id_salt(format!("ovf-sr-{i}"))
                            .selected_text(sr.as_ref().map(label).unwrap_or_default())
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for (r, _) in &srs {
                                    ui.selectable_value(sr, Some((*r).clone()), label(r));
                                }
                            });
                        ui.end_row();
                    }
                });
        });
        if let Some(e) = self.storage_error(conn) {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }

    fn network_page(&mut self, ui: &mut Ui, conn: &ConnectionView, show_hidden: bool) {
        let o = ovf_strings();
        ui.heading(o.page_network_title);
        ui.label(o.network_intro);
        ui.add_space(6.0);
        ui.label(o.network_label);
        let host = self.target.as_ref().and_then(|(_, h)| h.clone());
        let nets = rules::import_networks(&conn.model, host.as_ref(), show_hidden);
        let name = |r: &Ref<Network>| {
            conn.model
                .networks
                .get(r)
                .map(|n| xen_core::objects::network_name(&conn.model, n))
                .unwrap_or_default()
        };
        Grid::new("ovf-nics")
            .num_columns(3)
            .striped(true)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.strong(o.col_vm_vif);
                ui.strong(xva_strings().col_mac);
                ui.strong(o.col_target_network);
                ui.end_row();
                for (i, (row, net)) in self.nics.iter_mut().enumerate() {
                    let label = if row.label.is_empty() {
                        o.none
                    } else {
                        &row.label
                    };
                    ui.label(format!("{} - {label}", row.vm));
                    ui.label(if row.mac.is_empty() {
                        o.auto_mac
                    } else {
                        &row.mac
                    });
                    ComboBox::from_id_salt(format!("ovf-net-{i}"))
                        .selected_text(net.as_ref().map(name).unwrap_or_default())
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for (r, _) in &nets {
                                ui.selectable_value(net, Some((*r).clone()), name(r));
                            }
                        });
                    ui.end_row();
                }
            });
    }

    fn security_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_security_title);
        ui.label(o.security_intro);
        ui.add_space(6.0);
        let signed = self.package_ref().is_some_and(Package::has_signature);
        if signed {
            ui.label(o.verify_signature_text);
            ui.checkbox(&mut self.verify, o.verify_signature);
            if ui.button(o.view_certificate).clicked()
                || (self.verify && self.certificate.is_none())
            {
                let cert = self.package_ref().and_then(|p| p.certificate.clone());
                self.certificate = Some(
                    cert.as_deref()
                        .and_then(ovf_package::certificate_subject)
                        .ok_or_else(|| o.certificate_missing.to_owned()),
                );
            }
            match &self.certificate {
                Some(Ok(subject)) => {
                    ui.label(fill(o.certificate_subject, &[("subject", subject)]));
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {}
            }
        } else {
            ui.label(o.verify_manifest_text);
            ui.checkbox(&mut self.verify, o.verify_manifest);
        }
    }

    fn boot_page(&mut self, ui: &mut Ui) {
        let o = ovf_strings();
        ui.heading(o.page_boot_title);
        ui.label(o.boot_intro);
        ui.add_space(6.0);
        ui.label(RichText::new(o.boot_group).strong());
        ui.radio_value(&mut self.boot, ImageBoot::Bios, o.boot_bios);
        ui.radio_value(&mut self.boot, ImageBoot::Uefi, o.boot_uefi);
        ui.radio_value(&mut self.boot, ImageBoot::SecureUefi, o.boot_secure);
    }

    fn boot_text(&self) -> &'static str {
        let o = ovf_strings();
        match self.boot {
            ImageBoot::Bios => o.boot_bios,
            ImageBoot::Uefi => o.boot_uefi,
            ImageBoot::SecureUefi => o.boot_secure,
        }
    }

    fn finish_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let o = ovf_strings();
        let x = xva_strings();
        ui.heading(x.page_finish_import_title);
        ui.label(x.finish_import_intro);
        ui.add_space(6.0);
        let sr_name = |r: &Option<Ref<Sr>>| {
            r.as_ref()
                .and_then(|r| conn.model.srs.get(r))
                .map(|s| s.name_label.clone())
                .unwrap_or_default()
        };
        let net_name = |r: &Option<Ref<Network>>| {
            r.as_ref()
                .and_then(|r| conn.model.networks.get(r))
                .map(|n| xen_core::objects::network_name(&conn.model, n))
                .unwrap_or_default()
        };
        let mut rows = Vec::new();
        match &self.source {
            Source::Package(p) => {
                rows.push((o.summary_package_name.to_owned(), file_name(&self.path)));
                if p.has_manifest() || p.has_signature() {
                    rows.push((
                        o.summary_verify_manifest.to_owned(),
                        yes_no(self.verify && p.has_manifest()),
                    ));
                    rows.push((
                        o.summary_verify_signature.to_owned(),
                        yes_no(self.verify && p.has_signature()),
                    ));
                }
            }
            Source::Image { .. } => {
                rows.push((o.summary_image.to_owned(), self.path.clone()));
                rows.push((o.summary_vm_name.to_owned(), self.vm_name.trim().to_owned()));
                rows.push((o.summary_cpus.to_owned(), self.cpus.to_string()));
                rows.push((
                    o.summary_memory.to_owned(),
                    fill(
                        o.summary_memory_value,
                        &[("mb", &self.memory_mb.to_string())],
                    ),
                ));
                rows.push((o.summary_boot.to_owned(), self.boot_text().to_owned()));
            }
        }
        let several = self.systems.len() > 1;
        for (id, vm) in self.systems_or_image() {
            let (st, nt) = if several {
                (
                    fill(o.summary_vm_storage, &[("vm", &vm)]),
                    fill(o.summary_vm_network, &[("vm", &vm)]),
                )
            } else {
                (o.summary_storage.to_owned(), o.summary_network.to_owned())
            };
            for (i, (_, sr)) in self
                .disks
                .iter()
                .filter(|(r, _)| r.system == id)
                .enumerate()
            {
                let sr = if self.all_on_one { &self.sr } else { sr };
                rows.push((if i == 0 { st.clone() } else { String::new() }, sr_name(sr)));
            }
            for (i, (_, n)) in self.nics.iter().filter(|(r, _)| r.system == id).enumerate() {
                rows.push((if i == 0 { nt.clone() } else { String::new() }, net_name(n)));
            }
        }
        summary(ui, "ovf-import-summary", &rows);
        ui.add_space(8.0);
        ui.checkbox(&mut self.start, x.start_vms);
    }

    fn systems_or_image(&self) -> Vec<(String, String)> {
        if self.is_image() {
            self.disks
                .first()
                .map(|(r, _)| vec![(r.system.clone(), r.vm.clone())])
                .unwrap_or_default()
        } else {
            self.systems.clone()
        }
    }

    /// The import the wizard asks for.
    pub(crate) fn request(&self) -> ActionRequest {
        let affinity = if self.rbac_affinity.is_some() {
            None
        } else {
            self.target.as_ref().and_then(|(_, h)| h.clone())
        };
        let (image, systems) = match &self.source {
            Source::Package(_) => (None, self.systems.clone()),
            Source::Image { .. } => {
                // Made again with the boot mode chosen; its one disk and
                // one interface take the rows' choices.
                let env = self.image_envelope().expect("an image");
                let id = env.systems[0].id.clone();
                (
                    Some(Box::new(env)),
                    vec![(id, self.vm_name.trim().to_owned())],
                )
            }
        };
        let vms = systems
            .iter()
            .enumerate()
            .map(|(n, (id, name))| {
                let (disks, nics): (Vec<_>, Vec<_>) = match &image {
                    Some(env) => {
                        let hw = ovf::hardware(&env.systems[0]).expect("hardware");
                        let disk = hw.items.iter().find(|r| r.is_disk()).expect("a disk");
                        let nic = hw
                            .items
                            .iter()
                            .find(|r| r.resource_type == ovf::resource::NIC)
                            .expect("a NIC");
                        (
                            self.disks
                                .first()
                                .map(|(_, s)| (disk.instance_id.clone(), s.clone()))
                                .into_iter()
                                .collect(),
                            self.nics
                                .first()
                                .map(|(_, net)| (nic.instance_id.clone(), net.clone()))
                                .into_iter()
                                .collect(),
                        )
                    }
                    None => (
                        self.disks
                            .iter()
                            .filter(|(r, _)| r.system == *id)
                            .map(|(r, s)| (r.item.clone(), s.clone()))
                            .collect(),
                        self.nics
                            .iter()
                            .filter(|(r, _)| r.system == *id)
                            .map(|(r, net)| (r.item.clone(), net.clone()))
                            .collect(),
                    ),
                };
                let _ = n;
                VmMapping {
                    system: id.clone(),
                    name: name.clone(),
                    affinity: affinity.clone(),
                    srs: disks
                        .into_iter()
                        .filter_map(|(item, s)| {
                            let s = if self.all_on_one { self.sr.clone() } else { s };
                            s.map(|s| (item, s))
                        })
                        .collect(),
                    networks: nics
                        .into_iter()
                        .filter_map(|(item, n)| n.map(|n| (item, n)))
                        .collect(),
                }
            })
            .collect();
        let package = self.package_ref();
        ActionRequest::ImportOvf(Box::new(OvfImport {
            path: self.path.clone(),
            image,
            vms,
            verify_manifest: self.verify && package.is_some_and(Package::has_manifest),
            verify_signature: self.verify && package.is_some_and(Package::has_signature),
            start: self.start,
        }))
    }

    /// Draws the wizard.
    pub fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> Shown {
        let x = xva_strings();
        let o = ovf_strings();
        let n = crate::net_strings::net_strings();
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let conn = self
            .target
            .as_ref()
            .and_then(|(c, _)| state.connection(*c))
            .cloned();
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        let title = if self.is_image() {
            o.import_image_title
        } else {
            o.import_ovf_title
        };
        let mut open = true;
        let mut shown = Shown::Open;
        egui::Window::new(title)
            .id(egui::Id::unique("ovf-import-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([800.0, 470.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let mut names = vec![x.page_source];
                    names.extend(pages.iter().map(|p| Self::page_name(*p)));
                    page_list(ui, &names, index + 1);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(640.0);
                        ScrollArea::vertical()
                            .id_salt("ovf-import-page")
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match (self.page, conn.as_deref()) {
                                (OvfPage::VmDefinition, _) => self.vm_definition_page(ui),
                                (OvfPage::Eula, _) => self.eula_page(ui),
                                (OvfPage::Location, _) => self.location_page(ui, state),
                                (OvfPage::Rbac, _) => {
                                    ui.heading(x.page_rbac);
                                    ui.label(x.rbac_description);
                                    ui.add_space(6.0);
                                    if let Some(why) = &self.rbac_blocked {
                                        ui.colored_label(ui.visuals().error_fg_color, why);
                                    } else {
                                        if let Some(w) = &self.rbac_affinity {
                                            ui.colored_label(ui.visuals().warn_fg_color, w);
                                        }
                                        ui.label(x.rbac_click_next);
                                    }
                                }
                                (OvfPage::Storage, Some(c)) => self.storage_page(ui, c),
                                (OvfPage::Network, Some(c)) => {
                                    self.network_page(ui, c, show_hidden)
                                }
                                (OvfPage::Security, _) => self.security_page(ui),
                                (OvfPage::Boot, _) => self.boot_page(ui),
                                (OvfPage::Finish, Some(c)) => self.finish_page(ui, c),
                                _ => {}
                            });
                        error_label(ui, &self.error);
                    });
                });
                ui.separator();
                let last = self.page == OvfPage::Finish;
                let (previous, next, cancel) = nav(
                    ui,
                    true,
                    self.page_ok(state, conn.as_deref()),
                    if last { n.finish } else { n.next },
                );
                if previous {
                    self.error = None;
                    if index == 0 {
                        shown = Shown::Back {
                            path: self.path.clone(),
                            target: self.target.clone(),
                        };
                    } else {
                        self.page = pages[index - 1];
                    }
                }
                if next {
                    match self.page {
                        OvfPage::Finish => {
                            if let Some(c) = &conn {
                                ops.request(c, self.request(), backend);
                            }
                            shown = Shown::Closed;
                        }
                        OvfPage::VmDefinition => {
                            // The rows follow the VM's name.
                            self.rows_for = None;
                            self.disks.clear();
                            self.page = pages[index + 1];
                        }
                        OvfPage::Location => {
                            if let Some(c) = &conn {
                                self.configure_rbac(c);
                                self.make_rows(c, show_hidden);
                            }
                            let pages = self.pages();
                            let i = pages
                                .iter()
                                .position(|p| *p == OvfPage::Location)
                                .unwrap_or(0);
                            self.page = pages[i + 1];
                        }
                        _ => {
                            self.error = None;
                            self.page = pages[index + 1];
                        }
                    }
                }
                if cancel {
                    shown = Shown::Closed;
                }
            });
        if !open {
            return Shown::Closed;
        }
        let _ = connection_name;
        shown
    }
}
