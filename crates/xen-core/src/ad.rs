//! Active Directory: the rules of the original's Users tab (`AdPage`), its
//! dialogs and the AD actions.
//!
//! A pool joined to a domain has `external_auth_type = "AD"` on its
//! coordinator and the domain in `external_auth_service_name`. The users
//! and groups given access are `subject` objects (their directory details
//! in `other_config`), each with roles; a domain user logs in with the most
//! powerful role among its own subject's and its groups'.

use std::collections::HashMap;
use std::sync::Arc;

use xapi::{Ref, Role, RoleRecord, Subject, SubjectRecord};

use crate::model::ConnectionModel;
use crate::objects;
use crate::pool::{self, Restriction};
use crate::rbac;

/// `Auth.AUTH_TYPE_AD`.
pub const AUTH_TYPE_AD: &str = "AD";

/// `Subject.SUBJECT_NAME_KEY` and friends.
pub const SUBJECT_NAME_KEY: &str = "subject-name";
pub const SUBJECT_DISPLAYNAME_KEY: &str = "subject-displayname";
pub const SUBJECT_IS_GROUP_KEY: &str = "subject-is-group";

/// Where the pool stands with a domain (`AdPage.checkAdType`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdState {
    /// The coordinator is unknown.
    CoordinatorUnavailable,
    /// Joining or leaving runs.
    Configuring,
    /// Not joined.
    Disabled,
    /// Joined to `domain`.
    Enabled { domain: String },
}

/// The pool's AD state; `client_busy` is whether this client is joining or
/// leaving the domain on it.
pub fn state(model: &ConnectionModel, client_busy: bool) -> AdState {
    let Some(coordinator) = model.coordinator() else {
        return AdState::CoordinatorUnavailable;
    };
    if client_busy {
        AdState::Configuring
    } else if coordinator.external_auth_type.is_empty() {
        AdState::Disabled
    } else {
        // An unknown type counts as AD, as in the original.
        AdState::Enabled {
            domain: coordinator.external_auth_service_name.clone(),
        }
    }
}

/// `Subject.SubjectName` (`None` when absent).
pub fn subject_name(s: &SubjectRecord) -> Option<&str> {
    s.other_config.get(SUBJECT_NAME_KEY).map(String::as_str)
}

/// `Subject.DisplayName`.
pub fn display_name(s: &SubjectRecord) -> Option<&str> {
    s.other_config
        .get(SUBJECT_DISPLAYNAME_KEY)
        .map(String::as_str)
}

/// `Subject.IsGroup`.
pub fn is_group(s: &SubjectRecord) -> bool {
    s.other_config
        .get(SUBJECT_IS_GROUP_KEY)
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// The name the original shows: display name, else subject name, else the
/// SID.
pub fn shown_name(s: &SubjectRecord) -> &str {
    display_name(s)
        .or_else(|| subject_name(s))
        .unwrap_or(&s.subject_identifier)
}

/// `Subject.ExtractKvpInfo`: the display name and subject name, a blank
/// line, then the other details (key, value) in key order, without the
/// internal ids. Keys are the raw `other_config` keys; the UI names them.
pub fn details(s: &SubjectRecord) -> Vec<(String, String)> {
    let mut info: Vec<(&String, &String)> = s
        .other_config
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "subject-gid"
                    | "subject-uid"
                    | "subject-sid"
                    | "subject-is-group"
                    | "subject-gecos"
                    | "subject-upn"
            )
        })
        .collect();
    let mut out = Vec::new();
    if info.is_empty() {
        return out;
    }
    for key in [SUBJECT_DISPLAYNAME_KEY, SUBJECT_NAME_KEY] {
        if let Some(pos) = info.iter().position(|(k, _)| k.as_str() == key) {
            let (_, v) = info.remove(pos);
            out.push((v.clone(), String::new()));
        }
    }
    out.push((String::new(), String::new()));
    info.sort_by(|a, b| a.0.cmp(b.0));
    out.extend(info.into_iter().map(|(k, v)| (k.clone(), v.clone())));
    out
}

/// A subject's roles, most powerful first (`roles.Sort(); Reverse()`).
pub fn subject_roles(model: &ConnectionModel, s: &SubjectRecord) -> Vec<Arc<RoleRecord>> {
    let mut roles: Vec<Arc<RoleRecord>> = s
        .roles
        .iter()
        .filter_map(|r| model.roles.get(r))
        .cloned()
        .collect();
    roles.sort_by_key(|r| std::cmp::Reverse(rbac::role_rank(&r.name_label)));
    roles
}

