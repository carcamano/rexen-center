//! The performance figures of the Search tab's columns (CPU usage, used
//! memory, disk and network throughput), as the original's
//! `MetricUpdater`: every 30 seconds (six performance intervals) one
//! `rrd_updates?host=true` request per live host of the connection, all at
//! once, whose
//! newest row gives the latest value of every data source of that host
//! and of the VMs running on it.
//!
//! The UI makes a [`MetricsHandle`] / [`MetricsLink`] pair with [`pair`],
//! keeps the handle while the Search tab is shown and gives the link to
//! the backend in [`Command::WatchSearchMetrics`](crate::Command); dropping
//! the handle stops the requests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use xen_rrd::{RrdClient, UpdatesQuery};

use crate::backend::slot_get;
use crate::performance::PerfEnv;

/// Latest values by object UUID, then data source name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub values: HashMap<String, HashMap<String, f64>>,
    /// How many rounds have completed.
    pub rounds: u64,
}

impl Metrics {
    /// `MetricUpdater.GetValue`: `None` when unknown.
    pub fn get(&self, uuid: &str, name: &str) -> Option<f64> {
        self.values
            .get(uuid)?
            .get(name)
            .copied()
            .filter(|v| !v.is_nan())
    }
}

#[derive(Debug, Default)]
struct MetricsShared {
    data: Mutex<Arc<Metrics>>,
    closed: AtomicBool,
}

/// The UI's side; dropping it stops the requests.
#[derive(Debug)]
pub struct MetricsHandle {
    shared: Arc<MetricsShared>,
}

impl Drop for MetricsHandle {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

impl MetricsHandle {
    /// The latest figures.
    pub fn metrics(&self) -> Arc<Metrics> {
        self.shared
            .data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The backend's side.
#[derive(Debug)]
pub struct MetricsLink {
    shared: Arc<MetricsShared>,
}

/// A new handle and its link.
pub fn pair() -> (MetricsHandle, MetricsLink) {
    let shared = Arc::new(MetricsShared::default());
    (
        MetricsHandle {
            shared: shared.clone(),
        },
        MetricsLink { shared },
    )
}

/// Keeps one connection's figures up to date until the handle is dropped
/// or the connection is removed.
pub(crate) async fn run(env: PerfEnv, link: MetricsLink) {
    let shared = link.shared;
    let every = env.shared.options.perf_interval * 6;
    let repaint = env.shared.repaint_fn();
    let mut clients: HashMap<String, RrdClient> = HashMap::new();
    let mut last: HashMap<String, i64> = HashMap::new();
    while !shared.closed.load(Ordering::Relaxed) {
        let state = env.shared.state();
        let Some(view) = state.connection(env.connection) else {
            break;
        };
        let model = view.model.clone();
        let connected = view.status.is_connected();
        drop(state);
        if connected
            && let (Some(session), Some(endpoint)) = (slot_get(&env.session), env.endpoint_pub())
        {
            let mut values = shared
                .data
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values
                .clone();
            // Every host at once, as the original's `Parallel.ForEach`: a
            // server that does not answer must not hold up the others'
            // figures until its request times out.
            let mut requests = tokio::task::JoinSet::new();
            for (host_ref, host) in &model.hosts {
                if !crate::objects::host_is_live(&model, host) {
                    continue;
                }
                let ep = crate::performance::host_endpoint(&model, &endpoint, host_ref);
                let key = ep.authority();
                let client = match clients.get(&key) {
                    Some(c) => c.clone(),
                    None => match env.client(&model, host_ref, &ep) {
                        Ok(c) => {
                            clients.insert(key.clone(), c.clone());
                            c
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "search metrics: no client");
                            continue;
                        }
                    },
                };
                let query = UpdatesQuery {
                    start: last.get(&key).copied().unwrap_or(0),
                    interval: 5,
                    cf: xen_rrd::Cf::Average,
                    host: true,
                    vm_uuid: None,
                };
                let session = session.clone();
                requests.spawn(async move {
                    let result = client.updates(session.session_ref().as_str(), &query).await;
                    (key, result)
                });
            }
            while let Some(done) = requests.join_next().await {
                let Ok((key, result)) = done else {
                    continue;
                };
                match result {
                    Ok(updates) => {
                        // Rows come newest first; take each column's newest value.
                        let mut newest: Vec<Option<f64>> = vec![None; updates.legend.len()];
                        let mut rows = updates.rows.clone();
                        rows.sort_by_key(|r| std::cmp::Reverse(r.t));
                        for row in &rows {
                            for (i, v) in row.values.iter().enumerate() {
                                if newest.get(i).is_some_and(Option::is_none) && !v.is_nan() {
                                    newest[i] = Some(*v);
                                }
                            }
                        }
                        if let Some(t) = rows.first().map(|r| r.t) {
                            last.insert(key.clone(), t - 60);
                        }
                        for (id, v) in updates.legend.iter().zip(newest) {
                            if let Some(v) = v {
                                values
                                    .entry(id.uuid.clone())
                                    .or_default()
                                    .insert(id.name.clone(), v);
                            }
                        }
                        // Each host's figures show as soon as they come.
                        let mut data = shared.data.lock().unwrap_or_else(PoisonError::into_inner);
                        let rounds = data.rounds;
                        *data = Arc::new(Metrics {
                            values: values.clone(),
                            rounds,
                        });
                        drop(data);
                        repaint();
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "search metrics: request failed");
                        clients.remove(&key);
                    }
                }
                if shared.closed.load(Ordering::Relaxed) {
                    return;
                }
            }
            let mut data = shared.data.lock().unwrap_or_else(PoisonError::into_inner);
            let rounds = data.rounds + 1;
            *data = Arc::new(Metrics { values, rounds });
            drop(data);
            repaint();
        }
        let deadline = tokio::time::Instant::now() + every;
        while tokio::time::Instant::now() < deadline {
            if shared.closed.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200).min(every)).await;
        }
    }
}
