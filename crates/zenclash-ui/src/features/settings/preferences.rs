use crate::features::host::RuntimePage;
use crate::foundation::primitives::preference_previews::PreferencePreviews;
use crate::runtime_host::{
    services::{PreferenceScope, PreferencesRestored},
    task_token::PageTaskToken,
};
use gpui_kit::Context;
use zenclash_core::AppPreferences;

impl RuntimePage {
    #[cfg(test)]
    pub(crate) fn presented_preferences(&self) -> &AppPreferences {
        &self.preferences
    }

    pub(crate) fn merge_preferences_from_app(
        &mut self,
        preferences: &AppPreferences,
        scopes: &[PreferenceScope],
        previews: PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        for scope in scopes {
            scope.apply(&mut self.preferences, preferences);
        }
        self.settings.preference_previews = previews;
        self.settings
            .preference_previews
            .apply(&mut self.preferences);
        cx.notify();
    }

    pub(crate) fn set_preference_previews(
        &mut self,
        previews: PreferencePreviews,
        cx: &mut Context<Self>,
    ) {
        self.settings.preference_previews = previews;
        self.settings
            .preference_previews
            .apply(&mut self.preferences);
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

    pub(in crate::features) fn publish_restored_preferences(
        &mut self,
        observation: zenclash_core::AppPreferencesObservation,
        token: PageTaskToken,
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
                self.settings
                    .preference_previews
                    .apply(&mut self.preferences);
                self.settings.system_proxy_editor = None;
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

    pub(in crate::features) fn accept_preferences(
        &mut self,
        preferences: AppPreferences,
        scope: PreferenceScope,
        cx: &mut Context<Self>,
    ) {
        scope.apply(&mut self.preferences, &preferences);
        cx.emit(PreferencesRestored::new(preferences, scope));
    }
}
