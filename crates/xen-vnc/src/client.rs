//! The RFB session: handshake, then server messages applied to the
//! [`Screen`] while user input is sent (`VNCStream.cs` in the original).
//!
//! XAPI consoles need no VNC authentication (the session cookie of the
//! tunnel is the authentication); the original always answered
//! `RFB 003.003`. This client answers the server's version up to 3.8 and
//! accepts the "None" security type only.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;

use crate::decode::{Decoders, Outcome};
use crate::error::{Result, VncError};
use crate::framebuffer::{MAX_DIMENSION, Screen};
use crate::protocol::{
    ClientMessage, ENCODINGS, PixelFormat, Rect, latin1_decode, latin1_encode, server_msg,
};

/// Largest server text (desktop name, clipboard) accepted.
const MAX_TEXT: u32 = 1024 * 1024;

/// What the server said in its `ServerInit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInit {
    /// The protocol version used, e.g. `(3, 8)`.
    pub version: (u8, u8),
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
    pub name: String,
}

/// User input for the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A key. With a server that acknowledged the QEMU Extended Key Event,
    /// `scancode` (XT, QEMU form; see [`crate::keymap`]) is sent and the
    /// guest's layout decides the character; otherwise `keysym` is sent
    /// (and nothing when it is 0).
    Key {
        down: bool,
        scancode: u32,
        keysym: u32,
    },
    /// Mouse position in framebuffer pixels (clamped to the screen) and the
    /// buttons held (bit 0 left, 1 middle, 2 right, 3 wheel up, 4 wheel down).
    Pointer { buttons: u8, x: u16, y: u16 },
    /// Clipboard text for the guest (sent as Latin-1).
    CutText(String),
    /// Ask for the whole screen again.
    Refresh,
}

/// What the server did, for whoever shows the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// The session started (the screen has its size).
    Connected(ServerInit),
    /// Pixels or the cursor changed: redraw.
    Updated,
    /// The framebuffer changed size.
    Resized {
        width: u16,
        height: u16,
    },
    Bell,
    /// Clipboard text from the guest.
    CutText(String),
}

/// Performs the RFB handshake up to `ServerInit`.
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<ServerInit> {
    let mut version = [0u8; 12];
    stream.read_exact(&mut version).await?;
    let (major, minor) = parse_version(&version)?;
    // 3.3 and 3.7 as given; anything newer (3.8, 3.889, 4.x) as 3.8;
    // older or odd 3.x (e.g. 3.5) as 3.3.
    let minor = match (major, minor) {
        (3, 7) => 7,
        (3, m) if m >= 8 => 8,
        (m, _) if m > 3 => 8,
        _ => 3,
    };
    stream
        .write_all(format!("RFB 003.{minor:03}\n").as_bytes())
        .await?;
    stream.flush().await?;

    if minor == 3 {
        match stream.read_u32().await? {
            0 => return Err(VncError::Rejected(read_text(stream).await?)),
            1 => {}
            2 => {
                return Err(VncError::protocol(
                    "the server asks for a VNC password, which XAPI consoles never do",
                ));
            }
            other => return Err(VncError::protocol(format!("security type {other}"))),
        }
    } else {
        let count = stream.read_u8().await?;
        if count == 0 {
            return Err(VncError::Rejected(read_text(stream).await?));
        }
        let mut types = vec![0u8; usize::from(count)];
        stream.read_exact(&mut types).await?;
        if !types.contains(&1) {
            return Err(VncError::protocol(format!(
                "no supported security type (the server offers {types:?})"
            )));
        }
        stream.write_all(&[1]).await?;
        stream.flush().await?;
        if minor == 8 {
            let result = stream.read_u32().await?;
            if result != 0 {
                return Err(VncError::Rejected(read_text(stream).await?));
            }
        }
    }

    // ClientInit: shared, so other viewers stay connected (as the original).
    stream.write_all(&[1]).await?;
    stream.flush().await?;

    let width = stream.read_u16().await?;
    let height = stream.read_u16().await?;
    let mut format = [0u8; 16];
    stream.read_exact(&mut format).await?;
    let name = read_text(stream).await?;
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(VncError::protocol(format!(
            "framebuffer {width}x{height} is too large"
        )));
    }
    Ok(ServerInit {
        version: (3, minor),
        width,
        height,
        format: PixelFormat::from_bytes(&format),
        name,
    })
}

fn parse_version(bytes: &[u8; 12]) -> Result<(u32, u32)> {
    let bad = || {
        VncError::protocol(format!(
            "unexpected protocol version {:?}",
            String::from_utf8_lossy(bytes)
        ))
    };
    let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
    let rest = text.strip_prefix("RFB ").ok_or_else(bad)?;
    let rest = rest.strip_suffix('\n').ok_or_else(bad)?;
    let (major, minor) = rest.split_once('.').ok_or_else(bad)?;
    let major: u32 = major.parse().map_err(|_| bad())?;
    let minor: u32 = minor.parse().map_err(|_| bad())?;
    if major < 3 {
        return Err(bad());
    }
    Ok((major, minor))
}

