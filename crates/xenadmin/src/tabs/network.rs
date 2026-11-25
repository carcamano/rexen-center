//! Networking and NICs tabs (`NetworkPage`, `NetworkList`, `NICPage`).

use xapi::{Host, IpConfigurationMode, PifRecord, Ref, VmRecord};
use xen_core::objects::{self, LinkStatus, NetworkLinkStatus};

use super::{Ctx, Table, yes_no};
use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// The virtual interfaces of a VM, by device number.
pub fn vm_interfaces(ctx: Ctx<'_>, vm: &VmRecord) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_device,
        t.col_mac,
        t.col_limit,
        t.col_network,
        t.col_ip,
        t.col_active,
    ]);
    let mut vifs: Vec<&xapi::VifRecord> = vm
        .vifs
        .iter()
        .filter_map(|r| m.get(r))
        .map(|v| &**v)
        .filter(|vif| {
            ctx.show_hidden
                || !m
                    .get(&vif.network)
                    .is_some_and(|n| objects::is_guest_installer_network(n))
        })
        .collect();
    vifs.sort_by(|a, b| objects::natural_cmp(&a.device, &b.device));
    for vif in vifs {
        let ips = if vm.is_a_template {
            Vec::new()
        } else {
            objects::vif_ip_addresses(m, vm, &vif.device)
        };
        table.push(vec![
            vif.device.clone(),
            if vif.mac.is_empty() {
                t.mac_auto.to_owned()
            } else {
                vif.mac.clone()
            },
            if vif.qos_algorithm_type.is_empty() {
                String::new()
            } else {
                vif.qos_algorithm_params
                    .get("kbps")
                    .cloned()
                    .unwrap_or_default()
            },
            m.get(&vif.network)
                .map(|n| objects::network_name(m, n))
                .unwrap_or_else(|| t.hyphen.to_owned()),
            if ips.is_empty() {
                t.unknown.to_owned()
            } else {
                ips.join(", ")
            },
            yes_no(vif.currently_attached),
        ]);
    }
    table
}

fn link_label(status: LinkStatus) -> &'static str {
    let t = tab_strings();
    match status {
        LinkStatus::Connected => t.link_connected,
        LinkStatus::Disconnected => t.link_disconnected,
        LinkStatus::Unknown => t.unknown,
    }
}

/// The networks of a host or, with `host` `None`, of the pool.
pub fn networks(ctx: Ctx<'_>, host: Option<&Ref<Host>>) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_name,
        t.col_description,
        t.col_nic,
        t.col_vlan,
        t.col_auto,
        t.col_link_status,
        t.col_mac,
        t.col_mtu,
    ]);
    let mut rows = Vec::new();
    for (_, network) in objects::shown_networks(m, ctx.show_hidden) {
        let pif = objects::network_pif(m, network, host).map(|(_, p)| &**p);
        let mut name = objects::network_name(m, network);
        let bond_member = objects::is_bond_member_network(m, network);
        if ctx.show_hidden {
            if bond_member {
                name.push_str(t.bond_member_suffix);
            } else if objects::is_guest_installer_network(network)
                || network
                    .other_config
                    .get("HideFromXenCenter")
                    .map(String::as_str)
                    == Some("true")
            {
                name.push_str(t.hidden_suffix);
            }
        }
        let link = match host {
            None => match objects::network_link_status(m, network) {
                NetworkLinkStatus::None => t.none,
                NetworkLinkStatus::Connected => t.link_connected,
                NetworkLinkStatus::PartiallyConnected => t.link_partial,
                NetworkLinkStatus::Disconnected => t.link_disconnected,
                NetworkLinkStatus::Unknown => t.unknown,
            },
            Some(_) => match pif {
                None => t.none,
                Some(pif) => link_label(objects::pif_link_status(m, pif)),
            },
        };
        let tunnel = pif.is_some_and(|p| !p.tunnel_access_pif_of.is_empty());
        let cells = vec![
            name,
            network.name_description.clone(),
            pif.map(|p| objects::pif_name(m, p)).unwrap_or_default(),
            match pif {
                Some(p) if p.vlan != -1 => p.vlan.to_string(),
                _ => t.spaced_hyphen.to_owned(),
            },
            yes_no(network.other_config.get("automatic").map(String::as_str) != Some("false")),
            link.to_owned(),
            match pif {
                Some(p) if p.physical => p.mac.clone(),
                _ => t.spaced_hyphen.to_owned(),
            },
            if tunnel {
                t.spaced_hyphen.to_owned()
            } else {
                network.mtu.to_string()
            },
        ];
        rows.push((cells, bond_member));
    }
    // The original sorts by the NIC column.
    rows.sort_by(|(a, _), (b, _)| objects::natural_cmp(&a[2], &b[2]));
    for (cells, greyed) in rows {
        table.push(cells);
        if let Some(last) = table.rows.last_mut() {
            last.greyed = greyed;
        }
    }
    table
}

