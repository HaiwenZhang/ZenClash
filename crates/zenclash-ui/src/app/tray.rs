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

/// Consolidates native tray icon ownership, menu snapshots, refresh state, and command queues.
#[derive(Default)]
pub(super) struct TrayController {
    pub(super) state: TrayMenuState,
    pub(super) observation: Option<zenclash_core::CoreObservationIdentity>,
    pub(super) error: Option<String>,
    pub(super) command_error: Option<String>,
    pub(super) refreshing: bool,
    pub(super) refresh_pending: bool,
    pub(super) menu_requested: bool,
    pub(super) system_proxy_commands: LatestCommandQueue<(bool, u16)>,
    pub(super) tun_commands: LatestCommandQueue<bool>,
    pub(super) proxy_selection_commands:
        LatestCommandQueue<proxy_commands::TrayProxySelectionRequest>,
    pub(super) profile_selection_commands: LatestCommandQueue<String>,
    pub(super) icon: Option<NetworkTrayIcon>,
}

impl TrayController {
    pub(super) fn new(icon: Option<NetworkTrayIcon>) -> Self {
        Self {
            icon,
            ..Self::default()
        }
    }

    pub(super) fn feedback_error(&self) -> Option<String> {
        self.command_error.clone().or_else(|| self.error.clone())
    }
}

impl ZenClashApp {
    pub(super) fn start_tray_updates(&mut self, cx: &mut Context<Self>) {
        let Some(mut events) = self
            .tray
            .icon
            .as_mut()
            .and_then(NetworkTrayIcon::take_event_receiver)
        else {
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| {
                        let event = this
                            .tray
                            .icon
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
        });
        self.shell.tasks.push(task);
    }

    pub(in crate::app) fn handle_tray_click(&mut self, click: TrayClick, cx: &mut Context<Self>) {
        match click {
            TrayClick::ShowWindow => self.handle_tray_command(TrayCommand::ShowWindow, cx),
            TrayClick::ShowMenu => {
                self.tray.menu_requested = true;
                self.refresh_tray_menu(cx);
            }
        }
    }
}
