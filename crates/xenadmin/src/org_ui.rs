//! F13 in the window: the navigation modes (`NavigationPane`), the trees
//! of the organization views and saved searches with their context menus
//! (`NavigationView`), drag and drop (`DragDropIntoFolderCommand`,
//! `DragDropTagCommand`, `DragDropMigrateVMCommand`), the folder, tag,
//! custom field and saved search dialogs, the Properties dialogs' Folder,
//! Tags and Custom Fields editing ([`OrgEditor`]), a VM Properties dialog
//! with those pages, and the details of the objects only these views
//! show (networks, virtual disks, vApps, folders and groups).

use std::collections::BTreeSet;

use egui::collapsing_header::CollapsingState;
use egui::{Button, ComboBox, Context, Grid, RichText, ScrollArea, Sense, TextEdit, Ui};
use xapi::{Ref, Vm, VmPowerState};
use xen_core::org::{
    self, CustomFieldDefinition, CustomFieldType, CustomFieldValue, FolderItem, OrgObject,
    OrgRequest,
};
use xen_core::search::Search;
use xen_core::search_eval::{self, GroupTag, GroupValue, Node, NodeItem, ObjKey, OrgView, World};
use xen_core::{ActionRequest, AppState, BackendHandle, ConnectionId, ConnectionView, VmAction};

use crate::app::Selection;
use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::i18n::fill;
use crate::icons::{self, HostState};
use crate::ops_ui::OpsUi;
use crate::org_strings::org_strings;
use crate::pool_ui::window;
use crate::search_ui::{self, SearchAction, SearchPanel, group_label, world};

/// The navigation pane's modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavMode {
    Infrastructure,
    View(OrgView),
    /// A saved or built-in search, by UUID.
    Saved(String),
}

/// What a dragged tree row carries.
#[derive(Debug, Clone)]
pub struct Dragged(pub ObjKey);

/// A dialog of this part.
pub enum OrgDialog {
    NewFolder {
        parent: String,
        name: String,
    },
    RenameFolder {
        path: String,
        name: String,
    },
    DeleteFolder {
        path: String,
    },
    RenameTag {
        tag: String,
        name: String,
    },
    DeleteTag {
        tag: String,
    },
    SaveSearch {
        search: Search,
        name: String,
        connection: Option<ConnectionId>,
    },
    DeleteSearch {
        connection: ConnectionId,
        uuid: String,
        name: String,
    },
    Export {
        search: Search,
        path: String,
        message: Option<String>,
    },
    Import {
        path: String,
        message: Option<String>,
    },
    NewCustomField {
        connection: ConnectionId,
        name: String,
        kind: CustomFieldType,
    },
    DeleteCustomField {
        connection: ConnectionId,
        definition: CustomFieldDefinition,
    },
}

/// This part's state.
pub struct OrgUi {
    pub mode: NavMode,
    pub search: SearchPanel,
    pub dialog: Option<OrgDialog>,
    pub vm_properties: Option<VmPropertiesDialog>,
    /// The tab shown for folders, groups, networks, disks and vApps.
    pub tab_search: bool,
    /// A search to show on the Search tab instead of the selection's
    /// (an imported one).
    pub imported: Option<Search>,
    /// The drag-and-drop status text (`DragDropCommandActivated`).
    pub drop_hint: Option<String>,
}

impl Default for OrgUi {
    fn default() -> Self {
        Self {
            mode: NavMode::Infrastructure,
            search: SearchPanel::default(),
            dialog: None,
            vm_properties: None,
            tab_search: false,
            imported: None,
            drop_hint: None,
        }
    }
}

/// The selection for a tree or grid object.
pub fn selection_for(o: &ObjKey) -> Selection {
    match o {
        ObjKey::Pool(c, r) => Selection::Pool(*c, r.clone()),
        ObjKey::Host(c, r) => Selection::Host(*c, r.clone()),
        ObjKey::Vm(c, r) => Selection::Vm(*c, r.clone()),
        ObjKey::Sr(c, r) => Selection::Sr(*c, r.clone()),
        ObjKey::Vdi(c, r) => Selection::Vdi(*c, r.clone()),
        ObjKey::Network(c, r) => Selection::Network(*c, r.clone()),
        ObjKey::VApp(c, r) => Selection::VApp(*c, r.clone()),
        ObjKey::Folder(p) => Selection::Folder(p.clone()),
        ObjKey::DisconnectedServer(c) => Selection::Connection(*c),
    }
}

/// The object of a selection, if it is one the views show.
pub fn object_of(sel: &Selection) -> Option<ObjKey> {
    Some(match sel {
        Selection::Pool(c, r) => ObjKey::Pool(*c, r.clone()),
        Selection::Host(c, r) => ObjKey::Host(*c, r.clone()),
        Selection::Vm(c, r) => ObjKey::Vm(*c, r.clone()),
        Selection::Sr(c, r) => ObjKey::Sr(*c, r.clone()),
        Selection::Vdi(c, r) => ObjKey::Vdi(*c, r.clone()),
        Selection::Network(c, r) => ObjKey::Network(*c, r.clone()),
        Selection::VApp(c, r) => ObjKey::VApp(*c, r.clone()),
        Selection::Folder(p) => ObjKey::Folder(p.clone()),
        _ => return None,
    })
}

fn view_name(v: OrgView) -> &'static str {
    let s = org_strings();
    match v {
        OrgView::Objects => s.view_objects,
        OrgView::Tags => s.view_tags,
        OrgView::Folders => s.view_folders,
        OrgView::CustomFields => s.view_fields,
        OrgView::VApps => s.view_vapps,
    }
}

/// The connections that have a folder (by path), or every connected one.
fn folder_connections(w: &World<'_>, path: &str) -> Vec<ConnectionId> {
    let with: Vec<ConnectionId> = w
        .folder_indexes()
        .filter(|(_, idx)| idx.contains(path))
        .map(|(c, _)| c)
        .collect();
    if with.is_empty() {
        w.sources
            .iter()
            .filter(|s| s.connected)
            .map(|s| s.connection)
            .take(1)
            .collect()
    } else {
        with
    }
}

/// Sends `request` on connection `c`.
fn send(
    state: &AppState,
    ops: &mut OpsUi,
    backend: &BackendHandle,
    c: ConnectionId,
    request: OrgRequest,
) {
    if let Some(conn) = state.connection(c) {
        ops.request(conn, ActionRequest::Org(Box::new(request)), backend);
    }
}

/// The intra-pool live migration a VM dropped on a host makes, if the
/// server allows it (`DragDropMigrateVMCommand`).
fn migrate_request(
    conn: &ConnectionView,
    vm: &Ref<Vm>,
    host: &Ref<xapi::Host>,
) -> Option<ActionRequest> {
    let rec = conn.model.vms.get(vm)?;
    let target = conn.model.hosts.get(host)?;
    let ok = rec.power_state == VmPowerState::Running
        && rec
            .allowed_operations
            .contains(&xapi::VmOperations::PoolMigrate)
        && &rec.resident_on != host
        && target.enabled
        && xen_core::objects::host_is_live(&conn.model, target);
    ok.then(|| ActionRequest::Vm {
        vm: vm.clone(),
        action: VmAction::Migrate(host.clone()),
    })
}

