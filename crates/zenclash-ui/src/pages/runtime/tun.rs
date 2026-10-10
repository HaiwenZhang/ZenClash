use super::settings::forms::{
    settings_input_row as config_input_row, settings_switch as setting_switch,
};
use super::{
    Button, ButtonVariants, Context, Disableable, IconName, Input, IntoElement, ParentElement,
    RuntimeData, RuntimePage, Styled, context_note, h_flex, info_row, json, setting_card, v_flex,
};
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{ActiveTheme, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};
use zenclash_core::{
    CapabilityState, CaptureOutcome, CapturePlan, CoreTunPermissionStatus, Observation,
    ServiceHealthKind, ServicePhase,
};

mod maintenance;
mod service;

impl RuntimePage {
    pub(super) fn render_tun(
        &self,
        compact: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let tun = self.config().cloned().unwrap_or_default().tun;
        let snapshot = self.operational_status.snapshot();
        let status = super::home::tun_status_text(&snapshot.capture);
        let disabled_reason = self.capture_control_disabled_reason(false).or_else(|| {
            snapshot
                .capture
                .tun
                .value()
                .filter(|tun| tun.observed == CapabilityState::Unsupported)
                .map(|_| zenclash_i18n::text("home.controls.tun_unsupported"))
        });
        let enabled = self.controlled_bool("/tun/enable", tun.enable);
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .p_5()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .gap_4()
                            .child(
                                gpui_kit::div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child(zenclash_i18n::text("tun.switches.enable")),
                            )
                            .child(
                                crate::components::mint_switch::MintSwitch::new("tun-enable")
                                    .accessibility_label(zenclash_i18n::text("tun.switches.enable"))
                                    .checked(enabled)
                                    .disabled(disabled_reason.is_some())
                                    .on_click(cx.listener(|this, checked, window, cx| {
                                        if *checked
                                            && this.profile_service.service_state().is_some()
                                        {
                                            this.request_service_tun(window, cx);
                                        } else {
                                            this.apply_tun_plan(
                                                *checked,
                                                zenclash_i18n::text("tun.notices.enabled"),
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    )
                    .child(
                        gpui_kit::div()
                            .id("tun-control-status")
                            .test_support()
                            .role(gpui_kit::Role::Status)
                            .aria_label(status.clone())
                            .text_sm()
                            .child(status),
                    )
                    .when_some(disabled_reason, |view, reason| {
                        view.child(
                            gpui_kit::div()
                                .id("tun-control-disabled-reason")
                                .test_support()
                                .role(gpui_kit::Role::Status)
                                .aria_label(reason.clone())
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(reason),
                        )
                    })
                    .child(
                        gpui_kit::div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.controls.capture_exclusive")),
                    ),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .when(compact, |view| view.flex_col())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .when(compact, |view| view.w_full())
                            .child(
                                self.render_tun_routes(theme, cx)
                                    .child(self.render_tun_switches(theme, cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .when(compact, |view| view.w_full())
                            .children(self.render_service_status(theme, cx))
                            .child(self.render_tun_permissions(theme, cx))
                            .child(self.render_tun_runtime(theme))
                            .child(
                                setting_card(
                                    zenclash_i18n::text("settings_redesign.tun_routes"),
                                    theme,
                                )
                                .child(self.settings_list_row(
                                    "tun.routes.include",
                                    &self.config_inputs.tun.route_include_address,
                                    cx,
                                ))
                                .child(self.settings_list_row(
                                    "tun.routes.exclude",
                                    &self.config_inputs.tun.route_exclude_address,
                                    cx,
                                )),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(self.settings_cancel(cx))
                    .child(
                        Button::new("save-tun-advanced")
                            .icon(IconName::Check)
                            .label(zenclash_i18n::text("common.actions.save"))
                            .primary()
                            .loading(self.core_busy())
                            .disabled(self.core_busy())
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
            .into_any_element()
    }

    pub(super) fn capture_control_disabled_reason(&self, pending: bool) -> Option<String> {
        let phase = self
            .profile_service
            .service_state()
            .map(|state| state.phase());
        let key = if self.profile_service.pending_finalization().is_some()
            || phase == Some(ServicePhase::Unconfirmed)
        {
            "tun.control.confirm_service"
        } else {
            match phase {
                Some(ServicePhase::Checking) => "core_page.service.checking",
                Some(ServicePhase::Authorizing) => "core_page.service.authorizing",
                Some(ServicePhase::Switching) => "core_page.service.switching",
                Some(ServicePhase::Enabling) => "core_page.service.enabling",
                Some(ServicePhase::Restoring) => "core_page.service.restoring",
                _ if pending => "home.controls.capture_switching",
                _ if self.core_busy() => "tun.control.core_busy",
                _ => return None,
            }
        };
        Some(zenclash_i18n::text(key))
    }

    pub(super) fn render_service_status(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Div> {
        let state = self.profile_service.service_state()?;
        let status = match state.health() {
            Some(ServiceHealthKind::Ready) => "core_page.service.ready",
            Some(ServiceHealthKind::NotInstalled) => "core_page.service.missing",
            Some(ServiceHealthKind::Unavailable(_)) => "core_page.service.unavailable",
            Some(ServiceHealthKind::VersionMismatch) => "core_page.service.incompatible",
            _ => "core_page.service.unknown",
        };
        Some(
            setting_card(zenclash_i18n::text("core_page.service.title"), theme).child(
                h_flex()
                    .px_4()
                    .pb_4()
                    .gap_3()
                    .justify_between()
                    .child(gpui_kit::div().text_sm().child(zenclash_i18n::text(status)))
                    .child(
                        Button::new("manage-core-service")
                            .outline()
                            .label(zenclash_i18n::text("settings_redesign.manage_service"))
                            .on_click(cx.listener(|_, _, window, cx| {
                                if window.has_active_dialog(cx) {
                                    return;
                                }
                                let owner = cx.entity().downgrade();
                                window.open_dialog(cx, move |dialog, window, cx| {
                                    let content = owner
                                        .update(cx, |page, cx| {
                                            page.render_service_details(&cx.theme().clone(), cx)
                                        })
                                        .ok()
                                        .flatten();
                                    dialog
                                        .title(zenclash_i18n::text("core_page.service.title"))
                                        .bg(cx.theme().group_box)
                                        .width(window.rem_size() * 40.)
                                        .margin_top(
                                            ((window.viewport_size().height
                                                - window.rem_size() * 24.)
                                                / 2.)
                                                .max(window.rem_size()),
                                        )
                                        .when_some(content, |dialog, content| dialog.child(content))
                                });
                            })),
                    ),
            ),
        )
    }

    pub(super) fn render_service_details(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Div> {
        let state = self.profile_service.service_state()?;
        let status = if self.profile_service.pending_finalization().is_some() {
            "core_page.service.pending"
        } else {
            match state.phase() {
                ServicePhase::Checking => "core_page.service.checking",
                ServicePhase::Authorizing => "core_page.service.authorizing",
                ServicePhase::Unconfirmed => "core_page.service.pending",
                ServicePhase::Switching => "core_page.service.switching",
                ServicePhase::Enabling => "core_page.service.enabling",
                ServicePhase::Restoring => "core_page.service.restoring",
                ServicePhase::Cancelled => "core_page.service.cancelled",
                ServicePhase::Failed => "core_page.service.failed",
                _ => match state.health() {
                    Some(ServiceHealthKind::Ready) => "core_page.service.ready",
                    Some(ServiceHealthKind::NotInstalled) => "core_page.service.missing",
                    Some(ServiceHealthKind::Unavailable(_)) => "core_page.service.unavailable",
                    Some(ServiceHealthKind::VersionMismatch) => "core_page.service.incompatible",
                    Some(ServiceHealthKind::Unknown) | None => "core_page.service.unknown",
                },
            }
        };
        Some(
            setting_card(zenclash_i18n::text("core_page.service.title"), theme)
                .child(info_row(
                    zenclash_i18n::text("tun.permissions.status"),
                    zenclash_i18n::text(status),
                    theme,
                ))
                .children(
                    self.profile_service
                        .service_tun_warning()
                        .map(|warning| context_note(warning, theme)),
                )
                .child(self.render_service_tun_actions(&state, cx)),
        )
    }

    fn render_tun_runtime(&self, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
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
            card = card.child(context_note(
                zenclash_i18n::text("tun.runtime.loading"),
                theme,
            ));
        }
        card
    }

    fn render_tun_permissions(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let permissions = match &self.data {
            RuntimeData::Tun { permissions, .. }
                if self.data_runtime_version == self.core_session.generation() =>
            {
                Some(permissions)
            }
            _ => None,
        };
        // Only current evidence can authorize an action; a retained stale value cannot.
        let current = permissions
            .filter(|observation| observation.is_fresh())
            .and_then(Observation::value);
        let (granted, can_request) = current.map_or((false, false), |status| {
            (status.granted(), status.can_request())
        });
        let mut card = setting_card(zenclash_i18n::text("tun.permissions.title"), theme);
        if let Some(status) = current {
            card = card.child(info_row(
                zenclash_i18n::text("tun.permissions.status"),
                zenclash_i18n::text(if granted {
                    "tun.permissions.ready"
                } else if can_request {
                    "tun.permissions.install"
                } else {
                    "tun.permissions.unavailable"
                }),
                theme,
            ));
            match status {
                CoreTunPermissionStatus::Local(status) => {
                    card = card
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
                CoreTunPermissionStatus::Service => {
                    card = card.child(info_row(
                        zenclash_i18n::text("tun.permissions.verification"),
                        zenclash_i18n::text("tun.permissions.service_authority"),
                        theme,
                    ));
                }
                _ => {}
            }
        } else {
            let message = match permissions {
                Some(Observation::Failed { failure, .. } | Observation::Stale { failure, .. }) => {
                    failure.message.clone()
                }
                _ => zenclash_i18n::text("tun.permissions.loading"),
            };
            card = card.child(context_note(message, theme));
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
                    .loading(self.core_busy())
                    .disabled(self.core_busy() || granted || !can_request)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.grant_tun_permissions(window, cx)),
                    ),
            ),
        )
    }

    fn grant_tun_permissions(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        if self.profile_service.service_state().is_some() {
            self.request_service_tun(window, cx);
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
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let tun = self.config().cloned().unwrap_or_default().tun;
        v_flex()
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
            .when(cfg!(target_os = "linux"), |view| {
                view.child(setting_switch(
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
            })
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
        let Some(token) = self.begin_mutation(self.page) else {
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
                this.finish_mutation(token);
                match result {
                    Ok(CaptureOutcome::Applied { .. } | CaptureOutcome::Unchanged { .. }) => {
                        this.reload_controlled_config(cx);
                        if this.is_page_task_current(token) {
                            this.notice = Some(success);
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
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let inputs = &self.config_inputs.tun;
        setting_card(zenclash_i18n::text("settings_redesign.tun_basics"), theme)
            .child(config_input_row(
                zenclash_i18n::text("tun.switches.stack"),
                zenclash_i18n::text("tun.routes.stack_description"),
                self.settings_choice(
                    "tun-stack",
                    &inputs.stack,
                    &["mixed", "system", "gvisor"],
                    cx,
                ),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.routes.device_name"),
                zenclash_i18n::text("tun.routes.device_description"),
                Input::new(&inputs.device).cleanable(true),
                theme,
            ))
            .child(config_input_row(
                "MTU",
                zenclash_i18n::text("tun.routes.mtu_description"),
                Input::new(&inputs.mtu).cleanable(true),
                theme,
            ))
            .child(config_input_row(
                zenclash_i18n::text("tun.switches.dns_hijack"),
                zenclash_i18n::text("tun.routes.dns_description"),
                Input::new(&inputs.dns_hijack).cleanable(true),
                theme,
            ))
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
