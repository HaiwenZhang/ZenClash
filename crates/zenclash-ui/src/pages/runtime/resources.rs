use super::settings::forms::settings_switch as setting_switch;
use super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, InteractiveElement,
    IntoElement, Page, ParentElement, ProviderCatalog, ProviderKind, RuntimeConfig, RuntimeData,
    RuntimePage, Sizable, Styled, div, empty_dash, empty_state, format_profile_age, h_flex,
    info_row, json, load_page, px, setting_card, v_flex,
};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, WindowExt};

mod ruleset;

pub(super) use ruleset::RulesetUiState;

#[derive(Clone, Copy)]
enum BuiltinResource {
    GeoData,
    ExternalUi,
}

impl RuntimePage {
    fn update_builtin_resource(&mut self, resource: BuiltinResource, cx: &mut Context<Self>) {
        let supported = match resource {
            BuiltinResource::GeoData => self.core_kind.capabilities().geodata_update,
            BuiltinResource::ExternalUi => self.core_kind.capabilities().external_ui_update,
        };
        if !supported {
            self.error = Some(zenclash_i18n::text_with(
                "resources.errors.unsupported",
                &[("core", self.core_kind.display_name().to_owned())],
            ));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let client = self.client.clone();
        let session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            match resource {
                BuiltinResource::GeoData => session.update_geodata().await,
                BuiltinResource::ExternalUi => session.update_external_ui().await,
            }
            .map_err(|error| error.to_string())?;
            load_page(client, Page::Resources).await
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "resources.errors.builtin_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data, cx) {
                            this.notice = Some(match resource {
                                BuiltinResource::GeoData => {
                                    zenclash_i18n::text("resources.notices.geodata")
                                }
                                BuiltinResource::ExternalUi => {
                                    zenclash_i18n::text("resources.notices.external_ui")
                                }
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

    fn update_provider(&mut self, name: String, is_rule: bool, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let operations = self.provider_operations.clone();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            operations
                .update(
                    if is_rule {
                        ProviderKind::Rule
                    } else {
                        ProviderKind::Proxy
                    },
                    &name,
                )
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Resources).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "resources.errors.provider_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data, cx) {
                            this.notice = Some(zenclash_i18n::text("resources.notices.provider"));
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_all_providers(&mut self, cx: &mut Context<Self>) {
        let RuntimeData::Resources { proxy, rules, .. } = &self.data else {
            return;
        };
        let targets = proxy
            .providers
            .keys()
            .map(|name| (ProviderKind::Proxy, name.clone()))
            .chain(
                rules
                    .providers
                    .keys()
                    .map(|name| (ProviderKind::Rule, name.clone())),
            )
            .collect::<Vec<_>>();
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let operations = self.provider_operations.clone();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            let mut errors = Vec::new();
            for (kind, name) in targets {
                if let Err(error) = operations.update(kind, &name).await {
                    errors.push(format!("{name}: {error}"));
                }
            }
            (load_page(client, Page::Resources).await, errors)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((data, errors)) => {
                        match data {
                            Ok(data) => {
                                this.replace_page_data(token, data, cx);
                            }
                            Err(error) => this.set_page_error(token, error),
                        }
                        if !errors.is_empty() {
                            this.set_page_error(token, errors.join("\n"));
                        }
                    }
                    Err(error) => this.set_page_error(token, error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn healthcheck_provider(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Resources) else {
            return;
        };
        let operations = self.provider_operations.clone();
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            operations
                .healthcheck_proxy(&name)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Resources).await
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "resources.errors.healthcheck_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data, cx) {
                            this.notice =
                                Some(zenclash_i18n::text("resources.notices.healthcheck"));
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_resources(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let fallback_config = RuntimeConfig::default();
        let fallback_proxy = ProviderCatalog::default();
        let fallback_rules = ProviderCatalog::default();
        let (config, proxy, rules) = match &self.data {
            RuntimeData::Resources {
                config,
                proxy,
                rules,
            } => (config, proxy, rules),
            _ => (&fallback_config, &fallback_proxy, &fallback_rules),
        };
        v_flex()
            .gap_4()
            .child(self.render_builtin_resources(config, theme, cx))
            .child(
                setting_card(zenclash_i18n::text("settings_redesign.providers"), theme)
                    .child(
                        h_flex()
                            .px_4()
                            .pb_3()
                            .gap_3()
                            .child(gpui_kit::component::input::Input::new(
                                &self.settings_navigation.resource_search,
                            ))
                            .child(
                                Button::new("update-all-providers")
                                    .label(zenclash_i18n::text("unified.profiles.update_all"))
                                    .outline()
                                    .disabled(self.core_busy())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.update_all_providers(cx)),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .px_4()
                            .py_3()
                            .gap_3()
                            .flex_wrap()
                            .bg(theme.muted)
                            .text_sm()
                            .child(
                                div()
                                    .flex_1()
                                    .child(zenclash_i18n::text("settings_redesign.resource_name")),
                            )
                            .child(
                                div()
                                    .w_16()
                                    .child(zenclash_i18n::text("settings_redesign.resource_type")),
                            )
                            .child(
                                div()
                                    .w_16()
                                    .child(zenclash_i18n::text("settings_redesign.resource_count")),
                            )
                            .child(
                                div().w_32().child(zenclash_i18n::text(
                                    "settings_redesign.resource_updated",
                                )),
                            )
                            .child(
                                div().w(gpui_kit::rems(15.)).child(zenclash_i18n::text(
                                    "settings_redesign.resource_actions",
                                )),
                            ),
                    )
                    .child(self.render_provider_section(proxy, false, theme, cx))
                    .child(self.render_provider_section(rules, true, theme, cx)),
            )
            .child(
                Button::new("open-ruleset-converter")
                    .label(zenclash_i18n::text("settings_redesign.converter"))
                    .outline()
                    .on_click(cx.listener(|_, _, window, cx| {
                        let owner = cx.entity().downgrade();
                        window.open_dialog(cx, move |dialog, window, cx| {
                            let content = owner
                                .update(cx, |page, cx| {
                                    let theme = cx.theme().clone();
                                    page.render_ruleset_converter(&theme, cx).into_any_element()
                                })
                                .ok();
                            dialog
                                .title(zenclash_i18n::text("settings_redesign.converter"))
                                .bg(cx.theme().group_box)
                                .width(window.rem_size() * 48.)
                                .when_some(content, |dialog, content| dialog.child(content))
                        });
                    })),
            )
            .into_any_element()
    }

    fn render_builtin_resources(
        &self,
        config: &RuntimeConfig,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let owner = cx.entity().downgrade();
        let controlled = &self.controlled_config;
        let geodata_mode = config_bool(config, controlled, "geodata-mode");
        let geo_auto_update = config_bool(config, controlled, "geo-auto-update");
        let geo_interval = config_value(config, controlled, "geo-update-interval")
            .and_then(serde_json::Value::as_u64)
            .map_or_else(
                || "—".into(),
                |hours| {
                    zenclash_i18n::text_with(
                        "resources.builtin.hours",
                        &[("hours", hours.to_string())],
                    )
                },
            );
        let geox =
            config_value(config, controlled, "geox-url").and_then(serde_json::Value::as_object);
        let geoip = geox
            .and_then(|urls| urls.get("geoip"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("—");
        let geosite = geox
            .and_then(|urls| urls.get("geosite"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("—");
        let external_ui_url = config_value(config, controlled, "external-ui-url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("—");

        setting_card(zenclash_i18n::text("resources.builtin.title"), theme)
            .child(h_flex().flex_wrap().items_start()
                .child(v_flex().flex_1().min_w(gpui_kit::rems(20.))
            .child(setting_switch(
                zenclash_i18n::text("resources.builtin.geodata_mode"),
                zenclash_i18n::text("resources.builtin.geodata_mode_description"),
                self.controlled_bool("/geodata-mode", geodata_mode),
                "resource-geodata-mode",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"geodata-mode": *checked}),
                        zenclash_i18n::text("resources.notices.geodata_mode"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("resources.builtin.auto_update"),
                zenclash_i18n::text("resources.builtin.auto_update_description"),
                self.controlled_bool("/geo-auto-update", geo_auto_update),
                "resource-geo-auto-update",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"geo-auto-update": *checked}),
                        zenclash_i18n::text("resources.notices.geodata_auto"),
                        cx,
                    );
                }),
            ))
            .child(h_flex().px_4().py_3().justify_between()
                .child(div().text_sm().child(zenclash_i18n::text("resources.builtin.interval")))
                .child(Button::new("geodata-update-interval").outline().dropdown_caret(true).label(geo_interval)
                    .disabled(self.core_busy()).dropdown_menu(move |mut menu, _, _| {
                        for hours in [6_u64, 12, 24, 48, 168] {
                            let owner = owner.clone();
                            menu = menu.item(PopupMenuItem::new(zenclash_i18n::text_with("resources.builtin.hours", &[("hours", hours.to_string())]))
                                .on_click(move |_, _, cx| { let _ = owner.update(cx, |page, cx| page.apply_controlled_config(
                                    json!({"geo-update-interval": hours}), zenclash_i18n::text("resources.notices.geodata_auto"), cx)); }));
                        }
                        menu
                    })))
            )
            .child(v_flex().flex_1().min_w(gpui_kit::rems(20.))
            .child(info_row("GeoIP", geoip, theme))
            .child(info_row("GeoSite", geosite, theme))
            .child(info_row("External UI", external_ui_url, theme))))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_4()
                    .child(
                        Button::new("update-geodata")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("resources.builtin.update_geodata"))
                            .small()
                            .primary()
                            .loading(self.core_busy())
                            .disabled(
                                self.core_busy() || !self.core_kind.capabilities().geodata_update,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_builtin_resource(BuiltinResource::GeoData, cx);
                            })),
                    )
                    .child(
                        Button::new("update-external-ui")
                            .icon(crate::assets::AppIcon::RefreshCw)
                            .label(zenclash_i18n::text("resources.builtin.update_ui"))
                            .small()
                            .outline()
                            .disabled(
                                self.core_busy()
                                    || !self.core_kind.capabilities().external_ui_update,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_builtin_resource(BuiltinResource::ExternalUi, cx);
                            })),
                    ),
            )
    }
}