/// What dropping `dragged` on `target` would do: its status text and the
/// request(s) (`DragDropCommand.CanRun` / `StatusBarText`).
pub fn drop_action(
    state: &AppState,
    w: &World<'_>,
    dragged: &ObjKey,
    target: &DropTarget,
) -> Option<(String, Vec<(ConnectionId, ActionRequest)>)> {
    let s = org_strings();
    let c = dragged.connection();
    match target {
        DropTarget::Folder(path) => {
            let item = match dragged {
                ObjKey::Folder(p) => {
                    if org::is_within(path, p)
                        || org::parent_of(p).as_deref() == Some(path.as_str())
                    {
                        return None;
                    }
                    FolderItem::Folder(p.clone())
                }
                o => {
                    let org = o.org_object()?;
                    if w.folder(o).as_deref() == Some(path.as_str()) {
                        return None;
                    }
                    FolderItem::Object(org)
                }
            };
            let conns = match c {
                Some(c) => vec![c],
                None => folder_connections(
                    w,
                    match dragged {
                        ObjKey::Folder(p) => p,
                        _ => path,
                    },
                ),
            };
            let requests = conns
                .into_iter()
                .map(|c| {
                    (
                        c,
                        ActionRequest::Org(Box::new(OrgRequest::MoveToFolder {
                            items: vec![item.clone()],
                            folder: path.clone(),
                        })),
                    )
                })
                .collect();
            Some((
                fill(s.drop_folder, &[("folder", &org::folder_name(path))]),
                requests,
            ))
        }
        DropTarget::Tag(tag) => {
            let org = dragged.org_object()?;
            let m = w.model(c?)?;
            if org.tags(m).is_some_and(|t| t.iter().any(|x| x == tag)) {
                return None;
            }
            Some((
                fill(s.drop_tag, &[("tag", tag)]),
                vec![(
                    c?,
                    ActionRequest::Org(Box::new(OrgRequest::AddTag {
                        objects: vec![org],
                        tag: tag.clone(),
                    })),
                )],
            ))
        }
        DropTarget::Host(hc, host) => {
            let ObjKey::Vm(vc, vm) = dragged else {
                return None;
            };
            if vc != hc {
                return None;
            }
            let conn = state.connection(*vc)?;
            let request = migrate_request(conn, vm, host)?;
            let name = conn.model.hosts.get(host)?.name_label.clone();
            Some((
                fill(s.drop_migrate, &[("host", &name)]),
                vec![(*vc, request)],
            ))
        }
    }
}

/// Where something can be dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    Folder(String),
    Tag(String),
    Host(ConnectionId, Ref<xapi::Host>),
}

/// Makes a tree row draggable and a drop target; runs the drop.
#[allow(clippy::too_many_arguments)]
pub fn dnd(
    ui: &Ui,
    response: &egui::Response,
    state: &AppState,
    dragged: Option<ObjKey>,
    target: Option<DropTarget>,
    ops: &mut OpsUi,
    backend: &BackendHandle,
    hint: &mut Option<String>,
) {
    if let Some(d) = dragged {
        let r = response.interact(Sense::drag());
        r.dnd_set_drag_payload(Dragged(d));
    }
    let Some(target) = target else { return };
    if let Some(payload) = response.dnd_hover_payload::<Dragged>() {
        let w = world(state);
        match drop_action(state, &w, &payload.0, &target) {
            Some((text, _)) => {
                ui.painter().rect_stroke(
                    response.rect,
                    2.0,
                    ui.visuals().selection.stroke,
                    egui::StrokeKind::Outside,
                );
                *hint = Some(text);
            }
            None => *hint = None,
        }
    }
    if let Some(payload) = response.dnd_release_payload::<Dragged>() {
        let w = world(state);
        if let Some((_, requests)) = drop_action(state, &w, &payload.0, &target) {
            for (c, request) in requests {
                if let Some(conn) = state.connection(c) {
                    ops.request(conn, request, backend);
                }
            }
        }
        *hint = None;
    }
}

/// The objects under a group node (for Untag).
fn tag_of_parent(parent: Option<&GroupTag>) -> Option<String> {
    match parent {
        Some(GroupTag {
            value: GroupValue::Str(t),
            grouping,
            ..
        }) if grouping.kind
            == xen_core::search::GroupingKind::Property(xen_core::search::PropertyName::Tags) =>
        {
            Some(t.clone())
        }
        _ => None,
    }
}

impl OrgUi {
    /// The navigation tabs at the top of the tree (the original's
    /// `NavigationPane`): Infrastructure and Objects, then More, a menu of
    /// the organization views and the saved searches, lit while one of them
    /// shows. Returns whether the mode changed.
    pub fn nav_tabs(&mut self, ui: &mut Ui, state: &AppState) -> bool {
        let s = org_strings();
        let mut changed = false;
        let more_on = matches!(&self.mode, NavMode::View(v) if *v != OrgView::Objects)
            || matches!(self.mode, NavMode::Saved(_));
        let mut chosen = None;
        let current = self.mode.clone();
        changed |= crate::controls::segmented_with_menu(
            ui,
            &mut self.mode,
            [
                (NavMode::Infrastructure, s.nav_infrastructure),
                (NavMode::View(OrgView::Objects), s.nav_objects),
            ],
            s.nav_more,
            more_on,
            |ui| {
                let heading = |ui: &mut Ui, text: &str| {
                    ui.label(egui::RichText::new(text).small().weak());
                };
                heading(ui, s.nav_organization);
                for (v, label) in [
                    (OrgView::Tags, s.nav_tags),
                    (OrgView::Folders, s.nav_folders),
                    (OrgView::CustomFields, s.nav_fields),
                    (OrgView::VApps, s.nav_vapps),
                ] {
                    if ui
                        .selectable_label(current == NavMode::View(v), label)
                        .clicked()
                    {
                        chosen = Some(NavMode::View(v));
                        ui.close();
                    }
                }
                ui.separator();
                heading(ui, s.nav_saved);
                let w = world(state);
                for (_, search) in search_ui::all_searches(&w) {
                    let uuid = search.uuid.clone().unwrap_or_default();
                    if ui
                        .selectable_label(current == NavMode::Saved(uuid.clone()), &search.name)
                        .clicked()
                    {
                        chosen = Some(NavMode::Saved(uuid));
                        ui.close();
                    }
                }
            },
        );
        if let Some(mode) = chosen {
            self.mode = mode;
            changed = true;
        }
        changed
    }

    /// The saved search of the Saved Searches mode.
    pub fn saved_search(&self, w: &World<'_>) -> Option<(Option<ConnectionId>, Search)> {
        let NavMode::Saved(uuid) = &self.mode else {
            return None;
        };
        search_ui::all_searches(w)
            .into_iter()
            .find(|(_, s)| s.uuid.as_deref() == Some(uuid.as_str()))
    }

