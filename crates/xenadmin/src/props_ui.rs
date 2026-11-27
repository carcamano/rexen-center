//! The Properties dialog of a pool or a server (`PropertiesDialog` with the
//! pages [`xen_core::properties::pages`] lists): a list of pages on the
//! left with each page's summary, the page on the right, OK and Cancel.
//!
//! OK checks every page (the first invalid one is shown with its message,
//! as `ShowLocalValidationMessages`), then saves only what changed as one
//! [`ActionRequest::SetProperties`] through [`OpsUi`]. Cancel sends nothing.
//!
//! Some pages read from the server when the dialog opens: NRPE (the `nrpe`
//! plugin), Clustering (the cluster's network), and Power On when it is
//! first shown (the passwords kept in secrets), as the original does.

use std::collections::BTreeMap;

use egui::{Context, Grid, RichText, ScrollArea, TextEdit, Ui};
use xapi::{Host, Network, Ref};
use xen_core::operations::PrecheckResult;
use xen_core::properties::{
    self, ClusteringChange, ClusteringProblem, EmailForm, MAIL_LANGUAGES, NRPE_CHECKS, NrpeConfig,
    NrpeProblem, Page, PowerOnMode, PowerOnProblem, PropertiesTarget, PropertyChanges,
    ThresholdError,
};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView,
    PerfmonObjectRef, Precheck,
};

use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::fill;
use crate::ops_ui::OpsUi;
use crate::perfmon_ui::{self, PerfmonDialog};
use crate::pool_strings::pool_strings;
use crate::pool_ui::window;

/// What the NRPE page shows.
#[derive(Debug, Clone, PartialEq)]
pub enum NrpeState {
    Retrieving(u64),
    Failed,
    Loaded(NrpeConfig),
}

/// The Power On page's servers, once their passwords were read.
pub struct PowerOnState {
    /// `None` while reading.
    pub pending: Option<u64>,
    /// Server, name, the mode as read, the mode as edited.
    pub hosts: Vec<(Ref<Host>, String, PowerOnMode, PowerOnMode)>,
    /// The server whose mode the page edits.
    pub selected: usize,
}

/// The dialog.
pub struct PropertiesDialog {
    connection: ConnectionId,
    pub target: PropertiesTarget,
    object_name: String,
    pub pages: Vec<Page>,
    pub page: Page,
    // General.
    pub name: String,
    pub description: String,
    pub tags: String,
    pub iqn: String,
    original: (String, String, Vec<String>, String),
    // Alerts.
    pub perfmon: Option<PerfmonDialog>,
    // Email Notifications.
    pub email: EmailForm,
    email_original: EmailForm,
    // Multipathing.
    pub multipath: bool,
    multipath_original: bool,
    // Log Destination.
    pub log_remote: bool,
    pub log_server: String,
    log_original: Option<String>,
    // Power On.
    pub power: Option<PowerOnState>,
    // Auto-start.
    pub autostart: bool,
    autostart_original: bool,
    // Live Patching.
    pub live_patching_disabled: bool,
    live_patching_original: bool,
    // Network (IGMP snooping).
    pub igmp: bool,
    igmp_original: bool,
    // Clustering.
    pub clustering: bool,
    clustering_original: bool,
    pub cluster_network: Option<Ref<Network>>,
    cluster_pending: Option<u64>,
    // Advanced.
    pub compression: bool,
    compression_original: bool,
    // NRPE.
    pub nrpe_state: NrpeState,
    pub nrpe: NrpeConfig,
    /// Why OK did not save (the page's message).
    pub problem: Option<String>,
    /// The General page's folder and the Custom Fields page (F13).
    pub org: crate::org_ui::OrgEditor,
    /// A custom field dialog to open (the window shows it).
    pub org_dialog: Option<crate::org_ui::OrgDialog>,
    /// The GPU page (F14a).
    pub gpu: Option<crate::dev_ui::PoolGpuPage>,
}

/// The height of the page list and of the page (both scroll).
const PAGE_HEIGHT: f32 = 400.0;

fn next(next_id: &mut u64) -> u64 {
    *next_id += 1;
    (1 << 51) + *next_id
}

