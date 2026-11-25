# xenadmin-rs

A cross-platform (Windows, Linux, macOS) management console for XCP-ng pools,
written in Rust with [egui](https://github.com/emilk/egui). It is a rewrite of
XCP-ng Center / XenAdmin 25.04, whose C# source in `../original-source/` is kept
untouched as the reference.

`xenadmin-rs` is the product name for now. It is defined once, in
`crates/xenadmin/src/branding.rs`.

**Status: phase F2a (generated XenAPI bindings and full object cache) on top
of F1 (base).** What works:

- connecting to an XCP-ng 8.2+ host (XAPI API 2.16 or later) with a user name
  and password over JSON-RPC/HTTPS;
- trusting a self-signed certificate on first use: the app shows its SHA-256
  fingerprint and asks before sending credentials;
- a pool → hosts → VMs tree with power-state icons (templates, snapshots and
  control domains are hidden), kept live by `event.from` with no manual refresh;
- Start and Shut Down (clean) for VMs, enabled from `power_state` and
  `allowed_operations`;
- an About window;
- the complete XenAPI surface, generated from the upstream datamodel: every
  class, record, enum and call, with `Async.*` variants (`crates/xapi-codegen`);
- a client-side cache of every class the server delivers through events (VMs,
  hosts, SRs, VDIs, VBDs, networks, PIFs, VIFs, metrics, consoles, tasks,
  messages, roles…). It is loaded after an `event.inject` token, so nothing is
  lost between the load and the first `event.from`. It is reloaded in full on
  `EVENTS_LOST`, and on `SESSION_INVALID` the backend logs in again and
  reloads.

Nothing is persisted yet: trusted certificates, server lists and settings are
phase F2b work, as are reconnection with backoff, the heartbeat, the
`HOST_IS_SLAVE` redirect and the Events panel. The management tabs (General,
Memory, Storage, Network…) are phase F3. See the port plan for the full
roadmap.

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
cargo run -p xapi-mock --example serve             # HTTPS with a self-signed certificate
cargo run -p xapi-mock --example serve -- http     # plain HTTP
cargo run -p xapi-mock --example serve -- capture  # a sanitized real XCP-ng 8.3 host
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
with fixtures from `tests/fixtures/`: `records.json`, a small hand-written lab
pool, and `xcp-ng-8.3-capture.json`, a sanitized read-only capture of a real
XCP-ng 8.3 host covering every class it delivers. The tests use it to check,
end to end:

- login (including bad passwords and certificate pinning);
- that every real record decodes into the generated types and re-encodes
  without losing a field;
- the generated calls (`get_all_records`, `get_record`, `get_by_uuid`, field
  getters, `event.inject`, optional parameters) for any class;
- `VM.start` and `VM.clean_shutdown`;
- `event.from` long-polling and the live updates it drives, for any class;
- the cache's full reload on `EVENTS_LOST` and its re-login on
  `SESSION_INVALID`, and classes an older server does not know;
- the egui window itself, run headlessly with `egui_kittest`;
- that the committed generated code matches the pinned datamodel.

A read-only check of the real window code against a real host (it types the
credentials into the connect dialog, trusts only the expected certificate,
waits for the tree and clicks nothing else):

```sh
XENADMIN_REAL_HOST=192.0.2.10 XENADMIN_REAL_USER=root XENADMIN_REAL_PASSWORD=... \
XENADMIN_REAL_FINGERPRINT=AA:BB:... cargo test -p xenadmin real_host -- --ignored --nocapture
```

## Generated XenAPI bindings

`crates/xapi/src/generated/` is generated; do not edit it. To regenerate it
after changing the generator or the pinned datamodel:

```sh
cargo run -p xapi-codegen              # rewrite the generated files
cargo run -p xapi-codegen -- --check   # only check they are up to date
```

The datamodel is `crates/xapi-codegen/data/xenapi.json`, produced by xen-api's
`gen_json` at tag v25.27.0 (the XAPI of XCP-ng 8.3) and pinned by sha256. The
same directory explains how to rebuild it with opam.

## Layout

| Crate | Role |
|---|---|
| `crates/xapi` | XAPI client. Its hand-written core covers the transport, session, `event.from`, errors, dates, lenient decoding and TLS certificate pinning. `src/generated/` holds every API class, record, enum and call, generated from the pinned datamodel. |
| `crates/xapi-codegen` | The generator for `crates/xapi/src/generated/`, and the pinned datamodel in `data/`. |
| `crates/xen-core` | UI-independent backend: the live object cache (one map per class), the derived tree and the async backend actor. |
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
