use super::*;
use gpui_kit::component::{ActiveTheme, WindowExt};

impl RuntimePage {
    pub(super) fn render_diagnostic_step(
        &self,
        step: &DiagnosticStep,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let kind = step.kind;
        let succeeded = step.outcome.is_ok();
        h_flex()
            .px_3()
            .py_2()
            .gap_3()
            .min_h(gpui_kit::rems(2.75))
            .border_b_1()
            .border_color(theme.border)
            .when(!succeeded, |row| row.bg(theme.warning.opacity(0.08)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(diagnostic_step_label(kind)),
            )
            .child(
                h_flex()
                    .w_24()
                    .gap_2()
                    .id(format!("diagnostic-status:{kind:?}"))
                    .test_support()
                    .child(network_status_icon(succeeded, theme))
                    .child(div().text_sm().child(zenclash_i18n::text(if succeeded {
                        "network.latency.succeeded"
                    } else {
                        "network.latency.failed"
                    }))),
            )
            .child(
                div()
                    .w_20()
                    .text_sm()
                    .child(format!("{} ms", step.duration_ms)),
            )
            .when(!succeeded, |row| {
                row.child(
                    Button::new(format!("retry-diagnostic:{kind:?}"))
                        .small()
                        .outline()
                        .label(zenclash_i18n::text("common.actions.retry"))
                        .tooltip(zenclash_i18n::text("redesign.recheck_all"))
                        .disabled(self.network_probe.loading || self.core_busy())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_network_probe(cx))),
                )
            })
            .child(
                Button::new(format!("diagnostic-details:{kind:?}"))
                    .small()
                    .outline()
                    .label(zenclash_i18n::text("redesign.details"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_diagnostic_details(kind, window, cx)
                    })),
            )
            .into_any_element()
    }

    fn open_diagnostic_details(
        &mut self,
        kind: DiagnosticStepKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| page.diagnostic_details_content(kind, cx))
                .ok();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.diagnostic_details"))
                .width(window.rem_size() * 34.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 28.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .when_some(content, |dialog, content| dialog.child(content))
        });
    }

    fn diagnostic_details_content(
        &self,
        kind: DiagnosticStepKind,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let Some(step) = self
            .network_probe
            .report
            .as_ref()
            .and_then(|report| report.step(kind))
        else {
            return div()
                .child(zenclash_i18n::text("runtime.empty.unavailable"))
                .into_any_element();
        };
        let content = step
            .outcome
            .as_ref()
            .map(diagnostic_data_summary)
            .unwrap_or_else(|error| error.message.clone());
        let copy = format!(
            "{}\n{}\n{} ms\n{}",
            diagnostic_step_label(kind),
            diagnostic_route_label(step.route),
            step.duration_ms,
            content
        );
        v_flex()
            .gap_3()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(diagnostic_step_label(kind)),
            )
            .when(
                matches!(kind, DiagnosticStepKind::DnsA | DiagnosticStepKind::DnsAaaa),
                |view| {
                    view.child(info_row(
                        zenclash_i18n::text("network.diagnostics.dns_name"),
                        self.network_probe.dns_name.read(cx).value(),
                        &theme,
                    ))
                },
            )
            .child(info_row(
                zenclash_i18n::text("network.metrics.route"),
                diagnostic_route_label(step.route),
                &theme,
            ))
            .child(info_row(
                zenclash_i18n::text("redesign.duration"),
                format!("{} ms", step.duration_ms),
                &theme,
            ))
            .child(
                div()
                    .p_3()
                    .text_sm()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .child(content),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("copy-diagnostic-result")
                            .outline()
                            .label(zenclash_i18n::text("redesign.copy_content"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
                                    copy.clone(),
                                ))
                            }),
                    )
                    .child(
                        Button::new("retry-diagnostic-result")
                            .primary()
                            .label(zenclash_i18n::text("network.latency.retest"))
                            .disabled(self.network_probe.loading || self.core_busy())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.refresh_network_probe(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn open_network_options(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| {
                    let theme = cx.theme().clone();
                    let snapshot = page.network_probe.snapshot.clone().unwrap_or_default();
                    let (config, system) = match &page.data {
                        RuntimeData::Network { config, system } => (config.clone(), system.clone()),
                        _ => (RuntimeConfig::default(), SystemNetworkSnapshot::default()),
                    };
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .child(zenclash_i18n::text("network.diagnostics.dns_name")),
                        )
                        .child(Input::new(&page.network_probe.dns_name))
                        .child(page.render_public_ip_card(&snapshot, true, &theme, cx))
                        .child(page.render_system_network_card(&config, &system, true, &theme, cx))
                        .into_any_element()
                })
                .ok();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.network_options"))
                .width(window.rem_size() * 40.)
                .margin_top(window.rem_size() * 2.)
                .when_some(content, |dialog, content| dialog.child(content))
        });
    }

    pub(super) fn open_latency_details(
        &mut self,
        result: zenclash_core::NetworkLatencyResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        let route = self
            .network_probe
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.route.clone())
            .unwrap_or_default();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = result
                .error
                .clone()
                .unwrap_or_else(|| format!("{} ms", result.latency_ms.unwrap_or_default()));
            let copy = format!(
                "{}\n{}\n{}\n{}",
                result.target.name, result.target.url, route, content
            );
            let owner = owner.clone();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.diagnostic_details"))
                .width(window.rem_size() * 32.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 24.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .child(
                    v_flex()
                        .gap_3()
                        .child(result.target.name.clone())
                        .child(div().text_sm().child(result.target.url.clone()))
                        .child(div().text_sm().child(route.clone()))
                        .child(div().p_3().bg(cx.theme().secondary).child(content))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("copy-latency-result")
                                        .outline()
                                        .label(zenclash_i18n::text("redesign.copy_content"))
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(
                                                gpui_kit::ClipboardItem::new_string(copy.clone()),
                                            )
                                        }),
                                )
                                .child(
                                    Button::new("retry-latency-result")
                                        .primary()
                                        .label(zenclash_i18n::text("network.latency.retest"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = owner.update(cx, |page, cx| {
                                                page.refresh_network_probe(cx)
                                            });
                                        }),
                                ),
                        ),
                )
        });
    }

    pub(super) fn open_network_cache_confirmation(
        &mut self,
        action: DnsCacheAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_network_cache_flush(action, cx);
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let confirm = owner.clone();
            let cancel = owner.clone();
            dialog
                .confirm()
                .title(zenclash_i18n::text(match action {
                    DnsCacheAction::Dns => "network.diagnostics.confirm_dns_flush",
                    DnsCacheAction::FakeIp => "network.diagnostics.confirm_fake_ip_flush",
                }))
                .description(zenclash_i18n::text(match action {
                    DnsCacheAction::Dns => "redesign.dns_cache_scope",
                    DnsCacheAction::FakeIp => "redesign.fake_ip_cache_scope",
                }))
                .on_ok(move |_, _, cx| {
                    confirm
                        .update(cx, |page, cx| {
                            if page.core_busy() {
                                return false;
                            }
                            page.flush_network_cache(action, cx);
                            true
                        })
                        .unwrap_or(true)
                })
                .on_cancel(move |_, _, cx| {
                    let _ = cancel.update(cx, |page, cx| page.cancel_network_cache_flush(cx));
                    true
                })
        });
    }

    pub(super) fn open_network_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        window.focus(&self.focus_handle, cx);
        self.network_probe.adding_target = true;
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let content = owner
                .update(cx, |page, cx| page.network_target_content(cx))
                .ok();
            let owner = owner.clone();
            dialog
                .bg(cx.theme().group_box)
                .title(zenclash_i18n::text("redesign.add_target"))
                .width(window.rem_size() * 32.)
                .margin_top(
                    ((window.viewport_size().height - window.rem_size() * 24.) / 2.)
                        .max(gpui_kit::px(16.)),
                )
                .when_some(content, |dialog, content| dialog.child(content))
                .on_cancel(move |_, _, cx| {
                    owner
                        .update(cx, |page, _| {
                            if page
                                .mutation_busy(crate::pages::runtime::busy::MutationDomain::Network)
                            {
                                return false;
                            }
                            page.network_probe.adding_target = false;
                            true
                        })
                        .unwrap_or(true)
                })
        });
        use gpui_kit::Focusable;
        self.network_probe
            .latency_name
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn network_target_content(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let busy = self.mutation_busy(crate::pages::runtime::busy::MutationDomain::Network);
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .child(zenclash_i18n::text("network.latency.target_name")),
            )
            .child(Input::new(&self.network_probe.latency_name).disabled(busy))
            .child(
                div()
                    .text_sm()
                    .child(zenclash_i18n::text("network.latency.target_url")),
            )
            .child(Input::new(&self.network_probe.latency_url).disabled(busy))
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-network-target")
                            .outline()
                            .label(zenclash_i18n::text("common.actions.cancel"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.network_probe.adding_target = false;
                                window.close_dialog(cx);
                                this.restore_page_focus(crate::pages::Page::Network, cx);
                            })),
                    )
                    .child(
                        Button::new("save-network-target")
                            .primary()
                            .label(zenclash_i18n::text("common.actions.save"))
                            .loading(busy)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.add_network_latency_target(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::runtime::ui_tests::{Fixture, open};
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    fn invalid_network_target_stays_in_modal_and_keeps_preferences_unchanged(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, super::super::super::Page::Network);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                page.open_network_target(window, cx);
                page.network_probe.latency_name.update(cx, |input, cx| {
                    input.set_value("Invalid target", window, cx)
                });
                page.network_probe.latency_url.update(cx, |input, cx| {
                    input.set_value("file:///invalid", window, cx)
                });
            });
            window.render_frame(cx);
            window.render_frame(cx);
            window.click("save-network-target", cx);
            assert!(window.has_active_dialog(cx));
            assert!(page.read(cx).error.is_some());
            assert!(page.read(cx).preferences.network_latency_targets.is_empty());
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn cache_confirmation_escape_clears_intent_without_starting_mutation(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (window, page) = open(cx, &fixture, super::super::super::Page::Network);
        fixture.settle(cx, &page, |page| !page.persistent_loading);
        cx.update_window(window, |_, window, cx| {
            page.update(cx, |page, cx| {
                page.open_network_cache_confirmation(DnsCacheAction::FakeIp, window, cx)
            });
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                page.read(cx).network_probe.cache_confirmation,
                Some(DnsCacheAction::FakeIp)
            );
            window.press("escape", cx);
            assert!(!window.has_active_dialog(cx));
            assert_eq!(page.read(cx).network_probe.cache_confirmation, None);
            assert!(!page.read(cx).core_busy());
            window.remove_window();
        })
        .unwrap();
    }
}
