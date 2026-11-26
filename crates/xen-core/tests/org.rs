//! F13 against the mock (backend only): folders, tags, custom fields and
//! saved searches, asserting the exact keys and values XenCenter writes,
//! the calls made (and when the pool record is written), searches over
//! the live cache, and the Search tab's performance figures.

mod common;

use std::sync::Arc;

use common::{memory_store, spawn};
use serde_json::{Value, json};
use xapi::Ref;
use xapi_mock::MockServer;
use xen_core::org::{
    CustomFieldDefinition, CustomFieldType, CustomFieldValue, FolderItem, OrgObject, OrgRequest,
};
use xen_core::search::{Filter, ObjectTypes, PropertyName, Query, Search, StringMatch};
use xen_core::search_eval::{self, ObjKey, OrgView, Source, World};
use xen_core::{
    ActionRequest, AppState, BackendOptions, Command, ConnectionId, Operation, OperationStatus,
};

const POOL: &str = "OpaqueRef:pool-0001";
const WEB: &str = "OpaqueRef:vm-web";
const DB: &str = "OpaqueRef:vm-db";
const NFS: &str = "OpaqueRef:sr-nfs";

fn harness() -> common::Harness {
    spawn(BackendOptions {
        ..common::fast_options(memory_store().0)
    })
}

async fn connect(h: &common::Harness, server: &MockServer) -> ConnectionId {
    h.connect(server, xapi_mock::PASSWORD);
    let address = server.address();
    let state = h
        .wait_for("connected", |s| {
            s.connections
                .iter()
                .any(|c| c.server == address && c.status.is_connected() && c.rbac.is_some())
        })
        .await;
    state
        .connections
        .iter()
        .find(|c| c.server == address)
        .unwrap()
        .id
}

async fn run(h: &common::Harness, conn: ConnectionId, request: OrgRequest) -> Operation {
    let before = h
        .backend
        .state()
        .operations
        .iter()
        .map(|o| o.id)
        .max()
        .unwrap_or(0);
    h.backend.send(Command::Action {
        connection: conn,
        request: ActionRequest::Org(Box::new(request)),
    });
    let state = h
        .wait_for("operation ended", |s| {
            s.operations
                .iter()
                .any(|o| o.id > before && !o.is_running())
        })
        .await;
    (**state.operations.iter().find(|o| o.id > before).unwrap()).clone()
}

fn ok(op: &Operation) {
    assert!(
        matches!(op.status, OperationStatus::Succeeded),
        "{:?}: {:?}",
        op.kind,
        op.status
    );
}

/// The org calls made since `from` (tags, other_config, gui_config, names).
fn calls_since(server: &MockServer, from: usize) -> Vec<String> {
    server.calls()[from..]
        .iter()
        .filter(|c| {
            c.contains("other_config")
                || c.contains("_tags")
                || c.contains("gui_config")
                || c.contains("set_name")
        })
        .cloned()
        .collect()
}

fn oc(server: &MockServer, class: &str, r: &str) -> Value {
    server.field(class, r, "other_config").unwrap_or(json!({}))
}

/// Waits until the cache shows what the server has.
async fn synced(
    h: &common::Harness,
    conn: ConnectionId,
    pred: impl Fn(&xen_core::ConnectionModel) -> bool,
) -> Arc<AppState> {
    h.wait_for("cache", |s| {
        s.connection(conn).is_some_and(|c| pred(&c.model))
    })
    .await
}

fn vm(r: &str) -> OrgObject {
    OrgObject::Vm(Ref::new(r))
}

