//! Server address parsing.

use std::fmt;

use crate::error::{Error, Result};

/// Where a XAPI server listens.
///
/// HTTPS on port 443 is the default. Plain HTTP is accepted only when asked
/// for explicitly with an `http://` prefix (the original client allowed it on
/// ports 80/8080, and it keeps local test servers simple).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// Parses `host`, `host:port`, `[v6]:port`, `https://host[:port]` or
    /// `http://host[:port]` (a trailing path is ignored).
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        let (tls, rest) = if let Some(rest) = strip_prefix_ci(input, "https://") {
            (true, rest)
        } else if let Some(rest) = strip_prefix_ci(input, "http://") {
            (false, rest)
        } else {
            (true, input)
        };
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty() {
            return Err(Error::InvalidEndpoint(input.to_owned()));
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6
                .split_once(']')
                .ok_or_else(|| Error::InvalidEndpoint(input.to_owned()))?;
            let port = match after.strip_prefix(':') {
                Some(p) => parse_port(p, input)?,
                None if after.is_empty() => default_port,
                None => return Err(Error::InvalidEndpoint(input.to_owned())),
            };
            (host.to_owned(), port)
        } else if authority.matches(':').count() > 1 {
            // Bare IPv6 address without brackets: no port.
            (authority.to_owned(), default_port)
        } else if let Some((host, port)) = authority.split_once(':') {
            (host.to_owned(), parse_port(port, input)?)
        } else {
            (authority.to_owned(), default_port)
        };
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err(Error::InvalidEndpoint(input.to_owned()));
        }
        Ok(Self { tls, host, port })
    }

    /// `host:port`, the key used to pin certificates.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Base URL, e.g. `https://10.0.0.1:443`.
    pub fn base_url(&self) -> String {
        format!(
            "{}://{}",
            if self.tls { "https" } else { "http" },
            self.authority()
        )
    }

    /// Where to go when this server answers `HOST_IS_SLAVE` with `address`
    /// (the pool coordinator). XAPI sends a bare IP address or host name; the
    /// scheme and port of this endpoint are kept. An explicit port in
    /// `address` (test servers) wins.
    pub fn redirect(&self, address: &str) -> Result<Self> {
        let address = address.trim();
        let explicit_port = if let Some(v6) = address.strip_prefix('[') {
            v6.split_once(']')
                .is_some_and(|(_, after)| after.starts_with(':'))
        } else {
            address.matches(':').count() == 1
        };
        let mut target = Self::parse(address)?;
        target.tls = self.tls;
        if !explicit_port {
            target.port = self.port;
        }
        Ok(target)
    }

    /// URL of the JSON-RPC endpoint.
    pub fn jsonrpc_url(&self) -> String {
        format!("{}/jsonrpc", self.base_url())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let default_port = if self.tls { 443 } else { 80 };
        match (self.tls, self.port == default_port) {
            (true, true) => f.write_str(&self.host),
            (true, false) => f.write_str(&self.authority()),
            (false, _) => f.write_str(&self.base_url()),
        }
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn parse_port(p: &str, input: &str) -> Result<u16> {
    p.parse()
        .map_err(|_| Error::InvalidEndpoint(input.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let e = Endpoint::parse("10.1.1.20").unwrap();
        assert_eq!((e.tls, e.host.as_str(), e.port), (true, "10.1.1.20", 443));
        assert_eq!(e.jsonrpc_url(), "https://10.1.1.20:443/jsonrpc");
        assert_eq!(e.to_string(), "10.1.1.20");

        let e = Endpoint::parse("xcp.example:8443").unwrap();
        assert_eq!((e.tls, e.port), (true, 8443));

        let e = Endpoint::parse("HTTP://127.0.0.1:8080/jsonrpc").unwrap();
        assert_eq!((e.tls, e.host.as_str(), e.port), (false, "127.0.0.1", 8080));

        let e = Endpoint::parse("[fe80::1]:444").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("fe80::1", 444));
        assert_eq!(e.authority(), "[fe80::1]:444");

        let e = Endpoint::parse("fe80::1").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("fe80::1", 443));
    }

    #[test]
    fn redirect_keeps_scheme_and_port() {
        let member = Endpoint::parse("https://10.0.0.2:8443").unwrap();
        let c = member.redirect("10.0.0.1").unwrap();
        assert_eq!((c.tls, c.host.as_str(), c.port), (true, "10.0.0.1", 8443));
        let c = member.redirect("127.0.0.1:5000").unwrap();
        assert_eq!((c.tls, c.host.as_str(), c.port), (true, "127.0.0.1", 5000));
        let c = member.redirect("fe80::1").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("fe80::1", 8443));
        let plain = Endpoint::parse("http://127.0.0.1:80").unwrap();
        assert!(!plain.redirect("10.0.0.1").unwrap().tls);
        assert!(member.redirect("").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(Endpoint::parse("").is_err());
        assert!(Endpoint::parse("https://").is_err());
        assert!(Endpoint::parse("host:notaport").is_err());
        assert!(Endpoint::parse("my host").is_err());
    }
}
