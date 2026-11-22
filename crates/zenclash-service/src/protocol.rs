// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Thin v2.0 privileged launcher wire contract shared between client and daemon.
//!
//! This module contains zero platform I/O dependencies and prepares the minimal
//! `ping / start / stop / logs (+ macOS set_tun_dns)` protocol surface for
//! `zenclash-core::runtime::service_launcher`.

pub mod messages;

pub use crate::core::structure::{ProtocolInfo, ProtocolVersion, ServiceErrorCode};
pub use messages::{
    ActiveSessionSummary, LogsResponse, PingRequest, PingResponse, SetTunDnsRequest, SetTunDnsResponse,
    StartProcessRequest, StartProcessResponse, StopProcessRequest, StopProcessResponse, V2Endpoint,
    validate_session_id,
};

/// Maximum number of recent log lines retained by the bounded stderr/stdout ring buffer.
pub const MAX_LOG_RING_LINES: usize = 100;

/// Maximum total byte footprint of retained lines in the bounded log ring buffer (2 MiB).
pub const MAX_LOG_RING_BYTES: usize = 2 * 1024 * 1024;

/// Maximum accepted v2 control request body size (4 KiB).
pub const MAX_V2_REQUEST_BODY_BYTES: usize = 4 * 1024;

/// Maximum allowed `session_id` length in bytes.
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Maximum allowed path field length in v2 launcher requests.
pub const MAX_PATH_FIELD_LEN: usize = 4096;
