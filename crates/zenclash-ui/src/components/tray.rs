use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, menu::MenuEvent,
};
use zenclash_core::{TrafficMonitor, TrafficSnapshot, format_speed};

mod icon;
#[cfg(target_os = "macos")]
mod macos;
mod menu;

pub(crate) const MAX_TRAY_PROXY_NODES: usize = 24;

use icon::traffic_icon;
use menu::build_menu;

#[derive(Clone, Debug, Default)]
/// Complete state used to rebuild the native status menu.
pub struct TrayMenuState {
    /// Active Mihomo routing mode.
    pub mode: String,
    /// Whether an operating-system proxy is enabled.
    pub system_proxy: bool,
    /// Whether Mihomo TUN is enabled.
    pub tun: bool,
    /// Whether the compact traffic window is currently open.
    pub floating_visible: bool,
    /// Preferred local proxy port exposed in environment commands.
    pub mixed_port: u16,
    /// Display name of the active managed profile.
    pub profile_name: String,
    /// Managed profiles available for direct switching.
    pub profiles: Vec<TrayProfile>,
    /// Proxy groups rendered as nested native menus.
    pub groups: Vec<TrayProxyGroup>,
    /// Named directories exposed by the Open Directories submenu.
    pub directories: Vec<(String, PathBuf)>,
}

#[derive(Clone, Debug, Default)]
/// One managed profile shown in the native status menu.
pub struct TrayProfile {
    /// Stable profile identifier used by the profile store.
    pub id: String,
    /// User-facing profile name.
    pub name: String,
    /// Whether this profile is currently active.
    pub active: bool,
}

