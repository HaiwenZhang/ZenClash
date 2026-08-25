use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
}

impl ProfileRecord {
    /// Returns a concise localized source label for the UI.
    #[must_use]
    pub const fn source_label(&self) -> &'static str {
        match self.source {
            ProfileSource::Local { .. } => "本地 YAML",
            ProfileSource::Remote { .. } => "在线订阅",
        }
    }

    /// Returns whether this record can be updated from a subscription URL.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        matches!(&self.source, ProfileSource::Remote { .. })
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
