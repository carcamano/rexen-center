//! The detail tabs of the selected object: General, Memory, Storage,
//! Networking and NICs (read-only), and Console, as the original console
//! lays them out. The Console tab is drawn by [`crate::console_view`].
//!
//! Builders turn the connection's cached model into plain data ([`Section`]
//! lists, [`Table`]s, memory rows) so they can be tested without a window;
//! [`render`] draws that data. Everything is rebuilt from the current model
//! snapshot each frame, so the tabs follow `event.from` updates live.

pub mod general;
pub mod memory;
pub mod network;
pub mod render;
pub mod storage;

use xapi::{Host, HostRecord, PoolRecord, Ref, Sr, SrRecord, Vm, VmRecord};
use xen_core::ConnectionModel;
use xen_core::objects::{self, VmKind};

use crate::i18n::fill;
use crate::tab_strings::tab_strings;

/// A detail tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    General,
    Memory,
    /// A VM's disks.
    VmStorage,
    /// An SR's virtual disks.
    SrStorage,
    /// The SRs of a host or pool.
    PhysicalStorage,
    Networking,
    Nics,
    /// The VM's or host's console (`TabPageConsole`).
    Console,
    /// Performance graphs (`PerformancePage`), drawn by
    /// [`crate::perf_view`].
    Performance,
    /// A VM's snapshots (`SnapshotsPage`), drawn by
    /// [`crate::provision_ui`].
    Snapshots,
    /// A pool's high availability (`HAPage`), drawn by [`crate::ha_ui`].
    Ha,
    /// The users of a pool or standalone server (`AdPage`), drawn by
    /// [`crate::ad_ui`].
    Users,
    /// The Search tab of a pool or server (`SearchPage`), drawn by
    /// [`crate::search_ui`].
    Search,
    /// The GPUs of a pool or server (`GpuPage`), drawn by
    /// [`crate::dev_ui`].
    Gpu,
    /// A server's USB devices (`UsbPage`), drawn by [`crate::dev_ui`].
    Usb,
}

impl Tab {
    pub fn title(self) -> &'static str {
        let t = tab_strings();
        match self {
            Self::General => t.tab_general,
            Self::Memory => t.tab_memory,
            Self::VmStorage | Self::SrStorage | Self::PhysicalStorage => t.tab_storage,
            Self::Networking => t.tab_networking,
            Self::Nics => t.tab_nics,
            Self::Console => t.tab_console,
            Self::Performance => crate::perf_strings::perf_strings().tab_performance,
            Self::Snapshots => crate::prov_strings::prov_strings().tab_snapshots,
            Self::Ha => crate::ha_strings::ha_strings().tab_ha,
            Self::Users => crate::ha_strings::ha_strings().tab_users,
            Self::Search => crate::org_strings::org_strings().tab_search,
            Self::Gpu => crate::dev_strings::dev_strings().tab_gpu,
            Self::Usb => crate::dev_strings::dev_strings().tab_usb,
        }
    }
}

/// The object whose tabs are shown.
#[derive(Clone, Copy)]
pub enum Subject<'a> {
    Pool(&'a PoolRecord),
    Host(&'a Ref<Host>, &'a HostRecord),
    Vm(&'a Ref<Vm>, &'a VmRecord),
    Sr(&'a Ref<Sr>, &'a SrRecord),
}

/// What the builders read.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub model: &'a ConnectionModel,
    /// Current time, Unix seconds (for uptimes).
    pub now: i64,
    /// View > Hidden Objects.
    pub show_hidden: bool,
}

