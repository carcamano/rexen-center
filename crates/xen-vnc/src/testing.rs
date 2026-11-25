//! A fake VNC server standing in for a guest console, for tests (feature
//! `testing`).
//!
//! It speaks RFB 3.3 (as XAPI's consoles) or 3.8, keeps a screen the test
//! can paint, answers update requests with ZRLE, Hextile or Raw (the first
//! the client asks for), acknowledges the QEMU Extended Key Event, sends a
//! cursor shape, and records every client message so tests can check the
//! keys, pointer events and clipboard text that arrived.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use crate::protocol::{ClientMessage, PixelFormat, Rect, client_msg, encoding, server_msg};

/// Something the server sends outside update replies or with the next one.
#[derive(Debug, Clone)]
enum Pending {
    Bell,
    CutText(String),
}

#[derive(Debug)]
struct State {
    width: u16,
    height: u16,
    /// `[b, g, r, 0]` per pixel.
    pixels: Vec<[u8; 4]>,
    /// Region changed since the last update, per session generation.
    dirty: Option<Rect>,
    resized: bool,
    rfb_3_8: bool,
    ext_key: bool,
    send_cursor: bool,
    forced_encoding: Option<i32>,
    received: Vec<ClientMessage>,
    sessions: usize,
    active: usize,
    pending: VecDeque<Pending>,
}

/// The fake console. Cheap to clone; clones share the screen and the log.
#[derive(Clone)]
pub struct MockConsole {
    state: Arc<Mutex<State>>,
    /// Bumped on every change, so sessions wake up.
    changed: Arc<watch::Sender<u64>>,
    /// Bumped to drop every session (a guest reboot, a dead QEMU).
    kill: Arc<watch::Sender<u64>>,
}

impl std::fmt::Debug for MockConsole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockConsole").finish_non_exhaustive()
    }
}

