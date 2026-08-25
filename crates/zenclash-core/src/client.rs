//! Typed access to Mihomo's external-controller HTTP API.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::MihomoEndpoint;

mod api;
mod request;

#[cfg(test)]
mod tests;

/// Result type returned by Mihomo process, endpoint and API operations.
pub type MihomoResult<T> = Result<T, MihomoError>;

/// Error produced while configuring, launching or communicating with Mihomo.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MihomoError {
    /// Controller URL is empty, malformed or uses an unsupported scheme.
    #[error("invalid Mihomo controller endpoint: {0}")]
    InvalidEndpoint(String),
    /// HTTP transport, timeout or response-decoding failure.
    #[error("Mihomo request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Non-success response returned by Mihomo, including its bounded message.
    #[error("Mihomo API returned HTTP {status}: {message}")]
    Api {
        /// HTTP response status code.
        status: u16,
        /// Error message returned by Mihomo.
        message: String,
    },
    /// Invalid caller input rejected before a network request is sent.
    #[error("invalid Mihomo request input: {0}")]
    InvalidInput(String),
    /// Process, filesystem or native-platform operation failure.
    #[error("Mihomo process error: {0}")]
    Process(String),
}

/// Response returned by Mihomo's `/version` endpoint.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct VersionInfo {
    /// Whether the running core identifies itself as Mihomo/Clash Meta.
    #[serde(default)]
    pub meta: bool,
    /// Core version string.
    #[serde(default)]
    pub version: String,
}

/// Cloneable HTTP client for Mihomo's external-controller API.
#[derive(Clone)]
pub struct MihomoClient {
    endpoint: MihomoEndpoint,
    http: reqwest::Client,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
}

impl MihomoClient {
    /// Creates a client with bounded connection and request timeouts.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(endpoint: MihomoEndpoint) -> MihomoResult<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            endpoint,
            http,
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Returns the controller address and secret used by this client.
    #[must_use]
    pub const fn endpoint(&self) -> &MihomoEndpoint {
        &self.endpoint
    }
}
