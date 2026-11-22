#![deny(clippy::undocumented_unsafe_blocks)]

//! Read-only process privilege queries without prompting or changing execution mode.

/// Reads GUI privileges without prompting or changing execution mode.
/// Test builds and `test-support` model an ordinary, unelevated GUI owner.
#[must_use]
pub fn current_process_elevated() -> bool {
    // UI tests compile core as a dependency without cfg(test). Keep their
    // isolated owners unelevated too, including on administrator/root CI.
    // Native integration tests without test-support retain the system query.
    #[cfg(any(test, feature = "test-support"))]
    {
        false
    }
    #[cfg(all(windows, not(any(test, feature = "test-support"))))]
    {
        windows::elevated().unwrap_or(false)
    }
    #[cfg(all(unix, not(any(test, feature = "test-support"))))]
    {
        // SAFETY: `geteuid` has no preconditions or memory effects.
        unsafe { libc::geteuid() == 0 }
    }
}

#[cfg(all(windows, not(any(test, feature = "test-support"))))]
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
