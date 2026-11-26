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
    /// Where the hand-written uploads connect.
    target: Target,
    idle: Duration,
}

/// The server of an [`HttpTransfer`]: address, port and TLS.
#[derive(Debug, Clone)]
struct Target {
    host: String,
    port: u16,
    /// `host:port`, for the `Host` header.
    authority: String,
    tls: Option<Arc<rustls::ClientConfig>>,
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
        let https = base.starts_with("https:");
        let authority = base
            .split_once("://")
            .map_or(base, |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.contains(']') => (
                h.to_owned(),
                p.parse()
                    .map_err(|_| Error::InvalidEndpoint(authority.clone()))?,
            ),
            _ => (authority.clone(), if https { 443 } else { 80 }),
        };
        let target = Target {
            host,
            port,
            authority,
            tls: https.then(|| Arc::new(config.clone())),
        };
        builder = builder.tls_backend_preconfigured(config);
        Ok(Self {
            http: builder.build()?,
            base: base.trim_end_matches('/').to_owned(),
            target,
            idle,
        })
    }

    /// `/path?query` of a request.
    fn request_target(&self, path: &str, params: &[(&str, &str)]) -> String {
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect();
        format!("/{}?{}", path.trim_start_matches('/'), query.join("&"))
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

    /// `GET path?params`, read piece by piece with [`Download::chunk`]: the
    /// body is never held whole. A status other than success is an error.
    pub async fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<Download> {
        let response = self
            .http
            .get(self.url(path, params))
            .send()
            .await
            .map_err(|e| Error::Transport(e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Protocol(format!("HTTP {status} from /{path}")));
        }
        Ok(Download { response })
    }

    /// `PUT path?params` with `length` bytes read from `reader` as the
    /// body, `buffer` bytes at a time: one buffer is in memory whatever the
    /// size. `sent` is told the bytes sent so far; `cancelled` is asked
    /// before each piece.
    ///
    /// It is written by hand on its own connection, as the original's
    /// `HTTP.Put`: the body goes out while the answer is read, because
    /// XAPI's `/import` answers before it has read the body and ends its
    /// answer once it has taken everything in. Whatever goes wrong (the
    /// server refuses, the link drops, the file fails or ends before
    /// `length` bytes: a short read, the user cancels) the connection is
    /// closed at once, so the server never waits for the rest. A file
    /// error is [`Error::Io`], a cancellation [`Error::Cancelled`].
    #[allow(clippy::too_many_arguments)]
    pub async fn put_reader<R>(
        &self,
        path: &str,
        params: &[(&str, &str)],
        reader: R,
        length: u64,
        buffer: usize,
        sent: Arc<dyn Fn(u64) + Send + Sync>,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let t = &self.target;
        let tcp = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::net::TcpStream::connect((t.host.as_str(), t.port)),
        )
        .await
        .map_err(|_| link_error("connecting timed out"))?
        .map_err(|e| link_error(&e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        let head = format!(
            "PUT {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: XenAPI/{}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
            self.request_target(path, params),
            t.authority,
            crate::API_VERSION
        );
        let up = Upload {
            head,
            reader,
            length,
            buffer: buffer.max(1),
            sent,
            cancelled,
            idle: self.idle,
            path,
        };
        match &t.tls {
            None => up.run(tcp).await,
            Some(config) => {
                let name = rustls::pki_types::ServerName::try_from(
                    t.host
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .to_owned(),
                )
                .map_err(|_| Error::InvalidEndpoint(t.authority.clone()))?;
                let stream = tokio::time::timeout(
                    Duration::from_secs(15),
                    tokio_rustls::TlsConnector::from(config.clone()).connect(name, tcp),
                )
                .await
                .map_err(|_| link_error("the TLS handshake timed out"))?
                .map_err(|e| link_error(&e.to_string()))?;
                up.run(stream).await
            }
        }
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

/// A download in progress ([`HttpTransfer::get`]).
#[derive(Debug)]
pub struct Download {
    response: reqwest::Response,
}

impl Download {
    /// The next piece of the body, or `None` at its end. A server that
    /// sends no length ends the body by closing the connection, so the
    /// caller must check that the content is complete.
    pub async fn chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        self.response
            .chunk()
            .await
            .map_err(|e| Error::Transport(e.without_url()))
    }
}

/// A broken link or a server that stopped answering, during an upload.
fn link_error(why: &str) -> Error {
    Error::Protocol(format!("the upload connection failed: {why}"))
}

/// The hand-written `PUT` of [`HttpTransfer::put_reader`].
struct Upload<'a, R> {
    head: String,
    reader: R,
    length: u64,
    buffer: usize,
    sent: Arc<dyn Fn(u64) + Send + Sync>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    idle: Duration,
    path: &'a str,
}

impl<R: tokio::io::AsyncRead + Unpin> Upload<'_, R> {
    /// Sends the request on `stream` while reading the answer; returns once
    /// both ended. Dropping `stream` on any error closes the connection.
    async fn run<S>(self, stream: S) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Upload {
            head,
            mut reader,
            length,
            buffer,
            sent,
            cancelled,
            idle,
            path,
        } = self;
        let (mut rd, mut wr) = tokio::io::split(stream);
        let timed = |what: &'static str| move |_| link_error(what);
        let writing = async {
            tokio::time::timeout(idle, wr.write_all(head.as_bytes()))
                .await
                .map_err(timed("sending timed out"))?
                .map_err(|e| link_error(&e.to_string()))?;
            let mut piece = vec![0u8; buffer];
            let mut done = 0u64;
            while done < length {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                let want = usize::try_from((length - done).min(buffer as u64)).unwrap_or(buffer);
                let n = reader.read(&mut piece[..want]).await.map_err(Error::Io)?;
                if n == 0 {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("the file ended after {done} of {length} bytes"),
                    )));
                }
                tokio::time::timeout(idle, wr.write_all(&piece[..n]))
                    .await
                    .map_err(timed("sending timed out"))?
                    .map_err(|e| link_error(&e.to_string()))?;
                done += n as u64;
                sent(done);
            }
            tokio::time::timeout(idle, wr.flush())
                .await
                .map_err(timed("sending timed out"))?
                .map_err(|e| link_error(&e.to_string()))?;
            Ok::<(), Error>(())
        };
        // The answer: its status line and headers, then whatever follows
        // until the server closes the connection.
        let reading = async {
            let mut got = Vec::new();
            let mut chunk = [0u8; 4096];
            let end = loop {
                if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i;
                }
                if got.len() > 64 << 10 {
                    return Err(Error::Protocol(format!("a bad answer from /{path}")));
                }
                let n = rd
                    .read(&mut chunk)
                    .await
                    .map_err(|e| link_error(&e.to_string()))?;
                if n == 0 {
                    return Err(link_error("the server closed the connection"));
                }
                got.extend_from_slice(&chunk[..n]);
            };
            let status_line = String::from_utf8_lossy(&got[..end])
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            let code: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| Error::Protocol(format!("a bad answer from /{path}")))?;
            if !(200..300).contains(&code) {
                return Err(Error::Protocol(format!("HTTP {code} from /{path}")));
            }
            loop {
                let n = rd
                    .read(&mut chunk)
                    .await
                    .map_err(|e| link_error(&e.to_string()))?;
                if n == 0 {
                    return Ok::<(), Error>(());
                }
            }
        };
        tokio::pin!(writing, reading);
        let mut wrote = false;
        loop {
            tokio::select! {
                r = &mut writing, if !wrote => {
                    r?;
                    wrote = true;
                }
                r = tokio::time::timeout(
                    if wrote { idle } else { Duration::MAX },
                    &mut reading,
                ) => {
                    r.map_err(|_| link_error("the server did not finish its answer"))??;
                    return if wrote {
                        Ok(())
                    } else {
                        Err(link_error("the server ended its answer before the upload"))
                    };
                }
            }
        }
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
