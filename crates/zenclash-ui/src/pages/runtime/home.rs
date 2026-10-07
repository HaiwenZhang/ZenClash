use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui_kit::base::{StyledExt, TestSupportExt};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    chart::AreaChart,
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::{
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, div, px, rems,
};
use zenclash_core::{
    CapabilityState, CaptureOutcome, CapturePlan, CaptureStatus, ConnectionPolicy, Observation,
    OperationalSnapshot, ProcessRecoveryStatus, ProcessStatus, ProxyCatalog, ProxyGroup,
    ProxyGroupBehavior, ProxyOperations, RuntimeConfig, StreamStatus, StreamStatuses,
    SubscriptionUsage, SystemProxyOwnershipState, TrafficSample, format_speed,
};

use crate::{
    app::{NavigateProfiles, NavigateSystemProxy, SetDirectMode, SetGlobalMode, SetRuleMode},
    components::{
        mint_switch::MintSwitch as Switch, sidebar::OutboundMode, wave_percentage::WavePercentage,
    },
};

use super::{
    Context, FluentBuilder, Page, ProxySelectionChanged, RuntimeData, RuntimePage, context_note,
    format_bytes, format_profile_age, normalized_fraction,
};

mod dashboard;
#[cfg(test)]
mod flow;
#[cfg(test)]
mod history;
mod selector;
mod traffic;
#[cfg(all(test, target_os = "windows"))]
mod validation;

const HOME_PROFILE_PICKER_LIMIT: usize = 20;

const MIN_TRAFFIC_CHART_CEILING: u64 = 1_024;

