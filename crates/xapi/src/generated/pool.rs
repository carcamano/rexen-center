//! Calls on the `pool` class.

use std::collections::HashMap;

use super::{Pool, PoolRecord};
use crate::{Ref, Result, Session};

/// `pool.get_all_records`.
pub async fn get_all_records(session: &Session) -> Result<HashMap<Ref<Pool>, PoolRecord>> {
    session.call("pool.get_all_records", vec![]).await
}
