use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, EventEmitter, Focusable, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    switch::Switch,
    v_flex,
};
use serde_json::{Value, json};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AutostartStatus, ConfigDiffReport, ConnectionsSnapshot,
    ControlledConfigStore, CoreBinaryInfo, CoreKind, CoreSession, DiagnosticData, DiagnosticReport,
    DiagnosticRoute, DiagnosticStep, DiagnosticStepKind, LogMonitor, LogTimeSource, MihomoClient,
    MihomoLaunchConfig, MihomoLogLevel, MihomoProcess, NetworkLatencyTarget,
    NetworkProbeRoutePreference, NetworkProbeSnapshot, Observation, OperationalStatus,
    ProfileCatalog, ProfileStore, ProviderCatalog, ProviderKind, ProviderOperations,
    ProxyOperations, ProxyVisibility, PublicIpProvider, RecoveryAction, RemoteProfileOptions,
    RemoteProfileRoute, RuleCatalog, RuntimeConfig, SystemNetworkSnapshot, SystemProxyManager,
    SystemProxyMode, SystemProxySession, SystemProxyStatus, TrafficCaptureSession,
    TrafficHistoryStore, TrafficMonitor, TunPermissionManager, TunPermissionStatus, VersionInfo,
    YamlOverrideCatalog, YamlOverrideStore, default_pac_script, default_system_proxy_bypass,
    diff_yaml_configs, format_log_entries, format_log_entries_support_safe, format_speed,
    normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
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
    core_session: CoreSession,
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    log_monitor: Arc<LogMonitor>,
    operational_status: Arc<OperationalStatus>,
    traffic_capture: TrafficCaptureSession,
    process: Option<Arc<MihomoProcess>>,
    profile_path: Option<PathBuf>,
    profile_store: Option<ProfileStore>,
    controlled_config_store: ControlledConfigStore,
    controlled_config: Value,
    config_inputs: ConfigInputs,
    config_inputs_profile: Option<PathBuf>,
    profile_catalog: ProfileCatalog,
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    core_management: settings::CoreManagementUiState,
    app_update: settings::AppUpdateUiState,
    system_proxy_session: Option<SystemProxySession>,
    traffic_history_store: Option<TrafficHistoryStore>,
    profile_forms: profiles::ProfileFormState,
    connections: connections::ConnectionsUiState,
    logs: logs::LogUiState,
    rules: rules::RulesUiState,
    system_proxy_editor: Option<system_proxy::SystemProxyEditorState>,
    core_releases: CoreReleaseState,
    override_store: Option<YamlOverrideStore>,
    override_catalog: YamlOverrideCatalog,
    config_preview: Option<ConfigPreview>,
    profile_editor: overrides::ProfileEditorState,
    data: RuntimeData,
    home: home::HomeUiState,
    traffic_history: traffic::TrafficHistoryUiState,
    network_probe: network::NetworkProbeUiState,
    provider_operations: ProviderOperations,
    ruleset: resources::RulesetUiState,
    navigation_generation: u64,
    load_generation: u64,
    loading: bool,
    mutating: bool,
    error: Option<String>,
    startup_error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui::FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// Runtime services shared by the native Mihomo management pages.
pub struct RuntimePageServices {
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Serialized runtime-core transition owner.
    pub core_session: CoreSession,
    /// Typed Mihomo controller client.
    pub client: MihomoClient,
    /// Tokio runtime used for controller and filesystem work.
    pub runtime: tokio::runtime::Handle,
    /// Shared live traffic stream.
    pub traffic_monitor: Arc<TrafficMonitor>,
    /// Shared live log stream.
    pub log_monitor: Arc<LogMonitor>,
    /// Shared four-layer runtime and stream observation owner.
    pub operational_status: Arc<OperationalStatus>,
    /// Serialized System Proxy/TUN capture plan owner.
    pub traffic_capture: TrafficCaptureSession,
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
    /// Persistent native/PAC transaction owner used for editing proxy settings.
    pub system_proxy_session: Option<SystemProxySession>,
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
