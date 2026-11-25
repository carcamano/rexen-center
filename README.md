# xenadmin-rs

A cross-platform (Windows, Linux, macOS) management console for XCP-ng pools,
written in Rust with [egui](https://github.com/emilk/egui). It is a rewrite of
XCP-ng Center / XenAdmin 25.04, whose C# source in `../original-source/` is kept
untouched as the reference.

`xenadmin-rs` is the product name for now. It is defined once, in
`crates/xenadmin/src/branding.rs`.

**Status: phase F1 (base).** What works:

- connecting to an XCP-ng 8.2+ host (XAPI API 2.16 or later) with a user name
  and password over JSON-RPC/HTTPS;
- trusting a self-signed certificate on first use: the app shows its SHA-256
  fingerprint and asks before sending credentials;
- a pool → hosts → VMs tree with power-state icons (templates, snapshots and
  control domains are hidden), kept live by `event.from` with no manual refresh;
- Start and Shut Down (clean) for VMs, enabled from `power_state` and
  `allowed_operations`;
- an About window.

Phase F1 does not persist anything yet: trusted certificates, server lists and
settings are F2 work, as are automatic reconnection and the generated XAPI
bindings. See the port plan for the full roadmap.

## Build and run

You need a stable Rust toolchain (edition 2024, Rust 1.88 or newer) from
[rustup](https://rustup.rs).

```sh
cargo run --release -p xenadmin
```

On Linux, eframe also needs the usual X11/Wayland development packages (for
example `libxkbcommon-dev`, `libgl1-mesa-dev` and `libwayland-dev`).

To try the app without a real host, start the mock server in another terminal
and connect to the address it prints (user `root`, password `mock-password`):

```sh
cargo run -p xapi-mock --example serve          # HTTPS with a self-signed certificate
cargo run -p xapi-mock --example serve -- http  # plain HTTP
```

To check a real host read-only from the command line:

```sh
XAPI_HOST=192.0.2.10 XAPI_USER=root XAPI_PASSWORD=... cargo run -p xapi --example list_objects
```

Logging uses `RUST_LOG` (default `warn,xapi=info,xen_core=info,xenadmin=info`).
Passwords and session references are never logged.

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests need no XCP-ng host. `crates/xapi-mock` is a JSON-RPC server loaded
with fixtures from `tests/fixtures/`, and the tests use it to check, end to end:

- login (including bad passwords and certificate pinning);
- the record listing;
- `VM.start` and `VM.clean_shutdown`;
- `event.from` long-polling and the live updates it drives;
- the egui window itself, run headlessly with `egui_kittest`.

## Layout

| Crate | Role |
|---|---|
| `crates/xapi` | XAPI client. Its hand-written core covers the transport, session, `event.from`, errors and TLS certificate pinning. `src/generated/` holds the API types and calls: written by hand in F1, generated from `xenapi.json` from F2 on. |
| `crates/xen-core` | UI-independent backend: the live object cache, the derived tree and the async backend actor. |
| `crates/xenadmin` | The egui/eframe desktop app. |
| `crates/xapi-mock` | Test-only mock XAPI server. |

`xapi` and `xen-core` do not depend on egui. The app runs a tokio runtime on
background threads and keeps eframe on the main thread. It sends `Command`s to
the backend over an mpsc channel and reads an immutable `Arc<AppState>`
snapshot each frame. The backend calls `request_repaint` after each batch of
events, publishing at most about 10 snapshots per second.

## License

BSD-2-Clause; see [LICENSE](LICENSE). Credits go to the XenAdmin (Citrix
XenCenter) and XCP-ng Center projects this work derives from.