impl PropertiesDialog {
    /// The dialog for `target`, asking the server what its pages read.
    pub fn open(
        conn: &ConnectionView,
        target: PropertiesTarget,
        backend: &BackendHandle,
        next_id: &mut u64,
    ) -> Option<Self> {
        let m = &conn.model;
        let (_, pool) = m.pool()?;
        let pool = pool.clone();
        let pages = properties::pages(m, &target, conn.rbac.as_deref());
        let (object_name, name, description, tags, iqn, host) = match &target {
            PropertiesTarget::Pool(_) => (
                crate::pool_ui::connection_name(conn),
                pool.name_label.clone(),
                pool.name_description.clone(),
                pool.tags.clone(),
                String::new(),
                None,
            ),
            PropertiesTarget::Host(h) => {
                let rec = m.hosts.get(h)?;
                (
                    rec.name_label.clone(),
                    rec.name_label.clone(),
                    rec.name_description.clone(),
                    rec.tags.clone(),
                    properties::host_iqn(rec).to_owned(),
                    Some(rec.clone()),
                )
            }
        };
        let log_original = host
            .as_ref()
            .and_then(|h| properties::syslog_destination(h).map(str::to_owned));
        let multipath = host
            .as_ref()
            .is_some_and(|h| properties::multipath_enabled(h));
        let email = EmailForm::of(&pool);
        let clustering = !m.clusters.is_empty();
        let mut dialog = Self {
            connection: conn.id,
            object_name,
            page: Page::General,
            tags: tags.join(", "),
            original: (name.clone(), description.clone(), tags, iqn.clone()),
            name,
            description,
            iqn,
            perfmon: match &target {
                PropertiesTarget::Host(h) => {
                    PerfmonDialog::open(conn, PerfmonObjectRef::Host(h.clone()))
                }
                PropertiesTarget::Pool(_) => None,
            },
            email_original: email.clone(),
            email,
            multipath,
            multipath_original: multipath,
            log_remote: log_original.is_some(),
            log_server: log_original.clone().unwrap_or_default(),
            log_original,
            power: None,
            autostart: properties::autostart_enabled(&pool),
            autostart_original: properties::autostart_enabled(&pool),
            live_patching_disabled: pool.live_patching_disabled,
            live_patching_original: pool.live_patching_disabled,
            igmp: pool.igmp_snooping_enabled,
            igmp_original: pool.igmp_snooping_enabled,
            clustering,
            clustering_original: clustering,
            cluster_network: None,
            cluster_pending: None,
            compression: pool.migration_compression,
            compression_original: pool.migration_compression,
            nrpe_state: NrpeState::Failed,
            nrpe: NrpeConfig::default(),
            problem: None,
            org: {
                let object = match &target {
                    PropertiesTarget::Pool(p) => xen_core::org::OrgObject::Pool(p.clone()),
                    PropertiesTarget::Host(h) => xen_core::org::OrgObject::Host(h.clone()),
                };
                let mut e = crate::org_ui::OrgEditor::new(conn, object);
                e.edit_tags = false;
                e
            },
            org_dialog: None,
            gpu: pages
                .contains(&Page::Gpu)
                .then(|| crate::dev_ui::PoolGpuPage::new(conn, &target)),
            target,
            pages,
        };
        if dialog.pages.contains(&Page::Nrpe) {
            let host = match &dialog.target {
                PropertiesTarget::Host(h) => Some(h.clone()),
                PropertiesTarget::Pool(_) => m.pool().map(|(_, p)| p.master.clone()),
            };
            if let Some(host) = host {
                let id = next(next_id);
                dialog.nrpe_state = NrpeState::Retrieving(id);
                backend.send(Command::Precheck {
                    connection: conn.id,
                    id,
                    check: Precheck::Nrpe { host },
                });
            }
        }
        if dialog.pages.contains(&Page::Clustering)
            && let Some((cluster, _)) = m.clusters.iter().next()
        {
            let id = next(next_id);
            dialog.cluster_pending = Some(id);
            backend.send(Command::Precheck {
                connection: conn.id,
                id,
                check: Precheck::ClusterNetwork {
                    cluster: cluster.clone(),
                },
            });
        }
        Some(dialog)
    }

