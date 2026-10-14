//! Isolated controller protocol setup shared by core and UI tests.
//! Available only in tests or with the non-production `test-support` feature.

mod http;
pub use http::{
    ControllerRequest, is_controller_header_eof, read_controller_request,
    read_controller_request_blocking, read_http_status_line_blocking,
};
