//! Tier 3 black-box GPUI headless shell integration tests (`required-features = ["test-support"]`).
//!
//! Exercises full `ZenClashApp` navigation, theme/sidebar chrome, Safe Mode policy,
//! native tray menu bindings, preference previews, and outbound-mode coordination.

mod app_navigation_and_window;
mod tray_and_preferences;
