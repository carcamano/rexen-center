//! Renders ReXen Center's icon from its SVG source (see `../README.md`).
//!
//! ```text
//! icon-render png   <in.svg> <size> <out.png>
//! icon-render ico   <in.svg> <out.ico>
//! icon-render sheet <out.png> <name>=<tile.svg>,<mark.svg> ...
//! ```
//!
//! `sheet` draws a contact sheet: each concept at 16, 32, 128 and 512 px (and
//! the two small sizes magnified four times), as the rounded-square tile and
//! as the plain mark, on a light and on a dark background.

use std::process::ExitCode;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;

/// The sizes a Windows .ico carries.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// The sizes the contact sheet shows.
const SHEET_SIZES: [u32; 4] = [16, 32, 128, 512];
/// The magnification of the small sizes on the contact sheet.
const ZOOM: u32 = 4;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["png", input, size, out] => size
            .parse()
            .map_err(|e| format!("size {size}: {e}"))
            .and_then(|size| {
                std::fs::write(out, render_png(input, size)?).map_err(|e| e.to_string())
            }),
        ["ico", input, out] => {
            ico(input).and_then(|bytes| std::fs::write(out, bytes).map_err(|e| e.to_string()))
        }
        ["sheet", out, concepts @ ..] if !concepts.is_empty() => sheet(out, concepts),
        _ => Err(
            "usage: icon-render png <in.svg> <size> <out.png> | ico <in.svg> <out.ico> | \
                  sheet <out.png> <name>=<tile.svg>,<mark.svg> ..."
                .to_owned(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("icon-render: {e}");
            ExitCode::FAILURE
        }
    }
}

fn options() -> usvg::Options<'static> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    opt
}

fn parse(svg: &[u8]) -> Result<usvg::Tree, String> {
    usvg::Tree::from_data(svg, &options()).map_err(|e| e.to_string())
}

/// `tree` drawn into a `width` x `height` pixmap, scaled to fit.
fn rasterize(tree: &usvg::Tree, width: u32, height: u32) -> Result<Pixmap, String> {
    let mut pixmap = Pixmap::new(width, height).ok_or("empty size")?;
    let size = tree.size();
    let scale = (width as f32 / size.width()).min(height as f32 / size.height());
    resvg::render(
        tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

fn render_png(input: &str, size: u32) -> Result<Vec<u8>, String> {
    let svg = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let pixmap = rasterize(&parse(&svg)?, size, size)?;
    pixmap.encode_png().map_err(|e| e.to_string())
}

/// A Windows icon whose images are PNGs (Windows Vista and later).
fn ico(input: &str) -> Result<Vec<u8>, String> {
    let images = ICO_SIZES
        .iter()
        .map(|&size| render_png(input, size))
        .collect::<Result<Vec<_>, _>>()?;
    let count = images.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * u32::from(count);
    for (&size, png) in ICO_SIZES.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // width, height, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &images {
        out.extend_from_slice(png);
    }
    Ok(out)
}

fn sheet(out: &str, concepts: &[&str]) -> Result<(), String> {
    const MARGIN: u32 = 48;
    const GAP: u32 = 40;
    const LABEL_H: u32 = 64;
    const CAPTION_H: u32 = 36;
    let row_w: u32 = SHEET_SIZES.iter().sum::<u32>()
        + 16 * ZOOM
        + 32 * ZOOM
        + GAP * (SHEET_SIZES.len() as u32 + 1);
    let panel_w = row_w + 2 * GAP;
    let row_h = 512 + CAPTION_H;
    let block_h = LABEL_H + 2 * row_h + GAP;
    let width = MARGIN * 2 + panel_w * 2;
    let height = MARGIN * 2 + 96 + block_h * concepts.len() as u32;
    let backgrounds = [("#f4f5f7", "#3a3f47"), ("#1b1d21", "#c9ced6")];

    let mut body = String::new();
    body.push_str(&format!(
        r##"<rect width="{width}" height="{height}" fill="#8a9099"/>
<text x="{MARGIN}" y="{}" font-family="Helvetica, Arial, sans-serif" font-size="40" font-weight="bold" fill="#ffffff">ReXen Center icon concepts — tile (top) and plain mark (bottom) at 16, 32, 128 and 512 px; 16 and 32 px also shown ×4</text>"##,
        MARGIN + 44
    ));
    for (index, concept) in concepts.iter().enumerate() {
        let (name, files) = concept
            .split_once('=')
            .ok_or(format!("{concept}: expected name=tile.svg,mark.svg"))?;
        let (tile, mark) = files
            .split_once(',')
            .ok_or(format!("{concept}: expected two SVG files"))?;
        let top = MARGIN + 96 + block_h * index as u32;
        for (panel, (bg, fg)) in backgrounds.iter().enumerate() {
            let left = MARGIN + panel_w * panel as u32;
            body.push_str(&format!(
                r##"<rect x="{left}" y="{top}" width="{}" height="{}" rx="24" fill="{bg}"/>
<text x="{}" y="{}" font-family="Helvetica, Arial, sans-serif" font-size="40" font-weight="bold" fill="{fg}">{}. {}</text>"##,
                panel_w - 16,
                block_h - GAP,
                left + GAP,
                top + 52,
                index + 1,
                escape(name)
            ));
            for (row, input) in [tile, mark].iter().enumerate() {
                let svg = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
                let tree = parse(&svg)?;
                let y = top + LABEL_H + row_h * row as u32;
                let mut x = left + GAP;
                let mut cell = |size: u32,
                                shown: u32,
                                caption: String,
                                body: &mut String|
                 -> Result<(), String> {
                    let png = rasterize(&tree, size, size)?
                        .encode_png()
                        .map_err(|e| e.to_string())?;
                    let iy = y + 512 - shown;
                    body.push_str(&format!(
                        r##"<image x="{x}" y="{iy}" width="{shown}" height="{shown}" image-rendering="optimizeSpeed" href="data:image/png;base64,{}"/>
<text x="{x}" y="{}" font-family="Helvetica, Arial, sans-serif" font-size="22" fill="{fg}">{caption}</text>"##,
                        base64(&png),
                        y + 512 + 28,
                    ));
                    x += shown + GAP;
                    Ok(())
                };
                for size in SHEET_SIZES {
                    cell(size, size, format!("{size}"), &mut body)?;
                }
                for size in [16, 32] {
                    cell(size, size * ZOOM, format!("{size} ×{ZOOM}"), &mut body)?;
                }
            }
        }
    }
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">{body}</svg>"#
    );
    let pixmap = rasterize(&parse(svg.as_bytes())?, width, height)?;
    pixmap.save_png(out).map_err(|e| e.to_string())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