#[derive(Default)]
pub(super) struct HomeUiState {
    pub(super) profile_switching: Option<String>,
    pub(super) proxy_switching: Option<(String, String)>,
    pub(super) proxy_error: Option<String>,
    capture_transition: Option<CaptureTransition>,
    pub(super) action_error: Option<String>,
    mode_transition: Option<ModeTransition>,
    projection: Option<selector::HomeProxyProjection>,
    chart: traffic::HomeChartState,
    generation: u64,
    selected_group: Option<String>,
    selector_width_rems: f32,
    #[cfg(test)]
    flow: flow::HomeFlowState,
    #[cfg(test)]
    history: history::HomeHistoryState,
    #[cfg(test)]
    design_validation: Option<(OperationalSnapshot, zenclash_core::TrafficSnapshot)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CaptureTransition {
    displayed: CapturePlan,
    pending: bool,
    confirmed: Option<CaptureStatus>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CapturePresentation {
    system_proxy_enabled: bool,
    tun_enabled: bool,
    pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModeTransition {
    displayed: OutboundMode,
    pending: bool,
}

impl RuntimePage {
    fn home_operational_snapshot(&self) -> OperationalSnapshot {
        #[cfg(test)]
        if let Some((snapshot, _)) = &self.home.design_validation {
            return snapshot.clone();
        }
        self.operational_status.snapshot()
    }

    fn home_traffic_snapshot(&self) -> zenclash_core::TrafficSnapshot {
        #[cfg(test)]
        if let Some((_, snapshot)) = &self.home.design_validation {
            return snapshot.clone();
        }
        self.traffic_monitor.snapshot()
    }

    pub(in crate::pages::runtime) fn release_home_presentation(&mut self) {
        #[cfg(test)]
        {
            self.home.design_validation = None;
            self.home.flow.release();
            self.home.history.release();
        }
        self.home.chart = traffic::HomeChartState::default();
        self.home.projection = None;
        self.home.selected_group = None;
    }

    pub(in crate::pages::runtime) fn render_home(
        &self,
        theme: &gpui_kit::component::Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let fallback_config = RuntimeConfig::default();
        let config = match &self.data {
            RuntimeData::Dashboard { config, .. } => config.value().unwrap_or(&fallback_config),
            _ => &fallback_config,
        };
        let operational = self.home_operational_snapshot();

        v_flex()
            .gap_3()
            .child(self.render_home_controls(config, &operational.capture, theme, compact, cx))
            .when_some(
                self.app_update
                    .status
                    .as_ref()
                    .and_then(|status| match status {
                        zenclash_core::AppUpdateStatus::Available { release, .. } => {
                            Some(release.tag.clone())
                        }
                        zenclash_core::AppUpdateStatus::NoPublishedRelease { .. }
                        | zenclash_core::AppUpdateStatus::UpToDate { .. } => None,
                    }),
                |this, version| {
                    this.child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.success.opacity(0.35))
                            .bg(theme.success.opacity(0.08))
                            .child(div().text_sm().child(zenclash_i18n::text_with(
                                "home.app_update.available",
                                &[("version", version)],
                            )))
                            .child(
                                Button::new("home-view-app-update")
                                    .icon(IconName::ExternalLink)
                                    .label(zenclash_i18n::text("home.app_update.action"))
                                    .small()
                                    .outline()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.switch_to(Page::Settings, cx);
                                    })),
                            ),
                    )
                },
            )
            .child(self.render_home_proxy(theme, cx))
            .child(
                h_flex()
                    .id("home-traffic-row")
                    .test_support()
                    .items_stretch()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        v_flex()
                            .id("home-traffic-panel")
                            .test_support()
                            .flex_grow(1.75)
                            .flex_basis(rems(38.))
                            .min_w_0()
                            .gap_3()
                            .child(self.render_home_traffic(&operational.streams, theme, cx)),
                    )
                    .child(
                        v_flex()
                            .id("home-subscription-panel")
                            .test_support()
                            .flex_1()
                            .flex_basis(rems(22.))
                            .min_w_0()
                            .child(self.render_home_profile(theme, cx)),
                    ),
            )
            .when(
                operational
                    .process
                    .value()
                    .is_some_and(|process| !process.running),
                |view| view.child(self.render_home_evidence(&operational, theme)),
            )
            .into_any_element()
    }

    fn render_home_evidence(
        &self,
        operational: &OperationalSnapshot,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::AnyElement {
        let process = operational
            .process
            .is_fresh()
            .then(|| operational.process.value())
            .flatten()
            .filter(|process| process.generation == self.home.generation);
        let controller_ready = operational.controller.is_fresh()
            && operational.controller.value().is_some_and(|controller| {
                controller.authenticated && controller.generation == self.home.generation
            });
        let path_ready = operational.path.is_fresh()
            && operational
                .path
                .value()
                .is_some_and(|path| path.generation == self.home.generation);
        let (process_text, process_color) = process_evidence(process, theme);
        v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                h_flex()
                    .gap_4()
                    .flex_wrap()
                    .child(status_label(process_text, process_color, theme))
                    .child(status_label(
                        zenclash_i18n::text(if controller_ready {
                            "home.evidence.controller_verified"
                        } else {
                            "home.evidence.controller_unverified"
                        }),
                        if controller_ready {
                            theme.success
                        } else {
                            theme.warning
                        },
                        theme,
                    ))
                    .child(status_label(
                        capture_status_text(&operational.capture),
                        if operational.capture.is_active() {
                            theme.success
                        } else {
                            theme.warning
                        },
                        theme,
                    ))
                    .child(status_label(
                        zenclash_i18n::text(if path_ready {
                            "home.evidence.path_observed"
                        } else {
                            "home.evidence.path_unknown"
                        }),
                        if path_ready {
                            theme.success
                        } else {
                            theme.muted_foreground
                        },
                        theme,
                    )),
            )
            .when_some(
                process.and_then(|process| process.exit_reason.as_ref()),
                |this, reason| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(
                        zenclash_i18n::text_with(
                            "home.evidence.core_exit_reason",
                            &[("reason", reason.clone())],
                        ),
                    ))
                },
            )
            .into_any_element()
    }

    fn render_home_profile(
        &self,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let active = self.profiles.active_profile();
        let name = active.map_or_else(
            || zenclash_i18n::text("home.profile.none"),
            |profile| profile.name.clone(),
        );
        let source = active.map_or_else(
            || zenclash_i18n::text("home.profile.none_description"),
            |profile| {
                zenclash_i18n::text_with(
                    "profiles.catalog.updated",
                    &[("age", format_profile_age(profile.updated_at))],
                )
            },
        );
        let quota = active.and_then(|profile| profile.subscription.usage.as_ref());
        let expiry = active
            .and_then(|profile| profile.subscription.usage.as_ref())
            .map_or_else(
                || zenclash_i18n::text("home.profile.expiry_unavailable"),
                subscription_expiry,
            );
        let active_id = self.profiles.catalog.active.clone();
        let mut profiles = self
            .profiles
            .catalog
            .profiles
            .iter()
            .take(HOME_PROFILE_PICKER_LIMIT)
            .map(|profile| (profile.id.clone(), profile.name.clone()))
            .collect::<Vec<_>>();
        if let Some(active) = active
            && !profiles.iter().any(|(id, _)| id == &active.id)
        {
            profiles.push((active.id.clone(), active.name.clone()));
        }
        let can_switch = self.profiles.store.is_some() && !profiles.is_empty();
        let runtime_page = cx.entity().downgrade();
        let profile_switch_tooltip =
            zenclash_i18n::text_with("home.profile.switch_current", &[("name", name.clone())]);
        let profile_picker = Button::new("home-profile-picker")
            .label(name)
            .icon(Icon::new(IconName::File).size_6())
            .justify_start()
            .min_w_0()
            .overflow_hidden()
            .ghost()
            .small()
            .h_8()
            .text_lg()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .dropdown_caret(true)
            .tooltip(profile_switch_tooltip)
            .loading(self.home.profile_switching.is_some())
            .disabled(self.core_busy() || !can_switch)
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu
                    .min_w(px(240.))
                    .max_w(px(420.))
                    .max_h(px(360.))
                    .scrollable(true);
                for (id, name) in &profiles {
                    let is_active = active_id.as_deref() == Some(id.as_str());
                    let runtime_page = runtime_page.clone();
                    let id = id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(is_active)
                            .disabled(is_active)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                let _ = runtime_page.update(cx, |page, cx| {
                                    page.activate_home_profile(id, cx);
                                });
                            }),
                    );
                }
                menu.item(
                    PopupMenuItem::new(zenclash_i18n::text("home.profile.all")).on_click(
                        |_, window, cx| window.dispatch_action(Box::new(NavigateProfiles), cx),
                    ),
                )
            });

        v_flex()
            .flex_1()
            .line_height(gpui_kit::relative(1.25))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .min_w_0()
            .child(
                h_flex().px_4().pt_4().justify_between().gap_2().child(
                    div()
                        .text_lg()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(zenclash_i18n::text("home.profile.title")),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .child(h_flex().gap_2().child(profile_picker).when_some(
                        active,
                        |row, profile| {
                            row.child(
                                div()
                                    .text_xs()
                                    .px_2()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .bg(theme.chart_3.opacity(0.12))
                                    .text_color(theme.primary)
                                    .child(profile.source_label()),
                            )
                        },
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(source),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .items_center()
                            .flex_wrap()
                            .child(
                                WavePercentage::new(
                                    "home-subscription-quota",
                                    zenclash_i18n::text("profiles.design.used"),
                                )
                                .diameter(9.)
                                .value(
                                    quota.filter(|value| value.total > 0).map(|value| {
                                        100. * normalized_fraction(value.used(), value.total)
                                    }),
                                ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .pl_4()
                                    .border_l_1()
                                    .border_color(theme.border)
                                    .gap_2()
                                    .children(
                                        [
                                            (
                                                "profiles.design.used",
                                                quota.map(SubscriptionUsage::used),
                                            ),
                                            (
                                                "profiles.design.remaining",
                                                quota.filter(|value| value.total > 0).map(
                                                    |value| {
                                                        value.total.saturating_sub(value.used())
                                                    },
                                                ),
                                            ),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(key, bytes)| {
                                                v_flex()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(theme.muted_foreground)
                                                            .child(zenclash_i18n::text(key)),
                                                    )
                                                    .child(
                                                        div().text_size(px(26.)).font_bold().child(
                                                            bytes.map_or_else(
                                                                || "—".to_owned(),
                                                                format_bytes,
                                                            ),
                                                        ),
                                                    )
                                            },
                                        ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(zenclash_i18n::text("profiles.design.total"))
                                            .child(
                                                quota.filter(|value| value.total > 0).map_or_else(
                                                    || "—".into(),
                                                    |value| format_bytes(value.total),
                                                ),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(expiry)
                            .when_some(quota.and_then(subscription_expiry_date), |row, date| {
                                row.child(date)
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_home_controls(
        &self,
        config: &RuntimeConfig,
        capture: &CaptureStatus,
        theme: &gpui_kit::component::Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let transition = self.home.capture_transition.as_ref();
        let service_pending = self.profile_service.pending_finalization().is_some();
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
            theme.chart_3
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
                                                        blur_radius: px(14.),
                                                        spread_radius: px(3.),
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
                                                    .text_size(px(26.))
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
                            self.profile_service
                                .service_state()
                                .map(|state| state.phase()),
                            service_pending,
                            self.profile_service.service_tun_warning(),
                            theme,
                        )
                        .map(|feedback| {
                            feedback.child(
                                Button::new("home-service-details")
                                    .label(zenclash_i18n::text("navigation.tun.label"))
                                    .small()
                                    .outline()
                                    .on_click(|_, window, cx| {
                                        window
                                            .dispatch_action(Box::new(crate::app::NavigateTun), cx)
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
                                            ),
                                    )
                                    .child(
                                        Switch::new("home-system-proxy")
                                            .accessibility_label(zenclash_i18n::text(
                                                "tray.system_proxy",
                                            ))
                                            .checked(capture_presentation.system_proxy_enabled)
                                            .disabled(capture_presentation.pending)
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
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                    .child(zenclash_i18n::text(
                                                        "home.controls.tun",
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(tun_status.clone()),
                                            ),
                                    )
                                    .child(
                                        Button::new("home-tun")
                                            .label(zenclash_i18n::text(if tun_enabled {
                                                "home.controls.tun_disable"
                                            } else {
                                                "home.controls.tun_enable"
                                            }))
                                            .tooltip(tun_status.clone())
                                            .outline()
                                            .small()
                                            .disabled(
                                                capture_presentation.pending
                                                    || !tun_supported
                                                    || service_pending
                                                    || self.core_busy()
                                                    || self
                                                        .profile_service
                                                        .service_state()
                                                        .is_some_and(|state| state.is_busy()),
                                            )
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.apply_home_capture_plan(
                                                    if tun_enabled {
                                                        CapturePlan::Off
                                                    } else {
                                                        CapturePlan::Tun
                                                    },
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_home_traffic(
        &self,
        streams: &StreamStatuses,
        theme: &gpui_kit::component::Theme,
        _cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let traffic = self.home_traffic_snapshot();
        let unknown_stream = Observation::<StreamStatus>::Loading;
        let traffic_stream = if streams
            .traffic
            .value()
            .is_some_and(|status| status.generation != self.home.generation)
        {
            &unknown_stream
        } else {
            &streams.traffic
        };
        let (points, observed_seconds) = self.home.chart.points(self.home.generation);
        let ceiling = points
            .first()
            .map_or(MIN_TRAFFIC_CHART_CEILING as f64, |point| point.ceiling);
        let (axis_unit, axis_divisor) = if ceiling >= 1_048_576. {
            ("MiB/s", 1_048_576.)
        } else if ceiling >= 1_024. {
            ("KiB/s", 1_024.)
        } else {
            ("B/s", 1.)
        };
        let chart = AreaChart::new(points)
            .x(|point| point.label.clone())
            .y(|point| point.download)
            .name(zenclash_i18n::text("home.traffic.download"))
            .stroke(theme.chart_1)
            .fill(theme.chart_1.opacity(0.18))
            .natural()
            .y(|point| point.upload)
            .name(zenclash_i18n::text("home.traffic.upload"))
            .stroke(theme.chart_2)
            .fill(theme.chart_2.opacity(0.14))
            .natural()
            .y_domain(0., ceiling)
            .x_axis(false)
            .y_axis(false)
            .grid(false)
            .y_padding(0., 0.)
            .tooltip_value(|_, _, value| format_speed(value.max(0.) as u64).into());
        // A fixed two-minute frame is separate from the observed series. Unknown
        // history stays blank; stretching a few seconds across the frame would
        // misrepresent both the time axis and the rates.
        let plot = h_flex()
            .items_stretch()
            .h(rems(10.))
            .gap_2()
            .child(
                v_flex()
                    .w(rems(2.))
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .children((0..=3).rev().map(|tick| {
                        div().text_right().child(format!(
                            "{:.1}",
                            ceiling / axis_divisor * f64::from(tick) / 3.
                        ))
                    })),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        AreaChart::new(vec![(0., 0.), (1., 0.)])
                            .id("home-traffic-grid")
                            .x(|point| point.0.to_string())
                            .y(|point| point.1)
                            .y_domain(0., ceiling)
                            .y_padding(0., 0.)
                            .stroke(theme.transparent)
                            .fill(theme.transparent)
                            .x_axis(false)
                            .y_axis(false)
                            .y_tick_count(4)
                            .grid_columns(5)
                            .grid_dashed(false)
                            .interactive(false),
                    )
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .h_full()
                            .w(gpui_kit::relative(traffic::observed_span_fraction(
                                observed_seconds,
                            )))
                            .child(chart),
                    ),
            );
        let time_axis = h_flex()
            .pl(gpui_kit::rems(2.5))
            .justify_between()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(["−2m", "−1m 30s", "−1m", "−30s"].map(|label| div().child(label)))
            .child(zenclash_i18n::text("home.traffic.now"));

        v_flex()
            .flex_1()
            .min_w_0()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(
                        div()
                            .text_lg()
                            .font_bold()
                            .child(zenclash_i18n::text("home.traffic.title")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("home.traffic.range_120")),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(div().text_color(theme.muted_foreground).child(axis_unit))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(series_label(
                                "home-upload-legend",
                                IconName::ArrowUp,
                                zenclash_i18n::text("home.traffic.upload"),
                                format_speed(traffic.upload),
                                theme.chart_2,
                                theme,
                            ))
                            .child(series_label(
                                "home-download-legend",
                                IconName::ArrowDown,
                                zenclash_i18n::text("home.traffic.download"),
                                format_speed(traffic.download),
                                theme.chart_1,
                                theme,
                            )),
                    ),
            )
            .when(!traffic_stream.is_fresh(), |view| {
                let (status, color) = stream_status_text(traffic_stream, theme);
                view.child(status_label(status, color, theme))
            })
            .child(v_flex().gap_2().w_full().child(plot).child(time_axis))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                zenclash_i18n::text_with(
                    "home.traffic.observed_duration",
                    &[("seconds", observed_seconds.to_string())],
                ),
            ))
            .into_any_element()
    }

    fn change_home_proxy(&mut self, group: String, proxy: String, cx: &mut Context<Self>) {
        if self.page != Page::Home
            || self.home.profile_switching.is_some()
            || self.home.proxy_switching.is_some()
        {
            return;
        }
        self.invalidate_page_load();
        let token = self.page_task_token_for(Page::Home);
        self.home.proxy_switching = Some((group.clone(), proxy.clone()));
        self.home.proxy_error = None;
        let client = self.client.clone();
        let task_group = group.clone();
        let task_proxy = proxy.clone();
        let task = self.runtime.spawn(async move {
            ProxyOperations::new(client)
                .apply_selection(&task_group, &task_proxy, ConnectionPolicy::KeepExisting)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "proxies.errors.switch_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.home.proxy_switching.as_ref() != Some(&(group.clone(), proxy.clone())) {
                    return;
                }
                this.home.proxy_switching = None;
                match result {
                    Ok(receipt) => {
                        for warning in receipt.warnings {
                            tracing::warn!(%warning, "home proxy selection completed with a warning");
                        }
                        cx.emit(ProxySelectionChanged);
                        if this.is_page_task_current(token) {
                            apply_home_proxy_selection(&mut this.data, &group, &proxy);
                            this.prepare_home_projection();
                            this.notice = Some(zenclash_i18n::text_with(
                                "home.proxy.switched",
                                &[("name", proxy.clone())],
                            ));
                            this.refresh(cx);
                        }
                    }
                    Err(error) => {
                        if this.is_page_task_current(token) {
                            this.home.proxy_error = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_network_capture_settings(
        &self,
        config: Option<&RuntimeConfig>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let snapshot = self.operational_status.snapshot();
        let transition = self.home.capture_transition.as_ref();
        let capture = transition
            .and_then(|state| state.confirmed.as_ref())
            .unwrap_or(&snapshot.capture);
        let proxy = capture
            .system_proxy
            .value()
            .map_or(self.preferences.system_proxy_enabled, |value| {
                value.intent_enabled
            });
        let tun = capture
            .tun
            .value()
            .map_or(config.is_some_and(|value| value.tun.enable), |value| {
                value.requested || value.configured
            });
        let display = capture_presentation(proxy, tun, transition);
        let pending = display.pending
            || self.core_busy()
            || self.profile_service.pending_finalization().is_some();
        super::setting_card(zenclash_i18n::text("unified.settings.network"), theme)
            .child(super::common::setting_switch_disabled(
                zenclash_i18n::text("tray.system_proxy"),
                zenclash_i18n::text("unified.network.system_proxy_description"),
                display.system_proxy_enabled,
                "settings-system-proxy",
                theme,
                pending || config.is_none(),
                cx.listener(|this, checked, window, cx| {
                    this.apply_home_capture_plan(
                        if *checked {
                            CapturePlan::SystemProxy
                        } else {
                            CapturePlan::Off
                        },
                        window,
                        cx,
                    );
                }),
            ))
            .child(super::common::setting_switch_disabled(
                zenclash_i18n::text("navigation.tun.label"),
                zenclash_i18n::text("unified.network.tun_description"),
                display.tun_enabled,
                "settings-tun",
                theme,
                pending || config.is_none(),
                cx.listener(|this, checked, window, cx| {
                    this.apply_home_capture_plan(
                        if *checked {
                            CapturePlan::Tun
                        } else {
                            CapturePlan::Off
                        },
                        window,
                        cx,
                    );
                }),
            ))
            .child(super::common::setting_switch_disabled(
                zenclash_i18n::text("core_page.switches.allow_lan"),
                zenclash_i18n::text("core_page.switches.allow_lan_description"),
                config.is_some_and(|value| value.allow_lan),
                "settings-allow-lan",
                theme,
                pending || config.is_none(),
                cx.listener(|this, checked, _, cx| {
                    this.apply_controlled_config(
                        serde_json::json!({"allow-lan": *checked}),
                        zenclash_i18n::text("core_page.notices.allow_lan"),
                        cx,
                    );
                }),
            ))
    }

    fn apply_home_capture_plan(
        &mut self,
        plan: CapturePlan,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.page, Page::Home | Page::Settings)
            || self
                .home
                .capture_transition
                .as_ref()
                .is_some_and(|transition| transition.pending)
        {
            return;
        }
        if plan == CapturePlan::Tun
            && !(self.core_session.runtime_descriptor().backend()
                == zenclash_core::CoreRuntimeBackend::Local
                && zenclash_core::current_process_elevated())
        {
            self.request_service_tun(window, cx);
            return;
        }
        let previous_transition = self.home.capture_transition.take();
        let confirmed = previous_transition
            .as_ref()
            .and_then(|transition| transition.confirmed.clone());
        self.home.capture_transition = Some(CaptureTransition {
            displayed: plan,
            pending: true,
            confirmed,
        });
        self.home.action_error = None;
        let token = self.page_task_token_for(self.page);
        let capture = self.traffic_capture.clone();
        let preferences_store = self.preferences_store.clone();
        let task = self.runtime.spawn(async move {
            let outcome = capture
                .apply(plan)
                .await
                .map_err(|error| error.to_string())?;
            let preferences = if let Some(store) = preferences_store {
                tokio::task::spawn_blocking(move || store.load())
                    .await
                    .map_err(|error| error.to_string())?
                    .map(Some)
                    .map_err(|error| error.to_string())
            } else {
                Ok(None)
            };
            Ok::<_, String>((outcome, preferences))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((CaptureOutcome::RolledBack { failure, .. }, _))
                    | Ok((CaptureOutcome::ReconcileNeeded { failure, .. }, _)) => {
                        this.home.capture_transition = previous_transition;
                        this.home.action_error = Some(failure);
                    }
                    Ok((outcome, preferences)) => {
                        match preferences {
                            Ok(Some(preferences)) => {
                                this.accept_preferences(
                                    preferences,
                                    crate::pages::runtime::PreferenceScope::SystemProxy,
                                    cx,
                                );
                            }
                            Ok(None) => {}
                            Err(error) => this.home.action_error = Some(error),
                        }
                        let snapshot = outcome.snapshot();
                        this.home.capture_transition = Some(CaptureTransition {
                            displayed: plan,
                            pending: false,
                            confirmed: Some(CaptureStatus {
                                system_proxy: snapshot.system_proxy.clone(),
                                tun: snapshot.tun.clone(),
                            }),
                        });
                        if this.is_page_task_current(token) {
                            this.refresh(cx);
                        }
                    }
                    Err(error) => {
                        this.home.capture_transition = previous_transition;
                        this.home.action_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn reconcile_home_capture_transition(&mut self, capture: &CaptureStatus) {
        let Some(transition) = self.home.capture_transition.as_ref() else {
            return;
        };
        if !transition.pending && observed_capture_plan(capture) == Some(transition.displayed) {
            self.home.capture_transition = None;
        }
    }

    pub(crate) fn begin_home_mode_transition(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        self.home.mode_transition = Some(ModeTransition { displayed, pending });
        cx.notify();
    }

    pub(crate) fn update_home_mode_transition_if_active(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.home.mode_transition else {
            return;
        };
        let confirmed = self
            .config()
            .map(|config| OutboundMode::from_api(&config.mode));
        let next = if !pending && confirmed == Some(displayed) {
            None
        } else {
            Some(ModeTransition { displayed, pending })
        };
        if next != Some(current) {
            self.home.mode_transition = next;
            cx.notify();
        }
    }
}

fn apply_home_proxy_selection(data: &mut RuntimeData, group_name: &str, proxy_name: &str) -> bool {
    let RuntimeData::Dashboard { proxies, .. } = data else {
        return false;
    };
    let catalog = match proxies {
        Observation::Fresh { value, .. } | Observation::Stale { value, .. } => value,
        Observation::Loading | Observation::Failed { .. } => return false,
    };
    catalog.set_group_selection(group_name, proxy_name.to_owned())
}

#[derive(Clone, Debug, PartialEq)]
struct TrafficChartPoint {
    label: SharedString,
    upload: f64,
    download: f64,
    ceiling: f64,
}

#[cfg(test)]
fn traffic_chart_points(samples: &VecDeque<TrafficSample>) -> Vec<TrafficChartPoint> {
    let last_ix = samples.len().saturating_sub(1);
    let ceiling = traffic_chart_ceiling(samples);
    samples
        .iter()
        .enumerate()
        .map(|(ix, sample)| {
            let remaining_seconds = last_ix.saturating_sub(ix);
            let label = if remaining_seconds == 0 {
                zenclash_i18n::text("home.traffic.now").into()
            } else {
                format!("−{remaining_seconds}s").into()
            };
            TrafficChartPoint {
                label,
                upload: chart_value(sample.upload),
                download: chart_value(sample.download),
                ceiling,
            }
        })
        .collect()
}

fn traffic_chart_ceiling(samples: &VecDeque<TrafficSample>) -> f64 {
    let peak = samples
        .iter()
        .map(|sample| sample.upload.max(sample.download))
        .max()
        .unwrap_or_default()
        .min(u64::from(u32::MAX));
    let peak_with_headroom = peak.saturating_add(peak / 10);
    let ceiling = peak_with_headroom
        .max(MIN_TRAFFIC_CHART_CEILING)
        .next_power_of_two();
    ceiling as f64
}

fn chart_value(bytes_per_second: u64) -> f64 {
    f64::from(u32::try_from(bytes_per_second).unwrap_or(u32::MAX))
}

#[derive(Clone)]
struct CurrentProxySummary {
    group: String,
    node: String,
    kind: String,
}

fn current_proxy_summary(config: &RuntimeConfig, proxies: &ProxyCatalog) -> CurrentProxySummary {
    let mode = OutboundMode::from_api(&config.mode);
    if mode == OutboundMode::Direct {
        return CurrentProxySummary {
            group: zenclash_i18n::text("outbound_mode.direct_mode"),
            node: "DIRECT".into(),
            kind: zenclash_i18n::text("home.proxy.direct_description"),
        };
    }
    let Some(group) = current_proxy_group(config, proxies) else {
        return CurrentProxySummary {
            group: zenclash_i18n::text("home.proxy.no_group"),
            node: zenclash_i18n::text("home.proxy.no_node"),
            kind: zenclash_i18n::text("home.proxy.check_configuration"),
        };
    };
    proxy_summary_for_group(group, proxies)
}

fn proxy_summary_for_group(group: &ProxyGroup, proxies: &ProxyCatalog) -> CurrentProxySummary {
    if matches!(group.behavior, ProxyGroupBehavior::LoadBalance) {
        return CurrentProxySummary {
            group: group.name.clone(),
            node: zenclash_i18n::text("home.proxy.load_balance"),
            kind: zenclash_i18n::text("home.proxy.load_balance_description"),
        };
    }
    let node = group
        .all
        .iter()
        .find(|id| id.controller_name() == group.now)
        .and_then(|id| proxies.node(id));
    let behavior = match group.behavior {
        ProxyGroupBehavior::Automatic { fixed: true } => {
            Some(zenclash_i18n::text("home.proxy.fixed"))
        }
        ProxyGroupBehavior::Automatic { fixed: false } => {
            Some(zenclash_i18n::text("home.proxy.automatic"))
        }
        _ => None,
    };
    CurrentProxySummary {
        group: group.name.clone(),
        node: group.now.clone(),
        kind: behavior
            .into_iter()
            .chain(node.map(|node| {
                let capabilities = node.capabilities().collect::<Vec<_>>().join(" · ");
                if capabilities.is_empty() {
                    node.kind.clone()
                } else {
                    format!("{} · {capabilities}", node.kind)
                }
            }))
            .collect::<Vec<_>>()
            .join(" · "),
    }
}

fn home_group_can_switch(group: &ProxyGroup) -> bool {
    matches!(
        group.behavior,
        ProxyGroupBehavior::Selector | ProxyGroupBehavior::Automatic { .. }
    )
}

fn current_proxy_group<'a>(
    config: &'a RuntimeConfig,
    proxies: &'a ProxyCatalog,
) -> Option<&'a ProxyGroup> {
    (OutboundMode::from_api(&config.mode) != OutboundMode::Direct)
        .then(|| proxies.groups_for_mode(&config.mode).next())
        .flatten()
}

fn subscription_expiry_date(usage: &SubscriptionUsage) -> Option<String> {
    let timestamp = i64::try_from(usage.expire)
        .ok()
        .filter(|value| *value > 0)?;
    let date = chrono::DateTime::from_timestamp(timestamp, 0)?;
    Some(zenclash_i18n::text_with(
        "home.profile.expiry_date",
        &[("date", date.format("%Y-%m-%d").to_string())],
    ))
}

fn subscription_expiry(usage: &SubscriptionUsage) -> String {
    remaining_days(usage.expire).map_or_else(
        || zenclash_i18n::text("home.profile.expiry_unavailable"),
        |days| {
            if days == 0 {
                zenclash_i18n::text("home.profile.expired")
            } else {
                zenclash_i18n::text_with(
                    "home.profile.remaining_days",
                    &[("days", days.to_string())],
                )
            }
        },
    )
}

fn remaining_days(expire: u64) -> Option<u64> {
    if expire == 0 {
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if expire <= now {
        Some(0)
    } else {
        Some(expire.saturating_sub(now).saturating_add(86_399) / 86_400)
    }
}

fn system_proxy_status_text(capture: &CaptureStatus) -> String {
    match &capture.system_proxy {
        Observation::Loading => zenclash_i18n::text("home.controls.system_proxy_loading"),
        Observation::Failed { .. } => zenclash_i18n::text("home.controls.system_proxy_unavailable"),
        Observation::Fresh { value, .. } => describe_system_proxy(value),
        Observation::Stale {
            value,
            observed_at_ms,
            ..
        } => zenclash_i18n::text_with(
            "home.controls.system_proxy_stale",
            &[
                ("status", describe_system_proxy(value)),
                ("age", format_profile_age(observed_at_ms / 1_000)),
            ],
        ),
    }
}

fn describe_system_proxy(snapshot: &zenclash_core::SystemProxySessionSnapshot) -> String {
    if snapshot.ownership == SystemProxyOwnershipState::Lost {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_lost");
    }
    if snapshot.intent_enabled
        && snapshot.actual.active()
        && snapshot.ownership == SystemProxyOwnershipState::Unowned
    {
        return zenclash_i18n::text("home.controls.system_proxy_ownership_unknown");
    }
    match (snapshot.intent_enabled, snapshot.actual.active()) {
        (true, true) => zenclash_i18n::text("home.controls.system_proxy_on"),
        (true, false) => zenclash_i18n::text("home.controls.system_proxy_intent_only"),
        (false, true) => zenclash_i18n::text("home.controls.system_proxy_external"),
        (false, false) => zenclash_i18n::text("home.controls.system_proxy_off"),
    }
}

fn tun_status_text(capture: &CaptureStatus) -> String {
    match &capture.tun {
        Observation::Loading => zenclash_i18n::text("home.controls.tun_loading"),
        Observation::Failed { .. } => zenclash_i18n::text("home.controls.tun_unavailable"),
        Observation::Fresh { value, .. } | Observation::Stale { value, .. } => describe_tun(value),
    }
}

fn describe_tun(snapshot: &zenclash_core::TunCaptureStatus) -> String {
    match snapshot.observed {
        CapabilityState::Active => zenclash_i18n::text("home.controls.tun_on"),
        CapabilityState::Unsupported => zenclash_i18n::text("home.controls.tun_unsupported"),
        CapabilityState::Unknown if snapshot.requested || snapshot.configured => {
            zenclash_i18n::text("home.controls.tun_unverified")
        }
        CapabilityState::Inactive if snapshot.requested || snapshot.configured => {
            zenclash_i18n::text("home.controls.tun_inactive")
        }
        CapabilityState::Inactive | CapabilityState::Unknown => {
            zenclash_i18n::text("home.controls.tun_off")
        }
    }
}

fn capture_status_text(capture: &CaptureStatus) -> String {
    if capture.is_active() {
        return zenclash_i18n::text("home.controls.capture_active");
    }
    if capture
        .tun
        .value()
        .is_some_and(|tun| tun.configured && matches!(tun.observed, CapabilityState::Unknown))
    {
        return zenclash_i18n::text("home.controls.capture_unverified");
    }
    if matches!(
        &capture.system_proxy,
        Observation::Failed { .. } | Observation::Loading
    ) || matches!(
        &capture.tun,
        Observation::Failed { .. } | Observation::Loading
    ) {
        return zenclash_i18n::text("home.controls.capture_unknown");
    }
    zenclash_i18n::text("home.controls.capture_off")
}

fn process_evidence(
    process: Option<&ProcessStatus>,
    theme: &gpui_kit::component::Theme,
) -> (String, gpui_kit::Hsla) {
    let (key, attempts) = process_evidence_copy(process);
    let text = attempts.map_or_else(
        || zenclash_i18n::text(key),
        |attempt| zenclash_i18n::text_with(key, &[("attempt", attempt.to_string())]),
    );
    let color = match process.map(|process| process.recovery) {
        Some(ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External)
            if process.is_some_and(|process| process.running) =>
        {
            theme.chart_3
        }
        Some(
            ProcessRecoveryStatus::Recovering
            | ProcessRecoveryStatus::NetworkSuspended
            | ProcessRecoveryStatus::Unknown,
        ) => theme.warning,
        Some(
            ProcessRecoveryStatus::Stable
            | ProcessRecoveryStatus::Failed
            | ProcessRecoveryStatus::Stopped
            | ProcessRecoveryStatus::External,
        )
        | None => theme.danger,
    };
    (text, color)
}

fn process_evidence_copy(process: Option<&ProcessStatus>) -> (&'static str, Option<u32>) {
    let Some(process) = process else {
        return ("home.evidence.core_unavailable", None);
    };
    match process.recovery {
        ProcessRecoveryStatus::Recovering => (
            "home.evidence.core_recovering",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Failed => (
            "home.evidence.core_recovery_failed",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stopped => ("home.evidence.core_stopped", None),
        ProcessRecoveryStatus::Unknown => ("core_page.status.unreadable", None),
        ProcessRecoveryStatus::NetworkSuspended => ("automatic.network_stopped", None),
        ProcessRecoveryStatus::Stable if process.running && process.recovery_attempts > 0 => (
            "home.evidence.core_recovered",
            Some(process.recovery_attempts),
        ),
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External if process.running => {
            ("home.evidence.core_running", None)
        }
        ProcessRecoveryStatus::Stable | ProcessRecoveryStatus::External => {
            ("home.evidence.core_unavailable", None)
        }
    }
}

fn stream_status_text(
    observation: &Observation<StreamStatus>,
    theme: &gpui_kit::component::Theme,
) -> (String, gpui_kit::Hsla) {
    match observation {
        Observation::Fresh { .. } => (zenclash_i18n::text("home.traffic.live"), theme.success),
        Observation::Stale { observed_at_ms, .. } => (
            zenclash_i18n::text_with(
                "home.traffic.stale",
                &[("age", format_profile_age(observed_at_ms / 1_000))],
            ),
            theme.warning,
        ),
        Observation::Failed { .. } => (
            zenclash_i18n::text("home.traffic.unavailable"),
            theme.danger,
        ),
        Observation::Loading => (
            zenclash_i18n::text("home.traffic.reconnecting"),
            theme.warning,
        ),
    }
}

fn home_service_feedback(
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

fn mode_presentation(
    confirmed: OutboundMode,
    transition: Option<ModeTransition>,
) -> ModeTransition {
    transition.unwrap_or(ModeTransition {
        displayed: confirmed,
        pending: false,
    })
}

fn capture_presentation(
    confirmed_system_proxy: bool,
    confirmed_tun: bool,
    transition: Option<&CaptureTransition>,
) -> CapturePresentation {
    let Some(transition) = transition else {
        return CapturePresentation {
            system_proxy_enabled: confirmed_system_proxy,
            tun_enabled: confirmed_tun,
            pending: false,
        };
    };
    let (system_proxy_enabled, tun_enabled) = match transition.displayed {
        CapturePlan::Off => (false, false),
        CapturePlan::SystemProxy => (true, false),
        CapturePlan::Tun => (false, true),
    };
    CapturePresentation {
        system_proxy_enabled,
        tun_enabled,
        pending: transition.pending,
    }
}

fn capture_transition_status_text(
    confirmed: bool,
    displayed: bool,
    pending: bool,
    enabling_key: &'static str,
    disabling_key: &'static str,
) -> Option<String> {
    (pending && confirmed != displayed).then(|| {
        zenclash_i18n::text(if displayed {
            enabling_key
        } else {
            disabling_key
        })
    })
}

fn observed_capture_plan(capture: &CaptureStatus) -> Option<CapturePlan> {
    let system_proxy_enabled = capture.system_proxy.value()?.intent_enabled;
    let tun = capture.tun.value()?;
    let tun_enabled = tun.requested || tun.configured;
    match (system_proxy_enabled, tun_enabled) {
        (false, false) => Some(CapturePlan::Off),
        (true, false) => Some(CapturePlan::SystemProxy),
        (false, true) => Some(CapturePlan::Tun),
        (true, true) => None,
    }
}

fn status_label(
    label: impl Into<SharedString>,
    color: gpui_kit::Hsla,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    let label = label.into();
    h_flex()
        .gap_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().size_2().rounded_full().bg(color))
        .child(label)
        .into_any_element()
}

fn series_label(
    id: &'static str,
    icon: IconName,
    label: impl Into<SharedString>,
    value: String,
    color: gpui_kit::Hsla,
    _theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    h_flex()
        .id(id)
        .gap_1()
        .text_sm()
        .text_color(color)
        .child(Icon::new(icon).size_4())
        .child(label.into())
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(value.clone()).build(window, cx)
        })
        .into_any_element()
}

fn core_memory_bytes(streams: &StreamStatuses, generation: u64) -> Option<u64> {
    streams
        .memory
        .is_fresh()
        .then(|| streams.memory.value())
        .flatten()
        .filter(|value| value.generation == generation && value.memory > 0)
        .map(|value| value.memory)
}

fn format_core_memory(bytes: u64) -> String {
    let (unit, label) = if bytes >= 1_000_000_000 {
        (1_000_000_000, "GB")
    } else {
        (1_000_000, "MB")
    };
    format!("{}.{:01} {label}", bytes / unit, (bytes % unit) * 10 / unit)
}

#[cfg(test)]
mod tests {
    use zenclash_core::{
        DelayHistory, ProxyGroup, ProxyGroupBehavior, ProxyNode, SystemProxySessionSnapshot,
        SystemProxyStatus, TunCaptureStatus, TunRuntimeObservation,
    };

    use super::*;

    #[test]
    fn core_memory_uses_mb_minimum_and_requires_current_fresh_data() {
        assert_eq!(format_core_memory(1024), "0.0 MB");
        assert_eq!(format_core_memory(1_500_000), "1.5 MB");
        assert_eq!(format_core_memory(1_500_000_000), "1.5 GB");
        let mut streams = StreamStatuses::default();
        assert_eq!(core_memory_bytes(&streams, 2), None);
        streams.memory = Observation::Fresh {
            value: StreamStatus {
                generation: 2,
                memory: 47_000_000,
                ..Default::default()
            },
            observed_at_ms: 1,
        };
        assert_eq!(core_memory_bytes(&streams, 2), Some(47_000_000));
        assert_eq!(core_memory_bytes(&streams, 3), None);
        streams.memory = Observation::Stale {
            value: *streams.memory.value().unwrap(),
            observed_at_ms: 1,
            failure: zenclash_core::OperationalFailure {
                message: "read failed".into(),
                occurred_at_ms: 2,
            },
        };
        assert_eq!(core_memory_bytes(&streams, 2), None);
        streams.memory = Observation::Loading;
        assert_eq!(core_memory_bytes(&streams, 2), None);
        streams.memory = Observation::Fresh {
            value: StreamStatus {
                generation: 2,
                memory: 0,
                ..Default::default()
            },
            observed_at_ms: 1,
        };
        assert_eq!(core_memory_bytes(&streams, 2), None);
    }

    struct ServiceFeedbackView {
        phase: Option<zenclash_core::ServicePhase>,
        warning: Option<String>,
    }

    impl gpui_kit::Render for ServiceFeedbackView {
        fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::ActiveTheme;
            home_service_feedback(self.phase, false, self.warning.clone(), cx.theme())
                .unwrap_or_else(div)
        }
    }

    #[gpui_kit::test]
    fn home_service_feedback_native_uncertainty_and_independent_warning_remain_visible(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        cx.update(gpui_kit::init);
        let mut view = None;
        let window = cx.open_window(gpui_kit::size(px(900.), px(500.)), |window, cx| {
            let entity = cx.new(|_| ServiceFeedbackView {
                phase: Some(zenclash_core::ServicePhase::Unconfirmed),
                warning: None,
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("home-service-pending").label(),
                Some(zenclash_i18n::text("core_page.service.pending").as_str())
            );
            let warning = zenclash_i18n::text("core_page.service.failed");
            view.update(cx, |view, cx| {
                view.phase = Some(zenclash_core::ServicePhase::Completed);
                view.warning = Some(warning.clone());
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("home-service-pending").is_none());
            assert_eq!(
                window.find("home-service-warning").label(),
                Some(warning.as_str())
            );
            window.render_frame(cx);
            assert_eq!(
                window.find("home-service-warning").label(),
                Some(warning.as_str())
            );
            window.remove_window();
        })
        .unwrap();
    }

    #[test]
    fn process_recovery_attempts_have_a_visible_non_color_label() {
        let process = ProcessStatus {
            kind: zenclash_core::CoreKind::Mihomo,
            managed: true,
            pid: None,
            started_at_secs: None,
            running: false,
            generation: 3,
            exit_reason: Some("exit status: 23".into()),
            recovery_attempts: 2,
            recovery: ProcessRecoveryStatus::Recovering,
        };

        assert_eq!(
            process_evidence_copy(Some(&process)),
            ("home.evidence.core_recovering", Some(2))
        );
        assert_eq!(
            process_evidence_copy(Some(&ProcessStatus {
                recovery: ProcessRecoveryStatus::Failed,
                ..process.clone()
            })),
            ("home.evidence.core_recovery_failed", Some(2))
        );
        assert_eq!(
            process_evidence_copy(Some(&ProcessStatus {
                running: true,
                recovery: ProcessRecoveryStatus::Stable,
                ..process
            })),
            ("home.evidence.core_recovered", Some(2))
        );
    }

    #[test]
    fn direct_mode_summary_does_not_require_a_proxy_group() {
        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "direct".into(),
                ..RuntimeConfig::default()
            },
            &ProxyCatalog::default(),
        );

        assert_eq!(summary.node, "DIRECT");
    }

    #[test]
    fn rule_mode_summary_uses_the_primary_group_current_node() {
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                ProxyGroup {
                    name: "Proxy".into(),
                    now: "HK 01".into(),
                    ..ProxyGroup::default()
                },
                vec![ProxyNode {
                    name: "HK 01".into(),
                    kind: "Hysteria2".into(),
                    history: vec![DelayHistory {
                        delay: 42,
                        ..DelayHistory::default()
                    }],
                    ..ProxyNode::default()
                }],
            )],
            1,
        );

        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );
        assert_eq!(summary.group, "Proxy");
        assert_eq!(summary.node, "HK 01");
    }

    #[test]
    fn acknowledged_home_proxy_selection_updates_the_local_group_immediately() {
        let mut data = RuntimeData::Dashboard {
            config: Observation::Loading,
            proxies: Observation::Fresh {
                value: ProxyCatalog::from_group_nodes(
                    vec![(
                        ProxyGroup {
                            name: "Proxy".into(),
                            now: "HK 01".into(),
                            behavior: ProxyGroupBehavior::Automatic { fixed: false },
                            ..ProxyGroup::default()
                        },
                        Vec::new(),
                    )],
                    1,
                ),
                observed_at_ms: 10,
            },
        };

        assert!(apply_home_proxy_selection(&mut data, "Proxy", "US 02"));
        let RuntimeData::Dashboard { proxies, .. } = data else {
            panic!("expected dashboard data");
        };
        let group = &proxies.value().expect("fresh proxy catalog").groups()[0];
        assert_eq!(group.now, "US 02");
        assert_eq!(
            group.behavior,
            ProxyGroupBehavior::Automatic { fixed: true }
        );
    }

    #[test]
    fn load_balance_summary_does_not_offer_a_fake_manual_selection() {
        let group = ProxyGroup {
            name: "Balance".into(),
            behavior: ProxyGroupBehavior::LoadBalance,
            now: "HK 01".into(),
            ..ProxyGroup::default()
        };
        let catalog = ProxyCatalog::from_group_nodes(
            vec![(
                group.clone(),
                vec![ProxyNode {
                    name: "HK 01".into(),
                    ..Default::default()
                }],
            )],
            2,
        );

        let summary = current_proxy_summary(
            &RuntimeConfig {
                mode: "rule".into(),
                ..RuntimeConfig::default()
            },
            &catalog,
        );

        assert!(!home_group_can_switch(&group));
        assert_eq!(summary.node, zenclash_i18n::text("home.proxy.load_balance"));
    }

    #[test]
    fn system_proxy_intent_actual_and_lost_ownership_have_distinct_copy() {
        let intent_only = SystemProxySessionSnapshot {
            intent_enabled: true,
            actual: SystemProxyStatus::default(),
            ownership: SystemProxyOwnershipState::Unowned,
        };
        let external = SystemProxySessionSnapshot {
            intent_enabled: false,
            actual: SystemProxyStatus {
                enabled: true,
                ..SystemProxyStatus::default()
            },
            ownership: SystemProxyOwnershipState::Unowned,
        };
        let lost = SystemProxySessionSnapshot {
            intent_enabled: true,
            actual: external.actual.clone(),
            ownership: SystemProxyOwnershipState::Lost,
        };

        assert_eq!(
            describe_system_proxy(&intent_only),
            zenclash_i18n::text("home.controls.system_proxy_intent_only")
        );
        assert_eq!(
            describe_system_proxy(&external),
            zenclash_i18n::text("home.controls.system_proxy_external")
        );
        assert_eq!(
            describe_system_proxy(&lost),
            zenclash_i18n::text("home.controls.system_proxy_ownership_lost")
        );
    }

    #[test]
    fn tun_status_copy_distinguishes_off_unverified_active_and_unsupported() {
        let mut status = TunCaptureStatus {
            requested: false,
            configured: false,
            permission: CapabilityState::Inactive,
            runtime: TunRuntimeObservation {
                device_name: None,
                device: CapabilityState::Inactive,
                route: CapabilityState::Inactive,
                detail: String::new(),
            },
            observed: CapabilityState::Inactive,
        };

        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_off")
        );

        status.requested = true;
        status.configured = true;
        status.observed = CapabilityState::Unknown;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_unverified")
        );

        status.observed = CapabilityState::Active;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_on")
        );

        status.observed = CapabilityState::Unsupported;
        assert_eq!(
            describe_tun(&status),
            zenclash_i18n::text("home.controls.tun_unsupported")
        );
    }

    #[test]
    fn traffic_chart_points_keep_upload_and_download_separate() {
        let samples = VecDeque::from([
            TrafficSample {
                upload: 10,
                download: 20,
            },
            TrafficSample {
                upload: 30,
                download: 40,
            },
        ]);

        let points = traffic_chart_points(&samples);

        assert_eq!((points[1].upload, points[1].download), (30., 40.));
    }

    #[test]
    fn traffic_chart_points_have_unique_x_axis_labels() {
        let samples = VecDeque::from([TrafficSample::default(); 24]);

        let points = traffic_chart_points(&samples);

        assert!(points.windows(2).all(|pair| pair[0].label != pair[1].label));
    }

    #[test]
    fn traffic_chart_ceiling_is_stable_within_one_power_of_two_band() {
        let lower = VecDeque::from([TrafficSample {
            download: 700 * 1_024,
            ..TrafficSample::default()
        }]);
        let higher = VecDeque::from([TrafficSample {
            download: 800 * 1_024,
            ..TrafficSample::default()
        }]);

        assert_eq!(
            traffic_chart_ceiling(&lower),
            traffic_chart_ceiling(&higher)
        );
    }

    #[test]
    fn traffic_chart_last_point_is_labeled_as_now() {
        let samples = VecDeque::from([TrafficSample::default(); 3]);

        let points = traffic_chart_points(&samples);

        assert!(matches!(points[2].label.as_ref(), "现在" | "Now"));
    }

    #[test]
    fn pending_mode_transition_is_presented_before_controller_readback() {
        let presentation = mode_presentation(
            OutboundMode::Rule,
            Some(ModeTransition {
                displayed: OutboundMode::Global,
                pending: true,
            }),
        );

        assert_eq!(
            (presentation.displayed, presentation.pending),
            (OutboundMode::Global, true)
        );
    }

    #[test]
    fn pending_system_proxy_transition_is_presented_before_platform_readback() {
        let transition = CaptureTransition {
            displayed: CapturePlan::SystemProxy,
            pending: true,
            confirmed: None,
        };

        let presentation = capture_presentation(false, false, Some(&transition));

        assert_eq!(
            presentation,
            CapturePresentation {
                system_proxy_enabled: true,
                tun_enabled: false,
                pending: true,
            }
        );
    }

    #[test]
    fn pending_tun_transition_hides_stale_system_proxy_state() {
        let transition = CaptureTransition {
            displayed: CapturePlan::Tun,
            pending: true,
            confirmed: None,
        };

        let presentation = capture_presentation(true, false, Some(&transition));

        assert_eq!(
            presentation,
            CapturePresentation {
                system_proxy_enabled: false,
                tun_enabled: true,
                pending: true,
            }
        );
    }

    #[test]
    fn pending_off_transition_clears_system_proxy_before_platform_readback() {
        let transition = CaptureTransition {
            displayed: CapturePlan::Off,
            pending: true,
            confirmed: None,
        };

        let presentation = capture_presentation(true, false, Some(&transition));

        assert_eq!(
            presentation,
            CapturePresentation {
                system_proxy_enabled: false,
                tun_enabled: false,
                pending: true,
            }
        );
    }

    #[test]
    fn completed_transition_stays_visible_until_global_observation_catches_up() {
        let transition = CaptureTransition {
            displayed: CapturePlan::SystemProxy,
            pending: false,
            confirmed: None,
        };

        let presentation = capture_presentation(false, false, Some(&transition));

        assert_eq!(
            presentation,
            CapturePresentation {
                system_proxy_enabled: true,
                tun_enabled: false,
                pending: false,
            }
        );
    }
}
