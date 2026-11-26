//! A simulated Active Directory domain for the mock.
//!
//! **The domain.** [`AD_DOMAIN`] (`lab.example`, NetBIOS name `LAB`) holds
//! the users and groups of [`AD_USERS`]; [`AD_ADMIN`] may join servers to
//! it. Nothing outside the mock talks to it.
//!
//! **Calls.**
//!
//! * `pool.enable_external_auth(pool, config, service_name, auth_type)`:
//!   `auth_type` must be `AD` and `service_name` (or `config["domain"]`)
//!   the domain (`POOL_AUTH_ENABLE_FAILED_DOMAIN_LOOKUP_FAILED`), with the
//!   join credentials in `config["user"]` / `config["pass"]`
//!   (`POOL_AUTH_ENABLE_FAILED_WRONG_CREDENTIALS`, with the host); a pool
//!   already joined answers `POOL_AUTH_ALREADY_ENABLED`. Every server gets
//!   `external_auth_type`, `external_auth_service_name` and
//!   `external_auth_configuration`.
//! * `pool.disable_external_auth(pool, config)`: clears them
//!   (`POOL_AUTH_DISABLE_FAILED` when not joined); the subjects stay, as in
//!   XAPI.
//! * `auth.get_subject_identifier(name)` (`LAB\name`, `name@lab.example` or
//!   `name`; `SUBJECT_CANNOT_BE_RESOLVED`),
//!   `auth.get_subject_information_from_identifier(sid)` and
//!   `auth.get_group_membership(sid)`; `AUTH_IS_DISABLED` while the pool is
//!   not joined.
//! * `subject.create(record)` (`SUBJECT_ALREADY_EXISTS`; a new subject has
//!   the pool-admin role, the datamodel's default), `subject.destroy` (the
//!   sessions it granted end, a group's members' too),
//!   `subject.add_to_roles` / `remove_from_roles` (`ROLE_ALREADY_EXISTS`,
//!   `ROLE_NOT_FOUND`).
//! * Logins of domain users while joined: the session gets the most
//!   powerful role of the user's subject and of its groups' subjects; a
//!   user with no subject (or no role) is refused with
//!   `SESSION_AUTHORIZATION_FAILED`, a wrong password with
//!   `SESSION_AUTHENTICATION_FAILED`.
//! * `session.get_all_subject_identifiers`,
//!   `session.logout_subject_identifier(sid)` (every session of that user
//!   but the caller's), `session.get_subject`, `get_auth_user_sid` and
//!   `get_auth_user_name`.

use serde_json::{Value, json};

use crate::ops::{MockSession, ROLES, rank};
use crate::{Db, NULL_REF, Reply, api_error, str_param};

/// The simulated domain.
pub const AD_DOMAIN: &str = "lab.example";
/// Its NetBIOS name, used in subject names (`LAB\alice`).
const NETBIOS: &str = "LAB";
/// The account allowed to join servers to the domain, and its password.
/// Test-only credentials.
pub const AD_ADMIN: (&str, &str) = ("Administrator", "ad-admin-password");

/// One directory entry: `(account, display name, SID, password, groups)`;
/// a group has no password.
pub type DirectoryEntry = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static [&'static str],
);

/// The directory. Test-only credentials.
pub const AD_USERS: &[DirectoryEntry] = &[
    (
        "Administrator",
        "Domain Administrator",
        "S-1-5-21-1000-500",
        Some("ad-admin-password"),
        &["Domain Admins"],
    ),
    (
        "alice",
        "Alice Operator",
        "S-1-5-21-1000-1101",
        Some("alice-password"),
        &[],
    ),
    (
        "bob",
        "Bob Admin",
        "S-1-5-21-1000-1102",
        Some("bob-password"),
        &["VM Admins"],
    ),
    (
        "carol",
        "Carol Nobody",
        "S-1-5-21-1000-1103",
        Some("carol-password"),
        &[],
    ),
    (
        "dave",
        "Dave Pool",
        "S-1-5-21-1000-1104",
        Some("dave-password"),
        &[],
    ),
    ("VM Admins", "VM Admins", "S-1-5-21-1000-2201", None, &[]),
    (
        "Domain Admins",
        "Domain Admins",
        "S-1-5-21-1000-512",
        None,
        &[],
    ),
];

