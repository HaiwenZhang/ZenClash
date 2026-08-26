//! Persistent application preferences independent from Mihomo profiles.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profiles::atomic_write;
use crate::{CoreKind, NetworkLatencyTarget, PublicIpProvider, SystemProxyMode};

const MAX_PREFERENCES_BYTES: usize = 1024 * 1024;

/// Native application appearance selected by the user.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AppearancePreference {
    /// Follow the operating system's current window appearance.
    #[default]
    System,
    /// Use `ZenClash`'s dark network-console palette.
    Dark,
    /// Use `ZenClash`'s light palette.
    Light,
}

/// Route selected for application-level public network diagnostics.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkProbeRoutePreference {
    /// Send diagnostics directly through the operating system network stack.
    Direct,
    /// Send diagnostics through the current Mihomo HTTP or Mixed listener.
    #[default]
    Mihomo,
}

/// Small versioned set of native application preferences.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppPreferences {
    /// Schema version reserved for future migrations.
    pub version: u32,
    /// Runtime core requested for the next application start.
    pub core_kind: CoreKind,
    /// Optional user-selected executable paths for each supported runtime core.
    pub core_binaries: CoreBinaryPreferences,
    /// Most recent core that completed managed startup successfully.
    pub last_known_good_core: Option<CoreKind>,
    /// Exact executable used by the most recent successful managed startup.
    pub last_known_good_binary: Option<PathBuf>,
    /// Preferred native application appearance.
    pub appearance: AppearancePreference,
    /// Whether the live native traffic indicator is visible.
    pub traffic_tray_visible: bool,
    /// Whether real Mihomo connection deltas are persisted for historical reports.
    pub traffic_history_enabled: bool,
    /// Number of days retained by the local traffic-history database.
    pub traffic_retention_days: u16,
    /// Whether new Mihomo log entries are continuously written to disk.
    pub log_file_enabled: bool,
    /// Maximum size of the bounded Mihomo log file in mebibytes.
    pub log_file_max_mebibytes: u16,
    /// Public-IP data source selected on the network diagnostics page.
    pub network_ip_provider: PublicIpProvider,
    /// Route used for public network diagnostics.
    pub network_probe_route: NetworkProbeRoutePreference,
    /// User-defined latency targets appended to the three built-in endpoints.
    pub network_latency_targets: Vec<NetworkLatencyTarget>,
    /// Ordered native system-proxy bypass rules shared by the page and tray.
    pub system_proxy_bypass: Vec<String>,
    /// Whether native system proxying uses explicit endpoints or PAC.
    pub system_proxy_mode: SystemProxyMode,
    /// Host used by manual proxying and the local PAC listener.
    pub system_proxy_host: String,
    /// PAC JavaScript served when automatic proxy mode is selected.
    pub system_proxy_pac_script: String,
}

/// User-selected executable paths for the two supported runtime cores.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CoreBinaryPreferences {
    /// Custom Mihomo executable, or automatic discovery when absent.
    pub mihomo: Option<PathBuf>,
    /// Custom meow-rs executable, or automatic discovery when absent.
    pub meow: Option<PathBuf>,
}

impl CoreBinaryPreferences {
    /// Returns the custom executable selected for `kind`.
    #[must_use]
    pub fn path(&self, kind: CoreKind) -> Option<&Path> {
        match kind {
            CoreKind::Mihomo => self.mihomo.as_deref(),
            CoreKind::Meow => self.meow.as_deref(),
        }
    }

    /// Replaces the custom executable for `kind`.
    pub fn set(&mut self, kind: CoreKind, path: Option<PathBuf>) {
        match kind {
            CoreKind::Mihomo => self.mihomo = path,
            CoreKind::Meow => self.meow = path,
        }
    }
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            core_kind: CoreKind::Mihomo,
            core_binaries: CoreBinaryPreferences::default(),
            last_known_good_core: None,
            last_known_good_binary: None,
            appearance: AppearancePreference::System,
            traffic_tray_visible: true,
            traffic_history_enabled: true,
            traffic_retention_days: crate::DEFAULT_TRAFFIC_RETENTION_DAYS,
            log_file_enabled: true,
            log_file_max_mebibytes: 10,
            network_ip_provider: PublicIpProvider::default(),
            network_probe_route: NetworkProbeRoutePreference::Mihomo,
            network_latency_targets: Vec::new(),
            system_proxy_bypass: crate::default_system_proxy_bypass(),
            system_proxy_mode: SystemProxyMode::Manual,
            system_proxy_host: "127.0.0.1".into(),
            system_proxy_pac_script: crate::default_pac_script().into(),
        }
    }
}