/// "IP Address Configuration": the management interfaces of a host or of
/// every host of the pool.
pub fn ip_configuration(ctx: Ctx<'_>, host: Option<&Ref<Host>>) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_server,
        t.col_interface,
        t.col_network,
        t.col_nic,
        t.col_ip_setup,
        t.col_ip,
        t.col_subnet,
        t.col_gateway,
        t.col_dns,
    ]);
    let hosts: Vec<_> = objects::sorted_hosts(m)
        .into_iter()
        .filter(|(r, _)| host.is_none_or(|h| h == *r))
        .collect();
    for (host_ref, host) in hosts {
        let mut pifs: Vec<&PifRecord> = m
            .pifs
            .values()
            .filter(|p| &p.host == host_ref)
            .map(|p| &**p)
            .filter(|p| {
                let primary = p.management
                    && !m
                        .get(&p.network)
                        .is_some_and(|n| objects::is_guest_installer_network(n));
                primary || objects::pif_is_secondary_management(m, p, ctx.show_hidden)
            })
            .collect();
        pifs.sort_by(|a, b| {
            b.management.cmp(&a.management).then_with(|| {
                objects::natural_cmp(&objects::pif_name(m, a), &objects::pif_name(m, b))
            })
        });
        for pif in pifs {
            table.push(vec![
                host.name_label.clone(),
                if pif.management {
                    t.interface_management.to_owned()
                } else {
                    pif.other_config
                        .get("management_purpose")
                        .cloned()
                        .unwrap_or_else(|| t.interface_unknown.to_owned())
                },
                m.get(&pif.network)
                    .map(|n| objects::network_name(m, n))
                    .unwrap_or_default(),
                objects::pif_name(m, pif),
                match pif.ip_configuration_mode {
                    IpConfigurationMode::None => t.ip_none,
                    IpConfigurationMode::Dhcp => t.ip_dhcp,
                    IpConfigurationMode::Static => t.ip_static,
                    _ => t.unknown,
                }
                .to_owned(),
                pif.ip.clone(),
                pif.netmask.clone(),
                pif.gateway.clone(),
                pif.dns.clone(),
            ]);
        }
    }
    table
}

/// SR-IOV capability text of a NIC (without the remaining-VF count, which
/// needs an API call).
fn sriov_text(ctx: Ctx<'_>, pif: &PifRecord) -> String {
    let t = tab_strings();
    let m = ctx.model;
    if !pif.capabilities.iter().any(|c| c == "sriov") {
        return t.no.to_owned();
    }
    let Some(sriov) = pif.sriov_physical_pif_of.first() else {
        return t.sriov_create.to_owned();
    };
    match m.get(sriov) {
        None => t.sriov_reboot.to_owned(),
        Some(s) if s.requires_reboot => t.sriov_reboot.to_owned(),
        Some(s) if !m.get(&s.logical_pif).is_some_and(|p| p.currently_attached) => {
            t.sriov_unplugged.to_owned()
        }
        Some(_) => t.yes.to_owned(),
    }
}