impl MockConsole {
    /// A `width` × `height` console with a test pattern: a dark blue
    /// background and a white 8×8 square in the top-left corner.
    pub fn new(width: u16, height: u16) -> Self {
        let console = Self {
            state: Arc::new(Mutex::new(State {
                width,
                height,
                pixels: vec![[96, 32, 0, 0]; usize::from(width) * usize::from(height)],
                dirty: None,
                resized: false,
                rfb_3_8: false,
                ext_key: true,
                send_cursor: true,
                forced_encoding: None,
                received: Vec::new(),
                sessions: 0,
                active: 0,
                pending: VecDeque::new(),
            })),
            changed: Arc::new(watch::channel(0).0),
            kill: Arc::new(watch::channel(0).0),
        };
        console.fill(Rect::new(0, 0, 8, 8), [255, 255, 255]);
        console
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Answer `RFB 003.008` (with the 3.8 security exchange) instead of 3.3.
    pub fn set_rfb_3_8(&self, yes: bool) {
        self.lock().rfb_3_8 = yes;
    }

    /// Whether the QEMU Extended Key Event is acknowledged (default yes; a
    /// PV guest's text console does not).
    pub fn set_ext_key(&self, yes: bool) {
        self.lock().ext_key = yes;
    }

    /// Use this encoding for updates instead of the client's preference.
    pub fn force_encoding(&self, encoding: Option<i32>) {
        self.lock().forced_encoding = encoding;
    }

    /// Paints `rect` with `rgb`.
    pub fn fill(&self, rect: Rect, rgb: [u8; 3]) {
        {
            let mut s = self.lock();
            let (w, h) = (s.width, s.height);
            for y in rect.y..(rect.y + rect.height).min(h) {
                for x in rect.x..(rect.x + rect.width).min(w) {
                    s.pixels[usize::from(y) * usize::from(w) + usize::from(x)] =
                        [rgb[2], rgb[1], rgb[0], 0];
                }
            }
            s.dirty = Some(match s.dirty {
                Some(d) => d.union(&rect),
                None => rect,
            });
        }
        self.bump();
    }

    /// The colour at (`x`, `y`) as `[r, g, b]`.
    pub fn pixel(&self, x: u16, y: u16) -> [u8; 3] {
        let s = self.lock();
        let p = s.pixels[usize::from(y) * usize::from(s.width) + usize::from(x)];
        [p[2], p[1], p[0]]
    }

    /// The screen size.
    pub fn size(&self) -> (u16, u16) {
        let s = self.lock();
        (s.width, s.height)
    }

    /// Changes the screen size (a guest mode switch): DesktopSize goes out
    /// with the next update.
    pub fn resize(&self, width: u16, height: u16) {
        {
            let mut s = self.lock();
            s.width = width;
            s.height = height;
            s.pixels = vec![[0, 64, 0, 0]; usize::from(width) * usize::from(height)];
            s.resized = true;
            s.dirty = Some(Rect::new(0, 0, width, height));
        }
        self.bump();
    }

    /// Rings the bell.
    pub fn bell(&self) {
        self.lock().pending.push_back(Pending::Bell);
        self.bump();
    }

    /// Puts `text` on the guest's clipboard (ServerCutText).
    pub fn cut_text(&self, text: &str) {
        self.lock()
            .pending
            .push_back(Pending::CutText(text.to_owned()));
        self.bump();
    }

    /// Drops every session, as when the guest reboots.
    pub fn disconnect_all(&self) {
        self.kill.send_modify(|v| *v += 1);
    }

    /// Every message received from clients, oldest first.
    pub fn received(&self) -> Vec<ClientMessage> {
        self.lock().received.clone()
    }

    /// Forgets the received messages.
    pub fn clear_received(&self) {
        self.lock().received.clear();
    }

    /// The scancode key events received: `(down, scancode)`.
    pub fn scancodes(&self) -> Vec<(bool, u32)> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::QemuKey { down, scancode, .. } => Some((down, scancode)),
                _ => None,
            })
            .collect()
    }

    /// The keysym key events received: `(down, keysym)`.
    pub fn keysyms(&self) -> Vec<(bool, u32)> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::Key { down, keysym } => Some((down, keysym)),
                _ => None,
            })
            .collect()
    }

    /// The pointer events received: `(buttons, x, y)`.
    pub fn pointer_events(&self) -> Vec<(u8, u16, u16)> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::Pointer { buttons, x, y } => Some((buttons, x, y)),
                _ => None,
            })
            .collect()
    }

    /// The clipboard texts received (Latin-1 bytes decoded).
    pub fn cut_texts(&self) -> Vec<String> {
        self.received()
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::CutText(t) => Some(crate::protocol::latin1_decode(&t)),
                _ => None,
            })
            .collect()
    }

    /// Sessions started so far.
    pub fn sessions(&self) -> usize {
        self.lock().sessions
    }

    /// Sessions open now.
    pub fn active_sessions(&self) -> usize {
        self.lock().active
    }

    /// Accepts plain TCP clients on a localhost port (no tunnel).
    pub async fn listen(&self) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let console = self.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let console = console.clone();
                tokio::spawn(async move {
                    let _ = console.serve(stream).await;
                });
            }
        });
        addr
    }

    /// Serves one client until it leaves or [`MockConsole::disconnect_all`].
    pub async fn serve<S>(&self, stream: S) -> std::io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        {
            let mut s = self.lock();
            s.sessions += 1;
            s.active += 1;
        }
        let result = self.session(stream).await;
        self.lock().active -= 1;
        result
    }

    async fn session<S>(&self, stream: S) -> std::io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut kill = self.kill.subscribe();
        let mut changed = self.changed.subscribe();
        let (mut rd, mut wr) = tokio::io::split(stream);
        let rfb_3_8 = self.lock().rfb_3_8;

        wr.write_all(if rfb_3_8 {
            b"RFB 003.008\n"
        } else {
            b"RFB 003.003\n"
        })
        .await?;
        let mut version = [0u8; 12];
        rd.read_exact(&mut version).await?;
        if rfb_3_8 {
            wr.write_all(&[1, 1]).await?;
            let mut chosen = [0u8; 1];
            rd.read_exact(&mut chosen).await?;
            wr.write_all(&0u32.to_be_bytes()).await?;
        } else {
            wr.write_all(&1u32.to_be_bytes()).await?;
        }
        let mut shared = [0u8; 1];
        rd.read_exact(&mut shared).await?;
        let (w, h) = self.size();
        let mut init = Vec::new();
        init.extend_from_slice(&w.to_be_bytes());
        init.extend_from_slice(&h.to_be_bytes());
        init.extend_from_slice(&PixelFormat::BGRX.to_bytes());
        init.extend_from_slice(&(10u32).to_be_bytes());
        init.extend_from_slice(b"mock guest");
        wr.write_all(&init).await?;
        wr.flush().await?;

        let (tx, mut messages) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Ok(m) = read_client_message(&mut rd).await {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });

        let mut client_encodings: Vec<i32> = Vec::new();
        let mut request: Option<bool> = None;
        let mut announce_ext_key = false;
        let mut announce_cursor = false;
        let mut zlib = flate2::Compress::new(flate2::Compression::default(), true);
        let mut known_size = (w, h);
        let result = loop {
            tokio::select! {
                m = messages.recv() => match m {
                    None => break Ok(()),
                    Some(m) => {
                        match &m {
                            ClientMessage::SetEncodings(list) => {
                                client_encodings = list.clone();
                                let s = self.lock();
                                announce_ext_key = s.ext_key && list.contains(&encoding::QEMU_EXT_KEY);
                                announce_cursor = s.send_cursor && list.contains(&encoding::CURSOR);
                            }
                            ClientMessage::UpdateRequest { incremental, .. } => {
                                // A full request wins over an incremental one.
                                request = Some(request.unwrap_or(true) && *incremental);
                            }
                            _ => {}
                        }
                        self.lock().received.push(m);
                    }
                },
                _ = changed.changed() => {}
                _ = kill.changed() => break Ok(()),
            }

            // Out-of-band messages.
            let pending: Vec<Pending> = self.lock().pending.drain(..).collect();
            let mut out = Vec::new();
            for p in pending {
                match p {
                    Pending::Bell => out.push(server_msg::BELL),
                    Pending::CutText(t) => {
                        let bytes = crate::protocol::latin1_encode(&t);
                        out.extend_from_slice(&[server_msg::SERVER_CUT_TEXT, 0, 0, 0]);
                        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                        out.extend_from_slice(&bytes);
                    }
                }
            }

            if let Some(incremental) = request {
                let mut s = self.lock();
                let resized = s.resized || (s.width, s.height) != known_size;
                let region = if !incremental || resized {
                    Some(Rect::new(0, 0, s.width, s.height))
                } else {
                    s.dirty
                };
                if region.is_some() || announce_ext_key || announce_cursor {
                    let mut rects: Vec<Vec<u8>> = Vec::new();
                    if announce_ext_key {
                        rects.push(rect_header(Rect::default(), encoding::QEMU_EXT_KEY));
                        announce_ext_key = false;
                    }
                    if announce_cursor {
                        rects.push(cursor_rect());
                        announce_cursor = false;
                    }
                    if resized {
                        rects.push(rect_header(
                            Rect::new(0, 0, s.width, s.height),
                            encoding::DESKTOP_SIZE,
                        ));
                        known_size = (s.width, s.height);
                        s.resized = false;
                    }
                    if let Some(region) = region {
                        let enc = s.forced_encoding.unwrap_or_else(|| {
                            client_encodings
                                .iter()
                                .copied()
                                .find(|e| {
                                    [encoding::ZRLE, encoding::HEXTILE, encoding::RAW].contains(e)
                                })
                                .unwrap_or(encoding::RAW)
                        });
                        rects.push(encode_rect(&s, region, enc, &mut zlib));
                    }
                    s.dirty = None;
                    out.extend_from_slice(&[server_msg::FRAMEBUFFER_UPDATE, 0]);
                    out.extend_from_slice(&(rects.len() as u16).to_be_bytes());
                    for r in rects {
                        out.extend_from_slice(&r);
                    }
                    request = None;
                }
            }
            if !out.is_empty() {
                if let Err(e) = wr.write_all(&out).await {
                    break Err(e);
                }
                if let Err(e) = wr.flush().await {
                    break Err(e);
                }
            }
        };
        reader.abort();
        let _ = wr.shutdown().await;
        result
    }
}

