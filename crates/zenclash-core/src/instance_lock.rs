//! Process-lifetime lock preventing concurrent ZenClash owners.

use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

use thiserror::Error;

/// Failure while acquiring the process-lifetime application lock.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AppInstanceLockError {
    /// Another process already owns the same application-data directory.
    #[error("ZenClash 已在运行（实例锁：{}）", .0.display())]
    AlreadyRunning(PathBuf),
    /// The lock directory or file could not be opened or locked.
    #[error("ZenClash 实例锁 I/O 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// Operating-system lock held for the lifetime of one ZenClash process.
pub struct AppInstanceLock {
    _file: File,
}

impl AppInstanceLock {
    /// Opens and exclusively locks `path` without waiting.
    ///
    /// The file may remain after a crash, but the kernel lock never does. A
    /// later process can therefore safely reuse a stale on-disk lock file.
    ///
    /// # Errors
    ///
    /// Returns [`AppInstanceLockError::AlreadyRunning`] when another process
    /// owns the lock, or an I/O error for directory/file/lock failures.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self, AppInstanceLockError> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        try_lock(&file).map_err(|error| {
            if is_lock_contention(&error) {
                AppInstanceLockError::AlreadyRunning(path)
            } else {
                AppInstanceLockError::Io(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` owns a valid descriptor for the full duration of this
    // call. `flock` does not retain the Rust reference and closing the file
    // releases the process-lifetime lock, including after an abnormal exit.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn is_lock_contention(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
    )
}

#[cfg(windows)]
fn try_lock(file: &File) -> std::io::Result<()> {
    use std::{mem::zeroed, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY},
        System::IO::OVERLAPPED,
    };

    // SAFETY: the handle belongs to `file`, the zeroed OVERLAPPED value denotes
    // offset zero, and the pointer is valid for the duration of LockFileEx.
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    let locked = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if locked != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn is_lock_contention(error: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

    matches!(
        error.raw_os_error(),
        Some(code) if code == ERROR_LOCK_VIOLATION as i32 || code == ERROR_SHARING_VIOLATION as i32
    )
}

#[cfg(not(any(unix, windows)))]
fn try_lock(_file: &File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前平台不支持 ZenClash 实例锁",
    ))
}

#[cfg(not(any(unix, windows)))]
fn is_lock_contention(_error: &std::io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "zenclash-instance-lock-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn a_second_owner_is_refused_and_a_later_owner_can_recover() {
        let path = test_path();
        let first = AppInstanceLock::acquire(&path).unwrap();

        assert!(matches!(
            AppInstanceLock::acquire(&path),
            Err(AppInstanceLockError::AlreadyRunning(locked)) if locked == path
        ));

        drop(first);
        let recovered = AppInstanceLock::acquire(&path).unwrap();
        drop(recovered);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn creates_a_missing_private_parent_directory() {
        let root = test_path();
        let path = root.join("nested/instance.lock");

        let lock = AppInstanceLock::acquire(&path).unwrap();

        assert!(path.is_file());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