fn config_value<'a>(
    config: &'a RuntimeConfig,
    controlled: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    controlled.get(key).or_else(|| config.extra.get(key))
}

fn config_bool(config: &RuntimeConfig, controlled: &serde_json::Value, key: &str) -> bool {
    config_value(config, controlled, key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

impl RuntimePage {
    fn render_provider_section(
        &self,
        catalog: &ProviderCatalog,
        is_rule: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<RuntimePage>,
    ) -> gpui_kit::AnyElement {
        let mutating = self.core_busy();
        let operations = &self.provider_operations;
        let query = self
            .settings_navigation
            .resource_search
            .read(cx)
            .value()
            .to_lowercase();
        let count = catalog
            .providers
            .iter()
            .filter(|(name, provider)| {
                name.to_lowercase().contains(&query)
                    || provider.name.to_lowercase().contains(&query)
            })
            .count();
        v_flex()
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .when(count == 0, |this| {
                        this.child(empty_state(
                            zenclash_i18n::text("resources.providers.empty"),
                            theme,
                        ))
                    })
                    .children(
                        catalog
                            .providers
                            .iter()
                            .filter(|(name, provider)| {
                                name.to_lowercase().contains(&query)
                                    || provider.name.to_lowercase().contains(&query)
                            })
                            .enumerate()
                            .map(|(index, (key, provider))| {
                                let name = if provider.name.is_empty() {
                                    key.as_str()
                                } else {
                                    provider.name.as_str()
                                };
                                let display_name = name.to_owned();
                                let name_for_click = key.clone();
                                let name_for_healthcheck = key.clone();
                                let status = operations.status(
                                    if is_rule {
                                        ProviderKind::Rule
                                    } else {
                                        ProviderKind::Proxy
                                    },
                                    key,
                                );
                                let item_count = if is_rule {
                                    provider.rule_count
                                } else {
                                    provider.proxies.len()
                                };
                                let metadata = if is_rule {
                                    let behavior = empty_dash(&provider.behavior);
                                    let format = empty_dash(&provider.format).to_ascii_uppercase();
                                    zenclash_i18n::text_with(
                                        "resources.providers.rule_metadata",
                                        &[
                                            ("type", provider.vehicle_type.clone()),
                                            ("behavior", behavior),
                                            ("format", format),
                                            ("updated", provider_updated_at(&provider.updated_at)),
                                            ("count", item_count.to_string()),
                                        ],
                                    )
                                } else {
                                    zenclash_i18n::text_with(
                                        "resources.providers.proxy_metadata",
                                        &[
                                            ("type", provider.vehicle_type.clone()),
                                            ("updated", provider_updated_at(&provider.updated_at)),
                                            ("count", item_count.to_string()),
                                        ],
                                    )
                                };
                                let operation_metadata = status.as_ref().map(|status| {
                                    zenclash_i18n::text_with(
                                        "resources.providers.operation_metadata",
                                        &[
                                            (
                                                "success",
                                                provider_event_age(status.last_success_at_ms),
                                            ),
                                            (
                                                "failure",
                                                provider_failure_age(status.last_failure.as_ref()),
                                            ),
                                            ("update", provider_action_summary(&status.update)),
                                            (
                                                "health",
                                                if is_rule {
                                                    zenclash_i18n::text(
                                                        "resources.providers.not_applicable",
                                                    )
                                                } else {
                                                    provider_action_summary(&status.healthcheck)
                                                },
                                            ),
                                        ],
                                    )
                                });
                                h_flex()
                                    .id((
                                        if is_rule {
                                            "rule-provider"
                                        } else {
                                            "proxy-provider"
                                        },
                                        index,
                                    ))
                                    .min_h(px(58.))
                                    .flex_wrap()
                                    .py_2()
                                    .px_4()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .child(display_name.clone()),
                                    )
                                    .child(div().w_16().text_sm().child(zenclash_i18n::text(
                                        if is_rule {
                                            "settings_redesign.resource_rule"
                                        } else {
                                            "settings_redesign.resource_proxy"
                                        },
                                    )))
                                    .child(div().w_16().text_sm().child(item_count.to_string()))
                                    .child(
                                        div()
                                            .w_32()
                                            .truncate()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(provider_updated_at(&provider.updated_at)),
                                    )
                                    .child(
                                        h_flex()
                                            .w(gpui_kit::rems(15.))
                                            .justify_end()
                                            .gap_2()
                                            .child(
                                                Button::new(gpui_kit::SharedString::from(format!(
                                                    "provider-details-{is_rule}-{key}"
                                                )))
                                                .label(zenclash_i18n::text("redesign.details"))
                                                .small()
                                                .outline()
                                                .on_click(move |_, window, cx| {
                                                    let metadata = metadata.clone();
                                                    let operation_metadata =
                                                        operation_metadata.clone();
                                                    let title = display_name.clone();
                                                    window.open_dialog(
                                                        cx,
                                                        move |dialog, window, cx| {
                                                            dialog
                                                                .title(title.clone())
                                                                .bg(cx.theme().group_box)
                                                                .width(window.rem_size() * 34.)
                                                                .margin_top(
                                                                    ((window
                                                                        .viewport_size()
                                                                        .height
                                                                        - window.rem_size() * 20.)
                                                                        / 2.)
                                                                        .max(window.rem_size()),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_sm()
                                                                        .child(metadata.clone()),
                                                                )
                                                                .when_some(
                                                                    operation_metadata.clone(),
                                                                    |dialog, value| {
                                                                        dialog.child(
                                                                            div()
                                                                                .text_sm()
                                                                                .child(value),
                                                                        )
                                                                    },
                                                                )
                                                        },
                                                    );
                                                }),
                                            )
                                            .when(!is_rule, |row| {
                                                row.child(
                                                    Button::new(gpui_kit::SharedString::from(
                                                        format!("healthcheck-provider-{key}"),
                                                    ))
                                                    .icon(IconName::Heart)
                                                    .label(zenclash_i18n::text(
                                                        "resources.providers.healthcheck",
                                                    ))
                                                    .small()
                                                    .outline()
                                                    .disabled(mutating)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.healthcheck_provider(
                                                            name_for_healthcheck.clone(),
                                                            cx,
                                                        );
                                                    })),
                                                )
                                            })
                                            .child(
                                                Button::new(gpui_kit::SharedString::from(format!(
                                                    "update-provider-{is_rule}-{key}"
                                                )))
                                                .icon(crate::assets::AppIcon::RefreshCw)
                                                .label(zenclash_i18n::text(
                                                    "resources.providers.update",
                                                ))
                                                .small()
                                                .disabled(mutating)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.update_provider(
                                                        name_for_click.clone(),
                                                        is_rule,
                                                        cx,
                                                    );
                                                })),
                                            ),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }
}

