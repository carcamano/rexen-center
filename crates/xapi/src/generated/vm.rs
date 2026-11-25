//! Calls on the `VM` class.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{Vm, VmRecord};
use crate::{Ref, Result, Session};

/// `VM.get_all_records` (includes templates, snapshots and control domains).
pub async fn get_all_records(session: &Session) -> Result<HashMap<Ref<Vm>, VmRecord>> {
    session.call("VM.get_all_records", vec![]).await
}

/// `VM.start(vm, start_paused, force)`. Returns when the VM is running.
pub async fn start(session: &Session, vm: &Ref<Vm>, start_paused: bool, force: bool) -> Result<()> {
    session
        .call::<Value>(
            "VM.start",
            vec![json!(vm), json!(start_paused), json!(force)],
        )
        .await
        .map(drop)
}

/// `VM.clean_shutdown(vm)`. Returns when the guest has halted.
pub async fn clean_shutdown(session: &Session, vm: &Ref<Vm>) -> Result<()> {
    session
        .call::<Value>("VM.clean_shutdown", vec![json!(vm)])
        .await
        .map(drop)
}
