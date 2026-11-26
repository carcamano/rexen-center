//! The OVF/OVA and disk image wizards' rules (F11b): which file the Import
//! wizard was given, the rows of its storage and network pages and their
//! defaults, and the Export wizard's OVF checks.

use std::path::Path;
use std::sync::Arc;

use xapi::{Network, NetworkRecord, Ref, Sr, SrRecord, VmRecord};

use crate::disk_image::{self, Image, Unsupported};
use crate::model::ConnectionModel;
use crate::ovf::{self, Envelope};

/// What the Import wizard's source names (`ImportWizard.ImportType`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// `.xva` (`.xva.gz`: compressed).
    Xva {
        compressed: bool,
    },
    /// `.ovf`, `.ova` (`.ova.gz`: compressed).
    Ovf {
        ova: bool,
        compressed: bool,
    },
    /// A disk image: `.vhd`, `.vmdk`, and this console's raw images and
    /// ISOs (`.img`, `.raw`, `.iso`), plus the formats it names to refuse
    /// them (`.vhdx`, `.qcow2`, `.wim`).
    Image,
    Unknown,
}

/// `CheckIsSupportedType`: by the name's extension (lower case).
pub fn source_kind(path: &str) -> SourceKind {
    let p = path.trim().to_lowercase();
    if p.ends_with(".xva") || p.ends_with(".xva.gz") {
        return SourceKind::Xva {
            compressed: p.ends_with(".gz"),
        };
    }
    if p.ends_with(".ovf") || p.ends_with(".ova") || p.ends_with(".ova.gz") {
        return SourceKind::Ovf {
            ova: !p.ends_with(".ovf"),
            compressed: p.ends_with(".gz"),
        };
    }
    if [
        ".vhd", ".vmdk", ".img", ".raw", ".iso", ".vhdx", ".qcow2", ".qcow", ".wim",
    ]
    .iter()
    .any(|e| p.ends_with(e))
    {
        return SourceKind::Image;
    }
    SourceKind::Unknown
}

/// What a disk image file holds, as the source page needs it: the image,
/// or the format that would need converting.
pub fn inspect_image(path: &Path) -> std::io::Result<Result<Image, Unsupported>> {
    let lower = path.to_string_lossy().to_lowercase();
    // A name that says so is refused even when the content is unreadable.
    for (ext, kind) in [
        (".vhdx", Unsupported::Vhdx),
        (".qcow2", Unsupported::Qcow2),
        (".qcow", Unsupported::Qcow2),
        (".wim", Unsupported::Wim),
    ] {
        if lower.ends_with(ext) {
            return Ok(Err(kind));
        }
    }
    Ok(match disk_image::inspect(path)? {
        Image::Unsupported(kind) => Err(kind),
        image => Ok(image),
    })
}

/// The first disk of the package that would need converting: (file,
/// format).
pub fn unconvertible_disk(package: &crate::ovf_package::Package) -> Option<(String, &'static str)> {
    for f in &package.envelope.files {
        let lower = f.href.to_lowercase();
        if lower.ends_with(".mf") || lower.ends_with(".cert") || !package.has_file(&f.href) {
            continue;
        }
        if f.compression.is_some() {
            // Looked at once expanded, by the import.
            continue;
        }
        let Ok(pf) = package.open_file(&f.href) else {
            continue;
        };
        let mut file = pf.file;
        if let Ok(Image::Unsupported(kind)) = disk_image::inspect_at(&mut file, pf.offset, pf.len) {
            return Some((f.href.clone(), kind.name()));
        }
    }
    None
}

/// A disk row of the storage page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRow {
    pub system: String,
    pub vm: String,
    /// The disk item's `InstanceID`.
    pub item: String,
    pub label: String,
    /// The room the disk needs (its capacity, else its file's size).
    pub size: u64,
}

/// An interface row of the network page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicRow {
    pub system: String,
    pub vm: String,
    pub item: String,
    pub label: String,
    /// The MAC the package gives (empty: generated).
    pub mac: String,
    /// The item's connection (a network name or uuid).
    pub connection: String,
}

