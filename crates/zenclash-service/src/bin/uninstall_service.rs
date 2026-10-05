// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn main() {
    panic!("This program is not intended to run on this platform.");
}

mod shared;

use anyhow::Error;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use shared::run_command;
#[cfg(all(target_os = "macos", not(feature = "development-channel")))]
use shared::uninstall_old_service;
use shared::{enter_repair_gate, run_maintenance_if_requested};

/// Removes approved cores after service deletion. Locked files are left for a later retry.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn remove_installed_cores() {
    let paths = match zenclash_service::service_paths() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("Could not locate core directory for cleanup: {error}");
            return;
        }
    };
    let cores = paths.core_dir();
    #[cfg(windows)]
    remove_core_firewall_rules(&cores);
    match std::fs::remove_dir_all(&cores) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "Could not remove core directory {cores:?}: {error}. \
             A process may still hold a core open; this does not affect the uninstall."
        ),
    }
}

/// Removes recorded firewall rules and discovers cores staged by older installers.
/// Keep failed records outside `cores` for retries: netsh cannot distinguish an absent rule
/// from an unavailable firewall by exit status.
#[cfg(windows)]
fn remove_core_firewall_rules(cores: &std::path::Path) {
    let records = shared::core_firewall_records(cores);
    let mut names = std::collections::BTreeSet::new();
    for directory in [cores, records.as_path()] {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("Could not read firewall cleanup names from {directory:?}: {error}");
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let bookkeeping = path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case(zenclash_service::CORE_STAGING_EXTENSION)
                    || extension.eq_ignore_ascii_case(zenclash_service::CORE_DISPLACED_EXTENSION)
            });
            if !bookkeeping && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                names.insert(entry.file_name());
            }
        }
    }
    for file_name in names {
        let Ok(name) = shared::core_firewall_rule_name(std::path::Path::new(&file_name)) else {
            continue;
        };
        // Preserve newly discovered names from older installers before removing their cores.
        if let Err(error) = shared::record_core_firewall_rule(&cores.join(&file_name)) {
            eprintln!("Could not retain firewall cleanup record for {name:?}: {error:#}");
        }
        match shared::netsh_firewall(&["delete", "rule", &format!("name={name}")]) {
            Ok(()) => {
                let record = records.join(&file_name);
                if let Err(error) = std::fs::remove_file(&record)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("Could not remove firewall cleanup record {record:?}: {error}");
                }
            }
            Err(error) => eprintln!("Could not remove firewall rule {name:?}: {error:#}"),
        }
    }
    // Only remove an empty inventory; failed deletions must remain retryable.
    let _ = std::fs::remove_dir(&records);
}