fn rect_header(rect: Rect, enc: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    for v in [rect.x, rect.y, rect.width, rect.height] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&enc.to_be_bytes());
    b
}

/// A 4×4 cursor with its hot spot at (1, 1): a red square, the corner pixels
/// masked out.
fn cursor_rect() -> Vec<u8> {
    let mut b = rect_header(Rect::new(1, 1, 4, 4), encoding::CURSOR);
    for _ in 0..16 {
        b.extend_from_slice(&[0, 0, 255, 0]);
    }
    b.extend_from_slice(&[0b0110_0000, 0b1111_0000, 0b1111_0000, 0b0110_0000]);
    b
}

fn encode_rect(s: &State, rect: Rect, enc: i32, zlib: &mut flate2::Compress) -> Vec<u8> {
    let px = |x: u16, y: u16| s.pixels[usize::from(y) * usize::from(s.width) + usize::from(x)];
    let mut b = rect_header(rect, enc);
    match enc {
        encoding::HEXTILE => {
            for ty in (rect.y..rect.y + rect.height).step_by(16) {
                let th = (rect.y + rect.height - ty).min(16);
                for tx in (rect.x..rect.x + rect.width).step_by(16) {
                    let tw = (rect.x + rect.width - tx).min(16);
                    let first = px(tx, ty);
                    let solid = (ty..ty + th).all(|y| (tx..tx + tw).all(|x| px(x, y) == first));
                    if solid {
                        b.push(2);
                        b.extend_from_slice(&first);
                    } else {
                        b.push(1);
                        for y in ty..ty + th {
                            for x in tx..tx + tw {
                                b.extend_from_slice(&px(x, y));
                            }
                        }
                    }
                }
            }
        }
        encoding::ZRLE => {
            let mut raw = Vec::new();
            for ty in (rect.y..rect.y + rect.height).step_by(64) {
                let th = (rect.y + rect.height - ty).min(64);
                for tx in (rect.x..rect.x + rect.width).step_by(64) {
                    let tw = (rect.x + rect.width - tx).min(64);
                    let first = px(tx, ty);
                    let solid = (ty..ty + th).all(|y| (tx..tx + tw).all(|x| px(x, y) == first));
                    if solid {
                        raw.push(1);
                        raw.extend_from_slice(&first[..3]);
                    } else {
                        raw.push(0);
                        for y in ty..ty + th {
                            for x in tx..tx + tw {
                                raw.extend_from_slice(&px(x, y)[..3]);
                            }
                        }
                    }
                }
            }
            let compressed = zrle_chunk(zlib, &raw);
            b.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
            b.extend_from_slice(&compressed);
        }
        _ => {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    b.extend_from_slice(&px(x, y));
                }
            }
        }
    }
    b
}

