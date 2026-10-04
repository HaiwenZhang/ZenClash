//! Startup routing before any ordinary managed process is created.

use zenclash_core::{CoreKind, MihomoError, ServiceHealthKind, ServiceStartupRejection};

pub(crate) struct StartupLayers {
    pub(crate) overrides: Vec<std::path::PathBuf>,
    pub(crate) notices: Vec<String>,
    pub(crate) unknown: bool,
}

pub(crate) fn prepare_configuration_layers(
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupBlocked {
    Health(ServiceHealthKind),
    ConfigurationUnknown,
}

#[derive(Debug)]
pub(crate) enum StartupFailure<E> {
    Blocked(StartupBlocked),
    Operation(E),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupPlatform {
    Windows,
    Macos,
    Linux,
}

impl StartupPlatform {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// Clash Verge keeps installed-but-unavailable Unix services behind a user choice.
/// Windows permits automatic fallback after the separate native idle check.
pub(crate) fn permits_local_fallback(
    platform: StartupPlatform,
    health: ServiceHealthKind,
    explicit: bool,
) -> bool {
    match health {
        ServiceHealthKind::Ready => explicit,
        ServiceHealthKind::Missing => true,
        ServiceHealthKind::Stopped
        | ServiceHealthKind::RepairRequired
        | ServiceHealthKind::Incompatible
        | ServiceHealthKind::Unknown => platform == StartupPlatform::Windows || explicit,
        _ => false,
    }
}

pub(crate) fn route_startup_with_policy<T, E>(
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
    if explicit_local && permits_local_fallback(StartupPlatform::current(), health, true) {
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
    if permits_local_fallback(StartupPlatform::current(), health, explicit_local) {
        return local().map_err(StartupFailure::Operation);
    }
    Err(StartupFailure::Blocked(StartupBlocked::Health(health)))
}

pub(crate) fn effective_tun_enabled(value: &serde_json::Value) -> Option<bool> {
    let config = value.as_object()?;
    let Some(tun) = config.get("tun") else {
        return Some(false);
    };
    let tun = tun.as_object()?;
    tun.get("enable")
        .map_or(Some(false), serde_json::Value::as_bool)
}

pub(crate) fn blocked_message(reason: StartupBlocked) -> String {
    let key = match reason {
        StartupBlocked::ConfigurationUnknown => "startup.service_configuration_unconfirmed",
        StartupBlocked::Health(health) => match health {
            ServiceHealthKind::Stopped => "startup.service_stopped",
            ServiceHealthKind::RepairRequired => "startup.service_repair_required",
            ServiceHealthKind::MaintenancePending => "startup.service_pending",
            ServiceHealthKind::Unauthorized => "startup.service_unauthorized",
            ServiceHealthKind::Incompatible => "startup.service_incompatible",
            ServiceHealthKind::UnrecognizedInstallation => "startup.service_unrecognized",
            _ => "startup.service_unknown",
        },
    };
    zenclash_i18n::text(key)
}

pub(crate) fn connection_message(error: &MihomoError) -> String {
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
    use super::*;
    use std::cell::Cell;

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
                        Some(ServiceHealthKind::Missing),
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
    fn startup_authorization_and_maintenance_blocks_never_run_local_callback() {
        for (health, tun) in [
            (ServiceHealthKind::MaintenancePending, Some(false)),
            (ServiceHealthKind::Unauthorized, Some(false)),
            (ServiceHealthKind::UnrecognizedInstallation, Some(false)),
            (ServiceHealthKind::Missing, None),
        ] {
            let local_starts = Cell::new(0);
            let result = route_startup_with_policy(
                CoreKind::Mihomo,
                false,
                Some(health),
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
            Some(ServiceHealthKind::Missing),
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
    fn startup_explicit_external_and_experimental_core_do_not_acquire_mihomo_service() {
        for (kind, external) in [(CoreKind::Mihomo, true), (CoreKind::Meow, false)] {
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
                ServiceHealthKind::Missing,
                false
            ));
            for health in [
                ServiceHealthKind::Stopped,
                ServiceHealthKind::RepairRequired,
                ServiceHealthKind::Incompatible,
                ServiceHealthKind::Unknown,
            ] {
                assert_eq!(
                    permits_local_fallback(platform, health, false),
                    platform == StartupPlatform::Windows
                );
                assert!(permits_local_fallback(platform, health, true));
            }
            for health in [
                ServiceHealthKind::Unauthorized,
                ServiceHealthKind::MaintenancePending,
                ServiceHealthKind::UnrecognizedInstallation,
            ] {
                assert!(!permits_local_fallback(platform, health, false));
                assert!(!permits_local_fallback(platform, health, true));
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
            blocked_message(StartupBlocked::Health(ServiceHealthKind::Incompatible)),
            message
        );
    }
}
