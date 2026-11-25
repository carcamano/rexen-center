//! Storage repositories: the New SR wizard's request, when each SR
//! operation is offered, and the probe answers the wizard reads.
//!
//! The rules follow the original console:
//!
//! * the wizard types (`SrWizardType_*`): NFS, iSCSI, SMB, NFS ISO, SMB ISO
//!   and local storage (ext / LVM / XFS). Hardware HBA, software FCoE,
//!   GFS2 and the SR types only XCP-ng ships are not offered;
//! * the commands (`RepairSRCommand`, `DetachSRCommand`,
//!   `ReattachSRCommand`, `ForgetSRCommand`, `DestroySRCommand`,
//!   `SetAsDefaultSRCommand`, `TrimSRCommand`, the SR Storage tab's Rescan
//!   button) and the `SR` helpers they use (`HasRunningVMs`,
//!   `CanCreateWithXenCenter`, `SupportsTrim`, `SupportsVdiCreate`…);
//! * the probe answers (`SR.ParseSRListXML`,
//!   `ParseSupportedVersionsListXML`, the iSCSI IQN and LUN lists of
//!   `ISCSIPopulateIQNsAction` / `ISCSIPopulateLunsAction`, the trim
//!   plugin's error).
//!
//! How each operation runs is [`crate::storage_run`].

use std::collections::HashMap;
use std::fmt;

use quick_xml::Reader;
use quick_xml::events::Event;
use xapi::{Host, PbdRecord, Ref, Sr, SrRecord, StorageOperations, VdiType, VmPowerState};

use crate::model::ConnectionModel;
use crate::objects;

/// The prefix of the temporary objects the console makes (the iSCSI scans'
/// SR name, `Helpers.GuiTempObjectPrefix`).
pub const GUI_TEMP_PREFIX: &str = "__gui__";

/// The default iSCSI port.
pub const ISCSI_PORT: u16 = 3260;

/// Errors this client reports itself (codes of [`xapi::Error::Api`], worded
/// by the UI).
pub mod errors {
    /// The pool or its coordinator is gone (`NEW_SR_CONNECTION_LOST`).
    pub const NO_COORDINATOR: &str = "XENADMIN_NO_COORDINATOR";
    /// No IQN on the target (`NEWSR_NO_IQNS_FOUND`); param: the target.
    pub const NO_IQNS: &str = "XENADMIN_ISCSI_NO_IQNS";
    /// No LUN on the target (`NEWSR_NO_LUNS_FOUND`); param: the target.
    pub const NO_LUNS: &str = "XENADMIN_ISCSI_NO_LUNS";
    /// The scan's answer could not be read (`BadServerResponse`); param:
    /// the target.
    pub const BAD_RESPONSE: &str = "XENADMIN_ISCSI_BAD_RESPONSE";
    /// The scan's create call worked (`ISCSI_FAIL`).
    pub const SCAN_CREATED: &str = "XENADMIN_ISCSI_SCAN_CREATED";
    /// Trim on a detached SR (`SR_TRIM_NO_STORAGE_HOST_ERROR`).
    pub const TRIM_DETACHED: &str = "XENADMIN_SR_TRIM_DETACHED";
    /// The trim plugin answered with an error; params: its `errcode` and
    /// `errmsg` (either may be empty).
    pub const TRIM_FAILED: &str = "XENADMIN_SR_TRIM_FAILED";
    /// Reattach of an SR that still has PBDs (`STORAGE_IN_USE`).
    pub const SR_IN_USE: &str = "XENADMIN_SR_IN_USE";
}

/// A client-side error.
pub fn client_error(code: &str, params: &[&str]) -> xapi::Error {
    xapi::Error::Api {
        code: code.to_owned(),
        params: params.iter().map(|p| (*p).to_owned()).collect(),
    }
}

// ---- the wizard's types ----------------------------------------------------

/// The filesystem of a local SR (the local storage page's radio buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalFs {
    Ext,
    Lvm,
    Xfs,
}

impl LocalFs {
    pub const ALL: [LocalFs; 3] = [LocalFs::Ext, LocalFs::Lvm, LocalFs::Xfs];

    /// The SR type.
    pub fn sr_type(self) -> &'static str {
        match self {
            Self::Ext => "ext",
            Self::Lvm => "lvm",
            Self::Xfs => "xfs",
        }
    }
}

/// A type of the New SR wizard (`SrWizardType_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrKind {
    /// NFS VHD (`SrWizardType_VhdoNfs`).
    Nfs,
    /// LVM over iSCSI (`SrWizardType_Iscsi`).
    Iscsi,
    /// SMB VHD (`SrWizardType_Cifs`).
    Smb,
    /// NFS ISO library (`SrWizardType_NfsIso`).
    NfsIso,
    /// SMB/CIFS ISO library (`SrWizardType_CifsIso`).
    SmbIso,
    /// Local storage (`SrWizardType_LocalStorage`).
    Local(LocalFs),
}

impl SrKind {
    /// The types in the order of the type page: virtual disk storage, then
    /// ISO libraries.
    pub const ALL: [SrKind; 6] = [
        SrKind::Nfs,
        SrKind::Iscsi,
        SrKind::Smb,
        SrKind::Local(LocalFs::Ext),
        SrKind::NfsIso,
        SrKind::SmbIso,
    ];