/// Compresses one ZRLE payload with the session's zlib stream, ending it
/// with a sync flush (as QEMU does) so the stream continues in the next one.
fn zrle_chunk(zlib: &mut flate2::Compress, raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 1024);
    let mut input = raw;
    loop {
        if out.capacity() - out.len() < 1024 {
            out.reserve(64 * 1024);
        }
        let before = zlib.total_in();
        zlib.compress_vec(input, &mut out, flate2::FlushCompress::Sync)
            .expect("compress");
        input = &input[(zlib.total_in() - before) as usize..];
        if input.is_empty() && out.len() < out.capacity() {
            return out;
        }
    }
}

async fn read_client_message<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<ClientMessage> {
    let kind = r.read_u8().await?;
    Ok(match kind {
        client_msg::SET_PIXEL_FORMAT => {
            let mut pad = [0u8; 3];
            r.read_exact(&mut pad).await?;
            let mut f = [0u8; 16];
            r.read_exact(&mut f).await?;
            ClientMessage::SetPixelFormat(PixelFormat::from_bytes(&f))
        }
        client_msg::SET_ENCODINGS => {
            let _pad = r.read_u8().await?;
            let n = r.read_u16().await?;
            let mut list = Vec::with_capacity(usize::from(n));
            for _ in 0..n {
                list.push(r.read_i32().await?);
            }
            ClientMessage::SetEncodings(list)
        }
        client_msg::FRAMEBUFFER_UPDATE_REQUEST => {
            let incremental = r.read_u8().await? != 0;
            let x = r.read_u16().await?;
            let y = r.read_u16().await?;
            let w = r.read_u16().await?;
            let h = r.read_u16().await?;
            ClientMessage::UpdateRequest {
                incremental,
                rect: Rect::new(x, y, w, h),
            }
        }
        client_msg::KEY_EVENT => {
            let down = r.read_u8().await? != 0;
            let _pad = r.read_u16().await?;
            ClientMessage::Key {
                down,
                keysym: r.read_u32().await?,
            }
        }
        client_msg::POINTER_EVENT => ClientMessage::Pointer {
            buttons: r.read_u8().await?,
            x: r.read_u16().await?,
            y: r.read_u16().await?,
        },
        client_msg::CLIENT_CUT_TEXT => {
            let mut pad = [0u8; 3];
            r.read_exact(&mut pad).await?;
            let len = r.read_u32().await?;
            let mut text = vec![0u8; len as usize];
            r.read_exact(&mut text).await?;
            ClientMessage::CutText(text)
        }
        client_msg::QEMU => {
            let sub = r.read_u8().await?;
            if sub != client_msg::QEMU_EXT_KEY_EVENT {
                return Err(std::io::Error::other(format!("QEMU sub-message {sub}")));
            }
            let down = r.read_u16().await? != 0;
            let keysym = r.read_u32().await?;
            let scancode = r.read_u32().await?;
            ClientMessage::QemuKey {
                down,
                keysym,
                scancode,
            }
        }
        other => return Err(std::io::Error::other(format!("client message {other}"))),
    })
}
