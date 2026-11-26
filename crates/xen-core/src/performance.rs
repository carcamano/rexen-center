//! The performance data of a host or VM, kept up to date while its
//! Performance tab is open, as the original's `ArchiveMaintainer`:
//!
//! 1. the data sources and their units (`host.get_data_sources` /
//!    `VM.get_data_sources`);
//! 2. the whole RRD (`/host_rrds`, or `/vm_rrds?uuid=` on the host running
//!    the VM), which fills the four archives;
//! 3. every [`BackendOptions::perf_interval`](crate::BackendOptions) (5 s)
//!    the new 5-second rows (`/rrd_updates?interval=5&start=…`), every
//!    minute the new minute rows, every hour the hourly ones and every day
//!    the daily ones.
//!
//! The UI makes a [`PerfHandle`] / [`PerfLink`] pair with [`pair`], keeps
//! the handle and gives the link to the backend in
//! [`Command::OpenPerformance`](crate::Command::OpenPerformance). Dropping
//! the handle stops the updates.
//!
//! The HTTP requests go to the host's address with the connection's scheme
//! and port (to the connected server itself for the pool coordinator),
//! authenticated with the connection's current session (`session_id=`,
//! never logged), over TLS pinned like the console's.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use xapi::{CertificateType, DataSourceRecord, Fingerprint, Host, Ref, Session, Vm, VmPowerState};
use xen_rrd::{Archives, Format, Interval, ObjectKind, RrdClient, UpdatesQuery};

use crate::backend::{ConnectionId, SessionSlot, Shared, slot_get};
use crate::model::ConnectionModel;

/// Whose performance is shown.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PerfTarget {
    Host(Ref<Host>),
    Vm(Ref<Vm>),
}

/// Where the data stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerfStatus {
    /// Reading the data sources and the whole RRD.
    Loading,
    /// The archives are loaded and being updated.
    Live,
    /// The VM is not running: what was loaded stays, nothing new comes.
    NotRunning,
    /// The connection is down; waiting for it.
    Disconnected,
    /// The last request failed; retrying.
    Failed(String),
    /// The host or VM is gone from the cache.
    Gone,
}

/// What the UI reads.
#[derive(Debug, Clone)]
pub struct PerfData {
    pub status: PerfStatus,
    /// The object's series, by data source name.
    pub archives: Arc<Archives>,
    /// The object's data sources (name, units…), when the server told.
    pub sources: Arc<Vec<DataSourceRecord>>,
    /// The format the host answered in.
    pub format: Option<Format>,
    /// Successful `rrd_updates` requests so far.
    pub updates: u64,
}

#[derive(Debug)]
struct PerfShared {
    data: Mutex<PerfData>,
    closed: AtomicBool,
    close: Notify,
}

