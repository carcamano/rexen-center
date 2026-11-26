//! OVF descriptors (F11b): the parts of an OVF envelope this console
//! reads and writes, as the original's `XenOvfApi`.
//!
//! * [`Envelope::parse`] reads a descriptor with `roxmltree`, matching
//!   elements and attributes by local name (OVF 1.x and 2.x, the original's
//!   `xenovf` extensions, VMware's and VirtualBox's descriptors). OVF 2.0
//!   `StorageItem` and `EthernetPortItem` are read as items.
//! * [`export_descriptor`] writes one with `quick-xml`, the way the original
//!   exports (`OVF.Merge` / `FinalizeEnvelope`): `References`,
//!   `DiskSection`, `NetworkSection`, `StartupSection`, then one
//!   `VirtualSystem` (or a `VirtualSystemCollection` of several) with its
//!   `OperatingSystemSection`, `VirtualHardwareSection` (CPU type 3, memory
//!   type 4, NIC type 10, disk type 19, CD type 16, and the
//!   `xenovf:VirtualSystemOtherConfigurationData` entries) and EULAs.
//! * [`validate`] is the original's `OVF.Validate` with its default flags:
//!   the hard failures and the warnings.
//! * [`vm_name`], [`VmHardware`] and the disk and interface rows are what
//!   the import wizard and the import action read from a virtual system.

use std::collections::HashMap;

/// `ovf:` (DMTF OVF envelope 1).
pub const NS_OVF: &str = "http://schemas.dmtf.org/ovf/envelope/1";
pub const NS_RASD: &str =
    "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData";
pub const NS_VSSD: &str =
    "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData";
pub const NS_XENOVF: &str = "http://schemas.citrix.com/ovf/envelope/1";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const NS_CIM: &str = "http://schemas.dmtf.org/wbem/wscim/1/common";

/// `Disk@ovf:format` of a VHD.
pub const VHD_FORMAT: &str =
    "http://www.microsoft.com/technet/virtualserver/downloads/vhdspec.mspx";

/// Resource types (`rasd:ResourceType`).
pub mod resource {
    pub const CPU: u32 = 3;
    pub const MEMORY: u32 = 4;
    pub const IDE: u32 = 5;
    pub const SCSI: u32 = 6;
    pub const NIC: u32 = 10;
    pub const CD_DRIVE: u32 = 15;
    pub const DVD_DRIVE: u32 = 16;
    pub const DISK_DRIVE: u32 = 17;
    pub const STORAGE_EXTENT: u32 = 19;
    pub const OTHER_STORAGE: u32 = 20;
    pub const LOGICAL_DISK: u32 = 21;
}

/// A file of the package (`References/File`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FileRef {
    pub id: String,
    pub href: String,
    pub size: Option<u64>,
    /// `gzip` (any case), else none.
    pub compression: Option<String>,
}

/// A virtual disk (`DiskSection/Disk`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DiskDef {
    pub disk_id: String,
    pub file_ref: String,
    /// In bytes (the allocation units applied).
    pub capacity: u64,
    pub format: String,
    pub bootable: Option<bool>,
}

/// A network the package names (`NetworkSection/Network`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct NetworkDef {
    pub name: String,
    pub description: String,
}

/// A virtual system's start order (`StartupSection/Item`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StartupItem {
    pub id: String,
    pub order: i64,
    pub start_delay: i64,
    pub stop_delay: i64,
}

/// One resource of a virtual system (`Item`: a CIM RASD).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Rasd {
    pub resource_type: u32,
    pub instance_id: String,
    pub element_name: String,
    pub caption: String,
    pub description: String,
    pub allocation_units: String,
    pub resource_sub_type: String,
    pub virtual_quantity: Option<u64>,
    pub limit: Option<u64>,
    pub connection: Vec<String>,
    pub host_resource: Vec<String>,
    pub address: String,
    pub address_on_parent: String,
    pub parent: String,
    /// `ovf:required` (default true).
    pub required: bool,
}

impl Rasd {
    fn new(resource_type: u32) -> Self {
        Self {
            resource_type,
            required: true,
            ..Default::default()
        }
    }

    /// `key=value` from the item's connections (`device=0`, `network=…`).
    pub fn connection_value(&self, key: &str) -> Option<String> {
        self.connection.iter().find_map(|c| {
            c.split(',').find_map(|part| {
                let (k, v) = part.split_once('=')?;
                (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_owned())
            })
        })
    }

    /// `IS_DISK_RASD_FILTER`: a disk item, not a COM port, floppy, ISO or
    /// display.
    pub fn is_disk(&self) -> bool {
        use resource::*;
        matches!(
            self.resource_type,
            DISK_DRIVE | STORAGE_EXTENT | OTHER_STORAGE | LOGICAL_DISK
        ) && !["COM", "FLOPPY", "ISO", "DISPLAY"]
            .iter()
            .any(|w| self.caption.to_uppercase().contains(w))
    }

    pub fn is_cd(&self) -> bool {
        matches!(self.resource_type, resource::CD_DRIVE | resource::DVD_DRIVE)
    }
}

/// `VirtualSystemSettingData` (the `System` element).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Vssd {
    pub element_name: String,
    pub caption: String,
    pub description: String,
    pub instance_id: String,
    pub identifier: String,
    /// `VirtualSystemType` (may hold several, space separated).
    pub system_type: String,
}

/// `VirtualHardwareSection`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Hardware {
    pub info: String,
    pub vssd: Vssd,
    pub items: Vec<Rasd>,
    /// `xenovf:VirtualSystemOtherConfigurationData`: name, value, info.
    pub other_config: Vec<(String, String, String)>,
}

/// `VirtualSystem`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct VirtualSystem {
    pub id: String,
    pub info: String,
    pub name: String,
    pub os_id: Option<u32>,
    pub os_info: String,
    pub os_description: String,
    pub hardware: Vec<Hardware>,
    pub eulas: Vec<String>,
}

/// An OVF envelope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Envelope {
    /// `xenovf:Name` (the package name the original writes).
    pub name: String,
    /// `ovf:version` / `xenovf:version`, when given.
    pub version: Option<String>,
    pub files: Vec<FileRef>,
    pub disks: Vec<DiskDef>,
    pub networks: Vec<NetworkDef>,
    pub startup: Vec<StartupItem>,
    pub systems: Vec<VirtualSystem>,
    /// EULAs outside the virtual systems (the collection's).
    pub eulas: Vec<String>,
    /// A `SecuritySection`: the package is encrypted.
    pub encrypted: bool,
}

