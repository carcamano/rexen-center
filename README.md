# xenadmin-rs

A cross-platform (Windows, Linux, macOS) management console for XCP-ng pools,
written in Rust with [egui](https://github.com/emilk/egui). It is a rewrite of
XCP-ng Center / XenAdmin 25.04, whose C# source in `../original-source/` is kept
untouched as the reference.

`xenadmin-rs` is the product name for now. It is defined once, in
`crates/xenadmin/src/branding.rs`.

**Status: phase F15 (an MCP server, `xenadmin --mcp`, for AI assistants;
see [MCP server](#mcp-server)) on top of F1–F14b (light and dark themes,
the message catalogue for translations, native file dialogs, the home page
and release packages, base, connection and model, detail tabs, VM and host
operations, VNC console, performance graphs and alerts, VM creation,
templates, snapshots and disks, storage repositories, networking, pools
and pool and server properties, high availability, Active Directory and
RBAC, XVA export and import, OVF/OVA packages and disk images, cross-pool
migration and copy, Move VM, vApps, scheduled snapshots and disaster
recovery, search, navigation views, folders, tags and custom fields, GPUs
and vGPUs, USB pass-through, vTPMs, memory settings, external SSH and
Remote Desktop consoles).** What works:

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
- **OVF/OVA packages and disk images**, as the original's wizards. The
  Export wizard's default format is **OVF/OVA**: several VMs, an EULAs
  page (text files, by path), **Advanced Options** (create a manifest,
  create an OVA, compress; signing is shown but not available) and the
  summary with **Verify export on completion**. Each disk comes from `GET
  /export_raw_vdi?format=vhd` into `<folder>/<name>/<vdi uuid>.vhd`
  (gzipped with Compress), hashed on the way; then `<name>.ovf` (the
  original's descriptor: CPU, memory, interfaces, disks, DVD drive and the
  Xen settings) and the SHA-256 manifest `<name>.mf`. An OVA is written in
  one pass (descriptor, disks, manifest last). The Import wizard takes an
  `.ovf`, `.ova` (`.ova.gz` uncompressed first) or a disk image (`.vhd`,
  `.img`/`.raw`, `.iso`): a package shows its EULAs, the location, the
  RBAC page, Storage (one SR for all disks, or one per disk), Networking,
  **Security** (verify the manifest, or the signature against the
  certificate's key) and the summary; an image shows VM Definition (name,
  CPUs, memory), the location, Storage, Networking and **Boot options**
  (BIOS, UEFI, UEFI Secure Boot). The import checks the package first,
  then makes each VM with `VM.create`, its interfaces, an empty DVD drive
  and each disk (`VDI.create`, `PUT /import_raw_vdi`: a dynamic VHD as
  VHD, a fixed VHD, raw image or ISO as raw bytes, `VBD.create`). VMDK,
  VHDX, qcow2 and WIM disks are refused with a clear word: nothing is
  converted. A cancelled or failed export leaves no file, a cancelled or
  failed import no VM or disk. An imported package becomes a **vApp**
  named after it, its VMs with the package's start order and delays;
- **Migrate, Move and Copy VM** (the original's `CrossPoolMigrateWizard`):
  "Migrate VM wizard..." at the end of the Migrate to Server submenu, Move
  VM... (Migrate VM... for a suspended VM) and Copy VM... / Copy... of a
  halted VM or template that may migrate (`migrate_send` offered). Pages:
  Destination (each connected pool or standalone server and, per VM, its
  servers; each asked about as the original does, `host.migrate_receive`
  on the destination and `VM.assert_can_migrate` on the source, refused
  ones shown with the reason: current server, older server, the server's
  error), Permissions (blocking, for a restricted role on either side),
  Storage (all disks on one SR, or an SR per disk, with the free space
  checks), Networking (to another pool, a network per interface),
  Migration Network and Finish (the summary). Finish runs `VM.migrate_send`
  (live, `copy=true` for a copy) to another pool or with storage motion,
  `VM.pool_migrate` within the pool, or the Move action (each disk copied
  to its new SR) for a halted VM within the pool. Copy mode first chooses
  a copy within the pool (Name and Storage: fast clone or full copy) or to
  another pool. A VM that may not migrate gets the F7 Copy dialog or the
  Move VM dialog (an SR for all disks). Progress and Cancel are in the
  Events panel;
- **vApps**: Manage vApps... (Pool menu: the pool's vApps and their VMs;
  New vApp..., Delete, Properties, Start, Shut down, Export, Import), the
  New vApp wizard (Name, Virtual Machines, VM Startup Sequence with start
  order and delay, Finish), vApp Properties (General, Virtual Machines,
  VM Startup Sequence), and in the VM menus Start vApp, Shut Down vApp
  (asked first) and Assign to vApp (New vApp... and each vApp; assigning a
  VM of another vApp is asked first);
- **scheduled snapshots** (VMSS): VM Snapshot Schedules... (Pool menu: the
  pool's schedules with their status, number of VMs, next snapshot time
  here and on the server, last result; New..., Properties, Enable /
  Disable, Run Now, Delete asked first, and the selected schedule's run
  history, a failure listing the VMs that failed and why), the New
  Snapshot Schedule wizard (Name, Virtual Machines, Snapshot Type: disks
  only or disks and memory, Snapshot Schedule: hourly at a quarter, daily
  or weekly at a time with the days, the number of snapshots kept and the
  next run here and on the server; Finish with "run now"); a schedule
  without VMs is made disabled. Properties (General, Virtual Machines,
  Snapshot Type, Snapshot Schedule), Assign to Snapshot Schedule in the
  VM menus, and the VM's Snapshots tab says which schedule it is in. The
  schedule is stored in the server's local time, rounded to a quarter of
  an hour, as the original does;
- **disaster recovery**: Pool > Disaster Recovery > Configure... (the SRs
  that can hold the pool's metadata, at most 8) and the Disaster Recovery
  wizard (Failover, Failback, Test Failover): the SRs with VM
  configuration data, also found on an iSCSI target or the HBAs and
  introduced with a `DR_task`; the vApps and VMs of each pool found, with
  the power state after the recovery; the pre-checks (HA off for a test
  failover, nothing running in the source pool when connected, VMs and
  vApps already here, the SRs the recovery needs) with Resolve, Resolve
  All and Check Again; the recovery one vApp or VM at a time, then the
  start; the clean-up (the `DR_task`s, a test failover's VMs, vApps and
  SRs, what the fixes changed) and the summary report. Cancelling cleans
  up too;
- **navigation views** (the buttons under the tree, as the original's
  navigation pane): Infrastructure, Objects (by type), Organization Views
  (Objects by Tag, by Folder, by Custom Field, vApps) and Saved Searches
  (the seven built-in searches and those saved on the pools), each with
  its tree; the tree's search box narrows them. Networks, virtual disks,
  vApps and folders can be selected there; a folder, a group or a view
  shows the Search tab;
- the **Search tab** (pools and servers, and the views): what to search
  for, the filter tree (all of / any of / none of, name, description,
  UUID, tags, pool, server, memory, address, start time, power state,
  tools, OS, HA restart priority, read caching, Windows Update, size,
  shared, HA, partially upgraded, in a pool or standalone, network,
  vApp, parent or ancestor folder, not in a folder, custom fields), the
  grouping levels, and the results grid with its columns (Name, CPU Usage,
  Used Memory, Disks, Network, Address, HA, Uptime, one per custom field;
  the figures read from `rrd_updates` every 30 seconds), sorting,
  collapsible groups and a column chooser; Save... (on a pool's
  `gui_config`, as XenCenter), Export... and Import... of `.xensearch`
  files. The original's search XML is read and written for the filter
  and grouping classes its builder makes; others are kept but not edited;
- **folders, tags and custom fields**, stored exactly as XenCenter stores
  them (`other_config["folder"]`, the pool's `EMPTY_FOLDERS`, the `tags`
  field, the pool's `gui_config["XenCenter.CustomFields"]` XML and
  `other_config["XenCenter.CustomFields.<name>"]`): the General tab's
  Folder, Tags and Custom Fields rows; the Folder row and the Custom
  Fields page of the pool, server and SR Properties dialogs; a VM
  Properties dialog (General with folder and tags, Custom Fields); New
  Folder, Rename and Delete Folder, Remove from folder, Rename and Delete
  Tag, Untag in the views' menus; **drag and drop** of objects into a
  folder, onto a tag, and of a running VM onto another server of its pool
  (live migration, with the usual question);
- **GPUs** (F14a): the GPU tab of a pool or server (the placement policy,
  one row per group of identical physical GPUs with a bar per GPU showing
  its vGPUs, the allowed vGPU types; Edit... / Edit Selected GPUs... to
  allow or disallow types, a type in use cannot be disallowed); the pool
  and server Properties' GPU page (placement policy, integrated GPU
  pass-through after the next reboot); the VM Properties' GPU page (Add
  virtual GPU with the original's compatible-type and allowed-type rules,
  Delete; only while the VM is halted and not protected by HA);
- **USB pass-through**: the server's USB tab (enable or disable
  pass-through per device, asked first; not while a VM uses it) and the VM
  Properties' USB page (Attach USB with the devices of the servers the VM
  can run on, Detach, asked first; attaching switches the VM to the
  upstream device model, as the original);
