//! The client-side copy of the remote screen.
//!
//! Pixels are kept as opaque RGBA (what egui textures take). The decoders
//! write wire pixels ([`PixelFormat::BGRX`](crate::protocol::PixelFormat::BGRX))
//! into it; the console view uploads the region changed since its last look
//! ([`Framebuffer::take_dirty`]).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::protocol::{BYTES_PER_PIXEL, Rect};

/// Largest framebuffer side accepted from a server.
pub const MAX_DIMENSION: u16 = 8192;

/// The remote screen's pixels.
#[derive(Clone)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
    dirty: Option<Rect>,
    generation: u64,
}

impl std::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framebuffer")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("dirty", &self.dirty)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

impl Framebuffer {
    /// A black framebuffer of `width` × `height`.
    pub fn new(width: u16, height: u16) -> Self {
        let mut fb = Self {
            width: 0,
            height: 0,
            rgba: Vec::new(),
            dirty: None,
            generation: 0,
        };
        fb.resize(width, height);
        fb
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// RGBA bytes, row by row.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Increases with every change; the view compares it to know whether to
    /// look at [`Framebuffer::take_dirty`].
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The RGBA pixel at (`x`, `y`).
    pub fn pixel(&self, x: u16, y: u16) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4;
        self.rgba.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
    }

    /// Copies the RGBA pixels of `rect` (clipped to the screen), row by row.
    pub fn region(&self, rect: Rect) -> Vec<u8> {
        let rect = self.clip(rect);
        let mut out = Vec::with_capacity(usize::from(rect.width) * usize::from(rect.height) * 4);
        for row in rect.y..rect.y + rect.height {
            let start = self.offset(rect.x, row);
            out.extend_from_slice(&self.rgba[start..start + usize::from(rect.width) * 4]);
        }
        out
    }

    /// Changes the size; the content becomes black and all of it dirty.
    pub fn resize(&mut self, width: u16, height: u16) {
        let width = width.min(MAX_DIMENSION);
        let height = height.min(MAX_DIMENSION);
        self.width = width;
        self.height = height;
        self.rgba.clear();
        self.rgba.extend(
            std::iter::repeat_n([0, 0, 0, 255], usize::from(width) * usize::from(height)).flatten(),
        );
        self.dirty = Some(Rect::new(0, 0, width, height));
        self.generation += 1;
    }

    /// The region changed since the last call (the whole screen after a
    /// resize), or `None`.
    pub fn take_dirty(&mut self) -> Option<Rect> {
        self.dirty.take()
    }

    /// Writes `rect` from wire pixels (`[b, g, r, x]` each, row by row).
    /// The part outside the screen is dropped.
    pub fn put_bgrx(&mut self, rect: Rect, data: &[u8]) {
        let stride = usize::from(rect.width) * BYTES_PER_PIXEL;
        if data.len() < stride * usize::from(rect.height) {
            return;
        }
        let clipped = self.clip(rect);
        if clipped.is_empty() {
            return;
        }
        let dx = usize::from(clipped.x - rect.x);
        let dy = usize::from(clipped.y - rect.y);
        for row in 0..usize::from(clipped.height) {
            let src = &data[(dy + row) * stride + dx * BYTES_PER_PIXEL..];
            let start = self.offset(clipped.x, clipped.y + row as u16);
            let dst = &mut self.rgba[start..start + usize::from(clipped.width) * 4];
            let (dst, _) = dst.as_chunks_mut::<4>();
            let (src, _) = src.as_chunks::<BYTES_PER_PIXEL>();
            for (d, s) in dst.iter_mut().zip(src) {
                *d = [s[2], s[1], s[0], 255];
            }
        }
        self.mark(clipped);
    }

    /// Fills `rect` with one wire pixel (`[b, g, r, x]`).
    pub fn fill_bgrx(&mut self, rect: Rect, pixel: [u8; 4]) {
        let clipped = self.clip(rect);
        if clipped.is_empty() {
            return;
        }
        let rgba = [pixel[2], pixel[1], pixel[0], 255];
        for row in clipped.y..clipped.y + clipped.height {
            let start = self.offset(clipped.x, row);
            let row = &mut self.rgba[start..start + usize::from(clipped.width) * 4];
            row.as_chunks_mut::<4>().0.fill(rgba);
        }
        self.mark(clipped);
    }

    /// CopyRect: copies the `dst`-sized area at (`src_x`, `src_y`) to `dst`.
    pub fn copy_rect(&mut self, dst: Rect, src_x: u16, src_y: u16) {
        let src = Rect::new(src_x, src_y, dst.width, dst.height);
        if self.clip(src) != src || self.clip(dst) != dst || dst.is_empty() {
            return;
        }
        let row_bytes = usize::from(dst.width) * 4;
        let copy_row = |fb: &mut Self, row: u16| {
            let from = fb.offset(src_x, src_y + row);
            let to = fb.offset(dst.x, dst.y + row);
            fb.rgba.copy_within(from..from + row_bytes, to);
        };
        // Overlapping areas: copy rows in the direction that does not
        // overwrite rows still to be read.
        if dst.y > src_y {
            for row in (0..dst.height).rev() {
                copy_row(self, row);
            }
        } else {
            for row in 0..dst.height {
                copy_row(self, row);
            }
        }
        self.mark(dst);
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4
    }

