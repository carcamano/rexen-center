//! The New SR wizard (`NewSRWizard`): Type, Name and Location, as the
//! original lays them out, for NFS, iSCSI, SMB, local storage (ext / LVM /
//! XFS) and the NFS and SMB ISO libraries. Opened from Reattach, it is the
//! "Attach Storage Repository" wizard for that SR (`srToReattach`).
//!
//! The probes run as prechecks (no history entry): the NFS and SMB Scan
//! buttons (`SrProbeAction`), the iSCSI IQN and LUN scans, and on Finish
//! the iSCSI page's probe of the chosen LUN, whose answer opens the
//! original's choice between reattaching the SR found and formatting the
//! LUN (`LVMoIsciWarningDialog`). Before an existing SR is attached the
//! original's multi-pool warning asks (`AskUserIfShouldContinue`).
//! Finishing returns a [`NewSrSpec`] that [`xen_core`] runs.
//!
//! Hardware HBA, software FCoE and GFS2 (the provisioning page) are not
//! offered, nor the SR types only XCP-ng ships.

use egui::{Button, ComboBox, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Sr};
use xen_core::format::disk_size;
use xen_core::objects;
use xen_core::operations::{Precheck, PrecheckResult};
use xen_core::storage::{self, DeviceConfig, IqnInfo, LocalFs, LunInfo, NewSrSpec, SrInfo, SrKind};
use xen_core::{AppState, BackendHandle, Command, ConnectionId, ConnectionModel, ConnectionView};

use crate::branding;
use crate::i18n::{self, fill};
use crate::sr_strings::sr_strings;

/// The wizard's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Type,
    Name,
    Location,
}

const PAGES: [Page; 3] = [Page::Type, Page::Name, Page::Location];

impl Page {
    fn name(self) -> &'static str {
        let s = sr_strings();
        match self {
            Page::Type => s.page_type,
            Page::Name => s.page_name,
            Page::Location => s.page_location,
        }
    }
}

/// How the wizard ended (or that it has not).
pub enum WizardOutcome {
    Open,
    Cancelled,
    Finish(ConnectionId, Box<NewSrSpec>),
}

/// A probe the wizard waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// The NFS / SMB Scan button.
    Scan,
    /// The iSCSI "Scan Target Host" button.
    Iqns,
    /// The LUNs of the chosen IQN.
    Luns,
    /// Finish on the iSCSI page: what the chosen LUN holds.
    Lun,
}

/// Where an iSCSI error shows (next to the field it is about).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IscsiErrorAt {
    Host,
    Chap,
    Lun,
    Bottom,
}

/// The LUN choice (`LVMoIsciWarningDialog`).
struct LunChoice {
    found: Option<SrInfo>,
    /// The SR found is in use in this pool (only reattach is offered).
    in_use: bool,
}

/// A yes/no question before finishing (the multi-pool warning).
struct Warning {
    text: String,
    spec: NewSrSpec,
}

/// The wizard's state. Fields are public for the window tests.
pub struct NewSrWizard {
    connection: ConnectionId,
    pub page: Page,
    pub kind: SrKind,
    /// The detached SR being reattached (`srToReattach`).
    pub reattach: Option<Ref<Sr>>,
    reattach_uuid: Option<String>,
    pub name: String,
    pub description: String,
    pub auto_description: bool,
    /// The name was typed (a type change keeps it).
    name_edited: bool,

    // NFS / SMB: share, options / credentials, the scan's result.
    pub share: String,
    pub nfs_options: String,
    pub nfs_version: &'static str,
    pub smb_user: String,
    pub smb_password: String,
    pub found: Option<Vec<SrInfo>>,
    /// "Reattach an existing SR" chosen, with this UUID.
    pub reattach_existing: bool,
    pub selected_uuid: Option<String>,

    // ISO libraries.
    pub iso_share: String,
    pub nfs_iso_v4: bool,
    pub smb_iso_other_user: bool,
    pub smb_iso_user: String,
    pub smb_iso_password: String,

    // iSCSI.
    pub iscsi_host: String,
    pub iscsi_port: String,
    pub chap: bool,
    pub chap_user: String,
    pub chap_password: String,
    pub iqns: Option<Vec<IqnInfo>>,
    pub iqn: Option<usize>,
    pub luns: Option<Vec<LunInfo>>,
    pub lun: Option<usize>,
    pub iscsi_error: Option<(IscsiErrorAt, String)>,
    lun_choice: Option<LunChoice>,

    // Local storage.
    pub device: String,

    /// The probe running, if any (its precheck id).
    pending: Option<(u64, Probe)>,
    /// What the last probe answered when it failed.
    pub probe_error: Option<String>,
    warning: Option<Warning>,
    outcome: Option<WizardOutcome>,
}

