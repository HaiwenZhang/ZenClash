use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use gpui::{
    div, prelude::FluentBuilder, px, App, AppContext, ClipboardItem, Context, Entity, EventEmitter,
    Focusable, InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render,
    StatefulInteractiveElement, Styled, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    switch::Switch,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
};
use serde_json::{json, Value};
use zenclash_core::{
    default_pac_script, default_system_proxy_bypass, diff_yaml_configs, format_log_entries,
    format_speed, normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
    AppPreferences, AppPreferencesStore, AutostartStatus, ConfigDiffReport, ConnectionsSnapshot,
    ControlledConfigStore, CoreBinaryInfo, CoreKind, LogMonitor, MihomoClient, MihomoLaunchConfig,
    MihomoLogLevel, MihomoProcess, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, ProfileCatalog, ProfileStore, ProviderCatalog, PublicIpProvider,
    RemoteProfileOptions, RemoteProfileRoute, RuleCatalog, RuntimeConfig, SystemNetworkSnapshot,
    SystemProxyController, SystemProxyManager, SystemProxyMode, SystemProxyStatus,
    TrafficHistoryStore, TrafficMonitor, TrafficSnapshot, TunPermissionGrant, TunPermissionManager,
    TunPermissionStatus, VersionInfo, YamlOverrideCatalog, YamlOverrideStore,
};

use crate::app::{HideTrafficIcon, SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon};

use super::Page;

mod common;
mod config_inputs;
mod connections;
mod dns;
mod home;
mod lifecycle;
mod loader;
mod logs;
mod mihomo;
mod network;
mod overrides;
pub(crate) mod profiles;
mod resources;
mod rules;
mod settings;
mod sniffer;
mod state;
mod system_proxy;
mod traffic;
mod tun;
mod view;

use common::{
    compact_text, config_input_row, empty_dash, empty_state, format_bytes, format_port,
    format_profile_age, format_proxy, info_row, message_banner, metric, normalized_fraction,
    setting_card, setting_switch, yes_no,
};
use config_inputs::ConfigInputs;
use loader::{load_page, load_page_with_binary};
use mihomo::CoreReleaseState;
use overrides::ConfigPreview;
use state::{PageTaskToken, RuntimeData};

/// Stateful GPUI page host for Mihomo runtime, configuration, and diagnostics.
pub struct RuntimePage {
    page: Page,
    core_kind: CoreKind,
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    log_monitor: Arc<LogMonitor>,
    process: Option<Arc<MihomoProcess>>,
    profile_path: Option<PathBuf>,
    profile_store: Option<ProfileStore>,
    controlled_config_store: ControlledConfigStore,
    controlled_config: Value,
    config_inputs: ConfigInputs,
    config_inputs_profile: Option<PathBuf>,
    profile_catalog: ProfileCatalog,
    webdav: settings::webdav::WebDavUiState,
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    core_management: settings::CoreManagementUiState,
    system_proxy_controller: SystemProxyController,
    traffic_history_store: Option<TrafficHistoryStore>,
    profile_forms: profiles::ProfileFormState,
    network_latency_name: Entity<InputState>,
    network_latency_url: Entity<InputState>,
    connection_filter: Entity<InputState>,
    log_filter: Entity<InputState>,
    rule_filter: Entity<InputState>,
    system_proxy_editor: Option<system_proxy::SystemProxyEditorState>,
    core_releases: CoreReleaseState,
    override_store: Option<YamlOverrideStore>,
    override_catalog: YamlOverrideCatalog,
    config_preview: Option<ConfigPreview>,
    profile_editor: overrides::ProfileEditorState,
    data: RuntimeData,
    home_profile_switching: Option<String>,
    home_proxy_switching: Option<(String, String)>,
    live_traffic: LiveTrafficSeries,
    traffic_history: traffic::TrafficHistoryUiState,
    network_probe: network::NetworkProbeUiState,
    ruleset: resources::RulesetUiState,
    navigation_generation: u64,
    load_generation: u64,
    loading: bool,
    mutating: bool,
    closing_connections: HashSet<String>,
    error: Option<String>,
    startup_error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui::FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LiveTrafficSample {
    upload: u64,
    download: u64,
}

const LIVE_TRAFFIC_SAMPLE_COUNT: usize = 24;

#[derive(Debug)]
struct LiveTrafficSeries {
    samples: VecDeque<LiveTrafficSample>,
    last_frame_at_ms: u64,
    connected: bool,
}

impl Default for LiveTrafficSeries {
    fn default() -> Self {
        Self {
            samples: VecDeque::from(vec![
                LiveTrafficSample::default();
                LIVE_TRAFFIC_SAMPLE_COUNT
            ]),
            last_frame_at_ms: 0,
            connected: false,
        }
    }
}

impl LiveTrafficSeries {
    fn samples(&self) -> &VecDeque<LiveTrafficSample> {
        &self.samples
    }