/// Errors produced while loading or saving application preferences.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AppPreferencesError {
    /// Filesystem access failed.
    #[error("应用设置 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// The preferences document is malformed or incompatible.
    #[error("应用设置 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    /// The document exceeded the defensive read limit.
    #[error("应用设置超过 1 MiB 限制")]
    TooLarge,
    /// A platform data directory could not be determined.
    #[error("无法确定应用数据目录")]
    MissingDataDirectory,
    /// A preference value is outside its supported range.
    #[error("应用设置无效：{0}")]
    Invalid(String),
    /// Preferences changed after a reversible update was prepared.
    #[error("应用设置已被其他操作修改，请刷新后重试")]
    ConcurrentModification,
}

/// Result type for persistent application-preference operations.
pub type AppPreferencesResult<T> = Result<T, AppPreferencesError>;

/// Atomic, cloneable store for [`AppPreferences`].
#[derive(Clone, Debug)]
pub struct AppPreferencesStore {
    path: PathBuf,
    transaction: Arc<Mutex<()>>,
}

impl AppPreferencesStore {
    /// Opens the platform-default `ZenClash` preferences file.
    ///
    /// # Errors
    ///
    /// Returns an error when the user's application-data directory cannot be
    /// determined.
    pub fn discover() -> AppPreferencesResult<Self> {
        Ok(Self::new(default_data_dir()?.join("preferences.json")))
    }

