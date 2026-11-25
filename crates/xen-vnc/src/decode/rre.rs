//! RRE and CoRRE: a background colour and a list of solid sub-rectangles
//! (`VNCStream.ReadRREEncoding` / `ReadCoRREEncoding`). CoRRE uses one byte
//! per sub-rectangle coordinate instead of two.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Outcome, read_pixel};
use crate::error::{Result, VncError};
use crate::framebuffer::Screen;
use crate::protocol::Rect;

/// Most sub-rectangles accepted in one rectangle.
const MAX_SUBRECTS: u32 = 1 << 22;

pub(super) async fn decode<R: AsyncRead + Unpin>(
    input: &mut R,
    rect: Rect,
    screen: &Arc<Screen>,
    compact: bool,
) -> Result<Outcome> {
    let count = input.read_u32().await?;
    if count > MAX_SUBRECTS {
        return Err(VncError::protocol(format!("{count} RRE sub-rectangles")));
    }
    let background = read_pixel(input).await?;
    let mut fills = Vec::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        let colour = read_pixel(input).await?;
        let (x, y, w, h) = if compact {
            let mut b = [0u8; 4];
            input.read_exact(&mut b).await?;
            (
                u16::from(b[0]),
                u16::from(b[1]),
                u16::from(b[2]),
                u16::from(b[3]),
            )
        } else {
            (
                input.read_u16().await?,
                input.read_u16().await?,
                input.read_u16().await?,
                input.read_u16().await?,
            )
        };
        fills.push((sub_rect(rect, x, y, w, h), colour));
    }
    let mut state = screen.lock();
    state.framebuffer.fill_bgrx(rect, background);
    for (r, colour) in fills {
        state.framebuffer.fill_bgrx(r, colour);
    }
    Ok(Outcome::Drawn)
}

/// A sub-rectangle at (`x`, `y`) inside `rect`, clipped to it.
pub(super) fn sub_rect(rect: Rect, x: u16, y: u16, w: u16, h: u16) -> Rect {
    let x = x.min(rect.width);
    let y = y.min(rect.height);
    let w = w.min(rect.width - x);
    let h = h.min(rect.height - y);
    Rect::new(rect.x + x, rect.y + y, w, h)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{rgb, screen};
    use super::*;

    #[tokio::test]
    async fn rre_and_corre_fill_sub_rectangles() {
        let s = screen(6, 6);
        // RRE at (1,1) 4x4: blue background, one red 2x1 at (1,2).
        let mut data = vec![0, 0, 0, 1];
        data.extend_from_slice(&[255, 0, 0, 0]); // blue
        data.extend_from_slice(&[0, 0, 255, 0]); // red
        data.extend_from_slice(&[0, 1, 0, 2, 0, 2, 0, 1]);
        decode(&mut &data[..], Rect::new(1, 1, 4, 4), &s, false)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 0, 0), [0, 0, 0]);
        assert_eq!(rgb(&s, 1, 1), [0, 0, 255]);
        assert_eq!(rgb(&s, 2, 3), [255, 0, 0]);
        assert_eq!(rgb(&s, 3, 3), [255, 0, 0]);
        assert_eq!(rgb(&s, 4, 3), [0, 0, 255]);

        // CoRRE at (0,0) 2x2: green background, one white 1x1 at (1,1).
        let mut data = vec![0, 0, 0, 1];
        data.extend_from_slice(&[0, 255, 0, 0]);
        data.extend_from_slice(&[255, 255, 255, 0]);
        data.extend_from_slice(&[1, 1, 1, 1]);
        decode(&mut &data[..], Rect::new(0, 0, 2, 2), &s, true)
            .await
            .unwrap();
        assert_eq!(rgb(&s, 0, 0), [0, 255, 0]);
        assert_eq!(rgb(&s, 1, 1), [255, 255, 255]);
    }

    #[test]
    fn sub_rectangles_stay_inside() {
        let r = Rect::new(10, 10, 4, 4);
        assert_eq!(sub_rect(r, 3, 3, 9, 9), Rect::new(13, 13, 1, 1));
        assert_eq!(sub_rect(r, 9, 0, 1, 1), Rect::new(14, 10, 0, 1));
    }
}
