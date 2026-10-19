//! Acknowledgement, pinned observation, connection cleanup and automatic-selection restoration.

use crate::{MihomoClient, MihomoError, MihomoResult, ProxyCatalog};

use super::ProxyOperations;

/// Connection handling requested after Mihomo accepts a proxy selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionPolicy {
    /// Keep existing connections on their current chains.
    #[default]
    KeepExisting,
    /// Close only connections whose current chain contains this group and its previous member.
    RebuildAffected,
    /// Close every existing connection after the selection.
    ///
    /// Callers should require explicit user confirmation before selecting this policy.
    RebuildAll,
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

/// An accepted selection whose later observation is fixed to the original runtime binding.
#[derive(Clone)]
pub struct AcceptedProxySelection {
    operations: ProxyOperations,
    group_key: String,
    requested_member: String,
    warnings: Vec<String>,
}

impl std::fmt::Debug for AcceptedProxySelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcceptedProxySelection")
            .field("group_key", &self.group_key)
            .field("requested_member", &self.requested_member)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

/// Actual controller evidence or an explicit reason why selection remains unconfirmed.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProxySelectionReadback {
    /// The controller reported a nonempty current member; it may differ from the request.
    Observed(Box<ObservedProxySelection>),
    /// The group endpoint no longer exists.
    GroupMissing,
    /// The controller did not report a current member.
    NoCurrentMember,
    /// The command's runtime or retained configuration changed before observation completed.
    RuntimeChanged,
    /// Controller observation failed; command acceptance remains a separate fact.
    Unavailable(MihomoError),
}

/// Actual selection evidence bound to the controller and group that produced it.
#[derive(Clone)]
pub struct ObservedProxySelection {
    operations: ProxyOperations,
    group_key: String,
    observation: crate::ProxyGroupSelectionObservation,
}

impl std::fmt::Debug for ObservedProxySelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservedProxySelection")
            .field("group_key", &self.group_key)
            .field("observation", &self.observation)
            .finish_non_exhaustive()
    }
}

impl ObservedProxySelection {
    /// Returns the controller-observed member, independently of the requested member.
    #[must_use]
    pub fn member(&self) -> &str {
        self.observation.member()
    }
    /// Returns the controller group key that produced this observation.
    #[must_use]
    pub fn group_key(&self) -> &str {
        &self.group_key
    }
    /// Publishes only while the original runtime and any supplied configuration are current.
    /// Returns false for an ineligible observation or missing destination group.
    pub fn apply_to(&self, catalog: &mut ProxyCatalog) -> bool {
        self.apply_to_with(catalog, |_| {})
    }

    /// Applies actual selection and publishes its confirmation under one source guard.
    /// The callback receives the observed member only after the destination group is updated.
    /// It must only change in-memory presentation, without client/session locks or I/O.
    /// Returns false without invoking the callback for an ineligible source or missing group.
    pub fn apply_to_with(&self, catalog: &mut ProxyCatalog, on_applied: impl FnOnce(&str)) -> bool {
        self.operations
            .publish_if_current(|| {
                if !catalog.apply_group_observation(&self.group_key, &self.observation) {
                    return false;
                }
                on_applied(self.observation.member());
                true
            })
            .unwrap_or(false)
    }
}

impl AcceptedProxySelection {
    pub(crate) fn with_read_operations(mut self, operations: ProxyOperations) -> Self {
        self.operations = operations;
        self
    }
    /// Returns the controller group key used by this accepted command.
    #[must_use]
    pub fn group_key(&self) -> &str {
        &self.group_key
    }
    /// Returns the requested member, which is not evidence of actual selection.
    #[must_use]
    pub fn requested_member(&self) -> &str {
        &self.requested_member
    }
    /// Returns nonfatal connection-cleanup warnings.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    /// Returns whether the original runtime and any retained configuration remain eligible.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.operations.verify_observation_current().is_ok()
    }

    /// Publishes diagnostics only while the accepted command's original source is eligible.
    /// The callback must only change memory, without client/session locks or I/O.
    ///
    /// # Errors
    /// Returns a stale-binding error without invoking the callback when the source is ineligible.
    pub fn publish_if_current<T>(&self, publish: impl FnOnce() -> T) -> MihomoResult<T> {
        self.operations.publish_if_current(publish)
    }
    /// Reads one group's actual selection without migrating to a replacement runtime.
    /// A retained configuration waits for admitted work before and after the request,
    /// with all observation guards released during I/O. Changed sources remain unconfirmed.
    pub async fn read_actual(&self) -> ProxySelectionReadback {
        if self.wait_for_read_eligibility().await.is_err() {
            return ProxySelectionReadback::RuntimeChanged;
        }
        let result = self
            .operations
            .client
            .proxy_group_observation(&self.group_key)
            .await;
        if self.wait_for_read_eligibility().await.is_err() {
            return ProxySelectionReadback::RuntimeChanged;
        }
        match result {
            Ok(observation) if !observation.member().is_empty() => {
                ProxySelectionReadback::Observed(Box::new(ObservedProxySelection {
                    operations: self.operations.clone(),
                    group_key: self.group_key.clone(),
                    observation,
                }))
            }
            Ok(_) => ProxySelectionReadback::NoCurrentMember,
            Err(MihomoError::StaleBinding | MihomoError::StaleTransport) => {
                ProxySelectionReadback::RuntimeChanged
            }
            Err(MihomoError::Api { status: 404, .. }) => ProxySelectionReadback::GroupMissing,
            Err(error) => ProxySelectionReadback::Unavailable(error),
        }
    }

    async fn wait_for_read_eligibility(&self) -> MihomoResult<()> {
        self.operations.client.ensure_binding_current()?;
        if let Some(identity) = &self.operations.observation_identity {
            identity.wait_for_read_eligibility().await?;
        }
        self.operations.client.ensure_binding_current()
    }
}

