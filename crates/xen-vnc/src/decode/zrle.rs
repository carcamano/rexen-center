//! ZRLE (RFC 6143 §7.7.6): 64×64 tiles, each raw, solid, packed-palette,
//! plain RLE or palette RLE, all inside one zlib stream that lasts for the
//! whole connection.
//!
//! Derived from vnc-rs 0.6.1 `src/codec/zrle.rs` (MIT; see `LICENSE-vnc-rs`).
//! Changes: only this client's pixel format (32 bpp, depth 24, so a CPIXEL
//! is 3 bytes `[b, g, r]`), tiles are written straight into the framebuffer,
//! and the payload limits use this crate's constants.

use std::io::Read;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::zlib::ZlibReader;
use super::{MAX_COMPRESSED, Outcome, area};
use crate::error::{Result, VncError};
use crate::framebuffer::Screen;
use crate::protocol::Rect;

const TILE: u16 = 64;

fn invalid(what: &str) -> VncError {
    VncError::protocol(format!("invalid ZRLE data: {what}"))
}

fn read_run_length(reader: &mut ZlibReader<'_>, remaining: usize) -> Result<usize> {
    let mut run_length = 1;
    loop {
        let part = reader.read_u8()?;
        run_length += usize::from(part);
        if run_length > remaining {
            return Err(invalid("run longer than the tile"));
        }
        if part != 255 {
            return Ok(run_length);
        }
    }
}

/// A CPIXEL (`[b, g, r]`) as a wire pixel `[b, g, r, x]`.
fn read_cpixel(reader: &mut ZlibReader<'_>) -> Result<[u8; 4]> {
    let mut p = [0u8; 4];
    reader.read_exact(&mut p[..3])?;
    Ok(p)
}

fn palette_entry(palette: &[[u8; 4]], index: u8) -> Result<[u8; 4]> {
    palette
        .get(usize::from(index))
        .copied()
        .ok_or_else(|| invalid("palette index out of range"))
}

pub(super) struct Decoder {
    decompressor: Option<flate2::Decompress>,
}

impl Decoder {
    pub(super) fn new() -> Self {
        Self {
            decompressor: Some(flate2::Decompress::new(true)),
        }
    }

    pub(super) async fn decode<R: AsyncRead + Unpin>(
        &mut self,
        input: &mut R,
        rect: Rect,
        screen: &Arc<Screen>,
    ) -> Result<Outcome> {
        area(rect)?;
        let data_len = input.read_u32().await? as usize;
        if data_len > MAX_COMPRESSED {
            return Err(invalid("payload too large"));
        }
        let mut zlib_data = vec![0u8; data_len];
        input.read_exact(&mut zlib_data).await?;
        let decompressor = self
            .decompressor
            .take()
            .ok_or_else(|| invalid("zlib stream lost after an earlier error"))?;
        let mut reader = ZlibReader::new(decompressor, &zlib_data);

        let mut palette: Vec<[u8; 4]> = Vec::with_capacity(128);
        let mut pixels: Vec<u8> = Vec::with_capacity(usize::from(TILE) * usize::from(TILE) * 4);
        let mut y = 0;
        while y < rect.height {
            let height = (rect.height - y).min(TILE);
            let mut x = 0;
            while x < rect.width {
                let width = (rect.width - x).min(TILE);
                let pixel_count = usize::from(height) * usize::from(width);
                let control = reader.read_u8()?;
                let is_rle = control & 0x80 != 0;
                let palette_size = control & 0x7f;
                palette.clear();
                for _ in 0..palette_size {
                    palette.push(read_cpixel(&mut reader)?);
                }
                pixels.clear();
                match (is_rle, palette_size) {
                    (false, 0) => {
                        for _ in 0..pixel_count {
                            pixels.extend_from_slice(&read_cpixel(&mut reader)?);
                        }
                    }
                    (false, 1) => {
                        for _ in 0..pixel_count {
                            pixels.extend_from_slice(&palette[0]);
                        }
                    }
                    (false, 2..=16) => {
                        // Packed palette indexes; each row starts on a byte.
                        let bits: u32 = match palette_size {
                            2 => 1,
                            3..=4 => 2,
                            _ => 4,
                        };
                        let mask = (1u8 << bits) - 1;
                        for _ in 0..height {
                            let mut byte = 0u8;
                            let mut left = 0u32;
                            for _ in 0..width {
                                if left == 0 {
                                    byte = reader.read_u8()?;
                                    left = 8;
                                }
                                left -= bits;
                                let index = (byte >> left) & mask;
                                pixels.extend_from_slice(&palette_entry(&palette, index)?);
                            }
                        }
                    }
                    (true, 0) => {
                        let mut count = 0;
                        while count < pixel_count {
                            let pixel = read_cpixel(&mut reader)?;
                            let run = read_run_length(&mut reader, pixel_count - count)?;
                            for _ in 0..run {
                                pixels.extend_from_slice(&pixel);
                            }
                            count += run;
                        }
                    }
                    (true, 2..=127) => {
                        let mut count = 0;
                        while count < pixel_count {
                            let code = reader.read_u8()?;
                            let pixel = palette_entry(&palette, code & 0x7f)?;
                            let run = if code & 0x80 != 0 {
                                read_run_length(&mut reader, pixel_count - count)?
                            } else {
                                1
                            };
                            for _ in 0..run {
                                pixels.extend_from_slice(&pixel);
                            }
                            count += run;
                        }
                    }
                    _ => return Err(invalid(&format!("sub-encoding {control:#04x}"))),
                }
                screen
                    .lock()
                    .framebuffer
                    .put_bgrx(Rect::new(rect.x + x, rect.y + y, width, height), &pixels);
                x += width;
            }
            y += height;
        }
        self.decompressor = Some(reader.into_inner()?);
        Ok(Outcome::Drawn)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::tests::{rgb, screen};
    use super::*;

    /// One zlib stream shared by the payloads, sync-flushed after each one
    /// (as QEMU does).
    struct Encoder(flate2::write::ZlibEncoder<Vec<u8>>);

    impl Encoder {
        fn new() -> Self {
            Self(flate2::write::ZlibEncoder::new(
                Vec::new(),
                flate2::Compression::default(),
            ))
        }

        fn payload(&mut self, raw: &[u8]) -> Vec<u8> {
            self.0.write_all(raw).unwrap();
            self.0.flush().unwrap();
            let compressed = std::mem::take(self.0.get_mut());
            let mut out = (compressed.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&compressed);
            out
        }
    }

    #[tokio::test]
    async fn every_sub_encoding_over_one_zlib_stream() {
        let s = screen(70, 3);
        let mut enc = Encoder::new();
        let mut d = Decoder::new();

        // Rect 1, 70x1: tile 64x1 solid red, tile 6x1 raw.
        let mut raw = vec![1, 0, 0, 255];
        raw.push(0);
        for i in 0..6u8 {
            raw.extend_from_slice(&[i, 0, 0]);
        }
        let payload = enc.payload(&raw);
        d.decode(&mut &payload[..], Rect::new(0, 0, 70, 1), &s)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 0, 0), [255, 0, 0]);
        assert_eq!(rgb(&s, 63, 0), [255, 0, 0]);
        assert_eq!(rgb(&s, 64, 0), [0, 0, 0]);
        assert_eq!(rgb(&s, 69, 0), [0, 0, 5]);

