use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    switch::Switch,
    v_flex,
};
#[cfg(test)]
use gpui_kit::px;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, Focusable,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
};
use serde_json::{Value, json};
use zenclash_core::{
    AppPreferences, AppPreferencesStore, AutostartStatus, ControlledConfigStore, CoreKind,
    CoreSession, DiagnosticData, DiagnosticRoute, DiagnosticStep, DiagnosticStepKind, LogMonitor,
    LogTimeSource, MihomoClient, MihomoLogLevel, NetworkLatencyTarget, NetworkProbeRoutePreference,
    NetworkProbeSnapshot, OperationalStatus, ProfileCatalog, ProfileStore, ProviderCatalog,
    ProviderKind, ProviderOperations, PublicIpProvider, RemoteProfileOptions, RemoteProfileRoute,
    RuntimeConfig, SystemNetworkSnapshot, SystemProxyMode, SystemProxySession, SystemProxySummary,
    TrafficCaptureSession, TrafficHistoryStore, TrafficMonitor, VersionInfo, YamlOverrideCatalog,
    YamlOverrideStore, default_pac_script, default_system_proxy_bypass, format_speed,
    normalize_pac_script, normalize_system_proxy_bypass, normalize_system_proxy_host,
};
#[cfg(test)]
use zenclash_core::{ConnectionsSnapshot, CoreLaunchConfig};

use crate::app::{HideTrafficIcon, SetDarkTheme, SetLightTheme, SetSystemTheme, ShowTrafficIcon};

use super::Page;
#[cfg(all(test, target_os = "windows"))]
use zenclash_core::SystemProxyStatus;

mod busy;
mod common;
mod config_inputs;
mod connections;
mod dns;
mod feedback;
mod home;
mod lifecycle;
mod logs;
mod mihomo;
mod network;
mod overrides;
mod page_observation;
mod page_task;
pub(crate) mod profiles;
mod resources;
mod rules;
mod settings;
mod sniffer;
mod system_proxy;
mod traffic;
mod tun;
mod view;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
pub(crate) mod ui_tests;

use common::{
    contains_ascii_case_insensitive, context_note, empty_dash, empty_state, format_bytes,
    format_profile_age, format_proxy, info_row, list_page, metric, normalized_fraction,
    pagination_summary, setting_card, yes_no,
};
use config_inputs::{ConfigInputs, config_input_snapshot};
use mihomo::CoreReleaseState;
use page_observation::{PageSnapshotData, load_page};
use page_task::PageTaskToken;

/// Stateful GPUI page host for Mihomo runtime, configuration, and diagnostics.
pub struct RuntimePage {
    feedback_notifications: crate::components::feedback::FeedbackNotifications,
    page: Page,
    core_kind: CoreKind,
    core_session: CoreSession,
    application_commands: crate::ApplicationCommands,
    client: MihomoClient,
    runtime: tokio::runtime::Handle,
    traffic_monitor: Arc<TrafficMonitor>,
    log_monitor: Arc<LogMonitor>,
    operational_status: Arc<OperationalStatus>,
    traffic_capture: TrafficCaptureSession,
    profile_path: Option<PathBuf>,
    controlled_config_store: ControlledConfigStore,
    controlled_config: Value,
    config_inputs: ConfigInputs,
    persistent_loading: bool,
    preferences_store: Option<AppPreferencesStore>,
    preferences: AppPreferences,
    preference_previews: crate::preference_previews::PreferencePreviews,
    core_management: settings::CoreManagementUiState,
    body_scroll: gpui_kit::ScrollHandle,
    resources: resources::ResourcesState,
    system_proxy_session: Option<SystemProxySession>,
    profiles: profiles::ProfileLibrary,
    overrides: overrides::OverridesState,
    connections: Entity<connections::ConnectionsView>,
    logs: Entity<logs::LogsView>,
    rules: Entity<rules::RulesView>,
    system_proxy_editor: Option<system_proxy::SystemProxyEditorState>,
    core_releases: CoreReleaseState,
    page_observation: page_observation::PageObservationOwner,
    home: home::HomeUiState,
    traffic_history: traffic::TrafficHistory,
    network_diagnostics: network::NetworkDiagnosticsUiState,
    provider_operations: ProviderOperations,
    navigation_generation: u64,
    controlled_config_generation: u64,
    loading: bool,
    mutations: busy::MutationState,
    error: Option<String>,
    startup_error: Option<String>,
    notice: Option<String>,
    focus_handle: gpui_kit::FocusHandle,
    window_handle: AnyWindowHandle,
    ui_visibility: lifecycle::UiVisibility,
    live_updates_enabled: tokio::sync::watch::Sender<bool>,
    _subscriptions: Vec<Subscription>,
}

/// Runtime services shared by the native Mihomo management pages.
pub struct RuntimePageServices {
    /// Profile repository opened during bootstrap and shared with local pages.
    pub profile_store: Option<ProfileStore>,
    /// YAML override repository opened during bootstrap and shared with local pages.
    pub override_store: Option<YamlOverrideStore>,
    /// Explicit runtime core selected for this application process.
    pub core_kind: CoreKind,
    /// Serialized runtime-core transition owner.
    pub core_session: CoreSession,
    /// Shared application commands and recovery record; field name retained for compatibility.
    pub profile_service: crate::ApplicationCommands,
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
    /// Accepted core generation carried by the originating transaction.
    pub runtime_version: u64,
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

/// Explicit session choice to restart with an ordinary local kernel.
#[derive(Clone, Copy, Debug)]
pub struct ContinueLocalRequested;

impl EventEmitter<ContinueLocalRequested> for RuntimePage {}

/// Requests normal startup again after confirmed offline service repair.
#[derive(Clone, Copy, Debug)]
pub struct ServiceRepairRestartRequested;

impl EventEmitter<ServiceRepairRestartRequested> for RuntimePage {}

/// Event emitted after persisted application preference fields become authoritative.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PreferencesRestored {
    /// Fields made authoritative by this operation; only backup restore replaces all fields.
    pub scope: PreferenceScope,
    /// Persisted snapshot whose fields are selected by `scope`.
    pub preferences: AppPreferences,
    observation: Option<zenclash_core::AppPreferencesObservation>,
}

impl PreferencesRestored {
    /// Creates a scoped preference completion; restore callers use a persisted observation.
    pub fn new(preferences: AppPreferences, scope: PreferenceScope) -> Self {
        Self {
            preferences,
            scope,
            observation: None,
        }
    }
    pub(crate) fn observed(
        observation: zenclash_core::AppPreferencesObservation,
        scope: PreferenceScope,
    ) -> Self {
        Self {
            preferences: observation.preferences().clone(),
            scope,
            observation: Some(observation),
        }
    }
    pub(crate) fn restored(observation: zenclash_core::AppPreferencesObservation) -> Self {
        Self::observed(observation, PreferenceScope::Restore)
    }
    pub(crate) fn observation(&self) -> Option<&zenclash_core::AppPreferencesObservation> {
        self.observation.as_ref()
    }
}

impl EventEmitter<PreferencesRestored> for RuntimePage {}

mod preferences;
pub use preferences::PreferenceScope;
