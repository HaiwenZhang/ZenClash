use super::{
    AutostartStatus, Button, Context, Disableable, FluentBuilder, HideTrafficIcon, IconName,
    IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Selectable,
    SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon, Sizable, Styled, div, h_flex,
    info_row, px, setting_card, v_flex,
};
use crate::components::sidebar::dispatch_navigate;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::{
    InteractiveElement, ScrollAnchor, ScrollHandle, StatefulInteractiveElement, TestSupportExt,
};

pub(super) struct SettingsNavigationState {
    pub(super) scroll: ScrollHandle,
    appearance: ScrollAnchor,
    startup: ScrollAnchor,
    records: ScrollAnchor,
    selected: usize,
    advanced_network: bool,
}

impl Default for SettingsNavigationState {
    fn default() -> Self {
        let scroll = ScrollHandle::default();
        Self {
            appearance: ScrollAnchor::for_handle(scroll.clone()),
            startup: ScrollAnchor::for_handle(scroll.clone()),
            records: ScrollAnchor::for_handle(scroll.clone()),
            scroll,
            selected: 0,
            advanced_network: false,
        }
    }
}

mod app_update;
pub(super) mod backup;
mod core_management;
mod legal;
pub(in crate::pages::runtime) use app_update::AppUpdateUiState;
pub(in crate::pages::runtime) use core_management::CoreManagementUiState;

const PROXY_TOOL_PAGES: [Page; 2] = [Page::SystemProxy, Page::Tun];
const CONFIGURATION_TOOL_PAGES: [Page; 4] =
    [Page::Dns, Page::Sniffer, Page::Resources, Page::Override];
const DIAGNOSTIC_TOOL_PAGES: [Page; 1] = [Page::Mihomo];

