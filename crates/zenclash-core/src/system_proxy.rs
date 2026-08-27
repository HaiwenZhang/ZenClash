//! Native desktop system-proxy discovery and control.

use std::{net::IpAddr, sync::Arc};

use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod command;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
mod pac;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(any(target_os = "windows", test))]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
use unsupported as platform;
#[cfg(target_os = "windows")]
use windows as platform;

use crate::{AppPreferences, AppPreferencesError, AppPreferencesStore, MihomoError, MihomoResult};

pub use pac::{PacServer, PacServerStatus, default_pac_script, normalize_pac_script};

const MAX_BYPASS_ENTRIES: usize = 64;
const MAX_BYPASS_ENTRY_BYTES: usize = 253;

/// Native system-proxy mode selected by the user.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SystemProxyMode {
    /// Configure explicit HTTP and HTTPS proxy endpoints.
    #[default]
    Manual,
    /// Configure an automatic proxy configuration URL.
    Pac,
}

/// Current HTTP and HTTPS proxy state reported by the desktop operating system.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemProxyStatus {
    /// Human-readable network service or desktop proxy backend name.
    pub service: String,
    /// Whether the HTTP proxy is enabled.
    pub enabled: bool,
    /// HTTP proxy host.
    pub server: String,
    /// HTTP proxy port.
    pub port: u16,
    /// Whether the HTTPS proxy is enabled.
    pub secure_enabled: bool,
    /// HTTPS proxy host.
    pub secure_server: String,
    /// HTTPS proxy port.
    pub secure_port: u16,
    /// Native proxy-bypass entries reported by the operating system.
    pub bypass: Vec<String>,
    /// Whether automatic proxy configuration is enabled.
    pub auto_enabled: bool,
    /// Automatic proxy configuration URL reported by the OS.
    pub auto_url: String,
}

/// Exact native proxy state last applied and verified by ZenClash.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SystemProxyOwnership {
    /// Verified manual HTTP/HTTPS endpoint and bypass rules.
    Manual {
        /// Native network service selected during the write.
        service: String,
        /// Normalized HTTP/HTTPS proxy host.
        host: String,
        /// HTTP/HTTPS proxy port.
        port: u16,
        /// Normalized bypass rules observed after the write.
        bypass: Vec<String>,
    },
    /// Verified automatic proxy URL.
    Pac {
        /// Native network service selected during the write.
        service: String,
        /// PAC URL observed after the write.
        url: String,
    },
}

pub(crate) fn validate_system_proxy_ownership(
    ownership: &SystemProxyOwnership,
) -> MihomoResult<()> {
    let service = match ownership {
        SystemProxyOwnership::Manual {
            service,
            host,
            port,
            bypass,
        } => {
            if *port == 0 {
                return Err(MihomoError::Process("系统代理所有权端口不能为 0".into()));
            }
            if normalize_system_proxy_host(host)? != *host {
                return Err(MihomoError::Process(
                    "系统代理所有权主机不是规范形式".into(),
                ));
            }
            if normalize_system_proxy_bypass(bypass)? != *bypass {
                return Err(MihomoError::Process(
                    "系统代理所有权绕过规则不是规范形式".into(),
                ));
            }
            service
        }
        SystemProxyOwnership::Pac { service, url } => {
            if normalize_pac_url(url)? != *url {
                return Err(MihomoError::Process(
                    "系统代理所有权 PAC URL 不是规范形式".into(),
                ));
            }
            service
        }
    };
    if service.trim().is_empty() || service.len() > 256 || service.trim() != service {
        return Err(MihomoError::Process("系统代理所有权服务名称无效".into()));
    }
    Ok(())
}

impl SystemProxyStatus {
    /// Returns whether either manual or automatic system proxying is active.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.auto_enabled || self.enabled || self.secure_enabled
    }
}

/// Controller for the platform's active desktop system-proxy service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxyManager {
    service: String,
}

/// Coordinates the native proxy backend with the process-local PAC service.
#[derive(Clone, Debug, Default)]
pub struct SystemProxyController {
    pac_server: PacServer,
    operation: Arc<Mutex<()>>,
}