#[tokio::test]
async fn folders_are_stored_as_xencenter_stores_them() {
    let server = MockServer::start(false).await;
    let h = harness();
    let c = connect(&h, &server).await;

    // The fixture's web-01 is alone in /Production/Web (as XenCenter
    // left it). Moving it out keeps that folder: EMPTY_FOLDERS gets it.
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::MoveToFolder {
            items: vec![FolderItem::Object(vm(WEB))],
            folder: "/Apps".into(),
        },
    )
    .await);
    assert_eq!(oc(&server, "vm", WEB)["folder"], json!("/Apps"));
    assert_eq!(
        oc(&server, "pool", POOL)["EMPTY_FOLDERS"],
        json!("/Production/Web")
    );
    assert_eq!(
        calls_since(&server, from),
        vec![
            "VM.remove_from_other_config",
            "VM.add_to_other_config",
            "pool.remove_from_other_config",
            "pool.add_to_other_config"
        ]
    );
    synced(&h, c, |m| {
        xen_core::org::folder_of(m, &vm(WEB)).as_deref() == Some("/Apps")
            && !xen_core::org::empty_folders(m).is_empty()
    })
    .await;

    // A new folder for a VM in none: its other_config only, no pool write.
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::MoveToFolder {
            items: vec![FolderItem::Object(vm(DB))],
            folder: "/Ops".into(),
        },
    )
    .await);
    assert_eq!(oc(&server, "vm", DB)["folder"], json!("/Ops"));
    assert_eq!(
        calls_since(&server, from),
        vec!["VM.remove_from_other_config", "VM.add_to_other_config"]
    );
    synced(&h, c, |m| {
        xen_core::org::folder_of(m, &vm(DB)).as_deref() == Some("/Ops")
    })
    .await;

    // Remove from folder, the folder's only object: the folder is kept.
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::DeleteFromFolders {
            items: vec![FolderItem::Object(vm(DB))],
        },
    )
    .await);
    assert!(oc(&server, "vm", DB).get("folder").is_none());
    assert_eq!(
        oc(&server, "pool", POOL)["EMPTY_FOLDERS"],
        json!("/Production/Web;/Ops")
    );
    assert_eq!(
        calls_since(&server, from),
        vec![
            "VM.remove_from_other_config",
            "pool.remove_from_other_config",
            "pool.add_to_other_config"
        ]
    );
    synced(&h, c, |m| xen_core::org::empty_folders(m).len() == 2).await;

    // New Folder adds to the list; an existing one is refused.
    ok(&run(
        &h,
        c,
        OrgRequest::NewFolders {
            paths: vec!["/Test".into()],
        },
    )
    .await);
    assert_eq!(
        oc(&server, "pool", POOL)["EMPTY_FOLDERS"],
        json!("/Ops;/Production/Web;/Test")
    );
    synced(&h, c, |m| xen_core::org::empty_folders(m).len() == 3).await;
    let refused = run(
        &h,
        c,
        OrgRequest::NewFolders {
            paths: vec!["/Test".into()],
        },
    )
    .await;
    assert!(
        matches!(&refused.status, OperationStatus::Failed(_)),
        "{:?}",
        refused.status
    );

    // Moving into a listed folder takes it out of the list.
    ok(&run(
        &h,
        c,
        OrgRequest::MoveToFolder {
            items: vec![FolderItem::Object(vm(DB))],
            folder: "/Test".into(),
        },
    )
    .await);
    assert_eq!(oc(&server, "vm", DB)["folder"], json!("/Test"));
    assert_eq!(
        oc(&server, "pool", POOL)["EMPTY_FOLDERS"],
        json!("/Ops;/Production/Web")
    );
    synced(&h, c, |m| {
        xen_core::org::folder_of(m, &vm(DB)).is_some() && xen_core::org::empty_folders(m).len() == 2
    })
    .await;

    // Renaming moves the contents; deleting leaves the objects in no folder.
    ok(&run(
        &h,
        c,
        OrgRequest::RenameFolder {
            folder: "/Test".into(),
            name: "Renamed".into(),
        },
    )
    .await);
    assert_eq!(oc(&server, "vm", DB)["folder"], json!("/Renamed"));
    synced(&h, c, |m| {
        xen_core::org::folder_of(m, &vm(DB)).as_deref() == Some("/Renamed")
    })
    .await;
    ok(&run(
        &h,
        c,
        OrgRequest::MoveToFolder {
            items: vec![FolderItem::Object(OrgObject::Sr(Ref::new(NFS)))],
            folder: "/Renamed".into(),
        },
    )
    .await);
    assert_eq!(oc(&server, "sr", NFS)["folder"], json!("/Renamed"));
    synced(&h, c, |m| {
        xen_core::org::folder_of(m, &OrgObject::Sr(Ref::new(NFS))).is_some()
    })
    .await;
    ok(&run(
        &h,
        c,
        OrgRequest::DeleteFromFolders {
            items: vec![FolderItem::Folder("/Renamed".into())],
        },
    )
    .await);
    assert!(oc(&server, "vm", DB).get("folder").is_none());
    assert!(oc(&server, "sr", NFS).get("folder").is_none());
    // Nothing listed was under the deleted folder: the list is unchanged.
    assert_eq!(
        oc(&server, "pool", POOL)["EMPTY_FOLDERS"],
        json!("/Ops;/Production/Web")
    );
}

