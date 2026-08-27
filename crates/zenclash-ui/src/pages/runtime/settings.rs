use super::{
    AutostartStatus, Button, Context, Disableable, FluentBuilder, HideTrafficIcon, IconName,
    IntoElement, Page, ParentElement, PreferencesRestored, RuntimeConfig, RuntimeData, RuntimePage,
    Selectable, SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon, Sizable, Styled, div,
    h_flex, info_row, json, px, setting_card, setting_switch, v_flex,
};
use crate::components::sidebar::dispatch_navigate;

mod app_update;
mod backup;
mod core_management;
pub(in crate::pages::runtime) use app_update::AppUpdateUiState;
pub(in crate::pages::runtime) use core_management::CoreManagementUiState;

impl RuntimePage {
    pub(super) fn render_offline_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(self.render_core_management(theme, cx))
    }

    pub(super) fn render_settings(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, autostart) = match &self.data {
            RuntimeData::Settings { config, autostart } => (config.clone(), autostart.clone()),
            _ => (
                self.config().cloned().unwrap_or_default(),
                AutostartStatus::default(),
            ),
        };
        v_flex()
            .gap_4()
            .child(self.render_advanced_tools(theme))
            .child(self.render_core_management(theme, cx))
            .child(self.render_app_update(theme, cx))
            .child(self.render_application_settings(&config, &autostart, theme, cx))
            .when(self.core_kind.is_experimental(), |this| {
                this.child(super::message_banner(
                    zenclash_i18n::text("settings.experimental_core"),
                    theme.warning,
                    theme,
                ))
            })
            .child(self.render_backup_card(theme, cx))
            .into_any_element()
    }

    fn render_advanced_tools(&self, theme: &gpui_component::Theme) -> impl IntoElement {
        setting_card(zenclash_i18n::text("settings.advanced_tools.title"), theme).child(
            v_flex()
                .p_4()
                .gap_4()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("settings.advanced_tools.description")),
                )
                .child(advanced_tool_group(
                    zenclash_i18n::text("settings.advanced_tools.proxy.title"),
                    zenclash_i18n::text("settings.advanced_tools.proxy.description"),
                    &[Page::SystemProxy, Page::Tun],
                    theme,
                ))
                .child(advanced_tool_group(
                    zenclash_i18n::text("settings.advanced_tools.configuration.title"),
                    zenclash_i18n::text("settings.advanced_tools.configuration.description"),
                    &[
                        Page::Rules,
                        Page::Dns,
                        Page::Sniffer,
                        Page::Resources,
                        Page::Override,
                    ],
                    theme,
                ))
                .child(advanced_tool_group(
                    zenclash_i18n::text("settings.advanced_tools.diagnostics.title"),
                    zenclash_i18n::text("settings.advanced_tools.diagnostics.description"),
                    &[Page::Network, Page::Traffic, Page::Logs, Page::Mihomo],
                    theme,
                )),
        )
    }

    fn render_application_settings(
        &self,
        config: &RuntimeConfig,
        autostart: &AutostartStatus,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card(zenclash_i18n::text("settings.application.title"), theme)
            .child(info_row(
                zenclash_i18n::text("settings.application.controller"),
                self.client.endpoint().controller.clone(),
                theme,
            ))
            .child(setting_switch(
                zenclash_i18n::text("settings.application.autostart.title"),
                if autostart.enabled && !autostart.matches_current_executable {
                    zenclash_i18n::text("settings.application.autostart.stale")
                } else {
                    zenclash_i18n::text("settings.application.autostart.current")
                },
                autostart.enabled,
                "settings-autostart",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.set_autostart(*checked, cx);
                }),
            ))
            .child(info_row(
                zenclash_i18n::text("settings.application.autostart.location"),
                if autostart.location.is_empty() {
                    zenclash_i18n::text("settings.application.autostart.waiting")
                } else {
                    autostart.location.clone()
                },
                theme,
            ))
            .child(setting_switch(
                "IPv6",
                zenclash_i18n::text_with(
                    "settings.application.ipv6.description",
                    &[("core", self.core_kind.display_name().to_owned())],
                ),
                config.ipv6,
                "settings-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"ipv6": *checked}),
                        zenclash_i18n::text("settings.application.ipv6.saved"),
                        cx,
                    );
                }),
            ))
            .child(self.language_setting(theme, cx))
            .child(theme_setting(theme))
            .child(tray_setting(theme))
            .child(self.traffic_history_setting(theme, cx))
    }

    fn language_setting(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .child(div().text_sm().child(zenclash_i18n::text("language.title")))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("language.description")),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("language-zh-cn")
                            .label(zenclash_i18n::text("language.zh_cn"))
                            .small()
                            .outline()
                            .selected(
                                self.preferences.language
                                    == zenclash_core::LanguagePreference::ZhCn,
                            )
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_language(zenclash_core::LanguagePreference::ZhCn, cx);
                            })),
                    )
                    .child(
                        Button::new("language-en")
                            .label(zenclash_i18n::text("language.en"))
                            .small()
                            .outline()
                            .selected(
                                self.preferences.language == zenclash_core::LanguagePreference::En,
                            )
                            .disabled(self.mutating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_language(zenclash_core::LanguagePreference::En, cx);
                            })),
                    ),
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
        let Some(token) = self.begin_mutation(Page::Settings) else {
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
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        zenclash_i18n::set_locale(preferences.language.locale());
                        this.preferences = preferences.clone();
                        this.notice = Some(zenclash_i18n::text("language.saved"));
                        cx.emit(PreferencesRestored { preferences });
                    }
                    Ok(_) => {}
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
            let config = client
                .runtime_config()
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(RuntimeData::Settings {
                config,
                autostart: status,
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
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
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
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .child(setting_switch(
                zenclash_i18n::text("settings.traffic_history.title"),
                zenclash_i18n::text_with(
                    "settings.traffic_history.description",
                    &[("core", self.core_kind.display_name().to_owned())],
                ),
                self.preferences.traffic_history_enabled,
                "settings-traffic-history",
                theme,
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
                                                || self.mutating,
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
        let Some(token) = self.begin_mutation(Page::Settings) else {
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
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        this.preferences = preferences.clone();
                        this.notice = Some(success);
                        cx.emit(PreferencesRestored { preferences });
                    }
                    Ok(_) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn advanced_tool_group(
    label: String,
    description: String,
    pages: &'static [Page],
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .items_start()
        .gap_4()
        .child(
            v_flex()
                .w_32()
                .flex_none()
                .gap_1()
                .child(div().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(description),
                ),
        )
        .child(
            h_flex()
                .flex_1()
                .gap_2()
                .flex_wrap()
                .children(pages.iter().copied().map(advanced_tool_button)),
        )
        .into_any_element()
}

fn advanced_tool_button(page: Page) -> Button {
    Button::new(page.route())
        .icon(page.icon())
        .label(page.label())
        .small()
        .outline()
        .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
}

fn theme_setting(theme: &gpui_component::Theme) -> gpui::Div {
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
                    div()
                        .text_sm()
                        .child(zenclash_i18n::text("settings.appearance.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("settings.appearance.description")),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("theme-system")
                        .icon(IconName::Globe)
                        .label(zenclash_i18n::text("settings.appearance.system"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SetSystemTheme), cx);
                        }),
                )
                .child(
                    Button::new("theme-light")
                        .icon(IconName::Sun)
                        .label(zenclash_i18n::text("settings.appearance.light"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SetLightTheme), cx);
                        }),
                )
                .child(
                    Button::new("theme-dark")
                        .icon(IconName::Moon)
                        .label(zenclash_i18n::text("settings.appearance.dark"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SetDarkTheme), cx);
                        }),
                ),
        )
}

fn tray_setting(theme: &gpui_component::Theme) -> gpui::Div {
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
                    div()
                        .text_sm()
                        .child(zenclash_i18n::text("settings.tray.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(zenclash_i18n::text("settings.tray.description")),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("tray-show")
                        .label(zenclash_i18n::text("common.actions.show"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ShowTrafficIcon), cx);
                        }),
                )
                .child(
                    Button::new("tray-hide")
                        .label(zenclash_i18n::text("common.actions.hide"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(HideTrafficIcon), cx);
                        }),
                ),
        )
}