/// Exclusive native system-proxy operation owned by a controller clone.
///
/// Keep this guard alive while coordinating a native write with persistent
/// application state. This prevents page, tray, startup-reconciliation, and
/// shutdown workflows from interleaving their writes.
pub struct SystemProxyOperation<'a> {
    controller: &'a SystemProxyController,
    _guard: MutexGuard<'a, ()>,
}

/// Persisted manual/PAC settings independent of the enabled intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemProxySettings {
    /// Native system-proxy mode.
    pub mode: SystemProxyMode,
    /// Manual proxy or local PAC-listener host.
    pub host: String,
    /// Native bypass entries.
    pub bypass: Vec<String>,
    /// PAC JavaScript served in automatic mode.
    pub pac_script: String,
}

impl SystemProxySettings {
    /// Builds settings from a persisted preference snapshot.
    #[must_use]
    pub fn from_preferences(preferences: &AppPreferences) -> Self {
        Self {
            mode: preferences.system_proxy_mode,
            host: preferences.system_proxy_host.clone(),
            bypass: preferences.system_proxy_bypass.clone(),
            pac_script: preferences.system_proxy_pac_script.clone(),
        }
    }

    fn normalized(mut self) -> SystemProxySessionResult<Self> {
        self.host = normalize_system_proxy_host(&self.host)?;
        self.bypass = normalize_system_proxy_bypass(&self.bypass)?;
        self.pac_script = normalize_pac_script(&self.pac_script)?;
        Ok(self)
    }

    fn apply_to(&self, preferences: &mut AppPreferences) {
        preferences.system_proxy_mode = self.mode;
        preferences.system_proxy_host.clone_from(&self.host);
        preferences.system_proxy_bypass.clone_from(&self.bypass);
        preferences
            .system_proxy_pac_script
            .clone_from(&self.pac_script);
    }
}

/// Why owned native state was released during reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemProxyReleaseReason {
    /// The selected runtime core is unavailable.
    CoreUnavailable,
    /// The runtime core exposes no HTTP or Mixed listener.
    MissingPort,
}

/// Result of reconciling persistent intent with native state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemProxyReconcileOutcome {
    /// No owned native state needed a change.
    Unchanged,
    /// Enabled intent was applied and its ownership was persisted.
    Restored,
    /// Previously owned native state was released while enabled intent remained persistent.
    Released {
        /// Reason the native state cannot remain active.
        reason: SystemProxyReleaseReason,
        /// Whether the operating-system state still matched ZenClash ownership.
        native_matched: bool,
    },
}