#[tokio::test]
async fn tags_change_with_add_and_remove_tags() {
    let server = MockServer::start(false).await;
    let h = harness();
    let c = connect(&h, &server).await;
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::AddTag {
            objects: vec![vm(WEB), vm(DB), OrgObject::Sr(Ref::new(NFS))],
            tag: "prod".into(),
        },
    )
    .await);
    assert_eq!(
        calls_since(&server, from),
        vec!["VM.add_tags", "VM.add_tags", "SR.add_tags"]
    );
    // The fixture's web-01 had "web" and "production" already.
    assert_eq!(
        server.field("vm", WEB, "tags"),
        Some(json!(["web", "production", "prod"]))
    );
    assert_eq!(server.field("sr", NFS, "tags"), Some(json!(["prod"])));
    synced(&h, c, |m| xen_core::org::tag_users(m, "prod").len() == 3).await;

    // The Properties dialog's tags: removed then added, one call each.
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::FolderAndTags {
            object: vm(WEB),
            folder: Some("/Production/Web".into()),
            tags: vec!["web".into(), "zeta".into()],
        },
    )
    .await);
    assert_eq!(
        calls_since(&server, from),
        vec!["VM.remove_tags", "VM.remove_tags", "VM.add_tags"]
    );
    assert_eq!(
        server.field("vm", WEB, "tags"),
        Some(json!(["web", "zeta"]))
    );
    synced(&h, c, |m| xen_core::org::tag_users(m, "prod").len() == 2).await;

    ok(&run(
        &h,
        c,
        OrgRequest::RenameTag {
            old: "prod".into(),
            new: "production".into(),
        },
    )
    .await);
    assert_eq!(server.field("vm", DB, "tags"), Some(json!(["production"])));
    synced(&h, c, |m| {
        xen_core::org::tag_users(m, "production").len() == 2
    })
    .await;
    ok(&run(
        &h,
        c,
        OrgRequest::DeleteTag {
            tag: "production".into(),
        },
    )
    .await);
    assert_eq!(server.field("sr", NFS, "tags"), Some(json!([])));
    synced(&h, c, |m| {
        xen_core::org::tag_users(m, "production").is_empty()
    })
    .await;
    ok(&run(
        &h,
        c,
        OrgRequest::RemoveTag {
            objects: vec![vm(WEB)],
            tag: "zeta".into(),
        },
    )
    .await);
    assert_eq!(server.field("vm", WEB, "tags"), Some(json!(["web"])));
}