    /// The tree of an organization view or saved search.
    #[allow(clippy::too_many_arguments)]
    pub fn tree(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        selection: &mut Option<Selection>,
        filter: &str,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let s = org_strings();
        let w = world(state);
        let (root_label, root_sel, nodes) = match &self.mode {
            NavMode::View(v) => (
                view_name(*v).to_owned(),
                Selection::ViewRoot(*v),
                w.group(&v.tree_search(filter)),
            ),
            NavMode::Saved(uuid) => match self.saved_search(&w) {
                Some((_, search)) => {
                    let narrowed = if filter.trim().is_empty() {
                        search.clone()
                    } else {
                        search.add_filter(Some(search_eval::full_query_for(filter)))
                    };
                    (
                        search.name.clone(),
                        Selection::SavedSearch(uuid.clone()),
                        w.group(&narrowed),
                    )
                }
                None => {
                    ui.label(RichText::new(s.view_empty).weak());
                    return;
                }
            },
            NavMode::Infrastructure => return,
        };
        let view = match &self.mode {
            NavMode::View(v) => Some(*v),
            _ => None,
        };
        let id = ui.make_persistent_id(("org-root", format!("{:?}", self.mode)));
        let mut node = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if !filter.is_empty() {
            node.set_open(true);
        }
        node.show_header(ui, |ui| {
            let selected = selection.as_ref() == Some(&root_sel);
            let r = ui.selectable_label(selected, RichText::new(&root_label).strong());
            if r.clicked() {
                *selection = Some(root_sel.clone());
            }
            if view == Some(OrgView::Folders) {
                dnd(
                    ui,
                    &r,
                    state,
                    None,
                    Some(DropTarget::Folder("/".into())),
                    ops,
                    backend,
                    &mut self.drop_hint,
                );
                r.context_menu(|ui| {
                    if ui.button(s.new_folder).clicked() {
                        self.dialog = Some(OrgDialog::NewFolder {
                            parent: "/".into(),
                            name: String::new(),
                        });
                        ui.close();
                    }
                });
            }
            if let Selection::SavedSearch(uuid) = &root_sel
                && let Some((Some(c), search)) = self.saved_search(&w)
            {
                r.context_menu(|ui| {
                    if ui.button(s.delete).clicked() {
                        self.dialog = Some(OrgDialog::DeleteSearch {
                            connection: c,
                            uuid: uuid.clone(),
                            name: search.name.clone(),
                        });
                        ui.close();
                    }
                });
            }
        })
        .body(|ui| {
            if nodes.is_empty() {
                ui.label(RichText::new(s.view_empty).weak());
            }
            for (i, n) in nodes.iter().enumerate() {
                self.node(
                    ui,
                    state,
                    &w,
                    selection,
                    n,
                    None,
                    None,
                    &format!("{i}"),
                    filter,
                    ops,
                    backend,
                    view,
                );
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        w: &World<'_>,
        selection: &mut Option<Selection>,
        n: &Node,
        parent_folder: Option<&str>,
        parent_group: Option<&GroupTag>,
        path: &str,
        filter: &str,
        ops: &mut OpsUi,
        backend: &BackendHandle,
        view: Option<OrgView>,
    ) {
        let (label, sel, dragged, target) = match &n.item {
            NodeItem::Object(o) => {
                let target = match o {
                    ObjKey::Folder(p) => Some(DropTarget::Folder(p.clone())),
                    ObjKey::Host(c, h) => Some(DropTarget::Host(*c, h.clone())),
                    _ => None,
                };
                let draggable = !matches!(
                    o,
                    ObjKey::Pool(..) | ObjKey::VApp(..) | ObjKey::DisconnectedServer(_)
                );
                (
                    w.name(o),
                    selection_for(o),
                    draggable.then(|| o.clone()),
                    target,
                )
            }
            NodeItem::Group(tag) => {
                let target = match (&tag.grouping.kind, &tag.value) {
                    (
                        xen_core::search::GroupingKind::Property(
                            xen_core::search::PropertyName::Tags,
                        ),
                        GroupValue::Str(t),
                    ) => Some(DropTarget::Tag(t.clone())),
                    _ => None,
                };
                (
                    group_label(w, tag),
                    Selection::Group(Box::new(tag.clone())),
                    None,
                    target,
                )
            }
        };
        let in_org_view = view.is_some_and(|v| v != OrgView::Objects);
        let row = |me: &mut Self,
                   ui: &mut Ui,
                   selection: &mut Option<Selection>,
                   ops: &mut OpsUi,
                   backend: &BackendHandle| {
            if let NodeItem::Object(o) = &n.item {
                object_icon(ui, state, w, o);
            }
            let selected = selection.as_ref() == Some(&sel);
            let r = ui.selectable_label(selected, &label);
            if r.clicked() {
                *selection = Some(sel.clone());
                ui.ctx().request_repaint();
            }
            dnd(
                ui,
                &r,
                state,
                dragged.clone(),
                if in_org_view || matches!(target, Some(DropTarget::Host(..))) {
                    target.clone()
                } else {
                    None
                },
                ops,
                backend,
                &mut me.drop_hint,
            );
            r.context_menu(|ui| {
                me.context_menu(
                    ui,
                    state,
                    w,
                    n,
                    parent_folder,
                    parent_group,
                    in_org_view,
                    ops,
                    backend,
                )
            });
        };
        if n.children.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                row(self, ui, selection, ops, backend);
            });
            return;
        }
        let id = ui.make_persistent_id(("org-node", format!("{:?}", self.mode), path, &label));
        let mut state_ = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if !filter.is_empty() {
            state_.set_open(true);
        }
        let folder = match &n.item {
            NodeItem::Object(ObjKey::Folder(p)) => Some(p.clone()),
            _ => None,
        };
        let group = match &n.item {
            NodeItem::Group(t) => Some(t.clone()),
            _ => None,
        };
        state_
            .show_header(ui, |ui| row(self, ui, selection, ops, backend))
            .body(|ui| {
                for (i, c) in n.children.iter().enumerate() {
                    self.node(
                        ui,
                        state,
                        w,
                        selection,
                        c,
                        folder.as_deref(),
                        group.as_ref(),
                        &format!("{path}/{i}"),
                        filter,
                        ops,
                        backend,
                        view,
                    );
                }
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn context_menu(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        w: &World<'_>,
        n: &Node,
        parent_folder: Option<&str>,
        parent_group: Option<&GroupTag>,
        in_org_view: bool,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let s = org_strings();
        match &n.item {
            NodeItem::Object(ObjKey::Folder(p)) => {
                if ui.button(s.new_folder).clicked() {
                    self.dialog = Some(OrgDialog::NewFolder {
                        parent: p.clone(),
                        name: String::new(),
                    });
                    ui.close();
                }
                if ui.button(s.rename_folder).clicked() {
                    self.dialog = Some(OrgDialog::RenameFolder {
                        path: p.clone(),
                        name: org::folder_name(p),
                    });
                    ui.close();
                }
                if ui.button(s.delete_folder).clicked() {
                    self.dialog = Some(OrgDialog::DeleteFolder { path: p.clone() });
                    ui.close();
                }
            }
            NodeItem::Object(o) => {
                if in_org_view
                    && let (Some(folder), Some(org)) = (parent_folder, o.org_object())
                    && folder != "/"
                    && ui.button(s.remove_from_folder).clicked()
                {
                    if let Some(c) = o.connection() {
                        send(
                            state,
                            ops,
                            backend,
                            c,
                            OrgRequest::DeleteFromFolders {
                                items: vec![FolderItem::Object(org)],
                            },
                        );
                    }
                    ui.close();
                }
                if in_org_view
                    && let (Some(tag), Some(org), Some(c)) =
                        (tag_of_parent(parent_group), o.org_object(), o.connection())
                    && ui.button(s.untag).clicked()
                {
                    send(
                        state,
                        ops,
                        backend,
                        c,
                        OrgRequest::RemoveTag {
                            objects: vec![org],
                            tag,
                        },
                    );
                    ui.close();
                }
                if let ObjKey::Vm(c, vm) = o
                    && ui.button(s.properties).clicked()
                {
                    if let Some(conn) = state.connection(*c) {
                        self.vm_properties = VmPropertiesDialog::open(conn, vm.clone());
                    }
                    ui.close();
                }
            }
            NodeItem::Group(tag) => {
                if let Some(t) = tag_of_parent(Some(tag)) {
                    if ui.button(s.rename_tag).clicked() {
                        self.dialog = Some(OrgDialog::RenameTag {
                            tag: t.clone(),
                            name: t.clone(),
                        });
                        ui.close();
                    }
                    if ui.button(s.delete_tag).clicked() {
                        self.dialog = Some(OrgDialog::DeleteTag { tag: t });
                        ui.close();
                    }
                }
            }
        }
        let _ = w;
    }

    /// The details of a selection only this part handles.
    pub fn details(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        selection: &Selection,
        backend: &BackendHandle,
    ) -> Option<Selection> {
        let s = org_strings();
        let w = world(state);
        let (title, search, general): (String, Option<(Search, String)>, Option<ObjKey>) =
            match selection {
                Selection::ViewRoot(v) => (
                    view_name(*v).to_owned(),
                    Some((v.root_search(), view_name(*v).to_owned())),
                    None,
                ),
                Selection::SavedSearch(_) => match self.saved_search(&w) {
                    Some((_, search)) => (
                        search.name.clone(),
                        Some((search.clone(), search.name.clone())),
                        None,
                    ),
                    None => (String::new(), None, None),
                },
                Selection::Folder(p) => {
                    let search = match &self.mode {
                        NavMode::View(v) if *v != OrgView::Objects => {
                            (v.root_search(), view_name(*v).to_owned())
                        }
                        _ => {
                            let f = search_eval::search_for_folder(p);
                            let name = f.name.clone();
                            (f, name)
                        }
                    };
                    (org::folder_name(p), Some(search), None)
                }
                Selection::Group(tag) => {
                    let search = match &self.mode {
                        NavMode::View(OrgView::Objects) => {
                            let name = group_label(&w, tag);
                            (
                                search_eval::search_for_group(
                                    &w,
                                    tag,
                                    xen_core::search::ObjectTypes::ALL_EXC_FOLDERS,
                                    name.clone(),
                                ),
                                name,
                            )
                        }
                        NavMode::View(v) => (v.root_search(), view_name(*v).to_owned()),
                        NavMode::Saved(_) => match self.saved_search(&w) {
                            Some((_, s)) => (s.clone(), s.name.clone()),
                            None => return None,
                        },
                        NavMode::Infrastructure => return None,
                    };
                    (group_label(&w, tag), Some(search), None)
                }
                other => {
                    let o = object_of(other)?;
                    (w.name(&o), None, Some(o))
                }
            };
        ui.horizontal(|ui| {
            ui.heading(&title);
        });
        ui.separator();
        if let Some(o) = general {
            ui.horizontal(|ui| {
                let _ = ui.selectable_label(true, crate::tabs::Tab::General.title());
            });
            ui.separator();
            object_general(ui, &w, &o);
            return None;
        }
        let (base, name) = search?;
        let (base, name) = match self.imported.clone() {
            Some(imported) => {
                let n = imported.name.clone();
                (imported, n)
            }
            None => (base, name),
        };
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(true, s.tab_search);
        });
        ui.separator();
        let action = self.search.show(ui, state, backend, &base, &name);
        self.on_search_action(action)
    }

    /// Runs what the Search tab asked; a selection to make.
    pub fn on_search_action(&mut self, action: Option<SearchAction>) -> Option<Selection> {
        match action? {
            SearchAction::Select(o) => return Some(selection_for(&o)),
            SearchAction::Save(search) => {
                let name = search.name.clone();
                self.dialog = Some(OrgDialog::SaveSearch {
                    search,
                    name,
                    connection: None,
                });
            }
            SearchAction::Export(search) => {
                self.dialog = Some(OrgDialog::Export {
                    search,
                    path: String::new(),
                    message: None,
                });
            }
            SearchAction::Import => {
                self.dialog = Some(OrgDialog::Import {
                    path: String::new(),
                    message: None,
                });
            }
        }
        None
    }

    /// Draws the open dialogs.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        if let Some(d) = self.vm_properties.as_mut().and_then(|d| d.dialog.take()) {
            self.dialog = Some(d);
        }
        if let Some(d) = self.vm_properties.as_mut()
            && d.window(ctx, state, ops, backend)
        {
            self.vm_properties = None;
        }
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let s = org_strings();
        let w = world(state);
        let mut close = false;
        let mut ok = false;
        let title = match dialog {
            OrgDialog::NewFolder { .. } => s.new_folder_title,
            OrgDialog::RenameFolder { .. } => s.rename_folder_title,
            OrgDialog::DeleteFolder { .. } => s.delete_folder,
            OrgDialog::RenameTag { .. } => s.rename_tag_title,
            OrgDialog::DeleteTag { .. } => s.delete_tag,
            OrgDialog::SaveSearch { .. } => s.save_title,
            OrgDialog::DeleteSearch { .. } => s.delete,
            OrgDialog::Export { .. } => s.export_title,
            OrgDialog::Import { .. } => s.import_title,
            OrgDialog::NewCustomField { .. } => s.new_custom_field,
            OrgDialog::DeleteCustomField { .. } => s.delete,
        };
        let importing = matches!(dialog, OrgDialog::Import { .. });
        window(title.to_owned()).show(ctx, |ui| {
            ui.set_min_width(380.0);
            let mut can = true;
            match dialog {
                OrgDialog::NewFolder { name, .. } | OrgDialog::RenameFolder { name, .. } => {
                    ui.horizontal(|ui| {
                        let l = ui.label(s.new_folder_name);
                        ui.add(TextEdit::singleline(name).desired_width(240.0))
                            .labelled_by(l.id);
                    });
                    can = !org::fixup_relative_path(name).is_empty();
                }
                OrgDialog::DeleteFolder { path } => {
                    ui.label(fill(
                        s.delete_folder_prompt,
                        &[("name", &org::folder_name(path))],
                    ));
                }
                OrgDialog::RenameTag { name, .. } => {
                    ui.horizontal(|ui| {
                        let l = ui.label(s.new_tag);
                        ui.add(TextEdit::singleline(name).desired_width(240.0))
                            .labelled_by(l.id);
                    });
                    can = !name.trim().is_empty();
                }
                OrgDialog::DeleteTag { tag } => {
                    ui.label(fill(s.delete_tag_prompt, &[("tag", tag)]));
                }
                OrgDialog::SaveSearch {
                    name, connection, ..
                } => {
                    let connected: Vec<(ConnectionId, String)> = state
                        .connections
                        .iter()
                        .filter(|c| c.status.is_connected())
                        .filter_map(|c| {
                            c.model
                                .pool()
                                .map(|(_, p)| (c.id, xen_core::objects::pool_name(&c.model, p)))
                        })
                        .collect();
                    if connection.is_none() {
                        *connection = connected.first().map(|(c, _)| *c);
                    }
                    Grid::new("save-search").num_columns(2).show(ui, |ui| {
                        let l = ui.label(s.save_name);
                        ui.add(TextEdit::singleline(name).desired_width(240.0))
                            .labelled_by(l.id);
                        ui.end_row();
                        ui.label(s.save_location);
                        let current = connected
                            .iter()
                            .find(|(c, _)| Some(*c) == *connection)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_default();
                        ComboBox::from_id_salt("save-location")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (c, n) in &connected {
                                    ui.selectable_value(connection, Some(*c), n);
                                }
                            });
                        ui.end_row();
                    });
                    can = !name.trim().is_empty() && connection.is_some();
                }
                OrgDialog::DeleteSearch { name, .. } => {
                    ui.label(fill(s.delete_search_prompt, &[("name", name)]));
                }
                OrgDialog::Export { path, message, .. } | OrgDialog::Import { path, message } => {
                    let filters = crate::file_dialog::search_filters();
                    let pick = if importing {
                        crate::file_dialog::Pick::Open(filters)
                    } else {
                        crate::file_dialog::Pick::Save(filters)
                    };
                    ui.horizontal(|ui| {
                        let l = ui.label(s.file_label);
                        crate::file_dialog::path_row(ui, path, pick, |e| {
                            e.hint_text("search.xensearch")
                        })
                        .edit
                        .labelled_by(l.id);
                    });
                    if let Some(m) = message {
                        ui.colored_label(ui.visuals().warn_fg_color, m.as_str());
                    }
                    can = !path.trim().is_empty();
                }
                OrgDialog::NewCustomField {
                    connection,
                    name,
                    kind,
                } => {
                    ui.label(s.custom_field_blurb);
                    let duplicate = w
                        .definitions_of(*connection)
                        .iter()
                        .any(|d| d.name.trim() == name.trim());
                    Grid::new("new-field").num_columns(2).show(ui, |ui| {
                        let l = ui.label(s.field_name);
                        ui.add(TextEdit::singleline(name).desired_width(240.0))
                            .labelled_by(l.id);
                        ui.end_row();
                        ui.label(s.field_type);
                        ComboBox::from_id_salt("field-type")
                            .selected_text(if *kind == CustomFieldType::Date {
                                s.field_date
                            } else {
                                s.field_text
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(kind, CustomFieldType::String, s.field_text);
                                ui.selectable_value(kind, CustomFieldType::Date, s.field_date);
                            });
                        ui.end_row();
                    });
                    if duplicate {
                        ui.colored_label(ui.visuals().warn_fg_color, s.custom_field_duplicate);
                    }
                    can = !name.trim().is_empty() && !duplicate;
                }
                OrgDialog::DeleteCustomField { definition, .. } => {
                    ui.label(fill(s.delete_field_prompt, &[("name", &definition.name)]));
                }
            }
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                let label = match dialog {
                    OrgDialog::SaveSearch { .. } => s.save_button,
                    OrgDialog::RenameFolder { .. } | OrgDialog::RenameTag { .. } => s.rename,
                    OrgDialog::DeleteFolder { .. }
                    | OrgDialog::DeleteTag { .. }
                    | OrgDialog::DeleteSearch { .. }
                    | OrgDialog::DeleteCustomField { .. } => s.delete,
                    _ => s.ok,
                };
                if row
                    .add_enabled(ButtonRole::Primary, can, Button::new(label))
                    .clicked()
                {
                    ok = true;
                }
                if row.button(ButtonRole::Cancel, s.cancel).clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.dialog = None;
            return;
        }
        if !ok {
            return;
        }
        let dialog = self.dialog.take().expect("open");
        match dialog {
            OrgDialog::NewFolder { parent, name } => {
                let path = org::append_path(&parent, &org::fixup_relative_path(&name));
                for c in folder_connections(&w, &parent) {
                    send(
                        state,
                        ops,
                        backend,
                        c,
                        OrgRequest::NewFolders {
                            paths: vec![path.clone()],
                        },
                    );
                }
            }
            OrgDialog::RenameFolder { path, name } => {
                for c in folder_connections(&w, &path) {
                    send(
                        state,
                        ops,
                        backend,
                        c,
                        OrgRequest::RenameFolder {
                            folder: path.clone(),
                            name: name.clone(),
                        },
                    );
                }
            }
            OrgDialog::DeleteFolder { path } => {
                for c in folder_connections(&w, &path) {
                    send(
                        state,
                        ops,
                        backend,
                        c,
                        OrgRequest::DeleteFromFolders {
                            items: vec![FolderItem::Folder(path.clone())],
                        },
                    );
                }
            }
            OrgDialog::RenameTag { tag, name } => {
                for src in w.sources.iter().filter(|s| s.connected) {
                    if !org::tag_users(src.model, &tag).is_empty() {
                        send(
                            state,
                            ops,
                            backend,
                            src.connection,
                            OrgRequest::RenameTag {
                                old: tag.clone(),
                                new: name.trim().to_owned(),
                            },
                        );
                    }
                }
            }
            OrgDialog::DeleteTag { tag } => {
                for src in w.sources.iter().filter(|s| s.connected) {
                    if !org::tag_users(src.model, &tag).is_empty() {
                        send(
                            state,
                            ops,
                            backend,
                            src.connection,
                            OrgRequest::DeleteTag { tag: tag.clone() },
                        );
                    }
                }
            }
            OrgDialog::SaveSearch {
                mut search,
                name,
                connection,
            } => {
                // `SearchPage.buttonSave_Click`: a name already used gets " (2)"…
                let names: BTreeSet<String> = search_ui::all_searches(&w)
                    .into_iter()
                    .map(|(_, s)| s.name)
                    .collect();
                let mut final_name = name.trim().to_owned();
                if names.contains(&final_name) {
                    for i in 2.. {
                        let n = format!("{final_name} ({i})");
                        if !names.contains(&n) {
                            final_name = n;
                            break;
                        }
                    }
                }
                search.name = final_name.clone();
                search.default_search = false;
                let uuid = new_uuid();
                search.uuid = Some(uuid.clone());
                if let Some(c) = connection {
                    send(
                        state,
                        ops,
                        backend,
                        c,
                        OrgRequest::SaveSearch {
                            uuid,
                            name: final_name,
                            xml: xen_core::search::search_to_xml(&search),
                        },
                    );
                }
            }
            OrgDialog::DeleteSearch {
                connection,
                uuid,
                name,
            } => {
                send(
                    state,
                    ops,
                    backend,
                    connection,
                    OrgRequest::DeleteSearch { uuid, name },
                );
                if self.mode == NavMode::Saved(String::new()) {
                    self.mode = NavMode::Infrastructure;
                }
            }
            OrgDialog::Export {
                mut search, path, ..
            } => {
                if search.uuid.as_deref().is_none_or(str::is_empty) {
                    search.uuid = Some(new_uuid());
                }
                let xml = xen_core::search::searches_to_xml(&[search.clone()]);
                if let Err(e) = std::fs::write(path.trim(), xml) {
                    self.dialog = Some(OrgDialog::Export {
                        search,
                        message: Some(fill(
                            s.file_error,
                            &[("file", path.trim()), ("error", &e.to_string())],
                        )),
                        path,
                    });
                }
            }
            OrgDialog::Import { path, .. } => {
                // `ImportSearchCommand`: the file's first search is shown.
                let loaded = std::fs::read_to_string(path.trim())
                    .ok()
                    .filter(|_| path.trim().ends_with(".xensearch"))
                    .and_then(|x| xen_core::search::load_searches(&x).ok())
                    .and_then(|l| l.into_iter().next());
                match loaded {
                    Some(search) => {
                        self.search.set_base(&search, &search.name.clone());
                        self.imported = Some(search);
                        self.tab_search = true;
                    }
                    None => {
                        self.dialog = Some(OrgDialog::Import {
                            message: Some(fill(
                                s.import_failed,
                                &[("console", branding::PRODUCT_NAME), ("file", path.trim())],
                            )),
                            path,
                        });
                    }
                }
            }
            OrgDialog::NewCustomField {
                connection,
                name,
                kind,
            } => {
                send(
                    state,
                    ops,
                    backend,
                    connection,
                    OrgRequest::AddCustomField(CustomFieldDefinition::new(name.trim(), kind)),
                );
            }
            OrgDialog::DeleteCustomField {
                connection,
                definition,
            } => {
                send(
                    state,
                    ops,
                    backend,
                    connection,
                    OrgRequest::RemoveCustomField(definition),
                );
            }
        }
    }
}

