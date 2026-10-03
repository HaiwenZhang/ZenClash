use super::{
    AutostartStatus, Button, Context, Disableable, FluentBuilder, HideTrafficIcon, IconName,
    IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Selectable,
    SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon, Sizable, Styled, div, h_flex,
    info_row, json, px, setting_card, setting_switch, v_flex,
};
use crate::components::sidebar::dispatch_navigate;
use gpui_kit::component::{button::ButtonVariants, scroll::ScrollableElement};
use gpui_kit::{
    InteractiveElement, ScrollAnchor, ScrollHandle, StatefulInteractiveElement, TestSupportExt,
};

pub(super) struct SettingsNavigationState {
    pub(super) scroll: ScrollHandle,
    appearance: ScrollAnchor,
    startup: ScrollAnchor,
    records: ScrollAnchor,
    backup: ScrollAnchor,
    selected: usize,
}

impl Default for SettingsNavigationState {
    fn default() -> Self {
        let scroll = ScrollHandle::default();
        Self {
            appearance: ScrollAnchor::for_handle(scroll.clone()),
            startup: ScrollAnchor::for_handle(scroll.clone()),
            records: ScrollAnchor::for_handle(scroll.clone()),
            backup: ScrollAnchor::for_handle(scroll.clone()),
            scroll,
            selected: 0,
        }
    }
}