- **vTPM**: VM > Manage vTPMs... for UEFI VMs (Add vTPM, Remove vTPM,
  asked first, following the server's allowed operations), and the
  General tab's Device Security row;
- **memory settings**: Edit... on the Memory tab's VM rows (halted VMs, or
  running ones with known tools): the dialog for one VM (a fixed memory or
  a range; minimum, maximum and static maximum when the VM balloons) and
  the wizard for several, with the original's limits (the pool's memory
  ratio, the server's free memory, the template's maximum) and its
  question before a static maximum change reboots a running VM; the change
  runs live (`VM.set_memory_dynamic_range`) or with a clean shutdown and a
  start around `VM.set_memory_limits`;
- **external consoles**: Launch SSH Console on the Console tab (Linux VMs
  with an address, and servers) runs the platform's terminal with `ssh
  root@<address>`, or PuTTY or an OpenSSH binary set in Options > External
  Tools; Remote Desktop (Windows guests whose agent offers it: Turn on
  Remote Desktop, then Open Remote Desktop) finds the address that accepts
  port 3389 and starts the platform's client (`mstsc` on Windows, the
  `.rdp` handler on macOS, `xfreerdp` elsewhere, or a program set in
  Options). Remote Desktop is not embedded (plan decision);
- **light and dark themes** (F14b, this console's own): Tools > Options >
  Appearance chooses Light, Dark or the system's setting, saved and applied
  at once. Status texts, alert badges, tree icons, graph lines and the
  memory and GPU bars have a variant for each theme (a test checks their
  contrast against both);
- **languages**: every user-visible string is in one message catalogue
  (`crates/xenadmin/src/locale.rs`; the English catalogue is exported to
  `crates/xenadmin/locales/en.toml`), and Tools > Options > Language
  chooses among the shipped languages or follows the system. Only English
  ships (plan decision 5); `crates/xenadmin/locales/README.md` explains
  how to add pt-BR;
- **native file dialogs**: a Browse... button beside every file or folder
  field (certificates, server backups, XVA and OVF/OVA import and export,
  saved searches, the external tools) opens the platform's Open, Save or
  folder dialog; a typed path still works;
- a **home page** when nothing is selected (in place of the original's web
  page), with Add a Server;
- an About window and an Options window;
- the complete XenAPI surface, generated from the upstream datamodel: every
  class, record, enum and call, with `Async.*` variants (`crates/xapi-codegen`);
- a client-side cache of every class the server delivers through events,
  loaded after an `event.inject` token so nothing is lost between the load
  and the first `event.from`.

The original 25.04 still has the HA and Users pages but no longer adds them
to the tab list; they are shown as the earlier XenCenter showed them (HA on
a pool, Users on a pool or a live standalone server). Editing from the tabs
(the VM Properties pages beyond General, Custom Fields, GPU and USB, so
also a VM's own HA page) and custom graphs (New/Edit Graph) come in later
phases (Remote Desktop runs in the platform's own client, not embedded). The OVF wizards leave out signing a package (it
needs a `.pfx` reader), the OS Fixup page (its ISO is not shipped), the
LUN-per-VDI page, URL sources and disk conversion (no external
converter). The New VM wizard leaves out the pages the
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

## Release packages

Each platform has a script under `packaging/` that builds a release binary
(`cargo build --release --locked`) and packs it with the generic icon
(`packaging/icons/`, drawn by `make_icon.py`). Packages are not signed or
notarized. The output folder defaults to `target/dist`.

| Platform | Command | Result |
|---|---|---|
| macOS | `packaging/macos/build.sh [out-dir]` | `xenadmin-rs.app` and `xenadmin-rs-<version>-macos-<arch>.dmg` (Xcode command line tools only: `sips`, `iconutil`, `hdiutil`; `TARGET=x86_64-apple-darwin` builds for Intel) |
| Linux | `packaging/linux/build.sh [out-dir]` | `xenadmin-rs-<version>-linux-<arch>.tar.gz`, plus a `.deb` when `dpkg-deb` is present and an AppImage when `appimagetool` is present |
| Windows | `powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 [-Out <dir>]` | `xenadmin-rs-<version>-windows-<arch>.zip` (`xenadmin.exe`, `LICENSE`, `README.md`) |

The macOS package is built and started on the development machine; the
Linux and Windows scripts have not been run yet. Gatekeeper asks before
the first start of the unsigned app (right-click > Open). On Linux the file
dialogs use the XDG desktop portal (`xdg-desktop-portal` with a GTK or KDE
backend). Windows release builds have no console window, so the
command-line modes (`--add-server`, `--forget-server`) report through their
exit code only (`--mcp` still works through the pipes its client gives it).

## Settings, passwords and logs

The configuration folder is, in order of preference:

1. `XENADMIN_CONFIG_DIR`, if set;
2. `settings/` next to the executable when a file named `portable` sits
   beside it (**portable mode**);
3. the per-user folder: `~/Library/Application Support/xenadmin-rs` on
   macOS, `%APPDATA%\xenadmin-rs\config` on Windows,
   `$XDG_CONFIG_HOME/xenadmin-rs` on Linux.

It holds `settings.toml` (preferences, including `theme` and `language`,
and the saved servers),
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
(default `warn,xapi=info,xen_core=info,xen_mcp=info,xenadmin=info`). Passwords and session
references are never logged: calls are traced by method name only, and the
client redacts the session reference XAPI puts in `SESSION_INVALID` errors.
A test runs every flow at `trace` level and checks the log for both.
`Ctrl+C` or `SIGTERM` close the app normally, logging out of every server.

## MCP server

`xenadmin --mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server instead of the window, so an MCP client (Claude Code, Claude
Desktop, any other) can read the saved servers' pools, hosts, VMs, storage,
networks, alerts and performance data and, only when the configuration
allows it, start, stop and snapshot the VMs it names. It speaks JSON-RPC
over standard input and output (protocol revisions 2024-11-05 to
2025-11-25); logs go to stderr and `logs/xenadmin-rs-mcp.*.log`, never to
stdout.

