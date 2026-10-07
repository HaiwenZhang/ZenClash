mod catalog;
mod editor;
mod forms;

use gpui_kit::{Focusable, InteractiveElement, TestSupportExt};

use super::super::{
    Button, Disableable, FluentBuilder, IconName, IntoElement, ParentElement, RuntimeData,
    RuntimePage, Sizable, Styled, h_flex, v_flex,
};

impl RuntimePage {
    pub(in crate::pages::runtime) fn render_profile_commands(
        &self,
        cx: &mut gpui_kit::Context<Self>,
    ) -> gpui_kit::Div {
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("update-all-profiles")
                    .icon(IconName::RefreshCw)
                    .outline()
                    .h_10()
                    .small()
                    .label(zenclash_i18n::text("unified.profiles.update_all"))
                    .disabled(
                        self.core_busy()
                            || self.profiles.store.is_none()
                            || self.profiles.forms.catalog_view.remote_count == 0,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.update_all_managed_profiles(cx))),
            )
            .child(
                Button::new("toggle-add-subscription")
                    .icon(if self.profiles.forms.adding_subscription {
                        IconName::Close
                    } else {
                        IconName::Plus
                    })
                    .label(if self.profiles.forms.adding_subscription {
                        zenclash_i18n::text("profiles.actions.collapse_form")
                    } else {
                        zenclash_i18n::text("profiles.actions.add_remote")
                    })
                    .small()
                    .h_10()
                    .outline()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.profiles.forms.adding_subscription {
                            this.close_subscription_form(cx);
                        } else {
                            this.profiles.forms.adding_subscription = true;
                            this.profiles
                                .forms
                                .subscription_name
                                .focus_handle(cx)
                                .focus(window, cx);
                            cx.notify();
                        }
                    })),
            )
            .child(
                Button::new("choose-profile")
                    .icon(IconName::FolderOpen)
                    .label(zenclash_i18n::text("profiles.actions.import_local"))
                    .small()
                    .h_10()
                    .outline()
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.choose_profile(window, cx);
                    })),
            )
    }

    pub(in super::super) fn render_profile(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut gpui_kit::Context<Self>,
    ) -> gpui_kit::AnyElement {
        let config = match &self.data {
            RuntimeData::Profile { config, .. } => config.as_ref(),
            _ => None,
        };
        v_flex()
            .gap_4()
            .child(
                gpui_kit::component::input::Input::new(&self.profiles.forms.search)
                    .prefix(gpui_kit::component::Icon::new(IconName::Search)),
            )
            .when(self.profiles.forms.adding_subscription, |view| {
                view.child(self.render_subscription_form(theme, cx))
            })
            .when(
                self.profiles.recovery.is_some() || self.profiles.pending_finalization.is_some(),
                |view| view.child(self.render_profile_recovery(theme, cx)),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .when(compact, |row| row.flex_col())
                    .child(
                        v_flex()
                            .id("profiles-catalog-region")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .when(compact, |view| view.w_full())
                            .gap_4()
                            .child(self.render_managed_profiles(compact, theme, cx))
                            .child(self.render_current_profile(config, theme, cx)),
                    )
                    .when(!compact, |row| {
                        row.child(
                            v_flex()
                                .id("profiles-inspector-region")
                                .test_support()
                                .w(gpui_kit::rems(23.))
                                .flex_shrink_0()
                                .min_w_0()
                                .child(self.render_profile_inspector(theme, cx)),
                        )
                    }),
            )
            .into_any_element()
    }
}
