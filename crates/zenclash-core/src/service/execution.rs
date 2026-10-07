// ZenClash application execution lifetime adaptation, 2026-10-04. GPL-3.0-only.
// Native exclusion is provided by the upstream service; app reaping is retained from ZenClash.
use std::io;

/// Retains the upstream service/sidecar exclusion through the application's child lifetime.
pub struct CoreExecutionGuard {
    _native: zenclash_service::execution::CoreExecutionGuard,
}

impl CoreExecutionGuard {
    /// Acquires the same native exclusion as service-owned core execution.
    ///
    /// # Errors
    /// Returns native lock, residual-core or service-inspection errors when execution cannot be reserved.
    pub fn acquire() -> anyhow::Result<Self> {
        Ok(Self {
            _native: zenclash_service::execution::CoreExecutionGuard::acquire()?,
        })
    }

    /// Keeps exclusion until the application confirms termination, including failed kill/wait.
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
}

/// Reads GUI privileges without prompting or changing execution mode.
#[must_use]
pub fn current_process_elevated() -> bool {
    #[cfg(windows)]
    {
        windows::elevated().unwrap_or(false)
    }
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
}

/// Runs the upstream residual-service and core occupancy checks before sidecar fallback.
///
/// # Errors
/// Returns native inspection errors or evidence of a running service/residual core.
pub async fn check_sidecar_available() -> io::Result<()> {
    zenclash_service::execution::check_sidecar_available()
        .await
        .map_err(io::Error::other)
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
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
}
