//! Desktop management console for XCP-ng pools.
//!
//! Threading (port plan §4.2): `main` builds a multi-threaded tokio runtime
//! whose worker threads run the XAPI backend; eframe runs on the main thread
//! (required on macOS). The UI talks to the backend only through
//! [`xen_core::BackendHandle`].
//!
//! Settings live in the configuration directory chosen by
//! [`xen_core::ConfigPaths::resolve`] (portable mode when a `portable` file
//! sits next to the executable; `XENADMIN_CONFIG_DIR` overrides it), saved
//! passwords in the OS keyring (service name `xenadmin-rs`, or
//! `XENADMIN_KEYRING_SERVICE`), and logs in its `logs` folder.

mod alerts_view;
mod app;
mod branding;
mod cli;
mod console_keys;
mod console_view;
mod events;
mod friendly_errors;
mod i18n;
mod icons;
mod logging;
mod message_names;
mod new_sr;
mod new_vm;
mod ops_strings;
mod ops_ui;
mod perf_strings;
mod perf_view;
mod perfmon_ui;
mod prov_strings;
mod provision_ui;
mod sr_strings;
mod storage_ui;
mod tab_strings;
mod tabs;

use std::sync::Arc;

use xen_core::{ConfigPaths, KeyringSecretStore, MemorySecretStore, SecretStore, Store};

/// Overrides the keyring service name (e.g. to keep a test profile apart).
const KEYRING_SERVICE_ENV: &str = "XENADMIN_KEYRING_SERVICE";

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };
    if matches!(command, cli::Cli::Help) {
        println!("{}", cli::USAGE);
        return Ok(());
    }
    let paths = ConfigPaths::resolve(branding::PRODUCT_NAME);
    let _log_guard = logging::init(paths.as_ref());

    let service = std::env::var(KEYRING_SERVICE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| branding::PRODUCT_NAME.to_owned());
    let store = match paths {
        Some(paths) => {
            tracing::info!(dir = %paths.dir.display(), location = ?paths.location, "configuration");
            let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new(service));
            Store::open(paths, secrets)
        }
        None => {
            tracing::warn!("no configuration directory; settings are kept in memory");
            Store::in_memory(Arc::new(MemorySecretStore::new()))
        }
    };

    if !matches!(command, cli::Cli::Gui) {
        std::process::exit(cli::run(command, &store));
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("xapi-backend")
        .enable_all()
        .build()
        .expect("failed to start the async runtime");
    let handle = runtime.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(branding::PRODUCT_NAME)
            .with_app_id(branding::PRODUCT_NAME)
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    let store = Arc::new(store);
    let result = eframe::run_native(
        branding::PRODUCT_NAME,
        options,
        Box::new(move |cc| {
            // Ctrl+C or SIGTERM close the window normally, which logs out.
            let closer = cc.egui_ctx.clone();
            handle.spawn(async move {
                shutdown_signal().await;
                tracing::info!("termination signal; closing");
                closer.send_viewport_cmd(egui::ViewportCommand::Close);
                closer.request_repaint();
            });
            let ctx = cc.egui_ctx.clone();
            let backend = xen_core::BackendHandle::spawn(
                &handle,
                xen_core::BackendOptions {
                    originator: branding::originator(),
                    store,
                    ..Default::default()
                },
                Arc::new(move || ctx.request_repaint()),
            );
            Ok(Box::new(app::XenAdminApp::new(backend)))
        }),
    );
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

/// Resolves on Ctrl+C, or SIGTERM on Unix.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