/// A random UUID (version 4 layout) for a saved search.
pub fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

/// The icon of a tree object (the infrastructure tree's where it has one).
fn object_icon(ui: &mut Ui, state: &AppState, w: &World<'_>, o: &ObjKey) {
    let Some(c) = o.connection() else { return };
    let Some(m) = w.model(c) else { return };
    match o {
        ObjKey::Pool(..) => icons::pool(ui),
        ObjKey::Host(_, r) => {
            let live = m
                .hosts
                .get(r)
                .is_some_and(|h| xen_core::objects::host_is_live(m, h));
            let connected = state.connection(c).is_some_and(|v| v.status.is_connected());
            let st = if !connected || !live {
                HostState::Disconnected
            } else if m
                .hosts
                .get(r)
                .is_some_and(|h| xen_core::objects::host_in_maintenance(h))
            {
                HostState::Maintenance
            } else {
                HostState::Connected
            };
            icons::host_state(ui, st);
        }
        ObjKey::Vm(_, r) => {
            if let Some(vm) = m.vms.get(r) {
                icons::vm_state(ui, xen_core::objects::vm_icon(vm));
            }
        }
        ObjKey::Sr(_, r) => {
            if let Some(sr) = m.srs.get(r) {
                icons::sr(ui, xen_core::objects::sr_icon(m, r, sr));
            }
        }
        ObjKey::DisconnectedServer(_) => icons::host(ui, false),
        _ => {}
    }
}

