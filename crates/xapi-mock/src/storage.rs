//! Storage repositories in the mock, over a small simulated storage world.
//!
//! **The world.** Test-only targets, with the SRs already on them:
//!
//! * NFS server `nfs.lab` (or `192.0.2.50`): any export under `/exports/`;
//!   `/exports/vm` holds the fixture's "NFS VM storage", `/exports/old` an
//!   SR no pool knows (`f8000000-…-0001`, to introduce) and `/exports/iso`
//!   the fixture's ISO library with two ISO files. NFS versions 3, 4 and
//!   4.1. Another server: `SR_BACKEND_FAILURE_140`; another path:
//!   `SR_BACKEND_FAILURE_73`.
//! * SMB server `smb.lab`: user [`SMB_USER`] / [`SMB_PASSWORD`] for SMB SRs
//!   (else `SR_BACKEND_FAILURE_111`); `\\smb.lab\old` holds an SR to
//!   introduce (`…-0002`). ISO shares `//smb.lab/<share>` take guests,
//!   except `private`, which needs the same credentials (else
//!   `SR_BACKEND_FAILURE_222`).
//! * iSCSI: `iscsi.lab` (`192.0.2.60`) with two IQNs (`…:storage`: LUN 0
//!   holding an SR to introduce (`…-0003`) and an empty LUN 1; `…:archive`:
//!   an empty LUN 0); `chap.lab` (`192.0.2.61`) needs CHAP
//!   [`CHAP_USER`] / [`CHAP_PASSWORD`] (else `SR_BACKEND_FAILURE_68`);
//!   `unreachable.lab` answers `SR_BACKEND_FAILURE_141`; any other name
//!   `SR_BACKEND_FAILURE_140`. The IQN scan (`SR.create` with no
//!   `targetIQN`) fails with `SR_BACKEND_FAILURE_96` and the IQN list, the
//!   LUN scan (no `SCSIid`) with `SR_BACKEND_FAILURE_107` and the LUN list,
//!   as XAPI does.
//! * Local devices `/dev/sdb` and `/dev/sdc` on every host (else
//!   `SR_BACKEND_FAILURE_77`).
//!
//! **Calls.** `SR.create` (PBDs on every host for a shared SR, plugged
//! unless the host is set to fail plugs; passwords may come as
//! `<key>_secret` naming a `secret`), `SR.probe` (the SRs on the target as
//! XAPI's `<SRlist>`, with the NFS versions when `probeversion` is given),
//! `SR.introduce` (`SR_UUID_EXISTS` when known), `SR.forget` /
//! `SR.destroy` (only once every PBD is unplugged, as XAPI's
//! `allowed_operations` say), `SR.scan` (new ISO files become VDIs);
//! `PBD.create` (`PBD_EXISTS`), `plug` (checks the target again),
//! `unplug` (refused while a running VM uses the SR), `destroy`;
//! `secret.create` / `destroy`; the `trim` plugin (`"True"` when the SR's
//! SM plugin has `SR_TRIM`, else XAPI's `<trim_response>` error).
//! `allowed_operations` of the SRs follow their PBDs.

use serde_json::{Map, Value, json};

use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// Test-only SMB credentials.
pub const SMB_USER: &str = "smbuser";
pub const SMB_PASSWORD: &str = "smb-password";
/// Test-only CHAP credentials of `chap.lab`.
pub const CHAP_USER: &str = "chap-user";
pub const CHAP_PASSWORD: &str = "chap-secret";

/// The SR no pool knows on `nfs.lab:/exports/old`.
pub const OLD_NFS_SR: &str = "f8000000-0000-4000-8000-000000000001";
/// The SR no pool knows on `\\smb.lab\old`.
pub const OLD_SMB_SR: &str = "f8000000-0000-4000-8000-000000000002";
/// The SR on LUN 0 of `iqn.2026-11.lab.mock:storage`.
pub const OLD_ISCSI_SR: &str = "f8000000-0000-4000-8000-000000000003";

const GIB: i64 = 1 << 30;

/// Calls handled here.
const CALLS: &[(&str, &[&str])] = &[
    (
        "SR",
        &["create", "probe", "introduce", "forget", "destroy", "scan"],
    ),
    ("PBD", &["create", "plug", "unplug", "destroy"]),
    ("secret", &["create", "destroy"]),
];

/// Whether `class.call` is a storage call of the mock.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    CALLS
        .iter()
        .any(|(c, calls)| *c == class && calls.contains(&call))
}

/// An iSCSI LUN: `(lun id, SCSI id, serial, size)`.
type Lun = (i64, &'static str, &'static str, i64);

struct Target {
    names: &'static [&'static str],
    address: &'static str,
    chap: bool,
    iqns: &'static [(&'static str, &'static [Lun])],
}