impl PerfShared {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn update(&self, f: impl FnOnce(&mut PerfData)) {
        f(&mut self.data.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// The UI's side. Dropping it stops the updates.
#[derive(Debug)]
pub struct PerfHandle {
    shared: Arc<PerfShared>,
}

/// The backend's side, given in
/// [`Command::OpenPerformance`](crate::Command::OpenPerformance).
#[derive(Debug)]
pub struct PerfLink {
    shared: Arc<PerfShared>,
}

/// A new handle and the link for the backend.
pub fn pair() -> (PerfHandle, PerfLink) {
    let shared = Arc::new(PerfShared {
        data: Mutex::new(PerfData {
            status: PerfStatus::Loading,
            archives: Arc::default(),
            sources: Arc::default(),
            format: None,
            updates: 0,
        }),
        closed: AtomicBool::new(false),
        close: Notify::new(),
    });
    (
        PerfHandle {
            shared: shared.clone(),
        },
        PerfLink { shared },
    )
}

impl PerfHandle {
    /// The latest data (cheap: the archives are behind an `Arc`).
    pub fn data(&self) -> PerfData {
        self.shared
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for PerfHandle {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.close.notify_waiters();
        self.shared.close.notify_one();
    }
}

/// How often a waiting task looks at the cache again.
const POLL: Duration = Duration::from_millis(500);
/// Incomplete data sources are asked for again every this many update
/// intervals (30 s with the default 5 s).
const SOURCES_RETRY_INTERVALS: u32 = 6;
/// Timeout of one RRD request (a whole host RRD is several MB).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct PerfEnv {
    pub(crate) shared: Arc<Shared>,
    pub(crate) connection: ConnectionId,
    pub(crate) session: SessionSlot,
}

impl PerfEnv {
    /// `Err(true)`: the connection is gone; `Err(false)`: down for now.
    fn model(&self) -> Result<Arc<ConnectionModel>, bool> {
        let state = self.shared.state();
        match state.connection(self.connection) {
            None => Err(true),
            Some(view) if view.status.is_connected() => Ok(view.model.clone()),
            Some(_) => Err(false),
        }
    }

    fn endpoint(&self) -> Option<xapi::Endpoint> {
        let state = self.shared.state();
        let view = state.connection(self.connection)?;
        xapi::Endpoint::parse(&view.server).ok()
    }

    /// The connected server's endpoint (for [`crate::search_metrics`]).
    pub(crate) fn endpoint_pub(&self) -> Option<xapi::Endpoint> {
        self.endpoint()
    }

    /// An RRD client for `host` at `endpoint`.
    pub(crate) fn client(
        &self,
        model: &ConnectionModel,
        host: &Ref<Host>,
        endpoint: &xapi::Endpoint,
    ) -> xen_rrd::Result<RrdClient> {
        client_for(self, model, host, endpoint)
    }
}

/// What to ask for: the object's UUID, the host to ask, whether it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub uuid: String,
    pub host: Ref<Host>,
    pub running: bool,
}

/// Resolves `target` in `model` (`Err` = the status to show).
pub fn source(model: &ConnectionModel, target: &PerfTarget) -> Result<Source, PerfStatus> {
    match target {
        PerfTarget::Host(h) => {
            let host = model.get(h).ok_or(PerfStatus::Gone)?;
            Ok(Source {
                uuid: host.uuid.clone(),
                host: h.clone(),
                running: crate::objects::host_is_live(model, host),
            })
        }
        PerfTarget::Vm(v) => {
            let vm = model.get(v).ok_or(PerfStatus::Gone)?;
            let running = vm.power_state == VmPowerState::Running;
            // A VM that does not run: its archived RRD is on the coordinator.
            let host = vm
                .resident_on
                .non_null()
                .filter(|h| running && model.hosts.contains_key(*h))
                .cloned()
                .or_else(|| model.pool().map(|(_, p)| p.master.clone()))
                .ok_or(PerfStatus::Gone)?;
            Ok(Source {
                uuid: vm.uuid.clone(),
                host,
                running,
            })
        }
    }
}

/// The base URL of `host`'s HTTP handlers: the connected server for the
/// coordinator (or a host at that address), else the host's address with
/// the connection's scheme and port (`ArchiveMaintainer.BuildUri`).
pub fn host_endpoint(
    model: &ConnectionModel,
    connected: &xapi::Endpoint,
    host: &Ref<Host>,
) -> xapi::Endpoint {
    let coordinator = model.pool().map(|(_, p)| &p.master) == Some(host);
    match model.get(host) {
        Some(h) if !coordinator && !h.address.is_empty() && h.address != connected.host => {
            // An address with its own port (`host:port`, behind a port
            // forward or in the test mock) keeps it.
            match h.address.split_once(':') {
                Some((host, port)) if !port.contains(':') && port.parse::<u16>().is_ok() => {
                    xapi::Endpoint {
                        tls: connected.tls,
                        host: host.to_owned(),
                        port: port.parse().unwrap_or(connected.port),
                    }
                }
                _ => xapi::Endpoint {
                    tls: connected.tls,
                    host: h.address.clone(),
                    port: connected.port,
                },
            }
        }
        _ => connected.clone(),
    }
}

/// Certificates accepted for `host` at `endpoint`: the one pinned for that
/// address and the `host` certificates the pool lists for it.
pub fn accepted_fingerprints(
    model: &ConnectionModel,
    host: &Ref<Host>,
    endpoint: &xapi::Endpoint,
    trust: &xapi::TrustStore,
) -> Vec<Fingerprint> {
    let mut accepted: Vec<Fingerprint> = trust.get(&endpoint.authority()).into_iter().collect();
    for cert in model.all::<xapi::Certificate>().values() {
        if &cert.host == host
            && cert.r#type == CertificateType::Host
            && let Some(fp) = Fingerprint::parse(&cert.fingerprint_sha256)
            && !accepted.contains(&fp)
        {
            accepted.push(fp);
        }
    }
    accepted
}

/// Runs until the handle is dropped or the connection goes away.
pub(crate) async fn run(env: PerfEnv, target: PerfTarget, link: PerfLink) {
    let shared = link.shared;
    let repaint = env.shared.repaint_fn();
    let publish = |f: &dyn Fn(&mut PerfData)| {
        shared.update(f);
        repaint();
    };
    let interval = env.shared.options.perf_interval;
    // What was loaded, and from where (reloaded when that changes).
    let mut loaded: Option<Source> = None;
    let mut archives = Archives::new();
    let mut last_fetch: [Option<Instant>; 4] = [None; 4];
    // When the data sources were last asked for (again while incomplete).
    let mut sources_asked = Instant::now();
    let mut sources_complete = false;
    let mut cached: Option<(xapi::Endpoint, RrdClient)> = None;
    while !shared.is_closed() {
        let model = match env.model() {
            Ok(m) => m,
            Err(true) => break,
            Err(false) => {
                publish(&|d| d.status = PerfStatus::Disconnected);
                wait(&shared, POLL).await;
                continue;
            }
        };
        let src = match source(&model, &target) {
            Ok(s) => s,
            Err(status) => {
                publish(&|d| d.status = status.clone());
                wait(&shared, POLL).await;
                continue;
            }
        };
        let (Some(session), Some(connected)) = (slot_get(&env.session), env.endpoint()) else {
            publish(&|d| d.status = PerfStatus::Disconnected);
            wait(&shared, POLL).await;
            continue;
        };
        let endpoint = host_endpoint(&model, &connected, &src.host);
        // One client (and its kept-alive connection) per host; a new one
        // after a failure, in case the certificate changed.
        let client = match cached.as_ref().filter(|(e, _)| *e == endpoint) {
            Some((_, c)) => c.clone(),
            None => match client_for(&env, &model, &src.host, &endpoint) {
                Ok(c) => {
                    cached = Some((endpoint.clone(), c.clone()));
                    c
                }
                Err(e) => {
                    publish(&|d| d.status = PerfStatus::Failed(e.to_string()));
                    wait(&shared, interval.max(POLL)).await;
                    continue;
                }
            },
        };

        // (Re)load everything when the VM starts, stops or moves.
        if loaded.as_ref() != Some(&src) {
            let was_running = loaded.as_ref().is_some_and(|l| l.running);
            if loaded.is_none() || src.running != was_running {
                publish(&|d| d.status = PerfStatus::Loading);
            }
            let load = tokio::select! {
                r = initial_load(&client, &session, &target, &src) => r,
                _ = closed(&shared) => break,
            };
            if load.is_err() {
                cached = None;
            }
            match load {
                Ok((full, sources)) => {
                    // Keep what was drawn so far: the new RRD continues it.
                    archives.load_rrd(&full);
                    last_fetch = [Some(Instant::now()); 4];
                    let a = Arc::new(archives.clone());
                    let sources = Arc::new(sources);
                    let format = client.format();
                    sources_asked = Instant::now();
                    sources_complete =
                        !sources.is_empty() && sources.iter().all(|s| !s.units.is_empty());
                    let status = if src.running {
                        PerfStatus::Live
                    } else {
                        PerfStatus::NotRunning
                    };
                    publish(&|d| {
                        d.archives = a.clone();
                        if !sources.is_empty() {
                            d.sources = sources.clone();
                        }
                        d.format = format.or(d.format);
                        d.status = status.clone();
                    });
                    loaded = Some(src);
                }
                Err(e) => {
                    tracing::debug!(error = %e, "performance: initial load failed");
                    if src.running {
                        publish(&|d| d.status = PerfStatus::Failed(e.to_string()));
                        wait(&shared, interval.max(POLL)).await;
                        continue;
                    }
                    // A VM that never ran here has no archive: nothing to show.
                    publish(&|d| d.status = PerfStatus::NotRunning);
                    loaded = Some(src);
                }
            }
            continue;
        }

        // Right after a VM starts its data sources may be missing or without
        // units: ask again now and then until they are complete.
        if src.running
            && !sources_complete
            && sources_asked.elapsed() >= interval * SOURCES_RETRY_INTERVALS
        {
            sources_asked = Instant::now();
            let fetched = match &target {
                PerfTarget::Host(h) => xapi::host::get_data_sources(&session, h).await,
                PerfTarget::Vm(v) => xapi::vm::get_data_sources(&session, v).await,
            };
            if let Ok(list) = fetched
                && !list.is_empty()
            {
                sources_complete = list.iter().all(|s| !s.units.is_empty());
                let list = Arc::new(list);
                publish(&|d| d.sources = list.clone());
            }
        }

        if src.running {
            let mut failure = None;
            let mut got = 0;
            for (i, iv) in Interval::ALL.into_iter().enumerate() {
                let every = match iv {
                    Interval::FiveSeconds => interval,
                    other => Duration::from_secs(other.seconds() as u64),
                };
                if last_fetch[i].is_some_and(|t| t.elapsed() < every) {
                    continue;
                }
                let start = archives
                    .last_time(iv)
                    .map(|t| t - iv.seconds())
                    .unwrap_or(0);
                let query = UpdatesQuery {
                    start,
                    interval: iv.seconds(),
                    cf: xen_rrd::Cf::Average,
                    host: matches!(target, PerfTarget::Host(_)),
                    vm_uuid: matches!(target, PerfTarget::Vm(_)).then(|| src.uuid.clone()),
                };
                match client.updates(session.session_ref().as_str(), &query).await {
                    Ok(updates) => {
                        let kind = match target {
                            PerfTarget::Host(_) => ObjectKind::Host,
                            PerfTarget::Vm(_) => ObjectKind::Vm,
                        };
                        archives.merge_updates(iv, &updates, |id| {
                            id.kind == kind && id.uuid == src.uuid && id.cf == xen_rrd::Cf::Average
                        });
                        last_fetch[i] = Some(Instant::now());
                        got += 1;
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
                if shared.is_closed() {
                    break;
                }
            }
            let a = Arc::new(archives.clone());
            let format = client.format();
            match failure {
                None => publish(&|d| {
                    d.archives = a.clone();
                    d.format = format.or(d.format);
                    d.status = PerfStatus::Live;
                    d.updates += got;
                }),
                Some(e) => {
                    cached = None;
                    tracing::debug!(error = %e, "performance: update failed");
                    let text = e.to_string();
                    publish(&|d| {
                        d.archives = a.clone();
                        d.status = PerfStatus::Failed(text.clone());
                    });
                }
            }
        } else {
            publish(&|d| d.status = PerfStatus::NotRunning);
        }
        wait(&shared, interval).await;
    }
    tracing::debug!("performance updates stopped");
}

fn client_for(
    env: &PerfEnv,
    model: &ConnectionModel,
    host: &Ref<Host>,
    endpoint: &xapi::Endpoint,
) -> xen_rrd::Result<RrdClient> {
    let tls = if endpoint.tls {
        let accepted = accepted_fingerprints(model, host, endpoint, env.shared.store().trust());
        Some(
            xapi::pinned_client_config(accepted)
                .map_err(|e| xen_rrd::RrdError::Transport(e.to_string()))?,
        )
    } else {
        None
    };
    RrdClient::new(
        &endpoint.base_url(),
        tls,
        REQUEST_TIMEOUT,
        &env.shared.options.originator,
    )
}

/// The data sources and the whole RRD. The data sources are optional (the
/// graphs then guess units from the names).
async fn initial_load(
    client: &RrdClient,
    session: &Session,
    target: &PerfTarget,
    src: &Source,
) -> xen_rrd::Result<(xen_rrd::Rrd, Vec<DataSourceRecord>)> {
    let sources = match target {
        PerfTarget::Host(h) => xapi::host::get_data_sources(session, h).await,
        PerfTarget::Vm(v) if src.running => xapi::vm::get_data_sources(session, v).await,
        PerfTarget::Vm(_) => Ok(Vec::new()),
    };
    let sources = sources.unwrap_or_else(|e| {
        tracing::debug!(error = %e, "data sources unavailable");
        Vec::new()
    });
    let session_ref = session.session_ref().as_str();
    let rrd = match target {
        PerfTarget::Host(_) => client.host_rrd(session_ref).await?,
        PerfTarget::Vm(_) => client.vm_rrd(session_ref, &src.uuid).await?,
    };
    Ok((rrd, sources))
}

async fn closed(shared: &PerfShared) {
    loop {
        let notified = shared.close.notified();
        if shared.is_closed() {
            return;
        }
        notified.await;
    }
}

async fn wait(shared: &PerfShared, delay: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = closed(shared) => {}
    }
}

// ---- the default graphs ----------------------------------------------------

/// A graph of the Performance tab: its kind (for the title) and the data
/// sources it draws, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDef {
    pub kind: GraphKind,
    pub sources: Vec<String>,
}

/// The original's default graphs (`GraphList.LoadDefaultGraphs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphKind {
    Cpu,
    Memory,
    Network,
    Disk,
}

/// The default graphs of `target`: for a host, CPU (one line per CPU),
/// memory (total and free; drawn as used memory) and network (send and
/// receive per NIC); for a VM also disks (read and write per disk), with
/// one line per vCPU and per interface.
pub fn default_graphs(model: &ConnectionModel, target: &PerfTarget) -> Vec<GraphDef> {
    match target {
        PerfTarget::Host(h) => {
            let Some(host) = model.get(h) else {
                return Vec::new();
            };
            let mut cpus: Vec<i64> = host
                .host_cpus
                .iter()
                .filter_map(|c| model.get(c))
                .map(|c| c.number)
                .collect();
            cpus.sort_unstable();
            let mut pifs: Vec<String> = host
                .pifs
                .iter()
                .filter_map(|p| model.get(p))
                .map(|p| p.device.clone())
                .collect();
            pifs.sort();
            pifs.dedup();
            vec![
                GraphDef {
                    kind: GraphKind::Cpu,
                    sources: cpus.iter().map(|n| format!("cpu{n}")).collect(),
                },
                GraphDef {
                    kind: GraphKind::Memory,
                    sources: vec!["memory_total_kib".into(), "memory_free_kib".into()],
                },
                GraphDef {
                    kind: GraphKind::Network,
                    sources: pifs
                        .iter()
                        .flat_map(|d| [format!("pif_{d}_tx"), format!("pif_{d}_rx")])
                        .collect(),
                },
            ]
        }
        PerfTarget::Vm(v) => {
            let Some(vm) = model.get(v) else {
                return Vec::new();
            };
            let mut vifs: Vec<String> = vm
                .vifs
                .iter()
                .filter_map(|r| model.get(r))
                .map(|r| r.device.clone())
                .collect();
            vifs.sort_by_key(|d| (d.parse::<u64>().unwrap_or(u64::MAX), d.clone()));
            let mut vbds: Vec<String> = vm
                .vbds
                .iter()
                .filter_map(|r| model.get(r))
                .map(|r| r.device.clone())
                .filter(|d| !d.is_empty())
                .collect();
            vbds.sort();
            vec![
                GraphDef {
                    kind: GraphKind::Cpu,
                    sources: (0..vm.vcpus_at_startup.max(0))
                        .map(|i| format!("cpu{i}"))
                        .collect(),
                },
                GraphDef {
                    kind: GraphKind::Memory,
                    sources: vec!["memory".into(), "memory_internal_free".into()],
                },
                GraphDef {
                    kind: GraphKind::Network,
                    sources: vifs
                        .iter()
                        .flat_map(|d| [format!("vif_{d}_tx"), format!("vif_{d}_rx")])
                        .collect(),
                },
                GraphDef {
                    kind: GraphKind::Disk,
                    sources: vbds
                        .iter()
                        .flat_map(|d| [format!("vbd_{d}_read"), format!("vbd_{d}_write")])
                        .collect(),
                },
            ]
        }
    }
}

/// How a data source's values are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// 0–100 %.
    Percent,
    Bytes,
    BytesPerSecond,
    /// Anything else: the raw number.
    Plain,
}

