use crate::ServiceErrorCode;
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::paths::service_paths;
#[cfg(unix)]
use tracing::{info, warn};

pub(crate) async fn cleanup_legacy_owner_files(owner: &AuthenticatedOwner) -> Result<(), ServiceError> {
    let marker = service_paths()
        .map_err(|error| cleanup_error(error.to_string()))?
        .for_owner(&owner.identity)
        .root()
        .join("legacy-cleanup-v1");
    crate::core::paths::ensure_owner_state_directory(&owner.identity)
        .map_err(|error| cleanup_error(format!("failed to secure cleanup marker root: {error:#}")))?;
    if marker.is_file() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        let crate::OwnerIdentity::Unix { uid, .. } = owner.identity else {
            return Err(cleanup_error("Unix cleanup received a non-Unix owner"));
        };
        if uid != 0 {
            let root = owner.app_data_root.clone();
            tokio::task::spawn_blocking(move || cleanup_root_owned_entries(&root))
                .await
                .map_err(|error| cleanup_error(format!("legacy cleanup task failed: {error}")))??;
        }
    }

    #[cfg(windows)]
    {
        let root = owner.app_data_root.clone();
        tokio::task::spawn_blocking(move || crate::core::windows_legacy_cleanup::cleanup_system_owned_entries(&root))
            .await
            .map_err(|error| cleanup_error(format!("legacy cleanup task failed: {error}")))??;
    }

    tokio::fs::write(&marker, b"ok\n")
        .await
        .map_err(|error| cleanup_error(format!("failed to persist cleanup marker: {error}")))?;
    Ok(())
}

#[cfg(unix)]
fn cleanup_root_owned_entries(root: &std::path::Path) -> Result<(), ServiceError> {
    use std::os::unix::ffi::OsStrExt as _;

    let path = std::ffi::CString::new(root.as_os_str().as_bytes())
        .map_err(|_| cleanup_error("application data root contains NUL"))?;
    let fd = unsafe {
        platform_lib::open(
            path.as_ptr(),
            platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(cleanup_error(format!(
            "failed to open application data root: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
    if unsafe { platform_lib::fstat(fd, &mut stat) } != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { platform_lib::close(fd) };
        return Err(cleanup_error(format!(
            "failed to inspect application data root: {error}"
        )));
    }
    let result = cleanup_directory_fd(fd, stat.st_dev);
    unsafe { platform_lib::close(fd) };
    result
}

#[cfg(unix)]
fn cleanup_directory_fd(dirfd: std::os::fd::RawFd, root_device: platform_lib::dev_t) -> Result<(), ServiceError> {
    let duplicate = unsafe { platform_lib::dup(dirfd) };
    if duplicate < 0 {
        return Err(cleanup_error(format!(
            "failed to duplicate cleanup directory handle: {}",
            std::io::Error::last_os_error()
        )));
    }
    let directory = unsafe { platform_lib::fdopendir(duplicate) };
    if directory.is_null() {
        unsafe { platform_lib::close(duplicate) };
        return Err(cleanup_error(format!(
            "failed to enumerate cleanup directory: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut names = Vec::new();
    clear_errno();
    loop {
        let entry = unsafe { platform_lib::readdir(directory) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                unsafe { platform_lib::closedir(directory) };
                return Err(cleanup_error(format!(
                    "failed while enumerating cleanup directory: {error}"
                )));
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    unsafe { platform_lib::closedir(directory) };

    for name in names {
        cleanup_entry_at(dirfd, &name, root_device)?;
    }
    Ok(())
}

#[cfg(unix)]
fn clear_errno() {
    #[cfg(target_os = "macos")]
    unsafe {
        *platform_lib::__error() = 0;
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        *platform_lib::__errno_location() = 0;
    }
}

#[cfg(unix)]
fn cleanup_entry_at(
    dirfd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    root_device: platform_lib::dev_t,
) -> Result<(), ServiceError> {
    let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
    if unsafe { platform_lib::fstatat(dirfd, name.as_ptr(), &mut stat, platform_lib::AT_SYMLINK_NOFOLLOW) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(cleanup_error(format!("failed to inspect legacy entry: {error}")));
    }
    if stat.st_dev != root_device {
        return Ok(());
    }

    let file_type = stat.st_mode & platform_lib::S_IFMT;
    if stat.st_uid == 0 && (file_type == platform_lib::S_IFREG || file_type == platform_lib::S_IFLNK) {
        if unsafe { platform_lib::unlinkat(dirfd, name.as_ptr(), 0) } != 0 {
            return Err(cleanup_error(format!(
                "failed to unlink root-owned legacy entry: {}",
                std::io::Error::last_os_error()
            )));
        }
        info!(entry = %name.to_string_lossy(), "Removed root-owned legacy entry");
    } else if file_type == platform_lib::S_IFDIR {
        let child = unsafe {
            platform_lib::openat(
                dirfd,
                name.as_ptr(),
                platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
            )
        };
        if child < 0 {
            return Err(cleanup_error(format!(
                "failed to open root-owned legacy directory: {}",
                std::io::Error::last_os_error()
            )));
        }
        let result = cleanup_directory_fd(child, root_device);
        unsafe { platform_lib::close(child) };
        result?;
        if stat.st_uid == 0 && unsafe { platform_lib::unlinkat(dirfd, name.as_ptr(), platform_lib::AT_REMOVEDIR) } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(platform_lib::ENOTEMPTY) {
                return Err(cleanup_error(format!(
                    "failed to remove root-owned legacy directory: {error}"
                )));
            }
        } else if stat.st_uid == 0 {
            info!(entry = %name.to_string_lossy(), "Removed root-owned legacy directory");
        }
    } else if stat.st_uid == 0 {
        warn!(entry = %name.to_string_lossy(), "Skipped special root-owned legacy entry");
    }
    Ok(())
}

fn cleanup_error(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ServiceErrorCode::LegacyCleanupFailed, message)
}

#[cfg(all(test, unix))]
mod tests {
    use super::cleanup_root_owned_entries;

    #[test]
    fn cleanup_does_not_remove_current_user_files_or_follow_symlinks() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("legacy-cleanup-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("legacy-cleanup-outside-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("user-file"), b"keep")?;
        std::fs::write(&outside, b"keep")?;
        std::os::unix::fs::symlink(&outside, root.join("outside-link"))?;

        cleanup_root_owned_entries(&root).map_err(|error| anyhow::anyhow!(error))?;

        assert!(root.join("user-file").is_file());
        assert!(root.join("outside-link").is_symlink());
        assert_eq!(std::fs::read(&outside)?, b"keep");
        std::fs::remove_dir_all(root)?;
        std::fs::remove_file(outside)?;
        Ok(())
    }
}
