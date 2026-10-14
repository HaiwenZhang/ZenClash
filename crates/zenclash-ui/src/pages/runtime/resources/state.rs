//! Resource-specific input identity and ruleset presentation; operation authority remains shared.

use crate::pages::runtime::RuntimePage;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{App, AppContext, Context, Entity, Subscription, Window};

pub(in crate::pages::runtime) struct ResourcesState {
    search: Entity<InputState>,
    _search_subscription: Subscription,
    pub(super) ruleset: super::ruleset::RulesetUiState,
}

impl ResourcesState {
    pub(in crate::pages::runtime) fn new(
        window: &mut Window,
        cx: &mut Context<RuntimePage>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("settings_redesign.search_resources"))
        });
        let subscription = cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            _search_subscription: subscription,
            ruleset: super::ruleset::RulesetUiState::default(),
        }
    }

    pub(super) fn search(&self) -> &Entity<InputState> {
        &self.search
    }

    pub(super) fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().to_lowercase()
    }
}
