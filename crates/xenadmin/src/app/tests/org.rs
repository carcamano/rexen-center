//! F13 in the window, against the mock: the navigation modes and the tree
//! of each view, the Search tab (filters, grouping, columns, Save, Export,
//! Import, the built-in searches), folders, tags and custom fields through
//! the tree menus and the Properties dialogs, the General tab's rows, and
//! drag and drop (into a folder, onto a tag, a VM onto a server).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::org::CustomFieldType;
use xen_core::search::{Filter, GroupingKind, PropertyName, StringMatch};
use xen_core::search_eval::OrgView;

use super::provisioning::{catch_up, press, select, shows, type_into};
use super::xva::{succeeded, wait_end};
use super::*;
use crate::org_strings::org_strings;
use crate::org_ui::NavMode;

const POOL: &str = "OpaqueRef:pool-0001";
const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const H1: &str = "OpaqueRef:host-0001";
const H2: &str = "OpaqueRef:host-0002";
const DEFS: &str = "<CustomFieldDefinitions><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" /></CustomFieldDefinitions>";

/// A mock whose objects already carry folders, tags and a custom field,
/// as XenCenter writes them.
fn org_window(runtime: &tokio::runtime::Runtime) -> (MockServer, Harness<'static, XenAdminApp>) {
    let server = runtime.block_on(MockServer::start(false));
    server.set_field("vm", WEB, "tags", json!(["web", "prod"]));
    server.set_field("vm", DB, "tags", json!(["prod"]));
    let mut web_oc = server.field("vm", WEB, "other_config").unwrap_or(json!({}));
    web_oc["folder"] = json!("/Prod/Web");
    web_oc["XenCenter.CustomFields.Owner"] = json!("ops");
    server.set_field("vm", WEB, "other_config", web_oc);
    let mut pool_oc = server
        .field("pool", POOL, "other_config")
        .unwrap_or(json!({}));
    pool_oc["EMPTY_FOLDERS"] = json!("/Spare");
    server.set_field("pool", POOL, "other_config", pool_oc);
    let mut gui = server
        .field("pool", POOL, "gui_config")
        .unwrap_or(json!({}));
    gui["XenCenter.CustomFields"] = json!(DEFS);
    server.set_field("pool", POOL, "gui_config", gui);
    server.set_field("host", H2, "enabled", json!(true));
    server.set_task_timing(2, std::time::Duration::from_millis(20));
    let mut h = app_harness(runtime);
    connect_through_dialog(&mut h, &server.address(), xapi_mock::PASSWORD);
    step_until(&mut h, "tree", |h| {
        shows(h, "Lab Pool") && first_conn(h).is_some_and(|c| c.rbac.is_some())
    });
    settle(&mut h);
    (server, h)
}

fn conn(h: &Harness<'_, XenAdminApp>) -> ConnectionId {
    first_conn(h).unwrap().id
}

fn mode(h: &mut Harness<'_, XenAdminApp>, m: NavMode) {
    h.state_mut().org.mode = m;
    h.state_mut().selection = None;
    settle(h);
}

