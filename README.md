# xenadmin-rs

A cross-platform (Windows, Linux, macOS) management console for XCP-ng pools,
written in Rust with [egui](https://github.com/emilk/egui). It is a rewrite of
XCP-ng Center / XenAdmin 25.04, whose C# source in `../original-source/` is kept
untouched as the reference.

`xenadmin-rs` is the product name for now. It is defined once, in
`crates/xenadmin/src/branding.rs`.

**Status: phase F11a (XVA export and import) on top of F1–F10b (base,
connection and model, detail tabs, VM and host operations, VNC console,
performance graphs and alerts, VM creation, templates, snapshots and
disks, storage repositories, networking, pools and pool and server
properties, high availability, Active Directory and RBAC).** What works:

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
- **VM and host operations** (as the original's commands): Start, Shut
  Down, Reboot, Suspend, Resume, Pause, Unpause, Force Shut Down, Force
  Reboot, Start on Server, Resume on Server and Migrate to Server (live,
  within the pool) for VMs; Reboot, Shut Down, Power On, Enter/Exit
  Maintenance Mode and Restart Toolstack for hosts. They are offered from
  the server's `allowed_operations` (in the VM and Server menus, the
  toolbar, the details header and the tree's context menus), ask the
  original's confirmation questions, run as `Async.*` tasks with progress
  in the Events panel, and report XAPI errors with the original's friendly
  messages (`FriendlyErrorNames`). The server submenus check each server
  with `VM.assert_can_boot_here`, and maintenance mode lists the VMs that
  cannot be migrated (`host.get_vms_which_prevent_evacuation`);
- **RBAC**: the session's rights (and, for an Active Directory user, who
  they are) are read after login, each operation is
  checked against them before it runs, and a user whose role is not enough
  is offered the role elevation dialog (another user's credentials, checked
  and used for that operation only);
- the **Console tab** of a VM and of a server (its control domain's text
  console), as the original's: the guest's screen over VNC through XAPI's
  console tunnel (`CONNECT` to `console.location` with the session cookie,
  over TLS pinned to the host's certificate; a pool member's certificate is
  accepted when the pool lists it), **Scale** (fit, keeping the aspect
  ratio) or one screen pixel per guest pixel, **Undock** to its own window
  (Alt+Shift+U), **Fullscreen** (Ctrl+Alt+F), **Send Ctrl+Alt+Del** (also
  Ctrl+Alt+Insert), clipboard text both ways (RFB carries Latin-1), the
  guest's cursor shape, the mouse mapped to the guest pixel under it at
  any scale (wheel included), and Right Ctrl to release the keyboard.
  Keys go as **XT scancodes** of their physical position (QEMU Extended Key
  Event), so the guest's own layout (US, ABNT2…) decides the characters;
  consoles without it (PV guests, dom0) get keysyms. The console
  **reconnects by itself** after a reboot, a toolstack restart or a dropped
  link while the VM runs, shows the original's messages when it is shut
  down, suspended or paused (with "Click here to start it" / "resume it"),
  and the window **switches to the Console tab** when a VM started or
  resumed from it begins to run;
- the **Performance tab** of a server and of a VM, as the original's: the
  default graphs (CPU per CPU or vCPU, used memory, network send/receive
  per NIC or interface, and disk read/write per disk for VMs) drawn from
  XAPI's round-robin databases, with a key beside each graph, the
  original's data source names and units, and the VM lifecycle events
  (started, shut down…) as markers. **Zoom** shows the last 10 minutes,
  1 hour, 2 hours, 1 day, 1 week, 1 month or 1 year; as in the original
  the range picks the archive (5-second rows for 10 minutes, minute rows
  for 2 hours, hourly rows for a week, daily rows for a year) and the
  newest finer rows are averaged in so the line reaches the present. The
  whole RRD is read when the tab opens (`/host_rrds`, `/vm_rrds`), then
  `/rrd_updates` every 5 seconds (and every minute, hour and day for the
  coarser archives), asking for **JSON** (`json=true`, which XCP-ng 8.3
  serves) and reading XML when a server answers in XML. A VM that is not
  running shows its archived data;
- **alerts** in the notifications area (Alerts next to Events): the XAPI
  messages of every connected server, newest first, with severity,
  title and description (perfmon alarms and failed logins in words, other
  messages with the original's friendly texts), server or pool and date;
  **Dismiss**, **Dismiss Selected** and **Dismiss All** ask first and
  destroy the messages on the server (with the RBAC check and role
  elevation of other operations). VM lifecycle messages are not alerts
  (they go on the graphs), as in the original;
- **performance alert settings** (the original's "Alerts" properties
  page) for a VM (CPU, network, disk), a server (CPU, network, free
  memory) or an SR (storage throughput), from the Performance tab, an
  SR's header or an alarm's "Alarm Settings": thresholds, durations and
  the repeat interval, saved to `other_config["perfmon"]` in the
  original's XML (other perfmon variables are kept), followed by the
  perfmon plugin's `refresh` on the hosts concerned;
- the **New VM wizard**, as the original's: Template (custom templates,
  built-in ones and snapshots, with a search box), Name, Installation
  Media (an ISO from the ISO libraries, or the network) with the **boot
  mode** (BIOS, UEFI, UEFI Secure Boot, as the template supports) and an
  optional **vTPM**, Home Server, CPU & Memory (vCPUs, topology, memory),
  Storage (the template's disks, sizes and SRs, extra disks, fast clone
  or full copy of a custom template), Networking and Finish (start the VM
  automatically). It runs like `CreateVMAction`: the template is cloned
  (or copied) under a hidden name, provisioned with its disk instructions
  removed, then named and given its settings, DVD drive and ISO, disks
  (each owned by the VM), interfaces and vTPM, and started; a failure
  before the interfaces are made removes the half-made VM and its disks.
  The new VM is selected once made;
- **Copy VM / Copy Template** (fast clone or full copy to an SR),
  **Convert to Template**, **Delete VM / Delete Template** with the
  original's dialog (the disks only this VM uses, checked when its drives
  own them, and its snapshots), **New VM from Snapshot** and **Create
  Template from Snapshot**;
