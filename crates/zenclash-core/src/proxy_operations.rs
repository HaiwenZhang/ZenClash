//! Intent-oriented proxy selection and delay measurement.

use std::collections::BTreeMap;

use crate::{DelayResult, MihomoClient, MihomoError, MihomoResult, ProxyCatalog};

/// Hidden-group visibility requested by a proxy catalog consumer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyVisibility {
    /// Return only groups intended for normal user-facing surfaces.
    #[default]
    VisibleOnly,
    /// Include profile-internal groups marked hidden by Mihomo.
    IncludeHidden,
}

/// Connection handling requested after Mihomo accepts a proxy selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionPolicy {
    /// Keep existing connections on their current chains.
    #[default]
    KeepExisting,
    /// Close only connections whose current chain contains the previous member.
    RebuildAffected,
    /// Close every existing connection after the selection.
    ///
    /// Callers should require explicit user confirmation before selecting this policy.
    RebuildAll,
}

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

/// Confirmation that Mihomo accepted a proxy selection command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxySelectionReceipt {
    /// Non-fatal connection-cleanup failures after Mihomo accepted the switch.
    pub warnings: Vec<String>,
}

/// Result of the explicit group-delay operation that also restores automatic selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyGroupMeasurementOutcome {
    /// Latest delay keyed by group member name.
    pub delays: BTreeMap<String, u32>,
    /// Automatic-selection state read back after the group delay completes.
    pub selection: ProxySelectionOutcome,
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

    /// Fetches the current proxy catalog with an explicit hidden-group policy.
    ///
    /// # Errors
    ///
    /// Returns transport, API-status, or response-decoding errors.
    pub async fn catalog(&self, visibility: ProxyVisibility) -> MihomoResult<ProxyCatalog> {
        let mut catalog = self.client.proxy_catalog().await?;
        if visibility == ProxyVisibility::VisibleOnly {
            catalog.groups.retain(|group| !group.hidden);
        }
        Ok(catalog)
    }

    /// Selects a group member, applies the requested connection policy, and reads back truth.
    ///
    /// Once Mihomo accepts the selection, connection cleanup and readback are
    /// best-effort. Their failures are returned as warnings rather than
    /// misreporting the already-applied selection as a failed command.
    ///
    /// # Errors
    ///
    /// Returns an error only when Mihomo rejects the selection itself.
    pub async fn select(
        &self,
        group: &str,
        proxy: &str,
        connection_policy: ConnectionPolicy,
    ) -> MihomoResult<ProxySelectionOutcome> {
        let receipt = self
            .apply_selection(group, proxy, connection_policy)
            .await?;
        Ok(self.read_selection(group, receipt.warnings).await)
    }

    /// Applies a group selection without waiting for a full proxy-catalog readback.
    ///
    /// This is the acknowledgement seam for interactive callers: success means
    /// Mihomo accepted the selection and the requested connection policy has
    /// completed. Callers may reconcile controller state separately.
    ///
    /// # Errors
    ///
    /// Returns an error only when Mihomo rejects the selection itself.
    pub async fn apply_selection(
        &self,
        group: &str,
        proxy: &str,
        connection_policy: ConnectionPolicy,
    ) -> MihomoResult<ProxySelectionReceipt> {
        if group.trim().is_empty() {
            return Err(MihomoError::InvalidInput("代理组名称不能为空".into()));
        }
        if proxy.trim().is_empty() {
            return Err(MihomoError::InvalidInput("代理节点名称不能为空".into()));
        }
        let mut warnings = Vec::new();
        let previous_member = if connection_policy == ConnectionPolicy::RebuildAffected {
            match self.client.proxy_catalog().await {
                Ok(catalog) => catalog
                    .groups
                    .iter()
                    .find(|candidate| candidate.name == group)
                    .map(|candidate| candidate.now.clone())
                    .filter(|member| !member.is_empty()),
                Err(error) => {
                    warnings.push(error.to_string());
                    None
                }
            }
        } else {
            None
        };

        self.client.change_proxy(group, proxy).await?;

        match connection_policy {
            ConnectionPolicy::KeepExisting => {}
            ConnectionPolicy::RebuildAffected => {
                if let Some(previous_member) = previous_member {
                    match self.client.connections_snapshot().await {
                        Ok(snapshot) => {
                            for connection in snapshot.connections.iter().filter(|connection| {
                                connection
                                    .chains
                                    .iter()
                                    .any(|member| member == &previous_member)
                            }) {
                                if let Err(error) =
                                    self.client.close_connection(&connection.id).await
                                {
                                    warnings.push(error.to_string());
                                }
                            }
                        }
                        Err(error) => warnings.push(error.to_string()),
                    }
                }
            }
            ConnectionPolicy::RebuildAll => {
                if let Err(error) = self.client.close_all_connections().await {
                    warnings.push(error.to_string());
                }
            }
        }
        Ok(ProxySelectionReceipt { warnings })
    }

    /// Restores an automatic URL-test or fallback group and reads back controller truth.
    ///
    /// Once Mihomo accepts the restore command, readback is best-effort. A
    /// readback failure is returned as a warning because automatic selection
    /// has already been restored.
    ///
    /// # Errors
    ///
    /// Returns an error only when Mihomo rejects the restore command itself.
    pub async fn restore_auto(&self, group: &str) -> MihomoResult<ProxySelectionOutcome> {
        self.client.restore_proxy_group(group).await?;
        Ok(self.read_selection(group, Vec::new()).await)
    }

    /// Measures a whole group through Mihomo and restores automatic selection as a side effect.
    ///
    /// This operation is intentionally separate from [`Self::measure`] because
    /// Mihomo clears a URL-test or fallback group's fixed member when the group
    /// delay endpoint is used. Controller state is read back after the delay run.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, API-status, or decoding errors from the
    /// group-delay request. A subsequent readback failure is a warning.
    pub async fn measure_group_and_restore_auto(
        &self,
        group: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<ProxyGroupMeasurementOutcome> {
        let delays = self
            .client
            .proxy_group_delay(group, test_url, timeout_ms)
            .await?;
        let selection = self.read_selection(group, Vec::new()).await;
        Ok(ProxyGroupMeasurementOutcome { delays, selection })
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

    async fn read_selection(
        &self,
        group: &str,
        mut warnings: Vec<String>,
    ) -> ProxySelectionOutcome {
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

        ProxySelectionOutcome {
            actual,
            catalog,
            warnings,
        }
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
    async fn rebuild_all_selection_survives_cleanup_failure_and_uses_readback_truth() {
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
            .select("Proxy", "HK 01", ConnectionPolicy::RebuildAll)
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
    async fn keep_existing_selection_does_not_close_connections() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
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

        ProxyOperations::new(client)
            .select("Proxy", "HK 01", ConnectionPolicy::default())
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("PUT /proxies/Proxy "));
        assert!(requests[1].starts_with("GET /proxies "));
    }

    #[tokio::test]
    async fn apply_selection_returns_after_ack_without_waiting_for_catalog_readback() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8_lossy(&request[..bytes]).into_owned()
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let receipt = ProxyOperations::new(client)
            .apply_selection("Proxy", "HK 01", ConnectionPolicy::KeepExisting)
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert!(receipt.warnings.is_empty());
        assert!(request.starts_with("PUT /proxies/Proxy "));
    }

    #[tokio::test]
    async fn rebuild_affected_closes_only_chains_containing_the_previous_member() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                proxy_catalog_with_current("Old Node"),
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                connections_response(),
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                proxy_catalog_with_current("New Node"),
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
            .select("Proxy", "New Node", ConnectionPolicy::RebuildAffected)
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(outcome.actual.as_deref(), Some("New Node"));
        assert_eq!(requests.len(), 5);
        assert!(requests[0].starts_with("GET /proxies "));
        assert!(requests[1].starts_with("PUT /proxies/Proxy "));
        assert!(requests[2].starts_with("GET /connections "));
        assert!(requests[3].starts_with("DELETE /connections/affected "));
        assert!(requests[4].starts_with("GET /proxies "));
        assert!(
            requests
                .iter()
                .all(|request| !request.starts_with("DELETE /connections/unrelated "))
        );
    }

    #[tokio::test]
    async fn selection_rejection_remains_a_hard_error() {
        let client = MihomoClient::new(MihomoEndpoint::default()).unwrap();

        let error = ProxyOperations::new(client)
            .select("", "HK 01", ConnectionPolicy::KeepExisting)
            .await
            .unwrap_err();

        assert!(matches!(error, MihomoError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn visible_catalog_filters_hidden_groups_by_default() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
            stream
                .write_all(hidden_proxy_catalog_response().as_bytes())
                .unwrap();
            request
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let catalog = ProxyOperations::new(client)
            .catalog(ProxyVisibility::default())
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert!(request.starts_with("GET /proxies "));
        assert_eq!(catalog.groups.len(), 1);
        assert_eq!(catalog.groups[0].name, "Proxy");
    }

    #[tokio::test]
    async fn advanced_catalog_preserves_hidden_groups() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).unwrap();
            stream
                .write_all(hidden_proxy_catalog_response().as_bytes())
                .unwrap();
            String::from_utf8_lossy(&request[..bytes]).into_owned()
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let catalog = ProxyOperations::new(client)
            .catalog(ProxyVisibility::IncludeHidden)
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(catalog.groups.len(), 2);
        assert!(catalog.groups.iter().any(|group| group.hidden));
    }

    #[tokio::test]
    async fn restore_auto_deletes_group_and_reads_back_automatic_state() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                automatic_proxy_catalog_response(),
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
            .restore_auto("Auto Group")
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert!(requests[0].starts_with("DELETE /proxies/Auto%20Group "));
        assert!(requests[1].starts_with("GET /proxies "));
        assert_eq!(outcome.actual.as_deref(), Some("HK 01"));
        assert_eq!(
            outcome.catalog.unwrap().groups[0].behavior,
            crate::ProxyGroupBehavior::Automatic { fixed: false }
        );
    }

    #[tokio::test]
    async fn group_measurement_names_restore_side_effect_and_reads_back_state() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [group_delay_response(), automatic_proxy_catalog_response()] {
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
            .measure_group_and_restore_auto(
                "Auto Group",
                Some("https://www.gstatic.com/generate_204"),
                5_000,
            )
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert!(requests[0].starts_with("GET /group/Auto%20Group/delay?"));
        assert!(requests[0].contains("timeout=5000"));
        assert!(requests[1].starts_with("GET /proxies "));
        assert_eq!(outcome.delays.get("HK 01"), Some(&42));
        assert_eq!(
            outcome.selection.catalog.unwrap().groups[0].behavior,
            crate::ProxyGroupBehavior::Automatic { fixed: false }
        );
    }

    #[tokio::test]
    async fn individual_measurement_never_calls_the_group_restore_endpoint() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..bytes]).into_owned();
            let body = r#"{"delay":42}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            request
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let delay = ProxyOperations::new(client)
            .measure(
                &ProxyDelayTarget {
                    name: "HK 01".into(),
                    provider: None,
                },
                Some("https://www.gstatic.com/generate_204"),
                5_000,
            )
            .await
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(delay.delay, 42);
        assert!(request.starts_with("GET /proxies/HK%2001/delay?"));
        assert!(!request.contains("/group/"));
    }

    fn proxy_catalog_response() -> String {
        let body = r#"{"proxies":{"Proxy":{"name":"Proxy","type":"Selector","now":"HK 01","all":["HK 01"]},"HK 01":{"name":"HK 01","type":"Shadowsocks"}}}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn hidden_proxy_catalog_response() -> String {
        let body = r#"{"proxies":{"DIRECT":{"name":"DIRECT","type":"Direct"},"Proxy":{"name":"Proxy","type":"Selector","now":"DIRECT","all":["DIRECT"]},"Internal":{"name":"Internal","type":"Selector","now":"DIRECT","all":["DIRECT"],"hidden":true}}}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn automatic_proxy_catalog_response() -> String {
        let body = r#"{"proxies":{"HK 01":{"name":"HK 01","type":"Shadowsocks"},"Auto Group":{"name":"Auto Group","type":"URLTest","now":"HK 01","all":["HK 01"],"fixed":false}}}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn group_delay_response() -> String {
        let body = r#"{"HK 01":42}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn proxy_catalog_with_current(current: &str) -> String {
        let body = format!(
            r#"{{"proxies":{{"Old Node":{{"name":"Old Node","type":"Shadowsocks"}},"New Node":{{"name":"New Node","type":"Shadowsocks"}},"Proxy":{{"name":"Proxy","type":"Selector","now":"{current}","all":["Old Node","New Node"]}}}}}}"#
        );
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn connections_response() -> String {
        let body = r#"{"connections":[{"id":"affected","chains":["Proxy","Old Node"]},{"id":"unrelated","chains":["Proxy","Other Node"]}]}"#;
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
