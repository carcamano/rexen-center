//! F11b in the window, against the mock: the Export wizard's OVF/OVA pages
//! (EULAs, Advanced Options, the summary, the package written), the
//! Import wizard on that OVA (EULA, location, storage, networks, the
//! manifest check, the imported VM), on a disk image (VM definition, boot
//! mode, the new VM) and the formats it refuses.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use egui_kittest::Harness;

use super::provisioning::{catch_up, press, select_vm, shows};
use super::xva::{open_menu, pool_window, succeeded, wait_end};
use super::*;
use crate::ovf_strings::ovf_strings;
use crate::ovf_ui::OvfPage;
use crate::xva_strings::xva_strings;
use crate::xva_ui::{ExportFormat, ExportPage, ImportPage};

const DB: &str = "OpaqueRef:vm-db";
const NFS: &str = "OpaqueRef:sr-nfs";
const VLAN20: &str = "OpaqueRef:net-vlan20";

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("xen-f11b-ui-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn export_page(h: &Harness<'_, XenAdminApp>) -> Option<ExportPage> {
    h.state().xva.export.as_ref().map(|w| w.page)
}

fn ovf_page(h: &Harness<'_, XenAdminApp>) -> Option<OvfPage> {
    h.state().xva.import_ovf.as_ref().map(|w| w.page)
}

/// Exports db-01 as an OVA with a manifest and one EULA into `dir`
/// through the VM menu's wizard.
fn export_db_ova(h: &mut Harness<'_, XenAdminApp>, dir: &Path) {
    let x = xva_strings();
    let o = ovf_strings();
    let n = crate::net_strings::net_strings();
    select_vm(h, DB);
    open_menu(h, strings().menu_vm);
    press(h, x.menu_export);
    assert_eq!(export_page(h), Some(ExportPage::Appliance));
    {
        let w = h.state_mut().xva.export.as_mut().unwrap();
        // OVF/OVA is the default, as the original.
        assert_eq!(w.format, ExportFormat::Ovf);
        w.folder = dir.display().to_string();
    }
    settle(h);
    assert!(shows(h, o.export_ovf_title));
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Vms));
    assert!(shows(h, o.page_vms_title_ovf));
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Eula));
    // An empty EULA file is refused; a text one is added.
    let empty = dir.join("empty.txt");
    std::fs::write(&empty, "  ").unwrap();
    h.state_mut().xva.export.as_mut().unwrap().eula_input = empty.display().to_string();
    press(h, o.eula_add);
    assert!(shows(
        h,
        &fill(o.eula_invalid, &[("file", &empty.display().to_string())])
    ));
    let eula = dir.join("eula.txt");
    std::fs::write(&eula, "You agree to be nice.").unwrap();
    h.state_mut().xva.export.as_mut().unwrap().eula_input = eula.display().to_string();
    press(h, o.eula_add);
    assert_eq!(
        h.state().xva.export.as_ref().unwrap().eulas,
        vec![eula.display().to_string()]
    );
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Options));
    assert!(shows(h, o.sign_unavailable));
    press(h, o.create_manifest);
    press(h, o.create_ova);
    press(h, n.next);
    assert_eq!(export_page(h), Some(ExportPage::Finish));
    assert!(shows(h, o.summary_manifest) && shows(h, o.summary_ova));
    press(h, x.verify);
    press(h, n.finish);
    assert!(export_page(h).is_none());
    assert!(succeeded(&wait_end(
        h,
        "Export OVA package 'db-01' from 'Lab Pool'"
    )));
}