/// The General tab of a network, virtual disk or vApp: name,
/// description, tags, folder, custom fields.
pub fn object_general(ui: &mut Ui, w: &World<'_>, o: &ObjKey) {
    let s = org_strings();
    let t = crate::tab_strings::tab_strings();
    let tags = match w.property(o, xen_core::search::PropertyName::Tags) {
        search_eval::Value::Strs(mut l) => {
            l.sort();
            if l.is_empty() {
                s.none.to_owned()
            } else {
                l.join(", ")
            }
        }
        _ => s.none.to_owned(),
    };
    let folder = w
        .folder(o)
        .map(|f| org::point_to_path(&f).unwrap_or_default().join(" › "))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| s.none.to_owned());
    ScrollArea::vertical()
        .id_salt("org-general")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.label(RichText::new(t.tab_general).strong());
            Grid::new("org-general-grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label(t.name);
                    ui.label(w.name(o));
                    ui.end_row();
                    ui.label(t.description);
                    ui.label(w.description(o));
                    ui.end_row();
                    if o.org_object().is_some() {
                        ui.label(t.tags);
                        ui.label(tags);
                        ui.end_row();
                        ui.label(t.folder);
                        ui.label(folder);
                        ui.end_row();
                    }
                    if let Some(uuid) = w.uuid(o) {
                        ui.label(crate::tab_strings::tab_strings().uuid);
                        ui.label(uuid);
                        ui.end_row();
                    }
                });
            let fields = w.custom_fields(o);
            if !fields.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(s.page_custom_fields).strong());
                Grid::new("org-general-fields")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for (d, v) in fields {
                            ui.label(&d.name);
                            ui.label(v.display());
                            ui.end_row();
                        }
                    });
            }
        });
}