mod app_update;
pub(super) mod backup;
mod core_management;
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
        let (config, autostart, autostart_error) = match &self.data {
            RuntimeData::Settings { config, autostart } => (
                config.as_ref(),
                autostart.as_ref().ok(),
                autostart.as_ref().err(),
            ),
            _ => (None, None, None),
        };
        let primary = v_flex()
            .flex_basis(gpui_kit::rems(26.))
            .flex_grow(1.)
            .max_w_full()
            .min_w_0()
            .gap_4()
            .when(config.is_none(), |this| {
                this.child(super::message_banner(
                    zenclash_i18n::text("runtime.empty.unavailable"),
                    theme.warning,
                    theme,
                ))
            })
            .when_some(autostart_error, |this, error| {
                this.child(super::message_banner(error.clone(), theme.warning, theme))
            })
            .child(self.render_application_settings(config, autostart, theme, cx))
            .when(self.core_kind.is_experimental(), |this| {
                this.child(super::message_banner(
                    zenclash_i18n::text("settings.experimental_core"),
                    theme.warning,
                    theme,
                ))
            })
            .child(
                div()
                    .id("settings-backup-card")
                    .anchor_scroll(Some(self.settings_navigation.backup.clone()))
                    .child(self.render_backup_card(theme, cx)),
            )
            .child(self.render_core_management(theme, cx));
        let inspector = v_flex()
            .w(gpui_kit::rems(20.))
            .max_w_full()
            .min_w_0()
            .gap_4()
            .child(self.render_version_info(theme))
            .child(self.render_app_update(theme, cx))
            .child(self.render_advanced_tools(theme))
            .child(self.render_local_data_status(theme));
        h_flex()
            .w_full()
            .flex_wrap()
            .items_start()
            .gap_4()
            .child(primary)
            .child(inspector)
            .into_any_element()
    }

    pub(super) fn render_settings_navigation(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sections = [
            ("settings.design.general", None),
            (
                "settings.design.appearance",
                Some(self.settings_navigation.appearance.clone()),
            ),
            (
                "settings.design.startup",
                Some(self.settings_navigation.startup.clone()),
            ),
            (
                "settings.design.records",
                Some(self.settings_navigation.records.clone()),
            ),
            (
                "backup.local.title",
                Some(self.settings_navigation.backup.clone()),
            ),
        ];
        v_flex()
            .id("settings-navigation-scroll")
            .w(gpui_kit::rems(12.))
            .h_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .max_w_full()
            .flex_shrink_0()
            .gap_1()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .children(
                sections
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, anchor))| {
                        Button::new(("settings-section", index))
                            .accessibility_label(zenclash_i18n::text(label))
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(gpui_kit::component::Icon::new(match index {
                                        0 => IconName::Settings2,
                                        1 => IconName::Sun,
                                        2 => IconName::CircleCheck,
                                        3 => IconName::ChartPie,
                                        _ => IconName::FolderOpen,
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(zenclash_i18n::text(label)),
                                    ),
                            )
                            .min_h_10()
                            .justify_start()
                            .small()
                            .ghost()
                            .w_full()
                            .selected(self.settings_navigation.selected == index)
                            .when(self.settings_navigation.selected == index, |this| {
                                this.bg(theme.primary.opacity(0.12))
                                    .text_color(theme.primary)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.settings_navigation.selected = index;
                                if let Some(anchor) = &anchor {
                                    anchor.scroll_to(window, cx);
                                } else {
                                    this.settings_navigation.scroll.set_offset(gpui_kit::point(
                                        gpui_kit::px(0.),
                                        gpui_kit::px(0.),
                                    ));
                                }
                                cx.notify();
                            }))
                    }),
            )
            .child(div().my_2().border_t_1().border_color(theme.border))
            .children(
                PROXY_TOOL_PAGES
                    .into_iter()
                    .chain(CONFIGURATION_TOOL_PAGES)
                    .chain(DIAGNOSTIC_TOOL_PAGES)
                    .map(|page| {
                        Button::new((gpui_kit::ElementId::from("settings-tool"), page.route()))
                            .accessibility_label(page.label())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(gpui_kit::component::Icon::new(page.icon()))
                                    .child(div().min_w_0().truncate().child(page.label())),
                            )
                            .small()
                            .ghost()
                            .w_full()
                            .justify_start()
                            .on_click(move |_, window, cx| dispatch_navigate(page, window, cx))
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
        config: Option<&RuntimeConfig>,
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
            .child(info_row(
                zenclash_i18n::text("settings.application.controller"),
                self.client.endpoint().map_or_else(
                    || zenclash_i18n::text("settings.application.service_managed_controller"),
                    |endpoint| endpoint.controller,
                ),
                theme,
            ))
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
            .child(info_row(
                zenclash_i18n::text("settings.application.autostart.location"),
                if autostart.is_none() {
                    zenclash_i18n::text("common.status.unavailable")
                } else if autostart.is_some_and(|status| status.location.is_empty()) {
                    zenclash_i18n::text("settings.application.autostart.waiting")
                } else {
                    autostart
                        .map(|status| status.location.clone())
                        .unwrap_or_default()
                },
                theme,
            ))
            .when_some(config, |this, config| {
                this.child(setting_switch(
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
            })
            .child(tray_setting(theme));
        let records = setting_card(zenclash_i18n::text("settings.design.records"), theme)
            .id("settings-records-card")
            .anchor_scroll(Some(self.settings_navigation.records.clone()))
            .child(self.traffic_history_setting(theme, cx))
            .child(div().p_4().child(self.render_clear_history_control(cx)))
            .child(self.render_log_preferences_controls(theme, cx));
        v_flex()
            .gap_4()
            .child(appearance)
            .child(startup)
            .child(records)
    }

    fn language_setting(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .flex_shrink_0()
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
                            .disabled(self.mutation_busy(
                                crate::pages::runtime::busy::MutationDomain::Language,
                            ))
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
                            .disabled(self.mutation_busy(
                                crate::pages::runtime::busy::MutationDomain::Language,
                            ))
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
    theme: &gpui_kit::component::Theme,
    appearance: zenclash_core::AppearancePreference,
) -> gpui_kit::Div {
    h_flex()
        .min_h_24()
        .flex_wrap()
        .p_4()
        .gap_4()
        .items_start()
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
                        .h(gpui_kit::rems(7.))
                        .w_32()
                        .child(theme_preview(icon, key, appearance == value, theme))
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

fn theme_preview(
    icon: IconName,
    label: &str,
    selected: bool,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Div {
    let background = match icon {
        IconName::Moon => crate::design::color(crate::design::DEEP_INK),
        IconName::Sun => crate::design::color(crate::design::LIGHT_PANEL),
        _ => theme.background,
    };
    v_flex()
        .gap_2()
        .items_center()
        .child(
            h_flex()
                .w_24()
                .h(gpui_kit::rems(3.5))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(background)
                .overflow_hidden()
                .child(div().w_3().h_full().bg(theme.primary.opacity(0.25)))
                .child(
                    div().flex_1().flex().justify_center().child(
                        gpui_kit::component::Icon::new(icon)
                            .size_4()
                            .text_color(theme.primary),
                    ),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .when(selected, |this| {
                    this.child(gpui_kit::component::Icon::new(IconName::Check).size_3())
                })
                .child(div().text_xs().child(zenclash_i18n::text(label))),
        )
}

fn tray_setting(theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
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
            h_flex()
                .flex_shrink_0()
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
