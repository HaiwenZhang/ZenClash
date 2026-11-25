use gpui_kit::component::{
    Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Context, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, div,
};
use zenclash_core::{ProfileRecord, ProfileSource, RemoteProfileRoute};

use super::presentation::{
    format_subscription_expiry, format_subscription_remaining, format_update_interval,
    profile_heading, render_subscription_usage,
};
use crate::components::mint_switch::MintSwitch as Switch;
use crate::components::page_primitives::format_profile_age;
use crate::features::host::RuntimePage;

const UPDATE_INTERVALS: [u32; 4] = [60, 6 * 60, 12 * 60, 24 * 60];

impl RuntimePage {
    pub(super) fn render_managed_profile(
        &self,
        profile: &ProfileRecord,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let active = self.profiles.catalog().active.as_deref() == Some(profile.id.as_str());
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
                crate::foundation::design::brand_accent(theme)
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

    pub(super) fn render_profile_actions(
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
                        .icon(crate::foundation::assets::AppIcon::RefreshCw)
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
                this.child(super::super::forms::configuration_edit_button(
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
                        .icon(crate::foundation::assets::AppIcon::RefreshCw)
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
                                let catalog = page.profiles.catalog();
                                !page.core_busy()
                                    && (catalog.profiles.len() <= 1
                                        || catalog.active.as_deref() != Some(menu_id.as_str()))
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

    pub(super) fn render_profile_update_policy(
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
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w_24()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("profiles.design.download_route")),
                    )
                    .child(
                        Button::new(format!("profile-download-route:{}", profile.id))
                            .label(zenclash_i18n::text(
                                if download_route == RemoteProfileRoute::Mihomo {
                                    "profiles.route.proxy"
                                } else {
                                    "profiles.route.direct"
                                },
                            ))
                            .small()
                            .ghost()
                            .disabled(self.core_busy())
                            .dropdown_menu(move |mut menu, _, _| {
                                for (route, key) in [
                                    (RemoteProfileRoute::Mihomo, "profiles.route.proxy"),
                                    (RemoteProfileRoute::Direct, "profiles.route.direct"),
                                ] {
                                    let owner = route_owner.clone();
                                    let id = route_id.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(zenclash_i18n::text(key))
                                            .checked(
                                                download_route == route
                                                    || (route == RemoteProfileRoute::Direct
                                                        && download_route
                                                            == RemoteProfileRoute::DirectWithMihomoFallback),
                                            )
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |page, cx| {
                                                    page.set_profile_download_route(
                                                        id.clone(),
                                                        route,
                                                        cx,
                                                    )
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
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
                            .child(zenclash_i18n::text("profiles.form.fallback")),
                    )
                    .child(
                        Switch::new(format!("profile-download-fallback:{}", profile.id))
                            .accessibility_label(zenclash_i18n::text("profiles.form.fallback"))
                            .checked(download_route == RemoteProfileRoute::DirectWithMihomoFallback)
                            .disabled(
                                self.core_busy() || download_route == RemoteProfileRoute::Mihomo,
                            )
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.set_profile_download_route(
                                    fallback_id.clone(),
                                    if *checked {
                                        RemoteProfileRoute::DirectWithMihomoFallback
                                    } else {
                                        RemoteProfileRoute::Direct
                                    },
                                    cx,
                                );
                            })),
                    ),
            )
    }
}
