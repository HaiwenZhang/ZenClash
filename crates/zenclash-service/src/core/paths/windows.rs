// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
#[cfg(feature = "standalone")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath};

static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(super) fn persistent_state_dir() -> io::Result<PathBuf> {
    if let Some(path) = STATE_DIR.get() {
        return Ok(path.clone());
    }
    let (path, recovery) = resolve_state_dir(program_data(), registered_state_dir)?;
    // Keep one root for the process, including after the uninstaller deletes the SCM record.
    Ok(STATE_DIR
        .get_or_init(|| {
            if let Some(error) = recovery {
                eprintln!("Could not resolve ProgramData ({error}); using protected service registration: {path:?}");
                #[cfg(feature = "standalone")]
                tracing::warn!(
                    "Could not resolve ProgramData ({error}); using protected service registration: {path:?}"
                );
            }
            path
        })
        .clone())
}

fn resolve_state_dir(
    system_directory: io::Result<PathBuf>,
    registered_directory: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<(PathBuf, Option<io::Error>)> {
    match system_directory {
        Ok(directory) => Ok((directory.join(crate::SERVICE_SLUG), None)),
        Err(system_error) => match registered_directory() {
            Ok(directory) => Ok((directory, Some(system_error))),
            Err(registration_error) => Err(io::Error::other(format!(
                "failed to resolve ProgramData: {system_error}; no trusted installed service directory: {registration_error}"
            ))),
        },
    }
}

fn program_data() -> io::Result<PathBuf> {
    let mut raw = std::ptr::null_mut();
    let status = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, std::ptr::null_mut(), &mut raw) };
    let result = if status < 0 {
        Err(io::Error::other(format!(
            "SHGetKnownFolderPath(FOLDERID_ProgramData) failed with HRESULT 0x{:08X}",
            status as u32
        )))
    } else if raw.is_null() {
        Err(io::Error::other(
            "SHGetKnownFolderPath(FOLDERID_ProgramData) returned no path",
        ))
    } else {
        let mut length = 0;
        unsafe {
            while *raw.add(length) != 0 {
                length += 1;
            }
        }
        let path = PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, length) }));
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(io::Error::other(
                "SHGetKnownFolderPath(FOLDERID_ProgramData) returned a non-absolute path",
            ))
        }
    };
    if !raw.is_null() {
        unsafe { CoTaskMemFree(raw.cast()) };
    }
    result
}

#[cfg(feature = "standalone")]
fn registered_state_dir() -> io::Result<PathBuf> {
    use crate::core::trusted_core_location::{require_trusted_core_location, require_trusted_service_registration};
    use platform_lib::service::{ServiceAccess, ServiceType};
    use platform_lib::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(service_query_error)?;
    let service = manager
        .open_service(
            crate::WINDOWS_SERVICE_NAME,
            ServiceAccess::QUERY_CONFIG | ServiceAccess::READ_CONTROL,
        )
        .map_err(service_query_error)?;
    require_trusted_service_registration(&service).map_err(io::Error::other)?;
    let config = service.query_config().map_err(service_query_error)?;
    if config.service_type != ServiceType::OWN_PROCESS
        || !config.account_name.as_deref().is_some_and(|account| {
            matches!(
                account.to_string_lossy().to_ascii_lowercase().as_str(),
                "localsystem" | "nt authority\\system"
            )
        })
    {
        return Err(io::Error::other(
            "registered service is not an own-process LocalSystem service",
        ));
    }
    let executable = registered_executable(&config.executable_path)?;
    installed_state_root(&executable)?;
    let canonical = std::fs::canonicalize(&executable)?;
    let root = installed_state_root(&canonical)?;
    if !std::fs::metadata(&canonical)?.is_file() {
        return Err(io::Error::other(
            "registered service executable is not an ordinary file",
        ));
    }
    require_trusted_core_location(&canonical).map_err(io::Error::other)?;
    Ok(root)
}

#[cfg(feature = "standalone")]
fn service_query_error(error: platform_lib::Error) -> io::Error {
    match error {
        platform_lib::Error::Winapi(error) => error,
        error => io::Error::other(error),
    }
}

#[cfg(not(feature = "standalone"))]
fn registered_state_dir() -> io::Result<PathBuf> {
    Err(io::Error::other(
        "installed directory recovery requires the service or installer",
    ))
}

#[cfg(feature = "standalone")]
fn registered_executable(command: &Path) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    let mut wide: Vec<u16> = command.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(io::Error::other("registered service command is empty or contains NUL"));
    }
    wide.push(0);
    let mut count = 0;
    let arguments = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) };
    if arguments.is_null() {
        return Err(io::Error::last_os_error());
    }
    let result = if count != 1 {
        Err(io::Error::other(
            "registered service command must name only the installed executable",
        ))
    } else {
        let raw = unsafe { *arguments };
        let mut length = 0;
        unsafe {
            while *raw.add(length) != 0 {
                length += 1;
            }
        }
        Ok(PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(raw, length)
        })))
    };
    unsafe { LocalFree(arguments.cast()) };
    result
}

#[cfg(feature = "standalone")]
fn installed_state_root(executable: &Path) -> io::Result<PathBuf> {
    let matches_name = |path: &Path, name: &str| {
        path.file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
    };
    if executable.is_absolute()
        && !executable
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        && matches_name(executable, "zenclash-service.exe")
        && let Some(bin) = executable.parent()
        && matches_name(bin, "bin")
        && let Some(root) = bin.parent()
        && matches_name(root, crate::SERVICE_SLUG)
    {
        return Ok(root.to_path_buf());
    }
    Err(io::Error::other(format!(
        "registered executable {executable:?} does not use the installed {}/bin layout",
        crate::SERVICE_SLUG
    )))
}
