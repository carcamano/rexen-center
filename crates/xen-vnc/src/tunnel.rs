//! The XAPI console tunnel: `CONNECT <path of console.location>` with the
//! session cookie, over TLS to the host that runs the VM. After
//! `HTTP/1.x 200` the stream is plain RFB (`HTTP.HttpConnectStream`,
//! `XSVNCScreen.ConnectHostedConsole` in the original).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{Result, VncError};

/// Largest HTTP reply header accepted before the RFB stream starts.
const MAX_HEADER: usize = 16 * 1024;

/// A parsed `console.location`, e.g.
/// `https://192.0.2.10/console?uuid=…` or `…/console?ref=OpaqueRef:…`.
#[derive(Clone, PartialEq, Eq)]
pub struct ConsoleLocation {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, sent in the `CONNECT` line.
    pub path: String,
}

impl fmt::Debug for ConsoleLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The query names the console, not a secret, but keep logs short.
        write!(f, "ConsoleLocation({})", self.authority())
    }
}

impl ConsoleLocation {
    /// Parses an `http(s)://host[:port]/path?query` location.
    pub fn parse(location: &str) -> Result<Self> {
        let bad = || VncError::Location(location.to_owned());
        let location = location.trim();
        let (tls, rest) = if let Some(rest) = strip_prefix_ci(location, "https://") {
            (true, rest)
        } else if let Some(rest) = strip_prefix_ci(location, "http://") {
            (false, rest)
        } else {
            return Err(bad());
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or_else(bad)?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad())?,
                None if after.is_empty() => default_port,
                None => return Err(bad()),
            };
            (host.to_owned(), port)
        } else if let Some((host, port)) = authority.split_once(':') {
            (host.to_owned(), port.parse().map_err(|_| bad())?)
        } else {
            (authority.to_owned(), default_port)
        };
        if host.is_empty()
            || host.contains(char::is_whitespace)
            || path.contains(char::is_whitespace)
        {
            return Err(bad());
        }
        Ok(Self {
            tls,
            host,
            port,
            path: path.to_owned(),
        })
    }

    /// `host:port` (`[v6]:port`), the key certificates are pinned under.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

/// A byte stream carrying RFB.
pub trait ConsoleStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> ConsoleStream for T {}

/// Opens the tunnel to `location` with session `session_ref` (a secret:
/// never logged). `tls` must be given for an `https` location; it decides
/// which certificates are accepted.
pub async fn open(
    location: &ConsoleLocation,
    session_ref: &str,
    tls: Option<Arc<rustls::ClientConfig>>,
    timeout: Duration,
) -> Result<Box<dyn ConsoleStream>> {
    let work = async {
        let tcp = TcpStream::connect((location.host.as_str(), location.port)).await?;
        tcp.set_nodelay(true)?;
        if location.tls {
            let config = tls.ok_or_else(|| {
                VncError::Tls(rustls::Error::General("no TLS configuration".into()))
            })?;
            let name = ServerName::try_from(location.host.clone())
                .map_err(|_| VncError::Location(location.authority()))?;
            let stream = tokio_rustls::TlsConnector::from(config)
                .connect(name, tcp)
                .await
                .map_err(tls_error)?;
            let stream: Box<dyn ConsoleStream> =
                Box::new(connect(stream, location, session_ref).await?);
            Ok(stream)
        } else {
            let stream: Box<dyn ConsoleStream> =
                Box::new(connect(tcp, location, session_ref).await?);
            Ok(stream)
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| VncError::Timeout)?
}

/// A rustls error hidden inside an I/O error is reported as a TLS error.
fn tls_error(e: std::io::Error) -> VncError {
    let is_tls = e
        .get_ref()
        .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some());
    if is_tls {
        let inner = e.into_inner().expect("checked above");
        match inner.downcast::<rustls::Error>() {
            Ok(tls) => VncError::Tls(*tls),
            Err(other) => VncError::Io(std::io::Error::other(other)),
        }
    } else {
        VncError::Io(e)
    }
}

