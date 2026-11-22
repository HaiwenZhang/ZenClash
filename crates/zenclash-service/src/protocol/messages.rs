// Modified for the ZenClash v2.0 redesign; GPL-3.0-only.
//! Minimal 4+1 DTOs for the thin privileged launcher protocol.

use serde::{Deserialize, Serialize};
use strum_macros::{AsRefStr, EnumString};

use super::{MAX_PATH_FIELD_LEN, MAX_SESSION_ID_LEN, ProtocolVersion};

/// Minimal v2 IPC endpoints exposed by the thin privileged launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumString, AsRefStr)]
pub enum V2Endpoint {
    /// Read-only health check returning service version, helper path, and active session.
    #[strum(serialize = "/v2/ping")]
    Ping,
    /// Validate trusted core path and spawn a supervised Mihomo process bound to `session_id`.
    #[strum(serialize = "/v2/start")]
    Start,
    /// Stop the supervised process matching `session_id` (409 Conflict on mismatch).
    #[strum(serialize = "/v2/stop")]
    Stop,
    /// Return bounded recent output lines (<= 100 lines / <= 2 MiB).
    #[strum(serialize = "/v2/logs")]
    Logs,
    /// macOS-only explicit TUN DNS resolver synchronization bound to `session_id`.
    #[strum(serialize = "/v2/tun-dns")]
    SetTunDns,
}

/// Summary of the currently supervised core session returned by `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSessionSummary {
    pub session_id: String,
    pub pid: Option<u32>,
}

/// Request payload for `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingRequest {
    pub protocol_version: ProtocolVersion,
}

impl Default for PingRequest {
    fn default() -> Self {
        Self {
            protocol_version: ProtocolVersion::current(),
        }
    }
}

/// Response payload for `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingResponse {
    pub service_version: String,
    pub protocol: ProtocolVersion,
    pub helper_path: String,
    pub active_session: Option<ActiveSessionSummary>,
}

/// Request payload for `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartProcessRequest {
    pub session_id: String,
    pub core_path: String,
    pub home_dir: String,
    pub config_path: String,
    pub controller_ipc: String,
}

impl StartProcessRequest {
    /// Validates request field syntax and bounds before touching the filesystem or OS APIs.
    ///
    /// # Errors
    /// Returns a static diagnostic string when `session_id` or any path field is empty,
    /// oversized, or contains NUL/control characters.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_session_id(&self.session_id)?;
        validate_path_field(&self.core_path)?;
        validate_path_field(&self.home_dir)?;
        validate_path_field(&self.config_path)?;
        validate_path_field(&self.controller_ipc)?;
        Ok(())
    }
}

/// Response payload for `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartProcessResponse {
    pub session_id: String,
    pub pid: u32,
}

/// Request payload for `stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopProcessRequest {
    pub session_id: String,
}

impl StopProcessRequest {
    /// Validates `session_id` syntax.
    ///
    /// # Errors
    /// Returns a static diagnostic string when `session_id` is empty, oversized, or has invalid characters.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_session_id(&self.session_id)
    }
}

/// Response payload for `stop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopProcessResponse {
    pub stopped: bool,
    pub exit_confirmed: bool,
}

/// Response payload for `logs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsResponse {
    pub lines: Vec<String>,
    pub truncated: bool,
}

/// Request payload for macOS `set_tun_dns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTunDnsRequest {
    pub session_id: String,
    pub enabled: bool,
}

impl SetTunDnsRequest {
    /// Validates `session_id` syntax.
    ///
    /// # Errors
    /// Returns a static diagnostic string when `session_id` is invalid.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_session_id(&self.session_id)
    }
}

/// Response payload for macOS `set_tun_dns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTunDnsResponse {
    pub applied: bool,
}

/// Validates that a launcher `session_id` is non-empty, bounded, and uses safe ASCII characters.
///
/// # Errors
/// Returns a static diagnostic string when `session_id` violates length or character constraints.
pub fn validate_session_id(session_id: &str) -> Result<(), &'static str> {
    if session_id.is_empty() {
        return Err("session_id must not be empty");
    }
    if session_id.len() > MAX_SESSION_ID_LEN {
        return Err("session_id exceeds maximum length");
    }
    if !session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("session_id must contain only ASCII alphanumeric characters, '-' or '_'");
    }
    Ok(())
}

