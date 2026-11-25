//! Layer 2: Persistent repositories, stores, preferences, and traffic history.
//!
//! Groups managed profile storage, YAML override chains, application preferences,
//! and SQLite traffic history.

pub(crate) mod overrides;
pub(crate) mod preferences;
pub(crate) mod profiles;
pub(crate) mod traffic_history;

pub use crate::mihomo::errors::{AppPreferencesError, AppPreferencesResult};
pub use overrides::{
    YamlOverrideCatalog, YamlOverrideError, YamlOverrideRecord, YamlOverrideResult,
    YamlOverrideStore,
};
pub use preferences::{
    AppPreferences, AppPreferencesObservation, AppPreferencesStore, AppearancePreference,
    CoreBinaryPreferences, DEFAULT_TRAFFIC_RETENTION_DAYS, LanguagePreference,
    NetworkProbeRoutePreference,
};
pub use profiles::{
    DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MAX_PROFILE_UPDATE_INTERVAL_MINUTES,
    MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_UPDATE_INTERVAL_MINUTES, ProfileActivation,
    ProfileCatalog, ProfileRecord, ProfileSource, ProfileStore, ProfileStoreError,
    ProfileStoreResult, ProfileUpdate, RemoteProfileOptions, RemoteProfileRoute,
    SubscriptionAuthorization, SubscriptionMetadata, SubscriptionUsage, validate_clash_yaml,
};
pub use traffic_history::{
    TrafficAggregate, TrafficDeltaLogger, TrafficDimension, TrafficHistoryEntry,
    TrafficHistoryError, TrafficHistoryQuery, TrafficHistoryResult, TrafficHistorySelection,
    TrafficHistorySnapshot, TrafficHistoryStore, TrafficOverview, TrafficTotals, TrafficTrendPoint,
};
