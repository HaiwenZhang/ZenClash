use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::{
    AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use zenclash_core::{TrafficSnapshot, format_speed};

use crate::{
    app::ZenClashApp,
    components::{
        sidebar::OutboundMode,
        tray::{TrayCommand, TrayMenuState},
    },
};

struct StatusPanelSnapshot {
    state: TrayMenuState,
    traffic: TrafficSnapshot,
    mode: OutboundMode,
    unavailable: bool,
    error: Option<String>,
}

impl StatusPanelSnapshot {
    fn new(app: &ZenClashApp) -> Self {
        Self {
            state: app.tray_state.clone(),
            traffic: app.traffic_monitor.snapshot(),
            mode: app.outbound_mode.displayed(),
            unavailable: app.tray_error.is_some() || app.tray_state.mode.is_empty(),
            error: app
                .tray_command_error
                .clone()
                .or_else(|| app.mode_error.clone())
                .or_else(|| app.tray_error.clone()),
        }
    }
}

struct StatusPanel {
    owner: WeakEntity<ZenClashApp>,
    snapshot: StatusPanelSnapshot,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ZenClashApp {
    pub(super) fn toggle_status_panel(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.status_panel.take()
            && cx
                .update_window(handle, |_, window, _| window.remove_window())
                .is_ok()
        {
            return;
        }
        if self.quit_state != crate::app::system_proxy::QuitState::Idle {
            return;
        }
        cx.activate(true);
        let scale = cx
            .update_window(self.main_window, |_, window, _| window.scale_factor())
            .unwrap_or(1.);
        let anchor = self
            .network_tray
            .as_ref()
            .and_then(|tray| tray.panel_anchor(scale));
        let display = anchor
            .and_then(|anchor| {
                cx.displays()
                    .into_iter()
                    .find(|display| display.bounds().contains(&anchor.origin))
            })
            .or_else(|| cx.displays().into_iter().next());
        // Platform window bounds are logical pixels; controls inside the window use rem sizing.
        let bounds = display.as_ref().map(|display| {
            let screen = display.bounds();
            panel_bounds(
                anchor.unwrap_or(Bounds::new(screen.origin, size(px(0.), px(0.)))),
                screen,
            )
        });
        let owner = cx.entity().downgrade();
        // Opening the window paints immediately while the owner is still being updated.
        let snapshot = StatusPanelSnapshot::new(self);
        let options = WindowOptions {
            window_bounds: bounds.map(WindowBounds::Windowed),
            display_id: display.map(|display| display.id()),
            kind: WindowKind::PopUp,
            titlebar: None,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };
        match gpui_kit::open_window(options, cx, |window, cx| {
            let view = cx.new(|cx: &mut Context<StatusPanel>| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                let mut subscriptions =
                    vec![cx.observe_window_activation(window, |_, window, _| {
                        if !window.is_window_active() {
                            window.remove_window();
                        }
                    })];
                if let Some(app) = owner.upgrade() {
                    subscriptions.push(cx.observe(&app, |panel, owner, cx| {
                        panel.snapshot = StatusPanelSnapshot::new(owner.read(cx));
                        cx.notify();
                    }));
                }
                StatusPanel {
                    owner,
                    snapshot,
                    focus,
                    _subscriptions: subscriptions,
                }
            });
            window.activate_window();
            view
        }) {
            Ok((handle, _)) => self.status_panel = Some(handle),
            Err(error) => {
                self.tray_error = Some(error.to_string());
                self.show_main_window(cx);
            }
        }
        self.refresh_tray_menu(cx);
    }
}

fn panel_bounds(anchor: Bounds<Pixels>, screen: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = px(420.).min(screen.size.width);
    let height = px(560.).min(screen.size.height);
    let x = (anchor.right() - width)
        .max(screen.left())
        .min(screen.right() - width);
    let y = if anchor.bottom() + height <= screen.bottom() {
        anchor.bottom()
    } else {
        anchor.top() - height
    };
    Bounds::new(
        point(x, y.max(screen.top()).min(screen.bottom() - height)),
        size(width, height),
    )
}

impl StatusPanel {
    fn command(
        &self,
        command: TrayCommand,
    ) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
        let owner = self.owner.clone();
        move |_, _, cx| {
            let _ = owner.update(cx, |app, cx| app.handle_tray_command(command.clone(), cx));
        }
    }
}