#[derive(Clone, Debug, Default)]
/// One selectable Mihomo proxy group shown as a submenu.
pub struct TrayProxyGroup {
    /// Whether the group accepts an explicit member selection.
    pub selectable: bool,
    /// Whether a whole-group delay test must restore automatic selection.
    pub automatic: bool,
    /// Whether additional nodes are available on the full proxy page.
    pub has_more: bool,
    /// Mihomo proxy-group name.
    pub name: String,
    /// Currently selected member.
    pub now: String,
    /// Optional delay-test URL supplied by Mihomo.
    pub test_url: Option<String>,
    /// Selectable group members.
    pub proxies: Arc<[TrayProxyNode]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Proxy node and its most recent delay used by a tray submenu.
pub struct TrayProxyNode {
    /// Mihomo proxy name.
    pub name: String,
    /// Provider that supplied this proxy, when applicable.
    pub provider: Option<String>,
    /// Most recent measured delay in milliseconds.
    pub delay: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Shell syntax used when copying proxy environment variables.
pub enum EnvironmentShell {
    /// POSIX-compatible Bash/Zsh syntax.
    Bash,
    /// Windows Command Prompt syntax.
    CommandPrompt,
    /// Windows `PowerShell` syntax.
    PowerShell,
    /// Fish shell syntax.
    Fish,
    /// Nushell syntax.
    Nushell,
}

impl EnvironmentShell {
    const ALL: [Self; 5] = [
        Self::Bash,
        Self::CommandPrompt,
        Self::PowerShell,
        Self::Fish,
        Self::Nushell,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Bash => "Bash / Zsh",
            Self::CommandPrompt => "Command Prompt",
            Self::PowerShell => "PowerShell",
            Self::Fish => "Fish",
            Self::Nushell => "Nushell",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Application command encoded into a native menu item identifier.
pub enum TrayCommand {
    /// Activate the main `ZenClash` window.
    ShowWindow,
    /// Open or close the compact traffic window.
    ToggleFloatingWindow,
    /// Select rule routing mode.
    SetRuleMode,
    /// Select global routing mode.
    SetGlobalMode,
    /// Select direct routing mode.
    SetDirectMode,
    /// Enable or disable the operating-system proxy.
    SetSystemProxy {
        /// Requested enabled state.
        enabled: bool,
        /// Local Mihomo proxy port.
        port: u16,
    },
    /// Enable or disable Mihomo TUN.
    SetTun(bool),
    /// Run delay tests for all supplied group members.
    TestGroup {
        /// Mihomo proxy-group name.
        group: String,
        /// Whether Mihomo automatic selection must be restored after testing.
        automatic: bool,
        /// Optional group-specific test URL.
        test_url: Option<String>,
    },
    /// Select one member of a Mihomo proxy group.
    SelectProxy {
        /// Mihomo proxy-group name.
        group: String,
        /// Proxy member to select.
        proxy: String,
    },
    /// Activate a managed profile and reload its effective YAML.
    SelectProfile {
        /// Stable profile identifier.
        id: String,
    },
    /// Navigate to profile management.
    OpenProfiles,
    /// Navigate to the full proxy-group page.
    OpenProxies,
    /// Open a directory in the platform file manager.
    OpenDirectory(PathBuf),
    /// Copy shell proxy environment variables.
    CopyEnvironment {
        /// Local Mihomo proxy port.
        port: u16,
        /// Target shell syntax.
        shell: EnvironmentShell,
    },
    /// Hide the main app while retaining the status item.
    LightMode,
    /// Relaunch the current executable.
    Restart,
    /// Terminate `ZenClash`.
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Mouse gestures emitted by the native tray icon.
pub enum TrayClick {
    /// Primary click requests the main window.
    ShowWindow,
    /// Secondary click requests the native menu.
    ShowMenu,
}

pub(crate) enum NativeTrayEvent {
    Menu(MenuEvent),
    Click(TrayIconEvent),
}

static NATIVE_EVENT_SENDER: OnceLock<Mutex<Option<mpsc::UnboundedSender<NativeTrayEvent>>>> =
    OnceLock::new();
static NATIVE_EVENT_HANDLERS: OnceLock<()> = OnceLock::new();

fn install_native_event_handlers(sender: mpsc::UnboundedSender<NativeTrayEvent>) {
    *NATIVE_EVENT_SENDER.get_or_init(|| Mutex::new(None)).lock() = Some(sender);
    NATIVE_EVENT_HANDLERS.get_or_init(|| {
        MenuEvent::set_event_handler(Some(|event| {
            if let Some(sender) = NATIVE_EVENT_SENDER
                .get()
                .and_then(|sender| sender.lock().clone())
            {
                let _ = sender.send(NativeTrayEvent::Menu(event));
            }
        }));
        TrayIconEvent::set_event_handler(Some(|event| {
            if !matches!(
                event,
                TrayIconEvent::Click {
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                return;
            }
            if let Some(sender) = NATIVE_EVENT_SENDER
                .get()
                .and_then(|sender| sender.lock().clone())
            {
                let _ = sender.send(NativeTrayEvent::Click(event));
            }
        }));
    });
}

/// Native status-bar indicator. The arrows are rendered as a macOS template
/// image and the live upload/download rates are shown beside it.
pub struct NetworkTrayIcon {
    #[cfg(target_os = "macos")]
    _native_traffic_updater: macos::NativeTrafficUpdater,
    icon: TrayIcon,
    last_title: String,
    commands: HashMap<String, TrayCommand>,
    events: Option<mpsc::UnboundedReceiver<NativeTrayEvent>>,
}

impl NetworkTrayIcon {
    /// Creates the native traffic icon and its initial menu.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the icon, menu, or native tray cannot be created.
    pub fn new(
        core_kind: zenclash_core::CoreKind,
        traffic_monitor: Arc<TrafficMonitor>,
    ) -> Result<Self, String> {
        let (event_sender, events) = mpsc::unbounded_channel();
        install_native_event_handlers(event_sender);
        let icon = traffic_icon(0, 0)?;
        let (menu, commands) = build_menu(&TrayMenuState::default())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip(zenclash_i18n::text_with(
                "tray.tooltip",
                &[("core", core_kind.display_name().to_owned())],
            ))
            .with_title("↑ 0 B/s  ↓ 0 B/s")
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(native_menu_on_right_click())
            .build()
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        let native_traffic_updater = macos::NativeTrafficUpdater::new(
            tray.ns_status_item()
                .ok_or_else(|| "macOS status item is unavailable".to_owned())?,
            traffic_monitor,
        )?;
        #[cfg(not(target_os = "macos"))]
        drop(traffic_monitor);
        Ok(Self {
            #[cfg(target_os = "macos")]
            _native_traffic_updater: native_traffic_updater,
            icon: tray,
            last_title: String::new(),
            commands,
            events: Some(events),
        })
    }

    /// Updates the title, tooltip, and activity bars when throughput changes.
    ///
    /// # Errors
    ///
    /// Returns a platform error when a native property cannot be updated.
    pub fn update(&mut self, traffic: &TrafficSnapshot) -> Result<(), String> {
        let title = traffic_title(traffic);
        if title == self.last_title {
            return Ok(());
        }
        self.icon.set_title(Some(&title));
        self.icon
            .set_tooltip(Some(format!("ZenClash · {title}")))
            .map_err(|error| error.to_string())?;
        let icon = traffic_icon(traffic.upload, traffic.download)?;
        self.icon
            .set_icon_with_as_template(Some(icon), true)
            .map_err(|error| error.to_string())?;
        self.last_title = title;
        Ok(())
    }

    /// Shows or hides the native tray indicator.
    ///
    /// # Errors
    ///
    /// Returns a platform error if visibility cannot be changed.
    pub fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.icon
            .set_visible(visible)
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        if visible {
            self._native_traffic_updater.set_status_item(
                self.icon
                    .ns_status_item()
                    .ok_or_else(|| "macOS status item is unavailable".to_owned())?,
            );
        }
        Ok(())
    }

    /// Rebuilds the menu and atomically replaces its command mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if any native menu item cannot be created or appended.
    pub fn update_menu(&mut self, state: &TrayMenuState) -> Result<(), String> {
        let (menu, commands) = build_menu(state)?;
        self.icon.set_menu(Some(Box::new(menu)));
        self.commands = commands;
        Ok(())
    }

    pub(crate) fn take_event_receiver(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<NativeTrayEvent>> {
        self.events.take()
    }

    pub(crate) fn resolve_event(&self, event: NativeTrayEvent) -> Option<TrayEvent> {
        match event {
            NativeTrayEvent::Menu(event) => self
                .commands
                .get(event.id().as_ref())
                .cloned()
                .map(TrayEvent::Command),
            NativeTrayEvent::Click(TrayIconEvent::Click { id, button, .. })
                if id == self.icon.id() =>
            {
                click_action(button).map(TrayEvent::Click)
            }
            NativeTrayEvent::Click(_) => None,
        }
    }

    /// Opens the native status menu programmatically.
    pub fn show_menu(&self) {
        self.icon.show_menu();
    }
}

impl Drop for NetworkTrayIcon {
    fn drop(&mut self) {
        if let Some(sender) = NATIVE_EVENT_SENDER.get() {
            *sender.lock() = None;
        }
    }
}

pub(crate) enum TrayEvent {
    Command(TrayCommand),
    Click(TrayClick),
}

#[cfg(target_os = "windows")]
const fn native_menu_on_right_click() -> bool {
    true
}

#[cfg(not(target_os = "windows"))]
const fn native_menu_on_right_click() -> bool {
    false
}

const fn click_action(button: MouseButton) -> Option<TrayClick> {
    match button {
        MouseButton::Left => Some(TrayClick::ShowWindow),
        MouseButton::Right => right_click_action(),
        MouseButton::Middle => None,
    }
}

#[cfg(target_os = "windows")]
const fn right_click_action() -> Option<TrayClick> {
    None
}

#[cfg(not(target_os = "windows"))]
const fn right_click_action() -> Option<TrayClick> {
    Some(TrayClick::ShowMenu)
}

fn traffic_title(traffic: &TrafficSnapshot) -> String {
    if traffic.connected {
        format!(
            "↑ {}  ↓ {}",
            format_speed(traffic.upload),
            format_speed(traffic.download)
        )
    } else {
        zenclash_i18n::text("tray.core_offline")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_native_right_click_menu_without_manual_reentry() {
        assert!(native_menu_on_right_click());
        assert_eq!(click_action(MouseButton::Right), None);
    }

    #[test]
    fn connected_traffic_title_formats_both_directions() {
        let title = traffic_title(&TrafficSnapshot {
            upload: 1024,
            download: 2 * 1024 * 1024,
            connected: true,
            ..TrafficSnapshot::default()
        });

        assert_eq!(title, "↑ 1.0 KiB/s  ↓ 2.0 MiB/s");
    }

    #[test]
    fn disconnected_traffic_title_uses_offline_copy() {
        assert_eq!(
            traffic_title(&TrafficSnapshot::default()),
            zenclash_i18n::text("tray.core_offline")
        );
    }
}
