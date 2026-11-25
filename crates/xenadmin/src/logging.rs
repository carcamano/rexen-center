//! Logging: to stderr and, when there is a configuration directory, to a
//! daily-rotated file in its `logs` folder (the last 7 days are kept).
//!
//! The level comes from `RUST_LOG` (default
//! `warn,xapi=info,xen_core=info,xenadmin=info`). Passwords and session
//! references are never logged: calls are traced by method name only, the
//! password type redacts itself and the XAPI client redacts session
//! references out of `SESSION_INVALID` errors.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};
use xen_core::ConfigPaths;

const DEFAULT_FILTER: &str = "warn,xapi=info,xen_core=info,xenadmin=info";

/// Installs the global subscriber. Keep the returned guard alive until exit
/// so buffered file output is flushed.
pub fn init(paths: Option<&ConfigPaths>) -> Option<WorkerGuard> {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());
    let file = paths.and_then(|p| {
        if let Err(e) = std::fs::create_dir_all(p.log_dir()) {
            eprintln!("file logging disabled: {e}");
            return None;
        }
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(crate::branding::PRODUCT_NAME)
            .filename_suffix("log")
            .max_log_files(7)
            .build(p.log_dir())
            .map_err(|e| eprintln!("file logging disabled: {e}"))
            .ok()
    });
    let (file_layer, guard) = match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();
    guard
}