#[test]
fn navigation_views_build_their_trees() {
    let runtime = runtime();
    let (_server, mut h) = org_window(&runtime);
    let s = org_strings();

    // The buttons switch modes.
    press(&mut h, s.nav_objects);
    assert_eq!(h.state().org.mode, NavMode::View(OrgView::Objects));
    assert!(shows(&h, s.view_objects));
    assert!(shows(&h, s.type_vms) && shows(&h, s.type_servers) && shows(&h, s.type_networks));
    assert!(shows(&h, "web-01"));
    press(&mut h, s.nav_organization);
    press(&mut h, s.nav_tags);
    assert_eq!(h.state().org.mode, NavMode::View(OrgView::Tags));
    assert!(shows(&h, s.view_tags) && shows(&h, "prod") && shows(&h, "web"));
    assert!(shows(&h, "db-01"));

    mode(&mut h, NavMode::View(OrgView::Folders));
    for label in [s.view_folders, "Prod", "Web", "Spare", "web-01"] {
        assert!(shows(&h, label), "{label}");
    }
    mode(&mut h, NavMode::View(OrgView::CustomFields));
    assert!(shows(&h, "Owner") && shows(&h, "ops"));
    mode(&mut h, NavMode::View(OrgView::VApps));
    assert!(shows(&h, s.view_vapps));

    // A saved search's tree: VMs by Power State.
    press(&mut h, s.nav_saved);
    press(&mut h, s.search_vms_by_power_state);
    assert!(matches!(h.state().org.mode, NavMode::Saved(_)));
    assert!(shows(&h, "Running") && shows(&h, "Halted") && shows(&h, "web-01"));

    // The tree's search box narrows a view.
    mode(&mut h, NavMode::View(OrgView::Tags));
    h.state_mut().tree_filter = "db".into();
    settle(&mut h);
    assert!(shows(&h, "db-01"));
    assert!(!shows(&h, "web-01"));
    h.state_mut().tree_filter.clear();
    settle(&mut h);
    h.get_all_by_label(s.nav_infrastructure)
        .next()
        .unwrap()
        .click();
    settle(&mut h);
    assert_eq!(h.state().org.mode, NavMode::Infrastructure);
    assert!(shows(&h, "xcp-host-01"));
}

