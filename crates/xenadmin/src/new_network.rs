//! The New Network wizard (`NewNetworkWizard`): Select Type, then for an
//! external, single-server or cross-server private network the Name and
//! Network settings pages, for a bonded network the Bond Members page, as
//! the original lays them out. Also the bond members form the NICs tab's
//! Create Bond dialog shares (`BondDetails`).
//!
//! SR-IOV networks are not offered. The original hides the cross-server
//! private network on XCP-ng 8.x (its vSwitch Controller is gone); here it
//! is offered when the pool runs the Open vSwitch backend, which tunnels
//! need.

use egui::{Button, ComboBox, Context, Grid, RichText, TextEdit, Ui};
use xapi::{BondMode, Host, Network, Pif, Ref};
use xen_core::network::{
    self, HashingAlgorithm, MTU_DEFAULT, MTU_MAX, MTU_MIN, NewBondSpec, NewNetworkKind,
    NewNetworkSpec, VLAN_MAX,
};
use xen_core::objects::{self, LinkStatus, NetworkLinkStatus};
use xen_core::{ActionRequest, AppState, ConnectionId, ConnectionModel, ConnectionView};

use crate::branding;
use crate::i18n::fill;
use crate::net_strings::net_strings;
use crate::tab_strings::tab_strings;

/// The network types of the first page (`NetworkTypes` without SR-IOV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetType {
    External,
    Internal,
    Bonded,
    CrossServer,
}

/// The wizard's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Type,
    Name,
    Details,
    Bond,
}

impl Page {
    fn name(self) -> &'static str {
        let n = net_strings();
        match self {
            Page::Type => n.page_type,
            Page::Name => n.page_name,
            Page::Details => n.page_details,
            Page::Bond => n.page_bond,
        }
    }
}

/// How the wizard ended (or that it has not).
pub enum WizardOutcome {
    Open,
    Cancelled,
    Finish(ConnectionId, ActionRequest),
}

// ---- bond members -------------------------------------------------------------

/// The bond mode radio buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BondChoice {
    ActiveActive,
    ActivePassive,
    LacpSrcMac,
    LacpTcpUdp,
}

impl BondChoice {
    pub fn mode(self) -> BondMode {
        match self {
            Self::ActiveActive => BondMode::BalanceSlb,
            Self::ActivePassive => BondMode::ActiveBackup,
            Self::LacpSrcMac | Self::LacpTcpUdp => BondMode::Lacp,
        }
    }

    pub fn hashing(self) -> Option<HashingAlgorithm> {
        match self {
            Self::LacpSrcMac => Some(HashingAlgorithm::SrcMac),
            Self::LacpTcpUdp => Some(HashingAlgorithm::TcpUdpPorts),
            _ => None,
        }
    }

    /// The choice showing a bond's mode and hashing.
    pub fn of(mode: &BondMode, hashing: Option<HashingAlgorithm>) -> Option<Self> {
        Some(match mode {
            BondMode::BalanceSlb => Self::ActiveActive,
            BondMode::ActiveBackup => Self::ActivePassive,
            BondMode::Lacp if hashing == Some(HashingAlgorithm::TcpUdpPorts) => Self::LacpTcpUdp,
            BondMode::Lacp => Self::LacpSrcMac,
            _ => return None,
        })
    }
}

/// One NIC of the bond members list.
#[derive(Debug, Clone)]
pub struct BondRow {
    pub pif: Ref<Pif>,
    pub cells: Vec<String>,
    /// "already in Bond 0+1": cannot be chosen.
    pub in_bond: bool,
}

/// What `CanCreateBond` says about the chosen members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BondCheck {
    Ok,
    /// Refused, with the message.
    Error(String),
    /// Asked: the message and the label of the button that goes on.
    Warn(String, &'static str),
}

/// The bond members form (`BondDetails`).
#[derive(Debug, Clone)]
pub struct BondForm {
    pub rows: Vec<BondRow>,
    pub checked: Vec<Ref<Pif>>,
    pub choice: BondChoice,
    pub mtu: String,
    pub auto_plug: bool,
    /// `Helpers.BondSizeLimit`.
    pub limit: usize,
    /// LACP is offered (vSwitch backend).
    pub lacp: bool,
}

