use gpui_kit::{InteractiveElement, StatefulInteractiveElement};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{
    menu::{DropdownMenu, PopupMenuItem},
    progress::Progress,
};
use zenclash_core::{ProfileRecord, ProfileSource, RemoteProfileRoute, SubscriptionUsage};

use super::super::super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Icon, IconName, IntoElement,
    ParentElement, RuntimePage, Sizable, Styled, div, empty_state, format_bytes,
    format_profile_age, h_flex, setting_card, v_flex,
};

use crate::components::mint_switch::MintSwitch as Switch;

const UPDATE_INTERVALS: [u32; 4] = [60, 6 * 60, 12 * 60, 24 * 60];

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
            .is_current(&self.profiles.catalog)
        {
            return card.child(empty_state(
                zenclash_i18n::text("common.actions.loading"),
                theme,
            ));
        }
        if self.profiles.catalog.profiles.is_empty() {
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
                                        this.profiles
                                            .forms
                                            .catalog_view
                                            .set_filter(filter, &this.profiles.catalog);
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
                    self.profiles.catalog.profiles.get(index).map(|profile| {
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
            .is_current(&self.profiles.catalog)
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
            .and_then(|index| self.profiles.catalog.profiles.get(index));
        let Some(profile) = profile else {
            return setting_card(zenclash_i18n::text("profiles.catalog.title"), theme).child(
                empty_state(zenclash_i18n::text("profiles.catalog.empty"), theme),
            );
        };
        let active = self.profiles.catalog.active.as_deref() == Some(profile.id.as_str());
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
                if self.profiles.pending_finalization.is_some() {
                    "profiles.recovery.pending_description"
                } else {
                    "profiles.recovery.description"
                },
            )),
        );
        if let Some(version) = self.profiles.pending_finalization {
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
        if let Some(recovery) = &self.profiles.recovery {
            let known = recovery.last_known_good.as_ref().and_then(|version| {
                self.profiles
                    .catalog
                    .profiles
                    .iter()
                    .find(|profile| profile.id == version.profile_id)
            });
            let target = known.or_else(|| {
                self.profiles
                    .catalog
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

    fn render_managed_profile(
        &self,
        profile: &ProfileRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let active = self.profiles.catalog.active.as_deref() == Some(profile.id.as_str());
        let selected = self.profiles.forms.catalog_view.is_selected(&profile.id);
        let show_id = profile.id.clone();
        let expiry_hint = profile.subscription.usage.as_ref().map_or_else(
            || "—".into(),
            |usage| format_subscription_expiry(usage.expire),
        );

        v_flex()
            .flex_grow_1()
            .min_w_0()
            .w_full()
            .max_w_full()
            .min_h_0()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if selected {
                theme.chart_3
            } else {
                theme.border
            })
            .bg(if selected {
                theme.list_active.opacity(0.45)
            } else {
                theme.group_box
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(format!("inspect-profile:{}", profile.id))
                            .accessibility_label(profile.name.clone())
                            .tooltip(profile.name.clone())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::File).size_6())
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_2()
                                            .flex_wrap()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(gpui_kit::rems(6.))
                                                    .max_w_full()
                                                    .text_lg()
                                                    .truncate()
                                                    .child(profile.name.clone()),
                                            )
                                            .child(
                                                profile_heading(profile, false, theme)
                                                    .flex_shrink_0(),
                                            ),
                                    ),
                            )
                            .small()
                            .h_auto()
                            .min_h_10()
                            .ghost()
                            .flex_1()
                            .min_w_0()
                            .justify_start()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.profiles.forms.catalog_view.select(&show_id);
                                this.profiles.forms.details_open = true;
                                cx.notify();
                            })),
                    )
                    .when(active, |row| {
                        row.child(profile_heading(profile, true, theme))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when_some(profile.subscription.usage.as_ref(), |this, usage| {
                        this.child(render_subscription_usage(
                            usage,
                            format!("catalog-quota:{}", profile.id),
                            theme,
                        ))
                    })
                    .when(profile.subscription.usage.is_none(), |this| {
                        this.child(div().text_sm().text_color(theme.muted_foreground).child(
                            zenclash_i18n::text(if profile.is_remote() {
                                "home.profile.usage_unavailable"
                            } else {
                                "profiles.design.no_quota"
                            }),
                        ))
                    }),
            )
            .child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .gap_2()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "profiles.catalog.updated",
                            &[("age", format_profile_age(profile.updated_at))],
                        ),
                    ))
                    .child(
                        div()
                            .id((
                                gpui_kit::ElementId::from("profile-expiry"),
                                profile.id.clone(),
                            ))
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(expiry_hint.clone())
                                    .build(window, cx)
                            })
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(profile.subscription.usage.as_ref().map_or_else(
                                || "—".into(),
                                |usage| format_subscription_remaining(usage.expire),
                            )),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        self.render_profile_actions(profile, active, "", cx)
                            .flex_1(),
                    )
                    .child({
                        let id = profile.id.clone();
                        Button::new(format!("profile-details:{id}"))
                            .outline()
                            .small()
                            .h_10()
                            .label(zenclash_i18n::text("unified.profiles.details"))
                            .on_click(cx.listener(move |page, _, _, cx| {
                                let was_open = page.profiles.forms.catalog_view.is_selected(&id)
                                    && page.profiles.forms.details_open;
                                page.profiles.forms.catalog_view.select(&id);
                                page.profiles.forms.details_open = !was_open;
                                cx.notify();
                            }))
                    }),
            )
    }

    fn render_profile_actions(
        &self,
        profile: &ProfileRecord,
        active: bool,
        scope: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let activate_id = profile.id.clone();
        let update_id = profile.id.clone();
        let edit_id = profile.id.clone();
        let menu_id = profile.id.clone();
        let menu_name = profile.name.clone();
        let remote = profile.is_remote();
        let menu_owner = cx.entity().downgrade();
        if scope == "inspector-" && remote {
            return h_flex()
                .w_full()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new(format!("{scope}update-profile:{}", profile.id))
                        .icon(crate::assets::AppIcon::RefreshCw)
                        .label(zenclash_i18n::text("profiles.actions.update"))
                        .primary()
                        .flex_1()
                        .h_10()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_managed_profile(update_id.clone(), cx)
                        })),
                )
                .child(
                    Button::new(format!("{scope}edit-profile-request:{}", profile.id))
                        .icon(IconName::Settings2)
                        .label(zenclash_i18n::text("profiles.actions.request_settings"))
                        .outline()
                        .h_10()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.profiles.forms.catalog_view.select(&edit_id);
                            this.begin_edit_remote_profile(edit_id.clone(), window, cx);
                        })),
                )
                .when(!active, |row| {
                    row.child(
                        Button::new(format!("{scope}activate-profile:{}", profile.id))
                            .icon(IconName::ArrowRight)
                            .label(zenclash_i18n::text("profiles.actions.activate"))
                            .outline()
                            .h_10()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_managed_profile(activate_id.clone(), cx)
                            })),
                    )
                });
        }
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .flex_wrap()
            .when(!active, |this| {
                this.child(
                    Button::new(format!("{scope}activate-profile:{}", profile.id))
                        .icon(IconName::Play)
                        .label(if active {
                            zenclash_i18n::text("profiles.actions.active")
                        } else {
                            zenclash_i18n::text("profiles.actions.activate")
                        })
                        .small()
                        .h_10()
                        .outline()
                        .disabled(active || self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_managed_profile(activate_id.clone(), cx);
                        })),
                )
            })
            .when(!profile.is_remote(), |this| {
                this.child(super::forms::configuration_edit_button(
                    format!("{scope}edit-profile-config:{}", profile.id),
                    active,
                ))
            })
            .when(profile.is_remote(), |this| {
                this.when(!scope.is_empty(), |this| {
                    this.child(
                        Button::new(format!("{scope}edit-profile-request:{}", profile.id))
                            .icon(IconName::Settings2)
                            .when(!scope.is_empty(), |button| {
                                button
                                    .label(zenclash_i18n::text("profiles.actions.request_settings"))
                            })
                            .accessibility_label(zenclash_i18n::text(
                                "profiles.actions.request_settings",
                            ))
                            .tooltip(zenclash_i18n::text("profiles.actions.request_settings"))
                            .small()
                            .h_10()
                            .ghost()
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.profiles.forms.catalog_view.select(&edit_id);
                                this.begin_edit_remote_profile(edit_id.clone(), window, cx);
                            })),
                    )
                })
                .child(
                    Button::new(format!("{scope}update-profile:{}", profile.id))
                        .icon(crate::assets::AppIcon::RefreshCw)
                        .label(zenclash_i18n::text("profiles.actions.update"))
                        .small()
                        .h_10()
                        .outline()
                        .disabled(self.core_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_managed_profile(update_id.clone(), cx);
                        })),
                )
            })
            .when(scope.is_empty(), |this| {
                this.child(div().flex_1()).child(
                    Button::new(format!("profile-more:{}", profile.id))
                        .label("⋯")
                        .accessibility_label(zenclash_i18n::text_with(
                            "profiles.actions.more",
                            &[("name", profile.name.clone())],
                        ))
                        .small()
                        .h_10()
                        .outline()
                        .disabled(self.core_busy())
                        .dropdown_menu(move |mut menu, _, cx| {
                            let can_delete = menu_owner.upgrade().is_some_and(|page| {
                                let page = page.read(cx);
                                !page.core_busy()
                                    && page.profiles.catalog.active.as_deref()
                                        != Some(menu_id.as_str())
                            });
                            if remote {
                                let owner = menu_owner.clone();
                                let id = menu_id.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(zenclash_i18n::text(
                                        "profiles.actions.request_settings",
                                    ))
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = owner.update(cx, |page, cx| {
                                                page.profiles.forms.catalog_view.select(&id);
                                                page.begin_edit_remote_profile(
                                                    id.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    ),
                                );
                            }
                            let owner = menu_owner.clone();
                            let id = menu_id.clone();
                            menu.item(
                                PopupMenuItem::new(zenclash_i18n::text_with(
                                    "profiles.actions.delete",
                                    &[("name", menu_name.clone())],
                                ))
                                .disabled(!can_delete)
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |page, cx| {
                                        page.delete_managed_profile(id.clone(), cx)
                                    });
                                }),
                            )
                        }),
                )
            })
    }

    fn render_profile_update_policy(
        &self,
        profile: &ProfileRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let download_route = match &profile.source {
            ProfileSource::Remote { options, .. } => options.route(),
            _ => RemoteProfileRoute::Direct,
        };
        let route_id = profile.id.clone();
        let fallback_id = profile.id.clone();
        let route_owner = cx.entity().downgrade();
        let interval_minutes = profile.update_interval_minutes;
        let update_cron = profile.update_cron.clone();
        let auto_update = profile.auto_update;
        let policy_id = profile.id.clone();
        let interval_id = profile.id.clone();
        let runtime_page = cx.entity().downgrade();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w_24()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("profiles.catalog.auto_update")),
                    )
                    .child(
                        Switch::new(format!("auto-update-profile:{}", profile.id))
                            .accessibility_label(zenclash_i18n::text(
                                "profiles.catalog.auto_update",
                            ))
                            .checked(auto_update)
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, enabled, _, cx| {
                                this.set_profile_update_policy(
                                    policy_id.clone(),
                                    *enabled,
                                    interval_minutes,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w_24()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("profiles.catalog.update_interval")),
                    )
                    .child(
                        Button::new(format!("profile-update-interval:{}", profile.id))
                            .label(update_cron.as_deref().map_or_else(
                                || format_update_interval(interval_minutes),
                                |expression| format!("Cron {expression}"),
                            ))
                            .small()
                            .h_10()
                            .outline()
                            .icon(IconName::ChevronDown)
                            .disabled(!auto_update || self.core_busy())
                            .dropdown_menu(move |mut menu, _, _| {
                                for minutes in UPDATE_INTERVALS {
                                    let runtime_page = runtime_page.clone();
                                    let id = interval_id.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(format_update_interval(minutes))
                                            .checked(
                                                update_cron.is_none()
                                                    && minutes == interval_minutes,
                                            )
                                            .on_click(move |_, _, cx| {
                                                let _ = runtime_page.update(cx, |page, cx| {
                                                    page.set_profile_update_policy(
                                                        id.clone(),
                                                        true,
                                                        minutes,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    ),
            )
            .child(h_flex().gap_3().child(div().w_24().text_sm().text_color(theme.muted_foreground).child(zenclash_i18n::text("profiles.design.download_route"))).child(
                Button::new(format!("profile-download-route:{}", profile.id)).label(zenclash_i18n::text(if download_route == RemoteProfileRoute::Mihomo { "profiles.route.proxy" } else { "profiles.route.direct" })).small().ghost().disabled(self.core_busy()).dropdown_menu(move |mut menu, _, _| {
                    for (route, key) in [(RemoteProfileRoute::Mihomo, "profiles.route.proxy"), (RemoteProfileRoute::Direct, "profiles.route.direct")] {
                        let owner = route_owner.clone(); let id = route_id.clone();
                        menu = menu.item(PopupMenuItem::new(zenclash_i18n::text(key)).checked(download_route == route || (route == RemoteProfileRoute::Direct && download_route == RemoteProfileRoute::DirectWithMihomoFallback)).on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |page, cx| page.set_profile_download_route(id.clone(), route, cx));
                        }));
                    }
                    menu
                })
            ))
            .child(h_flex().gap_3().child(div().w_24().text_sm().text_color(theme.muted_foreground).child(zenclash_i18n::text("profiles.form.fallback"))).child(
                Switch::new(format!("profile-download-fallback:{}", profile.id)).accessibility_label(zenclash_i18n::text("profiles.form.fallback")).checked(download_route == RemoteProfileRoute::DirectWithMihomoFallback).disabled(self.core_busy() || download_route == RemoteProfileRoute::Mihomo).on_click(cx.listener(move |this, checked, _, cx| {
                    this.set_profile_download_route(fallback_id.clone(), if *checked { RemoteProfileRoute::DirectWithMihomoFallback } else { RemoteProfileRoute::Direct }, cx);
                }))
            ))
    }
}

fn format_update_interval(minutes: u32) -> String {
    if minutes.is_multiple_of(1_440) {
        zenclash_i18n::text_with(
            "profiles.catalog.every_days",
            &[("count", (minutes / 1_440).to_string())],
        )
    } else if minutes.is_multiple_of(60) {
        zenclash_i18n::text_with(
            "profiles.catalog.every_hours",
            &[("count", (minutes / 60).to_string())],
        )
    } else {
        zenclash_i18n::text_with(
            "profiles.catalog.every_minutes",
            &[("count", minutes.to_string())],
        )
    }
}

fn render_subscription_usage(
    usage: &SubscriptionUsage,
    id: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let quota = quota_percent(usage).map_or_else(
        || {
            zenclash_i18n::text_with(
                "profiles.usage.no_total",
                &[("used", format_bytes(usage.used()))],
            )
        },
        |percent| {
            zenclash_i18n::text_with(
                "profiles.design.usage_percent",
                &[("percent", format!("{percent:.1}"))],
            )
        },
    );
    let percent = quota_percent(usage);
    v_flex()
        .gap_2()
        .child(div().text_sm().child(quota))
        .when_some(percent, |this, percent| {
            this.child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .child(Progress::new(id).h_2().color(theme.chart_3).value(percent)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{percent:.1}%")),
                    ),
            )
        })
        .into_any_element()
}

fn quota_percent(usage: &SubscriptionUsage) -> Option<f32> {
    (usage.total > 0).then(|| {
        let used = u128::from(usage.used().min(usage.total));
        let tenths = used * 1_000 / u128::from(usage.total);
        f32::from(u16::try_from(tenths).unwrap_or(1_000)) / 10.
    })
}

fn subscription_quota_chart(
    _profile: &ProfileRecord,
    usage: &SubscriptionUsage,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    v_flex()
        .gap_2()
        .child(profile_detail_row(
            "profiles.design.used",
            quota_percent(usage).map_or_else(|| "—".into(), |value| format!("{value:.1}%")),
            theme,
        ))
        .child(profile_detail_row(
            "profiles.design.total",
            format!(
                "{} / {}",
                format_bytes(usage.used()),
                format_bytes(usage.total)
            ),
            theme,
        ))
}

fn profile_detail_row(
    key: &str,
    value: String,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w_24()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child(zenclash_i18n::text(key)),
        )
        .child(div().flex_1().min_w_0().child(value))
}

fn format_subscription_remaining(expire: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let valid = i64::try_from(expire)
        .ok()
        .and_then(|timestamp| chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0))
        .is_some();
    if expire > now && valid {
        let days = expire.saturating_sub(now).saturating_add(86_399) / 86_400;
        zenclash_i18n::text_with(
            "profiles.design.remaining_days",
            &[("days", days.to_string())],
        )
    } else {
        format_subscription_expiry_at(expire, now)
    }
}