impl RuntimePage {
    pub(super) fn render_settings(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (config, autostart, _autostart_error) = match &self.data {
            RuntimeData::Settings { config, autostart } => (
                config.as_ref(),
                autostart.as_ref().ok(),
                autostart.as_ref().err(),
            ),
            _ => (None, None, None),
        };
        let body = v_flex()
            .w_full()
            .max_w(gpui_kit::rems(52.))
            .min_w_0()
            .gap_4();
        match self.settings_navigation.selected {
            1 => body
                .child(self.render_network_capture_settings(config, theme, cx))
                .child(
                    Button::new("settings-advanced-network")
                        .label(zenclash_i18n::text("unified.network.advanced"))
                        .outline()
                        .selected(self.settings_navigation.advanced_network)
                        .icon(if self.settings_navigation.advanced_network {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings_navigation.advanced_network =
                                !this.settings_navigation.advanced_network;
                            cx.notify();
                        })),
                )
                .when(self.settings_navigation.advanced_network, |body| {
                    body.child(self.render_advanced_tools(theme))
                        .child(self.render_core_management(theme, cx))
                })
                .child(
                    Button::new("settings-network-diagnostics")
                        .label(Page::Network.label())
                        .outline()
                        .on_click(|_, window, cx| dispatch_navigate(Page::Network, window, cx)),
                )
                .into_any_element(),
            2 => body
                .child(self.render_application_settings(config, autostart, theme, cx))
                .child(self.render_backup_card(theme, cx))
                .child(self.render_local_data_status(theme))
                .into_any_element(),
            3 => body
                .child(self.render_version_info(theme))
                .child(self.render_app_update(theme, cx))
                .child(self.render_license_info(theme, cx))
                .into_any_element(),
            _ => body
                .child(self.render_application_settings(config, autostart, theme, cx))
                .into_any_element(),
        }
    }

    pub(super) fn render_settings_navigation(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .children(
                ["general", "network", "data", "about"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, section)| {
                        Button::new(("settings-section", index))
                            .label(zenclash_i18n::text(&format!("unified.settings.{section}")))
                            .small()
                            .ghost()
                            .min_h_10()
                            .selected(self.settings_navigation.selected == index)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings_navigation.selected = index;
                                this.settings_navigation
                                    .scroll
                                    .set_offset(gpui_kit::point(px(0.), px(0.)));
                                cx.notify();
                            }))
                    }),
            )
    }

    fn render_local_data_status(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        let count = |available: bool, value: usize| {
            if !self.persistent_loading && available {
                value.to_string()
            } else {
                zenclash_i18n::text("common.status.unknown")
            }
        };
        let enabled = |value: bool| {
            zenclash_i18n::text(if value {
                "common.status.enabled"
            } else {
                "common.status.disabled"
            })
        };
        div().id("settings-local-data").test_support().child(
            setting_card(zenclash_i18n::text("settings.local_data.title"), theme)
                .child(info_row(
                    zenclash_i18n::text("settings.local_data.profiles"),
                    count(
                        self.profiles.store.is_some(),
                        self.profiles.catalog.profiles.len(),
                    ),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("settings.local_data.overrides"),
                    count(
                        self.overrides.store.is_some(),
                        self.overrides.catalog.items.len(),
                    ),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("settings.local_data.history"),
                    enabled(self.preferences.traffic_history_enabled),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("settings.local_data.logs"),
                    enabled(self.preferences.log_file_enabled),
                    theme,
                )),
        )
    }

    fn render_version_info(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        let snapshot = self.operational_status.snapshot();
        let bundled = env!("ZENCLASH_BUILD_MIHOMO_VERSION");
        let running = match &snapshot.controller {
            zenclash_core::Observation::Fresh { value, .. }
                if !value.version.version.is_empty() =>
            {
                format!(
                    "{} {}",
                    self.core_kind.display_name(),
                    value.version.version
                )
            }
            _ => zenclash_i18n::text("settings.versions.unavailable"),
        };
        setting_card(zenclash_i18n::text("settings.versions.title"), theme)
            .child(info_row(
                zenclash_i18n::text("settings.versions.app"),
                env!("ZENCLASH_BUILD_VERSION"),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("settings.versions.bundled"),
                if bundled.is_empty() {
                    zenclash_i18n::text("settings.versions.not_bundled")
                } else {
                    bundled.to_owned()
                },
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("settings.versions.running"),
                running,
                theme,
            ))
    }

    fn render_advanced_tools(&self, theme: &gpui_kit::component::Theme) -> impl IntoElement {
        setting_card(zenclash_i18n::text("settings.advanced_tools.title"), theme).child(
            v_flex().px_4().pb_4().gap_1().children(
                PROXY_TOOL_PAGES
                    .into_iter()
                    .chain(CONFIGURATION_TOOL_PAGES)
                    .chain(DIAGNOSTIC_TOOL_PAGES)
                    .map(|page| {
                        Button::new(page.route())
                            .accessibility_label(page.label())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(gpui_kit::component::Icon::new(page.icon()))
                                    .child(div().min_w_0().truncate().child(page.label())),
                            )
                            .ghost()
                            .w_full()
                            .min_h_12()
                            .justify_start()
                            .tooltip(page.subtitle())
                            .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
                    }),
            ),
        )
    }

    fn render_application_settings(
        &self,
        _config: Option<&RuntimeConfig>,
        autostart: Option<&AutostartStatus>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let appearance = setting_card(zenclash_i18n::text("settings.design.appearance"), theme)
            .id("settings-appearance-card")
            .test_support()
            .anchor_scroll(Some(self.settings_navigation.appearance.clone()))
            .child(theme_setting(theme, self.preferences.appearance))
            .child(self.language_setting(theme, cx));
        let startup = setting_card(zenclash_i18n::text("settings.design.startup"), theme)
            .id("settings-startup-card")
            .test_support()
            .anchor_scroll(Some(self.settings_navigation.startup.clone()))
            .child(super::common::setting_switch_disabled(
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
            .child(tray_setting(theme, self.preferences.traffic_tray_visible));
        let records = setting_card(zenclash_i18n::text("settings.design.records"), theme)
            .id("settings-records-card")
            .anchor_scroll(Some(self.settings_navigation.records.clone()))
            .child(self.traffic_history_setting(theme, cx))
            .child(div().p_4().child(self.render_clear_history_control(cx)))
            .child(self.render_log_preferences_controls(theme, cx));
        v_flex()
            .gap_4()
            .when(self.settings_navigation.selected == 0, |body| {
                body.child(appearance).child(startup)
            })
            .when(self.settings_navigation.selected == 2, |body| {
                body.child(records)
            })
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
                    .flex_basis(gpui_kit::rems(16.))
                    .min_w_0()
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
            .child(crate::pages::runtime::common::setting_switch_disabled(
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
                .flex_basis(gpui_kit::rems(16.))
                .min_w_0()
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

#[cfg(test)]
mod tests {
    use super::{CONFIGURATION_TOOL_PAGES, DIAGNOSTIC_TOOL_PAGES, PROXY_TOOL_PAGES, Page};

    #[test]
    fn sidebar_runtime_destinations_are_not_repeated_in_application_settings() {
        let settings_tools = PROXY_TOOL_PAGES
            .into_iter()
            .chain(CONFIGURATION_TOOL_PAGES)
            .chain(DIAGNOSTIC_TOOL_PAGES)
            .collect::<Vec<_>>();

        for page in [Page::Rules, Page::Network, Page::Traffic, Page::Logs] {
            assert!(!settings_tools.contains(&page));
        }
    }
}