impl BondForm {
    /// The NICs of `host`, or of the pool when `host` is `None`.
    pub fn new(model: &ConnectionModel, host: Option<&Ref<Host>>) -> Self {
        let n = net_strings();
        let t = tab_strings();
        let pifs = match host {
            Some(h) => network::host_nics(model, h),
            None => network::pool_nics(model),
        };
        let rows = pifs
            .into_iter()
            .map(|(r, p)| {
                let bond = network::pif_bond_member_of(model, p)
                    .and_then(|(_, b)| model.get(&b.master))
                    .map(|m| objects::pif_name(model, m));
                let mut name = objects::pif_name(model, p);
                if let Some(b) = &bond {
                    name = format!("{name} {}", fill(n.already_in_bond, &[("bond", b)]));
                }
                let metrics = model.get(&p.metrics);
                let carrier = metrics.is_some_and(|m| m.carrier);
                let link = match model.get(&p.network) {
                    Some(net) if net.pifs.len() > 1 => {
                        match objects::network_link_status(model, net) {
                            NetworkLinkStatus::None => n.none,
                            NetworkLinkStatus::Connected => t.link_connected,
                            NetworkLinkStatus::PartiallyConnected => t.link_partial,
                            NetworkLinkStatus::Disconnected => t.link_disconnected,
                            NetworkLinkStatus::Unknown => t.unknown,
                        }
                    }
                    _ => match objects::pif_link_status(model, p) {
                        LinkStatus::Connected => t.link_connected,
                        LinkStatus::Disconnected => t.link_disconnected,
                        LinkStatus::Unknown => t.unknown,
                    },
                };
                let metric = |f: fn(&xapi::PifMetricsRecord) -> String| {
                    metrics.map(|m| f(m)).unwrap_or_default()
                };
                BondRow {
                    pif: r.clone(),
                    cells: vec![
                        name,
                        p.mac.clone(),
                        link.to_owned(),
                        match metrics {
                            Some(m) if carrier => {
                                fill(t.nic_speed, &[("speed", &m.speed.to_string())])
                            }
                            _ => t.hyphen.to_owned(),
                        },
                        match metrics {
                            Some(m) if carrier => if m.duplex {
                                t.duplex_full
                            } else {
                                t.duplex_half
                            }
                            .to_owned(),
                            _ => t.hyphen.to_owned(),
                        },
                        metric(|m| m.vendor_name.clone()),
                        metric(|m| m.device_name.clone()),
                        metric(|m| m.pci_bus_path.clone()),
                    ],
                    in_bond: bond.is_some(),
                }
            })
            .collect();
        Self {
            rows,
            checked: Vec::new(),
            choice: BondChoice::ActiveActive,
            mtu: MTU_DEFAULT.to_string(),
            auto_plug: false,
            limit: network::bond_size_limit(model),
            lacp: network::supports_lacp(model),
        }
    }

    /// The allowed MTU range for the chosen NICs.
    pub fn mtu_range(&self, model: &ConnectionModel) -> (i64, i64) {
        let max = self
            .checked
            .iter()
            .filter_map(|p| model.get(p))
            .map(|p| p.mtu)
            .min()
            .map(|m| m.min(MTU_MAX))
            .unwrap_or(MTU_MAX);
        (MTU_MIN, max.max(MTU_MIN))
    }

    fn mtu_value(&self, model: &ConnectionModel) -> Option<i64> {
        let (min, max) = self.mtu_range(model);
        if min == max {
            return Some(min);
        }
        self.mtu
            .trim()
            .parse()
            .ok()
            .filter(|v| (min..=max).contains(v))
    }

    /// `BondDetails.Valid`.
    pub fn valid(&self, model: &ConnectionModel) -> bool {
        (2..=self.limit).contains(&self.checked.len()) && self.mtu_value(model).is_some()
    }

    /// Whether a row's box can be ticked (`ToggleNICRowCheckable`).
    pub fn checkable(&self, row: &BondRow) -> bool {
        self.checked.contains(&row.pif) || (!row.in_bond && self.checked.len() < self.limit)
    }

    pub fn toggle(&mut self, model: &ConnectionModel, pif: &Ref<Pif>) {
        if let Some(i) = self.checked.iter().position(|p| p == pif) {
            self.checked.remove(i);
        } else if self.rows.iter().any(|r| &r.pif == pif && !r.in_bond)
            && self.checked.len() < self.limit
        {
            self.checked.push(pif.clone());
            // In row order.
            let order: Vec<Ref<Pif>> = self.rows.iter().map(|r| r.pif.clone()).collect();
            self.checked
                .sort_by_key(|p| order.iter().position(|o| o == p));
        }
        let (min, max) = self.mtu_range(model);
        if min == max || self.checked.is_empty() {
            self.mtu = if self.checked.is_empty() {
                MTU_DEFAULT
            } else {
                min
            }
            .to_string();
        }
    }

