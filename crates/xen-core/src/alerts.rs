//! Alerts: the XAPI `message` objects shown in the notifications area, as
//! the original's `MessageAlert` / `AlarmMessageAlert`.
//!
//! Every message becomes an alert except the VM lifecycle ones
//! (`VM_STARTED`, `VM_SHUTDOWN`…), which the original draws on the
//! performance graphs instead (`Message.ShowOnGraphs`), and
//! `HA_POOL_OVERCOMMITTED`, which always comes with
//! `HA_POOL_DROP_IN_PLAN_EXISTS_FOR` (`IsSquelched`). Messages arrive and
//! go through `event.from` like any cached class, so the list is live.
//! Dismissing an alert destroys its message on the server
//! (`message.destroy`), which every console connected to the pool sees.

use std::sync::Arc;

use xapi::{Cls, Host, Message, MessageRecord, Pool, Ref, Sr, Vm};

use crate::model::ConnectionModel;
use crate::perfmon::{self, Alarm};

/// The original's `AlertPriority` (XAPI message priorities 1–5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlertPriority {
    /// 1: data loss imminent.
    Critical,
    /// 2: service loss imminent.
    Major,
    /// 3: service degraded.
    Warning,
    /// 4: service recovered.
    Minor,
    /// 5: informational.
    Information,
    /// Anything else.
    Unknown,
}

impl AlertPriority {
    pub fn from_xapi(priority: i64) -> Self {
        match priority {
            1 => Self::Critical,
            2 => Self::Major,
            3 => Self::Warning,
            4 => Self::Minor,
            5 => Self::Information,
            _ => Self::Unknown,
        }
    }
}

/// The object a message is about, if it is in the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertObject {
    Pool(Ref<Pool>),
    Host(Ref<Host>),
    Vm(Ref<Vm>),
    Sr(Ref<Sr>),
    /// Another class, or an object no longer cached.
    Other,
}

/// One alert.
#[derive(Debug, Clone)]
pub struct Alert {
    pub message: Ref<Message>,
    pub record: Arc<MessageRecord>,
    pub priority: AlertPriority,
    pub object: AlertObject,
    /// The object's name (or its UUID when it is not cached).
    pub object_name: String,
    /// For `ALARM` messages, what perfmon measured.
    pub alarm: Option<Alarm>,
}

/// VM lifecycle messages: drawn on the performance graphs, not alerts.
pub fn shows_on_graphs(name: &str) -> bool {
    matches!(
        name,
        "VM_CLONED"
            | "VM_CRASHED"
            | "VM_REBOOTED"
            | "VM_RESUMED"
            | "VM_SHUTDOWN"
            | "VM_STARTED"
            | "VM_SUSPENDED"
    )
}

/// Messages the original never shows.
pub fn is_squelched(name: &str) -> bool {
    name == "HA_POOL_OVERCOMMITTED"
}

/// The object `record` is about.
pub fn object_of(model: &ConnectionModel, record: &MessageRecord) -> (AlertObject, String) {
    let uuid = record.obj_uuid.as_str();
    let found = match record.cls {
        Cls::Pool => model
            .pools
            .iter()
            .find(|(_, p)| p.uuid == uuid)
            .map(|(r, p)| {
                (
                    AlertObject::Pool(r.clone()),
                    crate::objects::pool_name(model, p),
                )
            }),
        Cls::Host => model
            .hosts
            .iter()
            .find(|(_, h)| h.uuid == uuid)
            .map(|(r, h)| (AlertObject::Host(r.clone()), h.name_label.clone())),
        Cls::Vm => model
            .vms
            .iter()
            .find(|(_, v)| v.uuid == uuid)
            .map(|(r, v)| (AlertObject::Vm(r.clone()), v.name_label.clone())),
        Cls::Sr => model
            .srs
            .iter()
            .find(|(_, s)| s.uuid == uuid)
            .map(|(r, s)| (AlertObject::Sr(r.clone()), s.name_label.clone())),
        _ => None,
    };
    found.unwrap_or_else(|| (AlertObject::Other, uuid.to_owned()))
}

/// The alerts of one connection, newest first.
pub fn alerts(model: &ConnectionModel) -> Vec<Alert> {
    let mut out: Vec<Alert> = model
        .messages
        .iter()
        .filter(|(_, m)| !shows_on_graphs(&m.name) && !is_squelched(&m.name))
        .map(|(r, m)| {
            let (object, object_name) = object_of(model, m);
            let alarm = (m.name == "ALARM").then(|| Alarm::parse(&m.body)).flatten();
            let priority = match alarm.as_ref().map(|a| a.variable.as_str()) {
                Some(perfmon::FS_USAGE) => AlertPriority::Major,
                Some(perfmon::LOG_FS_USAGE) => AlertPriority::Warning,
                _ => AlertPriority::from_xapi(m.priority),
            };
            Alert {
                message: r.clone(),
                record: m.clone(),
                priority,
                object,
                object_name,
                alarm,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.record
            .timestamp
            .unix_seconds()
            .cmp(&a.record.timestamp.unix_seconds())
            .then_with(|| a.message.cmp(&b.message))
    });
    out
}

/// What a `FAILED_LOGIN_ATTEMPTS` message says (`FailedLoginAttemptAlert`):
/// totals and the most common sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailedLogins {
    pub total: i64,
    pub unknown: i64,
    pub known: Vec<FailedLoginSource>,
}