    /// The SR type created.
    pub fn sr_type(self) -> &'static str {
        match self {
            Self::Nfs => "nfs",
            Self::Iscsi => "lvmoiscsi",
            Self::Smb => "smb",
            Self::NfsIso | Self::SmbIso => "iso",
            Self::Local(fs) => fs.sr_type(),
        }
    }

    /// The SR types an SR of this kind can have (`PossibleTypes`).
    pub fn possible_types(self) -> &'static [&'static str] {
        match self {
            Self::Nfs => &["nfs"],
            Self::Iscsi => &["lvmoiscsi"],
            Self::Smb => &["smb"],
            Self::NfsIso | Self::SmbIso => &["iso"],
            Self::Local(_) => &["ext", "lvm", "xfs"],
        }
    }

    /// `content_type` of the new SR.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::NfsIso | Self::SmbIso => "iso",
            _ => "",
        }
    }

    /// An ISO library.
    pub fn is_iso(self) -> bool {
        matches!(self, Self::NfsIso | Self::SmbIso)
    }

    /// Whether the SR is shared. The original always creates shared SRs;
    /// a local SR is not shared here (one host's disk).
    pub fn shared(self) -> bool {
        !matches!(self, Self::Local(_))
    }

    /// Introducing an SR found by the probe asks first
    /// (`ShowIntroducePrompt`).
    pub fn introduce_prompt(self) -> bool {
        matches!(self, Self::Nfs | Self::Local(_))
    }

    /// Reattaching a detached SR asks first (`ShowReattachWarning`).
    pub fn reattach_warning(self) -> bool {
        matches!(self, Self::Nfs | Self::Iscsi | Self::Local(_))
    }

    /// Whether a new SR may be created when reattaching (`AllowToCreateNewSr`
    /// is fixed to true for the ISO libraries and SMB).
    pub fn always_creates(self) -> bool {
        matches!(self, Self::Smb | Self::NfsIso | Self::SmbIso)
    }

    /// Whether the location page can scan for existing SRs.
    pub fn probes(self) -> bool {
        matches!(self, Self::Nfs | Self::Iscsi | Self::Smb)
    }

    /// Same wizard page (local storage kinds differ only by filesystem).
    pub fn same_page(self, other: SrKind) -> bool {
        matches!((self, other), (Self::Local(_), Self::Local(_))) || self == other
    }

    /// The kind that matches an existing SR (to reattach it): its type and,
    /// for ISO libraries, `sm_config["iso_type"]`.
    pub fn of_sr(sr: &SrRecord) -> Option<SrKind> {
        match sr.r#type.as_str() {
            "nfs" => Some(Self::Nfs),
            "lvmoiscsi" => Some(Self::Iscsi),
            "smb" => Some(Self::Smb),
            "ext" => Some(Self::Local(LocalFs::Ext)),
            "lvm" => Some(Self::Local(LocalFs::Lvm)),
            "xfs" => Some(Self::Local(LocalFs::Xfs)),
            "iso" => match sr.sm_config.get("iso_type").map(String::as_str) {
                Some("cifs") => Some(Self::SmbIso),
                Some("nfs_iso") => Some(Self::NfsIso),
                _ => None,
            },
            _ => None,
        }
    }
}

/// What the New SR wizard asks for.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct NewSrSpec {
    pub kind: SrKind,
    pub name: String,
    pub description: String,
    /// `device_config`; may hold passwords (`cifspassword`, `password`,
    /// `chappassword`), never logged.
    pub device_config: Vec<(String, String)>,
    pub sm_config: Vec<(String, String)>,
    /// The UUID of an existing SR the probe found, to introduce
    /// (`SrIntroduceAction`) or, with `reattach`, to reattach.
    pub uuid: Option<String>,
    /// The detached SR of this pool to reattach (`SrReattachAction`).
    pub reattach: Option<Ref<Sr>>,
}

impl fmt::Debug for NewSrSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device_config: Vec<(&str, &str)> = self
            .device_config
            .iter()
            .map(|(k, v)| {
                let v = if is_secret_key(k) { "<redacted>" } else { v };
                (k.as_str(), v)
            })
            .collect();
        f.debug_struct("NewSrSpec")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("device_config", &device_config)
            .field("sm_config", &self.sm_config)
            .field("uuid", &self.uuid)
            .field("reattach", &self.reattach)
            .finish()
    }
}

/// A `device_config` key that holds a password.
pub fn is_secret_key(key: &str) -> bool {
    key.to_ascii_lowercase().contains("password")
}

/// A password that never shows in logs.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A `device_config` whose passwords never show in logs.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct DeviceConfig(pub Vec<(String, String)>);

impl fmt::Debug for DeviceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(k, v)| {
                let v = if is_secret_key(k) { "<redacted>" } else { v };
                (k, v)
            }))
            .finish()
    }
}

impl DeviceConfig {
    pub fn map(&self) -> HashMap<String, String> {
        self.0.iter().cloned().collect()
    }
}

/// What a [`NewSrSpec`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSrMode {
    /// `SR.create`.
    Create,
    /// `SR.introduce` of an SR found by the probe.
    Introduce,
    /// New PBDs for a detached SR of this pool.
    Reattach,
}

impl NewSrSpec {
    pub fn mode(&self) -> NewSrMode {
        match (&self.uuid, &self.reattach) {
            (_, Some(_)) => NewSrMode::Reattach,
            (Some(_), None) => NewSrMode::Introduce,
            (None, None) => NewSrMode::Create,
        }
    }

    pub fn device_config_map(&self) -> HashMap<String, String> {
        self.device_config.iter().cloned().collect()
    }

    pub fn sm_config_map(&self) -> HashMap<String, String> {
        self.sm_config.iter().cloned().collect()
    }
}

/// Whether a type of the wizard is offered: its SM plugin is installed and
/// not hidden (`ChooseSrTypePage.PopulatePage`).
pub fn kind_available(model: &ConnectionModel, kind: SrKind, show_hidden: bool) -> bool {
    model.sms.values().any(|sm| {
        sm.r#type == kind.sr_type()
            && (show_hidden
                || sm
                    .other_config
                    .get(objects::HIDE_FROM_XENCENTER)
                    .map(String::as_str)
                    != Some("true"))
    })
}

/// The default name of a new SR of `kind`, unique among the pool's SRs
/// (`ResetSrName`, `SrWizardHelpers.DefaultSRName`).
pub fn default_sr_name(model: &ConnectionModel, base: &str) -> String {
    let names: Vec<&str> = model.srs.values().map(|s| s.name_label.as_str()).collect();
    if !names.contains(&base) {
        return base.to_owned();
    }
    (1..)
        .map(|i| format!("{base} ({i})"))
        .find(|n| !names.contains(&n.as_str()))
        .expect("an unused name")
}

