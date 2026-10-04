//! Cross-process exclusion shared by the GUI child and privileged service.
// Windows mutex owner-thread and ACL follow Clash Verge Service IPC (GPL-3.0).

use std::io;

/// Reservation held until the owned kernel has exited and been reaped.
pub struct CoreExecutionGuard {
    #[cfg(windows)]
    release: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(windows)]
    worker: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    file: std::fs::File,
}

impl CoreExecutionGuard {
    /// Reserves the machine-wide ZenClash kernel slot without waiting.
    ///
    /// # Errors
    /// Reports an occupied slot or unverified native coordination state.
    pub fn acquire() -> io::Result<Self> {
        Self::acquire_named("zenclash.core-execution")
    }

    /// Retains a cancelled owner's reservation while a background reaper retries.
    /// `confirm` must own the child handle and return true only after confirmed exit.
    pub fn release_after_exit(self, confirm: impl FnMut() -> bool + Send + 'static) {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Some((self, confirm))));
        let worker = pending.clone();
        let started = std::thread::Builder::new()
            .name("zenclash-core-reaper".into())
            .spawn(move || {
                loop {
                    let mut state = worker.lock().unwrap_or_else(|error| error.into_inner());
                    if state.as_mut().is_none_or(|(_, confirm)| confirm()) {
                        state.take();
                        return;
                    }
                    drop(state);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });
        if started.is_err() {
            // Thread creation failure must not publish a free slot for a live child.
            std::mem::forget(pending);
        }
    }

    fn acquire_named(name: &str) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let name = format!("Global\\{name}");
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            // Mutex ownership is thread-affine; a Tokio task may move threads.
            let worker = std::thread::Builder::new()
                .name("zenclash-core-reservation".into())
                .spawn(move || match windows::Owner::acquire(&name) {
                    Ok(owner) => {
                        if ready_tx.send(Ok(())).is_ok() {
                            let _ = release_rx.recv();
                        }
                        drop(owner);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                })?;
            match ready_rx
                .recv()
                .map_err(io::Error::other)
                .and_then(|result| result)
            {
                Ok(()) => Ok(Self {
                    release: Some(release_tx),
                    worker: Some(worker),
                }),
                Err(error) => {
                    drop(release_tx);
                    let _ = worker.join();
                    Err(error)
                }
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let path = std::path::Path::new("/tmp").join(format!("{name}.lock"));
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o444)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(file) => {
                    file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
                    file
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&path)?
                }
                Err(error) => return Err(error),
            };
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("invalid kernel reservation file"));
            }
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    io::Error::new(io::ErrorKind::WouldBlock, "ZenClash kernel already running")
                }
                std::fs::TryLockError::Error(error) => error,
            })?;
            Ok(Self { file })
        }
    }
}

impl Drop for CoreExecutionGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            drop(self.release.take());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
        #[cfg(unix)]
        {
            let _ = self.file.unlock();
        }
    }
}

/// Reads the current GUI's native privilege, without prompting or elevating.
#[must_use]
pub fn current_process_elevated() -> bool {
    #[cfg(windows)]
    {
        windows::elevated().unwrap_or(false)
    }
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no pointer arguments or side effects.
        unsafe { libc::geteuid() == 0 }
    }
}

/// Confirms that a fallback cannot collide with a service-owned kernel.
/// IPC failure alone is insufficient: native host and residual child checks follow.
///
/// # Errors
/// Rejects occupied, unauthorized, incompatible or uncertain native state.
pub async fn check_sidecar_available() -> io::Result<()> {
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::ServiceClient::inspect_busy(),
    )
    .await;
    match probe {
        Ok(Ok(false)) => {}
        Ok(Ok(true)) => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "service runtime is occupied",
            ));
        }
        Ok(Err(crate::ServiceClientError::Connection(_))) | Err(_) => {
            tokio::task::spawn_blocking(require_native_idle)
                .await
                .map_err(io::Error::other)??;
        }
        Ok(Err(error)) => return Err(io::Error::other(error)),
    }
    tokio::task::spawn_blocking(|| {
        require_no_residual_core()?;
        // The actual process owner acquires the same slot before spawning.
        drop(CoreExecutionGuard::acquire()?);
        Ok(())
    })
    .await
    .map_err(io::Error::other)?
}

fn require_native_idle() -> io::Result<()> {
    #[cfg(windows)]
    {
        crate::platform::require_stopped_service()?;
    }
    #[cfg(unix)]
    {
        // Both platforms expose native processes through the system ps utility.
        // Include stopped/starting hosts: an absent socket does not prove exit.
        let output = std::process::Command::new("/bin/ps")
            .args(["-axo", "comm="])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("cannot inspect service hosts"));
        }
        let processes = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        if processes.lines().any(|line| {
            std::path::Path::new(line.trim())
                .file_name()
                .is_some_and(|name| name == "zenclash-service")
        }) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "service host remains running",
            ));
        }
    }
    require_no_residual_core()
}

