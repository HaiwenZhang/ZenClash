//! Catalog-bound proxy selection submission, acknowledgement and controller readback.

use super::{CatalogTaskToken, ConnectionPolicy, Context, ProxiesPage, ProxySelectionRequest};
use std::sync::Arc;

impl ProxiesPage {
    pub(super) fn change_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.catalog_operation_pending() || self.switching.group_pending(&group) || self.loading
        {
            return;
        }
        let Some(operations) = self.capture_catalog_mutation_source(cx) else {
            return;
        };
        let Some(request) = self.switching.start(group, proxy) else {
            return;
        };
        self.error = None;
        self.notice = None;
        let submitted = request.clone();
        let token = CatalogTaskToken(self.catalog_generation);
        let session = self.core_session.clone();
        let submitted_operations = operations.clone();
        let task = self.runtime.spawn(async move {
            match session {
                Some(session) => {
                    let identity = submitted_operations
                        .observation_identity()
                        .ok_or_else(|| zenclash_core::MihomoError::StaleBinding.to_string())?;
                    session
                        .accept_observed_proxy_selection(
                            identity,
                            &submitted.group_key,
                            &submitted.requested_member,
                            ConnectionPolicy::KeepExisting,
                        )
                        .await
                        .map_err(|error| error.to_string())
                }
                None => submitted_operations
                    .accept_selection(
                        &submitted.group_key,
                        &submitted.requested_member,
                        ConnectionPolicy::KeepExisting,
                    )
                    .await
                    .map_err(|error| error.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    this.switching.reject(&request);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(accepted) => {
                        if !this.switching.acknowledge(&request) {
                            return;
                        }
                        let _ = operations.publish_if_current(|| {
                            this.notice = (!accepted.warnings().is_empty()).then(|| {
                                zenclash_i18n::text_with(
                                    "proxies.notices.applied_with_warning",
                                    &[("warning", accepted.warnings().join("; "))],
                                )
                            });
                        });
                        this.reconcile_proxy_selection(token, operations, request, accepted, cx);
                    }
                    Err(error) => {
                        if this.switching.reject(&request) {
                            let _ = operations.publish_if_current(|| this.error = Some(error));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reconcile_proxy_selection(
        &mut self,
        token: CatalogTaskToken,
        operations: zenclash_core::ProxyOperations,
        request: ProxySelectionRequest,
        accepted: zenclash_core::AcceptedProxySelection,
        cx: &mut Context<Self>,
    ) {
        let task = self.runtime.spawn(async move {
            let readback = accepted.read_actual().await;
            (accepted, readback)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !token.is_current(this.catalog_generation) {
                    this.switching.reject(&request);
                    cx.notify();
                    return;
                }
                match result {
                    Ok((accepted, readback)) => {
                        this.switching.observe(
                            &request,
                            &accepted,
                            readback,
                            this.catalog.as_mut().map(Arc::make_mut),
                        );
                    }
                    Err(error) => {
                        if operations
                            .publish_if_current(|| {
                                this.switching
                                    .observation_failed(&request, error.to_string());
                            })
                            .is_err()
                        {
                            this.switching.observation_failed(
                                &request,
                                zenclash_i18n::text("proxies.selection.runtime_changed"),
                            );
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests;
