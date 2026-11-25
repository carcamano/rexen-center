//! XAPI's HTTP handlers beside JSON-RPC that move files (`HTTP_actions` in
//! the original): `GET` a download into a file, `PUT` a file. Both take
//! the session and the task to report to in the query string
//! (`?session_id=…&task_id=…`), so URLs are never logged and errors leave
//! them out. The body streams: a large file is never held in memory.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};

/// A client for the HTTP handlers of one server (`https://host:port`).
#[derive(Debug, Clone)]
pub struct HttpTransfer {
    http: reqwest::Client,
    base: String,
}

fn encode(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl HttpTransfer {
    /// A client for `base`; for HTTPS, `tls` decides which certificates are
    /// accepted (see [`crate::pinned_client_config`]). `idle` bounds the
    /// wait for each piece of the transfer, not the whole of it.
    pub fn new(base: &str, tls: Option<rustls::ClientConfig>, idle: Duration) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(idle)
            .user_agent(format!("XenAPI/{}", crate::API_VERSION))
            .no_proxy();
        let config = match tls {
            Some(c) => c,
            None if base.starts_with("https:") => {
                return Err(Error::Protocol(
                    "an HTTPS server needs a TLS configuration".to_owned(),
                ));
            }
            None => {
                // reqwest wants a TLS backend even for plain HTTP.
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()?
                    .with_root_certificates(rustls::RootCertStore::empty())
                    .with_no_client_auth()
            }
        };
        builder = builder.tls_backend_preconfigured(config);
        Ok(Self {
            http: builder.build()?,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    fn url(&self, path: &str, params: &[(&str, &str)]) -> String {
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect();
        format!(
            "{}/{}?{}",
            self.base,
            path.trim_start_matches('/'),
            query.join("&")
        )
    }

    /// `GET path?params` into the file at `to` (created or truncated).
    /// `received` is told the bytes written so far; `cancelled` is asked
    /// between pieces. Returns the size written.
    pub async fn get_to_file(
        &self,
        path: &str,
        params: &[(&str, &str)],
        to: &Path,
        received: &(dyn Fn(i64) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<i64> {
        use tokio::io::AsyncWriteExt;
        let mut response = self
            .http
            .get(self.url(path, params))
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Protocol(format!("HTTP {status} from /{path}")));
        }
        let mut file = tokio::fs::File::create(to).await.map_err(Error::Io)?;
        let mut total = 0i64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Error::Transport(e.without_url()))?
        {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            file.write_all(&chunk).await.map_err(Error::Io)?;
            total += chunk.len() as i64;
            received(total);
        }
        file.flush().await.map_err(Error::Io)?;
        Ok(total)
    }

    /// `PUT path?params` with the file at `from` as the body.
    pub async fn put_file(&self, path: &str, params: &[(&str, &str)], from: &Path) -> Result<()> {
        let file = tokio::fs::File::open(from).await.map_err(Error::Io)?;
        let length = file.metadata().await.map_err(Error::Io)?.len();
        let response = self
            .http
            .put(self.url(path, params))
            .header(reqwest::header::CONTENT_LENGTH, length)
            .body(reqwest::Body::from(file))
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Protocol(format!("HTTP {status} from /{path}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_encoded() {
        let t = HttpTransfer::new("http://127.0.0.1:1", None, Duration::from_secs(1)).unwrap();
        assert_eq!(
            t.url(
                "host_backup",
                &[("session_id", "OpaqueRef:a b"), ("task_id", "x/y")]
            ),
            "http://127.0.0.1:1/host_backup?session_id=OpaqueRef:a%20b&task_id=x%2Fy"
        );
    }
}
