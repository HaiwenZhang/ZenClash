use super::{
    AppContext, ClipboardItem, Context, EnvironmentShell, NetworkTrayIcon, OutboundMode, Page,
    TrayClick, TrayCommand, TrayEvent, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode,
    ZenClashApp, open_directory, tray_directories,
};

mod commands;
pub(in crate::app) mod panel;
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
        #[cfg(target_os = "linux")]
        if let Some(tray) = self.network_tray.as_mut() {
            tray.start_native_event_loop(cx);
        }
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
                                TrayClick::ShowPanel => this.toggle_status_panel(cx),
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