impl Render for StatusPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.snapshot.state.clone();
        let traffic = &self.snapshot.traffic;
        let unavailable = self.snapshot.unavailable;
        let error = self.snapshot.error.clone();
        let profiles_owner = self.owner.clone();
        let refresh_owner = self.owner.clone();
        let mode = self.snapshot.mode;
        let mut content = v_flex()
            .gap_3()
            .p_4()
            .child(zenclash_i18n::text("panel.local"))
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(zenclash_i18n::text("app.name")),
                    )
                    .child(
                        Button::new("panel-main")
                            .small()
                            .ghost()
                            .label(zenclash_i18n::text("tray.show_window"))
                            .on_click(self.command(TrayCommand::ShowWindow)),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .child(format!("↑ {}", format_speed(traffic.upload)))
                    .child(format!("↓ {}", format_speed(traffic.download))),
            )
            .child(
                h_flex().gap_2().children(
                    [
                        (
                            "rule",
                            crate::components::sidebar::OutboundMode::Rule,
                            TrayCommand::SetRuleMode,
                        ),
                        (
                            "global",
                            crate::components::sidebar::OutboundMode::Global,
                            TrayCommand::SetGlobalMode,
                        ),
                        (
                            "direct",
                            crate::components::sidebar::OutboundMode::Direct,
                            TrayCommand::SetDirectMode,
                        ),
                    ]
                    .into_iter()
                    .map(|(id, value, command)| {
                        Button::new(id)
                            .small()
                            .outline()
                            .selected(mode == value)
                            .disabled(unavailable)
                            .label(value.label())
                            .on_click(self.command(command))
                    }),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("panel-system-proxy")
                            .small()
                            .outline()
                            .selected(state.system_proxy)
                            .disabled(unavailable && !state.system_proxy)
                            .label(zenclash_i18n::text("tray.system_proxy"))
                            .on_click(self.command(TrayCommand::SetSystemProxy {
                                enabled: !state.system_proxy,
                                port: state.mixed_port,
                            })),
                    )
                    .child(
                        Button::new("panel-tun")
                            .small()
                            .outline()
                            .selected(state.tun)
                            .disabled(unavailable && !state.tun)
                            .label(zenclash_i18n::text("navigation.tun.label"))
                            .on_click(self.command(TrayCommand::SetTun(!state.tun))),
                    ),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(zenclash_i18n::text("tray.profiles")),
            )
            .child(
                Button::new("panel-profile")
                    .small()
                    .outline()
                    .disabled(unavailable)
                    .label(state.profile_name.clone())
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for profile in &state.profiles {
                            let owner = profiles_owner.clone();
                            let id = profile.id.clone();
                            menu = menu.item(
                                PopupMenuItem::new(profile.name.clone())
                                    .checked(profile.active)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |app, cx| {
                                            app.handle_tray_command(
                                                TrayCommand::SelectProfile { id: id.clone() },
                                                cx,
                                            )
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            );
        if let Some(error) = error {
            content = content.child(div().text_color(cx.theme().danger).child(error));
        } else if !traffic.connected {
            content = content.child(zenclash_i18n::text("tray.core_offline"));
        }
        for group in state.groups {
            let owner = self.owner.clone();
            let name = group.name.clone();
            content = content.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(group.name.clone()),
                    )
                    .child(
                        Button::new((gpui_kit::ElementId::from("panel-group"), name.clone()))
                            .small()
                            .outline()
                            .label(group.now.clone())
                            .disabled(unavailable || !group.selectable)
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for node in group.proxies.iter() {
                                    let owner = owner.clone();
                                    let group_name = name.clone();
                                    let proxy = node.name.clone();
                                    let label = node.delay.map_or_else(
                                        || node.name.clone(),
                                        |delay| format!("{} · {delay} ms", node.name),
                                    );
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .checked(node.name == group.now)
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |app, cx| {
                                                    app.handle_tray_command(
                                                        TrayCommand::SelectProxy {
                                                            group: group_name.clone(),
                                                            proxy: proxy.clone(),
                                                        },
                                                        cx,
                                                    )
                                                });
                                            }),
                                    );
                                }
                                let owner = owner.clone();
                                menu.item(
                                    PopupMenuItem::new(zenclash_i18n::text("tray.open_proxies"))
                                        .on_click(move |_, _, cx| {
                                            let _ = owner.update(cx, |app, cx| {
                                                app.handle_tray_command(
                                                    TrayCommand::OpenProxies,
                                                    cx,
                                                )
                                            });
                                        }),
                                )
                            }),
                    ),
            );
        }
        content = content.child(
            Button::new("panel-refresh")
                .small()
                .ghost()
                .label(zenclash_i18n::text("panel.refresh"))
                .on_click(move |_, _, cx| {
                    let _ = refresh_owner.update(cx, |app, cx| app.refresh_tray_menu(cx));
                }),
        );
        v_flex()
            .id("status-panel")
            .track_focus(&self.focus)
            .key_context("ZenClashStatusPanel")
            .size_full()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .text_sm()
            .on_action(|_: &crate::app::CloseStatusPanel, window, cx| {
                window.remove_window();
                cx.stop_propagation();
            })
            .child(
                div()
                    .id("status-panel-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(content),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui_kit::test]
    fn tray_command_opens_and_renders_status_panel_while_owner_is_updating(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        let fixture = crate::pages::runtime::ui_tests::Fixture::new();
        let services = fixture.services();
        let app_services = crate::app::AppServices {
            initializing: true,
            await_service_handoff: false,
            profile_store: services.profile_store,
            override_store: services.override_store,
            preferences_store: None,
            preferences: services.preferences.clone(),
            core_kind: services.core_kind,
            core_session: services.core_session,
            client: services.client,
            traffic_monitor: services.traffic_monitor,
            log_monitor: services.log_monitor,
            traffic_history_store: services.traffic_history_store,
            traffic_history_session: None,
            profile_path: services.profile_path,
            controlled_config_store: services.controlled_config_store,
            runtime: services.runtime,
            startup_notice: None,
            startup_error: None,
            restart_after_exit: Default::default(),
        };
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::app::init(cx);
            cx.set_reduce_motion(true);
        });
        let mut owner = None;
        let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
            let preferences = app_services.preferences.clone();
            let app = cx.new(|cx| {
                ZenClashApp::new(app_services, None, None, None, preferences, window, cx)
            });
            owner = Some(app.clone());
            Root::new(app, window, cx)
        });
        let owner = owner.unwrap();
        cx.update(|cx| {
            owner.update(cx, |app, cx| {
                app.handle_tray_command(TrayCommand::ShowPanel, cx)
            });
        });
        cx.run_until_parked();
        let panel = cx.update(|cx| owner.read(cx).status_panel.unwrap());
        cx.update_window(panel, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("panel-main").visible());
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(main.into(), |_, window, _| window.remove_window())
            .unwrap();
    }

    #[test]
    fn panel_flips_above_bottom_tray_and_stays_on_negative_origin_monitor() {
        let screen = Bounds::new(point(px(-1280.), px(0.)), size(px(1280.), px(800.)));
        let anchor = Bounds::new(point(px(-100.), px(770.)), size(px(30.), px(30.)));
        let bounds = panel_bounds(anchor, screen);
        assert_eq!(bounds.bottom(), anchor.top());
        assert!(screen.contains(&bounds.origin));
        assert!(bounds.right() <= screen.right());
    }
    #[test]
    fn panel_fits_displays_smaller_than_its_preferred_size() {
        let screen = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(300.)));
        assert_eq!(panel_bounds(screen, screen), screen);
    }
}
