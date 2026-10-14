//! Read-only presentation of unresolved menu preference intents, independent of persisted facts.

use zenclash_core::{AppPreferences, AppearancePreference, TrayDisplayPreference};

#[derive(Clone, Copy)]
pub(crate) enum MenuPreferenceChange {
    Appearance(AppearancePreference),
    TrayDisplay(TrayDisplayPreference),
    TrayVisibility(bool),
}

impl MenuPreferenceChange {
    pub(crate) fn apply(self, preferences: &mut AppPreferences) {
        match self {
            Self::Appearance(value) => preferences.appearance = value,
            Self::TrayDisplay(value) => preferences.tray_display = value,
            Self::TrayVisibility(value) => preferences.traffic_tray_visible = value,
        }
    }
}

/// A derived value; the menu save owner remains the only source of pending/error state.
#[derive(Clone, Default)]
pub(crate) struct PreferencePreviews(Vec<MenuPreferenceChange>);

impl PreferencePreviews {
    pub(crate) fn new(changes: Vec<MenuPreferenceChange>) -> Self {
        Self(changes)
    }
    pub(crate) fn apply(&self, preferences: &mut AppPreferences) {
        for change in &self.0 {
            change.apply(preferences);
        }
    }
}
