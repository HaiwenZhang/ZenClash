//! Layer 3: horizontally isolated feature views and the `RuntimePage` host.

pub(crate) mod capture;
pub(crate) mod connections;
pub(crate) mod home;
/// Stateful GPUI host for runtime-core configuration, status, and diagnostics pages.
pub mod host;
pub(crate) mod kernel;
pub(crate) mod logs;
pub(crate) mod network;
pub(crate) mod overrides;
pub(crate) mod profiles;
/// Live proxy-group selection page.
pub mod proxies;
pub(crate) mod resources;
pub(crate) mod rules;
pub(crate) mod settings;
pub(crate) mod traffic;
