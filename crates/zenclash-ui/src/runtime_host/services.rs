//! Runtime page service dependencies, monitor bundle, preference scopes, and host events.

use std::{path::PathBuf, sync::Arc};

use zenclash_core::{
    AppPreferences, AppPreferencesStore, ControlledConfigStore, CoreKind, CoreSession, LogMonitor,
    MihomoClient, OperationalStatus, ProfileStore, ProviderOperations, SystemProxySession,
    TrafficCaptureSession, TrafficHistoryStore, TrafficMonitor, YamlOverrideStore,
};

pub(crate) struct RuntimeMonitors {
    pub(crate) traffic_monitor: Arc<TrafficMonitor>,
    pub(crate) log_monitor: Arc<LogMonitor>,
    pub(crate) operational_status: Arc<OperationalStatus>,
    pub(crate) traffic_capture: TrafficCaptureSession,
    pub(crate) provider_operations: ProviderOperations,
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

/// Event emitted after the sole active profile is cleared with no fallback file.
#[derive(Clone, Copy, Debug)]
pub struct ProfileCleared {
    /// Accepted core generation carried by the clearing transaction.
    pub runtime_version: u64,
}

/// Event emitted after the active member of a Mihomo proxy group changes.
#[derive(Clone, Copy, Debug)]
pub struct ProxySelectionChanged;

/// Event emitted after a controlled runtime configuration is accepted.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfigApplied;

/// Explicit session choice to restart with an ordinary local kernel.
#[derive(Clone, Copy, Debug)]
pub struct ContinueLocalRequested;

/// Requests normal startup again after confirmed offline service repair.
#[derive(Clone, Copy, Debug)]
pub struct ServiceRepairRestartRequested;

/// Fields owned by a completed preference operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceScope {
    /// A validated backup explicitly replaces all preferences.
    Restore,
    /// Interface appearance mode.
    Appearance,
    /// Native status tray icon visibility.
    TrayVisibility,
    /// Interface language.
    Language,
    /// Continuous log persistence settings.
    Logs,
    /// Traffic accounting and retention settings.
    TrafficHistory,
    /// Network diagnostics preferences.
    Network,
    /// Native proxy configuration and ownership receipt.
    SystemProxy,
    /// Executable selection for one backend.
    CoreBinary(CoreKind),
}

impl PreferenceScope {
    /// Merges only the fields owned by this operation, preserving newer unrelated edits.
    pub fn apply(self, current: &mut AppPreferences, saved: &AppPreferences) {
        match self {
            Self::Restore => *current = saved.clone(),
            Self::Appearance => current.appearance = saved.appearance,
            Self::TrayVisibility => current.traffic_tray_visible = saved.traffic_tray_visible,
            Self::Language => current.language = saved.language,
            Self::Logs => {
                current.log_file_enabled = saved.log_file_enabled;
                current.log_file_max_mebibytes = saved.log_file_max_mebibytes;
            }
            Self::TrafficHistory => {
                current.traffic_history_enabled = saved.traffic_history_enabled;
                current.traffic_retention_days = saved.traffic_retention_days;
            }
            Self::Network => {
                current.network_ip_provider = saved.network_ip_provider;
                current.network_probe_route = saved.network_probe_route;
                current
                    .network_latency_targets
                    .clone_from(&saved.network_latency_targets);
            }
            Self::SystemProxy => {
                current.system_proxy_enabled = saved.system_proxy_enabled;
                current
                    .system_proxy_ownership
                    .clone_from(&saved.system_proxy_ownership);
                current
                    .system_proxy_bypass
                    .clone_from(&saved.system_proxy_bypass);
                current.system_proxy_mode = saved.system_proxy_mode;
                current
                    .system_proxy_host
                    .clone_from(&saved.system_proxy_host);
                current
                    .system_proxy_pac_script
                    .clone_from(&saved.system_proxy_pac_script);
            }
            Self::CoreBinary(kind) => current.core_binaries.set(
                kind,
                saved
                    .core_binaries
                    .path(kind)
                    .map(std::path::Path::to_path_buf),
            ),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use zenclash_core::{AppearancePreference, LanguagePreference};

    #[test]
    fn delayed_log_save_preserves_newer_appearance_and_language() {
        let saved = AppPreferences {
            log_file_enabled: true,
            ..Default::default()
        };
        let mut current = AppPreferences {
            appearance: AppearancePreference::Dark,
            language: LanguagePreference::En,
            ..Default::default()
        };
        PreferenceScope::Logs.apply(&mut current, &saved);
        assert!(current.log_file_enabled);
        assert_eq!(current.appearance, AppearancePreference::Dark);
        assert_eq!(current.language, LanguagePreference::En);
    }

    #[test]
    fn saving_core_binary_preserves_unrelated_preferences() {
        let mut current = AppPreferences {
            appearance: AppearancePreference::Dark,
            ..Default::default()
        };
        let mut saved = AppPreferences::default();
        saved
            .core_binaries
            .set(CoreKind::Mihomo, Some("new-mihomo".into()));
        PreferenceScope::CoreBinary(CoreKind::Mihomo).apply(&mut current, &saved);
        assert_eq!(
            current.core_binaries.path(CoreKind::Mihomo),
            Some(std::path::Path::new("new-mihomo"))
        );
        assert_eq!(current.appearance, AppearancePreference::Dark);
    }

    #[test]
    fn narrow_appearance_and_tray_scopes_preserve_unrelated_preferences() {
        let saved = AppPreferences {
            appearance: AppearancePreference::Dark,
            traffic_tray_visible: false,
            language: LanguagePreference::En,
            ..Default::default()
        };
        let mut current = AppPreferences {
            appearance: AppearancePreference::Light,
            traffic_tray_visible: true,
            language: LanguagePreference::ZhCn,
            ..Default::default()
        };
        PreferenceScope::Appearance.apply(&mut current, &saved);
        assert_eq!(current.appearance, AppearancePreference::Dark);
        assert!(current.traffic_tray_visible);
        assert_eq!(current.language, LanguagePreference::ZhCn);

        PreferenceScope::TrayVisibility.apply(&mut current, &saved);
        assert!(!current.traffic_tray_visible);
        assert_eq!(current.language, LanguagePreference::ZhCn);
    }

    #[test]
    fn explicit_backup_restore_replaces_the_entire_snapshot() {
        let saved = AppPreferences::default();
        let mut current = AppPreferences {
            appearance: AppearancePreference::Dark,
            ..saved.clone()
        };
        PreferenceScope::Restore.apply(&mut current, &saved);
        assert_eq!(current, saved);
    }
}
