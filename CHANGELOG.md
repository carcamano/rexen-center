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