- the **Snapshots tab** of a VM: Take Snapshot (disks only, or disks and
  memory of a running VM), Revert To (optionally taking a snapshot of the
  current state first; a VM running at the snapshot is started or
  resumed again), the Actions menu and Delete (several at once; the
  snapshots' own disks go with them);
- on a VM's **Storage tab**: its **DVD drives** (load an ISO, Eject, or
  create a drive when it has none) and its disks with **Add**, **Attach**
  (an existing disk, optionally read-only), **Properties** (resize; a
  disk cannot shrink), **Detach**, **Move** (to another SR: storage live
  migration while a running VM uses it, else copy and re-attach) and
  **Delete**, with the original's confirmations; a disk or drive that
  cannot be plugged into a running VM says it needs a restart;
- **Install XCP-ng VM Tools**: the guest tools ISO into the VM's DVD
  drive (the original's leftover `InstallPVToolsAction`; XCP-ng Center
  25.04 offers no such menu on XCP-ng 8.x);
- the **New SR wizard**, as the original's: Type, Name (with the
  original's automatic description) and Location for **NFS** (Scan for
  existing SRs, NFS version, create or reattach one), **iSCSI** (CHAP,
  Scan Target Host, the target IQN and LUN lists, then the original's
  choice to reattach the SR found on the LUN or format it), **SMB**
  (credentials, Scan, create or reattach), **local storage** (ext, LVM or
  XFS on a device of the coordinator), and the **NFS** and **SMB/CIFS ISO
  libraries** (an already attached library is refused). It runs like
  `SrCreateAction` (passwords through a `secret` destroyed afterwards; a
  PBD that will not plug forgets the SR again; the pool's first shared
  SR becomes its default), `SrIntroduceAction` (an SR the scan found) or
  `SrReattachAction` (a detached SR: the wizard opens as "Attach Storage
  Repository"; a failed reattach removes the PBDs it made). Hardware HBA,
  software FCoE, GFS2 and the SR types only XCP-ng ships are not offered;
- the **Storage menu** and an SR's context menu, offered as the
  original's commands offer them: **Repair** (a dialog with each host's
  connection; missing PBDs made, unplugged ones plugged), **Set as
  Default**, **Reclaim freed space** (the `trim` plugin), **Detach**,
  **Reattach**, **Forget**, **Destroy** (with the original's
  confirmations) and **Properties** (name, description, tags, the SR's
  performance alerts and read caching); **New SR** on the toolbar and the
  pool and host menus; **Rescan** on an SR's Storage tab;
