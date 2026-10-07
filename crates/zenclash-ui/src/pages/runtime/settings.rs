use super::{
    AutostartStatus, Button, Context, Disableable, FluentBuilder, HideTrafficIcon, IconName,
    IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Selectable,
    SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon, Sizable, Styled, div, h_flex, px,
    setting_card, v_flex,
};
use crate::components::sidebar::dispatch_navigate;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::{InteractiveElement, ScrollHandle, TestSupportExt};

pub(super) struct SettingsNavigationState {
    pub(super) scroll: ScrollHandle,
    pub(super) resource_search: gpui_kit::Entity<gpui_kit::component::input::InputState>,
}

impl SettingsNavigationState {
    pub(super) fn new(
        window: &mut gpui_kit::Window,
        cx: &mut Context<RuntimePage>,
    ) -> (Self, gpui_kit::Subscription) {
        use gpui_kit::AppContext;
        use gpui_kit::component::input::{InputEvent, InputState};
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(zenclash_i18n::text("settings_redesign.search_resources"))
        });
        let subscription = cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        (
            Self {
                scroll: ScrollHandle::default(),
                resource_search: search,
            },
            subscription,
        )
    }
}

mod app_update;
mod choice;
pub(super) mod backup;
mod core_management;
pub(super) mod forms;
mod legal;
pub(in crate::pages::runtime) use core_management::CoreManagementUiState;

