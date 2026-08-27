use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use super::{MAX_PROFILE_BYTES, MAX_PROFILE_INDEX_BYTES, ProfileStoreError, ProfileStoreResult};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn read_profile_bytes(path: &Path) -> ProfileStoreResult<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut payload = Vec::new();
    file.take(MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_PROFILE_BYTES {
        return Err(ProfileStoreError::InvalidYaml(format!(
            "配置文件超过 {} MiB 限制",
            MAX_PROFILE_BYTES / 1024 / 1024
        )));
    }
    Ok(payload)
}

pub(super) fn read_index_bytes(path: &Path) -> ProfileStoreResult<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut payload = Vec::new();
    file.take(MAX_PROFILE_INDEX_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_PROFILE_INDEX_BYTES {
        return Err(ProfileStoreError::IndexTooLarge {
            limit_mib: MAX_PROFILE_INDEX_BYTES / 1024 / 1024,
        });
    }
    Ok(payload)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("data");
    let temporary = path.with_extension(format!(
        "{extension}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = fs::write(&temporary, bytes)
        .and_then(|()| restrict_private_file(&temporary))
        .and_then(|()| replace_file(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(unix)]
fn restrict_private_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_private_file(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)
}

#[cfg(target_os = "windows")]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both vectors are stable, NUL-terminated UTF-16 paths for the
    // duration of the call. The temporary file is in the destination folder.
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            path.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn home_dir() -> ProfileStoreResult<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| ProfileStoreError::InvalidYaml("无法确定用户主目录".into()))
}
