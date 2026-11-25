//! Rectangle decoders. Every decoder reads the rectangle's payload from the
//! stream first and then writes it into the [`Screen`] under a short lock
//! (never held across an `await`).
//!
//! Pixels always use [`PixelFormat::BGRX`](crate::protocol::PixelFormat::BGRX),
//! which this client sets right after the handshake.

mod hextile;
mod rre;
mod zlib;
mod zrle;

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{Result, VncError};
use crate::framebuffer::{Cursor, MAX_DIMENSION, Screen};
use crate::protocol::{BYTES_PER_PIXEL, Rect, encoding};

/// Largest rectangle (in pixels) accepted in one piece.
pub(crate) const MAX_PIXELS: usize = 8192 * 4096;

/// Largest compressed ZRLE payload accepted.
pub(crate) const MAX_COMPRESSED: usize = 64 * 1024 * 1024;

/// What a rectangle did, besides drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Pixels changed.
    Drawn,
    /// The framebuffer was resized (DesktopSize): ask for a full update.
    Resized,
    /// The cursor shape changed.
    Cursor,
    /// The server acknowledged the QEMU Extended Key Event.
    ExtKeyAck,
}

/// Decoder state kept for the whole connection (the ZRLE zlib stream).
pub(crate) struct Decoders {
    zrle: zrle::Decoder,
}

impl Decoders {
    pub(crate) fn new() -> Self {
        Self {
            zrle: zrle::Decoder::new(),
        }
    }

    /// Decodes one rectangle of a FramebufferUpdate into `screen`.
    pub(crate) async fn decode<R: AsyncRead + Unpin>(
        &mut self,
        enc: i32,
        rect: Rect,
        input: &mut R,
        screen: &Arc<Screen>,
    ) -> Result<Outcome> {
        match enc {
            encoding::RAW => {
                let data = read_pixels(input, area(rect)?).await?;
                screen.lock().framebuffer.put_bgrx(rect, &data);
                Ok(Outcome::Drawn)
            }
            encoding::COPY_RECT => {
                let src_x = input.read_u16().await?;
                let src_y = input.read_u16().await?;
                screen.lock().framebuffer.copy_rect(rect, src_x, src_y);
                Ok(Outcome::Drawn)
            }
            encoding::RRE => rre::decode(input, rect, screen, false).await,
            encoding::CORRE => rre::decode(input, rect, screen, true).await,
            encoding::HEXTILE => hextile::decode(input, rect, screen).await,
            encoding::ZRLE => self.zrle.decode(input, rect, screen).await,
            encoding::CURSOR => {
                let cursor = read_cursor(input, rect).await?;
                let mut state = screen.lock();
                state.cursor = cursor.map(Arc::new);
                state.cursor_generation += 1;
                Ok(Outcome::Cursor)
            }
            encoding::DESKTOP_SIZE => {
                if rect.width > MAX_DIMENSION || rect.height > MAX_DIMENSION {
                    return Err(VncError::protocol(format!(
                        "desktop size {}x{} is too large",
                        rect.width, rect.height
                    )));
                }
                screen.lock().framebuffer.resize(rect.width, rect.height);
                Ok(Outcome::Resized)
            }
            encoding::QEMU_EXT_KEY => {
                screen.lock().scancodes = true;
                Ok(Outcome::ExtKeyAck)
            }
            other => Err(VncError::protocol(format!(
                "unimplemented encoding {other}"
            ))),
        }
    }
}

/// Pixel count of `rect`, refusing absurd sizes.
pub(crate) fn area(rect: Rect) -> Result<usize> {
    let n = usize::from(rect.width) * usize::from(rect.height);
    if n > MAX_PIXELS {
        return Err(VncError::protocol(format!(
            "rectangle {}x{} is too large",
            rect.width, rect.height
        )));
    }
    Ok(n)
}

pub(crate) async fn read_pixel<R: AsyncRead + Unpin>(input: &mut R) -> Result<[u8; 4]> {
    let mut p = [0u8; 4];
    input.read_exact(&mut p).await?;
    Ok(p)
}

pub(crate) async fn read_pixels<R: AsyncRead + Unpin>(
    input: &mut R,
    count: usize,
) -> Result<Vec<u8>> {
    let mut data = vec![0u8; count * BYTES_PER_PIXEL];
    input.read_exact(&mut data).await?;
    Ok(data)
}

