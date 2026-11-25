//! Networking in the mock: networks, VLANs, bonds, cross-server tunnels,
//! the IP configuration and management interface of PIFs, NIC rescans and
//! the hot-plug of VM interfaces, with XAPI's checks and errors.
//!
//! **Calls.**
//!
//! * `network.create` / `destroy` (`NETWORK_CONTAINS_PIF`,
//!   `NETWORK_CONTAINS_VIF`);
//! * `pool.create_VLAN_from_PIF`: a VLAN on the NIC with the same device on
//!   every host (`VLAN_TAG_INVALID`, `PIF_IS_VLAN`, `PIF_VLAN_EXISTS`,
//!   `NETWORK_ALREADY_CONNECTED`); `VLAN.destroy`
//!   (`PIF_IS_MANAGEMENT_INTERFACE`);
//! * `Bond.create`: 2 to 4 unbonded NICs of one host
//!   (`PIF_BOND_NEEDS_MORE_MEMBERS`, `PIF_ALREADY_BONDED`,
//!   `PIF_BOND_MORE_THAN_ONE_IP`); the members' IP configuration, the
//!   management flag and their VLANs move to the bond interface, as XAPI
//!   does. `Bond.destroy` moves them back to the primary member;
//!   `set_mode`, `set_property` (`hashing_algorithm`);
//! * `tunnel.create` on a transport PIF with an IP configuration
//!   (`TRANSPORT_PIF_NOT_CONFIGURED`), `tunnel.destroy`;
//! * `PIF.reconfigure_ip` (`INVALID_IP_ADDRESS_SPECIFIED`, and the
//!   management interface cannot lose its address), `plug`, `unplug`
//!   (`PIF_IS_MANAGEMENT_INTERFACE`, `PIF_DOES_NOT_ALLOW_UNPLUG`),
//!   `set_disallow_unplug`, `forget`, `scan` (NICs added with
//!   [`crate::MockServer::add_spare_nic`] appear);
//! * `host.management_reconfigure` and `pool.management_reconfigure`
//!   (`PIF_HAS_NO_NETWORK_CONFIGURATION`);
//! * `VIF.plug` / `unplug` on running VMs (`VM_BAD_POWER_STATE`,
//!   `DEVICE_ALREADY_ATTACHED`, `DEVICE_ALREADY_DETACHED`). A VIF's
//!   `allowed_operations` follow its VM and attachment (hot-plug only
//!   with the guest's PV drivers), and its `currently_attached` the VM's
//!   power state.
//!
//! [`crate::MockServer::fail_on_host`] makes `Bond.create`,
//! `tunnel.create` or `PIF.reconfigure_ip` fail on one host only (to prove
//! the rollback of half-made bonds and networks).
//!
//! [`bond_lab_records`] is the fixture with two more NICs per host (eth2
//! and eth3, on their own networks) on the vSwitch backend, for bonds.

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// Calls handled here.
const CALLS: &[(&str, &[&str])] = &[
    ("network", &["create", "destroy"]),
    ("pool", &["create_VLAN_from_PIF", "management_reconfigure"]),
    ("VLAN", &["destroy"]),
    ("Bond", &["create", "destroy", "set_mode", "set_property"]),
    ("tunnel", &["create", "destroy"]),
    (
        "PIF",
        &[
            "reconfigure_ip",
            "plug",
            "unplug",
            "scan",
            "forget",
            "set_disallow_unplug",
        ],
    ),
    ("host", &["management_reconfigure"]),
    ("VIF", &["plug", "unplug"]),
];

/// Whether `class.call` is a networking call of the mock.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    CALLS
        .iter()
        .any(|(c, calls)| *c == class && calls.contains(&call))
}

/// The networking state that is not in the database.
#[derive(Debug, Default)]
pub(crate) struct World {
    /// `(method, host)`: the call fails on that host.
    pub(crate) host_failures: Vec<(String, String)>,
    /// NICs `PIF.scan` finds: `(host, device, MAC)`.
    pub(crate) spare_nics: Vec<(String, String, String)>,
}

