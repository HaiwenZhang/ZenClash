#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Windows SCM service installation, firewall rules, failure recovery, and uninstallation.

use anyhow::Error;
#[cfg(windows)]
use anyhow::{Context as _, bail};
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use super::{
    InstallOptions, bundled_service_binary, enter_repair_gate, install_service_cores, installed_payload_matches,
    prepare_requested_cores, publish_staged_binary, remove_installed_cores, remove_ordinary_file_if_exists,
    repair_active_owner_state, stage_binary, wait_for_service_ready,
};

pub(super) fn poll_until<T>(
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

/// Names the Windows Firewall rule that admits the approved copy of `core`.
#[cfg(windows)]
pub(super) fn core_firewall_rule_name(core: &Path) -> Result<String, Error> {
    let name = core.file_name().context("core path has no file name")?;
    Ok(format!(
        "{} core ({})",
        crate::SERVICE_DISPLAY_NAME,
        name.to_string_lossy()
    ))
}

/// Survives removal of `cores`, so a later uninstall can retry failed rule deletions.
#[cfg(windows)]
pub(super) fn core_firewall_records(cores: &Path) -> PathBuf {
    cores.with_file_name("core-firewall-rules")
}

#[cfg(windows)]
pub(super) fn record_core_firewall_rule(core: &Path) -> Result<(), Error> {
    // Record before changing the firewall. Independent empty files cannot truncate an
    // existing inventory if the installer crashes. The parent is installer-protected.
    let records = core_firewall_records(core.parent().context("core path has no parent")?);
    std::fs::create_dir_all(&records).context("failed to create firewall cleanup records")?;
    let record = records.join(core.file_name().context("core path has no file name")?);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&record) {
        Ok(file) => file.sync_all().context("failed to persist firewall cleanup record"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).context("failed to record firewall cleanup obligation"),
    }
}

#[cfg(windows)]
fn netsh_path() -> Result<PathBuf, Error> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    // The caller controls inherited environment variables, including SystemRoot and PATH.
    // Ask Windows instead.
    let mut buffer = vec![0_u16; 260];
    loop {
        // SAFETY: `buffer` is writable for its full length and the API writes at
        // most that many UTF-16 units.
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if length < buffer.len() {
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])).join("netsh.exe"));
        }
        buffer.resize(length, 0);
    }
}

/// Runs `netsh advfirewall firewall` with `arguments`, failing on a non-zero exit.
#[cfg(windows)]
pub(super) fn netsh_firewall(arguments: &[&str]) -> Result<(), Error> {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let netsh = netsh_path().context("failed to locate the Windows system netsh")?;
    let output = std::process::Command::new(&netsh)
        .args(["advfirewall", "firewall"])
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("failed to run {}", netsh.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "netsh advfirewall firewall {} failed (status: {}): {} {}",
        arguments.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Adds a best-effort inbound rule for the approved executable path.
#[cfg(windows)]
pub(super) fn allow_core_through_firewall(core: &Path) {
    let result = core_firewall_rule_name(core).and_then(|name| {
        record_core_firewall_rule(core)?;
        let _ = netsh_firewall(&["delete", "rule", &format!("name={name}")]);
        netsh_firewall(&[
            "add",
            "rule",
            &format!("name={name}"),
            "dir=in",
            "action=allow",
            "enable=yes",
            "profile=any",
            &format!("program={}", core.display()),
        ])
    });
    if let Err(error) = result {
        eprintln!(
            "Could not allow {} through Windows Firewall: {error:#}. TUN system/mixed stacks and LAN access \
             need an inbound rule for that path.",
            core.display()
        );
    }
}

/// Removes recorded firewall rules and discovers cores staged by older installers.
#[cfg(windows)]
pub(super) fn remove_core_firewall_rules(cores: &Path) {
    let records = core_firewall_records(cores);
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
                extension.eq_ignore_ascii_case(crate::CORE_STAGING_EXTENSION)
                    || extension.eq_ignore_ascii_case(crate::CORE_DISPLACED_EXTENSION)
            });
            if !bookkeeping && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                names.insert(entry.file_name());
            }
        }
    }
    for file_name in names {
        let Ok(name) = core_firewall_rule_name(Path::new(&file_name)) else {
            continue;
        };
        if let Err(error) = record_core_firewall_rule(&cores.join(&file_name)) {
            eprintln!("Could not retain firewall cleanup record for {name:?}: {error:#}");
        }
        match netsh_firewall(&["delete", "rule", &format!("name={name}")]) {
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
    let _ = std::fs::remove_dir(&records);
}

#[cfg(windows)]
fn configure_windows_service_recovery(service: &platform_lib::service::Service) -> platform_lib::Result<()> {
    use platform_lib::service::{ServiceAction, ServiceActionType, ServiceFailureActions, ServiceFailureResetPeriod};
    use std::time::Duration;

    let actions = [5, 10, 30]
        .into_iter()
        .map(|delay_secs| ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(delay_secs),
        })
        .collect();

    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(actions),
    })?;
    service.set_failure_actions_on_non_crash_failures(true)?;

    Ok(())
}

