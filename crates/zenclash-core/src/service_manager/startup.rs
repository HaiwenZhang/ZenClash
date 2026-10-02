//! Ordinary-executable evidence for startup before validation can execute it.

use std::{fs::File, path::Path};

use crate::{MihomoError, MihomoResult};

/// Verifies a Local startup executable without executing it or changing permissions.
///
/// Unix set-user-ID, set-group-ID and Linux file capabilities are rejected. A
/// root-owned ordinary executable is allowed: ownership alone does not elevate
/// its child. This check is not authorization to adopt a protected service copy.
/// Run on a background worker, before invoking executable validation.
///
/// # Errors
/// Returns an error for non-files, unreadable metadata/capabilities, or privilege
/// attributes. No native authorization, permission change or process is started.
pub fn verify_ordinary_local_executable(binary: &Path) -> MihomoResult<()> {
    let file = File::open(binary).map_err(|error| MihomoError::Process(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MihomoError::Process(error.to_string()))?;
    if !metadata.is_file() {
        return Err(MihomoError::InvalidInput(zenclash_i18n::text(
            "startup.local_privileged",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o6000 != 0 {
            return Err(MihomoError::InvalidInput(zenclash_i18n::text(
                "startup.local_privileged",
            )));
        }
    }
    #[cfg(target_os = "linux")]
    verify_no_linux_capabilities(&file)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_no_linux_capabilities(file: &File) -> MihomoResult<()> {
    use std::os::fd::AsRawFd;
    read_linux_capabilities(file.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn read_linux_capabilities(fd: std::os::fd::RawFd) -> MihomoResult<()> {
    // SAFETY: the descriptor remains open; this only asks for the attribute size
    // with a valid static C name and no output buffer.
    let size =
        unsafe { libc::fgetxattr(fd, c"security.capability".as_ptr(), std::ptr::null_mut(), 0) };
    if size == 0 {
        return Ok(());
    }
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
            return Ok(());
        }
        return Err(MihomoError::Process(error.to_string()));
    }
    Err(MihomoError::InvalidInput(zenclash_i18n::text(
        "startup.local_privileged",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "zenclash-ordinary-executable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"not executed").unwrap();
        path
    }

    #[test]
    fn ordinary_executable_rejects_unreadable_source() {
        let path = fixture();
        std::fs::remove_file(&path).unwrap();
        assert!(verify_ordinary_local_executable(&path).is_err());
    }

    #[test]
    fn ordinary_executable_accepts_regular_unprivileged_file_without_execution() {
        let path = fixture();
        verify_ordinary_local_executable(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"not executed");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_executable_rejects_user_owned_setid_attributes_without_execution() {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture();
        for mode in [0o4700, 0o2700] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(verify_ordinary_local_executable(&path).is_err());
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"not executed");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_capability_read_failure_is_not_absence() {
        assert!(read_linux_capabilities(-1).is_err());
    }
}