impl NewSrWizard {
    /// The wizard for `conn`; with `reattach`, for that detached SR.
    pub fn new(conn: &ConnectionView, reattach: Option<Ref<Sr>>) -> Self {
        let model = &conn.model;
        let reattach_rec = reattach.as_ref().and_then(|r| model.srs.get(r)).cloned();
        let kind = reattach_rec
            .as_ref()
            .and_then(|sr| SrKind::of_sr(sr))
            .or_else(|| {
                // The original preselects iSCSI.
                [SrKind::Iscsi]
                    .into_iter()
                    .chain(SrKind::ALL)
                    .find(|k| storage::kind_available(model, *k, false))
            })
            .unwrap_or(SrKind::Nfs);
        let mut wizard = Self {
            connection: conn.id,
            page: Page::Type,
            kind,
            reattach: reattach.clone(),
            reattach_uuid: reattach_rec.as_ref().map(|s| s.uuid.clone()),
            name: String::new(),
            description: String::new(),
            auto_description: true,
            name_edited: false,
            share: String::new(),
            nfs_options: String::new(),
            nfs_version: "3",
            smb_user: String::new(),
            smb_password: String::new(),
            found: None,
            reattach_existing: reattach.is_some(),
            selected_uuid: None,
            iso_share: String::new(),
            nfs_iso_v4: false,
            smb_iso_other_user: false,
            smb_iso_user: String::new(),
            smb_iso_password: String::new(),
            iscsi_host: String::new(),
            iscsi_port: storage::ISCSI_PORT.to_string(),
            chap: false,
            chap_user: String::new(),
            chap_password: String::new(),
            iqns: None,
            iqn: None,
            luns: None,
            lun: None,
            iscsi_error: None,
            lun_choice: None,
            device: String::new(),
            pending: None,
            probe_error: None,
            warning: None,
            outcome: None,
        };
        match reattach_rec {
            Some(sr) => {
                wizard.name = sr.name_label.clone();
                wizard.description = sr.name_description.clone();
                wizard.auto_description = sr.name_description.is_empty();
                wizard.name_edited = true;
                // One matching type: straight to the location page.
                wizard.page = Page::Location;
            }
            None => wizard.reset_name(model),
        }
        wizard
    }

    /// The default name of the chosen type (`ResetSrName`).
    fn reset_name(&mut self, model: &ConnectionModel) {
        let s = sr_strings();
        let base = match self.kind {
            SrKind::Nfs => s.default_nfs,
            SrKind::Iscsi => s.default_iscsi,
            SrKind::Smb => s.default_smb,
            SrKind::Local(_) => s.default_local,
            SrKind::NfsIso => s.default_nfs_iso,
            SrKind::SmbIso => s.default_smb_iso,
        };
        self.name = storage::default_sr_name(model, base);
    }

    /// Chooses the type (the type page's radio buttons).
    pub fn choose(&mut self, model: &ConnectionModel, kind: SrKind) {
        if self.kind.same_page(kind) && self.kind != kind {
            self.kind = kind;
            return;
        }
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.found = None;
        self.selected_uuid = None;
        self.reattach_existing = self.reattach.is_some();
        self.probe_error = None;
        if !self.name_edited {
            self.reset_name(model);
        }
    }

    /// The connection the wizard works on.
    pub fn connection_id(&self) -> ConnectionId {
        self.connection
    }

    pub fn is_reattach(&self) -> bool {
        self.reattach.is_some()
    }

    // ---- probes ----------------------------------------------------------

    fn ask(&mut self, backend: &BackendHandle, next_id: &mut u64, probe: Probe, check: Precheck) {
        *next_id += 1;
        let id = *next_id;
        self.pending = Some((id, probe));
        self.probe_error = None;
        backend.send(Command::Precheck {
            connection: self.connection,
            id,
            check,
        });
    }

    /// Whether `id` is the probe this wizard waits for.
    pub fn waits_for(&self, id: u64) -> bool {
        self.pending.is_some_and(|(p, _)| p == id)
    }

    /// The probe is running.
    pub fn probing(&self) -> bool {
        self.pending.is_some()
    }

    fn chap_pair(&self) -> Option<(&str, &str)> {
        self.chap
            .then_some((self.chap_user.as_str(), self.chap_password.as_str()))
    }

    fn iscsi_port_value(&self) -> Option<u16> {
        self.iscsi_port.trim().parse().ok()
    }

    /// Clears what the iSCSI scans found (a field changed).
    fn reset_iscsi(&mut self) {
        self.iqns = None;
        self.iqn = None;
        self.luns = None;
        self.lun = None;
        self.iscsi_error = None;
        if matches!(self.pending, Some((_, Probe::Iqns | Probe::Luns))) {
            self.pending = None;
        }
    }

    fn scan_iqns(&mut self, backend: &BackendHandle, next_id: &mut u64) {
        let Some(port) = self.iscsi_port_value() else {
            return;
        };
        self.reset_iscsi();
        let chap = self
            .chap_pair()
            .map(|(u, p)| (u.to_owned(), storage::Secret(p.to_owned())));
        let check = Precheck::IscsiIqns {
            target: self.iscsi_host.trim().to_owned(),
            port,
            chap,
        };
        self.ask(backend, next_id, Probe::Iqns, check);
    }

    fn scan_luns(&mut self, backend: &BackendHandle, next_id: &mut u64) {
        let Some(iqn) = self.selected_iqn().cloned() else {
            return;
        };
        self.luns = None;
        self.lun = None;
        self.iscsi_error = None;
        let chap = self
            .chap_pair()
            .map(|(u, p)| (u.to_owned(), storage::Secret(p.to_owned())));
        let check = Precheck::IscsiLuns {
            target: iqn.address.clone(),
            port: iqn.port,
            iqn: iqn.target_iqn.clone(),
            chap,
        };
        self.ask(backend, next_id, Probe::Luns, check);
    }

    pub fn selected_iqn(&self) -> Option<&IqnInfo> {
        self.iqn.and_then(|i| self.iqns.as_ref()?.get(i))
    }

    pub fn selected_lun(&self) -> Option<&LunInfo> {
        self.lun.and_then(|i| self.luns.as_ref()?.get(i))
    }

    /// The device_config of the Scan button (NFS, SMB).
    fn scan_config(&self) -> Vec<(String, String)> {
        match self.kind {
            SrKind::Smb => {
                storage::smb_device_config(self.share.trim(), &self.smb_user, &self.smb_password)
            }
            _ => storage::nfs_probe_config(&self.share, &self.nfs_options),
        }
    }

