//! `event.from`: the long-poll that keeps a client-side cache live.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::session::Session;

xapi_enum! {
    /// What happened to the object an [`Event`] is about.
    pub enum EventOperation {
        Add => "add",
        Mod => "mod",
        Del => "del",
    }
}

/// One change notification.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub id: Value,
    /// Lower-case class name, e.g. `vm`, `host`, `pool`.
    pub class: String,
    pub operation: EventOperation,
    /// `OpaqueRef:…` of the object.
    #[serde(rename = "ref")]
    pub opaque_ref: String,
    /// Full record after the change (absent or null for `del`).
    #[serde(default)]
    pub snapshot: Option<Value>,
}

/// Result of one `event.from` call.
#[derive(Debug, Clone, Deserialize)]
pub struct EventBatch {
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub valid_ref_counts: HashMap<String, i64>,
    /// Pass back on the next call to receive only newer events.
    pub token: String,
}

impl Session {
    /// `event.from(classes, token, timeout)`.
    ///
    /// With an empty `token` the server returns every current object of the
    /// requested classes as `add` events. Otherwise it blocks for up to
    /// `timeout` seconds until something changes. The HTTP client used must
    /// have a request timeout larger than `timeout`.
    pub async fn event_from(
        &self,
        classes: &[&str],
        token: &str,
        timeout: f64,
    ) -> Result<EventBatch> {
        self.call(
            "event.from",
            vec![json!(classes), json!(token), json!(timeout)],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_batch() {
        let batch: EventBatch = serde_json::from_value(json!({
            "events": [
                {"id": "1", "timestamp": "x", "class": "vm", "operation": "mod",
                 "ref": "OpaqueRef:v", "snapshot": {"power_state": "Running"}},
                {"id": "2", "class": "vm", "operation": "del", "ref": "OpaqueRef:w"},
                {"id": "3", "class": "vm", "operation": "weird", "ref": "OpaqueRef:z", "snapshot": null}
            ],
            "valid_ref_counts": {"vm": 2},
            "token": "42,0"
        }))
        .unwrap();
        assert_eq!(batch.token, "42,0");
        assert_eq!(batch.events[0].operation, EventOperation::Mod);
        assert!(batch.events[1].snapshot.is_none());
        assert_eq!(
            batch.events[2].operation,
            EventOperation::Unknown("weird".into())
        );
    }
}
