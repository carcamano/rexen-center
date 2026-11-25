//! Calls on the `host` class.

use std::collections::HashMap;

use serde_json::json;

use super::{Host, HostRecord};
use crate::{Ref, Result, Session};

/// `host.get_all_records`.
pub async fn get_all_records(session: &Session) -> Result<HashMap<Ref<Host>, HostRecord>> {
    session.call("host.get_all_records", vec![]).await
}

/// `host.get_record`.
pub async fn get_record(session: &Session, host: &Ref<Host>) -> Result<HostRecord> {
    session.call("host.get_record", vec![json!(host)]).await
}