    /// An answer to one of this wizard's prechecks.
    pub fn on_precheck(&mut self, id: u64, result: PrecheckResult, model: &ConnectionModel) {
        let Some((pending, probe)) = self.pending else {
            return;
        };
        if pending != id {
            return;
        }
        self.pending = None;
        let describe = |e: &xapi::Error| i18n::describe_xapi_error(e);
        match (probe, result) {
            (Probe::Scan, PrecheckResult::SrProbe(Ok(found))) => {
                let srs = found.srs;
                // `TryAndSelectUUID`: the SR being reattached, else nothing.
                self.selected_uuid = self
                    .reattach_uuid
                    .clone()
                    .filter(|u| srs.iter().any(|s| &s.uuid == u));
                self.reattach_existing = self.is_reattach();
                self.found = Some(srs);
            }
            (Probe::Scan, PrecheckResult::SrProbe(Err(e))) => {
                self.found = None;
                self.probe_error = Some(describe(&e));
            }
            (Probe::Iqns, PrecheckResult::IscsiIqns(Ok(iqns))) => {
                let valid: Vec<IqnInfo> = iqns
                    .into_iter()
                    .filter(|i| !i.target_iqn.is_empty())
                    .collect();
                self.iqn = (valid.len() == 1).then_some(0);
                self.iqns = Some(valid);
            }
            (Probe::Iqns, PrecheckResult::IscsiIqns(Err(e))) => self.iscsi_failure(&e),
            (Probe::Luns, PrecheckResult::IscsiLuns(Ok(luns))) => {
                self.lun = (luns.len() == 1).then_some(0);
                self.luns = Some(luns);
                self.check_lun(model);
            }
            (Probe::Luns, PrecheckResult::IscsiLuns(Err(e))) => self.iscsi_failure(&e),
            (Probe::Lun, PrecheckResult::SrProbe(Ok(found))) => {
                self.examine_lun(model, found.srs);
            }
            (Probe::Lun, PrecheckResult::SrProbe(Err(e))) => self.iscsi_failure(&e),
            _ => {}
        }
    }

    /// `LVMoISCSI.HandleFailure`.
    fn iscsi_failure(&mut self, e: &xapi::Error) {
        let s = sr_strings();
        let code = match e {
            xapi::Error::Api { code, .. } => code.as_str(),
            _ => "",
        };
        self.iscsi_error = Some(match code {
            "SR_BACKEND_FAILURE_68" => (IscsiErrorAt::Chap, s.iscsi_login_failed.to_owned()),
            "SR_BACKEND_FAILURE_140" => (IscsiErrorAt::Host, s.invalid_host.to_owned()),
            "SR_BACKEND_FAILURE_141" => (IscsiErrorAt::Host, s.iscsi_unreachable.to_owned()),
            _ => (IscsiErrorAt::Bottom, i18n::describe_xapi_error(e)),
        });
    }

    /// A LUN in use by an SR of this pool (`SrUsingLunExists`).
    fn check_lun(&mut self, model: &ConnectionModel) {
        let s = sr_strings();
        self.iscsi_error = None;
        let Some(lun) = self.selected_lun() else {
            return;
        };
        if let Some((sr_ref, sr)) = storage::sr_using_lun(model, &lun.scsi_id)
            && Some(sr_ref) != self.reattach.as_ref()
        {
            let pool = pool_name(model);
            self.iscsi_error = Some((
                IscsiErrorAt::Lun,
                fill(s.lun_in_use, &[("sr", &sr.name_label), ("pool", &pool)]),
            ));
        }
    }

    /// `ExamineIscsiProbeResults`.
    fn examine_lun(&mut self, model: &ConnectionModel, srs: Vec<SrInfo>) {
        let s = sr_strings();
        if let Some(uuid) = &self.reattach_uuid {
            match srs.first() {
                Some(found) if &found.uuid == uuid => {
                    self.selected_uuid = Some(uuid.clone());
                    self.finish(model);
                }
                _ => {
                    self.iscsi_error = Some((
                        IscsiErrorAt::Lun,
                        fill(s.incorrect_lun, &[("name", &self.name)]),
                    ));
                }
            }
            return;
        }
        let found = srs.into_iter().next();
        let in_use = found.as_ref().is_some_and(|f| {
            model
                .srs
                .values()
                .any(|sr| sr.uuid == f.uuid && storage::has_pbds(sr))
        });
        self.lun_choice = Some(LunChoice { found, in_use });
    }

    // ---- validity ----------------------------------------------------------

    fn already_attached(&self, model: &ConnectionModel) -> Option<String> {
        let s = sr_strings();
        let pool = pool_name(model);
        match self.kind {
            SrKind::NfsIso => storage::attached_iso_locations(model, "nfs_iso")
                .contains(&self.iso_share)
                .then(|| fill(s.nfs_iso_attached, &[("pool", &pool)])),
            SrKind::SmbIso => storage::attached_iso_locations(model, "cifs")
                .contains(&self.iso_share)
                .then(|| fill(s.smb_iso_attached, &[("pool", &pool)])),
            _ => None,
        }
    }

    /// Whether the location page can finish.
    pub fn location_ok(&self, model: &ConnectionModel) -> bool {
        if self.probing() {
            return false;
        }
        match self.kind {
            SrKind::Nfs | SrKind::Smb => {
                let valid = if self.kind == SrKind::Nfs {
                    storage::valid_nfs_share(&self.share)
                } else {
                    storage::valid_cifs_share(self.share.trim())
                };
                let create_allowed = !self.is_reattach() || self.kind.always_creates();
                valid
                    && if self.reattach_existing {
                        self.selected_uuid.is_some()
                    } else {
                        create_allowed
                    }
            }
            SrKind::NfsIso => {
                storage::valid_nfs_share(&self.iso_share) && self.already_attached(model).is_none()
            }
            SrKind::SmbIso => {
                storage::valid_cifs_share(&self.iso_share)
                    && !(self.smb_iso_other_user && self.smb_iso_user.is_empty())
                    && self.already_attached(model).is_none()
            }
            SrKind::Iscsi => {
                !self.iscsi_host.trim().is_empty()
                    && self.iscsi_port_value().is_some()
                    && (!self.chap || !self.chap_user.is_empty())
                    && self.selected_lun().is_some()
                    && self.iscsi_error.is_none()
            }
            SrKind::Local(_) => !self.device.trim().is_empty(),
        }
    }

