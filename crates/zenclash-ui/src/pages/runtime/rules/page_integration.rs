//! Connects retained rule display to the original observed runtime mutation owner.
use super::super::{Page, PageSnapshotData, RuntimePage};
use super::*;

impl RuntimePage {
    fn rule_view_context(&self) -> RuleViewContext {
        RuleViewContext {
            navigation: self.page_task_token_for(self.page),
            active: self.page == Page::Rules && self.live_updates_enabled(),
            toggle_supported: self.core_kind.capabilities().rule_toggle,
        }
    }

    pub(in crate::pages::runtime) fn synchronize_rule_context(&self, cx: &mut Context<Self>) {
        let context = self.rule_view_context();
        self.rules
            .update(cx, |view, cx| view.set_context(context, cx));
    }

    pub(in crate::pages::runtime) fn update_rule_presentation(&mut self, cx: &mut Context<Self>) {
        let PageSnapshotData::Rules {
            catalog,
            config,
            proxies,
        } = self.page_observation.data()
        else {
            self.rules
                .update(cx, |view, cx| view.release_presentation(cx));
            return;
        };
        let snapshot = RuleDisplaySnapshot {
            catalog: catalog.clone(),
            config: config.clone(),
            proxies: proxies.clone(),
        };
        let observation = self.page_observation.source().clone();
        let context = self.rule_view_context();
        self.rules.update(cx, |view, cx| {
            view.present(snapshot, observation, context, cx)
        });
    }

    pub(in crate::pages::runtime) fn handle_rule_event(
        &mut self,
        event: RuleEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            RuleEvent::PresentationFailed { navigation, error } => {
                self.set_page_error(navigation, error)
            }
            RuleEvent::ToggleRequested(request) => {
                if !self.is_page_task_current(request.navigation)
                    || !matches!(self.page_observation.data(),PageSnapshotData::Rules{catalog,..} if Arc::ptr_eq(catalog,&request.catalog))
                {
                    return;
                }
                self.apply_rule_toggle(request, cx);
            }
        }
    }

    fn apply_rule_toggle(&mut self, request: RuleToggleRequest, cx: &mut Context<Self>) {
        let index = request.rule_index;
        let enabled = request.enabled;
        if !self.core_kind.capabilities().rule_toggle {
            self.error = Some(zenclash_i18n::text_with(
                "rules.warnings.toggle_unavailable",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        if self.page != Page::Rules {
            return;
        }
        let Some(identity) = request.observation else {
            self.refresh(cx);
            return;
        };
        if !self
            .rules
            .update(cx, |view, cx| view.begin_toggle(index, cx))
        {
            return;
        }
        self.invalidate_page_load();
        let token = self.page_task_token_for(Page::Rules);
        self.error = None;
        let session = self.core_session.clone();
        let submitted_identity = identity.clone();
        let task = self.runtime.spawn(async move {
            session
                .apply_observed_rule_disabled(&submitted_identity, index, !enabled)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "rules.errors.status_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.rules
                    .update(cx, |view, cx| view.finish_toggle(index, cx));
                match result {
                    Ok(()) => {
                        if this.is_page_task_current(token) && identity.is_current() {
                            this.rules
                                .update(cx, |view, cx| view.confirm_toggle(index, !enabled, cx));
                            this.notice = Some(if enabled {
                                zenclash_i18n::text_with(
                                    "rules.notices.enabled",
                                    &[("index", index.to_string())],
                                )
                            } else {
                                zenclash_i18n::text_with(
                                    "rules.notices.disabled",
                                    &[("index", index.to_string())],
                                )
                            });
                            if !this.rules.read(cx).has_pending_toggles() {
                                this.refresh(cx);
                            }
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
