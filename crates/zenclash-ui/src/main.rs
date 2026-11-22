#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Native `ZenClash` executable entry point.
//!
//! Initializes logging, Tokio runtime, Safe Mode policy, and headless bootstrap
//! via [`zenclash_app::bootstrap::BootstrapOrchestrator`], then launches the
//! GPUI desktop shell and performs post-exit shutdown and optional restart.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracing_subscriber::{EnvFilter, filter::Directive};
use zenclash_app::{
    SafeMode,
    bootstrap::{
        BootstrapCleanup, BootstrapOrchestrator, StartupCancellation, spawn_restarted_process,
    },
};
use zenclash_core::AppInstanceLockError;
use zenclash_ui::{app, assets::Assets};

mod startup_failure;

const DEFAULT_TRACING_FILTER: &str =
    "zenclash=info,zenclash_app=info,zenclash_core=info,zenclash_ui=info";
const QUIET_NETWORK_TARGETS: [&str; 6] = [
    "tokio_tungstenite=warn",
    "tungstenite=warn",
    "reqwest=warn",
    "hyper=warn",
    "h2=warn",
    "rustls=warn",
];

/// Failures after the event loop starts are shutdown errors; a native dialog
/// would outlive its window, so those keep the stderr report only.
static GUI_STARTED: AtomicBool = AtomicBool::new(false);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_filter(std::env::var("RUST_LOG").ok().as_deref()))
        .init();

    if let Err(error) = run() {
        tracing::error!(%error, "ZenClash startup failed");
        let message = startup_failure_message(error.as_ref());
        eprintln!("{message}");
        if !GUI_STARTED.load(Ordering::Acquire) {
            startup_failure::show(&message);
        }
    }
}

/// Selects the user-facing startup message without exposing internal lock paths.
fn startup_failure_message(error: &(dyn std::error::Error + 'static)) -> String {
    if matches!(
        error.downcast_ref::<AppInstanceLockError>(),
        Some(AppInstanceLockError::AlreadyRunning(_))
    ) {
        zenclash_i18n::text("startup.already_running")
    } else {
        zenclash_i18n::text_with("startup.failure", &[("error", error.to_string())])
    }
}

fn tracing_filter(requested: Option<&str>) -> EnvFilter {
    let mut filter = requested
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_TRACING_FILTER));
    for value in QUIET_NETWORK_TARGETS {
        if let Ok(directive) = value.parse::<Directive>() {
            // Protocol frame dumps can recursively enter a controller's `/logs`
            // WebSocket. Keep those targets quiet even when application debug
            // logging is requested through `RUST_LOG`.
            filter = filter.add_directive(directive);
        }
    }
    filter
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let safe_mode = SafeMode::detect();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("zenclash-io")
            .build()?,
    );
    let _runtime_guard = runtime.enter();
    if safe_mode.allows_privileged_service() {
        runtime.block_on(zenclash_core::configure_service_ipc());
    }
    let (inputs, _instance_lock) = BootstrapOrchestrator::discover_inputs(safe_mode)?;
    let runtime_handle = runtime.handle().clone();
    let restart_after_exit = Arc::new(parking_lot::Mutex::new(None));
    let cancellation = StartupCancellation::new();
    let cleanup: BootstrapCleanup = Arc::new(parking_lot::Mutex::new(None));
    let pending_services = BootstrapOrchestrator::prepare_pending_services(
        &inputs,
        &runtime_handle,
        restart_after_exit.clone(),
    )?;
    let (finished_sender, finished_receiver) = tokio::sync::oneshot::channel();
    let worker_runtime = runtime.clone();
    let worker_cancellation = cancellation.clone();
    let worker_cleanup = cleanup.clone();
    let worker_restart = restart_after_exit.clone();
    let startup_task = runtime.spawn_blocking(move || {
        let outcome = BootstrapOrchestrator::prepare_application(
            &worker_runtime,
            inputs,
            worker_cancellation,
            worker_cleanup,
            worker_restart,
        );
        let _ = finished_sender.send(());
        outcome.into_application_result()
    });

    let application = gpui_kit::application().with_assets(Assets);
    application.on_reopen(app::reopen_main_window);
    GUI_STARTED.store(true, Ordering::Release);
    application.run(move |cx| {
        app::init(cx);
        app::create_main_window_with_pending_startup(pending_services, startup_task, cx);
        cx.activate(true);
    });
    // GPUI's native quit observers have a 200 ms deadline. Finish on Tokio after
    // the event loop returns, before dropping the runtime or allowing restart.
    cancellation.cancel();
    let shutdown_result = runtime.block_on(async {
        let _ = finished_receiver.await;
        let completed = cleanup.lock().take();
        if let Some(owner) = completed {
            owner.finish_native_shutdown().await
        } else {
            Ok(())
        }
    });
    shutdown_result.map_err(std::io::Error::other)?;
    drop(_instance_lock);
    if let Some(request) = restart_after_exit.lock().take() {
        spawn_restarted_process(&request).map_err(|error| {
            std::io::Error::other(zenclash_i18n::text_with(
                "startup.restart_failed",
                &[
                    ("path", request.executable.display().to_string()),
                    ("error", error.to_string()),
                ],
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use zenclash_core::AppInstanceLockError;

    use super::{startup_failure_message, tracing_filter};

    #[test]
    fn application_filter_overrides_verbose_protocol_targets() {
        let filter = tracing_filter(Some("debug,tungstenite=trace,tokio_tungstenite=trace"));
        let filter = filter.to_string();

        assert!(filter.contains("tungstenite=warn"));
        assert!(filter.contains("tokio_tungstenite=warn"));
        assert!(filter.contains("reqwest=warn"));
        assert!(!filter.contains("tungstenite=trace"));
        assert!(!filter.contains("tokio_tungstenite=trace"));
    }

    #[test]
    fn already_running_uses_a_dedicated_message_without_the_lock_path() {
        let error: Box<dyn std::error::Error> = Box::new(AppInstanceLockError::AlreadyRunning(
            "/tmp/zenclash-private.lock".into(),
        ));
        let message = startup_failure_message(error.as_ref());
        assert!(!message.contains("zenclash-private.lock"));
        assert!(message.contains("ZenClash"));
    }

    #[test]
    fn other_failures_keep_the_original_error_detail() {
        let error: Box<dyn std::error::Error> = Box::new(std::io::Error::other("disk unavailable"));
        let message = startup_failure_message(error.as_ref());
        assert!(message.contains("disk unavailable"));
    }
}
