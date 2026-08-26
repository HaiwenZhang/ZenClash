use super::{
    apply_zen_theme, px, App, AppContext, AppPreferences, AppPreferencesStore, AppServices,
    AppearancePreference, KeyBinding, LogMonitor, NetworkTrayIcon, Quit, Root, SharedString,
    ShowStatusMenu, ThemeMode, TitleBar, ToggleFloatingWindow, WindowBounds, WindowOptions,
    ZenClashApp,
};

/// Registers `ZenClash` actions and platform-appropriate global key bindings.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-shift-m", ShowStatusMenu, None),
            KeyBinding::new("cmd-shift-f", ToggleFloatingWindow, None),
        ]);
    } else {
        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("ctrl-shift-m", ShowStatusMenu, None),
            KeyBinding::new("ctrl-shift-f", ToggleFloatingWindow, None),
        ]);
    }
}

/// Opens the primary `ZenClash` window and installs the native traffic tray.
pub fn create_main_window(services: AppServices, cx: &mut App) {
    let title = SharedString::from("ZenClash");
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(gpui::size(px(1280.), px(820.)), cx)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    let (preferences_store, preferences) = match AppPreferencesStore::discover() {
        Ok(store) => match store.load() {
            Ok(preferences) => (Some(store), preferences),
            Err(error) => {
                tracing::warn!(%error, "failed to load application preferences");
                (Some(store), AppPreferences::default())
            }
        },
        Err(error) => {
            tracing::warn!(%error, "failed to discover application preferences");
            (None, AppPreferences::default())
        }
    };
    if let Err(error) = configure_log_monitor(
        &services.log_monitor,
        preferences_store.as_ref(),
        &preferences,
    ) {
        tracing::warn!(%error, "failed to configure continuous core log persistence");
    }

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