/// Sends the `CONNECT` request over an open stream and reads the reply.
/// The returned stream starts with the server's first RFB byte (the reader
/// buffer keeps anything read past the header).
pub async fn connect<S: ConsoleStream>(
    stream: S,
    location: &ConsoleLocation,
    session_ref: &str,
) -> Result<BufReader<S>> {
    let mut stream = BufReader::new(stream);
    let request = format!(
        "CONNECT {} HTTP/1.0\r\nHost: {}\r\nCookie: session_id={}\r\n\r\n",
        location.path, location.host, session_ref
    );
    stream.get_mut().write_all(request.as_bytes()).await?;
    stream.get_mut().flush().await?;

    let mut header = Vec::new();
    let mut status_line = None;
    loop {
        let mut line = Vec::new();
        let limit = (MAX_HEADER - header.len().min(MAX_HEADER)) as u64;
        let n = AsyncReadExt::take(&mut stream, limit)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Err(VncError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        header.extend_from_slice(&line);
        if header.len() >= MAX_HEADER {
            return Err(VncError::protocol("console reply header too long"));
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if status_line.is_none() {
            status_line = Some(text.to_owned());
        } else if text.is_empty() {
            break;
        }
    }
    let status_line = status_line.unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let reason = parts.next().unwrap_or_default().to_owned();
    if !version.starts_with("HTTP/") {
        return Err(VncError::protocol(format!(
            "unexpected console reply: {status_line:.60}"
        )));
    }
    if status != 200 {
        return Err(VncError::Refused { status, reason });
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    #[test]
    fn parses_xapi_console_locations() {
        let l = ConsoleLocation::parse("https://192.0.2.10/console?uuid=abc").unwrap();
        assert_eq!(
            (l.tls, l.host.as_str(), l.port, l.path.as_str()),
            (true, "192.0.2.10", 443, "/console?uuid=abc")
        );
        assert_eq!(l.authority(), "192.0.2.10:443");
        let l = ConsoleLocation::parse("http://127.0.0.1:8080/console?ref=OpaqueRef:x").unwrap();
        assert_eq!((l.tls, l.port), (false, 8080));
        assert_eq!(l.path, "/console?ref=OpaqueRef:x");
        let l = ConsoleLocation::parse("HTTPS://[fd00::1]:4443/console").unwrap();
        assert_eq!(l.host, "fd00::1");
        assert_eq!(l.authority(), "[fd00::1]:4443");
        assert!(ConsoleLocation::parse("ftp://h/console").is_err());
        assert!(ConsoleLocation::parse("https:///console").is_err());
        assert!(ConsoleLocation::parse("https://h:x/console").is_err());
    }

    #[tokio::test]
    async fn sends_connect_with_the_session_cookie_and_keeps_early_rfb_bytes() {
        let (client, mut server) = duplex(4096);
        let location = ConsoleLocation::parse("https://h.example/console?uuid=u1").unwrap();
        let server_task = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            while !got.ends_with(b"\r\n\r\n") {
                let n = server.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            // Reply and the first RFB bytes in one write.
            server
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\nRFB 003.008\n")
                .await
                .unwrap();
            String::from_utf8(got).unwrap()
        });
        let mut stream = connect(client, &location, "OpaqueRef:secret")
            .await
            .unwrap();
        let mut version = [0u8; 12];
        stream.read_exact(&mut version).await.unwrap();
        assert_eq!(&version, b"RFB 003.008\n");
        let request = server_task.await.unwrap();
        assert_eq!(
            request,
            "CONNECT /console?uuid=u1 HTTP/1.0\r\nHost: h.example\r\n\
             Cookie: session_id=OpaqueRef:secret\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn a_refusal_carries_the_status() {
        let (client, mut server) = duplex(4096);
        let location = ConsoleLocation::parse("http://h/console?uuid=gone").unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let _ = server.read(&mut buf).await;
            let _ = server.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n").await;
        });
        let err = connect(client, &location, "s").await.unwrap_err();
        assert!(
            matches!(&err, VncError::Refused { status: 404, reason } if reason == "Not Found"),
            "{err:?}"
        );
    }
}
