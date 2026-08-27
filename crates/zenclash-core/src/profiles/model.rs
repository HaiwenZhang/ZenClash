use std::{
    fmt,
    path::{Path, PathBuf},
};

use reqwest::header::HeaderValue;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{
    DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES,
    MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS, MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS,
};

const fn default_update_interval_minutes() -> u32 {
    DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES
}

/// Authorization header persisted for a remote subscription.
///
/// Debug formatting is deliberately redacted so bearer/basic credentials do
/// not leak through diagnostics containing a [`ProfileRecord`].
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SubscriptionAuthorization(String);

impl SubscriptionAuthorization {
    /// Validates a non-empty HTTP Authorization header value.
    ///
    /// # Errors
    ///
    /// Returns an error for blank values or bytes invalid in an HTTP header.
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err("Authorization 不能为空".into());
        }
        HeaderValue::from_str(&value)
            .map_err(|error| format!("Authorization 不是有效 HTTP 头：{error}"))?;
        Ok(Self(value))
    }

    /// Returns the secret only for constructing the outbound request.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SubscriptionAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl fmt::Debug for SubscriptionAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SubscriptionAuthorization([REDACTED])")
    }
}

/// Persistent request options for an online subscription.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteProfileOptions {
    /// Optional Authorization header, stored but always redacted from Debug.
    pub authorization: Option<SubscriptionAuthorization>,
    /// Whether downloads must traverse the current Mihomo HTTP/Mixed port.
    pub use_mihomo_proxy: bool,
    /// Whether a failed direct request should be retried through Mihomo.
    pub fallback_to_mihomo_proxy: bool,
    /// End-to-end timeout for each subscription request.
    pub download_timeout_seconds: u32,
    /// Whether provider-suggested refresh intervals must be ignored.
    pub fixed_update_interval: bool,
}

impl Default for RemoteProfileOptions {
    fn default() -> Self {
        Self {
            authorization: None,
            use_mihomo_proxy: false,
            fallback_to_mihomo_proxy: true,
            download_timeout_seconds: DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS,
            fixed_update_interval: false,
        }
    }
}

impl RemoteProfileOptions {
    /// Builds validated persistent options from UI values.
    ///
    /// # Errors
    ///
    /// Returns an error when a non-empty Authorization value is not a valid
    /// HTTP header value.
    pub fn new(authorization: impl Into<String>, use_mihomo_proxy: bool) -> Result<Self, String> {
        let authorization = authorization.into();
        let authorization = if authorization.trim().is_empty() {
            None
        } else {
            Some(SubscriptionAuthorization::new(authorization)?)
        };
        let route = if use_mihomo_proxy {
            RemoteProfileRoute::Mihomo
        } else {
            RemoteProfileRoute::DirectWithMihomoFallback
        };
        Ok(Self {
            authorization,
            ..Self::default()
        }
        .with_route(route))
    }

    /// Applies validated per-subscription download and interval policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout is outside the supported bounds.
    pub fn with_download_policy(
        mut self,
        timeout_seconds: u32,
        fixed_update_interval: bool,
    ) -> Result<Self, String> {
        if !(MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS..=MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS)
            .contains(&timeout_seconds)
        {
            return Err(format!(
                "订阅下载超时必须在 {MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS} 到 {MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS} 秒之间"
            ));
        }
        self.download_timeout_seconds = timeout_seconds;
        self.fixed_update_interval = fixed_update_interval;
        Ok(self)
    }

    /// Applies an explicit download route policy.
    #[must_use]
    pub const fn with_route(mut self, route: RemoteProfileRoute) -> Self {
        match route {
            RemoteProfileRoute::Direct => {
                self.use_mihomo_proxy = false;
                self.fallback_to_mihomo_proxy = false;
            }
            RemoteProfileRoute::DirectWithMihomoFallback => {
                self.use_mihomo_proxy = false;
                self.fallback_to_mihomo_proxy = true;
            }
            RemoteProfileRoute::Mihomo => {
                self.use_mihomo_proxy = true;
                self.fallback_to_mihomo_proxy = false;
            }
        }
        self
    }

    /// Returns the effective direct/Mihomo request route.
    #[must_use]
    pub const fn route(&self) -> RemoteProfileRoute {
        if self.use_mihomo_proxy {
            RemoteProfileRoute::Mihomo
        } else if self.fallback_to_mihomo_proxy {
            RemoteProfileRoute::DirectWithMihomoFallback
        } else {
            RemoteProfileRoute::Direct
        }
    }
}

/// Effective network route for an online subscription request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteProfileRoute {
    /// Never use the Mihomo listener.
    Direct,
    /// Try direct first, then retry network failures through Mihomo.
    #[default]
    DirectWithMihomoFallback,
    /// Always use the Mihomo HTTP/Mixed listener.
    Mihomo,
}

/// Traffic quota reported by a `subscription-userinfo` response header.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionUsage {
    /// Bytes uploaded during the provider's accounting period.
    pub upload: u64,
    /// Bytes downloaded during the provider's accounting period.
    pub download: u64,
    /// Total quota in bytes, or zero when unlimited/unknown.
    pub total: u64,
    /// Expiration as Unix seconds, or zero when absent.
    pub expire: u64,
}