    /// The new bond, once valid.
    pub fn spec(&self, model: &ConnectionModel) -> Option<NewBondSpec> {
        if !self.valid(model) {
            return None;
        }
        Some(NewBondSpec {
            name: fill(
                net_strings().bond_name,
                &[("suffix", &network::bond_suffix(model, &self.checked))],
            ),
            pifs: self.checked.clone(),
            mode: self.choice.mode(),
            hashing: self.choice.hashing(),
            mtu: self.mtu_value(model)?,
            auto_plug: self.auto_plug,
        })
    }

    /// `BondDetails.CanCreateBond`.
    pub fn check(&self, model: &ConnectionModel) -> BondCheck {
        let n = net_strings();
        let primary = network::contains_primary_management(model, &self.checked);
        let secondary = network::contains_secondary_management(model, &self.checked);
        let clustering = network::contains_clustering_pif(model, &self.checked);
        if primary && secondary {
            return BondCheck::Error(n.bond_create_both.to_owned());
        }
        if primary {
            if let Some((_, pool)) = model.pool().filter(|(_, p)| p.ha_enabled) {
                return BondCheck::Error(fill(
                    n.bond_create_ha,
                    &[("pool", &objects::pool_name(model, pool))],
                ));
            }
            return BondCheck::Warn(
                fill(
                    n.bond_create_primary,
                    &[("console", branding::PRODUCT_NAME)],
                ),
                n.bond_create_continue,
            );
        }
        if secondary {
            let text = if clustering {
                n.bond_create_clustering
            } else {
                n.bond_create_secondary
            };
            return BondCheck::Warn(text.to_owned(), n.ok);
        }
        BondCheck::Ok
    }

