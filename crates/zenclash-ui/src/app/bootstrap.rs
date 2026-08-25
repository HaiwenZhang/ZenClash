use super::{
    apply_zen_theme, px, App, AppContext, AppServices, KeyBinding, NetworkTrayIcon, Quit, Root,
    SharedString, ShowStatusMenu, ThemeMode, TitleBar, ToggleFloatingWindow, WindowBounds,
    WindowOptions, ZenClashApp,
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
    cx.on_action(|_: &Quit, cx| cx.quit());
}

/// Opens the primary `ZenClash` window and installs the native traffic tray.
pub fn create_main_window(services: AppServices, cx: &mut App) {
    let title = SharedString::from("ZenClash");
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(gpui::size(px(1280.), px(820.)), cx)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    cx.spawn(async move |cx| {
        let result = cx.open_window(options, |window, cx| {
            apply_zen_theme(ThemeMode::Dark, Some(window), cx);
            window.set_window_title(&title);
            window.activate_window();
            let network_tray = match NetworkTrayIcon::new() {
                Ok(tray) => Some(tray),
                Err(error) => {
                    tracing::warn!(%error, "failed to create native traffic tray icon");
                    None
                }
            };
            let app = cx.new(|cx| ZenClashApp::new(services, network_tray, window, cx));
            cx.new(|cx| Root::new(app, window, cx))
        });
        if let Err(error) = result {
            tracing::error!(%error, "failed to open ZenClash window");
        }
    })
    .detach();
}
