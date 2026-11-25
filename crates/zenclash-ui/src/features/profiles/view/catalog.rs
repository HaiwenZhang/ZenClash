mod card;
mod presentation;

use gpui_kit::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{Context, ParentElement, Styled, div};

use self::presentation::{profile_detail_row, profile_heading, subscription_quota_chart};
use crate::components::page_primitives::{empty_state, format_profile_age, setting_card};
use crate::features::host::RuntimePage;

impl RuntimePage {
    pub(super) fn render_managed_profiles(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut card = v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box);
        if !self
            .profiles
            .forms
            .catalog_view
            .is_current(self.profiles.catalog())
        {
            return card.child(empty_state(
                zenclash_i18n::text("common.actions.loading"),
                theme,
            ));
        }
        if self.profiles.catalog().profiles.is_empty() {
            return card.child(empty_state(
                zenclash_i18n::text("profiles.catalog.empty"),
                theme,
            ));
        }

        use super::super::state::ProfileFilter;
        card = card.child(
            h_flex()
                .px_3()
                .pt_3()
                .justify_between()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("profiles.catalog.title")),
                )
                .child(
                    h_flex()
                        .gap_0()
                        .p_0p5()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .bg(theme.muted.opacity(0.4))
                        .children(
                            [
                                (ProfileFilter::All, "profiles.design.filter_all"),
                                (ProfileFilter::Remote, "profiles.source.remote"),
                                (ProfileFilter::Local, "profiles.source.local"),
                            ]
                            .into_iter()
                            .map(|(filter, key)| {
                                Button::new(format!("profile-filter:{filter:?}"))
                                    .label(zenclash_i18n::text(key))
                                    .small()
                                    .h_8()
                                    .ghost()
                                    .when(
                                        self.profiles.forms.catalog_view.filter == filter,
                                        |button| {
                                            button.bg(theme.list_active).text_color(theme.primary)
                                        },
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.profiles.set_catalog_filter(filter);
                                        cx.notify();
                                    }))
                            }),
                        ),
                ),
        );
        let grid = div().grid().grid_cols(1).gap_3().p_3().children(
            self.profiles
                .forms
                .catalog_view
                .visible_indices()
                .iter()
                .filter_map(|&index| {
                    self.profiles.catalog().profiles.get(index).map(|profile| {
                        v_flex()
                            .gap_2()
                            .child(self.render_managed_profile(profile, theme, cx))
                            .when(
                                compact
                                    && self.profiles.forms.details_open
                                    && self.profiles.forms.catalog_view.is_selected(&profile.id),
                                |view| view.child(self.render_profile_inspector(theme, cx)),
                            )
                    })
                }),
        );
        card = card.child(grid).when(
            self.profiles
                .forms
                .catalog_view
                .visible_indices()
                .is_empty(),
            |this| {
                this.child(empty_state(
                    zenclash_i18n::text("profiles.design.filter_empty"),
                    theme,
                ))
            },
        );
        let page = self.profiles.forms.catalog_view.page;
        let pages = self.profiles.forms.catalog_view.page_count();
        card = card.when(pages > 1, |this| {
            this.child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("previous-profile-page")
                            .small()
                            .outline()
                            .label(zenclash_i18n::text("common.actions.previous_page"))
                            .disabled(page == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profiles
                                    .forms
                                    .catalog_view
                                    .set_page(page.saturating_sub(1));
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child(format!("{} / {pages}", page + 1)))
                    .child(
                        Button::new("next-profile-page")
                            .small()
                            .outline()
                            .label(zenclash_i18n::text("common.actions.next_page"))
                            .disabled(page + 1 >= pages)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profiles.forms.catalog_view.set_page(page + 1);
                                cx.notify();
                            })),
                    ),
            )
        });

        card
    }

    pub(super) fn render_profile_inspector(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        if !self
            .profiles
            .forms
            .catalog_view
            .is_current(self.profiles.catalog())
        {
            return setting_card(zenclash_i18n::text("profiles.catalog.title"), theme).child(
                empty_state(zenclash_i18n::text("common.actions.loading"), theme),
            );
        }
        let profile = self
            .profiles
            .forms
            .catalog_view
            .selected_index()
            .and_then(|index| self.profiles.catalog().profiles.get(index));
        let Some(profile) = profile else {
            return setting_card(zenclash_i18n::text("profiles.catalog.title"), theme).child(
                empty_state(zenclash_i18n::text("profiles.catalog.empty"), theme),
            );
        };
        let active = self.profiles.catalog().active.as_deref() == Some(profile.id.as_str());
        v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .px_4()
                    .pt_3()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(profile.name.clone()),
                    )
                    .when(active, |row| {
                        row.child(profile_heading(profile, true, theme))
                    }),
            )
            .child(
                v_flex()
                    .p_4()
                    .gap_4()
                    .when_some(profile.subscription.usage.as_ref(), |this, usage| {
                        this.child(subscription_quota_chart(profile, usage, theme))
                    })
                    .child(
                        v_flex()
                            .gap_3()
                            .py_3()
                            .border_y_1()
                            .border_color(theme.border)
                            .child(profile_detail_row(
                                "profiles.design.expiry_date",
                                profile
                                    .subscription
                                    .usage
                                    .as_ref()
                                    .and_then(|usage| i64::try_from(usage.expire).ok())
                                    .and_then(|timestamp| {
                                        chrono::DateTime::<chrono::Utc>::from_timestamp(
                                            timestamp, 0,
                                        )
                                    })
                                    .filter(|_| {
                                        profile
                                            .subscription
                                            .usage
                                            .as_ref()
                                            .is_some_and(|usage| usage.expire != 0)
                                    })
                                    .map_or_else(
                                        || "—".into(),
                                        |date| date.format("%Y-%m-%d").to_string(),
                                    ),
                                theme,
                            ))
                            .child(profile_detail_row(
                                "profiles.design.last_updated",
                                format_profile_age(profile.updated_at),
                                theme,
                            ))
                            .when(profile.is_remote(), |this| {
                                this.child(profile_detail_row(
                                    "profiles.design.source_link",
                                    zenclash_i18n::text("profiles.design.hidden"),
                                    theme,
                                ))
                            }),
                    )
                    .when(profile.is_remote(), |this| {
                        this.child(self.render_profile_update_policy(profile, theme, cx))
                    })
                    .child(self.render_profile_actions(profile, active, "inspector-", cx)),
            )
    }

    pub(super) fn render_profile_recovery(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let mut card = setting_card(zenclash_i18n::text("profiles.recovery.title"), theme).child(
            div().px_4().py_3().text_sm().child(zenclash_i18n::text(
                if self.profiles.pending_finalization().is_some() {
                    "profiles.recovery.pending_description"
                } else {
                    "profiles.recovery.description"
                },
            )),
        );
        if let Some(version) = self.profiles.pending_finalization() {
            return card.child(
                h_flex().px_4().pb_3().child(
                    Button::new("confirm-service-profile")
                        .label(zenclash_i18n::text("profiles.recovery.confirm"))
                        .outline()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_service_profile(version, cx)
                        })),
                ),
            );
        }
        if let Some(recovery) = &self.profiles.recovery() {
            let known = recovery.last_known_good.as_ref().and_then(|version| {
                self.profiles
                    .catalog()
                    .profiles
                    .iter()
                    .find(|profile| profile.id == version.profile_id)
            });
            let target = known.or_else(|| {
                self.profiles
                    .catalog()
                    .profiles
                    .iter()
                    .find(|profile| profile.id == recovery.attempted.profile_id)
            });
            if let Some(profile) = target {
                let id = profile.id.clone();
                card = card.child(
                    h_flex().px_4().pb_3().child(
                        Button::new("reapply-profile-recovery")
                            .label(zenclash_i18n::text_with(
                                "profiles.recovery.reapply",
                                &[("name", profile.name.clone())],
                            ))
                            .outline()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_managed_profile(id.clone(), cx)
                            })),
                    ),
                );
            }
        }
        card
    }
}
