//! Helpers shared by the backend integration tests.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use xapi_mock::MockServer;
use xen_core::{
    AppState, BackendHandle, BackendOptions, Command, ConnectionView, MemorySecretStore,
    Notification, Store,
};

pub const WAIT: Duration = Duration::from_secs(10);

/// Options with short timeouts so failures and recoveries happen quickly.
pub fn fast_options(store: Arc<Store>) -> BackendOptions {
    BackendOptions {
        originator: "xen-core-tests".into(),
        store,
        event_timeout: 2.0,
        heartbeat_interval: Duration::from_millis(200),
        heartbeat_timeout: Duration::from_millis(500),
        reconnect_delay: Duration::from_millis(100),
        max_reconnect_delay: Duration::from_millis(400),
        task_poll_interval: Duration::from_millis(50),
        ..BackendOptions::default()
    }
}

/// An in-memory store (no files, no OS keyring) and its secret store.
pub fn memory_store() -> (Arc<Store>, Arc<MemorySecretStore>) {
    let secrets = Arc::new(MemorySecretStore::new());
    (Arc::new(Store::in_memory(secrets.clone())), secrets)
}

pub struct Harness {
    pub backend: BackendHandle,
    pub repaints: Arc<AtomicUsize>,
}

pub fn spawn(options: BackendOptions) -> Harness {
    let repaints = Arc::new(AtomicUsize::new(0));
    let counter = repaints.clone();
    let backend = BackendHandle::spawn(
        &tokio::runtime::Handle::current(),
        options,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    Harness { backend, repaints }
}

/// A backend with fast options and an in-memory store.
pub fn harness() -> Harness {
    spawn(fast_options(memory_store().0))
}

impl Harness {
    pub async fn wait_for(&self, what: &str, pred: impl Fn(&AppState) -> bool) -> Arc<AppState> {
        wait_state(&self.backend, what, pred).await
    }

    /// Waits until the first connection satisfies `pred`.
    pub async fn wait_conn(
        &self,
        what: &str,
        pred: impl Fn(&ConnectionView) -> bool,
    ) -> Arc<AppState> {
        self.wait_for(what, |s| s.connections.first().is_some_and(|c| pred(c)))
            .await
    }

    pub fn connect(&self, server: &MockServer, password: &str) {
        self.connect_to(&server.address(), password);
    }

    pub fn connect_to(&self, address: &str, password: &str) {
        self.backend.send(Command::Connect {
            server: address.to_owned(),
            username: xapi_mock::USERNAME.into(),
            password: Some(password.into()),
            remember_password: true,
        });
    }

    pub async fn next_notification(&mut self) -> Notification {
        tokio::time::timeout(WAIT, self.backend.recv())
            .await
            .expect("timed out waiting for a notification")
            .expect("backend stopped")
    }
}

pub async fn wait_state(
    backend: &BackendHandle,
    what: &str,
    pred: impl Fn(&AppState) -> bool,
) -> Arc<AppState> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let state = backend.state();
        if pred(&state) {
            return state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; connections {:#?}",
            state
                .connections
                .iter()
                .map(|c| (&c.server, &c.status))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn wait_until(what: &str, pred: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !pred() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The first connection of a snapshot.
pub fn first(state: &AppState) -> &ConnectionView {
    state.connections.first().expect("a connection")
}
