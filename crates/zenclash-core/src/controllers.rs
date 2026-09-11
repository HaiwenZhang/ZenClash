//! Saved controllers and local Wi-Fi profile rules, independent of app preferences.
use crate::{AppPreferencesStore, MihomoEndpoint, profiles::atomic_write};

const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

/// Persistence or validation failure for saved network contexts.
#[derive(Debug, thiserror::Error)]
pub enum ContextStoreError {
    /// The store could not be read or atomically replaced.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The stored JSON is invalid; the original file is retained.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A schema version or value is unsupported.
    #[error("{0}")]
    Invalid(String),
}

/// A named external controller with stable identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControllerEntry {
    /// Immutable identifier used by selections and UI rows.
    pub id: String,
    /// User-visible name.
    pub name: String,
    /// Validated controller address and bearer token. Debug output redacts the token.
    pub endpoint: MihomoEndpoint,
}

impl ControllerEntry {
    /// Creates a controller entry with a random stable identifier.
    ///
    /// # Errors
    /// Returns an error for invalid input or unavailable OS randomness.
    pub fn new(name: String, endpoint: MihomoEndpoint) -> Result<Self, ContextStoreError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| ContextStoreError::Invalid(error.to_string()))?;
        let id = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let entry = Self { id, name, endpoint };
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), ContextStoreError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.name.trim().is_empty()
            || self.name.len() > 256
        {
            return Err(ContextStoreError::Invalid(
                "controllers.invalid_name".into(),
            ));
        }
        self.endpoint
            .http_url("/version")
            .map_err(|_| ContextStoreError::Invalid("controllers.invalid_endpoint".into()))?;
        if http::HeaderValue::from_str(&format!("Bearer {}", self.endpoint.secret)).is_err() {
            return Err(ContextStoreError::Invalid(
                "controllers.invalid_secret".into(),
            ));
        }
        Ok(())
    }
}

/// Versioned saved targets; `None` selects the local controller.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ControllerCatalog {
    /// Supported schema version.
    pub version: u32,
    /// Last successfully selected remote controller.
    pub active: Option<String>,
    /// Configured external controllers.
    pub entries: Vec<ControllerEntry>,
}

impl Default for ControllerCatalog {
    fn default() -> Self {
        Self {
            version: 1,
            active: None,
            entries: Vec::new(),
        }
    }
}

impl ControllerCatalog {
    fn validate(&self) -> Result<(), ContextStoreError> {
        if self.version != 1 || self.entries.len() > 100 {
            return Err(ContextStoreError::Invalid(
                "controllers.invalid_catalog".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for entry in &self.entries {
            entry.validate()?;
            if !ids.insert(&entry.id) {
                return Err(ContextStoreError::Invalid(
                    "controllers.duplicate_id".into(),
                ));
            }
        }
        if self.active.as_ref().is_some_and(|id| !ids.contains(id)) {
            return Err(ContextStoreError::Invalid(
                "controllers.missing_target".into(),
            ));
        }
        Ok(())
    }
}

/// Explicitly enabled mapping from an exact SSID to a stable local profile ID.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SsidRules {
    /// Supported schema version.
    pub version: u32,
    /// Whether automatic local profile switching is enabled.
    pub enabled: bool,
    /// Exact, case-sensitive Wi-Fi names and their profile IDs.
    pub profiles: BTreeMap<String, String>,
}

impl Default for SsidRules {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: false,
            profiles: BTreeMap::new(),
        }
    }
}

impl SsidRules {
    fn validate(&self) -> Result<(), ContextStoreError> {
        if self.version != 1
            || self.profiles.len() > 100
            || self.profiles.iter().any(|(ssid, id)| {
                ssid.is_empty() || ssid.len() > 32 || id.is_empty() || id.len() > 128
            })
        {
            return Err(ContextStoreError::Invalid("ssid.invalid_rules".into()));
        }
        Ok(())
    }
}

/// Atomic stores shared by the controller editor and SSID rule editor.
#[derive(Clone, Debug)]
pub struct ControllerStore {
    root: PathBuf,
}