    fn observe(&mut self, snapshot: &TrafficSnapshot) -> bool {
        let connection_changed = self.connected != snapshot.connected;
        self.connected = snapshot.connected;
        if !snapshot.connected
            || snapshot.updated_at_ms == 0
            || snapshot.updated_at_ms == self.last_frame_at_ms
        {
            return connection_changed;
        }

        self.last_frame_at_ms = snapshot.updated_at_ms;
        if self.samples.len() >= LIVE_TRAFFIC_SAMPLE_COUNT {
            self.samples.pop_front();
        }
        self.samples.push_back(LiveTrafficSample {
            upload: snapshot.upload,
            download: snapshot.download,
        });
        true
    }
}

/// Runtime services shared by the native Mihomo management pages.
pub struct RuntimePageServices {
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Typed Mihomo controller client.
    pub client: MihomoClient,
    /// Tokio runtime used for controller and filesystem work.
    pub runtime: tokio::runtime::Handle,
    /// Shared live traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared live log stream.
    pub log_monitor: Arc<LogMonitor>,
    /// Managed Mihomo process, when `ZenClash` launched the core.
    pub process: Option<Arc<MihomoProcess>>,
    /// Active YAML path used by the managed core.
    pub profile_path: Option<PathBuf>,
    /// Persistent controlled-config layer merged over the active profile.
    pub controlled_config_store: ControlledConfigStore,
    /// Persistent native application settings used by real runtime features.
    pub preferences_store: Option<AppPreferencesStore>,
    /// Application preferences loaded before the first page is rendered.
    pub preferences: AppPreferences,
    /// Shared native/PAC controller also used by the app tray.
    pub system_proxy_controller: SystemProxyController,
    /// Native `SQLite` traffic database, when the platform data directory is available.
    pub traffic_history_store: Option<TrafficHistoryStore>,
    /// Visible explanation when startup recovered from the requested core.
    pub startup_notice: Option<String>,
    /// Persistent startup failure while no eligible core/controller is available.
    pub startup_error: Option<String>,
}

/// Event emitted after a managed profile becomes the active Mihomo config.
#[derive(Clone, Debug)]
pub struct ProfileActivated {
    /// Managed YAML path accepted by Mihomo.
    pub path: PathBuf,
}

impl EventEmitter<ProfileActivated> for RuntimePage {}

/// Event emitted after the active member of a Mihomo proxy group changes.
#[derive(Clone, Copy, Debug)]
pub struct ProxySelectionChanged;

impl EventEmitter<ProxySelectionChanged> for RuntimePage {}

/// Event emitted after a controlled runtime configuration is accepted.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfigApplied;

impl EventEmitter<RuntimeConfigApplied> for RuntimePage {}

/// Event emitted after imported application preferences become authoritative.
#[derive(Clone, Debug)]
pub struct PreferencesRestored {
    /// Preferences loaded from the validated backup.
    pub preferences: AppPreferences,
}

impl EventEmitter<PreferencesRestored> for RuntimePage {}

/// Event requesting a graceful exit followed by a Windows RunAs relaunch.
#[derive(Clone, Copy, Debug)]
pub struct ElevatedRestartRequested;

impl EventEmitter<ElevatedRestartRequested> for RuntimePage {}
