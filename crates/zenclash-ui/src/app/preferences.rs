//! Application adoption of persisted preference events and independently retained previews.

use gpui_kit::AppContext;

use super::{AppearancePreference, Context, ThemeMode, ZenClashApp, apply_zen_theme, bootstrap};
use crate::pages::runtime::{PreferenceScope, PreferencesRestored};

#[derive(Clone)]
pub(super) struct PreferenceReconciliationRead {
    pub(super) identity: std::sync::Arc<()>,
    scopes: Vec<PreferenceScope>,
}

impl ZenClashApp {
    pub(super) fn accept_preferences_event(
        &mut self,
        event: &PreferencesRestored,
        cx: &mut Context<Self>,
    ) {
        if let Some(observation) = event.observation() {
            if self
                .preferences_store
                .as_ref()
                .is_some_and(|store| !observation.belongs_to(store))
            {
                return;
            }
            if event.scope == PreferenceScope::Restore {
                self.preferences_reconciliation_read = None;
            }
            if !self.adopt_observed_preferences(observation, &[event.scope], cx) {
                self.refresh_persisted_preferences(
                    observation.path().to_owned(),
                    &[event.scope],
                    cx,
                );
            }
        } else if event.scope == PreferenceScope::Restore {
            if let Some(store) = self.preferences_store.as_ref() {
                self.refresh_persisted_preferences(store.path().to_owned(), &[event.scope], cx);
            }
        } else {
            let previous_language = self.preferences.language;
            event.scope.apply(&mut self.preferences, &event.preferences);
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
            .preferences_store
            .as_ref()
            .is_some_and(|store| !observation.belongs_to(store))
        {
            return false;
        }
        let previous_language = self.preferences.language;
        let accepted = observation
            .publish_if_current(|preferences| {
                for scope in scopes {
                    scope.apply(&mut self.preferences, preferences);
                }
                self.menu_preference_saves
                    .previews()
                    .apply(&mut self.preferences);
            })
            .is_some();
        if !accepted {
            return false;
        }
        let preferences = self.preferences.clone();
        let previews = self.menu_preference_saves.previews();
        self.runtime_page.update(cx, |page, cx| {
            page.merge_preferences_from_app(&preferences, scopes, previews, cx)
        });
        for scope in scopes {
            self.apply_preference_effects(*scope, previous_language, cx);
        }
        true
    }

    pub(super) fn refresh_persisted_preferences(
        &mut self,
        path: std::path::PathBuf,
        scopes: &[PreferenceScope],
        cx: &mut Context<Self>,
    ) {
        let mut requested = self
            .preferences_reconciliation_read
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
        };
        self.preferences_reconciliation_read = Some(request.clone());
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
                if !this
                    .preferences_reconciliation_read
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
                            .preferences_store
                            .as_ref()
                            .is_some_and(|store| !observation.belongs_to(store))
                        {
                            this.preferences_reconciliation_read = None;
                            return;
                        }
                        if this.adopt_observed_preferences(&observation, &request.scopes, cx) {
                            this.preferences_reconciliation_read = None;
                        } else {
                            this.refresh_persisted_preferences(
                                observation.path().to_owned(),
                                &request.scopes,
                                cx,
                            );
                        }
                    }
                    Err(error) => {
                        this.preferences_reconciliation_read = None;
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
        if previous_language != self.preferences.language {
            zenclash_i18n::set_locale(self.preferences.language.locale());
            bootstrap::refresh_native_app_menu(cx);
            self.proxies_page.update(cx, |_, cx| cx.notify());
            let runtime_page = self.runtime_page.clone();
            let proxies_page = self.proxies_page.clone();
            let main_window = self.main_window;
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
        use crate::pages::runtime::PreferenceScope;
        if matches!(
            scope,
            PreferenceScope::Restore | PreferenceScope::TrafficHistory
        ) && let Some(history) = &self.traffic_history_session
        {
            history.update_preferences(&self.preferences);
        }
        if scope == PreferenceScope::Restore {
            if let Err(error) = bootstrap::configure_log_monitor(
                &self.log_monitor,
                self.preferences_store.as_ref(),
                &self.preferences,
            ) {
                tracing::warn!(%error, "failed to apply restored log persistence settings");
            }
            if let Some(tray) = self.network_tray.as_mut()
                && let Err(error) = tray.set_visible(self.preferences.traffic_tray_visible)
            {
                tracing::warn!(%error, "failed to apply restored tray visibility");
            }
            let appearance = self.preferences.appearance;
            let _ = cx.update_window(self.main_window, move |_, window, cx| {
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
