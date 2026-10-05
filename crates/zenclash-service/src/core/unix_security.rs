use anyhow::{Context as _, Result, bail};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

/// Uses the service-directory privacy rule, which also satisfies Unix installer requirements.
pub(crate) fn ensure_private_installer_directory(path: &Path) -> Result<()> {
    ensure_private_service_directory(path)
}

pub(crate) fn ensure_private_service_directory(path: &Path) -> Result<()> {
    ensure_service_directory(path, 0o700)
}

pub(crate) fn ensure_service_directory(path: &Path, mode: platform_lib::mode_t) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("failed to create {path:?}")),
    }

    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("service directory path contains NUL"))?;
    let fd = unsafe {
        platform_lib::open(
            path_c.as_ptr(),
            platform_lib::O_RDONLY | platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to open service directory {path:?}"));
    }
    let result = secure_open_directory(fd, path, mode);
    unsafe { platform_lib::close(fd) };
    result
}

pub(crate) fn secure_private_service_file_if_exists(path: &Path) -> Result<()> {
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("service file path contains NUL"))?;
    let fd = unsafe {
        platform_lib::open(
            path_c.as_ptr(),
            platform_lib::O_RDWR | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error).with_context(|| format!("failed to open service file {path:?}"));
    }
    let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
    let expected_uid = if unsafe { platform_lib::geteuid() } == 0 {
        0
    } else {
        unsafe { platform_lib::geteuid() }
    };
    let valid = unsafe { platform_lib::fstat(fd, &mut stat) } == 0
        && stat.st_uid == expected_uid
        && stat.st_mode & platform_lib::S_IFMT == platform_lib::S_IFREG;
    if !valid {
        unsafe { platform_lib::close(fd) };
        bail!("service file {path:?} has an unexpected owner or file type");
    }
    let chown_ok = expected_uid != 0 || unsafe { platform_lib::fchown(fd, 0, 0) } == 0;
    let chmod_ok = unsafe { platform_lib::fchmod(fd, 0o600 as platform_lib::mode_t) } == 0;
    let error = (!chown_ok || !chmod_ok).then(std::io::Error::last_os_error);
    unsafe { platform_lib::close(fd) };
    if let Some(error) = error {
        return Err(error).with_context(|| format!("failed to secure service file {path:?}"));
    }
    Ok(())
}

fn secure_open_directory(fd: std::os::fd::RawFd, path: &Path, mode: platform_lib::mode_t) -> Result<()> {
    let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
    if unsafe { platform_lib::fstat(fd, &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to inspect service directory {path:?}"));
    }
    let expected_uid = if unsafe { platform_lib::geteuid() } == 0 {
        0
    } else {
        unsafe { platform_lib::geteuid() }
    };
    if stat.st_uid != expected_uid || stat.st_mode & platform_lib::S_IFMT != platform_lib::S_IFDIR {
        bail!("service directory {path:?} has an unexpected owner or file type");
    }
    if expected_uid == 0 && unsafe { platform_lib::fchown(fd, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to set root ownership on {path:?}"));
    }
    if unsafe { platform_lib::fchmod(fd, mode) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to secure service directory {path:?}"));
    }
    Ok(())
}
