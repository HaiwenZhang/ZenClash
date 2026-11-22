//! Native GPUI application shell, components, and features for `ZenClash`.

#![deny(missing_docs)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI handlers conventionally receive mutable entities and Context values even when a particular render path only uses shared methods"
)]

/// Layer 1: reusable GPUI components and stateless page/form building blocks.
pub mod components;
/// Layer 3: horizontally isolated feature views and the `RuntimePage` host.
pub mod features;
/// Layer 0: shared actions, icon assets, design tokens, and UI primitives.
pub mod foundation;
/// Layer 2: runtime-page state machines, observation readers, and lifecycle schedules.
pub mod runtime_host;
/// Layer 4: desktop shell, window lifecycle, background controllers, and native tray.
pub mod shell;

/// Unified isolated UI integration test fixtures (only enabled under tests or `test-support`).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use foundation::{actions, assets, design};
pub use shell::app;
pub use shell::tray::native as tray;

pub(crate) use zenclash_app::commands as application_commands;
pub use zenclash_app::{ApplicationCommands, outbound_mode};

#[cfg(test)]
#[path = "../build_version.rs"]
mod build_version;
