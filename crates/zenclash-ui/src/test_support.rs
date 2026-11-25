//! Unified isolated test fixtures shared across runtime host, feature, and shell tests.
//!
//! Available only under `cfg(test)` or the non-production `test-support` feature.

/// Loopback mock Mihomo HTTP controller fixture.
pub mod controller;
#[cfg(test)]
pub(crate) mod owned_cores;
/// Loopback proxy-group controller fixture and headless `ProxiesPage` helpers.
pub mod proxies;
/// Loopback resource/provider controller fixture.
pub mod resources;
/// Isolated `RuntimePage` fixture and headless window harness helpers.
pub mod runtime_host;
#[cfg(test)]
pub(crate) mod service_workflow;
/// Isolated `ZenClashApp` shell, native tray menu, and preference-preview test harness.
pub mod shell_tray;

pub use controller::ControllerFixture;
#[cfg(test)]
pub(crate) use owned_cores::owned_ui_children;
pub use runtime_host::{RuntimePageFixture, open_runtime_page_window};
#[cfg(test)]
pub(crate) use service_workflow::{open_service_page, open_service_tun};
