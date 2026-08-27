//! Native GPUI application shell, components, and pages for `ZenClash`.

#![deny(missing_docs)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "GPUI handlers conventionally receive mutable entities and Context values even when a particular render path only uses shared methods"
)]

/// Application lifecycle, windows, actions, and native tray coordination.
pub mod app;
/// Embedded application-owned and component icon assets.
pub mod assets;
/// Reusable GPUI widgets used by the application shell and pages.
pub mod components;
/// `ZenClash` colors and gpui-component theme configuration.
pub mod design;
/// Navigable application pages and their live Mihomo views.
pub mod pages;