/// `server:/path` (`SrWizardHelpers.ValidateNfsSharename`).
pub fn valid_nfs_share(share: &str) -> bool {
    let Some((server, path)) = share.split_once(":/") else {
        return false;
    };
    let bad = |c: char| matches!(c, '*' | '?' | '<' | '>' | '|' | '\\');
    !server.is_empty()
        && !server.chars().any(|c| bad(c) || c == '/' || c == ':')
        && !path.chars().any(|c| bad(c) || c == ':')
}

/// `\\server\share[\path…]` (`SrWizardHelpers.ValidateCifsSharename`).
pub fn valid_cifs_share(share: &str) -> bool {
    let Some(rest) = share.strip_prefix('\\') else {
        return false;
    };
    // One or more `\name` parts, each optionally ending with a `\`.
    let parts: Vec<&str> = rest.split('\\').collect();
    if parts.len() < 2 || !parts[0].is_empty() {
        return false;
    }
    let names = &parts[1..];
    let bad = |c: char| matches!(c, '/' | ':' | '*' | '?' | '<' | '>' | '|');
    let last = names.len() - 1;
    names
        .iter()
        .enumerate()
        .all(|(i, n)| (!n.is_empty() || (i == last && last > 0)) && !n.chars().any(bad))
}

/// `device_config` of an NFS SR (the NFS page): `server`, `serverpath`,
/// `options` and the NFS version (`nfsversion` for 4 and 4.1).
pub fn nfs_device_config(share: &str, options: &str, version: &str) -> Vec<(String, String)> {
    let share = share.trim();
    let mut dc = vec![("options".to_owned(), options.to_owned())];
    let mut parts = share.split(':');
    dc.push((
        "server".to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    ));
    if let Some(path) = parts.next() {
        dc.push(("serverpath".to_owned(), path.to_owned()));
    }
    if matches!(version, "4" | "4.1") {
        dc.push(("nfsversion".to_owned(), version.to_owned()));
    }
    dc
}

/// `device_config` of the NFS probe (the Scan button).
pub fn nfs_probe_config(share: &str, options: &str) -> Vec<(String, String)> {
    let mut dc = nfs_device_config(share, options, "");
    dc.push(("probeversion".to_owned(), String::new()));
    dc
}

/// `device_config` of an NFS ISO library.
pub fn nfs_iso_device_config(share: &str, v4: bool) -> Vec<(String, String)> {
    let mut dc = vec![
        ("location".to_owned(), share.to_owned()),
        ("type".to_owned(), "nfs_iso".to_owned()),
    ];
    if v4 {
        dc.push(("nfsversion".to_owned(), "4".to_owned()));
    }
    dc
}

/// `device_config` of an SMB ISO library: `//server/share` and, for a
/// longer path, `iso_path`; optional credentials.
pub fn smb_iso_device_config(
    share: &str,
    credentials: Option<(&str, &str)>,
) -> Vec<(String, String)> {
    let location = share.replace('\\', "/");
    let bits: Vec<&str> = location.split('/').collect();
    let mut dc = Vec::new();
    if bits.len() > 4 {
        dc.push(("location".to_owned(), format!("//{}/{}", bits[2], bits[3])));
        dc.push(("iso_path".to_owned(), format!("/{}", bits[4..].join("/"))));
    } else {
        dc.push(("location".to_owned(), location.clone()));
    }
    dc.push(("type".to_owned(), "cifs".to_owned()));
    if let Some((user, password)) = credentials {
        dc.push(("username".to_owned(), user.to_owned()));
        dc.push(("cifspassword".to_owned(), password.to_owned()));
    }
    dc
}

/// `device_config` of an SMB SR (and its probe).
pub fn smb_device_config(share: &str, user: &str, password: &str) -> Vec<(String, String)> {
    let mut parts = share.split(':');
    let mut dc = vec![(
        "server".to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    )];
    if let Some(path) = parts.next() {
        dc.push(("serverpath".to_owned(), path.to_owned()));
    }
    if !user.trim().is_empty() || !password.trim().is_empty() {
        dc.push(("username".to_owned(), user.to_owned()));
        dc.push(("password".to_owned(), password.to_owned()));
    }
    dc
}

/// `device_config` of a local SR.
pub fn local_device_config(device: &str) -> Vec<(String, String)> {
    vec![("device".to_owned(), device.to_owned())]
}

/// `device_config` of an iSCSI SR on `lun` (`LVMoISCSI.GetDeviceConfig`).
pub fn iscsi_device_config(
    iqn: &IqnInfo,
    lun: &LunInfo,
    chap: Option<(&str, &str)>,
) -> Vec<(String, String)> {
    let mut dc = vec![
        ("target".to_owned(), iqn.address.clone()),
        ("port".to_owned(), iqn.port.to_string()),
        ("targetIQN".to_owned(), iqn.target_iqn.clone()),
    ];
    if lun.lun_id == -1 {
        dc.push(("LUNSerial".to_owned(), lun.serial.clone()));
    } else {
        dc.push(("SCSIid".to_owned(), lun.scsi_id.clone()));
    }
    if let Some((user, password)) = chap {
        dc.push(("chapuser".to_owned(), user.to_owned()));
        dc.push(("chappassword".to_owned(), password.to_owned()));
    }
    dc
}

/// The `device_config` of the iSCSI scans (`ISCSIPopulateIQNsAction`,
/// `ISCSIPopulateLunsAction`).
pub fn iscsi_scan_config(
    target: &str,
    port: u16,
    iqn: Option<&str>,
    chap: Option<(&str, &str)>,
) -> HashMap<String, String> {
    let mut dc = HashMap::from([
        ("target".to_owned(), target.to_owned()),
        ("port".to_owned(), port.to_string()),
    ]);
    if let Some(iqn) = iqn {
        dc.insert("targetIQN".to_owned(), iqn.to_owned());
    }
    if let Some((user, password)) = chap
        && !user.is_empty()
    {
        dc.insert("chapuser".to_owned(), user.to_owned());
        dc.insert("chappassword".to_owned(), password.to_owned());
    }
    dc
}

