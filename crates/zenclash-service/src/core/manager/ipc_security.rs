// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Core IPC socket/named-pipe preparation and owner-only ACL hardening.

use crate::OwnerIdentity;
#[cfg(any(unix, windows))]
use anyhow::Context as _;
use anyhow::Result;
use std::time::Duration;
#[cfg(any(unix, windows))]
use tracing::info;

pub(in crate::core) const CORE_IPC_READY_TIMEOUT: Duration = Duration::from_secs(15);
const CORE_IPC_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn prepare_core_ipc_socket(core_ipc_path: &str, owner: &OwnerIdentity) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let OwnerIdentity::Unix { uid, .. } = owner else {
            anyhow::bail!("Unix core IPC path received a non-Unix owner");
        };
        let target = std::path::Path::new(core_ipc_path);
        let directory = target.parent().context("core IPC path has no parent directory")?;
        let directory_c = std::ffi::CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| anyhow::anyhow!("core IPC directory contains NUL"))?;
        // SAFETY: `directory_c` is a NUL-terminated path and the returned
        // descriptor is owned by this scope.
        let fd = unsafe {
            platform_lib::open(
                directory_c.as_ptr(),
                platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to open core IPC directory {directory:?}"));
        }

        let result = (|| -> Result<()> {
            // SAFETY: `stat` is a plain C struct; all-zero is valid before `fstat`.
            let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
            // SAFETY: `fd` is live and `stat` is writable storage for one `stat`.
            if unsafe { platform_lib::fstat(fd, &mut stat) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to inspect core IPC directory");
            }
            // SAFETY: `geteuid` has no preconditions or memory effects.
            let effective_uid = unsafe { platform_lib::geteuid() };
            if stat.st_mode & platform_lib::S_IFMT != platform_lib::S_IFDIR
                || (stat.st_uid != 0 && stat.st_uid != *uid && stat.st_uid != effective_uid)
            {
                anyhow::bail!("core IPC directory has an unexpected owner or file type");
            }
            // SAFETY: `fchmod` operates on the live descriptor and the mode is valid.
            if unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to make core IPC directory private");
            }
            // SAFETY: `fchown` operates on the live descriptor with valid ids.
            if effective_uid == 0 && unsafe { platform_lib::fchown(fd, 0, 0) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to take ownership of core IPC directory");
            }

            let file_name = target.file_name().context("core IPC path has no file name")?;
            let file_name_c = std::ffi::CString::new(file_name.as_bytes())
                .map_err(|_| anyhow::anyhow!("core IPC file name contains NUL"))?;
            // SAFETY: `fd` is live and `file_name_c` is a NUL-terminated component.
            if unsafe { platform_lib::unlinkat(fd, file_name_c.as_ptr(), 0) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error).context("failed to clear stale core IPC entry");
                }
            }
            Ok(())
        })();
        // SAFETY: `fd` was not closed on the success path and is owned here.
        unsafe { platform_lib::close(fd) };
        result
    }

    #[cfg(windows)]
    {
        let _ = (core_ipc_path, owner);
        Ok(())
    }
}

#[cfg(unix)]
fn grant_core_ipc_directory_to_owner(target: &std::path::Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let directory = target.parent().context("core IPC path has no parent directory")?;
    let directory_c = std::ffi::CString::new(directory.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("core IPC directory contains NUL"))?;
    // SAFETY: `directory_c` is a NUL-terminated path and the returned
    // descriptor is owned by this scope.
    let fd = unsafe {
        platform_lib::open(
            directory_c.as_ptr(),
            platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to reopen core IPC directory");
    }
    // SAFETY: `geteuid` has no preconditions; `fchown` operates on the live fd.
    let result = if unsafe { platform_lib::geteuid() } == 0 && unsafe { platform_lib::fchown(fd, uid, gid) } != 0 {
        Err(std::io::Error::last_os_error()).context("failed to grant core IPC directory to owner")
    // SAFETY: `fchmod` operates on the live descriptor and the mode is valid.
    } else if unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } != 0 {
        Err(std::io::Error::last_os_error()).context("failed to secure granted core IPC directory")
    } else {
        Ok(())
    };
    // SAFETY: `fd` was not closed on the success path and is owned here.
    unsafe { platform_lib::close(fd) };
    result
}