    /// Creates a store backed by an explicit path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            transaction: Arc::new(Mutex::new(())),
        }
    }

    /// Loads preferences, returning defaults when no file exists yet.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures, oversized input, or invalid
    /// JSON. Corrupt preferences are not silently overwritten.
    pub fn load(&self) -> AppPreferencesResult<AppPreferences> {
        let _transaction = self.transaction.lock();
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> AppPreferencesResult<AppPreferences> {
        if !self.path.exists() {
            return Ok(AppPreferences::default());
        }
        let file = fs::File::open(&self.path)?;
        let mut bytes = Vec::new();
        file.take(MAX_PREFERENCES_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PREFERENCES_BYTES {
            return Err(AppPreferencesError::TooLarge);
        }
        let preferences: AppPreferences = serde_json::from_slice(&bytes)?;
        validate_preferences(&preferences)?;
        Ok(preferences)
    }

    /// Atomically saves the complete preference snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or the atomic filesystem update
    /// fails.
    pub fn save(&self, preferences: &AppPreferences) -> AppPreferencesResult<()> {
        let _transaction = self.transaction.lock();
        self.save_unlocked(preferences)
    }

    /// Applies a field-level mutation to the latest stored preferences.
    ///
    /// The read, mutation, validation, and atomic write share one transaction,
    /// so callers using clones of this store cannot overwrite unrelated fields
    /// with an older in-memory snapshot.
    ///
    /// # Errors
    ///
    /// Returns a load, validation, serialization, or filesystem error without
    /// modifying the stored preferences.
    pub fn update(
        &self,
        mutate: impl FnOnce(&mut AppPreferences),
    ) -> AppPreferencesResult<AppPreferences> {
        let _transaction = self.transaction.lock();
        let mut preferences = self.load_unlocked()?;
        mutate(&mut preferences);
        self.save_unlocked(&preferences)?;
        Ok(preferences)
    }

    /// Replaces a complete preference snapshot only when it is still current.
    ///
    /// # Errors
    ///
    /// Returns [`AppPreferencesError::ConcurrentModification`] when another
    /// writer committed after `expected` was loaded, or a validation/write
    /// error for `next`.
    pub fn replace(
        &self,
        expected: &AppPreferences,
        next: &AppPreferences,
    ) -> AppPreferencesResult<()> {
        let _transaction = self.transaction.lock();
        if &self.load_unlocked()? != expected {
            return Err(AppPreferencesError::ConcurrentModification);
        }
        self.save_unlocked(next)
    }

    fn save_unlocked(&self, preferences: &AppPreferences) -> AppPreferencesResult<()> {
        validate_preferences(preferences)?;
        let bytes = serde_json::to_vec_pretty(preferences)?;
        atomic_write(&self.path, &bytes)?;
        Ok(())
    }

    /// Returns the preferences file path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the bounded Mihomo log-file path associated with this store.
    #[must_use]
    pub fn log_file_path(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("logs/mihomo.log")
    }
}

fn validate_preferences(preferences: &AppPreferences) -> AppPreferencesResult<()> {
    if !(1..=366).contains(&preferences.traffic_retention_days) {
        return Err(AppPreferencesError::Invalid(
            "流量历史保留天数必须在 1 到 366 之间".into(),
        ));
    }
    if !(1..=100).contains(&preferences.log_file_max_mebibytes) {
        return Err(AppPreferencesError::Invalid(
            "日志文件大小上限必须在 1 到 100 MiB 之间".into(),
        ));
    }
    if preferences.network_latency_targets.len() > 13 {
        return Err(AppPreferencesError::Invalid(
            "自定义延迟目标最多支持 13 个".into(),
        ));
    }
    let mut urls = Vec::with_capacity(preferences.network_latency_targets.len());
    for target in &preferences.network_latency_targets {
        let normalized = NetworkLatencyTarget::new(&target.name, &target.url)
            .map_err(|error| AppPreferencesError::Invalid(error.to_string()))?;
        if urls.contains(&normalized.url) {
            return Err(AppPreferencesError::Invalid(
                "自定义延迟目标 URL 不可重复".into(),
            ));
        }
        urls.push(normalized.url);
    }
    let normalized = crate::normalize_system_proxy_bypass(&preferences.system_proxy_bypass)
        .map_err(|error| AppPreferencesError::Invalid(error.to_string()))?;
    if normalized != preferences.system_proxy_bypass {
        return Err(AppPreferencesError::Invalid(
            "系统代理绕过规则必须去除空行、首尾空格和重复项".into(),
        ));
    }
    let host = crate::normalize_system_proxy_host(&preferences.system_proxy_host)
        .map_err(|error| AppPreferencesError::Invalid(error.to_string()))?;
    if host != preferences.system_proxy_host {
        return Err(AppPreferencesError::Invalid(
            "系统代理主机必须去除首尾空格".into(),
        ));
    }
    let script = crate::normalize_pac_script(&preferences.system_proxy_pac_script)
        .map_err(|error| AppPreferencesError::Invalid(error.to_string()))?;
    if script != preferences.system_proxy_pac_script {
        return Err(AppPreferencesError::Invalid(
            "PAC 脚本必须使用规范化换行结尾".into(),
        ));
    }
    Ok(())
}

fn default_data_dir() -> AppPreferencesResult<PathBuf> {
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or(AppPreferencesError::MissingDataDirectory)
    };
    if cfg!(target_os = "macos") {
        Ok(home()?.join("Library/Application Support/ZenClash"))
    } else if cfg!(target_os = "windows") {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home()?.join("AppData/Local"))
            .join("ZenClash"))
    } else {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or(home()?.join(".local/share"))
            .join("zenclash"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_path(name: &str) -> PathBuf {
        let sequence = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "zenclash-preferences-{name}-{}-{sequence}.json",
            std::process::id()
        ))
    }

    #[test]
    fn saves_and_loads_preferences_atomically() {
        let path = test_path("roundtrip");
        let store = AppPreferencesStore::new(&path);
        let preferences = AppPreferences {
            core_kind: CoreKind::Meow,
            core_binaries: CoreBinaryPreferences {
                mihomo: None,
                meow: Some(PathBuf::from("/opt/zenclash/meow")),
            },
            last_known_good_core: Some(CoreKind::Mihomo),
            last_known_good_binary: Some(PathBuf::from("/opt/zenclash/mihomo")),
            appearance: AppearancePreference::Light,
            traffic_tray_visible: false,
            ..AppPreferences::default()
        };

        store.save(&preferences).unwrap();

        assert_eq!(store.load().unwrap(), preferences);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn replace_refuses_to_overwrite_a_newer_preference_snapshot() {
        let path = test_path("replace-conflict");
        let store = AppPreferencesStore::new(&path);
        let expected = store.load().unwrap();
        let mut newer = expected.clone();
        newer.appearance = AppearancePreference::Dark;
        store.save(&newer).unwrap();
        let mut stale_next = expected.clone();
        stale_next.traffic_tray_visible = false;

        assert!(matches!(
            store.replace(&expected, &stale_next),
            Err(AppPreferencesError::ConcurrentModification)
        ));
        assert_eq!(store.load().unwrap(), newer);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn update_preserves_fields_committed_by_another_store_clone() {
        let path = test_path("update-latest");
        let store = AppPreferencesStore::new(&path);
        let other_writer = store.clone();

        other_writer
            .update(|preferences| preferences.appearance = AppearancePreference::Dark)
            .unwrap();
        let updated = store
            .update(|preferences| preferences.traffic_tray_visible = false)
            .unwrap();

        assert_eq!(updated.appearance, AppearancePreference::Dark);
        assert!(!updated.traffic_tray_visible);
        assert_eq!(store.load().unwrap(), updated);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_unknown_fields_instead_of_losing_newer_settings() {
        let path = test_path("unknown");
        fs::write(
            &path,
            r#"{"version":1,"appearance":"dark","traffic_tray_visible":true,"traffic_history_enabled":true,"traffic_retention_days":30,"future":1}"#,
        )
        .unwrap();
        let store = AppPreferencesStore::new(&path);

        assert!(matches!(store.load(), Err(AppPreferencesError::Json(_))));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_invalid_traffic_retention() {
        let path = test_path("invalid-retention");
        let store = AppPreferencesStore::new(&path);
        let preferences = AppPreferences {
            traffic_retention_days: 0,
            ..AppPreferences::default()
        };

        assert!(matches!(
            store.save(&preferences),
            Err(AppPreferencesError::Invalid(_))
        ));
        assert!(!path.exists());
    }

    #[test]
    fn rejects_unnormalized_system_proxy_bypass_rules() {
        let path = test_path("invalid-bypass");
        let store = AppPreferencesStore::new(&path);
        let preferences = AppPreferences {
            system_proxy_bypass: vec![" localhost ".into(), "LOCALHOST".into()],
            ..AppPreferences::default()
        };

        assert!(matches!(
            store.save(&preferences),
            Err(AppPreferencesError::Invalid(_))
        ));
        assert!(!path.exists());
    }

    #[test]
    fn migrates_preferences_written_before_traffic_history_fields() {
        let path = test_path("legacy");
        fs::write(
            &path,
            r#"{"version":1,"appearance":"light","traffic_tray_visible":false}"#,
        )
        .unwrap();
        let store = AppPreferencesStore::new(&path);

        let preferences = store.load().unwrap();

        assert!(preferences.traffic_history_enabled);
        assert_eq!(preferences.core_kind, CoreKind::Mihomo);
        assert_eq!(preferences.traffic_retention_days, 30);
        assert!(preferences.log_file_enabled);
        assert_eq!(preferences.log_file_max_mebibytes, 10);
        assert_eq!(preferences.network_ip_provider, PublicIpProvider::IpSb);
        assert_eq!(
            preferences.network_probe_route,
            NetworkProbeRoutePreference::Mihomo
        );
        assert!(preferences.network_latency_targets.is_empty());
        assert_eq!(
            preferences.system_proxy_bypass,
            crate::default_system_proxy_bypass()
        );
        assert_eq!(preferences.system_proxy_mode, SystemProxyMode::Manual);
        assert_eq!(preferences.system_proxy_host, "127.0.0.1");
        assert_eq!(
            preferences.system_proxy_pac_script,
            crate::default_pac_script()
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_invalid_log_file_limit() {
        let path = test_path("invalid-log-limit");
        let store = AppPreferencesStore::new(&path);
        let preferences = AppPreferences {
            log_file_max_mebibytes: 0,
            ..AppPreferences::default()
        };

        assert!(matches!(
            store.save(&preferences),
            Err(AppPreferencesError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_duplicate_network_latency_targets() {
        let path = test_path("duplicate-network-targets");
        let store = AppPreferencesStore::new(&path);
        let target = NetworkLatencyTarget::new("One", "https://example.com/ping").unwrap();
        let preferences = AppPreferences {
            network_latency_targets: vec![target.clone(), target],
            ..AppPreferences::default()
        };

        assert!(matches!(
            store.save(&preferences),
            Err(AppPreferencesError::Invalid(_))
        ));
    }
}
