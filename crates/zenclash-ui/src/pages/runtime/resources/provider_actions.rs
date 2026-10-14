//! Provider intents submitted with the resource page's original observation.
use super::super::{Context, PageSnapshotData, ProviderKind, RuntimePage};
use zenclash_core::CoreResourceAction;

impl RuntimePage {
    pub(super) fn update_provider(
        &mut self,
        provider_key: String,
        kind: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        self.start_resource_action(
            CoreResourceAction::UpdateProvider {
                kind,
                key: provider_key,
            },
            Some(zenclash_i18n::text("resources.notices.provider")),
            "resources.errors.provider_task",
            cx,
        );
    }

    pub(super) fn update_all_providers(&mut self, cx: &mut Context<Self>) {
        let PageSnapshotData::Resources { proxy, rules, .. } = self.page_observation.data() else {
            return;
        };
        let targets = proxy
            .providers
            .keys()
            .map(|name| zenclash_core::ProviderUpdateTarget::new(ProviderKind::Proxy, name.clone()))
            .chain(rules.providers.keys().map(|name| {
                zenclash_core::ProviderUpdateTarget::new(ProviderKind::Rule, name.clone())
            }))
            .collect();
        self.start_resource_action(
            CoreResourceAction::UpdateProviders(targets),
            None,
            "resources.errors.provider_task",
            cx,
        );
    }

    pub(super) fn healthcheck_provider(&mut self, name: String, cx: &mut Context<Self>) {
        self.start_resource_action(
            CoreResourceAction::HealthcheckProvider { key: name },
            Some(zenclash_i18n::text("resources.notices.healthcheck")),
            "resources.errors.healthcheck_task",
            cx,
        );
    }
}

#[cfg(test)]
mod tests;