It uses the window's configuration folder, read only:

- it connects to the **saved servers** (all of them, or those `mcp.toml`
  lists) with their **saved passwords** from the keyring. Save one in the
  window with "Remember password", or with `xenadmin --add-server`;
- it accepts only **pinned certificates** (`known_servers.toml`): an
  unknown or changed certificate is refused and `list_servers` says why.
  Connect once in the window to trust it;
- it never changes the saved list, the pins or the keyring, and never sends
  a password, session reference or keyring detail to the client, the log
  or the audit log.

### Registering it in a client

Claude Code (the server then starts with each session; `--scope user`
makes it available in every project):

```sh
claude mcp add --scope user xenadmin -- /path/to/xenadmin --mcp
# with another configuration folder or keyring service:
claude mcp add --scope user xenadmin \
  -e XENADMIN_CONFIG_DIR=/path/to/config -e XENADMIN_KEYRING_SERVICE=xenadmin-rs \
  -- /path/to/xenadmin --mcp
```

On macOS the binary of the packaged app is
`/Applications/xenadmin-rs.app/Contents/MacOS/xenadmin`. Other clients take
the same command in their JSON configuration (a project's `.mcp.json`,
Claude Desktop's `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "xenadmin": {
      "command": "/path/to/xenadmin",
      "args": ["--mcp"]
    }
  }
}
```

`--mcp-config <file>` reads another file than `mcp.toml`. On Windows the
release build has no console window, but its standard input and output
still work through the pipes an MCP client gives it.

### Configuration: `mcp.toml`

In the configuration folder, next to `settings.toml`. Without the file the
server connects to every saved server and is **read only**.

```toml
# Saved servers to connect to (addresses as in the server list).
# Empty or absent: all of them.
servers = ["192.0.2.10"]

