use gpui_kit::base::TestSupportExt;
use std::collections::{HashMap, HashSet};

mod dashboard;
mod projection;

use super::{
    AppContext, Button, Context, Disableable, Entity, FluentBuilder, IconName, Input, InputEvent,
    InputState, InteractiveElement, IntoElement, Page, ParentElement, RuntimeData, RuntimePage,
    Sizable, Styled, Subscription, Switch, Window, contains_ascii_case_insensitive, div,
    empty_state, h_flex, list_page, message_banner, pagination_summary, v_flex,
};

const RULES_PER_PAGE: usize = 100;

pub(super) struct RulesUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) page: usize,
    pub(super) pending: HashSet<usize>,
    query: String,
    projection: Option<projection::RuleProjection>,
    worker: projection::ProjectionWorker,
    pub(super) confirmed_disabled: HashMap<usize, bool>,
    pub(super) projecting: bool,
    selected: Option<usize>,
}

impl RulesUiState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<RuntimePage>) -> (Self, Subscription) {
        let filter = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.rule_filter"))
        });
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.rules.page = 0;
                this.rules.query = normalize_rule_query(&this.rules.filter.read(cx).value());
                this.update_rule_presentation(cx);
                cx.notify();
            }
        });
        (
            Self {
                filter,
                page: 0,
                pending: HashSet::new(),
                query: String::new(),
                projection: None,
                worker: projection::ProjectionWorker::default(),
                confirmed_disabled: HashMap::new(),
                projecting: false,
                selected: None,
            },
            subscription,
        )
    }
}

impl RulesUiState {
    pub(super) fn cancel_projection(&mut self) {
        self.worker.cancel();
        self.projecting = false;
    }

    pub(super) fn release_presentation(&mut self) {
        self.cancel_projection();
        self.projection = None;
        self.confirmed_disabled.clear();
        self.selected = None;
    }
}

impl RuntimePage {
    pub(super) fn update_rule_presentation(&mut self, cx: &mut Context<Self>) {
        let RuntimeData::Rules(snapshot) = &self.data else {
            self.rules.release_presentation();
            return;
        };
        if self.page != Page::Rules || !self.live_updates_enabled() {
            return;
        }
        if self.rules.projection.as_ref().is_some_and(|projection| {
            std::sync::Arc::ptr_eq(&projection.snapshot, snapshot)
                && projection.query == self.rules.query
        }) {
            return;
        }
        let source = snapshot.clone();
        let (generation, task) =
            self.rules
                .worker
                .start(&self.runtime, source.clone(), self.rules.query.clone());
        self.rules.projecting = true;
        self.rules.projection = None;
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| error.to_string()).and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.rules.worker.is_current(generation)
                    || !matches!(&this.data, RuntimeData::Rules(current) if std::sync::Arc::ptr_eq(current, &source)) { return; }
                this.rules.projecting = false;
                match result {
                    Ok(Some(projection)) => this.rules.projection = Some(projection),
                    Ok(None) => {},
                    Err(error) => this.error = Some(zenclash_i18n::text_with("rules.errors.filter_task", &[("error", error)])),
                }
                cx.notify();
            });
        }).detach();
    }

    fn set_rule_enabled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        if !self.core_kind.capabilities().rule_toggle {
            self.error = Some(zenclash_i18n::text_with(
                "rules.warnings.toggle_unavailable",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        if self.page != Page::Rules || !self.rules.pending.insert(index) {
            return;
        }
        self.invalidate_page_load();
        let token = self.page_task_token_for(Page::Rules);
        self.error = None;
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .apply_rule_disabled(index, !enabled)
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
                this.rules.pending.remove(&index);
                match result {
                    Ok(()) => {
                        if this.is_page_task_current(token) {
                            this.rules.confirmed_disabled.insert(index, !enabled);
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
                            if this.rules.pending.is_empty() {
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

    fn set_rules_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.rules.page = page;
        cx.notify();
    }

    pub(super) fn render_rules(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        self.rule_dashboard(theme, cx)
    }
}

fn normalize_rule_query(query: &str) -> String {
    query.trim().to_owned()
}

fn rule_matches(rule: &zenclash_core::Rule, query: &str) -> bool {
    query.is_empty()
        || contains_ascii_case_insensitive(&rule.kind, query)
        || contains_ascii_case_insensitive(&rule.payload, query)
        || contains_ascii_case_insensitive(&rule.proxy, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_all_visible_rule_fields_case_insensitively() {
        let rule = zenclash_core::Rule {
            kind: "DomainSuffix".into(),
            payload: "Example.COM".into(),
            proxy: "Auto Select".into(),
            ..Default::default()
        };

        assert!(rule_matches(&rule, &normalize_rule_query("DOMAIN")));
        assert!(rule_matches(&rule, &normalize_rule_query("example.com")));
        assert!(rule_matches(&rule, &normalize_rule_query("auto select")));
        assert!(!rule_matches(&rule, &normalize_rule_query("DIRECT")));
    }
}