- the **New Network wizard** (Add Network on a host's or the pool's
  Networking tab), as the original's: an **external network** (a VLAN on
  a NIC or bond of every host, with the original's NIC, VLAN tag and MTU
  checks), a **single-server private network**, a **cross-server private
  network** (a tunnel over each host's management interface; as the
  original, not shown on XCP-ng 8.2 and later, and on older servers
  offered only with a vSwitch Controller) and a **bonded network** (2 to 4 NICs,
  active-active, active-passive or LACP with its load balancing; the
  original's warnings before moving a management or secondary interface
  onto the bond). It runs like `NetworkAction`, `CreateChinAction` and
  `CreateBondAction` (bonds on every host, the coordinator last); unlike
  the original, a network whose VLAN or tunnels cannot be made is
  removed again, and a bond that fails on one host is undone on every
  host. SR-IOV networks are not offered;
- a host's or the pool's **Networking tab** with **Properties** (name,
  description, tags, automatically add to new VMs, MTU, the NIC and VLAN
  of a VLAN or private network, a bond's mode; a new MTU replugs the
  network) and **Remove Network** (a bond's network is deleted as a
  bond), and **Configure** for the **IP addresses**: the management
  interface and secondary interfaces of a host, or of every host of a
  pool (an address range), DHCP or static, added, renamed, moved to
  another network or removed, with the original's warning before the
  management interface changes (`ChangeNetworkingAction`; a pool moves
  its management interface with `pool.management_reconfigure`);
- the **NICs tab**: **Create Bond**, **Delete Bond** (with the original's
  warnings) and **Rescan** (`PIF.scan`);
- a VM's **Networking tab**: **Add Interface** (network, MAC address
  generated or typed, with the original's duplicate MAC question, and a
  QoS limit), **Properties** (the interface made again with the new
  settings), **Remove Interface** (confirmed) and **Activate** /
  **Deactivate**; a new interface of a running VM is hot-plugged when the
  server allows it, else the user is told to restart the VM;
- **pools** (the original's Pool menu and the server menu's pool
  entries): **New Pool** (name, description, coordinator, and every
  connected server listed with the reason it can or cannot join, as
  `PoolJoinRules`), **Add Server** / **Add to Pool** (the servers that
  cannot join are listed with why; the move is confirmed; the licensing,
  CPU and Active Directory questions; **Add New Server** connects first),
  **Remove Server** / **Remove From Pool** (confirmed; the removed server
  is connected to again once it answers), **Make into standalone server**,
  and in **Enter Maintenance Mode** on a pool's coordinator the choice of
  the **new coordinator** (`pool.designate_new_master`; the connection
  follows it). A joined server leaves the console's own connections. When
  no member could join, New Pool is undone (the original leaves the named
  pool behind);
- the **pool and server Properties** dialogs with the pages the original
  shows for the object and server version: General (name, description,
  tags, a server's iSCSI IQN), Alerts, Email Notifications, Multipathing
  (maintenance mode only; PBDs replugged), Log Destination (remote
  syslog), Power On (disabled, Wake-on-LAN, DRAC, custom script; passwords
  in secrets), Auto-start on server boot, Live Patching (before XCP-ng
  8.3), Network (IGMP snooping), Clustering, Advanced (migration
  compression) and NRPE (read with the `nrpe` plugin), each with the
  original's checks; OK saves only what changed;
- the server menu's **Control Domain Memory** (maintenance mode; the
  server reboots), **Password → Change Server Password** (the current
  password checked; later logins use the new one), **Certificate →
  Install / Reset**, **Back Up** and **Restore From Backup** (files streamed
  through `GET /host_backup` and `PUT /host_restore`; paths are typed, there
  is no native file dialog), and the Pool menu's **Rotate Pool Secret** and
  **Enable Certificate Verification**;