    /// Draws the form.
    pub fn ui(&mut self, ui: &mut Ui, model: &ConnectionModel, id: &str) {
        let n = net_strings();
        let columns = [
            "",
            n.col_nic,
            n.col_mac,
            n.col_link_status,
            n.col_speed,
            n.col_duplex,
            n.col_vendor,
            n.col_device,
            n.col_pci,
        ];
        let mut toggled = None;
        egui::ScrollArea::both()
            .id_salt((id, "nics"))
            .max_height(150.0)
            .show(ui, |ui| {
                Grid::new((id, "grid"))
                    .striped(true)
                    .num_columns(columns.len())
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for c in columns {
                            ui.strong(c);
                        }
                        ui.end_row();
                        for row in &self.rows {
                            let mut on = self.checked.contains(&row.pif);
                            let enabled = self.checkable(row);
                            if ui
                                .add_enabled(enabled, egui::Checkbox::without_text(&mut on))
                                .clicked()
                            {
                                toggled = Some(row.pif.clone());
                            }
                            for cell in &row.cells {
                                if enabled {
                                    ui.label(cell);
                                } else {
                                    ui.label(RichText::new(cell).weak());
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(p) = toggled {
            self.toggle(model, &p);
        }
        ui.add_space(6.0);
        ui.label(RichText::new(n.bond_mode).strong());
        ui.radio_value(
            &mut self.choice,
            BondChoice::ActiveActive,
            n.mode_active_active,
        );
        ui.radio_value(
            &mut self.choice,
            BondChoice::ActivePassive,
            n.mode_active_passive,
        );
        if self.lacp {
            ui.radio_value(
                &mut self.choice,
                BondChoice::LacpSrcMac,
                n.mode_lacp_src_mac,
            );
            ui.radio_value(&mut self.choice, BondChoice::LacpTcpUdp, n.mode_lacp_tcpudp);
        }
        if self.choice.hashing().is_some() {
            ui.colored_label(ui.visuals().warn_fg_color, n.lacp_warning);
        }
        ui.add_space(6.0);
        let (min, max) = self.mtu_range(model);
        ui.horizontal(|ui| {
            let label = ui.label(n.mtu_label);
            ui.add_enabled(
                min != max,
                TextEdit::singleline(&mut self.mtu).desired_width(70.0),
            )
            .labelled_by(label.id);
            ui.label(
                RichText::new(if min == max {
                    fill(n.mtu_value, &[("value", &min.to_string())])
                } else {
                    fill(
                        n.mtu_range,
                        &[("min", &min.to_string()), ("max", &max.to_string())],
                    )
                })
                .weak(),
            );
        });
        ui.checkbox(&mut self.auto_plug, n.auto_add);
    }
}

// ---- the wizard -----------------------------------------------------------------

/// The New Network wizard.
pub struct NewNetworkWizard {
    connection: ConnectionId,
    /// The host the pages show (the coordinator for a pool).
    host: Ref<Host>,
    /// Opened on the pool (`Pool != null`).
    pool: bool,
    pub page: Page,
    pub kind: NetType,
    /// The type the name page was filled for.
    named_for: Option<NetType>,
    pub name: String,
    pub description: String,
    pub nic: Option<Ref<Pif>>,
    pub vlan: String,
    pub mtu: String,
    pub auto_plug: bool,
    pub interface: Option<Ref<Network>>,
    pub bond: BondForm,
    /// The bond check's question or error, while shown.
    pub question: Option<BondCheck>,
}

impl NewNetworkWizard {
    /// The wizard for `host`, or for the pool (its coordinator) when `host`
    /// is `None`.
    pub fn new(conn: &ConnectionView, host: Option<Ref<Host>>) -> Option<Self> {
        let model = &conn.model;
        let pool = host.is_none();
        let host = match host {
            Some(h) => h,
            None => network::coordinator(model)?.clone(),
        };
        let bond = BondForm::new(model, (!pool).then_some(&host));
        Some(Self {
            connection: conn.id,
            host,
            pool,
            page: Page::Type,
            kind: NetType::External,
            named_for: None,
            name: String::new(),
            description: String::new(),
            nic: None,
            vlan: String::new(),
            mtu: MTU_DEFAULT.to_string(),
            auto_plug: false,
            interface: None,
            bond,
            question: None,
        })
    }

    /// Whether the wizard shows the cross-server private network at all
    /// (`NetWTypeSelect.PopulatePage`): not on XCP-ng 8.x (Stockholm or
    /// later), which has no vSwitch Controller.
    pub fn chin_shown(model: &ConnectionModel) -> bool {
        !xen_core::pool::stockholm_or_greater(model.coordinator().map(|h| &**h))
    }

    /// Whether the cross-server private network can be chosen: shown, and
    /// the pool has a vSwitch Controller (`Pool.vSwitchController`).
    pub fn chin_offered(model: &ConnectionModel) -> bool {
        Self::chin_shown(model) && network::vswitch_controller(model)
    }

    fn pages(&self) -> Vec<Page> {
        match self.kind {
            NetType::Bonded => vec![Page::Type, Page::Bond],
            _ => vec![Page::Type, Page::Name, Page::Details],
        }
    }

    /// `UpdateWizardContent` after the type page: the name and the
    /// details are filled for the chosen type.
    fn enter_type(&mut self, model: &ConnectionModel, show_hidden: bool) {
        if self.named_for != Some(self.kind) {
            self.named_for = Some(self.kind);
            let n = net_strings();
            let stub = match self.kind {
                NetType::External => n.new_network_name,
                _ => n.new_private_name,
            };
            self.name = network::unique_name(stub, &network::network_names(model));
            self.nic = network::vlan_nics(model, &self.host, show_hidden)
                .first()
                .map(|(r, _)| (*r).clone());
            self.interface =
                network::chin_interfaces(model, (!self.pool).then_some(&self.host), show_hidden)
                    .first()
                    .map(|(r, _)| (*r).clone());
            self.vlan = String::new();
            self.nic_changed(model);
        }
    }

    /// `cmbHostNicList_SelectedIndexChanged`: the MTU range follows the
    /// NIC; a VLAN in use is replaced by the first free one.
    pub fn nic_changed(&mut self, model: &ConnectionModel) {
        let Some(pif) = self.nic.as_ref().and_then(|p| model.get(p)) else {
            return;
        };
        let (min, max) = (MTU_MIN, pif.mtu.clamp(MTU_MIN, MTU_MAX));
        if min == max {
            self.mtu = min.to_string();
        }
        let used = network::vlans_in_use(model, &pif.device);
        let current: Option<i64> = self.vlan.trim().parse().ok();
        if current.is_none_or(|v| used.contains(&v)) {
            self.vlan = network::first_free_vlan(&used).to_string();
        }
    }

    fn mtu_range(&self, model: &ConnectionModel) -> (i64, i64) {
        let max = self
            .nic
            .as_ref()
            .and_then(|p| model.get(p))
            .map(|p| p.mtu.min(MTU_MAX))
            .unwrap_or(MTU_MAX);
        (MTU_MIN, max.max(MTU_MIN))
    }

    /// The VLAN problem (an error) or remark, as `ValidateVLANValue`.
    pub fn vlan_message(&self, model: &ConnectionModel) -> Option<(bool, String)> {
        let n = net_strings();
        let min = if network::vlan0_allowed(model) { 0 } else { 1 };
        let Ok(v) = self.vlan.trim().parse::<i64>() else {
            return Some((true, n.invalid_number.to_owned()));
        };
        if !(min..=VLAN_MAX).contains(&v) {
            return Some((
                true,
                fill(
                    n.vlan_range,
                    &[("min", &min.to_string()), ("max", &VLAN_MAX.to_string())],
                ),
            ));
        }
        let device = self
            .nic
            .as_ref()
            .and_then(|p| model.get(p))
            .map(|p| p.device.clone())
            .unwrap_or_default();
        if network::vlans_in_use(model, &device).contains(&v) {
            return Some((true, n.vlan_in_use.to_owned()));
        }
        (v == 0).then(|| (false, n.vlan_zero.to_owned()))
    }

    fn mtu_value(&self, model: &ConnectionModel) -> Option<i64> {
        let (min, max) = self.mtu_range(model);
        self.mtu
            .trim()
            .parse()
            .ok()
            .filter(|v| (min..=max).contains(v))
    }

    fn page_ok(&self, model: &ConnectionModel) -> bool {
        match self.page {
            Page::Type => self.kind != NetType::CrossServer || Self::chin_offered(model),
            Page::Name => !self.name.trim().is_empty(),
            Page::Details => match self.kind {
                NetType::External => {
                    self.nic.is_some()
                        && !self.vlan_message(model).is_some_and(|(e, _)| e)
                        && self.mtu_value(model).is_some()
                }
                NetType::CrossServer => self.interface.is_some(),
                _ => true,
            },
            Page::Bond => self.bond.valid(model),
        }
    }

    /// The request the wizard makes.
    pub fn request(&self, model: &ConnectionModel) -> Option<ActionRequest> {
        if self.kind == NetType::Bonded {
            return self
                .bond
                .spec(model)
                .map(|s| ActionRequest::NewBond(Box::new(s)));
        }
        let kind = match self.kind {
            NetType::External => NewNetworkKind::External {
                pif: self.nic.clone()?,
                vlan: self.vlan.trim().parse().ok()?,
            },
            NetType::Internal => NewNetworkKind::Internal,
            NetType::CrossServer => NewNetworkKind::CrossServer {
                interface: self.interface.clone()?,
            },
            NetType::Bonded => unreachable!(),
        };
        let mtu = match self.kind {
            NetType::External => {
                let (min, max) = self.mtu_range(model);
                // A fixed MTU is not sent (`MTU == -1`).
                if min == max {
                    None
                } else {
                    self.mtu_value(model)
                }
            }
            NetType::CrossServer => self
                .interface
                .as_ref()
                .and_then(|n| model.networks.get(n))
                .map(|n| n.mtu),
            _ => None,
        };
        Some(ActionRequest::NewNetwork(Box::new(NewNetworkSpec {
            name: self.name.trim().to_owned(),
            description: self.description.clone(),
            auto_plug: self.auto_plug,
            mtu,
            kind,
        })))
    }

    /// Next / Finish was pressed on the current page.
    pub fn next(&mut self, model: &ConnectionModel, show_hidden: bool) -> Option<ActionRequest> {
        if !self.page_ok(model) {
            return None;
        }
        let pages = self.pages();
        let index = pages.iter().position(|p| *p == self.page).unwrap_or(0);
        if self.page == Page::Type {
            self.enter_type(model, show_hidden);
        }
        if self.page == Page::Name {
            // `CheckUniqueName`.
            self.name =
                network::checked_unique_name(self.name.trim(), &network::network_names(model));
        }
        if index + 1 < pages.len() {
            self.page = pages[index + 1];
            return None;
        }
        if self.page == Page::Bond {
            match self.bond.check(model) {
                BondCheck::Ok => {}
                other => {
                    self.question = Some(other);
                    return None;
                }
            }
        }
        self.request(model)
    }

    /// The bond question was answered to go on.
    pub fn confirm_bond(&mut self, model: &ConnectionModel) -> Option<ActionRequest> {
        self.question = None;
        self.request(model)
    }

    pub fn previous(&mut self) {
        let pages = self.pages();
        if let Some(i) = pages.iter().position(|p| *p == self.page)
            && i > 0
        {
            self.page = pages[i - 1];
        }
    }

    /// Draws the wizard.
    pub fn show(&mut self, ctx: &Context, state: &AppState) -> WizardOutcome {
        let n = net_strings();
        let Some(conn) = state.connection(self.connection) else {
            return WizardOutcome::Cancelled;
        };
        let model = conn.model.clone();
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let pool_name = model
            .pool()
            .map(|(_, p)| objects::pool_name(&model, p))
            .unwrap_or_default();
        let mut open = true;
        let mut outcome = WizardOutcome::Open;
        egui::Window::new(fill(n.wizard_title, &[("pool", &pool_name)]))
            .id(egui::Id::unique("new-network-wizard"))
            .collapsible(false)
            .resizable(false)
            .fixed_size([760.0, 440.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(130.0);
                        for page in self.pages() {
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
                            .id_salt("new-network-page")
                            .max_height(340.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| match self.page {
                                Page::Type => self.type_page(ui, &model),
                                Page::Name => self.name_page(ui),
                                Page::Details => self.details_page(ui, &model, show_hidden),
                                Page::Bond => {
                                    ui.heading(n.page_bond_title);
                                    ui.add_space(6.0);
                                    self.bond.ui(ui, &model, "new-network-bond");
                                }
                            });
                    });
                });
                ui.separator();
                if let Some(question) = self.question.clone() {
                    match question {
                        BondCheck::Error(text) => {
                            ui.colored_label(ui.visuals().error_fg_color, text);
                            if ui.button(n.ok).clicked() {
                                self.question = None;
                            }
                        }
                        BondCheck::Warn(text, yes) => {
                            ui.colored_label(ui.visuals().warn_fg_color, text);
                            ui.horizontal(|ui| {
                                if ui.button(yes).clicked()
                                    && let Some(r) = self.confirm_bond(&model)
                                {
                                    outcome = WizardOutcome::Finish(self.connection, r);
                                }
                                if ui.button(n.cancel).clicked() {
                                    self.question = None;
                                }
                            });
                        }
                        BondCheck::Ok => self.question = None,
                    }
                    return;
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.page != Page::Type, Button::new(n.previous))
                        .clicked()
                    {
                        self.previous();
                    }
                    let last = self.pages().last() == Some(&self.page);
                    let label = if last { n.finish } else { n.next };
                    if ui
                        .add_enabled(self.page_ok(&model), Button::new(label))
                        .clicked()
                        && let Some(r) = self.next(&model, show_hidden)
                    {
                        outcome = WizardOutcome::Finish(self.connection, r);
                    }
                    if ui.button(n.cancel).clicked() {
                        outcome = WizardOutcome::Cancelled;
                    }
                });
            });
        if !open {
            return WizardOutcome::Cancelled;
        }
        outcome
    }

