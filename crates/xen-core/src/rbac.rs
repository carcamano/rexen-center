//! Role-based access control: what the logged-in user may do, checked
//! before an operation runs so the console can ask for another user's
//! credentials instead of failing (the original's `Role.CanPerform` and its
//! role elevation dialog).
//!
//! After login the connection asks `session.get_is_local_superuser`; for
//! other users (Active Directory subjects) it also reads
//! `session.get_rbac_permissions`, the permission names (lowercase
//! `class.method`, plus the names of the user's roles) the session holds.
//! [`check`] compares an operation's API calls with those permissions and,
//! when one is missing, lists the roles that could do it (from the cached
//! `role` objects: a role can perform a call when one of its subroles is that
//! permission).

use std::collections::HashSet;
use std::sync::Arc;

use xapi::{Ref, Role, RoleRecord, Session};

use crate::model::ConnectionModel;

/// What the session of a connection may do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRbac {
    /// `root` (or any local superuser): everything is allowed.
    pub is_local_superuser: bool,
    /// Permission and role names held (lowercase).
    pub permissions: HashSet<String>,
    /// An Active Directory user: their SID (`session.get_auth_user_sid`).
    pub user_sid: Option<String>,
    /// The subject that gave them access (`session.get_subject`; the
    /// user's own, or one of their groups').
    pub subject: Option<Ref<xapi::Subject>>,
}

impl SessionRbac {
    /// A local superuser.
    pub fn superuser() -> Self {
        Self {
            is_local_superuser: true,
            ..Self::default()
        }
    }

    /// Whether the session holds `method` (a [`normalize`]d name), honouring
    /// key-specific permissions (`host.add_to_other_config/key:foo`) and
    /// their `*` wildcards like the original's `Role.CanPerform`.
    fn holds(&self, method: &str) -> bool {
        self.is_local_superuser
            || self.permissions.contains(method)
            || self
                .permissions
                .iter()
                .any(|p| permission_matches(p, method))
    }
}

/// Reads what `session` may do. Servers without RBAC calls count as
/// superuser sessions (the server enforces anyway).
pub async fn fetch(session: &Session) -> xapi::Result<SessionRbac> {
    let me = session.session_ref().clone();
    match xapi::session::get_is_local_superuser(session, &me).await {
        Ok(true) => return Ok(SessionRbac::superuser()),
        Ok(false) => {}
        Err(e) if e.is_api(xapi::error::codes::MESSAGE_METHOD_UNKNOWN) => {
            return Ok(SessionRbac::superuser());
        }
        Err(e) => return Err(e),
    }
    let permissions = xapi::session::get_rbac_permissions(session, &me).await?;
    // Who the user is, for the Users tab (a local RBAC account has none).
    let user_sid = xapi::session::get_auth_user_sid(session, &me)
        .await
        .ok()
        .filter(|s| !s.is_empty());
    let subject = xapi::session::get_subject(session, &me)
        .await
        .ok()
        .filter(|s| !s.is_null());
    Ok(SessionRbac {
        is_local_superuser: false,
        permissions: permissions.iter().map(|p| p.to_lowercase()).collect(),
        user_sid,
        subject,
    })
}

/// `RbacMethod`: lowercase, without `async_`.
pub fn normalize(method: &str) -> String {
    method.to_lowercase().replace("async_", "")
}

/// Whether permission `name` (a role's `name_label`) covers `method`.
fn permission_matches(name: &str, method: &str) -> bool {
    if name == method {
        return true;
    }
    // A key-specific permission covers the call on that key only.
    match name.strip_suffix('*') {
        Some(prefix) if method.contains("/key:") => method.starts_with(prefix),
        _ => false,
    }
}

/// The result of an RBAC check.
#[derive(Debug, Clone, PartialEq)]
pub enum RbacCheck {
    /// The session may run the operation (or nothing is known about its
    /// rights: the server will decide).
    Allowed,
    /// The session lacks a permission. `authorized` are the roles that hold
    /// them all, most powerful first, without internal roles.
    Denied { authorized: Vec<Arc<RoleRecord>> },
}

/// Checks whether `rbac` covers every call in `methods`.
pub fn check(model: &ConnectionModel, rbac: Option<&SessionRbac>, methods: &[&str]) -> RbacCheck {
    let Some(rbac) = rbac else {
        return RbacCheck::Allowed;
    };
    let methods: Vec<String> = methods.iter().map(|m| normalize(m)).collect();
    if methods.iter().all(|m| rbac.holds(m)) {
        return RbacCheck::Allowed;
    }
    RbacCheck::Denied {
        authorized: authorized_roles(model, &methods),
    }
}

