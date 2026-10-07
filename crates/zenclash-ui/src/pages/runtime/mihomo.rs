use super::settings::forms::{
    settings_input_row as config_input_row, settings_switch as setting_switch,
};
use super::{
    Button, ButtonVariants, Context, Disableable, FluentBuilder, IconName, Input, IntoElement,
    Page, ParentElement, RuntimeConfig, RuntimeData, RuntimePage, Sizable, Styled, VersionInfo,
    h_flex, info_row, json, load_page, setting_card, v_flex,
};
use gpui_kit::component::{ActiveTheme, WindowExt};
use zenclash_core::{CoreMaintenanceIntent, CoreRuntimeBackend, Observation};

mod maintenance;

pub(super) use maintenance::CoreReleaseState;

impl RuntimePage {
    fn confirm_core_action(
        &mut self,
        restart: bool,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || self.core_busy() {
            return;
        }
        let owner = cx.entity().downgrade();
        let profile = self.profile_path.clone();
        let generation = self.core_session.generation();
        let confirm = std::rc::Rc::new(move |cx: &mut gpui_kit::App| {
            owner
                .update(cx, |this, cx| {
                    if this.page != Page::Mihomo
                        || this.profile_path != profile
                        || this.core_session.generation() != generation
                        || this.core_busy()
                    {
                        return false;
                    }
                    if restart {
                        this.restart_managed_core(cx);
                    } else {
                        this.stop_managed_core(cx);
                    }
                    true
                })
                .unwrap_or(true)
        });
        window.open_dialog(cx, move |dialog, window, cx| {
            let confirm_button = confirm.clone();
            let confirm_key = confirm.clone();
            dialog
                .bg(cx.theme().group_box)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 16.) / 2.)
                        .max(window.rem_size()),
                )
                .title(zenclash_i18n::text(if restart {
                    "core_page.maintenance.restart"
                } else {
                    "automatic.stop"
                }))
                .child(zenclash_i18n::text("settings_redesign.core_interrupt"))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .label(zenclash_i18n::text("common.actions.cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("ok")
                                .primary()
                                .label(zenclash_i18n::text("settings_redesign.confirm"))
                                .on_click(move |_, window, cx| {
                                    if confirm_button(cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
                .on_ok(move |_, _, cx| confirm_key(cx))
        });
    }

    pub(super) fn render_core_extra_network(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        v_flex()
            .child(config_input_row(
                "Redir",
                "",
                Input::new(&self.config_inputs.core.redir_port),
                &cx.theme().clone(),
            ))
            .child(config_input_row(
                "TPROXY",
                "",
                Input::new(&self.config_inputs.core.tproxy_port),
                &cx.theme().clone(),
            ))
            .child(config_input_row(
                zenclash_i18n::text("logs.collection_level"),
                "",
                self.settings_choice(
                    "core-extra-log-level",
                    &self.config_inputs.core.log_level,
                    &["silent", "error", "warning", "info", "debug"],
                    cx,
                ),
                &cx.theme().clone(),
            ))
            .child(
                Button::new("save-core-extra-network")
                    .outline()
                    .small()
                    .label(zenclash_i18n::text("common.actions.save"))
                    .disabled(self.core_busy())
                    .on_click(cx.listener(|this, _, _, cx| {
                        match this.config_inputs.core.advanced_patch(cx) {
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
            )
    }

    fn open_core_updates(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| {
                    let version = match &page.data {
                        RuntimeData::Core { version, .. } => version.version.clone(),
                        _ => String::new(),
                    };
                    let local = page.core_session.runtime_descriptor().backend()
                        == CoreRuntimeBackend::Local;
                    page.render_versioned_core_updates(&version, local, &cx.theme().clone(), cx)
                        .into_any_element()
                })
                .ok();
            dialog
                .title(zenclash_i18n::text("settings_redesign.core_updates"))
                .bg(cx.theme().group_box)
                .width(window.rem_size() * 42.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 28.) / 2.)
                        .max(window.rem_size()),
                )
                .when_some(content, |dialog, content| dialog.child(content))
        });
    }

    fn stop_managed_core(&mut self, cx: &mut Context<Self>) {
        if !self.core_session.is_managed() {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Mihomo) else {
            return;
        };
        let core = self.core_session.clone();
        let capture = self.traffic_capture.clone();
        let initial_version = core.generation();
        let task = self.runtime.spawn(async move {
            // Stop intent must suppress both crash recovery and network resume.
            let version = core
                .maintain(CoreMaintenanceIntent::Stop)
                .await
                .map_err(|error| (initial_version, error.to_string()))?;
            if core.generation() != version {
                return Ok((version, RuntimeData::Empty));
            }
            match capture
                .release_owned()
                .await
                .map_err(|error| (version, error.to_string()))?
            {
                zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } => {
                    Err((version, failure))
                }
                _ => Ok((version, RuntimeData::Empty)),
            }
        });
        Self::finish_core_maintenance(
            task,
            token,
            initial_version,
            zenclash_i18n::text("automatic.user_stopped"),
            cx,
        );
    }

