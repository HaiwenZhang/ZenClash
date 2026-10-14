//! Retained rule display, complete filter facts and requests to the original runtime owner.

use super::{
    AppContext, Button, Context, Disableable, Entity, FluentBuilder, IconName, Input, InputEvent,
    InputState, InteractiveElement, IntoElement, ParentElement, Sizable, Styled, Subscription,
    Window, contains_ascii_case_insensitive, div, empty_state, h_flex, list_page,
    pagination_summary, v_flex,
};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::ActiveTheme;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(in crate::pages::runtime) mod catalog_read;
mod dashboard;
mod page_integration;
mod projection;

const RULES_PER_PAGE: usize = 50;

#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub(super) struct RuleFilterSelection {
    query: String,
    rule_kind: Option<String>,
    policy: Option<String>,
    disabled_only: bool,
}

/// Facts from the page owner's accepted read, not mutation authority.
#[derive(Clone)]
#[non_exhaustive]
pub(super) struct RuleDisplaySnapshot {
    pub(super) catalog: Arc<zenclash_core::RuleCatalog>,
    pub(super) config: Option<zenclash_core::RuntimeConfig>,
    pub(super) proxies: Option<zenclash_core::ProxyCatalog>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) struct RuleViewContext {
    pub(super) navigation: super::PageTaskToken,
    pub(super) active: bool,
    pub(super) toggle_supported: bool,
}

#[derive(Clone)]
#[non_exhaustive]
pub(super) struct RuleToggleRequest {
    pub(super) navigation: super::PageTaskToken,
    pub(super) catalog: Arc<zenclash_core::RuleCatalog>,
    pub(super) observation: Option<zenclash_core::CoreObservationIdentity>,
    pub(super) rule_index: usize,
    pub(super) enabled: bool,
}

#[derive(Clone)]
pub(super) enum RuleEvent {
    ToggleRequested(RuleToggleRequest),
    PresentationFailed {
        navigation: super::PageTaskToken,
        error: String,
    },
}

pub(super) struct RulesView {
    runtime: tokio::runtime::Handle,
    source: Option<RuleDisplaySnapshot>,
    observation: Option<zenclash_core::CoreObservationIdentity>,
    context: Option<RuleViewContext>,
    filter_input: Entity<InputState>,
    _filter_subscription: Subscription,
    page_index: usize,
    pending_toggles: HashSet<usize>,
    filters: RuleFilterSelection,
    projection: Option<projection::RuleProjection>,
    worker: projection::ProjectionWorker,
    confirmed_disabled: HashMap<usize, bool>,
    projecting: bool,
    selected: Option<usize>,
    details: Option<Arc<zenclash_core::RuleCatalog>>,
}

impl gpui_kit::EventEmitter<RuleEvent> for RulesView {}

