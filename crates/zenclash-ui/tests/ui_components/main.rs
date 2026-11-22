//! Tier 1 black-box GPUI headless component integration tests.
//!
//! Runs with default crate features (zero `test-support`, zero Tokio, zero Core)
//! to validate theme tokens, surface primitives, sidebar navigation, and controls.

mod controls;
mod sidebar_navigation;
mod theme_and_surface;