/// Errors produced by native-state and preference transactions.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SystemProxySessionError {
    /// Persistent preference access failed.
    #[error(transparent)]
    Preferences(#[from] AppPreferencesError),
    /// Native validation, write, or readback failed.
    #[error(transparent)]
    Native(#[from] MihomoError),
    /// Enabling was requested without a usable proxy port.
    #[error("启用系统代理需要可用的 HTTP 或 Mixed 端口")]
    MissingPort,
    /// Native state was enabled but no verified ownership was returned.
    #[error("系统代理启用后未返回可验证所有权")]
    MissingOwnership,
    /// Persistence failed after a native write and recovery was attempted.
    #[error("系统代理事务失败：{0}")]
    Transaction(String),
}

/// Result type for system-proxy intent transactions.
pub type SystemProxySessionResult<T> = Result<T, SystemProxySessionError>;

/// Deep interface joining persistent intent, native state, and verified ownership.
#[derive(Clone)]
pub struct SystemProxySession {
    store: AppPreferencesStore,
    controller: SystemProxyController,
}

impl SystemProxySession {
    /// Opens a system-proxy session over shared preference and native owners.
    #[must_use]
    pub fn new(store: AppPreferencesStore, controller: SystemProxyController) -> Self {
        Self { store, controller }
    }

    /// Applies enabled intent and commits verified ownership atomically.
    ///
    /// # Errors
    ///
    /// Returns validation, native, persistence, or rollback errors.
    pub fn set_enabled(
        &self,
        enabled: bool,
        port: u16,
    ) -> SystemProxySessionResult<AppPreferences> {
        if enabled && port == 0 {
            return Err(SystemProxySessionError::MissingPort);
        }
        let operation = self.controller.begin_operation();
        let expected = self.store.load()?;
        let settings = SystemProxySettings::from_preferences(&expected);
        let ownership = if enabled {
            Some(apply_owned_system_proxy(&operation, port, &settings)?)
        } else {
            release_system_proxy(&operation, &expected)?;
            None
        };
        match self.store.update(|preferences| {
            preferences.system_proxy_enabled = enabled;
            preferences.system_proxy_ownership.clone_from(&ownership);
        }) {
            Ok(preferences) => Ok(preferences),
            Err(error) => Err(SystemProxySessionError::Transaction(
                restore_after_persist_failure(
                    &self.store,
                    &operation,
                    port,
                    &expected,
                    &settings,
                    &error.to_string(),
                ),
            )),
        }
    }

    /// Saves normalized manual/PAC settings and reapplies them when intent is enabled.
    ///
    /// # Errors
    ///
    /// Returns validation, native, persistence, or rollback errors.
    pub fn save_settings(
        &self,
        settings: SystemProxySettings,
        port: u16,
    ) -> SystemProxySessionResult<AppPreferences> {
        let settings = settings.normalized()?;
        let operation = self.controller.begin_operation();
        let expected = self.store.load()?;
        let previous = SystemProxySettings::from_preferences(&expected);
        let active = expected.system_proxy_enabled;
        if active && port == 0 {
            return Err(SystemProxySessionError::MissingPort);
        }
        let ownership = active
            .then(|| apply_owned_system_proxy(&operation, port, &settings))
            .transpose()?;
        match self.store.update(|preferences| {
            settings.apply_to(preferences);
            if active {
                preferences.system_proxy_ownership.clone_from(&ownership);
            }
        }) {
            Ok(preferences) => Ok(preferences),
            Err(error) if active => Err(SystemProxySessionError::Transaction(
                restore_after_persist_failure(
                    &self.store,
                    &operation,
                    port,
                    &expected,
                    &previous,
                    &error.to_string(),
                ),
            )),
            Err(error) => Err(error.into()),
        }
    }

    /// Reconciles persistent enabled intent with core availability and native ownership.
    ///
    /// Enabled intent remains persistent when the core is temporarily unavailable;
    /// only owned native state is released.
    ///
    /// # Errors
    ///
    /// Returns native, persistence, or recovery errors.
    pub fn reconcile(
        &self,
        core_available: bool,
        port: Option<u16>,
    ) -> SystemProxySessionResult<SystemProxyReconcileOutcome> {
        let operation = self.controller.begin_operation();
        let preferences = self.store.load()?;
        if !preferences.system_proxy_enabled {
            return Ok(SystemProxyReconcileOutcome::Unchanged);
        }
        let release_reason = if !core_available {
            Some(SystemProxyReleaseReason::CoreUnavailable)
        } else if port.is_none() {
            Some(SystemProxyReleaseReason::MissingPort)
        } else {
            None
        };
        if let Some(reason) = release_reason {
            let Some(ownership) = preferences.system_proxy_ownership else {
                return Ok(SystemProxyReconcileOutcome::Unchanged);
            };
            let native_matched = operation.release_if_owned(&ownership)?;
            self.store
                .update(|preferences| preferences.system_proxy_ownership = None)?;
            return Ok(SystemProxyReconcileOutcome::Released {
                reason,
                native_matched,
            });
        }

        let settings = SystemProxySettings::from_preferences(&preferences);
        let ownership =
            apply_owned_system_proxy(&operation, port.expect("available port checked"), &settings)?;
        if let Err(error) = self.store.update(|preferences| {
            preferences.system_proxy_ownership = Some(ownership.clone());
        }) {
            let release = operation.release_if_owned(&ownership);
            return Err(SystemProxySessionError::Transaction(match release {
                Ok(_) => format!("保存所有权失败，已释放新写入的系统代理：{error}"),
                Err(release) => {
                    format!("保存所有权失败：{error}；释放新写入状态失败：{release}")
                }
            }));
        }
        Ok(SystemProxyReconcileOutcome::Restored)
    }

    /// Releases native state only when it still matches persisted ZenClash ownership.
    ///
    /// # Errors
    ///
    /// Returns native or persistence errors.
    pub fn release_owned(&self) -> SystemProxySessionResult<bool> {
        let operation = self.controller.begin_operation();
        let preferences = self.store.load()?;
        if !preferences.system_proxy_enabled {
            return Ok(false);
        }
        let Some(ownership) = preferences.system_proxy_ownership else {
            return Ok(false);
        };
        let released = operation.release_if_owned(&ownership)?;
        self.store
            .update(|preferences| preferences.system_proxy_ownership = None)?;
        Ok(released)
    }
}

fn apply_owned_system_proxy(
    operation: &SystemProxyOperation<'_>,
    port: u16,
    settings: &SystemProxySettings,
) -> SystemProxySessionResult<SystemProxyOwnership> {
    if port == 0 {
        return Err(SystemProxySessionError::MissingPort);
    }
    operation
        .apply(
            true,
            settings.mode,
            &settings.host,
            port,
            &settings.bypass,
            &settings.pac_script,
        )?
        .ok_or(SystemProxySessionError::MissingOwnership)
}

fn release_system_proxy(
    operation: &SystemProxyOperation<'_>,
    preferences: &AppPreferences,
) -> SystemProxySessionResult<()> {
    if let Some(ownership) = &preferences.system_proxy_ownership {
        operation.release_if_owned(ownership)?;
    } else {
        operation.set_enabled(false, preferences.system_proxy_mode, "", 0, &[], "")?;
    }
    Ok(())
}

fn restore_after_persist_failure(
    store: &AppPreferencesStore,
    operation: &SystemProxyOperation<'_>,
    current_port: u16,
    expected: &AppPreferences,
    previous: &SystemProxySettings,
    error: &str,
) -> String {
    if !expected.system_proxy_enabled {
        return match operation.set_enabled(false, previous.mode, "", 0, &[], "") {
            Ok(()) => format!("保存失败，已释放新写入的系统代理：{error}"),
            Err(rollback) => format!("保存失败：{error}；释放新写入状态失败：{rollback}"),
        };
    }
    let previous_port = ownership_port(expected.system_proxy_ownership.as_ref())
        .filter(|port| *port != 0)
        .unwrap_or(current_port);
    match apply_owned_system_proxy(operation, previous_port, previous) {
        Ok(ownership) => {
            if expected.system_proxy_ownership.as_ref() == Some(&ownership) {
                return format!("保存失败，已恢复上一系统代理状态：{error}");
            }
            match store.update(|preferences| {
                preferences.system_proxy_ownership = Some(ownership.clone());
            }) {
                Ok(_) => format!("保存失败，已恢复上一系统代理状态：{error}"),
                Err(ownership_error) => match operation.release_if_owned(&ownership) {
                    Ok(_) => format!(
                        "保存失败：{error}；恢复后的所有权保存失败并已释放：{ownership_error}"
                    ),
                    Err(release_error) => format!(
                        "保存失败：{error}；恢复后的所有权保存失败：{ownership_error}；释放失败：{release_error}"
                    ),
                },
            }
        }
        Err(rollback) => format!("保存失败：{error}；恢复上一系统代理状态失败：{rollback}"),
    }
}

fn ownership_port(ownership: Option<&SystemProxyOwnership>) -> Option<u16> {
    match ownership {
        Some(SystemProxyOwnership::Manual { port, .. }) => Some(*port),
        _ => None,
    }
}

impl SystemProxyController {
    /// Creates a controller backed by a shared PAC service owner.
    #[must_use]
    pub fn new(pac_server: PacServer) -> Self {
        Self {
            pac_server,
            operation: Arc::default(),
        }
    }

    /// Acquires the shared native-operation lock used by every controller clone.
    #[must_use]
    pub fn begin_operation(&self) -> SystemProxyOperation<'_> {
        SystemProxyOperation {
            controller: self,
            _guard: self.operation.lock(),
        }
    }

    /// Applies the selected proxy mode and verifies native state after writing.
    ///
    /// The PAC listener is started before its URL becomes visible to the OS and
    /// remains alive if disabling the native proxy fails. Switching to manual
    /// mode stops the obsolete PAC listener only after the OS accepts the new
    /// endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error from validation, PAC startup, platform detection, the
    /// native write, or state readback.
    pub fn set_enabled(
        &self,
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> MihomoResult<()> {
        self.begin_operation()
            .set_enabled(enabled, mode, host, port, bypass, pac_script)
    }

    fn apply_unlocked(
        &self,
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> MihomoResult<Option<SystemProxyOwnership>> {
        let manager = SystemProxyManager::detect()?;
        if !enabled {
            manager.set_enabled_with_bypass(false, "", 0, &[])?;
            self.pac_server.stop();
            return Ok(None);
        }
        let ownership = match mode {
            SystemProxyMode::Manual => {
                manager.set_enabled_with_bypass(true, host, port, bypass)?;
                self.pac_server.stop();
                let status = manager.status()?;
                SystemProxyOwnership::Manual {
                    service: manager.service().to_owned(),
                    host: status.server,
                    port: status.port,
                    bypass: status.bypass,
                }
            }
            SystemProxyMode::Pac => {
                let pac = self.pac_server.start(host, pac_script, port)?;
                if let Err(error) = manager.set_pac_enabled(true, &pac.url) {
                    self.pac_server.stop();
                    return Err(error);
                }
                SystemProxyOwnership::Pac {
                    service: manager.service().to_owned(),
                    url: pac.url,
                }
            }
        };
        Ok(Some(ownership))
    }

    /// Returns the process-local PAC listener status, when present.
    #[must_use]
    pub fn pac_status(&self) -> Option<PacServerStatus> {
        self.pac_server.status()
    }
}

impl SystemProxyOperation<'_> {
    /// Applies one native proxy state without reacquiring the controller lock.
    ///
    /// # Errors
    ///
    /// Returns an error from validation, PAC startup, platform detection, the
    /// native write, or state readback.
    pub fn set_enabled(
        &self,
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> MihomoResult<()> {
        self.apply(enabled, mode, host, port, bypass, pac_script)
            .map(drop)
    }

    /// Applies native proxy state and returns verified ownership when enabled.
    ///
    /// # Errors
    ///
    /// Returns an error from validation, native writes, PAC startup, or state
    /// readback.
    pub fn apply(
        &self,
        enabled: bool,
        mode: SystemProxyMode,
        host: &str,
        port: u16,
        bypass: &[String],
        pac_script: &str,
    ) -> MihomoResult<Option<SystemProxyOwnership>> {
        self.controller
            .apply_unlocked(enabled, mode, host, port, bypass, pac_script)
    }

    /// Disables the native proxy only if it still matches ZenClash's last
    /// verified write. A replaced third-party proxy is left untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when native service detection, status readback, or the
    /// owned-state clear fails.
    pub fn release_if_owned(&self, ownership: &SystemProxyOwnership) -> MihomoResult<bool> {
        let manager = SystemProxyManager::detect()?;
        let status = manager.status()?;
        let matches = match ownership {
            SystemProxyOwnership::Manual {
                service,
                host,
                port,
                bypass,
            } => {
                manager.service() == service
                    && !status.auto_enabled
                    && status.enabled
                    && status.secure_enabled
                    && status.server == *host
                    && status.secure_server == *host
                    && status.port == *port
                    && status.secure_port == *port
                    && status.bypass == *bypass
            }
            SystemProxyOwnership::Pac { service, url } => {
                manager.service() == service
                    && status.auto_enabled
                    && status.auto_url == *url
                    && !status.enabled
                    && !status.secure_enabled
            }
        };
        if matches {
            manager.set_enabled_with_bypass(false, "", 0, &[])?;
        }
        if matches!(ownership, SystemProxyOwnership::Pac { .. }) {
            self.controller.pac_server.stop();
        }
        Ok(matches)
    }
}

impl SystemProxyManager {
    /// Detects the desktop proxy backend and its active network service.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform is unsupported or its native proxy
    /// command is unavailable.
    pub fn detect() -> MihomoResult<Self> {
        platform::detect().map(|service| Self { service })
    }

    /// Returns the human-readable service selected during detection.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Reads the current HTTP and HTTPS proxy settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform command fails or its settings cannot
    /// be queried.
    pub fn status(&self) -> MihomoResult<SystemProxyStatus> {
        platform::status(&self.service)
    }

    /// Enables or disables the HTTP and HTTPS proxy for this service.
    ///
    /// When enabling, the backend disables the active proxy before writing all
    /// values and only re-enables it after every write succeeds. This favors a
    /// safe disabled state over exposing a partially configured proxy.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty host, a zero port, or a failed native
    /// platform command.
    pub fn set_enabled(&self, enabled: bool, server: &str, port: u16) -> MihomoResult<()> {
        self.set_enabled_with_bypass(enabled, server, port, &default_system_proxy_bypass())
    }

    /// Enables or disables the HTTP and HTTPS proxy with explicit bypass rules.
    ///
    /// Entries are normalized, validated, deduplicated, written through the
    /// native backend, and then read back. A successful return therefore means
    /// the requested endpoint and bypass list are observable from the OS.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid endpoint or bypass rule, a failed native
    /// command, or a state that does not match after the write.
    pub fn set_enabled_with_bypass(
        &self,
        enabled: bool,
        server: &str,
        port: u16,
        bypass: &[String],
    ) -> MihomoResult<()> {
        let server = if enabled {
            if port == 0 {
                return Err(MihomoError::Process("系统代理端口不能为 0".into()));
            }
            normalize_system_proxy_host(server)?
        } else {
            String::new()
        };
        let bypass = normalize_system_proxy_bypass(bypass)?;
        platform::set_enabled(&self.service, enabled, &server, port, &bypass)?;
        let status = self.status()?;
        if !enabled {
            if status.active() {
                return Err(MihomoError::Process(
                    "系统代理关闭后状态回读仍显示为启用".into(),
                ));
            }
            return Ok(());
        }
        let endpoint_matches = status.enabled
            && status.secure_enabled
            && status.server == server
            && status.secure_server == server
            && status.port == port
            && status.secure_port == port;
        if !endpoint_matches || status.bypass != bypass {
            return Err(MihomoError::Process(format!(
                "系统代理写后回读不一致：期望 {server}:{port} / {:?}，实际 HTTP {}:{}、HTTPS {}:{} / {:?}",
                bypass,
                status.server,
                status.port,
                status.secure_server,
                status.secure_port,
                status.bypass
            )));
        }
        Ok(())
    }

    /// Enables an operating-system automatic proxy configuration URL.
    ///
    /// Enabling PAC first disables manual HTTP/HTTPS proxying. The resulting
    /// automatic URL is read back before this method reports success.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid HTTP URL, a failed native command, or a
    /// state that does not match after the write.
    pub fn set_pac_enabled(&self, enabled: bool, url: &str) -> MihomoResult<()> {
        let url = if enabled {
            normalize_pac_url(url)?
        } else {
            String::new()
        };
        platform::set_pac_enabled(&self.service, enabled, &url)?;
        let status = self.status()?;
        if enabled {
            if !status.auto_enabled || status.auto_url != url {
                return Err(MihomoError::Process(format!(
                    "PAC 写后回读不一致：期望 {url}，实际启用={}、URL={}",
                    status.auto_enabled, status.auto_url
                )));
            }
        } else if status.active() {
            return Err(MihomoError::Process(
                "系统代理关闭后状态回读仍显示为启用".into(),
            ));
        }
        Ok(())
    }
}

/// Trims and validates a host used by manual proxying or the PAC listener.
///
/// # Errors
///
/// Returns an error for an empty, oversized, or syntactically unsafe host.
pub fn normalize_system_proxy_host(host: &str) -> MihomoResult<String> {
    let host = host.trim();
    if host.is_empty() || host.len() > MAX_BYPASS_ENTRY_BYTES {
        return Err(MihomoError::Process("系统代理主机无效".into()));
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if address.is_unspecified() {
            return Err(MihomoError::Process("系统代理主机不能是未指定地址".into()));
        }
        return Ok(host.to_owned());
    }
    let valid = !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        });
    if !valid {
        return Err(MihomoError::Process(format!(
            "系统代理主机格式无效：{host}"
        )));
    }
    Ok(host.to_owned())
}

