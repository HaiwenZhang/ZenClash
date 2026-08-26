use super::{
    div, h_flex, info_row, json, px, setting_card, setting_switch, v_flex, AutostartStatus, Button,
    Context, Disableable, FluentBuilder, HideTrafficIcon, IconName, IntoElement, Page,
    ParentElement, PreferencesRestored, RuntimeConfig, RuntimeData, RuntimePage, Selectable,
    SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon, Sizable, Styled,
};

mod backup;
mod core_management;
pub(in crate::pages::runtime) mod webdav;
pub(in crate::pages::runtime) use core_management::CoreManagementUiState;

impl RuntimePage {
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
            .child(self.render_core_management(theme, cx))
            .child(self.render_application_settings(&config, &autostart, theme, cx))
            .when(self.core_kind.is_experimental(), |this| {
                this.child(super::message_banner(
                    "meow-rs 实验模式：代理、流量、日志、连接和 Profile 可复用；完整配置通过托管进程重启应用，规则启停、Mihomo 升级、GeoData/UI 更新与 MRS 转换会被禁用。".into(),
                    theme.warning,
                    theme,
                ))
            })
            .child(self.render_backup_card(theme, cx))
            .child(self.render_webdav_card(theme, cx))
            .into_any_element()
    }

    fn render_application_settings(
        &self,
        config: &RuntimeConfig,
        autostart: &AutostartStatus,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        setting_card("应用与控制器", theme)
            .child(info_row("界面技术", "Rust · GPUI · gpui-component", theme))
            .child(info_row(
                "控制器",
                &self.client.endpoint().controller,
                theme,
            ))
            .child(setting_switch(
                "登录时自动启动",
                if autostart.enabled && !autostart.matches_current_executable {
                    "检测到旧程序路径；重新开启可修复启动项"
                } else {
                    "使用当前 ZenClash 程序注册原生系统启动项"
                },
                autostart.enabled,
                "settings-autostart",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.set_autostart(*checked, cx);
                }),
            ))
            .child(info_row(
                "自动启动位置",
                if autostart.location.is_empty() {
                    "等待系统状态"
                } else {
                    &autostart.location
                },
                theme,
            ))
            .child(setting_switch(
                "IPv6",
                format!("同步修改 {} 运行时设置", self.core_kind.display_name()),
                config.ipv6,
                "settings-ipv6",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        json!({"ipv6": *checked}),
                        "IPv6 设置已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(theme_setting(theme))
            .child(tray_setting(theme))
            .child(self.traffic_history_setting(theme, cx))
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
            .map_err(|error| format!("自动启动设置任务异常结束：{error}"))??;
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
                .map_err(|error| format!("自动启动设置任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(if enabled {
                                "登录自动启动已启用并通过系统状态回读".into()
                            } else {
                                "登录自动启动已关闭并通过系统状态回读".into()
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
                "记录流量历史",
                format!(
                    "从真实 {} 连接计数器计算增量，并写入本地 SQLite",
                    self.core_kind.display_name()
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
                            .child(div().text_sm().child("历史保留"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("写入新样本时自动清理过期记录"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .children([7_u16, 30, 90].into_iter().enumerate().map(
                                |(index, days)| {
                                    Button::new(("traffic-retention", index))
                                        .label(format!("{days} 天"))
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
                "流量历史记录已启用"
            } else {
                "流量历史记录已关闭"
            },
            cx,
        );
    }

    fn set_traffic_retention(&mut self, days: u16, cx: &mut Context<Self>) {
        self.persist_traffic_preferences(None, Some(days), "流量历史保留策略已保存", cx);
    }

    fn persist_traffic_preferences(
        &mut self,
        history_enabled: Option<bool>,
        retention_days: Option<u16>,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.preferences_store.clone() else {
            self.error = Some("应用设置存储不可用；请检查应用数据目录权限".into());
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
            .map_err(|error| format!("应用设置保存任务异常结束：{error}"))?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("应用设置保存任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(preferences) if this.is_page_task_current(token) => {
                        this.preferences = preferences.clone();
                        this.notice = Some(success.into());
                        cx.emit(PreferencesRestored {
                            preferences: preferences.clone(),
                        });
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

fn theme_setting(theme: &gpui_component::Theme) -> gpui::Div {
    h_flex()
        .min_h(px(58.))
        .px_4()
        .gap_3()
        .justify_between()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex().gap_1().child(div().text_sm().child("主题")).child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("固定明暗外观，或实时跟随操作系统"),
            ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("theme-system")
                        .icon(IconName::Globe)
                        .label("跟随系统")
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SetSystemTheme), cx);
                        }),
                )
                .child(
                    Button::new("theme-light")
                        .icon(IconName::Sun)
                        .label("浅色")
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SetLightTheme), cx);
                        }),
                )
                .child(
                    Button::new("theme-dark")
                        .icon(IconName::Moon)
                        .label("深色")
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
                .child(div().text_sm().child("状态栏流量图标"))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("显示实时上传、下载速率和动态箭头图标"),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("tray-show")
                        .label("显示")
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ShowTrafficIcon), cx);
                        }),
                )
                .child(
                    Button::new("tray-hide")
                        .label("隐藏")
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(HideTrafficIcon), cx);
                        }),
                ),
        )
}
