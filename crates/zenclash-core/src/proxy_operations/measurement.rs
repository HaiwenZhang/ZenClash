//! Provider-aware and group latency measurements with explicit selection side effects.

use std::collections::BTreeMap;

use crate::{DelayResult, MihomoError, MihomoResult};

use super::{ProxyOperations, ProxySelectionOutcome};

/// One proxy-delay target with its source-provider identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyDelayTarget {
    /// Proxy name exposed by Mihomo.
    pub name: String,
    /// Provider that supplied the proxy, when applicable.
    pub provider: Option<String>,
}

/// Result of the explicit group-delay operation that also restores automatic selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyGroupMeasurementOutcome {
    /// Latest delay keyed by group member name.
    pub delays: BTreeMap<String, u32>,
    /// Automatic-selection state read back after the group delay completes.
    pub selection: ProxySelectionOutcome,
}

impl ProxyOperations {
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
        let operations = self.pin_current_runtime()?;
        let client = &operations.client;
        let delays = client
            .proxy_group_delay(group, test_url, timeout_ms)
            .await?;
        let selection = Self::read_selection_on(client, group, Vec::new()).await;
        Ok(ProxyGroupMeasurementOutcome { delays, selection })
    }

    /// Measures every member through Mihomo's group-delay endpoint.
    ///
    /// This endpoint may clear a URL-test or fallback group's fixed selection.
    /// Use [`Self::measure_group_and_restore_auto`] when the caller also needs
    /// the subsequent selection facts.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, API-status, or decoding errors from the
    /// group-delay request.
    pub async fn measure_group(
        &self,
        group: &str,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<BTreeMap<String, u32>> {
        let operations = self.pin_current_runtime()?;
        operations
            .client
            .proxy_group_delay(group, test_url, timeout_ms)
            .await
    }

    /// Measures one proxy through its provider endpoint when source identity is known.
    /// A supplied configuration observation is checked before admission to the delay
    /// queue, after waiting and after the response; no observation lock spans I/O.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, API-status, decoding or stale-observation errors.
    pub async fn measure(
        &self,
        target: &ProxyDelayTarget,
        test_url: Option<&str>,
        timeout_ms: u64,
    ) -> MihomoResult<DelayResult> {
        let operations = self.pin_current_runtime()?;
        let client = &operations.client;
        let _permit = client
            .delay_gate
            .acquire()
            .await
            .map_err(|error| MihomoError::Process(error.to_string()))?;
        operations.verify_observation_current()?;
        let delay = client
            .proxy_delay_with_provider(
                &target.name,
                test_url,
                timeout_ms,
                target.provider.as_deref(),
            )
            .await?;
        operations.verify_observation_current()?;
        Ok(delay)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

    #[tokio::test]
    async fn group_measurements_reject_stale_observation_before_sending() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let session = CoreSession::open(CoreKind::Mihomo, client).unwrap();
        let observation = session.begin_observation().await.unwrap();
        let operations = ProxyOperations::from_observation(&observation);
        session.invalidate_runtime_observations();

        assert!(matches!(
            operations.measure_group("Proxy", None, 500).await,
            Err(MihomoError::StaleBinding)
        ));
        assert!(matches!(
            operations
                .measure_group_and_restore_auto("Auto Group", None, 500)
                .await,
            Err(MihomoError::StaleBinding)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }
}