fn normalize_pac_url(value: &str) -> MihomoResult<String> {
    let url = reqwest::Url::parse(value.trim())
        .map_err(|error| MihomoError::Process(format!("PAC URL 无效：{error}")))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(MihomoError::Process(
            "PAC URL 必须是无凭据的 HTTP 地址".into(),
        ));
    }
    Ok(url.to_string())
}

/// Returns the platform-appropriate default system-proxy bypass list.
#[must_use]
pub fn default_system_proxy_bypass() -> Vec<String> {
    #[cfg(target_os = "windows")]
    const DEFAULTS: &[&str] = &[
        "localhost",
        "127.*",
        "10.*",
        "172.16.*",
        "172.17.*",
        "172.18.*",
        "172.19.*",
        "172.20.*",
        "172.21.*",
        "172.22.*",
        "172.23.*",
        "172.24.*",
        "172.25.*",
        "172.26.*",
        "172.27.*",
        "172.28.*",
        "172.29.*",
        "172.30.*",
        "172.31.*",
        "192.168.*",
        "<local>",
    ];
    #[cfg(target_os = "macos")]
    const DEFAULTS: &[&str] = &[
        "127.0.0.1",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "localhost",
        "*.local",
        "*.crashlytics.com",
        "<local>",
    ];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const DEFAULTS: &[&str] = &[
        "localhost",
        "127.0.0.1",
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "::1",
    ];
    DEFAULTS.iter().map(|entry| (*entry).to_owned()).collect()
}

