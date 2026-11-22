//! Application adoption of persisted preference events and independently retained previews.

use std::time::Duration;

use gpui_kit::AppContext;
use zenclash_core::{AppPreferences, AppPreferencesError, AppPreferencesStore};

use crate::foundation::design::apply_zen_theme;
use crate::runtime_host::services::PreferenceScope;
use crate::runtime_host::services::PreferencesRestored;
use crate::shell::app::ZenClashApp;
use crate::shell::bootstrap;
use gpui_kit::Context;
use gpui_kit::component::ThemeMode;
use zenclash_core::AppearancePreference;

pub(in crate::shell) const MAX_RECONCILIATION_RETRIES: u32 = 10;

pub(in crate::shell) fn reconciliation_backoff(attempt: u32) -> Duration {
    let shift = attempt.min(8);
    let millis = 10_u64.saturating_mul(1_u64 << shift).min(2_000);
    Duration::from_millis(millis)
}

#[derive(Clone)]
pub(in crate::shell) struct PreferenceReconciliationRead {
    pub(in crate::shell) identity: std::sync::Arc<()>,
    scopes: Vec<PreferenceScope>,
    attempt: u32,
}

/// Consolidates preference store, current snapshot, serialized menu saves, and reconciliation state.
pub(in crate::shell) struct PreferenceController {
    pub(in crate::shell) store: Option<AppPreferencesStore>,
    pub(in crate::shell) current: AppPreferences,
    pub(in crate::shell) save_task: Option<tokio::task::JoinHandle<()>>,
    pub(in crate::shell) menu_saves:
        crate::shell::controllers::menu_preferences::MenuPreferenceSaves,
    pub(in crate::shell) reconciliation_read: Option<PreferenceReconciliationRead>,
}

impl PreferenceController {
    pub(in crate::shell) fn new(
        store: Option<AppPreferencesStore>,
        current: AppPreferences,
    ) -> Self {
        Self {
            store,
            current,
            save_task: None,
            menu_saves: Default::default(),
            reconciliation_read: None,
        }
    }
}

impl ZenClashApp {
    pub(in crate::shell) fn accept_preferences_event(
        &mut self,
        event: &PreferencesRestored,
        cx: &mut Context<Self>,
    ) {
        if let Some(observation) = event.observation() {
            if self
                .preferences
                .store
                .as_ref()
                .is_some_and(|store| !observation.belongs_to(store))
            {
                return;
            }
            if event.scope == PreferenceScope::Restore {
                self.preferences.reconciliation_read = None;
            }
            if !self.adopt_observed_preferences(observation, &[event.scope], cx) {
                self.refresh_persisted_preferences(
                    observation.path().to_owned(),
                    &[event.scope],
                    cx,
                );
            }
        } else if event.scope == PreferenceScope::Restore {
            if let Some(store) = self.preferences.store.as_ref() {
                self.refresh_persisted_preferences(store.path().to_owned(), &[event.scope], cx);
            }
        } else {
            let previous_language = self.preferences.current.language;
            event
                .scope
                .apply(&mut self.preferences.current, &event.preferences);
            self.apply_preference_effects(event.scope, previous_language, cx);
        }
    }

    fn adopt_observed_preferences(
        &mut self,
        observation: &zenclash_core::AppPreferencesObservation,
        scopes: &[PreferenceScope],
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .preferences
            .store
            .as_ref()
            .is_some_and(|store| !observation.belongs_to(store))
        {
            return false;
        }
        let previous_language = self.preferences.current.language;
        let accepted = observation
            .publish_if_current(|preferences| {
                for scope in scopes {
                    scope.apply(&mut self.preferences.current, preferences);
                }
                self.preferences
                    .menu_saves
                    .previews()
                    .apply(&mut self.preferences.current);
            })
            .is_some();
        if !accepted {
            return false;
        }
        let preferences = self.preferences.current.clone();
        let previews = self.preferences.menu_saves.previews();
        self.runtime_page.update(cx, |page, cx| {
            page.merge_preferences_from_app(&preferences, scopes, previews, cx)
        });
        for scope in scopes {
            self.apply_preference_effects(*scope, previous_language, cx);
        }
        true
    }

    pub(in crate::shell) fn refresh_persisted_preferences(
        &mut self,
        path: std::path::PathBuf,
        scopes: &[PreferenceScope],
        cx: &mut Context<Self>,
    ) {
        self.schedule_persisted_preferences_refresh(path, scopes, 0, cx);
    }

