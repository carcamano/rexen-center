//! Active Directory in the window: the Users tab (`AdPage`) of a pool or
//! standalone server, with Join Domain / Leave Domain (`AdPasswordPrompt`),
//! Add Users (`ResolvingSubjectsDialog`), Remove, Log Out and Change Role
//! (`RoleSelectionDialog`), and the questions the original asks before
//! each.
//!
//! Who is logged in comes from `session.get_all_subject_identifiers`, asked
//! every 5 seconds while the tab shows (`AdPage.updateLoggedInStatus`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Button, Context, Grid, RichText, TextEdit, Ui};
use xapi::{Ref, Role, RoleRecord, Subject};
use xen_core::ad::{self, AdState, SubjectProgress};
use xen_core::operations::{AdKind, OperationKind};
use xen_core::{
    ActionRequest, AppState, BackendHandle, Command, ConnectionId, ConnectionView, OperationId,
    Password, Precheck, PrecheckResult,
};

use crate::branding;
use crate::controls::{ButtonRole, dialog_buttons};
use crate::ha_strings::{ha_strings, role_description, subject_key};
use crate::i18n::{self, fill};
use crate::ops_strings::ops_strings;
use crate::ops_ui::OpsUi;
use crate::pool_ui::{connection_name, window};

/// `AdPage.updateLoggedInStatus`'s interval.
const STATUS_INTERVAL: Duration = Duration::from_secs(5);

/// Whether `conn` is a pool (else a standalone server), for the wording.
fn is_pool(conn: &ConnectionView) -> bool {
    xen_core::pool::visible_pool(&conn.model).is_some()
}

/// Whether this client is joining or leaving a domain on `conn`.
pub fn client_busy(state: &AppState, conn: ConnectionId) -> bool {
    state.operations.iter().any(|o| {
        o.connection == Some(conn)
            && o.is_running()
            && matches!(
                o.kind,
                OperationKind::Ad(AdKind::Enable { .. } | AdKind::Disable { .. })
            )
    })
}

/// A Yes/No question (`ThreeButtonDialog`).
#[derive(Clone)]
struct Question {
    title: &'static str,
    text: String,
    no_default: bool,
}

/// What follows the questions.
enum Then {
    Run(ActionRequest),
    /// The Leave Domain prompt.
    LeavePrompt,
}

struct Flow {
    connection: ConnectionId,
    questions: VecDeque<Question>,
    then: Then,
}

/// `AdPasswordPrompt`: join (with the domain) or leave (domain shown).
struct Prompt {
    connection: ConnectionId,
    join: bool,
    domain: String,
    username: String,
    password: String,
}

/// `ResolvingSubjectsDialog`.
struct AddUsers {
    connection: ConnectionId,
    names: String,
    /// Once Grant Access was pressed: the newest operation id then (the
    /// dialog follows the next Add Users operation of its connection).
    after: Option<OperationId>,
}

/// `RoleSelectionDialog`.
struct Roles {
    connection: ConnectionId,
    subjects: Vec<Ref<Subject>>,
    /// Per role: checked, unchecked or mixed (`None`).
    rows: Vec<(Ref<Role>, Arc<RoleRecord>, Option<bool>)>,
    /// The row whose description shows.
    current: usize,
    /// The state the dialog opened with.
    initial: Vec<Option<bool>>,
}

/// The logged-in status of one connection.
#[derive(Default)]
struct Status {
    /// `None`: not known (the status is "lost").
    sids: Option<Vec<String>>,
    asked: Option<Instant>,
    pending: Option<u64>,
}

/// The Users tab's selection on one connection.
#[derive(Default, Clone)]
struct Selection {
    root: bool,
    subjects: Vec<Ref<Subject>>,
}

/// The Users tab and its dialogs.
#[derive(Default)]
pub struct AdUi {
    prompt: Option<Prompt>,
    flow: Option<Flow>,
    add: Option<AddUsers>,
    roles: Option<Roles>,
    status: HashMap<ConnectionId, Status>,
    selection: HashMap<ConnectionId, Selection>,
    expanded: HashSet<(ConnectionId, Option<Ref<Subject>>)>,
    /// `_storedDomain`, `_storedUsername`.
    stored: (String, String),
    next_id: u64,
    pending: HashMap<u64, ConnectionId>,
    /// Messages for the window's error list.
    pub errors: Vec<String>,
}