// ---- Properties: Folder, Tags and Custom Fields -----------------------------------

/// One custom field row of the Custom Fields page.
#[derive(Debug, Clone)]
pub struct FieldRow {
    pub definition: CustomFieldDefinition,
    /// The text, or the date as typed.
    pub text: String,
}

impl FieldRow {
    /// `CustomFieldRow.GetValue`; `Err` for a date that does not read.
    pub fn value(&self) -> Result<Option<CustomFieldValue>, ()> {
        let t = self.text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        match self.definition.kind {
            CustomFieldType::String => Ok(Some(CustomFieldValue::Text(self.text.clone()))),
            CustomFieldType::Date => org::parse_date(t)
                .map(|d| Some(CustomFieldValue::Date(d)))
                .ok_or(()),
        }
    }
}

/// The General page's Folder and Tags (`GeneralEditPage`) and the Custom
/// Fields page (`CustomFieldsDisplayPage`) of a Properties dialog.
pub struct OrgEditor {
    pub connection: ConnectionId,
    pub object: OrgObject,
    pub folder: Option<String>,
    folder_original: Option<String>,
    pub tags: Vec<String>,
    tags_original: Vec<String>,
    pub new_tag: String,
    /// The Change Folder chooser is open, with the folder typed for a new
    /// one.
    pub choosing_folder: bool,
    pub new_folder: String,
    pub fields: Vec<FieldRow>,
    /// Whether the General page edits the tags here (the VM dialog); the
    /// pool, server and SR dialogs keep their own tags box.
    pub edit_tags: bool,
}

impl OrgEditor {
    pub fn new(conn: &ConnectionView, object: OrgObject) -> Self {
        let m = &conn.model;
        let folder = org::folder_of(m, &object);
        let mut tags = object.tags(m).unwrap_or_default().to_vec();
        tags.sort();
        let mut editor = Self {
            connection: conn.id,
            folder: folder.clone(),
            folder_original: folder,
            tags: tags.clone(),
            tags_original: tags,
            new_tag: String::new(),
            choosing_folder: false,
            new_folder: String::new(),
            fields: Vec::new(),
            edit_tags: true,
            object,
        };
        editor.rebuild(conn, true);
        editor
    }

