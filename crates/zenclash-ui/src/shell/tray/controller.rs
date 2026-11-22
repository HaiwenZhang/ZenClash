use crate::shell::app::ZenClashApp;
use crate::shell::tray::native::{
    NetworkTrayIcon, TrayClick, TrayCommand, TrayEvent, TrayMenuState,
};
use crate::shell::tray::{LatestCommandQueue, proxy_commands};
use gpui_kit::Context;

/// Consolidates native tray icon ownership, menu snapshots, refresh state, and command queues.
#[derive(Default)]
pub(in crate::shell) struct TrayController {
    pub(in crate::shell) state: TrayMenuState,
    pub(in crate::shell) observation: Option<zenclash_core::CoreObservationIdentity>,
    pub(in crate::shell) error: Option<String>,
    pub(in crate::shell) command_error: Option<String>,
    pub(in crate::shell) refreshing: bool,
    pub(in crate::shell) refresh_pending: bool,
    pub(in crate::shell) menu_requested: bool,
    pub(in crate::shell) system_proxy_commands: LatestCommandQueue<(bool, u16)>,
    pub(in crate::shell) tun_commands: LatestCommandQueue<bool>,
    pub(in crate::shell) proxy_selection_commands:
        LatestCommandQueue<proxy_commands::TrayProxySelectionRequest>,
    pub(in crate::shell) profile_selection_commands: LatestCommandQueue<String>,
    pub(in crate::shell) icon: Option<NetworkTrayIcon>,
}

impl TrayController {
    pub(in crate::shell) fn new(icon: Option<NetworkTrayIcon>) -> Self {
        Self {
            icon,
            ..Self::default()
        }
    }

    pub(in crate::shell) fn feedback_error(&self) -> Option<String> {
        self.command_error.clone().or_else(|| self.error.clone())
    }
}

impl ZenClashApp {
    pub(in crate::shell) fn start_tray_updates(&mut self, cx: &mut Context<Self>) {
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

    pub(in crate::shell) fn handle_tray_click(&mut self, click: TrayClick, cx: &mut Context<Self>) {
        match click {
            TrayClick::ShowWindow => self.handle_tray_command(TrayCommand::ShowWindow, cx),
            TrayClick::ShowMenu => {
                self.tray.menu_requested = true;
                self.refresh_tray_menu(cx);
            }
        }
    }
}
