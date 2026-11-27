//! Networking in the window: the buttons of the Networking tab of a VM
//! (Add Interface, Properties, Remove Interface, Activate / Deactivate —
//! `NetworkList`), of a host or pool (Add Network, Properties, Remove
//! Network, and Configure for the IP addresses — `NetworkPage`) and of the
//! NICs tab (Create Bond, Delete Bond, Rescan — `NICPage`); the virtual
//! interface dialog (`VIFDialog`), the network Properties dialog (General
//! and Network Settings — `EditNetworkPage`), the Configure IP Addresses
//! dialog (`NetworkingProperties` and its pages) and the Create Bond dialog
//! (`BondProperties`). The New Network wizard is [`crate::new_network`].
//!
//! Every operation ends in an [`ActionRequest`] sent through
//! [`OpsUi::request`] (removals ask the original's questions first) or
//! [`OpsUi::dispatch`], so the RBAC check and role elevation apply.

use std::collections::HashMap;

use egui::{Align2, Button, ComboBox, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Host, IpConfigurationMode, Network, Pif, Ref, Vif, Vm, VmRecord};
use xen_core::network::{
    self, IpPage, MTU_MAX, MTU_MIN, NetworkChanges, PifChange, QOS_KBPS, QOS_RATE_LIMIT, VLAN_MAX,
    VifHint, VifSpec,
};
use xen_core::objects;
use xen_core::{
    ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionModel, ConnectionView,
};

use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::fill;
use crate::net_strings::net_strings;
use crate::new_network::{BondCheck, BondChoice, BondForm, NewNetworkWizard, WizardOutcome};
use crate::ops_ui::{Confirmation, OpsUi};
use crate::tabs::{self, Table};
use crate::window_frame::{DialogWindow, dialog_window};

/// The question asked before a networking request, if any (the
/// original's `WarningDialog`s).
pub fn confirmation(conn: &ConnectionView, request: &ActionRequest) -> Option<Confirmation> {
    let n = net_strings();
    let m = &conn.model;
    let plain = |title, text: &str| Confirmation {
        title,
        text: text.to_owned(),
        yes: n.yes,
        no_default: false,
    };
    match request {
        ActionRequest::RemoveNetwork { .. } => {
            Some(plain(n.network_delete_title, n.network_delete_text))
        }
        ActionRequest::DeleteVif { .. } => Some(plain(n.vif_delete_title, n.vif_delete_text)),
        ActionRequest::DestroyBond { bond } => {
            // `DestroyBondCommand`: the bond network's PIFs.
            let net = m
                .get(bond)
                .and_then(|b| m.get(&b.master))
                .and_then(|p| m.networks.get(&p.network))?;
            let pifs: Vec<Ref<Pif>> = net.pifs.clone();
            let first = pifs.first().and_then(|p| m.get(p))?;
            let question = fill(
                n.delete_bond_text,
                &[("bond", &objects::pif_name(m, first))],
            );
            let primary = network::contains_primary_management(m, &pifs);
            let secondary = network::contains_secondary_management(m, &pifs);
            let console = ("console", branding::PRODUCT_NAME);
            Some(if primary {
                Confirmation {
                    title: n.delete_bond_title,
                    text: fill(
                        if secondary {
                            n.bond_delete_both
                        } else {
                            n.bond_delete_primary
                        },
                        &[("question", &question), console],
                    ),
                    yes: n.bond_delete_continue,
                    no_default: true,
                }
            } else if secondary {
                Confirmation {
                    title: n.delete_bond_title,
                    text: fill(n.bond_delete_secondary, &[("question", &question)]),
                    yes: n.ok,
                    no_default: false,
                }
            } else {
                Confirmation {
                    title: n.delete_bond_title,
                    text: question,
                    yes: n.ok,
                    no_default: false,
                }
            })
        }
        _ => None,
    }
}

/// Draws `table` with selectable rows; returns the row clicked.
fn select_table(ui: &mut Ui, id: &str, table: &Table, selected: Option<usize>) -> Option<usize> {
    if table.rows.is_empty() {
        ui.label(RichText::new(crate::tab_strings::tab_strings().empty_list).weak());
        return None;
    }
    let mut clicked = None;
    egui::ScrollArea::horizontal()
        .id_salt(("net-table-scroll", id))
        .show(ui, |ui| {
            Grid::new(("net-table", id))
                .striped(true)
                .num_columns(table.columns.len())
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for column in &table.columns {
                        ui.strong(*column);
                    }
                    ui.end_row();
                    for (i, row) in table.rows.iter().enumerate() {
                        for cell in &row.cells {
                            let text = if row.greyed {
                                RichText::new(cell).weak()
                            } else {
                                RichText::new(cell)
                            };
                            // Any cell selects the row, as a list view does.
                            if ui.selectable_label(selected == Some(i), text).clicked() {
                                clicked = Some(i);
                            }
                        }
                        ui.end_row();
                    }
                });
        });
    clicked
}

// ---- the virtual interface dialog ----------------------------------------------

/// `VIFDialog`: a new interface of a VM, or the properties of one.
pub struct VifDialog {
    vm: Ref<Vm>,
    existing: Option<Ref<Vif>>,
    device: String,
    pub network: Option<Ref<Network>>,
    pub auto_mac: bool,
    pub mac: String,
    pub qos: bool,
    pub qos_limit: String,
    /// The duplicate MAC question, while shown (the other VM's name).
    pub duplicate: Option<String>,
}

impl VifDialog {
    pub fn new(
        conn: &ConnectionView,
        vm: Ref<Vm>,
        existing: Option<Ref<Vif>>,
        show_hidden: bool,
    ) -> Option<Self> {
        let m = &conn.model;
        let record = m.vms.get(&vm)?;
        let current = existing.as_ref().and_then(|v| m.get(v));
        let networks = network::vif_networks(m, show_hidden);
        let device = match current {
            Some(v) => v.device.clone(),
            None => network::next_vif_device(m, record).to_string(),
        };
        let network = current
            .map(|v| v.network.clone())
            .filter(|n| networks.iter().any(|(r, _)| *r == n))
            .or_else(|| networks.first().map(|(r, _)| (*r).clone()));
        Some(Self {
            vm,
            device,
            network,
            auto_mac: current.is_none_or(|v| v.mac.is_empty()),
            mac: current.map(|v| v.mac.clone()).unwrap_or_default(),
            qos: current.is_some_and(|v| v.qos_algorithm_type == QOS_RATE_LIMIT),
            qos_limit: current
                .map(|v| network::vif_limit(v).to_owned())
                .unwrap_or_default(),
            existing,
            duplicate: None,
        })
    }