    /// Takes the answer of a precheck this dialog asked.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        match result {
            PrecheckResult::Nrpe(r) if self.nrpe_state == NrpeState::Retrieving(id) => {
                match r {
                    Ok(config) => {
                        self.nrpe = config.clone();
                        self.nrpe_state = NrpeState::Loaded(config.clone());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "could not read the NRPE configuration");
                        self.nrpe_state = NrpeState::Failed;
                    }
                }
                true
            }
            PrecheckResult::ClusterNetwork(n) if self.cluster_pending == Some(id) => {
                self.cluster_pending = None;
                self.cluster_network = n.clone();
                true
            }
            PrecheckResult::PowerOnSecrets(secrets)
                if self.power.as_ref().is_some_and(|p| p.pending == Some(id)) =>
            {
                let p = self.power.as_mut().expect("checked");
                p.pending = None;
                for (host, _, read, edited) in &mut p.hosts {
                    if let Some((_, secret)) = secrets.iter().find(|(h, _)| h == host) {
                        let fill_secret = |m: &mut PowerOnMode| match m {
                            PowerOnMode::Drac { password, .. } => *password = secret.clone(),
                            PowerOnMode::Custom { config, .. } => {
                                if let Some(v) =
                                    config.get_mut(properties::POWER_ON_PASSWORD_SECRET)
                                {
                                    *v = secret.clone();
                                }
                            }
                            _ => {}
                        };
                        fill_secret(read);
                        fill_secret(edited);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// `HostPowerONEditPage.LoadPowerOnMode`: read once, when the page is
    /// first shown.
    fn load_power_on(&mut self, conn: &ConnectionView, backend: &BackendHandle, next_id: &mut u64) {
        if self.power.is_some() {
            return;
        }
        let hosts: Vec<(Ref<Host>, String, PowerOnMode, PowerOnMode)> =
            properties::power_on_hosts(&conn.model, &self.target)
                .into_iter()
                .map(|(r, h)| {
                    let mode = PowerOnMode::of(h, None);
                    (r.clone(), h.name_label.clone(), mode.clone(), mode)
                })
                .collect();
        let with_secret: Vec<Ref<Host>> = hosts
            .iter()
            .filter(|(r, ..)| {
                conn.model.hosts.get(r).is_some_and(|h| {
                    h.power_on_config
                        .contains_key(properties::POWER_ON_PASSWORD_SECRET)
                })
            })
            .map(|(r, ..)| r.clone())
            .collect();
        let pending = (!with_secret.is_empty()).then(|| {
            let id = next(next_id);
            backend.send(Command::Precheck {
                connection: conn.id,
                id,
                check: Precheck::PowerOnSecrets { hosts: with_secret },
            });
            id
        });
        self.power = Some(PowerOnState {
            pending,
            hosts,
            selected: 0,
        });
    }

    /// The page's title.
    pub fn page_title(page: Page) -> &'static str {
        let s = pool_strings();
        match page {
            Page::General => s.page_general,
            Page::CustomFields => crate::org_strings::org_strings().page_custom_fields,
            Page::Alerts => s.page_alerts,
            Page::EmailOptions => s.page_email,
            Page::Multipath => s.page_multipath,
            Page::LogDestination => s.page_log,
            Page::PowerOn => s.page_power_on,
            Page::Autostart => s.page_autostart,
            Page::Gpu => crate::dev_strings::dev_strings().page_gpu,
            Page::LivePatching => s.page_live_patching,
            Page::NetworkOptions => s.page_network,
            Page::Clustering => s.page_clustering,
            Page::Advanced => s.page_advanced,
            Page::Nrpe => s.page_nrpe,
        }
    }

    /// The page's summary under its title (`SubText`).
    pub fn subtext(&self, page: Page) -> String {
        let s = pool_strings();
        let on_off = |b: bool| if b { s.enabled } else { s.disabled }.to_owned();
        match page {
            Page::General => self.name.clone(),
            Page::CustomFields => self.org.custom_fields_subtext(),
            Page::Alerts => self
                .perfmon
                .as_ref()
                .map(|p| perfmon_ui::summary(&p.form))
                .unwrap_or_default(),
            Page::EmailOptions => {
                if self.email.enabled {
                    self.email.address.clone()
                } else {
                    s.none_defined.to_owned()
                }
            }
            Page::Multipath => if self.multipath {
                s.multipath_active
            } else {
                s.multipath_not_active
            }
            .to_owned(),
            Page::LogDestination => {
                if self.log_remote {
                    fill(
                        s.log_local_and_remote,
                        &[("server", self.log_server.trim())],
                    )
                } else {
                    s.log_local.to_owned()
                }
            }
            Page::PowerOn => match &self.power {
                Some(p) => {
                    let mut modes: Vec<String> = p.hosts.iter().map(|h| mode_label(&h.3)).collect();
                    modes.dedup();
                    match modes.len() {
                        0 => String::new(),
                        1 => modes.remove(0),
                        _ => s.power_mixed.to_owned(),
                    }
                }
                None => String::new(),
            },
            Page::Autostart => on_off(self.autostart),
            Page::Gpu => self.gpu.as_ref().map(|g| g.subtext()).unwrap_or_default(),
            Page::LivePatching => on_off(!self.live_patching_disabled),
            Page::NetworkOptions => if self.igmp {
                s.igmp_enabled
            } else {
                s.igmp_disabled
            }
            .to_owned(),
            Page::Clustering => on_off(self.clustering),
            Page::Advanced => if self.compression {
                s.compression_on
            } else {
                s.compression_off
            }
            .to_owned(),
            Page::Nrpe => s.nrpe_subtext.to_owned(),
        }
    }

    /// The first page that cannot be saved, with its message.
    pub fn invalid(&self, conn: &ConnectionView) -> Option<(Page, String)> {
        let s = pool_strings();
        if self.pages.contains(&Page::General) {
            if self.name.trim().is_empty() {
                return Some((Page::General, s.invalid_name.to_owned()));
            }
            if matches!(self.target, PropertiesTarget::Host(_))
                && self.iqn.trim() != self.original.3
                && !properties::is_valid_iqn(self.iqn.trim())
            {
                return Some((Page::General, s.invalid_iqn.to_owned()));
            }
        }
        if !self.org.valid() {
            return Some((
                Page::CustomFields,
                crate::org_strings::org_strings()
                    .field_date_invalid
                    .to_owned(),
            ));
        }
        if let Some(p) = &self.perfmon
            && !p.form.is_valid()
        {
            return Some((
                Page::Alerts,
                crate::perf_strings::perf_strings()
                    .pm_interval_multiple
                    .to_owned(),
            ));
        }
        if self.pages.contains(&Page::EmailOptions) && !self.email.is_valid() {
            return Some((Page::EmailOptions, s.email_invalid.to_owned()));
        }
        if self.pages.contains(&Page::LogDestination)
            && self.log_remote
            && !properties::is_valid_log_server(self.log_server.trim())
        {
            return Some((Page::LogDestination, s.log_invalid.to_owned()));
        }
        if let Some(p) = &self.power {
            for (_, name, _, mode) in &p.hosts {
                if let Some(problem) = mode.problem() {
                    let text = match problem {
                        PowerOnProblem::InvalidIp => s.power_invalid_ip,
                        PowerOnProblem::NoScript => s.power_no_script,
                    };
                    let text = if p.hosts.len() > 1 {
                        format!("{name}: {text}")
                    } else {
                        text.to_owned()
                    };
                    return Some((Page::PowerOn, text));
                }
            }
        }
        if self.pages.contains(&Page::Clustering)
            && self.clustering
            && !self.clustering_original
            && self.cluster_network.is_none()
        {
            return Some((Page::Clustering, s.clustering_select_network.to_owned()));
        }
        if let NrpeState::Loaded(_) = &self.nrpe_state
            && let Some(problem) = properties::nrpe_problem(&self.nrpe)
        {
            return Some((Page::Nrpe, nrpe_problem_text(&problem)));
        }
        let _ = conn;
        None
    }

    /// What OK saves.
    pub fn changes(&self, conn: &ConnectionView) -> PropertyChanges {
        let mut c = PropertyChanges::default();
        let (name, description, tags, iqn) = &self.original;
        if self.name.trim() != name && !self.name.trim().is_empty() {
            c.name = Some(self.name.trim().to_owned());
        }
        if &self.description != description {
            c.description = Some(self.description.clone());
        }
        let new_tags = properties::parse_tags(&self.tags);
        let (mut a, mut b) = (new_tags.clone(), tags.clone());
        a.sort();
        b.sort();
        if a != b {
            c.tags = Some(new_tags);
        }
        if matches!(self.target, PropertiesTarget::Host(_)) && self.iqn.trim() != iqn {
            c.iqn = Some(self.iqn.trim().to_owned());
        }
        if let Some(p) = &self.perfmon
            && let Some(ActionRequest::SetPerfmon { config, .. }) = p.request()
        {
            c.perfmon = Some(config);
        }
        if self.pages.contains(&Page::EmailOptions) && self.email != self.email_original {
            c.email = Some(self.email.settings());
        }
        if self.pages.contains(&Page::Multipath) && self.multipath != self.multipath_original {
            c.multipath = Some(self.multipath);
        }
        if self.pages.contains(&Page::LogDestination) {
            let new = self.log_remote.then(|| self.log_server.trim().to_owned());
            if new != self.log_original {
                c.syslog = Some(new);
            }
        }
        if let Some(p) = &self.power {
            c.power_on = p
                .hosts
                .iter()
                .filter(|(_, _, read, edited)| read != edited)
                .map(|(h, _, _, edited)| (h.clone(), edited.clone()))
                .collect();
        }
        if self.pages.contains(&Page::Autostart) && self.autostart != self.autostart_original {
            c.autostart = Some(self.autostart);
        }
        if self.pages.contains(&Page::LivePatching)
            && self.live_patching_disabled != self.live_patching_original
        {
            c.live_patching_disabled = Some(self.live_patching_disabled);
        }
        if self.pages.contains(&Page::NetworkOptions) && self.igmp != self.igmp_original {
            c.igmp_snooping = Some(self.igmp);
        }
        if self.pages.contains(&Page::Clustering) && self.clustering != self.clustering_original {
            c.clustering = if self.clustering {
                self.cluster_network.clone().map(ClusteringChange::Enable)
            } else {
                Some(ClusteringChange::Disable)
            };
        }
        if self.pages.contains(&Page::Advanced) && self.compression != self.compression_original {
            c.migration_compression = Some(self.compression);
        }
        if let NrpeState::Loaded(original) = &self.nrpe_state
            && self.nrpe != *original
        {
            c.nrpe = Some((self.nrpe.clone(), original.clone()));
        }
        let _ = conn;
        c
    }

    /// Draws the dialog; returns whether it closes.
    pub fn window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let Some(conn) = state.connection(self.connection).cloned() else {
            return true;
        };
        let s = pool_strings();
        let (mut ok, mut cancel) = (false, false);
        if self.page == Page::PowerOn {
            let mut id = 1 << 32;
            self.load_power_on(&conn, backend, &mut id);
        }
        window(fill(s.props_title, &[("name", &self.object_name)])).show(ctx, |ui| {
            ui.set_width(740.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    ScrollArea::vertical()
                        .id_salt("props-pages")
                        .max_height(PAGE_HEIGHT)
                        .show(ui, |ui| {
                            for page in self.pages.clone() {
                                let text =
                                    format!("{}\n{}", Self::page_title(page), self.subtext(page));
                                if ui.selectable_label(self.page == page, text).clicked() {
                                    self.page = page;
                                    self.problem = None;
                                }
                            }
                        });
                });
                ui.add_space(12.0); // a separator would stretch to the screen height
                ui.vertical(|ui| {
                    ui.set_width(500.0);
                    ScrollArea::vertical()
                        .id_salt("props-page")
                        .max_height(PAGE_HEIGHT)
                        .show(ui, |ui| {
                            self.page_ui(ui, &conn);
                        });
                });
            });
            if let Some(p) = &self.problem {
                ui.colored_label(ui.visuals().error_fg_color, p);
            }
            ui.add_space(6.0);
            dialog_buttons(ui, |row| {
                ok = row.button(ButtonRole::Primary, s.ok).clicked();
                cancel = row.button(ButtonRole::Cancel, s.cancel).clicked();
            });
        });
        if cancel {
            return true;
        }
        if ok {
            if let Some((page, why)) = self.invalid(&conn) {
                self.page = page;
                self.problem = Some(why);
                return false;
            }
            let changes = self.changes(&conn);
            if !changes.is_empty() {
                ops.request(
                    &conn,
                    ActionRequest::SetProperties {
                        target: self.target.clone(),
                        changes: Box::new(changes),
                    },
                    backend,
                );
            }
            for request in self.org.requests(&conn) {
                ops.request(&conn, request, backend);
            }
            for request in self.gpu.iter().flat_map(|g| g.requests()) {
                ops.request(&conn, request, backend);
            }
            return true;
        }
        false
    }

    fn page_ui(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = pool_strings();
        ui.heading(Self::page_title(self.page));
        ui.add_space(4.0);
        match self.page {
            Page::General => {
                ui.label(fill(
                    s.general_blurb,
                    &[("console", branding::PRODUCT_NAME)],
                ));
                ui.add_space(6.0);
                Grid::new("props-general").num_columns(2).show(ui, |ui| {
                    let label = ui.label(crate::pool_strings::pool_strings().name_label);
                    ui.add(TextEdit::singleline(&mut self.name).desired_width(320.0))
                        .labelled_by(label.id);
                    ui.end_row();
                    ui.label(s.description_label);
                    ui.add(
                        TextEdit::multiline(&mut self.description)
                            .desired_rows(2)
                            .desired_width(320.0),
                    );
                    ui.end_row();
                    if matches!(self.target, PropertiesTarget::Host(_)) {
                        let label = ui.label(s.general_iqn);
                        ui.add(TextEdit::singleline(&mut self.iqn).desired_width(320.0))
                            .labelled_by(label.id);
                        ui.end_row();
                        ui.label("");
                        ui.label(RichText::new(s.general_iqn_hint).weak());
                        ui.end_row();
                    }
                    ui.label(s.general_tags);
                    ui.add(
                        TextEdit::singleline(&mut self.tags)
                            .desired_width(320.0)
                            .hint_text(s.general_tags_hint),
                    );
                    ui.end_row();
                    self.org.general_rows(ui, conn);
                });
            }
            Page::CustomFields => {
                self.org.custom_fields_page(ui, conn, &mut self.org_dialog);
            }
            Page::Alerts => {
                if let Some(p) = self.perfmon.as_mut() {
                    perfmon_ui::form_ui(ui, &mut p.form);
                }
            }
            Page::EmailOptions => email_page(ui, &mut self.email),
            Page::Multipath => {
                let editable = match &self.target {
                    PropertiesTarget::Host(h) => conn
                        .model
                        .hosts
                        .get(h)
                        .is_some_and(|h| properties::multipath_editable(&conn.model, h)),
                    PropertiesTarget::Pool(_) => false,
                };
                ui.label(s.multipath_blurb);
                ui.add_space(6.0);
                if !editable {
                    ui.colored_label(ui.visuals().warn_fg_color, s.multipath_maintenance);
                }
                ui.add_enabled(
                    editable,
                    egui::Checkbox::new(&mut self.multipath, s.multipath_enable),
                );
            }
            Page::LogDestination => {
                ui.strong(s.log_title);
                ui.label(fill(s.log_blurb, &[("product", branding::PLATFORM_NAME)]));
                ui.add_space(6.0);
                ui.checkbox(&mut self.log_remote, s.log_remote);
                ui.horizontal(|ui| {
                    ui.label(s.log_server);
                    let r = ui.add(TextEdit::singleline(&mut self.log_server).desired_width(280.0));
                    if r.gained_focus() {
                        // `ServerTextBox_Enter`.
                        self.log_remote = true;
                    }
                });
            }
            Page::PowerOn => self.power_page(ui),
            Page::Autostart => {
                ui.checkbox(&mut self.autostart, s.autostart_enable);
                ui.label(RichText::new(s.autostart_blurb).weak());
            }
            Page::LivePatching => {
                ui.label(s.live_patching_blurb);
                ui.radio_value(
                    &mut self.live_patching_disabled,
                    false,
                    s.live_patching_enable,
                );
                ui.radio_value(
                    &mut self.live_patching_disabled,
                    true,
                    s.live_patching_disable,
                );
            }
            Page::NetworkOptions => {
                ui.label(s.igmp_blurb);
                ui.radio_value(&mut self.igmp, true, s.igmp_enable);
                ui.radio_value(&mut self.igmp, false, s.igmp_disable);
                ui.label(RichText::new(s.igmp_warning).weak());
            }
            Page::Clustering => self.clustering_page(ui, conn),
            Page::Advanced => {
                ui.strong(s.advanced_group);
                ui.checkbox(&mut self.compression, s.advanced_compression);
                ui.label(RichText::new(s.advanced_blurb).weak());
                ui.label(RichText::new(s.advanced_note).weak());
            }
            Page::Nrpe => self.nrpe_page(ui),
            Page::Gpu => {
                if let Some(g) = self.gpu.as_mut() {
                    g.ui(ui);
                }
            }
        }
    }

    fn power_page(&mut self, ui: &mut Ui) {
        let s = pool_strings();
        let pool = matches!(self.target, PropertiesTarget::Pool(_));
        ui.label(if pool {
            s.power_pool_blurb
        } else {
            s.power_server_blurb
        });
        let Some(p) = self.power.as_mut() else {
            return;
        };
        if p.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(s.power_loading);
            });
            return;
        }
        if pool {
            Grid::new("power-hosts")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(s.power_server);
                    ui.strong(s.power_mode);
                    ui.end_row();
                    for (i, (_, name, _, mode)) in p.hosts.iter().enumerate() {
                        if ui.selectable_label(p.selected == i, name).clicked() {
                            p.selected = i;
                        }
                        ui.label(mode_label(mode));
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
        }
        let Some((_, _, _, mode)) = p.hosts.get_mut(p.selected) else {
            return;
        };
        ui.strong(s.power_mode);
        let kind = mode_kind(mode);
        let mut chosen = kind;
        ui.radio_value(&mut chosen, 0, s.power_disabled);
        ui.radio_value(&mut chosen, 1, s.power_wol);
        ui.radio_value(&mut chosen, 2, s.power_drac);
        ui.radio_value(&mut chosen, 3, s.power_custom);
        if chosen != kind {
            *mode = match chosen {
                0 => PowerOnMode::Disabled,
                1 => PowerOnMode::WakeOnLan,
                2 => PowerOnMode::Drac {
                    ip: String::new(),
                    user: String::new(),
                    password: String::new(),
                },
                _ => PowerOnMode::Custom {
                    mode: String::new(),
                    config: BTreeMap::new(),
                },
            };
        }
        ui.add_space(4.0);
        ui.strong(s.power_options);
        match mode {
            PowerOnMode::Drac { ip, user, password } => {
                Grid::new("power-drac").num_columns(2).show(ui, |ui| {
                    ui.label(s.power_ip);
                    ui.text_edit_singleline(ip);
                    ui.end_row();
                    ui.label(s.power_user);
                    ui.text_edit_singleline(user);
                    ui.end_row();
                    let label = ui.label(s.power_password);
                    ui.add(TextEdit::singleline(password).password(true))
                        .labelled_by(label.id);
                    ui.end_row();
                });
            }
            PowerOnMode::Custom {
                mode: script,
                config,
            } => {
                ui.text_edit_singleline(script);
                Grid::new("power-custom").num_columns(2).show(ui, |ui| {
                    ui.strong(s.power_key);
                    ui.strong(s.power_value);
                    ui.end_row();
                    let keys: Vec<String> = config.keys().cloned().collect();
                    for k in keys {
                        ui.label(&k);
                        if let Some(v) = config.get_mut(&k) {
                            ui.text_edit_singleline(v);
                        }
                        ui.end_row();
                    }
                });
                if ui.small_button(s.power_add_row).clicked() {
                    config.insert(format!("key{}", config.len() + 1), String::new());
                }
            }
            _ => {}
        }
    }

    fn clustering_page(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = pool_strings();
        ui.label(s.clustering_blurb);
        ui.add_space(4.0);
        let problem = properties::clustering_problem(&conn.model);
        let disabled = problem
            .is_some_and(|p| p != ClusteringProblem::Gfs2Attached || self.clustering_original);
        let networks = properties::cluster_networks(&conn.model);
        ui.add_enabled(
            !disabled,
            egui::Checkbox::new(&mut self.clustering, s.clustering_enable),
        );
        ui.horizontal(|ui| {
            ui.label(s.clustering_network);
            let shown = self
                .cluster_network
                .as_ref()
                .and_then(|n| networks.iter().find(|(r, _)| *r == n))
                .map(|(_, name)| name.clone())
                .unwrap_or_default();
            ui.add_enabled_ui(!disabled && !self.clustering_original, |ui| {
                egui::ComboBox::from_id_salt("cluster-network")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (r, name) in &networks {
                            if ui
                                .selectable_value(
                                    &mut self.cluster_network,
                                    Some((*r).clone()),
                                    name,
                                )
                                .clicked()
                                && !self.clustering_original
                            {
                                self.clustering = true;
                            }
                        }
                    });
            });
        });
        if let Some(p) = problem {
            let text = match p {
                ClusteringProblem::NoNetwork => s.clustering_no_network,
                ClusteringProblem::Gfs2Attached => s.clustering_gfs2,
                ClusteringProblem::HaEnabled => s.clustering_ha,
                ClusteringProblem::SecretRotationPending => s.clustering_psr,
            };
            ui.colored_label(ui.visuals().warn_fg_color, text);
        }
        if self.clustering_original
            && self.cluster_pending.is_none()
            && self.cluster_network.is_none()
        {
            ui.colored_label(ui.visuals().warn_fg_color, s.clustering_inconsistent);
        }
        ui.label(RichText::new(s.clustering_info).weak());
        if conn.model.hosts.len() < 3 {
            ui.label(RichText::new(s.clustering_host_count).weak());
        }
    }

    fn nrpe_page(&mut self, ui: &mut Ui) {
        let s = pool_strings();
        ui.label(if matches!(self.target, PropertiesTarget::Pool(_)) {
            s.nrpe_pool_blurb
        } else {
            s.nrpe_host_blurb
        });
        ui.add_space(4.0);
        match &self.nrpe_state {
            NrpeState::Retrieving(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(s.nrpe_retrieving);
                });
                return;
            }
            NrpeState::Failed => {
                ui.colored_label(ui.visuals().error_fg_color, s.nrpe_failed);
                return;
            }
            NrpeState::Loaded(_) => {}
        }
        let n = &mut self.nrpe;
        ui.checkbox(&mut n.enabled, s.nrpe_enable);
        ui.add_enabled_ui(n.enabled, |ui| {
            ui.strong(s.nrpe_general);
            ui.horizontal(|ui| {
                ui.label(s.nrpe_allow_hosts);
                ui.add(
                    TextEdit::singleline(&mut n.allow_hosts)
                        .desired_width(320.0)
                        .hint_text(s.nrpe_allow_hosts_placeholder),
                );
            });
            ui.checkbox(&mut n.debug, s.nrpe_debug);
            ui.checkbox(&mut n.ssl_logging, s.nrpe_ssl);
            Grid::new("nrpe-checks")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(s.nrpe_metric);
                    ui.strong(s.nrpe_warning);
                    ui.strong(s.nrpe_critical);
                    ui.end_row();
                    for (name, kind) in NRPE_CHECKS {
                        let entry = n
                            .thresholds
                            .entry((*name).to_owned())
                            .or_insert_with(|| (String::new(), String::new()));
                        let label = ui.label(check_label(name)).on_hover_text(*name);
                        ui.add(TextEdit::singleline(&mut entry.0).desired_width(110.0))
                            .labelled_by(label.id);
                        ui.add(TextEdit::singleline(&mut entry.1).desired_width(110.0));
                        if let Some(e) = properties::threshold_problem(*kind, &entry.0, &entry.1) {
                            ui.colored_label(ui.visuals().error_fg_color, threshold_text(e));
                        }
                        ui.end_row();
                    }
                });
        });
    }
}