/// Why a descriptor could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvfError(pub String);

impl std::fmt::Display for OvfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OvfError {}

// ---- reading ------------------------------------------------------------------------------

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn children<'a, 'i>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &'static str) -> Option<Node<'a, 'i>> {
    children(n, name).next()
}

fn text(n: Option<Node<'_, '_>>) -> String {
    n.map(|n| {
        n.descendants()
            .filter(|d| d.is_text())
            .filter_map(|d| d.text())
            .collect::<String>()
            .trim()
            .to_owned()
    })
    .unwrap_or_default()
}

/// An attribute by local name, whatever its namespace.
fn attr(n: Node<'_, '_>, name: &str) -> Option<String> {
    n.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value().to_owned())
}

/// `byte * 2^20`, `MegaBytes`, `GB`… as a multiplier (`None`: not
/// understood).
pub fn allocation_units(units: &str) -> Option<u64> {
    let u = units.trim();
    if u.is_empty() {
        return None;
    }
    let lower = u.to_lowercase().replace(' ', "");
    if let Some(rest) = lower.strip_prefix("byte") {
        if rest.is_empty() {
            return Some(1);
        }
        // byte*2^20, byte*1024^2, byte*10^6.
        let rest = rest.strip_prefix('*')?;
        let mut v = 1u64;
        for factor in rest.split('*') {
            let f = match factor.split_once('^') {
                Some((b, p)) => b.parse::<u64>().ok()?.checked_pow(p.parse().ok()?)?,
                None => factor.parse().ok()?,
            };
            v = v.checked_mul(f)?;
        }
        return Some(v);
    }
    Some(match lower.as_str() {
        "kb" | "kilobytes" | "kilobyte" => 1 << 10,
        "mb" | "megabytes" | "megabyte" => 1 << 20,
        "gb" | "gigabytes" | "gigabyte" => 1 << 30,
        "tb" | "terabytes" | "terabyte" => 1 << 40,
        _ => return None,
    })
}

fn parse_rasd(n: Node<'_, '_>) -> Rasd {
    let field = |name: &'static str| text(child(n, name));
    let all = |name: &'static str| -> Vec<String> {
        children(n, name)
            .map(|c| text(Some(c)))
            .filter(|t| !t.is_empty())
            .collect()
    };
    let number = |name: &'static str| field(name).parse::<u64>().ok();
    Rasd {
        resource_type: field("ResourceType").parse().unwrap_or(0),
        instance_id: field("InstanceID"),
        element_name: field("ElementName"),
        caption: field("Caption"),
        description: field("Description"),
        allocation_units: field("AllocationUnits"),
        resource_sub_type: field("ResourceSubType"),
        virtual_quantity: number("VirtualQuantity"),
        limit: number("Limit"),
        connection: all("Connection"),
        host_resource: all("HostResource"),
        address: field("Address"),
        address_on_parent: field("AddressOnParent"),
        parent: field("Parent"),
        required: attr(n, "required").is_none_or(|v| v != "false" && v != "0"),
    }
}

fn parse_hardware(n: Node<'_, '_>) -> Hardware {
    let mut hw = Hardware {
        info: text(child(n, "Info")),
        ..Default::default()
    };
    if let Some(s) = child(n, "System") {
        let f = |name: &'static str| text(child(s, name));
        hw.vssd = Vssd {
            element_name: f("ElementName"),
            caption: f("Caption"),
            description: f("Description"),
            instance_id: f("InstanceID"),
            identifier: f("VirtualSystemIdentifier"),
            system_type: f("VirtualSystemType"),
        };
    }
    for c in n.children().filter(Node::is_element) {
        match c.tag_name().name() {
            "Item" | "StorageItem" | "EthernetPortItem" => hw.items.push(parse_rasd(c)),
            "VirtualSystemOtherConfigurationData" => {
                let name = attr(c, "Name").unwrap_or_default();
                let value = match child(c, "Value") {
                    Some(v) => text(Some(v)),
                    None => text(Some(c)),
                };
                hw.other_config.push((name, value, text(child(c, "Info"))));
            }
            _ => {}
        }
    }
    hw
}

fn parse_system(n: Node<'_, '_>) -> VirtualSystem {
    let os = child(n, "OperatingSystemSection");
    VirtualSystem {
        id: attr(n, "id").unwrap_or_default(),
        info: text(child(n, "Info")),
        name: text(child(n, "Name")),
        os_id: os.and_then(|o| attr(o, "id")).and_then(|i| i.parse().ok()),
        os_info: text(os.and_then(|o| child(o, "Info"))),
        os_description: text(os.and_then(|o| child(o, "Description"))),
        hardware: children(n, "VirtualHardwareSection")
            .map(parse_hardware)
            .collect(),
        eulas: children(n, "EulaSection")
            .map(|e| text(child(e, "License")))
            .collect(),
    }
}

fn collect_systems(n: Node<'_, '_>, env: &mut Envelope) {
    for c in n.children().filter(Node::is_element) {
        match c.tag_name().name() {
            "VirtualSystem" => env.systems.push(parse_system(c)),
            "VirtualSystemCollection" => {
                env.eulas
                    .extend(children(c, "EulaSection").map(|e| text(child(e, "License"))));
                collect_systems(c, env);
            }
            _ => {}
        }
    }
}

