use super::{
    AppContext, ClipboardItem, Context, EnvironmentShell, FloatingTrafficWindow, NetworkTrayIcon,
    OutboundMode, Page, Root, TitleBar, TrayClick, TrayCommand, TrayEvent, TrayMenuState,
    TrayProfile, TrayProxyGroup, TrayProxyNode, WindowBounds, WindowKind, WindowOptions,
    ZenClashApp, open_directory, px, tray_directories,
};

mod commands;
mod queue;
mod refresh;
mod window;

pub(in crate::app) use queue::LatestCommandQueue;

impl ZenClashApp {
    pub(super) fn start_tray_updates(&mut self, cx: &mut Context<Self>) {
        let Some(mut events) = self
            .network_tray
            .as_mut()
            .and_then(NetworkTrayIcon::take_event_receiver)
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| {
                        let event = this
                            .network_tray
                            .as_ref()
                            .and_then(|tray| tray.resolve_event(event));
                        match event {
                            Some(TrayEvent::Command(command)) => {
                                this.handle_tray_command(command, cx);
                            }
                            Some(TrayEvent::Click(click)) => match click {
                                TrayClick::ShowWindow => this.show_main_window(cx),
                                TrayClick::ShowMenu => {
                                    this.tray_menu_requested = true;
                                    this.refresh_tray_menu(cx);
                                }
                            },
                            None => {}
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}
