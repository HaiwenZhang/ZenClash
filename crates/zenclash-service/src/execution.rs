#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Cooperative exclusion between Service supervision and local Sidecar execution.
#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context as _, Result};
#[cfg(unix)]
use std::fs::OpenOptions;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct CoreExecutionGuard {
    #[cfg(unix)]
    _file: File,
    #[cfg(windows)]
    _reservation: windows_lock::Reservation,
}

#[cfg(unix)]
impl Drop for CoreExecutionGuard {
    fn drop(&mut self) {
        // fork can briefly inherit this open file description before exec closes it.
        let _ = self._file.unlock();
    }
}

impl CoreExecutionGuard {
    ///
    /// # Errors
    /// Returns native lock, service-status or process-inspection errors, or an error when another core owns execution.
    pub fn acquire() -> Result<Self> {
        #[cfg(unix)]
        return Self::acquire_at(&coordination_path()?);
        #[cfg(windows)]
        {
            Self::acquire_windows_at(
                windows_lock::NAME,
                &coordination_path()?,
                windows_fallback::require_no_registered_services,
                || windows_fallback::require_no_core_process(false),
            )
        }
    }

    #[cfg(windows)]
    fn acquire_windows_at(
        name: &str,
        path: &Path,
        require_services_absent: impl FnOnce() -> Result<()>,
        require_cores_absent: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        let reservation = windows_lock::Reservation::try_acquire(name, path)?
            .context("another Service or Sidecar owns core execution")?;
        if !reservation.has_legacy_lock() {
            require_services_absent()?;
        }
        require_cores_absent()?;
        Ok(Self {
            _reservation: reservation,
        })
    }

    #[cfg(any(unix, test))]
    fn acquire_at(path: &Path) -> Result<Self> {
        #[cfg(windows)]
        {
            Self::acquire_windows_at(&test_coordination_name(path), path, || Ok(()), || Ok(()))
        }
        #[cfg(unix)]
        {
            let file = open_coordination_file(path)?;
            match file.try_lock() {
                Ok(()) => Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => bail!("another Service or Sidecar owns core execution"),
                Err(std::fs::TryLockError::Error(error)) => Err(error).context("could not reserve core execution"),
            }
        }
    }

    /// Keep the reservation until the OS confirms exit, even if killing or event delivery fails.
    pub fn release_after_exit(
        self,
        pid: u32,
        mut terminated: tokio::sync::oneshot::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut events_open = true;
            while !process_has_exited(pid) {
                tokio::select! {
                    result = &mut terminated, if events_open => {
                        if result.is_ok() { break; }
                        events_open = false;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                }
            }
            drop(self);
        })
    }

    ///
    /// # Errors
    /// Returns native lock, service-status or process-inspection errors, or an error when another core owns execution.
    pub fn is_held() -> Result<bool> {
        #[cfg(windows)]
        {
            Ok(windows_lock::Reservation::try_acquire(windows_lock::NAME, &coordination_path()?)?.is_none())
        }
        #[cfg(unix)]
        {
            let file = open_coordination_file(&coordination_path()?)?;
            match file.try_lock() {
                Ok(()) => {
                    file.unlock()?;
                    Ok(false)
                }
                Err(std::fs::TryLockError::WouldBlock) => Ok(true),
                Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
            }
        }
    }
}