[writes]
# The write tools exist only when this is true.
enabled = false

# Per server, the only VMs the write tools may touch: UUIDs, or names that
# exactly one VM has. A server without an entry allows no write.
[writes.allow]
"192.0.2.10" = ["test-vm-01", "0b0b0b0b-0000-4000-8000-000000000003"]
```

An unknown key (a misspelt `enable = true`) stops the start with an error
instead of leaving the server in another mode than intended. Optional
timings: `connect_wait_seconds` (30), `operation_timeout_seconds` (600),
`performance_wait_seconds` (60).

### Tools

Objects are named by UUID or exact name (a name several objects share is
refused, listing their UUIDs). `server` (a saved address, pool name or pool
UUID) may be left out when one server is configured; list tools without it
cover every configured server. Read tools answer from the live cache the
connection keeps through `event.from`, so they make no call to the server;
`get_performance` reads the RRDs as the Performance tab does.

| Tool | What it returns or does |
|---|---|
| `list_servers` | The configured servers: status (or why one cannot connect), pool, API version, the write switch and allow-list |
| `get_pool` | Pool: coordinator, hosts, VM counts, default SR, HA |
| `list_hosts`, `get_host` | Hosts: address, enabled, live, maintenance mode, memory, CPUs, versions, NICs, storage, running VMs |
| `list_vms`, `get_vm` | VMs (filters: name, power state, host, tag; templates, snapshots and control domains on request): power state, host, vCPUs, memory, OS, IP addresses, disks, interfaces, snapshots, allowed operations |
| `list_srs`, `get_sr` | Storage: type, shared, size, utilisation, hosts, virtual disks and their VMs (never `device_config`) |
| `list_networks`, `get_network` | Networks: bridge, MTU, VLAN, NICs, VM interfaces |
| `list_alerts` | Alerts newest first (filters: priority, object) |
| `get_performance` | A host's or VM's recent data per data source (interval 5s, 1m, 1h or 1d; CPU in percent, memory in bytes, throughput in bytes per second) |
| `vm_start`, `vm_shutdown` (`force` for a forced shutdown), `vm_reboot` (`force`), `vm_suspend`, `vm_resume` | VM lifecycle (**write**) |
| `vm_snapshot` (`name`, `description`, `memory`), `vm_snapshot_revert`, `vm_snapshot_delete` | Snapshots (**write**) |

Write tools are listed only when `[writes] enabled = true`, and each call
must pass `confirm: true`. Before anything reaches the server a write is
refused, with the reason, when: writes are disabled; there is no
confirmation; the VM (for a snapshot, the VM it belongs to) is not in the
server's allow-list; the window would not offer the operation now (power
state, `allowed_operations`, a host to run on, an operation of this client
already running on it); the pool's HA protects a VM to start or resume (the
window checks the HA plan first); or the user's roles do not allow it (no
role elevation here). Otherwise the operation runs exactly as from the
window, and the tool waits for its end. Nothing that changes a host, pool,
SR or network is a tool.

### Resources

The same views, as JSON: `xenadmin://servers`;
`xenadmin://server/{server}/{list}` with `{list}` one of `pool`, `hosts`,
`vms`, `srs`, `networks`, `alerts` (listed by `resources/list`); and the
templates `xenadmin://server/{server}/{kind}/{uuid}` (`kind`: pool, host,
vm, sr, network) and `xenadmin://server/{server}/{host|vm}/{uuid}/performance`
(the last hour, a point a minute). `{server}` is the saved address,
percent-encoded.