/// The unit of data source `name` and the factor its raw values are
/// multiplied by (`DataSet`'s units table): fractions become percent, KiB
/// become bytes, MiB/s become B/s.
pub fn unit_of(name: &str, sources: &[DataSourceRecord]) -> (Unit, f64) {
    let units = sources
        .iter()
        .find(|s| s.name_label == name)
        .map(|s| s.units.as_str());
    match units {
        Some("percent") => (Unit::Percent, 1.0),
        Some("(fraction)") => (Unit::Percent, 100.0),
        Some("B") => (Unit::Bytes, 1.0),
        Some("KiB") => (Unit::Bytes, 1024.0),
        Some("B/s") => (Unit::BytesPerSecond, 1.0),
        Some("MiB/s") => (Unit::BytesPerSecond, 1024.0 * 1024.0),
        // A VM that just started may list its sources without units yet.
        Some("" | "unknown") => guess_unit(name),
        Some(_) => (Unit::Plain, 1.0),
        // Unknown units: from the names of the default graphs.
        None => guess_unit(name),
    }
}

fn guess_unit(name: &str) -> (Unit, f64) {
    let is_cpu = name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit());
    if is_cpu || name == "cpu_avg" || name == "cpu_usage" {
        (Unit::Percent, 100.0)
    } else if name.ends_with("_kib") || name == "memory_internal_free" {
        (Unit::Bytes, 1024.0)
    } else if name == "memory" || name == "memory_target" {
        (Unit::Bytes, 1.0)
    } else if (name.starts_with("pif_") || name.starts_with("vif_"))
        && (name.ends_with("_rx") || name.ends_with("_tx"))
        || name.starts_with("vbd_") && (name.ends_with("_read") || name.ends_with("_write"))
    {
        (Unit::BytesPerSecond, 1.0)
    } else {
        (Unit::Plain, 1.0)
    }
}