- **high availability** (the Pool menu's **High Availability**, the pool's
  **HA** tab): the **Configure HA** wizard (prerequisites; a blocking RBAC
  page for users who may not configure HA; the **heartbeat SR**, each
  shared SR checked with `SR.assert_can_host_ha_statefile`; each VM's
  **restart priority**, **start order** and **delay**, with its agility
  from `VM.assert_agile` and the original's proposals; the **number of
  server failures to tolerate**, its maximum computed by the server for
  the chosen priorities and capped by the pool's HCI limit; the summary),
  the **HA settings** dialog once HA is on, **Disable HA** (confirmed), and
  the HA tab (configured and current failure capacity, heartbeating per
  server and heartbeat SR). Other operations ask what the original asks:
  maintenance mode, reboot and shut down of a server lower the tolerance
  first (after a question), it can be raised again once a server leaves
  maintenance mode, a VM start that breaks the failover plan offers a
  lower tolerance, a protected VM that is not agile has its priority
  cleared before it starts; and HA blocks what the original blocks;
- **Active Directory** (the **Users** tab of a pool or standalone server):
  **Join Domain** / **Leave Domain** (with the domain account, or
  "Ignore"), **Add** users and groups (each name resolved, then given
  access, with its progress), **Remove**, **Change Role** (one role;
  logged-in users whose role changes are logged out), **Logout User**,
  who is logged in (every 5 seconds), and the original's questions,
  including removing or logging out oneself. **Add Server** and **New
  Pool** join a server to the pool's domain first, asking for a domain
  account (asked again when refused);
- **XVA export and import**, as the original's wizards: **Export...** (VM
  menu, and the context menus of VMs, servers and pools) opens the Export
  wizard for the XVA format (file name and folder, checked; a blocking
  RBAC page for restricted users; the halted or suspended VM to export,
  with the folder's free space checked; **Verify export on completion**);
  templates and snapshots have **Export to File...** / **Export Snapshot
  As Template...** (file, verify, a question when space may be short).
  The archive streams from `GET /export` into `<file>.tmp`, checked as it
  arrives (tar headers, every block's SHA-1 or XXH64 checksum, the end of
  the archive), then renamed; a cancelled or failed export removes it,
  with the original's messages (checksum mismatch, disk full, disk in use
  elsewhere). **Import...** (File menu, and the context menus of servers
  and pools) opens the Import wizard: the `.xva` (an `.xva.gz` is
  uncompressed next to it first), the home server or pool, the RBAC page,
  the SR, then **Import >** streams the file to `PUT /import` with its
  progress and Cancel; once the server made the VM the wizard shows its
  interfaces (MAC, network, add, delete), then the summary and **Start
  the new VM(s) automatically**. The imported VM gets a free name and its
  home server; interfaces are moved, created or removed as asked, and
  networks the import made but left unused are destroyed. A cancelled or
  failed transfer leaves no VM or disk behind. Files are never held in
  memory (32 KiB at a time). Progress and Cancel are in the Events panel;
- an About window and an Options window;
- the complete XenAPI surface, generated from the upstream datamodel: every
  class, record, enum and call, with `Async.*` variants (`crates/xapi-codegen`);
- a client-side cache of every class the server delivers through events,
  loaded after an `event.inject` token so nothing is lost between the load
  and the first `event.from`.

