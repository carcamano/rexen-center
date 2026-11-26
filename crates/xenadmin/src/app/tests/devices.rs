//! F14a in the window, against the mock with the device fixture: the GPU
//! tab (placement policy, rows, allowed types), the USB tab, the pool and
//! server Properties' GPU page, the VM Properties' GPU and USB pages with
//! their dialogs, the vTPM manager from the VM menu, the memory settings
//! dialog and wizard from the Memory tab, and the external SSH and Remote
//! Desktop consoles (command lines recorded, nothing started).

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::consoles::{self, Platform};
use xen_core::memory::MemoryField;

use super::provisioning::{catch_up, press, select, select_vm, shows, tab};
use super::xva::{open_menu, succeeded, wait_end};
use super::*;
use crate::dev_strings::dev_strings;
use crate::org_strings::org_strings;

const POOL: &str = "OpaqueRef:pool-0001";
const H1: &str = "OpaqueRef:host-0001";
const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const BUILD: &str = "OpaqueRef:vm-build";
const K220: &str = "OpaqueRef:vgpu-type-k220q";
const PA: &str = "OpaqueRef:pgpu-k2-a";
const U2: &str = "OpaqueRef:pusb-2";
const GIB: i64 = 1 << 30;

fn device_window(runtime: &tokio::runtime::Runtime) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start(false));
    server.add_device_fixtures();
    server.set_task_timing(2, std::time::Duration::from_millis(20));
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| {
        shows(h, "Lab Pool")
            && first_conn(h).is_some_and(|c| c.rbac.is_some() && !c.model.pgpus.is_empty())
    });
    h.state_mut().dev.dry_run = true;
    settle(&mut h);
    (server, h)
}

fn conn(h: &Harness<'_, XenAdminApp>) -> ConnectionId {
    first_conn(h).unwrap().id
}

fn ok(h: &mut Harness<'_, XenAdminApp>, title: &str) {
    let status = wait_end(h, title);
    assert!(succeeded(&status), "{title}: {status:?}");
}

