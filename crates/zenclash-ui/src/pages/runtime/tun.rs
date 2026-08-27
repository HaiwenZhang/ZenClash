use super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, Page,
    ParentElement, RuntimeData, RuntimePage, Styled, config_input_row, empty_dash, h_flex,
    info_row, json, message_banner, setting_card, setting_switch, v_flex,
};
use zenclash_core::{CapabilityState, CaptureOutcome, CapturePlan};

impl RuntimePage {
    pub(super) fn render_tun(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        v_flex()
            .gap_4()
            .child(self.render_tun_permissions(theme, cx))
            .child(self.render_tun_runtime(theme))
            .child(self.render_tun_switches(theme, cx))
            .child(self.render_tun_routes(theme, cx))
            .into_any_element()
    }

    fn render_tun_runtime(&self, theme: &gpui_component::Theme) -> gpui::Div {
        let snapshot = self.operational_status.snapshot();
        let tun = snapshot.capture.tun.value();
        let mut card = setting_card(zenclash_i18n::text("tun.runtime.title"), theme);
        if let Some(tun) = tun {
            card = card
                .child(info_row(
                    zenclash_i18n::text("tun.runtime.aggregate"),
                    tun_state_label(tun.observed),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("tun.runtime.permission"),
                    tun_state_label(tun.permission),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("tun.runtime.device"),
                    format!(
                        "{} · {}",
                        tun.runtime.device_name.as_deref().unwrap_or("—"),
                        tun_state_label(tun.runtime.device)
                    ),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("tun.runtime.route"),
                    tun_state_label(tun.runtime.route),
                    theme,
                ))
                .child(info_row(
                    zenclash_i18n::text("tun.runtime.evidence"),
                    &tun.runtime.detail,
                    theme,
                ));
        } else {
            card = card.child(message_banner(
                zenclash_i18n::text("tun.runtime.loading"),
                theme.primary,
                theme,
            ));
        }
        card
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
        let mut card = setting_card(zenclash_i18n::text("tun.permissions.title"), theme);
        match permissions {
            Some(Ok(status)) => {
                card = card
                    .child(info_row(
                        zenclash_i18n::text("tun.permissions.status"),
                        if status.granted {
                            zenclash_i18n::text("tun.permissions.ready")
                        } else if !status.can_request {
                            zenclash_i18n::text("tun.permissions.unavailable")
                        } else {
                            zenclash_i18n::text("tun.permissions.install")
                        },
                        theme,
                    ))
                    .child(info_row(
                        zenclash_i18n::text("tun.permissions.verification"),
                        &status.detail,
                        theme,
                    ))
                    .child(info_row(
                        zenclash_i18n::text("tun.permissions.core"),
                        status.binary.display().to_string(),
                        theme,
                    ));
            }
            Some(Err(error)) => {
                card = card.child(message_banner(error.clone(), theme.warning, theme));
            }
            None => {
                card = card.child(message_banner(
                    zenclash_i18n::text("tun.permissions.loading"),
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
                        zenclash_i18n::text("tun.permissions.action_ready")
                    } else {
                        zenclash_i18n::text("tun.permissions.action_install")
                    })
                    .primary()
                    .loading(self.mutating)
                    .disabled(self.mutating || granted || !can_request)
                    .on_click(cx.listener(|this, _, _, cx| this.grant_tun_permissions(cx))),
            ),
        )
    }

    fn grant_tun_permissions(&mut self, cx: &mut Context<Self>) {
        if self.mihomo_binary().is_none() {
            self.error = Some(zenclash_i18n::text("tun.errors.external_core"));
            cx.notify();
            return;
        }
        self.apply_tun_plan(
            true,
            zenclash_i18n::text("tun.notices.permission_ready"),
            cx,
        );
    }

