//! JSON-RPC 2.0 transport (`POST <base>/jsonrpc`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::tls::{PinnedVerifier, TrustStore};

/// Options for building a [`JsonRpcClient`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Whole-request timeout. Long-polling clients (`event.from`) need a value
    /// comfortably above the server-side poll timeout.
    pub timeout: Duration,
    /// Connection timeout.
    pub connect_timeout: Duration,
    /// HTTP `User-Agent`.
    pub user_agent: String,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(15),
            user_agent: format!("XenAPI/{}", crate::API_VERSION),
        }
    }
}

/// A JSON-RPC client bound to one server.
///
/// For HTTPS endpoints only the certificate pinned in the [`TrustStore`] at
/// construction time is accepted; any other certificate fails the request with
/// [`Error::UntrustedCertificate`] carrying the fingerprint that was seen.
#[derive(Debug)]
pub struct JsonRpcClient {
    http: reqwest::Client,
    endpoint: Endpoint,
    url: String,
    verifier: Option<Arc<PinnedVerifier>>,
    next_id: AtomicU64,
}

impl JsonRpcClient {
    /// Builds a client for `endpoint`.
    pub fn new(endpoint: &Endpoint, trust: &TrustStore, options: &ClientOptions) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(options.timeout)
            .connect_timeout(options.connect_timeout)
            .user_agent(options.user_agent.clone())
            .tcp_keepalive(Duration::from_secs(30))
            .no_proxy();
        // A TLS config is always supplied (reqwest would otherwise look for a
        // process-wide rustls provider); for plain-HTTP endpoints it is unused.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let pinned = Arc::new(PinnedVerifier::new(
            trust.get(&endpoint.authority()),
            &provider,
        ));
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(pinned.clone())
            .with_no_client_auth();
        builder = builder.tls_backend_preconfigured(config);
        let verifier = endpoint.tls.then_some(pinned);
        Ok(Self {
            http: builder.build()?,
            endpoint: endpoint.clone(),
            url: endpoint.jsonrpc_url(),
            verifier,
            next_id: AtomicU64::new(1),
        })
    }

    /// The server this client talks to.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Calls `method` with positional `params` and decodes the result.
    ///
    /// Parameters are never logged: they contain passwords and session refs.
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Vec<Value>) -> Result<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        tracing::debug!(method, id, "xapi call");

        let response = match self.http.post(&self.url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => return Err(self.classify_send_error(e)),
        };
        let status = response.status();
        let bytes = response.bytes().await?;
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) if !status.is_success() => {
                return Err(Error::Protocol(format!("HTTP {status} from {method}")));
            }
            Err(source) => {
                return Err(Error::Decode {
                    method: method.to_owned(),
                    source,
                });
            }
        };
        let result = parse_response(value)?;
        serde_json::from_value(result).map_err(|source| Error::Decode {
            method: method.to_owned(),
            source,
        })
    }

    /// Like [`JsonRpcClient::call`] for calls that return nothing (`void`).
    pub async fn call_void(&self, method: &str, params: Vec<Value>) -> Result<()> {
        self.call::<Value>(method, params).await.map(drop)
    }

    fn classify_send_error(&self, e: reqwest::Error) -> Error {
        if let Some(fingerprint) = self.verifier.as_ref().and_then(|v| v.take_rejected()) {
            return Error::UntrustedCertificate {
                host: self.endpoint.authority(),
                fingerprint,
            };
        }
        Error::Transport(e)
    }
}

/// Extracts `result` from a JSON-RPC response or turns `error` into
/// [`Error::Api`]. Handles both v2 (`{message: code, data: [params]}`) and v1
/// (`["CODE", params…]`) error shapes.
pub(crate) fn parse_response(mut value: Value) -> Result<Value> {
    let error = value
        .get_mut("error")
        .map(Value::take)
        .unwrap_or(Value::Null);
    match error {
        Value::Null => {}
        Value::Array(items) => {
            let mut strings = items.into_iter().map(value_to_string);
            let code = strings.next().unwrap_or_else(|| "UNKNOWN_ERROR".to_owned());
            return Err(Error::Api {
                code,
                params: strings.collect(),
            });
        }
        Value::Object(mut obj) => {
            let code = obj
                .remove("message")
                .map(value_to_string)
                .unwrap_or_else(|| "UNKNOWN_ERROR".to_owned());
            let params = match obj.remove("data") {
                Some(Value::Array(items)) => items.into_iter().map(value_to_string).collect(),
                Some(Value::Null) | None => Vec::new(),
                Some(other) => vec![value_to_string(other)],
            };
            return Err(Error::Api { code, params });
        }
        other => return Err(Error::Protocol(format!("unexpected error member: {other}"))),
    }
    match value.get_mut("result") {
        Some(result) => Ok(result.take()),
        None => Err(Error::Protocol(
            "response has neither result nor error".to_owned(),
        )),
    }
}

fn value_to_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result() {
        let v = parse_response(json!({"jsonrpc":"2.0","id":1,"result":"OpaqueRef:x"})).unwrap();
        assert_eq!(v, json!("OpaqueRef:x"));
    }

    #[test]
    fn parses_v2_error() {
        let e = parse_response(json!({
            "jsonrpc":"2.0","id":1,
            "error":{"code":1,"message":"HOST_IS_SLAVE","data":["10.0.0.2"]}
        }))
        .unwrap_err();
        match e {
            Error::Api { code, params } => {
                assert_eq!(code, "HOST_IS_SLAVE");
                assert_eq!(params, vec!["10.0.0.2"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_v1_error() {
        let e =
            parse_response(json!({"id":1,"result":null,"error":["SESSION_INVALID","OpaqueRef:s"]}))
                .unwrap_err();
        assert!(e.is_api("SESSION_INVALID"));
        assert_eq!(e.to_string(), "SESSION_INVALID (OpaqueRef:s)");
    }

    #[test]
    fn missing_result_is_protocol_error() {
        assert!(matches!(
            parse_response(json!({"id":1})),
            Err(Error::Protocol(_))
        ));
    }
}