impl ControllerStore {
    /// Finds the existing application data directory without reading preferences.
    ///
    /// # Errors
    /// Returns an error when the application data directory is unavailable.
    pub fn discover() -> Result<Self, ContextStoreError> {
        let preferences = AppPreferencesStore::discover()
            .map_err(|error| ContextStoreError::Invalid(error.to_string()))?;
        let root = preferences
            .path()
            .parent()
            .ok_or_else(|| ContextStoreError::Invalid("controllers.missing_store".into()))?
            .to_path_buf();
        Ok(Self::new(root))
    }
    /// Opens an explicit directory, primarily for tests and portable hosts.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    /// Reads saved controllers, using defaults only when the file is absent.
    ///
    /// # Errors
    /// Rejects unreadable, oversized, corrupt or unsupported data.
    pub fn load(&self) -> Result<ControllerCatalog, ContextStoreError> {
        let catalog: ControllerCatalog = read_json(&self.root.join("controllers.json"))?;
        catalog.validate()?;
        Ok(catalog)
    }
    /// Atomically writes a validated controller catalog.
    ///
    /// # Errors
    /// Returns a validation or filesystem error, preserving the previous file.
    pub fn save(&self, catalog: &ControllerCatalog) -> Result<(), ContextStoreError> {
        catalog.validate()?;
        write_json(&self.root.join("controllers.json"), catalog)
    }
    /// Reads the independently versioned SSID rules.
    ///
    /// # Errors
    /// Rejects unreadable, oversized, corrupt or unsupported data.
    pub fn load_ssid(&self) -> Result<SsidRules, ContextStoreError> {
        let rules: SsidRules = read_json(&self.root.join("ssid-rules.json"))?;
        rules.validate()?;
        Ok(rules)
    }
    /// Atomically saves SSID rules without touching controller or preference files.
    ///
    /// # Errors
    /// Returns a validation or filesystem error, preserving the previous file.
    pub fn save_ssid(&self, rules: &SsidRules) -> Result<(), ContextStoreError> {
        rules.validate()?;
        write_json(&self.root.join("ssid-rules.json"), rules)
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ContextStoreError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_CONTEXT_BYTES {
        return Err(ContextStoreError::Invalid(
            "controllers.store_too_large".into(),
        ));
    }
    atomic_write(path, &bytes)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned + Default>(
    path: &Path,
) -> Result<T, ContextStoreError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONTEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONTEXT_BYTES {
        return Err(ContextStoreError::Invalid(
            "controllers.store_too_large".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Debounces observations and prevents repeat attempts on one Wi-Fi association.
#[derive(Default)]
pub struct SsidSwitchState {
    candidate: Option<(String, String)>,
    attempted: Option<(String, String)>,
}

impl SsidSwitchState {
    /// Returns a profile ID once two consecutive observations agree. Unknown
    /// SSIDs, disabled rules and remote targets reset pending observations.
    pub fn observe(
        &mut self,
        ssid: Option<&str>,
        rules: &SsidRules,
        local: bool,
        current_profile: Option<&str>,
    ) -> Option<String> {
        let target = ssid.filter(|_| local && rules.enabled).and_then(|ssid| {
            rules
                .profiles
                .get(ssid)
                .map(|profile| (ssid.to_owned(), profile.clone()))
        });
        let stable = target.is_some() && self.candidate == target;
        self.candidate = target.clone();
        let Some(target) = target else {
            self.attempted = None;
            return None;
        };
        if !stable || self.attempted.as_ref() == Some(&target) {
            return None;
        }
        self.attempted = Some(target.clone());
        (current_profile != Some(target.1.as_str())).then_some(target.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_save_preserves_the_previous_readable_catalog() {
        let entry = ControllerEntry::new("router".into(), MihomoEndpoint::default()).unwrap();
        let root = std::env::temp_dir().join(format!("zenclash-store-limit-{}", entry.id));
        let store = ControllerStore::new(root.clone());
        let original = ControllerCatalog {
            entries: vec![entry],
            ..Default::default()
        };
        store.save(&original).unwrap();
        let mut candidate = original.clone();
        candidate.entries[0].endpoint.secret = "x".repeat(MAX_CONTEXT_BYTES);
        assert!(store.save(&candidate).is_err());
        assert_eq!(store.load().unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_unknown_and_already_active_wifi_rules_do_not_switch() {
        let mut rules = SsidRules {
            enabled: true,
            profiles: BTreeMap::from([("Office".into(), "one".into())]),
            ..Default::default()
        };
        let mut state = SsidSwitchState::default();
        for _ in 0..3 {
            assert_eq!(state.observe(Some("office"), &rules, true, None), None);
            assert_eq!(state.observe(None, &rules, true, None), None);
        }
        assert_eq!(
            state.observe(Some("Office"), &rules, true, Some("one")),
            None
        );
        assert_eq!(
            state.observe(Some("Office"), &rules, true, Some("one")),
            None
        );
        rules.enabled = false;
        for _ in 0..3 {
            assert_eq!(state.observe(Some("Office"), &rules, true, None), None);
        }
    }

    #[test]
    fn editing_a_wifi_mapping_requires_stability_before_the_new_profile_is_applied() {
        let mut rules = SsidRules {
            enabled: true,
            profiles: BTreeMap::from([("Office".into(), "one".into())]),
            ..Default::default()
        };
        let mut state = SsidSwitchState::default();
        state.observe(Some("Office"), &rules, true, None);
        rules.profiles.insert("Office".into(), "two".into());
        assert_eq!(state.observe(Some("Office"), &rules, true, None), None);
        assert_eq!(
            state.observe(Some("Office"), &rules, true, None),
            Some("two".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_controller_secret_is_private_and_preferences_are_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let entry = ControllerEntry::new(
            "router".into(),
            MihomoEndpoint::new("localhost:9090", "test-token"),
        )
        .unwrap();
        let root = std::env::temp_dir().join(format!("zenclash-secret-{}", entry.id));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("preferences.json"), b"unchanged").unwrap();
        let store = ControllerStore::new(root.clone());
        let catalog = ControllerCatalog {
            entries: vec![entry],
            ..Default::default()
        };
        store.save(&catalog).unwrap();
        assert_eq!(store.load().unwrap(), catalog);
        assert_eq!(
            std::fs::metadata(root.join("controllers.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read(root.join("preferences.json")).unwrap(),
            b"unchanged"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn duplicate_ids_and_dangling_active_targets_are_rejected() {
        let entry = ControllerEntry::new("router".into(), MihomoEndpoint::default()).unwrap();
        assert!(
            ControllerCatalog {
                entries: vec![entry.clone(), entry],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ControllerCatalog {
                active: Some("missing".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn invalid_endpoint_credentials_never_appear_in_error_or_debug() {
        let endpoint = MihomoEndpoint::new("localhost:9090", "private-token");
        assert!(!format!("{endpoint:?}").contains("private-token"));
        assert!(
            ControllerEntry::new(
                "test".into(),
                MihomoEndpoint::new("https://user:password@host", "")
            )
            .is_err()
        );
    }
    #[test]
    fn wifi_switch_requires_stability_and_does_not_override_manual_selection_repeatedly() {
        let rules = SsidRules {
            enabled: true,
            profiles: BTreeMap::from([("office".into(), "profile-a".into())]),
            ..Default::default()
        };
        let mut state = SsidSwitchState::default();
        assert_eq!(state.observe(Some("office"), &rules, true, None), None);
        assert_eq!(
            state.observe(Some("office"), &rules, true, None),
            Some("profile-a".into())
        );
        assert_eq!(
            state.observe(Some("office"), &rules, true, Some("manual-profile")),
            None
        );
        assert_eq!(state.observe(Some("office"), &rules, false, None), None);
        assert_eq!(state.observe(Some("office"), &rules, true, None), None);
        assert_eq!(
            state.observe(Some("office"), &rules, true, None),
            Some("profile-a".into())
        );
    }
    #[test]
    fn corrupt_and_future_stores_are_preserved() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-controller-test-{}",
            ControllerEntry::new("test".into(), MihomoEndpoint::default())
                .unwrap()
                .id
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = ControllerStore::new(root.clone());
        let bytes = b"{\"version\":999}";
        std::fs::write(root.join("controllers.json"), bytes).unwrap();
        assert!(store.load().is_err());
        assert_eq!(std::fs::read(root.join("controllers.json")).unwrap(), bytes);
        store.save_ssid(&SsidRules::default()).unwrap();
        assert_eq!(store.load_ssid().unwrap(), SsidRules::default());
        std::fs::remove_dir_all(root).unwrap();
    }
}