impl Envelope {
    /// Reads a descriptor.
    pub fn parse(xml: &str) -> Result<Self, OvfError> {
        let doc = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                ..Default::default()
            },
        )
        .map_err(|e| OvfError(e.to_string()))?;
        let root = doc.root_element();
        if root.tag_name().name() != "Envelope" {
            return Err(OvfError("the root element is not an OVF Envelope".into()));
        }
        let mut env = Envelope {
            name: attr(root, "Name").unwrap_or_default(),
            version: attr(root, "version"),
            ..Default::default()
        };
        if let Some(r) = child(root, "References") {
            env.files = children(r, "File")
                .map(|f| FileRef {
                    id: attr(f, "id").unwrap_or_default(),
                    href: attr(f, "href").unwrap_or_default(),
                    size: attr(f, "size").and_then(|s| s.parse().ok()),
                    compression: attr(f, "compression")
                        .filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("identity")),
                })
                .collect();
        }
        for section in root.descendants().filter(Node::is_element) {
            match section.tag_name().name() {
                "DiskSection" => {
                    for d in children(section, "Disk") {
                        let units = attr(d, "capacityAllocationUnits")
                            .and_then(|u| allocation_units(&u))
                            .unwrap_or(1);
                        env.disks.push(DiskDef {
                            disk_id: attr(d, "diskId").unwrap_or_default(),
                            file_ref: attr(d, "fileRef").unwrap_or_default(),
                            capacity: attr(d, "capacity")
                                .and_then(|c| c.parse::<u64>().ok())
                                .unwrap_or(0)
                                .saturating_mul(units),
                            format: attr(d, "format").unwrap_or_default(),
                            bootable: attr(d, "isBootable").map(|b| b == "true" || b == "1"),
                        });
                    }
                }
                "NetworkSection" => {
                    for net in children(section, "Network") {
                        env.networks.push(NetworkDef {
                            name: attr(net, "name").unwrap_or_default(),
                            description: text(child(net, "Description")),
                        });
                    }
                }
                "StartupSection" => {
                    for i in children(section, "Item") {
                        let n = |k: &str| attr(i, k).and_then(|v| v.parse().ok()).unwrap_or(0);
                        env.startup.push(StartupItem {
                            id: attr(i, "id").unwrap_or_default(),
                            order: n("order"),
                            start_delay: n("startDelay"),
                            stop_delay: n("stopDelay"),
                        });
                    }
                }
                "SecuritySection" => env.encrypted = true,
                _ => {}
            }
        }
        collect_systems(root, &mut env);
        Ok(env)
    }

    pub fn system(&self, id: &str) -> Option<&VirtualSystem> {
        self.systems.iter().find(|s| s.id == id)
    }

    /// `HasEula`.
    pub fn all_eulas(&self) -> Vec<&str> {
        self.eulas
            .iter()
            .chain(self.systems.iter().flat_map(|s| s.eulas.iter()))
            .map(String::as_str)
            .filter(|e| !e.is_empty())
            .collect()
    }

    /// `FindRasdFileName`: the disk and file an item uses (substring
    /// matches, as the original).
    pub fn disk_of(&self, rasd: &Rasd) -> Option<(&DiskDef, &FileRef)> {
        let reference = rasd
            .host_resource
            .first()
            .or(Some(&rasd.instance_id).filter(|i| !i.is_empty()))
            .or(rasd.connection.first())?;
        let disk = self
            .disks
            .iter()
            .filter(|d| !d.disk_id.is_empty())
            .find(|d| reference.contains(&d.disk_id))?;
        let file = self
            .files
            .iter()
            .find(|f| !disk.file_ref.is_empty() && f.id.contains(&disk.file_ref))?;
        Some((disk, file))
    }
}

// ---- the virtual systems --------------------------------------------------------------------

/// `FindVirtualHardwareSectionByAffinity(…, "xen")`: the section for Xen,
/// else HVM, else the 301 (Microsoft), else VMware, else the first.
pub fn hardware(system: &VirtualSystem) -> Option<&Hardware> {
    let kind = |h: &Hardware| h.vssd.system_type.to_lowercase();
    let pick = |f: &dyn Fn(&str) -> bool| system.hardware.iter().find(|h| f(&kind(h)));
    pick(&|t| t.split_whitespace().any(|t| t.starts_with("xen")))
        .or_else(|| {
            pick(&|t| {
                t.split_whitespace()
                    .any(|t| t.starts_with("hvm") || t.ends_with("xen:hvm"))
            })
        })
        .or_else(|| pick(&|t| t.split_whitespace().any(|t| t == "301")))
        .or_else(|| pick(&|t| t.split_whitespace().any(|t| t.starts_with("vmx"))))
        .or_else(|| system.hardware.first())
}

fn is_guid(s: &str) -> bool {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    s.trim_matches(|c| c == '{' || c == '}').len() >= 32
        && hex
            .trim_matches(|c| c == '{' || c == '}')
            .chars()
            .all(|c| c.is_ascii_hexdigit())
        && hex.trim_matches(|c| c == '{' || c == '}').len() == 32
}

/// `ImportWizard.FindVMName`: the first name that is not a GUID among the
/// system's name, its id, its VSSD identifier, its OS description, the
/// package's name and the file's name; else the first GUID; else a new one.
pub fn vm_name(env: &Envelope, system: &VirtualSystem, file_name: &str) -> String {
    let vssd = hardware(system).map(|h| h.vssd.identifier.clone());
    let candidates = [
        Some(system.name.clone()),
        Some(system.id.clone()),
        vssd,
        Some(system.os_description.clone()),
        Some(env.name.clone()),
        Some(file_name.to_owned()),
    ];
    let names: Vec<String> = candidates
        .into_iter()
        .flatten()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    names
        .iter()
        .find(|n| !is_guid(n))
        .or_else(|| names.first())
        .cloned()
        .unwrap_or_else(new_guid)
}

/// A random GUID (from the OS's random source through `ring`).
pub fn new_guid() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// What `CreateVm` makes of a system's hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmHardware {
    pub memory: i64,
    pub vcpus_at_startup: i64,
    pub vcpus_max: i64,
    /// The other-config entries (names lower-case, `-` as `_`).
    pub other_config: Vec<(String, String)>,
}

/// `CreateVm`'s memory and vCPUs: memory is the sum of the memory items
/// (MB unless the units say otherwise; 256 MiB when none); the vCPUs at
/// startup the sum of the CPU items' quantities, the maximum the sum of
/// their limits (a limit of 100000 or more meaning none); both at least 1.
pub fn vm_hardware(hw: &Hardware) -> VmHardware {
    let mut memory: u64 = 0;
    let mut startup: u64 = 0;
    let mut max: u64 = 0;
    for r in &hw.items {
        match r.resource_type {
            resource::MEMORY => {
                let units = allocation_units(&r.allocation_units).unwrap_or(1 << 20);
                memory =
                    memory.saturating_add(r.virtual_quantity.unwrap_or(0).saturating_mul(units));
            }
            resource::CPU => {
                let q = r.virtual_quantity.unwrap_or(0);
                startup += q;
                max += match r.limit {
                    Some(l) if l < 100_000 => l,
                    _ => q,
                };
            }
            _ => {}
        }
    }
    if memory == 0 {
        memory = 256 << 20;
    }
    let startup = startup.max(1);
    VmHardware {
        memory: i64::try_from(memory).unwrap_or(i64::MAX),
        vcpus_at_startup: startup as i64,
        vcpus_max: max.max(startup) as i64,
        other_config: hw
            .other_config
            .iter()
            .map(|(k, v, _)| (k.to_lowercase().replace('-', "_"), v.clone()))
            .collect(),
    }
}

