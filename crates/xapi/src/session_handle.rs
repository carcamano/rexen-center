//! XAPI sessions (hand-written core).
//!
//! The generated `session` module holds the `session.*` calls; this module is
//! the authenticated handle every generated call takes.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::SessionClass;
use crate::error::Result;
use crate::refs::Ref;
use crate::transport::JsonRpcClient;

/// An authenticated session on one server.
///
/// Cheap to clone. [`Session::with_client`] reuses the same session over a
/// different HTTP client, e.g. one with a longer timeout for `event.from`
/// long-polls that must not block regular calls.
#[derive(Debug, Clone)]
pub struct Session {
    client: Arc<JsonRpcClient>,
    session_ref: Ref<SessionClass>,
}

impl Session {
    /// `session.login_with_password(user, password, version, originator)`.
    pub async fn login_with_password(
        client: Arc<JsonRpcClient>,
        username: &str,
        password: &str,
        originator: &str,
    ) -> Result<Self> {
        let session_ref = client
            .call(
                "session.login_with_password",
                vec![
                    json!(username),
                    json!(password),
                    json!(crate::API_VERSION),
                    json!(originator),
                ],
            )
            .await?;
        Ok(Self {
            client,
            session_ref,
        })
    }

    /// `session.logout`. Errors are returned but the session is unusable afterwards either way.
    pub async fn logout(&self) -> Result<()> {
        self.client
            .call::<Value>("session.logout", vec![json!(self.session_ref)])
            .await?;
        Ok(())
    }

    /// The same session over another client bound to the same server.
    pub fn with_client(&self, client: Arc<JsonRpcClient>) -> Self {
        Self {
            client,
            session_ref: self.session_ref.clone(),
        }
    }

    /// The underlying client.
    pub fn client(&self) -> &Arc<JsonRpcClient> {
        &self.client
    }

    /// The opaque session reference. Treat it as a secret: never log it.
    pub fn session_ref(&self) -> &Ref<SessionClass> {
        &self.session_ref
    }

    /// Calls `method` with the session reference prepended to `params`.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Vec<Value>) -> Result<T> {
        let mut all = Vec::with_capacity(params.len() + 1);
        all.push(json!(self.session_ref));
        all.extend(params);
        self.client.call(method, all).await
    }

    /// Like [`Session::call`] for calls that return nothing (`void`).
    pub async fn call_void(&self, method: &str, params: Vec<Value>) -> Result<()> {
        self.call::<Value>(method, params).await.map(drop)
    }
}