fn list(rec: &Map<String, Value>, field: &str) -> Vec<String> {
    rec.get(field)
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn text(rec: &Map<String, Value>, field: &str) -> String {
    rec.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn int(rec: &Map<String, Value>, field: &str) -> i64 {
    match rec.get(field) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn flag(rec: &Map<String, Value>, field: &str) -> bool {
    rec.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn is_ipv4(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

/// The IP configuration fields of a PIF.
const IP_FIELDS: &[&str] = &["ip_configuration_mode", "IP", "netmask", "gateway", "DNS"];

impl Db {
    fn host_fails(&self, method: &str, host: &str) -> bool {
        self.networking
            .host_failures
            .iter()
            .any(|(m, h)| m == method && h == host)
    }

    fn pif(&self, r: &str) -> Option<Map<String, Value>> {
        self.record("pif", r).cloned()
    }

    fn pifs_of_host(&self, host: &str) -> Vec<(String, Map<String, Value>)> {
        self.objects
            .iter()
            .filter(|((c, _), rec)| {
                c == "pif" && rec.get("host").and_then(Value::as_str) == Some(host)
            })
            .filter_map(|((_, r), rec)| rec.as_object().map(|o| (r.clone(), o.clone())))
            .collect()
    }

    fn hosts(&self) -> Vec<String> {
        self.objects
            .keys()
            .filter(|(c, _)| c == "host")
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Runs a networking call; `None` if it is not one.
    pub(crate) fn networking_call(
        &mut self,
        class: &str,
        call: &str,
        params: &[Value],
    ) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i);
        Some(match (class, call) {
            ("network", "create") => self.network_create(params.get(1)),
            ("network", "destroy") => self.network_destroy(p(1)),
            ("pool", "create_VLAN_from_PIF") => {
                let tag = params.get(3).and_then(|v| match v {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                });
                self.vlan_create(p(1), p(2), tag.unwrap_or(-1))
            }
            ("pool", "management_reconfigure") => self.pool_management_reconfigure(p(1)),
            ("VLAN", "destroy") => self.vlan_destroy(p(1)),
            ("Bond", "create") => self.bond_create(params),
            ("Bond", "destroy") => self.bond_destroy(p(1)),
            ("Bond", "set_mode") => self.bond_set(p(1), "mode", p(2)),
            ("Bond", "set_property") => {
                if p(2) != "hashing_algorithm" || !matches!(p(3), "src_mac" | "tcpudp_ports") {
                    api_error("INVALID_VALUE", &[p(2), p(3)])
                } else {
                    let (b, value) = (p(1).to_owned(), p(3).to_owned());
                    if !self.exists("bond", &b) {
                        return Some(api_error("HANDLE_INVALID", &["Bond", &b]));
                    }
                    self.modify("bond", &b, |rec| {
                        let props = rec.entry("properties").or_insert_with(|| json!({}));
                        if let Value::Object(m) = props {
                            m.insert("hashing_algorithm".into(), json!(value));
                        }
                    });
                    Ok(json!(""))
                }
            }
            ("tunnel", "create") => self.tunnel_create(p(1), p(2)),
            ("tunnel", "destroy") => self.tunnel_destroy(p(1)),
            ("PIF", "reconfigure_ip") => self.reconfigure_ip(params),
            ("PIF", "plug") => self.pif_plug(p(1), true),
            ("PIF", "unplug") => self.pif_plug(p(1), false),
            ("PIF", "scan") => self.pif_scan(p(1)),
            ("PIF", "forget") => self.pif_forget(p(1)),
            ("PIF", "set_disallow_unplug") => {
                let r = p(1).to_owned();
                if !self.exists("pif", &r) {
                    return Some(api_error("HANDLE_INVALID", &["PIF", &r]));
                }
                let value = params.get(2).and_then(Value::as_bool).unwrap_or(false);
                self.modify("pif", &r, |rec| {
                    rec.insert("disallow_unplug".into(), json!(value));
                });
                Ok(json!(""))
            }
            ("host", "management_reconfigure") => self.host_management_reconfigure(p(1)),
            ("VIF", "plug") => self.vif_plug(p(1), true),
            ("VIF", "unplug") => self.vif_plug(p(1), false),
            _ => unreachable!("handled call"),
        })
    }

    fn network_create(&mut self, record: Option<&Value>) -> Reply {
        let rec = record
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(mode) = rec.get("default_locking_mode").and_then(Value::as_str)
            && !matches!(mode, "unlocked" | "disabled")
        {
            return api_error("INVALID_VALUE", &["default_locking_mode", mode]);
        }
        let (r, uuid) = self.new_ref("network");
        let n = self.next_object;
        let mtu = match int(&rec, "MTU") {
            0 => 1500,
            m => m,
        };
        if !(68..=9216).contains(&mtu) {
            return api_error("MTU_RANGE_INVALID", &[&mtu.to_string(), "68", "9216"]);
        }
        self.insert(
            "network",
            &r,
            json!({
                "uuid": uuid, "name_label": text(&rec, "name_label"),
                "name_description": text(&rec, "name_description"),
                "MTU": mtu, "bridge": format!("xapi{n}"), "managed": rec.get("managed").cloned().unwrap_or(json!(true)),
                "other_config": rec.get("other_config").cloned().unwrap_or(json!({})),
                "PIFs": [], "VIFs": [], "tags": rec.get("tags").cloned().unwrap_or(json!([])),
                "allowed_operations": [], "current_operations": {}, "blobs": {},
                "default_locking_mode": "unlocked", "assigned_ips": {}, "purpose": []
            }),
        );
        Ok(json!(r))
    }

    fn network_destroy(&mut self, net: &str) -> Reply {
        let Some(rec) = self.record("network", net).cloned() else {
            return api_error("HANDLE_INVALID", &["network", net]);
        };
        let pifs = list(&rec, "PIFs");
        if !pifs.is_empty() {
            let refs: Vec<&str> = pifs.iter().map(String::as_str).collect();
            return api_error("NETWORK_CONTAINS_PIF", &refs);
        }
        let vifs = list(&rec, "VIFs");
        if !vifs.is_empty() {
            let refs: Vec<&str> = vifs.iter().map(String::as_str).collect();
            return api_error("NETWORK_CONTAINS_VIF", &refs);
        }
        self.remove("network", net);
        Ok(json!(""))
    }

    /// A new PIF record on `host` and `network`, listed by both.
    fn add_pif(&mut self, host: &str, network: &str, fields: Value) -> String {
        let (r, uuid) = self.new_ref("pif");
        let mut rec = json!({
            "uuid": uuid, "host": host, "network": network, "MAC": "", "MTU": 1500,
            "VLAN": -1, "metrics": NULL_REF, "physical": false, "currently_attached": true,
            "management": false, "ip_configuration_mode": "None", "IP": "", "netmask": "",
            "gateway": "", "DNS": "", "managed": true, "capabilities": [],
            "bond_slave_of": NULL_REF, "bond_master_of": [], "VLAN_master_of": NULL_REF,
            "VLAN_slave_of": [], "tunnel_access_PIF_of": [], "tunnel_transport_PIF_of": [],
            "sriov_logical_PIF_of": [], "sriov_physical_PIF_of": [], "other_config": {},
            "disallow_unplug": false, "properties": {}, "ipv6_configuration_mode": "None",
            "IPv6": [], "ipv6_gateway": "", "primary_address_type": "IPv4",
            "igmp_snooping_status": "unknown", "PCI": NULL_REF
        });
        if let (Value::Object(base), Value::Object(extra)) = (&mut rec, fields) {
            base.extend(extra);
        }
        self.insert("pif", &r, rec);
        self.push_ref("host", host, "PIFs", &r);
        self.push_ref("network", network, "PIFs", &r);
        r
    }

    fn drop_pif(&mut self, pif: &str) {
        let Some(rec) = self.pif(pif) else { return };
        self.drop_ref("host", &text(&rec, "host"), "PIFs", pif);
        self.drop_ref("network", &text(&rec, "network"), "PIFs", pif);
        self.remove("pif", pif);
    }

    /// The PIF of `network` on `host`.
    fn network_pif_on(&self, network: &str, host: &str) -> Option<String> {
        let rec = self.record("network", network)?;
        list(rec, "PIFs")
            .into_iter()
            .find(|p| self.get_str("pif", p, "host").as_deref() == Some(host))
    }

    fn vlan_create(&mut self, pif: &str, network: &str, tag: i64) -> Reply {
        let Some(base) = self.pif(pif) else {
            return api_error("HANDLE_INVALID", &["PIF", pif]);
        };
        let Some(net) = self.record("network", network).cloned() else {
            return api_error("HANDLE_INVALID", &["network", network]);
        };
        if !(0..=4094).contains(&tag) {
            return api_error("VLAN_TAG_INVALID", &[&tag.to_string()]);
        }
        if int(&base, "VLAN") != -1 || !list(&base, "tunnel_access_PIF_of").is_empty() {
            return api_error("PIF_IS_VLAN", &[pif]);
        }
        if !list(&net, "PIFs").is_empty() {
            return api_error("NETWORK_ALREADY_CONNECTED", &[network, pif]);
        }
        let device = text(&base, "device");
        // The tagged NIC on every host, checked before anything is made.
        let mut tagged = Vec::new();
        for host in self.hosts() {
            let pifs = self.pifs_of_host(&host);
            let Some((t, trec)) = pifs
                .iter()
                .find(|(_, p)| {
                    text(p, "device") == device
                        && int(p, "VLAN") == -1
                        && list(p, "tunnel_access_PIF_of").is_empty()
                        && text(p, "bond_slave_of") == NULL_REF
                })
                .cloned()
            else {
                continue;
            };
            if pifs
                .iter()
                .any(|(_, p)| text(p, "device") == device && int(p, "VLAN") == tag)
            {
                return api_error("PIF_VLAN_EXISTS", &[&t]);
            }
            tagged.push((host, t, trec));
        }
        let mtu = int(&net, "MTU");
        let mut made = Vec::new();
        for (host, t, trec) in tagged {
            let (vlan, uuid) = self.new_ref("vlan");
            let untagged = self.add_pif(
                &host,
                network,
                json!({
                    "device": device, "MAC": text(&trec, "MAC"), "MTU": mtu, "VLAN": tag,
                    "metrics": trec.get("metrics").cloned().unwrap_or(json!(NULL_REF)),
                    "currently_attached": flag(&trec, "currently_attached"),
                    "VLAN_master_of": vlan,
                }),
            );
            self.insert(
                "vlan",
                &vlan,
                json!({"uuid": uuid, "tagged_PIF": t, "untagged_PIF": untagged, "tag": tag, "other_config": {}}),
            );
            self.push_ref("pif", &t, "VLAN_slave_of", &vlan);
            made.push(json!(untagged));
        }
        Ok(Value::Array(made))
    }

    fn vlan_destroy(&mut self, vlan: &str) -> Reply {
        let Some(rec) = self.record("vlan", vlan).cloned() else {
            return api_error("HANDLE_INVALID", &["VLAN", vlan]);
        };
        let untagged = text(&rec, "untagged_PIF");
        if self.pif(&untagged).is_some_and(|p| flag(&p, "management")) {
            return api_error("PIF_IS_MANAGEMENT_INTERFACE", &[&untagged]);
        }
        self.drop_ref("pif", &text(&rec, "tagged_PIF"), "VLAN_slave_of", vlan);
        self.drop_pif(&untagged);
        self.remove("vlan", vlan);
        Ok(json!(""))
    }

    fn bond_create(&mut self, params: &[Value]) -> Reply {
        let network = str_param(params, 1).to_owned();
        let members: Vec<String> = params
            .get(2)
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mode = params
            .get(4)
            .and_then(Value::as_str)
            .unwrap_or("balance-slb")
            .to_owned();
        let properties = params.get(5).cloned().unwrap_or(json!({}));
        let Some(net) = self.record("network", &network).cloned() else {
            return api_error("HANDLE_INVALID", &["network", &network]);
        };
        if !matches!(mode.as_str(), "balance-slb" | "active-backup" | "lacp") {
            return api_error("INVALID_VALUE", &["mode", &mode]);
        }
        if members.len() < 2 {
            return api_error("PIF_BOND_NEEDS_MORE_MEMBERS", &[]);
        }
        if members.len() > 4 {
            return api_error("PIF_BOND_MORE_THAN_4_MEMBERS", &[]);
        }
        let mut recs = Vec::new();
        for m in &members {
            let Some(rec) = self.pif(m) else {
                return api_error("HANDLE_INVALID", &["PIF", m]);
            };
            if text(&rec, "bond_slave_of") != NULL_REF {
                return api_error("PIF_ALREADY_BONDED", &[m]);
            }
            if !flag(&rec, "physical") || int(&rec, "VLAN") != -1 {
                return api_error("PIF_IS_VLAN", &[m]);
            }
            recs.push(rec);
        }
        let host = text(&recs[0], "host");
        if recs.iter().any(|r| text(r, "host") != host) {
            return api_error("PIF_CANNOT_BOND_CROSS_HOST", &[]);
        }
        if self.host_fails("Bond.create", &host) {
            return api_error(
                "PIF_CONFIGURATION_ERROR",
                &[&members[0], "bond device busy"],
            );
        }
        if self.network_pif_on(&network, &host).is_some() {
            return api_error("NETWORK_ALREADY_CONNECTED", &[&network, &members[0]]);
        }
        let configured: Vec<usize> = recs
            .iter()
            .enumerate()
            .filter(|(_, r)| text(r, "ip_configuration_mode") != "None" || flag(r, "management"))
            .map(|(i, _)| i)
            .collect();
        if configured.len() > 1 {
            return api_error("PIF_BOND_MORE_THAN_ONE_IP", &[]);
        }
        let primary = configured.first().copied().unwrap_or(0);
        let source = recs[primary].clone();
        let n = self
            .pifs_of_host(&host)
            .iter()
            .filter(|(_, p)| text(p, "device").starts_with("bond"))
            .count();
        let (bond, uuid) = self.new_ref("bond");
        let mut fields = json!({
            "device": format!("bond{n}"), "MAC": text(&recs[0], "MAC"), "MTU": int(&net, "MTU"),
            "bond_master_of": [bond], "management": flag(&source, "management"),
            "disallow_unplug": flag(&source, "disallow_unplug"),
            "other_config": source.get("other_config").cloned().unwrap_or(json!({})),
        });
        for f in IP_FIELDS {
            fields[*f] = source.get(*f).cloned().unwrap_or(json!(""));
        }
        let master = self.add_pif(&host, &network, fields);
        self.insert(
            "bond",
            &bond,
            json!({
                "uuid": uuid, "master": master, "slaves": members, "other_config": {},
                "primary_slave": members[primary], "mode": mode, "properties": properties,
                "links_up": members.len(), "auto_update_mac": true
            }),
        );
        for (m, rec) in members.iter().zip(&recs) {
            let vlans = list(rec, "VLAN_slave_of");
            self.modify("pif", m, |p| {
                p.insert("bond_slave_of".into(), json!(bond));
                p.insert("currently_attached".into(), json!(false));
                p.insert("management".into(), json!(false));
                p.insert("disallow_unplug".into(), json!(false));
                p.insert("ip_configuration_mode".into(), json!("None"));
                for f in ["IP", "netmask", "gateway", "DNS"] {
                    p.insert(f.into(), json!(""));
                }
                p.insert("VLAN_slave_of".into(), json!([]));
                if let Some(Value::Object(oc)) = p.get_mut("other_config") {
                    oc.remove("management_purpose");
                }
            });
            for v in vlans {
                self.modify("vlan", &v, |r| {
                    r.insert("tagged_PIF".into(), json!(master));
                });
                self.push_ref("pif", &master, "VLAN_slave_of", &v);
            }
        }
        Ok(json!(bond))
    }

    fn bond_destroy(&mut self, bond: &str) -> Reply {
        let Some(rec) = self.record("bond", bond).cloned() else {
            return api_error("HANDLE_INVALID", &["Bond", bond]);
        };
        let master = text(&rec, "master");
        let members = list(&rec, "slaves");
        let primary = Some(text(&rec, "primary_slave"))
            .filter(|p| members.contains(p))
            .or_else(|| members.first().cloned())
            .unwrap_or_default();
        let Some(mrec) = self.pif(&master) else {
            return api_error("HANDLE_INVALID", &["PIF", &master]);
        };
        for m in &members {
            let back = m == &primary;
            let mrec = mrec.clone();
            self.modify("pif", m, |p| {
                p.insert("bond_slave_of".into(), json!(NULL_REF));
                p.insert("currently_attached".into(), json!(true));
                if back {
                    for f in IP_FIELDS {
                        p.insert((*f).into(), mrec.get(*f).cloned().unwrap_or(json!("")));
                    }
                    p.insert("management".into(), json!(flag(&mrec, "management")));
                    p.insert(
                        "disallow_unplug".into(),
                        json!(flag(&mrec, "disallow_unplug")),
                    );
                    if let Some(purpose) = mrec
                        .get("other_config")
                        .and_then(|o| o.get("management_purpose"))
                        .cloned()
                    {
                        let oc = p.entry("other_config").or_insert_with(|| json!({}));
                        if let Value::Object(oc) = oc {
                            oc.insert("management_purpose".into(), purpose);
                        }
                    }
                }
            });
        }
        for v in list(&mrec, "VLAN_slave_of") {
            self.modify("vlan", &v, |r| {
                r.insert("tagged_PIF".into(), json!(primary));
            });
            self.push_ref("pif", &primary, "VLAN_slave_of", &v);
        }
        self.drop_pif(&master);
        self.remove("bond", bond);
        Ok(json!(""))
    }

    fn bond_set(&mut self, bond: &str, field: &str, value: &str) -> Reply {
        if !self.exists("bond", bond) {
            return api_error("HANDLE_INVALID", &["Bond", bond]);
        }
        if !matches!(value, "balance-slb" | "active-backup" | "lacp") {
            return api_error("INVALID_VALUE", &[field, value]);
        }
        let value = value.to_owned();
        self.modify("bond", bond, |rec| {
            rec.insert(field.into(), json!(value));
        });
        Ok(json!(""))
    }

    fn tunnel_create(&mut self, transport: &str, network: &str) -> Reply {
        let Some(trec) = self.pif(transport) else {
            return api_error("HANDLE_INVALID", &["PIF", transport]);
        };
        if !self.exists("network", network) {
            return api_error("HANDLE_INVALID", &["network", network]);
        }
        let host = text(&trec, "host");
        if self.host_fails("tunnel.create", &host) {
            return api_error("OPENVSWITCH_NOT_ACTIVE", &[]);
        }
        if text(&trec, "ip_configuration_mode") == "None" {
            return api_error("TRANSPORT_PIF_NOT_CONFIGURED", &[transport]);
        }
        if self.network_pif_on(network, &host).is_some() {
            return api_error("NETWORK_ALREADY_CONNECTED", &[network, transport]);
        }
        let (tunnel, uuid) = self.new_ref("tunnel");
        let access = self.add_pif(
            &host,
            network,
            json!({
                "device": text(&trec, "device"), "MAC": text(&trec, "MAC"),
                "MTU": int(&trec, "MTU"), "tunnel_access_PIF_of": [tunnel],
            }),
        );
        self.insert(
            "tunnel",
            &tunnel,
            json!({"uuid": uuid, "access_PIF": access, "transport_PIF": transport,
                   "status": {"active": "true"}, "other_config": {}, "protocol": "gre"}),
        );
        self.push_ref("pif", transport, "tunnel_transport_PIF_of", &tunnel);
        Ok(json!(tunnel))
    }

    fn tunnel_destroy(&mut self, tunnel: &str) -> Reply {
        let Some(rec) = self.record("tunnel", tunnel).cloned() else {
            return api_error("HANDLE_INVALID", &["tunnel", tunnel]);
        };
        self.drop_ref(
            "pif",
            &text(&rec, "transport_PIF"),
            "tunnel_transport_PIF_of",
            tunnel,
        );
        self.drop_pif(&text(&rec, "access_PIF"));
        self.remove("tunnel", tunnel);
        Ok(json!(""))
    }

    fn reconfigure_ip(&mut self, params: &[Value]) -> Reply {
        let pif = str_param(params, 1).to_owned();
        let mode = str_param(params, 2).to_owned();
        let (ip, netmask, gateway, dns) = (
            str_param(params, 3).to_owned(),
            str_param(params, 4).to_owned(),
            str_param(params, 5).to_owned(),
            str_param(params, 6).to_owned(),
        );
        let Some(rec) = self.pif(&pif) else {
            return api_error("HANDLE_INVALID", &["PIF", &pif]);
        };
        if self.host_fails("PIF.reconfigure_ip", &text(&rec, "host")) {
            return api_error("PIF_CONFIGURATION_ERROR", &[&pif, "dhclient failed"]);
        }
        match mode.as_str() {
            "None" | "DHCP" => {}
            "Static" => {
                if !is_ipv4(&ip) {
                    return api_error("INVALID_IP_ADDRESS_SPECIFIED", &["IP"]);
                }
                if !is_ipv4(&netmask) {
                    return api_error("INVALID_IP_ADDRESS_SPECIFIED", &["netmask"]);
                }
                if !gateway.is_empty() && !is_ipv4(&gateway) {
                    return api_error("INVALID_IP_ADDRESS_SPECIFIED", &["gateway"]);
                }
            }
            _ => return api_error("INVALID_VALUE", &["mode", &mode]),
        }
        if mode == "None" && flag(&rec, "management") {
            return api_error("PIF_IS_MANAGEMENT_INTERFACE", &[&pif]);
        }
        if text(&rec, "bond_slave_of") != NULL_REF {
            return api_error("PIF_IS_BOND_SLAVE", &[&pif]);
        }
        let static_ip = mode == "Static";
        self.modify("pif", &pif, |p| {
            p.insert("ip_configuration_mode".into(), json!(mode));
            p.insert(
                "IP".into(),
                json!(if static_ip { ip } else { String::new() }),
            );
            p.insert(
                "netmask".into(),
                json!(if static_ip { netmask } else { String::new() }),
            );
            p.insert(
                "gateway".into(),
                json!(if static_ip { gateway } else { String::new() }),
            );
            p.insert(
                "DNS".into(),
                json!(if static_ip { dns } else { String::new() }),
            );
        });
        Ok(json!(""))
    }

    fn pif_plug(&mut self, pif: &str, plug: bool) -> Reply {
        let Some(rec) = self.pif(pif) else {
            return api_error("HANDLE_INVALID", &["PIF", pif]);
        };
        if plug && text(&rec, "bond_slave_of") != NULL_REF {
            return api_error("CANNOT_PLUG_BOND_SLAVE", &[pif]);
        }
        if !plug {
            if flag(&rec, "management") {
                return api_error("PIF_IS_MANAGEMENT_INTERFACE", &[pif]);
            }
            if flag(&rec, "disallow_unplug") {
                return api_error("PIF_DOES_NOT_ALLOW_UNPLUG", &[pif]);
            }
        }
        self.modify("pif", pif, |p| {
            p.insert("currently_attached".into(), json!(plug));
        });
        Ok(json!(""))
    }

    fn pif_forget(&mut self, pif: &str) -> Reply {
        let Some(rec) = self.pif(pif) else {
            return api_error("HANDLE_INVALID", &["PIF", pif]);
        };
        if flag(&rec, "management") {
            return api_error("PIF_IS_MANAGEMENT_INTERFACE", &[pif]);
        }
        self.drop_pif(pif);
        Ok(json!(""))
    }

    fn pif_scan(&mut self, host: &str) -> Reply {
        if !self.exists("host", host) {
            return api_error("HANDLE_INVALID", &["host", host]);
        }
        let found: Vec<(String, String)> = self
            .networking
            .spare_nics
            .iter()
            .filter(|(h, _, _)| h == host)
            .map(|(_, d, m)| (d.clone(), m.clone()))
            .collect();
        self.networking.spare_nics.retain(|(h, _, _)| h != host);
        for (device, mac) in found {
            // The pool-wide network of that device, or a new one.
            let label = format!("Pool-wide network associated with {device}");
            let net = self
                .objects
                .iter()
                .find(|((c, _), r)| c == "network" && r.get("name_label") == Some(&json!(label)))
                .map(|((_, r), _)| r.clone());
            let net = match net {
                Some(n) => n,
                None => {
                    let record = json!({"name_label": label, "other_config": {}, "MTU": 1500});
                    match self.network_create(Some(&record)) {
                        Ok(Value::String(r)) => r,
                        _ => continue,
                    }
                }
            };
            self.add_pif(
                host,
                &net,
                json!({"device": device, "MAC": mac, "physical": true, "currently_attached": false}),
            );
        }
        Ok(json!(""))
    }

    /// Moves the management flag of `host` to `pif`.
    fn set_management(&mut self, host: &str, pif: &str) {
        for (p, rec) in self.pifs_of_host(host) {
            let now = p == pif;
            if flag(&rec, "management") != now {
                self.modify("pif", &p, |r| {
                    r.insert("management".into(), json!(now));
                });
            }
        }
    }

    fn host_management_reconfigure(&mut self, pif: &str) -> Reply {
        let Some(rec) = self.pif(pif) else {
            return api_error("HANDLE_INVALID", &["PIF", pif]);
        };
        if text(&rec, "ip_configuration_mode") == "None" {
            return api_error("PIF_HAS_NO_NETWORK_CONFIGURATION", &[pif]);
        }
        let host = text(&rec, "host");
        self.set_management(&host, pif);
        Ok(json!(""))
    }

    fn pool_management_reconfigure(&mut self, network: &str) -> Reply {
        if !self.exists("network", network) {
            return api_error("HANDLE_INVALID", &["network", network]);
        }
        let mut moves = Vec::new();
        for host in self.hosts() {
            let Some(pif) = self.network_pif_on(network, &host) else {
                return api_error("PIF_NOT_PRESENT", &[&host, network]);
            };
            if self
                .get_str("pif", &pif, "ip_configuration_mode")
                .as_deref()
                == Some("None")
            {
                return api_error("PIF_HAS_NO_NETWORK_CONFIGURATION", &[&pif]);
            }
            moves.push((host, pif));
        }
        for (host, pif) in moves {
            self.set_management(&host, &pif);
        }
        Ok(json!(""))
    }

    /// A VIF's `allowed_operations` from its VM's power state.
    pub(crate) fn refresh_vif_ops(&mut self, vif: &str) {
        let Some(rec) = self.record("vif", vif) else {
            return;
        };
        let vm = text(rec, "VM");
        let attached = flag(rec, "currently_attached");
        // Hot-plug needs the guest's PV drivers.
        let drivers = self
            .record("vm", &vm)
            .and_then(|r| r.get("guest_metrics"))
            .and_then(Value::as_str)
            .and_then(|g| self.record("vm_guest_metrics", g))
            .is_some_and(|g| flag(g, "PV_drivers_detected"));
        let ops: Vec<&str> = match self.vm_power(&vm).as_str() {
            "Running" if drivers => vec![if attached { "unplug" } else { "plug" }],
            "Running" => vec![],
            "Halted" => vec!["attach"],
            _ => vec![],
        };
        self.modify("vif", vif, |r| {
            r.insert("allowed_operations".into(), json!(ops));
        });
    }

    /// Like XAPI: a VM's interfaces are plugged while it runs.
    pub(crate) fn sync_vifs(&mut self, vm: &str) {
        let running = matches!(self.vm_power(vm).as_str(), "Running" | "Paused");
        let vifs = self
            .record("vm", vm)
            .map(|r| list(r, "VIFs"))
            .unwrap_or_default();
        for vif in vifs {
            let attached = self
                .record("vif", &vif)
                .is_some_and(|r| flag(r, "currently_attached"));
            if attached != running {
                self.modify("vif", &vif, |r| {
                    r.insert("currently_attached".into(), json!(running));
                });
            }
            self.refresh_vif_ops(&vif);
        }
    }

    /// Gives every VIF its `allowed_operations` (fixtures have none).
    pub(crate) fn init_vif_ops(&mut self) {
        let vifs: Vec<String> = self
            .objects
            .keys()
            .filter(|(c, _)| c == "vif")
            .map(|(_, r)| r.clone())
            .collect();
        for vif in vifs {
            self.refresh_vif_ops(&vif);
        }
    }

    fn vif_plug(&mut self, vif: &str, plug: bool) -> Reply {
        let Some(rec) = self.record("vif", vif).cloned() else {
            return api_error("HANDLE_INVALID", &["VIF", vif]);
        };
        let vm = text(&rec, "VM");
        let power = self.vm_power(&vm);
        if power != "Running" {
            return api_error(
                "VM_BAD_POWER_STATE",
                &[&vm, "running", &power.to_lowercase()],
            );
        }
        let attached = flag(&rec, "currently_attached");
        if plug && attached {
            return api_error("DEVICE_ALREADY_ATTACHED", &[vif]);
        }
        if !plug && !attached {
            return api_error("DEVICE_ALREADY_DETACHED", &[vif]);
        }
        self.modify("vif", vif, |r| {
            r.insert("currently_attached".into(), json!(plug));
        });
        self.refresh_vif_ops(vif);
        Ok(json!(""))
    }
}

/// The fixture with two more NICs (eth2, eth3) per host on their own
/// pool-wide networks, on the vSwitch backend: room for bonds.
pub fn bond_lab_records() -> Value {
    let mut db = crate::fixture_records();
    let hosts = [("OpaqueRef:host-0001", "01"), ("OpaqueRef:host-0002", "02")];
    for (i, device) in ["eth2", "eth3"].iter().enumerate() {
        let net = format!("OpaqueRef:net-{device}");
        let mut pifs = Vec::new();
        for (host, h) in hosts {
            let pif = format!("OpaqueRef:pif-{h}-{device}");
            let metrics = format!("OpaqueRef:pifm-{h}-{device}");
            db["pif"][&pif] = json!({
                "uuid": format!("be000000-0000-4000-8000-0000000{h}{i}2{i}"), "device": device,
                "host": host, "network": net, "MAC": format!("02:00:00:00:{h}:0{}", i + 2),
                "MTU": 1500, "VLAN": -1, "metrics": metrics, "physical": true,
                "currently_attached": true, "management": false,
                "ip_configuration_mode": "None", "IP": "", "netmask": "", "gateway": "", "DNS": "",
                "managed": true, "capabilities": [], "bond_slave_of": NULL_REF,
                "bond_master_of": [], "VLAN_master_of": NULL_REF, "VLAN_slave_of": [],
                "tunnel_access_PIF_of": [], "tunnel_transport_PIF_of": [],
                "sriov_logical_PIF_of": [], "sriov_physical_PIF_of": [], "other_config": {},
                "disallow_unplug": false
            });
            db["pif_metrics"][&metrics] = json!({
                "uuid": format!("ce000000-0000-4000-8000-0000000{h}{i}2{i}"), "carrier": true,
                "speed": 10000, "duplex": true, "vendor_name": "Intel Corporation",
                "device_name": "Ethernet Controller X710 for 10GbE SFP+",
                "pci_bus_path": format!("0000:02:00.{i}"), "vendor_id": "8086", "device_id": "1572"
            });
            if let Some(Value::Array(l)) = db["host"][host].get_mut("PIFs") {
                l.push(json!(pif));
            }
            pifs.push(json!(pif));
        }
        db["network"][&net] = json!({
            "uuid": format!("ae000000-0000-4000-8000-00000000002{i}"),
            "name_label": format!("Pool-wide network associated with {device}"),
            "name_description": "", "PIFs": pifs, "VIFs": [], "MTU": 1500,
            "bridge": format!("xenbr{}", i + 2), "other_config": {}, "managed": true, "tags": []
        });
    }
    for (host, _) in hosts {
        if let Some(Value::Object(sv)) = db["host"][host].get_mut("software_version") {
            sv.insert("network_backend".into(), json!("openvswitch"));
        }
    }
    db
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockServer;

    #[tokio::test(flavor = "multi_thread")]
    async fn the_lab_and_the_vif_operations() {
        let db = bond_lab_records();
        for pif in [
            "OpaqueRef:pif-01-eth2",
            "OpaqueRef:pif-01-eth3",
            "OpaqueRef:pif-02-eth2",
            "OpaqueRef:pif-02-eth3",
        ] {
            assert_eq!(db["pif"][pif]["physical"], true, "{pif}");
        }
        assert_eq!(
            db["host"]["OpaqueRef:host-0001"]["PIFs"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            db["host"]["OpaqueRef:host-0002"]["software_version"]["network_backend"],
            "openvswitch"
        );
        let server = MockServer::start_with_records(false, db).await;
        // Running with drivers: plug/unplug; halted: attach.
        assert_eq!(
            server.field("vif", "OpaqueRef:vif-web-0", "allowed_operations"),
            Some(json!(["unplug"]))
        );
        assert_eq!(
            server.field("vif", "OpaqueRef:vif-db-0", "allowed_operations"),
            Some(json!(["attach"]))
        );
        server.set_vm_power_state("OpaqueRef:vm-db", "Running");
        // No PV drivers: plugged by the start, no hot-plug.
        assert_eq!(
            server.field("vif", "OpaqueRef:vif-db-0", "currently_attached"),
            Some(json!(true))
        );
        assert_eq!(
            server.field("vif", "OpaqueRef:vif-db-0", "allowed_operations"),
            Some(json!([]))
        );
    }
}