fn email_page(ui: &mut Ui, e: &mut EmailForm) {
    let s = pool_strings();
    ui.label(s.email_blurb);
    ui.checkbox(&mut e.enabled, s.email_send);
    ui.add_enabled_ui(e.enabled, |ui| {
        ui.strong(s.email_delivery);
        Grid::new("props-email").num_columns(2).show(ui, |ui| {
            let label = ui.label(s.email_address);
            ui.add(TextEdit::singleline(&mut e.address).desired_width(280.0))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(s.email_server);
            ui.add(TextEdit::singleline(&mut e.server).desired_width(280.0))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(s.email_port);
            ui.add(TextEdit::singleline(&mut e.port).desired_width(80.0))
                .labelled_by(label.id);
            ui.end_row();
            ui.label(s.email_language);
            egui::ComboBox::from_id_salt("mail-language")
                .selected_text(language_name(e.language))
                .show_ui(ui, |ui| {
                    for i in 0..MAIL_LANGUAGES.len() {
                        ui.selectable_value(&mut e.language, i, language_name(i));
                    }
                });
            ui.end_row();
        });
    });
}

fn language_name(i: usize) -> &'static str {
    match i {
        1 => "Chinese (Simplified)",
        2 => "Japanese (Japan)",
        _ => "English (United States)",
    }
}

