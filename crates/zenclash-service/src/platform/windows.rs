//! Native Windows privilege, transport and service management.

#[cfg(feature = "server")]
#[path = "windows/child.rs"]
mod child;
#[path = "windows/identity.rs"]
mod identity;
#[path = "windows/install.rs"]
mod install;
#[path = "windows/scm.rs"]
mod scm;
#[path = "windows/security.rs"]
mod security;
#[path = "windows/transport.rs"]
mod transport;

#[cfg(feature = "server")]
pub(crate) use child::NativeChild;
pub(crate) use identity::current_identity;
#[cfg(feature = "server")]
pub(crate) use identity::{peer_alive, process_identity as identity_from_pid, require_admin};
pub(crate) use install::{open_pinned_file, request_maintenance};
#[cfg(feature = "server")]
pub(crate) use scm::{
    dispatch_service, register_service, report_ready, service_state, start_service, stop_service,
    unregister_service,
};
#[cfg(feature = "server")]
pub(crate) use security::create_private_directory;
#[cfg(feature = "server")]
pub(crate) use security::{service_root, validate_protected_path};
#[cfg(feature = "server")]
pub(crate) use transport::Listener;
pub(crate) use transport::connect;

#[cfg(feature = "server")]
use std::ptr;
use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(feature = "server")]
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

pub(crate) const SERVICE_NAME: &str = "ZenClashService";
pub(crate) const PIPE_NAME: &str = r"\\.\pipe\ZenClash.Service.v1";

fn wide(value: impl AsRef<OsStr>) -> io::Result<Vec<u16>> {
    let value: Vec<_> = value.as_ref().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
    }
    Ok(value.into_iter().chain([0]).collect())
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// RAII handle which is never inherited by children.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: Handle exclusively owns a valid Win32 handle.
        unsafe { CloseHandle(self.0) };
    }
}

// Win32 kernel handles may be used from any thread. No borrowed buffers are held.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

/// Close-on-crash child containment. Create this before launching any child.
#[cfg(feature = "server")]
struct ChildGuard(Handle);

#[cfg(feature = "server")]
impl ChildGuard {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Null security attributes create a non-inheritable anonymous job.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = Handle(job);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits is the correct structure with its exact byte size.
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}