/// One source of failed logins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailedLoginSource {
    pub number: i64,
    pub date: String,
    pub ip: String,
    pub originator: String,
    pub username: String,
    pub useragent: String,
}

impl FailedLogins {
    /// Parses the body (`<body><total>…</total><unknown>…</unknown>
    /// <known><number>…</number><ip>…</ip>…</known>…</body>`); `None` if it
    /// cannot be read.
    pub fn parse(body: &str) -> Option<Self> {
        use quick_xml::events::Event;
        let mut reader = quick_xml::Reader::from_str(body);
        let mut out = Self::default();
        let mut path: Vec<String> = Vec::new();
        loop {
            match reader.read_event().ok()? {
                Event::Start(e) => {
                    path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                    if path.last().map(String::as_str) == Some("known") {
                        out.known.push(FailedLoginSource::default());
                    }
                }
                Event::End(_) => {
                    path.pop();
                }
                Event::Text(t) => {
                    let text = t.decode().ok()?.trim().to_owned();
                    let tag = path.last().map(String::as_str).unwrap_or_default();
                    let in_known = path.iter().any(|p| p == "known");
                    let int = || text.parse::<i64>().unwrap_or(0);
                    match (in_known, tag, out.known.last_mut()) {
                        (false, "total", _) => out.total = int(),
                        (false, "unknown", _) => out.unknown = int(),
                        (true, "number", Some(k)) => k.number = int(),
                        (true, "date", Some(k)) => k.date = text,
                        (true, "ip", Some(k)) => k.ip = text,
                        (true, "originator", Some(k)) => k.originator = text,
                        (true, "username", Some(k)) => k.username = text,
                        (true, "useragent", Some(k)) => k.useragent = text,
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Some(out)
    }
}

/// The lifecycle messages of the object with `uuid`, for the performance
/// graphs: `(time, message name)`, oldest first.
pub fn graph_events(model: &ConnectionModel, uuid: &str) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = model
        .messages
        .values()
        .filter(|m| m.obj_uuid == uuid && shows_on_graphs(&m.name))
        .filter_map(|m| Some((m.timestamp.unix_seconds()?, m.name.clone())))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_messages_become_alerts_like_the_original() {
        let model = ConnectionModel::from_database(&xapi_mock_capture());
        let all = alerts(&model);
        assert!(!all.is_empty());
        // Lifecycle messages are left to the graphs.
        assert!(all.iter().all(|a| !shows_on_graphs(&a.record.name)));
        // Newest first.
        assert!(
            all.windows(2)
                .all(|w| w[0].record.timestamp.unix_seconds()
                    >= w[1].record.timestamp.unix_seconds())
        );
        // Each points at its object when cached.
        for a in &all {
            match (&a.record.cls, &a.object) {
                (Cls::Pool, AlertObject::Pool(_))
                | (Cls::Host, AlertObject::Host(_))
                | (Cls::Vm, AlertObject::Vm(_))
                | (Cls::Sr, AlertObject::Sr(_))
                | (_, AlertObject::Other) => {}
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn failed_logins() {
        let f = FailedLogins::parse(
            "<body><total>7</total><unknown>2</unknown><known><number>5</number>\
             <date>20261125T10:00:00Z</date><ip>192.0.2.9</ip><username>admin</username>\
             </known></body>",
        )
        .unwrap();
        assert_eq!((f.total, f.unknown, f.known.len()), (7, 2, 1));
        assert_eq!(
            (f.known[0].number, f.known[0].ip.as_str()),
            (5, "192.0.2.9")
        );
        assert_eq!(f.known[0].username, "admin");
        assert!(FailedLogins::parse("plain text").is_some_and(|f| f.total == 0));
    }

    #[test]
    fn alarms_and_priorities() {
        assert_eq!(AlertPriority::from_xapi(1), AlertPriority::Critical);
        assert_eq!(AlertPriority::from_xapi(5), AlertPriority::Information);
        assert_eq!(AlertPriority::from_xapi(0), AlertPriority::Unknown);
        assert_eq!(AlertPriority::from_xapi(9), AlertPriority::Unknown);
        assert!(shows_on_graphs("VM_STARTED"));
        assert!(!shows_on_graphs("ALARM"));
        assert!(is_squelched("HA_POOL_OVERCOMMITTED"));
    }

    fn xapi_mock_capture() -> serde_json::Value {
        let text = include_str!("../../../tests/fixtures/xcp-ng-8.3-capture.json");
        serde_json::from_str(text).unwrap()
    }
}