    /// `ValidateInput`: `Err(Some(message))` shows the message, `Err(None)`
    /// only disables OK.
    pub fn validate(&self) -> Result<(), Option<&'static str>> {
        let n = net_strings();
        if self.network.is_none() {
            return Err(Some(n.no_networks));
        }
        if !self.auto_mac {
            if self.mac.trim().is_empty() {
                return Err(None);
            }
            if !network::is_valid_mac(self.mac.trim()) {
                return Err(Some(n.mac_invalid));
            }
        }
        if self.qos {
            if self.qos_limit.trim().is_empty() {
                return Err(None);
            }
            if !self.qos_limit.trim().parse::<i64>().is_ok_and(|v| v > 0) {
                return Err(Some(n.qos_invalid));
            }
        }
        Ok(())
    }

    fn mac_value(&self) -> String {
        if self.auto_mac {
            String::new()
        } else {
            self.mac.trim().to_owned()
        }
    }

    /// `GetNewSettings`.
    pub fn spec(&self, m: &ConnectionModel) -> Option<VifSpec> {
        let current = self.existing.as_ref().and_then(|v| m.get(v));
        let mut qos_type = current
            .map(|v| v.qos_algorithm_type.clone())
            .unwrap_or_default();
        let mut params = current
            .map(|v| v.qos_algorithm_params.clone())
            .unwrap_or_default();
        if self.qos {
            qos_type = QOS_RATE_LIMIT.to_owned();
        } else if qos_type == QOS_RATE_LIMIT {
            qos_type = String::new();
        }
        if !self.qos_limit.trim().is_empty() {
            params = HashMap::from([(QOS_KBPS.to_owned(), self.qos_limit.trim().to_owned())]);
        }
        Some(VifSpec {
            network: self.network.clone()?,
            mac: self.mac_value(),
            device: self.device.clone(),
            qos_algorithm_type: qos_type,
            qos_algorithm_params: params,
        })
    }

    /// `ChangesHaveBeenMade`.
    pub fn changed(&self, m: &ConnectionModel) -> bool {
        let Some(v) = self.existing.as_ref().and_then(|v| m.get(v)) else {
            return true;
        };
        if self.network.as_ref() != Some(&v.network)
            || v.mac != self.mac_value()
            || v.device != self.device
        {
            return true;
        }
        if v.qos_algorithm_type == QOS_RATE_LIMIT {
            !self.qos || network::vif_limit(v) != self.qos_limit.trim()
        } else {
            v.qos_algorithm_type.is_empty() && self.qos
        }
    }

    /// OK was pressed: the request to run, or `None` while the duplicate
    /// MAC question is asked or nothing changed.
    pub fn accept(&mut self, m: &ConnectionModel, confirmed: bool) -> Option<ActionRequest> {
        let mac_changed = match self.existing.as_ref().and_then(|v| m.get(v)) {
            None => true,
            Some(v) => self.mac != v.mac,
        };
        if !confirmed
            && mac_changed
            && let Some(other) =
                network::duplicate_mac(m, &self.mac_value(), self.existing.as_ref())
        {
            self.duplicate = Some(other.name_label.clone());
            return None;
        }
        if !self.changed(m) {
            return None;
        }
        let spec = self.spec(m)?;
        Some(match &self.existing {
            None => ActionRequest::NewVif {
                vm: self.vm.clone(),
                spec,
            },
            Some(vif) => ActionRequest::UpdateVif {
                vif: vif.clone(),
                spec,
            },
        })
    }
}

// ---- the network Properties dialog ----------------------------------------------

/// The NIC choice of the Network Settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NicChoice {
    Internal,
    Nic(Ref<Pif>),
}

/// The pages of the network Properties dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertiesTab {
    General,
    Settings,
}

/// The network Properties dialog (`PropertiesDialog` with
/// `EditNetworkPage`).
pub struct NetworkProperties {
    network: Ref<Network>,
    pub tab: PropertiesTab,
    pub name: String,
    pub description: String,
    pub tags: String,
    pub auto_plug: bool,
    pub mtu: String,
    /// `None` when the NIC and VLAN are not editable (a NIC's or bond's
    /// network, a cross-server network).
    pub nic: Option<NicChoice>,
    pub vlan: String,
    pub bond: Option<BondChoice>,
    original: Original,
}

/// The values the network Properties dialog opened with.
struct Original {
    name: String,
    description: String,
    tags: Vec<String>,
    auto_plug: bool,
    mtu: i64,
    nic: Option<NicChoice>,
    vlan: i64,
    bond: Option<BondChoice>,
}

impl NetworkProperties {
    pub fn new(conn: &ConnectionView, network_ref: Ref<Network>) -> Option<Self> {
        let m = &conn.model;
        let net = m.networks.get(&network_ref)?;
        let host = network::coordinator(m)?;
        let pif = network::pif_on_host(m, net, host).map(|(_, p)| p);
        let editable = pif.is_none_or(|p| {
            !objects::pif_is_physical(p)
                && !network::pif_is_tunnel_access(p)
                && p.sriov_logical_pif_of.is_empty()
        });
        let (nic, vlan) = match pif {
            None => (Some(NicChoice::Internal), 0),
            Some(p) if editable => {
                // `FindAssociatedPhysicalPIF`.
                let physical = m
                    .pifs
                    .iter()
                    .find(|(_, q)| {
                        objects::pif_is_physical(q) && &q.host == host && q.device == p.device
                    })
                    .map(|(r, _)| r.clone());
                (physical.map(NicChoice::Nic), p.vlan)
            }
            Some(_) => (None, 0),
        };
        let bond = network::network_bonds(m, net)
            .first()
            .and_then(|(_, b)| BondChoice::of(&b.mode, network::bond_hashing(b)));
        let auto = network::network_auto_plug(net);
        Some(Self {
            tab: PropertiesTab::General,
            name: net.name_label.clone(),
            description: net.name_description.clone(),
            tags: net.tags.join(", "),
            auto_plug: auto,
            mtu: net.mtu.to_string(),
            nic: nic.clone(),
            vlan: vlan.to_string(),
            bond,
            original: Original {
                name: net.name_label.clone(),
                description: net.name_description.clone(),
                tags: net.tags.clone(),
                auto_plug: auto,
                mtu: net.mtu,
                nic,
                vlan,
                bond,
            },
            network: network_ref,
        })
    }