### Audit log

Every tool call and resource read appends one JSON line to
`logs/mcp-audit.jsonl`: the time (UTC), the tool, its arguments (any
argument whose name looks like a secret, such as `password`, `token` or
`session`, is replaced by `[redacted]`), the server and object it resolved
to, the outcome (`ok`, `refused` or `error`), the refusal or error text or
the operation's end, and the duration.

### Not in this phase

There is no HTTP transport. It would be a second front end on the same
server object (`xen_mcp::McpServer`): a local `hyper` listener taking the
"Streamable HTTP" `POST` bodies to the same request handler, with a bearer
token and the `Origin` check the specification asks for. The operations of
the later phases (disks, storage, networking, pools, HA, migration and
copy, vApps, scheduled snapshots, disaster recovery, folders and tags,
devices, memory settings, performance alerts, dismissing alerts) are not
tools yet; each could follow the write tools' pattern.

## Test

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

After adding or changing a UI string, rewrite the exported English
catalogue with `XENADMIN_BLESS=1 cargo test -p xenadmin english_catalogue`
(the test fails while `locales/en.toml` is out of date). Native file
dialogs never open under test: tests queue the path the dialog returns.

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
- OVF/OVA and disk images against the mock (`xapi_mock::raw_vdi` and
  `xapi_mock::vhd`: `GET /export_raw_vdi` as a dynamic VHD or raw bytes,
  `PUT /import_raw_vdi` read as it arrives, each disk's content kept and
  hashed; `VM.create`): a VM exported as an OVA and as a compressed OVF
  folder with a 4 KiB buffer and imported back (the disk's content, the
  memory, vCPUs, boot settings, the interface and its MAC checked); a raw
  image and a fixed VHD imported as VMs, the raw image's VM exported again
  with the image's bytes; cancelled transfers, server errors, a dropped
  link, a full disk and a file that ends early leaving no file, VM or
  disk; a corrupt manifest, a changed disk, an unlisted file and a bad
  signature found before anything is made; a signed package accepted; a
  VMDK refused; in the window: the Export wizard's OVF pages, the Import
  wizard on that OVA and on a disk image, and the formats it refuses;