/// The disk rows of `systems` ((id, VM name) pairs).
pub fn disk_rows(env: &Envelope, systems: &[(String, String)]) -> Vec<DiskRow> {
    let mut out = Vec::new();
    for (id, vm) in systems {
        let Some(hw) = env.system(id).and_then(ovf::hardware) else {
            continue;
        };
        for r in hw.items.iter().filter(|r| r.is_disk()) {
            let Some((d, f)) = env.disk_of(r) else {
                continue;
            };
            out.push(DiskRow {
                system: id.clone(),
                vm: vm.clone(),
                item: r.instance_id.clone(),
                label: if r.element_name.is_empty() {
                    f.href.clone()
                } else {
                    r.element_name.clone()
                },
                size: if d.capacity > 0 {
                    d.capacity
                } else {
                    f.size.unwrap_or(0)
                },
            });
        }
    }
    out
}

/// The interface rows of `systems`.
pub fn nic_rows(env: &Envelope, systems: &[(String, String)]) -> Vec<NicRow> {
    let mut out = Vec::new();
    for (id, vm) in systems {
        let Some(hw) = env.system(id).and_then(ovf::hardware) else {
            continue;
        };
        for r in hw
            .items
            .iter()
            .filter(|r| r.resource_type == ovf::resource::NIC)
        {
            let connection = r
                .connection_value("network")
                .or_else(|| r.connection.first().cloned())
                .unwrap_or_default();
            out.push(NicRow {
                system: id.clone(),
                vm: vm.clone(),
                item: r.instance_id.clone(),
                label: if !r.element_name.is_empty() {
                    r.element_name.clone()
                } else if !connection.is_empty() {
                    connection.clone()
                } else {
                    String::new()
                },
                mac: ovf::mac_with_colons(&r.address),
                connection,
            });
        }
    }
    out
}

/// The network an interface goes on by default: the offered network the
/// package names (by uuid, then name, then bridge), else the first one.
pub fn default_network(
    row: &NicRow,
    offered: &[(&Ref<Network>, &Arc<NetworkRecord>)],
) -> Option<Ref<Network>> {
    let c = row.connection.trim();
    if !c.is_empty() {
        let by = |f: &dyn Fn(&NetworkRecord) -> bool| {
            offered
                .iter()
                .find(|(_, n)| f(n))
                .map(|(r, _)| (*r).clone())
        };
        if let Some(r) = by(&|n| n.uuid == c)
            .or_else(|| by(&|n| n.name_label == c))
            .or_else(|| by(&|n| !n.bridge.is_empty() && n.bridge == c))
        {
            return Some(r);
        }
    }
    offered.first().map(|(r, _)| (*r).clone())
}

/// The SR disks go on by default: the pool's default SR when offered, else
/// the first offered.
pub fn default_sr(
    model: &ConnectionModel,
    offered: &[(&Ref<Sr>, &Arc<SrRecord>)],
) -> Option<Ref<Sr>> {
    let default = model.pool().map(|(_, p)| p.default_sr.clone());
    offered
        .iter()
        .find(|(r, _)| Some(*r) == default.as_ref())
        .or_else(|| offered.first())
        .map(|(r, _)| (*r).clone())
}

/// The free room of an SR (its size less what it uses).
pub fn sr_free(sr: &SrRecord) -> u64 {
    (sr.physical_size - sr.physical_utilisation).max(0) as u64
}

/// `SR.DISK_MAX_SIZE` (2 TiB): larger disks cannot go into an OVF.
pub const OVF_DISK_MAX: i64 = 2 << 40;

/// `ExportSelectVMsPage`: the VM's disks add up to more than an OVF
/// takes.
pub fn too_large_for_ovf(model: &ConnectionModel, vm: &VmRecord) -> bool {
    crate::xva::export_space(model, vm) > OVF_DISK_MAX
}