fn format_subscription_expiry(expire: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_subscription_expiry_at(expire, now)
}

fn format_subscription_expiry_at(expire: u64, now: u64) -> String {
    let date = i64::try_from(expire)
        .ok()
        .and_then(|timestamp| chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0));
    let Some(date) = date.filter(|_| expire != 0) else {
        return zenclash_i18n::text("profiles.usage.expiry_unavailable");
    };
    if expire <= now {
        zenclash_i18n::text("profiles.usage.expired")
    } else {
        let days = expire.saturating_sub(now).saturating_add(86_399) / 86_400;
        zenclash_i18n::text_with(
            "profiles.usage.remaining_date",
            &[
                ("days", days.to_string()),
                ("date", date.format("%Y-%m-%d").to_string()),
            ],
        )
    }
}

fn profile_heading(
    profile: &ProfileRecord,
    active: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    h_flex().gap_2().flex_wrap().child(
        div()
            .px_2()
            .py_0p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(if active || profile.is_remote() {
                theme.list_active
            } else {
                theme.chart_1.opacity(0.08)
            })
            .text_sm()
            .font_weight(gpui_kit::FontWeight::NORMAL)
            .text_color(if active || profile.is_remote() {
                theme.primary
            } else {
                theme.muted_foreground
            })
            .child(if active {
                zenclash_i18n::text("profiles.catalog.current")
            } else {
                profile.source_label()
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        format_subscription_expiry, format_subscription_expiry_at, format_update_interval,
        quota_percent,
    };
    use zenclash_core::SubscriptionUsage;

    #[test]
    fn quota_visualization_preserves_provider_accounting_and_unknown_totals() {
        let usage = SubscriptionUsage {
            upload: 84,
            download: 600,
            total: 2_000,
            expire: 0,
        };
        assert_eq!(quota_percent(&usage), Some(34.2));
        assert_eq!(
            quota_percent(&SubscriptionUsage { total: 0, ..usage }),
            None
        );
        assert_eq!(
            quota_percent(&SubscriptionUsage {
                upload: u64::MAX,
                download: u64::MAX,
                total: 10,
                expire: 0
            }),
            Some(100.)
        );
    }

    #[test]
    fn update_interval_label_preserves_minutes_and_hours() {
        assert!(format_update_interval(15).contains("15"));
        assert!(format_update_interval(360).contains('6'));
        assert!(format_update_interval(1_440).contains('1'));
    }

    #[test]
    fn subscription_expiry_uses_calendar_dates_and_rounds_partial_days() {
        let expire = 1_735_689_600;
        assert_eq!(
            format_subscription_expiry_at(expire, expire - 86_401),
            zenclash_i18n::text_with(
                "profiles.usage.remaining_date",
                &[("days", "2".into()), ("date", "2025-01-01".into())],
            ),
        );
        assert_eq!(
            format_subscription_expiry_at(u64::MAX, 0),
            zenclash_i18n::text("profiles.usage.expiry_unavailable"),
        );
        assert_eq!(
            format_subscription_expiry_at(expire, expire),
            zenclash_i18n::text("profiles.usage.expired"),
        );
    }

    #[test]
    fn subscription_expiry_distinguishes_missing_and_expired_values() {
        let missing = format_subscription_expiry(0);
        let expired = format_subscription_expiry(1);
        assert!(!missing.is_empty());
        assert!(!expired.is_empty());
        assert_ne!(missing, expired);
    }
}
