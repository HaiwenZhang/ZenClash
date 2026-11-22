//! Startup routing and cancellation before any ordinary managed process is created.

use std::path::PathBuf;

use zenclash_core::{CoreKind, MihomoError, ServiceHealthKind, ServiceStartupRejection};

/// Shared cancellation token propagated across every blocking and async stage
/// of application bootstrap.
#[derive(Clone)]
pub struct StartupCancellation {
    sender: tokio::sync::watch::Sender<bool>,
}

impl Default for StartupCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupCancellation {
    /// Creates an active, uncancelled startup token.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = tokio::sync::watch::channel(false);
        Self { sender }
    }

    /// Signals that the application is exiting and startup should abort promptly.
    pub fn cancel(&self) {
        let _ = self.sender.send_replace(true);
    }

    /// Returns `true` once [`Self::cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    /// Returns an `Interrupted` I/O error if startup has been cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::ErrorKind::Interrupted`] when [`Self::is_cancelled`] is `true`.
    pub fn ensure_active(&self) -> std::io::Result<()> {
        if self.is_cancelled() {
            Err(startup_cancelled_error())
        } else {
            Ok(())
        }
    }

    /// Resolves as soon as [`Self::cancel`] is triggered.
    pub async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        if *receiver.borrow_and_update() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                return;
            }
        }
    }

    /// Races `future` against startup cancellation, short-circuiting with
    /// [`std::io::ErrorKind::Interrupted`] when cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::ErrorKind::Interrupted`] if cancellation occurs
    /// before or immediately after `future` completes.
    pub async fn run_until_cancelled<F, T>(&self, future: F) -> std::io::Result<T>
    where
        F: std::future::Future<Output = T> + Send,
    {
        if self.is_cancelled() {
            return Err(startup_cancelled_error());
        }
        tokio::select! {
            biased;
            () = self.cancelled() => Err(startup_cancelled_error()),
            output = future => {
                if self.is_cancelled() {
                    Err(startup_cancelled_error())
                } else {
                    Ok(output)
                }
            }
        }
    }
}

/// Constructs the canonical `Interrupted` error representing a cancelled startup.
#[must_use]
pub fn startup_cancelled_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "application closed during startup",
    )
}

/// Returns `true` when `error` is a startup-cancellation `Interrupted` I/O error.
#[must_use]
pub fn is_startup_cancelled(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted)
}

/// Validated configuration layers collected before deciding the startup backend.
pub struct StartupLayers {
    /// Enabled YAML override file paths in application order.
    pub overrides: Vec<PathBuf>,
    /// User-facing recovery notices produced when quarantining damaged files.
    pub notices: Vec<String>,
    /// `true` when one or more layers could not be confirmed intact.
    pub unknown: bool,
}

/// Verifies or quarantines persistent profile, controlled-config, and YAML
/// override layers prior to core launch.
///
/// For managed Mihomo launches, damaged saved intent remains blocked across
/// restarts until explicit user recovery.
///
/// # Errors
///
/// Returns an error if required configuration layers cannot be loaded or quarantined.
pub fn prepare_configuration_layers(
    managed_mihomo: bool,
    profiles: &zenclash_core::ProfileStore,
    controlled: &zenclash_core::ControlledConfigStore,
    overrides: &zenclash_core::YamlOverrideStore,
) -> Result<StartupLayers, Box<dyn std::error::Error>> {
    if managed_mihomo {
        // Damaged saved intent must remain unreadable on every launch until explicit recovery.
        profiles.load()?;
        controlled.load()?;
        return Ok(StartupLayers {
            overrides: overrides.load_enabled_paths()?,
            notices: Vec::new(),
            unknown: false,
        });
    }
    let mut layers = StartupLayers {
        overrides: Vec::new(),
        notices: Vec::new(),
        unknown: false,
    };
    for (path, key) in [
        (
            profiles.quarantine_invalid_index()?,
            "startup.quarantine.profile_index",
        ),
        (
            controlled.quarantine_invalid_patch()?,
            "startup.quarantine.controlled_config",
        ),
        (
            overrides.quarantine_invalid_manifest()?,
            "startup.quarantine.overrides",
        ),
    ] {
        if let Some(path) = path {
            layers.unknown = true;
            layers.notices.push(zenclash_i18n::text_with(
                key,
                &[("path", path.display().to_string())],
            ));
        }
    }
    layers.overrides = overrides.load_enabled_paths()?;
    Ok(layers)
}