fn strs(v: Option<serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

/// Shows a page of the open VM Properties dialog.
fn vm_page(h: &mut Harness<'_, XenAdminApp>, page: crate::org_ui::VmPage) {
    h.state_mut()
        .org
        .vm_properties
        .as_mut()
        .expect("VM Properties")
        .page = page;
    settle(h);
}

fn enabled(h: &Harness<'_, XenAdminApp>, label: &str) -> bool {
    h.query_all_by_label(label)
        .last()
        .is_some_and(|n| !n.accesskit_node().is_disabled())
}

#[test]
fn gpu_and_usb_tabs_and_the_gpu_properties_page() {
    let runtime = runtime();
    let (server, mut h) = device_window(&runtime);
    let d = dev_strings();
    let c = conn(&h);

    // The pool's GPU tab: the placement policy, the K2 row, the Intel row.
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    tab(&mut h, d.tab_gpu);
    assert!(shows(
        &h,
        &fill(d.gpu_placement, &[("policy", d.policy_density)])
    ));
    assert!(shows(&h, "GK104GL [GRID K2]") && shows(&h, "HD Graphics P630"));
    assert!(shows(&h, "GRID K240Q virtual GPU (4 per GPU)"));
    assert!(shows(&h, d.passthrough_whole));
    assert!(shows(&h, "On xcp-host-01:") && shows(&h, "On xcp-host-02:"));
    // Edit the three K2s' allowed types: allow K220Q.
    press(&mut h, d.gpu_edit_selected);
    let dlg = h
        .state()
        .dev
        .gpu_config
        .as_ref()
        .expect("the dialog is open");
    assert_eq!(dlg.pgpus.len(), 3);
    assert!(shows(&h, "GK104GL [GRID K2] (3 GPUs)"));
    // K240Q is in use on the first GPU: its box cannot be cleared.
    let rows = dlg.rows.clone();
    let k240 = rows
        .iter()
        .find(|r| r.0.as_str() == "OpaqueRef:vgpu-type-k240q")
        .unwrap();
    assert!(k240.3);
    for row in &mut h.state_mut().dev.gpu_config.as_mut().unwrap().rows {
        if row.0.as_str() == K220 {
            row.2 = true;
        }
    }
    press(&mut h, d.ok);
    ok(&mut h, d.op_vgpu_config);
    assert!(strs(server.field("pgpu", PA, "enabled_VGPU_types")).contains(&K220.to_owned()));

    // The placement policy's Edit... opens the pool's Properties at GPU.
    press(&mut h, d.gpu_edit);
    let props = h.state_mut().pool.props().expect("properties open");
    assert_eq!(props.page, xen_core::properties::Page::Gpu);
    press(&mut h, d.policy_performance);
    press(&mut h, crate::pool_strings::pool_strings().ok);
    ok(&mut h, d.op_placement);
    assert_eq!(
        server.field(
            "gpu_group",
            "OpaqueRef:gpu-group-k2",
            "allocation_algorithm"
        ),
        Some(json!("breadth_first"))
    );

    // A server: its GPU tab (its own GPUs) and USB tab.
    select(&mut h, Selection::Host(c, Ref::new(H1)));
    tab(&mut h, d.tab_gpu);
    assert!(!shows(&h, "On xcp-host-02:"));
    tab(&mut h, d.tab_usb);
    for label in [
        d.usb_devices,
        "1-1",
        "SanDisk Corp._Cruzer Blade",
        "Vendor: 046d; Product: c52b",
        "Yubico.com_Yubikey 4",
        "db-01",
    ] {
        assert!(shows(&h, label), "{label}");
    }
    // The device used by db-01 cannot be disabled.
    press(&mut h, "2-1");
    assert!(!enabled(&h, d.usb_disable));
    // Enable the Logitech receiver: asked first.
    press(&mut h, "1-2");
    press(&mut h, d.usb_enable);
    assert!(shows(&h, d.usb_confirm_enable));
    press(&mut h, d.usb_yes_enable);
    ok(&mut h, &fill(d.op_usb_enabling, &[("device", "046d:c52b")]));
    assert_eq!(
        server.field("pusb", U2, "passthrough_enabled"),
        Some(json!(true))
    );
    catch_up(&mut h);
    assert!(enabled(&h, d.usb_disable));

    // The server's Properties: integrated GPU pass-through.
    h.state_mut().ops.dialog_requests.push((
        c,
        crate::provision_ui::DialogRequest::Properties(
            xen_core::properties::PropertiesTarget::Host(Ref::new(H1)),
        ),
    ));
    settle(&mut h);
    h.state_mut()
        .pool
        .select_properties_page(xen_core::properties::Page::Gpu);
    settle(&mut h);
    assert!(shows(&h, d.integrated_using));
    press(&mut h, d.integrated_will_not_use);
    press(&mut h, crate::pool_strings::pool_strings().ok);
    ok(&mut h, &fill(d.op_integrated, &[("host", "xcp-host-01")]));
    assert_eq!(
        server.field("host", H1, "display"),
        Some(json!("disable_on_reboot"))
    );
    assert_eq!(
        server.field("pgpu", "OpaqueRef:pgpu-intel", "dom0_access"),
        Some(json!("disable_on_reboot"))
    );
}

#[test]
fn vm_properties_gpu_and_usb_pages() {
    let runtime = runtime();
    let (server, mut h) = device_window(&runtime);
    let d = dev_strings();
    let c = conn(&h);

    // web-01 runs: both pages refuse changes.
    select_vm(&mut h, WEB);
    press(&mut h, org_strings().properties);
    vm_page(&mut h, crate::org_ui::VmPage::Gpu);
    assert!(shows(&h, d.gpu_stop_vm));
    assert!(shows(&h, "GRID K240Q"));
    assert!(!enabled(&h, d.gpu_add));
    vm_page(&mut h, crate::org_ui::VmPage::Usb);
    assert!(shows(&h, d.usb_not_halted));
    assert!(!enabled(&h, d.usb_attach));
    press(&mut h, org_strings().cancel);

    // db-01 (halted): add a K260Q vGPU.
    select_vm(&mut h, DB);
    press(&mut h, org_strings().properties);
    vm_page(&mut h, crate::org_ui::VmPage::Gpu);
    assert!(enabled(&h, d.gpu_add));
    press(&mut h, d.gpu_add);
    assert!(shows(&h, d.add_vgpu_title));
    assert!(shows(&h, "NVIDIA Corporation GK104GL [GRID K2] GPUs"));
    press(&mut h, "    GRID K260Q virtual GPU (2 per GPU)");
    press(&mut h, d.add_vgpu_add);
    assert!(shows(&h, "GRID K260Q"));
    assert!(shows(&h, d.gpu_need_driver) && shows(&h, d.gpu_single));
    press(&mut h, org_strings().ok);
    ok(&mut h, &fill(d.op_set_gpu_vm, &[("vm", "db-01")]));
    let vgpus = strs(server.field("vm", DB, "VGPUs"));
    assert_eq!(vgpus.len(), 1);
    assert_eq!(
        server.field("vgpu", &vgpus[0], "type"),
        Some(json!("OpaqueRef:vgpu-type-k260q"))
    );

    // build-runner (halted): attach the SanDisk stick, then detach it.
    select_vm(&mut h, BUILD);
    press(&mut h, org_strings().properties);
    vm_page(&mut h, crate::org_ui::VmPage::Usb);
    assert!(shows(&h, &format!("{}\n{}", d.page_usb, d.usb_sub_none)));
    press(&mut h, d.usb_attach);
    assert!(shows(&h, d.attach_usb_title) && shows(&h, d.attach_usb_warn3));
    step_until(&mut h, "possible hosts", |h| {
        shows(h, "1-1 SanDisk Corp._Cruzer Blade")
    });
    // Not offered: the disabled receiver, the stick db-01 uses.
    assert!(!shows(&h, "1-2 Vendor: 046d; Product: c52b"));
    assert!(!shows(&h, "2-1 Yubico.com_Yubikey 4"));
    press(&mut h, "1-1 SanDisk Corp._Cruzer Blade");
    press(&mut h, d.usb_attach);
    ok(
        &mut h,
        &fill(
            d.op_vusb_creating,
            &[
                ("device", "SanDisk Corp._Cruzer Blade"),
                ("vm", "build-runner"),
            ],
        ),
    );
    assert_eq!(
        server.field("vm", BUILD, "platform").unwrap()["device-model"],
        json!("qemu-upstream-compat")
    );
    step_until(&mut h, "the VUSB", |h| {
        shows(h, &format!("{}\n{}", d.page_usb, d.usb_sub_one))
    });
    press(&mut h, "1-1");
    press(&mut h, d.usb_detach);
    assert!(shows(&h, d.usb_detach_confirm));
    press(&mut h, d.yes);
    ok(
        &mut h,
        &fill(
            d.op_vusb_deleting,
            &[
                ("device", "SanDisk Corp._Cruzer Blade"),
                ("vm", "build-runner"),
            ],
        ),
    );
    assert!(strs(server.field("vm", BUILD, "VUSBs")).is_empty());
    let _ = c;
}

#[test]
fn vtpm_manager_and_memory_settings() {
    let runtime = runtime();
    let (server, mut h) = device_window(&runtime);
    let d = dev_strings();
    let c = conn(&h);

    // build-runner (UEFI): add a vTPM, then remove it.
    select_vm(&mut h, BUILD);
    open_menu(&mut h, strings().menu_vm);
    press(&mut h, d.vtpm_menu);
    assert!(shows(&h, d.vtpm_none));
    assert!(enabled(&h, d.vtpm_add));
    press(&mut h, d.vtpm_add);
    ok(&mut h, &fill(d.op_vtpm_attach, &[("vm", "build-runner")]));
    step_until(&mut h, "the vTPM", |h| shows(h, d.vtpm_protected));
    assert!(shows(&h, d.vtpm_unique_info));
    assert!(!enabled(&h, d.vtpm_add));
    press(&mut h, d.vtpm_remove);
    assert!(shows(&h, d.vtpm_remove_warning));
    press(&mut h, d.yes);
    ok(&mut h, &fill(d.op_vtpm_remove, &[("vm", "build-runner")]));
    assert!(strs(server.field("vm", BUILD, "VTPMs")).is_empty());
    press(&mut h, d.close);
    assert!(h.state().dev.vtpm.is_none(), "Close closes the manager");

    // web-01's Memory tab: Edit... (Advanced), the static maximum, the
    // question, the reboot.
    select_vm(&mut h, WEB);
    tab(&mut h, crate::tab_strings::tab_strings().tab_memory);
    press(&mut h, d.mem_edit);
    assert!(shows(&h, d.mem_title) && shows(&h, d.mem_rubric_advanced));
    assert!(shows(&h, d.mem_static_max));
    h.state_mut()
        .dev
        .memory
        .as_mut()
        .unwrap()
        .edit
        .as_mut()
        .unwrap()
        .set(MemoryField::StaticMax, 6.0);
    press(&mut h, d.ok);
    assert!(shows(&h, d.mem_confirm_static_max));
    press(&mut h, d.yes);
    ok(&mut h, &fill(d.op_memory, &[("vm", "web-01")]));
    assert_eq!(
        server.field("vm", WEB, "memory_static_max"),
        Some(json!(6 * GIB))
    );
    assert_eq!(server.vm_power_state(WEB), "Running");

    // web-01 boots with BIOS: the menu item is there, disabled.
    select_vm(&mut h, WEB);
    open_menu(&mut h, strings().menu_vm);
    assert!(shows(&h, d.vtpm_menu));
    assert!(!enabled(&h, d.vtpm_menu));
    open_menu(&mut h, strings().menu_vm); // closes it
    assert!(!shows(&h, d.vtpm_menu));

    // The wizard for several VMs: db-01 and build-runner, fixed 2 GiB
    // (db-01's static minimum lowered first: the fixed memory cannot go
    // below the first VM's).
    server.set_field("vm", DB, "memory_static_min", json!(GIB));
    step_until(&mut h, "db-01's minimum", |h| {
        first_conn(h).unwrap().model.vms[&Ref::new(DB)].memory_static_min == GIB
    });
    h.state_mut().ops.dialog_requests.push((
        c,
        crate::provision_ui::DialogRequest::MemorySettings(vec![Ref::new(DB), Ref::new(BUILD)]),
    ));
    settle(&mut h);
    assert!(shows(&h, d.wiz_choose_vms_title));
    press(&mut h, d.next);
    assert!(shows(&h, d.wiz_memory_text) && shows(&h, d.mem_fixed));
    h.state_mut()
        .dev
        .memory
        .as_mut()
        .unwrap()
        .edit
        .as_mut()
        .unwrap()
        .set(MemoryField::Fixed, 2.0);
    press(&mut h, d.finish);
    ok(&mut h, &fill(d.op_memory, &[("vm", "db-01")]));
    ok(&mut h, &fill(d.op_memory, &[("vm", "build-runner")]));
    for vm in [DB, BUILD] {
        assert_eq!(
            server.field("vm", vm, "memory_static_max"),
            Some(json!(2 * GIB))
        );
    }
}

#[test]
fn external_ssh_and_remote_desktop_consoles() {
    let runtime = runtime();
    let (server, mut h) = device_window(&runtime);
    let d = dev_strings();

    // A local listener stands in for the guest's Remote Desktop.
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    h.state_mut().dev.rdp_port = listener.local_addr().unwrap().port();
    server.set_field(
        "vm_guest_metrics",
        "OpaqueRef:vm-guest-metrics-web",
        "other",
        json!({"feature-balloon": "1", "feature-ts": "1", "feature-ts2": "1", "data-ts": "0"}),
    );
    server.set_field(
        "vm_guest_metrics",
        "OpaqueRef:vm-guest-metrics-web",
        "networks",
        json!({"0/ip": "127.0.0.1", "0/ipv4/0": "127.0.0.1"}),
    );
    select_vm(&mut h, WEB);
    tab(&mut h, crate::tab_strings::tab_strings().tab_console);
    step_until(&mut h, "the console buttons", |h| shows(h, d.rdp_turn_on));

    // SSH: the platform's terminal with ssh root@<address>.
    assert!(enabled(&h, d.ssh_button));
    press(&mut h, d.ssh_button);
    let tools = h
        .state()
        .backend
        .state()
        .settings
        .preferences
        .external_tools
        .clone();
    let expected =
        consoles::ssh_command(&tools, Platform::current(), "root", "127.0.0.1", |_| true).unwrap();
    assert_eq!(h.state().dev.launched, vec![expected.to_string()]);
    assert!(shows(
        &h,
        &fill(d.console_launched, &[("command", &expected.to_string())])
    ));

    // Turn on Remote Desktop (asked first), then open it.
    press(&mut h, d.rdp_turn_on);
    assert!(shows(&h, d.rdp_force_enable));
    press(&mut h, d.yes);
    ok(&mut h, &fill(d.op_enable_rdp, &[("vm", "web-01")]));
    step_until(&mut h, "Remote Desktop usable", |h| enabled(h, d.rdp_open));
    press(&mut h, d.rdp_open);
    step_until(&mut h, "the client", |h| h.state().dev.launched.len() == 2);
    let uuid = first_conn(&h).unwrap().model.vms[&Ref::new(WEB)]
        .uuid
        .clone();
    let expected = consoles::rdp_command(
        &tools,
        Platform::current(),
        "127.0.0.1",
        &uuid,
        &std::env::temp_dir(),
    );
    assert_eq!(h.state().dev.launched[1], expected.to_string());

    // A PuTTY without a path: the original's message, then Options.
    let mut prefs = h.state().backend.state().settings.preferences.clone();
    prefs.external_tools.ssh_console = xen_core::settings::SshConsole::Putty;
    h.state().backend.send(Command::SetPreferences(prefs));
    step_until(&mut h, "the preference", |h| {
        h.state()
            .backend
            .state()
            .settings
            .preferences
            .external_tools
            .ssh_console
            == xen_core::settings::SshConsole::Putty
    });
    press(&mut h, d.ssh_button);
    assert!(shows(&h, d.ssh_not_configured));
    press(&mut h, d.ssh_configure);
    assert!(h.state().options_open);
    assert!(shows(&h, d.tools_ssh_group));
    assert_eq!(h.state().dev.launched.len(), 2);
    drop(listener);
}