impl SubscriptionUsage {
    /// Returns accounted upload plus download without overflow.
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }
}

/// Optional metadata returned alongside a subscription YAML response.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionMetadata {
    /// Parsed provider quota and expiration.
    pub usage: Option<SubscriptionUsage>,
    /// Provider account/home page URL.
    pub home_url: Option<String>,
    /// Provider-suggested refresh cadence in minutes.
    pub suggested_update_interval_minutes: Option<u32>,
}

impl SubscriptionMetadata {
    pub(super) fn merge_from(&mut self, newer: Self) {
        if newer.usage.is_some() {
            self.usage = newer.usage;
        }
        if newer.home_url.is_some() {
            self.home_url = newer.home_url;
        }
        if newer.suggested_update_interval_minutes.is_some() {
            self.suggested_update_interval_minutes = newer.suggested_update_interval_minutes;
        }
    }
}

/// Origin of a managed Clash/Mihomo profile.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileSource {
    /// YAML imported from a local filesystem path.
    Local {
        /// Original user-selected path.
        original_path: String,
    },
    /// YAML downloaded from an HTTP(S) subscription.
    Remote {
        /// Subscription endpoint.
        url: String,
        /// User-Agent sent while downloading the subscription.
        user_agent: String,
        /// Authorization and explicit Mihomo-proxy policy.
        #[serde(default)]
        options: RemoteProfileOptions,
    },
}

/// Metadata for one profile managed by [`super::ProfileStore`].
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProfileRecord {
    /// Stable identifier used by the catalog and storage filename.
    pub id: String,
    /// User-facing profile name.
    pub name: String,
    /// YAML filename relative to the store's files directory.
    pub file_name: String,
    /// Source used to create or update the profile.
    pub source: ProfileSource,
    /// Last update time as Unix seconds.
    pub updated_at: u64,
    /// Stored YAML payload size.
    pub size_bytes: u64,
    /// Whether this remote profile should be refreshed in the background.
    #[serde(default)]
    pub auto_update: bool,
    /// Background refresh cadence in minutes.
    #[serde(default = "default_update_interval_minutes")]
    pub update_interval_minutes: u32,
    /// Optional five-field local-time cron expression overriding the interval.
    #[serde(default)]
    pub update_cron: Option<String>,
    /// Provider quota, account page, and suggested update cadence.
    #[serde(default)]
    pub subscription: SubscriptionMetadata,
}

impl ProfileRecord {
    /// Returns a concise localized source label for the UI.
    #[must_use]
    pub fn source_label(&self) -> String {
        match self.source {
            ProfileSource::Local { .. } => zenclash_i18n::text("profiles.source.local"),
            ProfileSource::Remote { .. } => zenclash_i18n::text("profiles.source.remote"),
        }
    }

    /// Returns whether this record can be updated from a subscription URL.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        matches!(&self.source, ProfileSource::Remote { .. })
    }

    /// Returns whether the remote profile should be refreshed at `now`.
    #[must_use]
    pub fn update_due(&self, now: u64) -> bool {
        if !self.is_remote() || !self.auto_update {
            return false;
        }
        if let Some(expression) = self.update_cron.as_deref() {
            return super::schedule::cron_update_due(expression, self.updated_at, now)
                .unwrap_or(false);
        }
        let interval_seconds = u64::from(self.update_interval_minutes.max(1)).saturating_mul(60);
        now.saturating_sub(self.updated_at) >= interval_seconds
    }
}

/// Persistent collection of managed profiles and the active profile ID.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProfileCatalog {
    /// ID of the active profile, when one has been selected.
    pub active: Option<String>,
    /// Profiles currently present in the store.
    pub profiles: Vec<ProfileRecord>,
}

impl ProfileCatalog {
    /// Resolves [`Self::active`] to its profile record.
    #[must_use]
    pub fn active_profile(&self) -> Option<&ProfileRecord> {
        let active = self.active.as_deref()?;
        self.profiles.iter().find(|profile| profile.id == active)
    }

    /// Returns IDs of remote profiles whose automatic update is due.
    #[must_use]
    pub fn due_profile_ids(&self, now: u64) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|profile| profile.update_due(now))
            .map(|profile| profile.id.clone())
            .collect()
    }
}

/// A downloaded profile update together with the previous on-disk state.
///
/// Pass this value to [`super::ProfileStore::rollback_update`] if Mihomo
/// rejects the downloaded configuration after YAML validation.
#[derive(Debug)]
pub struct ProfileUpdate {
    /// Updated profile metadata.
    pub record: ProfileRecord,
    pub(super) previous_record: ProfileRecord,
    pub(super) previous_payload: Vec<u8>,
    pub(super) applied_payload: Vec<u8>,
}

/// Token for reverting a persisted active-profile change.
///
/// Tokens are produced by [`super::ProfileStore::activate_reversible`] and are
/// consumed by [`super::ProfileStore::rollback_activation`].
#[derive(Debug)]
pub struct ProfileActivation {
    pub(super) activated_id: String,
    pub(super) previous_active: Option<String>,
    pub(super) path: PathBuf,
}

impl ProfileActivation {
    /// Returns the managed YAML path selected by the activation.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