/// The points to draw for `name` between `from` and `to`, scaled to its
/// unit. Memory graphs draw used memory: `memory_free_kib` becomes
/// total − free (and `memory_internal_free` memory − free); the totals
/// themselves are not drawn (`DataSet.AddPoint`).
pub fn plot_points(
    archives: &Archives,
    sources: &[DataSourceRecord],
    name: &str,
    from: i64,
    to: i64,
) -> Vec<(i64, f64)> {
    let scaled = |n: &str| {
        let (_, factor) = unit_of(n, sources);
        archives
            .points(n, from, to)
            .into_iter()
            .map(move |(t, v)| (t, v * factor))
    };
    let used = |total: &str| {
        let totals: std::collections::HashMap<i64, f64> = scaled(total).collect();
        scaled(name)
            .map(|(t, free)| {
                let v = match totals.get(&t) {
                    Some(total) if total.is_finite() && free.is_finite() => total - free,
                    _ => f64::NAN,
                };
                (t, v)
            })
            .collect()
    };
    match name {
        "memory_free_kib" => used("memory_total_kib"),
        "memory_internal_free" => used("memory"),
        _ => scaled(name).collect(),
    }
}

/// Whether `name` is drawn (the totals of the memory graph are not).
pub fn is_drawn(name: &str) -> bool {
    name != "memory_total_kib" && name != "memory"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab() -> ConnectionModel {
        let text = include_str!("../../../tests/fixtures/records.json");
        ConnectionModel::from_database(&serde_json::from_str(text).unwrap())
    }

    #[test]
    fn default_graphs_follow_the_original() {
        let model = lab();
        let host = Ref::<Host>::new("OpaqueRef:host-0001");
        let graphs = default_graphs(&model, &PerfTarget::Host(host));
        let kinds: Vec<GraphKind> = graphs.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [GraphKind::Cpu, GraphKind::Memory, GraphKind::Network]
        );
        assert_eq!(graphs[0].sources, ["cpu0", "cpu1"]);
        assert_eq!(graphs[1].sources, ["memory_total_kib", "memory_free_kib"]);
        assert!(graphs[2].sources.contains(&"pif_eth0_tx".to_owned()));
        assert!(graphs[2].sources.contains(&"pif_eth1_rx".to_owned()));

        let (vm_ref, vm) = model
            .vms
            .iter()
            .find(|(_, v)| crate::model::is_guest(v) && !v.vifs.is_empty() && !v.vbds.is_empty())
            .unwrap();
        let graphs = default_graphs(&model, &PerfTarget::Vm(vm_ref.clone()));
        let kinds: Vec<GraphKind> = graphs.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [
                GraphKind::Cpu,
                GraphKind::Memory,
                GraphKind::Network,
                GraphKind::Disk
            ]
        );
        assert_eq!(graphs[0].sources.len() as i64, vm.vcpus_at_startup);
        assert_eq!(graphs[1].sources, ["memory", "memory_internal_free"]);
        assert!(graphs[2].sources[0].starts_with("vif_") && graphs[2].sources[0].ends_with("_tx"));
        assert!(graphs[3].sources.iter().all(|s| s.starts_with("vbd_")));
    }

    #[test]
    fn units_scale_like_the_original() {
        let ds = |name: &str, units: &str| DataSourceRecord {
            name_label: name.into(),
            units: units.into(),
            ..Default::default()
        };
        let sources = vec![
            ds("cpu0", "(fraction)"),
            ds("memory_free_kib", "KiB"),
            ds("x", "MiB/s"),
        ];
        assert_eq!(unit_of("cpu0", &sources), (Unit::Percent, 100.0));
        assert_eq!(unit_of("memory_free_kib", &sources), (Unit::Bytes, 1024.0));
        assert_eq!(unit_of("x", &sources), (Unit::BytesPerSecond, 1048576.0));
        // Without data sources, from the name.
        assert_eq!(unit_of("cpu3", &[]), (Unit::Percent, 100.0));
        assert_eq!(unit_of("memory", &[]), (Unit::Bytes, 1.0));
        assert_eq!(unit_of("vif_0_rx", &[]), (Unit::BytesPerSecond, 1.0));
        assert_eq!(unit_of("vbd_xvda_write", &[]), (Unit::BytesPerSecond, 1.0));
        assert_eq!(unit_of("loadavg", &[]), (Unit::Plain, 1.0));
        // Listed without units (a VM that just started): from the name.
        let bare = vec![ds("cpu1", ""), ds("vif_3_tx", "")];
        assert_eq!(unit_of("cpu1", &bare), (Unit::Percent, 100.0));
        assert_eq!(unit_of("vif_3_tx", &bare), (Unit::BytesPerSecond, 1.0));
    }

    #[test]
    fn memory_graphs_draw_used_memory() {
        let mut a = Archives::new();
        a.insert(
            Interval::FiveSeconds,
            "memory_total_kib",
            &[(10, 1000.0), (15, 1000.0)],
        );
        a.insert(
            Interval::FiveSeconds,
            "memory_free_kib",
            &[(10, 400.0), (15, f64::NAN)],
        );
        let p = plot_points(&a, &[], "memory_free_kib", 0, 20);
        assert_eq!(p[0], (10, 600.0 * 1024.0));
        assert!(p[1].1.is_nan());
        assert!(!is_drawn("memory_total_kib"));
        assert!(is_drawn("memory_free_kib"));
        let cpu = {
            a.insert(Interval::FiveSeconds, "cpu0", &[(10, 0.25)]);
            plot_points(&a, &[], "cpu0", 0, 20)
        };
        assert_eq!(cpu, [(10, 25.0)]);
    }

    #[test]
    fn requests_go_to_the_host_running_the_vm() {
        let model = lab();
        let connected = xapi::Endpoint::parse("https://192.0.2.11:8443").unwrap();
        let coordinator = model.pool().unwrap().1.master.clone();
        assert_eq!(host_endpoint(&model, &connected, &coordinator), connected);
        let other = model.hosts.keys().find(|h| **h != coordinator).unwrap();
        let e = host_endpoint(&model, &connected, other);
        assert_eq!((e.host.as_str(), e.port, e.tls), ("192.0.2.12", 8443, true));

        let (running, _) = model
            .vms
            .iter()
            .find(|(_, v)| crate::model::is_guest(v) && v.power_state == VmPowerState::Running)
            .unwrap();
        let s = source(&model, &PerfTarget::Vm(running.clone())).unwrap();
        assert!(s.running);
        assert_eq!(&s.host, &model.vms[running].resident_on);
        let (halted, _) = model
            .vms
            .iter()
            .find(|(_, v)| crate::model::is_guest(v) && v.power_state == VmPowerState::Halted)
            .unwrap();
        let s = source(&model, &PerfTarget::Vm(halted.clone())).unwrap();
        assert!(!s.running);
        assert_eq!(s.host, coordinator);
        assert_eq!(
            source(&model, &PerfTarget::Vm(Ref::new("OpaqueRef:nope"))),
            Err(PerfStatus::Gone)
        );
    }
}