/// The roles a subject can be given (`RoleSelectionDialog`): those with
/// permissions, without the internal ones, most powerful first.
pub fn selectable_roles(model: &ConnectionModel) -> Vec<(Ref<Role>, Arc<RoleRecord>)> {
    let mut roles: Vec<(Ref<Role>, Arc<RoleRecord>)> = model
        .roles
        .iter()
        .filter(|(_, r)| !r.subroles.is_empty() && !r.is_internal)
        .map(|(k, r)| (k.clone(), r.clone()))
        .collect();
    roles.sort_by(|a, b| {
        rbac::role_rank(&b.1.name_label)
            .cmp(&rbac::role_rank(&a.1.name_label))
            .then_with(|| a.1.name_label.cmp(&b.1.name_label))
    });
    roles
}

/// The subjects of the Users tab, in the original's default order (by
/// subject name).
pub fn subjects(model: &ConnectionModel) -> Vec<(&Ref<Subject>, &SubjectRecord)> {
    let mut v: Vec<(&Ref<Subject>, &SubjectRecord)> =
        model.subjects.iter().map(|(r, s)| (r, &**s)).collect();
    v.sort_by(|a, b| {
        objects::natural_cmp(
            subject_name(a.1).unwrap_or(""),
            subject_name(b.1).unwrap_or(""),
        )
        .then(a.0.cmp(b.0))
    });
    v
}

/// The role selection dialog's starting state for `selected`: per role,
/// checked when every subject has it, unchecked when none has, else mixed
/// (`None`).
pub fn role_selection(
    model: &ConnectionModel,
    selected: &[Ref<Subject>],
) -> Vec<(Ref<Role>, Arc<RoleRecord>, Option<bool>)> {
    selectable_roles(model)
        .into_iter()
        .map(|(r, rec)| {
            let with = selected
                .iter()
                .filter(|s| model.subjects.get(*s).is_some_and(|s| s.roles.contains(&r)))
                .count();
            let state = if with == selected.len() && with > 0 {
                Some(true)
            } else if with == 0 {
                Some(false)
            } else {
                None
            };
            (r, rec, state)
        })
        .collect()
}

/// `AddRemoveRolesAction`: the roles to add to and remove from `subject`
/// so it has exactly `roles` (among the selectable ones).
pub fn role_changes(
    model: &ConnectionModel,
    subject: &SubjectRecord,
    roles: &[Ref<Role>],
) -> (Vec<Ref<Role>>, Vec<Ref<Role>>) {
    let selectable = selectable_roles(model);
    let add = selectable
        .iter()
        .filter(|(r, _)| roles.contains(r) && !subject.roles.contains(r))
        .map(|(r, _)| r.clone())
        .collect();
    let remove = selectable
        .iter()
        .filter(|(r, _)| !roles.contains(r) && subject.roles.contains(r))
        .map(|(r, _)| r.clone())
        .collect();
    (add, remove)
}

/// Whether the role selection changes something for some subject
/// (`RoleSelectionDialog.UpdateButtonsAndInfo`).
pub fn roles_change_anything(
    model: &ConnectionModel,
    selected: &[Ref<Subject>],
    roles: &[Ref<Role>],
) -> bool {
    selected
        .iter()
        .filter_map(|s| model.subjects.get(s))
        .any(|s| {
            let (a, r) = role_changes(model, s, roles);
            !a.is_empty() || !r.is_empty()
        })
}

/// Whether removing pool-admin from someone shows the "exit procedure"
/// advice (`RoleSelectionDialog` and the remove question): on Stockholm or
/// later with pool secret rotation allowed.
pub fn admin_exit_advice(model: &ConnectionModel) -> bool {
    pool::stockholm_or_greater(model.coordinator().map(|h| &**h))
        && !pool::feature_forbidden(model, Restriction::PoolSecretRotation)
}

/// Whether `subject` has the pool-admin role.
pub fn is_pool_admin(model: &ConnectionModel, s: &SubjectRecord) -> bool {
    s.roles
        .iter()
        .filter_map(|r| model.roles.get(r))
        .any(|r| r.name_label == "pool-admin")
}