async fn read_text<R: AsyncRead + Unpin>(r: &mut R) -> Result<String> {
    let len = r.read_u32().await?;
    if len > MAX_TEXT {
        return Err(VncError::protocol(format!("{len}-byte text")));
    }
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes).await?;
    Ok(latin1_decode(&bytes))
}

/// Runs one RFB session over `stream` until the server goes away (an error)
/// or `inputs` is closed (`Ok`).
///
/// The screen is reset from the server's `ServerInit`; then the client
/// asks for [`PixelFormat::BGRX`] and [`ENCODINGS`], requests a full
/// update, and asks for an incremental update after each one (one request
/// outstanding, as the original). Inputs queued before the call are sent
/// once the session is up.
pub async fn run<S>(
    mut stream: S,
    screen: &Arc<Screen>,
    inputs: &mut mpsc::UnboundedReceiver<Input>,
    on_event: &(dyn Fn(ServerEvent) + Send + Sync),
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let init = handshake(&mut stream).await?;
    {
        let mut state = screen.lock();
        state.framebuffer.resize(init.width, init.height);
        state.desktop_name = init.name.clone();
        state.scancodes = false;
        state.cursor = None;
        state.cursor_generation += 1;
    }
    on_event(ServerEvent::Connected(init.clone()));

    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::with_capacity(64 * 1024, read_half);
    let mut writer = BufWriter::new(write_half);

    let mut out = Vec::new();
    ClientMessage::SetPixelFormat(PixelFormat::BGRX).encode(&mut out);
    ClientMessage::SetEncodings(ENCODINGS.to_vec()).encode(&mut out);
    full_request(screen, false).encode(&mut out);
    writer.write_all(&out).await?;
    writer.flush().await?;

    // The reader asks the writer for the next update through this channel.
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<ClientMessage>();

    let read_loop = async {
        let mut decoders = Decoders::new();
        loop {
            match reader.read_u8().await? {
                server_msg::FRAMEBUFFER_UPDATE => {
                    let mut pad = [0u8; 1];
                    reader.read_exact(&mut pad).await?;
                    let count = reader.read_u16().await?;
                    let mut resized = false;
                    for _ in 0..count {
                        let x = reader.read_u16().await?;
                        let y = reader.read_u16().await?;
                        let width = reader.read_u16().await?;
                        let height = reader.read_u16().await?;
                        let encoding = reader.read_i32().await?;
                        let rect = Rect::new(x, y, width, height);
                        if decoders.decode(encoding, rect, &mut reader, screen).await?
                            == Outcome::Resized
                        {
                            resized = true;
                            on_event(ServerEvent::Resized { width, height });
                        }
                    }
                    on_event(ServerEvent::Updated);
                    // After a resize, ask for the whole new screen.
                    let _ = requests_tx.send(full_request(screen, !resized));
                }
                server_msg::SET_COLOUR_MAP_ENTRIES => {
                    // Only sent for colour-mapped formats, which this client
                    // never asks for; skip the entries.
                    let mut head = [0u8; 3];
                    reader.read_exact(&mut head).await?;
                    let count = reader.read_u16().await?;
                    let mut skip = vec![0u8; usize::from(count) * 6];
                    reader.read_exact(&mut skip).await?;
                }
                server_msg::BELL => on_event(ServerEvent::Bell),
                server_msg::SERVER_CUT_TEXT => {
                    let mut pad = [0u8; 3];
                    reader.read_exact(&mut pad).await?;
                    let text = read_text(&mut reader).await?;
                    on_event(ServerEvent::CutText(text));
                }
                other => {
                    return Err::<(), _>(VncError::protocol(format!(
                        "unknown server message {other}"
                    )));
                }
            }
        }
    };

    let write_loop = async {
        loop {
            let message = tokio::select! {
                biased;
                request = requests.recv() => match request {
                    Some(m) => m,
                    None => return Ok(()),
                },
                input = inputs.recv() => match input {
                    Some(input) => match translate(input, screen) {
                        Some(m) => m,
                        None => continue,
                    },
                    // The console was closed.
                    None => return Ok(()),
                },
            };
            out.clear();
            message.encode(&mut out);
            // Send whatever else is already queued in the same write.
            while let Ok(more) = requests.try_recv() {
                more.encode(&mut out);
            }
            writer.write_all(&out).await?;
            writer.flush().await?;
        }
    };

    tokio::select! {
        r = read_loop => r,
        w = write_loop => w,
    }
}

/// An update request for the whole screen.
fn full_request(screen: &Screen, incremental: bool) -> ClientMessage {
    let state = screen.lock();
    ClientMessage::UpdateRequest {
        incremental,
        rect: Rect::new(0, 0, state.framebuffer.width(), state.framebuffer.height()),
    }
}

