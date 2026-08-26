use super::{
    config_input_row, empty_dash, h_flex, info_row, json, message_banner, setting_card,
    setting_switch, v_flex, Button, ButtonVariants, Context, Disableable, IconName, Input,
    IntoElement, Page, ParentElement, RuntimeData, RuntimePage, Styled, TunPermissionGrant,
    TunPermissionManager,
};

impl RuntimePage {
    pub(super) fn render_tun(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_tun_permissions(theme, cx))
            .child(self.render_tun_switches(theme, cx))
            .child(self.render_tun_routes(theme, cx))
            .into_any_element()
    }

    fn render_tun_permissions(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let permissions = match &self.data {
            RuntimeData::Tun { permissions, .. } => Some(permissions),
            _ => None,
        };
        let (granted, can_request) = permissions
            .and_then(|permissions| permissions.as_ref().ok())
            .map_or((false, self.mihomo_binary().is_some()), |status| {
                (status.granted, status.can_request)
            });
        let mut card = setting_card("TUN 系统权限", theme);
        match permissions {
            Some(Ok(status)) => {
                card = card
                    .child(info_row(
                        "状态",
                        if status.granted {
                            "已就绪"
                        } else if status.requires_relaunch {
                            "需要管理员重启"
                        } else {
                            "需要安装权限"
                        },
                        theme,
                    ))
                    .child(info_row("校验", &status.detail, theme))
                    .child(info_row(
                        "内核",
                        &status.binary.display().to_string(),
                        theme,
                    ));
            }
            Some(Err(error)) => {
                card = card.child(message_banner(error.clone(), theme.warning, theme));
            }
            None => {
                card = card.child(message_banner(
                    "正在读取 TUN 权限状态…".into(),
                    theme.primary,
                    theme,
                ));
            }
        }
        card.child(
            h_flex().justify_end().p_4().child(
                Button::new("grant-tun-permissions")
                    .icon(if granted {
                        IconName::CircleCheck
                    } else {
                        IconName::TriangleAlert
                    })
                    .label(if granted {
                        "权限已就绪"
                    } else {
                        "安装 / 修复 TUN 权限"
                    })
                    .primary()
                    .loading(self.mutating)
                    .disabled(self.mutating || granted || !can_request)
                    .on_click(cx.listener(|this, _, _, cx| this.grant_tun_permissions(cx))),
            ),
        )
    }

    fn grant_tun_permissions(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.mihomo_binary() else {
            self.error = Some("当前连接的是外部内核，无法确定需要授权的可执行文件".into());
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Tun) else {
            return;
        };
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                TunPermissionManager::new(binary)
                    .and_then(|manager| manager.request_grant())
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("TUN 权限任务异常结束：{error}"))?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("TUN 权限任务异常结束：{error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(TunPermissionGrant::Ready(_)) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some("TUN 权限已安装并通过系统状态回读".into());
                            this.refresh(cx);
                        }
                    }
                    Ok(TunPermissionGrant::RelaunchRequested) => cx.quit(),
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_tun_switches(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let tun = self.config().cloned().unwrap_or_default().tun;
        setting_card("虚拟网卡", theme)
            .child(setting_switch(
                "启用 TUN",
                format!(
                    "通过 {} TUN 接管系统网络流量",
                    self.core_kind.display_name()
                ),
                self.controlled_bool("/tun/enable", tun.enable),
                "tun-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool("enable", *checked, "TUN 状态已保存并热重载", cx);
                }),
            ))
            .child(info_row("网络栈", &tun.stack, theme))
            .child(info_row("设备", &empty_dash(&tun.device), theme))
            .child(info_row("DNS 劫持", &tun.dns_hijack.join(", "), theme))
            .child(setting_switch(
                "自动路由",
                format!("让 {} 自动安装 TUN 路由", self.core_kind.display_name()),
                self.controlled_bool("/tun/auto-route", tun.auto_route),
                "tun-auto-route",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool("auto-route", *checked, "TUN 自动路由已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "自动检测接口",
                "自动选择默认出站网络接口",
                self.controlled_bool("/tun/auto-detect-interface", tun.auto_detect_interface),
                "tun-auto-detect-interface",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "auto-detect-interface",
                        *checked,
                        "TUN 接口检测已保存并热重载",
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                "严格路由",
                "使用严格路由避免流量绕过 TUN",
                self.controlled_bool("/tun/strict-route", tun.strict_route),
                "tun-strict-route",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool("strict-route", *checked, "TUN 严格路由已保存并热重载", cx);
                }),
            ))
            .child(setting_switch(
                "自动重定向",
                "Linux 下使用 nftables 提升 TUN 路由性能",
                self.controlled_bool("/tun/auto-redirect", false),
                "tun-auto-redirect",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "auto-redirect",
                        *checked,
                        "TUN 自动重定向已保存并热重载",
                        cx,
                    );
                }),
            ))
    }

    fn patch_tun_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.apply_controlled_config(json!({"tun": {key: value}}), success, cx);
    }

    fn render_tun_routes(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.tun;
        setting_card("接口与路由", theme)
            .child(config_input_row(
                "网络栈",
                "gvisor / mixed / system",
                Input::new(&inputs.stack),
                theme,
            ))
            .child(config_input_row(
                "设备名称",
                "系统中的 TUN 设备名称",
                Input::new(&inputs.device),
                theme,
            ))
            .child(config_input_row(
                "MTU",
                "范围 1 到 65535",
                Input::new(&inputs.mtu),
                theme,
            ))
            .child(config_input_row(
                "DNS 劫持",
                "逗号分隔，例如 any:53",
                Input::new(&inputs.dns_hijack),
                theme,
            ))
            .child(config_input_row(
                "包含路由",
                "每行一个需要接管的 CIDR",
                Input::new(&inputs.route_include_address),
                theme,
            ))
            .child(config_input_row(
                "排除路由",
                "每行一个不接管的 CIDR",
                Input::new(&inputs.route_exclude_address),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-tun-advanced")
                        .icon(IconName::Check)
                        .label("保存 TUN 高级配置")
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.tun.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    "TUN 高级配置已保存并热重载",
                                    cx,
                                ),
                                Err(error) => {
                                    this.error = Some(error);
                                    cx.notify();
                                }
                            }
                        })),
                ),
            )
    }
}
