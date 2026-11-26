//! F13 on a real host, through the real window code (ignored test, run
//! with the `XENADMIN_REAL_*` variables of the other real-host tests and
//! `XENADMIN_REAL_LOG` outside the repository).
//!
//! `real_f13_views_search_and_test_vm_folder_tags` reads everything and
//! writes nothing but its disposable VM, named [`TEST`] and refused if one
//! already exists:
//!
//! 1. read only: every navigation view is built (Objects, Tags, Folders,
//!    Custom Fields, vApps) and every built-in search run; the Search tab
//!    of the pool (or server) shows its overview; the folders, tags,
//!    custom field definitions and saved searches XenCenter left on the
//!    server are listed;
//! 2. the VM [`TEST`] is made from the "Other install media" template
//!    with one 64 MiB disk on "Local storage", never started;
//! 3. its VM Properties dialog puts it in folder [`FOLDER`] and tags it
//!    [`TAG`] (checked first: the plan writes only the VM, since no folder
//!    of that name is listed empty on the pool); the Folders and Tags views
//!    and a tag search show it;
//! 4. the tag is removed again in the Properties dialog (one
//!    `VM.remove_tags`);
//! 5. the folder is cleared with the plan's VM call only
//!    (`VM.remove_from_other_config`): the product's "Remove from folder"
//!    would also keep the emptied folder in the pool's `EMPTY_FOLDERS`, a
//!    pool write this run must not make — that step is shown on the mock;
//! 6. the VM is deleted with its disk.
//!
//! The pool record (its `other_config` and `gui_config`) must come out as
//! it went in. Custom field values need a definition on the pool (a pool
//! write), so they are only exercised on the mock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::Value;
use xapi::{
    ClientOptions, Endpoint, JsonRpcClient, Ref, Session, Sr, TrustStore, Vm, VmPowerState,
};
use xapi_mock::MockServer;
use xen_core::ActionRequest;
use xen_core::org::{FolderItem, OrgObject, OrgRequest};
use xen_core::provisioning::{DiskSource, InstallMedia, NewVmDisk, NewVmSpec, NewVmVif};
use xen_core::search::{Filter, ObjectTypes, PropertyName, Query};
use xen_core::search_eval::{NodeItem, ObjKey, OrgView};

use super::*;
use crate::org_strings::org_strings;
use crate::org_ui::NavMode;

/// The test VM.
const TEST: &str = "xenadmin-rs-test-f13";
/// Its folder (no folder of that name may exist).
const FOLDER: &str = "/xenadmin-rs-test-f13";
/// Its tag (no object may carry it).
const TAG: &str = "xenadmin-rs-test-f13";
const TEST_SR: &str = "Local storage";
const DISK: i64 = 64 << 20;

/// How the run opens its own session for the folder's VM-only call.
pub(super) struct Login {
    pub address: String,
    pub user: String,
    pub password: String,
    pub fingerprint: Option<xapi::Fingerprint>,
}

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

fn conn(h: &Harness<'_, XenAdminApp>) -> Arc<xen_core::ConnectionView> {
    first_conn(h).expect("connected")
}

