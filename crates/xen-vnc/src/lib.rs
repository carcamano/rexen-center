//! VNC (RFB) client for XAPI consoles.
//!
//! * [`tunnel`]: `CONNECT <console.location>` with the session cookie, over
//!   TLS with pinned certificates, to the host that runs the VM;
//! * [`client`]: the RFB handshake (3.3 as XAPI, up to 3.8) and a session
//!   that applies server updates to a shared [`Screen`] and sends user
//!   [`Input`];
//! * [`protocol`]: the wire format, including the QEMU Extended Key Event
//!   (scancodes, so the guest's keyboard layout applies);
//! * the decoders for Raw, CopyRect, RRE, CoRRE, Hextile and ZRLE, plus the
//!   Cursor, DesktopSize and QEMU Extended Key pseudo-encodings;
//! * [`framebuffer`]: the RGBA copy of the remote screen with its changed
//!   region, and the cursor shape;
//! * [`keymap`]: physical keys and their XT scancodes.
//!
//! It ports the original console's `VNCStream.cs`, `KeyMap.cs` and the
//! tunnel of `XSVNCScreen.cs`. The ZRLE decoder and zlib reader derive from
//! [vnc-rs](https://github.com/HsuJv/vnc-rs) 0.6.1 (MIT; see
//! `LICENSE-vnc-rs`). The crate has no UI dependency.

pub mod client;
mod decode;
pub mod error;
pub mod framebuffer;
pub mod keymap;
pub mod protocol;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tunnel;

pub use client::{Input, ServerEvent, ServerInit};
pub use error::{Result, VncError};
pub use framebuffer::{Cursor, Framebuffer, Screen, ScreenState};
pub use keymap::KeyCode;
pub use protocol::Rect;
pub use tunnel::{ConsoleLocation, ConsoleStream};