    fn restart_managed_core(&mut self, cx: &mut Context<Self>) {
        if !self.core_session.is_managed() {
            self.error = Some(zenclash_i18n::text("core_page.errors.external_restart"));
            cx.notify();
            return;
        }
        let Some(token) = self.begin_mutation(Page::Mihomo) else {
            return;
        };
        let client = self.client.clone();
        let core_session = self.core_session.clone();
        let capture = self.traffic_capture.clone();
        let initial_version = core_session.generation();
        let task = self.runtime.spawn(async move {
            let version = core_session
                .maintain(CoreMaintenanceIntent::Restart)
                .await
                .map_err(|error| (initial_version, error.to_string()))?;
            if core_session.generation() != version {
                return Ok((version, RuntimeData::Empty));
            }
            if let zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } = capture
                .reconcile()
                .await
                .map_err(|error| (version, error.to_string()))?
            {
                return Err((version, failure));
            }
            let data = load_page(client, Page::Mihomo)
                .await
                .map_err(|error| (version, error))?;
            Ok((version, data))
        });
        Self::finish_core_maintenance(
            task,
            token,
            initial_version,
            zenclash_i18n::text_with(
                "core_page.notices.restarted",
                &[("core", self.core_kind.display_name().to_owned())],
            ),
            cx,
        );
    }

    fn finish_core_maintenance(
        task: tokio::task::JoinHandle<Result<(u64, RuntimeData), (u64, String)>>,
        token: super::PageTaskToken,
        initial_version: u64,
        success: String,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    (
                        initial_version,
                        zenclash_i18n::text_with(
                            "core_page.errors.maintenance_task",
                            &[("error", error.to_string())],
                        ),
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((version, data)) => {
                        if !this.profile_service.is_current(version) {
                            this.invalidate_page_load();
                            this.refresh(cx);
                            return;
                        }
                        if this.replace_page_data(token, data, cx) {
                            this.notice = Some(success);
                        }
                    }
                    Err((version, error)) => {
                        if this.profile_service.is_current(version) {
                            this.set_page_error(token, error);
                        } else {
                            this.invalidate_page_load();
                            this.refresh(cx);
                        }
                    }
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
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let (version, config, has_runtime_data) = match &self.data {
            RuntimeData::Core { version, config }
                if self.data_runtime_version == self.core_session.generation() =>
            {
                (version.clone(), config.clone(), true)
            }
            _ => (VersionInfo::default(), RuntimeConfig::default(), false),
        };
        let descriptor = self.core_session.runtime_descriptor();
        let managed_process = descriptor.backend() != CoreRuntimeBackend::Direct;
        let local_source = descriptor.backend() == CoreRuntimeBackend::Local;
        let operational = self.operational_status.snapshot();
        let (process_status, _) = core_status_copy(
            descriptor.backend(),
            &operational.process,
            self.core_session.generation(),
            has_runtime_data,
        );

        let left = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(self.render_core_management(theme, cx))
            .when(local_source, |this| {
                this.child(
                    setting_card(zenclash_i18n::text("core_page.process.title"), theme)
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.binary"),
                            descriptor
                                .binary()
                                .map_or_else(|| "—".to_owned(), |path| path.display().to_string()),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.config"),
                            descriptor
                                .config_file()
                                .map_or_else(|| "—".to_owned(), |path| path.display().to_string()),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("core_page.process.directory"),
                            descriptor
                                .home_dir()
                                .map_or_else(|| "—".to_owned(), |path| path.display().to_string()),
                            theme,
                        )),
                )
            });
        let right = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .when(has_runtime_data, |this| {
                this.child(
                    setting_card(
                        zenclash_i18n::text("settings_redesign.network_behavior"),
                        theme,
                    )
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
                    ))
                    .child(config_input_row(
                        zenclash_i18n::text("core_page.listeners.interface"),
                        "",
                        Input::new(&self.config_inputs.core.interface_name),
                        theme,
                    ))
                    .child(
                        h_flex().justify_end().p_4().child(
                            Button::new("save-core-interface")
                                .outline()
                                .label(zenclash_i18n::text("common.actions.save"))
                                .disabled(self.core_busy())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let interface = this
                                        .config_inputs
                                        .core
                                        .interface_name
                                        .read(cx)
                                        .value()
                                        .trim()
                                        .to_owned();
                                    this.apply_controlled_config(
                                        json!({"interface-name": interface}),
                                        zenclash_i18n::text("core_page.notices.listeners"),
                                        cx,
                                    );
                                })),
                        ),
                    ),
                )
            })
            .children(self.render_service_status(theme, cx))
            .child(
                Button::new("open-core-updates")
                    .outline()
                    .label(zenclash_i18n::text("settings_redesign.core_updates"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_core_updates(window, cx)),
                    ),
            );
        v_flex()
            .gap_4()
            .child(
                setting_card(zenclash_i18n::text("core_page.metrics.status"), theme).child(
                    h_flex()
                        .px_4()
                        .pb_4()
                        .gap_4()
                        .flex_wrap()
                        .child(
                            gpui_kit::div()
                                .text_lg()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(self.core_kind.display_name()),
                        )
                        .child(gpui_kit::div().text_sm().child(process_status))
                        .child(
                            gpui_kit::div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(version.version.clone()),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .ml_auto()
                                .child(
                                    Button::new("stop-mihomo-core")
                                        .label(zenclash_i18n::text("automatic.stop"))
                                        .small()
                                        .outline()
                                        .disabled(self.core_busy() || !managed_process)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_core_action(false, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("restart-mihomo-core")
                                        .icon(crate::assets::AppIcon::RefreshCw)
                                        .label(zenclash_i18n::text("core_page.maintenance.restart"))
                                        .small()
                                        .outline()
                                        .loading(self.core_busy())
                                        .disabled(self.core_busy() || !managed_process)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_core_action(true, window, cx);
                                        })),
                                ),
                        ),
                ),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .when(compact, |view| view.flex_col())
                    .child(left.when(compact, |view| view.w_full()))
                    .child(right.when(compact, |view| view.w_full())),
            )
            .into_any_element()
    }

    pub(super) fn render_core_inputs(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let inputs = &self.config_inputs.core;
        setting_card(
            zenclash_i18n::text("settings_redesign.listener_ports"),
            theme,
        )
        .child(config_input_row(
            zenclash_i18n::text("core_page.listeners.mixed"),
            zenclash_i18n::text("core_page.listeners.mixed_description"),
            Input::new(&inputs.mixed_port).cleanable(true),
            theme,
        ))
        .child(config_input_row(
            zenclash_i18n::text("core_page.listeners.http"),
            zenclash_i18n::text("core_page.listeners.http_description"),
            Input::new(&inputs.port).cleanable(true),
            theme,
        ))
        .child(config_input_row(
            zenclash_i18n::text("core_page.listeners.socks"),
            zenclash_i18n::text("core_page.listeners.socks_description"),
            Input::new(&inputs.socks_port).cleanable(true),
            theme,
        ))
        .child(config_input_row(
            zenclash_i18n::text("core_page.listeners.bind"),
            zenclash_i18n::text("core_page.listeners.bind_description"),
            Input::new(&inputs.bind_address).cleanable(true),
            theme,
        ))
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .p_4()
                .child(self.settings_cancel(cx))
                .child(
                    Button::new("save-core-listeners")
                        .icon(IconName::Check)
                        .label(zenclash_i18n::text("common.actions.save"))
                        .primary()
                        .loading(self.core_busy())
                        .disabled(self.core_busy())
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.core.listener_patch(cx) {
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

// The view must not infer native liveness from controller data or a previous binding.
fn core_status_copy(
    backend: CoreRuntimeBackend,
    observation: &Observation<zenclash_core::ProcessStatus>,
    generation: u64,
    controller_available: bool,
) -> (String, Option<bool>) {
    let observed = match observation {
        Observation::Fresh { value, .. } if value.generation == generation => Some(value),
        _ => None,
    };
    let direct = backend == CoreRuntimeBackend::Direct;
    let copy = if direct {
        zenclash_i18n::text(if controller_available {
            "core_page.status.external_connected"
        } else {
            "core_page.status.external_unreachable"
        })
    } else if let Some(process) = observed {
        if process.running {
            process.pid.filter(|pid| *pid != 0).map_or_else(
                || zenclash_i18n::text("core_page.status.running_without_pid"),
                |pid| {
                    zenclash_i18n::text_with(
                        "core_page.status.running",
                        &[("pid", pid.to_string())],
                    )
                },
            )
        } else {
            zenclash_i18n::text("core_page.status.stopped")
        }
    } else {
        zenclash_i18n::text("core_page.status.unreadable")
    };
    (
        copy,
        if direct {
            Some(controller_available)
        } else {
            observed.map(|status| status.running)
        },
    )
}

#[cfg(test)]
mod owner_status_tests {
    use super::*;
    use zenclash_core::{CoreKind, ProcessRecoveryStatus, ProcessStatus};

    #[test]
    fn service_unknown_is_neither_external_nor_stopped_even_with_controller_data() {
        let (copy, running) =
            core_status_copy(CoreRuntimeBackend::Service, &Observation::Loading, 3, true);
        assert_eq!(copy, zenclash_i18n::text("core_page.status.unreadable"));
        assert_eq!(running, None);
    }

    #[test]
    fn an_old_owner_cannot_supply_liveness_and_an_unavailable_pid_is_not_zero() {
        let status = ProcessStatus {
            kind: CoreKind::Mihomo,
            managed: true,
            pid: None,
            started_at_secs: None,
            running: true,
            generation: 2,
            exit_reason: None,
            recovery_attempts: 0,
            recovery: ProcessRecoveryStatus::Stable,
        };
        let observation = Observation::Fresh {
            value: status,
            observed_at_ms: 0,
        };
        assert_eq!(
            core_status_copy(CoreRuntimeBackend::Local, &observation, 3, true).1,
            None
        );
        let (copy, running) = core_status_copy(CoreRuntimeBackend::Service, &observation, 2, true);
        assert_eq!(running, Some(true));
        assert_eq!(
            copy,
            zenclash_i18n::text("core_page.status.running_without_pid")
        );
        assert!(!copy.contains("PID 0"));
    }
}