/// Cursor pseudo-encoding: the shape's pixels, then a 1-bit mask (rows
/// padded to whole bytes, most significant bit first). The hot spot is the
/// rectangle's position. An empty shape hides the cursor.
async fn read_cursor<R: AsyncRead + Unpin>(input: &mut R, rect: Rect) -> Result<Option<Cursor>> {
    let pixels = read_pixels(input, area(rect)?).await?;
    let row_bytes = usize::from(rect.width).div_ceil(8);
    let mut mask = vec![0u8; row_bytes * usize::from(rect.height)];
    input.read_exact(&mut mask).await?;
    if rect.is_empty() {
        return Ok(None);
    }
    let mut rgba = Vec::with_capacity(pixels.len());
    for y in 0..usize::from(rect.height) {
        for x in 0..usize::from(rect.width) {
            let p = &pixels[(y * usize::from(rect.width) + x) * 4..][..4];
            let visible = mask[y * row_bytes + x / 8] & (0x80 >> (x % 8)) != 0;
            rgba.extend_from_slice(&[p[2], p[1], p[0], if visible { 255 } else { 0 }]);
        }
    }
    Ok(Some(Cursor {
        width: rect.width,
        height: rect.height,
        hotspot_x: rect.x,
        hotspot_y: rect.y,
        rgba,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn screen(w: u16, h: u16) -> Arc<Screen> {
        let screen = Screen::new();
        screen.lock().framebuffer.resize(w, h);
        screen
    }

    pub(super) fn rgb(screen: &Screen, x: u16, y: u16) -> [u8; 3] {
        let p = screen.lock().framebuffer.pixel(x, y).expect("in screen");
        [p[0], p[1], p[2]]
    }

    #[tokio::test]
    async fn raw_and_copy_rect() {
        let s = screen(3, 2);
        let mut d = Decoders::new();
        // Raw 2x1 at (0,0): red, green (wire order b, g, r, x).
        let data = [0, 0, 255, 0, 0, 255, 0, 0];
        let out = d
            .decode(encoding::RAW, Rect::new(0, 0, 2, 1), &mut &data[..], &s)
            .await
            .unwrap();
        assert_eq!(out, Outcome::Drawn);
        assert_eq!(rgb(&s, 0, 0), [255, 0, 0]);
        assert_eq!(rgb(&s, 1, 0), [0, 255, 0]);
        // CopyRect: copy the 2x1 at (0,0) to (1,1).
        let src = [0, 0, 0, 0];
        d.decode(
            encoding::COPY_RECT,
            Rect::new(1, 1, 2, 1),
            &mut &src[..],
            &s,
        )
        .await
        .unwrap();
        assert_eq!(rgb(&s, 1, 1), [255, 0, 0]);
        assert_eq!(rgb(&s, 2, 1), [0, 255, 0]);
    }

    #[tokio::test]
    async fn pseudo_encodings() {
        let s = screen(4, 4);
        let mut d = Decoders::new();
        let none: [u8; 0] = [];
        assert_eq!(
            d.decode(
                encoding::DESKTOP_SIZE,
                Rect::new(0, 0, 8, 6),
                &mut &none[..],
                &s
            )
            .await
            .unwrap(),
            Outcome::Resized
        );
        assert_eq!(s.lock().framebuffer.width(), 8);
        assert!(!s.lock().scancodes);
        assert_eq!(
            d.decode(encoding::QEMU_EXT_KEY, Rect::default(), &mut &none[..], &s)
                .await
                .unwrap(),
            Outcome::ExtKeyAck
        );
        assert!(s.lock().scancodes);

        // A 2x2 cursor with hot spot (1, 0): white pixels, mask 10 / 01.
        let mut data = vec![255u8; 16];
        data.extend_from_slice(&[0b1000_0000, 0b0100_0000]);
        assert_eq!(
            d.decode(encoding::CURSOR, Rect::new(1, 0, 2, 2), &mut &data[..], &s)
                .await
                .unwrap(),
            Outcome::Cursor
        );
        let cursor = s.lock().cursor.clone().expect("cursor");
        assert_eq!((cursor.hotspot_x, cursor.hotspot_y), (1, 0));
        let alpha: Vec<u8> = cursor.rgba.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alpha, [255, 0, 0, 255]);
        // An empty shape hides it.
        d.decode(encoding::CURSOR, Rect::new(0, 0, 0, 0), &mut &none[..], &s)
            .await
            .unwrap();
        assert!(s.lock().cursor.is_none());

        let err = d
            .decode(77, Rect::new(0, 0, 1, 1), &mut &none[..], &s)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unimplemented encoding 77"),
            "{err}"
        );
    }
}
