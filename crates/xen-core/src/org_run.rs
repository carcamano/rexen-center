//! How the F13 operations run (see [`crate::org`]): the request's plan is
//! made from the connection's model when the operation starts, then its
//! calls are made one by one, in the original's order (destructive calls
//! last, as `FolderAction` does), with the progress moving per call.

use xapi::Session;

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::operations::TaskOutcome;
use crate::org::{OrgCall, OrgObject, PlanError};

/// Runs an F13 request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let ActionRequest::Org(request) = request else {
        unreachable!("not an organization request")
    };
    let model = crate::storage_run::model(shared, connection);
    let calls = match request.plan(&model) {
        Ok(calls) => calls,
        Err(e) => {
            let code = match e {
                PlanError::FolderExists => "FOLDER_ALREADY_EXISTS",
                PlanError::NoPool => "POOL_GONE",
            };
            return TaskOutcome::Failed(xapi::Error::Api {
                code: code.to_owned(),
                params: Vec::new(),
            });
        }
    };
    let n = calls.len().max(1) as f64;
    for (i, call) in calls.iter().enumerate() {
        if let Err(e) = make(session, call).await {
            return TaskOutcome::Failed(e);
        }
        shared.update_operation(op, |o| o.progress = Some((i + 1) as f64 / n));
    }
    TaskOutcome::Succeeded
}

/// Makes one call.
pub(crate) async fn make(s: &Session, call: &OrgCall) -> xapi::Result<()> {
    match call {
        OrgCall::RemoveFromOtherConfig { object, key } => match object {
            OrgObject::Pool(r) => xapi::pool::remove_from_other_config(s, r, key).await,
            OrgObject::Host(r) => xapi::host::remove_from_other_config(s, r, key).await,
            OrgObject::Vm(r) => xapi::vm::remove_from_other_config(s, r, key).await,
            OrgObject::Sr(r) => xapi::sr::remove_from_other_config(s, r, key).await,
            OrgObject::Vdi(r) => xapi::vdi::remove_from_other_config(s, r, key).await,
            OrgObject::Network(r) => xapi::network::remove_from_other_config(s, r, key).await,
        },
        OrgCall::AddToOtherConfig { object, key, value } => match object {
            OrgObject::Pool(r) => xapi::pool::add_to_other_config(s, r, key, value).await,
            OrgObject::Host(r) => xapi::host::add_to_other_config(s, r, key, value).await,
            OrgObject::Vm(r) => xapi::vm::add_to_other_config(s, r, key, value).await,
            OrgObject::Sr(r) => xapi::sr::add_to_other_config(s, r, key, value).await,
            OrgObject::Vdi(r) => xapi::vdi::add_to_other_config(s, r, key, value).await,
            OrgObject::Network(r) => xapi::network::add_to_other_config(s, r, key, value).await,
        },
        OrgCall::AddTag { object, tag } => match object {
            OrgObject::Pool(r) => xapi::pool::add_tags(s, r, tag).await,
            OrgObject::Host(r) => xapi::host::add_tags(s, r, tag).await,
            OrgObject::Vm(r) => xapi::vm::add_tags(s, r, tag).await,
            OrgObject::Sr(r) => xapi::sr::add_tags(s, r, tag).await,
            OrgObject::Vdi(r) => xapi::vdi::add_tags(s, r, tag).await,
            OrgObject::Network(r) => xapi::network::add_tags(s, r, tag).await,
        },
        OrgCall::RemoveTag { object, tag } => match object {
            OrgObject::Pool(r) => xapi::pool::remove_tags(s, r, tag).await,
            OrgObject::Host(r) => xapi::host::remove_tags(s, r, tag).await,
            OrgObject::Vm(r) => xapi::vm::remove_tags(s, r, tag).await,
            OrgObject::Sr(r) => xapi::sr::remove_tags(s, r, tag).await,
            OrgObject::Vdi(r) => xapi::vdi::remove_tags(s, r, tag).await,
            OrgObject::Network(r) => xapi::network::remove_tags(s, r, tag).await,
        },
        OrgCall::RemoveFromGuiConfig { pool, key } => {
            xapi::pool::remove_from_gui_config(s, pool, key).await
        }
        OrgCall::AddToGuiConfig { pool, key, value } => {
            xapi::pool::add_to_gui_config(s, pool, key, value).await
        }
        OrgCall::SetName { vm, name } => xapi::vm::set_name_label(s, vm, name).await,
        OrgCall::SetDescription { vm, description } => {
            xapi::vm::set_name_description(s, vm, description).await
        }
    }
}
