//! `event.from` results: the long-poll that keeps a client-side cache live
//! (hand-written core). The generated `event` module has the raw calls
//! (`event::from`, `event::inject`); [`EventBatch`] is what `event.from` returns.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::EventOperation;
use crate::error::Result;
use crate::session_handle::Session;

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