#[test]
fn search_tab_filters_groups_saves_exports_and_imports() {
    let runtime = runtime();
    let (server, mut h) = org_window(&runtime);
    let s = org_strings();
    let c = conn(&h);
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    click_tab(&mut h, s.tab_search);
    settle(&mut h);
    assert!(shows(&h, "Lab Pool: Overview"));
    for col in [
        s.col_name,
        s.col_cpu,
        s.col_memory,
        s.col_disks,
        s.col_network,
        s.col_address,
        s.col_uptime,
        "Owner",
    ] {
        assert!(shows(&h, col), "{col}");
    }
    assert!(shows(&h, "web-01") && shows(&h, "db-01") && shows(&h, "xcp-host-01"));
    // The custom field column has its value.
    assert!(shows(&h, "ops"));

    // Edit: filter on the name, group by power state.
    press(&mut h, s.edit_search);
    assert!(shows(&h, s.search_for) && shows(&h, s.filters) && shows(&h, s.group_by));
    {
        let panel = &mut h.state_mut().org.search;
        panel.search.query.filter = Some(Filter::String {
            property: PropertyName::Label,
            query: "-01".into(),
            kind: StringMatch::EndsWith,
            case_sensitive: false,
        });
        panel.search.grouping = xen_core::search::Grouping::from_levels(&[GroupingKind::Property(
            PropertyName::PowerState,
        )]);
    }
    settle(&mut h);
    assert!(shows(&h, "- Running") && shows(&h, "- Halted"));
    let found = |h: &Harness<'_, XenAdminApp>| -> Vec<String> {
        let state = h.state().backend.state();
        let w = crate::search_ui::world(&state);
        let mut v: Vec<String> = w
            .group(&h.state().org.search.search)
            .iter()
            .flat_map(|n| n.objects())
            .map(|o| w.name(o))
            .collect();
        v.sort();
        v
    };
    assert_eq!(found(&h), vec!["db-01", "web-01", "xcp-host-01"]);
    // Sorting by name and collapsing a group.
    press(&mut h, s.col_name);
    assert_eq!(h.state().org.search.sort, Some(("name".into(), true)));
    press(&mut h, "- Running");
    assert!(shows(&h, "+ Running"));
    press(&mut h, "+ Running");

    // Save on the pool's gui_config.
    press(&mut h, s.save);
    assert!(shows(&h, s.save_title));
    // The name starts as the tab's title.
    match h.state_mut().org.dialog.as_mut() {
        Some(crate::org_ui::OrgDialog::SaveSearch { name, .. }) => {
            assert_eq!(name, "Lab Pool: Overview");
            name.clear();
        }
        _ => panic!("save dialog"),
    }
    settle(&mut h);
    type_into(&mut h, s.save_name, "Ending in 01");
    press(&mut h, s.save_button);
    assert!(succeeded(&wait_end(&mut h, "Save Search 'Ending in 01'")));
    let gui = server.field("pool", POOL, "gui_config").unwrap();
    let (key, xml) = gui
        .as_object()
        .unwrap()
        .iter()
        .find(|(k, _)| k.starts_with("xensearch.Search-"))
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .expect("saved search");
    let saved = xen_core::search::load_search(&xml).unwrap();
    assert_eq!(saved.name, "Ending in 01");
    assert_eq!(
        saved.uuid.as_deref(),
        Some(&key["xensearch.Search-".len()..])
    );
    assert!(xml.contains("<StringPropertyQuery property=\"label\" casesensitive=\"no\" query=\"-01\" type=\"endswith\" />"));
    assert!(xml.contains("<PropertyGrouping property=\"power_state\" />"));
    assert!(xml.contains("<sort column=\"name\" ascending=\"yes\" />"));
    catch_up(&mut h);
    // It is in the Saved Searches menu and mode (the navigation pane's
    // button, not the Search tab's menu of the same name).
    h.get_all_by_label(s.nav_saved).next().unwrap().click();
    settle(&mut h);
    assert!(shows(&h, "Ending in 01"));
    h.get_all_by_label("Ending in 01")
        .find(|n| n.accesskit_node().role() != egui::accesskit::Role::Label)
        .expect("menu item")
        .click_accesskit();
    settle(&mut h);
    assert!(matches!(h.state().org.mode, NavMode::Saved(_)));

    // Export, then import into the tab.
    let dir = std::env::temp_dir().join(format!("xenadmin-f13-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("exported.xensearch");
    mode(&mut h, NavMode::Infrastructure);
    select(&mut h, Selection::Pool(c, Ref::new(POOL)));
    click_tab(&mut h, s.tab_search);
    settle(&mut h);
    press(&mut h, s.export);
    // The native Save dialog fills the field (cancelled first: unchanged).
    crate::file_dialog::tests::answer_next(None);
    press(&mut h, strings().browse);
    crate::file_dialog::tests::answer_next(Some(file.to_str().unwrap()));
    press(&mut h, strings().browse);
    assert_eq!(
        crate::file_dialog::tests::opened(),
        ["save Saved Searches", "save Saved Searches"]
    );
    press(&mut h, s.ok);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<!DOCTYPE xensearch"));
    assert!(text.contains("<Searches xmlns=\"http://www.citrix.com/XenCenter/XenSearch/schema\">"));
    let mut imported = xen_core::search::load_searches(&text).unwrap().remove(0);
    imported.name = "From a file".into();
    let other = dir.join("other.xensearch");
    std::fs::write(&other, xen_core::search::searches_to_xml(&[imported])).unwrap();
    press(&mut h, s.import);
    type_into(&mut h, s.file_label, other.to_str().unwrap());
    press(&mut h, s.ok);
    assert!(shows(&h, "From a file"));
    // A bad file is reported.
    press(&mut h, s.import);
    type_into(
        &mut h,
        s.file_label,
        dir.join("missing.xensearch").to_str().unwrap(),
    );
    press(&mut h, s.ok);
    assert!(
        h.query_all_by_label_contains("was unable to import a saved search")
            .next()
            .is_some()
    );
    press(&mut h, s.cancel);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folders_tags_and_custom_fields_from_the_window() {
    let runtime = runtime();
    let (server, mut h) = org_window(&runtime);
    let s = org_strings();
    let c = conn(&h);

    // The General tab shows folder, tags and the custom field.
    select(&mut h, Selection::Vm(c, Ref::new(WEB)));
    assert!(shows(&h, "prod, web"));
    assert!(shows(&h, "Prod › Web"));
    // Its section starts collapsed, as every section but General.
    assert!(shows(&h, s.page_custom_fields));
    press(&mut h, s.page_custom_fields);
    assert!(shows(&h, "Owner") && shows(&h, "ops"));

    // The VM Properties dialog: rename, a new folder, a tag, a custom field.
    press(&mut h, s.properties);
    assert!(h.state().org.vm_properties.is_some());
    {
        let d = h.state_mut().org.vm_properties.as_mut().unwrap();
        d.description = "web front end".into();
        d.editor.folder = Some("/Ops/Front".into());
        d.editor.tags.push("blue".into());
        d.editor.tags.retain(|t| t != "prod");
        d.page = crate::org_ui::VmPage::CustomFields;
    }
    settle(&mut h);
    {
        let d = h.state_mut().org.vm_properties.as_mut().unwrap();
        d.editor.fields[0].text = "infra".into();
    }
    press(&mut h, s.ok);
    assert!(succeeded(&wait_end(&mut h, "Save folder and tags")));
    assert!(succeeded(&wait_end(&mut h, "Save Custom Fields")));
    assert!(succeeded(&wait_end(&mut h, "Saving properties of web-01")));
    let oc = server.field("vm", WEB, "other_config").unwrap();
    assert_eq!(oc["folder"], json!("/Ops/Front"));
    assert_eq!(oc["XenCenter.CustomFields.Owner"], json!("infra"));
    assert_eq!(
        server.field("vm", WEB, "tags"),
        Some(json!(["web", "blue"]))
    );
    assert_eq!(
        server.field("vm", WEB, "name_description"),
        Some(json!("web front end"))
    );
    // /Prod/Web lost its only VM: kept as an empty folder, like XenCenter.
    let pool_oc = server.field("pool", POOL, "other_config").unwrap();
    assert_eq!(pool_oc["EMPTY_FOLDERS"], json!("/Spare;/Prod/Web"));
    catch_up(&mut h);

    // A new custom field from the Custom Fields page (pool gui_config).
    h.state_mut().org.dialog = Some(crate::org_ui::OrgDialog::NewCustomField {
        connection: c,
        name: String::new(),
        kind: CustomFieldType::Date,
    });
    settle(&mut h);
    type_into(&mut h, s.field_name, "Review");
    press(&mut h, s.ok);
    assert!(succeeded(&wait_end(&mut h, "Add custom field 'Review'")));
    let gui = server.field("pool", POOL, "gui_config").unwrap();
    assert_eq!(
        gui["XenCenter.CustomFields"],
        json!(
            "<CustomFieldDefinitions><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" /><CustomFieldDefinition name=\"Review\" type=\"Date\" defaultValue=\"\" /></CustomFieldDefinitions>"
        )
    );
    catch_up(&mut h);

    // The SR and pool Properties dialogs have the folder and the page.
    select(&mut h, Selection::Sr(c, Ref::new("OpaqueRef:sr-nfs")));
    press(&mut h, crate::sr_strings::sr_strings().properties);
    h.state_mut().storage.sr_properties_mut().unwrap().tab =
        crate::storage_ui::PropertiesTab::CustomFields;
    settle(&mut h);
    assert!(shows(&h, "Owner") && shows(&h, "Review"));
    {
        let d = h
            .state_mut()
            .storage
            .sr_properties_mut()
            .expect("properties");
        d.org
            .fields
            .iter_mut()
            .find(|f| f.definition.name == "Review")
            .unwrap()
            .text = "2026-12-24 08:00:00".into();
        d.org.folder = Some("/Storage".into());
    }
    h.get_by_label(crate::sr_strings::sr_strings().ok)
        .click_accesskit();
    settle(&mut h);
    assert!(succeeded(&wait_end(&mut h, "Save Custom Fields")));
    let sr_oc = server
        .field("sr", "OpaqueRef:sr-nfs", "other_config")
        .unwrap();
    assert_eq!(
        sr_oc["XenCenter.CustomFields.Review"],
        json!("2026-12-24 08:00:00Z")
    );
    assert!(succeeded(&wait_end(&mut h, "Save folder and tags")));
    assert_eq!(
        server
            .field("sr", "OpaqueRef:sr-nfs", "other_config")
            .unwrap()["folder"],
        json!("/Storage")
    );
    catch_up(&mut h);

    // Folder menus in the Folders view: New Folder, Rename, Delete.
    mode(&mut h, NavMode::View(OrgView::Folders));
    right_click(&mut h, s.view_folders);
    press(&mut h, s.new_folder);
    type_into(&mut h, s.new_folder_name, "Lab");
    press(&mut h, s.ok);
    assert!(succeeded(&wait_end(&mut h, "Create new folder '/Lab'")));
    assert_eq!(
        server.field("pool", POOL, "other_config").unwrap()["EMPTY_FOLDERS"],
        json!("/Prod/Web;/Spare;/Lab")
    );
    catch_up(&mut h);
    right_click(&mut h, "Lab");
    press(&mut h, s.rename_folder);
    h.state_mut().org.dialog = Some(crate::org_ui::OrgDialog::RenameFolder {
        path: "/Lab".into(),
        name: "Lab2".into(),
    });
    settle(&mut h);
    press(&mut h, s.rename);
    assert!(succeeded(&wait_end(
        &mut h,
        "Renaming folder 'Lab' to 'Lab2'..."
    )));
    catch_up(&mut h);
    assert!(shows(&h, "Lab2"));
    right_click(&mut h, "Front");
    press(&mut h, s.delete_folder);
    press(&mut h, s.delete);
    assert!(succeeded(&wait_end(&mut h, "Deleting object...")));
    assert!(
        server
            .field("vm", WEB, "other_config")
            .unwrap()
            .get("folder")
            .is_none()
    );
    catch_up(&mut h);

    // Tag menus: Rename and Delete everywhere; Untag one object.
    mode(&mut h, NavMode::View(OrgView::Tags));
    right_click(&mut h, "prod");
    press(&mut h, s.rename_tag);
    h.state_mut().org.dialog = Some(crate::org_ui::OrgDialog::RenameTag {
        tag: "prod".into(),
        name: "production".into(),
    });
    settle(&mut h);
    press(&mut h, s.rename);
    assert!(succeeded(&wait_end(&mut h, "Rename tag 'prod'")));
    assert_eq!(server.field("vm", DB, "tags"), Some(json!(["production"])));
    catch_up(&mut h);
    right_click(&mut h, "blue");
    press(&mut h, s.delete_tag);
    press(&mut h, s.delete);
    assert!(succeeded(&wait_end(&mut h, "Delete tag 'blue'")));
    assert_eq!(server.field("vm", WEB, "tags"), Some(json!(["web"])));
}

/// Drags the last node labelled `from` onto the last one labelled `to`.
fn drag(h: &mut Harness<'_, XenAdminApp>, from: &str, to: &str) {
    let a = h.get_all_by_label(from).last().unwrap().rect().center();
    let b = h.get_all_by_label(to).last().unwrap().rect().center();
    h.hover_at(a);
    h.step();
    h.drag_at(a);
    h.step();
    for i in 1..=6 {
        h.hover_at(a + (b - a) * (i as f32 / 6.0));
        h.step();
    }
    h.drop_at(b);
    settle(h);
}

#[test]
fn drag_and_drop_into_folders_onto_tags_and_onto_servers() {
    let runtime = runtime();
    let (server, mut h) = org_window(&runtime);

    // Into a folder (Folders view): db-01 into Spare.
    mode(&mut h, NavMode::View(OrgView::Folders));
    // db-01 is in no folder, so it is not in this view: drag web-01.
    drag(&mut h, "web-01", "Spare");
    assert!(succeeded(&wait_end(
        &mut h,
        "Move object 'web-01' to folder 'Spare'..."
    )));
    assert_eq!(
        server.field("vm", WEB, "other_config").unwrap()["folder"],
        json!("/Spare")
    );
    // Spare left the empty list, /Prod/Web joined it.
    assert_eq!(
        server.field("pool", POOL, "other_config").unwrap()["EMPTY_FOLDERS"],
        json!("/Prod/Web")
    );
    catch_up(&mut h);

    // Onto a tag (Tags view): db-01 onto "web".
    mode(&mut h, NavMode::View(OrgView::Tags));
    drag(&mut h, "db-01", "web");
    assert!(succeeded(&wait_end(&mut h, "Add tag 'web'")));
    assert_eq!(server.field("vm", DB, "tags"), Some(json!(["prod", "web"])));
    catch_up(&mut h);

    // A running VM onto the other server (Infrastructure): live migration
    // through the existing path.
    mode(&mut h, NavMode::Infrastructure);
    drag(&mut h, "web-01", "xcp-host-02");
    // The original's migration question.
    press(&mut h, "Yes");
    step_until(&mut h, "migrated", |_| {
        server.field("vm", WEB, "resident_on") == Some(json!(H2))
    });
    let _ = H1;
}