impl ProxyOperations {
    /// Selects a group member, applies the requested connection policy, and reads back truth.
    ///
    /// Once Mihomo accepts the selection, connection cleanup and readback are
    /// best-effort. Their failures are returned as warnings rather than
    /// misreporting the already-applied selection as a failed command.
    ///
    /// # Errors
    ///
    /// Returns invalid input, controller rejection, transport or stale-binding errors.
    /// A transport failure after sending may leave the command outcome uncertain.
    pub async fn select(
        &self,
        group: &str,
        proxy: &str,
        connection_policy: ConnectionPolicy,
    ) -> MihomoResult<ProxySelectionOutcome> {
        let client = self.client.pin_binding()?;
        let receipt = Self::apply_selection_on(&client, group, proxy, connection_policy).await?;
        Ok(Self::read_selection_on(&client, group, receipt.warnings).await)
    }

    /// Requests a selection and returns its bound acknowledgement before any catalog readback.
    ///
    /// Cleanup follows the requested policy before acknowledgement, while failures
    /// remain warnings. The returned capability can observe actual selection later.
    ///
    /// # Errors
    /// Returns invalid input, controller rejection, transport or stale-binding errors.
    /// A transport failure after sending may leave the command outcome uncertain.
    pub async fn accept_selection(
        &self,
        group_key: &str,
        requested_member: &str,
        connection_policy: ConnectionPolicy,
    ) -> MihomoResult<AcceptedProxySelection> {
        let operations = self.pin_current_runtime()?;
        let client = &operations.client;
        let receipt =
            Self::apply_selection_on(client, group_key, requested_member, connection_policy)
                .await?;
        Ok(AcceptedProxySelection {
            operations,
            group_key: group_key.to_owned(),
            requested_member: requested_member.to_owned(),
            warnings: receipt.warnings,
        })
    }

    /// Applies a group selection without waiting for a full proxy-catalog readback.
    ///
    /// This is the acknowledgement seam for interactive callers: success means
    /// Mihomo accepted the selection and the requested connection policy has
    /// completed. Callers may reconcile controller state separately.
    ///
    /// # Errors
    ///
    /// Returns invalid input, controller rejection, transport or stale-binding errors.
    /// A transport failure after sending may leave the command outcome uncertain.
    pub async fn apply_selection(
        &self,
        group: &str,
        proxy: &str,
        connection_policy: ConnectionPolicy,
    ) -> MihomoResult<ProxySelectionReceipt> {
        let accepted = self
            .accept_selection(group, proxy, connection_policy)
            .await?;
        Ok(ProxySelectionReceipt {
            warnings: accepted.warnings,
        })
    }

    async fn apply_selection_on(
        client: &MihomoClient,
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
            match client.proxy_catalog().await {
                Ok(catalog) => catalog
                    .groups()
                    .iter()
                    .find(|candidate| candidate.controller_name() == group)
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

        client.change_proxy(group, proxy).await?;

        match connection_policy {
            ConnectionPolicy::KeepExisting => {}
            ConnectionPolicy::RebuildAffected => {
                if let Some(previous_member) = previous_member {
                    match client.connections_snapshot().await {
                        Ok(snapshot) => {
                            for connection in snapshot.connections.iter().filter(|connection| {
                                connection.chains.iter().any(|member| member == group)
                                    && connection
                                        .chains
                                        .iter()
                                        .any(|member| member == &previous_member)
                            }) {
                                if let Err(error) = client.close_connection(&connection.id).await {
                                    warnings.push(error.to_string());
                                }
                            }
                        }
                        Err(error) => warnings.push(error.to_string()),
                    }
                }
            }
            ConnectionPolicy::RebuildAll => {
                if let Err(error) = client.close_all_connections().await {
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
        let client = self.client.pin_binding()?;
        client.restore_proxy_group(group).await?;
        Ok(Self::read_selection_on(&client, group, Vec::new()).await)
    }

    pub(super) async fn read_selection_on(
        client: &MihomoClient,
        group: &str,
        mut warnings: Vec<String>,
    ) -> ProxySelectionOutcome {
        let catalog = match client.proxy_catalog().await {
            Ok(catalog) => Some(catalog),
            Err(error) => {
                warnings.push(error.to_string());
                None
            }
        };
        let actual = catalog.as_ref().and_then(|catalog| {
            catalog
                .groups()
                .iter()
                .find(|candidate| candidate.controller_name() == group)
                .map(|candidate| candidate.now.clone())
                .filter(|member| !member.is_empty())
        });

        ProxySelectionOutcome {
            actual,
            catalog,
            warnings,
        }
    }
}
