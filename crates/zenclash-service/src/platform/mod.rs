//! Native transport and privilege boundaries selected at compile time.

#[cfg(all(feature = "server", any(target_os = "macos", test)))]
#[path = "unix/launchd_probe.rs"]
mod launchd_probe;

#[cfg(all(feature = "server", any(target_os = "macos", test)))]
#[path = "unix/macos_registration.rs"]
mod macos_registration;

#[cfg(all(feature = "server", any(target_os = "linux", test)))]
#[path = "unix/selinux.rs"]
mod selinux;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

pub(crate) fn root_directory() -> std::io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    {
        service_root()
    }
    #[cfg(unix)]
    {
        Ok(service_root())
    }
}

#[cfg(feature = "server")]
pub(crate) fn private_directory(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        create_private_directory(path, false)
    }
    #[cfg(unix)]
    {
        create_protected_directory(path, 0o700)
    }
}

#[cfg(all(windows, feature = "server"))]
pub(crate) fn service_was_running() -> std::io::Result<bool> {
    Ok(service_state()? == Some(windows_sys::Win32::System::Services::SERVICE_RUNNING))
}