    fn type_page(&mut self, ui: &mut Ui, model: &ConnectionModel) {
        let n = net_strings();
        ui.heading(n.page_type_title);
        ui.add_space(6.0);
        ui.label(n.type_blurb);
        ui.add_space(6.0);
        let chin = Self::chin_offered(model);
        let shown = Self::chin_shown(model);
        for (kind, label, blurb, enabled) in [
            (
                NetType::External,
                n.type_external,
                n.type_external_blurb,
                true,
            ),
            (
                NetType::Internal,
                n.type_internal,
                n.type_internal_blurb,
                true,
            ),
            (NetType::Bonded, n.type_bonded, n.type_bonded_blurb, true),
            (NetType::CrossServer, n.type_chin, n.type_chin_blurb, chin),
        ] {
            if kind == NetType::CrossServer && !shown {
                continue;
            }
            ui.add_enabled_ui(enabled, |ui| {
                ui.radio_value(&mut self.kind, kind, label);
                ui.indent(("net-type", label), |ui| {
                    ui.label(RichText::new(blurb).weak());
                });
            });
            if kind == NetType::CrossServer && !chin {
                ui.indent("chin-warning", |ui| {
                    ui.colored_label(ui.visuals().warn_fg_color, n.chin_needs_vswitch);
                });
            }
            ui.add_space(4.0);
        }
    }

