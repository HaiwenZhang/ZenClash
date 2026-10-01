use gpui_kit::{Menu, MenuItem};

use super::{
    App, AppContext, AppPreferences, AppPreferencesStore, AppServices, AppearancePreference,
    KeyBinding, LogMonitor, NetworkTrayIcon, Quit, SharedString, ShowStatusMenu, ThemeMode,
    TitleBar, ToggleFloatingWindow, WindowBounds, WindowOptions, ZenClashApp, apply_zen_theme, px,
};

/// Registers `ZenClash` actions, native menus, and platform-appropriate key bindings.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    cx.bind_keys([KeyBinding::new(
        "escape",
        super::CloseStatusPanel,
        Some("ZenClashStatusPanel"),
    )]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-shift-m", ShowStatusMenu, None),
            KeyBinding::new("cmd-shift-f", ToggleFloatingWindow, None),
        ]);
        refresh_native_app_menu(cx);
    } else {
        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-shift-m", ShowStatusMenu, None),
            KeyBinding::new("ctrl-shift-f", ToggleFloatingWindow, None),
        ]);
    }
}

pub(super) fn refresh_native_app_menu(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.set_menus(vec![Menu {
            name: zenclash_i18n::text("app.menu.title").into(),
            items: vec![MenuItem::action(zenclash_i18n::text("app.menu.quit"), Quit)],
            disabled: false,
        }]);
    }
}

#[cfg(target_os = "macos")]
fn keep_main_window_alive_when_closed(
    window: &gpui_kit::Window,
    app: gpui_kit::WeakEntity<ZenClashApp>,
    cx: &App,
) {
    window.on_window_should_close(cx, move |window, cx| {
        let _ = app.update(cx, |app, cx| {
            app.park_main_window(window);
            app.release_hidden_page_data(cx);
            cx.hide();
        });
        false
    });
}

/// Opens the primary `ZenClash` window and installs the native traffic tray.
pub fn create_main_window(services: AppServices, cx: &mut App) {
    let title = SharedString::from("ZenClash");
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(
            gpui_kit::size(px(1280.), px(820.)),
            cx,
        )),
        titlebar: Some(TitleBar::title_bar_options()),
        app_owns_titlebar_drag: false,
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

    let core_session = services.core_session.clone();
    let history = services.traffic_history_session.clone();
    let runtime = services.runtime.clone();
    cx.on_app_quit(move |_| {
        let core_session = core_session.clone();
        let history = history.clone();
        let task = runtime.spawn(async move {
            if let Some(history) = history
                && let Err(error) = history.shutdown().await
            {
                tracing::warn!(%error, "failed to flush traffic history during native application quit");
            }
            if let Err(error) = core_session.shutdown().await {
                tracing::warn!(%error, "failed to stop managed core during native application quit");
            }
        });
        async move { let _ = task.await; }
    })
    .detach();

    cx.spawn(async move |cx| {
        let result = cx.update(|cx| {
            gpui_kit::open_window(options, cx, |window, cx| {
                let theme = match preferences.appearance {
                    AppearancePreference::System => ThemeMode::from(window.appearance()),
                    AppearancePreference::Dark => ThemeMode::Dark,
                    AppearancePreference::Light => ThemeMode::Light,
                };
                apply_zen_theme(theme, Some(window), cx);
                window.set_window_title(&title);
                window.activate_window();
                let network_tray = match NetworkTrayIcon::new(
                    services.core_kind,
                    services.traffic_monitor.clone(),
                ) {
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
                #[cfg(target_os = "macos")]
                keep_main_window_alive_when_closed(window, app.downgrade(), cx);
                #[cfg(target_os = "windows")]
                {
                    let app_for_native_close = app.downgrade();
                    window.on_window_should_close(cx, move |_, cx| {
                        let _ = app_for_native_close.update(cx, |app, cx| app.begin_quit(None, cx));
                        false
                    });
                }
                let app_for_global_quit = app.downgrade();
                cx.on_action(move |_: &Quit, cx| {
                    let _ = app_for_global_quit.update(cx, |app, cx| app.begin_quit(None, cx));
                });
                app
            })
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
    let store = store.ok_or_else(|| zenclash_i18n::text("app.errors.preferences_unavailable"))?;
    monitor
        .configure_persistence(
            store.log_file_path(),
            preferences.log_file_enabled,
            preferences.log_file_max_mebibytes,
        )
        .map_err(|error| error.to_string())
}