#[test]
fn the_ovf_export_and_import_wizards() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let x = xva_strings();
    let o = ovf_strings();
    let n = crate::net_strings::net_strings();
    let dir = temp_dir();
    export_db_ova(&mut h, &dir);
    let ova = dir.join("db-01").join("db-01.ova");
    assert!(ova.is_file());
    let names: Vec<String> = xen_core::ova::index(&ova)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names.first().map(String::as_str), Some("db-01.ovf"));
    assert_eq!(names.last().map(String::as_str), Some("db-01.mf"));

    // Again: the package exists now.
    select_vm(&mut h, DB);
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, x.menu_export);
    h.state_mut().xva.export.as_mut().unwrap().folder = dir.display().to_string();
    press(&mut h, n.next);
    assert!(shows(&h, o.error_appliance_exists));
    press(&mut h, n.cancel);

    // Import it: File > Import..., the pool selected.
    let vms_before = server.refs("vm").len();
    let c = first_conn(&h).unwrap().id;
    h.state_mut().selection = Some(Selection::Pool(c, Ref::new("OpaqueRef:pool-0001")));
    open_menu(&mut h, strings().menu_file);
    press(&mut h, x.menu_import);
    h.state_mut().xva.import.as_mut().unwrap().path = ova.display().to_string();
    press(&mut h, n.next);
    // The OVF pages take over, on the EULA.
    assert!(h.state().xva.import.is_none());
    assert_eq!(ovf_page(&h), Some(OvfPage::Eula));
    assert!(shows(&h, o.import_ovf_title));
    assert!(shows(&h, "You agree to be nice."));
    assert!(!super::provisioning::last_enabled(&h, n.next));
    press(&mut h, o.eula_accept);
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Location));
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Storage));
    {
        let w = h.state().xva.import_ovf.as_ref().unwrap();
        assert_eq!(w.sr, Some(Ref::new(NFS)), "the pool's default SR");
        assert_eq!(w.disks.len(), 1);
    }
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Network));
    // The interface's network: VLAN 20.
    h.state_mut().xva.import_ovf.as_mut().unwrap().nics[0].1 = Some(Ref::new(VLAN20));
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Security));
    press(&mut h, o.verify_manifest);
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Finish));
    assert!(shows(&h, o.summary_verify_manifest) && shows(&h, "db-01.ova"));
    h.state_mut().xva.import_ovf.as_mut().unwrap().start = false;
    settle(&mut h);
    press(&mut h, n.finish);
    assert!(ovf_page(&h).is_none());
    assert!(succeeded(&wait_end(
        &mut h,
        "Import Appliance 'db-01' to 'Lab Pool'"
    )));
    catch_up(&mut h);
    let vms: Vec<String> = server.refs("vm");
    assert_eq!(vms.len(), vms_before + 1);
    let new: Vec<String> = vms
        .into_iter()
        .filter(|r| r != DB && server.field("vm", r, "name_label").unwrap() == "db-01")
        .collect();
    assert_eq!(new.len(), 1);
    let vif = server.field("vm", &new[0], "VIFs").unwrap()[0]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(server.field("vif", &vif, "network").unwrap(), VLAN20);
    assert!(server.calls().iter().any(|c| c == "http/import_raw_vdi"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_disk_image_import_wizard() {
    let runtime = runtime();
    let (server, mut h) = pool_window(&runtime);
    let x = xva_strings();
    let o = ovf_strings();
    let n = crate::net_strings::net_strings();
    let dir = temp_dir();
    let c = first_conn(&h).unwrap().id;
    h.state_mut().selection = Some(Selection::Pool(c, Ref::new("OpaqueRef:pool-0001")));
    open_menu(&mut h, strings().menu_file);
    press(&mut h, x.menu_import);

    // Formats that need converting are refused with a clear word.
    for (name, magic, format) in [
        ("a.vmdk", &b"KDMV"[..], "VMDK"),
        ("a.vhdx", b"vhdxfile", "VHDX"),
        ("a.qcow2", b"QFI\xfb", "qcow2"),
    ] {
        let p = dir.join(name);
        let mut b = magic.to_vec();
        b.resize(4096, 0);
        std::fs::write(&p, b).unwrap();
        h.state_mut().xva.import.as_mut().unwrap().path = p.display().to_string();
        press(&mut h, n.next);
        assert!(
            shows(
                &h,
                &fill(
                    o.error_needs_conversion,
                    &[
                        ("format", format),
                        ("console", crate::branding::PRODUCT_NAME)
                    ]
                )
            ),
            "{name}"
        );
        assert_eq!(
            h.state().xva.import.as_ref().map(|w| w.page),
            Some(ImportPage::Source)
        );
    }

    // A raw image of 2 MiB.
    let mut raw = vec![0u8; 2 << 20];
    raw[..512].fill(0x5A);
    let img = dir.join("disk.img");
    std::fs::write(&img, &raw).unwrap();
    h.state_mut().xva.import.as_mut().unwrap().path = img.display().to_string();
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::VmDefinition));
    assert!(shows(&h, o.import_image_title));
    // Previous goes back to the source page with the path.
    press(&mut h, n.previous);
    assert_eq!(
        h.state().xva.import.as_ref().map(|w| w.path.clone()),
        Some(img.display().to_string())
    );
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::VmDefinition));
    assert!(
        !super::provisioning::last_enabled(&h, n.next),
        "a name first"
    );
    {
        let w = h.state_mut().xva.import_ovf.as_mut().unwrap();
        w.vm_name = "from-image".into();
        w.cpus = 2;
        w.memory_mb = 512;
    }
    settle(&mut h);
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Location));
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Storage));
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Network));
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Boot));
    press(&mut h, o.boot_uefi);
    press(&mut h, n.next);
    assert_eq!(ovf_page(&h), Some(OvfPage::Finish));
    assert!(shows(&h, o.summary_image) && shows(&h, "from-image"));
    h.state_mut().xva.import_ovf.as_mut().unwrap().start = false;
    settle(&mut h);
    press(&mut h, n.finish);
    assert!(succeeded(&wait_end(
        &mut h,
        "Import VM 'from-image' from Disk Image to 'Lab Pool'"
    )));
    catch_up(&mut h);
    let vm: Vec<String> = server
        .refs("vm")
        .into_iter()
        .filter(|r| server.field("vm", r, "name_label").unwrap() == "from-image")
        .collect();
    assert_eq!(vm.len(), 1);
    let rec = server.record("vm", &vm[0]).unwrap();
    assert_eq!(rec["VCPUs_max"], 2);
    assert_eq!(rec["memory_static_max"], 512 << 20);
    assert_eq!(rec["HVM_boot_params"]["firmware"], "uefi");
    let vbd = rec["VBDs"][0].as_str().unwrap();
    let vdi = server.field("vbd", vbd, "VDI").unwrap();
    let vdi = vdi.as_str().unwrap();
    assert_eq!(
        server.field("vdi", vdi, "name_label").unwrap(),
        "from-image"
    );
    assert_eq!(server.field("vdi", vdi, "virtual_size").unwrap(), 2 << 20);
    assert_eq!(
        server.vdi_digest(vdi),
        Some(xapi_mock::vhd::content_digest(&raw))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