const TARGETS: &[Target] = &[
    Target {
        names: &["iscsi.lab", "192.0.2.60"],
        address: "192.0.2.60",
        chap: false,
        iqns: &[
            (
                "iqn.2026-11.lab.mock:storage",
                &[
                    (0, "36001405000000000000000000000a000", "lun-a0", 100 * GIB),
                    (1, "36001405000000000000000000000a001", "lun-a1", 50 * GIB),
                ],
            ),
            (
                "iqn.2026-11.lab.mock:archive",
                &[(0, "36001405000000000000000000000b000", "lun-b0", 200 * GIB)],
            ),
        ],
    },
    Target {
        names: &["chap.lab", "192.0.2.61"],
        address: "192.0.2.61",
        chap: true,
        iqns: &[(
            "iqn.2026-11.lab.mock:secure",
            &[(0, "36001405000000000000000000000c000", "lun-c0", 20 * GIB)],
        )],
    },
];

/// A place on the storage that can hold SRs (an export, a share, a LUN, a
/// disk).
#[derive(Debug, Clone)]
struct Location {
    key: String,
    srs: Vec<String>,
    isos: Vec<String>,
    size: i64,
}

/// The simulated storage.
#[derive(Debug)]
pub(crate) struct World {
    locations: Vec<Location>,
    /// Hosts whose PBDs cannot be plugged (`SR_BACKEND_FAILURE_47`).
    pub(crate) plug_failures: Vec<String>,
}

impl Default for World {
    fn default() -> Self {
        let loc = |key: &str, srs: &[&str], isos: &[&str], size: i64| Location {
            key: key.to_owned(),
            srs: srs.iter().map(|s| (*s).to_owned()).collect(),
            isos: isos.iter().map(|s| (*s).to_owned()).collect(),
            size,
        };
        Self {
            locations: vec![
                loc(
                    "nfs:nfs.lab:/exports/vm",
                    &["5e000000-0000-4000-8000-000000000003"],
                    &[],
                    2048 * GIB,
                ),
                loc("nfs:nfs.lab:/exports/old", &[OLD_NFS_SR], &[], 1024 * GIB),
                loc(
                    "iso:nfs:nfs.lab:/exports/iso",
                    &["5e000000-0000-4000-8000-000000000004"],
                    &["debian-12.iso", "ubuntu-24.04-live-server.iso"],
                    100 * GIB,
                ),
                loc(r"smb:\\smb.lab\old", &[OLD_SMB_SR], &[], 1024 * GIB),
                loc(
                    "iscsi:36001405000000000000000000000a000",
                    &[OLD_ISCSI_SR],
                    &[],
                    100 * GIB,
                ),
            ],
            plug_failures: Vec::new(),
        }
    }
}

impl World {
    fn location(&mut self, key: &str, size: i64) -> &mut Location {
        if let Some(i) = self.locations.iter().position(|l| l.key == key) {
            return &mut self.locations[i];
        }
        self.locations.push(Location {
            key: key.to_owned(),
            srs: Vec::new(),
            isos: Vec::new(),
            size,
        });
        self.locations.last_mut().expect("just pushed")
    }

    fn find(&self, key: &str) -> Option<&Location> {
        self.locations.iter().find(|l| l.key == key)
    }

    /// Adds an ISO file to an ISO share (`"nfs.lab:/exports/iso"` or
    /// `"//smb.lab/iso"`).
    pub(crate) fn add_iso(&mut self, share: &str, name: &str) {
        let key = if share.starts_with("//") {
            format!("iso:cifs:{share}")
        } else {
            format!("iso:nfs:{share}")
        };
        let loc = self.location(&key, 100 * GIB);
        if !loc.isos.iter().any(|i| i == name) {
            loc.isos.push(name.to_owned());
        }
    }
}

type Failure = (String, Vec<String>);

fn fail(code: &str, params: &[&str]) -> Failure {
    (
        code.to_owned(),
        params.iter().map(|p| (*p).to_owned()).collect(),
    )
}

fn backend(n: u32, message: &str) -> Failure {
    fail(&format!("SR_BACKEND_FAILURE_{n}"), &["", message, ""])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `<SRlist>` with `srs` (and the NFS versions).
fn sr_list(srs: &[(String, i64)], versions: bool) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" ?>\n<SRlist>\n");
    for (uuid, size) in srs {
        xml.push_str(&format!(
            "\t<SR>\n\t\t<UUID>{}</UUID>\n\t\t<size>{size}</size>\n\t</SR>\n",
            xml_escape(uuid)
        ));
    }
    if versions {
        xml.push_str("\t<SupportedVersions>\n");
        for v in ["3", "4", "4.1"] {
            xml.push_str(&format!("\t\t<Version>{v}</Version>\n"));
        }
        xml.push_str("\t</SupportedVersions>\n");
    }
    xml.push_str("</SRlist>\n");
    xml
}

