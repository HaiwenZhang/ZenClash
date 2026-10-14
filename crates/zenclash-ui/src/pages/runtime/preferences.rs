use super::{AppPreferences, Context, PreferencesRestored, RuntimePage};
use zenclash_core::CoreKind;

/// Fields owned by a completed preference operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceScope {
    /// A validated backup explicitly replaces all preferences.
    Restore,
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

impl RuntimePage {
    #[cfg(test)]
    pub(crate) fn presented_preferences(&self) -> &AppPreferences {
        &self.preferences
    }

    pub(crate) fn merge_preferences_from_app(
        &mut self,
        preferences: &AppPreferences,
        scopes: &[PreferenceScope],
        previews: crate::preference_previews::PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        for scope in scopes {
            scope.apply(&mut self.preferences, preferences);
        }
        self.preference_previews = previews;
        self.preference_previews.apply(&mut self.preferences);
        cx.notify();
    }

    pub(crate) fn set_preference_previews(
        &mut self,
        previews: crate::preference_previews::PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        self.preference_previews = previews;
        self.preference_previews.apply(&mut self.preferences);
        cx.notify();
    }

    pub(crate) fn report_preferences_refresh_error(
        &mut self,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.error = Some(error);
        cx.notify();
    }

    pub(in crate::pages::runtime) fn publish_restored_preferences(
        &mut self,
        observation: zenclash_core::AppPreferencesObservation,
        token: super::PageTaskToken,
        cx: &mut Context<Self>,
    ) {
        if self
            .preferences_store
            .as_ref()
            .is_some_and(|store| !observation.belongs_to(store))
        {
            self.finish_mutation(token);
            self.set_page_error(
                token,
                zenclash_core::AppPreferencesError::ConcurrentModification.to_string(),
            );
            return;
        }
        let accepted = observation
            .publish_if_current(|preferences| {
                self.preferences = preferences.clone();
                self.preference_previews.apply(&mut self.preferences);
                self.system_proxy_editor = None;
                cx.emit(PreferencesRestored::restored(observation.clone()));
            })
            .is_some();
        if accepted {
            self.finish_mutation(token);
            return;
        }
        let path = observation.path().to_owned();
        let task = self.runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tokio::task::spawn_blocking(move || {
                zenclash_core::AppPreferencesStore::new(path).observe()
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(observation) => this.publish_restored_preferences(observation, token, cx),
                    Err(error) => {
                        this.finish_mutation(token);
                        this.set_page_error(token, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn accept_preferences(
        &mut self,
        preferences: AppPreferences,
        scope: PreferenceScope,
        cx: &mut Context<Self>,
    ) {
        scope.apply(&mut self.preferences, &preferences);
        cx.emit(PreferencesRestored::new(preferences, scope));
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