    fn name_page(&mut self, ui: &mut Ui) {
        let n = net_strings();
        ui.heading(n.page_name_title);
        ui.add_space(6.0);
        ui.label(n.name_blurb);
        ui.add_space(6.0);
        Grid::new("new-network-name").num_columns(2).show(ui, |ui| {
            let label = ui.label(n.name_label);
            ui.add(TextEdit::singleline(&mut self.name).desired_width(380.0))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(n.description_label);
            ui.add(
                TextEdit::multiline(&mut self.description)
                    .desired_rows(3)
                    .desired_width(380.0),
            )
            .labelled_by(label.id);
            ui.end_row();
        });
    }

    fn details_page(&mut self, ui: &mut Ui, model: &ConnectionModel, show_hidden: bool) {
        let n = net_strings();
        ui.heading(n.page_details_title);
        ui.add_space(6.0);
        match self.kind {
            NetType::External => {
                ui.label(n.external_blurb);
                ui.add_space(6.0);
                let nics = network::vlan_nics(model, &self.host, show_hidden);
                let selected = self
                    .nic
                    .as_ref()
                    .and_then(|p| model.get(p))
                    .map(|p| objects::pif_name(model, p))
                    .unwrap_or_default();
                let mut changed = false;
                Grid::new("new-network-details")
                    .num_columns(3)
                    .show(ui, |ui| {
                        let label = ui.label(n.nic_label);
                        ComboBox::from_id_salt("new-network-nic")
                            .selected_text(selected)
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for (r, p) in &nics {
                                    if ui
                                        .selectable_label(
                                            self.nic.as_ref() == Some(*r),
                                            objects::pif_name(model, p),
                                        )
                                        .clicked()
                                    {
                                        self.nic = Some((*r).clone());
                                        changed = true;
                                    }
                                }
                            })
                            .response
                            .labelled_by(label.id);
                        ui.label("");
                        ui.end_row();
                        let label = ui.label(n.vlan_label);
                        ui.add(TextEdit::singleline(&mut self.vlan).desired_width(70.0))
                            .labelled_by(label.id);
                        match self.vlan_message(model) {
                            Some((true, text)) => {
                                ui.colored_label(ui.visuals().error_fg_color, text);
                            }
                            Some((false, text)) => {
                                ui.label(RichText::new(text).weak());
                            }
                            None => {
                                ui.label("");
                            }
                        }
                        ui.end_row();
                        let (min, max) = self.mtu_range(model);
                        let label = ui.label(n.mtu_label);
                        ui.add_enabled(
                            min != max,
                            TextEdit::singleline(&mut self.mtu).desired_width(70.0),
                        )
                        .labelled_by(label.id);
                        let text = if self.mtu.trim().parse::<i64>().is_err() {
                            n.invalid_number.to_owned()
                        } else if min == max {
                            fill(n.mtu_value, &[("value", &min.to_string())])
                        } else {
                            fill(
                                n.mtu_range,
                                &[("min", &min.to_string()), ("max", &max.to_string())],
                            )
                        };
                        ui.label(RichText::new(text).weak());
                        ui.end_row();
                    });
                if changed {
                    self.nic_changed(model);
                }
            }
            NetType::CrossServer => {
                ui.label(n.chin_blurb);
                ui.add_space(6.0);
                let interfaces = network::chin_interfaces(
                    model,
                    (!self.pool).then_some(&self.host),
                    show_hidden,
                );
                let selected = self
                    .interface
                    .as_ref()
                    .and_then(|r| model.networks.get(r))
                    .map(|net| objects::network_name(model, net))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    let label = ui.label(n.chin_interface_label);
                    ComboBox::from_id_salt("new-network-interface")
                        .selected_text(selected)
                        .width(240.0)
                        .show_ui(ui, |ui| {
                            for (r, net) in &interfaces {
                                if ui
                                    .selectable_label(
                                        self.interface.as_ref() == Some(*r),
                                        objects::network_name(model, net),
                                    )
                                    .clicked()
                                {
                                    self.interface = Some((*r).clone());
                                }
                            }
                        })
                        .response
                        .labelled_by(label.id);
                });
            }
            _ => {
                ui.label(n.internal_blurb);
            }
        }
        ui.add_space(8.0);
        ui.checkbox(&mut self.auto_plug, n.auto_add);
    }
}
