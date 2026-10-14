use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, menu::MenuEvent,
};

mod command_bindings;
mod icon;
use command_bindings::TrayCommandBindings;
#[cfg(target_os = "macos")]
mod macos;
mod menu;

pub(crate) const MAX_TRAY_PROXY_NODES: usize = 24;

use icon::app_icon;
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
#[non_exhaustive]
pub struct TrayProxyGroup {
    /// Controller group key; may differ from its user-facing label.
    pub controller_name: String,
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

impl TrayProxyGroup {
    pub(crate) fn controller_name(&self) -> &str {
        if self.controller_name.is_empty() {
            &self.name
        } else {
            &self.controller_name
        }
    }
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
    /// Relaunch the current executable.
    Restart,
    /// Terminate `ZenClash`.
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Mouse gestures emitted by the native tray icon.
pub enum TrayClick {
    /// Primary click activates the main window.
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

/// Native application tray icon and its command menu.
pub struct NetworkTrayIcon {
    icon: TrayIcon,
    commands: TrayCommandBindings,
    events: Option<mpsc::UnboundedReceiver<NativeTrayEvent>>,
}

impl NetworkTrayIcon {
    /// Creates the native application icon and its initial menu.
    ///
    /// # Errors
    ///
    /// Returns a platform error when the icon, menu, or native tray cannot be created.
    pub fn new() -> Result<Self, String> {
        let (event_sender, events) = mpsc::unbounded_channel();
        install_native_event_handlers(event_sender);
        let icon = app_icon()?;
        let (menu, commands) = build_menu(&TrayMenuState::default())?;
        let builder = TrayIconBuilder::new()
            .with_tooltip("ZenClash")
            .with_title("");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon);
        let tray = builder
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(native_menu_on_right_click())
            .build()
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        macos::prepare_app_icon(&tray);
        Ok(Self {
            icon: tray,
            commands: TrayCommandBindings::new(commands, None),
            events: Some(events),
        })
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
            macos::prepare_app_icon(&self.icon);
        }
        Ok(())
    }

    /// Rebuilds the menu and atomically replaces its command mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if any native menu item cannot be created or appended.
    pub fn update_menu(&mut self, state: &TrayMenuState) -> Result<(), String> {
        self.update_menu_with_observation(state, None)
    }

    pub(crate) fn update_menu_with_observation(
        &mut self,
        state: &TrayMenuState,
        observation: Option<zenclash_core::CoreObservationIdentity>,
    ) -> Result<(), String> {
        let (menu, commands) = build_menu(state)?;
        self.icon.set_menu(Some(Box::new(menu)));
        self.commands = TrayCommandBindings::new(commands, observation);
        Ok(())
    }

    pub(crate) fn take_event_receiver(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<NativeTrayEvent>> {
        self.events.take()
    }

    pub(crate) fn resolve_event(&self, event: NativeTrayEvent) -> Option<TrayEvent> {
        match event {
            NativeTrayEvent::Menu(event) => self.commands.resolve(event.id().as_ref()),
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
    Command(TrayCommand, Option<zenclash_core::CoreObservationIdentity>),
    Click(TrayClick),
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
const fn native_menu_on_right_click() -> bool {
    true
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
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

#[cfg(any(target_os = "windows", target_os = "linux"))]
const fn right_click_action() -> Option<TrayClick> {
    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
const fn right_click_action() -> Option<TrayClick> {
    Some(TrayClick::ShowMenu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn native_right_click_menu_does_not_require_manual_reentry() {
        assert!(native_menu_on_right_click());
        assert_eq!(click_action(MouseButton::Right), None);
    }

    #[test]
    fn primary_tray_click_shows_the_main_window() {
        assert_eq!(click_action(MouseButton::Left), Some(TrayClick::ShowWindow));
        assert_eq!(click_action(MouseButton::Middle), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn secondary_tray_click_requests_the_native_menu() {
        assert!(!native_menu_on_right_click());
        assert_eq!(click_action(MouseButton::Right), Some(TrayClick::ShowMenu));
    }
}