/// The SRs of other connections or this pool using the same LUN, by SCSI
/// id (`LVMoISCSI.SrUsingLunExists`).
pub fn sr_using_lun<'a>(
    model: &'a ConnectionModel,
    scsi_id: &str,
) -> Option<(&'a Ref<Sr>, &'a SrRecord)> {
    model
        .srs
        .iter()
        .filter(|(_, sr)| matches!(sr.r#type.as_str(), "lvmoiscsi" | "gfs2"))
        .find(|(_, sr)| {
            sr.pbds
                .first()
                .and_then(|p| model.get(p))
                .and_then(|p| p.device_config.get("SCSIid"))
                .is_some_and(|id| id == scsi_id)
        })
        .map(|(r, sr)| (r, &**sr))
}

/// The location strings of the ISO libraries of `iso_type` (`nfs_iso` or
/// `cifs`) already attached to this pool, as the share field spells them
/// (the ISO pages' "already attached" check).
pub fn attached_iso_locations(model: &ConnectionModel, iso_type: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sr in model.srs.values().filter(|s| s.r#type == "iso") {
        for pbd in sr.pbds.iter().filter_map(|p| model.get(p)) {
            if pbd.device_config.get("type").map(String::as_str) != Some(iso_type) {
                continue;
            }
            if let Some(location) = pbd.device_config.get("location") {
                let location = if iso_type == "cifs" {
                    location.replace('/', "\\")
                } else {
                    location.clone()
                };
                if !out.contains(&location) {
                    out.push(location);
                }
            }
        }
    }
    out
}

// ---- SR facts --------------------------------------------------------------

/// The SM plugin of an SR type, if installed.
pub fn sm_for<'a>(model: &'a ConnectionModel, sr_type: &str) -> Option<&'a xapi::SmRecord> {
    model
        .sms
        .values()
        .find(|sm| sm.r#type == sr_type)
        .map(|sm| &**sm)
}

/// `SR.HasPBDs`.
pub fn has_pbds(sr: &SrRecord) -> bool {
    !sr.pbds.is_empty()
}

/// `SR.HasRunningVMs`: a running VM uses one of its disks (a control domain
/// on a metadata disk does not count).
pub fn has_running_vms(model: &ConnectionModel, sr: &SrRecord) -> bool {
    sr.vdis.iter().filter_map(|v| model.get(v)).any(|vdi| {
        vdi.vbds.iter().filter_map(|b| model.get(b)).any(|vbd| {
            let Some(vm) = model.vms.get(&vbd.vm) else {
                return false;
            };
            if vdi.r#type == VdiType::Metadata && vm.is_control_domain {
                return false;
            }
            vm.power_state == VmPowerState::Running
        })
    })
}

/// `SR.CanCreateWithXenCenter`: the types this console makes (and so may
/// forget and destroy).
pub fn can_create_with_console(sr: &SrRecord) -> bool {
    matches!(
        sr.r#type.as_str(),
        "iso" | "lvmoiscsi" | "nfs" | "lvmohba" | "smb" | "lvmofcoe" | "gfs2"
    )
}

/// `SR.SupportsTrim`: the SM plugin has the `SR_TRIM` feature.
pub fn supports_trim(model: &ConnectionModel, sr: &SrRecord) -> bool {
    sm_for(model, &sr.r#type).is_some_and(|sm| sm.features.contains_key("SR_TRIM"))
}

/// `SR.SupportsVdiCreate` (ISO libraries never, as in the original).
pub fn supports_vdi_create(model: &ConnectionModel, sr: &SrRecord) -> bool {
    if sr.content_type == "iso" || sr.r#type == "tmpfs" {
        return false;
    }
    sm_for(model, &sr.r#type).is_some_and(|sm| sm.capabilities.iter().any(|c| c == "VDI_CREATE"))
}

/// `SR.SupportsReadCaching`: the SM plugin has `VDI_READ_CACHING`.
pub fn supports_read_caching(model: &ConnectionModel, sr: &SrRecord) -> bool {
    sm_for(model, &sr.r#type).is_some_and(|sm| sm.features.contains_key("VDI_READ_CACHING"))
}

/// `SR.GetReadCachingEnabled`: on unless `other_config["o_direct"]` is true.
pub fn read_caching_enabled(model: &ConnectionModel, sr: &SrRecord) -> bool {
    supports_read_caching(model, sr)
        && !sr
            .other_config
            .get("o_direct")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// `SR.GetFirstAttachedStorageHost`: the host of the first plugged PBD.
pub fn first_attached_host<'a>(model: &'a ConnectionModel, sr: &SrRecord) -> Option<&'a Ref<Host>> {
    sr.pbds
        .iter()
        .filter_map(|p| model.get(p))
        .filter(|p| p.currently_attached)
        .find_map(|p| model.hosts.get_key_value(&p.host).map(|(k, _)| k))
}

/// The pool's default SR is `sr`.
pub fn is_default_sr(model: &ConnectionModel, sr: &Ref<Sr>) -> bool {
    model.pool().is_some_and(|(_, p)| &p.default_sr == sr)
}

/// The PBD of `host` to `sr`, if any (`Host.GetPBDTo`).
pub fn pbd_of<'a>(
    model: &'a ConnectionModel,
    sr: &SrRecord,
    host: &Ref<Host>,
) -> Option<(&'a Ref<xapi::Pbd>, &'a PbdRecord)> {
    sr.pbds.iter().find_map(|p| {
        let (r, rec) = model.pbds.get_key_value(p)?;
        (&rec.host == host).then_some((r, &**rec))
    })
}

/// The hosts that should see `sr`: every host when it is shared, else its
/// storage host (`RepairSRDialog.Build`, `SrRepairAction`). The coordinator
/// comes first.
pub fn repair_hosts<'a>(model: &'a ConnectionModel, sr: &SrRecord) -> Vec<&'a Ref<Host>> {
    let home = objects::sr_home(model, sr);
    let mut hosts: Vec<&Ref<Host>> = objects::sorted_hosts(model)
        .into_iter()
        .map(|(r, _)| r)
        .filter(|r| sr.shared || Some(*r) == home)
        .collect();
    if let Some(pos) = hosts.iter().position(|h| objects::is_coordinator(model, h)) {
        let c = hosts.remove(pos);
        hosts.insert(0, c);
    }
    hosts
}

