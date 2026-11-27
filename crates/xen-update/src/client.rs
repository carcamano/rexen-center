//! The release server: GitHub's REST API for the latest release, and the
//! downloads of its files.
//!
//! The client sends nothing about the user or the machine: an anonymous
//! `GET` with a fixed `User-Agent` (GitHub refuses requests without one) and
//! no cookies, tokens or identifiers. It uses the system's proxy settings
//! (`HTTPS_PROXY` / `NO_PROXY`, and on macOS and Windows the system's) and
//! the system's certificate authorities (through reqwest's rustls stack and
//! its platform verifier). Only `https://` is followed, except to the local
//! machine, which the tests' fake server uses.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::Product;
use crate::release::{Asset, Release};

/// GitHub's REST API.
pub const DEFAULT_API: &str = "https://api.github.com";

/// Overrides the API's address (testing against a fake release server; a
/// plain `http://` address is accepted only for the local machine).
pub const API_ENV: &str = "REXENCENTER_UPDATE_API";

/// The `User-Agent`: the program's name, no version or anything else.
pub const USER_AGENT: &str = "rexen-center-updater";

/// The largest checksum file read (it lists a handful of files).
const MAX_CHECKSUMS: u64 = 256 * 1024;

/// The largest release answer read.
const MAX_RELEASE: u64 = 4 * 1024 * 1024;

/// The largest package downloaded.
pub const MAX_PACKAGE: u64 = 1024 * 1024 * 1024;

/// What went wrong while checking or downloading.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the update address {0} is not allowed (https only)")]
    Insecure(String),
    #[error("cannot reach the release server: {0}")]
    Network(String),
    #[error("the release server answered {0}")]
    Status(u16),
    #[error("no release has been published yet")]
    NoRelease,
    #[error("the release server's answer was not understood: {0}")]
    Malformed(String),
    #[error("{0} is larger than expected")]
    TooLarge(String),
    #[error("the download was cancelled")]
    Cancelled,
    #[error("cannot save the download: {0}")]
    Io(String),
}

/// Talks to the release server.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    api: String,
    repository: String,
}

impl Client {
    /// A client for `product`'s releases at `api` ([`DEFAULT_API`], or the
    /// address in [`API_ENV`]).
    pub fn new(product: &Product, api: &str) -> Result<Self, ClientError> {
        let api = api.trim_end_matches('/').to_owned();
        if !allowed_url(&api) {
            return Err(ClientError::Insecure(api));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls_platform_verifier::Verifier::new(provider.clone())
            .map_err(|e| ClientError::Network(e.to_string()))?;
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| ClientError::Network(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if allowed_url(attempt.url().as_str()) {
                    attempt.follow()
                } else {
                    attempt.error("redirected to an address that is not https")
                }
            }))
            .build()
            .map_err(|e| ClientError::Network(e.to_string()))?;
        Ok(Self {
            http,
            api,
            repository: product.repository.clone(),
        })
    }

    /// The API address from [`API_ENV`], or GitHub's.
    pub fn api_from_env() -> String {
        std::env::var(API_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_API.to_owned())
    }

    /// The latest stable release (GitHub's "latest" is the newest release
    /// that is neither a draft nor a pre-release).
    pub async fn latest_release(&self) -> Result<Release, ClientError> {
        let url = format!("{}/repos/{}/releases/latest", self.api, self.repository);
        let response = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(network)?;
        match response.status().as_u16() {
            200 => {}
            404 => return Err(ClientError::NoRelease),
            code => return Err(ClientError::Status(code)),
        }
        let body = read_limited(response, MAX_RELEASE, "the release answer").await?;
        serde_json::from_slice(&body).map_err(|e| ClientError::Malformed(e.to_string()))
    }

    /// The checksum file's text.
    pub async fn checksums(&self, asset: &Asset) -> Result<String, ClientError> {
        let response = self.get(&asset.browser_download_url).await?;
        let body = read_limited(response, MAX_CHECKSUMS, &asset.name).await?;
        String::from_utf8(body).map_err(|e| ClientError::Malformed(e.to_string()))
    }

    /// Downloads `asset` to `dest` (created new), calling `progress` with
    /// the bytes so far and the total, and stopping when `cancel` is set.
    /// A download that stops for any reason leaves no file behind.
    pub async fn download(
        &self,
        asset: &Asset,
        dest: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<u64, ClientError> {
        let result = self.download_inner(asset, dest, progress, cancel).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(dest).await;
        }
        result
    }

    async fn download_inner(
        &self,
        asset: &Asset,
        dest: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<u64, ClientError> {
        let mut response = self.get(&asset.browser_download_url).await?;
        let total = response
            .content_length()
            .or((asset.size > 0).then_some(asset.size));
        if total.is_some_and(|t| t > MAX_PACKAGE) {
            return Err(ClientError::TooLarge(asset.name.clone()));
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        let mut done: u64 = 0;
        progress(0, total);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(ClientError::Cancelled);
            }
            let chunk = tokio::time::timeout(Duration::from_secs(60), response.chunk())
                .await
                .map_err(|_| ClientError::Network("the download stalled".to_owned()))?
                .map_err(network)?;
            let Some(chunk) = chunk else { break };
            done += chunk.len() as u64;
            if done > MAX_PACKAGE || total.is_some_and(|t| done > t) {
                return Err(ClientError::TooLarge(asset.name.clone()));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| ClientError::Io(e.to_string()))?;
            progress(done, total);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(ClientError::Cancelled);
        }
        if total.is_some_and(|t| done != t) {
            return Err(ClientError::Network(format!(
                "the download of {} ended early",
                asset.name
            )));
        }
        file.flush()
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        file.sync_all()
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        Ok(done)
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response, ClientError> {
        if !allowed_url(url) {
            return Err(ClientError::Insecure(url.to_owned()));
        }
        let response = self.http.get(url).send().await.map_err(network)?;
        if !response.status().is_success() {
            return Err(ClientError::Status(response.status().as_u16()));
        }
        Ok(response)
    }
}

fn network(e: reqwest::Error) -> ClientError {
    ClientError::Network(e.to_string())
}

async fn read_limited(
    mut response: reqwest::Response,
    limit: u64,
    what: &str,
) -> Result<Vec<u8>, ClientError> {
    if response.content_length().is_some_and(|n| n > limit) {
        return Err(ClientError::TooLarge(what.to_owned()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(ClientError::TooLarge(what.to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// `https://…`, or `http://` to the local machine.
pub fn allowed_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // No credentials in a local address either.
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_the_local_machine() {
        for ok in [
            "https://api.github.com",
            "https://github.com/carcamano/rexen-center/releases/download/v0.2.0/SHA256SUMS",
            "HTTPS://objects.githubusercontent.com/x",
            "http://127.0.0.1:8080",
            "http://localhost/x",
            "http://[::1]:9000/repos",
        ] {
            assert!(allowed_url(ok), "{ok}");
        }
        for bad in [
            "http://api.github.com",
            "http://127.0.0.1.evil.example/x",
            "http://localhost@evil.example/",
            "http://evil.example/?h=127.0.0.1",
            "ftp://127.0.0.1/x",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
        assert!(matches!(
            Client::new(&Product::rexen_center(), "http://example.com"),
            Err(ClientError::Insecure(_))
        ));
    }
}
