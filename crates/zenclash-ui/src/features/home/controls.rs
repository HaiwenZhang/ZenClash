use super::status::{capture_status_text, process_evidence, system_proxy_status_text};
use super::transitions::{capture_presentation, capture_transition_status_text, mode_presentation};
use super::*;

impl RuntimePage {
    pub(super) fn render_home_controls(
        &self,
        config: &RuntimeConfig,
        capture: &CaptureStatus,
        theme: &gpui_kit::component::Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let transition = self.home.capture_transition.as_ref();
        let service_pending = self.application_commands.pending_finalization().is_some();
        let effective_capture = transition
            .and_then(|transition| transition.confirmed.as_ref())
            .unwrap_or(capture);
        let proxy = effective_capture.system_proxy.value();
        let confirmed_system_proxy = proxy
            .map_or(self.preferences.system_proxy_enabled, |snapshot| {
                snapshot.intent_enabled
            });
        let confirmed_tun = effective_capture
            .tun
            .value()
            .map_or(config.tun.enable, |tun| tun.requested || tun.configured);
        let capture_presentation =
            capture_presentation(confirmed_system_proxy, confirmed_tun, transition);
        let proxy_status = capture_transition_status_text(
            confirmed_system_proxy,
            capture_presentation.system_proxy_enabled,
            capture_presentation.pending,
            "home.controls.system_proxy_enabling",
            "home.controls.system_proxy_disabling",
        )
        .unwrap_or_else(|| system_proxy_status_text(effective_capture));
        let tun_status = capture_transition_status_text(
            confirmed_tun,
            capture_presentation.tun_enabled,
            capture_presentation.pending,
            "home.controls.tun_enabling",
            "home.controls.tun_disabling",
        )
        .unwrap_or_else(|| tun_status_text(effective_capture));
        let tun_enabled = capture_presentation.tun_enabled;
        let tun_supported = effective_capture
            .tun
            .value()
            .is_none_or(|tun| tun.observed != CapabilityState::Unsupported);
        let capture_disabled_reason =
            self.capture_control_disabled_reason(capture_presentation.pending);
        let tun_disabled_reason = capture_disabled_reason.clone().or_else(|| {
            (!tun_supported).then(|| zenclash_i18n::text("home.controls.tun_unsupported"))
        });
        let capture_status = if capture_presentation.pending {
            zenclash_i18n::text("home.controls.capture_switching")
        } else {
            capture_status_text(effective_capture)
        };
        let presentation = mode_presentation(
            OutboundMode::from_api(&config.mode),
            self.home.mode_transition,
        );
        let mode = presentation.displayed;
        let mode_pending = presentation.pending;
        let port = config.system_proxy_port().map_or_else(
            || zenclash_i18n::text("home.controls.no_proxy_port"),
            |port| format!("127.0.0.1:{port}"),
        );
        let memory = core_memory_bytes(
            &self.home_operational_snapshot().streams,
            self.home.generation,
        )
        .map_or_else(|| "—".to_owned(), format_core_memory);

        let snapshot = self.home_operational_snapshot();
        let process = snapshot
            .process
            .is_fresh()
            .then(|| snapshot.process.value())
            .flatten()
            .filter(|process| process.generation == self.home.generation);
        let (runtime_status, runtime_color) = process_evidence(process, theme);
        let runtime_detail = crate::components::sidebar::runtime_uptime(process).map_or_else(
            || self.core_kind.display_name().to_owned(),
            |uptime| format!("{} · {uptime}", self.core_kind.display_name()),
        );
        let orb_color = if process.is_some_and(|process| process.running) {
            crate::foundation::design::online_indicator(theme)
        } else {
            runtime_color
        };
        let mode_controls = h_flex()
            .gap_0()
            .p_0p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(mode_button(
                "home-mode-rule",
                OutboundMode::Rule,
                mode,
                mode_pending,
                SetRuleMode,
                cx,
            ))
            .child(mode_button(
                "home-mode-global",
                OutboundMode::Global,
                mode,
                mode_pending,
                SetGlobalMode,
                cx,
            ))
            .child(mode_button(
                "home-mode-direct",
                OutboundMode::Direct,
                mode,
                mode_pending,
                SetDirectMode,
                cx,
            ));
        v_flex()
            .id("home-runtime-card")
            .test_support()
            .flex_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .w_full()
            .child(
                h_flex()
                    .px_4()
                    .py_4()
                    .gap_3()
                    .flex_wrap()
                    .justify_between()
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_3()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .size_16()
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme.chart_2.opacity(0.35))
                                    .bg(orb_color.opacity(0.08))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .size_8()
                                            .rounded_full()
                                            .bg(orb_color.opacity(0.10))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                div().size_5().rounded_full().bg(orb_color).shadow(
                                                    vec![gpui_kit::BoxShadow {
                                                        inset: false,
                                                        color: orb_color.opacity(0.38),
                                                        offset: gpui_kit::point(px(0.), px(0.)),
                                                        blur_radius: theme.font_size * 0.875,
                                                        spread_radius: theme.font_size * 0.1875,
                                                    }],
                                                ),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_4()
                                            .flex_wrap()
                                            .child(
                                                div()
                                                    .text_size(gpui_kit::rems(1.625))
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .child(runtime_status),
                                            )
                                            .child(
                                                h_flex()
                                                    .id("home-core-memory")
                                                    .test_support()
                                                    .flex_shrink_0()
                                                    .gap_2()
                                                    .px_3()
                                                    .py_1p5()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(theme.primary.opacity(0.16))
                                                    .bg(theme.primary.opacity(0.06))
                                                    .text_sm()
                                                    .child(
                                                        Icon::new(IconName::Cpu)
                                                            .size_4()
                                                            .text_color(theme.primary),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_color(theme.muted_foreground)
                                                            .child(zenclash_i18n::text(
                                                                "home.traffic.core_memory",
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .font_weight(
                                                                gpui_kit::FontWeight::SEMIBOLD,
                                                            )
                                                            .child(memory),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .when(compact, |label| label.text_xs())
                                            .text_color(theme.muted_foreground)
                                            .child(runtime_detail),
                                    ),
                            ),
                    )
                    .child(mode_controls),
            )
            .child(self.render_home_metrics(&snapshot.streams, theme))
            .child(
                v_flex()
                    .px_4()
                    .pb_3()
                    .gap_2()
                    .when(mode_pending, |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(
                            zenclash_i18n::text_with(
                                "home.controls.mode_switching",
                                &[("mode", mode.label())],
                            ),
                        ))
                    })
                    .children(
                        home_service_feedback(
                            self.application_commands
                                .service_state()
                                .map(|state| state.phase()),
                            service_pending,
                            self.application_commands.service_tun_warning(),
                            theme,
                        )
                        .map(|feedback| {
                            feedback.child(
                                Button::new("home-service-details")
                                    .label(zenclash_i18n::text("navigation.tun.label"))
                                    .small()
                                    .outline()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(NavigateTun), cx)
                                    }),
                            )
                        }),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .pt_3()
                            .border_t_1()
                            .border_color(theme.border)
                            .items_start()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w(rems(10.))
                                    .justify_between()
                                    .gap_3()
                                    .py_1()
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                Button::new("home-open-system-proxy")
                                                    .label(zenclash_i18n::text("tray.system_proxy"))
                                                    .tooltip(format!(
                                                        "{capture_status} · {proxy_status}"
                                                    ))
                                                    .small()
                                                    .text()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .on_click(|_, window, cx| {
                                                        window.dispatch_action(
                                                            Box::new(NavigateSystemProxy),
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(port),
                                            )
                                            .child(
                                                div()
                                                    .id("home-system-proxy-status")
                                                    .test_support()
                                                    .role(gpui_kit::Role::Status)
                                                    .aria_label(proxy_status.clone())
                                                    .text_xs()
                                                    .child(proxy_status),
                                            ),
                                    )
                                    .child(
                                        Switch::new("home-system-proxy")
                                            .accessibility_label(zenclash_i18n::text(
                                                "tray.system_proxy",
                                            ))
                                            .checked(capture_presentation.system_proxy_enabled)
                                            .disabled(capture_disabled_reason.is_some())
                                            .on_click(cx.listener(|this, checked, window, cx| {
                                                this.apply_home_capture_plan(
                                                    if *checked {
                                                        CapturePlan::SystemProxy
                                                    } else {
                                                        CapturePlan::Off
                                                    },
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w(rems(10.))
                                    .pl_4()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .justify_between()
                                    .gap_3()
                                    .py_1()
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                Button::new("home-open-tun")
                                                    .label(zenclash_i18n::text("home.controls.tun"))
                                                    .small()
                                                    .text()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .on_click(|_, window, cx| {
                                                        window.dispatch_action(
                                                            Box::new(NavigateTun),
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .id("home-tun-status")
                                                    .test_support()
                                                    .role(gpui_kit::Role::Status)
                                                    .aria_label(tun_status.clone())
                                                    .text_xs()
                                                    .child(tun_status),
                                            )
                                            .when_some(
                                                tun_disabled_reason.as_ref(),
                                                |view, reason| {
                                                    view.child(
                                                        div()
                                                            .id("home-tun-disabled-reason")
                                                            .test_support()
                                                            .role(gpui_kit::Role::Status)
                                                            .aria_label(reason.clone())
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(reason.clone()),
                                                    )
                                                },
                                            ),
                                    )
                                    .child(
                                        Switch::new("home-tun")
                                            .accessibility_label(zenclash_i18n::text(
                                                "home.controls.tun",
                                            ))
                                            .checked(tun_enabled)
                                            .disabled(tun_disabled_reason.is_some())
                                            .on_click(cx.listener(
                                                move |this, checked, window, cx| {
                                                    this.apply_home_capture_plan(
                                                        if *checked {
                                                            CapturePlan::Tun
                                                        } else {
                                                            CapturePlan::Off
                                                        },
                                                        window,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .when_some(self.home.action_error.as_ref(), |view, error| {
                        view.child(
                            div()
                                .id("home-capture-error")
                                .test_support()
                                .role(gpui_kit::Role::Alert)
                                .aria_label(error.clone())
                                .text_sm()
                                .text_color(theme.danger)
                                .child(error.clone()),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.controls.capture_exclusive")),
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn home_service_feedback(
    phase: Option<zenclash_core::ServicePhase>,
    pending_commit: bool,
    warning: Option<String>,
    theme: &gpui_kit::component::Theme,
) -> Option<gpui_kit::Div> {
    let pending = pending_commit || phase == Some(zenclash_core::ServicePhase::Unconfirmed);
    if !pending && warning.is_none() {
        return None;
    }
    let pending_message = zenclash_i18n::text("core_page.service.pending");
    Some(
        v_flex()
            .gap_3()
            .children(pending.then(|| {
                div()
                    .id("home-service-pending")
                    .role(gpui_kit::Role::Status)
                    .aria_label(pending_message.clone())
                    .child(context_note(pending_message, theme))
                    .test_support()
            }))
            .children(warning.map(|warning| {
                div()
                    .id("home-service-warning")
                    .role(gpui_kit::Role::Status)
                    .aria_label(warning.clone())
                    .child(context_note(warning, theme))
                    .test_support()
            })),
    )
}

fn mode_button<A>(
    id: &'static str,
    value: OutboundMode,
    selected: OutboundMode,
    pending: bool,
    action: A,
    cx: &mut Context<RuntimePage>,
) -> Button
where
    A: gpui_kit::Action + Clone + 'static,
{
    let is_selected = value == selected;
    Button::new(id)
        .label(value.label())
        .tooltip(mode_description(value))
        .small()
        .w(rems(3.75))
        .custom(
            ButtonCustomVariant::new(cx)
                .color(if is_selected {
                    cx.theme().list_active
                } else {
                    cx.theme().transparent
                })
                .foreground(if is_selected {
                    cx.theme().primary
                } else {
                    cx.theme().muted_foreground
                })
                .hover(cx.theme().list_active)
                .active(cx.theme().list_active),
        )
        .selected(is_selected)
        .loading(is_selected && pending)
        .disabled(pending)
        .on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(action.clone()), cx);
        })
}

fn mode_description(mode: OutboundMode) -> String {
    zenclash_i18n::text(match mode {
        OutboundMode::Rule => "home.controls.mode_rule",
        OutboundMode::Global => "home.controls.mode_global",
        OutboundMode::Direct => "home.controls.mode_direct",
    })
}
