// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
use anyhow::{Context as _, Result};
use std::fs::{File, OpenOptions};

pub const REPAIR_IN_PROGRESS_EXIT_CODE: i32 = 75;

pub struct ServiceRepairGate {
    _file: File,
}

///
/// # Errors
/// Returns errors preparing or locking the protected repair file; an occupied lock returns `None`.
pub fn acquire_service_repair_gate() -> Result<Option<ServiceRepairGate>> {
    let directory = prepare_repair_directory()?;
    let path = directory.join(".repair.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open service repair gate {path:?}"))?;

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::PermissionsExt as _;

        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure service repair gate {path:?}"))?;
        let status = unsafe { platform_lib::flock(file.as_raw_fd(), platform_lib::LOCK_EX | platform_lib::LOCK_NB) };
        if status == 0 {
            return Ok(Some(ServiceRepairGate { _file: file }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        Err(error).with_context(|| format!("failed to lock service repair gate {path:?}"))
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, GetLastError};
        use windows_sys::Win32::Storage::FileSystem::LockFile;

        if unsafe { LockFile(file.as_raw_handle(), 0, 0, u32::MAX, u32::MAX) } != 0 {
            return Ok(Some(ServiceRepairGate { _file: file }));
        }
        if unsafe { GetLastError() } == ERROR_LOCK_VIOLATION {
            return Ok(None);
        }
        Err(std::io::Error::last_os_error()).with_context(|| format!("failed to lock service repair gate {path:?}"))
    }
}

fn prepare_repair_directory() -> Result<std::path::PathBuf> {
    let result = crate::prepare_service_install_directory();
    #[cfg(all(windows, feature = "client", not(feature = "test")))]
    if let Err(error) = result {
        let paths = crate::service_paths()?;
        let recovered = crate::core::windows_security::legacy_repair::recover(
            paths.persistent_state_dir(),
            crate::execution::reserve_legacy_install_repair,
        );
        return match recovered {
            Ok(Some(backup)) => {
                eprintln!("Preserved legacy service state at {:?}", backup.path);
                // Keep core execution reserved until the fresh, private layout is ready.
                crate::prepare_service_install_directory()
                    .with_context(|| format!("legacy service state was preserved at {:?}", backup.path))
            }
            Ok(None) => Err(error),
            Err(repair_error) => Err(error.context(format!("legacy directory recovery failed: {repair_error:#}"))),
        };
    }
    result
}