        // Rect 2, 3x2 packed palette (2 colours, 1 bit): rows 101 / 010.
        let mut raw = vec![2];
        raw.extend_from_slice(&[0, 255, 0]); // green
        raw.extend_from_slice(&[255, 255, 255]); // white
        raw.extend_from_slice(&[0b1010_0000, 0b0100_0000]);
        let payload = enc.payload(&raw);
        d.decode(&mut &payload[..], Rect::new(0, 1, 3, 2), &s)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 0, 1), [255, 255, 255]);
        assert_eq!(rgb(&s, 1, 1), [0, 255, 0]);
        assert_eq!(rgb(&s, 2, 1), [255, 255, 255]);
        assert_eq!(rgb(&s, 0, 2), [0, 255, 0]);
        assert_eq!(rgb(&s, 1, 2), [255, 255, 255]);

        // Rect 3, 4x1 plain RLE: blue x3, red x1.
        let mut raw = vec![128];
        raw.extend_from_slice(&[255, 0, 0, 2]);
        raw.extend_from_slice(&[0, 0, 255, 0]);
        let payload = enc.payload(&raw);
        d.decode(&mut &payload[..], Rect::new(10, 2, 4, 1), &s)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 12, 2), [0, 0, 255]);
        assert_eq!(rgb(&s, 13, 2), [255, 0, 0]);

        // Rect 4, one 64x5 tile of palette RLE with a run over 255:
        // index 1 x319, then index 0 x1.
        let s2 = screen(64, 5);
        let mut raw = vec![128 | 2];
        raw.extend_from_slice(&[0, 0, 0]);
        raw.extend_from_slice(&[0, 0, 255]);
        raw.extend_from_slice(&[0x80 | 1, 255, 63, 0]);
        let payload = enc.payload(&raw);
        d.decode(&mut &payload[..], Rect::new(0, 0, 64, 5), &s2)
            .await
            .unwrap();
        assert_eq!(rgb(&s2, 0, 0), [255, 0, 0]);
        assert_eq!(rgb(&s2, 62, 4), [255, 0, 0]);
        assert_eq!(rgb(&s2, 63, 4), [0, 0, 0]);
    }

    #[tokio::test]
    async fn bad_data_is_an_error() {
        let s = screen(4, 4);
        let mut enc = Encoder::new();
        let mut d = Decoder::new();
        // Palette RLE with an index outside the palette.
        let payload = enc.payload(&[128 | 2, 0, 0, 0, 1, 1, 1, 5]);
        let err = d
            .decode(&mut &payload[..], Rect::new(0, 0, 1, 1), &s)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("palette index"), "{err}");
    }
}
