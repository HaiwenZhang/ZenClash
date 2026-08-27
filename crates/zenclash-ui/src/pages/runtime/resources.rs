use super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, Icon, IconName,
    InteractiveElement, IntoElement, Page, ParentElement, ProviderCatalog, ProviderKind,
    RuntimeConfig, RuntimeData, RuntimePage, Sizable, Styled, div, empty_dash, empty_state,
    format_profile_age, h_flex, info_row, json, load_page, message_banner, px, setting_card,
    setting_switch, v_flex,
};

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
        let task = self.runtime.spawn(async move {
            match resource {
                BuiltinResource::GeoData => client.update_geodata().await,
                BuiltinResource::ExternalUi => client.update_external_ui().await,
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
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
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
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
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
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
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
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (config, proxy, rules) = match &self.data {
            RuntimeData::Resources {
                config,
                proxy,
                rules,
            } => (config.clone(), proxy.clone(), rules.clone()),
            _ => (
                RuntimeConfig::default(),
                ProviderCatalog::default(),
                ProviderCatalog::default(),
            ),
        };
        v_flex()
            .gap_4()
            .child(self.render_builtin_resources(&config, theme, cx))
            .child(self.render_ruleset_converter(theme, cx))
            .child(provider_section(
                zenclash_i18n::text("resources.providers.proxy"),
                proxy,
                false,
                self.mutating,
                &self.provider_operations,
                theme,
                cx,
            ))
            .child(provider_section(
                zenclash_i18n::text("resources.providers.rule"),
                rules,
                true,
                self.mutating,
                &self.provider_operations,
                theme,
                cx,
            ))
            .into_any_element()
    }

    fn render_builtin_resources(
        &self,
        config: &RuntimeConfig,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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
            .when(
                !self.core_kind.capabilities().geodata_update
                    || !self.core_kind.capabilities().external_ui_update,
                |card| {
                    card.child(message_banner(
                        zenclash_i18n::text_with(
                            "resources.builtin.unsupported",
                            &[("core", self.core_kind.display_name().to_owned())],
                        ),
                        theme.warning,
                        theme,
                    ))
                },
            )
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
            .child(info_row(
                zenclash_i18n::text("resources.builtin.interval"),
                &geo_interval,
                theme,
            ))
            .child(info_row("GeoIP", geoip, theme))
            .child(info_row("GeoSite", geosite, theme))
            .child(info_row("External UI", external_ui_url, theme))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .p_4()
                    .child(
                        Button::new("update-geodata")
                            .icon(IconName::Redo2)
                            .label(zenclash_i18n::text("resources.builtin.update_geodata"))
                            .small()
                            .primary()
                            .loading(self.mutating)
                            .disabled(
                                self.mutating || !self.core_kind.capabilities().geodata_update,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.update_builtin_resource(BuiltinResource::GeoData, cx);
                            })),
                    )
                    .child(
                        Button::new("update-external-ui")
                            .icon(IconName::Redo2)
                            .label(zenclash_i18n::text("resources.builtin.update_ui"))
                            .small()
                            .outline()
                            .disabled(
                                self.mutating || !self.core_kind.capabilities().external_ui_update,
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

fn provider_section(
    title: String,
    catalog: ProviderCatalog,
    is_rule: bool,
    mutating: bool,
    operations: &super::ProviderOperations,
    theme: &gpui_component::Theme,
    cx: &mut Context<RuntimePage>,
) -> gpui::AnyElement {
    let count = catalog.providers.len();
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    zenclash_i18n::text_with(
                        "resources.providers.count",
                        &[("count", count.to_string())],
                    ),
                )),
        )
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
                .children(catalog.providers.into_iter().enumerate().map(
                    |(index, (key, provider))| {
                        let name = if provider.name.is_empty() {
                            key
                        } else {
                            provider.name
                        };
                        let name_for_click = name.clone();
                        let name_for_healthcheck = name.clone();
                        let status = operations.status(
                            if is_rule {
                                ProviderKind::Rule
                            } else {
                                ProviderKind::Proxy
                            },
                            &name,
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
                                    ("success", provider_event_age(status.last_success_at_ms)),
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
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Icon::new(IconName::Inbox).size_4())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .child(div().text_sm().child(name))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(metadata),
                                    )
                                    .when_some(operation_metadata, |this, metadata| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(metadata),
                                        )
                                    }),
                            )
                            .when(!is_rule, |row| {
                                row.child(
                                    Button::new(("healthcheck-provider", index))
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
                                Button::new(("update-provider", index))
                                    .icon(IconName::Redo2)
                                    .label(zenclash_i18n::text("resources.providers.update"))
                                    .small()
                                    .disabled(mutating)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update_provider(name_for_click.clone(), is_rule, cx);
                                    })),
                            )
                    },
                )),
        )
        .into_any_element()
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