impl AdUi {
    fn next_id(&mut self, conn: ConnectionId) -> u64 {
        self.next_id += 1;
        let id = (1 << 53) + self.next_id;
        self.pending.insert(id, conn);
        id
    }

    /// The SIDs logged in on `conn`, if known.
    fn logged_in(&self, conn: ConnectionId) -> Option<&Vec<String>> {
        self.status.get(&conn).and_then(|s| s.sids.as_ref())
    }

    /// An answer to a precheck this part asked.
    pub fn on_precheck(&mut self, id: u64, result: &PrecheckResult) -> bool {
        let Some(conn) = self.pending.remove(&id) else {
            return false;
        };
        if let PrecheckResult::LoggedInSubjects(r) = result {
            let s = self.status.entry(conn).or_default();
            if s.pending == Some(id) {
                s.pending = None;
                s.sids = r.as_ref().ok().cloned();
            }
        }
        true
    }

    /// Asks who is logged in when it is time.
    fn refresh_status(&mut self, conn: &ConnectionView, backend: &BackendHandle) {
        let due = self.status.get(&conn.id).is_none_or(|s| {
            s.pending.is_none() && s.asked.is_none_or(|t| t.elapsed() >= STATUS_INTERVAL)
        });
        if !due {
            return;
        }
        let id = self.next_id(conn.id);
        let s = self.status.entry(conn.id).or_default();
        s.pending = Some(id);
        s.asked = Some(Instant::now());
        backend.send(Command::Precheck {
            connection: conn.id,
            id,
            check: Precheck::LoggedInSubjects,
        });
    }

    /// The Users tab (`AdPage`).
    pub fn users_tab(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        conn: &ConnectionView,
        backend: &BackendHandle,
    ) {
        let h = ha_strings();
        let name = connection_name(conn);
        let pool = is_pool(conn);
        let ad_state = ad::state(&conn.model, client_busy(state, conn.id));
        ui.label(RichText::new(h.ad_tab_title).strong().size(15.0));
        ui.add_space(4.0);
        let blurb = match &ad_state {
            AdState::CoordinatorUnavailable => fill(
                h.ad_coordinator_unavailable,
                &[("product", branding::PRODUCT_NAME)],
            ),
            AdState::Configuring => fill(
                if pool {
                    h.ad_configuring_blurb
                } else {
                    h.ad_configuring_blurb_host
                },
                &[("pool", &name)],
            ),
            AdState::Disabled => fill(
                if pool {
                    h.ad_not_configured_blurb
                } else {
                    h.ad_not_configured_blurb_host
                },
                &[("pool", &name)],
            ),
            AdState::Enabled { domain } => fill(
                if pool {
                    h.ad_configured_blurb
                } else {
                    h.ad_configured_blurb_host
                },
                &[("pool", &name), ("domain", domain)],
            ),
        };
        ui.label(blurb);
        ui.add_space(6.0);
        let enabled = matches!(ad_state, AdState::Enabled { .. });
        let usable = conn.status.is_connected()
            && matches!(ad_state, AdState::Enabled { .. } | AdState::Disabled);
        let join_label = if enabled {
            h.leave_domain
        } else {
            h.join_domain
        };
        if ui.add_enabled(usable, Button::new(join_label)).clicked() {
            self.join_or_leave(conn, &ad_state);
        }
        ui.add_space(8.0);
        ui.label(RichText::new(h.users_with_access).strong());
        if !enabled {
            ui.label(RichText::new(h.ad_disabled).italics());
            ui.add_space(4.0);
            ui.label(h.changes_take_effect);
            return;
        }
        self.refresh_status(conn, backend);
        ui.ctx().request_repaint_after(STATUS_INTERVAL);
        self.subject_grid(ui, conn);
        ui.add_space(6.0);
        self.buttons(ui, conn);
        ui.add_space(4.0);
        ui.label(h.changes_take_effect);
    }