    fn render_tun_switches(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let tun = self.config().cloned().unwrap_or_default().tun;
        setting_card(zenclash_i18n::text("tun.switches.title"), theme)
            .child(setting_switch(
                zenclash_i18n::text("tun.switches.enable"),
                zenclash_i18n::text_with(
                    "tun.switches.enable_description",
                    &[("core", self.core_kind.display_name().to_owned())],
                ),
                self.controlled_bool("/tun/enable", tun.enable),
                "tun-enable",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.apply_tun_plan(*checked, zenclash_i18n::text("tun.notices.enabled"), cx);
                }),
            ))
            .child(info_row(
                zenclash_i18n::text("tun.switches.stack"),
                &tun.stack,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("tun.switches.device"),
                empty_dash(&tun.device),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("tun.switches.dns_hijack"),
                tun.dns_hijack.join(", "),
                theme,
            ))
            .child(setting_switch(
                zenclash_i18n::text("tun.switches.auto_route"),
                zenclash_i18n::text_with(
                    "tun.switches.auto_route_description",
                    &[("core", self.core_kind.display_name().to_owned())],
                ),
                self.controlled_bool("/tun/auto-route", tun.auto_route),
                "tun-auto-route",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "auto-route",
                        *checked,
                        zenclash_i18n::text("tun.notices.auto_route"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("tun.switches.auto_interface"),
                zenclash_i18n::text("tun.switches.auto_interface_description"),
                self.controlled_bool("/tun/auto-detect-interface", tun.auto_detect_interface),
                "tun-auto-detect-interface",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "auto-detect-interface",
                        *checked,
                        zenclash_i18n::text("tun.notices.auto_interface"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("tun.switches.strict_route"),
                zenclash_i18n::text("tun.switches.strict_route_description"),
                self.controlled_bool("/tun/strict-route", tun.strict_route),
                "tun-strict-route",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "strict-route",
                        *checked,
                        zenclash_i18n::text("tun.notices.strict_route"),
                        cx,
                    );
                }),
            ))
            .child(setting_switch(
                zenclash_i18n::text("tun.switches.auto_redirect"),
                zenclash_i18n::text("tun.switches.auto_redirect_description"),
                self.controlled_bool("/tun/auto-redirect", false),
                "tun-auto-redirect",
                theme,
                cx.listener(|this, checked, _, cx| {
                    this.patch_tun_bool(
                        "auto-redirect",
                        *checked,
                        zenclash_i18n::text("tun.notices.auto_redirect"),
                        cx,
                    );
                }),
            ))
    }

    fn patch_tun_bool(
        &mut self,
        key: &'static str,
        value: bool,
        success: String,
        cx: &mut Context<Self>,
    ) {
        self.apply_controlled_config(json!({"tun": {key: value}}), success, cx);
    }

    fn apply_tun_plan(&mut self, enabled: bool, success: String, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Tun) else {
            return;
        };
        let capture = self.traffic_capture.clone();
        let task = self.runtime.spawn(async move {
            capture
                .apply(if enabled {
                    CapturePlan::Tun
                } else {
                    CapturePlan::Off
                })
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "tun.errors.permission_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                match result {
                    Ok(CaptureOutcome::Applied { .. } | CaptureOutcome::Unchanged { .. }) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(success);
                            this.reload_controlled_config(cx);
                            this.refresh(cx);
                        }
                    }
                    Ok(
                        CaptureOutcome::RolledBack { failure, .. }
                        | CaptureOutcome::ReconcileNeeded { failure, .. },
                    ) => this.set_page_error(token, failure),
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_tun_routes(
        &self,
        theme: &gpui_component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let inputs = &self.config_inputs.tun;
        setting_card(zenclash_i18n::text("tun.routes.title"), theme)
            .child(config_input_row(
                zenclash_i18n::text("tun.switches.stack"),
                zenclash_i18n::text("tun.routes.stack_description"),
                Input::new(&inputs.stack),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.routes.device_name"),
                zenclash_i18n::text("tun.routes.device_description"),
                Input::new(&inputs.device),
                theme,
            ))
            .child(config_input_row(
                "MTU",
                zenclash_i18n::text("tun.routes.mtu_description"),
                Input::new(&inputs.mtu),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.switches.dns_hijack"),
                zenclash_i18n::text("tun.routes.dns_description"),
                Input::new(&inputs.dns_hijack),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.routes.include"),
                zenclash_i18n::text("tun.routes.include_description"),
                Input::new(&inputs.route_include_address),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.routes.exclude"),
                zenclash_i18n::text("tun.routes.exclude_description"),
                Input::new(&inputs.route_exclude_address),
                theme,
            ))
            .child(
                h_flex().justify_end().p_4().child(
                    Button::new("save-tun-advanced")
                        .icon(IconName::Check)
                        .label(zenclash_i18n::text("tun.routes.save"))
                        .primary()
                        .loading(self.mutating)
                        .disabled(self.mutating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            match this.config_inputs.tun.patch(cx) {
                                Ok(patch) => this.apply_controlled_config(
                                    patch,
                                    zenclash_i18n::text("tun.notices.advanced"),
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

fn tun_state_label(state: CapabilityState) -> String {
    zenclash_i18n::text(match state {
        CapabilityState::Active => "tun.runtime.states.active",
        CapabilityState::Inactive => "tun.runtime.states.inactive",
        CapabilityState::Unknown => "tun.runtime.states.unknown",
        CapabilityState::Unsupported => "tun.runtime.states.unsupported",
    })
}
