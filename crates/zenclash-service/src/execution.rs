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
    let raw = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32);
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    (unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) }) == WAIT_OBJECT_0
}

fn coordination_path() -> Result<PathBuf> {
    #[cfg(unix)]
    let root = PathBuf::from("/tmp");
    #[cfg(windows)]
    let root = {
        use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
        let mut buffer = vec![0u16; 32768];
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
    unsafe {
        LocalFree(descriptor);
    }
    if handle == INVALID_HANDLE_VALUE {
        return Err(error).with_context(|| format!("could not open or create legacy core execution lock {path:?}"));
    }
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
#[path = "execution_windows.rs"]
mod windows_fallback;

#[cfg(windows)]
#[path = "execution_windows_lock.rs"]
mod windows_lock;

#[cfg(all(windows, feature = "client", feature = "standalone", not(feature = "test")))]
pub(crate) fn reserve_legacy_install_repair() -> Result<CoreExecutionGuard> {
    anyhow::ensure!(
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() } != 0,
        "legacy service directory recovery requires an elevated installer"
    );
    let guard = CoreExecutionGuard::acquire()?;
    windows_fallback::require_stopped_service()
        .context("stop the service before repairing its legacy state directory")?;
    windows_fallback::require_no_core_process(true)
        .context("stop remaining service and core processes before repairing legacy state")?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    fn set_test_dacl(path: &Path, sddl: &str) {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
                DACL_SECURITY_INFORMATION, SetFileSecurityW,
            },
        };
        let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0);
        let secured = unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
        unsafe {
            LocalFree(descriptor);
        }
        assert_ne!(secured, 0);
    }

    #[cfg(windows)]
    #[test]
    fn missing_legacy_lock_does_not_require_parent_write_access() -> Result<()> {
        let root = std::env::temp_dir().join(format!("execution-readonly-parent-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let path = root.join("core.lock");
        set_test_dacl(&root, "D:P(D;;0x2;;;WD)(A;;FA;;;AU)(A;;FA;;;SY)");
        let result = (|| -> Result<()> {
            let name = test_coordination_name(&path);
            for service_absent in [false, true] {
                let refused = CoreExecutionGuard::acquire_windows_at(
                    &name,
                    &path,
                    || {
                        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
                        anyhow::ensure!(service_absent, "registered stopped service");
                        Ok(())
                    },
                    || {
                        assert!(service_absent);
                        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
                        anyhow::bail!("surviving core after owner exit")
                    },
                );
                let message = refused.unwrap_err().to_string();
                assert!(message.contains(if service_absent {
                    "surviving core"
                } else {
                    "registered stopped service"
                }));
            }
            let first = CoreExecutionGuard::acquire_at(&path)?;
            assert!(CoreExecutionGuard::acquire_at(&path).is_err());
            drop(first);
            drop(CoreExecutionGuard::acquire_at(&path)?);
            Ok(())
        })();
        assert!(!path.exists());
        std::fs::remove_dir(&root)?;
        result
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_legacy_file_fails_closed_without_leaking_the_mutex() -> Result<()> {
        let path = std::env::temp_dir().join(format!("execution-unreadable-{}", std::process::id()));
        std::fs::write(&path, b"")?;
        set_test_dacl(&path, "D:P(D;;GR;;;WD)(A;;FA;;;AU)(A;;FA;;;SY)");
        let refused = CoreExecutionGuard::acquire_at(&path);
        set_test_dacl(&path, "D:P(A;;FA;;;AU)(A;;FA;;;SY)");
        let message = refused.unwrap_err().to_string();
        assert!(message.contains("legacy core execution lock"));
        assert!(message.contains(path.file_name().unwrap().to_str().unwrap()));
        drop(CoreExecutionGuard::acquire_at(&path)?);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn legacy_and_global_reservations_exclude_each_other() -> Result<()> {
        let path = std::env::temp_dir().join(format!("execution-legacy-bridge-{}", std::process::id()));
        let legacy = open_coordination_file(&path)?;
        legacy.try_lock()?;
        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
        legacy.unlock()?;
        let modern = CoreExecutionGuard::acquire_at(&path)?;
        assert!(matches!(legacy.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(modern);
        legacy.try_lock()?;
        drop(legacy);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn child_reservation_holder() -> Result<()> {
        if let Some(path) = std::env::var_os("ZENCLASH_TEST_HOLD_LOCK") {
            let path = Path::new(&path);
            let _guard = CoreExecutionGuard::acquire_at(path)?;
            std::fs::write(path.with_extension("ready"), b"ready")?;
            loop {
                std::thread::park();
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn terminated_process_releases_both_reservations() -> Result<()> {
        use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
        use windows_sys::Win32::System::Threading::OpenMutexW;
        let path = std::env::temp_dir().join(format!("execution-crash-{}", std::process::id()));
        let ready = path.with_extension("ready");
        let mut keep_mutex_alive = None;
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "execution::tests::child_reservation_holder"])
            .env("ZENCLASH_TEST_HOLD_LOCK", &path)
            .spawn()?;
        let result = (|| -> Result<()> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !ready.exists() {
                anyhow::ensure!(
                    std::time::Instant::now() < deadline && child.try_wait()?.is_none(),
                    "child did not reserve execution"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(CoreExecutionGuard::acquire_at(&path).is_err());
            let wide: Vec<u16> = test_coordination_name(&path).encode_utf16().chain(Some(0)).collect();
            let handle = unsafe { OpenMutexW(0x00100000, 0, wide.as_ptr()) };
            anyhow::ensure!(!handle.is_null(), "could not retain mutex for abandonment test");
            keep_mutex_alive = Some(unsafe { OwnedHandle::from_raw_handle(handle) });
            Ok(())
        })();
        let _ = child.kill();
        child.wait()?;
        result?;
        let checked = std::cell::Cell::new(false);
        let refused = CoreExecutionGuard::acquire_windows_at(
            &test_coordination_name(&path),
            &path,
            || panic!("the legacy lock exists"),
            || {
                checked.set(true);
                anyhow::bail!("residual core is still running");
            },
        );
        assert!(checked.get());
        assert!(refused.unwrap_err().to_string().contains("residual core"));
        drop(CoreExecutionGuard::acquire_at(&path)?);
        drop(keep_mutex_alive);
        std::fs::remove_file(path)?;
        std::fs::remove_file(ready)?;
        Ok(())
    }

    #[tokio::test]
    async fn termination_event_releases_reservation_even_when_pid_still_exists() -> Result<()> {
        let path = std::env::temp_dir().join(format!("execution-reused-pid-test-{}", std::process::id()));
        let guard = CoreExecutionGuard::acquire_at(&path).context("initial reservation")?;
        let (terminated, receiver) = tokio::sync::oneshot::channel();
        let waiter = guard.release_after_exit(std::process::id(), receiver);
        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
        terminated
            .send(())
            .map_err(|_| anyhow::anyhow!("exit observer dropped"))?;
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await??;
        drop(CoreExecutionGuard::acquire_at(&path).context("reservation after termination event")?);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[cfg(all(target_os = "linux", feature = "client"))]
    #[test]
    fn linux_idle_helper_from_other_channel_does_not_block_fallback() -> Result<()> {
        let other = if cfg!(feature = "development-channel") {
            "zenclash-service"
        } else {
            "zenclash-service-dev"
        };
        let current = crate::SERVICE_SLUG;
        require_no_unix_core_processes(&format!("zenclash-servic /var/lib/{other}/bin/zenclash-service"), true)?;
        assert!(
            require_no_unix_core_processes(
                &format!("zenclash-servic /var/lib/{current}/bin/zenclash-service"),
                true
            )
            .is_err()
        );
        assert!(require_no_unix_core_processes("zenclash-servic /tmp/zenclash-service", true).is_err());
        assert!(
            require_no_unix_core_processes(
                &format!("zenclash-servic /var/lib/{other}/bin/zenclash-service\nmihomo-alpha /tmp/mihomo-alpha"),
                true
            )
            .is_err()
        );
        Ok(())
    }
    #[cfg(all(unix, feature = "client"))]
    #[test]
    fn alpha_core_blocks_fallback() {
        for ipc_failed in [false, true] {
            assert!(require_no_unix_core_processes("mihomo-alpha", ipc_failed).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn fifo_coordination_entry_does_not_block() -> Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::env::temp_dir().join(format!("execution-fifo-test-{}", std::process::id()));
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        anyhow::ensure!(unsafe { platform_lib::mkfifo(name.as_ptr(), 0o600) } == 0);
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "execution::tests::child_lock_probe"])
            .env("ZENCLASH_TEST_LOCK", &path)
            .spawn()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let result = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        if result.is_none() {
            child.kill()?;
            child.wait()?;
        }
        std::fs::remove_file(path)?;
        assert!(
            result.is_some_and(|status| status.success()),
            "opening a FIFO must fail without waiting for a writer"
        );
        Ok(())
    }
    #[cfg(all(target_os = "macos", feature = "client"))]
    #[test]
    fn an_idle_helper_from_the_other_channel_does_not_block_fallback() -> Result<()> {
        let production =
            "/Library/PrivilegedHelperTools/org.zenclash.app.service.bundle/Contents/MacOS/zenclash-service";
        let development =
            "/Library/PrivilegedHelperTools/org.zenclash.app.dev.service.bundle/Contents/MacOS/zenclash-service";
        let (current, other) = if cfg!(feature = "development-channel") {
            (development, production)
        } else {
            (production, development)
        };
        require_no_unix_core_processes(other, true)?;
        for residual in [
            current.to_owned(),
            "zenclash-service".into(),
            format!("/tmp{other}"),
            format!("{other}\n/Library/Application Support/zenclash-service/cores/mihomo"),
        ] {
            assert!(
                require_no_unix_core_processes(&residual, true).is_err(),
                "unconfirmed idle state must remain blocked: {residual}"
            );
        }
        for residual in [
            current.to_owned(),
            format!("/tmp/mihomo\n{current}"),
            format!("{current}\n/tmp/mihomo"),
        ] {
            let refusal = require_no_unix_core_processes(&residual, true).expect_err("residual helper must block");
            assert!(
                refusal.downcast_ref::<ResidualServiceError>().is_some(),
                "a residual helper must stay repairable: {residual}"
            );
        }
        Ok(())
    }

    #[test]
    fn child_lock_probe() -> Result<()> {
        if let Some(path) = std::env::var_os("ZENCLASH_TEST_LOCK") {
            assert!(CoreExecutionGuard::acquire_at(Path::new(&path)).is_err());
        }
        Ok(())
    }

    #[test]
    fn reservation_excludes_other_processes_until_drop() -> Result<()> {
        let root = std::env::temp_dir().join(format!("execution-lock-test-{}", std::process::id()));
        let first = CoreExecutionGuard::acquire_at(&root)?;
        assert!(CoreExecutionGuard::acquire_at(&root).is_err());
        let child = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "execution::tests::child_lock_probe"])
            .env("ZENCLASH_TEST_LOCK", &root)
            .status()?;
        assert!(child.success());
        drop(first);
        drop(CoreExecutionGuard::acquire_at(&root)?);
        std::fs::remove_file(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reservation_survives_stop_request_until_process_exit() -> Result<()> {
        let root = std::env::temp_dir().join(format!("execution-exit-test-{}", std::process::id()));
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let guard = CoreExecutionGuard::acquire_at(&root)?;
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let waiter = guard.release_after_exit(child.id(), receiver);
        assert!(CoreExecutionGuard::acquire_at(&root).is_err());
        child.kill()?;
        child.wait()?;
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter).await??;
        drop(CoreExecutionGuard::acquire_at(&root)?);
        std::fs::remove_file(root)?;
        Ok(())
    }
}
