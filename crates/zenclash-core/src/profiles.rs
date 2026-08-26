//! Persistent local and subscription-based Clash/Mihomo profile management.

use std::{path::PathBuf, sync::Arc};

use parking_lot::Mutex;
use thiserror::Error;

mod activation;
mod download;
mod edit;
mod model;
mod remote;
mod schedule;
mod storage;
mod store;
mod validation;

#[cfg(test)]
mod tests;

use download::download_profile;
pub use model::{
    ProfileActivation, ProfileCatalog, ProfileRecord, ProfileSource, ProfileUpdate,
    RemoteProfileOptions, RemoteProfileRoute, SubscriptionAuthorization, SubscriptionMetadata,
    SubscriptionUsage,
};
pub use storage::atomic_write;
pub use storage::read_profile_bytes;
use storage::{home_dir, read_index_bytes};
pub use validation::validate_clash_yaml;
use validation::{
    normalized_profile_name, normalized_remote_url, normalized_user_agent, unique_id,
    unix_timestamp,
};

const DEFAULT_USER_AGENT: &str = "clash.meta";
/// Default automatic refresh cadence for remote profiles.
pub const DEFAULT_PROFILE_UPDATE_INTERVAL_MINUTES: u32 = 24 * 60;
/// Smallest accepted automatic refresh cadence for remote profiles.
pub const MIN_PROFILE_UPDATE_INTERVAL_MINUTES: u32 = 15;
/// Largest accepted automatic refresh cadence for remote profiles.
pub const MAX_PROFILE_UPDATE_INTERVAL_MINUTES: u32 = 30 * 24 * 60;
/// Default end-to-end timeout for one subscription download.
pub const DEFAULT_PROFILE_DOWNLOAD_TIMEOUT_SECONDS: u32 = 30;
/// Smallest accepted per-subscription download timeout.
pub const MIN_PROFILE_DOWNLOAD_TIMEOUT_SECONDS: u32 = 1;
/// Largest accepted per-subscription download timeout.
pub const MAX_PROFILE_DOWNLOAD_TIMEOUT_SECONDS: u32 = 10 * 60;
pub const MAX_PROFILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROFILE_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// Errors produced while validating, downloading, or persisting profiles.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileStoreError {
    /// Filesystem access failed.
    #[error("配置仓库 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// The persistent profile index could not be decoded or encoded.
    #[error("配置仓库索引无效：{0}")]
    Index(#[from] serde_json::Error),
    /// The profile index exceeded the defensive in-memory read limit.
    #[error("配置仓库索引超过 {limit_mib} MiB 限制")]
    IndexTooLarge {
        /// Configured index-size limit in mebibytes.
        limit_mib: usize,
    },
    /// The supplied content is not a supported Clash/Mihomo YAML profile.
    #[error("Clash YAML 无效：{0}")]
    InvalidYaml(String),
    /// A subscription request failed.
    #[error("在线订阅请求失败：{0}")]
    Http(#[from] reqwest::Error),
    /// The requested profile does not exist or is not of the required kind.
    #[error("找不到配置：{0}")]
    NotFound(String),
    /// The active profile cannot be deleted.
    #[error("活动配置不能删除，请先切换到其他配置")]
    ActiveProfile,
    /// A multi-file update or its rollback could not be completed safely.
    #[error("配置仓库事务失败：{0}")]
    Transaction(String),
}

/// Result type returned by profile-store operations.
pub type ProfileStoreResult<T> = Result<T, ProfileStoreError>;

/// Manages an indexed directory of local and remote Clash/Mihomo profiles.
///
/// Clones share a transaction lock. This prevents concurrent UI workflows
/// from overwriting a newer catalog snapshot while a profile file and the
/// index are being updated together.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    root: PathBuf,
    transaction: Arc<Mutex<()>>,
}