    fn net<'a>(&self, m: &'a ConnectionModel) -> Option<&'a xapi::NetworkRecord> {
        m.networks.get(&self.network).map(|n| &**n)
    }

    /// `blockDueToAttachedVMs` / `blockDueToManagement` (the management
    /// interface's name).
    fn blocked(&self, m: &ConnectionModel, show_hidden: bool) -> (bool, Option<String>) {
        let Some(net) = self.net(m) else {
            return (false, None);
        };
        let vms = net
            .vifs
            .iter()
            .filter_map(|v| m.get(v))
            .any(|v| v.currently_attached);
        let management = network::network_pifs(m, net)
            .into_iter()
            .find(|(_, p)| network::pif_is_management_interface(m, p, show_hidden))
            .map(|(_, p)| {
                if p.management {
                    net_strings().management.to_owned()
                } else {
                    network::management_purpose(p)
                        .unwrap_or(net_strings().purpose_unknown)
                        .to_owned()
                }
            });
        (vms, management)
    }

    /// Whether the VLAN box is enabled (`SetNetSettingsEnablement`).
    pub fn vlan_enabled(&self, m: &ConnectionModel, show_hidden: bool) -> bool {
        let Some(net) = self.net(m) else {
            return false;
        };
        if self.nic == Some(NicChoice::Internal) || self.nic.is_none() {
            return false;
        }
        if net.pifs.is_empty() {
            return true;
        }
        let (vms, management) = self.blocked(m, show_hidden);
        !vms && management.is_none()
    }

    /// The warning under the NIC and VLAN, if any.
    pub fn nic_warning(&self, m: &ConnectionModel, show_hidden: bool) -> Option<String> {
        let n = net_strings();
        let net = self.net(m)?;
        if net.pifs.is_empty() || self.nic.is_none() {
            return None;
        }
        let (vms, management) = self.blocked(m, show_hidden);
        match management {
            Some(i) => Some(fill(n.cannot_change_nic_management, &[("interface", &i)])),
            None if vms => Some(n.cannot_change_nic_vms.to_owned()),
            None => None,
        }
    }

    /// Whether the NIC list is enabled.
    pub fn nic_enabled(&self, m: &ConnectionModel, show_hidden: bool) -> bool {
        let (vms, management) = self.blocked(m, show_hidden);
        self.nic.is_some() && !vms && management.is_none()
    }

    /// The MTU box: shown, enabled, the remark under it.
    pub fn mtu_state(
        &self,
        m: &ConnectionModel,
        show_hidden: bool,
    ) -> (bool, bool, Option<String>) {
        let n = net_strings();
        let Some(net) = self.net(m) else {
            return (false, false, None);
        };
        if !network::network_can_use_jumbo(m, net) {
            return (false, false, None);
        }
        if self.nic == Some(NicChoice::Internal) {
            return (true, false, None);
        }
        let host = network::coordinator(m);
        let pif = host
            .and_then(|h| network::pif_on_host(m, net, h))
            .map(|(_, p)| p);
        if let Some(p) = pif.filter(|p| network::pif_is_management_interface(m, p, show_hidden)) {
            let name = if p.management {
                n.management.to_owned()
            } else {
                network::management_purpose(p)
                    .unwrap_or(n.purpose_unknown)
                    .to_owned()
            };
            return (
                true,
                false,
                Some(fill(
                    n.cannot_change_mtu_management,
                    &[("interface", &name)],
                )),
            );
        }
        let without_tools = net
            .vifs
            .iter()
            .filter_map(|v| m.get(v))
            .filter_map(|v| m.vms.get(&v.vm))
            .any(|vm| {
                vm.power_state == xapi::VmPowerState::Running
                    && !network::io_drivers_installed(m, vm)
            });
        if without_tools {
            return (true, false, Some(n.cannot_change_mtu_no_tools.to_owned()));
        }
        (true, true, None)
    }

    fn mtu_range(&self, m: &ConnectionModel) -> (i64, i64) {
        let current = self.net(m).map(|n| n.mtu).unwrap_or(MTU_MIN);
        (current.min(MTU_MIN), current.max(MTU_MAX))
    }

    fn mtu_value(&self, m: &ConnectionModel) -> Option<i64> {
        let (min, max) = self.mtu_range(m);
        self.mtu
            .trim()
            .parse()
            .ok()
            .filter(|v| (min..=max).contains(v))
    }

    fn vlan_value(&self) -> Option<i64> {
        self.vlan
            .trim()
            .parse()
            .ok()
            .filter(|v| (0..=VLAN_MAX).contains(v))
    }

    /// The physical NICs of the NIC list (`populateHostNicList`).
    pub fn nic_choices(
        m: &ConnectionModel,
        show_hidden: bool,
    ) -> Vec<(&Ref<Pif>, &xapi::PifRecord)> {
        let Some(host) = network::coordinator(m) else {
            return Vec::new();
        };
        network::vlan_nics(m, host, show_hidden)
    }

    /// What changed (`HasChanged` / `SaveSettings`).
    pub fn changes(&self, m: &ConnectionModel, show_hidden: bool) -> NetworkChanges {
        let Original {
            name,
            description,
            tags,
            auto_plug: auto,
            mtu,
            nic,
            vlan,
            bond,
        } = &self.original;
        let new_tags: Vec<String> = self
            .tags
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let (_, mtu_enabled, _) = self.mtu_state(m, show_hidden);
        let new_mtu = self.mtu_value(m).filter(|v| mtu_enabled && v != mtu);
        let bond_changed = self.bond.filter(|b| Some(*b) != *bond);
        let pifs = match (&self.nic, nic) {
            (Some(NicChoice::Internal), Some(NicChoice::Internal)) | (None, _) | (_, None) => None,
            (Some(NicChoice::Internal), Some(_)) => Some(PifChange::Internal),
            (Some(NicChoice::Nic(p)), old) => {
                let v = self.vlan_value();
                match v {
                    Some(v) if old.as_ref() != Some(&NicChoice::Nic(p.clone())) || v != *vlan => {
                        Some(PifChange::External {
                            pif: p.clone(),
                            vlan: v,
                        })
                    }
                    _ => None,
                }
            }
        };
        NetworkChanges {
            name: (self.name.trim() != name && !self.name.trim().is_empty())
                .then(|| self.name.trim().to_owned()),
            description: (&self.description != description).then(|| self.description.clone()),
            tags: (&new_tags != tags).then_some(new_tags),
            auto_plug: (self.auto_plug != *auto).then_some(self.auto_plug),
            mtu: new_mtu,
            bond_mode: bond_changed
                .filter(|b| bond.is_none_or(|o| o.mode() != b.mode()))
                .map(|b| b.mode()),
            hashing: bond_changed
                .and_then(|b| b.hashing())
                .filter(|h| bond.and_then(|o| o.hashing()) != Some(*h)),
            pifs,
        }
    }

    /// `WillDisrupt`.
    pub fn will_disrupt(&self, m: &ConnectionModel, show_hidden: bool) -> bool {
        let c = self.changes(m, show_hidden);
        c.mtu.is_some() || c.bond_mode.is_some() || c.hashing.is_some() || c.pifs.is_some()
    }
}

// ---- the Configure IP Addresses dialog ---------------------------------------------

/// One page of the dialog.
#[derive(Debug, Clone)]
pub struct IpForm {
    pub original: Option<Ref<Pif>>,
    pub network: Option<Ref<Network>>,
    pub dhcp: bool,
    pub ip: String,
    pub netmask: String,
    pub gateway: String,
    pub dns: [String; 3],
    /// `None` on the management page.
    pub purpose: Option<String>,
}

/// `NetworkingProperties`.
pub struct IpDialog {
    host: Ref<Host>,
    pool: bool,
    object: String,
    pub pages: Vec<IpForm>,
    pub selected: usize,
    ha: bool,
    /// The management warning, while asked.
    pub warning: Option<Box<network::IpChange>>,
}

impl IpDialog {
    /// For `host`, or the pool (`host` `None`): a pool of one host is
    /// configured as that host.
    pub fn new(
        conn: &ConnectionView,
        host: Option<Ref<Host>>,
        selected: Option<&Ref<Pif>>,
        show_hidden: bool,
    ) -> Option<Self> {
        let m = &conn.model;
        let (host, pool, object) = match host {
            Some(h) => {
                let name = m.hosts.get(&h)?.name_label.clone();
                (h, false, name)
            }
            None => {
                let (_, p) = m.pool()?;
                let coordinator = network::coordinator(m)?.clone();
                let name = objects::pool_name(m, p);
                (coordinator, m.hosts.len() > 1, name)
            }
        };
        let all = network::known_pifs(m, &host, true);
        let management = all.iter().find(|(_, p)| p.management)?;
        let form = |r: &Ref<Pif>, p: &xapi::PifRecord, purpose: Option<String>| {
            let dns: Vec<&str> = p.dns.split(',').collect();
            IpForm {
                original: Some(r.clone()),
                network: Some(p.network.clone()),
                dhcp: p.ip_configuration_mode == IpConfigurationMode::Dhcp,
                ip: p.ip.clone(),
                netmask: p.netmask.clone(),
                gateway: p.gateway.clone(),
                dns: [
                    dns.first().copied().unwrap_or_default().to_owned(),
                    dns.get(1).copied().unwrap_or_default().to_owned(),
                    dns.get(2).copied().unwrap_or_default().to_owned(),
                ],
                purpose,
            }
        };
        let mut pages = vec![form(management.0, management.1, None)];
        for (r, p) in network::known_pifs(m, &host, show_hidden) {
            if r != management.0 && objects::pif_is_secondary_management(m, p, true) {
                let purpose = network::management_purpose(p)
                    .unwrap_or(net_strings().purpose_unknown)
                    .to_owned();
                pages.push(form(r, p, Some(purpose)));
            }
        }
        let selected = selected
            .and_then(|s| pages.iter().position(|p| p.original.as_ref() == Some(s)))
            .unwrap_or(0);
        Some(Self {
            ha: pool && m.pool().is_some_and(|(_, p)| p.ha_enabled),
            host,
            pool,
            object,
            pages,
            selected,
            warning: None,
        })
    }