/// The VM has a vTPM (its data is not kept in an OVF).
pub fn has_vtpm(vm: &VmRecord) -> bool {
    !vm.vtpms.is_empty()
}

/// `ApplianceExistsCheck`: `dir/name/name.ovf` or `dir/name/name.ova`
/// (or `.ova.gz`) exists.
pub fn appliance_exists(dir: &Path, name: &str) -> bool {
    let d = dir.join(name);
    ["ovf", "ova", "ova.gz"]
        .iter()
        .any(|e| d.join(format!("{name}.{e}")).exists())
}

/// `ImageVMConfigPage` limits: memory from 256 MiB to 1 TiB, in MB.
pub const IMAGE_MEMORY_MIN_MB: i64 = 256;
pub const IMAGE_MEMORY_MAX_MB: i64 = 1 << 20;
/// CPUs from 1 to 256.
pub const IMAGE_CPUS_MAX: i64 = 256;

/// The boot mode of a disk image's VM (`ImportBootOptionPage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageBoot {
    Bios,
    Uefi,
    SecureUefi,
}

impl ImageBoot {
    /// (boot parameters, platform keys) added to the envelope.
    pub fn settings(self) -> (&'static str, &'static str) {
        match self {
            Self::Bios => ("", ""),
            Self::Uefi => ("firmware=uefi;", ""),
            Self::SecureUefi => ("firmware=uefi;", "secureboot=true;"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        assert_eq!(source_kind("a.XVA"), SourceKind::Xva { compressed: false });
        assert_eq!(
            source_kind("a.xva.gz"),
            SourceKind::Xva { compressed: true }
        );
        assert_eq!(
            source_kind("/x/a.ova.gz"),
            SourceKind::Ovf {
                ova: true,
                compressed: true
            }
        );
        assert_eq!(
            source_kind("a.ovf"),
            SourceKind::Ovf {
                ova: false,
                compressed: false
            }
        );
        for i in [
            "a.vhd", "a.vmdk", "a.iso", "a.img", "a.raw", "a.qcow2", "a.vhdx", "a.wim",
        ] {
            assert_eq!(source_kind(i), SourceKind::Image, "{i}");
        }
        assert_eq!(source_kind("a.txt"), SourceKind::Unknown);
    }

    #[test]
    fn rows_and_defaults() {
        let vm = ovf::ExportedVm {
            id: "s1".into(),
            name: "vm".into(),
            vcpus_at_startup: 1,
            vcpus_max: 1,
            memory_mib: 512,
            nics: vec![(
                "net-uuid-2".into(),
                "B".into(),
                String::new(),
                "02:00:00:00:00:01".into(),
            )],
            disks: vec![ovf::ExportedDisk {
                disk_id: "d1".into(),
                file: "d1.vhd".into(),
                name: "root".into(),
                capacity: 5 << 30,
                userdevice: "0".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let env = Envelope::parse(&ovf::export_descriptor("p", &[vm], &[])).unwrap();
        let systems = vec![("s1".to_owned(), "vm".to_owned())];
        let d = disk_rows(&env, &systems);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].label.as_str(), d[0].size), ("root", 5 << 30));
        let n = nic_rows(&env, &systems);
        assert_eq!(n[0].connection, "net-uuid-2");
        assert_eq!(n[0].mac, "02:00:00:00:00:01");
        let a = Arc::new(NetworkRecord {
            uuid: "net-uuid-1".into(),
            name_label: "A".into(),
            ..Default::default()
        });
        let b = Arc::new(NetworkRecord {
            uuid: "net-uuid-2".into(),
            name_label: "B".into(),
            ..Default::default()
        });
        let (ra, rb) = (Ref::new("a"), Ref::new("b"));
        let offered = vec![(&ra, &a), (&rb, &b)];
        assert_eq!(default_network(&n[0], &offered), Some(rb.clone()));
        let other = NicRow {
            connection: "elsewhere".into(),
            ..n[0].clone()
        };
        assert_eq!(default_network(&other, &offered), Some(ra));
    }
}