    fn subject_grid(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let h = ha_strings();
        let o = ops_strings();
        let model = conn.model.clone();
        let logged = self.logged_in(conn.id).cloned();
        let selection = self.selection.entry(conn.id).or_insert_with(|| Selection {
            root: true,
            subjects: Vec::new(),
        });
        // Subjects that went away leave the selection.
        selection
            .subjects
            .retain(|s| model.subjects.contains_key(s));
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        let mut clicked: Option<Option<Ref<Subject>>> = None;
        let mut toggle: Option<Option<Ref<Subject>>> = None;
        Grid::new("ad-subjects")
            .num_columns(5)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label("");
                ui.label("");
                ui.strong(h.col_subject);
                ui.strong(h.col_roles);
                ui.strong(h.col_logged_in);
                ui.end_row();
                // The local root account first.
                let open = self.expanded.contains(&(conn.id, None));
                if expander(ui, open).clicked() {
                    toggle = Some(None);
                }
                person(ui, false).on_hover_text(h.user);
                ui.vertical(|ui| {
                    if ui.selectable_label(selection.root, h.local_root).clicked() {
                        clicked = Some(None);
                    }
                    if open {
                        ui.label("");
                        ui.label(h.always_granted);
                    }
                });
                ui.label("");
                ui.label("-");
                ui.end_row();
                for (r, s) in ad::subjects(&model) {
                    let key = (conn.id, Some(r.clone()));
                    let open = self.expanded.contains(&key);
                    if expander(ui, open).clicked() {
                        toggle = Some(Some(r.clone()));
                    }
                    let group = ad::is_group(s);
                    person(ui, group).on_hover_text(if group { h.group } else { h.user });
                    let selected = selection.subjects.contains(r);
                    ui.vertical(|ui| {
                        let shown = ad::display_name(s)
                            .or_else(|| ad::subject_name(s))
                            .unwrap_or_default();
                        if ui.selectable_label(selected, shown).clicked() {
                            clicked = Some(Some(r.clone()));
                        }
                        if open {
                            for (k, v) in ad::details(s).into_iter().skip(1) {
                                if v.is_empty() {
                                    ui.label(k);
                                } else {
                                    let value = match v.as_str() {
                                        "true" => o.yes.to_owned(),
                                        "false" => o.no.to_owned(),
                                        _ => v,
                                    };
                                    ui.label(format!("{}: {value}", subject_key(&k)));
                                }
                            }
                        }
                    });
                    let roles = ad::subject_roles(&model, s);
                    let names: Vec<String> = roles
                        .iter()
                        .map(|r| i18n::role_name(&r.name_label))
                        .collect();
                    ui.label(match (open, names.as_slice()) {
                        (true, _) => names.join("\n"),
                        (false, []) => String::new(),
                        (false, [one]) => one.clone(),
                        (false, [first, ..]) => format!("{first}…"),
                    });
                    ui.label(match (&logged, group) {
                        (Some(sids), false) => {
                            if sids.contains(&s.subject_identifier) {
                                o.yes
                            } else {
                                o.no
                            }
                        }
                        _ => "-",
                    });
                    ui.end_row();
                }
            });
        if let Some(row) = clicked {
            match (row, multi) {
                (None, false) => {
                    selection.root = true;
                    selection.subjects.clear();
                }
                (None, true) => selection.root = !selection.root,
                (Some(r), false) => {
                    selection.root = false;
                    selection.subjects = vec![r];
                }
                (Some(r), true) => {
                    if let Some(i) = selection.subjects.iter().position(|x| *x == r) {
                        selection.subjects.remove(i);
                    } else {
                        selection.subjects.push(r);
                    }
                }
            }
        }
        if let Some(row) = toggle {
            let key = (conn.id, row);
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
        }
    }

    /// `AdPage.EnableButtons` and the buttons' actions.
    fn buttons(&mut self, ui: &mut Ui, conn: &ConnectionView) {
        let h = ha_strings();
        let sel = self.selection.get(&conn.id).cloned().unwrap_or_default();
        let logged = self.logged_in(conn.id).cloned().unwrap_or_default();
        let any = sel.root || !sel.subjects.is_empty();
        let all_logged_in = !sel.root
            && sel.subjects.iter().all(|s| {
                conn.model
                    .subjects
                    .get(s)
                    .is_some_and(|r| !ad::is_group(r) && logged.contains(&r.subject_identifier))
            });
        let modify = any && !sel.root;
        ui.horizontal(|ui| {
            if ui.button(h.add).clicked() {
                self.add = Some(AddUsers {
                    connection: conn.id,
                    names: String::new(),
                    after: None,
                });
            }
            let hint = |r: egui::Response| {
                if sel.root {
                    r.on_disabled_hover_text(h.cannot_modify_root)
                } else {
                    r
                }
            };
            if hint(ui.add_enabled(modify, Button::new(h.remove))).clicked() {
                self.remove(conn, &sel.subjects);
            }
            if hint(ui.add_enabled(modify && all_logged_in, Button::new(h.logout_user))).clicked() {
                self.logout(conn, &sel.subjects);
            }
            if hint(ui.add_enabled(modify, Button::new(h.change_role))).clicked() {
                let rows = ad::role_selection(&conn.model, &sel.subjects);
                let initial = rows.iter().map(|r| r.2).collect();
                self.roles = Some(Roles {
                    connection: conn.id,
                    subjects: sel.subjects.clone(),
                    rows,
                    current: 0,
                    initial,
                });
            }
        });
    }

    /// The local superuser (or a session whose rights are unknown).
    fn superuser(conn: &ConnectionView) -> bool {
        conn.rbac.as_ref().is_none_or(|r| r.is_local_superuser)
    }

    /// `buttonJoinLeave_Click`.
    fn join_or_leave(&mut self, conn: &ConnectionView, state: &AdState) {
        let h = ha_strings();
        let name = connection_name(conn);
        match state {
            AdState::Disabled => {
                self.prompt = Some(Prompt {
                    connection: conn.id,
                    join: true,
                    domain: self.stored.0.clone(),
                    username: self.stored.1.clone(),
                    password: String::new(),
                });
            }
            AdState::Enabled { domain } => {
                let question = if Self::superuser(conn) {
                    Question {
                        title: h.ad_feature_name,
                        text: fill(h.leave_confirm, &[("pool", &name), ("domain", domain)]),
                        no_default: true,
                    }
                } else {
                    Question {
                        title: h.ad_tab_title,
                        text: fill(
                            if is_pool(conn) {
                                h.leave_warning
                            } else {
                                h.leave_warning_host
                            },
                            &[("pool", &name), ("domain", domain)],
                        ),
                        no_default: true,
                    }
                };
                self.flow = Some(Flow {
                    connection: conn.id,
                    questions: VecDeque::from([question]),
                    then: Then::LeavePrompt,
                });
            }
            _ => {}
        }
    }

    /// `ButtonRemove_Click`.
    fn remove(&mut self, conn: &ConnectionView, subjects: &[Ref<Subject>]) {
        let h = ha_strings();
        let records: Vec<&xapi::SubjectRecord> = subjects
            .iter()
            .filter_map(|s| conn.model.subjects.get(s).map(|r| &**r))
            .collect();
        let mut text = match records.as_slice() {
            [] => return,
            [one] => fill(h.remove_user_one, &[("name", ad::shown_name(one))]),
            many => fill(h.remove_user_many, &[("n", &many.len().to_string())]),
        };
        if ad::admin_exit_advice(&conn.model) {
            let (admins, all) = ad::admins_removed(&conn.model, subjects);
            let advice = if admins == all && admins > 0 {
                Some(if admins == 1 {
                    h.admin_exit_one.to_owned()
                } else {
                    h.admin_exit_many.to_owned()
                })
            } else if admins == 1 {
                Some(h.admin_exit_one_of_many.to_owned())
            } else if admins > 1 {
                Some(fill(
                    h.admin_exit_some_of_many,
                    &[("n", &admins.to_string())],
                ))
            } else {
                None
            };
            if let Some(a) = advice {
                text = format!("{text}\n\n{a} {}", h.admin_exit_advisory);
            }
        }
        let mut questions = VecDeque::from([Question {
            title: h.ad_feature_name,
            text,
            no_default: true,
        }]);
        // Removing the subject this session logged in through.
        let mine = conn.rbac.as_ref().and_then(|r| r.subject.clone());
        if let Some(me) = mine
            && let Some(rec) = subjects
                .iter()
                .find(|s| **s == me)
                .and_then(|s| conn.model.subjects.get(s))
        {
            let shown = ad::display_name(rec)
                .or_else(|| ad::subject_name(rec))
                .unwrap_or(&rec.subject_identifier);
            questions.push_back(Question {
                title: h.ad_feature_name,
                text: fill(
                    if ad::is_group(rec) {
                        h.confirm_logout_current_group
                    } else {
                        h.confirm_logout_current_user
                    },
                    &[("name", shown), ("pool", &connection_name(conn))],
                ),
                no_default: true,
            });
        }
        self.flow = Some(Flow {
            connection: conn.id,
            questions,
            then: Then::Run(ActionRequest::RemoveSubjects {
                subjects: subjects.to_vec(),
            }),
        });
    }

    /// `AdPage.ConfirmLogout`: the questions before logging `subjects` out.
    fn logout_questions(
        conn: &ConnectionView,
        subjects: &[Ref<Subject>],
        role_change: bool,
    ) -> VecDeque<Question> {
        let h = ha_strings();
        let mine = conn.rbac.as_ref().and_then(|r| r.subject.clone());
        let myself = mine.is_some_and(|m| subjects.contains(&m));
        let many = subjects.len() > 1;
        let pool = connection_name(conn);
        let one_name = subjects
            .first()
            .and_then(|s| conn.model.subjects.get(s))
            .map(|r| ad::shown_name(r).to_owned())
            .unwrap_or_default();
        let text = match (myself, role_change, many) {
            (true, true, true) => fill(h.logout_current_many_role, &[("pool", &pool)]),
            (true, true, false) => fill(h.logout_current_one_role, &[("pool", &pool)]),
            (true, false, true) => fill(h.logout_current_many, &[("pool", &pool)]),
            (true, false, false) => fill(h.logout_current_one, &[("pool", &pool)]),
            (false, true, false) => fill(h.logout_user_one_role, &[("name", &one_name)]),
            (false, true, true) => fill(
                h.logout_user_many_role,
                &[("n", &subjects.len().to_string())],
            ),
            (false, false, false) => fill(h.logout_user_one, &[("name", &one_name)]),
            (false, false, true) => fill(h.logout_user_many, &[("n", &subjects.len().to_string())]),
        };
        VecDeque::from([Question {
            title: h.ad_feature_name,
            text,
            no_default: true,
        }])
    }

    /// `ButtonLogout_Click`.
    fn logout(&mut self, conn: &ConnectionView, subjects: &[Ref<Subject>]) {
        let logged = self.logged_in(conn.id).cloned().unwrap_or_default();
        let subjects: Vec<Ref<Subject>> = subjects
            .iter()
            .filter(|s| {
                conn.model
                    .subjects
                    .get(*s)
                    .is_some_and(|r| logged.contains(&r.subject_identifier))
            })
            .cloned()
            .collect();
        if subjects.is_empty() {
            return;
        }
        self.flow = Some(Flow {
            connection: conn.id,
            questions: Self::logout_questions(conn, &subjects, false),
            then: Then::Run(ActionRequest::LogoutSubjects { subjects }),
        });
    }

    /// Draws the dialogs and runs what they ask.
    pub fn windows(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        self.flow_window(ctx, state, ops, backend);
        self.prompt_window(ctx, state, ops, backend);
        self.add_window(ctx, state, ops, backend);
        self.roles_window(ctx, state);
        // Statuses of connections that went away are dropped.
        self.status.retain(|c, _| state.connection(*c).is_some());
    }

    fn flow_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(flow) = self.flow.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(flow.connection) else {
            self.flow = None;
            return;
        };
        let Some(q) = flow.questions.front().cloned() else {
            let flow = self.flow.take().expect("open");
            match flow.then {
                Then::Run(request) => ops.dispatch(conn, request, backend),
                Then::LeavePrompt => {
                    if let AdState::Enabled { domain } = ad::state(&conn.model, false) {
                        self.prompt = Some(Prompt {
                            connection: conn.id,
                            join: false,
                            domain,
                            username: String::new(),
                            password: String::new(),
                        });
                    }
                }
            }
            return;
        };
        let o = ops_strings();
        let (mut yes, mut no) = (false, false);
        window(q.title.to_owned()).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(&q.text);
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                let y = row.button(ButtonRole::Primary, o.yes);
                let n = row.button(ButtonRole::Cancel, o.no);
                if q.no_default {
                    n.request_focus();
                }
                yes = y.clicked();
                no = n.clicked();
            });
        });
        if yes {
            flow.questions.pop_front();
        } else if no {
            self.flow = None;
        }
    }

    fn prompt_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(p.connection) else {
            self.prompt = None;
            return;
        };
        let h = ha_strings();
        let (mut ok, mut ignore, mut cancel) = (false, false, false);
        window(if p.join { h.join_title } else { h.leave_title }.to_owned()).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(if p.join {
                h.join_blurb_short.to_owned()
            } else {
                h.leaving_ad.to_owned()
            });
            ui.add_space(6.0);
            Grid::new("ad-prompt")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    let l = ui.label(h.domain);
                    ui.add_enabled(p.join, TextEdit::singleline(&mut p.domain))
                        .labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label(h.username);
                    ui.text_edit_singleline(&mut p.username).labelled_by(l.id);
                    ui.end_row();
                    let l = ui.label(h.password);
                    ui.add(TextEdit::singleline(&mut p.password).password(true))
                        .labelled_by(l.id);
                    ui.end_row();
                });
            if !p.join {
                ui.label(h.leaving_additional);
            }
            ui.add_space(8.0);
            // `EnableOkButton`.
            let ready = if p.join {
                !p.domain.trim().is_empty()
                    && !p.username.trim().is_empty()
                    && !p.password.trim().is_empty()
            } else {
                !p.username.trim().is_empty()
            };
            dialog_buttons(ui, |row| {
                ok = row
                    .add_enabled(
                        ButtonRole::Primary,
                        ready,
                        Button::new(if p.join { h.ok } else { h.disable }),
                    )
                    .clicked();
                if !p.join {
                    ignore = row.button(ButtonRole::Other, h.ignore).clicked();
                }
                cancel = row.button(ButtonRole::Cancel, h.cancel).clicked();
            });
        });
        if cancel {
            if p.join {
                self.stored = (p.domain.trim().to_owned(), p.username.trim().to_owned());
            }
            self.prompt = None;
            return;
        }
        if !ok && !ignore {
            return;
        }
        let p = self.prompt.take().expect("open");
        let request = if p.join {
            self.stored = (p.domain.trim().to_owned(), p.username.trim().to_owned());
            ActionRequest::EnableAd {
                domain: p.domain.trim().to_owned(),
                user: p.username.trim().to_owned(),
                password: Password::new(p.password),
            }
        } else if ignore {
            ActionRequest::DisableAd { credentials: None }
        } else {
            ActionRequest::DisableAd {
                credentials: Some((p.username.trim().to_owned(), Password::new(p.password))),
            }
        };
        ops.dispatch(conn, request, backend);
    }

    fn add_window(
        &mut self,
        ctx: &Context,
        state: &AppState,
        ops: &mut OpsUi,
        backend: &BackendHandle,
    ) {
        let Some(d) = self.add.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.add = None;
            return;
        };
        let h = ha_strings();
        let op = d.after.and_then(|after| {
            state.operations.iter().rev().find(|o| {
                o.id > after
                    && o.connection == Some(d.connection)
                    && matches!(o.kind, OperationKind::Ad(AdKind::AddRemove { .. }))
            })
        });
        let (mut grant, mut close) = (false, false);
        window(h.add_users_title.to_owned()).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label(fill(h.add_users_blurb, &[("pool", &connection_name(conn))]));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let l = ui.label(h.names);
                ui.add_enabled(
                    d.after.is_none(),
                    TextEdit::singleline(&mut d.names).desired_width(400.0),
                )
                .labelled_by(l.id);
            });
            if let Some(op) = op {
                ui.add_space(6.0);
                Grid::new("ad-add")
                    .num_columns(3)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong(h.col_name);
                        ui.strong(h.col_resolution);
                        ui.strong(h.col_grant);
                        ui.end_row();
                        for e in &op.subjects {
                            let (resolution, grant) = progress_texts(&e.progress);
                            ui.label(&e.name);
                            ui.label(resolution);
                            ui.label(grant);
                            ui.end_row();
                        }
                    });
                ui.add(egui::ProgressBar::new(op.progress.unwrap_or(0.0) as f32));
                ui.label(match &op.status {
                    xen_core::OperationStatus::Running => {
                        if op
                            .subjects
                            .iter()
                            .any(|e| e.progress == SubjectProgress::Resolving)
                        {
                            h.resolving_names
                        } else {
                            h.adding_resolved
                        }
                    }
                    xen_core::OperationStatus::Succeeded => h.completed,
                    _ => h.completed_with_errors,
                });
            } else if d.after.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(h.resolving_names);
                });
            }
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                if d.after.is_none() {
                    grant = row
                        .add_enabled(
                            ButtonRole::Primary,
                            !ad::split_names(&d.names).is_empty(),
                            Button::new(h.grant_access),
                        )
                        .clicked();
                }
                let done = op.is_some_and(|o| !o.is_running());
                close = row
                    .button(ButtonRole::Cancel, if done { h.close } else { h.cancel })
                    .clicked();
            });
        });
        if grant {
            let names = ad::split_names(&d.names);
            d.after = Some(state.operations.iter().map(|o| o.id).max().unwrap_or(0));
            ops.dispatch(conn, ActionRequest::AddSubjects { names }, backend);
        }
        if close {
            self.add = None;
        }
    }

    fn roles_window(&mut self, ctx: &Context, state: &AppState) {
        let Some(d) = self.roles.as_mut() else {
            return;
        };
        let Some(conn) = state.connection(d.connection) else {
            self.roles = None;
            return;
        };
        let h = ha_strings();
        let records: Vec<&xapi::SubjectRecord> = d
            .subjects
            .iter()
            .filter_map(|s| conn.model.subjects.get(s).map(|r| &**r))
            .collect();
        let groups = records.iter().filter(|r| ad::is_group(r)).count();
        let blurb = match records.as_slice() {
            [one] => fill(
                if ad::is_group(one) {
                    h.select_role_group
                } else {
                    h.select_role_user
                },
                &[(
                    "name",
                    ad::display_name(one)
                        .or_else(|| ad::subject_name(one))
                        .unwrap_or(h.unknown_ad_user),
                )],
            ),
            many if groups == many.len() => h.select_role_group_many.to_owned(),
            _ if groups == 0 => h.select_role_user_many.to_owned(),
            _ => h.select_role_mixed.to_owned(),
        };
        let chosen: Vec<Ref<Role>> = d
            .rows
            .iter()
            .filter(|r| r.2 == Some(true))
            .map(|r| r.0.clone())
            .collect();
        // `UpdateButtonsAndInfo`: Save once something changes; the advice
        // when pool-admin is taken from someone.
        let changed = d.rows.iter().zip(&d.initial).any(|(r, i)| r.2 != *i)
            && ad::roles_change_anything(&conn.model, &d.subjects, &chosen);
        let admin_removed = d.rows.iter().zip(&d.initial).any(|(r, i)| {
            r.1.name_label == "pool-admin" && r.2 == Some(false) && *i != Some(false)
        }) && ad::admin_exit_advice(&conn.model);
        let (mut save, mut cancel) = (false, false);
        window(h.roles_title.to_owned()).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(blurb);
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.strong(h.roles_column);
                    let mut pick = None;
                    for (i, (_, role, state)) in d.rows.iter().enumerate() {
                        // A mixed role (some of the subjects have it) shows
                        // unchecked until one is picked.
                        let r = ui.radio(*state == Some(true), i18n::role_name(&role.name_label));
                        if r.clicked() {
                            pick = Some(i);
                        }
                        if r.hovered() {
                            d.current = i;
                        }
                    }
                    // `CheckRowByIndex`: one role only.
                    if let Some(i) = pick {
                        for (j, r) in d.rows.iter_mut().enumerate() {
                            r.2 = Some(i == j);
                        }
                        d.current = i;
                    }
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.set_max_width(300.0);
                    egui::ScrollArea::vertical()
                        .id_salt("role-description")
                        .max_height(170.0)
                        .show(ui, |ui| {
                            if let Some((_, role, _)) = d.rows.get(d.current) {
                                ui.label(role_description(&role.name_label));
                            }
                        });
                });
            });
            if admin_removed {
                ui.colored_label(ui.visuals().warn_fg_color, h.roles_admin_advice);
            }
            ui.add_space(8.0);
            dialog_buttons(ui, |row| {
                save = row
                    .add_enabled(ButtonRole::Primary, changed, Button::new(h.roles_save))
                    .clicked();
                cancel = row.button(ButtonRole::Cancel, h.cancel).clicked();
            });
        });
        if cancel {
            self.roles = None;
            return;
        }
        if !save {
            return;
        }
        let d = self.roles.take().expect("open");
        let logged = self.logged_in(d.connection).cloned().unwrap_or_default();
        let log_out = ad::logged_in_to_log_out(&conn.model, &d.subjects, &chosen, &logged);
        let questions = if log_out.is_empty() {
            VecDeque::new()
        } else {
            Self::logout_questions(conn, &log_out, true)
        };
        self.flow = Some(Flow {
            connection: d.connection,
            questions,
            then: Then::Run(ActionRequest::SetSubjectRoles {
                subjects: d.subjects,
                roles: chosen,
                log_out,
            }),
        });
    }

    /// A dialog of the Users tab is open (tests).
    #[cfg(test)]
    pub fn dialog_open(&self) -> bool {
        self.prompt.is_some() || self.flow.is_some() || self.add.is_some() || self.roles.is_some()
    }
}

