//! Stateful GPUI host (`RuntimePage`) aggregating horizontally isolated feature views.

use std::path::PathBuf;

use gpui_kit::{Entity, EventEmitter, Subscription};
use serde_json::Value;
use zenclash_core::{
    AppPreferences, AppPreferencesStore, ControlledConfigStore, CoreKind, CoreSession,
    MihomoClient, SystemProxySession,
};

use crate::foundation::actions::Page;
use crate::foundation::primitives::preference_previews::PreferencePreviews;
use crate::runtime_host::busy::MutationState;
use crate::runtime_host::config_inputs::ConfigInputs;
use crate::runtime_host::lifecycle_schedule::UiVisibility;
use crate::runtime_host::observation::PageObservationOwner;
use crate::runtime_host::services::RuntimeMonitors;

pub use crate::runtime_host::services::{
    ContinueLocalRequested, PreferenceScope, PreferencesRestored, ProfileActivated, ProfileCleared,
    ProxySelectionChanged, RuntimeConfigApplied, RuntimePageServices,
    ServiceRepairRestartRequested,
};

pub(in crate::features) mod config_inputs;
pub(in crate::features) mod feedback;
pub(in crate::features) mod form_helpers;
pub(in crate::features) mod lifecycle;
pub(in crate::features) mod view;

#[cfg(test)]
pub(crate) mod ui_tests;

pub(in crate::features) struct SettingsWorkspaceState {
    pub(in crate::features) controlled_config: Value,
    pub(in crate::features) controlled_config_generation: u64,
    pub(in crate::features) core_management: crate::features::settings::CoreManagementUiState,
    pub(in crate::features) core_releases: crate::features::kernel::mihomo::CoreReleaseState,
    pub(in crate::features) system_proxy_editor:
        Option<crate::features::capture::system_proxy::SystemProxyEditorState>,
    pub(in crate::features) system_proxy_session: Option<SystemProxySession>,
    pub(in crate::features) preference_previews: PreferencePreviews,
}

pub(in crate::features) struct LibraryPagesState {
    pub(in crate::features) resources: crate::features::resources::ResourcesState,
    pub(in crate::features) overrides: crate::features::overrides::OverridesState,
}

pub(in crate::features) struct ShellPresentationState {
    pub(in crate::features) feedback_notifications:
        crate::components::feedback::FeedbackNotifications,
    pub(in crate::features) body_scroll: gpui_kit::ScrollHandle,
    pub(in crate::features) window_handle: gpui_kit::AnyWindowHandle,
    pub(in crate::features) ui_visibility: UiVisibility,
    pub(in crate::features) live_updates_enabled: tokio::sync::watch::Sender<bool>,
    pub(in crate::features) startup_error: Option<String>,
    pub(in crate::features) tasks: Vec<gpui_kit::Task<()>>,
    pub(in crate::features) _subscriptions: Vec<Subscription>,
}

/// Stateful GPUI page host for Mihomo runtime, configuration, and diagnostics.
pub struct RuntimePage {
    pub(in crate::features) page: Page,
    pub(in crate::features) core_kind: CoreKind,
    pub(in crate::features) core_session: CoreSession,
    pub(in crate::features) application_commands: crate::ApplicationCommands,
    pub(in crate::features) client: MihomoClient,
    pub(in crate::features) runtime: tokio::runtime::Handle,
    pub(in crate::features) monitors: RuntimeMonitors,
    pub(in crate::features) settings: SettingsWorkspaceState,
    pub(in crate::features) library: LibraryPagesState,
    pub(in crate::features) shell: ShellPresentationState,
    pub(in crate::features) profile_path: Option<PathBuf>,
    pub(in crate::features) controlled_config_store: ControlledConfigStore,
    pub(crate) config_inputs: ConfigInputs,
    pub(crate) persistent_loading: bool,
    pub(in crate::features) preferences_store: Option<AppPreferencesStore>,
    pub(in crate::features) preferences: AppPreferences,
    pub(in crate::features) profiles: crate::features::profiles::ProfileLibrary,
    pub(in crate::features) connections: Entity<crate::features::connections::ConnectionsView>,
    pub(in crate::features) logs: Entity<crate::features::logs::LogsView>,
    pub(in crate::features) rules: Entity<crate::features::rules::RulesView>,
    pub(in crate::features) page_observation: PageObservationOwner,
    pub(in crate::features) home: crate::features::home::HomeUiState,
    pub(in crate::features) traffic_history: crate::features::traffic::TrafficHistory,
    pub(in crate::features) network_diagnostics:
        crate::features::network::NetworkDiagnosticsUiState,
    pub(in crate::features) navigation_generation: u64,
    pub(crate) loading: bool,
    pub(in crate::features) mutations: MutationState,
    pub(crate) error: Option<String>,
    pub(in crate::features) notice: Option<String>,
    pub(in crate::features) focus_handle: gpui_kit::FocusHandle,
}

impl EventEmitter<ProfileActivated> for RuntimePage {}
impl EventEmitter<ProfileCleared> for RuntimePage {}
impl EventEmitter<ProxySelectionChanged> for RuntimePage {}
impl EventEmitter<RuntimeConfigApplied> for RuntimePage {}
impl EventEmitter<ContinueLocalRequested> for RuntimePage {}
impl EventEmitter<ServiceRepairRestartRequested> for RuntimePage {}
impl EventEmitter<PreferencesRestored> for RuntimePage {}

#[cfg(test)]
impl RuntimePage {
    pub(crate) fn connections_idle(&self, cx: &gpui_kit::App) -> bool {
        self.connections.read(cx).is_idle()
    }
}