/// Which of the removed subjects are pool admins, for the remove question
/// (`AdPage.ButtonRemove_Click`): `(admins, all)`.
pub fn admins_removed(model: &ConnectionModel, removed: &[Ref<Subject>]) -> (usize, usize) {
    let subjects: Vec<&SubjectRecord> = removed
        .iter()
        .filter_map(|s| model.subjects.get(s).map(|s| &**s))
        .collect();
    let admins = subjects.iter().filter(|s| is_pool_admin(model, s)).count();
    (admins, subjects.len())
}

/// The subjects whose roles change and who are logged in: they are logged
/// out once the change is made (`AdPage.ButtonChangeRoles_Click`).
pub fn logged_in_to_log_out(
    model: &ConnectionModel,
    selected: &[Ref<Subject>],
    roles: &[Ref<Role>],
    logged_in: &[String],
) -> Vec<Ref<Subject>> {
    let mut wanted: Vec<&Ref<Role>> = roles.iter().collect();
    wanted.sort();
    selected
        .iter()
        .filter(|s| {
            model.subjects.get(*s).is_some_and(|rec| {
                if !logged_in.contains(&rec.subject_identifier) {
                    return false;
                }
                let mut have: Vec<&Ref<Role>> = rec.roles.iter().collect();
                have.sort();
                have != wanted
            })
        })
        .cloned()
        .collect()
}

/// Splits the names typed in the Add Users dialog
/// (`ResolvingSubjectsDialog.BeginResolve`): comma or semicolon separated,
/// trimmed, without blanks or repeats, in order.
pub fn split_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in text.split([',', ';']) {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

/// What became of one name in Add Users (`ListViewItemSubjectWrapper`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectProgress {
    /// "Resolving...".
    Resolving,
    /// Not found or refused (`error`: the error code and parameters).
    ResolveFailed { error: Vec<String> },
    /// Found as `resolved` (its subject name), being added.
    Adding { resolved: String },
    /// Added.
    Added { resolved: String },
    /// Found, but adding it failed.
    AddFailed {
        resolved: String,
        error: Vec<String>,
    },
    /// Already has access (`AD_USER_ALREADY_HAS_ACCESS`).
    AlreadyHasAccess { resolved: String },
}

/// One entered name and its progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectEntry {
    pub name: String,
    pub progress: SubjectProgress,
}

/// `AD_FAILURE_WITH_HOST`-style errors of `EnableAdAction` that mean the
/// credentials were wrong (the original asks again).
pub fn is_credentials_failure(e: &xapi::Error) -> bool {
    e.is_api("POOL_AUTH_ENABLE_FAILED_WRONG_CREDENTIALS")
        || e.is_api("AUTH_ENABLE_FAILED_WRONG_CREDENTIALS")
}

/// The details of a subject keyed by their `other_config` key, for tests.
pub fn info_map(s: &SubjectRecord) -> HashMap<&str, &str> {
    s.other_config
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// RBAC calls each AD operation makes.
pub mod rbac_calls {
    pub const LEAVE: &[&str] = &["pool.disable_external_auth"];
    pub const ADD: &[&str] = &["subject.create"];
    pub const REMOVE: &[&str] = &["session.logout_subject_identifier", "subject.destroy"];
    pub const LOGOUT: &[&str] = &["session.logout_subject_identifier"];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn subject(other: serde_json::Value) -> SubjectRecord {
        serde_json::from_value(
            json!({"uuid": "s", "subject_identifier": "S-1", "other_config": other, "roles": []}),
        )
        .unwrap()
    }

    #[test]
    fn subject_details_like_the_original() {
        let s = subject(json!({
            "subject-name": "LAB\\alice", "subject-displayname": "Alice", "subject-sid": "S-1",
            "subject-is-group": "false", "subject-upn": "alice@lab", "subject-account-locked": "false",
            "subject-account-disabled": "false"
        }));
        assert_eq!(shown_name(&s), "Alice");
        assert!(!is_group(&s));
        assert_eq!(
            details(&s),
            vec![
                ("Alice".into(), String::new()),
                ("LAB\\alice".into(), String::new()),
                (String::new(), String::new()),
                ("subject-account-disabled".into(), "false".into()),
                ("subject-account-locked".into(), "false".into()),
            ]
        );
        let bare = subject(json!({}));
        assert_eq!(shown_name(&bare), "S-1");
        assert!(details(&bare).is_empty());
        assert_eq!(
            split_names(" LAB\\alice, bob;; alice ,LAB\\alice;"),
            ["LAB\\alice", "bob", "alice"]
        );
    }
}
