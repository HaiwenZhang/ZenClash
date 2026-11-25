//! Shell tray controller, command queues, menu refresh, and runtime observation.

pub(in crate::shell) mod commands;
pub(in crate::shell) mod controller;
pub(in crate::shell) mod directories;
pub(in crate::shell) mod proxy_commands;
pub(in crate::shell) mod queue;
pub(in crate::shell) mod refresh;
pub(in crate::shell) mod runtime_observation;

pub(in crate::shell) use controller::TrayController;
pub(in crate::shell) use queue::LatestCommandQueue;
