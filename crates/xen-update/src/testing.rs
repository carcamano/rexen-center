//! A fake GitHub release server on the local machine, for the tests (no
//! real network): it answers the "latest release" API call and serves the
//! release's files, and records the requests it got.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Product;
use crate::checksum::Sha256;

/// An answer.
#[derive(Clone)]
pub struct Route {
    pub status: u16,
    pub body: Vec<u8>,
    /// Sent in pieces of this many bytes with a pause between them (to
    /// watch the progress or cancel a download).
    pub throttle: Option<(usize, Duration)>,
}

/// A request the server got.
#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    /// Header names in lower case, with their values.
    pub headers: Vec<(String, String)>,
}

/// What the checksum file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksums {
    /// The right digest of every file.
    Correct,
    /// No checksum file in the release.
    Missing,
    /// Every digest right except this file's.
    WrongFor(String),
}

/// A release to publish.
#[derive(Debug, Clone)]
pub struct FakeRelease {
    pub version: String,
    pub notes: String,
    pub prerelease: bool,
    /// File names and contents.
    pub assets: Vec<(String, Vec<u8>)>,
    pub checksums: Checksums,
}

impl FakeRelease {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            notes: format!("## What's Changed\n* Release {version}"),
            prerelease: false,
            assets: Vec::new(),
            checksums: Checksums::Correct,
        }
    }

    pub fn asset(mut self, name: &str, body: Vec<u8>) -> Self {
        self.assets.push((name.to_owned(), body));
        self
    }
}

/// The server; stops when dropped.
pub struct FakeReleaseServer {
    url: String,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
}

impl FakeReleaseServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake release server");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (routes, requests, stop) = (routes.clone(), requests.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let (routes, requests) = (routes.clone(), requests.clone());
                            std::thread::spawn(move || serve(stream, &routes, &requests));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
        }
        Self {
            url,
            routes,
            requests,
            stop,
        }
    }

    /// `http://127.0.0.1:<port>`: the API address, and the files' host.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn route(&self, path: &str, route: Route) {
        self.routes.lock().unwrap().insert(path.to_owned(), route);
    }

    pub fn set(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.route(
            path,
            Route {
                status,
                body: body.into(),
                throttle: None,
            },
        );
    }

    /// Serves `path` slowly: `chunk` bytes every `pause`.
    pub fn throttle(&self, path: &str, chunk: usize, pause: Duration) {
        if let Some(route) = self.routes.lock().unwrap().get_mut(path) {
            route.throttle = Some((chunk, pause));
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The path of a release file.
    pub fn asset_path(version: &str, name: &str) -> String {
        format!("/download/v{version}/{name}")
    }

    /// The API path of the latest release.
    pub fn latest_path(product: &Product) -> String {
        format!("/repos/{}/releases/latest", product.repository)
    }

    /// Publishes `release` as `product`'s latest.
    pub fn publish(&self, product: &Product, release: &FakeRelease) {
        let v = &release.version;
        let mut assets = Vec::new();
        let mut add = |name: &str, body: Vec<u8>| {
            let path = Self::asset_path(v, name);
            assets.push(serde_json::json!({
                "name": name,
                "size": body.len(),
                "browser_download_url": format!("{}{path}", self.url),
            }));
            self.set(&path, 200, body);
        };
        for (name, body) in &release.assets {
            add(name, body.clone());
        }
        if release.checksums != Checksums::Missing {
            let sums: String = release
                .assets
                .iter()
                .map(|(name, body)| {
                    let digest = match &release.checksums {
                        Checksums::WrongFor(bad) if bad == name => Sha256::of_bytes(b"tampered"),
                        _ => Sha256::of_bytes(body),
                    };
                    format!("{digest}  {name}\n")
                })
                .collect();
            add(crate::release::CHECKSUMS_ASSET, sums.into_bytes());
        }
        let json = serde_json::json!({
            "tag_name": format!("v{v}"),
            "name": format!("{} {v}", product.bundle_name),
            "body": release.notes,
            "html_url": format!("{}/releases/tag/v{v}", self.url),
            "draft": false,
            "prerelease": release.prerelease,
            "assets": assets,
        });
        self.set(&Self::latest_path(product), 200, json.to_string());
    }
}

impl Drop for FakeReleaseServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(
    stream: TcpStream,
    routes: &Mutex<HashMap<String, Route>>,
    requests: &Mutex<Vec<Request>>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    requests.lock().unwrap().push(Request {
        path: path.clone(),
        headers,
    });
    let route = routes.lock().unwrap().get(&path).cloned().unwrap_or(Route {
        status: 404,
        body: b"{\"message\":\"Not Found\"}".to_vec(),
        throttle: None,
    });
    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        route.status,
        route.body.len()
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    match route.throttle {
        None => {
            let _ = stream.write_all(&route.body);
        }
        Some((chunk, pause)) => {
            for piece in route.body.chunks(chunk.max(1)) {
                if stream
                    .write_all(piece)
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(pause);
            }
        }
    }
    let _ = stream.flush();
}

/// The release tarball of `version`, as `packaging/linux/build.sh` lays it
/// out (`<app_id>-<version>/bin/<binary>` and the desktop entry).
pub fn release_tarball(product: &Product, version: &str) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        let top = format!("{}-{version}", product.app_id);
        let files = [
            (
                format!("{top}/bin/{}", product.binary),
                format!("binary {version}"),
                0o755,
            ),
            (
                format!("{top}/share/applications/{}.desktop", product.app_id),
                "[Desktop Entry]".to_owned(),
                0o644,
            ),
        ];
        for (name, body, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(body.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, name, body.as_bytes())
                .expect("tar entry");
        }
        tar.into_inner().expect("tar").finish().expect("gzip");
    }
    out
}

/// A tarball installation of `version` in `dir`; returns its method.
pub fn install_tarball(
    product: &Product,
    dir: &std::path::Path,
    version: &str,
) -> crate::InstallMethod {
    let root = dir.join(format!("{}-{version}", product.app_id));
    let bin = root.join("bin");
    let apps = root.join("share/applications");
    std::fs::create_dir_all(&bin).expect("bin folder");
    std::fs::create_dir_all(&apps).expect("share folder");
    std::fs::write(bin.join(&product.binary), format!("binary {version}")).expect("binary");
    std::fs::write(
        apps.join(format!("{}.desktop", product.app_id)),
        "[Desktop Entry]",
    )
    .expect("desktop entry");
    crate::InstallMethod::LinuxTarball { root }
}
