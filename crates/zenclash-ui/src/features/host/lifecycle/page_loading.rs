//! Page observation loading, snapshot adoption, and scoped mutation coordination.

use gpui_kit::Context;
use zenclash_core::{ProviderKind, RuntimeConfig};

use crate::features::host::RuntimePage;
use crate::foundation::actions::Page;
use crate::foundation::primitives::runtime_read_source::RuntimeReadSource;
use crate::runtime_host::{
    busy::MutationDomain,
    observation::{ObservedPage, PageSnapshotData},
    task_token::PageTaskToken,
};

impl RuntimePage {
    pub(in crate::features) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_config_inputs(cx);
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let page = self.page;
        let request = self
            .page_observation
            .begin(page, self.navigation_generation);
        let session = self.core_session.clone();
        let read_source = std::sync::Arc::new(RuntimeReadSource::default());
        let submitted_source = read_source.clone();
        let task = self.runtime.spawn(async move {
            ObservedPage::load_with_source(session, page, &submitted_source).await
        });
        self.page_observation.track(request, &task);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this
                    .page_observation
                    .finish(request, this.page, this.navigation_generation)
                {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(observed) => {
                        if this.accept_page_observation(observed, cx) && page == Page::Network {
                            this.refresh_network_diagnostics(cx);
                        }
                    }
                    Err(error) => {
                        if this.shell.startup_error.is_none() {
                            this.publish_page_refresh_error(&read_source, error);
                        } else {
                            tracing::debug!(%error, "offline recovery controller probe failed");
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn publish_page_refresh_error(
        &mut self,
        source: &RuntimeReadSource,
        error: String,
    ) -> bool {
        match source.identity() {
            Some(identity) => {
                identity.belongs_to(&self.core_session)
                    && identity
                        .publish_if_current(|| self.error = Some(error))
                        .is_some()
            }
            // The caller checks the original query; no controller I/O began before capture.
            None => {
                self.error = Some(error);
                true
            }
        }
    }

    pub(super) fn restore_page_observation(&mut self) {
        self.page_observation
            .restore_current(self.page, &self.core_session);
    }

    pub(in crate::features) fn accept_page_observation(
        &mut self,
        observed: ObservedPage,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut data_accepted = false;
        let accepted = observed.publish(|result, runtime| {
            let data = match result {
                Ok(data) => data,
                Err(error) => {
                    if self.shell.startup_error.is_none() {
                        self.error = Some(error);
                    }
                    return;
                }
            };
            self.page_observation.adopt(data, runtime);
            self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
            data_accepted = true;
        });
        if accepted {
            if !data_accepted {
                return false;
            }
            self.update_observed_page_presentation(cx);
        } else {
            self.refresh(cx);
        }
        accepted
    }

    pub(in crate::features) fn update_observed_page_presentation(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        if let PageSnapshotData::Resources { proxy, rules, .. } = self.page_observation.data()
            && let Some(identity) = self.page_observation.source().as_ref()
        {
            self.monitors
                .provider_operations
                .observe_catalog_for_runtime(ProviderKind::Proxy, proxy, identity);
            self.monitors
                .provider_operations
                .observe_catalog_for_runtime(ProviderKind::Rule, rules, identity);
        }
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.update_rule_presentation(cx);
    }

    pub(in crate::features) fn core_busy(&self) -> bool {
        self.mutation_busy(MutationDomain::Core)
    }

    pub(in crate::features) fn mutation_busy(&self, domain: MutationDomain) -> bool {
        self.persistent_loading || self.mutations.busy(domain)
    }

    pub(in crate::features) fn begin_mutation(&mut self, page: Page) -> Option<PageTaskToken> {
        self.begin_scoped_mutation(page, MutationDomain::Core)
    }

    pub(in crate::features) fn begin_scoped_mutation(
        &mut self,
        page: Page,
        domain: MutationDomain,
    ) -> Option<PageTaskToken> {
        if self.persistent_loading {
            return None;
        }
        let mutation = self.mutations.begin(domain)?;
        self.page_observation.clear_cache();
        if matches!(
            domain,
            MutationDomain::Core | MutationDomain::Backup | MutationDomain::BackupRecovery
        ) {
            self.invalidate_page_load();
        }
        self.error = None;
        self.notice = None;
        Some(PageTaskToken {
            mutation: Some(mutation),
            ..self.page_task_token_for(page)
        })
    }

    pub(in crate::features) fn finish_mutation(&mut self, token: PageTaskToken) {
        self.synchronize_profile_recovery();
        if let Some(mutation) = token.mutation {
            self.mutations.finish(mutation);
        }
    }

    pub(in crate::features) fn invalidate_page_load(&mut self) {
        self.page_observation.cancel();
        self.loading = false;
    }

    pub(in crate::features) const fn page_task_token_for(&self, page: Page) -> PageTaskToken {
        PageTaskToken {
            page,
            navigation_generation: self.navigation_generation,
            mutation: None,
        }
    }

    pub(in crate::features) fn is_page_task_current(&self, token: PageTaskToken) -> bool {
        token.is_current(self.page, self.navigation_generation)
    }

    pub(in crate::features) fn replace_page_data(
        &mut self,
        token: PageTaskToken,
        data: PageSnapshotData,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_page_task_current(token) {
            return false;
        }
        if let PageSnapshotData::Resources { proxy, rules, .. } = &data {
            self.monitors
                .provider_operations
                .observe_catalog(ProviderKind::Proxy, proxy);
            self.monitors
                .provider_operations
                .observe_catalog(ProviderKind::Rule, rules);
        }
        self.page_observation
            .replace_unobserved(data, self.core_session.generation());
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
        self.update_rule_presentation(cx);
        true
    }

    pub(in crate::features) fn set_page_error(&mut self, token: PageTaskToken, error: String) {
        if self.is_page_task_current(token) {
            self.error = Some(error);
        } else {
            tracing::warn!(page = ?token.page, %error, "discarded error from an inactive page task");
        }
    }

    pub(in crate::features) fn config(&self) -> Option<&RuntimeConfig> {
        self.page_observation.config(self.core_session.generation())
    }

    pub(in crate::features) fn mihomo_binary(&self) -> Option<std::path::PathBuf> {
        self.core_session
            .runtime_descriptor()
            .binary()
            .map(std::path::Path::to_path_buf)
    }
}