/// The row expander (the original's triangle).
fn expander(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let c = rect.center();
    let points = if open {
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 3.0),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -4.0),
            c + egui::vec2(3.0, 0.0),
            c + egui::vec2(-2.0, 4.0),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response
}

/// A user (or group: two users) icon.
fn person(ui: &mut Ui, group: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 16.0), egui::Sense::hover());
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let draw = |x: f32| {
        let head = egui::pos2(rect.left() + x, rect.top() + 5.0);
        ui.painter().circle_filled(head, 3.0, color);
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(head.x, rect.top() + 12.5),
                egui::vec2(8.0, 6.0),
            ),
            2.0,
            color,
        );
    };
    if group {
        draw(6.0);
        draw(12.0);
    } else {
        draw(9.0);
    }
    response
}

/// The resolution and grant columns of Add Users.
fn progress_texts(p: &SubjectProgress) -> (String, String) {
    let h = ha_strings();
    match p {
        SubjectProgress::Resolving => (h.resolving.to_owned(), String::new()),
        SubjectProgress::ResolveFailed { .. } => {
            (h.could_not_resolve.to_owned(), h.failed_to_add.to_owned())
        }
        SubjectProgress::Adding { resolved } => (
            fill(h.resolved_as, &[("name", resolved)]),
            h.adding_to_list.to_owned(),
        ),
        SubjectProgress::Added { resolved } => (
            fill(h.resolved_as, &[("name", resolved)]),
            h.added_to_list.to_owned(),
        ),
        SubjectProgress::AddFailed { resolved, .. } => (
            fill(h.resolved_as, &[("name", resolved)]),
            h.failed_to_add.to_owned(),
        ),
        SubjectProgress::AlreadyHasAccess { resolved } => (
            fill(h.resolved_as, &[("name", resolved)]),
            fill(h.already_has_access, &[("name", resolved)]),
        ),
    }
}
