//! HTTP client for the RRD handlers of one host.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::{Cf, Result, Rrd, RrdError, Updates, format_of, parse_rrd, parse_updates};

/// The format a server answered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Xml,
}

/// An `rrd_updates` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesQuery {
    /// Rows newer than this (Unix seconds); 0 for everything the archive has.
    pub start: i64,
    /// The archive, by its step in seconds (5, 60, 3600 or 86400).
    pub interval: i64,
    pub cf: Cf,
    /// `host=true`: the host's own data sources (and those of its VMs).
    pub host: bool,
    /// Only this VM's data sources.
    pub vm_uuid: Option<String>,
}

/// Reads performance data from one host (`https://<address>:<port>`).
///
/// The session reference goes in the query string, so URLs are never
/// logged and errors leave them out.
#[derive(Debug, Clone)]
pub struct RrdClient {
    http: reqwest::Client,
    base: String,
    prefer_json: bool,
    seen: Arc<Mutex<Option<Format>>>,
}

impl RrdClient {
    /// A client for `base` (`https://host:port` or `http://…`). For HTTPS,
    /// `tls` decides which certificates are accepted (see
    /// `xapi::pinned_client_config`); it is required then.
    pub fn new(
        base: &str,
        tls: Option<rustls::ClientConfig>,
        timeout: Duration,
        user_agent: &str,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(15).min(timeout))
            .user_agent(user_agent)
            .no_proxy();
        match tls {
            Some(config) => builder = builder.tls_backend_preconfigured(config),
            None if base.starts_with("https:") => {
                return Err(RrdError::Transport(
                    "an HTTPS host needs a TLS configuration".to_owned(),
                ));
            }
            None => {
                // reqwest wants a TLS backend even for plain HTTP.
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|e| RrdError::Transport(e.to_string()))?
                    .with_root_certificates(rustls::RootCertStore::empty())
                    .with_no_client_auth();
                builder = builder.tls_backend_preconfigured(config);
            }
        }
        Ok(Self {
            http: builder
                .build()
                .map_err(|e| RrdError::Transport(e.without_url().to_string()))?,
            base: base.trim_end_matches('/').to_owned(),
            prefer_json: true,
            seen: Arc::default(),
        })
    }

    /// Asks for XML instead of JSON (to check the XML path against a
    /// server that has both).
    pub fn prefer_xml(mut self) -> Self {
        self.prefer_json = false;
        self
    }

    /// The format of the last answer, once there has been one.
    pub fn format(&self) -> Option<Format> {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `GET /rrd_updates`.
    pub async fn updates(&self, session: &str, query: &UpdatesQuery) -> Result<Updates> {
        let mut params = vec![
            ("start", query.start.to_string()),
            ("cf", query.cf.as_str().to_owned()),
            ("interval", query.interval.to_string()),
        ];
        if query.host {
            params.push(("host", "true".to_owned()));
        }
        if let Some(uuid) = &query.vm_uuid {
            params.push(("vm_uuid", uuid.clone()));
        }
        let body = self.get("rrd_updates", session, params).await?;
        parse_updates(&body)
    }

    /// `GET /host_rrds`: the whole RRD of this host.
    pub async fn host_rrd(&self, session: &str) -> Result<Rrd> {
        let body = self.get("host_rrds", session, Vec::new()).await?;
        parse_rrd(&body)
    }

    /// `GET /vm_rrds?uuid=…`: the whole RRD of a VM running on this host.
    pub async fn vm_rrd(&self, session: &str, vm_uuid: &str) -> Result<Rrd> {
        let body = self
            .get("vm_rrds", session, vec![("uuid", vm_uuid.to_owned())])
            .await?;
        parse_rrd(&body)
    }

    async fn get(
        &self,
        path: &str,
        session: &str,
        mut params: Vec<(&str, String)>,
    ) -> Result<Vec<u8>> {
        params.insert(0, ("session_id", session.to_owned()));
        if self.prefer_json {
            params.push(("json", "true".to_owned()));
        }
        tracing::debug!(path, "rrd request");
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect();
        let response = self
            .http
            .get(format!("{}/{path}?{}", self.base, query.join("&")))
            .send()
            .await
            .map_err(|e| RrdError::Transport(e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(RrdError::Http(status.as_u16()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| RrdError::Transport(e.without_url().to_string()))?
            .to_vec();
        if let Some(format) = format_of(&body) {
            let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            if *seen != Some(format) {
                tracing::debug!(?format, "performance data format");
                *seen = Some(format);
            }
        }
        Ok(body)
    }
}

/// Percent-encodes a query value (everything but unreserved characters).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