/// Roles (with subroles) able to make every call in `methods`, most
/// powerful first, internal roles left out (`Role.ValidRoleList`).
pub fn authorized_roles(model: &ConnectionModel, methods: &[String]) -> Vec<Arc<RoleRecord>> {
    let mut roles: Vec<Arc<RoleRecord>> = model
        .roles
        .values()
        .filter(|r| !r.subroles.is_empty() && !r.is_internal)
        .filter(|r| {
            let names = permission_names(model, &r.subroles);
            methods
                .iter()
                .all(|m| names.iter().any(|n| permission_matches(n, m)))
        })
        .cloned()
        .collect();
    roles.sort_by_key(|r| std::cmp::Reverse(role_rank(&r.name_label)));
    roles
}

/// The roles a session holds: the cached roles with subroles whose name is
/// among its permissions (`Session.Roles` in the original).
pub fn session_roles(model: &ConnectionModel, rbac: &SessionRbac) -> Vec<Arc<RoleRecord>> {
    let mut roles: Vec<Arc<RoleRecord>> = model
        .roles
        .values()
        .filter(|r| !r.subroles.is_empty() && !r.is_internal)
        .filter(|r| {
            rbac.is_local_superuser && r.name_label.eq_ignore_ascii_case("pool-admin")
                || rbac.permissions.contains(&r.name_label.to_lowercase())
        })
        .cloned()
        .collect();
    roles.sort_by_key(|r| std::cmp::Reverse(role_rank(&r.name_label)));
    roles
}

fn permission_names(model: &ConnectionModel, subroles: &[Ref<Role>]) -> Vec<String> {
    subroles
        .iter()
        .filter_map(|r| model.roles.get(r))
        .map(|r| r.name_label.to_lowercase())
        .collect()
}

/// `Role.RoleRank`: read-only lowest, pool-admin highest.
pub fn role_rank(name: &str) -> i32 {
    match name.to_lowercase().as_str() {
        "read-only" => 0,
        "vm-operator" => 1,
        "vm-admin" => 2,
        "vm-power-admin" => 3,
        "pool-operator" => 4,
        "pool-admin" => 5,
        _ => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> ConnectionModel {
        let role = |name: &str, subroles: &[&str]| json!({"uuid": name, "name_label": name, "name_description": "", "subroles": subroles, "is_internal": false});
        ConnectionModel::from_database(&json!({
            "role": {
                "OpaqueRef:p-start": role("vm.start", &[]),
                "OpaqueRef:p-migrate": role("vm.pool_migrate", &[]),
                "OpaqueRef:p-key": role("host.add_to_other_config/key:folder*", &[]),
                "OpaqueRef:op": role("vm-operator", &["OpaqueRef:p-start"]),
                "OpaqueRef:pa": role("vm-power-admin", &["OpaqueRef:p-start", "OpaqueRef:p-migrate", "OpaqueRef:p-key"]),
            }
        }))
    }

    #[test]
    fn superuser_and_unknown_rights_are_allowed() {
        let m = model();
        assert_eq!(check(&m, None, &["vm.pool_migrate"]), RbacCheck::Allowed);
        let root = SessionRbac::superuser();
        assert_eq!(
            check(&m, Some(&root), &["vm.pool_migrate"]),
            RbacCheck::Allowed
        );
    }

    #[test]
    fn missing_permission_lists_the_authorized_roles() {
        let m = model();
        let operator = SessionRbac {
            is_local_superuser: false,
            permissions: ["vm-operator", "vm.start"].map(String::from).into(),
            ..SessionRbac::default()
        };
        assert_eq!(
            check(&m, Some(&operator), &["VM.async_start"]),
            RbacCheck::Allowed
        );
        let RbacCheck::Denied { authorized } =
            check(&m, Some(&operator), &["vm.pool_migrate", "vm.start"])
        else {
            panic!("expected a denial");
        };
        let names: Vec<_> = authorized.iter().map(|r| r.name_label.as_str()).collect();
        assert_eq!(names, ["vm-power-admin"]);
        let roles = session_roles(&m, &operator);
        assert_eq!(roles[0].name_label, "vm-operator");
        // Key wildcards.
        assert!(permission_matches(
            "host.add_to_other_config/key:folder*",
            "host.add_to_other_config/key:folder.x"
        ));
        assert!(!permission_matches(
            "host.add_to_other_config/key:folder*",
            "host.add_to_other_config"
        ));
    }
}