/// The physical NICs (and bonds) of a host, in the server's order.
pub fn nics(ctx: Ctx<'_>, host: &Ref<Host>) -> Table {
    let t = tab_strings();
    let m = ctx.model;
    let mut table = Table::new(vec![
        t.col_nic,
        t.col_mac,
        t.col_link_status,
        t.col_speed,
        t.col_duplex,
        t.col_vendor,
        t.col_device_name,
        t.col_pci,
        t.col_fcoe,
        t.col_sriov_capable,
    ]);
    let Some(host) = m.hosts.get(host) else {
        return table;
    };
    let mut pifs: Vec<&PifRecord> = host
        .pifs
        .iter()
        .filter_map(|p| m.get(p))
        .map(|p| &**p)
        .filter(|p| objects::pif_is_physical(p))
        .collect();
    // A cache has no server order; NIC names sort the same way.
    pifs.sort_by(|a, b| objects::natural_cmp(&objects::pif_name(m, a), &objects::pif_name(m, b)));
    for pif in pifs {
        let metrics = m.get(&pif.metrics);
        let carrier = metrics.is_some_and(|x| x.carrier);
        let metric = |f: fn(&xapi::PifMetricsRecord) -> String| {
            metrics.map(|x| f(x)).unwrap_or_else(|| t.hyphen.to_owned())
        };
        table.push(vec![
            objects::pif_name(m, pif),
            pif.mac.clone(),
            if carrier {
                t.link_connected
            } else {
                t.link_disconnected
            }
            .to_owned(),
            match metrics {
                Some(x) if carrier => fill(t.nic_speed, &[("speed", &x.speed.to_string())]),
                _ => t.hyphen.to_owned(),
            },
            match metrics {
                Some(x) if carrier => if x.duplex {
                    t.duplex_full
                } else {
                    t.duplex_half
                }
                .to_owned(),
                _ => t.hyphen.to_owned(),
            },
            metric(|x| x.vendor_name.clone()),
            metric(|x| x.device_name.clone()),
            metric(|x| x.pci_bus_path.clone()),
            yes_no(pif.capabilities.iter().any(|c| c == "fcoe")),
            sriov_text(ctx, pif),
        ]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn vm_interfaces_show_guest_ips() {
        let m = fixture();
        let t = tab_strings();
        let web = &m.vms[&Ref::new("OpaqueRef:vm-web")];
        let table = vm_interfaces(ctx(&m), web);
        assert_eq!(table.column(t.col_device), ["0", "1"]);
        assert_eq!(table.column(t.col_network), ["Network 0", "Guests VLAN 20"]);
        assert_eq!(
            table.column(t.col_ip),
            ["192.0.2.101, 2001:db8::101", "10.20.0.5"]
        );
        assert_eq!(table.column(t.col_limit), ["10240", ""]);
        let db = &m.vms[&Ref::new("OpaqueRef:vm-db")];
        let table = vm_interfaces(ctx(&m), db);
        assert_eq!(table.column(t.col_ip), [t.unknown]);
        assert_eq!(table.column(t.col_active), ["No"]);
    }

    #[test]
    fn host_and_pool_networks() {
        let m = fixture();
        let t = tab_strings();
        let host = Ref::new("OpaqueRef:host-0002");
        let table = networks(ctx(&m), Some(&host));
        assert_eq!(
            table.column(t.col_name),
            ["Guests VLAN 20", "Network 0", "Storage network"]
        );
        // The VLAN network has no PIF on host 2.
        assert_eq!(table.column(t.col_nic), ["", "NIC 0", "NIC 1"]);
        assert_eq!(
            table.column(t.col_link_status),
            [t.none, t.link_connected, t.link_disconnected]
        );
        assert_eq!(table.column(t.col_auto), ["Yes", "Yes", "No"]);
        assert_eq!(table.column(t.col_mtu), ["1500", "1500", "9000"]);
        let pool = networks(ctx(&m), None);
        assert_eq!(
            pool.column(t.col_name),
            ["Network 0", "Guests VLAN 20", "Storage network"]
        );
        assert_eq!(
            pool.column(t.col_link_status),
            [t.link_connected, t.link_connected, t.link_partial]
        );
        assert_eq!(pool.column(t.col_vlan), [" - ", "20", " - "]);
        // The host-internal network appears with hidden objects on.
        let hidden = Ctx {
            show_hidden: true,
            ..ctx(&m)
        };
        assert!(
            networks(hidden, None)
                .column(t.col_name)
                .contains(&"Host internal management network (Hidden)")
        );

        let ip = ip_configuration(ctx(&m), None);
        assert_eq!(
            ip.column(t.col_server),
            ["xcp-host-01", "xcp-host-01", "xcp-host-02"]
        );
        assert_eq!(
            ip.column(t.col_interface),
            ["Management", "Storage", "Management"]
        );
        assert_eq!(ip.column(t.col_ip_setup), ["Static", "Static", "DHCP"]);
        assert_eq!(ip.column(t.col_ip), ["192.0.2.11", "198.51.100.11", ""]);
    }

    #[test]
    fn host_nics() {
        let m = fixture();
        let t = tab_strings();
        let table = nics(ctx(&m), &Ref::new("OpaqueRef:host-0002"));
        assert_eq!(table.column(t.col_nic), ["NIC 0", "NIC 1"]);
        assert_eq!(
            table.column(t.col_link_status),
            [t.link_connected, t.link_disconnected]
        );
        assert_eq!(table.column(t.col_speed), ["1000 Mbit/s", "-"]);
        assert_eq!(table.column(t.col_duplex), ["Full", "-"]);
        assert_eq!(table.column(t.col_pci), ["0000:01:00.0", "0000:01:00.1"]);
        // VLAN interfaces are not NICs.
        let table = nics(ctx(&m), &Ref::new("OpaqueRef:host-0001"));
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.column(t.col_fcoe), ["Yes", "No"]);
    }

    #[test]
    fn real_capture_network_tables_build() {
        let m = capture();
        let c = ctx(&m);
        let host = m.hosts.keys().next().unwrap().clone();
        assert_eq!(nics(c, &host).rows.len(), 6);
        assert!(!networks(c, Some(&host)).rows.is_empty());
        assert!(!ip_configuration(c, None).rows.is_empty());
        for vm in m.vms.values() {
            vm_interfaces(c, vm);
        }
    }
}