/// Whether this is the pool's first shared SR for disks: it becomes the
/// default SR once made (`isFirstSharedNonISOSR`).
pub fn first_shared_disk_sr(
    model: &ConnectionModel,
    kind: SrKind,
    except: Option<&Ref<Sr>>,
) -> bool {
    if kind.is_iso() || !kind.shared() {
        return false;
    }
    !model
        .srs
        .iter()
        .any(|(r, sr)| Some(r) != except && sr.r#type != "iso" && sr.shared)
}

// ---- when each SR operation is offered -------------------------------------

/// SR operations of the Storage menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrOp {
    Repair,
    SetDefault,
    Trim,
    Detach,
    Reattach,
    Forget,
    Destroy,
    Rescan,
    Properties,
}

/// The SR operations that run without a wizard or dialog of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SrAction {
    /// `SrRepairAction`: missing PBDs created, unplugged ones plugged.
    Repair,
    /// `DetachSrAction`: every PBD unplugged (the coordinator's last).
    Detach,
    /// `ForgetSrAction`.
    Forget,
    /// `DestroySrAction`.
    Destroy,
    /// `SetSrAsDefaultAction`: the pool's default, suspend and crash dump SR.
    SetDefault,
    /// `SrRefreshAction` (the SR Storage tab's Rescan): `SR.scan`.
    Rescan,
    /// `SrTrimAction`: the `trim` plugin on the first attached host.
    Trim,
}

impl SrAction {
    /// The menu operation it is.
    pub fn op(self) -> SrOp {
        match self {
            Self::Repair => SrOp::Repair,
            Self::Detach => SrOp::Detach,
            Self::Forget => SrOp::Forget,
            Self::Destroy => SrOp::Destroy,
            Self::SetDefault => SrOp::SetDefault,
            Self::Rescan => SrOp::Rescan,
            Self::Trim => SrOp::Trim,
        }
    }
}

/// What the SR Properties dialog changed (`None`: unchanged).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SrChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Read caching on (`other_config["o_direct"]` removed) or off (`"true"`).
    pub read_caching: Option<bool>,
}

impl SrChanges {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Whether `op` is offered for `sr`. `busy` means this client has an
/// operation running on it (`HelpersGUI.GetActionInProgress`).
pub fn sr_op_available(
    model: &ConnectionModel,
    sr_ref: &Ref<Sr>,
    sr: &SrRecord,
    op: SrOp,
    busy: bool,
) -> bool {
    let allowed = |o: StorageOperations| sr.allowed_operations.contains(&o);
    match op {
        SrOp::Repair => {
            has_pbds(sr)
                && (objects::sr_is_broken(model, sr) || !objects::sr_multipath_ok(model, sr))
                && !busy
        }
        SrOp::SetDefault => {
            has_pbds(sr)
                && !is_default_sr(model, sr_ref)
                && supports_vdi_create(model, sr)
                && (sr.shared || model.hosts.len() <= 1)
                && !busy
        }
        SrOp::Trim => supports_trim(model, sr) && first_attached_host(model, sr).is_some(),
        SrOp::Detach => !objects::sr_is_detached(model, sr) && !has_running_vms(model, sr) && !busy,
        SrOp::Reattach => {
            !has_pbds(sr)
                && !busy
                && sm_for(model, &sr.r#type).is_some()
                && SrKind::of_sr(sr).is_some()
        }
        SrOp::Forget => {
            !has_running_vms(model, sr)
                && can_create_with_console(sr)
                && allowed(StorageOperations::Forget)
                && !busy
        }
        SrOp::Destroy => {
            !has_running_vms(model, sr)
                && can_create_with_console(sr)
                && allowed(StorageOperations::Destroy)
                && !busy
        }
        SrOp::Rescan => !objects::sr_is_detached(model, sr) && !busy && !being_scanned(sr),
        SrOp::Properties => true,
    }
}

/// A scan of the SR is running (`HelpersGUI.BeingScanned`).
pub fn being_scanned(sr: &SrRecord) -> bool {
    sr.current_operations
        .values()
        .any(|o| *o == StorageOperations::Scan)
}

/// Why `op` is not offered, when the original says (its
/// `GetCantRunReasonCore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrOpProblem {
    Detached,
    RunningVms,
    NotCreatedByConsole,
    InProgress,
    TrimUnsupported,
    ScanInProgress,
}

/// The original's reason why `op` cannot run on `sr`, if it says one.
pub fn sr_op_problem(
    model: &ConnectionModel,
    sr: &SrRecord,
    op: SrOp,
    busy: bool,
) -> Option<SrOpProblem> {
    let detached = objects::sr_is_detached(model, sr);
    let running = has_running_vms(model, sr);
    match op {
        SrOp::Detach if detached => Some(SrOpProblem::Detached),
        SrOp::Destroy if !has_pbds(sr) => Some(SrOpProblem::Detached),
        SrOp::Detach | SrOp::Destroy | SrOp::Forget if running => Some(SrOpProblem::RunningVms),
        SrOp::Destroy | SrOp::Forget if !can_create_with_console(sr) => {
            Some(SrOpProblem::NotCreatedByConsole)
        }
        SrOp::Trim if !supports_trim(model, sr) => Some(SrOpProblem::TrimUnsupported),
        SrOp::Rescan if detached => Some(SrOpProblem::Detached),
        SrOp::Rescan if being_scanned(sr) => Some(SrOpProblem::ScanInProgress),
        _ if busy => Some(SrOpProblem::InProgress),
        _ => None,
    }
}

// ---- probe answers ---------------------------------------------------------

/// An SR a probe found (`SR.SRInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SrInfo {
    pub uuid: String,
    pub size: i64,
    pub aggregate: String,
    pub name_label: String,
    pub name_description: String,
    pub pool_metadata_detected: bool,
}

/// What `SR.probe` answered: the SRs found and, for NFS, the versions the
/// server supports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeResult {
    pub srs: Vec<SrInfo>,
    pub nfs_versions: Vec<String>,
}