/// `k=v;k=v` as a map (a value without `=` under `order`: the legacy boot
/// order string).
pub fn split_pairs(s: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('=') {
            Some((k, v)) => {
                m.insert(k.trim().to_owned(), v.trim().to_owned());
            }
            None if m.is_empty() => {
                m.insert("order".to_owned(), part.to_owned());
            }
            None => {}
        }
    }
    m
}

/// The device an item asks for: its connection's `device=`, else its
/// `AddressOnParent`.
pub fn wanted_device(r: &Rasd) -> Option<String> {
    r.connection_value("device")
        .or_else(|| Some(r.address_on_parent.clone()).filter(|a| !a.is_empty()))
}

/// `SetIfDeviceIsBootable`.
pub fn disk_bootable(env: &Envelope, r: &Rasd) -> bool {
    if let Some(b) = r.connection_value("bootable") {
        return b == "true";
    }
    if let Some((d, _)) = env.disk_of(r)
        && let Some(b) = d.bootable
    {
        return b;
    }
    r.address_on_parent == "0" || r.connection_value("device").as_deref() == Some("0")
}

/// A MAC written without separators gets its colons.
pub fn mac_with_colons(mac: &str) -> String {
    let m = mac.trim();
    if m.is_empty() || m.contains(':') || m.contains('-') {
        return m.replace('-', ":").to_lowercase();
    }
    m.as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(":")
        .to_lowercase()
}

// ---- validation -------------------------------------------------------------------------------

/// A finding of [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The package has no usable descriptor.
    NoDescriptor,
    /// A referenced file is not in the package.
    MissingFile(String),
    NoVersion,
    UnsupportedVersion(String),
    UnsupportedExtension(String),
    UnexpectedHardware(String),
    CpuInvalid,
    CpuLimit(u64, u64),
    MemoryNoInstanceId,
    MemoryInvalid,
    MemoryUnits,
    NetworkNotConnected,
    UnknownRequired(String, String),
    NoDiskLink(String),
}

impl Finding {
    /// A hard failure (the package cannot be imported).
    pub fn is_error(&self) -> bool {
        matches!(self, Self::NoDescriptor | Self::MissingFile(_))
    }
}

const VERSIONS: &[&str] = &[
    "0.9", "1.0", "1.0.0", "1.0.0a", "1.0.0.b", "1.0.0c", "1.0.0d", "1.0.1", "1.0.0e", "2.0.0",
    "2.0", "2.1.0", "2.1.1",
];
const EXTENSIONS: &[&str] = &[
    ".vhd", ".pvp", ".vmdk", ".mf", ".cert", ".xva", ".ovf", ".wim", ".vdi", ".sdi", ".iso", ".gz",
    ".img", ".raw",
];
const SYSTEM_TYPES: &[&str] = &[
    "xen-3.0-unknown",
    "xen-3.0-x32",
    "xen-3.0-x86",
    "xen-3.0-x64",
    "hvm-3.0-unknown",
    "hvm-3.0-x32",
    "hvm-3.0-x86",
    "hvm-3.0-x64",
    "hvm-3.0-hvm",
    "301",
    "vmx-4",
    "vmx-04",
    "vmx-6",
    "vmx-06",
    "vmx-7",
    "vmx-07",
    "dmtf:xen:pv",
    "dmtf:xen:hvm",
    "virtualbox-2.2",
];
/// `Properties.Settings.MaxVCPUs`-like ceiling of the original's check.
const MAX_VCPUS: u64 = 64;