    /// The networks pages use, with their page indexes.
    fn in_use(&self) -> HashMap<Ref<Network>, Vec<usize>> {
        let mut map: HashMap<Ref<Network>, Vec<usize>> = HashMap::new();
        for (i, page) in self.pages.iter().enumerate() {
            if let Some(n) = &page.network {
                map.entry(n.clone()).or_default().push(i);
            }
        }
        map
    }

    fn purpose_of(&self, index: usize) -> String {
        match &self.pages[index].purpose {
            None => net_strings().management.to_owned(),
            Some(p) => p.clone(),
        }
    }

    /// The other page using `network`, by its purpose (`FindOtherPurpose`).
    pub fn other_purpose(&self, index: usize, network: &Ref<Network>) -> Option<String> {
        self.in_use()
            .get(network)?
            .iter()
            .find(|i| **i != index)
            .map(|i| self.purpose_of(*i))
    }

    /// The in-use warning of a page.
    pub fn in_use_warning(&self, m: &ConnectionModel, index: usize) -> Option<String> {
        let n = net_strings();
        let net = self.pages[index].network.as_ref()?;
        let other = self.other_purpose(index, net)?;
        let name = m
            .networks
            .get(net)
            .map(|x| objects::network_name(m, x))
            .unwrap_or_default();
        Some(
            if self.pages[index].purpose.is_some() && self.pages[0].network.as_ref() == Some(net) {
                fill(n.in_use_management, &[("network", &name)])
            } else {
                fill(n.in_use, &[("network", &name), ("purpose", &other)])
            },
        )
    }

    fn page(&self, index: usize) -> Option<IpPage> {
        let f = &self.pages[index];
        Some(IpPage {
            original: f.original.clone(),
            network: f.network.clone()?,
            dhcp: f.dhcp,
            ip: f.ip.trim().to_owned(),
            netmask: f.netmask.trim().to_owned(),
            gateway: f.gateway.trim().to_owned(),
            dns: f.dns.clone().map(|d| d.trim().to_owned()),
            purpose: f.purpose.clone(),
        })
    }

    /// Whether every page is acceptable (OK enabled).
    pub fn valid(&self, m: &ConnectionModel) -> bool {
        (0..self.pages.len()).all(|i| {
            self.page(i).is_some_and(|p| network::ip_page_valid(&p))
                && self.in_use_warning(m, i).is_none()
        })
    }

    /// Add IP address: a new secondary page on the first unused network.
    pub fn add_page(&mut self, m: &ConnectionModel, show_hidden: bool) {
        let used = self.in_use();
        let network = network::ip_networks(m, &self.host, show_hidden)
            .into_iter()
            .map(|(r, _)| r.clone())
            .find(|r| !used.contains_key(r));
        let names: Vec<String> = self
            .pages
            .iter()
            .filter_map(|p| p.purpose.clone())
            .collect();
        let purpose = (1..)
            .map(|i| fill(net_strings().aux_tab_name, &[("n", &i.to_string())]))
            .find(|n| !names.contains(n))
            .unwrap_or_default();
        self.pages.push(IpForm {
            original: None,
            network,
            dhcp: true,
            ip: String::new(),
            netmask: String::new(),
            gateway: String::new(),
            dns: Default::default(),
            purpose: Some(purpose),
        });
        self.selected = self.pages.len() - 1;
    }

    /// Whether Add IP address is enabled (`ShownPIFs.Count > pages`).
    pub fn can_add(&self, m: &ConnectionModel, show_hidden: bool) -> bool {
        network::known_pifs(m, &self.host, show_hidden).len() > self.pages.len()
    }

    pub fn remove_page(&mut self, index: usize) {
        if index > 0 && index < self.pages.len() {
            self.pages.remove(index);
            self.selected = self.selected.min(self.pages.len() - 1);
        }
    }

    /// OK: the requests to run now, or `None` while the management
    /// warning is asked (or nothing changed).
    pub fn accept(
        &mut self,
        m: &ConnectionModel,
        show_hidden: bool,
        confirmed: bool,
    ) -> Result<Vec<ActionRequest>, String> {
        let pages: Vec<IpPage> = (0..self.pages.len()).filter_map(|i| self.page(i)).collect();
        let plan = network::plan_ip_changes(m, &self.host, self.pool, &pages, show_hidden)
            .map_err(|_| net_strings().reconfig_connection_lost.to_owned())?;
        let mut out = Vec::new();
        if let Some(change) = plan.change {
            if plan.warn_management && !confirmed {
                self.warning = Some(Box::new(change));
                return Ok(Vec::new());
            }
            if !plan.renames.is_empty() {
                out.push(ActionRequest::RenameInterfaces {
                    pool: self.pool,
                    pifs: plan.renames,
                });
            }
            out.push(ActionRequest::ChangeNetworking(Box::new(change)));
        } else if !plan.renames.is_empty() {
            out.push(ActionRequest::RenameInterfaces {
                pool: self.pool,
                pifs: plan.renames,
            });
        }
        Ok(out)
    }
}

// ---- the networking part of the window ----------------------------------------------

enum Dialog {
    Vif(VifDialog),
    Properties(Box<NetworkProperties>),
    Ip(Box<IpDialog>),
    Bond(BondForm, Option<BondCheck>),
}

/// The networking part of the window.
#[derive(Default)]
pub struct NetUi {
    /// The New Network wizard, when open.
    pub wizard: Option<NewNetworkWizard>,
    dialog: Option<(ConnectionId, Dialog)>,
    vif_selection: Option<(ConnectionId, Ref<Vif>)>,
    network_selection: Option<(ConnectionId, Ref<Network>)>,
    ip_selection: Option<(ConnectionId, Ref<Pif>)>,
    nic_selection: Option<(ConnectionId, Ref<Pif>)>,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
}