#[cfg(any(windows, test))]
fn poll_until<T>(
    max_attempts: usize,
    mut probe: impl FnMut() -> Result<Option<T>, Error>,
    mut pause: impl FnMut(),
    timeout_message: &str,
) -> Result<T, Error> {
    for attempt in 0..max_attempts {
        if let Some(value) = probe()? {
            return Ok(value);
        }
        if attempt + 1 < max_attempts {
            pause();
        }
    }
    Err(anyhow::anyhow!("{timeout_message}"))
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Error> {
    use std::env;
    use std::path::Path;

    if run_maintenance_if_requested()? {
        return Ok(());
    }
    let _gate = enter_repair_gate()?;
    let debug = env::args().any(|arg| arg == "--debug");

    #[cfg(not(feature = "development-channel"))]
    let _ = uninstall_old_service();
    let bundle_path = format!(
        "/Library/PrivilegedHelperTools/{}.bundle",
        zenclash_service::MACOS_SERVICE_ID
    );
    let plist_file = format!("/Library/LaunchDaemons/{}.plist", zenclash_service::MACOS_SERVICE_ID);
    let service_id = zenclash_service::MACOS_SERVICE_ID;

    let _ = run_command("launchctl", &["stop", service_id], debug);
    let _ = run_command("launchctl", &["disable", &format!("system/{}", service_id)], debug);
    let _ = run_command("launchctl", &["bootout", &format!("system/{}", service_id)], debug);

    if Path::new(&plist_file).exists() {
        std::fs::remove_file(&plist_file).map_err(|e| anyhow::anyhow!("Failed to remove plist file: {}", e))?;
    }

    if Path::new(&bundle_path).exists() {
        std::fs::remove_dir_all(&bundle_path)
            .map_err(|e| anyhow::anyhow!("Failed to remove bundle directory: {}", e))?;
    }

    if let Err(error) = shared::repair_active_owner_state() {
        eprintln!("Warning: failed to repair active owner state during uninstall: {error:#}");
    }
    remove_installed_cores();

    Ok(())
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Error> {
    use std::env;

    if run_maintenance_if_requested()? {
        return Ok(());
    }
    let _gate = enter_repair_gate()?;
    let debug = env::args().any(|arg| arg == "--debug");
    let service_name = zenclash_service::SERVICE_SLUG;

    let _ = run_command("systemctl", &["stop", &format!("{}.service", service_name)], debug);
    let _ = run_command("systemctl", &["disable", &format!("{}.service", service_name)], debug);

    let unit_file = format!("/etc/systemd/system/{}.service", service_name);
    if std::path::Path::new(&unit_file).exists() {
        std::fs::remove_file(&unit_file).map_err(|e| anyhow::anyhow!("Failed to remove service file: {}", e))?;
    }

    let _ = run_command("systemctl", &["daemon-reload"], debug);
    let target = zenclash_service::prepare_service_install_directory()?.join("zenclash-service");
    if target.exists() {
        std::fs::remove_file(&target)
            .map_err(|error| anyhow::anyhow!("Failed to remove service binary {target:?}: {error}"))?;
    }
    // A fallback publish may have displaced a locked service image aside; best-effort.
    let _ = std::fs::remove_file(target.with_extension(zenclash_service::CORE_DISPLACED_EXTENSION));

    if let Err(error) = shared::repair_active_owner_state() {
        eprintln!("Warning: failed to repair active owner state during uninstall: {error:#}");
    }
    remove_installed_cores();

    Ok(())
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use platform_lib::{
        Error as WindowsServiceError,
        service::{ServiceAccess, ServiceState},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    use std::{thread, time::Duration};

    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
    const POLL_ATTEMPTS: usize = 200;
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    fn has_raw_error(error: &WindowsServiceError, code: i32) -> bool {
        matches!(error, WindowsServiceError::Winapi(error) if error.raw_os_error() == Some(code))
    }

    if run_maintenance_if_requested()? {
        return Ok(());
    }
    // Resolve before deleting the SCM registration that may supply the recovery path.
    zenclash_service::service_paths()?;
    let _gate = enter_repair_gate()?;
    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    match service_manager.open_service(zenclash_service::WINDOWS_SERVICE_NAME, service_access) {
        Ok(service) => {
            let service_status = service.query_status()?;
            if service_status.current_state != ServiceState::Stopped {
                if let Err(error) = service.stop()
                    && !has_raw_error(&error, ERROR_SERVICE_NOT_ACTIVE)
                {
                    return Err(error.into());
                }
                poll_until(
                    POLL_ATTEMPTS,
                    || {
                        let status = service.query_status()?;
                        Ok((status.current_state == ServiceState::Stopped).then_some(()))
                    },
                    || thread::sleep(POLL_INTERVAL),
                    "timed out waiting for service to stop",
                )?;
            }

            service.delete()?;
            drop(service);
            poll_until(
                POLL_ATTEMPTS,
                || match service_manager
                    .open_service(zenclash_service::WINDOWS_SERVICE_NAME, ServiceAccess::QUERY_STATUS)
                {
                    Ok(service) => {
                        drop(service);
                        Ok(None)
                    }
                    Err(error) if has_raw_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => Ok(Some(())),
                    Err(error) => Err(error.into()),
                },
                || thread::sleep(POLL_INTERVAL),
                "timed out waiting for service deletion",
            )?;
        }
        // A retry must finish file cleanup even if the service was already deleted.
        Err(error) if has_raw_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let target = zenclash_service::prepare_service_install_directory()?.join("zenclash-service.exe");
    if target.exists() {
        std::fs::remove_file(&target)
            .map_err(|error| anyhow::anyhow!("Failed to remove service binary {target:?}: {error}"))?;
    }
    // A fallback publish may have displaced a locked service image aside; best-effort.
    let _ = std::fs::remove_file(target.with_extension(zenclash_service::CORE_DISPLACED_EXTENSION));
    shared::repair_active_owner_state()?;
    remove_installed_cores();
    println!("Service uninstalled successfully. Resource cleanup warnings can be ignored.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::poll_until;
    use std::cell::Cell;

    #[test]
    fn poll_until_retries_transient_state_before_success() -> anyhow::Result<()> {
        let attempts = Cell::new(0);
        let pauses = Cell::new(0);

        let result = poll_until(
            3,
            || {
                let next = attempts.get() + 1;
                attempts.set(next);
                Ok((next == 3).then_some("deleted"))
            },
            || pauses.set(pauses.get() + 1),
            "service deletion timed out",
        )?;

        assert_eq!(result, "deleted");
        assert_eq!(attempts.get(), 3);
        assert_eq!(pauses.get(), 2);
        Ok(())
    }
}