/// The tabs shown for `subject` (`MainWindow.GetNewTabPages`).
pub fn tabs_for(model: &ConnectionModel, subject: Subject<'_>) -> Vec<Tab> {
    match subject {
        // The 25.04 source keeps the HA and Users pages but no longer adds
        // them to the tab list; they are shown where the earlier XenCenter
        // showed them: HA on a pool, Users on a pool or a live standalone
        // server.
        Subject::Pool(..) => {
            let mut tabs = vec![
                Tab::General,
                Tab::Memory,
                Tab::PhysicalStorage,
                Tab::Networking,
            ];
            if xen_core::devices::gpu_tab_shown(model) {
                tabs.push(Tab::Gpu);
            }
            tabs.extend([Tab::Ha, Tab::Users, Tab::Search]);
            tabs
        }
        Subject::Host(_, host) if objects::host_is_live(model, host) => {
            let mut tabs = vec![
                Tab::General,
                Tab::Memory,
                Tab::PhysicalStorage,
                Tab::Networking,
                Tab::Nics,
            ];
            if xen_core::devices::gpu_tab_shown(model) {
                tabs.push(Tab::Gpu);
            }
            if xen_core::devices::usb_tab_shown(model, host) {
                tabs.push(Tab::Usb);
            }
            tabs.extend([Tab::Console, Tab::Performance]);
            if xen_core::pool::visible_pool(model).is_none() {
                tabs.push(Tab::Users);
            }
            tabs.push(Tab::Search);
            tabs
        }
        Subject::Host(..) => vec![Tab::General],
        Subject::Vm(_, vm) => {
            let mut tabs = vec![Tab::General, Tab::Memory];
            // Templates with provisioning instructions have no disks yet.
            let provision = vm.is_a_template && vm.other_config.contains_key("disks");
            if !provision {
                tabs.push(Tab::VmStorage);
            }
            tabs.push(Tab::Networking);
            // A real VM has a console (not templates or snapshots).
            if objects::vm_kind(vm) == VmKind::Guest {
                tabs.push(Tab::Console);
                tabs.push(Tab::Performance);
                tabs.push(Tab::Snapshots);
            }
            tabs
        }
        Subject::Sr(..) => vec![Tab::General, Tab::SrStorage],
    }
}

/// The page title above a tab's content.
pub fn page_title(tab: Tab, subject: Subject<'_>) -> &'static str {
    let t = tab_strings();
    match (tab, subject) {
        (Tab::General, Subject::Vm(_, vm)) => match objects::vm_kind(vm) {
            VmKind::Snapshot => t.hdr_snapshot_general,
            VmKind::UserTemplate | VmKind::DefaultTemplate => t.hdr_template_general,
            _ => t.hdr_vm_general,
        },
        (Tab::General, Subject::Host(..)) => t.hdr_host_general,
        (Tab::General, Subject::Pool(..)) => t.hdr_pool_general,
        (Tab::General, Subject::Sr(..)) => t.hdr_sr_general,
        (Tab::Memory, _) => t.hdr_memory,
        (Tab::VmStorage, _) => t.hdr_vm_disks,
        (Tab::SrStorage, _) => t.hdr_virtual_disks,
        (Tab::PhysicalStorage, _) => t.hdr_physical_storage,
        (Tab::Networking, Subject::Vm(..)) => t.hdr_vm_networks,
        (Tab::Networking, Subject::Host(..)) => t.hdr_host_networks,
        (Tab::Networking, _) => t.hdr_pool_networks,
        (Tab::Nics, _) => t.hdr_nics,
        (Tab::Console, _) => t.tab_console,
        (Tab::Performance, _) => crate::perf_strings::perf_strings().hdr_performance,
        (Tab::Snapshots, _) => crate::prov_strings::prov_strings().hdr_snapshots,
        (Tab::Ha, _) => crate::ha_strings::ha_strings().high_availability,
        (Tab::Users, _) => crate::ha_strings::ha_strings().ad_tab_title,
        (Tab::Search, _) => crate::org_strings::org_strings().tab_search,
        (Tab::Gpu, _) => crate::dev_strings::dev_strings().tab_gpu,
        (Tab::Usb, _) => crate::dev_strings::dev_strings().usb_devices,
    }
}

/// One label/value line of a General-tab section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    /// Shown in the warning colour (the original's red text).
    pub warn: bool,
}

impl Row {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            warn: false,
        }
    }

    pub fn warn(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            warn: true,
            ..Self::new(label, value)
        }
    }
}

/// A titled group of rows. Sections without rows are not shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

impl Section {
    /// The value of the first row labelled `label`.
    #[cfg(test)]
    pub fn value(&self, label: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.label == label)
            .map(|r| r.value.as_str())
    }
}

/// A grid of the original's list views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<&'static str>,
    pub rows: Vec<TableRow>,
}

impl Table {
    pub fn new(columns: Vec<&'static str>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, cells: Vec<String>) {
        self.rows.push(TableRow {
            cells,
            greyed: false,
        });
    }

    /// The cells of column `column` (by header), top to bottom.
    #[cfg(test)]
    pub fn column(&self, column: &str) -> Vec<&str> {
        let Some(i) = self.columns.iter().position(|c| *c == column) else {
            return Vec::new();
        };
        self.rows
            .iter()
            .map(|r| r.cells.get(i).map(String::as_str).unwrap_or_default())
            .collect()
    }
}

/// One line of a [`Table`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<String>,
    /// Drawn greyed out (bond members).
    pub greyed: bool,
}

