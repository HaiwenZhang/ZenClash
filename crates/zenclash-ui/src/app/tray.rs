use super::{
    ClipboardItem, Context, EnvironmentShell, NetworkTrayIcon, OutboundMode, Page, TrayClick,
    TrayCommand, TrayEvent, TrayMenuState, TrayProfile, TrayProxyGroup, TrayProxyNode, ZenClashApp,
    open_directory,
};

mod commands;
mod directories;
pub(in crate::app) mod proxy_commands;
mod queue;
mod refresh;
mod runtime_observation;
#[cfg(test)]
mod test_support;

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
                            Some(TrayEvent::Command(command, observation)) => {
                                this.handle_observed_tray_command(command, observation, cx);
                            }
                            Some(TrayEvent::Click(click)) => this.handle_tray_click(click, cx),
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

    pub(in crate::app) fn handle_tray_click(&mut self, click: TrayClick, cx: &mut Context<Self>) {
        match click {
            TrayClick::ShowWindow => self.handle_tray_command(TrayCommand::ShowWindow, cx),
            TrayClick::ShowMenu => {
                self.tray_menu_requested = true;
                self.refresh_tray_menu(cx);
            }
        }
    }
}
