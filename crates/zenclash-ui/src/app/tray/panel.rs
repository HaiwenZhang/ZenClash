use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    chart::AreaChart,
    h_flex, v_flex,
};
use gpui_kit::{
    AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Pixels, Render, Styled, Subscription, Task, WeakEntity, Window, WindowBounds, WindowKind,
    WindowOptions, div, point, px, rems, size,
};
use zenclash_core::{TrafficSnapshot, format_speed};

use crate::{
    app::ZenClashApp,
    components::{
        mint_switch::MintSwitch,
        sidebar::OutboundMode,
        tray::{TrayCommand, TrayMenuState},
    },
};

mod traffic;
mod view;
pub(in crate::app) use traffic::TrafficHistory;
use traffic::TrafficPoint;

struct StatusPanelSnapshot {
    state: TrayMenuState,
    traffic: TrafficSnapshot,
    points: Vec<TrafficPoint>,
    mode: OutboundMode,
    mode_pending: bool,
    capture_pending: bool,
    current_node: Option<String>,
    generation: u64,
    unavailable: bool,
    error: Option<String>,
}

impl StatusPanelSnapshot {
    fn new(app: &ZenClashApp) -> Self {
        let generation = app.core_session.generation();
        Self {
            state: app.tray_state.clone(),
            traffic: app.traffic_monitor.snapshot(),
            points: app.status_panel_traffic.points(generation),
            mode: app.outbound_mode.displayed(),
            mode_pending: app.outbound_mode.is_pending(),
            capture_pending: app.system_proxy_commands.is_running()
                || app.tun_commands.is_running(),
            current_node: app
                .tray_current_node
                .as_ref()
                .filter(|(mode, _)| *mode == app.outbound_mode.displayed())
                .map(|(_, node)| node.clone()),
            generation,
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
    _refresh_task: Option<Task<()>>,
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
                let mut panel = StatusPanel {
                    owner,
                    snapshot,
                    focus,
                    _subscriptions: subscriptions,
                    _refresh_task: None,
                };
                panel.start_refresh(cx);
                panel
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
    let height = px(500.).min(screen.size.height);
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
    fn start_refresh(&mut self, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        // Automatic groups can change their current node without a UI command.
        // The panel owns this task, so closing it stops catalog refreshes.
        self._refresh_task = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(5))
                    .await;
                if owner
                    .update(cx, |app, cx| app.refresh_tray_menu(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    fn app_services(fixture: &crate::pages::runtime::ui_tests::Fixture) -> crate::app::AppServices {
        let services = fixture.services();
        crate::app::AppServices {
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
        }
    }

    #[gpui_kit::test]
    fn application_reopen_restores_hidden_main_window(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::Root;
        let fixture = crate::pages::runtime::ui_tests::Fixture::new();
        let services = app_services(&fixture);
        cx.executor().allow_parking();
        cx.update(crate::app::init);
        let mut owner = None;
        let main = cx.open_window(size(px(1280.), px(820.)), |window, cx| {
            let preferences = services.preferences.clone();
            let app =
                cx.new(|cx| ZenClashApp::new(services, None, None, None, preferences, window, cx));
            owner = Some(app.clone());
            Root::new(app, window, cx)
        });
        let owner = owner.unwrap();
        // The headless platform has no native application-hide implementation.
        // Reproduce the retained window state established by the close handler.
        cx.update_window(main.into(), |_, window, cx| {
            owner.update(cx, |app, cx| {
                #[cfg(target_os = "macos")]
                app.park_main_window(window);
                app.release_hidden_page_data(cx);
            });
        })
        .unwrap();
        assert!(!cx.update(|cx| owner.read(cx).main_window_visible));
        cx.update(crate::app::bootstrap::reopen_main_window);
        assert!(cx.update(|cx| owner.read(cx).main_window_visible));
        #[cfg(target_os = "macos")]
        assert!(cx.update(|cx| owner.read(cx).main_window_memory.restore_size.is_none()));
        cx.update(crate::app::bootstrap::reopen_main_window);
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        cx.update_window(main.into(), |_, window, _| window.remove_window())
            .unwrap();
    }

    #[gpui_kit::test]
    fn tray_command_opens_and_renders_status_panel_while_owner_is_updating(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt;
        let fixture = crate::pages::runtime::ui_tests::Fixture::new();
        let app_services = app_services(&fixture);
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
                app.tray_state.mode = "rule".into();
                app.tray_current_node = Some((OutboundMode::Rule, "🇭🇰 香港 · HK 01".into()));
                app.status_panel_traffic.observe(&TrafficSnapshot {
                    generation: app.core_session.generation(),
                    updated_at_ms: 1_000,
                    upload: 100,
                    download: 200,
                    ..Default::default()
                });
                app.handle_tray_command(TrayCommand::ShowPanel, cx)
            });
        });
        cx.run_until_parked();
        let panel = cx.update(|cx| owner.read(cx).status_panel.unwrap());
        cx.update_window(panel, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("panel-traffic").visible());
            assert!(window.find("panel-mode-rule").visible());
            assert!(window.find("panel-mode-global").visible());
            assert!(window.find("panel-mode-direct").visible());
            assert!(window.find("panel-system-proxy").visible());
            assert!(window.find("panel-tun").visible());
            assert!(window.find("panel-current-node").visible());
            let node = window.find("panel-current-node-name");
            assert_eq!(node.label(), Some("🇭🇰 香港 · HK 01"));
            assert_eq!(node.role(), Some(gpui_kit::Role::Status));
            window.click("panel-current-node-name", cx);
            assert!(!owner.read(cx).proxy_selection_commands.is_running());
            assert!(
                window
                    .try_find((gpui_kit::ElementId::from("panel-group"), "first".to_owned()))
                    .is_none()
            );
            assert!(node.visible());
            assert!(node.bounds().bottom() <= window.viewport_size().height);
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.update_window(panel, |_, _, _| ()).is_err());
        cx.update(|cx| {
            owner.update(cx, |app, cx| {
                app.handle_tray_command(TrayCommand::ShowPanel, cx)
            });
            let app = owner.read(cx);
            assert_eq!(
                app.status_panel_traffic
                    .points(app.core_session.generation())
                    .len(),
                1
            );
        });
        cx.update(|cx| {
            owner.update(cx, |app, cx| {
                app.handle_tray_command(
                    TrayCommand::SetDisplay(zenclash_core::TrayDisplayPreference::Icon),
                    cx,
                )
            });
            assert_eq!(
                owner.read(cx).preferences.tray_display,
                zenclash_core::TrayDisplayPreference::Icon
            );
            assert_eq!(
                owner.read(cx).tray_state.display,
                zenclash_core::TrayDisplayPreference::Icon
            );
            assert!(owner.read(cx).preferences.traffic_tray_visible);
            owner.update(cx, |app, cx| {
                app.handle_tray_command(
                    TrayCommand::SetDisplay(zenclash_core::TrayDisplayPreference::Traffic),
                    cx,
                )
            });
            assert_eq!(
                owner.read(cx).preferences.tray_display,
                zenclash_core::TrayDisplayPreference::Traffic
            );
        });
        let reopened = cx.update(|cx| owner.read(cx).status_panel.unwrap());
        cx.update_window(reopened, |_, window, _| window.remove_window())
            .unwrap();
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
