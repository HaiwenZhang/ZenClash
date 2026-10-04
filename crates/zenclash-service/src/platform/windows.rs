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
pub(crate) use security::{service_root, validate_protected_path};
#[cfg(feature = "server")]
pub(crate) use transport::Listener;
pub(crate) use transport::connect;
#[cfg(feature = "server")]
pub(crate) use transport::{
    Stream as MaintenanceStream, maintenance_endpoint_absent, maintenance_peer,
};

#[cfg(feature = "server")]
pub(crate) fn maintenance_host_registered(peer: &crate::session::PeerIdentity) -> io::Result<()> {
    if scm::service_pid()? != Some(peer.pid()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "registered host identity changed",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) fn maintenance_identity(peer: &crate::session::PeerIdentity) -> bool {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::OpenProcessToken;
    let verified = || -> io::Result<bool> {
        let process = identity::process_handle(peer.pid())?;
        if identity::identity_from_handle(peer.pid(), process.0)? != *peer {
            return Ok(false);
        }
        if peer.user() == "S-1-5-18" {
            return Ok(true);
        }
        let mut token = std::ptr::null_mut();
        // SAFETY: process is a live native process and token is an owned output handle.
        if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Handle(token);
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0;
        // SAFETY: elevation points to correctly sized initialized native storage.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(size as usize == std::mem::size_of::<TOKEN_ELEVATION>()
            && elevation.TokenIsElevated != 0
            && identity::identity_from_handle(peer.pid(), process.0)? == *peer)
    };
    verified().unwrap_or(false)
}

#[cfg(feature = "server")]
pub(crate) fn maintenance_service_absent() -> io::Result<()> {
    if service_state()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "service remains registered",
        ));
    }
    Ok(())
}

#[cfg(feature = "server")]
pub(crate) fn maintenance_peer_exited(peer: &crate::session::PeerIdentity) -> io::Result<bool> {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    let process = match identity::process_handle(peer.pid()) {
        Ok(process) => process,
        Err(error) if error.raw_os_error() == Some(87) => return Ok(true),
        Err(error) => return Err(error),
    };
    // SAFETY: process owns a valid synchronizable native process handle.
    match unsafe { WaitForSingleObject(process.0, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => {
            Ok(identity::identity_from_handle(peer.pid(), process.0)?.birth() != peer.birth())
        }
        _ => Err(io::Error::last_os_error()),
    }
}

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