/// The message for one input, given what the server supports.
fn translate(input: Input, screen: &Screen) -> Option<ClientMessage> {
    match input {
        Input::Key {
            down,
            scancode,
            keysym,
        } => {
            if screen.lock().scancodes && scancode != 0 {
                Some(ClientMessage::QemuKey {
                    down,
                    keysym,
                    scancode,
                })
            } else if keysym != 0 {
                Some(ClientMessage::Key { down, keysym })
            } else {
                None
            }
        }
        Input::Pointer { buttons, x, y } => {
            let state = screen.lock();
            let max_x = state.framebuffer.width().saturating_sub(1);
            let max_y = state.framebuffer.height().saturating_sub(1);
            Some(ClientMessage::Pointer {
                buttons,
                x: x.min(max_x),
                y: y.min(max_y),
            })
        }
        Input::CutText(text) => Some(ClientMessage::CutText(latin1_encode(&text))),
        Input::Refresh => Some(full_request(screen, false)),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    async fn server_side(
        mut s: tokio::io::DuplexStream,
        version: &'static [u8],
        security: Vec<u8>,
    ) -> (tokio::io::DuplexStream, Vec<u8>) {
        s.write_all(version).await.unwrap();
        let mut reply = [0u8; 12];
        s.read_exact(&mut reply).await.unwrap();
        s.write_all(&security).await.unwrap();
        (s, reply.to_vec())
    }

    fn server_init(w: u16, h: u16, name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&PixelFormat::BGRX.to_bytes());
        b.extend_from_slice(&(name.len() as u32).to_be_bytes());
        b.extend_from_slice(name.as_bytes());
        b
    }

    #[tokio::test]
    async fn handshake_3_3_as_xapi_and_the_original() {
        let (mut client, server) = duplex(4096);
        let srv = tokio::spawn(async move {
            let (mut s, reply) = server_side(server, b"RFB 003.003\n", vec![0, 0, 0, 1]).await;
            let mut shared = [0u8; 1];
            s.read_exact(&mut shared).await.unwrap();
            s.write_all(&server_init(800, 600, "QEMU (vm)"))
                .await
                .unwrap();
            (reply, shared[0], s)
        });
        let init = handshake(&mut client).await.unwrap();
        let (reply, shared, _s) = srv.await.unwrap();
        assert_eq!(reply, b"RFB 003.003\n");
        assert_eq!(shared, 1);
        assert_eq!(init.version, (3, 3));
        assert_eq!((init.width, init.height), (800, 600));
        assert_eq!(init.name, "QEMU (vm)");
    }

    #[tokio::test]
    async fn handshake_3_8_picks_none_and_reads_the_result() {
        let (mut client, server) = duplex(4096);
        let srv = tokio::spawn(async move {
            let (mut s, reply) = server_side(server, b"RFB 003.008\n", vec![2, 2, 1]).await;
            let mut chosen = [0u8; 1];
            s.read_exact(&mut chosen).await.unwrap();
            s.write_all(&[0, 0, 0, 0]).await.unwrap();
            let mut shared = [0u8; 1];
            s.read_exact(&mut shared).await.unwrap();
            s.write_all(&server_init(1024, 768, "")).await.unwrap();
            (reply, chosen[0], s)
        });
        let init = handshake(&mut client).await.unwrap();
        let (reply, chosen, _s) = srv.await.unwrap();
        assert_eq!(reply, b"RFB 003.008\n");
        assert_eq!(chosen, 1);
        assert_eq!(init.version, (3, 8));
        assert_eq!(init.width, 1024);
    }

    #[tokio::test]
    async fn handshake_failures_are_reported() {
        // 3.3 with security 0 and a reason.
        let (mut client, server) = duplex(4096);
        tokio::spawn(async move {
            let mut security = vec![0, 0, 0, 0, 0, 0, 0, 4];
            security.extend_from_slice(b"busy");
            let (s, _) = server_side(server, b"RFB 003.003\n", security).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(s);
        });
        let err = handshake(&mut client).await.unwrap_err();
        assert!(
            matches!(&err, VncError::Rejected(r) if r == "busy"),
            "{err:?}"
        );

        // 3.8 offering only VNC authentication.
        let (mut client, server) = duplex(4096);
        tokio::spawn(async move {
            let (s, _) = server_side(server, b"RFB 003.008\n", vec![1, 2]).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(s);
        });
        let err = handshake(&mut client).await.unwrap_err();
        assert!(
            err.to_string().contains("no supported security type"),
            "{err}"
        );

        // Not RFB at all.
        let (mut client, mut server) = duplex(4096);
        tokio::spawn(async move {
            server.write_all(b"HTTP/1.1 400").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });
        let err = handshake(&mut client).await.unwrap_err();
        assert!(
            err.to_string().contains("unexpected protocol version"),
            "{err}"
        );
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version(b"RFB 003.889\n").unwrap(), (3, 889));
        assert!(parse_version(b"RFB 002.000\n").is_err());
    }
}