    /// `CustomFieldsDisplayPage.Rebuild`: a row per definition, new ones
    /// added, removed ones dropped.
    pub fn rebuild(&mut self, conn: &ConnectionView, reset: bool) {
        let defs = org::definitions(&conn.model);
        let oc = self
            .object
            .other_config(&conn.model)
            .cloned()
            .unwrap_or_default();
        self.fields.retain(|r| defs.contains(&r.definition));
        for d in defs {
            let text = match org::value_of(&oc, &d) {
                Some(CustomFieldValue::Text(t)) => t,
                Some(CustomFieldValue::Date(t)) => {
                    org::format_u(t).trim_end_matches('Z').to_owned()
                }
                None => String::new(),
            };
            match self.fields.iter_mut().find(|r| r.definition == d) {
                Some(r) if reset => r.text = text,
                Some(_) => {}
                None => self.fields.push(FieldRow {
                    definition: d,
                    text,
                }),
            }
        }
    }

    /// The Folder and Tags rows of a General page grid.
    pub fn general_rows(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        if self.edit_tags {
            self.tags_row(ui, conn);
        }
        self.folder_row(ui, conn);
    }

    fn tags_row(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = org_strings();
        ui.label(s.tags_label);
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                let mut gone = None;
                for (i, t) in self.tags.iter().enumerate() {
                    if ui
                        .small_button(format!("{t}  x"))
                        .on_hover_text(s.remove)
                        .clicked()
                    {
                        gone = Some(i);
                    }
                }
                if let Some(i) = gone {
                    self.tags.remove(i);
                }
                if self.tags.is_empty() {
                    ui.label(RichText::new(s.none).weak());
                }
            });
            ui.horizontal(|ui| {
                let all: Vec<String> = org::tag_counts(&conn.model)
                    .into_iter()
                    .map(|(t, _)| t)
                    .filter(|t| !self.tags.contains(t))
                    .collect();
                ComboBox::from_id_salt("edit-tags-existing")
                    .selected_text(s.edit_tags)
                    .show_ui(ui, |ui| {
                        for t in all {
                            if ui.selectable_label(false, &t).clicked() {
                                self.tags.push(t);
                                self.tags.sort();
                            }
                        }
                    });
                let l = ui.label(s.new_tag);
                ui.add(TextEdit::singleline(&mut self.new_tag).desired_width(120.0))
                    .labelled_by(l.id);
                let tag = self.new_tag.trim().to_owned();
                if ui
                    .add_enabled(!tag.is_empty(), Button::new(s.create))
                    .clicked()
                {
                    if !self.tags.contains(&tag) {
                        self.tags.push(tag);
                        self.tags.sort();
                    }
                    self.new_tag.clear();
                }
            });
        });
        ui.end_row();
    }

    fn folder_row(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let s = org_strings();
        ui.label(s.folder_label);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let shown = self
                    .folder
                    .as_deref()
                    .and_then(org::point_to_path)
                    .map(|p| p.join(" › "))
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| s.none.to_owned());
                ui.label(shown);
                if ui.button(s.change).clicked() {
                    self.choosing_folder = !self.choosing_folder;
                }
            });
            if self.choosing_folder {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(s.change_folder_title).strong());
                    let mut in_folder = self.folder.is_some();
                    if ui.radio_value(&mut in_folder, false, s.no_folder).clicked() {
                        self.folder = None;
                    }
                    ui.radio_value(&mut in_folder, true, s.in_folder);
                    let folders: Vec<String> = org::FolderIndex::build(&conn.model)
                        .folders
                        .into_iter()
                        .filter(|f| f != "/")
                        .collect();
                    ScrollArea::vertical()
                        .id_salt("folder-choices")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for f in &folders {
                                let depth = org::point_to_path(f).map(|p| p.len()).unwrap_or(1);
                                ui.horizontal(|ui| {
                                    ui.add_space(depth as f32 * 12.0);
                                    if ui
                                        .selectable_label(
                                            self.folder.as_deref() == Some(f.as_str()),
                                            org::folder_name(f),
                                        )
                                        .clicked()
                                    {
                                        self.folder = Some(f.clone());
                                    }
                                });
                            }
                        });
                    ui.horizontal(|ui| {
                        let l = ui.label(s.new_folder_name);
                        ui.add(TextEdit::singleline(&mut self.new_folder).desired_width(140.0))
                            .labelled_by(l.id);
                        let name = org::fixup_relative_path(&self.new_folder);
                        if ui
                            .add_enabled(!name.is_empty(), Button::new(s.create))
                            .clicked()
                        {
                            let parent = self.folder.clone().unwrap_or_else(|| "/".into());
                            self.folder = Some(org::append_path(&parent, &name));
                            self.new_folder.clear();
                        }
                    });
                });
            }
        });
        ui.end_row();
    }

    /// The Custom Fields page (`CustomFieldsDisplayPage`).
    pub fn custom_fields_page(
        &mut self,
        ui: &mut Ui,
        conn: &ConnectionView,
        dialog: &mut Option<OrgDialog>,
    ) {
        let s = org_strings();
        self.rebuild(conn, false);
        ui.label(s.custom_fields_blurb);
        ui.add_space(6.0);
        Grid::new(("custom-fields", self.connection))
            .num_columns(3)
            .show(ui, |ui| {
                for row in &mut self.fields {
                    let l = ui.label(&row.definition.name);
                    let mut edit = TextEdit::singleline(&mut row.text).desired_width(240.0);
                    if row.definition.kind == CustomFieldType::Date {
                        edit = edit.hint_text(s.field_date_hint);
                    }
                    ui.add(edit).labelled_by(l.id);
                    if ui.small_button("x").on_hover_text(s.delete).clicked() {
                        *dialog = Some(OrgDialog::DeleteCustomField {
                            connection: self.connection,
                            definition: row.definition.clone(),
                        });
                    }
                    ui.end_row();
                }
            });
        if self.fields.iter().any(|r| r.value().is_err()) {
            ui.colored_label(ui.visuals().warn_fg_color, s.field_date_invalid);
        }
        if ui.button(s.add_custom_field).clicked() {
            *dialog = Some(OrgDialog::NewCustomField {
                connection: self.connection,
                name: String::new(),
                kind: CustomFieldType::String,
            });
        }
    }

    /// The Custom Fields page's summary (`SubText`).
    pub fn custom_fields_subtext(&self) -> String {
        let s = org_strings();
        let parts: Vec<String> = self
            .fields
            .iter()
            .filter(|r| !r.text.trim().is_empty())
            .map(|r| format!("{}: {}", r.definition.name, r.text.trim()))
            .collect();
        if parts.is_empty() {
            s.none.to_owned()
        } else {
            parts.join(", ")
        }
    }

    /// Whether every value can be saved.
    pub fn valid(&self) -> bool {
        self.fields.iter().all(|r| r.value().is_ok())
    }

    /// What OK saves: `GeneralEditPageAction` when the folder or tags
    /// changed, `SaveCustomFieldsAction` for the changed fields.
    pub fn requests(&self, conn: &ConnectionView) -> Vec<ActionRequest> {
        let mut out = Vec::new();
        let mut tags = if self.edit_tags {
            self.tags.clone()
        } else {
            self.tags_original.clone()
        };
        tags.sort();
        if self.folder.as_deref().and_then(org::normalise)
            != self.folder_original.as_deref().and_then(org::normalise)
            || tags != self.tags_original
        {
            out.push(ActionRequest::Org(Box::new(OrgRequest::FolderAndTags {
                object: self.object.clone(),
                folder: self.folder.clone(),
                tags,
            })));
        }
        let edited: Vec<(CustomFieldDefinition, Option<CustomFieldValue>)> = self
            .fields
            .iter()
            .filter_map(|r| r.value().ok().map(|v| (r.definition.clone(), v)))
            .collect();
        let changed = org::changed_values(&conn.model, &self.object, &edited);
        if !changed.is_empty() {
            out.push(ActionRequest::Org(Box::new(OrgRequest::SaveCustomFields {
                object: self.object.clone(),
                values: changed,
            })));
        }
        out
    }
}

