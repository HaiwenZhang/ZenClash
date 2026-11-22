//! Unified crate-internal test fixtures shared across runtime host, features, and shell tests.

pub(crate) mod controller;
pub(crate) mod owned_cores;
pub(crate) mod proxies;
pub(crate) mod resources;
pub(crate) mod runtime_host;
pub(crate) mod service_workflow;
pub(crate) mod shell_tray;

pub(crate) use controller::ControllerFixture;
pub(crate) use owned_cores::owned_ui_children;
pub(crate) use runtime_host::{RuntimePageFixture, open_runtime_page_window};
pub(crate) use service_workflow::{open_service_page, open_service_tun};
