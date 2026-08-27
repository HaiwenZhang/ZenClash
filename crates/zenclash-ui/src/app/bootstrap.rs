use gpui::{Menu, MenuItem};

use super::{
    apply_zen_theme, px, App, AppContext, AppPreferences, AppPreferencesStore, AppServices,
    AppearancePreference, KeyBinding, LogMonitor, NetworkTrayIcon, Quit, Root, SharedString,
    ShowStatusMenu, ThemeMode, TitleBar, ToggleFloatingWindow, WindowBounds, WindowOptions,
    ZenClashApp,
};

/// Registers `ZenClash` actions, native menus, and platform-appropriate key bindings.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-shift-m", ShowStatusMenu, None),
            KeyBinding::new("cmd-shift-f", ToggleFloatingWindow, None),
        ]);
        cx.set_menus(vec![Menu {
            name: "ZenClash".into(),
            items: vec![MenuItem::action("退出 ZenClash", Quit)],
        }]);
    } else {
        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-shift-m", ShowStatusMenu, None),
            KeyBinding::new("ctrl-shift-f", ToggleFloatingWindow, None),
        ]);
    }
}

#[cfg(target_os = "macos")]
fn keep_main_window_alive_when_closed(window: &gpui::Window, cx: &App) {
    window.on_window_should_close(cx, |_, cx| {
        cx.hide();
        false
    });
}

/// Opens the primary `ZenClash` window and installs the native traffic tray.
pub fn create_main_window(services: AppServices, cx: &mut App) {
    let title = SharedString::from("ZenClash");
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(gpui::size(px(1280.), px(820.)), cx)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    let preferences_store = services.preferences_store.clone();
    let preferences = services.preferences.clone();
    if let Err(error) = configure_log_monitor(
        &services.log_monitor,
        preferences_store.as_ref(),
        &preferences,
    ) {
        tracing::warn!(%error, "failed to configure continuous core log persistence");
    }

    let managed_process = services.mihomo_process.clone();
    cx.on_app_quit(move |_| {
        if let Some(process) = managed_process.as_ref() {
            if let Err(error) = process.stop() {
                tracing::warn!(%error, "failed to stop managed core during native application quit");
            }
        }
        std::future::ready(())
    })
    .detach();

    cx.spawn(async move |cx| {
        let result = cx.open_window(options, |window, cx| {
            let theme = match preferences.appearance {
                AppearancePreference::System => ThemeMode::from(window.appearance()),
                AppearancePreference::Dark => ThemeMode::Dark,
                AppearancePreference::Light => ThemeMode::Light,
            };
            apply_zen_theme(theme, Some(window), cx);
            window.set_window_title(&title);
            window.activate_window();
            #[cfg(target_os = "macos")]
            keep_main_window_alive_when_closed(window, cx);
            let network_tray = match NetworkTrayIcon::new(services.core_kind) {
                Ok(tray) => {
                    if let Err(error) = tray.set_visible(preferences.traffic_tray_visible) {
                        tracing::warn!(%error, "failed to restore traffic tray visibility");
                    }
                    Some(tray)
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to create native traffic tray icon");
                    None
                }
            };
            let app = cx.new(|cx| {
                ZenClashApp::new(
                    services,
                    network_tray,
                    preferences_store,
                    preferences,
                    window,
                    cx,
                )
            });
            let app_for_global_quit = app.downgrade();
            cx.on_action(move |_: &Quit, cx| {
                let _ = app_for_global_quit.update(cx, |app, cx| app.begin_quit(None, cx));
            });
            cx.new(|cx| Root::new(app, window, cx))
        });
        if let Err(error) = result {
            tracing::error!(%error, "failed to open ZenClash window");
        }
    })
    .detach();
}

pub(super) fn configure_log_monitor(
    monitor: &LogMonitor,
    store: Option<&AppPreferencesStore>,
    preferences: &AppPreferences,
) -> Result<(), String> {
    let store = store.ok_or_else(|| "应用设置存储不可用".to_owned())?;
    monitor
        .configure_persistence(
            store.log_file_path(),
            preferences.log_file_enabled,
            preferences.log_file_max_mebibytes,
        )
        .map_err(|error| error.to_string())
}