fn provider_event_age(timestamp_ms: Option<u64>) -> String {
    timestamp_ms.map_or_else(
        || zenclash_i18n::text("resources.providers.never"),
        |timestamp| format_profile_age(timestamp / 1_000),
    )
}

fn provider_updated_at(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.starts_with("0001-01-01T00:00:00") {
        "—".into()
    } else {
        value.into()
    }
}

fn provider_failure_age(failure: Option<&zenclash_core::ProviderOperationFailure>) -> String {
    failure.map_or_else(
        || zenclash_i18n::text("resources.providers.never"),
        |failure| format_profile_age(failure.occurred_at_ms / 1_000),
    )
}

fn provider_action_summary(status: &zenclash_core::ProviderActionStatus) -> String {
    match (&status.last_success_at_ms, &status.last_failure) {
        (Some(success), Some(failure)) if failure.occurred_at_ms > *success => {
            zenclash_i18n::text_with(
                "resources.providers.failed_age",
                &[("age", format_profile_age(failure.occurred_at_ms / 1_000))],
            )
        }
        (Some(success), _) => zenclash_i18n::text_with(
            "resources.providers.success_age",
            &[("age", format_profile_age(success / 1_000))],
        ),
        (None, Some(failure)) => zenclash_i18n::text_with(
            "resources.providers.failed_age",
            &[("age", format_profile_age(failure.occurred_at_ms / 1_000))],
        ),
        (None, None) => zenclash_i18n::text("resources.providers.never"),
    }
}

#[cfg(test)]
mod tests {
    use super::provider_updated_at;

    #[test]
    fn provider_update_sentinel_is_presented_as_unknown() {
        assert_eq!(provider_updated_at("0001-01-01T00:00:00Z"), "—");
        assert_eq!(provider_updated_at(""), "—");
        assert_eq!(
            provider_updated_at("2026-08-27T12:34:56Z"),
            "2026-08-27T12:34:56Z"
        );
    }
}
