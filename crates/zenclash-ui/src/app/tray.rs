use super::{
    open_directory, px, tray_directories, AppContext, ClipboardItem, Context, Duration,
    EnvironmentShell, FloatingTrafficWindow, NetworkTrayIcon, OutboundMode, Page, Root, TitleBar,
    TrayClick, TrayCommand, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode,
    WindowBounds, WindowKind, WindowOptions, ZenClashApp,
};

mod commands;
mod queue;
mod refresh;
mod window;

pub(in crate::app) use queue::LatestCommandQueue;

impl ZenClashApp {
    pub(super) fn start_tray_updates(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if this
                .update(cx, |this, cx| {
                    if let Some(command) = this
                        .network_tray
                        .as_ref()
                        .and_then(NetworkTrayIcon::next_command)
                    {
                        this.handle_tray_command(command, cx);
                    }
                    if let Some(click) = this
                        .network_tray
                        .as_ref()
                        .and_then(NetworkTrayIcon::next_click)
                    {
                        match click {
                            TrayClick::ShowWindow => this.show_main_window(cx),
                            TrayClick::ShowMenu => {
                                this.tray_menu_requested = true;
                                this.refresh_tray_menu(cx);
                            }
                        }
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }
}