impl NetUi {
    /// The open VIF dialog (tests).
    #[cfg(test)]
    pub fn vif_dialog(&mut self) -> Option<&mut VifDialog> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Vif(d))) => Some(d),
            _ => None,
        }
    }

    /// The open network Properties dialog (tests).
    #[cfg(test)]
    pub fn properties(&mut self) -> Option<&mut NetworkProperties> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Properties(d))) => Some(d),
            _ => None,
        }
    }

    /// The open Configure IP Addresses dialog (tests).
    #[cfg(test)]
    pub fn ip_dialog(&mut self) -> Option<&mut IpDialog> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Ip(d))) => Some(d),
            _ => None,
        }
    }

    /// The open Create Bond form (tests).
    #[cfg(test)]
    pub fn bond_form(&mut self) -> Option<&mut BondForm> {
        match self.dialog.as_mut() {
            Some((_, Dialog::Bond(f, _))) => Some(f),
            _ => None,
        }
    }

    /// Selects a row of the tabs (tests).
    #[cfg(test)]
    pub fn select_vif(&mut self, connection: ConnectionId, vif: Ref<Vif>) {
        self.vif_selection = Some((connection, vif));
    }

    /// Selects a network of the host or pool tab (tests).
    #[cfg(test)]
    pub fn select_network(&mut self, connection: ConnectionId, network: Ref<Network>) {
        self.network_selection = Some((connection, network));
    }

    /// Selects a NIC of the NICs tab (tests).
    #[cfg(test)]
    pub fn select_nic(&mut self, connection: ConnectionId, pif: Ref<Pif>) {
        self.nic_selection = Some((connection, pif));
    }

    // ---- the VM Networking tab ----------------------------------------------

    /// The Networking tab of a VM: its interfaces with the buttons.
    #[allow(clippy::too_many_arguments)]
    pub fn vm_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        vm_ref: &Ref<Vm>,
        vm: &VmRecord,
        ctx: tabs::Ctx<'_>,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let n = net_strings();
        let m = &conn.model;
        let (table, refs) = tabs::network::vm_interface_rows(ctx, vm);
        if self
            .vif_selection
            .as_ref()
            .is_some_and(|(c, r)| *c != conn.id || !refs.contains(r))
        {
            self.vif_selection = None;
        }
        let selected = self
            .vif_selection
            .as_ref()
            .and_then(|(_, r)| refs.iter().position(|x| x == r));
        let vif = selected.and_then(|i| m.vifs.get_key_value(&refs[i]));
        let locked =
            !conn.status.is_connected() || conn.busy_vms.contains(vm_ref) || vm.is_a_snapshot;
        let hint = network::vif_hint(m, vm);
        let mut request: Option<(ActionRequest, bool)> = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!locked, Button::new(n.add_interface))
                .clicked()
            {
                let max = network::max_vifs(vm);
                if refs.len() >= max {
                    self.errors
                        .push(fill(n.vifs_max, &[("max", &max.to_string())]));
                } else if let Some(d) = VifDialog::new(conn, vm_ref.clone(), None, ctx.show_hidden)
                {
                    self.dialog = Some((conn.id, Dialog::Vif(d)));
                }
            }
            let can_edit = !locked && vif.is_some_and(|(_, v)| network::vif_can_remove(v));
            let mut r = ui.add_enabled(can_edit, Button::new(n.interface_properties));
            if let Some(h) = hint {
                r = r.on_hover_text(match h {
                    VifHint::Suspended | VifHint::NoIoDrivers => n.tooltip_edit_io_drivers,
                });
            }
            if r.clicked()
                && let Some((r, _)) = vif
                && let Some(d) =
                    VifDialog::new(conn, vm_ref.clone(), Some(r.clone()), ctx.show_hidden)
            {
                self.dialog = Some((conn.id, Dialog::Vif(d)));
            }
            let mut r = ui.add_enabled(can_edit, Button::new(n.remove_interface));
            if let Some(h) = hint {
                r = r.on_hover_text(match h {
                    VifHint::Suspended => n.tooltip_remove_suspended,
                    VifHint::NoIoDrivers => n.tooltip_remove_io_drivers,
                });
            }
            if r.clicked()
                && let Some((r, _)) = vif
            {
                request = Some((ActionRequest::DeleteVif { vif: r.clone() }, true));
            }
            let attached = vif.is_some_and(|(_, v)| v.currently_attached);
            let can_toggle = !locked && vif.is_some_and(|(_, v)| network::vif_can_toggle(v));
            let label = if attached { n.deactivate } else { n.activate };
            let mut r = ui.add_enabled(can_toggle, Button::new(label));
            if let Some(h) = hint {
                r = r.on_hover_text(match (h, attached) {
                    (VifHint::Suspended, true) => n.tooltip_deactivate_suspended,
                    (VifHint::Suspended, false) => n.tooltip_activate_suspended,
                    (VifHint::NoIoDrivers, true) => n.tooltip_deactivate_tools,
                    (VifHint::NoIoDrivers, false) => n.tooltip_activate_tools,
                });
            }
            if r.clicked()
                && let Some((r, _)) = vif
            {
                let vif = r.clone();
                request = Some((
                    if attached {
                        ActionRequest::UnplugVif { vif }
                    } else {
                        ActionRequest::PlugVif { vif }
                    },
                    false,
                ));
            }
            if conn.busy_vms.contains(vm_ref) {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        if let Some(i) = select_table(ui, "vifs", &table, selected) {
            self.vif_selection = Some((conn.id, refs[i].clone()));
        }
        match request {
            Some((r, true)) => ops.request(conn, r, backend),
            Some((r, false)) => ops.dispatch(conn, r, backend),
            None => {}
        }
    }

    // ---- the host / pool Networking tab ----------------------------------------

    /// The Networking tab of a host (`host`) or of the pool.
    #[allow(clippy::too_many_arguments)]
    pub fn host_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        host: Option<&Ref<Host>>,
        ctx: tabs::Ctx<'_>,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let n = net_strings();
        let t = crate::tab_strings::tab_strings();
        let m = &conn.model;
        let usable = conn.status.is_connected();
        ui.strong(t.networks_label);
        let (table, refs) = tabs::network::network_rows(ctx, host);
        if self
            .network_selection
            .as_ref()
            .is_some_and(|(c, r)| *c != conn.id || !refs.contains(r))
        {
            self.network_selection = None;
        }
        let selected = self
            .network_selection
            .as_ref()
            .and_then(|(_, r)| refs.iter().position(|x| x == r));
        let net = selected.and_then(|i| m.networks.get_key_value(&refs[i]));
        ui.horizontal(|ui| {
            if ui.add_enabled(usable, Button::new(n.add_network)).clicked()
                && let Some(w) = NewNetworkWizard::new(conn, host.cloned())
            {
                self.wizard = Some(w);
            }
            let can_edit = usable && net.is_some_and(|(_, x)| network::network_can_edit(m, x));
            if ui
                .add_enabled(can_edit, Button::new(n.network_properties))
                .clicked()
                && let Some((r, _)) = net
                && let Some(d) = NetworkProperties::new(conn, r.clone())
            {
                self.dialog = Some((conn.id, Dialog::Properties(Box::new(d))));
            }
            let physical = net.is_some_and(|(_, x)| network::network_has_physical_nic(m, x));
            let can_remove = can_edit && !physical;
            let mut r = ui.add_enabled(can_remove, Button::new(n.remove_network));
            if physical {
                r = r.on_disabled_hover_text(n.tooltip_remove_pif);
            }
            if r.clicked()
                && let Some((r, x)) = net
            {
                if network::network_is_bond(m, x) {
                    self.delete_bond(conn, r, ops, backend);
                } else {
                    ops.request(
                        conn,
                        ActionRequest::RemoveNetwork { network: r.clone() },
                        backend,
                    );
                }
            }
        });
        ui.add_space(4.0);
        if let Some(i) = select_table(ui, "networks", &table, selected) {
            self.network_selection = Some((conn.id, refs[i].clone()));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.strong(t.ip_config_label);
            let ha = m.pool().is_some_and(|(_, p)| p.ha_enabled);
            let mut r = ui.add_enabled(usable && !ha, Button::new(n.configure_ip));
            if ha {
                r = r.on_disabled_hover_text(n.disable_ha_configure_ip);
            }
            if r.clicked() {
                let selected = self
                    .ip_selection
                    .as_ref()
                    .filter(|(c, _)| *c == conn.id)
                    .map(|(_, p)| p);
                let target = match host {
                    Some(h) => Some(h.clone()),
                    // A pool of one host is configured as that host.
                    None if m.hosts.len() <= 1 => network::coordinator(m).cloned(),
                    None => None,
                };
                match IpDialog::new(conn, target, selected, ctx.show_hidden) {
                    Some(d) => self.dialog = Some((conn.id, Dialog::Ip(Box::new(d)))),
                    None => self.errors.push(n.reconfig_connection_lost.to_owned()),
                }
            }
        });
        let (table, refs) = tabs::network::ip_configuration_rows(ctx, host);
        let selected = self
            .ip_selection
            .as_ref()
            .filter(|(c, _)| *c == conn.id)
            .and_then(|(_, r)| refs.iter().position(|x| x == r));
        if let Some(i) = select_table(ui, "ip-config", &table, selected) {
            self.ip_selection = Some((conn.id, refs[i].clone()));
        }
    }

    /// `DestroyBondCommand` for the bond on network `network`.
    fn delete_bond(
        &mut self,
        conn: &ConnectionView,
        network_ref: &Ref<Network>,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let n = net_strings();
        let m = &conn.model;
        let Some(net) = m.networks.get(network_ref) else {
            return;
        };
        let pifs = network::network_pifs(m, net);
        let Some((_, first)) = pifs.first() else {
            tracing::error!("network has no PIFs");
            return;
        };
        let refs: Vec<Ref<Pif>> = pifs.iter().map(|(r, _)| (*r).clone()).collect();
        if network::contains_primary_management(m, &refs)
            && let Some((_, pool)) = m.pool().filter(|(_, p)| p.ha_enabled)
        {
            self.errors.push(fill(
                n.bond_delete_ha,
                &[
                    ("bond", &objects::pif_name(m, first)),
                    ("pool", &objects::pool_name(m, pool)),
                ],
            ));
            return;
        }
        if let Some((bond, _)) = network::bond_of_interface(m, first) {
            ops.request(
                conn,
                ActionRequest::DestroyBond { bond: bond.clone() },
                backend,
            );
        }
    }

    // ---- the NICs tab ---------------------------------------------------------

    /// The NICs tab of a host.
    #[allow(clippy::too_many_arguments)]
    pub fn nics_tab(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        host: &Ref<Host>,
        ctx: tabs::Ctx<'_>,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let n = net_strings();
        let m = &conn.model;
        let usable = conn.status.is_connected();
        let (table, refs) = tabs::network::nic_rows(ctx, host);
        if self
            .nic_selection
            .as_ref()
            .is_some_and(|(c, r)| *c != conn.id || !refs.contains(r))
        {
            self.nic_selection = None;
        }
        let selected = self
            .nic_selection
            .as_ref()
            .and_then(|(_, r)| refs.iter().position(|x| x == r));
        let pif = selected.and_then(|i| m.pifs.get_key_value(&refs[i]));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(usable, Button::new(n.create_bond_button))
                .clicked()
            {
                self.dialog = Some((conn.id, Dialog::Bond(BondForm::new(m, Some(host)), None)));
            }
            let bond = pif.and_then(|(_, p)| network::bond_of_interface(m, p));
            if ui
                .add_enabled(usable && bond.is_some(), Button::new(n.delete_bond_button))
                .clicked()
                && let Some((_, p)) = pif
            {
                let net = p.network.clone();
                self.delete_bond(conn, &net, ops, backend);
            }
            if ui.add_enabled(usable, Button::new(n.rescan)).clicked() {
                ops.dispatch(
                    conn,
                    ActionRequest::RescanPifs { host: host.clone() },
                    backend,
                );
            }
        });
        ui.add_space(4.0);
        if let Some(i) = select_table(ui, "nics", &table, selected) {
            self.nic_selection = Some((conn.id, refs[i].clone()));
        }
    }

    // ---- windows ----------------------------------------------------------------

    /// Draws the wizard and the open dialog.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(wizard) = self.wizard.as_mut() {
            match wizard.show(ctx, state) {
                WizardOutcome::Open => {}
                WizardOutcome::Cancelled => self.wizard = None,
                WizardOutcome::Finish(connection, request) => {
                    self.wizard = None;
                    if let Some(conn) = state.connection(connection) {
                        ops.dispatch(conn, request, backend);
                    }
                }
            }
        }
        let show_hidden = state.settings.preferences.show_hidden_objects;
        let Some((connection, dialog)) = self.dialog.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(*connection) else {
            self.dialog = None;
            return;
        };
        let mut errors = Vec::new();
        let close = match dialog {
            Dialog::Vif(d) => vif_window(ctx, conn, d, ops, backend, show_hidden),
            Dialog::Properties(d) => properties_window(ctx, conn, d, ops, backend, show_hidden),
            Dialog::Ip(d) => ip_window(ctx, conn, d, ops, backend, show_hidden, &mut errors),
            Dialog::Bond(form, question) => bond_window(ctx, conn, form, question, ops, backend),
        };
        self.errors.append(&mut errors);
        if close {
            self.dialog = None;
        }
    }
}

