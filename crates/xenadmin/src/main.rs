//! ReXen Center: a desktop management console for XCP-ng pools.
//!
//! Threading (port plan §4.2): `main` builds a multi-threaded tokio runtime
//! whose worker threads run the XAPI backend; eframe runs on the main thread
//! (required on macOS). The UI talks to the backend only through
//! [`xen_core::BackendHandle`].
//!
//! Settings live in the configuration directory chosen by
//! [`xen_core::ConfigPaths::resolve`] (portable mode when a `portable` file
//! sits next to the executable; `REXENCENTER_CONFIG_DIR` overrides it), saved
//! passwords in the OS keyring (service name `rexen-center`, or
//! `REXENCENTER_KEYRING_SERVICE`), and logs in its `logs` folder. The former
//! variable names `XENADMIN_CONFIG_DIR` and `XENADMIN_KEYRING_SERVICE` are
//! still honoured when the new ones are not set.
//!
//! On the first start under the per-user folder, the settings and saved
//! passwords of the console's former name (`xenadmin-rs`) are copied over
//! ([`xen_core::migration`]); the old ones are left in place.
//!
//! Release builds for Windows use the GUI subsystem (no console window);
//! the command-line modes then report through their exit code only.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about;
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
mod window_frame;
mod xva_strings;
mod xva_ui;

use std::sync::Arc;

use xen_core::branding::LEGACY_APP_ID;
use xen_core::migration::Migrated;
use xen_core::{
    ConfigLocation, ConfigPaths, KeyringSecretStore, MemorySecretStore, SecretStore, Store,
};

/// Overrides the keyring service name (e.g. to keep a test profile apart).
const KEYRING_SERVICE_ENV: &str = "REXENCENTER_KEYRING_SERVICE";

/// [`KEYRING_SERVICE_ENV`]'s former name: honoured when the new one is not
/// set, and it names the service the legacy passwords are copied from.
const LEGACY_KEYRING_SERVICE_ENV: &str = "XENADMIN_KEYRING_SERVICE";

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n{}", cli::usage());
            std::process::exit(2);
        }
    };
    if matches!(command, cli::Cli::Help) {
        println!("{}", cli::usage());
        return Ok(());
    }
    let paths = ConfigPaths::resolve(branding::APP_ID);
    let service = keyring_service();
    // Before logging starts: the log folder would create the new folder.
    let migration = migrate_legacy(paths.as_ref(), &service);
    if let cli::Cli::Mcp { config } = command {
        std::process::exit(run_mcp(paths, config, migration));
    }
    let _log_guard = logging::init(paths.as_ref(), branding::APP_ID);
    report_migration(migration);

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

    // GNOME and KDE Plasma: the window draws its own title bar.
    let window_frame = window_frame::detect();
    if let Some(frame) = &window_frame {
        tracing::info!(flavor = ?frame.flavor, layout = ?frame.layout, "own title bar");
    }
    let options = eframe::NativeOptions {
        viewport: viewport(window_frame.is_some()),
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
            Ok(Box::new(
                app::XenAdminApp::new(backend).with_window_frame(window_frame),
            ))
        }),
    );
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}

/// The first non-empty environment variable of `names`.
fn env_value(names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|n| std::env::var(n).ok())
        .find(|v| !v.is_empty())
}

/// The keyring service name: `rexen-center`, or `REXENCENTER_KEYRING_SERVICE`
/// (or the former `XENADMIN_KEYRING_SERVICE`).
fn keyring_service() -> String {
    env_value(&[KEYRING_SERVICE_ENV, LEGACY_KEYRING_SERVICE_ENV])
        .unwrap_or_else(|| branding::APP_ID.to_owned())
}

/// The keyring service the former name saved passwords under:
/// `xenadmin-rs`, or `XENADMIN_KEYRING_SERVICE`.
fn legacy_keyring_service() -> String {
    env_value(&[LEGACY_KEYRING_SERVICE_ENV]).unwrap_or_else(|| LEGACY_APP_ID.to_owned())
}

/// Copies the former name's per-user settings and saved passwords on the
/// first start ([`xen_core::migration`]). Only the per-user folder migrates:
/// a portable `settings` folder keeps its place and an explicit folder
/// (`REXENCENTER_CONFIG_DIR`) is used as it is.
fn migrate_legacy(paths: Option<&ConfigPaths>, service: &str) -> Option<Result<Migrated, String>> {
    let paths = paths.filter(|p| p.location == ConfigLocation::User)?;
    let old = ConfigPaths::user_dir(LEGACY_APP_ID)?;
    xen_core::migration::migrate_legacy_settings(
        &old,
        &paths.dir,
        &KeyringSecretStore::new(legacy_keyring_service()),
        &KeyringSecretStore::new(service),
    )
    .map_err(|e| format!("could not migrate the settings from {}: {e}", old.display()))
    .transpose()
}

/// Logs what [`migrate_legacy`] did (once logging is set up).
fn report_migration(migration: Option<Result<Migrated, String>>) {
    match migration {
        Some(Ok(done)) => {
            tracing::info!("{done}");
            for error in &done.password_errors {
                tracing::warn!(%error, "saved password not migrated");
            }
        }
        Some(Err(e)) => tracing::warn!("{e}"),
        None => {}
    }
}

/// `--mcp`: the MCP server on stdin/stdout, with the saved servers, pins
/// and keyring read only; returns the exit code.
fn run_mcp(
    paths: Option<ConfigPaths>,
    config: Option<std::path::PathBuf>,
    migration: Option<Result<Migrated, String>>,
) -> i32 {
    let _log_guard = logging::init(paths.as_ref(), &format!("{}-mcp", branding::APP_ID));
    report_migration(migration);
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

/// The main window: titled with the product's name. With `own_title_bar`
/// (GNOME, KDE Plasma) it has no system frame and a transparent background,
/// so that the title bar drawn by the app gives it rounded corners.
fn viewport(own_title_bar: bool) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(branding::PRODUCT_NAME)
        .with_app_id(branding::APP_ID)
        .with_inner_size([1100.0, 760.0])
        .with_min_inner_size([640.0, 400.0])
        .with_icon(window_icon())
        .with_decorations(!own_title_bar)
        .with_transparent(own_title_bar)
}

/// The application icon ([`branding::ICON_PNG`]).
fn window_icon() -> Arc<egui::IconData> {
    match branding::icon_data() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_titled_with_the_product_name() {
        let viewport = viewport(false);
        assert_eq!(viewport.title.as_deref(), Some("ReXen Center"));
        assert_eq!(viewport.app_id.as_deref(), Some("rexen-center"));
        assert_eq!(viewport.decorations, Some(true));
        let own = super::viewport(true);
        assert_eq!(
            (own.decorations, own.transparent),
            (Some(false), Some(true))
        );
        assert_eq!(
            branding::originator(),
            format!("rexen-center/{}", branding::VERSION)
        );
    }

    #[test]
    fn only_the_per_user_folder_migrates() {
        // Explicit and portable folders are used as they are; the legacy
        // per-user folder is never looked at for them.
        let dir =
            std::env::temp_dir().join(format!("rexencenter-no-migration-{}", std::process::id()));
        let explicit = ConfigPaths::at(&dir);
        assert!(migrate_legacy(Some(&explicit), "rexen-center-test (unused)").is_none());
        let portable = ConfigPaths {
            dir: dir.clone(),
            location: ConfigLocation::Portable,
        };
        assert!(migrate_legacy(Some(&portable), "rexen-center-test (unused)").is_none());
        assert!(migrate_legacy(None, "rexen-center-test (unused)").is_none());
        assert!(!dir.exists());
    }
}
