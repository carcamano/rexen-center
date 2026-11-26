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
//!
//! Release builds for Windows use the GUI subsystem (no console window);
//! the command-line modes then report through their exit code only.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ad_ui;
mod alerts_view;
mod app;
mod branding;
mod cli;
mod console_keys;
mod console_view;
mod cpm_strings;
mod dev_strings;
mod dev_ui;
mod dr_strings;
mod dr_ui;
mod events;
mod file_dialog;
mod friendly_errors;
mod ha_strings;
mod ha_ui;
mod i18n;
mod icons;
mod locale;
mod logging;
mod message_names;
mod migrate_ui;
mod net_strings;
mod net_ui;
mod new_network;
mod new_sr;
mod new_vm;
mod ops_strings;
mod ops_ui;
mod org_strings;
mod org_ui;
mod ovf_strings;
mod ovf_ui;
mod perf_strings;
mod perf_view;
mod perfmon_ui;
mod pool_strings;
mod pool_ui;
mod props_ui;
mod prov_strings;
mod provision_ui;
mod search_ui;
mod sr_strings;
mod storage_ui;
mod tab_strings;
mod tabs;
mod theme;
mod vapp_strings;
mod vapp_ui;
mod vmss_strings;
mod vmss_ui;
mod xva_strings;
mod xva_ui;

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
    if let cli::Cli::Mcp { config } = command {
        std::process::exit(run_mcp(paths, config));
    }
    let _log_guard = logging::init(paths.as_ref(), branding::PRODUCT_NAME);

    let service = keyring_service();
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
            .with_min_inner_size([640.0, 400.0])
            .with_icon(window_icon()),
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

/// The keyring service name: `xenadmin-rs`, or `XENADMIN_KEYRING_SERVICE`.
fn keyring_service() -> String {
    std::env::var(KEYRING_SERVICE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| branding::PRODUCT_NAME.to_owned())
}

/// `--mcp`: the MCP server on stdin/stdout, with the saved servers, pins
/// and keyring read only; returns the exit code.
fn run_mcp(paths: Option<ConfigPaths>, config: Option<std::path::PathBuf>) -> i32 {
    let _log_guard = logging::init(paths.as_ref(), &format!("{}-mcp", branding::PRODUCT_NAME));
    let Some(paths) = paths else {
        eprintln!("no configuration directory: the MCP server needs the saved servers");
        return 1;
    };
    let config_file = config.unwrap_or_else(|| paths.dir.join(xen_mcp::config::CONFIG_FILE));
    let config = match xen_mcp::McpConfig::load(&config_file) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "MCP configuration");
            eprintln!("invalid MCP configuration: {e}");
            return 2;
        }
    };
    tracing::info!(
        dir = %paths.dir.display(),
        config = %config_file.display(),
        writes = config.writes.enabled,
        "MCP server starting"
    );
    let audit = match xen_mcp::AuditLog::in_dir(&paths.log_dir()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "cannot open the audit log in {}: {e}",
                paths.log_dir().display()
            );
            return 1;
        }
    };
    let secrets: Arc<dyn SecretStore> = Arc::new(xen_mcp::ReadOnlySecrets(
        KeyringSecretStore::new(keyring_service()),
    ));
    let store = Arc::new(Store::open_read_only(paths, secrets));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("xen-mcp")
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to start the async runtime: {e}");
            return 1;
        }
    };
    let run = xen_mcp::run_stdio(
        store,
        branding::originator(),
        config,
        audit,
        shutdown_signal(),
    );
    let code = match runtime.block_on(run) {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!(error = %e, "MCP transport");
            1
        }
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    code
}

/// The generic application icon (`packaging/icons/make_icon.py`).
fn window_icon() -> Arc<egui::IconData> {
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        Ok(icon) => Arc::new(icon),
        Err(e) => {
            tracing::warn!(error = %e, "window icon");
            Arc::new(egui::IconData::default())
        }
    }
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