fn window(title: String) -> DialogWindow<'static> {
    dialog_window(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
}

/// Returns whether the dialog closes.
fn vif_window(
    ctx: &Context,
    conn: &ConnectionView,
    d: &mut VifDialog,
    ops: &mut OpsUi,
    backend: &BackendHandle,
    show_hidden: bool,
) -> bool {
    let n = net_strings();
    let m = &conn.model;
    let mut close = false;
    let mut accept = None;
    let title = if d.existing.is_some() {
        n.vif_properties_title
    } else {
        n.add_vif_title
    };
    window(title.to_owned()).show(ctx, |ui| {
        ui.set_max_width(460.0);
        if let Some(vm) = d.duplicate.clone() {
            ui.label(RichText::new(n.duplicate_mac_title).strong());
            ui.label(fill(n.duplicate_mac, &[("mac", &d.mac), ("vm", &vm)]));
            dialog_buttons(ui, |row| {
                if row.button(ButtonRole::Primary, n.yes).clicked() {
                    d.duplicate = None;
                    accept = Some(true);
                }
                if row.button(ButtonRole::Cancel, n.no).clicked() {
                    d.duplicate = None;
                }
            });
            return;
        }
        ui.label(n.vif_blurb);
        ui.add_space(6.0);
        let networks = network::vif_networks(m, show_hidden);
        let selected = d
            .network
            .as_ref()
            .and_then(|r| m.networks.get(r))
            .map(|x| objects::network_name(m, x))
            .unwrap_or_else(|| n.none.to_owned());
        Grid::new("vif-dialog")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                let label = ui.label(n.network_label);
                ComboBox::from_id_salt("vif-network")
                    .selected_text(selected)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for (r, x) in &networks {
                            if ui
                                .selectable_label(
                                    d.network.as_ref() == Some(*r),
                                    objects::network_name(m, x),
                                )
                                .clicked()
                            {
                                d.network = Some((*r).clone());
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
                ui.label(n.mac_label);
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.auto_mac, true, n.mac_auto);
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.auto_mac, false, n.mac_use);
                        let r = ui.add(
                            TextEdit::singleline(&mut d.mac)
                                .hint_text("aa:bb:cc:dd:ee:ff")
                                .desired_width(140.0),
                        );
                        if r.gained_focus() || r.changed() {
                            d.auto_mac = false;
                        }
                    });
                });
                ui.end_row();
                ui.label(n.qos_label);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.qos, n.qos_enable);
                    let r = ui.add(TextEdit::singleline(&mut d.qos_limit).desired_width(70.0));
                    if r.gained_focus() || r.changed() {
                        d.qos = true;
                    }
                    ui.label(n.qos_units);
                });
                ui.end_row();
            });
        let valid = d.validate();
        if let Err(Some(text)) = valid {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
        ui.add_space(8.0);
        dialog_buttons(ui, |row| {
            let ok = if d.existing.is_some() { n.ok } else { n.add };
            if row
                .add_enabled(ButtonRole::Primary, valid.is_ok(), Button::new(ok))
                .clicked()
            {
                accept = Some(false);
            }
            if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                close = true;
            }
        });
    });
    if let Some(confirmed) = accept {
        if let Some(request) = d.accept(m, confirmed) {
            ops.dispatch(conn, request, backend);
            close = true;
        } else if d.duplicate.is_none() {
            // Nothing changed: the dialog closes as cancelled.
            close = true;
        }
    }
    close
}