/// Trims, validates, and case-insensitively deduplicates proxy-bypass entries.
///
/// Empty lines are ignored so this function can consume a multiline editor
/// directly. The accepted syntax covers IP addresses, CIDR ranges, hostnames,
/// wildcard host patterns, and the native `<local>` marker.
///
/// # Errors
///
/// Returns an error when there are too many rules or an entry is oversized,
/// syntactically unsafe, or contains no alphanumeric host/IP content.
pub fn normalize_system_proxy_bypass(entries: &[String]) -> MihomoResult<Vec<String>> {
    let mut normalized = Vec::with_capacity(entries.len().min(MAX_BYPASS_ENTRIES));
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry.len() > MAX_BYPASS_ENTRY_BYTES {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则超过 {MAX_BYPASS_ENTRY_BYTES} 字节：{entry}"
            )));
        }
        if !is_valid_bypass_entry(entry) {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则格式无效：{entry}"
            )));
        }
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(entry))
        {
            continue;
        }
        if normalized.len() >= MAX_BYPASS_ENTRIES {
            return Err(MihomoError::Process(format!(
                "系统代理绕过规则最多支持 {MAX_BYPASS_ENTRIES} 条"
            )));
        }
        normalized.push(entry.to_owned());
    }
    Ok(normalized)
}