/// A target IQN (`IScsiIqnInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IqnInfo {
    pub index: i64,
    pub target_iqn: String,
    pub address: String,
    pub port: u16,
}

impl IqnInfo {
    /// `"{iqn} ({address}:{port})"`, as the IQN list shows it.
    pub fn label(&self) -> String {
        format!("{} ({}:{})", self.target_iqn, self.address, self.port)
    }
}

/// A LUN of a target (`ISCSIInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LunInfo {
    pub scsi_id: String,
    /// -1 when the server did not say.
    pub lun_id: i64,
    pub vendor: String,
    pub serial: String,
    /// Bytes; -1 when the server did not say.
    pub size: i64,
}

/// An XML element with its text and children (enough for the probe
/// answers, which are small).
#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

impl Node {
    /// Every descendant (and self) named `name`, case-insensitively.
    fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        if self.name.eq_ignore_ascii_case(name) {
            out.push(self);
        }
        for c in &self.children {
            c.find_all(name, out);
        }
    }

    fn all(&self, name: &str) -> Vec<&Node> {
        let mut out = Vec::new();
        self.find_all(name, &mut out);
        out
    }

    fn child_text(&self, name: &str) -> Option<&str> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .map(|c| c.text.trim())
    }
}

/// Parses `xml` into a tree under a synthetic root; `None` if it is not
/// well-formed.
fn parse_tree(xml: &str) -> Option<Node> {
    let mut reader = Reader::from_str(xml);
    let mut stack = vec![Node::default()];
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => stack.push(Node {
                name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                ..Node::default()
            }),
            Event::Empty(e) => {
                let node = Node {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Node::default()
                };
                stack.last_mut()?.children.push(node);
            }
            Event::End(_) => {
                let node = stack.pop()?;
                stack.last_mut()?.children.push(node);
                if stack.is_empty() {
                    return None;
                }
            }
            Event::Text(t) => {
                let text = t.decode().ok()?;
                stack.last_mut()?.text.push_str(&text);
            }
            Event::CData(t) => {
                let text = String::from_utf8_lossy(&t).into_owned();
                stack.last_mut()?.text.push_str(&text);
            }
            Event::GeneralRef(r) => {
                let resolved = if let Ok(Some(c)) = r.resolve_char_ref() {
                    c.to_string()
                } else {
                    let name = r.decode().ok()?;
                    quick_xml::escape::resolve_predefined_entity(&name)?.to_owned()
                };
                stack.last_mut()?.text.push_str(&resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (stack.len() == 1).then(|| stack.pop()).flatten()
}

/// The text of a task's `result` holding a string: XAPI writes it as
/// `<value>…</value>` with the XML inside escaped (`AsyncAction` reads the
/// `value` element's inner text).
pub fn task_string_result(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.starts_with("<value")
        && let Some(root) = parse_tree(trimmed)
        && let Some(v) = root.all("value").first()
    {
        return v.text.clone();
    }
    trimmed.to_owned()
}

/// `SR.ParseSRListXML` and `ParseSupportedVersionsListXML` on one answer.
pub fn parse_probe(xml: &str) -> Option<ProbeResult> {
    let xml = task_string_result(xml);
    if xml.trim().is_empty() {
        return Some(ProbeResult::default());
    }
    let root = parse_tree(&xml)?;
    let srs = root
        .all("SR")
        .into_iter()
        .map(|n| SrInfo {
            uuid: n.child_text("uuid").unwrap_or_default().to_owned(),
            size: n
                .child_text("size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            aggregate: n.child_text("aggregate").unwrap_or_default().to_owned(),
            name_label: n.child_text("name_label").unwrap_or_default().to_owned(),
            name_description: n
                .child_text("name_description")
                .unwrap_or_default()
                .to_owned(),
            pool_metadata_detected: n
                .child_text("pool_metadata_detected")
                .is_some_and(|v| v.eq_ignore_ascii_case("true")),
        })
        .collect();
    let nfs_versions = root
        .all("SupportedVersions")
        .into_iter()
        .flat_map(|n| n.all("Version"))
        .map(|v| v.text.trim().to_owned())
        .collect();
    Some(ProbeResult { srs, nfs_versions })
}

/// `ParseIPAddress`: an IPv6 address gets brackets.
fn iscsi_address(address: &str) -> String {
    let trimmed = address.trim();
    match trimmed.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
        Ok(ip) => ip.to_string(),
        Err(_) => trimmed.to_owned(),
    }
}

/// The IQN list in the fourth parameter of `SR_BACKEND_FAILURE_96`
/// (`<iscsi-target-iqns><TGT>…</TGT></iscsi-target-iqns>`), sorted by IQN
/// with `*` last (CA-40066).
pub fn parse_iqns(xml: &str) -> Option<Vec<IqnInfo>> {
    let root = parse_tree(xml)?;
    let mut out = Vec::new();
    for list in root.all("iscsi-target-iqns") {
        for target in &list.children {
            let mut info = IqnInfo {
                index: -1,
                target_iqn: String::new(),
                address: String::new(),
                port: ISCSI_PORT,
            };
            for n in &target.children {
                match n.name.to_ascii_lowercase().as_str() {
                    "index" => info.index = n.text.trim().parse().ok()?,
                    "ipaddress" => info.address = iscsi_address(&n.text),
                    "port" => info.port = n.text.trim().parse().ok()?,
                    "targetiqn" => info.target_iqn = n.text.trim().to_owned(),
                    _ => {}
                }
            }
            out.push(info);
        }
    }
    out.sort_by(|a, b| {
        (a.target_iqn == "*")
            .cmp(&(b.target_iqn == "*"))
            .then_with(|| objects::natural_cmp(&a.target_iqn, &b.target_iqn))
    });
    Some(out)
}

/// The LUN list in the fourth parameter of `SR_BACKEND_FAILURE_107` / `_87`
/// (`<iscsi-target><LUN>…</LUN></iscsi-target>`), sorted by LUN id.
pub fn parse_luns(xml: &str) -> Option<Vec<LunInfo>> {
    let root = parse_tree(xml)?;
    let mut out = Vec::new();
    for target in root.all("iscsi-target") {
        for lun in &target.children {
            let mut info = LunInfo {
                scsi_id: String::new(),
                lun_id: -1,
                vendor: String::new(),
                serial: String::new(),
                size: -1,
            };
            for n in &lun.children {
                let text = n.text.trim();
                match n.name.to_ascii_lowercase().as_str() {
                    "lunid" => info.lun_id = text.parse().ok()?,
                    "vendor" => info.vendor = text.to_owned(),
                    "serial" => info.serial = text.to_owned(),
                    "size" => info.size = text.parse().unwrap_or(-1),
                    "scsiid" => info.scsi_id = text.to_owned(),
                    _ => {}
                }
            }
            out.push(info);
        }
    }
    out.sort_by_key(|l| l.lun_id);
    Some(out)
}

/// The LUN list's label (`"LUN 0: serial: 10 GB (vendor)"`).
pub fn lun_label(lun: &LunInfo) -> String {
    let mut label = "LUN".to_owned();
    if lun.lun_id != -1 {
        label.push_str(&format!(" {}", lun.lun_id));
    }
    if !lun.serial.is_empty() {
        label.push_str(&format!(": {}", lun.serial));
    }
    if lun.size >= 0 {
        label.push_str(&format!(": {}", crate::format::disk_size(lun.size)));
    }
    if !lun.vendor.is_empty() {
        label.push_str(&format!(" ({})", lun.vendor));
    }
    label
}

/// The trim plugin's error (`<trim_response>` key/value pairs): `errcode`
/// and `errmsg`, when the answer is not `"True"`.
pub fn parse_trim_error(xml: &str) -> Option<(String, String)> {
    let root = parse_tree(xml)?;
    let mut code = String::new();
    let mut message = String::new();
    for pair in root.all("key_value_pair") {
        let key = pair.child_text("key").unwrap_or_default();
        let value = pair.child_text("value").unwrap_or_default().to_owned();
        match key {
            "errcode" => code = value,
            "errmsg" => message = value,
            _ => {}
        }
    }
    (!code.is_empty() || !message.is_empty()).then_some((code, message))
}

/// What an iSCSI scan answers.
pub type IscsiScan<T> = Result<Vec<T>, std::sync::Arc<xapi::Error>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ConnectionModel {
        ConnectionModel::from_database(
            &serde_json::from_str(include_str!("../../../tests/fixtures/records.json")).unwrap(),
        )
    }

    #[test]
    fn share_names() {
        assert!(valid_nfs_share("nfs.lab:/exports/vm"));
        assert!(valid_nfs_share("10.0.0.5:/a/b c"));
        assert!(!valid_nfs_share("nfs.lab/exports"));
        assert!(!valid_nfs_share(":/exports"));
        assert!(!valid_nfs_share("nfs.lab:exports"));
        assert!(valid_cifs_share(r"\\smb.lab\vms"));
        assert!(valid_cifs_share(r"\\smb.lab\iso\linux"));
        assert!(valid_cifs_share(r"\\smb.lab\iso\"));
        // The original's pattern takes a bare server too.
        assert!(valid_cifs_share(r"\\smb.lab"));
        assert!(!valid_cifs_share(r"\\"));
        assert!(!valid_cifs_share(r"\smb.lab\vms"));
        assert!(!valid_cifs_share("//smb.lab/vms"));
        assert!(!valid_cifs_share(r"\\smb:lab\vms"));
    }

    #[test]
    fn device_configs() {
        let dc = nfs_device_config("nfs.lab:/exports/vm", "", "4.1");
        assert_eq!(
            dc,
            [
                ("options".into(), "".into()),
                ("server".into(), "nfs.lab".into()),
                ("serverpath".into(), "/exports/vm".into()),
                ("nfsversion".into(), "4.1".into())
            ]
        );
        assert!(
            !nfs_device_config("a:/b", "", "3")
                .iter()
                .any(|(k, _)| k == "nfsversion")
        );
        let dc = smb_iso_device_config(r"\\smb.lab\iso\linux\debian", Some(("u", "p")));
        let get = |k: &str| dc.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
        assert_eq!(get("location"), Some("//smb.lab/iso"));
        assert_eq!(get("iso_path"), Some("/linux/debian"));
        assert_eq!(get("cifspassword"), Some("p"));
        let dc = smb_iso_device_config(r"\\smb.lab\iso", None);
        assert_eq!(dc[0].1, "//smb.lab/iso");
        assert_eq!(dc.len(), 2);
        let dc = smb_device_config(r"\\smb.lab\vms", "", "");
        assert_eq!(dc, [("server".into(), r"\\smb.lab\vms".into())]);
    }

    #[test]
    fn probe_answers() {
        let xml = "<value>&lt;?xml version=\"1.0\" ?&gt;&lt;SRlist&gt;&lt;SR&gt;&lt;UUID&gt;aaaa&lt;/UUID&gt;\
                   &lt;size&gt;100&lt;/size&gt;&lt;/SR&gt;&lt;SupportedVersions&gt;&lt;Version&gt;3&lt;/Version&gt;\
                   &lt;Version&gt;4.1&lt;/Version&gt;&lt;/SupportedVersions&gt;&lt;/SRlist&gt;</value>";
        let r = parse_probe(xml).unwrap();
        assert_eq!(r.srs.len(), 1);
        assert_eq!((r.srs[0].uuid.as_str(), r.srs[0].size), ("aaaa", 100));
        assert_eq!(r.nfs_versions, ["3", "4.1"]);
        assert_eq!(parse_probe("").unwrap(), ProbeResult::default());
        assert!(parse_probe("<SRlist><SR>").is_none());

        let iqns = parse_iqns(
            "<?xml version=\"1.0\" ?><iscsi-target-iqns>\
             <TGT><Index>1</Index><IPAddress>10.0.0.9</IPAddress><TargetIQN>*</TargetIQN></TGT>\
             <TGT><Index>0</Index><IPAddress>fe80::1</IPAddress><Port>3261</Port><TargetIQN>iqn.2026-01.lab:b</TargetIQN></TGT>\
             <TGT><Index>2</Index><IPAddress>10.0.0.9</IPAddress><TargetIQN>iqn.2026-01.lab:a</TargetIQN></TGT>\
             </iscsi-target-iqns>",
        )
        .unwrap();
        let names: Vec<_> = iqns.iter().map(|i| i.target_iqn.as_str()).collect();
        assert_eq!(names, ["iqn.2026-01.lab:a", "iqn.2026-01.lab:b", "*"]);
        assert_eq!(iqns[1].address, "[fe80::1]");
        assert_eq!(iqns[1].port, 3261);
        assert_eq!(iqns[0].port, ISCSI_PORT);
        assert_eq!(iqns[0].label(), "iqn.2026-01.lab:a (10.0.0.9:3260)");

        let luns = parse_luns(
            "<iscsi-target><LUN><vendor>LIO</vendor><serial>s1</serial><LUNid>1</LUNid>\
             <size>10737418240</size><SCSIid>36001</SCSIid></LUN>\
             <LUN><LUNid>0</LUNid><SCSIid>36000</SCSIid></LUN></iscsi-target>",
        )
        .unwrap();
        assert_eq!(luns[0].scsi_id, "36000");
        assert_eq!(lun_label(&luns[0]), "LUN 0");
        assert_eq!(lun_label(&luns[1]), "LUN 1: s1: 10 GB (LIO)");

        let trim = "<trim_response><key_value_pair><key>errcode</key><value>UnsupportedSRForTrim</value></key_value_pair>\
                    <key_value_pair><key>errmsg</key><value>Trim on [x] not supported</value></key_value_pair></trim_response>";
        assert_eq!(
            parse_trim_error(trim),
            Some((
                "UnsupportedSRForTrim".into(),
                "Trim on [x] not supported".into()
            ))
        );
        assert_eq!(parse_trim_error("True"), None);
    }

    #[test]
    fn spec_debug_hides_passwords() {
        let spec = NewSrSpec {
            kind: SrKind::Smb,
            name: "s".into(),
            description: String::new(),
            device_config: smb_device_config(r"\\x\y", "u", "hunter2"),
            sm_config: Vec::new(),
            uuid: None,
            reattach: None,
        };
        let text = format!("{spec:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<redacted>"));
        assert_eq!(spec.mode(), NewSrMode::Create);
    }

    #[test]
    fn operations_follow_the_original_rules() {
        let m = model();
        let get = |r: &str| {
            let r = Ref::<Sr>::new(r);
            let sr = m.srs[&r].clone();
            (r, sr)
        };
        // The NFS SR is the default SR and has web-01's running disk.
        let (nfs_ref, nfs) = get("OpaqueRef:sr-nfs");
        assert!(has_running_vms(&m, &nfs));
        assert!(!sr_op_available(&m, &nfs_ref, &nfs, SrOp::Detach, false));
        assert!(!sr_op_available(
            &m,
            &nfs_ref,
            &nfs,
            SrOp::SetDefault,
            false
        ));
        assert_eq!(
            sr_op_problem(&m, &nfs, SrOp::Forget, false),
            Some(SrOpProblem::RunningVms)
        );
        // The ISO library is broken (unplugged on host 2): Repair.
        let (iso_ref, iso) = get("OpaqueRef:sr-iso");
        assert!(sr_op_available(&m, &iso_ref, &iso, SrOp::Repair, false));
        assert!(!sr_op_available(&m, &iso_ref, &iso, SrOp::Repair, true));
        assert!(!sr_op_available(
            &m,
            &iso_ref,
            &iso,
            SrOp::SetDefault,
            false
        ));
        // web-01 runs with an ISO of it in its DVD drive.
        assert!(!sr_op_available(&m, &iso_ref, &iso, SrOp::Detach, false));
        assert!(!sr_op_available(&m, &iso_ref, &iso, SrOp::Reattach, false));
        // Local storage is never forgotten or destroyed by the console.
        let (local_ref, local) = get("OpaqueRef:sr-local-01");
        assert!(!sr_op_available(
            &m,
            &local_ref,
            &local,
            SrOp::Destroy,
            false
        ));
        assert_eq!(
            sr_op_problem(&m, &local, SrOp::Destroy, false),
            Some(SrOpProblem::RunningVms)
        );
        // Trim needs SR_TRIM (lvm has it) and a plugged PBD.
        assert!(sr_op_available(&m, &local_ref, &local, SrOp::Trim, false));
        let (ext_ref, ext) = get("OpaqueRef:sr-local-02");
        assert!(!sr_op_available(&m, &ext_ref, &ext, SrOp::Trim, false));
        assert!(sr_op_available(&m, &ext_ref, &ext, SrOp::Detach, false));
        assert!(sr_op_available(&m, &ext_ref, &ext, SrOp::Rescan, false));
        assert!(!sr_op_available(&m, &ext_ref, &ext, SrOp::Rescan, true));
        assert_eq!(
            sr_op_problem(&m, &ext, SrOp::Trim, false),
            Some(SrOpProblem::TrimUnsupported)
        );
        assert!(!first_shared_disk_sr(&m, SrKind::Nfs, None));
        assert!(!first_shared_disk_sr(&m, SrKind::NfsIso, None));
        assert_eq!(default_sr_name(&m, "NFS VM storage"), "NFS VM storage (1)");
        assert_eq!(default_sr_name(&m, "SMB ISO library"), "SMB ISO library");
        for kind in SrKind::ALL {
            assert!(kind_available(&m, kind, false), "{kind:?}");
        }
        assert_eq!(SrKind::of_sr(&iso), Some(SrKind::NfsIso));
        let mut plain = (*iso).clone();
        plain.sm_config.clear();
        assert_eq!(SrKind::of_sr(&plain), None);
        let hosts = repair_hosts(&m, &iso);
        assert_eq!(hosts.len(), 2);
        assert!(objects::is_coordinator(&m, hosts[0]));
        assert_eq!(repair_hosts(&m, &ext).len(), 1);
    }
}