type Entry = DirectoryEntry;

/// The account named `name` (`LAB\name`, `name@lab.example` or `name`).
fn lookup(name: &str) -> Option<&'static Entry> {
    let account = name
        .rsplit_once('\\')
        .map(|(d, a)| {
            (
                d.eq_ignore_ascii_case(NETBIOS) || d.eq_ignore_ascii_case(AD_DOMAIN),
                a,
            )
        })
        .or_else(|| {
            name.split_once('@')
                .map(|(a, d)| (d.eq_ignore_ascii_case(AD_DOMAIN), a))
        })
        .map_or(Some(name), |(ok, a)| ok.then_some(a))?;
    AD_USERS.iter().find(|e| e.0.eq_ignore_ascii_case(account))
}

fn by_sid(sid: &str) -> Option<&'static Entry> {
    AD_USERS.iter().find(|e| e.2 == sid)
}

/// `auth.get_subject_information_from_identifier`.
fn information(e: &Entry) -> Value {
    let group = e.3.is_none();
    json!({
        "subject-name": format!("{NETBIOS}\\{}", e.0),
        "subject-displayname": e.1,
        "subject-sid": e.2,
        "subject-is-group": group.to_string(),
        "subject-uid": e.2.rsplit('-').next().unwrap_or_default(),
        "subject-gid": "10513",
        "subject-upn": if group { String::new() } else { format!("{}@{AD_DOMAIN}", e.0) },
        "subject-account-disabled": "false",
        "subject-account-locked": "false",
        "subject-account-expired": "false",
        "subject-password-expired": "false",
    })
}

/// SIDs of `e` and of its groups.
fn identities(e: &Entry) -> Vec<&'static str> {
    let mut sids = vec![e.2];
    sids.extend(e.4.iter().filter_map(|g| lookup(g)).map(|g| g.2));
    sids
}

/// Session calls handled here.
pub(crate) fn session_call(call: &str) -> bool {
    matches!(
        call,
        "get_all_subject_identifiers"
            | "logout_subject_identifier"
            | "get_subject"
            | "get_auth_user_sid"
            | "get_auth_user_name"
    )
}

/// Calls handled by [`Db::ad_call`].
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(
        (class, call),
        ("pool", "enable_external_auth" | "disable_external_auth")
            | (
                "auth",
                "get_subject_identifier"
                    | "get_subject_information_from_identifier"
                    | "get_group_membership"
            )
            | (
                "subject",
                "create" | "destroy" | "add_to_roles" | "remove_from_roles"
            )
    )
}

impl Db {
    /// The domain this server is joined to, if any.
    fn ad_joined(&self) -> bool {
        self.objects.iter().any(|((c, _), rec)| {
            c == "host" && rec.get("external_auth_type").and_then(Value::as_str) == Some("AD")
        })
    }

    /// The subject whose identifier is `sid`.
    fn subject_by_sid(&self, sid: &str) -> Option<String> {
        self.objects
            .iter()
            .find(|((c, _), rec)| {
                c == "subject" && rec.get("subject_identifier").and_then(Value::as_str) == Some(sid)
            })
            .map(|((_, r), _)| r.clone())
    }

    /// The name of role `r`.
    fn role_name(&self, r: &str) -> Option<String> {
        self.record("role", r)
            .and_then(|rec| rec.get("name_label"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Logs a domain user in (see the module documentation).
    pub(crate) fn ad_login(&self, user: &str, password: &str) -> Result<MockSession, &'static str> {
        let failed = "SESSION_AUTHENTICATION_FAILED";
        if !self.ad_joined() {
            return Err(failed);
        }
        let Some(e) = lookup(user) else {
            return Err(failed);
        };
        if e.3 != Some(password) {
            return Err(failed);
        }
        let mut best: Option<(usize, String, String)> = None;
        for sid in identities(e) {
            let Some(subject) = self.subject_by_sid(sid) else {
                continue;
            };
            let roles: Vec<String> = self
                .record("subject", &subject)
                .and_then(|r| r.get("roles"))
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(Value::as_str)
                        .filter_map(|r| self.role_name(r))
                        .collect()
                })
                .unwrap_or_default();
            for role in roles.iter().filter(|r| ROLES.contains(&r.as_str())) {
                let k = rank(role);
                // The user's own subject wins a tie with a group's.
                if best.as_ref().is_none_or(|(b, _, _)| k > *b) {
                    best = Some((k, role.clone(), subject.clone()));
                }
            }
        }
        let Some((_, role, subject)) = best else {
            return Err("SESSION_AUTHORIZATION_FAILED");
        };
        Ok(MockSession {
            role: Some(role),
            user: user.to_owned(),
            sid: Some(e.2.to_owned()),
            subject: Some(subject),
        })
    }