impl RulesView {
    pub(super) fn new(
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("runtime.placeholders.rule_filter"))
        });
        let subscription = cx.subscribe(&filter_input, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.page_index = 0;
                this.filters.query = normalize_rule_query(&input.read(cx).value());
                this.update_rule_presentation(cx);
                cx.notify();
            }
        });
        Self {
            runtime,
            source: None,
            observation: None,
            context: None,
            filter_input,
            _filter_subscription: subscription,
            page_index: 0,
            pending_toggles: HashSet::new(),
            filters: RuleFilterSelection::default(),
            projection: None,
            worker: projection::ProjectionWorker::default(),
            confirmed_disabled: HashMap::new(),
            projecting: false,
            selected: None,
            details: None,
        }
    }

    pub(super) fn set_context(&mut self, context: RuleViewContext, cx: &mut Context<Self>) {
        if self.context == Some(context) {
            return;
        }
        self.context = Some(context);
        if !context.active {
            self.cancel_projection();
        }
        cx.notify();
    }

    pub(super) fn present(
        &mut self,
        source: RuleDisplaySnapshot,
        observation: Option<zenclash_core::CoreObservationIdentity>,
        context: RuleViewContext,
        cx: &mut Context<Self>,
    ) {
        self.source = Some(source);
        self.observation = observation;
        self.set_context(context, cx);
        self.update_rule_presentation(cx);
        cx.notify();
    }

    pub(super) fn suspend_presentation(&mut self, cx: &mut Context<Self>) {
        self.cancel_projection();
        if let Some(context) = &mut self.context {
            context.active = false;
        }
        cx.notify();
    }

    fn cancel_projection(&mut self) {
        self.worker.cancel();
        self.projecting = false;
    }

    pub(super) fn release_presentation(&mut self, cx: &mut Context<Self>) {
        self.cancel_projection();
        self.source = None;
        self.observation = None;
        self.projection = None;
        self.confirmed_disabled.clear();
        self.selected = None;
        self.details = None;
        if let Some(context) = &mut self.context {
            context.active = false;
        }
        cx.notify();
    }

    pub(super) fn is_snapshot_retainable(&self) -> bool {
        self.pending_toggles.is_empty() && self.confirmed_disabled.is_empty()
    }

    pub(super) fn clear_confirmed(&mut self, cx: &mut Context<Self>) {
        if !self.confirmed_disabled.is_empty() {
            self.confirmed_disabled.clear();
            cx.notify();
        }
    }

    pub(super) fn begin_toggle(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if !self.pending_toggles.insert(index) {
            return false;
        }
        cx.notify();
        true
    }

    pub(super) fn finish_toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        self.pending_toggles.remove(&index);
        cx.notify();
    }

    pub(super) fn confirm_toggle(&mut self, index: usize, disabled: bool, cx: &mut Context<Self>) {
        self.confirmed_disabled.insert(index, disabled);
        cx.notify();
    }

    pub(super) fn has_pending_toggles(&self) -> bool {
        !self.pending_toggles.is_empty()
    }

    pub(super) fn refresh_localized_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_input.update(cx, |input, cx| {
            input.set_placeholder(
                zenclash_i18n::text("runtime.placeholders.rule_filter"),
                window,
                cx,
            )
        });
    }

    fn request_rule_enabled(&self, rule_index: usize, enabled: bool, cx: &mut Context<Self>) {
        if let (Some(source), Some(projection), Some(context)) =
            (&self.source, &self.projection, self.context)
        {
            if !Arc::ptr_eq(&source.catalog, &projection.snapshot) {
                return;
            }
            cx.emit(RuleEvent::ToggleRequested(RuleToggleRequest {
                navigation: context.navigation,
                catalog: source.catalog.clone(),
                observation: self.observation.clone(),
                rule_index,
                enabled,
            }));
        }
    }

    fn set_rules_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        self.page_index = page_index;
        cx.notify();
    }

    fn update_rule_presentation(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            self.release_presentation(cx);
            return;
        };
        if !self.context.is_some_and(|context| context.active) {
            return;
        }
        let snapshot = &source.catalog;
        if self.projection.as_ref().is_some_and(|projection| {
            Arc::ptr_eq(&projection.snapshot, snapshot) && projection.filter == self.filters
        }) {
            self.cancel_projection();
            return;
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|projection| !Arc::ptr_eq(&projection.snapshot, snapshot))
        {
            self.projection = None;
        }
        let source = snapshot.clone();
        let (generation, task) =
            self.worker
                .start(&self.runtime, source.clone(), self.filters.clone());
        self.projecting = true;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if !this.worker.is_current(generation)
                    || !this
                        .source
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(&current.catalog, &source))
                {
                    return;
                }
                this.projecting = false;
                match result {
                    Ok(Some(projection)) => this.projection = Some(projection),
                    Ok(None) => {}
                    Err(error) => {
                        if let Some(context) = this.context {
                            cx.emit(RuleEvent::PresentationFailed {
                                navigation: context.navigation,
                                error: zenclash_i18n::text_with(
                                    "rules.errors.filter_task",
                                    &[("error", error)],
                                ),
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(super) fn hold_projection(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.worker.hold_projection()
    }

    #[cfg(test)]
    pub(super) fn is_projecting(&self) -> bool {
        self.projecting
    }

    #[cfg(test)]
    pub(super) fn filter_input(&self) -> &Entity<InputState> {
        &self.filter_input
    }
}

impl gpui_kit::Render for RulesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.rule_dashboard(
            window.viewport_size().width < window.rem_size() * 68.,
            &theme,
            cx,
        )
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
mod tests;