fn mode_kind(mode: &PowerOnMode) -> u8 {
    match mode {
        PowerOnMode::Disabled => 0,
        PowerOnMode::WakeOnLan => 1,
        PowerOnMode::Drac { .. } => 2,
        PowerOnMode::Custom { .. } => 3,
    }
}

/// `PowerOnMode.FriendlyName`.
pub fn mode_label(mode: &PowerOnMode) -> String {
    let s = pool_strings();
    match mode {
        PowerOnMode::Disabled => s.power_disabled.to_owned(),
        PowerOnMode::WakeOnLan => s.power_wol_short.to_owned(),
        PowerOnMode::Drac { .. } => "DRAC".to_owned(),
        PowerOnMode::Custom { mode, .. } => mode.clone(),
    }
}

fn check_label(name: &str) -> &'static str {
    let s = pool_strings();
    match name {
        "check_host_load" => s.nrpe_check_host_load,
        "check_host_cpu" => s.nrpe_check_host_cpu,
        "check_host_memory" => s.nrpe_check_host_memory,
        "check_vgpu" => s.nrpe_check_vgpu,
        "check_vgpu_memory" => s.nrpe_check_vgpu_memory,
        "check_load" => s.nrpe_check_load,
        "check_cpu" => s.nrpe_check_cpu,
        "check_memory" => s.nrpe_check_memory,
        "check_swap" => s.nrpe_check_swap,
        "check_disk_root" => s.nrpe_check_disk_root,
        _ => s.nrpe_check_disk_log,
    }
}

fn threshold_text(e: ThresholdError) -> &'static str {
    let s = pool_strings();
    match e {
        ThresholdError::Empty => s.nrpe_empty,
        ThresholdError::NotANumber => s.nrpe_number,
        ThresholdError::OutOfRange => s.nrpe_range,
        ThresholdError::NotThreeNumbers => s.nrpe_three,
        ThresholdError::WarningNotBelowCritical => s.nrpe_less,
        ThresholdError::WarningNotAboveCritical => s.nrpe_greater,
    }
}

fn nrpe_problem_text(p: &NrpeProblem) -> String {
    let s = pool_strings();
    match p {
        NrpeProblem::AllowHostsEmpty => s.nrpe_hosts_empty.to_owned(),
        NrpeProblem::AllowHostsFormat => s.nrpe_hosts_format.to_owned(),
        NrpeProblem::AllowHostsDuplicate => s.nrpe_hosts_duplicate.to_owned(),
        NrpeProblem::Threshold { name, error } => {
            format!("{}: {}", check_label(name), threshold_text(*error))
        }
    }
}