/// The pages of the VM Properties dialog this phase adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmPage {
    General,
    CustomFields,
    /// GPU (`GpuEditPage`, F14a).
    Gpu,
    /// USB (`USBEditPage`, F14a).
    Usb,
}

/// A VM's Properties dialog: General (name, description, folder, tags)
/// and Custom Fields.
pub struct VmPropertiesDialog {
    pub connection: ConnectionId,
    pub vm: Ref<Vm>,
    pub page: VmPage,
    pub name: String,
    pub description: String,
    original: (String, String),
    pub editor: OrgEditor,
    pub dialog: Option<OrgDialog>,
    /// The GPU page, when the VM has it (F14a).
    pub gpu: Option<crate::dev_ui::VmGpuPage>,
    /// The USB page, when the VM has it (F14a).
    pub usb: Option<crate::dev_ui::VmUsbPage>,
}

impl OrgUi {
    /// The VM Properties dialog's Attach USB question (F14a).
    pub fn on_device_precheck(
        &mut self,
        id: u64,
        result: &xen_core::operations::PrecheckResult,
    ) -> bool {
        self.vm_properties
            .as_mut()
            .and_then(|d| d.usb.as_mut())
            .is_some_and(|u| u.on_precheck(id, result))
    }
}

impl VmPropertiesDialog {
    pub fn open(conn: &ConnectionView, vm: Ref<Vm>) -> Option<Self> {
        let rec = conn.model.vms.get(&vm)?;
        Some(Self {
            connection: conn.id,
            page: VmPage::General,
            name: rec.name_label.clone(),
            description: rec.name_description.clone(),
            original: (rec.name_label.clone(), rec.name_description.clone()),
            editor: OrgEditor::new(conn, OrgObject::Vm(vm.clone())),
            dialog: None,
            gpu: crate::dev_ui::VmGpuPage::new(conn, rec),
            usb: crate::dev_ui::VmUsbPage::new(conn, rec),
            vm,
        })
    }

    /// The requests OK sends.
    pub fn requests(&self, conn: &ConnectionView) -> Vec<ActionRequest> {
        let mut out = Vec::new();
        let name = self.name.trim();
        let rename = (name != self.original.0 && !name.is_empty()).then(|| name.to_owned());
        let describe = (self.description != self.original.1).then(|| self.description.clone());
        if rename.is_some() || describe.is_some() {
            out.push(ActionRequest::Org(Box::new(OrgRequest::VmNames {
                vm: self.vm.clone(),
                name: rename,
                description: describe,
            })));
        }
        out.extend(self.editor.requests(conn));
        out.extend(self.gpu.as_ref().and_then(|g| g.request(&self.vm)));
        out
    }

    /// Draws the dialog; whether it closes.
    pub fn window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) -> bool {
        let s = org_strings();
        let Some(conn) = state.connection(self.connection).cloned() else {
            return true;
        };
        let Some(vm_rec) = conn.model.vms.get(&self.vm).cloned() else {
            return true;
        };
        let (mut ok, mut cancel) = (false, false);
        window(fill(s.vm_properties_title, &[("name", &self.original.0)])).show(ctx, |ui| {
            ui.set_width(700.0);
            let mut divider = 0.0;
            let row = ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    let d = crate::dev_strings::dev_strings();
                    let mut pages = vec![
                        (VmPage::General, s.page_general, self.name.clone()),
                        (
                            VmPage::CustomFields,
                            s.page_custom_fields,
                            self.editor.custom_fields_subtext(),
                        ),
                    ];
                    if let Some(g) = &self.gpu {
                        pages.push((VmPage::Gpu, d.page_gpu, g.subtext(&conn)));
                    }
                    if self.usb.is_some() {
                        pages.push((
                            VmPage::Usb,
                            d.page_usb,
                            crate::dev_ui::VmUsbPage::subtext(&vm_rec),
                        ));
                    }
                    if let Some(p) = crate::controls::page_list(ui, &self.page, pages) {
                        self.page = p;
                    }
                });
                divider = crate::controls::divider_gap(ui);
                ui.vertical(|ui| {
                    ui.set_width(500.0);
                    ScrollArea::vertical()
                        .id_salt("vm-props")
                        .max_height(380.0)
                        .show(ui, |ui| match self.page {
                            VmPage::General => {
                                ui.label(fill(
                                    s.general_blurb,
                                    &[("console", branding::PRODUCT_NAME)],
                                ));
                                ui.add_space(6.0);
                                Grid::new("vm-props-general").num_columns(2).show(ui, |ui| {
                                    let l = ui.label(s.name_label);
                                    ui.add(
                                        TextEdit::singleline(&mut self.name).desired_width(320.0),
                                    )
                                    .labelled_by(l.id);
                                    ui.end_row();
                                    let l = ui.label(s.description_label);
                                    ui.add(
                                        TextEdit::multiline(&mut self.description)
                                            .desired_rows(2)
                                            .desired_width(320.0),
                                    )
                                    .labelled_by(l.id);
                                    ui.end_row();
                                    self.editor.general_rows(ui, &conn);
                                });
                            }
                            VmPage::CustomFields => {
                                self.editor.custom_fields_page(ui, &conn, &mut self.dialog);
                            }
                            VmPage::Gpu => {
                                if let Some(g) = self.gpu.as_mut() {
                                    g.ui(ui, &conn, &vm_rec);
                                }
                            }
                            VmPage::Usb => {
                                if let Some(u) = self.usb.as_mut() {
                                    u.ui(ui, &conn, &self.vm, &vm_rec, ops, backend);
                                }
                            }
                        });
                });
            });
            crate::controls::paint_divider(ui, divider, row.response.rect);
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                let can = !self.name.trim().is_empty() && self.editor.valid();
                ok = row
                    .add_enabled(ButtonRole::Primary, can, Button::new(s.ok))
                    .clicked();
                cancel = row.button(ButtonRole::Cancel, s.cancel).clicked();
            });
        });
        // The page's dialogs, above it.
        if let Some(g) = self.gpu.as_mut() {
            g.add_window(ctx);
        }
        if let Some(u) = self.usb.as_mut() {
            u.attach_window(ctx, &conn, &self.vm, &vm_rec, ops, backend);
        }
        if cancel {
            return true;
        }
        if ok {
            for request in self.requests(&conn) {
                ops.request(&conn, request, backend);
            }
            return true;
        }
        false
    }
}
