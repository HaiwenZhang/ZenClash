use gpui::{div, px, InteractiveElement, IntoElement, ParentElement, Render, Styled, Window};
use gpui_component::{
    badge::Badge,
    button::{Button, ButtonGroup},
    h_flex,
    progress::Progress,
    v_flex, ActiveTheme, Selectable, Sizable,
};
use zenclash_core::format_speed;

use super::FloatingTrafficWindow;
use crate::{
    components::sidebar::OutboundMode,
    design::{color, throughput_activity_percent, SIGNAL_CYAN, UPLINK_AMBER},
};

impl Render for FloatingTrafficWindow {
    #[allow(
        clippy::too_many_lines,
        reason = "the compact floating window is a single declarative GPUI element tree"
    )]
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let connected = self.traffic.connected;
        let outbound_mode = self.outbound_mode.displayed();
        let total = self.traffic.upload.saturating_add(self.traffic.download);
        let activity = throughput_activity_percent(total);

        v_flex()
            .id("floating-traffic-window")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(|_: &crate::app::ToggleFloatingWindow, window, _| window.remove_window())
            .child(
                h_flex()
                    .h(px(48.))
                    .px_4()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Badge::new()
                                    .dot()
                                    .color(if connected {
                                        theme.success
                                    } else {
                                        theme.danger
                                    })
                                    .child(
                                        div()
                                            .size(px(28.))
                                            .rounded(theme.radius)
                                            .bg(theme.secondary)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_size(px(10.))
                                            .child("ZC"),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .child("ZenClash Signal"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(if connected {
                                                theme.success
                                            } else {
                                                theme.danger
                                            })
                                            .child(if connected {
                                                "MIHOMO ONLINE"
                                            } else {
                                                "RECONNECTING"
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format_speed(total)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(speed_panel(
                                "DOWNLOAD",
                                "↓",
                                format_speed(self.traffic.download),
                                color(SIGNAL_CYAN),
                                theme,
                            ))
                            .child(speed_panel(
                                "UPLOAD",
                                "↑",
                                format_speed(self.traffic.upload),
                                color(UPLINK_AMBER),
                                theme,
                            )),
                    )
                    .child(Progress::new().h(px(3.)).bg(theme.primary).value(activity))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.muted_foreground)
                                    .child("OUTBOUND MODE"),
                            )
                            .child(
                                ButtonGroup::new("floating-mode")
                                    .small()
                                    .compact()
                                    .outline()
                                    .child(
                                        Button::new("floating-rule")
                                            .label("RULE")
                                            .selected(outbound_mode == OutboundMode::Rule),
                                    )
                                    .child(
                                        Button::new("floating-global")
                                            .label("GLOBAL")
                                            .selected(outbound_mode == OutboundMode::Global),
                                    )
                                    .child(
                                        Button::new("floating-direct")
                                            .label("DIRECT")
                                            .selected(outbound_mode == OutboundMode::Direct),
                                    )
                                    .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                        if selected.contains(&0) {
                                            this.set_mode(OutboundMode::Rule, cx);
                                        } else if selected.contains(&1) {
                                            this.set_mode(OutboundMode::Global, cx);
                                        } else if selected.contains(&2) {
                                            this.set_mode(OutboundMode::Direct, cx);
                                        }
                                    })),
                            ),
                    ),
            )
    }
}

fn speed_panel(
    label: &'static str,
    arrow: &'static str,
    value: String,
    accent: gpui::Hsla,
    theme: &gpui_component::Theme,
) -> gpui::AnyElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .justify_between()
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .child(
            v_flex()
                .gap_0()
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .font_family(theme.mono_font_family.clone())
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child(value),
                ),
        )
        .child(div().text_xl().text_color(accent).child(arrow))
        .into_any_element()
}
