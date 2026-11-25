//! Layer 4: desktop shell, window lifecycle, background controllers, platform glue, and native tray.

/// Root GPUI application entity, window lifecycle, controllers, and tray coordinator.
pub mod app;
/// Platform-specific window, directory opener, and startup-failure reporting helpers.
pub mod platform;
/// Native operating-system status tray icon, menu builder, and command bindings.
pub mod tray_native;

pub use app::{
    AppServices, BootstrappedApplication, RestartRequest, TrafficHistorySession, ZenClashApp,
    create_main_window, create_main_window_with_pending_startup,
    create_main_window_with_service_startup, finish_native_shutdown, init, reopen_main_window,
};
pub use tray_native as tray;
