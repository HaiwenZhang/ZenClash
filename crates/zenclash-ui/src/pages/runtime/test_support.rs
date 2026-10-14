//! Isolated setup shared by runtime capabilities and application integration tests.
pub(in crate::pages::runtime) mod controller_fixture;
pub(in crate::pages::runtime) mod owned_cores;
pub(in crate::pages::runtime) mod runtime_page_fixture;
pub(in crate::pages::runtime) mod service_workflow;
pub(in crate::pages::runtime) use controller_fixture::ControllerFixture;
pub(in crate::pages::runtime) use owned_cores::owned_ui_children;
pub(crate) use runtime_page_fixture::RuntimePageFixture;
pub(in crate::pages::runtime) use runtime_page_fixture::open_runtime_page_window;
pub(in crate::pages::runtime) use service_workflow::{open_service_page, open_service_tun};