#[cfg(windows)]
pub(super) fn install(mut options: InstallOptions) -> Result<(), Error> {
    use platform_lib::{
        Error as WindowsServiceError,
        service::{ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState, ServiceType},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    use std::ffi::{OsStr, OsString};
    use std::{thread, time::Duration};

    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    if options.debug {
        println!(
            "Installing service with {} explicitly requested cores",
            options.cores.len()
        );
    }
    let source = bundled_service_binary()?;
    let install_dir = crate::prepare_service_install_directory()?;
    let target = install_dir.join("zenclash-service.exe");
    let staged = stage_binary(&source, &target)?;

    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;
    let service_info = ServiceInfo {
        name: OsString::from(crate::WINDOWS_SERVICE_NAME),
        display_name: OsString::from(crate::SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::OnDemand,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    let service_access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::QUERY_CONFIG
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG;
    match service_manager.open_service(crate::WINDOWS_SERVICE_NAME, service_access) {
        Ok(service) => {
            const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
            let status = service.query_status()?;
            if options.ensure_service
                && status.current_state == ServiceState::Stopped
                && service
                    .query_config()?
                    .executable_path
                    .to_string_lossy()
                    .trim_matches('"')
                    == target.to_string_lossy()
                && service.query_config()?.start_type == ServiceStartType::OnDemand
                && crate::windows_service_can_activate(&service)?
                && installed_payload_matches(&source, &target, &options.cores, &crate::service_paths()?.core_dir())?
            {
                service.start(&Vec::<&OsStr>::new())?;
                wait_for_service_ready()?;
                remove_ordinary_file_if_exists(&staged)?;
                return Ok(());
            }
            if status.current_state != ServiceState::Stopped {
                if let Err(error) = service.stop()
                    && !matches!(
                        &error,
                        WindowsServiceError::Winapi(error)
                            if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE)
                    )
                {
                    return Err(error.into());
                }
                for _ in 0..200 {
                    if service.query_status()?.current_state == ServiceState::Stopped {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if service.query_status()?.current_state != ServiceState::Stopped {
                    bail!("timed out waiting for service to stop before replacement");
                }
            }

            repair_active_owner_state()?;
            install_service_cores(&options.cores)?;
            publish_staged_binary(&staged, &target)?;
            service.change_config(&service_info)?;
            crate::configure_windows_service_activation(&service)?;
            configure_windows_service_recovery(&service)?;
            service.start(&Vec::<&OsStr>::new())?;
            wait_for_service_ready()?;
            return Ok(());
        }
        Err(WindowsServiceError::Winapi(error)) if error.raw_os_error() == Some(1060) => {}
        Err(error) => return Err(error.into()),
    }

    repair_active_owner_state()?;
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target)?;
    let start_access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START;
    let service = service_manager.create_service(&service_info, start_access)?;

    service.set_description("ZenClash Service manages the Mihomo core")?;
    crate::configure_windows_service_activation(&service)?;
    configure_windows_service_recovery(&service)?;
    service.start(&Vec::<&OsStr>::new())?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(windows)]
pub(super) fn uninstall(_debug: bool) -> Result<(), Error> {
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

    // Resolve before deleting the SCM registration that may supply the recovery path.
    crate::service_paths()?;
    let _gate = enter_repair_gate()?;
    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    match service_manager.open_service(crate::WINDOWS_SERVICE_NAME, service_access) {
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
                || match service_manager.open_service(crate::WINDOWS_SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
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
        Err(error) if has_raw_error(&error, ERROR_SERVICE_DOES_NOT_EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let target = crate::prepare_service_install_directory()?.join("zenclash-service.exe");
    if target.exists() {
        std::fs::remove_file(&target)
            .map_err(|error| anyhow::anyhow!("Failed to remove service binary {target:?}: {error}"))?;
    }
    let _ = std::fs::remove_file(target.with_extension(crate::CORE_DISPLACED_EXTENSION));
    repair_active_owner_state()?;
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

    #[cfg(windows)]
    #[test]
    fn netsh_ignores_caller_system_root() -> anyhow::Result<()> {
        const EXPECTED: &str = "CVR_TEST_EXPECTED_NETSH";
        if let Some(expected) = std::env::var_os(EXPECTED) {
            assert_eq!(super::netsh_path()?, std::path::PathBuf::from(expected));
            return Ok(());
        }
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "install::windows::tests::netsh_ignores_caller_system_root"])
            .env(EXPECTED, super::netsh_path()?)
            .env("SystemRoot", std::env::temp_dir().join("caller controlled (windows)"))
            .status()?;
        assert!(status.success());
        Ok(())
    }
}