/// `OVF.Validate` with the default flags (version, files, CPU, memory,
/// networks, capability). `has_file` says whether the package holds a
/// file.
pub fn validate(env: &Envelope, has_file: &dyn Fn(&str) -> bool) -> Vec<Finding> {
    let mut out = Vec::new();
    match &env.version {
        None => out.push(Finding::NoVersion),
        Some(v) if !VERSIONS.contains(&v.as_str()) => {
            out.push(Finding::UnsupportedVersion(v.clone()));
        }
        _ => {}
    }
    for f in &env.files {
        let lower = f.href.to_lowercase();
        if lower.ends_with(".mf") || lower.ends_with(".cert") {
            continue;
        }
        if !has_file(&f.href) {
            out.push(Finding::MissingFile(f.href.clone()));
        }
        if !EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
            out.push(Finding::UnsupportedExtension(f.href.clone()));
        }
    }
    for s in &env.systems {
        for hw in &s.hardware {
            for t in hw.vssd.system_type.split_whitespace() {
                if !SYSTEM_TYPES.contains(&t.to_lowercase().as_str()) {
                    out.push(Finding::UnexpectedHardware(t.to_owned()));
                }
            }
            for r in &hw.items {
                match r.resource_type {
                    resource::CPU => match r.virtual_quantity {
                        None | Some(0) => out.push(Finding::CpuInvalid),
                        Some(q) if q > MAX_VCPUS => out.push(Finding::CpuLimit(q, MAX_VCPUS)),
                        _ => {}
                    },
                    resource::MEMORY => {
                        if r.instance_id.is_empty() {
                            out.push(Finding::MemoryNoInstanceId);
                        }
                        if r.virtual_quantity.unwrap_or(0) == 0 {
                            out.push(Finding::MemoryInvalid);
                        }
                        if !r.allocation_units.is_empty()
                            && allocation_units(&r.allocation_units).is_none()
                        {
                            out.push(Finding::MemoryUnits);
                        }
                    }
                    resource::NIC => {
                        let linked = r.connection.iter().any(|c| {
                            env.networks.iter().any(|n| {
                                (!n.name.is_empty() && c.contains(&n.name))
                                    || (!n.description.is_empty() && c.contains(&n.description))
                            })
                        });
                        if !linked && !env.networks.is_empty() {
                            out.push(Finding::NetworkNotConnected);
                        }
                    }
                    t if r.is_disk() => {
                        let _ = t;
                        if env.disk_of(r).is_none() && !env.disks.is_empty() {
                            out.push(Finding::NoDiskLink(r.element_name.clone()));
                        }
                    }
                    t if r.required && !(3..=10).contains(&t) && !(15..=21).contains(&t) => {
                        out.push(Finding::UnknownRequired(
                            r.instance_id.clone(),
                            r.element_name.clone(),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

// ---- writing --------------------------------------------------------------------------------

/// What the export writes about one VM (`Export.cs`).
#[derive(Debug, Clone, Default)]
pub struct ExportedVm {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hvm: bool,
    pub arch: String,
    pub os_id: Option<u32>,
    pub os_description: String,
    pub os_info: String,
    pub vcpus_at_startup: i64,
    pub vcpus_max: i64,
    /// `memory_dynamic_max` in MiB.
    pub memory_mib: i64,
    pub order: i64,
    pub start_delay: i64,
    pub shutdown_delay: i64,
    /// (network uuid, name, description, MAC).
    pub nics: Vec<(String, String, String, String)>,
    pub disks: Vec<ExportedDisk>,
    /// (VBD uuid, userdevice).
    pub cds: Vec<(String, String)>,
    /// name, value, info.
    pub other_config: Vec<(String, String, String)>,
}

/// A disk of an [`ExportedVm`].
#[derive(Debug, Clone, Default)]
pub struct ExportedDisk {
    pub disk_id: String,
    pub file: String,
    pub file_size: Option<u64>,
    pub compressed: bool,
    pub name: String,
    pub description: String,
    pub capacity: u64,
    pub bootable: bool,
    pub userdevice: String,
}

/// Writes elements by hand-made events through `quick-xml`.
struct Xml {
    w: quick_xml::Writer<Vec<u8>>,
}

impl Xml {
    fn new() -> Self {
        Self {
            w: quick_xml::Writer::new_with_indent(Vec::new(), b' ', 2),
        }
    }

    fn open(&mut self, name: &str, attrs: &[(&str, &str)]) {
        use quick_xml::events::{BytesStart, Event};
        let mut e = BytesStart::new(name);
        for (k, v) in attrs {
            e.push_attribute((*k, *v));
        }
        let _ = self.w.write_event(Event::Start(e));
    }

    fn close(&mut self, name: &str) {
        use quick_xml::events::{BytesEnd, Event};
        let _ = self.w.write_event(Event::End(BytesEnd::new(name)));
    }

    fn empty(&mut self, name: &str, attrs: &[(&str, &str)]) {
        use quick_xml::events::{BytesStart, Event};
        let mut e = BytesStart::new(name);
        for (k, v) in attrs {
            e.push_attribute((*k, *v));
        }
        let _ = self.w.write_event(Event::Empty(e));
    }

    fn leaf(&mut self, name: &str, attrs: &[(&str, &str)], value: &str) {
        use quick_xml::events::{BytesText, Event};
        self.open(name, attrs);
        let _ = self.w.write_event(Event::Text(BytesText::new(value)));
        self.close(name);
    }

    fn finish(self) -> String {
        let body = String::from_utf8(self.w.into_inner()).unwrap_or_default();
        format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n{body}\n")
    }
}

/// Fields of one item, in the order the CIM schema wants them.
fn rasd(x: &mut Xml, attrs: &[(&str, &str)], fields: &[(&str, &str)]) {
    x.open("ovf:Item", attrs);
    let mut sorted: Vec<&(&str, &str)> = fields.iter().filter(|(_, v)| !v.is_empty()).collect();
    sorted.sort_by_key(|(k, _)| *k);
    for (k, v) in sorted {
        x.leaf(&format!("rasd:{k}"), &[], v);
    }
    x.close("ovf:Item");
}

/// The descriptor of an export of `vms` as the package `name` (EULA texts
/// in `eulas`).
pub fn export_descriptor(name: &str, vms: &[ExportedVm], eulas: &[String]) -> String {
    let mut x = Xml::new();
    let package_id = new_guid();
    x.open(
        "ovf:Envelope",
        &[
            ("xmlns:ovf", NS_OVF),
            ("xmlns:rasd", NS_RASD),
            ("xmlns:vssd", NS_VSSD),
            ("xmlns:xsi", NS_XSI),
            ("xmlns:cim", NS_CIM),
            ("xmlns:xenovf", NS_XENOVF),
            ("ovf:version", "1.0.0"),
            ("xenovf:Name", name),
            ("xenovf:id", &package_id),
            ("xenovf:version", "1.0.0"),
            ("xml:lang", "en-US"),
        ],
    );
    x.open("ovf:References", &[]);
    for d in vms.iter().flat_map(|v| v.disks.iter()) {
        let size = d.file_size.map(|s| s.to_string());
        let mut a = vec![
            ("ovf:id", d.disk_id.as_str()),
            ("ovf:href", d.file.as_str()),
        ];
        if let Some(s) = &size {
            a.push(("ovf:size", s));
        }
        if d.compressed {
            a.push(("ovf:compression", "gzip"));
        }
        x.empty("ovf:File", &a);
    }
    x.close("ovf:References");
    x.open("ovf:DiskSection", &[]);
    x.leaf("ovf:Info", &[], "Available Disks");
    for d in vms.iter().flat_map(|v| v.disks.iter()) {
        let cap = d.capacity.to_string();
        x.empty(
            "ovf:Disk",
            &[
                ("ovf:diskId", &d.disk_id),
                ("ovf:fileRef", &d.disk_id),
                ("ovf:capacity", &cap),
                ("ovf:format", VHD_FORMAT),
                (
                    "xenovf:isBootable",
                    if d.bootable { "true" } else { "false" },
                ),
            ],
        );
    }
    x.close("ovf:DiskSection");
    x.open("ovf:NetworkSection", &[]);
    x.leaf("ovf:Info", &[], "Available Networks");
    let mut seen = Vec::new();
    for (uuid, _, desc, _) in vms.iter().flat_map(|v| v.nics.iter()) {
        if seen.contains(uuid) {
            continue;
        }
        seen.push(uuid.clone());
        x.open("ovf:Network", &[("ovf:name", uuid)]);
        x.leaf(
            "ovf:Description",
            &[],
            if desc.is_empty() {
                "Network Adapter"
            } else {
                desc
            },
        );
        x.close("ovf:Network");
    }
    x.close("ovf:NetworkSection");
    x.open("ovf:StartupSection", &[]);
    x.leaf("ovf:Info", &[], "Startup Options");
    for v in vms {
        let (o, sd, st) = (
            v.order.to_string(),
            v.start_delay.to_string(),
            v.shutdown_delay.to_string(),
        );
        x.empty(
            "ovf:Item",
            &[
                ("ovf:id", &v.id),
                ("ovf:order", &o),
                ("ovf:startAction", "powerOn"),
                ("ovf:startDelay", &sd),
                ("ovf:stopAction", "powerOff"),
                ("ovf:stopDelay", &st),
                ("ovf:waitingForGuest", "false"),
            ],
        );
    }
    x.close("ovf:StartupSection");
    let several = vms.len() > 1;
    if several {
        x.open("ovf:VirtualSystemCollection", &[("ovf:id", name)]);
        x.leaf("ovf:Info", &[], "OVF/OVA Packages");
    }
    for v in vms {
        system(&mut x, v, if several { &[] } else { eulas });
    }
    if several {
        eula_sections(&mut x, eulas);
        x.close("ovf:VirtualSystemCollection");
    }
    x.close("ovf:Envelope");
    x.finish()
}

fn eula_sections(x: &mut Xml, eulas: &[String]) {
    for e in eulas {
        x.open("ovf:EulaSection", &[]);
        x.leaf("ovf:Info", &[], "License Agreement");
        x.leaf("ovf:License", &[], e);
        x.close("ovf:EulaSection");
    }
}

fn system(x: &mut Xml, v: &ExportedVm, eulas: &[String]) {
    x.open("ovf:VirtualSystem", &[("ovf:id", &v.id)]);
    x.leaf("ovf:Info", &[], "XenOVF Generated");
    x.leaf("ovf:Name", &[], &v.name);
    let os_id = v.os_id.unwrap_or(1).to_string();
    x.open("ovf:OperatingSystemSection", &[("ovf:id", &os_id)]);
    x.leaf(
        "ovf:Info",
        &[],
        if v.os_info.is_empty() {
            "Guest Operating System"
        } else {
            &v.os_info
        },
    );
    if !v.os_description.is_empty() {
        x.leaf("ovf:Description", &[], &v.os_description);
    }
    x.close("ovf:OperatingSystemSection");
    x.open("ovf:VirtualHardwareSection", &[]);
    x.leaf(
        "ovf:Info",
        &[],
        &format!(
            "Virtual Hardware Requirements: {} MB RAM; {} CPU(s), {} Disk(s), {} Network(s)",
            v.memory_mib,
            v.vcpus_at_startup,
            v.disks.len(),
            v.nics.len()
        ),
    );
    x.open("ovf:System", &[]);
    let kind = format!(
        "{}-3.0-{}",
        if v.hvm { "hvm" } else { "xen" },
        if v.arch.is_empty() {
            "unknown"
        } else {
            &v.arch
        }
    );
    let ident = new_guid();
    for (k, val) in [
        ("Caption", "Xen Virtual Machine"),
        ("Description", v.description.as_str()),
        ("ElementName", v.name.as_str()),
        ("InstanceID", ident.as_str()),
        ("VirtualSystemIdentifier", ident.as_str()),
        ("VirtualSystemType", kind.as_str()),
    ] {
        x.leaf(&format!("vssd:{k}"), &[], val);
    }
    x.close("ovf:System");
    let (startup, max, mem) = (
        v.vcpus_at_startup.to_string(),
        v.vcpus_max.to_string(),
        v.memory_mib.to_string(),
    );
    let cpu_id = new_guid();
    rasd(
        x,
        &[],
        &[
            ("AllocationUnits", "Processor Cores"),
            ("Caption", "Processor"),
            ("ConsumerVisibility", "0"),
            ("Description", "Processor"),
            ("ElementName", "Processor"),
            ("InstanceID", &cpu_id),
            ("Limit", &max),
            ("MappingBehavior", "0"),
            ("ResourceType", "3"),
            ("VirtualQuantity", &startup),
            ("Weight", "100"),
        ],
    );
    let mem_id = new_guid();
    rasd(
        x,
        &[],
        &[
            ("AllocationUnits", "byte * 2^20"),
            ("Caption", "Memory"),
            ("ConsumerVisibility", "2"),
            ("Description", "Memory Setting Data"),
            ("ElementName", "Memory"),
            ("InstanceID", &mem_id),
            ("Limit", "128"),
            ("ResourceSubType", "Machine Memory"),
            ("ResourceType", "4"),
            ("VirtualQuantity", &mem),
        ],
    );
    for (uuid, net_name, desc, mac) in &v.nics {
        let id = new_guid();
        rasd(
            x,
            &[],
            &[
                ("Address", mac),
                ("AllocationUnits", "Ports"),
                ("Caption", "Network Adapter"),
                ("Connection", uuid),
                (
                    "Description",
                    if desc.is_empty() {
                        "Network Adapter"
                    } else {
                        desc
                    },
                ),
                ("ElementName", net_name),
                ("InstanceID", &id),
                ("Limit", "1"),
                ("ResourceType", "10"),
                ("Weight", "0"),
            ],
        );
    }
    for d in &v.disks {
        let host = format!("ovf:/disk/{}", d.disk_id);
        let conn = format!("{},device={}", d.disk_id, d.userdevice);
        rasd(
            x,
            &[],
            &[
                ("AddressOnParent", &d.userdevice),
                ("AllocationUnits", "Disk"),
                ("Caption", "Hard Disk Image"),
                ("Connection", &conn),
                ("Description", &d.description),
                ("ElementName", &d.name),
                ("HostResource", &host),
                ("InstanceID", &d.disk_id),
                ("Limit", "1"),
                ("ResourceSubType", "Virtual Hard Disk Image"),
                ("ResourceType", "19"),
                ("Weight", "100"),
            ],
        );
    }
    for (vbd, device) in &v.cds {
        let conn = format!("device={device}");
        rasd(
            x,
            &[("ovf:required", "false")],
            &[
                ("AllocationUnits", "DVD Drives"),
                ("Caption", "CD/DVD Drive"),
                ("Connection", &conn),
                ("Description", "Xen CD/DVD Drive Setting Data."),
                ("ElementName", "CD/DVD Drive"),
                ("InstanceID", vbd),
                ("ResourceType", "16"),
            ],
        );
    }
    for (n, value, info) in &v.other_config {
        let id = new_guid();
        x.open(
            "xenovf:VirtualSystemOtherConfigurationData",
            &[
                ("xenovf:Name", n),
                ("xenovf:id", &id),
                ("ovf:required", "false"),
            ],
        );
        x.leaf("ovf:Info", &[], info);
        x.leaf("xenovf:Value", &[], value);
        x.close("xenovf:VirtualSystemOtherConfigurationData");
    }
    x.close("ovf:VirtualHardwareSection");
    eula_sections(x, eulas);
    x.close("ovf:VirtualSystem");
}

/// The envelope a disk image import makes (`OVF.CreateOvfEnvelope`): one
/// HVM system with `cpus`, `memory_mib`, one interface and one bootable
/// disk whose file is `path` (absolute), with the boot mode's extra
/// boot parameters and platform keys.
#[allow(clippy::too_many_arguments)]
pub fn image_envelope(
    vm_name: &str,
    cpus: i64,
    memory_mib: i64,
    boot_params: &str,
    platform: &str,
    capacity: u64,
    path: &str,
    file_size: u64,
) -> Envelope {
    let disk_id = new_guid();
    let system_id = new_guid();
    let items = vec![
        Rasd {
            instance_id: new_guid(),
            virtual_quantity: Some(cpus as u64),
            limit: Some(100_000),
            allocation_units: "Processor Cores".into(),
            element_name: "Processor".into(),
            ..Rasd::new(resource::CPU)
        },
        Rasd {
            instance_id: new_guid(),
            virtual_quantity: Some(memory_mib as u64),
            allocation_units: "MB".into(),
            element_name: "Memory".into(),
            ..Rasd::new(resource::MEMORY)
        },
        Rasd {
            instance_id: new_guid(),
            element_name: "Network 0".into(),
            description: "Virtual Network".into(),
            connection: vec![new_guid()],
            ..Rasd::new(resource::NIC)
        },
        Rasd {
            instance_id: disk_id.clone(),
            // The original's "Virtual Disk"; named after the VM here so
            // the disk can be told apart.
            element_name: vm_name.to_owned(),
            description: "Created by Disk Image Import".into(),
            caption: "Virtual Disk".into(),
            host_resource: vec![format!("ovf:/disk/{disk_id}")],
            ..Rasd::new(resource::STORAGE_EXTENT)
        },
    ];
    Envelope {
        name: vm_name.to_owned(),
        version: Some("1.0.0".into()),
        files: vec![FileRef {
            id: disk_id.clone(),
            href: path.to_owned(),
            size: Some(file_size),
            compression: None,
        }],
        disks: vec![DiskDef {
            disk_id: disk_id.clone(),
            file_ref: disk_id,
            capacity,
            format: String::new(),
            bootable: Some(true),
        }],
        systems: vec![VirtualSystem {
            id: system_id,
            info: String::new(),
            name: vm_name.to_owned(),
            hardware: vec![Hardware {
                vssd: Vssd {
                    element_name: vm_name.to_owned(),
                    caption: "Virtual Machine".into(),
                    description: "Created by Disk Image Import".into(),
                    system_type: "hvm-3.0-unknown".into(),
                    ..Default::default()
                },
                items,
                other_config: vec![
                    ("HVM_boot_policy".into(), "BIOS order".into(), String::new()),
                    (
                        "HVM_boot_params".into(),
                        format!("order=dc;{boot_params}"),
                        String::new(),
                    ),
                    (
                        "platform".into(),
                        format!("nx=true;acpi=true;apic=true;pae=true;stdvga=0;{platform}"),
                        String::new(),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `k=v;k=v;` of a map, keys sorted.
pub fn join_pairs(m: &HashMap<String, String>) -> String {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    keys.iter().map(|k| format!("{k}={};", m[*k])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exported() -> ExportedVm {
        ExportedVm {
            id: "sys-1".into(),
            name: "db & co".into(),
            description: "a <test>".into(),
            hvm: true,
            vcpus_at_startup: 2,
            vcpus_max: 4,
            memory_mib: 1024,
            nics: vec![(
                "net-uuid".into(),
                "Pool-wide network".into(),
                String::new(),
                "aa:bb:cc:dd:ee:ff".into(),
            )],
            disks: vec![ExportedDisk {
                disk_id: "d1".into(),
                file: "u1.vhd".into(),
                file_size: Some(1234),
                name: "disk 0".into(),
                capacity: 10 << 30,
                bootable: true,
                userdevice: "0".into(),
                ..Default::default()
            }],
            cds: vec![("vbd-cd".into(), "3".into())],
            other_config: vec![
                (
                    "HVM_boot_policy".into(),
                    "BIOS order".into(),
                    "Boot Order".into(),
                ),
                (
                    "platform".into(),
                    "acpi=1;nx=true;".into(),
                    "Platform".into(),
                ),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn an_export_reads_back() {
        let xml = export_descriptor("pkg", &[exported()], &["Be nice.".into()]);
        let env = Envelope::parse(&xml).unwrap();
        assert_eq!(env.name, "pkg");
        assert_eq!(env.version.as_deref(), Some("1.0.0"));
        assert_eq!(env.files.len(), 1);
        assert_eq!(env.files[0].size, Some(1234));
        assert_eq!(env.disks[0].capacity, 10 << 30);
        assert_eq!(env.disks[0].bootable, Some(true));
        assert_eq!(env.networks[0].name, "net-uuid");
        assert_eq!(env.startup.len(), 1);
        assert_eq!(env.all_eulas(), ["Be nice."]);
        let s = &env.systems[0];
        assert_eq!(s.name, "db & co");
        assert_eq!(vm_name(&env, s, "pkg.ovf"), "db & co");
        let hw = hardware(s).unwrap();
        assert_eq!(hw.vssd.description, "a <test>");
        assert_eq!(hw.vssd.system_type, "hvm-3.0-unknown");
        let h = vm_hardware(hw);
        assert_eq!((h.memory, h.vcpus_at_startup, h.vcpus_max), (1 << 30, 2, 4));
        assert_eq!(
            h.other_config[0],
            ("hvm_boot_policy".into(), "BIOS order".into())
        );
        let disk = hw.items.iter().find(|r| r.is_disk()).unwrap();
        assert_eq!(env.disk_of(disk).unwrap().1.href, "u1.vhd");
        assert_eq!(wanted_device(disk).as_deref(), Some("0"));
        assert!(disk_bootable(&env, disk));
        let cd = hw.items.iter().find(|r| r.is_cd()).unwrap();
        assert!(!cd.required);
        assert_eq!(wanted_device(cd).as_deref(), Some("3"));
        let nic = hw.items.iter().find(|r| r.resource_type == 10).unwrap();
        assert_eq!(nic.address, "aa:bb:cc:dd:ee:ff");
        assert!(
            validate(&env, &|_| true).is_empty(),
            "{:?}",
            validate(&env, &|_| true)
        );
        assert_eq!(
            validate(&env, &|_| false),
            [Finding::MissingFile("u1.vhd".into())]
        );
    }

    #[test]
    fn several_vms_make_a_collection() {
        let mut b = exported();
        b.id = "sys-2".into();
        b.name = "web".into();
        b.disks[0].disk_id = "d2".into();
        let xml = export_descriptor("pkg", &[exported(), b], &["E".into()]);
        assert!(xml.contains("VirtualSystemCollection"));
        let env = Envelope::parse(&xml).unwrap();
        assert_eq!(env.systems.len(), 2);
        assert_eq!(env.eulas, ["E"]);
        assert_eq!(env.networks.len(), 1);
    }

    #[test]
    fn foreign_descriptors() {
        // VirtualBox style: default namespace, MB units in a different
        // form, VMDK disks, IDE controller.
        let xml = r#"<?xml version="1.0"?>
<Envelope ovf:version="1.0" xml:lang="en-US" xmlns="http://schemas.dmtf.org/ovf/envelope/1" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:vssd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData">
  <References><File ovf:href="disk1.vmdk" ovf:id="file1"/></References>
  <DiskSection><Info>x</Info><Disk ovf:capacity="8" ovf:capacityAllocationUnits="byte * 2^30" ovf:diskId="vmdisk1" ovf:fileRef="file1" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized"/></DiskSection>
  <NetworkSection><Info>x</Info><Network ovf:name="NAT"><Description>NAT</Description></Network></NetworkSection>
  <VirtualSystem ovf:id="3f2504e0-4f89-11d3-9a0c-0305e82c3301">
    <Info>x</Info>
    <OperatingSystemSection ovf:id="96"><Info>x</Info><Description>Debian_64</Description></OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>x</Info>
      <System><vssd:ElementName>Virtual Hardware Family</vssd:ElementName><vssd:InstanceID>0</vssd:InstanceID><vssd:VirtualSystemIdentifier>deb</vssd:VirtualSystemIdentifier><vssd:VirtualSystemType>virtualbox-2.2</vssd:VirtualSystemType></System>
      <Item><rasd:Caption>1 virtual CPU</rasd:Caption><rasd:ElementName>1 virtual CPU</rasd:ElementName><rasd:InstanceID>1</rasd:InstanceID><rasd:ResourceType>3</rasd:ResourceType><rasd:VirtualQuantity>1</rasd:VirtualQuantity></Item>
      <Item><rasd:AllocationUnits>MegaBytes</rasd:AllocationUnits><rasd:InstanceID>2</rasd:InstanceID><rasd:ResourceType>4</rasd:ResourceType><rasd:VirtualQuantity>512</rasd:VirtualQuantity></Item>
      <Item><rasd:InstanceID>3</rasd:InstanceID><rasd:ResourceType>5</rasd:ResourceType></Item>
      <Item><rasd:AutomaticAllocation>true</rasd:AutomaticAllocation><rasd:Connection>NAT</rasd:Connection><rasd:InstanceID>4</rasd:InstanceID><rasd:ResourceType>10</rasd:ResourceType></Item>
      <Item><rasd:AddressOnParent>0</rasd:AddressOnParent><rasd:ElementName>disk1</rasd:ElementName><rasd:HostResource>/disk/vmdisk1</rasd:HostResource><rasd:InstanceID>5</rasd:InstanceID><rasd:Parent>3</rasd:Parent><rasd:ResourceType>17</rasd:ResourceType></Item>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>"#;
        let env = Envelope::parse(xml).unwrap();
        let s = &env.systems[0];
        // The id is a GUID: the VSSD identifier is the name.
        assert_eq!(vm_name(&env, s, "x.ova"), "deb");
        let hw = hardware(s).unwrap();
        let h = vm_hardware(hw);
        assert_eq!((h.memory, h.vcpus_max), (512 << 20, 1));
        let disk = hw.items.iter().find(|r| r.is_disk()).unwrap();
        let (d, f) = env.disk_of(disk).unwrap();
        assert_eq!((d.capacity, f.href.as_str()), (8 << 30, "disk1.vmdk"));
        assert!(disk_bootable(&env, disk));
        assert!(validate(&env, &|_| true).is_empty());
    }

    #[test]
    fn small_rules() {
        assert_eq!(allocation_units("byte * 2^20"), Some(1 << 20));
        assert_eq!(allocation_units("byte*1024^3"), Some(1 << 30));
        assert_eq!(allocation_units("GB"), Some(1 << 30));
        assert_eq!(allocation_units("furlongs"), None);
        assert_eq!(mac_with_colons("AABBCCDDEEFF"), "aa:bb:cc:dd:ee:ff");
        assert_eq!(mac_with_colons("AA-BB-CC-DD-EE-FF"), "aa:bb:cc:dd:ee:ff");
        assert!(is_guid("3f2504e0-4f89-11d3-9a0c-0305e82c3301"));
        assert!(is_guid(&new_guid()));
        assert!(!is_guid("db-01"));
        let m = split_pairs("order=dc;firmware=uefi;");
        assert_eq!(m["order"], "dc");
        assert_eq!(m["firmware"], "uefi");
        assert_eq!(split_pairs("cdn")["order"], "cdn");
        assert!(Envelope::parse("<x/>").is_err());
        let env = Envelope::parse(r#"<Envelope><SecuritySection/></Envelope>"#).unwrap();
        assert!(env.encrypted);
        assert_eq!(validate(&env, &|_| true), [Finding::NoVersion]);
    }

    #[test]
    fn images_make_an_envelope() {
        let env = image_envelope(
            "img",
            2,
            512,
            "firmware=uefi;",
            "secureboot=true;",
            1 << 30,
            "/tmp/a.vhd",
            77,
        );
        let s = &env.systems[0];
        let hw = hardware(s).unwrap();
        let h = vm_hardware(hw);
        assert_eq!(
            (h.memory, h.vcpus_at_startup, h.vcpus_max),
            (512 << 20, 2, 2)
        );
        let disk = hw.items.iter().find(|r| r.is_disk()).unwrap();
        assert_eq!(env.disk_of(disk).unwrap().1.href, "/tmp/a.vhd");
        assert!(disk_bootable(&env, disk));
        assert!(
            h.other_config
                .iter()
                .any(|(k, v)| k == "hvm_boot_params" && v == "order=dc;firmware=uefi;")
        );
    }
}