- migrations and vApps against two mock pools (`xapi_mock::migrate`:
  `host.migrate_receive`, `VM.assert_can_migrate` and `VM.migrate_send`
  between mock servers in one process; `xapi_mock::appliance`: vApps that
  start their VMs by order with their delays): a running VM migrated to
  another pool with a disk per SR and its interface on another network,
  a halted VM copied there, each refused check (current server, older
  server, the server's errors, a role that may not, too little memory),
  a migration failing in the middle and one cancelled leaving everything
  as it was, Move VM and storage motion within a pool; vApps made, started
  in order, shut down in reverse order, edited, assigned and deleted; in
  the window: Move VM through the wizard to another pool, a refused
  destination with the server's reason, Copy VM within the pool and to
  another pool, and Manage vApps with the New vApp wizard and Properties;
- scheduled snapshots and disaster recovery against the mock
  (`xapi_mock::vmss`: policies, a clock that runs the enabled ones on
  schedule with their retention and history messages; `xapi_mock::dr`:
  metadata replicated to a shared SR that two mock pools see, `DR_task`,
  metadata sessions and `VM.recover` / `VM_appliance.recover`): a
  schedule made, run three times keeping two snapshots, failing for a VM,
  run now, disabled (no more runs), edited, assigned and deleted; DR
  configured on one pool, a test failover on the other listing the
  recoverable VMs and vApps, each failing pre-check and its fix (HA on, a
  VM running in the source pool, a newer VM here, local storage, a missing
  shared SR attached with a `DR_task`), a recovery failing in the middle
  and one cancelled leaving nothing, a failover and a failback; in the
  window: the New Snapshot Schedule wizard, the schedule's history,
  Disable, Properties and Delete, Configure, and the Disaster Recovery
  wizard's test failover through an iSCSI scan to its report, and a
  cancelled wizard cleaning up;
- folders, tags, custom fields and searches against the mock
  (`xapi_mock::org`: `add_tags` / `remove_tags`, the pool's
  `add_to_gui_config` / `remove_from_gui_config`): the exact keys,
  values and calls XenCenter makes (an emptied folder kept in
  `EMPTY_FOLDERS`, New Folder, Rename, Delete, tags added and removed one
  by one, custom field definitions as XML and values with dates in UTC),
  saved searches on the pool, every view and the built-in searches over
  the live cache, the `.xensearch` format, and the Search tab's figures;
  in the window: every navigation view, the Search tab's filters,
  grouping, sorting, Save, Export and Import, the Properties dialogs and
  tree menus, and drag and drop into a folder, onto a tag and onto a
  server;
- GPUs, USB, vTPMs, memory and the external consoles against the mock
  with its device fixture (`xapi_mock::devices`, `tests/fixtures/devices.json`:
  three GRID K2 GPUs with four vGPU types and a running vGPU, an
  integrated GPU, four USB devices): the GPU tab's rows, bars and types,
  the allowed types, placement policy and integrated GPU calls, a VM's
  vGPUs added and removed, USB pass-through and devices attached and
  detached, vTPMs added and removed, memory changed live, on a halted VM
  and around a reboot, Remote Desktop turned on and its address probed,
  and every refusal; in the window: both tabs, the Properties pages and
  dialogs, the vTPM manager, the memory dialog and wizard, and the SSH
  and Remote Desktop command lines (recorded, never started; a unit test
  starts a harmless local program through the launcher);
- the MCP server (`crates/xen-mcp/tests`, a scripted client over the same
  line protocol as stdio): the handshake and version negotiation, the tool
  and resource lists, every read tool and resource checked against the
  fixture, pinned certificates only, every write tool refused when writes
  are disabled, without `confirm: true`, outside the allow-list (by name,
  UUID, a shared name, a server without a list), when not offered, for a
  read-only user and for an HA-protected VM, each time with no call reaching
  the server; an allowed VM started, rebooted, suspended, resumed, shut
  down and snapshotted (taken, reverted, deleted); the audit log's line for
  every call; and no password or session reference in any response, the
  audit log or the trace log. `crates/xenadmin/tests/mcp_stdio.rs` runs
  `xenadmin --mcp` itself over real stdin and stdout and checks that the
  configuration folder is only read;
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

OVF/OVA and disk images on a real host (`app/tests/ovf_real.rs`, same
variables plus `XENADMIN_REAL_LOG` and `XENADMIN_REAL_OVF_DIR`):
`real_f11b_ova_and_image` makes one disposable VM at a time, all named
`xenadmin-rs-test-f11b*` with their disks on "Local storage" and never
started: the test VM (100 MiB disk) exported as an OVA with the wizard,
deleted, imported back, checked and deleted; then a 16 MiB raw image
imported as a VM, checked, exported as an OVA (its disk's content checked
against the image), deleted, imported back, checked and deleted.
`f11b_real_run_on_the_mock` rehearses it and checks no other object
changes and only the calls it needs are made.

vApps on a real host (`app/tests/migrate_real.rs`, same variables plus
`XENADMIN_REAL_LOG`): `real_f12a_vapp_and_copy_wizard` makes the
disposable VM `xenadmin-rs-test-f12a` (64 MiB disk on "Local storage",
never started), opens Copy VM... on it and cancels on the Name and
Storage page (nothing is asked of the server), makes the vApp
`xenadmin-rs-test-f12a-vapp` with that VM only, edits its start order,
delay and description, and deletes the vApp and the VM. It never calls a
migration or feasibility call, nor starts or shuts down a vApp;
`f12a_real_run_on_the_mock` rehearses it and checks exactly that.

Scheduled snapshots on a real host (`app/tests/vmss_dr_real.rs`, same
variables): `real_f12b_snapshot_schedule_and_dr_read_only` makes the
disposable VM `xenadmin-rs-test-f12b` (64 MiB disk on "Local storage",
never started), makes the schedule `xenadmin-rs-test-f12b-schedule`
without VMs (so disabled), assigns the VM to it, edits it and deletes it,
then the VM. The schedule is never enabled or run. Disaster recovery is
only looked at: Configure and the wizard (up to its Storage page) are
opened and cancelled; no replication, `DR_task`, recovery or SR change is
made. `f12b_real_run_on_the_mock` rehearses it and checks exactly that.

Search, views, folders and tags on a real host (`app/tests/org_real.rs`,
same variables): `real_f13_views_search_and_test_vm_folder_tags` builds
every view and runs every built-in search and the Search tab read only,
then makes the disposable VM `xenadmin-rs-test-f13`, gives it the folder
`/xenadmin-rs-test-f13` and the tag `xenadmin-rs-test-f13` through its
Properties dialog (checked to write only the VM), clears the tag there
and the folder with the VM's own call (the product's Remove from folder
would also list the emptied folder on the pool), then deletes the VM. The
pool record must not change. `f13_real_run_on_the_mock` rehearses it.