    fn page_ok(&self, model: &ConnectionModel, page: Page) -> bool {
        match page {
            Page::Type => true,
            Page::Name => !self.name.trim().is_empty(),
            Page::Location => self.location_ok(model),
        }
    }

    // ---- finishing ---------------------------------------------------------

    /// The description the location page gives (`SrDescription`).
    fn page_description(&self) -> String {
        let s = sr_strings();
        match self.kind {
            SrKind::Nfs => fill(s.desc_nfs, &[("share", &self.share)]),
            SrKind::Smb => fill(s.desc_smb, &[("share", &self.share)]),
            SrKind::NfsIso => fill(s.desc_nfs_iso, &[("share", &self.iso_share)]),
            SrKind::SmbIso => fill(s.desc_smb_iso, &[("share", &self.iso_share)]),
            SrKind::Local(_) => fill(s.desc_local, &[("device", &self.device)]),
            SrKind::Iscsi => match (self.selected_iqn(), self.selected_lun()) {
                (Some(iqn), Some(lun)) => fill(
                    s.desc_iscsi,
                    &[
                        ("target", &iqn.address),
                        ("iqn", &iqn.target_iqn),
                        ("lun", &storage::lun_label(lun)),
                    ],
                ),
                _ => String::new(),
            },
        }
    }

    /// The request as the pages fill it (`uuid` from the page: the SR to
    /// introduce or reattach).
    pub fn spec(&self) -> NewSrSpec {
        let device_config = match self.kind {
            SrKind::Nfs => {
                storage::nfs_device_config(&self.share, &self.nfs_options, self.nfs_version)
            }
            SrKind::Smb => {
                storage::smb_device_config(self.share.trim(), &self.smb_user, &self.smb_password)
            }
            SrKind::NfsIso => storage::nfs_iso_device_config(&self.iso_share, self.nfs_iso_v4),
            SrKind::SmbIso => storage::smb_iso_device_config(
                &self.iso_share,
                self.smb_iso_other_user
                    .then_some((self.smb_iso_user.as_str(), self.smb_iso_password.as_str())),
            ),
            SrKind::Iscsi => match (self.selected_iqn(), self.selected_lun()) {
                (Some(iqn), Some(lun)) => storage::iscsi_device_config(iqn, lun, self.chap_pair()),
                _ => Vec::new(),
            },
            SrKind::Local(_) => storage::local_device_config(self.device.trim()),
        };
        let uuid = match self.kind {
            SrKind::Nfs | SrKind::Smb if self.reattach_existing => self.selected_uuid.clone(),
            SrKind::Iscsi => self.selected_uuid.clone(),
            _ => self.reattach_uuid.clone().filter(|_| self.is_reattach()),
        };
        NewSrSpec {
            kind: self.kind,
            name: self.name.trim().to_owned(),
            description: if self.auto_description {
                self.page_description()
            } else {
                self.description.clone()
            },
            device_config,
            sm_config: Vec::new(),
            uuid,
            reattach: self.reattach.clone(),
        }
    }

    /// Finish: the iSCSI page probes its LUN first; an existing SR asks the
    /// multi-pool warning first.
    fn finish(&mut self, model: &ConnectionModel) {
        let spec = self.spec();
        let s = sr_strings();
        let warn = match (&spec.uuid, &spec.reattach) {
            (Some(_), None) => spec.kind.introduce_prompt(),
            (Some(_), Some(_)) => spec.kind.reattach_warning(),
            _ => false,
        };
        if warn {
            let sr = match &spec.reattach {
                Some(r) => model
                    .srs
                    .get(r)
                    .map(|x| x.name_label.clone())
                    .unwrap_or_default(),
                None => spec.uuid.clone().unwrap_or_default(),
            };
            self.warning = Some(Warning {
                text: fill(
                    s.multi_pool_warning,
                    &[("console", branding::PRODUCT_NAME), ("sr", &sr)],
                ),
                spec,
            });
            return;
        }
        self.outcome = Some(WizardOutcome::Finish(self.connection, Box::new(spec)));
    }

    fn start_finish(
        &mut self,
        model: &ConnectionModel,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) {
        if self.kind == SrKind::Iscsi {
            let dc = self.spec().device_config;
            self.ask(
                backend,
                next_id,
                Probe::Lun,
                Precheck::SrProbe {
                    sr_type: "lvmoiscsi".into(),
                    device_config: DeviceConfig(dc),
                },
            );
            return;
        }
        self.finish(model);
    }

    // ---- drawing -----------------------------------------------------------

