//! Application-menu previews and the results of their serialized preference writes.

use std::sync::Arc;

use super::{Context, ZenClashApp};

pub(super) use crate::preference_previews::MenuPreferenceChange;
use crate::preference_previews::PreferencePreviews;

impl MenuPreferenceChange {
    fn failure_key(self) -> &'static str {
        match self {
            Self::Appearance(_) => "app.preferences.appearance_unsaved",
            Self::TrayVisibility(_) => "app.preferences.tray_visibility_unsaved",
        }
    }
}

struct MenuPreferenceSaveReceipt {
    change: MenuPreferenceChange,
    identity: Arc<()>,
}

struct MenuPreferenceSave {
    change: MenuPreferenceChange,
    identity: Arc<()>,
    pending: bool,
    error: Option<String>,
}

/// Each field retains its latest intent and unresolved failure independently.
#[derive(Default)]
pub(super) struct MenuPreferenceSaves {
    appearance: Option<MenuPreferenceSave>,
    tray_visibility: Option<MenuPreferenceSave>,
}

impl MenuPreferenceSaves {
    fn field_mut(&mut self, change: MenuPreferenceChange) -> &mut Option<MenuPreferenceSave> {
        match change {
            MenuPreferenceChange::Appearance(_) => &mut self.appearance,
            MenuPreferenceChange::TrayVisibility(_) => &mut self.tray_visibility,
        }
    }

    fn begin(&mut self, change: MenuPreferenceChange) -> MenuPreferenceSaveReceipt {
        let field = self.field_mut(change);
        let error = field.take().and_then(|previous| previous.error);
        let identity = Arc::new(());
        *field = Some(MenuPreferenceSave {
            change,
            identity: identity.clone(),
            pending: true,
            error,
        });
        MenuPreferenceSaveReceipt { change, identity }
    }

    fn complete(&mut self, receipt: MenuPreferenceSaveReceipt, result: Result<(), String>) -> bool {
        let Some(current) = self.field_mut(receipt.change).as_mut() else {
            return false;
        };
        if !Arc::ptr_eq(&current.identity, &receipt.identity) {
            return false;
        }
        current.pending = false;
        current.error = result.err();
        true
    }

    fn fields(&self) -> impl Iterator<Item = &MenuPreferenceSave> {
        [&self.appearance, &self.tray_visibility]
            .into_iter()
            .filter_map(Option::as_ref)
    }

    pub(super) fn previews(&self) -> PreferencePreviews {
        PreferencePreviews::new(
            self.fields()
                .filter(|field| field.pending || field.error.is_some())
                .map(|field| field.change)
                .collect(),
        )
    }

    pub(super) fn failure_message(&self) -> Option<String> {
        let failures: Vec<_> = self
            .fields()
            .filter_map(|field| {
                field.error.as_ref().map(|error| {
                    zenclash_i18n::text_with(
                        field.change.failure_key(),
                        &[("error", error.clone())],
                    )
                })
            })
            .collect();
        (!failures.is_empty()).then(|| failures.join("\n"))
    }

    pub(super) fn has_pending_retry(&self) -> bool {
        self.fields()
            .any(|field| field.pending && field.error.is_some())
    }

    fn retry_changes(&self) -> Vec<MenuPreferenceChange> {
        self.fields()
            .filter(|field| !field.pending && field.error.is_some())
            .map(|field| field.change)
            .collect()
    }
}

impl ZenClashApp {
    pub(super) fn update_menu_preference(
        &mut self,
        change: MenuPreferenceChange,
        cx: &mut Context<Self>,
    ) {
        change.apply(&mut self.preferences);
        let receipt = self.menu_preference_saves.begin(change);
        let preferences = self.preferences.clone();
        let previews = self.menu_preference_saves.previews();
        self.runtime_page.update(cx, |page, cx| {
            page.preferences_restored_from_app(preferences, previews, cx);
        });
        let Some(store) = self.preferences_store.clone() else {
            self.complete_menu_preference(
                receipt,
                Err(zenclash_i18n::text("app.errors.preferences_unavailable")),
                cx,
            );
            return;
        };
        let previous = self.preferences_save_task.take();
        let (completion, result) = tokio::sync::oneshot::channel();
        // Keep persistence owned by the existing task chain; quitting waits for
        // it even when the foreground result observer is dropped.
        self.preferences_save_task = Some(self.runtime.spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let result = tokio::task::spawn_blocking(move || {
                store.update(move |preferences| change.apply(preferences))
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map(|_| ()).map_err(|error| error.to_string()));
            let _ = completion.send(result);
        }));
        cx.spawn(async move |this, cx| {
            let result = result
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.complete_menu_preference(receipt, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn complete_menu_preference(
        &mut self,
        receipt: MenuPreferenceSaveReceipt,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = &result {
            tracing::warn!(%error, "failed to save application menu preference");
        }
        let committed = result.is_ok();
        if self.menu_preference_saves.complete(receipt, result) {
            let previews = self.menu_preference_saves.previews();
            self.runtime_page
                .update(cx, |page, cx| page.set_preference_previews(previews, cx));
            if committed && let Some(store) = self.preferences_store.as_ref() {
                self.refresh_persisted_preferences(
                    store.path().to_owned(),
                    &[crate::pages::runtime::PreferenceScope::Restore],
                    cx,
                );
            }
            cx.notify();
        }
    }

    pub(super) fn retry_menu_preferences(&mut self, cx: &mut Context<Self>) {
        for change in self.menu_preference_saves.retry_changes() {
            self.update_menu_preference(change, cx);
        }
    }
}

#[cfg(test)]
mod tests;
