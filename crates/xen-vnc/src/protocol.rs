//! RFB wire format: constants, the pixel format and the client messages
//! (RFC 6143, plus the QEMU and XenServer extensions the original console
//! used, `VNCStream.cs`).

/// Encodings and pseudo-encodings (`SetEncodings` values).
pub mod encoding {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;
    pub const CORRE: i32 = 4;
    pub const HEXTILE: i32 = 5;
    pub const ZRLE: i32 = 16;
    /// The server sends the cursor shape; the client draws it.
    pub const CURSOR: i32 = -239;
    /// The server announces framebuffer size changes (guest mode switches).
    pub const DESKTOP_SIZE: i32 = -223;
    /// QEMU Extended Key Event: key events carry XT scancodes, so the guest's
    /// keyboard layout (not the client's) turns keys into characters.
    pub const QEMU_EXT_KEY: i32 = -258;
}

/// The encodings this client asks for, preferred first.
///
/// The original console asked for CoRRE, RRE, CopyRect and Raw (it could
/// also decode Hextile) plus the Cursor, DesktopSize, XenCenter (-254, never
/// sent by the server) and QEMU Extended Key Event pseudo-encodings. This
/// client adds ZRLE (compressed, QEMU's best) and Hextile in front of them;
/// a server picks from the list, so a server without them uses the others.
pub const ENCODINGS: &[i32] = &[
    encoding::ZRLE,
    encoding::HEXTILE,
    encoding::CORRE,
    encoding::RRE,
    encoding::COPY_RECT,
    encoding::RAW,
    encoding::CURSOR,
    encoding::DESKTOP_SIZE,
    encoding::QEMU_EXT_KEY,
];

/// Client-to-server message types.
pub mod client_msg {
    pub const SET_PIXEL_FORMAT: u8 = 0;
    pub const SET_ENCODINGS: u8 = 2;
    pub const FRAMEBUFFER_UPDATE_REQUEST: u8 = 3;
    pub const KEY_EVENT: u8 = 4;
    pub const POINTER_EVENT: u8 = 5;
    pub const CLIENT_CUT_TEXT: u8 = 6;
    /// QEMU client message; sub-type 0 is the extended key event.
    pub const QEMU: u8 = 255;
    pub const QEMU_EXT_KEY_EVENT: u8 = 0;
}

/// Server-to-client message types.
pub mod server_msg {
    pub const FRAMEBUFFER_UPDATE: u8 = 0;
    pub const SET_COLOUR_MAP_ENTRIES: u8 = 1;
    pub const BELL: u8 = 2;
    pub const SERVER_CUT_TEXT: u8 = 3;
}

/// A rectangle of the framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the rectangle covers no pixel.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Exclusive right edge.
    pub fn right(&self) -> u32 {
        u32::from(self.x) + u32::from(self.width)
    }

    /// Exclusive bottom edge.
    pub fn bottom(&self) -> u32 {
        u32::from(self.y) + u32::from(self.height)
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(
            x,
            y,
            (right - u32::from(x)) as u16,
            (bottom - u32::from(y)) as u16,
        )
    }
}

/// An RFB pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// The format this client always asks for (the original's
    /// `Force32bpp`): 32 bits per pixel, depth 24, little endian, true
    /// colour with 8 bits per channel. On the wire each pixel is then the
    /// bytes `[blue, green, red, unused]`, and a ZRLE/TRLE "CPIXEL" is
    /// `[blue, green, red]`.
    pub const BGRX: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    /// The 16-byte wire form (including the 3 padding bytes).
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = u8::from(self.big_endian);
        b[3] = u8::from(self.true_colour);
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }

    /// Parses the 16-byte wire form.
    pub fn from_bytes(b: &[u8; 16]) -> Self {
        Self {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        }
    }
}

/// Bytes per pixel on the wire with [`PixelFormat::BGRX`].
pub const BYTES_PER_PIXEL: usize = 4;

/// A message from the client to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<i32>),
    UpdateRequest {
        incremental: bool,
        rect: Rect,
    },
    /// A standard key event (an X11 keysym; the layout is the client's).
    Key {
        down: bool,
        keysym: u32,
    },
    /// A QEMU extended key event: `scancode` is the XT scancode in QEMU's
    /// form (an `E0`-prefixed key has the high bit set: `E0 53` → `0xD3`).
    QemuKey {
        down: bool,
        keysym: u32,
        scancode: u32,
    },
    /// Mouse position and buttons (bit 0 left, 1 middle, 2 right, 3/4 the
    /// wheel up/down).
    Pointer {
        buttons: u8,
        x: u16,
        y: u16,
    },
    /// Clipboard text, Latin-1 encoded.
    CutText(Vec<u8>),
}

