//! Hextile (RFC 6143 §7.7.4; `VNCStream.ReadHextileEncoding`): the rectangle
//! is cut into 16×16 tiles, each raw or a background plus solid
//! sub-rectangles. The background and foreground colours carry over from one
//! tile to the next.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::rre::sub_rect;
use super::{Outcome, read_pixel, read_pixels};
use crate::error::Result;
use crate::framebuffer::Screen;
use crate::protocol::Rect;

const RAW: u8 = 1;
const BACKGROUND_SPECIFIED: u8 = 2;
const FOREGROUND_SPECIFIED: u8 = 4;
const ANY_SUBRECTS: u8 = 8;
const SUBRECTS_COLOURED: u8 = 16;

pub(super) async fn decode<R: AsyncRead + Unpin>(
    input: &mut R,
    rect: Rect,
    screen: &Arc<Screen>,
) -> Result<Outcome> {
    let mut background = [0u8; 4];
    let mut foreground = [0u8; 4];
    let mut ty = 0;
    while ty < rect.height {
        let th = (rect.height - ty).min(16);
        let mut tx = 0;
        while tx < rect.width {
            let tw = (rect.width - tx).min(16);
            let tile = Rect::new(rect.x + tx, rect.y + ty, tw, th);
            let mask = input.read_u8().await?;
            if mask & RAW != 0 {
                let data = read_pixels(input, usize::from(tw) * usize::from(th)).await?;
                screen.lock().framebuffer.put_bgrx(tile, &data);
            } else {
                if mask & BACKGROUND_SPECIFIED != 0 {
                    background = read_pixel(input).await?;
                }
                if mask & FOREGROUND_SPECIFIED != 0 {
                    foreground = read_pixel(input).await?;
                }
                let mut fills = Vec::new();
                if mask & ANY_SUBRECTS != 0 {
                    let count = input.read_u8().await?;
                    for _ in 0..count {
                        let colour = if mask & SUBRECTS_COLOURED != 0 {
                            read_pixel(input).await?
                        } else {
                            foreground
                        };
                        let xy = input.read_u8().await?;
                        let wh = input.read_u8().await?;
                        let r = sub_rect(
                            tile,
                            u16::from(xy >> 4),
                            u16::from(xy & 15),
                            u16::from(wh >> 4) + 1,
                            u16::from(wh & 15) + 1,
                        );
                        fills.push((r, colour));
                    }
                }
                let mut state = screen.lock();
                state.framebuffer.fill_bgrx(tile, background);
                for (r, colour) in fills {
                    state.framebuffer.fill_bgrx(r, colour);
                }
            }
            tx += tw;
        }
        ty += th;
    }
    Ok(Outcome::Drawn)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{rgb, screen};
    use super::*;

    const RED: [u8; 4] = [0, 0, 255, 0];
    const GREEN: [u8; 4] = [0, 255, 0, 0];
    const BLUE: [u8; 4] = [255, 0, 0, 0];

    #[tokio::test]
    async fn tiles_raw_solid_and_with_subrects() {
        // 20x17: tiles 16x16, 4x16, 16x1, 4x1.
        let s = screen(20, 17);
        let mut data = Vec::new();
        // Tile 1: background red, foreground green, 1 sub-rect at (2,3) 3x2.
        data.push(BACKGROUND_SPECIFIED | FOREGROUND_SPECIFIED | ANY_SUBRECTS);
        data.extend_from_slice(&RED);
        data.extend_from_slice(&GREEN);
        data.push(1);
        data.extend_from_slice(&[0x23, 0x21]);
        // Tile 2: raw 4x16, all blue.
        data.push(RAW);
        for _ in 0..64 {
            data.extend_from_slice(&BLUE);
        }
        // Tile 3: nothing specified: keeps the red background.
        data.push(0);
        // Tile 4: coloured sub-rect (blue) at (3,0) 1x1 over the red background.
        data.push(ANY_SUBRECTS | SUBRECTS_COLOURED);
        data.push(1);
        data.extend_from_slice(&BLUE);
        data.extend_from_slice(&[0x30, 0x00]);
        decode(&mut &data[..], Rect::new(0, 0, 20, 17), &s)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 0, 0), [255, 0, 0]);
        assert_eq!(rgb(&s, 2, 3), [0, 255, 0]);
        assert_eq!(rgb(&s, 4, 4), [0, 255, 0]);
        assert_eq!(rgb(&s, 5, 4), [255, 0, 0]);
        assert_eq!(rgb(&s, 4, 5), [255, 0, 0]);
        assert_eq!(rgb(&s, 17, 10), [0, 0, 255]);
        assert_eq!(rgb(&s, 5, 16), [255, 0, 0]);
        assert_eq!(rgb(&s, 16, 16), [255, 0, 0]);
        assert_eq!(rgb(&s, 19, 16), [0, 0, 255]);
    }
}