fn step_for(
    h: &mut Harness<'_, XenAdminApp>,
    limit: Duration,
    done: impl Fn(&Harness<'_, XenAdminApp>) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        h.step();
        if done(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn last_op(h: &Harness<'_, XenAdminApp>) -> u64 {
    h.state()
        .backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0)
}

fn wait_op(
    h: &mut Harness<'_, XenAdminApp>,
    prefix: &str,
    after: u64,
    limit: Duration,
) -> xen_core::Operation {
    let found = |h: &Harness<'_, XenAdminApp>| {
        h.state()
            .backend
            .state()
            .operations
            .iter()
            .find(|o| {
                o.id > after
                    && !o.is_running()
                    && i18n::operation_title(&o.kind, &o.server).starts_with(prefix)
            })
            .map(|o| (**o).clone())
    };
    assert!(
        step_for(h, limit, |h| found(h).is_some()),
        "timed out waiting for {prefix}"
    );
    step_for(h, Duration::from_millis(600), |_| false);
    found(h).unwrap()
}

fn ok(op: &xen_core::Operation) {
    assert!(
        matches!(op.status, xen_core::OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

fn test_vms(h: &Harness<'_, XenAdminApp>) -> Vec<Ref<Vm>> {
    conn(h)
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.name_label.starts_with(TEST))
        .map(|(r, _)| r.clone())
        .collect()
}

fn disks(h: &Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> Vec<Ref<xapi::Vdi>> {
    let c = conn(h);
    let Some(v) = c.model.vms.get(vm) else {
        return Vec::new();
    };
    v.vbds
        .iter()
        .filter_map(|b| c.model.vbds.get(b))
        .filter(|b| b.r#type != xapi::VbdType::Cd)
        .map(|b| b.vdi.clone())
        .filter(|d| c.model.vdis.contains_key(d))
        .collect()
}

/// Deletes the test VM with its disk (refused for anything else).
fn delete_vm(h: &mut Harness<'_, XenAdminApp>, vm: &Ref<Vm>) -> xen_core::Operation {
    let disks = disks(h, vm);
    {
        let c = conn(h);
        let rec = &c.model.vms[vm];
        assert!(rec.name_label.starts_with(TEST), "refusing: not a test VM");
        assert_eq!(
            rec.power_state,
            VmPowerState::Halted,
            "refusing: not halted"
        );
        assert!(rec.snapshots.is_empty(), "refusing: a VM with snapshots");
        for d in &disks {
            let vdi = &c.model.vdis[d];
            assert_eq!(c.model.srs[&vdi.sr].name_label, TEST_SR);
            assert!(
                vdi.name_label.starts_with(TEST),
                "refusing: a disk not named {TEST}*"
            );
            assert_eq!(vdi.vbds.len(), 1, "refusing: a shared disk");
        }
    }
    let before = last_op(h);
    h.state().backend.send(xen_core::Command::Action {
        connection: conn(h).id,
        request: ActionRequest::DeleteVm {
            vm: vm.clone(),
            disks,
            snapshots: Vec::new(),
        },
    });
    wait_op(h, "Deleting VM", before, Duration::from_secs(300))
}

/// The pool's `other_config` and `gui_config`, which must not change.
fn pool_maps(h: &Harness<'_, XenAdminApp>) -> String {
    let c = conn(h);
    let (_, p) = c.model.pool().expect("pool");
    let mut oc: Vec<(&String, &String)> = p.other_config.iter().collect();
    oc.sort();
    let mut gui: Vec<(&String, &String)> = p.gui_config.iter().collect();
    gui.sort();
    format!("other_config {oc:?} gui_config {gui:?}")
}

/// Waits until the cache has caught up.
fn until(
    h: &mut Harness<'_, XenAdminApp>,
    what: &str,
    done: impl Fn(&xen_core::ConnectionModel) -> bool,
) {
    assert!(
        step_for(h, Duration::from_secs(60), |h| done(&conn(h).model)),
        "timed out: {what}"
    );
}

/// The objects (not groups) under some nodes.
fn count_objects(nodes: &[xen_core::search_eval::Node]) -> usize {
    nodes.iter().map(|n| n.objects().len()).sum()
}

pub(super) fn f13_run(
    h: &mut Harness<'_, XenAdminApp>,
    template: &str,
    login: &Login,
    runtime: &tokio::runtime::Runtime,
) -> String {
    let s = org_strings();
    let mut log = String::new();
    let cn = conn(h);
    assert!(
        test_vms(h).is_empty(),
        "refusing: a {TEST}* VM already exists"
    );
    assert!(
        !cn.model
            .vdis
            .values()
            .any(|v| v.name_label.starts_with(TEST)),
        "refusing: a {TEST}* disk already exists"
    );
    assert!(
        !xen_core::org::FolderIndex::build(&cn.model).contains(FOLDER),
        "refusing: folder {FOLDER} exists"
    );
    assert!(
        xen_core::org::tag_users(&cn.model, TAG).is_empty(),
        "refusing: tag {TAG} is in use"
    );
    let pool_before = pool_maps(h);

    // 1. Read only: what XenCenter left, every view and built-in search.
    let index = xen_core::org::FolderIndex::build(&cn.model);
    note(
        &mut log,
        format!(
            "XenCenter data on the server: folders {:?}, empty-folder list {:?}, tags {:?}, custom fields {:?}, saved searches {}",
            index
                .folders
                .iter()
                .filter(|f| *f != "/")
                .collect::<Vec<_>>(),
            xen_core::org::empty_folders(&cn.model),
            xen_core::org::tag_counts(&cn.model),
            xen_core::org::definitions(&cn.model)
                .iter()
                .map(|d| format!("{} ({})", d.name, d.kind.as_str()))
                .collect::<Vec<_>>(),
            cn.model
                .pool()
                .map(|(_, p)| p
                    .gui_config
                    .keys()
                    .filter(|k| k.starts_with(xen_core::org::SEARCH_KEY_PREFIX))
                    .count())
                .unwrap_or(0)
        ),
    );
    for (v, button, root) in [
        (OrgView::Objects, s.nav_objects, s.view_objects),
        (OrgView::Tags, s.nav_tags, s.view_tags),
        (OrgView::Folders, s.nav_folders, s.view_folders),
        (OrgView::CustomFields, s.nav_fields, s.view_fields),
        (OrgView::VApps, s.nav_vapps, s.view_vapps),
    ] {
        h.state_mut().org.mode = NavMode::View(v);
        settle(h);
        assert!(h.query_all_by_label(root).next().is_some(), "{root} tree");
        let state = h.state().backend.state();
        let w = crate::search_ui::world(&state);
        let nodes = w.group(&v.tree_search(""));
        let groups: Vec<String> = nodes
            .iter()
            .map(|n| match &n.item {
                NodeItem::Object(o) => w.name(o),
                NodeItem::Group(t) => crate::search_ui::group_label(&w, t),
            })
            .take(12)
            .collect();
        note(
            &mut log,
            format!(
                "view {button}: {} top nodes {groups:?}, {} objects",
                nodes.len(),
                count_objects(&nodes)
            ),
        );
    }
    for search in crate::search_ui::default_searches() {
        let state = h.state().backend.state();
        let w = crate::search_ui::world(&state);
        let nodes = w.group(&search);
        note(
            &mut log,
            format!(
                "built-in search '{}': {} groups, {} objects",
                search.name,
                nodes.len(),
                count_objects(&nodes)
            ),
        );
        h.state_mut().org.mode = NavMode::Saved(search.uuid.clone().unwrap());
        settle(h);
        assert!(
            h.query_all_by_label(&search.name).next().is_some(),
            "{} tree",
            search.name
        );
    }
    h.state_mut().org.mode = NavMode::Infrastructure;
    let (_, pool) = cn.model.pool().unwrap();
    let host = pool.master.clone();
    let overview = if xen_core::objects::pool_is_visible(&cn.model) {
        Selection::Pool(cn.id, cn.model.pool().unwrap().0.clone())
    } else {
        Selection::Host(cn.id, host.clone())
    };
    h.state_mut().selection = Some(overview);
    h.state_mut().tab = crate::tabs::Tab::Search;
    for _ in 0..6 {
        settle(h);
    }
    assert!(
        h.query_all_by_label(s.col_cpu).next().is_some(),
        "the Search tab's grid"
    );
    // The figures come from rrd_updates every 30 seconds.
    let figures = step_for(h, Duration::from_secs(40), |h| {
        h.query_all_by_label_contains("% of").next().is_some()
    });
    {
        let state = h.state().backend.state();
        let w = crate::search_ui::world(&state);
        let nodes = w.group(&h.state().org.search.search);
        note(
            &mut log,
            format!(
                "Search tab '{}': {} objects; CPU figures shown: {figures}",
                h.state().org.search.title,
                count_objects(&nodes)
            ),
        );
    }
    h.state_mut().tab = crate::tabs::Tab::General;
    settle(h);

    let templates: Vec<_> = cn
        .model
        .vms
        .iter()
        .filter(|(_, v)| v.is_a_template && v.name_label == template)
        .collect();
    assert_eq!(templates.len(), 1, "the template {template:?}");
    let (template_ref, template_rec) = (templates[0].0.clone(), templates[0].1.clone());
    let srs: Vec<Ref<Sr>> = cn
        .model
        .srs
        .iter()
        .filter(|(_, x)| {
            x.name_label == TEST_SR
                && x.pbds
                    .iter()
                    .filter_map(|p| cn.model.pbds.get(p))
                    .any(|p| p.host == host)
        })
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(srs.len(), 1, "one {TEST_SR} on the coordinator");
    let sr = srs[0].clone();
    let management = cn
        .model
        .pifs
        .values()
        .find(|p| p.management && p.host == host)
        .map(|p| p.network.clone())
        .expect("the management network");
    let vdis_on_sr = cn.model.srs[&sr].vdis.len();

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 2. The disposable VM.
        let memory = template_rec.memory_static_max.max(256 << 20);
        let spec = NewVmSpec {
            template: template_ref.clone(),
            name: TEST.to_owned(),
            description: "F13 folder and tag test (disposable)".to_owned(),
            media: InstallMedia::None,
            affinity: Some(host.clone()),
            vcpus_max: 1,
            vcpus_at_startup: 1,
            cores_per_socket: Some(1),
            memory_static_max: memory,
            memory_dynamic_min: memory,
            memory_dynamic_max: memory,
            disks: vec![NewVmDisk {
                source: DiskSource::New,
                system: false,
                userdevice: "0".to_owned(),
                name: format!("{TEST} disk"),
                description: String::new(),
                size: DISK,
                sr: sr.clone(),
                bootable: true,
            }],
            full_copy_sr: None,
            vifs: vec![NewVmVif {
                network: management.clone(),
                mac: String::new(),
            }],
            boot_mode: xen_core::provisioning::default_boot_mode(&template_rec),
            vtpm: false,
            start: false,
        };
        let before = last_op(h);
        {
            let cc = conn(h);
            let app = h.state_mut();
            app.ops
                .request(&cc, ActionRequest::NewVm(Box::new(spec)), &app.backend);
        }
        let op = wait_op(h, "Create VM", before, Duration::from_secs(600));
        ok(&op);
        let vm = op.created.clone().expect("the new VM");
        assert_eq!(conn(h).model.vms[&vm].power_state, VmPowerState::Halted);
        note(
            &mut log,
            format!("created {TEST}: halted, 1 disk of 64 MiB on {TEST_SR}, 1 interface"),
        );

        // 3. Folder and tag through the VM Properties dialog.
        h.state_mut().selection = Some(Selection::Vm(conn(h).id, vm.clone()));
        settle(h);
        h.get_all_by_label(s.properties)
            .last()
            .expect("Properties")
            .click();
        settle(h);
        {
            let d = h
                .state_mut()
                .org
                .vm_properties
                .as_mut()
                .expect("the VM Properties dialog");
            d.editor.folder = Some(FOLDER.into());
            d.editor.tags.push(TAG.into());
        }
        settle(h);
        let requests = {
            let cc = conn(h);
            h.state().org.vm_properties.as_ref().unwrap().requests(&cc)
        };
        let mut planned = Vec::new();
        for r in &requests {
            let ActionRequest::Org(org) = r else {
                panic!("unexpected request {r:?}")
            };
            let calls = org.plan(&conn(h).model).unwrap();
            assert!(
                calls.iter().all(|c| !c.writes_pool()),
                "refusing: the plan writes the pool: {calls:?}"
            );
            planned.extend(calls.iter().map(|c| c.method()));
        }
        assert_eq!(
            planned,
            vec![
                "VM.remove_from_other_config",
                "VM.add_to_other_config",
                "VM.add_tags"
            ]
        );
        note(
            &mut log,
            format!(
                "Properties: folder {FOLDER} and tag {TAG}; planned calls {planned:?} (no pool write)"
            ),
        );
        let before = last_op(h);
        h.get_all_by_label(s.ok).last().unwrap().click_accesskit();
        settle(h);
        ok(&wait_op(
            h,
            s.op_folder_tags,
            before,
            Duration::from_secs(120),
        ));
        until(h, "folder and tag in the cache", |m| {
            m.vms.get(&vm).is_some_and(|v| {
                v.other_config.get("folder").map(String::as_str) == Some(FOLDER)
                    && v.tags.iter().any(|t| t == TAG)
            })
        });
        note(
            &mut log,
            format!("server: other_config[\"folder\"] = {FOLDER:?}, tags = [{TAG:?}]"),
        );
        let folder_name = &FOLDER[1..];
        h.state_mut().org.mode = NavMode::View(OrgView::Folders);
        settle(h);
        assert!(
            h.query_all_by_label(folder_name).next().is_some()
                && h.query_all_by_label(TEST).next().is_some()
        );
        h.state_mut().org.mode = NavMode::View(OrgView::Tags);
        settle(h);
        assert!(
            h.query_all_by_label(TAG).count() >= 1 && h.query_all_by_label(TEST).next().is_some()
        );
        {
            let state = h.state().backend.state();
            let w = crate::search_ui::world(&state);
            let q = Query::new(
                Some(ObjectTypes::ALL_EXC_FOLDERS),
                Some(Filter::ListContainsString {
                    property: PropertyName::Tags,
                    query: TAG.into(),
                    contains: true,
                }),
            );
            let found: Vec<String> = w
                .objects()
                .into_iter()
                .filter(|o| w.query_matches(&q, o))
                .map(|o| w.name(&o))
                .collect();
            assert_eq!(found, vec![TEST.to_owned()]);
        }
        note(
            &mut log,
            "the Folders and Tags views and a tag search show the test VM (and only it)".into(),
        );
        h.state_mut().org.mode = NavMode::Infrastructure;
        settle(h);

        // 4. The tag removed in the Properties dialog.
        h.state_mut().selection = Some(Selection::Vm(conn(h).id, vm.clone()));
        settle(h);
        h.get_all_by_label(s.properties).last().unwrap().click();
        settle(h);
        h.state_mut()
            .org
            .vm_properties
            .as_mut()
            .unwrap()
            .editor
            .tags
            .retain(|t| t != TAG);
        settle(h);
        let calls: Vec<String> = {
            let cc = conn(h);
            let requests = h.state().org.vm_properties.as_ref().unwrap().requests(&cc);
            requests
                .iter()
                .flat_map(|r| match r {
                    ActionRequest::Org(o) => o.plan(&cc.model).unwrap(),
                    _ => Vec::new(),
                })
                .map(|c| c.method())
                .collect()
        };
        assert_eq!(calls, vec!["VM.remove_tags"]);
        let before = last_op(h);
        h.get_all_by_label(s.ok).last().unwrap().click_accesskit();
        settle(h);
        ok(&wait_op(
            h,
            s.op_folder_tags,
            before,
            Duration::from_secs(120),
        ));
        until(h, "tag gone", |m| {
            m.vms.get(&vm).is_some_and(|v| v.tags.is_empty())
        });
        note(&mut log, "tag cleared: VM.remove_tags only".into());

        // 5. The folder cleared with the VM call only.
        let product = OrgRequest::DeleteFromFolders {
            items: vec![FolderItem::Object(OrgObject::Vm(vm.clone()))],
        }
        .plan(&conn(h).model)
        .unwrap();
        assert!(
            product.iter().any(|c| c.writes_pool()),
            "the product's Remove from folder keeps the emptied folder"
        );
        let vm_only: Vec<_> = product.iter().filter(|c| !c.writes_pool()).collect();
        assert_eq!(vm_only.len(), 1);
        note(
            &mut log,
            format!(
                "Remove from folder would make {:?}; only {:?} is made here (the pool step is shown on the mock)",
                product.iter().map(|c| c.method()).collect::<Vec<_>>(),
                vm_only[0].method()
            ),
        );
        runtime.block_on(async {
            let endpoint = Endpoint::parse(&login.address).unwrap();
            let trust = TrustStore::new();
            if let Some(fp) = login.fingerprint {
                trust.trust(&endpoint.authority(), fp);
            }
            let client =
                Arc::new(JsonRpcClient::new(&endpoint, &trust, &ClientOptions::default()).unwrap());
            let session = Session::login_with_password(
                client,
                &login.user,
                &login.password,
                "xenadmin-rs-f13-test",
            )
            .await
            .unwrap();
            xapi::vm::remove_from_other_config(&session, &vm, xen_core::org::FOLDER_KEY)
                .await
                .unwrap();
            let _ = session.logout().await;
        });
        until(h, "folder gone", |m| {
            m.vms
                .get(&vm)
                .is_some_and(|v| !v.other_config.contains_key("folder"))
        });
        note(
            &mut log,
            "folder cleared: VM.remove_from_other_config only".into(),
        );

        // 6. The VM deleted.
        let op = delete_vm(h, &vm);
        ok(&op);
        note(&mut log, format!("deleted {TEST} with its disk"));
    }));
    if outcome.is_err() {
        note(&mut log, "a step failed: removing the test VM".into());
        for vm in test_vms(h) {
            let op = delete_vm(h, &vm);
            note(&mut log, format!("cleanup VM: {:?}", op.status));
        }
    }
    step_for(h, Duration::from_secs(2), |_| false);
    let cn = conn(h);
    let left_vms = test_vms(h).len();
    let left_disks = cn
        .model
        .vdis
        .values()
        .filter(|v| v.name_label.starts_with(TEST))
        .count();
    let pool_after = pool_maps(h);
    note(
        &mut log,
        format!(
            "after: {left_vms} {TEST}* VMs, {left_disks} disks; {TEST_SR} holds {} disks (before {vdis_on_sr}); pool other_config/gui_config unchanged: {}",
            cn.model.srs[&sr].vdis.len(),
            pool_after == pool_before
        ),
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
    assert_eq!((left_vms, left_disks), (0, 0));
    assert_eq!(pool_after, pool_before, "the pool record");
    log
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables); reads every view and search, and sets and clears the folder and a tag of a disposable test VM"]
fn real_f13_views_search_and_test_vm_folder_tags() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let login = Login {
        address: var("XENADMIN_REAL_HOST"),
        user: var("XENADMIN_REAL_USER"),
        password: var("XENADMIN_REAL_PASSWORD"),
        fingerprint: xapi::Fingerprint::parse(&var("XENADMIN_REAL_FINGERPRINT")),
    };
    let runtime = runtime();
    let mut h = real_host_harness(&runtime);
    let log = f13_run(&mut h, "Other install media", &login, &runtime);
    std::fs::write(&log_path, log).expect("write the log");
}

/// The same run on the mock: only the test VM's calls, no pool write,
/// nothing else changed.
#[test]
fn f13_real_run_on_the_mock() {
    let runtime = runtime();
    let server = runtime.block_on(MockServer::start(false));
    server.set_task_timing(2, Duration::from_millis(20));
    let mut h = app_harness(&runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "connected", |h| {
        first_conn(h).is_some_and(|c| c.status.is_connected() && c.rbac.is_some())
    });
    settle(&mut h);
    let snapshot = |s: &MockServer| {
        [
            "vm",
            "vdi",
            "sr",
            "network",
            "vm_appliance",
            "pool",
            "pbd",
            "host",
        ]
        .iter()
        .flat_map(|c| s.refs(c).into_iter().map(move |r| (c.to_string(), r)))
        .map(|(c, r)| {
            let mut rec: Value = s.record(&c, &r).unwrap();
            for f in [
                "physical_utilisation",
                "virtual_allocation",
                "VDIs",
                "VIFs",
                "VBDs",
            ] {
                rec.as_object_mut().unwrap().remove(f);
            }
            (c, r, rec)
        })
        .collect::<Vec<_>>()
    };
    let before = snapshot(&server);
    let calls_before = server.calls().len();
    let login = Login {
        address: server.address(),
        user: xapi_mock::USERNAME.into(),
        password: xapi_mock::PASSWORD.into(),
        fingerprint: None,
    };
    let log = f13_run(&mut h, "Debian Bookworm 12", &login, &runtime);
    assert!(
        log.contains("pool other_config/gui_config unchanged: true"),
        "{log}"
    );
    assert_eq!(snapshot(&server), before, "only the test VM changed");
    let calls: Vec<String> = server.calls()[calls_before..].to_vec();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("pool.") && !c.contains(".get_")),
        "a pool write: {calls:?}"
    );
    let org_calls: Vec<&String> = calls
        .iter()
        .filter(|c| c.contains("_tags") || c.ends_with("gui_config"))
        .collect();
    assert_eq!(org_calls, vec!["VM.add_tags", "VM.remove_tags"]);
    let _ = ObjKey::Folder(String::new());
}
