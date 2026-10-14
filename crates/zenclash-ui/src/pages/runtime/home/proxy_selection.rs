//! Home proxy selection submission and observation in its original dashboard query.

use super::*;

struct HomeProxySelectionContext {
    token: super::super::PageTaskToken,
    observation: zenclash_core::CoreObservationIdentity,
    query_revision: u64,
}

impl HomeProxySelectionContext {
    fn belongs_to_current_query(&self, page: &RuntimePage) -> bool {
        page.is_page_task_current(self.token)
            && page.page_observation.query_revision() == self.query_revision
            && page
                .page_observation
                .source()
                .as_ref()
                .is_some_and(|current| current.is_same_observation(&self.observation))
    }
}

impl RuntimePage {
    pub(super) fn change_home_proxy(
        &mut self,
        group: String,
        proxy: String,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Home
            || self.home.profile_switching.is_some()
            || self.home.proxy_selection.any_pending()
        {
            return;
        }
        let Some(identity) = self
            .page_observation
            .source()
            .clone()
            .filter(|identity| identity.belongs_to(&self.core_session))
        else {
            self.home.proxy_error = Some(zenclash_core::MihomoError::StaleBinding.to_string());
            cx.notify();
            return;
        };
        let Some(request) = self.home.proxy_selection.start(group, proxy) else {
            return;
        };
        self.invalidate_page_load();
        let context = HomeProxySelectionContext {
            token: self.page_task_token_for(Page::Home),
            observation: identity.clone(),
            query_revision: self.page_observation.query_revision(),
        };
        self.home.proxy_error = None;
        let session = self.core_session.clone();
        let submitted_identity = identity;
        let submitted = request.clone();
        let task = self.runtime.spawn(async move {
            session
                .accept_observed_proxy_selection(
                    &submitted_identity,
                    &submitted.group_key,
                    &submitted.requested_member,
                    ConnectionPolicy::KeepExisting,
                )
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if !context.belongs_to_current_query(this) {
                    this.home.proxy_selection.reject(&request);
                    cx.notify();
                    return;
                }
                match result {
                    Ok(accepted) => {
                        if !this.home.proxy_selection.acknowledge(&request) {
                            return;
                        }
                        cx.emit(ProxySelectionChanged);
                        this.reconcile_home_proxy_selection(context, request, accepted, cx);
                    }
                    Err(error) => {
                        if this.home.proxy_selection.reject(&request) {
                            context
                                .observation
                                .publish_if_current(|| this.home.proxy_error = Some(error));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reconcile_home_proxy_selection(
        &mut self,
        context: HomeProxySelectionContext,
        request: crate::proxy_selection::ProxySelectionRequest,
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
                if !context.belongs_to_current_query(this) {
                    this.home.proxy_selection.reject(&request);
                    cx.notify();
                    return;
                }
                match result {
                    Ok((accepted, readback)) => {
                        let catalog = this.page_observation.dashboard_catalog_mut();
                        let notice = &mut this.notice;
                        if this.home.proxy_selection.observe_with(
                            &request,
                            &accepted,
                            readback,
                            catalog,
                            |actual| {
                                *notice = Some(zenclash_i18n::text_with(
                                    "home.proxy.switched",
                                    &[("name", actual.to_owned())],
                                ));
                                cx.emit(ProxySelectionChanged);
                            },
                        ) {
                            this.prepare_home_projection();
                        }
                    }
                    Err(error) => {
                        if context
                            .observation
                            .publish_if_current(|| {
                                this.home
                                    .proxy_selection
                                    .observation_failed(&request, error.to_string());
                            })
                            .is_none()
                        {
                            this.home.proxy_selection.observation_failed(
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
