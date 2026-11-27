# Changelog

All notable changes to ReXen Center. Versions follow the workspace version in
`Cargo.toml`; each is released from the tag `v<version>`.

## Unreleased

- **GNOME and KDE Plasma title bar**: on these desktops the main window and
  the undocked consoles no longer show winit's plain frame. The window draws
  its own title bar with the menus, the title and the window buttons in
  Adwaita's or Breeze's look, in the order the desktop's settings give
  (`button-layout`, `kwinrc`), rounded corners, and resizing at the edges.
  `REXENCENTER_TITLE_BAR=system` brings the system's frame back.
- **Windows title bar**: the main window and the undocked consoles put the
  menus in the title bar, with the title and Windows 11's caption buttons
  (Minimize, Maximize or Restore, Close; light and dark; faded while the
  window is inactive), instead of the system's frame above an in-window menu
  row. Dragging the bar uses the system's move loop (Snap still works),
  double-click maximizes or restores, and the system keeps the shadow and the
  rounded corners. Snap Layouts on hovering Maximize is not available (winit
  cannot answer `WM_NCHITTEST` without unsafe code).
  `REXENCENTER_TITLE_BAR=system` brings the system's frame back; `windows`
  forces this look elsewhere.
- **Dialogs in the desktop's look**: with that title bar, every dialog (Connect,
  Options, About, the wizards, the confirmations and the other dialogs) gets
  the same desktop's look: libadwaita's rounded card with a flat header and
  Adwaita's round close button on GNOME, Breeze's title bar and close glyph on
  KDE Plasma, the title on the left and Windows 11's Close button on Windows,
  with the close button at the window's close button's end. The window behind
  an open dialog is dimmed and takes no input; its title bar still moves it.
  The dialogs keep their sizes, buttons and keys, and keep egui's look on
  other desktops and platforms.
- **macOS menu bar and unified title bar**: on macOS the menus move from the
  window to the system's menu bar (through `muda`, a macOS-only dependency):
  a ReXen Center menu with About, Settings… (⌘,; Tools > Options elsewhere),
  Services, Hide, Hide Others, Show All and Quit (⌘Q), then File, Edit (Cut,
  Copy, Paste, Select All), View (with Enter Full Screen), Pool, VM, Server
  and Storage (following the selection and carrying out the same operations
  and dialogs), Window (Minimize, Zoom, Bring All to Front) and Help. The
  toolbar moves into the title bar beside the window buttons; its empty
  space moves the window and a double-click zooms or minimizes it as System
  Settings say. `REXENCENTER_TITLE_BAR=system` keeps the system's title bar.
  Linux and Windows are unchanged.
- **Dialog buttons at the bottom right**: every dialog's and wizard's buttons
  (OK, Cancel, Yes, No, Previous, Next, Finish…) sit at the dialog's bottom
  right instead of the left, in the platform's order: Cancel and then the
  main action at the right end on macOS, GNOME and the other Linux desktops,
  as Apple's and GNOME's guidelines ask; the main action and then Cancel on
  Windows and KDE Plasma (a forced `REXENCENTER_TITLE_BAR` look brings its
  desktop's order). Path fields fill their row with the Browse... button at
  its right end, so those buttons line up with each other and with the
  dialog's (Tools > Options > External Tools among others).
- **Tabs that look like tabs**: the detail tabs (General, Memory, Storage…)
  are drawn as macOS draws a tab view: a segmented control centred on the
  line at the top of the pane, the selected tab raised, instead of buttons
  like the toolbar's. The Alerts and Events switch uses the same segmented
  control.

## 0.1.1 — 2026-11-27

A documentation and housekeeping release: the app behaves as in 0.1.0.

- **README**: rebuilt as a landing page with the icon, badges, screenshots, a
  download table per platform, install steps for each platform (including how to
  start the unsigned builds on macOS and Windows), the features by area, the MCP
  server, building from source, and credits, in collapsible sections.
- **Screenshots**: rendered by the app's test harness against the mock XAPI
  server (made-up hosts, VMs and documentation addresses only), stored in
  `docs/screenshots/`. An ignored test behind the new `readme-screenshots`
  feature regenerates them.
- **Clean-up**: removed a stray file from `crates/xenadmin`; tests no longer name
  the maintainer's lab objects. The real-host tests that act on one VM take its
  name from `XENADMIN_REAL_NAMED_VM` (default `rexen-test-vm`) and are renamed
  `real_host_named_vm_lifecycle`, `real_host_named_vm_friendly_error`,
  `real_console_named_vm`, `real_performance_named_vm` and `real_f7_named_vm`.

## 0.1.0 — 2026-11-27

The first release of ReXen Center, a Rust rewrite of XCP-ng Center / XenAdmin
25.04 for macOS, Windows and Linux.

- Connections to XCP-ng 8.2+ over JSON-RPC/HTTPS with certificate pinning,
  saved servers, passwords in the OS keyring and automatic reconnection.
- The infrastructure tree, detail tabs, views, search, folders, tags and custom
  fields.
- VM and host operations, the VNC console, performance graphs and alerts.
- The New VM, New SR, New Network, HA, import/export (XVA, OVF/OVA, disk
  images), Migrate/Move/Copy, vApp, scheduled snapshot and disaster recovery
  wizards; pools, Active Directory, GPUs, USB, vTPMs and memory settings.
- An MCP server (`rexencenter --mcp`) with read tools and allow-listed VM
  lifecycle and snapshot tools.
- Packages: macOS `.dmg` (Apple Silicon and Intel), Windows `.msi` and portable
  `.zip`, Debian/Ubuntu `.deb`, Fedora `.rpm` and a Linux `.tar.gz`, built by
  GitHub Actions and not signed.
