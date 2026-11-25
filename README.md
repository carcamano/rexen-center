# xenadmin-rs

A cross-platform (Windows, Linux, macOS) management console for XCP-ng pools,
written in Rust with [egui](https://github.com/emilk/egui). It is a rewrite of
XCP-ng Center / XenAdmin 25.04, whose C# source in `../original-source/` is kept
untouched as the reference.

`xenadmin-rs` is the product name for now. It is defined once, in
`crates/xenadmin/src/branding.rs`.

**Status: phase F2 (F2a + F2b: connection and model foundation) on top of
F1 (base).** What works:

- connecting to XCP-ng 8.2+ hosts (XAPI API 2.16 or later) with a user name
  and password over JSON-RPC/HTTPS, **several servers at once**;
- trust on first use: an unknown certificate shows its SHA-256 fingerprint
  and is asked about before credentials are sent; a **changed** certificate
  is asked about again, showing both fingerprints. Trusted fingerprints are
  saved per server (`known_servers.toml`);
- **pool members redirect to the coordinator** (`HOST_IS_SLAVE`), and
  connecting twice to the same server or pool is refused;
- **automatic reconnection**: when a server goes away (toolstack restart,
  network loss, reboot) the connection retries with exponential backoff
  (2 s doubling up to 60 s; "Retry Now" skips the wait) while the last tree
  stays visible. A **heartbeat** (`host.get_servertime` every 15 s) notices a
  dead link that a long-poll would not. `event.from` uses a 60 s server
  timeout, always shorter than its HTTP timeout. The cache is reloaded in
  full on `EVENTS_LOST`, and the session is re-established on
  `SESSION_INVALID`;
- **saved servers and passwords**: servers that connect are saved; with
  "Remember password" the password goes to the OS keyring (macOS Keychain,
  Windows Credential Manager, Secret Service on Linux) and is read from
  there at each login. Servers that were connected when the app closed
  **reconnect on start without asking for the password**;
- **settings** in TOML (`settings.toml`) with a **portable mode**, and
  **file logging** (never with passwords or session references);
- an **Events panel**: the history of this client's operations and
  connection events, with progress and Cancel for those that run as XAPI
  tasks, plus the tasks other clients run on the connected servers (also
  cancellable);
- an infrastructure tree per connection, as in the original: pool → hosts →
  VMs and local storage, with shared storage, VMs without a home server and
  templates under the pool. Icons show the VM power state (running, halted,
  suspended, paused, busy), templates, snapshots, hosts in maintenance mode or
  unreachable, and broken or default SRs. The View menu shows or hides custom
  and built-in templates, local storage, snapshots (under their VM) and hidden
  objects (saved in the settings), and a filter keeps the objects whose name
  matches and their parents. Control domains are never shown. The tree is
  kept live by `event.from` with no manual refresh, and also lists the saved
  servers that are not connected;
- read-only detail tabs for the selected object, with the original's
  layout and fields: **General** (VM, template, snapshot, host, pool, SR),
  **Memory** (host bars and per-VM settings), **Storage** (a VM's disks, an
  SR's virtual disks, the SRs of a host or pool), **Networking** (a VM's
  interfaces with guest IPs; the networks and IP configuration of a host or
  pool) and **NICs**. They follow `event.from` live;
- Start and Shut Down (clean) for VMs, enabled from `power_state` and
  `allowed_operations`, run as `Async.VM.*` tasks;
- an About window and an Options window;
- the complete XenAPI surface, generated from the upstream datamodel: every
  class, record, enum and call, with `Async.*` variants (`crates/xapi-codegen`);
- a client-side cache of every class the server delivers through events,
  loaded after an `event.inject` token so nothing is lost between the load
  and the first `event.from`.

Editing from the tabs, the console and performance graphs come in later
phases. See the port plan for the full roadmap.

## Build and run

You need a stable Rust toolchain (edition 2024, Rust 1.88 or newer) from
[rustup](https://rustup.rs).

```sh
cargo run --release -p xenadmin
```

On Linux, eframe also needs the usual X11/Wayland development packages (for
example `libxkbcommon-dev`, `libgl1-mesa-dev` and `libwayland-dev`), and the
keyring needs `libdbus-1-dev` (Secret Service).

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

## Settings, passwords and logs

The configuration folder is, in order of preference:

1. `XENADMIN_CONFIG_DIR`, if set;
2. `settings/` next to the executable when a file named `portable` sits
   beside it (**portable mode**);
3. the per-user folder: `~/Library/Application Support/xenadmin-rs` on
   macOS, `%APPDATA%\xenadmin-rs\config` on Windows,
   `$XDG_CONFIG_HOME/xenadmin-rs` on Linux.

It holds `settings.toml` (preferences and the saved servers),
`known_servers.toml` (the trusted certificate fingerprint of each
`host:port`) and `logs/` (one file per day, the last 7 kept). Tools > Options
shows where it is.

Passwords are never written to these files: with "Remember password" they go
to the OS keyring under the service `xenadmin-rs`
(`XENADMIN_KEYRING_SERVICE` overrides the name, e.g. to keep a test profile
apart), one entry per `user@https://host:port`. Without it the password stays
in memory for the session only. Forgetting a server (right-click a saved
server > Forget Server) deletes its saved password.

Servers can also be saved from a script, without opening the window:

```sh
printf '%s\n' "$PASSWORD" | xenadmin --add-server 192.0.2.10 --user root --password-stdin
xenadmin --forget-server 192.0.2.10
```

Logging goes to stderr and the log folder; the level comes from `RUST_LOG`
(default `warn,xapi=info,xen_core=info,xenadmin=info`). Passwords and session
references are never logged: calls are traced by method name only, and the
client redacts the session reference XAPI puts in `SESSION_INVALID` errors.
A test runs every flow at `trace` level and checks the log for both.
`Ctrl+C` or `SIGTERM` close the app normally, logging out of every server.

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests need no XCP-ng host and never touch the OS keyring (they use an
in-memory store). `crates/xapi-mock` is a JSON-RPC server loaded
with fixtures from `tests/fixtures/`: `records.json`, a small hand-written lab
pool (two hosts, one in maintenance mode, VMs in several power states,
templates, a snapshot, local, NFS and ISO storage with a broken SR, networks
with a VLAN, NICs and metrics), and `xcp-ng-8.3-capture.json`, a sanitized read-only capture of a real
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
- reconnection after the server stops and restarts on the same port (every
  session forgotten, like a toolstack restart), "Retry Now", and the
  heartbeat noticing a server that stops answering;
- the `HOST_IS_SLAVE` redirect to the coordinator (and redirect loops),
  duplicate servers and pools being refused, several pools at once;
- the "certificate changed" question after the server comes back with a
  renewed certificate, and refusing it;
- settings, pinned certificates and passwords surviving an app restart and
  restoring the connection with no password and no question; a password not
  remembered never reaching the keyring; the password being read from the
  keyring at each reconnection;
- VM operations as tasks with progress, cancel and `task.destroy`, and other
  clients' tasks followed and cancelled;
- logs at `trace` level free of passwords and session references;
- the tree (homes, storage, templates, snapshots, view options, filter) and
  every tab's data against the hand-written pool and the real-host capture;
- the egui window itself, run headlessly with `egui_kittest`: connect dialog,
  certificate prompt, tree, View options and filter, the detail tabs and
  their live updates, Events panel (progress, cancel, other clients' tasks),
  the reconnection shown after a server restart, servers restored on start,
  and Options;
- that the committed generated code matches the pinned datamodel.

A read-only check of the real window code against a real host (it types the
credentials into the connect dialog, trusts only the expected certificate,
waits for the tree and clicks nothing else):

```sh
XENADMIN_REAL_HOST=192.0.2.10 XENADMIN_REAL_USER=root XENADMIN_REAL_PASSWORD=... \
XENADMIN_REAL_FINGERPRINT=AA:BB:... cargo test -p xenadmin real_host -- --ignored --nocapture
```

With the same variables, `real_host_tabs_read_only` also draws every tab of
the pool, its coordinator, one VM (`XENADMIN_REAL_VM=<uuid>`, else the first
running guest) and one SR (`XENADMIN_REAL_SR=<uuid>`, else the default SR),
and writes what they show to `XENADMIN_REAL_DUMP` (a JSON file) for
comparison with `xe vm-param-list` and friends. It only reads.

A round trip through the real OS keyring, under a test service name, with
the entry removed again: `cargo test -p xen-core os_keyring -- --ignored`.

To watch a reconnection by hand, run the mock on a fixed port, connect, stop
it with Ctrl+C and start it again:

```sh
cargo run -p xapi-mock --example serve -- --port 47123
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
| `crates/xen-core` | UI-independent backend: the connection manager and per-connection actors (login, TOFU, redirect, heartbeat, reconnection), the live object cache (one map per class), its tree and the facts derived from it (VM home, SR state, host memory, NIC and network names), the operation history, settings, known servers and the keyring store. |
| `crates/xenadmin` | The egui/eframe desktop app. |
| `crates/xapi-mock` | Test-only mock XAPI server. |

`xapi` and `xen-core` do not depend on egui. The app runs a tokio runtime on
background threads and keeps eframe on the main thread. It sends `Command`s to
the backend over an mpsc channel and reads an immutable `Arc<AppState>`
snapshot each frame (every connection, the operation history and the
settings). The backend calls `request_repaint` after each batch of events,
publishing at most about 10 snapshots per second per connection.

## License

BSD-2-Clause; see [LICENSE](LICENSE). Credits go to the XenAdmin (Citrix
XenCenter) and XCP-ng Center projects this work derives from.
