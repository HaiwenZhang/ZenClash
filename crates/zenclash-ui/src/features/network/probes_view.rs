use gpui_kit::component::Selectable;
use serde_json::json;

use super::model::{
    format_asn, format_coordinates, format_proxy_flags, join_present, public_ip_checked_at,
};
use super::*;

impl RuntimePage {
    pub(super) fn render_public_ip_card(
        &self,
        snapshot: &NetworkProbeSnapshot,
        expanded: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let provider = self.preferences.network_ip_provider;
        let info = snapshot.public_ip.as_ref();
        let ip_version = info
            .and_then(|info| info.ip.parse::<std::net::IpAddr>().ok())
            .map(|ip| if ip.is_ipv4() { "IPv4" } else { "IPv6" });
        let checked_at = public_ip_checked_at(snapshot, self.network_diagnostics.query.report())
            .and_then(|timestamp| i64::try_from(timestamp).ok())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            });
        network_card(zenclash_i18n::text("redesign.exit_info"), theme)
            .when(!expanded, |view| {
                view.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_2()
                        .child(
                            Button::new("refresh-network-probe")
                                .small()
                                .outline()
                                .label(zenclash_i18n::text("network.public_ip.refresh"))
                                .disabled(
                                    self.network_diagnostics.query.is_loading() || self.core_busy(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_network_diagnostics(cx)
                                })),
                        )
                        .child(
                            Button::new("network-options")
                                .small()
                                .ghost()
                                .label(zenclash_i18n::text("redesign.details"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_network_options(window, cx)
                                })),
                        ),
                )
            })
            .when(expanded, |card| {
                card.child(
                    h_flex()
                        .min_h(gpui_kit::rems(3.625))
                        .px_4()
                        .gap_3()
                        .justify_between()
                        .child(h_flex().gap_2().children(
                            PublicIpProvider::ALL.into_iter().enumerate().map(
                                |(index, candidate)| {
                                    Button::new(("network-provider", index))
                                        .label(candidate.label())
                                        .small()
                                        .h_10()
                                        .outline()
                                        .selected(candidate == provider)
                                        .disabled(
                                            self.mutation_busy(
                                                crate::runtime_host::busy::MutationDomain::Network,
                                            ) || self.network_diagnostics.query.is_loading(),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.persist_network_preference(
                                                NetworkPreferenceChange::Provider(candidate),
                                                zenclash_i18n::text("network.notices.provider"),
                                                cx,
                                            );
                                        }))
                                },
                            ),
                        ))
                        .child(
                            Button::new("dialog-refresh-network-probe")
                                .icon(crate::foundation::assets::AppIcon::RefreshCw)
                                .label(if self.network_diagnostics.query.is_loading() {
                                    zenclash_i18n::text("network.public_ip.probing")
                                } else {
                                    zenclash_i18n::text("network.public_ip.refresh")
                                })
                                .small()
                                .h_10()
                                .primary()
                                .disabled(
                                    self.network_diagnostics.query.is_loading() || self.core_busy(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh_network_diagnostics(cx);
                                })),
                        ),
                )
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .child(
                        div()
                            .w(gpui_kit::rems(7.))
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(zenclash_i18n::text("network.public_ip.ip")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(model::public_exit_label(
                                self.network_diagnostics.query.snapshot(),
                                self.network_diagnostics.query.is_loading(),
                            )),
                    )
                    .when_some(ip_version.filter(|_| expanded), |row, version| {
                        row.child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.secondary)
                                .text_sm()
                                .child(version),
                        )
                    }),
            )
            .child(info_row(
                zenclash_i18n::text("network.metrics.route"),
                empty_dash(&snapshot.route),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.public_ip.country_region"),
                info.map_or_else(
                    || "—".into(),
                    |info| {
                        join_present(&[
                            info.country.as_deref(),
                            info.region.as_deref(),
                            info.city.as_deref(),
                        ])
                    },
                ),
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.public_ip.organization"),
                info.map_or_else(
                    || "—".into(),
                    |info| format_asn(info.asn, info.organization.as_deref()),
                ),
                theme,
            ))
            .when(expanded, |view| {
                view.child(info_row(
                    zenclash_i18n::text("network.metrics.average_latency"),
                    model::average_latency(snapshot)
                        .map_or_else(|| "—".into(), |value| format!("{value} ms")),
                    theme,
                ))
            })
            .child(info_row(
                zenclash_i18n::text("network.public_ip.updated_at"),
                checked_at.unwrap_or_else(|| "—".into()),
                theme,
            ))
            .when(expanded, |this| {
                this.when_some(info, |this, info| {
                    this.child(info_row("ISP", info.isp.as_deref().unwrap_or(""), theme))
                        .child(info_row(
                            zenclash_i18n::text("network.public_ip.timezone"),
                            info.timezone.as_deref().unwrap_or(""),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("network.public_ip.coordinates"),
                            format_coordinates(info.latitude, info.longitude),
                            theme,
                        ))
                        .child(info_row(
                            zenclash_i18n::text("network.public_ip.proxy_detection"),
                            format_proxy_flags(info.is_proxy, info.is_vpn),
                            theme,
                        ))
                })
            })
    }

    pub(super) fn render_system_network_card(
        &self,
        config: &RuntimeConfig,
        system: &SystemNetworkSnapshot,
        expanded: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let capture = self.network_diagnostics.query.report().and_then(|report| {
            match report
                .step(DiagnosticStepKind::Capture)?
                .outcome
                .as_ref()
                .ok()?
            {
                DiagnosticData::Capture(capture) => Some(capture),
                _ => None,
            }
        });
        let system_proxy = capture
            .and_then(|capture| capture.system_proxy.value())
            .map(|proxy| proxy.actual.active());
        let tun = capture
            .and_then(|capture| capture.tun.value())
            .and_then(|tun| match tun.observed {
                zenclash_core::CapabilityState::Active => Some(true),
                zenclash_core::CapabilityState::Inactive => Some(false),
                _ => None,
            });
        network_card(zenclash_i18n::text("redesign.local_network"), theme)
            .child(network_capability_row(
                "IPv6",
                matches!(
                    self.page_observation.data(),
                    PageSnapshotData::Network { .. }
                )
                .then_some(config.ipv6),
                theme,
            ))
            .child(network_capability_row(
                zenclash_i18n::text("tray.system_proxy"),
                system_proxy,
                theme,
            ))
            .child(network_capability_row(
                zenclash_i18n::text("home.controls.tun"),
                tun,
                theme,
            ))
            .child(info_row("DNS", system.dns_servers.join(", "), theme))
            .child(info_row(
                zenclash_i18n::text("network.system.interface"),
                &system.interface,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.system.gateway"),
                &system.gateway,
                theme,
            ))
            .child(info_row(
                zenclash_i18n::text("network.system.local_address"),
                &system.local_ipv4,
                theme,
            ))
            .when(expanded, |card| {
                card.child(
                    h_flex()
                        .min_h(gpui_kit::rems(3.625))
                        .px_4()
                        .gap_3()
                        .justify_between()
                        .flex_wrap()
                        .py_2()
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            zenclash_i18n::text_with(
                                "network.system.pinned",
                                &[("interface", empty_dash(&config.interface_name))],
                            ),
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("use-system-interface")
                                        .icon(IconName::Check)
                                        .label(zenclash_i18n::text("network.system.pin"))
                                        .small()
                                        .h_10()
                                        .primary()
                                        .disabled(system.interface.is_empty() || self.core_busy())
                                        .on_click({
                                            let interface = system.interface.clone();
                                            cx.listener(move |this, _, _, cx| {
                                                this.apply_controlled_config(
                                                    json!({"interface-name": interface}),
                                                    zenclash_i18n::text(
                                                        "network.notices.interface_pinned",
                                                    ),
                                                    cx,
                                                );
                                            })
                                        }),
                                )
                                .child(
                                    Button::new("clear-system-interface")
                                        .icon(crate::foundation::assets::AppIcon::RefreshCw)
                                        .label(zenclash_i18n::text("network.system.automatic"))
                                        .small()
                                        .h_10()
                                        .outline()
                                        .disabled(
                                            config.interface_name.is_empty() || self.core_busy(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.apply_controlled_config(
                                                json!({"interface-name": ""}),
                                                zenclash_i18n::text(
                                                    "network.notices.interface_auto",
                                                ),
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                )
            })
            .child(
                h_flex().px_4().py_3().gap_2().flex_wrap().children(
                    [
                        (
                            DnsCacheAction::Dns,
                            if expanded {
                                "dialog-request-dns-cache-flush"
                            } else {
                                "request-dns-cache-flush"
                            },
                            "network.diagnostics.flush_dns",
                        ),
                        (
                            DnsCacheAction::FakeIp,
                            if expanded {
                                "dialog-request-fake-ip-cache-flush"
                            } else {
                                "request-fake-ip-cache-flush"
                            },
                            "network.diagnostics.flush_fake_ip",
                        ),
                    ]
                    .into_iter()
                    .map(|(action, id, label)| {
                        Button::new(id)
                            .small()
                            .outline()
                            .label(zenclash_i18n::text(label))
                            .disabled(self.core_busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_network_cache_confirmation(action, window, cx)
                            }))
                    }),
                ),
            )
    }
}