fn iqn_list(target: &Target) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" ?>\n<iscsi-target-iqns>\n");
    for (i, (iqn, _)) in target.iqns.iter().enumerate() {
        xml.push_str(&format!(
            "\t<TGT>\n\t\t<Index>{i}</Index>\n\t\t<IPAddress>{}</IPAddress>\n\t\t<TargetIQN>{iqn}</TargetIQN>\n\t</TGT>\n",
            target.address
        ));
    }
    xml.push_str("</iscsi-target-iqns>\n");
    xml
}

fn lun_list(luns: &[Lun]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" ?>\n<iscsi-target>\n");
    for (id, scsi, serial, size) in luns {
        xml.push_str(&format!(
            "\t<LUN>\n\t\t<vendor>LIO-ORG</vendor>\n\t\t<serial>{serial}</serial>\n\t\t<LUNid>{id}</LUNid>\n\t\t<size>{size}</size>\n\t\t<SCSIid>{scsi}</SCSIid>\n\t</LUN>\n"
        ));
    }
    xml.push_str("</iscsi-target>\n");
    xml
}

fn map_of(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

fn get<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

/// What a `device_config` designates.
struct Located {
    key: String,
    size: i64,
    /// Formatting replaces what was there (LUNs, disks).
    exclusive: bool,
}

impl Db {
    /// A `device_config` value, through its secret if it is one
    /// (`<key>_secret`).
    fn secret_value(
        &self,
        dconf: &Map<String, Value>,
        key: &str,
    ) -> Result<Option<String>, Failure> {
        if let Some(v) = get(dconf, key) {
            return Ok(Some(v.to_owned()));
        }
        let Some(uuid) = get(dconf, &format!("{key}_secret")) else {
            return Ok(None);
        };
        self.objects
            .iter()
            .find(|((c, _), rec)| c == "secret" && get_str(rec, "uuid") == Some(uuid))
            .map(|(_, rec)| Some(get_str(rec, "value").unwrap_or_default().to_owned()))
            .ok_or_else(|| fail("UUID_INVALID", &["secret", uuid]))
    }

    /// Checks `dconf` of an SR of `sr_type` against the world; the place it
    /// designates, or the error the SM plugin answers. `scan` is the IQN /
    /// LUN scan's create.
    fn locate(&self, sr_type: &str, dconf: &Map<String, Value>) -> Result<Located, Failure> {
        match sr_type {
            "nfs" => {
                let server = get(dconf, "server").unwrap_or_default();
                if !matches!(server, "nfs.lab" | "192.0.2.50") {
                    return Err(backend(140, "Incorrect DNS name, unable to resolve."));
                }
                let path = get(dconf, "serverpath").unwrap_or_default();
                if !path.starts_with("/exports/") || path.len() <= "/exports/".len() {
                    return Err(backend(73, "NFS mount error"));
                }
                Ok(Located {
                    key: format!("nfs:nfs.lab:{}", path.trim_end_matches('/')),
                    size: 1024 * GIB,
                    exclusive: false,
                })
            }
            "smb" => {
                let server = get(dconf, "server").unwrap_or_default();
                let host = server
                    .trim_start_matches('\\')
                    .split('\\')
                    .next()
                    .unwrap_or_default();
                if host != "smb.lab" || server.matches('\\').count() < 3 {
                    return Err(backend(140, "Incorrect DNS name, unable to resolve."));
                }
                let user = get(dconf, "username").unwrap_or_default();
                let password = self.secret_value(dconf, "password")?.unwrap_or_default();
                if user != SMB_USER || password != SMB_PASSWORD {
                    return Err(backend(111, "SMB mount error"));
                }
                Ok(Located {
                    key: format!("smb:{}", server.trim_end_matches('\\')),
                    size: 1024 * GIB,
                    exclusive: false,
                })
            }
            "iso" => {
                let location = get(dconf, "location").unwrap_or_default();
                match get(dconf, "type") {
                    Some("nfs_iso") => {
                        let Some((server, path)) = location.split_once(":/") else {
                            return Err(backend(225, "Incorrect Location string format"));
                        };
                        if !matches!(server, "nfs.lab" | "192.0.2.50") {
                            return Err(backend(140, "Incorrect DNS name, unable to resolve."));
                        }
                        if !path.starts_with("exports/") {
                            return Err(backend(222, "Could not mount the directory"));
                        }
                        Ok(Located {
                            key: format!("iso:nfs:nfs.lab:/{}", path.trim_end_matches('/')),
                            size: 100 * GIB,
                            exclusive: false,
                        })
                    }
                    Some("cifs") => {
                        let Some(rest) = location.strip_prefix("//") else {
                            return Err(backend(225, "Incorrect Location string format"));
                        };
                        let (host, share) = rest.split_once('/').unwrap_or((rest, ""));
                        if host != "smb.lab" {
                            return Err(backend(140, "Incorrect DNS name, unable to resolve."));
                        }
                        if share.is_empty() {
                            return Err(backend(225, "Incorrect Location string format"));
                        }
                        let user = get(dconf, "username");
                        let password = self.secret_value(dconf, "cifspassword")?;
                        let authenticated =
                            user == Some(SMB_USER) && password.as_deref() == Some(SMB_PASSWORD);
                        if (share == "private" || user.is_some()) && !authenticated {
                            return Err(backend(222, "Could not mount the directory"));
                        }
                        Ok(Located {
                            key: format!("iso:cifs://smb.lab/{share}"),
                            size: 100 * GIB,
                            exclusive: false,
                        })
                    }
                    _ => Err(backend(225, "Incorrect Location string format")),
                }
            }
            "lvmoiscsi" => {
                let target = self.iscsi_target(dconf)?;
                let Some(iqn) = get(dconf, "targetIQN") else {
                    return Err(fail("SR_BACKEND_FAILURE_96", &["", "", &iqn_list(target)]));
                };
                let Some((_, luns)) = target.iqns.iter().find(|(i, _)| *i == iqn) else {
                    return Err(backend(202, "General backend error: unknown target IQN"));
                };
                let lun = match (get(dconf, "SCSIid"), get(dconf, "LUNSerial")) {
                    (Some(id), _) => luns.iter().find(|l| l.1 == id),
                    (None, Some(serial)) => luns.iter().find(|l| l.2 == serial),
                    (None, None) => {
                        return Err(fail("SR_BACKEND_FAILURE_107", &["", "", &lun_list(luns)]));
                    }
                };
                let Some(lun) = lun else {
                    return Err(backend(47, "The SR is not available"));
                };
                Ok(Located {
                    key: format!("iscsi:{}", lun.1),
                    size: lun.3,
                    exclusive: true,
                })
            }
            "ext" | "lvm" | "xfs" => {
                let device = get(dconf, "device").unwrap_or_default();
                if !matches!(device, "/dev/sdb" | "/dev/sdc") {
                    return Err(backend(77, "Logical Volume group creation failed"));
                }
                Ok(Located {
                    key: format!("local:{device}"),
                    size: 500 * GIB,
                    exclusive: true,
                })
            }
            other => Err(fail("SR_UNKNOWN_DRIVER", &[other])),
        }
    }

    fn iscsi_target(&self, dconf: &Map<String, Value>) -> Result<&'static Target, Failure> {
        let name = get(dconf, "target").unwrap_or_default();
        if name == "unreachable.lab" {
            return Err(backend(141, "Unable to connect to ISCSI service on target"));
        }
        let Some(target) = TARGETS.iter().find(|t| t.names.contains(&name)) else {
            return Err(backend(140, "Incorrect DNS name, unable to resolve."));
        };
        if target.chap {
            let user = get(dconf, "chapuser");
            let password = self.secret_value(dconf, "chappassword")?;
            if user != Some(CHAP_USER) || password.as_deref() != Some(CHAP_PASSWORD) {
                return Err(backend(68, "ISCSI login failed, verify CHAP credentials"));
            }
        }
        Ok(target)
    }

    /// The place an SR's first PBD designates.
    fn sr_location(&self, sr: &str) -> Option<Located> {
        let rec = self.record("sr", sr)?;
        let sr_type = get(rec, "type")?.to_owned();
        let pbd = rec
            .get("PBDs")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(Value::as_str)?;
        let dconf = map_of(self.record("pbd", pbd)?.get("device_config"));
        self.locate(&sr_type, &dconf).ok()
    }

    fn sm_has(&self, sr_type: &str, feature: &str) -> Option<bool> {
        self.objects
            .iter()
            .find(|((c, _), rec)| c == "sm" && get_str(rec, "type") == Some(sr_type))
            .map(|(_, rec)| {
                rec.get("features")
                    .and_then(Value::as_object)
                    .is_some_and(|f| f.contains_key(feature))
            })
    }

    fn pbds_of(&self, sr: &str) -> Vec<String> {
        self.record("sr", sr)
            .and_then(|r| r.get("PBDs"))
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn pbd_attached(&self, pbd: &str) -> bool {
        self.record("pbd", pbd)
            .and_then(|r| r.get("currently_attached"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `allowed_operations` of an SR from its PBDs (XAPI: forget and
    /// destroy need every PBD unplugged; destroy needs one to read the
    /// configuration from).
    pub(crate) fn refresh_sr_ops(&mut self, sr: &str) {
        let Some(rec) = self.record("sr", sr) else {
            return;
        };
        let iso = get(rec, "content_type") == Some("iso");
        let pbds = self.pbds_of(sr);
        let attached = pbds.iter().any(|p| self.pbd_attached(p));
        let ops: Vec<&str> = if pbds.is_empty() {
            vec!["forget", "pbd_create"]
        } else if attached {
            let mut ops = vec![
                "scan",
                "unplug",
                "plug",
                "pbd_create",
                "pbd_destroy",
                "update",
                "vdi_create",
                "vdi_destroy",
                "vdi_clone",
            ];
            if !iso {
                ops.extend(["vdi_snapshot", "vdi_resize", "vdi_mirror"]);
            }
            ops
        } else {
            vec!["forget", "destroy", "plug", "pbd_create", "pbd_destroy"]
        };
        self.modify("sr", sr, |rec| {
            rec.insert("allowed_operations".into(), json!(ops));
        });
    }

    /// Fills `allowed_operations` of the SRs that have none (the fixture's).
    pub(crate) fn init_sr_ops(&mut self) {
        let bare: Vec<String> = self
            .objects
            .iter()
            .filter(|((c, _), rec)| c == "sr" && rec.get("allowed_operations").is_none())
            .map(|((_, r), _)| r.clone())
            .collect();
        for sr in bare {
            self.refresh_sr_ops(&sr);
        }
    }

    /// Runs a storage call; `None` if it is not one.
    pub(crate) fn storage_call(
        &mut self,
        class: &str,
        call: &str,
        params: &[Value],
    ) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        Some(match (class, call) {
            ("SR", "create") => self.sr_create(params),
            ("SR", "probe") => self.sr_probe(params),
            ("SR", "introduce") => self.sr_introduce(params),
            ("SR", "forget") => self.sr_remove(str_param(params, 1), false),
            ("SR", "destroy") => self.sr_remove(str_param(params, 1), true),
            ("SR", "scan") => self.sr_scan(str_param(params, 1)),
            ("PBD", "create") => self.pbd_create(params.get(1)),
            ("PBD", "plug") => self.pbd_plug(str_param(params, 1)),
            ("PBD", "unplug") => self.pbd_unplug(str_param(params, 1)),
            ("PBD", "destroy") => self.pbd_destroy(str_param(params, 1)),
            ("secret", "create") => {
                let rec = map_of(params.get(1));
                let (r, uuid) = self.new_ref("secret");
                let value = get(&rec, "value").unwrap_or_default().to_owned();
                self.insert(
                    "secret",
                    &r,
                    json!({"uuid": uuid, "value": value, "other_config": {}}),
                );
                Ok(json!(r))
            }
            ("secret", "destroy") => {
                let r = str_param(params, 1).to_owned();
                if !self.exists("secret", &r) {
                    return Some(api_error("HANDLE_INVALID", &["secret", &r]));
                }
                self.remove("secret", &r);
                Ok(json!(""))
            }
            _ => unreachable!("handled call"),
        })
    }

    fn sr_create(&mut self, params: &[Value]) -> Reply {
        let host = str_param(params, 1).to_owned();
        if !self.exists("host", &host) {
            return api_error("HANDLE_INVALID", &["host", &host]);
        }
        let dconf = map_of(params.get(2));
        let name = str_param(params, 4).to_owned();
        let description = str_param(params, 5).to_owned();
        let sr_type = str_param(params, 6).to_owned();
        let content_type = str_param(params, 7).to_owned();
        let shared = params.get(8).and_then(Value::as_bool).unwrap_or(false);
        let mut sm_config = map_of(params.get(9));
        if self.sm_has(&sr_type, "").is_none() {
            return api_error("SR_UNKNOWN_DRIVER", &[&sr_type]);
        }
        let located = self.locate(&sr_type, &dconf)?;
        if sr_type == "iso"
            && let Some(t) = get(&dconf, "type")
        {
            sm_config.insert("iso_type".into(), json!(t));
        }
        let (sr, uuid) = self.new_ref("sr");
        let hosts: Vec<String> = if shared {
            self.objects
                .keys()
                .filter(|(c, _)| c == "host")
                .map(|(_, r)| r.clone())
                .collect()
        } else {
            vec![host]
        };
        self.insert(
            "sr",
            &sr,
            json!({
                "uuid": uuid, "name_label": name, "name_description": description,
                "type": sr_type, "content_type": content_type, "shared": shared,
                "PBDs": [], "VDIs": [], "physical_size": located.size,
                "physical_utilisation": 0, "virtual_allocation": 0,
                "other_config": {}, "sm_config": sm_config, "tags": [],
                "introduced_by": NULL_REF, "is_tools_sr": false,
                "allowed_operations": [], "current_operations": {},
                "local_cache_enabled": false, "clustered": false, "blobs": {}
            }),
        );
        for h in hosts {
            let (pbd, pbd_uuid) = self.new_ref("pbd");
            let plugged = !self.storage.plug_failures.contains(&h);
            self.insert(
                "pbd",
                &pbd,
                json!({"uuid": pbd_uuid, "host": h, "SR": sr, "device_config": dconf,
                       "currently_attached": plugged, "other_config": {}}),
            );
            self.push_ref("sr", &sr, "PBDs", &pbd);
            self.push_ref("host", &h, "PBDs", &pbd);
        }
        let loc = self.storage.location(&located.key, located.size);
        if located.exclusive {
            loc.srs.clear();
        }
        loc.srs.push(uuid);
        let isos = loc.isos.clone();
        self.add_isos(&sr, &isos);
        self.refresh_sr_ops(&sr);
        Ok(json!(sr))
    }

    /// New VDIs for the ISO files an ISO SR does not list yet.
    fn add_isos(&mut self, sr: &str, isos: &[String]) {
        let known: Vec<String> = self
            .record("sr", sr)
            .and_then(|r| r.get("VDIs"))
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|v| self.get_str("vdi", v, "name_label"))
                    .collect()
            })
            .unwrap_or_default();
        for iso in isos.iter().filter(|i| !known.contains(i)) {
            let (vdi, uuid) = self.new_ref("vdi");
            self.insert(
                "vdi",
                &vdi,
                json!({"uuid": uuid, "name_label": iso, "name_description": "", "SR": sr,
                       "VBDs": [], "type": "user", "virtual_size": 700 * 1024 * 1024,
                       "physical_utilisation": 700 * 1024 * 1024, "read_only": false,
                       "sharable": false, "managed": true, "missing": false,
                       "is_a_snapshot": false, "snapshot_of": NULL_REF, "snapshots": [],
                       "other_config": {}, "sm_config": {}, "location": iso,
                       "allowed_operations": [], "current_operations": {}, "cbt_enabled": false}),
            );
            self.push_ref("sr", sr, "VDIs", &vdi);
        }
    }

    fn sr_probe(&self, params: &[Value]) -> Reply {
        let host = str_param(params, 1);
        if !self.exists("host", host) {
            return api_error("HANDLE_INVALID", &["host", host]);
        }
        let dconf = map_of(params.get(2));
        let sr_type = str_param(params, 3);
        if self.sm_has(sr_type, "").is_none() {
            return api_error("SR_UNKNOWN_DRIVER", &[sr_type]);
        }
        let located = self.locate(sr_type, &dconf)?;
        let srs: Vec<(String, i64)> = self
            .storage
            .find(&located.key)
            .map(|l| l.srs.iter().map(|s| (s.clone(), l.size)).collect())
            .unwrap_or_default();
        Ok(json!(sr_list(&srs, dconf.contains_key("probeversion"))))
    }

    fn sr_introduce(&mut self, params: &[Value]) -> Reply {
        let uuid = str_param(params, 1).to_owned();
        if self
            .objects
            .iter()
            .any(|((c, _), rec)| c == "sr" && get_str(rec, "uuid") == Some(&uuid))
        {
            return api_error("SR_UUID_EXISTS", &[&uuid]);
        }
        let sr_type = str_param(params, 4).to_owned();
        let (sr, _) = self.new_ref("sr");
        self.insert(
            "sr",
            &sr,
            json!({
                "uuid": uuid, "name_label": str_param(params, 2),
                "name_description": str_param(params, 3), "type": sr_type,
                "content_type": str_param(params, 5),
                "shared": params.get(6).and_then(Value::as_bool).unwrap_or(false),
                "PBDs": [], "VDIs": [], "physical_size": 0, "physical_utilisation": 0,
                "virtual_allocation": 0, "other_config": {},
                "sm_config": params.get(7).cloned().unwrap_or(json!({})), "tags": [],
                "introduced_by": NULL_REF, "is_tools_sr": false,
                "allowed_operations": [], "current_operations": {},
                "local_cache_enabled": false, "clustered": false, "blobs": {}
            }),
        );
        self.refresh_sr_ops(&sr);
        Ok(json!(sr))
    }

    /// `SR.forget` (`destroy` false) and `SR.destroy`.
    fn sr_remove(&mut self, sr: &str, destroy: bool) -> Reply {
        let Some(rec) = self.record("sr", sr) else {
            return api_error("HANDLE_INVALID", &["SR", sr]);
        };
        let op = if destroy { "destroy" } else { "forget" };
        let allowed = rec
            .get("allowed_operations")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|o| o.as_str() == Some(op)));
        if !allowed {
            let code = if destroy && self.pbds_of(sr).is_empty() {
                "SR_HAS_NO_PBDS"
            } else {
                "SR_HAS_PBD"
            };
            return api_error(code, &[sr]);
        }
        let uuid = get(rec, "uuid").unwrap_or_default().to_owned();
        let vdis: Vec<String> = rec
            .get("VDIs")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if destroy && let Some(located) = self.sr_location(sr) {
            let loc = self.storage.location(&located.key, located.size);
            loc.srs.retain(|s| *s != uuid);
        }
        for pbd in self.pbds_of(sr) {
            if let Some(host) = self.get_str("pbd", &pbd, "host") {
                self.drop_ref("host", &host, "PBDs", &pbd);
            }
            self.remove("pbd", &pbd);
        }
        for vdi in vdis {
            self.remove("vdi", &vdi);
        }
        self.remove("sr", sr);
        let pools: Vec<String> = self
            .objects
            .keys()
            .filter(|(c, _)| c == "pool")
            .map(|(_, r)| r.clone())
            .collect();
        for pool in pools {
            self.modify("pool", &pool, |rec| {
                for field in ["default_SR", "suspend_image_SR", "crash_dump_SR"] {
                    if rec.get(field).and_then(Value::as_str) == Some(sr) {
                        rec.insert(field.into(), json!(NULL_REF));
                    }
                }
            });
        }
        Ok(json!(""))
    }

    fn sr_scan(&mut self, sr: &str) -> Reply {
        if !self.exists("sr", sr) {
            return api_error("HANDLE_INVALID", &["SR", sr]);
        }
        let pbds = self.pbds_of(sr);
        if pbds.is_empty() {
            return api_error("SR_HAS_NO_PBDS", &[sr]);
        }
        if !pbds.iter().any(|p| self.pbd_attached(p)) {
            return api_error("SR_NOT_ATTACHED", &[sr]);
        }
        if let Some(located) = self.sr_location(sr) {
            let isos = self
                .storage
                .find(&located.key)
                .map(|l| l.isos.clone())
                .unwrap_or_default();
            self.add_isos(sr, &isos);
        }
        Ok(json!(""))
    }

    fn pbd_create(&mut self, record: Option<&Value>) -> Reply {
        let rec = map_of(record);
        let host = get(&rec, "host").unwrap_or_default().to_owned();
        let sr = get(&rec, "SR").unwrap_or_default().to_owned();
        if !self.exists("host", &host) {
            return api_error("HANDLE_INVALID", &["host", &host]);
        }
        if !self.exists("sr", &sr) {
            return api_error("HANDLE_INVALID", &["SR", &sr]);
        }
        if self
            .pbds_of(&sr)
            .iter()
            .any(|p| self.get_str("pbd", p, "host").as_deref() == Some(&host))
        {
            return api_error("PBD_EXISTS", &[&sr, &host]);
        }
        let (pbd, uuid) = self.new_ref("pbd");
        self.insert(
            "pbd",
            &pbd,
            json!({"uuid": uuid, "host": host, "SR": sr,
                   "device_config": rec.get("device_config").cloned().unwrap_or(json!({})),
                   "currently_attached": false,
                   "other_config": rec.get("other_config").cloned().unwrap_or(json!({}))}),
        );
        self.push_ref("sr", &sr, "PBDs", &pbd);
        self.push_ref("host", &host, "PBDs", &pbd);
        self.refresh_sr_ops(&sr);
        Ok(json!(pbd))
    }

    fn pbd_plug(&mut self, pbd: &str) -> Reply {
        let Some(rec) = self.record("pbd", pbd) else {
            return api_error("HANDLE_INVALID", &["PBD", pbd]);
        };
        if self.pbd_attached(pbd) {
            return Ok(json!(""));
        }
        let host = get(rec, "host").unwrap_or_default().to_owned();
        let sr = get(rec, "SR").unwrap_or_default().to_owned();
        let dconf = map_of(rec.get("device_config"));
        if self.storage.plug_failures.contains(&host) {
            return Err(backend(47, "The SR is not available"));
        }
        let sr_type = self.get_str("sr", &sr, "type").unwrap_or_default();
        let located = self.locate(&sr_type, &dconf)?;
        let uuid = self.get_str("sr", &sr, "uuid").unwrap_or_default();
        let loc = self.storage.location(&located.key, located.size);
        if !loc.srs.contains(&uuid) {
            // XAPI's SM finds no SR with this UUID there.
            return Err(backend(
                47,
                "The SR is not available: no SR with this UUID at this location",
            ));
        }
        let (size, isos) = (loc.size, loc.isos.clone());
        self.modify("pbd", pbd, |rec| {
            rec.insert("currently_attached".into(), json!(true));
        });
        if self
            .record("sr", &sr)
            .is_some_and(|r| r.get("physical_size").and_then(Value::as_i64) == Some(0))
        {
            self.modify("sr", &sr, |rec| {
                rec.insert("physical_size".into(), json!(size));
            });
        }
        if sr_type == "iso" {
            self.add_isos(&sr, &isos);
        }
        self.refresh_sr_ops(&sr);
        Ok(json!(""))
    }

    /// A running VM uses a disk of `sr`.
    fn sr_in_use(&self, sr: &str) -> Option<String> {
        let vdis: Vec<String> = self
            .record("sr", sr)?
            .get("VDIs")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        vdis.into_iter().find(|vdi| {
            self.record("vdi", vdi)
                .and_then(|r| r.get("VBDs"))
                .and_then(Value::as_array)
                .is_some_and(|vbds| {
                    vbds.iter().filter_map(Value::as_str).any(|vbd| {
                        self.get_str("vbd", vbd, "VM")
                            .is_some_and(|vm| self.vm_power(&vm) == "Running")
                    })
                })
        })
    }

    fn pbd_unplug(&mut self, pbd: &str) -> Reply {
        let Some(rec) = self.record("pbd", pbd) else {
            return api_error("HANDLE_INVALID", &["PBD", pbd]);
        };
        let sr = get(rec, "SR").unwrap_or_default().to_owned();
        if !self.pbd_attached(pbd) {
            return Ok(json!(""));
        }
        if let Some(vdi) = self.sr_in_use(&sr) {
            return api_error("VDI_IN_USE", &[&vdi, "unplug"]);
        }
        self.modify("pbd", pbd, |rec| {
            rec.insert("currently_attached".into(), json!(false));
        });
        self.refresh_sr_ops(&sr);
        Ok(json!(""))
    }

    fn pbd_destroy(&mut self, pbd: &str) -> Reply {
        let Some(rec) = self.record("pbd", pbd) else {
            return api_error("HANDLE_INVALID", &["PBD", pbd]);
        };
        if self.pbd_attached(pbd) {
            return api_error("OPERATION_NOT_ALLOWED", &["PBD is currently attached"]);
        }
        let sr = get(rec, "SR").unwrap_or_default().to_owned();
        let host = get(rec, "host").unwrap_or_default().to_owned();
        self.drop_ref("sr", &sr, "PBDs", pbd);
        self.drop_ref("host", &host, "PBDs", pbd);
        self.remove("pbd", pbd);
        self.refresh_sr_ops(&sr);
        Ok(json!(""))
    }

    /// `host.call_plugin(host, "trim", "do_trim", {sr_uuid})`.
    pub(crate) fn trim(&self, params: &[Value]) -> Reply {
        let args = map_of(params.get(4));
        let uuid = get(&args, "sr_uuid").unwrap_or_default();
        let Some(sr_type) = self
            .objects
            .iter()
            .find(|((c, _), rec)| c == "sr" && get_str(rec, "uuid") == Some(uuid))
            .and_then(|(_, rec)| get_str(rec, "type"))
        else {
            return api_error("UUID_INVALID", &["SR", uuid]);
        };
        if self.sm_has(sr_type, "SR_TRIM") == Some(true) {
            return Ok(json!("True"));
        }
        Ok(json!(format!(
            "<?xml version=\"1.0\" ?><trim_response><key_value_pair><key>errcode</key>\
             <value>UnsupportedSRForTrim</value></key_value_pair><key_value_pair><key>errmsg</key>\
             <value>Trim on [{uuid}] not supported</value></key_value_pair></trim_response>"
        )))
    }
}

