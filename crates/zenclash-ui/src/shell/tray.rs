//! Unified native status tray implementation and shell tray controller.

pub(in crate::shell) mod commands;
pub(in crate::shell) mod controller;
pub(in crate::shell) mod directories;
/// Native operating-system status tray icon, menu builder, and command bindings.
pub mod native;
pub(in crate::shell) mod proxy_commands;
pub(in crate::shell) mod queue;
pub(in crate::shell) mod refresh;
pub(in crate::shell) mod runtime_observation;

pub(in crate::shell) use controller::TrayController;
pub(in crate::shell) use queue::LatestCommandQueue;