/// Returns whether the dialog closes.
fn properties_window(
    ctx: &Context,
    conn: &ConnectionView,
    d: &mut NetworkProperties,
    ops: &mut OpsUi,
    backend: &BackendHandle,
    show_hidden: bool,
) -> bool {
    let n = net_strings();
    let m = &conn.model;
    let Some(net) = m.networks.get(&d.network).cloned() else {
        return true;
    };
    let mut close = false;
    let mut save = false;
    window(fill(
        n.properties_title,
        &[("name", &objects::network_name(m, &net))],
    ))
    .show(ctx, |ui| {
        ui.set_min_width(600.0);
        let mut divider = 0.0;
        let row = ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(140.0);
                let pages = [
                    (PropertiesTab::General, n.tab_general, ""),
                    (PropertiesTab::Settings, n.tab_network_settings, ""),
                ];
                if let Some(tab) = crate::controls::page_list(ui, &d.tab, pages) {
                    d.tab = tab;
                }
            });
            divider = crate::controls::divider_gap(ui);
            ui.vertical(|ui| {
                ui.set_min_width(420.0);
                match d.tab {
                    PropertiesTab::General => {
                        Grid::new("network-general").num_columns(2).show(ui, |ui| {
                            let label = ui.label(n.name_label);
                            ui.add(TextEdit::singleline(&mut d.name).desired_width(320.0))
                                .labelled_by(label.id);
                            ui.end_row();
                            let label = ui.label(n.description_label);
                            ui.add(
                                TextEdit::multiline(&mut d.description)
                                    .desired_rows(3)
                                    .desired_width(320.0),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                            let label = ui.label(n.tags_label);
                            ui.add(
                                TextEdit::singleline(&mut d.tags)
                                    .hint_text(n.tags_hint)
                                    .desired_width(320.0),
                            )
                            .labelled_by(label.id);
                            ui.end_row();
                        });
                    }
                    PropertiesTab::Settings => settings_page(ui, m, d, show_hidden),
                }
            });
        });
        crate::controls::paint_divider(ui, divider, row.response.rect);
        ui.add_space(8.0);
        dialog_buttons(ui, |row| {
            if row
                .add_enabled(
                    ButtonRole::Primary,
                    !d.name.trim().is_empty(),
                    Button::new(n.ok),
                )
                .clicked()
            {
                save = true;
                close = true;
            }
            if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                close = true;
            }
        });
    });
    if save {
        let changes = d.changes(m, show_hidden);
        if !changes.is_empty() {
            ops.dispatch(
                conn,
                ActionRequest::SetNetworkProperties {
                    network: d.network.clone(),
                    changes,
                },
                backend,
            );
        }
    }
    close
}

/// The Network Settings page (`EditNetworkPage`).
fn settings_page(ui: &mut Ui, m: &ConnectionModel, d: &mut NetworkProperties, show_hidden: bool) {
    let n = net_strings();
    ui.label(n.settings_blurb);
    ui.add_space(6.0);
    if d.nic.is_some() {
        let nics = NetworkProperties::nic_choices(m, show_hidden);
        let selected = match &d.nic {
            Some(NicChoice::Nic(p)) => m
                .get(p)
                .map(|x| objects::pif_name(m, x))
                .unwrap_or_default(),
            _ => n.internal.to_owned(),
        };
        let nic_enabled = d.nic_enabled(m, show_hidden);
        let vlan_enabled = d.vlan_enabled(m, show_hidden);
        Grid::new("network-settings").num_columns(2).show(ui, |ui| {
            let label = ui.label(n.nic_label);
            ui.add_enabled_ui(nic_enabled, |ui| {
                ComboBox::from_id_salt("network-settings-nic")
                    .selected_text(selected)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(d.nic == Some(NicChoice::Internal), n.internal)
                            .clicked()
                        {
                            d.nic = Some(NicChoice::Internal);
                        }
                        for (r, p) in &nics {
                            let choice = NicChoice::Nic((*r).clone());
                            if ui
                                .selectable_label(
                                    d.nic.as_ref() == Some(&choice),
                                    objects::pif_name(m, p),
                                )
                                .clicked()
                            {
                                d.nic = Some(choice);
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
            });
            ui.end_row();
            ui.label("");
            ui.label(RichText::new(n.nic_help).weak());
            ui.end_row();
            let label = ui.label(n.vlan_label);
            ui.add_enabled(
                vlan_enabled,
                TextEdit::singleline(&mut d.vlan).desired_width(70.0),
            )
            .labelled_by(label.id);
            ui.end_row();
        });
        if vlan_enabled && d.vlan.trim() == "0" {
            ui.label(RichText::new(n.vlan_zero).weak());
        }
        if let Some(w) = d.nic_warning(m, show_hidden) {
            ui.label(RichText::new(w).weak());
        }
    }
    if let Some(choice) = d.bond.as_mut() {
        ui.add_space(6.0);
        ui.label(RichText::new(n.bond_mode).strong());
        ui.radio_value(choice, BondChoice::ActiveActive, n.mode_active_active);
        ui.radio_value(choice, BondChoice::ActivePassive, n.mode_active_passive);
        if network::supports_lacp(m) {
            ui.radio_value(choice, BondChoice::LacpSrcMac, n.mode_lacp_src_mac);
            ui.radio_value(choice, BondChoice::LacpTcpUdp, n.mode_lacp_tcpudp);
        }
        if choice.hashing().is_some() {
            ui.colored_label(ui.visuals().warn_fg_color, n.lacp_warning);
        }
    }
    let (shown, enabled, remark) = d.mtu_state(m, show_hidden);
    if shown {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = ui.label(n.mtu_label);
            ui.add_enabled(
                enabled,
                TextEdit::singleline(&mut d.mtu).desired_width(70.0),
            )
            .labelled_by(label.id);
        });
        if let Some(r) = remark {
            ui.label(RichText::new(r).weak());
        }
    }
    ui.add_space(6.0);
    let is_guest_installer = m
        .networks
        .get(&d.network)
        .is_some_and(|x| objects::is_guest_installer_network(x));
    ui.add_enabled(
        !is_guest_installer,
        egui::Checkbox::new(&mut d.auto_plug, n.auto_add_properties),
    );
    if d.will_disrupt(m, show_hidden) {
        ui.add_space(6.0);
        ui.colored_label(ui.visuals().warn_fg_color, n.disruption_warning);
    }
}

