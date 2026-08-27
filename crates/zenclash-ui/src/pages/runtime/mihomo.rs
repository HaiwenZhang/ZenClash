use super::{
    config_input_row, format_port, h_flex, info_row, json, load_page, message_banner, metric,
    setting_card, setting_switch, v_flex, Button, ButtonVariants, Context, Disableable,
    FluentBuilder, IconName, Input, IntoElement, Page, ParentElement, RuntimeConfig, RuntimeData,
    RuntimePage, Sizable, Styled, VersionInfo,
};
use zenclash_core::CoreMaintenanceIntent;

mod maintenance;

pub(super) use maintenance::CoreReleaseState;

impl RuntimePage {
    fn restart_managed_core(&mut self, cx: &mut Context<Self>) {
        if !self.core_session.snapshot().managed {
            self.error = Some(zenclash_i18n::text("core_page.errors.external_restart"));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Mihomo) else {
            return;
        };
        let client = self.client.clone();
        let core_session = self.core_session.clone();
        let task = self.runtime.spawn(async move {
            core_session
                .maintain(CoreMaintenanceIntent::Restart)
                .await
                .map_err(|error| error.to_string())?;
            load_page(client, Page::Mihomo).await
        });
        Self::finish_core_maintenance(
            task,
            token,
            zenclash_i18n::text_with(
                "core_page.notices.restarted",
                &[("core", self.core_kind.display_name().to_owned())],
            ),
            cx,
        );
    }