#[tokio::test]
async fn custom_fields_use_the_pools_gui_config_and_other_config() {
    let server = MockServer::start(false).await;
    let h = harness();
    let c = connect(&h, &server).await;
    let owner = CustomFieldDefinition::new("Owner", CustomFieldType::String);
    let due = CustomFieldDefinition::new("Due", CustomFieldType::Date);
    ok(&run(&h, c, OrgRequest::AddCustomField(owner.clone())).await);
    synced(&h, c, |m| xen_core::org::definitions(m).len() == 1).await;
    ok(&run(&h, c, OrgRequest::AddCustomField(due.clone())).await);
    assert_eq!(
        server.field("pool", POOL, "gui_config").unwrap()["XenCenter.CustomFields"],
        json!(
            "<CustomFieldDefinitions><CustomFieldDefinition name=\"Owner\" type=\"String\" defaultValue=\"\" /><CustomFieldDefinition name=\"Due\" type=\"Date\" defaultValue=\"\" /></CustomFieldDefinitions>"
        )
    );
    synced(&h, c, |m| xen_core::org::definitions(m).len() == 2).await;

    let t = xen_core::org::parse_date("2026-12-01 09:30:00Z").unwrap();
    ok(&run(
        &h,
        c,
        OrgRequest::SaveCustomFields {
            object: vm(WEB),
            values: vec![
                (
                    owner.clone(),
                    Some(CustomFieldValue::Text("ops team".into())),
                ),
                (due.clone(), Some(CustomFieldValue::Date(t))),
            ],
        },
    )
    .await);
    let web = oc(&server, "vm", WEB);
    assert_eq!(web["XenCenter.CustomFields.Owner"], json!("ops team"));
    assert_eq!(
        web["XenCenter.CustomFields.Due"],
        json!("2026-12-01 09:30:00Z")
    );
    ok(&run(
        &h,
        c,
        OrgRequest::SaveCustomFields {
            object: OrgObject::Host(Ref::new("OpaqueRef:host-0001")),
            values: vec![(owner.clone(), Some(CustomFieldValue::Text("infra".into())))],
        },
    )
    .await);
    let state = synced(&h, c, |m| {
        let w = m.vms.get(&Ref::new(WEB)).unwrap();
        xen_core::org::values(&xen_core::org::definitions(m), &w.other_config).len() == 2
    })
    .await;

    // Searches and the Custom Fields view see them.
    let w = world(&state);
    let groups = w.group(&OrgView::CustomFields.tree_search(""));
    assert_eq!(groups.len(), 2, "{groups:?}");
    let f = Filter::CustomField {
        definition: owner.clone(),
        query: "ops".into(),
        kind: StringMatch::StartsWith,
    };
    let found: Vec<String> = w
        .objects()
        .into_iter()
        .filter(|o| {
            w.query_matches(
                &Query::new(Some(ObjectTypes::ALL_EXC_FOLDERS), Some(f.clone())),
                o,
            )
        })
        .map(|o| w.name(&o))
        .collect();
    assert_eq!(found, vec!["web-01"]);
    drop(w);

    // Clearing a value removes the key; deleting the field removes the
    // definition and every value.
    ok(&run(
        &h,
        c,
        OrgRequest::SaveCustomFields {
            object: vm(WEB),
            values: vec![(due.clone(), None)],
        },
    )
    .await);
    assert!(
        oc(&server, "vm", WEB)
            .get("XenCenter.CustomFields.Due")
            .is_none()
    );
    synced(&h, c, |m| {
        !m.vms
            .get(&Ref::new(WEB))
            .unwrap()
            .other_config
            .contains_key("XenCenter.CustomFields.Due")
    })
    .await;
    ok(&run(&h, c, OrgRequest::RemoveCustomField(owner.clone())).await);
    assert_eq!(
        server.field("pool", POOL, "gui_config").unwrap()["XenCenter.CustomFields"],
        json!(
            "<CustomFieldDefinitions><CustomFieldDefinition name=\"Due\" type=\"Date\" defaultValue=\"\" /></CustomFieldDefinitions>"
        )
    );
    assert!(
        oc(&server, "vm", WEB)
            .get("XenCenter.CustomFields.Owner")
            .is_none()
    );
    assert!(
        oc(&server, "host", "OpaqueRef:host-0001")
            .get("XenCenter.CustomFields.Owner")
            .is_none()
    );
}

fn world(state: &AppState) -> World<'_> {
    World::new(
        state
            .connections
            .iter()
            .map(|c| Source {
                connection: c.id,
                model: &c.model,
                connected: c.status.is_connected(),
                name: c.server.clone(),
            })
            .collect(),
        false,
        0,
    )
}

#[tokio::test]
async fn saved_searches_live_in_the_pools_gui_config() {
    let server = MockServer::start(false).await;
    let h = harness();
    let c = connect(&h, &server).await;
    let mut search = Search::new(
        Query::new(
            Some(ObjectTypes::VM),
            Some(Filter::String {
                property: PropertyName::Label,
                query: "web".into(),
                kind: StringMatch::Contains,
                case_sensitive: false,
            }),
        ),
        None,
        "Web VMs",
    );
    search.uuid = Some("6b1b2a9e-0000-4000-8000-000000000001".into());
    let xml = xen_core::search::search_to_xml(&search);
    let from = server.calls().len();
    ok(&run(
        &h,
        c,
        OrgRequest::SaveSearch {
            uuid: search.uuid.clone().unwrap(),
            name: search.name.clone(),
            xml: xml.clone(),
        },
    )
    .await);
    assert_eq!(
        calls_since(&server, from),
        vec!["pool.remove_from_gui_config", "pool.add_to_gui_config"]
    );
    let gui = server.field("pool", POOL, "gui_config").unwrap();
    assert_eq!(
        gui["xensearch.Search-6b1b2a9e-0000-4000-8000-000000000001"],
        json!(xml)
    );
    let state = synced(&h, c, |m| {
        m.pool()
            .unwrap()
            .1
            .gui_config
            .keys()
            .any(|k| k.starts_with("xensearch.Search-"))
    })
    .await;
    let w = world(&state);
    let saved = search_eval::saved_searches(&w);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].1.name, "Web VMs");
    let found: Vec<String> = w
        .group(&saved[0].1)
        .iter()
        .flat_map(|n| n.objects())
        .map(|o| w.name(o))
        .collect();
    assert_eq!(found, vec!["web-01"]);
    drop(w);
    ok(&run(
        &h,
        c,
        OrgRequest::DeleteSearch {
            uuid: search.uuid.clone().unwrap(),
            name: search.name.clone(),
        },
    )
    .await);
    assert!(
        server
            .field("pool", POOL, "gui_config")
            .unwrap()
            .get("xensearch.Search-6b1b2a9e-0000-4000-8000-000000000001")
            .is_none()
    );
}

