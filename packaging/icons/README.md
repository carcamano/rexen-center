# ReXen Center icon

The icon is concept 7 of the eight drawn for the product: a badge whose rim
is half a smooth blue ring and half a rust-orange gear, with a white rocket
crossing the split. It is a redrawn blend of the XCP-ng Center rocket badge
and the Rust gear, not a copy of either logo; both source marks are
trademarks of their projects (Vates / the XCP-ng project, and the Rust
Foundation), so check their usage policies before a public release.

| File | What it is |
|---|---|
| `icon.svg` | The source: the macOS rounded-square tile on a 1024 px grid (a copy of `concepts/7-half-badge.svg`). |
| `icon-small.svg` | The same drawing simplified for 16 and 24 px: a thicker blue rim, a gear with ten chunky teeth instead of twenty fine ones, a smaller navy disc and no porthole on the rocket. |
| `png/icon-<size>.png` | Rendered sizes, 16 to 1024 px: the macOS `.icns` and the Linux hicolor icons. |
| `icon.ico` | The Windows icon (16 to 256 px), embedded in `rexencenter.exe`. |
| `../../crates/xenadmin/assets/icon-256.png` | The window and taskbar icon and the About window's, compiled into the binary. |
| `concepts/` | All eight concepts, each as the tile and as the plain mark. 1-4 are original designs; 5-8 blend the XCP-ng and Rust marks. Only concept 7 ships. |
| `render/` | `icon-render`, the resvg-based renderer (its own Cargo project, outside the workspace). |

After changing an SVG, run `packaging/icons/render.sh` and commit the
rendered files with it.