    fn schedule_persisted_preferences_refresh(
        &mut self,
        path: std::path::PathBuf,
        scopes: &[PreferenceScope],
        attempt: u32,
        cx: &mut Context<Self>,
    ) {
        if attempt >= MAX_RECONCILIATION_RETRIES {
            tracing::warn!(
                attempt,
                "preference reconciliation reached maximum retries; stopping"
            );
            self.preferences.reconciliation_read = None;
            self.runtime_page.update(cx, |page, cx| {
                page.report_preferences_refresh_error(
                    AppPreferencesError::ConcurrentModification.to_string(),
                    cx,
                )
            });
            cx.notify();
            return;
        }
        let mut requested = self
            .preferences
            .reconciliation_read
            .as_ref()
            .map_or_else(Vec::new, |request| request.scopes.clone());
        for scope in scopes {
            if *scope == PreferenceScope::Restore {
                requested.clear();
                requested.push(*scope);
                break;
            }
            if !requested.contains(&PreferenceScope::Restore) && !requested.contains(scope) {
                requested.push(*scope);
            }
        }
        let request = PreferenceReconciliationRead {
            identity: std::sync::Arc::new(()),
            scopes: requested,
            attempt,
        };
        self.preferences.reconciliation_read = Some(request.clone());
        let delay = reconciliation_backoff(attempt);
        let task = self.services.runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            tokio::task::spawn_blocking(move || AppPreferencesStore::new(path).observe())
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
                if !this
                    .preferences
                    .reconciliation_read
                    .as_ref()
                    .is_some_and(|current| {
                        std::sync::Arc::ptr_eq(&current.identity, &request.identity)
                    })
                {
                    return;
                }
                match result {
                    Ok(observation) => {
                        if this
                            .preferences
                            .store
                            .as_ref()
                            .is_some_and(|store| !observation.belongs_to(store))
                        {
                            this.preferences.reconciliation_read = None;
                            return;
                        }
                        if this.adopt_observed_preferences(&observation, &request.scopes, cx) {
                            this.preferences.reconciliation_read = None;
                        } else {
                            this.schedule_persisted_preferences_refresh(
                                observation.path().to_owned(),
                                &request.scopes,
                                request.attempt.saturating_add(1),
                                cx,
                            );
                        }
                    }
                    Err(error) => {
                        this.preferences.reconciliation_read = None;
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_preferences_refresh_error(error, cx)
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_preference_effects(
        &mut self,
        scope: PreferenceScope,
        previous_language: zenclash_core::LanguagePreference,
        cx: &mut Context<Self>,
    ) {
        if previous_language != self.preferences.current.language {
            zenclash_i18n::set_locale(self.preferences.current.language.locale());
            bootstrap::refresh_native_app_menu(cx);
            self.proxies_page.update(cx, |_, cx| cx.notify());
            let runtime_page = self.runtime_page.clone();
            let proxies_page = self.proxies_page.clone();
            let main_window = self.shell.main_window;
            cx.defer(move |cx| {
                let _ = cx.update_window(main_window, |_, window, cx| {
                    proxies_page.update(cx, |page, cx| {
                        page.refresh_localized_placeholders(window, cx);
                    });
                    runtime_page.update(cx, |page, cx| {
                        page.refresh_localized_placeholders(window, cx);
                    });
                });
            });
        }
        if matches!(
            scope,
            PreferenceScope::Restore | PreferenceScope::TrafficHistory
        ) && let Some(history) = &self.services.traffic_history_session
        {
            history.update_preferences(&self.preferences.current);
        }
        if scope == PreferenceScope::Restore
            && let Err(error) = bootstrap::configure_log_monitor(
                &self.services.log_monitor,
                self.preferences.store.as_ref(),
                &self.preferences.current,
            )
        {
            tracing::warn!(%error, "failed to apply restored log persistence settings");
        }
        if matches!(
            scope,
            PreferenceScope::Restore | PreferenceScope::TrayVisibility
        ) && let Some(tray) = self.tray.icon.as_mut()
            && let Err(error) = tray.set_visible(self.preferences.current.traffic_tray_visible)
        {
            tracing::warn!(%error, "failed to apply tray visibility preference");
        }
        if matches!(
            scope,
            PreferenceScope::Restore | PreferenceScope::Appearance
        ) {
            let appearance = self.preferences.current.appearance;
            let _ = cx.update_window(self.shell.main_window, move |_, window, cx| {
                let mode = match appearance {
                    AppearancePreference::System => ThemeMode::from(window.appearance()),
                    AppearancePreference::Light => ThemeMode::Light,
                    AppearancePreference::Dark => ThemeMode::Dark,
                };
                apply_zen_theme(mode, Some(window), cx);
            });
        }
        self.refresh_tray_menu(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::MAX_RECONCILIATION_RETRIES;
    use super::reconciliation_backoff;

    #[test]
    fn reconciliation_backoff_grows_exponentially_and_is_capped() {
        assert_eq!(reconciliation_backoff(0), Duration::from_millis(10));
        assert_eq!(reconciliation_backoff(1), Duration::from_millis(20));
        assert_eq!(reconciliation_backoff(2), Duration::from_millis(40));
        assert_eq!(reconciliation_backoff(3), Duration::from_millis(80));
        assert_eq!(reconciliation_backoff(8), Duration::from_millis(2_000));
        assert_eq!(
            reconciliation_backoff(MAX_RECONCILIATION_RETRIES),
            Duration::from_millis(2_000)
        );
        assert_eq!(MAX_RECONCILIATION_RETRIES, 10);
    }
}
