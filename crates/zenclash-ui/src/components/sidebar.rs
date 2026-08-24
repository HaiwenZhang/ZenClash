use gpui::{
    div, prelude::FluentBuilder, px, App, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{h_flex, scroll::ScrollableElement, v_flex, ActiveTheme, Icon};
use zenclash_core::{format_speed, TrafficSnapshot};

use crate::{
    app::{
        NavigateConnections, NavigateDns, NavigateLogs, NavigateMihomo, NavigateNetwork,
        NavigateOverride, NavigateProfiles, NavigateProxies, NavigateResources, NavigateRules,
        NavigateSettings, NavigateSniffer, NavigateSubStore, NavigateSystemProxy, NavigateTraffic,
        NavigateTun, SetDirectMode, SetGlobalMode, SetRuleMode,
    },
    pages::Page,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutboundMode {
    #[default]
    Rule,
    Global,
    Direct,
}

impl OutboundMode {
    fn label(self) -> &'static str {
        match self {
            Self::Rule => "规则",
            Self::Global => "全局",
            Self::Direct => "直连",
        }
    }
}

#[derive(IntoElement)]
pub struct Sidebar {
    current_page: Page,
    mode: OutboundMode,
    traffic: TrafficSnapshot,
    samples: Vec<u64>,
}

impl Sidebar {
    pub fn new(
        current_page: Page,
        mode: OutboundMode,
        traffic: TrafficSnapshot,
        samples: Vec<u64>,
    ) -> Self {
        Self {
            current_page,
            mode,
            traffic,
            samples,
        }
    }

    fn render_mode_switcher(&self, theme: &gpui_component::Theme) -> impl IntoElement {
        h_flex()
            .gap_1()
            .p_1()
            .rounded(theme.radius)
            .bg(theme.muted)
            .children(
                [
                    OutboundMode::Rule,
                    OutboundMode::Global,
                    OutboundMode::Direct,
                ]
                .into_iter()
                .map(|mode| {
                    let active = mode == self.mode;
                    div()
                        .id(mode.label())
                        .flex_1()
                        .py_1()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .text_center()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(mode.label())
                        .when(active, |this| {
                            this.bg(theme.primary).text_color(theme.primary_foreground)
                        })
                        .when(!active, |this| {
                            this.text_color(theme.muted_foreground)
                                .hover(|this| this.bg(theme.background))
                        })
                        .on_click(move |_, window, cx| match mode {
                            OutboundMode::Rule => window.dispatch_action(Box::new(SetRuleMode), cx),
                            OutboundMode::Global => {
                                window.dispatch_action(Box::new(SetGlobalMode), cx)
                            }
                            OutboundMode::Direct => {
                                window.dispatch_action(Box::new(SetDirectMode), cx)
                            }
                        })
                }),
            )
    }

    fn render_card(&self, page: Page, theme: &gpui_component::Theme) -> gpui::AnyElement {
        let active = page == self.current_page;
        let is_connections = page == Page::Connections;
        let background = if active {
            theme.primary
        } else {
            theme.secondary
        };
        let foreground = if active {
            theme.primary_foreground
        } else {
            theme.foreground
        };

        let mut card = v_flex()
            .id(page.route())
            .relative()
            .flex_1()
            .min_w(px(98.))
            .h(px(if is_connections { 102. } else { 72. }))
            .justify_between()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(if active { theme.primary } else { theme.border })
            .bg(background)
            .text_color(foreground)
            .cursor_pointer()
            .hover(|this| if active { this } else { this.bg(theme.muted) })
            .on_click(move |_, window, cx| dispatch_navigate(page, window, cx));

        if is_connections {
            let maximum = self.samples.iter().copied().max().unwrap_or(1).max(1);
            card = card
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .child(Icon::new(page.icon()).size_5())
                        .child(
                            v_flex()
                                .items_end()
                                .gap_0()
                                .text_xs()
                                .child(format!("↑ {}", format_speed(self.traffic.upload)))
                                .child(format!("↓ {}", format_speed(self.traffic.download))),
                        ),
                )
                .child(h_flex().h(px(16.)).items_end().gap_1().children(
                    self.samples.iter().enumerate().map(|(index, value)| {
                        let height = 2. + 14. * (*value as f32 / maximum as f32);
                        div()
                            .id(("traffic-sample", index))
                            .flex_1()
                            .h(px(height))
                            .rounded_sm()
                            .bg(foreground.opacity(0.42))
                    }),
                ))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(page.label()),
                );
        } else {
            card = card.child(Icon::new(page.icon()).size_5()).child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(page.label()),
            );
        }

        card.into_any_element()
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let mut cards = v_flex().gap_2();
        for pair in Page::SIDEBAR_CARDS.chunks(2) {
            let mut row = h_flex().gap_2();
            for page in pair {
                row = row.child(self.render_card(*page, theme));
            }
            if pair.len() == 1 {
                row = row.child(div().flex_1().min_w(px(98.)));
            }
            cards = cards.child(row);
        }

        v_flex()
            .w(px(250.))
            .h_full()
            .flex_none()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(49.))
                    .px_3()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(gpui_component::IconName::Star)
                            .size_6()
                            .text_color(theme.primary),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("ZenClash"),
                    ),
            )
            .child(div().px_2().pb_2().child(self.render_mode_switcher(theme)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px_2()
                    .child(cards),
            )
            .child(
                div()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(self.render_card(Page::Settings, theme)),
            )
    }
}

fn dispatch_navigate(page: Page, window: &mut Window, cx: &mut App) {
    match page {
        Page::SystemProxy => window.dispatch_action(Box::new(NavigateSystemProxy), cx),
        Page::Tun => window.dispatch_action(Box::new(NavigateTun), cx),
        Page::Profiles => window.dispatch_action(Box::new(NavigateProfiles), cx),
        Page::Proxies => window.dispatch_action(Box::new(NavigateProxies), cx),
        Page::Mihomo => window.dispatch_action(Box::new(NavigateMihomo), cx),
        Page::Connections => window.dispatch_action(Box::new(NavigateConnections), cx),
        Page::Dns => window.dispatch_action(Box::new(NavigateDns), cx),
        Page::Sniffer => window.dispatch_action(Box::new(NavigateSniffer), cx),
        Page::Logs => window.dispatch_action(Box::new(NavigateLogs), cx),
        Page::Rules => window.dispatch_action(Box::new(NavigateRules), cx),
        Page::Resources => window.dispatch_action(Box::new(NavigateResources), cx),
        Page::Override => window.dispatch_action(Box::new(NavigateOverride), cx),
        Page::SubStore => window.dispatch_action(Box::new(NavigateSubStore), cx),
        Page::Network => window.dispatch_action(Box::new(NavigateNetwork), cx),
        Page::Traffic => window.dispatch_action(Box::new(NavigateTraffic), cx),
        Page::Settings => window.dispatch_action(Box::new(NavigateSettings), cx),
    }
}