pub(super) async fn secure_core_ipc_socket(
    core_ipc_path: String,
    owner: OwnerIdentity,
    expected_pid: Option<u32>,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::FileTypeExt as _;

        let _ = expected_pid;
        let OwnerIdentity::Unix { uid, gid } = owner else {
            anyhow::bail!("Unix core IPC path received a non-Unix owner");
        };
        let target = std::path::PathBuf::from(core_ipc_path);
        let mut found = false;
        let deadline = tokio::time::Instant::now() + CORE_IPC_READY_TIMEOUT;
        loop {
            match tokio::fs::symlink_metadata(&target).await {
                Ok(metadata) if metadata.file_type().is_socket() => {
                    found = true;
                    break;
                }
                Ok(_) => {
                    anyhow::bail!("core IPC path {target:?} is not a socket");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(CORE_IPC_POLL_INTERVAL).await;
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }
        if !found {
            anyhow::bail!("core IPC socket did not appear at {target:?} within {CORE_IPC_READY_TIMEOUT:?}");
        }
        let path = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| anyhow::anyhow!("core IPC socket path contains NUL"))?;
        // SAFETY: `geteuid` has no preconditions; `lchown` uses a
        // NUL-terminated path and does not follow symlinks.
        let chown_ok =
            unsafe { platform_lib::geteuid() } != 0 || unsafe { platform_lib::lchown(path.as_ptr(), uid, gid) } == 0;
        // SAFETY: `path` is NUL-terminated and `fchmodat` is told not to
        // follow symlinks; the mode is valid.
        let chmod_ok = unsafe {
            platform_lib::fchmodat(
                platform_lib::AT_FDCWD,
                path.as_ptr(),
                0o600 as platform_lib::mode_t,
                platform_lib::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        let os_error = (!chown_ok || !chmod_ok).then(std::io::Error::last_os_error);
        if !chown_ok || !chmod_ok {
            return Err(os_error.unwrap_or_else(std::io::Error::last_os_error).into());
        }
        grant_core_ipc_directory_to_owner(&target, uid, gid)?;
        info!("Secured core IPC socket {:?} for uid {}", target, uid);
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_KERNEL_OBJECT, SetSecurityInfo,
        };
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        };
        use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

        let OwnerIdentity::Windows { sid } = owner else {
            anyhow::bail!("Windows core IPC path received a non-Windows owner");
        };
        let mut pipe: Vec<u16> = std::ffi::OsStr::new(&core_ipc_path).encode_wide().collect();
        pipe.push(0);
        let deadline = tokio::time::Instant::now() + CORE_IPC_READY_TIMEOUT;
        let handle_value = loop {
            // SAFETY: `pipe` is NUL-terminated; the returned handle is checked
            // against INVALID_HANDLE_VALUE before adoption.
            let handle_value = unsafe {
                CreateFileW(
                    pipe.as_ptr(),
                    READ_CONTROL | WRITE_DAC,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            } as isize;
            if handle_value != INVALID_HANDLE_VALUE as isize {
                break handle_value;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("core IPC pipe did not become ready within {CORE_IPC_READY_TIMEOUT:?}"));
            }
            tokio::time::sleep(CORE_IPC_POLL_INTERVAL).await;
        };
        let handle = handle_value as *mut std::ffi::c_void;
        // SAFETY: `handle` is a valid non-INVALID_HANDLE_VALUE handle and the
        // File now owns it.
        let _pipe = unsafe { std::fs::File::from_raw_handle(handle) };
        let mut server_pid = 0u32;
        // SAFETY: The pipe handle is live and `server_pid` is writable storage.
        if unsafe { GetNamedPipeServerProcessId(handle_value as _, &mut server_pid) } == 0 {
            return Err(std::io::Error::last_os_error()).context("failed to identify core IPC pipe server");
        }
        if Some(server_pid) != expected_pid {
            anyhow::bail!("core IPC pipe server PID {server_pid} did not match spawned core PID {expected_pid:?}");
        }

        let sddl = windows_owner_pipe_sddl(&sid);
        let mut wide: Vec<u16> = sddl.encode_utf16().collect();
        wide.push(0);
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and `descriptor` is a writable
        // out-parameter; the allocation is freed by LocalDescriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
            || descriptor.is_null()
        {
            return Err(std::io::Error::last_os_error().into());
        }
        struct LocalDescriptor(*mut std::ffi::c_void);
        impl Drop for LocalDescriptor {
            fn drop(&mut self) {
                // SAFETY: `self.0` is a descriptor allocation from
                // ConvertStringSecurityDescriptorToSecurityDescriptorW.
                unsafe { LocalFree(self.0) };
            }
        }
        let descriptor_guard = LocalDescriptor(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: The descriptor is live and all out-parameters are writable.
        if unsafe { GetSecurityDescriptorDacl(descriptor_guard.0, &mut present, &mut dacl, &mut defaulted) } == 0
            || present == 0
            || dacl.is_null()
        {
            anyhow::bail!("failed to read owner core IPC DACL");
        }
        // SAFETY: The pipe handle is live and `dacl` is borrowed from the live
        // descriptor for the duration of the call.
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            anyhow::bail!("failed to secure core IPC pipe: Windows error {status}");
        }
        info!("Secured core IPC pipe for owner SID");
        Ok(())
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_owner_pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)")
}
