#![deny(clippy::undocumented_unsafe_blocks)]

//! Low-level bounded regular-file reads, private atomic writes, and per-user file protection.

use std::{
    fmt, fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(windows)]
pub(crate) mod windows_private;

/// Defensive maximum size in bytes accepted for Clash/Mihomo profile and override files.
pub const MAX_PROFILE_BYTES: usize = 16 * 1024 * 1024;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Failure returned by bounded regular-file reads.
#[derive(Debug)]
pub enum BoundedReadError {
    /// Filesystem open/read error or target was not a regular file.
    Io(std::io::Error),
    /// The file exceeded the configured byte budget.
    TooLarge {
        /// Maximum permitted size in bytes.
        limit_bytes: usize,
    },
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::TooLarge { limit_bytes } => {
                write!(
                    formatter,
                    "配置文件超过 {} MiB 限制",
                    limit_bytes / 1024 / 1024
                )
            }
        }
    }
}

impl std::error::Error for BoundedReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TooLarge { .. } => None,
        }
    }
}

impl From<std::io::Error> for BoundedReadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Opens a regular file without blocking indefinitely on Unix FIFOs or special device nodes.
///
/// # Errors
///
/// Returns an I/O error if the path cannot be opened or does not refer to a regular file.
pub(crate) fn open_regular_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never block forever when opening a FIFO or special device node.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target path is not a regular file",
        ));
    }
    Ok(file)
}

/// Reads at most `max_bytes` from a regular file.
///
/// # Errors
///
/// Returns [`BoundedReadError::Io`] on open/read failure or non-regular files, and
/// [`BoundedReadError::TooLarge`] when the file exceeds `max_bytes`.
pub fn read_bounded_regular_file(
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, BoundedReadError> {
    let file = open_regular_file(path)?;
    read_file_bounded(file, max_bytes)
}

/// Reads a profile or override file bounded by [`MAX_PROFILE_BYTES`].
///
/// In production builds this rejects non-regular files (such as FIFOs or device nodes).
/// Under `#[cfg(test)]`, single-open barrier tests intentionally supply a Unix FIFO.
///
/// # Errors
///
/// Returns [`BoundedReadError::Io`] on filesystem failure or [`BoundedReadError::TooLarge`]
/// when the file exceeds [`MAX_PROFILE_BYTES`].
pub fn read_bounded_profile_bytes(path: &Path) -> Result<Vec<u8>, BoundedReadError> {
    // In production, refuse to block indefinitely on FIFOs or special device nodes.
    // Under #[cfg(test)], single-open barrier tests intentionally supply a Unix FIFO.
    #[cfg(not(test))]
    let file = open_regular_file(path)?;
    #[cfg(test)]
    let file = fs::File::open(path)?;
    read_file_bounded(file, MAX_PROFILE_BYTES)
}

fn read_file_bounded(file: fs::File, max_bytes: usize) -> Result<Vec<u8>, BoundedReadError> {
    let mut payload = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut payload)?;
    if payload.len() > max_bytes {
        return Err(BoundedReadError::TooLarge {
            limit_bytes: max_bytes,
        });
    }
    Ok(payload)
}

/// Atomically writes `bytes` to `path` via a sibling private temporary file.
///
/// Creates missing parent directories, writes and `sync_all`s the temporary file
/// with `0o600` permissions on Unix, and atomically replaces `path`.
///
/// # Errors
///
/// Returns any filesystem error encountered while creating, writing, syncing,
/// restricting permissions on, or replacing the target file.
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
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Create secret-bearing temporary files privately before the first byte is written.
    let mut file = options.open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written
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
    use std::{iter, os::windows::ffi::OsStrExt, ptr};

    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
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
    if succeeded != 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
        return Err(error);
    }
    // MoveFileExW can reject an open destination even with delete sharing.
    // ReplaceFileW preserves existing readers while publishing the new file.
    // SAFETY: The UTF-16 paths remain valid; optional backup and reserved pointers are null.
    if unsafe {
        ReplaceFileW(
            path.as_ptr(),
            temporary.as_ptr(),
            ptr::null(),
            0,
            ptr::null(),
            ptr::null(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Ensures `path` is owned by and restricted to the current Windows user (plus SYSTEM/Administrators).
#[cfg(windows)]
pub(crate) fn protect_current_user_file(
    path: &Path,
    create: bool,
) -> anyhow::Result<std::fs::File> {
    windows_private::protect_file(path, create)
}

/// Creates a new Windows private file restricted to the current user (plus SYSTEM/Administrators).
#[cfg(windows)]
pub(crate) fn create_private_current_user_file(path: &Path) -> anyhow::Result<std::fs::File> {
    windows_private::create_private_file(path)
}

/// Opens an existing Windows private file after validating its owner, type, and DACL.
#[cfg(windows)]
pub(crate) fn open_private_current_user_file(path: &Path) -> anyhow::Result<std::fs::File> {
    windows_private::open_private_file(path)
}

/// Opens or creates a Windows private file without weakening an existing file's ACL.
#[cfg(windows)]
pub(crate) fn open_or_create_private_current_user_file(
    path: &Path,
) -> anyhow::Result<std::fs::File> {
    windows_private::open_or_create_private_file(path)
}
