use crate::outbound_mode::OutboundMode;
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
    ProxyGroupBehavior, RuntimeConfig, StreamStatus, StreamStatuses, SubscriptionUsage,
    SystemProxyOwnershipState, TrafficSample, format_speed,
};

use crate::{
    components::{mint_switch::MintSwitch as Switch, wave_percentage::WavePercentage},
    foundation::actions::{
        NavigateProfiles, NavigateSystemProxy, NavigateTun, SetDirectMode, SetGlobalMode,
        SetRuleMode,
    },
};

use crate::components::page_primitives::context_note;
use crate::components::page_primitives::format_bytes;
use crate::components::page_primitives::format_profile_age;
use crate::components::page_primitives::{normalized_fraction, tun_status_text};
use crate::features::host::RuntimePage;
use crate::foundation::actions::Page;
use crate::runtime_host::observation::PageSnapshotData;
use crate::runtime_host::services::ProxySelectionChanged;
use gpui_kit::Context;
use gpui_kit::prelude::FluentBuilder;

mod cards;
mod controls;
mod dashboard;
mod menu_size;
use menu_size::HomeMenuSize;
mod proxy_selection;
mod selector;
mod status;
mod traffic;
mod transitions;
#[cfg(test)]
mod validation;

const MIN_TRAFFIC_CHART_CEILING: u64 = 1_024;

#[derive(Default)]
pub(in crate::features) struct HomeUiState {
    pub(in crate::features) profile_switching: Option<String>,
    pub(in crate::features) proxy_selection:
        crate::foundation::primitives::proxy_selection::ProxySelectionUiState,
    pub(in crate::features) proxy_error: Option<String>,
    capture_transition: Option<transitions::CaptureTransition>,
    pub(in crate::features) action_error: Option<String>,
    mode_transition: Option<transitions::ModeTransition>,
    projection: Option<selector::HomeProxyProjection>,
    chart: traffic::HomeChartState,
    generation: u64,
    selected_group: Option<String>,
    selector_width_rems: f32,
    #[cfg(test)]
    design_validation: Option<(OperationalSnapshot, zenclash_core::TrafficSnapshot)>,
}

impl HomeUiState {
    pub(in crate::features) fn begin_profile_operation(&mut self, marker: String) {
        self.profile_switching = Some(marker);
        self.action_error = None;
    }

    pub(in crate::features) fn finish_profile_operation(&mut self) {
        self.profile_switching = None;
    }

    pub(in crate::features) fn show_profile_error(&mut self, error: String) {
        self.action_error = Some(error);
    }
}

impl RuntimePage {
    fn home_operational_snapshot(&self) -> OperationalSnapshot {
        #[cfg(test)]
        if let Some((snapshot, _)) = &self.home.design_validation {
            return snapshot.clone();
        }
        self.monitors.operational_status.snapshot()
    }

    fn home_traffic_snapshot(&self) -> zenclash_core::TrafficSnapshot {
        #[cfg(test)]
        if let Some((_, snapshot)) = &self.home.design_validation {
            return snapshot.clone();
        }
        self.monitors.traffic_monitor.snapshot()
    }

    pub(in crate::features) fn release_home_presentation(&mut self) {
        #[cfg(test)]
        {
            self.home.design_validation = None;
        }
        // Stream history is bounded and survives page/window presentation changes.
        self.home.projection = None;
        self.home.selected_group = None;
    }

    pub(in crate::features) fn render_home(
        &self,
        theme: &gpui_kit::component::Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let fallback_config = RuntimeConfig::default();
        let config = match self.page_observation.data() {
            PageSnapshotData::Dashboard { config, .. } => {
                config.value().unwrap_or(&fallback_config)
            }
            _ => &fallback_config,
        };
        let operational = self.home_operational_snapshot();

        v_flex()
            .gap_3()
            .child(self.render_home_controls(config, &operational.capture, theme, compact, cx))
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
        let (process_text, process_color) = status::process_evidence(process, theme);
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
                        status::capture_status_text(&operational.capture),
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
}

#[derive(Clone, Debug, PartialEq)]
struct TrafficChartPoint {
    label: SharedString,
    upload: f64,
    download: f64,
    ceiling: f64,
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
mod tests;

#[cfg(test)]
mod ui_tests;
