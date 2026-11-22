//! Layer 4: desktop shell, window lifecycle, background controllers, platform glue, and native tray.

pub(in crate::shell) mod actions;
/// Root GPUI application entity and public bootstrap/shutdown entry points.
pub mod app;
pub(in crate::shell) mod bootstrap;
pub(in crate::shell) mod controllers;
pub(crate) mod navigation;
/// Platform-specific window, directory opener, and startup-failure reporting helpers.
pub mod platform;
/// Unified native tray icon, menu builder, and shell tray controller.
pub mod tray;
pub(in crate::shell) mod view;
pub(in crate::shell) mod window;

pub use app::{
    AppServices, BootstrappedApplication, RestartRequest, TrafficHistorySession, ZenClashApp,
    create_main_window, create_main_window_with_pending_startup,
    create_main_window_with_service_startup, finish_native_shutdown, init, reopen_main_window,
};
