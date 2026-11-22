//! Isolated controller protocol setup shared by core and UI tests.
//! Available only in tests or with the non-production `test-support` feature.

/// Mock HTTP controller request/response helpers.
pub mod http;
#[cfg(test)]
pub(crate) mod session;
#[cfg(test)]
pub(crate) mod stream_publication;
#[cfg(test)]
pub(crate) mod tun;

pub use http::{
    ControllerRequest, is_controller_header_eof, read_controller_request,
    read_controller_request_blocking, read_controller_request_with_limit,
    read_http_status_line_blocking,
};