    fn finish_core_maintenance(
        task: tokio::task::JoinHandle<Result<RuntimeData, String>>,
        token: super::PageTaskToken,
        success: String,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "core_page.errors.maintenance_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(data) => {
                        if this.replace_page_data(token, data) {
                            this.notice = Some(success);
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

    #[allow(
        clippy::too_many_lines,
        reason = "the core settings card is a single declarative GPUI element tree"
    )]
    pub(super) fn render_core(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (version, config, has_runtime_data) = match &self.data {
            RuntimeData::Core { version, config } => (version.clone(), config.clone(), true),
            _ => (VersionInfo::default(), RuntimeConfig::default(), false),
        };
        let process = self.process.as_ref().map(|process| process.snapshot());
        let managed_process = process.is_some();
        let process_running = process
            .as_ref()
            .map_or(has_runtime_data, |snapshot| snapshot.running);
        let process_status = process.as_ref().map_or_else(
            || {
                if has_runtime_data {
                    zenclash_i18n::text("core_page.status.external_connected")
                } else {
                    zenclash_i18n::text("core_page.status.external_unreachable")
                }
            },
            |snapshot| {
                if snapshot.running {
                    zenclash_i18n::text_with(
                        "core_page.status.running",
                        &[("pid", snapshot.pid.unwrap_or_default().to_string())],
                    )
                } else {
                    zenclash_i18n::text("core_page.status.stopped")
                }
            },
        );

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(metric(
                        zenclash_i18n::text("core_page.metrics.version"),
                        if has_runtime_data && !version.version.is_empty() {
                            version.version.clone()
                        } else {
                            zenclash_i18n::text("core_page.status.unreadable")
                        },
                        theme.primary,
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("core_page.metrics.status"),
                        process_status,
                        if process_running {
                            theme.success
                        } else {
                            theme.danger
                        },
                        theme,
                    ))
                    .child(metric(
                        zenclash_i18n::text("core_page.metrics.mode"),
                        if has_runtime_data && !config.mode.is_empty() {
                            config.mode.clone()
                        } else {
                            zenclash_i18n::text("core_page.status.unreadable")
                        },
                        theme.warning,
                        theme,
                    )),
            )
            .when(!has_runtime_data, |this| {
                this.child(message_banner(
                    zenclash_i18n::text("core_page.status.no_placeholder"),
                    theme.danger,
                    theme,
                ))
            })
            .when(has_runtime_data, |this| {
                this.child(
                    setting_card(zenclash_i18n::text("core_page.switches.title"), theme)
                        .child(setting_switch(
                            "IPv6",
                            zenclash_i18n::text_with(
                                "core_page.switches.ipv6_description",
                                &[("core", self.core_kind.display_name().to_owned())],
                            ),
                            config.ipv6,
                            "runtime-ipv6",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"ipv6": *checked}),
                                    zenclash_i18n::text("core_page.notices.ipv6"),
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            zenclash_i18n::text("core_page.switches.allow_lan"),
                            zenclash_i18n::text("core_page.switches.allow_lan_description"),
                            config.allow_lan,
                            "runtime-allow-lan",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"allow-lan": *checked}),
                                    zenclash_i18n::text("core_page.notices.allow_lan"),
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            zenclash_i18n::text("core_page.switches.tcp_concurrent"),
                            zenclash_i18n::text("core_page.switches.tcp_concurrent_description"),
                            config.tcp_concurrent,
                            "runtime-tcp-concurrent",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"tcp-concurrent": *checked}),
                                    zenclash_i18n::text("core_page.notices.tcp_concurrent"),
                                    cx,
                                );
                            }),
                        ))
                        .child(setting_switch(
                            zenclash_i18n::text("core_page.switches.unified_delay"),
                            zenclash_i18n::text("core_page.switches.unified_delay_description"),
                            config.unified_delay,
                            "runtime-unified-delay",
                            theme,
                            cx.listener(|this, checked, _, cx| {
                                this.apply_controlled_config(
                                    json!({"unified-delay": *checked}),
                                    zenclash_i18n::text("core_page.notices.unified_delay"),
                                    cx,
                                );
                            }),
                        )),
                )
            })
            .when(has_runtime_data, |this| {
                this.child(
                    setting_card(zenclash_i18n::text("core_page.controller.title"), theme)
                        .child(info_row(
                            "External Controller",
                            &self.client.endpoint().controller,
                            theme,
                        ))
                        .child(info_row("HTTP", format_port(config.port), theme))
                        .child(info_row("SOCKS", format_port(config.socks_port), theme))
                        .child(info_row("Mixed", format_port(config.mixed_port), theme))
                        .child(info_row(
                            zenclash_i18n::text("core_page.controller.log_level"),
                            &config.log_level,
                            theme,
                        )),
                )
            })
            .when(has_runtime_data, |this| {
                this.child(self.render_core_inputs(theme, cx))
            })
            .child(
                setting_card(zenclash_i18n::text("core_page.maintenance.title"), theme)
                    .child(info_row(
                        zenclash_i18n::text("core_page.maintenance.channel"),
                        if self.core_kind.capabilities().core_upgrade {
                            zenclash_i18n::text("core_page.maintenance.official")
                        } else {
                            zenclash_i18n::text("core_page.maintenance.unsupported")
                        },
                        theme,
                    ))
                    .child(info_row(
                        zenclash_i18n::text("core_page.maintenance.restart_capability"),
                        if managed_process {
                            zenclash_i18n::text("core_page.maintenance.managed")
                        } else {
                            zenclash_i18n::text("core_page.maintenance.external")
                        },
                        theme,
                    ))
                    .child(
                        h_flex().justify_end().gap_2().p_4().child(
                            Button::new("restart-mihomo-core")
                                .icon(IconName::Redo2)
                                .label(zenclash_i18n::text("core_page.maintenance.restart"))
                                .small()
                                .outline()
                                .loading(self.mutating)
                                .disabled(self.mutating || !managed_process)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.restart_managed_core(cx);
                                })),
                        ),
                    ),
            )
            .child(self.render_versioned_core_updates(&version.version, managed_process, theme, cx))
            .when_some(process, |this, snapshot| {
                this.child(
                    setting_card(zenclash_i18n::text("core_page.process.title"), theme)
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.binary"),
                            snapshot.binary.display().to_string(),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.config"),
                            snapshot.config_file.display().to_string(),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.directory"),
                            snapshot.home_dir.display().to_string(),
                            theme,
                        )),
                )
            })
            .into_any_element()
    }

    fn render_core_inputs(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.core;
        setting_card(zenclash_i18n::text("core_page.listeners.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.http"),
                zenclash_i18n::text("core_page.listeners.http_description"),
                Input::new(&inputs.port),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.socks"),
                zenclash_i18n::text("core_page.listeners.socks_description"),
                Input::new(&inputs.socks_port),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.mixed"),
                zenclash_i18n::text("core_page.listeners.mixed_description"),
                Input::new(&inputs.mixed_port),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.redir"),
                zenclash_i18n::text("core_page.listeners.redir_description"),
                Input::new(&inputs.redir_port),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.tproxy"),
                zenclash_i18n::text("core_page.listeners.tproxy_description"),
                Input::new(&inputs.tproxy_port),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.bind"),
                zenclash_i18n::text("core_page.listeners.bind_description"),
                Input::new(&inputs.bind_address),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.listeners.interface"),
                zenclash_i18n::text("core_page.listeners.interface_description"),
                Input::new(&inputs.interface_name),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("core_page.controller.log_level"),
                "silent / error / warning / info / debug",
                Input::new(&inputs.log_level),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-core-listeners")
                        .icon(IconName::Check)
                        .label(zenclash_i18n::text_with(
                            "core_page.listeners.save",
                            &[("core", self.core_kind.display_name().to_owned())],
                        ))
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.core.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    zenclash_i18n::text("core_page.notices.listeners"),
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
