// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use anyhow::{Context as _, Result, bail};
use platform_lib::{
    Error as WindowsServiceError,
    service::ServiceAccess,
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, ERROR_SERVICE_DOES_NOT_EXIST, INVALID_HANDLE_VALUE},
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    },
};

pub(super) fn require_no_registered_services() -> Result<()> {
    require_services_absent(&["zenclash_service", "zenclash_service_dev"])
}

fn require_services_absent(names: &[&str]) -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    for name in names {
        match manager.open_service(name, ServiceAccess::QUERY_STATUS) {
            Err(WindowsServiceError::Winapi(error))
                if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {}
            Ok(_) => bail!(
                "legacy core execution lock is unavailable while service {name} is registered; start or upgrade the service before Sidecar fallback"
            ),
            Err(error) => {
                return Err(error).context("cannot rule out a registered service using the legacy core execution lock");
            }
        }
    }
    Ok(())
}

#[cfg(feature = "client")]
pub(super) fn require_stopped_service() -> Result<()> {
    use platform_lib::service::ServiceState;
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(crate::WINDOWS_SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => {
            let status = service.query_status()?;
            if status.current_state != ServiceState::Stopped || status.process_id.is_some_and(|pid| pid != 0) {
                bail!(
                    "service is {:?}; Sidecar fallback requires a stopped service",
                    status.current_state
                );
            }
            Ok(())
        }
        Err(WindowsServiceError::Winapi(error))
            if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
        {
            Ok(())
        }
        Err(error) => Err(error).context("cannot confirm that the service has stopped"),
    }
}

pub(super) fn require_no_core_process(include_service: bool) -> Result<()> {
    #[cfg(not(feature = "test"))]
    const CORE_NAMES: &[&str] = &["mihomo.exe", "mihomo-alpha.exe"];
    #[cfg(feature = "test")]
    const CORE_NAMES: &[&str] = &["mock_binary.exe", "crash_binary.exe"];
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("cannot inspect existing core processes");
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        let length = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        // SCM can be stopped while a core from an earlier service process remains alive.
        if CORE_NAMES.iter().any(|core| name.eq_ignore_ascii_case(core))
            || (include_service && name.eq_ignore_ascii_case("zenclash-service.exe"))
        {
            bail!(
                "process {name} (PID {}) is still running; refusing a second core",
                entry.th32ProcessID
            );
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error).context("core process enumeration did not complete");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn registered_service_is_refused_without_changing_its_state() {
        let error = super::require_services_absent(&["RpcSs"]).unwrap_err();
        assert!(error.to_string().contains("service RpcSs is registered"));
    }

    #[test]
    fn service_absence_can_be_confirmed_without_installing_a_service() {
        let name = format!(
            "zenclash-absent-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        super::require_services_absent(&[&name]).unwrap();
    }
}