    /// The `session.*` calls of [`session_call`].
    pub(crate) fn ad_session_call(
        &mut self,
        call: &str,
        params: &[Value],
        session: &str,
        me: &MockSession,
    ) -> Reply {
        match call {
            "get_all_subject_identifiers" => {
                let mut sids: Vec<&str> = self
                    .sessions
                    .values()
                    .filter_map(|s| s.sid.as_deref())
                    .collect();
                sids.sort_unstable();
                sids.dedup();
                Ok(json!(sids))
            }
            "logout_subject_identifier" => {
                let sid = str_param(params, 1).to_owned();
                self.sessions
                    .retain(|r, s| r == session || s.sid.as_deref() != Some(sid.as_str()));
                Ok(json!(""))
            }
            "get_subject" => Ok(json!(me.subject.as_deref().unwrap_or(NULL_REF))),
            "get_auth_user_sid" => Ok(json!(me.sid.as_deref().unwrap_or_default())),
            "get_auth_user_name" => Ok(json!(me.user)),
            _ => api_error("MESSAGE_METHOD_UNKNOWN", &[call]),
        }
    }

    /// The calls of [`handles`].
    pub(crate) fn ad_call(&mut self, class: &str, call: &str, params: &[Value]) -> Option<Reply> {
        if !handles(class, call) {
            return None;
        }
        let p = |i| str_param(params, i);
        Some(match (class, call) {
            ("pool", "enable_external_auth") => self.enable_external_auth(params),
            ("pool", "disable_external_auth") => {
                if !self.ad_joined() {
                    return Some(api_error(
                        "POOL_AUTH_DISABLE_FAILED",
                        &[NULL_REF, "external authentication is not enabled"],
                    ));
                }
                for h in self.refs("host") {
                    self.modify("host", &h, |rec| {
                        rec.insert("external_auth_type".into(), json!(""));
                        rec.insert("external_auth_service_name".into(), json!(""));
                        rec.insert("external_auth_configuration".into(), json!({}));
                    });
                }
                Ok(json!(""))
            }
            ("auth", _) if !self.ad_joined() => api_error("AUTH_IS_DISABLED", &[]),
            ("auth", "get_subject_identifier") => match lookup(p(1)) {
                Some(e) => Ok(json!(e.2)),
                None => api_error(
                    "SUBJECT_CANNOT_BE_RESOLVED",
                    &[&format!("{} could not be found in {AD_DOMAIN}", p(1))],
                ),
            },
            ("auth", "get_subject_information_from_identifier") => match by_sid(p(1)) {
                Some(e) => Ok(information(e)),
                None => api_error("SUBJECT_CANNOT_BE_RESOLVED", &[p(1)]),
            },
            ("auth", "get_group_membership") => match by_sid(p(1)) {
                Some(e) => Ok(json!(identities(e)[1..].to_vec())),
                None => api_error("SUBJECT_CANNOT_BE_RESOLVED", &[p(1)]),
            },
            ("subject", "create") => {
                let record = params.get(1).cloned().unwrap_or(json!({}));
                let sid = record
                    .get("subject_identifier")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if self.subject_by_sid(&sid).is_some() {
                    return Some(api_error("SUBJECT_ALREADY_EXISTS", &[]));
                }
                let (r, uuid) = self.new_ref("subject");
                let admin = self
                    .refs("role")
                    .into_iter()
                    .find(|r| self.role_name(r).as_deref() == Some("pool-admin"));
                self.insert(
                    "subject",
                    &r,
                    json!({
                        "uuid": uuid,
                        "subject_identifier": sid,
                        "other_config": record.get("other_config").cloned().unwrap_or(json!({})),
                        "roles": admin.map(|a| vec![a]).unwrap_or_default(),
                    }),
                );
                Ok(json!(r))
            }
            ("subject", "destroy") => {
                if !self.exists("subject", p(1)) {
                    return Some(api_error("HANDLE_INVALID", &["subject", p(1)]));
                }
                // The sessions it granted end (a group's members too).
                let gone = p(1).to_owned();
                self.sessions
                    .retain(|_, s| s.subject.as_deref() != Some(gone.as_str()));
                self.remove("subject", p(1));
                Ok(json!(""))
            }
            ("subject", "add_to_roles" | "remove_from_roles") => {
                let (subject, role) = (p(1).to_owned(), p(2).to_owned());
                let Some(rec) = self.record("subject", &subject) else {
                    return Some(api_error("HANDLE_INVALID", &["subject", &subject]));
                };
                if !self.exists("role", &role) {
                    return Some(api_error("HANDLE_INVALID", &["role", &role]));
                }
                let has = rec
                    .get("roles")
                    .and_then(Value::as_array)
                    .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(role.as_str())));
                if call == "add_to_roles" {
                    if has {
                        return Some(api_error("ROLE_ALREADY_EXISTS", &[]));
                    }
                    self.push_ref("subject", &subject, "roles", &role);
                } else {
                    if !has {
                        return Some(api_error("ROLE_NOT_FOUND", &[]));
                    }
                    self.drop_ref("subject", &subject, "roles", &role);
                }
                Ok(json!(""))
            }
            _ => api_error("MESSAGE_METHOD_UNKNOWN", &[call]),
        })
    }

    fn enable_external_auth(&mut self, params: &[Value]) -> Reply {
        let config = params.get(2).cloned().unwrap_or(json!({}));
        let get = |k: &str| {
            config
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let service = str_param(params, 3).to_owned();
        let auth_type = str_param(params, 4);
        let host = self.refs("host").into_iter().next().unwrap_or_default();
        if self.ad_joined() {
            return api_error("POOL_AUTH_ALREADY_ENABLED", &[&host]);
        }
        let domain = if service.is_empty() {
            get("domain")
        } else {
            service.clone()
        };
        if auth_type != "AD" || !domain.eq_ignore_ascii_case(AD_DOMAIN) {
            return api_error(
                "POOL_AUTH_ENABLE_FAILED_DOMAIN_LOOKUP_FAILED",
                &[&host, &format!("cannot find the domain {domain}")],
            );
        }
        let user = get("user");
        let admin = lookup(&user).is_some_and(|e| e.0 == AD_ADMIN.0);
        if !admin || get("pass") != AD_ADMIN.1 {
            return api_error(
                "POOL_AUTH_ENABLE_FAILED_WRONG_CREDENTIALS",
                &[&host, "The user name or password is wrong"],
            );
        }
        let stored = json!({"domain": AD_DOMAIN, "user": user});
        for h in self.refs("host") {
            let stored = stored.clone();
            self.modify("host", &h, |rec| {
                rec.insert("external_auth_type".into(), json!("AD"));
                rec.insert("external_auth_service_name".into(), json!(AD_DOMAIN));
                rec.insert("external_auth_configuration".into(), stored);
            });
        }
        Ok(json!(""))
    }

    /// References of every object of `class`.
    pub(crate) fn refs(&self, class: &str) -> Vec<String> {
        self.objects
            .keys()
            .filter(|(c, _)| c == class)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_in_every_form() {
        for n in [
            "alice",
            "LAB\\alice",
            "lab.example\\ALICE",
            "alice@lab.example",
        ] {
            assert_eq!(lookup(n).map(|e| e.2), Some("S-1-5-21-1000-1101"), "{n}");
        }
        assert!(lookup("OTHER\\alice").is_none());
        assert!(lookup("alice@other.example").is_none());
        assert_eq!(
            identities(lookup("bob").unwrap()),
            ["S-1-5-21-1000-1102", "S-1-5-21-1000-2201"]
        );
    }
}