GPUs, USB, memory and vTPM on a real host (`app/tests/devices_real.rs`,
same variables): `real_f14a_devices_memory_vtpm_and_consoles` reads the GPU
tab, the USB tab, the integrated GPU and the Memory tab's figures, opens
and cancels the server Properties' GPU page, then makes the disposable VM
`xenadmin-rs-test-f14a` (UEFI), changes its memory limits through the
Memory tab's Edit..., adds and removes a vTPM, shows the external console
command lines as dry runs, and deletes the VM. No GPU, USB, server or pool
record may change. `f14a_real_run_on_the_mock` rehearses it and checks
that no other call is made.

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
| `crates/xen-core` | UI-independent backend: the connection manager and per-connection actors (login, TOFU, redirect, heartbeat, reconnection), the live object cache (one map per class), its tree and the facts derived from it (VM home, SR state, host memory, NIC and network names), the VM and host operations (when each is offered, how each runs as XAPI tasks), VM creation, templates, snapshots and disks (`provisioning`, `provision_run`), storage (`storage`, `storage_run`), networking (`network`, `network_run`), pools and pool and server settings (`pool`, `properties`, `pool_run`), high availability and Active Directory (`ha`, `ad`, `ha_run`), XVA files and the import and export wizards' rules (`xva`: the streaming verifier, XXH64, `ova.xml`, `.gz`) and the transfers (`xva_run`), OVF/OVA packages and disk images (`ovf`: the descriptor read with roxmltree and written with quick-xml; `ovf_package`: folders and OVAs, manifests and signatures; `ova`: tar; `disk_image`: VHD, raw, ISO and the formats refused; `ovf_rules`: the wizards' rules) and their transfers (`ovf_run`), the Migrate / Move / Copy wizard's rules and runs (`migrate`, `migrate_run`), vApps (`vapp`, `vapp_run`), scheduled snapshots (`vmss`, `vmss_run`) and disaster recovery (`dr`, `dr_run`), folders, tags and custom fields (`org`, `org_run`), GPUs, USB, vTPMs and memory settings (`devices`, `memory`, `devices_run`), the external SSH and Remote Desktop consoles (`consoles`) and searches (`search`: the model and the `.xensearch` XML; `search_eval`: properties, filters, grouping, views and built-in searches; `search_metrics`: the Search tab's figures), the consoles (finding a VM's console, opening it, reconnecting), the performance feeds and default graphs, alerts and the perfmon settings, the RBAC check, the operation history, settings, known servers and the keyring store. |
| `crates/xen-rrd` | XAPI performance data: the `rrd_updates`, `host_rrds` and `vm_rrds` client (JSON or XML, the session reference kept out of logs and errors), their parsers, and the four archives the graphs draw from. |
| `crates/xen-vnc` | VNC client for XAPI consoles: the `CONNECT` tunnel, the RFB protocol and decoders, the framebuffer, XT scancodes, and a fake guest console for tests (feature `testing`). Its ZRLE decoder derives from [vnc-rs](https://github.com/HsuJv/vnc-rs) 0.6.1 (MIT, see `crates/xen-vnc/LICENSE-vnc-rs`). |
| `crates/xen-mcp` | The MCP server (`xenadmin --mcp`): the JSON-RPC stdio transport, the tools and resources over `xen-core`'s cache and operations, `mcp.toml`, the write rules and the audit log. |
| `crates/xenadmin` | The egui/eframe desktop app; its message catalogue's English export and translation guide are in `locales/`. |
| `packaging/` | The release package scripts and the generic icon (not a crate). |
| `crates/xapi-mock` | Test-only mock XAPI server. |

`xapi`, `xen-rrd`, `xen-vnc`, `xen-core` and `xen-mcp` do not depend on egui. The app runs a tokio runtime on
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