/// Returns whether the dialog closes.
fn ip_window(
    ctx: &Context,
    conn: &ConnectionView,
    d: &mut IpDialog,
    ops: &mut OpsUi,
    backend: &BackendHandle,
    show_hidden: bool,
    errors: &mut Vec<String>,
) -> bool {
    let n = net_strings();
    let m = &conn.model;
    let mut close = false;
    let mut accept = None;
    window(fill(n.ip_title, &[("name", &d.object)])).show(ctx, |ui| {
        ui.set_min_width(680.0);
        if let Some(change) = &d.warning {
            let text = if change.pool {
                n.warn_management_pool
            } else {
                n.warn_management_host
            };
            ui.colored_label(
                ui.visuals().warn_fg_color,
                fill(text, &[("console", branding::PRODUCT_NAME)]),
            );
            dialog_buttons(ui, |row| {
                if row.button(ButtonRole::Primary, n.ok).clicked() {
                    accept = Some(true);
                }
                if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                    d.warning = None;
                }
            });
            return;
        }
        let blurb = if d.pool {
            n.ip_blurb_pool
        } else {
            n.ip_blurb_host
        };
        ui.label(fill(blurb, &[("name", &d.object)]));
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(170.0);
                for i in 0..d.pages.len() {
                    let page = &d.pages[i];
                    let title = match &page.purpose {
                        None => n.management_tab.to_owned(),
                        Some(p) => p.clone(),
                    };
                    let sub = page
                        .network
                        .as_ref()
                        .and_then(|r| m.networks.get(r))
                        .map(|x| {
                            fill(
                                n.page_subtext,
                                &[
                                    ("network", &objects::network_name(m, x)),
                                    ("mode", if page.dhcp { n.pif_dhcp } else { n.pif_static }),
                                ],
                            )
                        })
                        .unwrap_or_else(|| n.none.to_owned());
                    if ui
                        .selectable_label(d.selected == i, format!("{title}\n{sub}"))
                        .clicked()
                    {
                        d.selected = i;
                    }
                }
                if ui
                    .add_enabled(d.can_add(m, show_hidden), Button::new(n.add_ip))
                    .clicked()
                {
                    d.add_page(m, show_hidden);
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                ip_page(ui, m, d, show_hidden);
            });
        });
        ui.add_space(8.0);
        dialog_buttons(ui, |row| {
            if row
                .add_enabled(ButtonRole::Primary, d.valid(m), Button::new(n.ok))
                .clicked()
            {
                accept = Some(false);
            }
            if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                close = true;
            }
        });
    });
    if let Some(confirmed) = accept {
        match d.accept(m, show_hidden, confirmed) {
            Err(e) => {
                errors.push(e);
                close = true;
            }
            Ok(requests) if requests.is_empty() && d.warning.is_some() => {}
            Ok(requests) => {
                for r in requests {
                    ops.dispatch(conn, r, backend);
                }
                close = true;
            }
        }
    }
    close
}

/// One page of the Configure IP Addresses dialog (`NetworkingPropertiesPage`).
fn ip_page(ui: &mut Ui, m: &ConnectionModel, d: &mut IpDialog, show_hidden: bool) {
    let n = net_strings();
    let index = d.selected.min(d.pages.len().saturating_sub(1));
    let secondary = d.pages[index].purpose.is_some();
    let primary_ha = !secondary && d.ha;
    let networks = network::ip_networks(m, &d.host, show_hidden);
    let labels: Vec<(Ref<Network>, String)> = networks
        .iter()
        .filter(|(_, x)| secondary || !network::network_is_sriov(m, x))
        .map(|(r, x)| {
            let name = objects::network_name(m, x);
            let label = match d.other_purpose(index, r) {
                Some(p) if Some(&p) != d.pages[index].purpose.as_ref() => {
                    fill(n.network_in_use_by, &[("network", &name), ("purpose", &p)])
                }
                _ => name,
            };
            ((*r).clone(), label)
        })
        .collect();
    let warning = d.in_use_warning(m, index);
    let pool = d.pool;
    let hosts = m.hosts.len();
    let mut remove = false;
    ui.add_enabled_ui(!primary_ha, |ui| {
        let page = &mut d.pages[index];
        if primary_ha {
            ui.colored_label(ui.visuals().warn_fg_color, n.ha_enabled);
        }
        Grid::new(("ip-page", index))
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                if let Some(purpose) = page.purpose.as_mut() {
                    let label = ui.label(n.name_label);
                    ui.add(TextEdit::singleline(purpose).desired_width(240.0))
                        .labelled_by(label.id);
                    ui.end_row();
                }
                let label = ui.label(n.network_label);
                let selected = page
                    .network
                    .as_ref()
                    .and_then(|r| labels.iter().find(|(x, _)| x == r))
                    .map(|(_, l)| l.clone())
                    .unwrap_or_default();
                ComboBox::from_id_salt(("ip-network", index))
                    .selected_text(selected)
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        for (r, l) in &labels {
                            if ui
                                .selectable_label(page.network.as_ref() == Some(r), l)
                                .clicked()
                            {
                                page.network = Some(r.clone());
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            });
        if let Some(w) = &warning {
            ui.colored_label(ui.visuals().warn_fg_color, w);
        }
        ui.add_space(6.0);
        ui.label(if secondary {
            n.ip_settings
        } else {
            n.ip_dns_settings
        });
        ui.radio_value(&mut page.dhcp, true, n.dhcp);
        ui.radio_value(&mut page.dhcp, false, n.static_ip);
        ui.add_enabled_ui(!page.dhcp, |ui| {
            Grid::new(("ip-static", index))
                .num_columns(3)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    let label = ui.label(if pool { n.ip_range } else { n.ip_address });
                    ui.add(TextEdit::singleline(&mut page.ip).desired_width(140.0))
                        .labelled_by(label.id);
                    if pool && network::is_ip_address(page.ip.trim()) {
                        let end = network::ip_in_range(page.ip.trim(), hosts.saturating_sub(1))
                            .unwrap_or_default();
                        ui.label(fill(n.ip_range_end, &[("end", &end)]));
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                    for (label_text, field) in [
                        (n.subnet, &mut page.netmask),
                        (n.gateway, &mut page.gateway),
                    ] {
                        let label = ui.label(label_text);
                        ui.add(TextEdit::singleline(field).desired_width(140.0))
                            .labelled_by(label.id);
                        ui.label("");
                        ui.end_row();
                    }
                    if !secondary {
                        let [a, b, c] = &mut page.dns;
                        for (label_text, field) in
                            [(n.dns_preferred, a), (n.dns_alt1, b), (n.dns_alt2, c)]
                        {
                            let label = ui.label(label_text);
                            ui.add(TextEdit::singleline(field).desired_width(140.0))
                                .labelled_by(label.id);
                            ui.label("");
                            ui.end_row();
                        }
                    }
                });
        });
        if secondary {
            ui.add_space(6.0);
            if ui.button(n.remove_ip).clicked() {
                remove = true;
            }
        }
    });
    if remove {
        d.remove_page(index);
    }
}

/// The Create Bond dialog (`BondProperties`); returns whether it closes.
fn bond_window(
    ctx: &Context,
    conn: &ConnectionView,
    form: &mut BondForm,
    question: &mut Option<BondCheck>,
    ops: &mut OpsUi,
    backend: &BackendHandle,
) -> bool {
    let n = net_strings();
    let m = &conn.model;
    let mut close = false;
    let mut create = false;
    window(n.bond_create_title.to_owned()).show(ctx, |ui| {
        ui.set_min_width(700.0);
        if let Some(q) = question.clone() {
            match q {
                BondCheck::Error(text) => {
                    ui.colored_label(ui.visuals().error_fg_color, text);
                    dialog_buttons(ui, |row| {
                        if row.button(ButtonRole::Primary, n.ok).clicked() {
                            *question = None;
                        }
                    });
                }
                BondCheck::Warn(text, yes) => {
                    ui.colored_label(ui.visuals().warn_fg_color, text);
                    dialog_buttons(ui, |row| {
                        if row.button(ButtonRole::Primary, yes).clicked() {
                            *question = None;
                            create = true;
                        }
                        if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                            *question = None;
                        }
                    });
                }
                BondCheck::Ok => *question = None,
            }
            return;
        }
        form.ui(ui, m, "create-bond");
        ui.add_space(8.0);
        dialog_buttons(ui, |row| {
            if row
                .add_enabled(ButtonRole::Primary, form.valid(m), Button::new(n.create))
                .clicked()
            {
                match form.check(m) {
                    BondCheck::Ok => create = true,
                    other => *question = Some(other),
                }
            }
            if row.button(ButtonRole::Cancel, n.cancel).clicked() {
                close = true;
            }
        });
    });
    if create && let Some(spec) = form.spec(m) {
        ops.dispatch(conn, ActionRequest::NewBond(Box::new(spec)), backend);
        close = true;
    }
    close
}