fn is_valid_bypass_entry(entry: &str) -> bool {
    if entry.eq_ignore_ascii_case("<local>") || entry.parse::<IpAddr>().is_ok() {
        return true;
    }
    if let Some((address, prefix)) = entry.split_once('/') {
        if address.contains('/') || prefix.contains('/') {
            return false;
        }
        let Ok(address) = address.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix) = prefix.parse::<u8>() else {
            return false;
        };
        return prefix <= if address.is_ipv4() { 32 } else { 128 };
    }
    if entry.contains(['<', '>', ':', '[', ']']) || entry.starts_with('.') || entry.ends_with('.') {
        return false;
    }
    entry.split('.').all(|label| {
        !label.is_empty()
            && (label == "*"
                || label
                    .chars()
                    .any(|character| character.is_ascii_alphanumeric()))
            && label.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '*' | '?')
            })
    })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use super::{
        SystemProxyController, SystemProxyManager, SystemProxyMode, SystemProxySession,
        SystemProxySessionError, SystemProxySettings, normalize_system_proxy_bypass,
        normalize_system_proxy_host,
    };
    use crate::AppPreferencesStore;

    #[test]
    fn controller_clones_serialize_complete_native_operations() {
        let controller = Arc::new(SystemProxyController::default());
        let (first_acquired, first_acquired_rx) = mpsc::channel();
        let (release_first, release_first_rx) = mpsc::channel();
        let first = {
            let controller = Arc::clone(&controller);
            thread::spawn(move || {
                let _operation = controller.begin_operation();
                first_acquired.send(()).unwrap();
                release_first_rx.recv().unwrap();
            })
        };
        first_acquired_rx.recv().unwrap();

        let (second_acquired, second_acquired_rx) = mpsc::channel();
        let second = {
            let controller = Arc::clone(&controller);
            thread::spawn(move || {
                let _operation = controller.begin_operation();
                second_acquired.send(()).unwrap();
            })
        };

        assert!(
            second_acquired_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        release_first.send(()).unwrap();
        second_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        first.join().unwrap();
        second.join().unwrap();
    }

    #[test]
    fn rejects_invalid_proxy_endpoint_before_platform_command() {
        let manager = SystemProxyManager {
            service: "test-service".into(),
        };

        assert!(manager.set_enabled(true, "   ", 7890).is_err());
        assert!(manager.set_enabled(true, "127.0.0.1", 0).is_err());
    }

    #[test]
    fn bypass_normalization_trims_ignores_empty_and_deduplicates() {
        let normalized = normalize_system_proxy_bypass(&[
            " localhost ".into(),
            String::new(),
            "LOCALHOST".into(),
            "192.168.0.0/16".into(),
            "*.example.com".into(),
        ])
        .unwrap();

        assert_eq!(normalized, ["localhost", "192.168.0.0/16", "*.example.com"]);
    }

    #[test]
    fn bypass_normalization_rejects_command_and_gvariant_delimiters() {
        let error = normalize_system_proxy_bypass(&["localhost';evil".into()]).unwrap_err();

        assert!(error.to_string().contains("格式无效"));
    }

    #[test]
    fn bypass_normalization_rejects_invalid_cidr_prefix() {
        let error = normalize_system_proxy_bypass(&["192.168.0.0/99".into()]).unwrap_err();

        assert!(error.to_string().contains("格式无效"));
    }

    #[test]
    fn bypass_normalization_accepts_a_wildcard_dns_label_but_not_empty_labels() {
        assert_eq!(
            normalize_system_proxy_bypass(&["*.example.com".into()]).unwrap(),
            ["*.example.com"]
        );
        assert!(normalize_system_proxy_bypass(&["example..com".into()]).is_err());
    }

    #[test]
    fn system_proxy_host_accepts_loopback_and_rejects_unspecified_addresses() {
        assert_eq!(
            normalize_system_proxy_host(" localhost ").unwrap(),
            "localhost"
        );
        assert!(normalize_system_proxy_host("0.0.0.0").is_err());
    }

    #[test]
    fn inactive_settings_are_normalized_and_persisted_without_native_writes() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-system-proxy-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = AppPreferencesStore::new(root.join("preferences.json"));
        let session = SystemProxySession::new(store.clone(), SystemProxyController::default());

        let saved = session
            .save_settings(
                SystemProxySettings {
                    mode: SystemProxyMode::Manual,
                    host: " localhost ".into(),
                    bypass: vec![" localhost ".into(), "LOCALHOST".into()],
                    pac_script: super::default_pac_script().into(),
                },
                0,
            )
            .unwrap();

        assert!(!saved.system_proxy_enabled);
        assert_eq!(saved.system_proxy_host, "localhost");
        assert_eq!(saved.system_proxy_bypass, ["localhost"]);
        assert_eq!(store.load().unwrap(), saved);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enabled_intent_without_a_port_is_rejected_before_native_or_persistent_change() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-system-proxy-port-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = AppPreferencesStore::new(root.join("preferences.json"));
        let session = SystemProxySession::new(store.clone(), SystemProxyController::default());

        let error = session.set_enabled(true, 0).unwrap_err();

        assert!(matches!(error, SystemProxySessionError::MissingPort));
        assert!(!store.load().unwrap().system_proxy_enabled);
    }
}
