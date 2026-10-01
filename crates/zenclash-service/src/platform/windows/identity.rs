use std::{io, ptr};

use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, LocalFree, WAIT_TIMEOUT},
    Security::Authorization::ConvertSidToStringSidW,
    Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::Threading::{
        GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
    },
};

use super::{Handle, denied};
use crate::session::PeerIdentity;

pub(crate) fn sid_string(sid: *mut std::ffi::c_void) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: sid is supplied by a valid Windows token or security descriptor.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0;
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated LocalAlloc buffer.
    let value = unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

pub(crate) fn token_sid(token: HANDLE) -> io::Result<String> {
    let mut size = 0;
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size) };
    if size == 0 || size > 64 * 1024 {
        return Err(io::Error::last_os_error());
    }
    // usize alignment is sufficient for TOKEN_USER and the embedded SID.
    let mut data = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe { GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

pub(crate) fn process_handle(pid: u32) -> io::Result<Handle> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(handle))
}

pub(crate) fn process_identity(pid: u32) -> io::Result<PeerIdentity> {
    let process = process_handle(pid)?;
    identity_from_handle(pid, process.0)
}

pub(crate) fn identity_from_handle(pid: u32, process: HANDLE) -> io::Result<PeerIdentity> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { WaitForSingleObject(process, 0) } != WAIT_TIMEOUT {
        return Err(denied("peer process is no longer alive"));
    }
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let birth = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Ok(PeerIdentity::new(token_sid(token.0)?, pid, birth))
}

pub(crate) fn current_identity() -> io::Result<PeerIdentity> {
    process_identity(std::process::id())
}

#[cfg(feature = "server")]
pub(crate) fn require_admin() -> io::Result<()> {
    use windows_sys::Win32::{
        Security::{TOKEN_ELEVATION, TokenElevation},
        System::Threading::GetCurrentProcess,
    };
    let mut raw = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(raw);
    let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
    let mut len = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of_val(&elevation) as u32,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if elevation.TokenIsElevated == 0 {
        return Err(denied(
            "service maintenance requires administrator authorization",
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "server"))]
pub(crate) fn peer_alive(peer: &PeerIdentity) -> bool {
    process_identity(peer.pid()).is_ok_and(|actual| actual == *peer)
}

pub(crate) fn process_image(process: HANDLE) -> io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 32768];
    let mut len = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(len as usize);
    Ok(std::ffi::OsString::from_wide(&buffer).into())
}