The original 25.04 still has the HA and Users pages but no longer adds them
to the tab list; they are shown as the earlier XenCenter showed them (HA on
a pool, Users on a pool or a live standalone server). Editing from the tabs
(VM properties, so also a VM's own HA page), custom graphs
(New/Edit Graph), OVF/OVA packages and disk images (the Export wizard
offers the XVA format only, and the Import wizard refuses those files
for now) and cross-pool migration come in later phases (RDP is not
embedded). The New VM wizard leaves out the pages the
original shows only for Citrix features (vGPU, cloud-config, LUN per VDI,
copying BIOS strings). See the port plan for the full roadmap.

Console limitations: shortcuts the operating system keeps (Alt+Tab, the
Windows key, Ctrl+Alt+Del) never reach the console, as in the original
without its Windows keyboard hook; use the buttons and Ctrl+Alt+Insert.
egui reports numpad keys as their main-keyboard twins (same characters,
other scancodes), and gives no key event for numpad `*` and `.` on some
platforms. A VNC password (a guest's own VNC server) is not supported: XAPI
consoles never ask for one.

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
and connect to the address it prints (user `root`, password `mock-password`;
users with one RBAC role each, such as `vm-operator-user` /
`vm-operator-password`, are listed in `crates/xapi-mock/src/ops.rs`):

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
templates, a snapshot, local, NFS and ISO storage with a broken SR, the
storage types' SM plugins, networks with a VLAN, NICs and metrics), and `xcp-ng-8.3-capture.json`, a sanitized read-only capture of a real
XCP-ng 8.3 host covering every class it delivers. The tests use it to check,
end to end:

- login (including bad passwords and certificate pinning);
- that every real record decodes into the generated types and re-encodes
  without losing a field;
- the generated calls (`get_all_records`, `get_record`, `get_by_uuid`, field
  getters, `event.inject`, optional parameters) for any class;
- every VM and host operation of the console (power state, placement and
  `allowed_operations` changes, host disable/evacuate/reboot/shutdown/power
  on, toolstack restart), injected failures, and RBAC users whose calls are
  checked against their role;
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
- VM and host operations as tasks with progress, cancel and `task.destroy`
  (forced operations cancelling what still runs on the VM, hosts enabled
  again after a failed reboot or evacuation), and other clients' tasks
  followed and cancelled;
- RBAC: the session's rights, the roles that could run an operation, a
  server-side `RBAC_PERMISSION_DENIED`, and role elevation (wrong password,
  a user without the role, a user with it);
- logs at `trace` level free of passwords and session references;
- the tree (homes, storage, templates, snapshots, view options, filter) and
  every tab's data against the hand-written pool and the real-host capture;
- the egui window itself, run headlessly with `egui_kittest`: connect dialog,
  certificate prompt, tree, View options and filter, the detail tabs and
  their live updates, Events panel (progress, cancel, other clients' tasks),
  context menus offering only what can run, confirmations, the Migrate and
  Start on Server submenus, the maintenance mode dialog, host operations,
  the role elevation dialog and friendly error messages,
  the reconnection shown after a server restart, servers restored on start,
  and Options;
- the console: the RFB handshake (3.3 and 3.8), every decoder (Raw,
  CopyRect, RRE, CoRRE, Hextile, ZRLE over one zlib stream, the Cursor,
  DesktopSize and QEMU Extended Key pseudo-encodings), key events as
  scancodes and as keysyms, Ctrl+Alt+Del, pointer, clipboard, bell, desktop
  resize and a dropped session, against a fake guest console
  (`xen_vnc::testing::MockConsole`, served by the mock XAPI behind
  `CONNECT /console` with the session cookie checked); the scancode of every
  key position the US and ABNT2 layouts use, checked against the Linux
  keymaps of those layouts; the backend console reconnecting after a reboot
  and a toolstack restart, waiting while the VM is halted, and the dom0
  console in keysym mode; and in the window: the Console tab, keys (US and
  ABNT2 positions, Tab and arrows kept from egui), the mouse landing on the
  guest pixel under it while scaled, Ctrl+Alt+Del, paste, undock, full
  screen, reconnection after a reboot, the switch to the Console tab on
  Start, and the server console sending nothing;
- performance data: `rrd_updates` and whole RRDs in JSON and XML, read
  from sanitized answers of a real XCP-ng 8.3 host
  (`tests/fixtures/rrd/`) and served by the mock (made-up values that
  move with the clock); the archives, the finer rows averaged in, a VM
  that stops (its graphs kept) and starts again, a server without JSON,
  data sources listed without units, the requests stopping when the tab
  closes, and the session reference never reaching the log;
- alerts from messages (perfmon alarms, failed logins, friendly titles),
  dismissing them (one already gone is fine), the RBAC check, and the
  performance alert settings (units, other variables kept, the key
  removed when nothing is on, the perfmon refresh); in the window: the
  Performance tab's graphs, zoom and feeds, the Alerts view with its
  questions, and the settings window;
- VM creation, templates, snapshots and disks against the mock, which
  implements `VM.clone` / `copy` / `provision` / `snapshot` /
  `checkpoint` / `revert` / `destroy`, the `VDI`, `VBD`, `VIF` and `VTPM`
  calls, field setters and map calls, and refuses empty enum values in
  records as XAPI does: a new VM from a built-in template (ISO, disks,
  networks, UEFI Secure Boot, vTPM, start) and the exact call order, a
  network boot, the rollback of a half-made VM, new VMs from a snapshot
  and from a custom template copied to an SR, copy and fast clone,
  templates, deletion with chosen disks and snapshots, snapshots with and
  without memory, revert (restarting or resuming the VM), deleting
  snapshots with their disks, every disk operation (new, attach, detach,
  resize, move halted and live, delete, a full SR), DVD drives, ISOs and
  the tools ISO, and RBAC; in the window: the New VM wizard end to end,
  the Snapshots tab and its dialogs, Copy / Convert / Delete from the
  menus, the Storage tab's disk buttons and DVD drives, Install Tools,
  and role elevation;
- storage repositories against the mock's simulated storage (NFS, SMB
  and iSCSI targets with SRs on them, CHAP, local devices, ISO shares):
  the probes and the iSCSI IQN and LUN scans (and the answers the client
  cannot use), creating every SR type (passwords as secrets, the first
  shared SR as default), introducing SRs the scan found, reattaching a
  detached SR, the rollbacks of a create whose PBD does not plug and of a
  failed reattach, repair (plugging and re-creating PBDs), detach,
  forget, destroy, set as default, rescan, trim (supported, unsupported,
  detached), properties and RBAC; in the window: the wizard for every
  type with its scans, LUN choice, warnings and errors, the Storage menu
  and context menu with their confirmations, the Repair and Properties
  dialogs, Rescan, and role elevation;
- networking against the mock's networks, VLANs, bonds, tunnels and PIF
  configuration (`xapi_mock::networking`, with
  `networking::bond_lab_records` adding NICs for bonds): every network
  type, the rollbacks of a half-made VLAN network, cross-server network
  and bond, bonds over management and secondary interfaces, removing
  networks and bonds, the network properties (NIC and VLAN changes, MTU
  replug, bond mode), NIC rescans, IP configuration of one host and of a
  pool (secondary interfaces, the management interface's address and its
  move) with failures, a VM's interfaces on halted, running and
  driverless VMs, and RBAC; in the window: a VM's Networking tab, the
  wizard for every type with its checks, the network Properties and
  Remove Network, the NICs tab (Create Bond, Delete Bond, Rescan), the
  Configure IP Addresses dialog, and role elevation;
- pools against two mock servers in one process (`MockServer::start_lab`
  with `xapi_mock::lab_records`, standalone XCP-ng 8.3 servers that can
  join each other's pool, `xapi_mock::pool`): New Pool, Remove Server and
  the server connected again, Add Server, the coordinator handing over the
  pool; failed joins (refused by the joining server, by the coordinator,
  shared storage) that change nothing, a failed New Pool undone; the join
  rules; every Properties page saved (and failures: a power-on secret
  destroyed again, PBDs replugged after a multipath failure); Control
  Domain Memory, Change Server Password, Rotate Pool Secret, certificate
  verification, Install / Reset Certificate, Back Up / Restore; RBAC; in
  the window: the Pool menu, the New Pool dialog, Remove and Add Server,
  the new coordinator choice, every Properties page with its checks, and
  the server dialogs;
- high availability and Active Directory against the mock
  (`xapi_mock::ha`, `xapi_mock::ad`: a planner, statefiles, agility, a
  simulated domain `lab.example` with users and groups, domain logins with
  their roles): HA turned on and off on the fixture's 2-host pool, its
  settings changed, the plan refusing a VM start and a server leaving and
  the lowered or raised tolerance, failures (SR not shared, the HA daemon,
  a missing statefile disk, a non-agile VM, a tolerance above the plan);
  joining and leaving the domain (wrong account, unknown domain), users
  and groups added, roles changed, sessions logged out and access removed,
  a domain user with the VM Operator role offered role elevation, and Add
  Server joining the domain first; in the window: the wizard, the HA tab,
  the settings dialog and Disable HA, the questions of other operations,
  the Users tab and its dialogs, and role elevation for a domain VM
  Operator;
- XVA export and import against the mock (`xapi_mock::xva`: streamed
  `GET /export` and `PUT /import` that answers before reading the body as
  XAPI does, task progress, the VM's and disks' objects made from
  `ova.xml`, every block's checksum checked): a VM and a template exported
  and imported back with a 4 KiB buffer (the disk's content checked, the
  name, home server and interfaces as the wizard asked, start after
  import); cancelled transfers, a server error, a disk in use, a full disk,
  a file that ends early, a dropped link and a wrong block checksum, each
  leaving no file and no VM (also when the server does not clean up); the
  upload read no more than one buffer at a time; in the window: the Export
  wizard and its checks, Export to File of a template, the Import wizard
  from an `.xva.gz`, and a transfer cancelled from the wizard;
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

The console on a real host, through the real window (same variables, plus
`XENADMIN_REAL_LOG=<file>` and `XENADMIN_REAL_SHOTS=<folder>` for the log and
framebuffer snapshots, both outside the repository):

- `real_console_dom0` opens the server's console view-only (it sends no key,
  mouse, Ctrl+Alt+Del or clipboard input, and checks so);
- `real_console_fluxer` acts on the VM named exactly "09 - Ubuntu Fluxer"
  only (refused unless exactly one VM has that name and it is halted): it
  starts it from the toolbar, checks the switch to the Console tab, watches
  the boot, types a test text at the login prompt without Enter and erases
  it, moves the mouse, reboots it (the console must reconnect) and shuts it
  down, leaving it halted.

```sh
cargo test -p xenadmin real_console_dom0 -- --ignored --nocapture
```

`real_performance_host` (same variables plus `XENADMIN_REAL_LOG`) only
reads: it follows the server's Performance tab for a minute.

The performance graphs, alerts and alert settings on a real host
(`real_performance_fluxer`, same variables plus `XENADMIN_REAL_LOG`): it
reads the server's graphs, then acts on the VM named exactly "09 - Ubuntu
Fluxer" only (refused unless exactly one VM has that name and it is
halted): starts it and watches its graphs move during the boot, creates a
test message on it and dismisses that one alert alone from the Alerts
view (checking every other message is still there), turns its CPU alert
on and off again (the `perfmon` key ends as found; the perfmon refresh is
not called), and shuts it down.

VM creation, snapshots and disks on a real host
(`app/tests/provisioning_real.rs`, same variables plus `XENADMIN_REAL_LOG`
and `XENADMIN_REAL_SHOTS`):

- `real_f7_fluxer` acts on the VM named exactly "09 - Ubuntu Fluxer" only
  (refused unless exactly one VM has that name and it is halted): it
  takes a snapshot, loads an ISO into its DVD drive and ejects it,
  reverts to the snapshot it took and deletes it, leaving the VM halted
  with no new snapshot or disk;
- `real_f7_test_vm_create` makes one disposable test VM
  (`xenadmin-rs-test-f7`, refused if one exists) with the New VM wizard,
  its disk on the SR named "Local storage"; `real_f7_console_driver`
  types into that VM's console from command files
  (`XENADMIN_REAL_DRIVER`); `real_f7_test_vm_operations` runs the disk,
  DVD, tools, snapshot, copy and template operations on it and then
  deletes everything named after it.

Storage repositories on a real host, read only
(`app/tests/storage_real.rs`, same variables plus `XENADMIN_REAL_LOG`):
`real_f8_srs_read_only` draws every SR's General and Storage tabs, its
Storage menu (only to read what it offers) and its Properties dialog
(cancelled), checks them against the cache, opens the New SR wizard's
type page and cancels; it never presses an operation's button, and the
history must hold the connection only at the end.

Networking on a real host (`app/tests/network_real.rs`, same variables
plus `XENADMIN_REAL_LOG`): `real_f9_networking` checks the pool's and the
host's Networking tabs and the NICs tab against the cache, opens each
network's Properties, the Configure IP Addresses dialog, the Create Bond
dialog and the New Network wizard and cancels them (no network, VLAN,
bond, tunnel or PIF is ever changed); then, on the halted VM named
exactly "09 - Ubuntu Fluxer" only, it adds an interface on the network
of its first interface, changes its QoS limit and removes it, and checks
the VM ends with exactly the interfaces it had. `f9_real_run_on_the_mock`
rehearses the same run on the capture.

Pools and server settings on a real host, strictly read only
(`app/tests/pool_real.rs`, same variables plus `XENADMIN_REAL_LOG`):
`real_f10a_properties_read_only` opens the pool and server Properties
dialogs (every page; NRPE, the cluster network and power-on passwords are
read as the dialogs do), compares their values with the cache and
cancels; opens and cancels New Pool, Change Server Password, Control
Domain Memory, Install Certificate, Back Up and Restore. No operation
runs. `f10a_real_run_on_the_mock` rehearses it on the capture and checks
every call made is a read.

High availability and users on a real host, strictly read only
(`app/tests/ha_real.rs`, same variables plus `XENADMIN_REAL_LOG`):
`real_f10b_ha_and_users_read_only` checks the pool's HA tab and Pool menu
against the cache, opens the Configure HA wizard as far as its priorities
page (the heartbeat SR, agility and planner checks change nothing) and
cancels it, checks the Users tab and opens and cancels the Join (or Leave)
Domain prompt. No operation runs. `f10b_real_run_on_the_mock` rehearses it
on the capture and checks every call made is a read or one of those checks.

XVA on a real host (`app/tests/xva_real.rs`, same variables plus
`XENADMIN_REAL_LOG` and `XENADMIN_REAL_XVA_DIR`, a folder outside the
repository): `real_f11a_xva_round_trip` makes one disposable VM
(`xenadmin-rs-test-f11a`, a 100 MiB disk on "Local storage", never
started, refused if one exists), exports it with the Export wizard into
that folder, deletes it, imports it back with the Import wizard, checks
it and deletes it. Nothing else is acted on. `f11a_real_run_on_the_mock`
rehearses it on the lab pool and checks no other object changes and only
the calls it needs are made.

A view-only snapshot of a console from the command line (no input is ever
sent): `XAPI_HOST=… XAPI_USER=root XAPI_PASSWORD=… XAPI_FINGERPRINT=AA:BB:…
CONSOLE_HOST=1 CONSOLE_OUT=/tmp/dom0.ppm cargo run -p xen-core --example
console_snapshot` (or `CONSOLE_VM=<exact name>` for a VM).

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
| `crates/xapi` | XAPI client. Its hand-written core covers the transport, session, `event.from`, errors, dates, lenient decoding, TLS certificate pinning and the streamed file transfers of the HTTP handlers (`http`: downloads read piece by piece; uploads written by hand on their own connection, read from the file one buffer at a time while the server's answer is read, the connection closed at once on any failure or cancel). `src/generated/` holds every API class, record, enum and call, generated from the pinned datamodel. |
| `crates/xapi-codegen` | The generator for `crates/xapi/src/generated/`, and the pinned datamodel in `data/`. |
| `crates/xen-core` | UI-independent backend: the connection manager and per-connection actors (login, TOFU, redirect, heartbeat, reconnection), the live object cache (one map per class), its tree and the facts derived from it (VM home, SR state, host memory, NIC and network names), the VM and host operations (when each is offered, how each runs as XAPI tasks), VM creation, templates, snapshots and disks (`provisioning`, `provision_run`), storage (`storage`, `storage_run`), networking (`network`, `network_run`), pools and pool and server settings (`pool`, `properties`, `pool_run`), high availability and Active Directory (`ha`, `ad`, `ha_run`), XVA files and the import and export wizards' rules (`xva`: the streaming verifier, XXH64, `ova.xml`, `.gz`) and the transfers (`xva_run`), the consoles (finding a VM's console, opening it, reconnecting), the performance feeds and default graphs, alerts and the perfmon settings, the RBAC check, the operation history, settings, known servers and the keyring store. |
| `crates/xen-rrd` | XAPI performance data: the `rrd_updates`, `host_rrds` and `vm_rrds` client (JSON or XML, the session reference kept out of logs and errors), their parsers, and the four archives the graphs draw from. |
| `crates/xen-vnc` | VNC client for XAPI consoles: the `CONNECT` tunnel, the RFB protocol and decoders, the framebuffer, XT scancodes, and a fake guest console for tests (feature `testing`). Its ZRLE decoder derives from [vnc-rs](https://github.com/HsuJv/vnc-rs) 0.6.1 (MIT, see `crates/xen-vnc/LICENSE-vnc-rs`). |
| `crates/xenadmin` | The egui/eframe desktop app. |
| `crates/xapi-mock` | Test-only mock XAPI server. |

`xapi`, `xen-rrd`, `xen-vnc` and `xen-core` do not depend on egui. The app runs a tokio runtime on
background threads and keeps eframe on the main thread. It sends `Command`s to
the backend over an mpsc channel and reads an immutable `Arc<AppState>`
snapshot each frame (every connection, the operation history and the
settings). The backend calls `request_repaint` after each batch of events,
publishing at most about 10 snapshots per second per connection.

## License

BSD-2-Clause; see [LICENSE](LICENSE). Credits go to the XenAdmin (Citrix
XenCenter) and XCP-ng Center projects this work derives from. The ZRLE
decoder and zlib reader in `crates/xen-vnc` derive from vnc-rs 0.6.1 and are
used under its MIT license (`crates/xen-vnc/LICENSE-vnc-rs`).