impl RuntimePage {
    pub(super) fn render_settings(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, autostart) = match &self.data {
            RuntimeData::Settings { config, autostart } => {
                (config.as_ref(), autostart.as_ref().ok())
            }
            _ => (None, None),
        };
        let left = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(self.render_application_settings(config, autostart, theme, cx))
            .child(self.render_backup_card(theme, cx));
        let right = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(
                setting_card(zenclash_i18n::text("settings_redesign.records"), theme)
                    .child(self.traffic_history_setting(theme, cx))
                    .child(div().p_4().child(self.render_clear_history_control(cx)))
                    .child(
                        div()
                            .p_4()
                            .child(self.render_log_preferences_controls(theme, cx)),
                    )
                    .child(
                        div().px_4().pb_4().child(
                            Button::new("settings-open-log-settings")
                                .outline()
                                .small()
                                .label(zenclash_i18n::text("redesign.log_settings_title"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_log_settings(window, cx)
                                })),
                        ),
                    ),
            )
            .child(
                self.render_app_update(theme, cx)
                    .child(self.render_license_info(theme, cx)),
            );
        h_flex()
            .w_full()
            .items_start()
            .gap_4()
            .when(compact, |view| view.flex_col())
            .child(left.when(compact, |view| view.w_full()))
            .child(right.when(compact, |view| view.w_full()))
            .into_any_element()
    }

    pub(super) fn render_settings_navigation(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .flex_wrap()
            .children(Page::SETTINGS.into_iter().map(|page| {
                Button::new(gpui_kit::SharedString::from(format!(
                    "settings-tab-{}",
                    page.route()
                )))
                .label(zenclash_i18n::text(match page {
                    Page::Settings => "unified.settings.general",
                    Page::SystemProxy => "settings_redesign.tab_proxy",
                    Page::Tun => "settings_redesign.tab_tun",
                    Page::Dns => "settings_redesign.tab_dns",
                    Page::Sniffer => "settings_redesign.tab_sniffer",
                    Page::Resources => "settings_redesign.tab_resources",
                    _ => "settings_redesign.tab_core",
                }))
                .small()
                .ghost()
                .min_h_10()
                .selected(self.page == page)
                .when(self.page == page, |button| {
                    button.custom(
                        gpui_kit::component::button::ButtonCustomVariant::new(cx)
                            .color(theme.sidebar_accent)
                            .foreground(theme.primary)
                            .hover(theme.sidebar_accent)
                            .active(theme.sidebar_accent),
                    )
                })
                .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
            }))
    }

    fn render_application_settings(
        &self,
        _config: Option<&RuntimeConfig>,
        autostart: Option<&AutostartStatus>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card(
            zenclash_i18n::text("settings_redesign.appearance_startup"),
            theme,
        )
        .id("settings-appearance-card")
        .test_support()
        .child(theme_setting(theme, self.preferences.appearance))
        .child(self.language_setting(theme, cx))
        .child(super::settings::forms::settings_switch_disabled(
            zenclash_i18n::text("settings.application.autostart.title"),
            if autostart.is_none() {
                zenclash_i18n::text("common.status.unavailable")
            } else if autostart
                .is_some_and(|status| status.enabled && !status.matches_current_executable)
            {
                zenclash_i18n::text("settings.application.autostart.stale")
            } else {
                zenclash_i18n::text("settings.application.autostart.current")
            },
            autostart.is_some_and(|status| status.enabled),
            "settings-autostart",
            theme,
            autostart.is_none() || self.core_busy(),
            cx.listener(|this, checked, _, cx| {
                this.set_autostart(*checked, cx);
            }),
        ))
        .child(tray_setting(theme, self.preferences.traffic_tray_visible))
    }

    fn language_setting(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let selected = self.preferences.language;
        h_flex()
            .min_h(gpui_kit::rems(3.5))
            .px_4()
            .py_3()
            .gap_3()
            .flex_wrap()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(gpui_kit::rems(8.))
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().child(zenclash_i18n::text("language.title"))),
            )
            .child(
                Button::new("settings-language")
                    .outline()
                    .icon(IconName::ChevronDown)
                    .label(zenclash_i18n::text(
                        if selected == zenclash_core::LanguagePreference::ZhCn {
                            "language.zh_cn"
                        } else {
                            "language.en"
                        },
                    ))
                    .disabled(
                        self.mutation_busy(crate::pages::runtime::busy::MutationDomain::Language),
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for (language, key) in [
                            (zenclash_core::LanguagePreference::ZhCn, "language.zh_cn"),
                            (zenclash_core::LanguagePreference::En, "language.en"),
                        ] {
                            let owner = owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(zenclash_i18n::text(key))
                                    .checked(language == selected)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner
                                            .update(cx, |page, cx| page.set_language(language, cx));
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

    fn set_language(
        &mut self,
        language: zenclash_core::LanguagePreference,
        cx: &mut Context<Self>,
    ) {
        if language == self.preferences.language {
            return;
        }
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text_with(
                "settings.language.save_error",
                &[(
                    "error",
                    zenclash_i18n::text("settings.errors.preferences_unavailable"),
                )],
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_scoped_mutation(
            Page::Settings,
            crate::pages::runtime::busy::MutationDomain::Language,
        ) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                store
                    .update(|preferences| preferences.language = language)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        zenclash_i18n::set_locale(preferences.language.locale());
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("language.saved"));
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::Language,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.set_page_error(
                            token,
                            zenclash_i18n::text_with(
                                "settings.language.save_error",
                                &[("error", error)],
                            ),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_autostart(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Settings) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let status = tokio::task::spawn_blocking(move || {
                let manager = zenclash_core::AutostartManager::discover()
                    .map_err(|error| error.to_string())?;
                manager
                    .set_enabled(enabled)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "settings.application.autostart.task_error",
                    &[("error", error.to_string())],
                )
            })??;
            let config = client.runtime_config().await.ok();
            Ok::<_, String>(RuntimeData::Settings {
                config,
                autostart: Ok(status),
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "settings.application.autostart.task_error",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data, cx) {
                            this.notice = Some(if enabled {
                                zenclash_i18n::text("settings.application.autostart.enabled")
                            } else {
                                zenclash_i18n::text("settings.application.autostart.disabled")
                            });
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

    fn traffic_history_setting(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .child(crate::pages::runtime::settings::forms::settings_switch_disabled(
                zenclash_i18n::text("settings.traffic_history.title"),
                zenclash_i18n::text_with(
                    "settings.traffic_history.description",
                    &[("core", self.core_kind.display_name().to_owned())],
                ),
                self.preferences.traffic_history_enabled,
                "settings-traffic-history",
                theme,
                self.mutation_busy(crate::pages::runtime::busy::MutationDomain::TrafficHistory),
                cx.listener(|this, checked, _, cx| {
                    this.set_traffic_history_enabled(*checked, cx);
                }),
            ))
            .child(
                h_flex()
                    .min_h(px(58.))
                    .px_4()
                    .gap_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div().text_sm().child(zenclash_i18n::text(
                                    "settings.traffic_history.retention",
                                )),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                zenclash_i18n::text(
                                    "settings.traffic_history.retention_description",
                                ),
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .children([7_u16, 30, 90].into_iter().enumerate().map(
                                |(index, days)| {
                                    Button::new(("traffic-retention", index))
                                        .label(zenclash_i18n::text_with(
                                            "settings.traffic_history.days",
                                            &[("days", days.to_string())],
                                        ))
                                        .small()
                                        .outline()
                                        .selected(self.preferences.traffic_retention_days == days)
                                        .disabled(
                                            !self.preferences.traffic_history_enabled
                                                || self.mutation_busy(crate::pages::runtime::busy::MutationDomain::TrafficHistory),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.set_traffic_retention(days, cx);
                                        }))
                                },
                            )),
                    ),
            )
    }

    fn set_traffic_history_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.persist_traffic_preferences(
            Some(enabled),
            None,
            if enabled {
                zenclash_i18n::text("settings.traffic_history.enabled")
            } else {
                zenclash_i18n::text("settings.traffic_history.disabled")
            },
            cx,
        );
    }

    fn set_traffic_retention(&mut self, days: u16, cx: &mut Context<Self>) {
        self.persist_traffic_preferences(
            None,
            Some(days),
            zenclash_i18n::text("settings.traffic_history.retention_saved"),
            cx,
        );
    }

    fn persist_traffic_preferences(
        &mut self,
        history_enabled: Option<bool>,
        retention_days: Option<u16>,
        success: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some(zenclash_i18n::text(
                "settings.errors.preferences_unavailable",
            ));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_scoped_mutation(
            Page::Settings,
            crate::pages::runtime::busy::MutationDomain::TrafficHistory,
        ) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                store
                    .update(|preferences| {
                        if let Some(enabled) = history_enabled {
                            preferences.traffic_history_enabled = enabled;
                        }
                        if let Some(days) = retention_days {
                            preferences.traffic_retention_days = days;
                        }
                    })
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "settings.errors.preferences_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "settings.errors.preferences_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(preferences) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(success);
                        }
                        this.accept_preferences(
                            preferences,
                            crate::pages::runtime::PreferenceScope::TrafficHistory,
                            cx,
                        );
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

fn theme_setting(
    _theme: &gpui_kit::component::Theme,
    appearance: zenclash_core::AppearancePreference,
) -> gpui_kit::Div {
    h_flex()
        .min_h_16()
        .flex_wrap()
        .p_4()
        .gap_4()
        .justify_between()
        .child(
            div()
                .text_sm()
                .child(zenclash_i18n::text("settings.appearance.title")),
        )
        .child(
            h_flex().gap_3().flex_wrap().children(
                [
                    (
                        "theme-light",
                        zenclash_core::AppearancePreference::Light,
                        IconName::Sun,
                        "settings.appearance.light",
                    ),
                    (
                        "theme-dark",
                        zenclash_core::AppearancePreference::Dark,
                        IconName::Moon,
                        "settings.appearance.dark",
                    ),
                    (
                        "theme-system",
                        zenclash_core::AppearancePreference::System,
                        IconName::Globe,
                        "settings.appearance.system",
                    ),
                ]
                .into_iter()
                .map(|(id, value, icon, key)| {
                    Button::new(id)
                        .selected(appearance == value)
                        .accessibility_label(zenclash_i18n::text(key))
                        .outline()
                        .h_10()
                        .icon(icon)
                        .label(zenclash_i18n::text(key))
                        .on_click(move |_, window, cx| match value {
                            zenclash_core::AppearancePreference::Light => {
                                window.dispatch_action(Box::new(SetLightTheme), cx)
                            }
                            zenclash_core::AppearancePreference::Dark => {
                                window.dispatch_action(Box::new(SetDarkTheme), cx)
                            }
                            zenclash_core::AppearancePreference::System => {
                                window.dispatch_action(Box::new(SetSystemTheme), cx)
                            }
                        })
                }),
            ),
        )
}

fn tray_setting(theme: &gpui_kit::component::Theme, visible: bool) -> gpui_kit::Div {
    h_flex()
        .min_h(px(58.))
        .px_4()
        .py_3()
        .gap_3()
        .flex_wrap()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex()
                .flex_1()
                .flex_basis(gpui_kit::rems(8.))
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child(zenclash_i18n::text("settings.tray.title")),
                ),
        )
        .child(
            crate::components::mint_switch::MintSwitch::new("settings-tray-visible")
                .accessibility_label(zenclash_i18n::text("settings.tray.title"))
                .checked(visible)
                .on_click(|checked, window, cx| {
                    if *checked {
                        window.dispatch_action(Box::new(ShowTrafficIcon), cx);
                    } else {
                        window.dispatch_action(Box::new(HideTrafficIcon), cx);
                    }
                }),
        )
}
