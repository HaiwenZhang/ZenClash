//! Intent-oriented proxy selection and delay measurement.

use crate::{DelayResult, MihomoClient, MihomoResult, ProxyCatalog};

/// One proxy-delay target with its source-provider identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyDelayTarget {
    /// Proxy name exposed by Mihomo.
    pub name: String,
    /// Provider that supplied the proxy, when applicable.
    pub provider: Option<String>,
}

/// Result of a proxy selection after best-effort cleanup and readback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxySelectionOutcome {
    /// Member reported by Mihomo after the selection, when readback succeeded.
    pub actual: Option<String>,
    /// Fresh catalog returned by the readback, when available.
    pub catalog: Option<ProxyCatalog>,
    /// Non-fatal cleanup or readback failures after Mihomo accepted the switch.
    pub warnings: Vec<String>,
}

/// Deep interface for proxy selection and provider-aware latency checks.
#[derive(Clone)]
pub struct ProxyOperations {
    client: MihomoClient,
}

impl ProxyOperations {
    /// Creates proxy operations over one Mihomo controller client.
    #[must_use]
    pub fn new(client: MihomoClient) -> Self {
        Self { client }
    }

    /// Selects a group member, closes stale connections, and reads back truth.
    ///
    /// Once Mihomo accepts the selection, connection cleanup and readback are
    /// best-effort. Their failures are returned as warnings rather than
    /// misreporting the already-applied selection as a failed command.
    ///
    /// # Errors
    ///
    /// Returns an error only when Mihomo rejects the selection itself.
    pub async fn select(&self, group: &str, proxy: &str) -> MihomoResult<ProxySelectionOutcome> {
        self.client.change_proxy(group, proxy).await?;

        let mut warnings = Vec::new();
        if let Err(error) = self.client.close_all_connections().await {
            warnings.push(error.to_string());
        }
        let catalog = match self.client.proxy_catalog().await {
            Ok(catalog) => Some(catalog),
            Err(error) => {
                warnings.push(error.to_string());
                None
            }
        };
        let actual = catalog.as_ref().and_then(|catalog| {
            catalog
                .groups
                .iter()
                .find(|candidate| candidate.name == group)
                .map(|candidate| candidate.now.clone())
        });

        Ok(ProxySelectionOutcome {
            actual,
            catalog,
            warnings,
        })
    }

    /// Measures one proxy through its provider endpoint when source identity is known.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, API-status, or decoding errors.
    pub async fn measure(
        &self,
        target: &ProxyDelayTarget,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<DelayResult> {
        self.client
            .proxy_delay_with_provider(
                &target.name,
                test_url,
                timeout_ms,
                target.provider.as_deref(),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{MihomoEndpoint, MihomoError};

    use super::*;

    #[tokio::test]
    async fn accepted_selection_survives_cleanup_failure_and_uses_readback_truth() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"message\":\"busy\"}".to_owned(),
                proxy_catalog_response(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4_096];
                let bytes = stream.read(&mut request).unwrap();
                requests.push(String::from_utf8_lossy(&request[..bytes]).into_owned());
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let outcome = ProxyOperations::new(client)
            .select("Proxy", "HK 01")
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(outcome.actual.as_deref(), Some("HK 01"));
        assert_eq!(outcome.warnings.len(), 1);
        assert!(requests[0].starts_with("PUT /proxies/Proxy "));
        assert!(requests[1].starts_with("DELETE /connections "));
        assert!(requests[2].starts_with("GET /proxies "));
    }

    #[tokio::test]
    async fn selection_rejection_remains_a_hard_error() {
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();

        let error = ProxyOperations::new(client)
            .select("", "HK 01")
            .await
            .unwrap_err();

        assert!(matches!(error, MihomoError::InvalidInput(_)));
    }

    fn proxy_catalog_response() -> String {
        let body = r#"{"proxies":{"Proxy":{"name":"Proxy","type":"Selector","now":"HK 01","all":["HK 01"]},"HK 01":{"name":"HK 01","type":"Shadowsocks"}}}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