impl ClientMessage {
    /// Appends the wire form to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::SetPixelFormat(format) => {
                out.extend_from_slice(&[client_msg::SET_PIXEL_FORMAT, 0, 0, 0]);
                out.extend_from_slice(&format.to_bytes());
            }
            Self::SetEncodings(list) => {
                out.extend_from_slice(&[client_msg::SET_ENCODINGS, 0]);
                out.extend_from_slice(&(list.len() as u16).to_be_bytes());
                for e in list {
                    out.extend_from_slice(&e.to_be_bytes());
                }
            }
            Self::UpdateRequest { incremental, rect } => {
                out.push(client_msg::FRAMEBUFFER_UPDATE_REQUEST);
                out.push(u8::from(*incremental));
                for v in [rect.x, rect.y, rect.width, rect.height] {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Self::Key { down, keysym } => {
                out.extend_from_slice(&[client_msg::KEY_EVENT, u8::from(*down), 0, 0]);
                out.extend_from_slice(&keysym.to_be_bytes());
            }
            Self::QemuKey {
                down,
                keysym,
                scancode,
            } => {
                out.extend_from_slice(&[client_msg::QEMU, client_msg::QEMU_EXT_KEY_EVENT]);
                out.extend_from_slice(&u16::from(*down).to_be_bytes());
                out.extend_from_slice(&keysym.to_be_bytes());
                out.extend_from_slice(&scancode.to_be_bytes());
            }
            Self::Pointer { buttons, x, y } => {
                out.extend_from_slice(&[client_msg::POINTER_EVENT, *buttons]);
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
            }
            Self::CutText(text) => {
                out.extend_from_slice(&[client_msg::CLIENT_CUT_TEXT, 0, 0, 0]);
                out.extend_from_slice(&(text.len() as u32).to_be_bytes());
                out.extend_from_slice(text);
            }
        }
    }

    /// The wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// Encodes clipboard text for RFB, which carries Latin-1 only: other
/// characters become `?`, and line ends become `\n` (RFB's convention).
pub fn latin1_encode(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

/// Decodes RFB clipboard text (Latin-1).
pub fn latin1_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_have_the_rfb_wire_form() {
        assert_eq!(
            ClientMessage::SetPixelFormat(PixelFormat::BGRX).to_bytes(),
            [
                0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0
            ]
        );
        assert_eq!(
            ClientMessage::SetEncodings(vec![16, -258]).to_bytes(),
            [2, 0, 0, 2, 0, 0, 0, 16, 0xff, 0xff, 0xfe, 0xfe]
        );
        assert_eq!(
            ClientMessage::UpdateRequest {
                incremental: true,
                rect: Rect::new(1, 2, 640, 480)
            }
            .to_bytes(),
            [3, 1, 0, 1, 0, 2, 2, 128, 1, 224]
        );
        assert_eq!(
            ClientMessage::Key {
                down: true,
                keysym: 0xffe3
            }
            .to_bytes(),
            [4, 1, 0, 0, 0, 0, 0xff, 0xe3]
        );
        // The original's WriteQemuExtKey: 255, 0, down as u16, keysym, keycode.
        assert_eq!(
            ClientMessage::QemuKey {
                down: false,
                keysym: 0xffff,
                scancode: 0xd3
            }
            .to_bytes(),
            [255, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0xd3]
        );
        assert_eq!(
            ClientMessage::Pointer {
                buttons: 1,
                x: 300,
                y: 2
            }
            .to_bytes(),
            [5, 1, 1, 44, 0, 2]
        );
        assert_eq!(
            ClientMessage::CutText(b"hi".to_vec()).to_bytes(),
            [6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']
        );
    }

    #[test]
    fn pixel_format_round_trips() {
        let b = PixelFormat::BGRX.to_bytes();
        assert_eq!(PixelFormat::from_bytes(&b), PixelFormat::BGRX);
    }

    #[test]
    fn clipboard_text_is_latin1() {
        assert_eq!(latin1_encode("ação\r\n€"), b"a\xe7\xe3o\n?");
        assert_eq!(latin1_decode(b"a\xe7\xe3o"), "ação");
    }

    #[test]
    fn rect_union() {
        let a = Rect::new(10, 10, 5, 5);
        let b = Rect::new(0, 12, 2, 10);
        assert_eq!(a.union(&b), Rect::new(0, 10, 15, 12));
        assert_eq!(Rect::default().union(&b), b);
    }
}