fn get_str<'a>(rec: &'a Value, field: &str) -> Option<&'a str> {
    rec.get(field).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_world_answers_like_the_sm_plugins() {
        let state = crate::State::new(crate::fixture_records());
        let db = state.db();
        let nfs = |server: &str, path: &str| {
            let mut m = Map::new();
            m.insert("server".into(), json!(server));
            m.insert("serverpath".into(), json!(path));
            m
        };
        assert_eq!(
            db.locate("nfs", &nfs("nfs.lab", "/exports/vm"))
                .unwrap()
                .key,
            "nfs:nfs.lab:/exports/vm"
        );
        assert_eq!(
            db.locate("nfs", &nfs("other.lab", "/exports/vm"))
                .err()
                .unwrap()
                .0,
            "SR_BACKEND_FAILURE_140"
        );
        assert_eq!(
            db.locate("nfs", &nfs("nfs.lab", "/srv")).err().unwrap().0,
            "SR_BACKEND_FAILURE_73"
        );
        let mut iscsi = Map::new();
        iscsi.insert("target".into(), json!("iscsi.lab"));
        let (code, params) = db.locate("lvmoiscsi", &iscsi).err().unwrap();
        assert_eq!(code, "SR_BACKEND_FAILURE_96");
        assert!(params[2].contains("iqn.2026-11.lab.mock:storage"));
        iscsi.insert("targetIQN".into(), json!("iqn.2026-11.lab.mock:storage"));
        let (code, params) = db.locate("lvmoiscsi", &iscsi).err().unwrap();
        assert_eq!(code, "SR_BACKEND_FAILURE_107");
        assert!(params[2].contains("<LUNid>1</LUNid>"));
        let mut chap = Map::new();
        chap.insert("target".into(), json!("chap.lab"));
        assert_eq!(
            db.locate("lvmoiscsi", &chap).err().unwrap().0,
            "SR_BACKEND_FAILURE_68"
        );
        chap.insert("chapuser".into(), json!(CHAP_USER));
        chap.insert("chappassword".into(), json!(CHAP_PASSWORD));
        assert_eq!(
            db.locate("lvmoiscsi", &chap).err().unwrap().0,
            "SR_BACKEND_FAILURE_96"
        );
        assert!(sr_list(&[("u".into(), 1)], true).contains("<Version>4.1</Version>"));
    }
}
