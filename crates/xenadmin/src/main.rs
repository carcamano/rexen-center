//! Desktop management console for XCP-ng pools.
//!
//! Threading (port plan §4.2): `main` builds a multi-threaded tokio runtime
//! whose worker threads run the XAPI backend; eframe runs on the main thread
//! (required on macOS). The UI talks to the backend only through
//! [`xen_core::BackendHandle`].

mod app;
mod branding;
mod i18n;
mod icons;

use std::sync::Arc;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,xapi=info,xen_core=info,xenadmin=info")
            }),
        )
        .init();

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
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        branding::PRODUCT_NAME,
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let backend = xen_core::BackendHandle::spawn(
                &handle,
                xen_core::BackendOptions {
                    originator: branding::originator(),
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
