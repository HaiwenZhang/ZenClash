use std::{collections::HashMap, path::PathBuf};

use tray_icon::{
    menu::MenuEvent, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use zenclash_core::{format_speed, TrafficSnapshot};

mod icon;
mod menu;

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
    /// Mihomo proxy-group name.
    pub name: String,
    /// Currently selected member.
    pub now: String,
    /// Optional delay-test URL supplied by Mihomo.
    pub test_url: Option<String>,
    /// Selectable group members.
    pub proxies: Vec<TrayProxyNode>,
}

#[derive(Clone, Debug, Default)]
/// Proxy node and its most recent delay used by a tray submenu.
pub struct TrayProxyNode {
    /// Mihomo proxy name.
    pub name: String,
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
        /// Proxy names to test.
        proxies: Vec<String>,
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

/// Native status-bar indicator. The arrows are rendered as a macOS template
/// image and the live upload/download rates are shown beside it.
pub struct NetworkTrayIcon {
    icon: TrayIcon,
    last_title: String,
    commands: HashMap<String, TrayCommand>,
}

impl NetworkTrayIcon {
    /// Creates the native traffic icon and its initial menu.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the icon, menu, or native tray cannot be created.
    pub fn new(core_kind: zenclash_core::CoreKind) -> Result<Self, String> {
        let icon = traffic_icon(0, 0)?;
        let (menu, commands) = build_menu(&TrayMenuState::default())?;
        let tray = TrayIconBuilder::new()
            .with_tooltip(format!("ZenClash · {} 网络流量", core_kind.display_name()))
            .with_title("↑ 0 B/s  ↓ 0 B/s")
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(false)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            icon: tray,
            last_title: String::new(),
            commands,
        })
    }

    /// Updates the title, tooltip, and activity bars when throughput changes.
    ///
    /// # Errors
    ///
    /// Returns a platform error when a native property cannot be updated.
    pub fn update(&mut self, traffic: &TrafficSnapshot) -> Result<(), String> {
        let title = if traffic.connected {
            format!(
                "↑ {}  ↓ {}",
                format_speed(traffic.upload),
                format_speed(traffic.download)
            )
        } else {
            "内核离线".into()
        };
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
            .map_err(|error| error.to_string())
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

    /// Drains menu events until it finds a command owned by this tray.
    #[must_use]
    pub fn next_command(&self) -> Option<TrayCommand> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(command) = self.commands.get(event.id().as_ref()) {
                return Some(command.clone());
            }
        }
        None
    }

    /// Drains icon events until it finds a released click owned by this tray.
    #[must_use]
    pub fn next_click(&self) -> Option<TrayClick> {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                id,
                button,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if id != self.icon.id() {
                    continue;
                }
                return match button {
                    MouseButton::Left => Some(TrayClick::ShowWindow),
                    MouseButton::Right => Some(TrayClick::ShowMenu),
                    MouseButton::Middle => None,
                };
            }
        }
        None
    }

    /// Opens the native status menu programmatically.
    pub fn show_menu(&self) {
        self.icon.show_menu();
    }
}