#[cfg(all(windows, test))]
fn test_coordination_name(path: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let digest: String = Sha256::digest(path.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(r"Global\zenclash-execution-test-{digest}")
}

#[cfg(unix)]
fn process_has_exited(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` only probes process existence and has no memory
    // effects; the PID is validated by the caller.
    (unsafe { platform_lib::kill(pid as i32, 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(platform_lib::ESRCH)
}

#[cfg(windows)]
fn process_has_exited(pid: u32) -> bool {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject},
    };
    // SAFETY: The process API has no memory preconditions; the returned handle
    // is checked for null before adoption.
    let raw = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32);
    }
    // SAFETY: `raw` is a valid non-null handle; OwnedHandle now owns it.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: The handle is live for the duration of the call.
    (unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) }) == WAIT_OBJECT_0
}

fn coordination_path() -> Result<PathBuf> {
    #[cfg(unix)]
    let root = PathBuf::from("/tmp");
    #[cfg(windows)]
    let root = {
        use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
        let mut buffer = vec![0u16; 32768];
        // SAFETY: `buffer` is writable for its full length and the API writes
        // at most that many UTF-16 units.
        let size = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        anyhow::ensure!(
            size > 0 && size < buffer.len(),
            "could not locate Windows coordination directory"
        );
        PathBuf::from(String::from_utf16(&buffer[..size])?).join("Temp")
    };
    // Both channels may keep idle helpers, but core execution is shared.
    #[cfg(feature = "test")]
    let name = "zenclash-service.core-execution-test.lock";
    #[cfg(not(feature = "test"))]
    let name = "zenclash-service.core-execution.lock";
    Ok(root.join(name))
}

#[cfg(unix)]
fn open_coordination_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(path)
    {
        Ok(file) => {
            file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(platform_lib::O_NOFOLLOW | platform_lib::O_NONBLOCK)
                .open(path)?;
            anyhow::ensure!(file.metadata()?.is_file(), "coordination entry is not a regular file");
            Ok(file)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn open_coordination_file(path: &Path) -> Result<File> {
    use std::os::windows::{ffi::OsStrExt as _, io::FromRawHandle as _};
    use windows_sys::Win32::{
        Foundation::{INVALID_HANDLE_VALUE, LocalFree},
        Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        },
    };
    let sddl: Vec<u16> = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)(A;;GR;;;AU)\0"
        .encode_utf16()
        .collect();
    let mut descriptor = std::ptr::null_mut();
    anyhow::ensure!(
        // SAFETY: `sddl` is NUL-terminated and `descriptor` is a writable
        // out-parameter; the allocation is freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } != 0,
        "could not create coordination permissions"
    );
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and `attributes` points at the live
    // descriptor; the returned handle is checked below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0x80000000,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_ALWAYS,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: `descriptor` is the allocation returned by the conversion above
    // and is freed exactly once here.
    unsafe {
        LocalFree(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(error).with_context(|| format!("could not open or create legacy core execution lock {path:?}"));
    }
    // SAFETY: `handle` is valid and not INVALID_HANDLE_VALUE; File now owns it.
    let file = unsafe { File::from_raw_handle(handle) };
    anyhow::ensure!(
        file.metadata()?.is_file() && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "coordination entry is not a regular file"
    );
    Ok(file)
}

#[cfg(feature = "client")]
///
/// # Errors
/// Returns native lock, service-status or process-inspection errors, or an error when another core owns execution.
pub async fn check_sidecar_available() -> Result<()> {
    // GUI lifecycle requests can retry for minutes; occupancy checks need their own deadline.
    let inspection = tokio::time::timeout(std::time::Duration::from_secs(1), crate::inspect_installation(&[])).await;
    let ipc_failed = match inspection {
        Ok(Ok(status)) => {
            anyhow::ensure!(
                status
                    .protocol
                    .supports_client(crate::ProtocolVersion::current(), crate::MIN_REQUIRED_SERVICE_REVISION),
                "service upgrade is required before Sidecar handoff"
            );
            anyhow::ensure!(
                !status.core_busy,
                "the service has an active or recovering core session"
            );
            false
        }
        Ok(Err(_)) | Err(_) => true,
    };
    tokio::task::spawn_blocking(move || -> Result<()> {
        #[cfg(windows)]
        {
            if ipc_failed {
                windows_fallback::require_stopped_service()?;
            }
            windows_fallback::require_no_core_process(false)?;
        }
        #[cfg(unix)]
        {
            let mut command = std::process::Command::new("ps");
            #[cfg(target_os = "linux")]
            command.args(["-axo", "comm=,args="]);
            #[cfg(not(target_os = "linux"))]
            command.args(["-axo", "comm="]);
            let output = command.output().context("could not inspect remaining cores")?;
            anyhow::ensure!(output.status.success(), "could not inspect remaining cores");
            require_no_unix_core_processes(&String::from_utf8_lossy(&output.stdout), ipc_failed)?;
        }
        Ok(())
    })
    .await
    .context("core process inspection failed")?
}

#[cfg(all(unix, feature = "client"))]
fn require_no_unix_core_processes(processes: &str, include_service: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    let other_helper = PathBuf::from("/Library/PrivilegedHelperTools")
        .join(format!(
            "{}.bundle",
            if cfg!(feature = "development-channel") {
                "org.zenclash.app.service"
            } else {
                "org.zenclash.app.dev.service"
            }
        ))
        .join("Contents/MacOS/zenclash-service");
    #[cfg(target_os = "linux")]
    let other_helper = PathBuf::from("/var/lib")
        .join(if cfg!(feature = "development-channel") {
            "zenclash-service"
        } else {
            "zenclash-service-dev"
        })
        .join("bin/zenclash-service");
    // A residual helper outranks a core so the refusal stays repairable in any ps order.
    let mut remaining_core = None;
    for process in processes.lines() {
        #[cfg(target_os = "linux")]
        let (process, arguments) = process
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((process.trim(), ""));
        let executable = Path::new(process.trim());
        let name = executable.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if ["mihomo", "mihomo-alpha"].contains(&name) {
            remaining_core.get_or_insert(name);
        }
        if include_service && ["zenclash-service", "zenclash-servic"].contains(&name) {
            // Linux comm is truncated; argv[0] retains the installed helper's channel path.
            #[cfg(target_os = "linux")]
            let executable = Path::new(arguments.split_whitespace().next().unwrap_or(""));
            if executable != other_helper {
                return Err(ResidualServiceError {
                    process: executable.to_path_buf(),
                }
                .into());
            }
        }
    }
    if let Some(name) = remaining_core {
        bail!("process {name} remains after IPC failure; refusing a second core");
    }
    Ok(())
}

/// A Service helper still runs without answering IPC; only reinstalling the Service replaces it.
#[cfg(feature = "client")]
#[derive(Debug)]
pub struct ResidualServiceError {
    process: PathBuf,
}

#[cfg(feature = "client")]
impl std::fmt::Display for ResidualServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "service process {} is running without IPC; reinstall the service to replace it",
            self.process.display()
        )
    }
}

#[cfg(feature = "client")]
impl std::error::Error for ResidualServiceError {}

#[cfg(feature = "client")]
///
/// # Errors
/// Returns native lock, service-status or process-inspection errors, or an error when another core owns execution.
pub async fn reserve_sidecar() -> Result<CoreExecutionGuard> {
    check_sidecar_available().await?;
    CoreExecutionGuard::acquire()
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as windows_fallback;

#[cfg(windows)]
mod windows_lock;

#[cfg(test)]
mod tests;
