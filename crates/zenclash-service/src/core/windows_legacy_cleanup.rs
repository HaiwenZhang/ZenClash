// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::ServiceErrorCode;
use crate::core::auth::ServiceError;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_DIR_NOT_EMPTY, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, OWNER_SECURITY_INFORMATION, SECURITY_MAX_SID_SIZE, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FileDispositionInfo,
    GetFileInformationByHandle, GetFileType, GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL,
    SetFileInformationByHandle,
};

pub(crate) fn cleanup_system_owned_entries(root: &Path) -> Result<(), ServiceError> {
    let root_handle = open_entry(root)
        .map_err(|error| cleanup_error(format!("failed to open application data root {root:?}: {error}")))?;
    let root_path = final_path(root_handle.0)?;
    cleanup_directory(root, &root_path)
}

fn cleanup_directory(path: &Path, root_path: &str) -> Result<(), ServiceError> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| cleanup_error(format!("failed to enumerate legacy directory {path:?}: {error}")))?;
    for entry in entries {
        let entry = entry.map_err(|error| cleanup_error(format!("failed to read legacy directory entry: {error}")))?;
        cleanup_entry(&entry.path(), root_path)?;
    }
    Ok(())
}

fn cleanup_entry(path: &Path, root_path: &str) -> Result<(), ServiceError> {
    let handle = match open_entry(path) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(cleanup_error(format!("failed to open legacy entry {path:?}: {error}")));
        }
    };
    let opened_path = final_path(handle.0)?;
    if !path_is_below_root(root_path, &opened_path) {
        return Err(cleanup_error("legacy entry escaped the authenticated application root"));
    }

    let information = handle_information(handle.0)?;
    let is_directory = information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let is_reparse = information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let system_owned = is_local_system_owned(handle.0)?;

    if is_directory && !is_reparse {
        cleanup_directory(path, root_path)?;
        if system_owned {
            delete_open_entry(handle.0, true)?;
        }
    } else if system_owned && (is_reparse || unsafe { GetFileType(handle.0) } == FILE_TYPE_DISK) {
        delete_open_entry(handle.0, false)?;
    }
    Ok(())
}

fn open_entry(path: &Path) -> std::io::Result<OwnedHandle> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "legacy cleanup path contains NUL",
        ));
    }
    wide.push(0);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(OwnedHandle(handle))
}

fn handle_information(handle: HANDLE) -> Result<BY_HANDLE_FILE_INFORMATION, ServiceError> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(cleanup_error(format!(
            "failed to inspect legacy entry: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(information)
}

fn final_path(handle: HANDLE) -> Result<String, ServiceError> {
    let mut buffer = vec![0_u16; 512];
    loop {
        let length = unsafe { GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) } as usize;
        if length == 0 {
            return Err(cleanup_error(format!(
                "failed to resolve legacy entry handle: {}",
                std::io::Error::last_os_error()
            )));
        }
        if length < buffer.len() {
            return String::from_utf16(&buffer[..length])
                .map_err(|_| cleanup_error("legacy entry path is not valid UTF-16"));
        }
        buffer.resize(length + 1, 0);
    }
}

fn path_is_below_root(root: &str, candidate: &str) -> bool {
    let root = root.trim_end_matches(['\\', '/']).to_lowercase();
    let candidate = candidate.trim_end_matches(['\\', '/']).to_lowercase();
    candidate
        .strip_prefix(&root)
        .is_some_and(|tail| tail.starts_with('\\') || tail.starts_with('/'))
}

fn is_local_system_owned(handle: HANDLE) -> Result<bool, ServiceError> {
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() || owner.is_null() {
        return Err(cleanup_error(format!(
            "failed to inspect legacy entry owner: Windows error {status}"
        )));
    }
    let descriptor = LocalDescriptor(descriptor);

    let words = (SECURITY_MAX_SID_SIZE as usize).div_ceil(std::mem::size_of::<usize>());
    let mut sid = vec![0_usize; words];
    let mut sid_size = SECURITY_MAX_SID_SIZE;
    if unsafe {
        CreateWellKnownSid(
            WinLocalSystemSid,
            std::ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
        )
    } == 0
    {
        return Err(cleanup_error(format!(
            "failed to create LocalSystem SID: {}",
            std::io::Error::last_os_error()
        )));
    }
    let is_system = unsafe { EqualSid(owner, sid.as_mut_ptr().cast()) } != 0;
    drop(descriptor);
    Ok(is_system)
}

fn delete_open_entry(handle: HANDLE, is_directory: bool) -> Result<(), ServiceError> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        let error = std::io::Error::last_os_error();
        if is_directory && error.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY as i32) {
            return Ok(());
        }
        return Err(cleanup_error(format!(
            "failed to delete SYSTEM-owned legacy entry: {error}"
        )));
    }
    Ok(())
}

fn cleanup_error(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ServiceErrorCode::LegacyCleanupFailed, message)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalDescriptor(*mut std::ffi::c_void);

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::path_is_below_root;

    #[test]
    fn final_path_check_is_component_bounded_and_case_insensitive() {
        assert!(path_is_below_root(
            r"\\?\C:\Users\Alice\AppData\Roaming\org.zenclash.app",
            r"\\?\c:\users\alice\appdata\roaming\org.zenclash.app\logs\a.log"
        ));
        assert!(!path_is_below_root(
            r"\\?\C:\Users\Alice\AppData\Roaming\app",
            r"\\?\C:\Users\Alice\AppData\Roaming\app-escape\file"
        ));
    }
}