/// Reason why automatic managed startup was refused before launching a local kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupBlocked {
    /// Privileged service health did not authorize starting a competing local kernel.
    Health(ServiceHealthKind),
    /// Effective configuration / TUN intent could not be confirmed safely.
    ConfigurationUnknown,
}

/// Error returned by [`route_startup_with_policy`].
#[derive(Debug)]
pub enum StartupFailure<E> {
    /// Startup policy refused to launch a competing kernel.
    Blocked(StartupBlocked),
    /// The selected startup callback failed.
    Operation(E),
}

/// Operating system platform affecting service-to-local fallback policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPlatform {
    /// Windows (permits automatic sidecar fallback after idle verification).
    Windows,
    /// macOS (requires explicit user choice when an installed service is unavailable).
    Macos,
    /// Linux (requires explicit user choice when an installed service is unavailable).
    Linux,
}

impl StartupPlatform {
    /// Returns the compile-time target platform.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// Returns whether falling back to an ordinary local managed core is permitted
/// for the observed `health` state on `platform`.
///
/// Installed-but-unavailable Unix services stay behind an explicit user choice.
/// Windows permits automatic fallback after the separate native idle check.
#[must_use]
pub fn permits_local_fallback(
    platform: StartupPlatform,
    health: &ServiceHealthKind,
    explicit: bool,
) -> bool {
    match health {
        ServiceHealthKind::NotInstalled => true,
        ServiceHealthKind::Unavailable(_) | ServiceHealthKind::VersionMismatch => {
            platform == StartupPlatform::Windows || explicit
        }
        ServiceHealthKind::Ready | ServiceHealthKind::Unknown => false,
    }
}

/// Routes startup to either the privileged `service` path or the ordinary
/// `local` path according to platform policy, service health, and confirmed TUN
/// configuration.
///
/// # Errors
///
/// Returns [`StartupFailure::Blocked`] if policy forbids starting a competing
/// kernel, or [`StartupFailure::Operation`] if the chosen callback fails.
pub fn route_startup_with_policy<T, E>(
    kind: CoreKind,
    external: bool,
    health: Option<ServiceHealthKind>,
    tun_enabled: Option<bool>,
    explicit_local: bool,
    service: impl FnOnce() -> Result<T, E>,
    local: impl FnOnce() -> Result<T, E>,
) -> Result<T, StartupFailure<E>> {
    if external || kind != CoreKind::Mihomo {
        return local().map_err(StartupFailure::Operation);
    }
    let health = health.unwrap_or(ServiceHealthKind::Unknown);
    if explicit_local && tun_enabled.is_none() {
        return Err(StartupFailure::Blocked(
            StartupBlocked::ConfigurationUnknown,
        ));
    }
    if explicit_local && permits_local_fallback(StartupPlatform::current(), &health, true) {
        return local().map_err(StartupFailure::Operation);
    }
    if health == ServiceHealthKind::Ready {
        return service().map_err(StartupFailure::Operation);
    }
    if tun_enabled.is_none() {
        return Err(StartupFailure::Blocked(
            StartupBlocked::ConfigurationUnknown,
        ));
    }
    if permits_local_fallback(StartupPlatform::current(), &health, explicit_local) {
        return local().map_err(StartupFailure::Operation);
    }
    Err(StartupFailure::Blocked(StartupBlocked::Health(health)))
}

/// Inspects a merged effective JSON config to confirm whether `tun.enable` is
/// explicitly boolean (`Some(true)` / `Some(false)`), or `None` when unreadable.
#[must_use]
pub fn effective_tun_enabled(value: &serde_json::Value) -> Option<bool> {
    let config = value.as_object()?;
    let Some(tun) = config.get("tun") else {
        return Some(false);
    };
    let tun = tun.as_object()?;
    tun.get("enable")
        .map_or(Some(false), serde_json::Value::as_bool)
}

/// Formats a localized user-facing message for a [`StartupBlocked`] reason.
#[must_use]
pub fn blocked_message(reason: StartupBlocked) -> String {
    let key = match reason {
        StartupBlocked::ConfigurationUnknown => "startup.service_configuration_unconfirmed",
        StartupBlocked::Health(health) => match health {
            ServiceHealthKind::Unavailable(_) => "startup.service_unavailable",
            ServiceHealthKind::VersionMismatch => "startup.service_incompatible",
            _ => "startup.service_unknown",
        },
    };
    zenclash_i18n::text(key)
}

/// Formats a localized user-facing message for a service-connection error
/// without exposing raw IPC or credential details.
#[must_use]
pub fn connection_message(error: &MihomoError) -> String {
    let key = match error.service_startup_rejection() {
        Some(ServiceStartupRejection::Occupied) => "startup.service_occupied",
        Some(ServiceStartupRejection::Unauthorized) => "startup.service_unauthorized",
        Some(ServiceStartupRejection::Incompatible) => "startup.service_incompatible",
        Some(ServiceStartupRejection::MaintenancePending) => "startup.service_pending",
        None => "startup.service_unknown",
    };
    zenclash_i18n::text(key)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn startup_damaged_layers_remain_blocked_across_restarts() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-startup-layers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        for (directory, filename) in [
            ("profiles", "profiles.json"),
            ("controlled", "override.yaml"),
            ("overrides", "overrides.json"),
        ] {
            let test_root = root.join(directory);
            let profiles = zenclash_core::ProfileStore::new(test_root.join("profiles")).unwrap();
            let controlled =
                zenclash_core::ControlledConfigStore::new(test_root.join("controlled"));
            let overrides =
                zenclash_core::YamlOverrideStore::new(test_root.join("overrides")).unwrap();
            let damaged = test_root.join(directory).join(filename);
            std::fs::create_dir_all(damaged.parent().unwrap()).unwrap();
            std::fs::write(&damaged, b"[broken").unwrap();
            let starts = Cell::new(0);
            for attempt in 0..2 {
                let result = prepare_configuration_layers(true, &profiles, &controlled, &overrides);
                if let Ok(layers) = result {
                    let routed = route_startup_with_policy(
                        CoreKind::Mihomo,
                        false,
                        Some(ServiceHealthKind::NotInstalled),
                        (!layers.unknown).then_some(false),
                        false,
                        || Ok::<(), ()>(()),
                        || {
                            starts.set(starts.get() + 1);
                            Ok(())
                        },
                    );
                    assert!(
                        routed.is_err(),
                        "restart {attempt} accepted damaged {filename}"
                    );
                }
            }
            assert_eq!(starts.get(), 0);
            assert_eq!(std::fs::read(damaged).unwrap(), b"[broken");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_ready_service_never_launches_local_and_keeps_acquire_failure() {
        let local_starts = Cell::new(0);
        let acquired = Cell::new(false);
        let result = route_startup_with_policy(
            CoreKind::Mihomo,
            false,
            Some(ServiceHealthKind::Ready),
            Some(true),
            false,
            || {
                acquired.set(true);
                Err::<(), _>("occupied after health observation")
            },
            || {
                local_starts.set(local_starts.get() + 1);
                Ok(())
            },
        );
        assert!(
            acquired.get(),
            "startup must attempt the verified service owner"
        );
        assert_eq!(
            local_starts.get(),
            0,
            "Acquire failure must not start Local"
        );
        assert!(matches!(
            result,
            Err(StartupFailure::Operation(
                "occupied after health observation"
            ))
        ));
    }

    #[test]
    fn startup_unknown_health_or_configuration_never_runs_local_callback() {
        for (health, tun) in [
            (ServiceHealthKind::Unknown, Some(false)),
            (ServiceHealthKind::Unknown, Some(true)),
            (ServiceHealthKind::VersionMismatch, None),
            (ServiceHealthKind::NotInstalled, None),
        ] {
            let local_starts = Cell::new(0);
            let result = route_startup_with_policy(
                CoreKind::Mihomo,
                false,
                Some(health.clone()),
                tun,
                false,
                || Ok::<(), ()>(()),
                || {
                    local_starts.set(local_starts.get() + 1);
                    Ok(())
                },
            );
            assert_eq!(
                local_starts.get(),
                0,
                "blocked startup changed capture: {health:?}/{tun:?}"
            );
            assert!(matches!(result, Err(StartupFailure::Blocked(_))));
        }
    }

    #[test]
    fn startup_missing_service_with_disabled_tun_preserves_ordinary_local_start() {
        let started = Cell::new(0);
        let result = route_startup_with_policy(
            CoreKind::Mihomo,
            false,
            Some(ServiceHealthKind::NotInstalled),
            Some(false),
            false,
            || Err::<(), _>("unexpected Acquire"),
            || {
                started.set(started.get() + 1);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(started.get(), 1);
    }

    #[test]
    fn startup_explicit_external_does_not_acquire_mihomo_service() {
        let (kind, external) = (CoreKind::Mihomo, true);
        let result = route_startup_with_policy(
            kind,
            external,
            Some(ServiceHealthKind::Ready),
            Some(true),
            false,
            || Err::<(), _>("unexpected service adoption"),
            || Ok(()),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn platform_matrix_matches_verge_automatic_and_explicit_sidecar_choices() {
        for platform in [
            StartupPlatform::Windows,
            StartupPlatform::Macos,
            StartupPlatform::Linux,
        ] {
            assert!(permits_local_fallback(
                platform,
                &ServiceHealthKind::NotInstalled,
                false
            ));
            for health in [
                ServiceHealthKind::Unavailable("native transport unavailable".into()),
                ServiceHealthKind::VersionMismatch,
            ] {
                assert_eq!(
                    permits_local_fallback(platform, &health, false),
                    platform == StartupPlatform::Windows
                );
                assert!(permits_local_fallback(platform, &health, true));
            }
            for health in [ServiceHealthKind::Unknown, ServiceHealthKind::Ready] {
                assert!(!permits_local_fallback(platform, &health, false));
                assert!(!permits_local_fallback(platform, &health, true));
            }
        }
    }

    #[test]
    fn startup_unreadable_or_nonboolean_tun_does_not_prove_disabled() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"tun":true}),
            serde_json::json!({"tun":{"enable":"false"}}),
        ] {
            assert_eq!(effective_tun_enabled(&value), None);
        }
        assert_eq!(
            effective_tun_enabled(&serde_json::json!({"tun":{"enable":false}})),
            Some(false)
        );
        assert_eq!(
            effective_tun_enabled(&serde_json::json!({"rules":[]})),
            Some(false)
        );
    }

    #[test]
    fn startup_connection_error_preserves_unknown_without_exposing_raw_details() {
        let error = MihomoError::Process("private IPC details and controller credentials".into());
        let message = connection_message(&error);
        assert_eq!(
            message,
            blocked_message(StartupBlocked::Health(ServiceHealthKind::Unknown))
        );
        assert!(!message.contains("credentials"));
        assert_ne!(
            blocked_message(StartupBlocked::Health(ServiceHealthKind::VersionMismatch)),
            message
        );
    }

    #[tokio::test]
    async fn startup_cancellation_interrupts_pending_wait_promptly() {
        let cancellation = StartupCancellation::new();
        let trigger = cancellation.clone();
        let started = std::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            trigger.cancel();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            cancellation.run_until_cancelled(std::future::pending::<()>()),
        )
        .await
        .expect("cancellation must interrupt pending startup waits promptly");
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn startup_cancellation_recognizes_interrupted_errors_only() {
        let cancelled: Box<dyn std::error::Error> = Box::new(startup_cancelled_error());
        let other: Box<dyn std::error::Error> = Box::new(std::io::Error::other("startup failed"));
        assert!(is_startup_cancelled(cancelled.as_ref()));
        assert!(!is_startup_cancelled(other.as_ref()));
    }
}