fn require_no_residual_core() -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::require_no_residual_core()
    }
    #[cfg(unix)]
    {
        let root = crate::platform::root_directory()?;
        let output = std::process::Command::new("/bin/ps")
            .args(["-axo", "command="])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("cannot inspect residual kernels"));
        }
        let processes = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        if processes
            .lines()
            .any(|line| line.contains(root.to_string_lossy().as_ref()) && line.contains("mihomo"))
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "service kernel remains running",
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        },
        System::Threading::{
            CreateMutexExW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, WaitForSingleObject,
        },
    };

    pub(super) struct Owner(OwnedHandle);
    impl Owner {
        pub(super) fn acquire(name: &str) -> io::Result<Self> {
            let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;RC;;;OW)(A;;0x00100001;;;AU)\0"
                .encode_utf16()
                .collect();
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: NUL-terminated SDDL and writable descriptor output.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: initialized attributes and NUL-terminated name remain alive.
            let raw = unsafe { CreateMutexExW(&attributes, wide.as_ptr(), 0, 0x00100001) };
            let error = io::Error::last_os_error();
            // SAFETY: descriptor was allocated by the native conversion routine.
            unsafe {
                LocalFree(descriptor);
            }
            if raw.is_null() {
                return Err(error);
            }
            // SAFETY: raw is a newly owned handle.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: handle is live and wait does not outlive it.
            match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
                WAIT_TIMEOUT => Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "ZenClash kernel already running",
                )),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: drop occurs on the same thread that acquired this mutex.
            unsafe {
                ReleaseMutex(self.0.as_raw_handle());
            }
        }
    }

    pub(super) fn elevated() -> io::Result<bool> {
        let mut token = std::ptr::null_mut();
        // SAFETY: current process pseudo-handle and writable token output.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: token is a newly owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0;
        // SAFETY: elevation output has the required native size.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(elevation.TokenIsElevated != 0)
    }

    pub(super) fn require_no_residual_core() -> io::Result<()> {
        use windows_sys::Win32::{
            Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                Threading::{
                    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
                },
            },
        };
        let root = crate::platform::root_directory()?;
        // SAFETY: snapshot uses no caller-owned pointers.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: snapshot is a newly owned handle.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: PROCESSENTRY32W requires zeroed fields plus its native size.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: initialized output and live snapshot.
        let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            let length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            if name.eq_ignore_ascii_case("mihomo.exe") {
                // SAFETY: query-only access, no handle inheritance.
                let raw = unsafe {
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID)
                };
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: process is a newly owned query handle.
                let process = unsafe { OwnedHandle::from_raw_handle(raw) };
                let mut path = vec![0_u16; 32768];
                let mut size = path.len() as u32;
                // SAFETY: native size matches writable UTF-16 output buffer.
                if unsafe {
                    QueryFullProcessImageNameW(
                        process.as_raw_handle(),
                        0,
                        path.as_mut_ptr(),
                        &mut size,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let path =
                    std::path::PathBuf::from(String::from_utf16_lossy(&path[..size as usize]));
                if path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with(&format!(
                        "{}\\",
                        root.to_string_lossy()
                            .to_ascii_lowercase()
                            .trim_end_matches('\\')
                    ))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "service kernel remains running",
                    ));
                }
            }
            // SAFETY: initialized output and live snapshot.
            found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_excludes_other_threads_and_releases_after_drop() {
        let name = format!(
            "zenclash-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let guard = CoreExecutionGuard::acquire_named(&name).unwrap();
        let other = name.clone();
        assert_eq!(
            std::thread::spawn(move || CoreExecutionGuard::acquire_named(&other)
                .err()
                .unwrap()
                .kind())
            .join()
            .unwrap(),
            io::ErrorKind::WouldBlock
        );
        drop(guard);
        drop(CoreExecutionGuard::acquire_named(&name).unwrap());
    }

    #[test]
    fn reservation_excludes_another_process() {
        let name = format!("zenclash-process-test-{}", std::process::id());
        let _guard = CoreExecutionGuard::acquire_named(&name).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution::tests::child_reservation_attempt",
                "--nocapture",
            ])
            .env("ZENCLASH_TEST_EXECUTION_LOCK", &name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn child_reservation_attempt() {
        if let Ok(name) = std::env::var("ZENCLASH_TEST_EXECUTION_LOCK") {
            assert_eq!(
                CoreExecutionGuard::acquire_named(&name)
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn cancelled_owner_keeps_reservation_until_exit_is_confirmed() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let name = format!("zenclash-reaper-test-{}", std::process::id());
        let exited = Arc::new(AtomicBool::new(false));
        let observed = exited.clone();
        CoreExecutionGuard::acquire_named(&name)
            .unwrap()
            .release_after_exit(move || observed.load(Ordering::Acquire));
        assert!(CoreExecutionGuard::acquire_named(&name).is_err());
        exited.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Ok(guard) = CoreExecutionGuard::acquire_named(&name) {
                drop(guard);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