    /// Draws the wizard; `next_id` numbers its prechecks.
    pub fn show(
        &mut self,
        ctx: &Context,
        state: &AppState,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> WizardOutcome {
        let s = sr_strings();
        let Some(conn) = state.connection(self.connection) else {
            return WizardOutcome::Cancelled;
        };
        let model = conn.model.clone();
        let pool = pool_name(&model);
        let title = if self.is_reattach() {
            fill(s.wizard_title_attach, &[("pool", &pool)])
        } else {
            fill(s.wizard_title, &[("pool", &pool)])
        };
        let mut open = true;
        let mut cancelled = false;
        egui::Window::new(title)
            .id(egui::Id::unique("new-sr-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([760.0, 470.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(130.0);
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
                        ui.set_min_width(580.0);
                        egui::ScrollArea::vertical()
                            .id_salt("new-sr-page")
                            .max_height(370.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                Page::Type => self.type_page(ui, &model, state),
                                Page::Name => self.name_page(ui),
                                Page::Location => self.location_page(ui, &model, backend, next_id),
                            });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let index = PAGES.iter().position(|x| *x == self.page).unwrap_or(0);
                    // Reattaching: the type is the SR's, the name page is
                    // the first (`NewSrWizardNamePage.EnablePrevious`).
                    let first = if self.is_reattach() {
                        Page::Name
                    } else {
                        Page::Type
                    };
                    if ui
                        .add_enabled(
                            self.page != first && !self.probing(),
                            Button::new(s.previous),
                        )
                        .clicked()
                    {
                        self.page = PAGES[index - 1];
                    }
                    let ok = self.page_ok(&model, self.page);
                    if self.page == Page::Location {
                        if ui.add_enabled(ok, Button::new(s.finish)).clicked() {
                            self.start_finish(&model, backend, next_id);
                        }
                    } else if ui.add_enabled(ok, Button::new(s.next)).clicked() {
                        self.page = PAGES[index + 1];
                    }
                    if ui.button(s.cancel).clicked() {
                        cancelled = true;
                    }
                    if self.probing() {
                        ui.spinner();
                    }
                });
            });
        self.lun_choice_window(ctx, &model);
        self.warning_window(ctx);
        if !open || cancelled {
            return WizardOutcome::Cancelled;
        }
        self.outcome.take().unwrap_or(WizardOutcome::Open)
    }

    fn type_page(&mut self, ui: &mut Ui, model: &ConnectionModel, state: &AppState) {
        let s = sr_strings();
        ui.heading(s.type_title);
        ui.add_space(6.0);
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let reattach_kind = self
            .reattach
            .as_ref()
            .and_then(|r| model.srs.get(r))
            .and_then(|sr| SrKind::of_sr(sr));
        let mut chosen = None;
        let groups: [(&str, &[SrKind]); 2] = [
            (
                s.label_vdi_storage,
                &[
                    SrKind::Nfs,
                    SrKind::Iscsi,
                    SrKind::Smb,
                    SrKind::Local(LocalFs::Ext),
                ],
            ),
            (s.label_iso_library, &[SrKind::NfsIso, SrKind::SmbIso]),
        ];
        for (label, kinds) in groups {
            let shown: Vec<SrKind> = kinds
                .iter()
                .copied()
                .filter(|k| match k {
                    SrKind::Local(_) => LocalFs::ALL
                        .iter()
                        .any(|fs| storage::kind_available(model, SrKind::Local(*fs), show_hidden)),
                    k => storage::kind_available(model, *k, show_hidden),
                })
                .collect();
            if shown.is_empty() {
                continue;
            }
            ui.label(RichText::new(label).strong());
            for kind in shown {
                let enabled = reattach_kind.is_none_or(|r| r.same_page(kind));
                let selected = self.kind.same_page(kind);
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.radio(selected, kind_name(kind)).clicked() {
                        chosen = Some(kind);
                    }
                });
            }
            ui.add_space(4.0);
        }
        if let Some(kind) = chosen {
            self.choose(model, kind);
        }
        ui.separator();
        ui.label(RichText::new(kind_name(self.kind)).strong());
        ui.label(kind_blurb(self.kind));
    }

    fn name_page(&mut self, ui: &mut Ui) {
        let s = sr_strings();
        ui.heading(s.name_title);
        ui.add_space(6.0);
        ui.label(s.name_blurb);
        ui.add_space(6.0);
        Grid::new("new-sr-name").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.name_label);
            if ui
                .add(TextEdit::singleline(&mut self.name).desired_width(380.0))
                .labelled_by(label.id)
                .changed()
            {
                self.name_edited = true;
            }
            ui.end_row();
            let label = ui.label(s.description_label);
            ui.add_enabled(
                !self.auto_description,
                TextEdit::multiline(&mut self.description)
                    .desired_rows(3)
                    .desired_width(380.0),
            )
            .labelled_by(label.id);
            ui.end_row();
        });
        ui.checkbox(&mut self.auto_description, s.auto_description);
    }

    fn location_page(
        &mut self,
        ui: &mut Ui,
        model: &ConnectionModel,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) {
        let s = sr_strings();
        let title = match self.kind {
            SrKind::Nfs => s.path_nfs,
            SrKind::Iscsi => s.path_iscsi,
            SrKind::Smb => s.path_smb,
            SrKind::NfsIso => s.path_nfs_iso,
            SrKind::SmbIso => s.path_smb_iso,
            SrKind::Local(_) => s.path_local,
        };
        ui.heading(title);
        ui.add_space(6.0);
        match self.kind {
            SrKind::Nfs | SrKind::Smb => self.share_page(ui, backend, next_id),
            SrKind::NfsIso => self.nfs_iso_page(ui, model),
            SrKind::SmbIso => self.smb_iso_page(ui, model),
            SrKind::Iscsi => self.iscsi_page(ui, model, backend, next_id),
            SrKind::Local(_) => self.local_page(ui, model),
        }
        if let Some(error) = &self.probe_error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    /// NFS (`VHDoNFS`) and SMB (`CifsFrontend`).
    fn share_page(&mut self, ui: &mut Ui, backend: &BackendHandle, next_id: &mut u64) {
        let s = sr_strings();
        let nfs = self.kind == SrKind::Nfs;
        ui.label(s.share_blurb);
        ui.add_space(6.0);
        let mut changed = false;
        Grid::new("share-form").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.share_name);
            ui.vertical(|ui| {
                changed |= ui
                    .add(TextEdit::singleline(&mut self.share).desired_width(360.0))
                    .labelled_by(label.id)
                    .changed();
                ui.label(
                    RichText::new(if nfs { s.nfs_example } else { s.smb_example })
                        .weak()
                        .small(),
                );
            });
            ui.end_row();
            if nfs {
                let label = ui.label(s.advanced_options);
                changed |= ui
                    .add(TextEdit::singleline(&mut self.nfs_options).desired_width(360.0))
                    .labelled_by(label.id)
                    .changed();
                ui.end_row();
                ui.label(s.nfs_version);
                ui.horizontal(|ui| {
                    for (v, label) in [("3", s.nfs_v3), ("4", s.nfs_v4), ("4.1", s.nfs_v41)] {
                        ui.radio_value(&mut self.nfs_version, v, label);
                    }
                });
                ui.end_row();
            } else {
                let label = ui.label(s.username);
                changed |= ui
                    .add(TextEdit::singleline(&mut self.smb_user).desired_width(240.0))
                    .labelled_by(label.id)
                    .changed();
                ui.end_row();
                let label = ui.label(s.password);
                changed |= ui
                    .add(
                        TextEdit::singleline(&mut self.smb_password)
                            .password(true)
                            .desired_width(240.0),
                    )
                    .labelled_by(label.id)
                    .changed();
                ui.end_row();
            }
        });
        if changed {
            // `NfsServerPathTextBox_TextChanged`: the list is out of date.
            self.found = None;
            self.selected_uuid = None;
            self.reattach_existing = self.is_reattach();
            self.probe_error = None;
        }
        let valid = if nfs {
            storage::valid_nfs_share(&self.share)
        } else {
            storage::valid_cifs_share(self.share.trim())
        };
        ui.add_space(6.0);
        if ui
            .add_enabled(valid && !self.probing(), Button::new(s.scan))
            .clicked()
        {
            let sr_type = if nfs { "nfs" } else { "smb" };
            let check = Precheck::SrProbe {
                sr_type: sr_type.into(),
                device_config: DeviceConfig(self.scan_config()),
            };
            self.ask(backend, next_id, Probe::Scan, check);
        }
        ui.add_space(6.0);
        let create_allowed = !self.is_reattach() || self.kind.always_creates();
        ui.add_enabled_ui(create_allowed, |ui| {
            if ui.radio(!self.reattach_existing, s.create_new).clicked() {
                self.reattach_existing = false;
                self.selected_uuid = None;
            }
        });
        let found = self.found.clone().unwrap_or_default();
        ui.add_enabled_ui(!found.is_empty(), |ui| {
            if ui
                .radio(self.reattach_existing, s.reattach_existing)
                .clicked()
            {
                self.reattach_existing = true;
            }
        });
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(420.0);
            ui.set_min_height(60.0);
            match &self.found {
                Some(list) if list.is_empty() => {
                    ui.label(RichText::new(s.no_srs_found).weak());
                }
                Some(list) => {
                    for info in list {
                        // A reattach must pick the SR being reattached.
                        let allowed = self.reattach_uuid.as_ref().is_none_or(|u| *u == info.uuid);
                        let selected = self.selected_uuid.as_deref() == Some(info.uuid.as_str());
                        let clicked = ui
                            .add_enabled(allowed, egui::Button::selectable(selected, &info.uuid))
                            .clicked();
                        if clicked {
                            self.selected_uuid = Some(info.uuid.clone());
                            self.reattach_existing = true;
                        }
                    }
                }
                None => {}
            }
        });
    }

    /// `NFS_ISO`.
    fn nfs_iso_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let s = sr_strings();
        ui.label(s.nfs_iso_blurb);
        ui.add_space(6.0);
        Grid::new("nfs-iso-form").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.share_name);
            ui.vertical(|ui| {
                ui.add(TextEdit::singleline(&mut self.iso_share).desired_width(360.0))
                    .labelled_by(label.id);
                ui.label(RichText::new(s.nfs_example).weak().small());
            });
            ui.end_row();
            ui.label(s.nfs_version);
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.nfs_iso_v4, false, s.nfs_v3);
                ui.radio_value(&mut self.nfs_iso_v4, true, s.nfs_v4);
            });
            ui.end_row();
        });
        if let Some(error) = self.already_attached(model) {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    /// `CIFS_ISO`.
    fn smb_iso_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let s = sr_strings();
        ui.label(s.smb_iso_blurb);
        ui.add_space(6.0);
        Grid::new("smb-iso-form").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.share_name);
            ui.vertical(|ui| {
                ui.add(TextEdit::singleline(&mut self.iso_share).desired_width(360.0))
                    .labelled_by(label.id);
                ui.label(RichText::new(s.smb_iso_example).weak().small());
            });
            ui.end_row();
        });
        if let Some(error) = self.already_attached(model) {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.checkbox(&mut self.smb_iso_other_user, s.use_different_user);
        ui.add_enabled_ui(self.smb_iso_other_user, |ui| {
            Grid::new("smb-iso-login").num_columns(2).show(ui, |ui| {
                let label = ui.label(s.username);
                ui.add(TextEdit::singleline(&mut self.smb_iso_user).desired_width(240.0))
                    .labelled_by(label.id);
                ui.end_row();
                let label = ui.label(s.password);
                ui.add(
                    TextEdit::singleline(&mut self.smb_iso_password)
                        .password(true)
                        .desired_width(240.0),
                )
                .labelled_by(label.id);
                ui.end_row();
            });
        });
    }

    /// `LVMoISCSI`.
    fn iscsi_page(
        &mut self,
        ui: &mut Ui,
        model: &ConnectionModel,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) {
        let s = sr_strings();
        let error_color = ui.visuals().error_fg_color;
        let error_at = |w: &Self, at: IscsiErrorAt| {
            w.iscsi_error
                .as_ref()
                .filter(|(a, _)| *a == at)
                .map(|(_, t)| t.clone())
        };
        ui.label(s.iscsi_blurb);
        ui.add_space(6.0);
        let mut changed = false;
        let busy = self.probing();
        Grid::new("iscsi-form").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.target_host);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    changed |= ui
                        .add(TextEdit::singleline(&mut self.iscsi_host).desired_width(260.0))
                        .labelled_by(label.id)
                        .changed();
                    ui.label(":");
                    changed |= ui
                        .add(TextEdit::singleline(&mut self.iscsi_port).desired_width(50.0))
                        .changed();
                });
            });
            ui.end_row();
            if let Some(e) = error_at(self, IscsiErrorAt::Host) {
                ui.label("");
                ui.colored_label(error_color, e);
                ui.end_row();
            }
        });
        ui.add_enabled_ui(!busy, |ui| {
            changed |= ui.checkbox(&mut self.chap, s.use_chap).changed();
            ui.add_enabled_ui(self.chap, |ui| {
                Grid::new("iscsi-chap").num_columns(2).show(ui, |ui| {
                    let label = ui.label(s.chap_user);
                    changed |= ui
                        .add(TextEdit::singleline(&mut self.chap_user).desired_width(240.0))
                        .labelled_by(label.id)
                        .changed();
                    ui.end_row();
                    let label = ui.label(s.chap_password);
                    changed |= ui
                        .add(
                            TextEdit::singleline(&mut self.chap_password)
                                .password(true)
                                .desired_width(240.0),
                        )
                        .labelled_by(label.id)
                        .changed();
                    ui.end_row();
                });
            });
        });
        if let Some(e) = error_at(self, IscsiErrorAt::Chap) {
            ui.colored_label(error_color, e);
        }
        if changed {
            self.reset_iscsi();
        }
        let can_scan = !self.iscsi_host.trim().is_empty()
            && self.iscsi_port_value().is_some()
            && (!self.chap || !self.chap_user.is_empty())
            && !busy;
        if ui
            .add_enabled(can_scan, Button::new(s.scan_target))
            .clicked()
        {
            self.scan_iqns(backend, next_id);
        }
        if busy {
            let target = self.iscsi_host.trim();
            let text = match self.pending.map(|(_, p)| p) {
                Some(Probe::Iqns) => fill(s.scanning_iqns, &[("target", target)]),
                Some(Probe::Luns) => fill(s.scanning_luns, &[("target", target)]),
                _ => fill(s.scanning, &[("type", s.type_iscsi), ("target", target)]),
            };
            ui.label(RichText::new(text).weak());
        }
        ui.add_space(6.0);
        ui.label(RichText::new(s.iscsi_target_group).strong());
        let mut scan_luns = false;
        let mut lun_changed = false;
        Grid::new("iscsi-target").num_columns(2).show(ui, |ui| {
            ui.label(s.target_iqn);
            let iqns = self.iqns.clone().unwrap_or_default();
            ui.add_enabled_ui(!iqns.is_empty() && !busy, |ui| {
                let text = self
                    .selected_iqn()
                    .map(IqnInfo::label)
                    .unwrap_or_else(|| s.select_iqn.to_owned());
                ComboBox::from_id_salt("iscsi-iqn")
                    .selected_text(text)
                    .width(380.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.iqn.is_none(), s.select_iqn)
                            .clicked()
                        {
                            self.iqn = None;
                            self.luns = None;
                            self.lun = None;
                        }
                        for (i, iqn) in iqns.iter().enumerate() {
                            if ui
                                .selectable_label(self.iqn == Some(i), iqn.label())
                                .clicked()
                                && self.iqn != Some(i)
                            {
                                self.iqn = Some(i);
                                scan_luns = true;
                            }
                        }
                    });
            });
            ui.end_row();
            ui.label(s.target_lun);
            let luns = self.luns.clone().unwrap_or_default();
            ui.add_enabled_ui(!luns.is_empty() && !busy, |ui| {
                let text = self
                    .selected_lun()
                    .map(storage::lun_label)
                    .unwrap_or_else(|| s.select_lun.to_owned());
                ComboBox::from_id_salt("iscsi-lun")
                    .selected_text(text)
                    .width(380.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.lun.is_none(), s.select_lun)
                            .clicked()
                        {
                            self.lun = None;
                            lun_changed = true;
                        }
                        for (i, lun) in luns.iter().enumerate() {
                            if ui
                                .selectable_label(self.lun == Some(i), storage::lun_label(lun))
                                .clicked()
                            {
                                self.lun = Some(i);
                                lun_changed = true;
                            }
                        }
                    });
            });
            ui.end_row();
        });
        // One IQN: its LUNs are asked for at once (the original selects it).
        if self.iqn.is_some() && self.luns.is_none() && !busy && self.iscsi_error.is_none() {
            scan_luns = true;
        }
        if scan_luns {
            self.scan_luns(backend, next_id);
        }
        if lun_changed {
            self.check_lun(model);
        }
        for at in [IscsiErrorAt::Lun, IscsiErrorAt::Bottom] {
            if let Some(e) = error_at(self, at) {
                ui.colored_label(error_color, e);
            }
        }
    }

    /// `LocalStorageFrontend`.
    fn local_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let s = sr_strings();
        ui.label(s.local_blurb);
        ui.add_space(6.0);
        Grid::new("local-form").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.device_path);
            ui.add(TextEdit::singleline(&mut self.device).desired_width(300.0))
                .labelled_by(label.id);
            ui.end_row();
            ui.label(s.filesystem);
            ui.horizontal(|ui| {
                for (fs, label) in [
                    (LocalFs::Ext, s.fs_ext),
                    (LocalFs::Lvm, s.fs_lvm),
                    (LocalFs::Xfs, s.fs_xfs),
                ] {
                    let available = storage::kind_available(model, SrKind::Local(fs), true);
                    let reattach_ok = self
                        .reattach
                        .as_ref()
                        .and_then(|r| model.srs.get(r))
                        .is_none_or(|sr| sr.r#type == fs.sr_type());
                    ui.add_enabled_ui(available && reattach_ok, |ui| {
                        if ui.radio(self.kind == SrKind::Local(fs), label).clicked() {
                            self.kind = SrKind::Local(fs);
                        }
                    });
                }
            });
            ui.end_row();
        });
    }

    fn lun_choice_window(&mut self, ctx: &Context, model: &ConnectionModel) {
        let s = sr_strings();
        let Some(choice) = &self.lun_choice else {
            return;
        };
        let type_name = s.type_iscsi;
        let mut reattach = false;
        let mut format = false;
        let mut cancel = false;
        egui::Window::new(s.lvm_dialog_title)
            .id(egui::Id::unique("new-sr-lun-choice"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(
                    RichText::new(match &choice.found {
                        Some(_) => fill(s.lvm_found, &[("type", type_name)]),
                        None => s.lvm_none.to_owned(),
                    })
                    .strong(),
                );
                if let Some(found) = &choice.found {
                    ui.label(fill(
                        s.lvm_sr_details,
                        &[("size", &disk_size(found.size)), ("uuid", &found.uuid)],
                    ));
                }
                ui.add_space(6.0);
                ui.label(s.lvm_select);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(choice.found.is_some(), Button::new(s.lvm_reattach))
                        .clicked()
                    {
                        reattach = true;
                    }
                    ui.label(match choice.found {
                        Some(_) => fill(s.lvm_reattach_info, &[("type", type_name)]),
                        None => s.lvm_nothing_to_reattach.to_owned(),
                    });
                });
                if !choice.in_use {
                    ui.horizontal(|ui| {
                        if ui.button(s.lvm_format).clicked() {
                            format = true;
                        }
                        ui.label(fill(s.lvm_format_info, &[("type", type_name)]));
                    });
                }
                ui.add_space(6.0);
                let warning = if choice.in_use {
                    let pool = pool_name(model);
                    let sr = choice
                        .found
                        .as_ref()
                        .and_then(|f| model.srs.values().find(|x| x.uuid == f.uuid))
                        .map(|x| x.name_label.clone())
                        .unwrap_or_default();
                    fill(s.lun_in_use, &[("sr", &sr), ("pool", &pool)])
                } else {
                    fill(
                        s.lvm_warning,
                        &[("product", "XCP-ng"), ("console", branding::PRODUCT_NAME)],
                    )
                };
                ui.label(RichText::new(warning).weak());
                ui.add_space(6.0);
                if ui.button(s.cancel).clicked() {
                    cancel = true;
                }
            });
        if reattach {
            self.selected_uuid = choice.found.as_ref().map(|f| f.uuid.clone());
            self.lun_choice = None;
            self.finish(model);
        } else if format {
            self.selected_uuid = None;
            self.lun_choice = None;
            self.finish(model);
        } else if cancel {
            self.lun_choice = None;
        }
    }

    fn warning_window(&mut self, ctx: &Context) {
        let s = sr_strings();
        let Some(warning) = &self.warning else {
            return;
        };
        let mut yes = false;
        let mut no = false;
        egui::Window::new(s.lvm_dialog_title)
            .id(egui::Id::unique("new-sr-warning"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(&warning.text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(s.yes).clicked() {
                        yes = true;
                    }
                    if ui.button(s.no).clicked() {
                        no = true;
                    }
                });
            });
        if yes {
            let warning = self.warning.take().expect("shown");
            self.outcome = Some(WizardOutcome::Finish(
                self.connection,
                Box::new(warning.spec),
            ));
        } else if no {
            self.warning = None;
        }
    }

    /// A question is open (the LUN choice or the multi-pool warning).
    #[cfg(test)]
    pub fn has_question(&self) -> bool {
        self.warning.is_some() || self.lun_choice.is_some()
    }
}

/// `Helpers.GetName(connection)`.
fn pool_name(model: &ConnectionModel) -> String {
    model
        .pool()
        .map(|(_, p)| objects::pool_name(model, p))
        .unwrap_or_default()
}

/// The type page's name of a kind (`FrontendTypeName`).
pub fn kind_name(kind: SrKind) -> &'static str {
    let s = sr_strings();
    match kind {
        SrKind::Nfs => s.type_nfs,
        SrKind::Iscsi => s.type_iscsi,
        SrKind::Smb => s.type_smb,
        SrKind::Local(_) => s.type_local,
        SrKind::NfsIso => s.type_nfs_iso,
        SrKind::SmbIso => s.type_smb_iso,
    }
}

fn kind_blurb(kind: SrKind) -> &'static str {
    let s = sr_strings();
    match kind {
        SrKind::Nfs => s.blurb_nfs,
        SrKind::Iscsi => s.blurb_iscsi,
        SrKind::Smb => s.blurb_smb,
        SrKind::Local(_) => s.blurb_local,
        SrKind::NfsIso => s.blurb_nfs_iso,
        SrKind::SmbIso => s.blurb_smb_iso,
    }
}
