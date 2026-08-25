use super::{
    color, div, format_speed, h_flex, px, throughput_activity_percent, v_flex, AnyElement, Badge,
    Button, ButtonGroup, Divider, InteractiveElement, IntoElement, OutboundMode, ParentElement,
    Progress, Selectable, SetDirectMode, SetGlobalMode, SetRuleMode, Sizable, Styled, VecDeque,
    ZenClashApp, SIGNAL_CYAN, UPLINK_AMBER,
};

impl ZenClashApp {
    fn render_signal_wave(
        samples: &VecDeque<u64>,
        accent: gpui::Hsla,
        id: &'static str,
    ) -> AnyElement {
        let maximum = samples.iter().copied().max().unwrap_or(1).max(1);

        h_flex()
            .h(px(13.))
            .w(px(142.))
            .items_end()
            .gap(px(2.))
            .children(samples.iter().enumerate().map(|(index, value)| {
                let normalized = normalized_sample(*value, maximum);
                div()
                    .id((id, index))
                    .flex_1()
                    .h(px(2. + normalized * 11.))
                    .rounded_full()
                    .bg(accent.opacity(0.28 + normalized * 0.72))
            }))
            .into_any_element()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the signal rail is one declarative GPUI element tree whose layout context is shared across every child"
    )]
    pub(super) fn render_signal_rail(&self, theme: &gpui_component::Theme) -> AnyElement {
        let online = self.traffic.connected;
        let outbound_mode = self.outbound_mode.displayed();
        let download_color = color(SIGNAL_CYAN);
        let upload_color = color(UPLINK_AMBER);
        let total = self.traffic.upload.saturating_add(self.traffic.download);
        let activity = throughput_activity_percent(total);

        h_flex()
            .h(px(76.))
            .flex_none()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .w(px(216.))
                    .h_full()
                    .flex_none()
                    .pl(px(72.))
                    .pr_3()
                    .gap_2()
                    .border_r_1()
                    .border_color(theme.sidebar_border)
                    .child(div().size(px(9.)).rounded_full().bg(theme.primary))
                    .child(
                        v_flex()
                            .gap_0()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_sm()
                                    .child("ZENCLASH"),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.muted_foreground)
                                    .child("MIHOMO CONTROL"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px_5()
                    .gap_5()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                Badge::new()
                                    .dot()
                                    .color(if online { theme.success } else { theme.danger })
                                    .child(
                                        div()
                                            .size(px(31.))
                                            .rounded(theme.radius)
                                            .border_1()
                                            .border_color(theme.border)
                                            .bg(theme.secondary)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_size(px(10.))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .child("M"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_0()
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(theme.muted_foreground)
                                            .child("CORE"),
                                    )
                                    .child(
                                        div()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(if online {
                                                theme.success
                                            } else {
                                                theme.danger
                                            })
                                            .child(if online { "ONLINE" } else { "RECONNECT" }),
                                    ),
                            ),
                    )
                    .child(Divider::vertical().h(px(34.)).color(theme.border))
                    .child(
                        v_flex()
                            .w(px(178.))
                            .gap_1()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_size(px(10.))
                                    .text_color(theme.muted_foreground)
                                    .child("LIVE THROUGHPUT")
                                    .child(
                                        div()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(theme.foreground)
                                            .child(format_speed(total)),
                                    ),
                            )
                            .child(Progress::new().h(px(3.)).bg(download_color).value(activity)),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(Self::render_signal_wave(
                                        &self.download_samples,
                                        download_color,
                                        "download-signal",
                                    ))
                                    .child(Self::render_signal_wave(
                                        &self.upload_samples,
                                        upload_color,
                                        "upload-signal",
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(11.))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_color(download_color)
                                            .child("↓")
                                            .child(format_speed(self.traffic.download)),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_color(upload_color)
                                            .child("↑")
                                            .child(format_speed(self.traffic.upload)),
                                    ),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        v_flex()
                            .items_end()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.muted_foreground)
                                    .child("OUTBOUND MODE"),
                            )
                            .child(
                                ButtonGroup::new("outbound-mode")
                                    .small()
                                    .compact()
                                    .outline()
                                    .child(
                                        Button::new("outbound-rule")
                                            .label(OutboundMode::Rule.code())
                                            .selected(outbound_mode == OutboundMode::Rule),
                                    )
                                    .child(
                                        Button::new("outbound-global")
                                            .label(OutboundMode::Global.code())
                                            .selected(outbound_mode == OutboundMode::Global),
                                    )
                                    .child(
                                        Button::new("outbound-direct")
                                            .label(OutboundMode::Direct.code())
                                            .selected(outbound_mode == OutboundMode::Direct),
                                    )
                                    .on_click(|selected, window, cx| {
                                        if selected.contains(&0) {
                                            window.dispatch_action(Box::new(SetRuleMode), cx);
                                        } else if selected.contains(&1) {
                                            window.dispatch_action(Box::new(SetGlobalMode), cx);
                                        } else if selected.contains(&2) {
                                            window.dispatch_action(Box::new(SetDirectMode), cx);
                                        }
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn normalized_sample(value: u64, maximum: u64) -> f32 {
    let thousandths = (u128::from(value) * 1_000 / u128::from(maximum.max(1))).min(1_000);
    f32::from(u16::try_from(thousandths).unwrap_or(1_000)) / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::normalized_sample;

    #[test]
    fn normalized_sample_is_bounded_and_proportional() {
        assert!((normalized_sample(0, 10) - 0.0).abs() < f32::EPSILON);
        assert!((normalized_sample(5, 10) - 0.5).abs() < f32::EPSILON);
        assert!((normalized_sample(20, 10) - 1.0).abs() < f32::EPSILON);
    }
}