fn validate_path_field(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("path field must not be empty");
    }
    if path.len() > MAX_PATH_FIELD_LEN {
        return Err("path field exceeds maximum length");
    }
    if path.bytes().any(|byte| byte == 0 || byte == b'\n' || byte == b'\r') {
        return Err("path field must not contain NUL or newline characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_endpoints_serialize_to_expected_paths() {
        assert_eq!(V2Endpoint::Ping.as_ref(), "/v2/ping");
        assert_eq!(V2Endpoint::Start.as_ref(), "/v2/start");
        assert_eq!(V2Endpoint::Stop.as_ref(), "/v2/stop");
        assert_eq!(V2Endpoint::Logs.as_ref(), "/v2/logs");
        assert_eq!(V2Endpoint::SetTunDns.as_ref(), "/v2/tun-dns");
    }

    #[test]
    fn session_id_validation_enforces_bounds_and_charset() {
        assert!(validate_session_id("sess_01-abcDEF").is_ok());
        assert!(validate_session_id(&"a".repeat(MAX_SESSION_ID_LEN)).is_ok());
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(&"a".repeat(MAX_SESSION_ID_LEN + 1)).is_err());
        assert!(validate_session_id("bad session").is_err());
        assert!(validate_session_id("../escape").is_err());
        assert!(validate_session_id("nul\0byte").is_err());
    }

    #[test]
    fn start_process_request_round_trips_and_validates() -> Result<(), Box<dyn std::error::Error>> {
        let request = StartProcessRequest {
            session_id: "session-2026-01".to_owned(),
            core_path: "/var/lib/zenclash-service/cores/mihomo".to_owned(),
            home_dir: "/home/alice/.config/zenclash".to_owned(),
            config_path: "/home/alice/.config/zenclash/config.yaml".to_owned(),
            controller_ipc: "/run/zenclash-service/users/1000/mihomo.sock".to_owned(),
        };
        assert!(request.validate().is_ok());
        let json = serde_json::to_vec(&request)?;
        let decoded: StartProcessRequest = serde_json::from_slice(&json)?;
        assert_eq!(decoded, request);

        let mut bad_path = request.clone();
        bad_path.core_path = "/tmp/mihomo\0evil".to_owned();
        assert!(bad_path.validate().is_err());

        let mut empty_home = request;
        empty_home.home_dir.clear();
        assert!(empty_home.validate().is_err());
        Ok(())
    }

    #[test]
    fn ping_stop_logs_and_tun_dns_dtos_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let ping = PingResponse {
            service_version: crate::VERSION.to_owned(),
            protocol: ProtocolVersion::current(),
            helper_path: "/usr/lib/zenclash/zenclash-service".to_owned(),
            active_session: Some(ActiveSessionSummary {
                session_id: "sess_42".to_owned(),
                pid: Some(4242),
            }),
        };
        let encoded = serde_json::to_vec(&ping)?;
        assert_eq!(serde_json::from_slice::<PingResponse>(&encoded)?, ping);

        let stop_req = StopProcessRequest {
            session_id: "sess_42".to_owned(),
        };
        assert!(stop_req.validate().is_ok());
        let stop_resp = StopProcessResponse {
            stopped: true,
            exit_confirmed: true,
        };
        assert_eq!(
            serde_json::from_slice::<StopProcessResponse>(&serde_json::to_vec(&stop_resp)?)?,
            stop_resp
        );

        let logs = LogsResponse {
            lines: vec!["core started".to_owned()],
            truncated: false,
        };
        assert_eq!(
            serde_json::from_slice::<LogsResponse>(&serde_json::to_vec(&logs)?)?,
            logs
        );

        let dns_req = SetTunDnsRequest {
            session_id: "sess_42".to_owned(),
            enabled: true,
        };
        assert!(dns_req.validate().is_ok());
        let dns_resp = SetTunDnsResponse { applied: true };
        assert_eq!(
            serde_json::from_slice::<SetTunDnsResponse>(&serde_json::to_vec(&dns_resp)?)?,
            dns_resp
        );
        Ok(())
    }
}