/// "Yes" / "No".
pub fn yes_no(value: bool) -> String {
    let t = tab_strings();
    if value { t.yes } else { t.no }.to_owned()
}

/// "3 days 4 hours 5 minutes", like the original's `PrettyTimeSpan`.
pub fn pretty_duration(seconds: i64) -> String {
    let t = tab_strings();
    let (d, h, m) = xen_core::format::split_duration(seconds);
    let part = |n: i64, one: &str, many: &str| {
        fill(if n == 1 { one } else { many }, &[("n", &n.to_string())])
    };
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(part(d, t.day, t.days));
    }
    if d > 0 || h > 0 {
        parts.push(part(h, t.hour, t.hours));
    }
    parts.push(part(m, t.minute, t.minutes));
    parts.join(" ")
}

/// Name of a VM record as the original shows it ("Control domain on host
/// X" without the colon).
pub fn vm_display_name(vm: &VmRecord) -> String {
    match vm.name_label.strip_prefix("Control domain on host: ") {
        Some(host) if vm.is_control_domain => format!("Control domain on host {host}"),
        _ => vm.name_label.clone(),
    }
}

/// Pool name, or the coordinator's name for an unnamed pool.
pub fn pool_display_name(model: &ConnectionModel, pool: &PoolRecord) -> String {
    xen_core::objects::pool_name(model, pool)
}

#[cfg(test)]
pub(crate) mod test_support {
    use xen_core::ConnectionModel;

    use super::Ctx;

    /// The hand-written fixture pool.
    pub fn fixture() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::fixture_records())
    }

    /// The sanitized real-host capture.
    pub fn capture() -> ConnectionModel {
        ConnectionModel::from_database(&xapi_mock::capture_records())
    }

    /// 2026-11-25T12:00:00Z.
    pub const NOW: i64 = 1_795_608_000;

    pub fn ctx(model: &ConnectionModel) -> Ctx<'_> {
        Ctx {
            model,
            now: NOW,
            show_hidden: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn tabs_follow_the_object_type() {
        let m = fixture();
        let (_, p) = m.pool().unwrap();
        assert_eq!(
            tabs_for(&m, Subject::Pool(p)),
            [
                Tab::General,
                Tab::Memory,
                Tab::PhysicalStorage,
                Tab::Networking,
                Tab::Ha,
                Tab::Users,
                Tab::Search
            ]
        );
        // A pool member has no Users tab (the pool has it).
        let hr = Ref::new("OpaqueRef:host-0001");
        let host_tabs = tabs_for(&m, Subject::Host(&hr, &m.hosts[&hr]));
        assert_eq!(host_tabs.len(), 8);
        assert_eq!(
            host_tabs[5..],
            [Tab::Console, Tab::Performance, Tab::Search]
        );
        // A live standalone server has one.
        let lab = ConnectionModel::from_database(&xapi_mock::lab_records("t", 1));
        let lr = Ref::new("OpaqueRef:host-0001-t");
        assert_eq!(
            tabs_for(&lab, Subject::Host(&lr, &lab.hosts[&lr]))
                .iter()
                .rev()
                .nth(1),
            Some(&Tab::Users)
        );
        let vr = Ref::new("OpaqueRef:vm-web");
        assert_eq!(
            tabs_for(&m, Subject::Vm(&vr, &m.vms[&vr])),
            [
                Tab::General,
                Tab::Memory,
                Tab::VmStorage,
                Tab::Networking,
                Tab::Console,
                Tab::Performance,
                Tab::Snapshots
            ]
        );
        // A built-in template still to be provisioned has no Storage tab,
        // and templates have no console.
        let tr = Ref::new("OpaqueRef:vm-template-debian");
        let template_tabs = tabs_for(&m, Subject::Vm(&tr, &m.vms[&tr]));
        assert!(!template_tabs.contains(&Tab::VmStorage));
        assert!(!template_tabs.contains(&Tab::Console));
        assert!(!template_tabs.contains(&Tab::Performance));
        assert!(!template_tabs.contains(&Tab::Snapshots));
        let sr = Ref::new("OpaqueRef:sr-nfs");
        assert_eq!(
            tabs_for(&m, Subject::Sr(&sr, &m.srs[&sr])),
            [Tab::General, Tab::SrStorage]
        );
    }

    #[test]
    fn durations_read_like_the_original() {
        assert_eq!(pretty_duration(59), "0 minutes");
        assert_eq!(pretty_duration(3660), "1 hour 1 minute");
        assert_eq!(
            pretty_duration(86_400 + 12 * 60),
            "1 day 0 hours 12 minutes"
        );
    }
}
