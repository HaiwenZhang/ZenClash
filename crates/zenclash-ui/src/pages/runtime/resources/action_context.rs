//! Resource mutation origin and completion eligibility across page reads and configuration changes.

use super::super::page_observation::ObservedPage;
use super::{Context, PageTaskToken, RuntimePage};
use crate::pages::Page;
use zenclash_core::{CoreObservationIdentity, CoreResourceAction, CoreSession, ProviderOperations};

struct ResourceActionContext {
    token: PageTaskToken,
    observation: Option<CoreObservationIdentity>,
    query_revision: u64,
}

struct ResourceActionCompletion {
    observation: Option<CoreObservationIdentity>,
    page: Result<ObservedPage, String>,
    failures: Vec<String>,
}

impl ResourceActionContext {
    fn belongs_to_current_query(&self, page: &RuntimePage) -> bool {
        page.is_page_task_current(self.token)
            && page.page_observation.query_revision() == self.query_revision
            && match (page.page_observation.source(), &self.observation) {
                (Some(current), Some(original)) => current.is_same_observation(original),
                (None, None) => true,
                _ => false,
            }
    }
}

impl ResourceActionCompletion {
    async fn execute(
        session: CoreSession,
        providers: ProviderOperations,
        source: Option<CoreObservationIdentity>,
        action: CoreResourceAction,
    ) -> Self {
        // Builtins have no catalog-derived target. Capture before business when no query exists.
        let observation = match source {
            Some(source) => source,
            None => match session.begin_observation().await {
                Ok(observation) => observation.identity().clone(),
                Err(error) => {
                    return Self {
                        observation: None,
                        page: Err(error.to_string()),
                        failures: Vec::new(),
                    };
                }
            },
        };
        let (page, failures) = match session
            .apply_observed_resource_action(&observation, &providers, action)
            .await
        {
            Ok(receipt) => {
                let (read_source, failures) = receipt.into_parts();
                let page =
                    ObservedPage::load_from_observation(session, read_source, Page::Resources)
                        .await;
                (page, failures)
            }
            Err(error) => (Err(error.to_string()), Vec::new()),
        };
        Self {
            observation: Some(observation),
            page,
            failures,
        }
    }
}

impl RuntimePage {
    fn begin_resource_action_context(
        &mut self,
        allow_without_catalog: bool,
        cx: &mut Context<Self>,
    ) -> Option<ResourceActionContext> {
        let observation = self.page_observation.source().clone();
        if observation
            .as_ref()
            .is_some_and(|source| !source.belongs_to(&self.core_session))
            || (observation.is_none() && !allow_without_catalog)
        {
            self.error = Some(zenclash_core::MihomoError::StaleBinding.to_string());
            cx.notify();
            return None;
        }
        let token = self.begin_mutation(Page::Resources)?;
        Some(ResourceActionContext {
            token,
            observation,
            query_revision: self.page_observation.query_revision(),
        })
    }

    pub(super) fn start_resource_action(
        &mut self,
        action: CoreResourceAction,
        success_notice: Option<String>,
        task_error_key: &'static str,
        cx: &mut Context<Self>,
    ) {
        let allow_without_catalog = matches!(
            action,
            CoreResourceAction::GeoData | CoreResourceAction::ExternalUi
        );
        let Some(context) = self.begin_resource_action_context(allow_without_catalog, cx) else {
            return;
        };
        let session = self.core_session.clone();
        let providers = self.provider_operations.clone();
        let submitted = context.observation.clone();
        let task = self.runtime.spawn(ResourceActionCompletion::execute(
            session, providers, submitted, action,
        ));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(context.token);
                match result {
                    Ok(completion) => {
                        this.complete_resource_action(context, completion, success_notice, cx)
                    }
                    Err(error) => {
                        // Without a retained source a panicked worker cannot prove where it failed.
                        if let Some(source) = context.observation.as_ref()
                            && context.belongs_to_current_query(this)
                        {
                            source.publish_if_current(|| {
                                this.error = Some(zenclash_i18n::text_with(
                                    task_error_key,
                                    &[("error", error.to_string())],
                                ));
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn complete_resource_action(
        &mut self,
        context: ResourceActionContext,
        completion: ResourceActionCompletion,
        success_notice: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !context.belongs_to_current_query(self) {
            return;
        }
        let ResourceActionCompletion {
            observation,
            page,
            failures,
        } = completion;
        let Some(source) = observation else {
            // Only source capture can return this branch; no business has been admitted.
            if let Err(error) = page {
                self.error = Some(error);
            }
            return;
        };
        if context
            .observation
            .as_ref()
            .is_some_and(|original| !original.is_same_observation(&source))
        {
            return;
        }
        let mut data_accepted = false;
        match page {
            Ok(observed) => {
                if !observed.belongs_to_observation(&source) {
                    return;
                }
                observed.publish(|result, runtime| {
                    let read_error = match result {
                        Ok(data) => {
                            self.page_observation.adopt(data, runtime);
                            self.rules.update(cx, |view, cx| view.clear_confirmed(cx));
                            data_accepted = true;
                            None
                        }
                        Err(error) => Some(error),
                    };
                    self.publish_resource_feedback(failures, read_error, success_notice);
                });
            }
            Err(error) => {
                source.publish_if_current(|| {
                    self.publish_resource_feedback(failures, Some(error), None)
                });
            }
        }
        if data_accepted {
            self.update_observed_page_presentation(cx);
        }
    }

    fn publish_resource_feedback(
        &mut self,
        mut failures: Vec<String>,
        read_error: Option<String>,
        success_notice: Option<String>,
    ) {
        failures.extend(read_error);
        if failures.is_empty() {
            self.notice = success_notice;
        } else {
            self.error = Some(failures.join("\n"));
        }
    }
}

#[cfg(test)]
mod tests;
