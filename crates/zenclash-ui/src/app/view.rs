use super::{
    ActiveTheme, App, Context, Focusable, InteractiveElement, IntoElement, Page, ParentElement,
    Render, Sidebar, Styled, Window, ZenClashApp, div, h_flex, v_flex,
};

impl Focusable for ZenClashApp {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ZenClashApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match self.current_page {
            Page::Proxies => self.proxies_page.clone().into_any_element(),
            _ => self.runtime_page.clone().into_any_element(),
        };

        v_flex()
            .id("zenclash-app")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("ZenClash")
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_navigate_home))
            .on_action(cx.listener(Self::on_navigate_system_proxy))
            .on_action(cx.listener(Self::on_navigate_tun))
            .on_action(cx.listener(Self::on_navigate_profiles))
            .on_action(cx.listener(Self::on_navigate_proxies))
            .on_action(cx.listener(Self::on_navigate_mihomo))
            .on_action(cx.listener(Self::on_navigate_connections))
            .on_action(cx.listener(Self::on_navigate_dns))
            .on_action(cx.listener(Self::on_navigate_sniffer))
            .on_action(cx.listener(Self::on_navigate_logs))
            .on_action(cx.listener(Self::on_navigate_rules))
            .on_action(cx.listener(Self::on_navigate_resources))
            .on_action(cx.listener(Self::on_navigate_override))
            .on_action(cx.listener(Self::on_navigate_network))
            .on_action(cx.listener(Self::on_navigate_traffic))
            .on_action(cx.listener(Self::on_navigate_settings))
            .on_action(cx.listener(Self::on_set_rule_mode))
            .on_action(cx.listener(Self::on_set_global_mode))
            .on_action(cx.listener(Self::on_set_direct_mode))
            .on_action(cx.listener(Self::on_set_system_theme))
            .on_action(cx.listener(Self::on_set_light_theme))
            .on_action(cx.listener(Self::on_set_dark_theme))
            .on_action(cx.listener(Self::on_show_traffic_icon))
            .on_action(cx.listener(Self::on_hide_traffic_icon))
            .on_action(cx.listener(Self::on_show_status_menu))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_floating_window))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(Sidebar::new(self.current_page).collapsed(self.sidebar_collapsed))
                    .child(div().flex_1().h_full().min_w_0().child(content)),
            )
    }
}