    fn clip(&self, rect: Rect) -> Rect {
        let x = rect.x.min(self.width);
        let y = rect.y.min(self.height);
        let right = rect.right().min(u32::from(self.width));
        let bottom = rect.bottom().min(u32::from(self.height));
        Rect::new(
            x,
            y,
            right.saturating_sub(u32::from(x)) as u16,
            bottom.saturating_sub(u32::from(y)) as u16,
        )
    }

    fn mark(&mut self, rect: Rect) {
        self.dirty = Some(match self.dirty {
            Some(d) => d.union(&rect),
            None => rect,
        });
        self.generation += 1;
    }
}

/// The pointer shape the server sends (Cursor pseudo-encoding); the client
/// draws it at the local mouse position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub width: u16,
    pub height: u16,
    /// The hot spot, from the cursor's top-left corner.
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    /// RGBA, transparent where the server's mask is clear.
    pub rgba: Vec<u8>,
}

/// What the console view reads: the framebuffer and the cursor.
#[derive(Debug, Default)]
pub struct ScreenState {
    pub framebuffer: Framebuffer,
    /// `None` until the server sends a shape, or when it hides the cursor
    /// (an empty shape).
    pub cursor: Option<Arc<Cursor>>,
    /// Increases with every cursor change.
    pub cursor_generation: u64,
    /// The desktop name from the server's `ServerInit`.
    pub desktop_name: String,
    /// Whether the server acknowledged the QEMU Extended Key Event (key
    /// events then carry scancodes).
    pub scancodes: bool,
}

/// A [`ScreenState`] shared between the protocol task and the view.
#[derive(Debug, Default)]
pub struct Screen {
    state: Mutex<ScreenState>,
}

impl Screen {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Locks the state (held only briefly by the protocol task).
    pub fn lock(&self) -> MutexGuard<'_, ScreenState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgrx(r: u8, g: u8, b: u8) -> [u8; 4] {
        [b, g, r, 0]
    }

    #[test]
    fn writes_convert_wire_pixels_to_rgba_and_track_the_dirty_region() {
        let mut fb = Framebuffer::new(4, 3);
        assert_eq!(fb.take_dirty(), Some(Rect::new(0, 0, 4, 3)));
        assert_eq!(fb.take_dirty(), None);
        let g0 = fb.generation();
        let data: Vec<u8> = [bgrx(1, 2, 3), bgrx(4, 5, 6)].concat();
        fb.put_bgrx(Rect::new(1, 1, 2, 1), &data);
        assert_eq!(fb.pixel(1, 1), Some([1, 2, 3, 255]));
        assert_eq!(fb.pixel(2, 1), Some([4, 5, 6, 255]));
        assert_eq!(fb.pixel(0, 0), Some([0, 0, 0, 255]));
        fb.fill_bgrx(Rect::new(3, 2, 1, 1), bgrx(9, 9, 9));
        assert!(fb.generation() > g0);
        assert_eq!(fb.take_dirty(), Some(Rect::new(1, 1, 3, 2)));
    }

    #[test]
    fn writes_outside_the_screen_are_clipped() {
        let mut fb = Framebuffer::new(2, 2);
        fb.take_dirty();
        let data: Vec<u8> = [bgrx(1, 1, 1), bgrx(2, 2, 2), bgrx(3, 3, 3), bgrx(4, 4, 4)].concat();
        fb.put_bgrx(Rect::new(1, 1, 2, 2), &data);
        assert_eq!(fb.pixel(1, 1), Some([1, 1, 1, 255]));
        assert_eq!(fb.take_dirty(), Some(Rect::new(1, 1, 1, 1)));
        fb.fill_bgrx(Rect::new(5, 5, 3, 3), bgrx(7, 7, 7));
        assert_eq!(fb.take_dirty(), None);
    }

    #[test]
    fn copy_rect_handles_overlap() {
        let mut fb = Framebuffer::new(1, 4);
        for y in 0..4u16 {
            fb.fill_bgrx(Rect::new(0, y, 1, 1), bgrx(y as u8, 0, 0));
        }
        // Move rows 0..3 down by one (overlapping).
        fb.copy_rect(Rect::new(0, 1, 1, 3), 0, 0);
        let reds: Vec<u8> = (0..4).map(|y| fb.pixel(0, y).unwrap()[0]).collect();
        assert_eq!(reds, [0, 0, 1, 2]);
        // And back up.
        fb.copy_rect(Rect::new(0, 0, 1, 3), 0, 1);
        let reds: Vec<u8> = (0..4).map(|y| fb.pixel(0, y).unwrap()[0]).collect();
        assert_eq!(reds, [0, 1, 2, 2]);
        assert_eq!(
            fb.region(Rect::new(0, 1, 1, 2)),
            [1, 0, 0, 255, 2, 0, 0, 255]
        );
    }

    #[test]
    fn resize_clears_and_marks_everything() {
        let mut fb = Framebuffer::new(2, 2);
        fb.fill_bgrx(Rect::new(0, 0, 2, 2), bgrx(5, 5, 5));
        fb.take_dirty();
        fb.resize(3, 1);
        assert_eq!((fb.width(), fb.height()), (3, 1));
        assert_eq!(fb.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(fb.take_dirty(), Some(Rect::new(0, 0, 3, 1)));
    }
}