#[tokio::test]
async fn views_and_default_searches_over_the_live_cache() {
    let server = MockServer::start(false).await;
    server.set_field("vm", WEB, "tags", json!(["web", "prod"]));
    server.set_field("vm", DB, "tags", json!(["prod"]));
    server.set_field("vm", WEB, "other_config", json!({"folder": "/Prod/Web"}));
    server.set_field(
        "pool",
        POOL,
        "other_config",
        json!({"EMPTY_FOLDERS": "/Spare"}),
    );
    let h = harness();
    let c = connect(&h, &server).await;
    let state = synced(&h, c, |m| !xen_core::org::tag_users(m, "prod").is_empty()).await;
    let w = world(&state);
    let names = |nodes: &[search_eval::Node]| -> Vec<String> {
        nodes
            .iter()
            .map(|n| match &n.item {
                search_eval::NodeItem::Object(o) => w.name(o),
                search_eval::NodeItem::Group(t) => format!("{:?}", t.value),
            })
            .collect()
    };
    let tags = w.group(&OrgView::Tags.tree_search(""));
    assert_eq!(names(&tags), vec!["Str(\"prod\")", "Str(\"web\")"]);
    assert_eq!(names(&tags[0].children), vec!["db-01", "web-01"]);
    let folders = w.group(&OrgView::Folders.tree_search(""));
    assert_eq!(names(&folders), vec!["Prod", "Spare"]);
    assert_eq!(names(&folders[0].children), vec!["Web"]);
    assert_eq!(names(&folders[0].children[0].children), vec!["web-01"]);
    let objects = w.group(&OrgView::Objects.tree_search(""));
    assert!(objects.len() >= 5, "{:?}", names(&objects));
    let by_power = search_eval::default_searches()
        .into_iter()
        .find(|(k, _)| *k == "vms_by_power_state")
        .unwrap()
        .1;
    assert_eq!(
        names(&w.group(&by_power)),
        vec![
            "Enum(\"Halted\")",
            "Enum(\"Running\")",
            "Enum(\"Suspended\")"
        ]
    );
    let (pool_search, _) = search_eval::search_for(&w, Some(&ObjKey::Pool(c, Ref::new(POOL))));
    let g = w.group(&pool_search);
    assert_eq!(names(&g), vec!["Lab Pool"]);
}

#[tokio::test]
async fn search_metrics_read_rrd_updates_of_each_host() {
    let server = MockServer::start(false).await;
    let h = harness();
    let c = connect(&h, &server).await;
    let (handle, link) = xen_core::search_metrics::pair();
    h.backend.send(Command::WatchSearchMetrics {
        connection: c,
        link,
    });
    let state = h.backend.state();
    let host = state
        .connection(c)
        .unwrap()
        .model
        .hosts
        .get(&Ref::new("OpaqueRef:host-0001"))
        .unwrap()
        .uuid
        .clone();
    common::wait_until("metrics", || handle.metrics().get(&host, "cpu0").is_some()).await;
    let m = handle.metrics();
    assert!(
        m.get(&host, "cpu0").is_some(),
        "{:?}",
        m.values.keys().collect::<Vec<_>>()
    );
    assert!(m.get(&host, "memory_total_kib").is_some());
    assert!(server.calls().iter().any(|c| c == "http/rrd_updates"));
    drop(handle);
}
