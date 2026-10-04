//! Native admission lock shared by business owners and maintenance workers.

use std::{fs, io, path::Path};

pub(crate) struct MaintenanceLock {
    file: fs::File,
    _root: fs::File,
}

impl MaintenanceLock {
    pub(crate) fn acquire(root: &Path) -> io::Result<Self> {
        Self::acquire_with(root, false, &crate::platform::validate_protected_path)
    }

    pub(crate) fn acquire_shared(root: &Path) -> io::Result<Self> {
        Self::acquire_with(root, true, &crate::platform::validate_protected_path)
    }

    pub(crate) fn acquire_with(
        root: &Path,
        shared: bool,
        validate: &impl Fn(&Path, bool) -> io::Result<()>,
    ) -> io::Result<Self> {
        validate(root, true)?;
        let root_pin = open_pin(root, true)?;
        let path = root.join(".maintenance.lock");
        match fs::symlink_metadata(&path) {
            Ok(_) => validate(&path, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            options
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        }
        let file = options.open(&path)?;
        validate_file_kind(&file, false)?;
        validate(&path, false)?;
        verify_path(&root_pin, root, true)?;
        verify_path(&file, &path, false)?;
        lock(&file, shared)?;
        // Check again after acquiring: an inode changed during admission must
        // never publish a guard for a different maintenance namespace.
        validate(root, true)?;
        validate(&path, false)?;
        verify_path(&root_pin, root, true)?;
        verify_path(&file, &path, false)?;
        Ok(Self {
            file,
            _root: root_pin,
        })
    }
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "maintenance lock namespace changed",
    )
}

fn open_pin(path: &Path, directory: bool) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
            );
    }
    let file = options.open(path)?;
    validate_file_kind(&file, directory)?;
    Ok(file)
}

fn validate_file_kind(file: &fs::File, directory: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return Err(denied());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(denied());
        }
    }
    Ok(())
}

fn verify_path(file: &fs::File, path: &Path, directory: bool) -> io::Result<()> {
    let current = open_pin(path, directory)?;
    if file_identity(file)? != file_identity(&current)? {
        return Err(denied());
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(file: &fs::File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &fs::File) -> io::Result<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: File owns the live handle, and information is a correctly sized output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

fn lock(file: &fs::File, shared: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: file owns a live descriptor; the lock is nonblocking.
        let mode = if shared { libc::LOCK_SH } else { libc::LOCK_EX };
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::ERROR_LOCK_VIOLATION,
            Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
            System::IO::OVERLAPPED,
        };
        let mut offset = OVERLAPPED::default();
        let mode = LOCKFILE_FAIL_IMMEDIATELY | if shared { 0 } else { LOCKFILE_EXCLUSIVE_LOCK };
        // SAFETY: valid file, live OVERLAPPED at offset zero and nonblocking lock.
        if unsafe { LockFileEx(file.as_raw_handle().cast(), mode, 0, 1, 0, &mut offset) } == 0 {
            let error = io::Error::last_os_error();
            return Err(
                if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "service maintenance admission is busy",
                    )
                } else {
                    error
                },
            );
        }
    }
    Ok(())
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the lock's owned descriptor remains open during drop.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED};
            let mut offset = OVERLAPPED::default();
            // Keep the lock file: unlinking it would create a second admission inode.
            // SAFETY: owned handle remains live and offset/range match the acquired lock.
            unsafe { UnlockFileEx(self.file.as_raw_handle().cast(), 0, 1, 0, &mut offset) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (crate::installer::OwnedTestRoot, std::path::PathBuf) {
        let fixture = crate::installer::OwnedTestRoot::create().unwrap();
        let root = fixture.path().join("service");
        fixture.create_directory(&root).unwrap();
        (fixture, root)
    }

    struct LockChild(std::process::Child);

    impl Drop for LockChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn maintenance_lock_child_fixture(root: std::path::PathBuf) -> ! {
        let shared = std::env::var_os("ZENCLASH_LOCK_TEST_SHARED").unwrap() == "1";
        // The parent owns this ordinary-user fixture. This tests native locking,
        // not the separate production administrator/path-protection policy.
        let _guard = MaintenanceLock::acquire_with(&root, shared, &|_, _| Ok(())).unwrap();
        fs::write(root.join("ready"), b"locked").unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn maintenance_process_exit_releases_native_lock() {
        if let Some(root) = std::env::var_os("ZENCLASH_LOCK_TEST_ROOT") {
            maintenance_lock_child_fixture(root.into());
        }
        for shared in [true, false] {
            let (fixture, root) = fixture();
            let validate = |path: &Path, directory| fixture.validate(path, directory);
            let mut child = LockChild(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "maintenance_lock::tests::maintenance_process_exit_releases_native_lock",
                        "--nocapture",
                    ])
                    .env("ZENCLASH_LOCK_TEST_ROOT", &root)
                    .env("ZENCLASH_LOCK_TEST_SHARED", if shared { "1" } else { "0" })
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !root.join("ready").try_exists().unwrap() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "lock child exited early"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "lock child did not become ready"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(matches!(
                MaintenanceLock::acquire_with(&root, false, &validate),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
            ));
            let other_owner = MaintenanceLock::acquire_with(&root, true, &validate);
            if shared {
                assert!(other_owner.is_ok());
            } else {
                assert!(
                    matches!(&other_owner, Err(error) if error.kind() == io::ErrorKind::WouldBlock)
                );
            }
            drop(other_owner);
            let path = root.join(".maintenance.lock");
            let identity = file_identity(&open_pin(&path, false).unwrap()).unwrap();
            child.0.kill().unwrap();
            assert!(!child.0.wait().unwrap().success());
            // Windows may finish automatic lock cleanup after process exit.
            // Only a busy lock permits a bounded retry; other failures are fatal.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let worker = loop {
                match MaintenanceLock::acquire_with(&root, false, &validate) {
                    Ok(worker) => break worker,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "lock survived child exit"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("lock acquisition after child exit failed: {error}"),
                }
            };
            assert_eq!(file_identity(&worker.file).unwrap(), identity);
        }
    }

    #[test]
    fn maintenance_shared_owners_coexist_until_last_owner_drops() {
        let (fixture, root) = fixture();
        let validate = |path: &Path, directory| fixture.validate(path, directory);
        let first = MaintenanceLock::acquire_with(&root, true, &validate).unwrap();
        let second = MaintenanceLock::acquire_with(&root, true, &validate).unwrap();
        assert!(MaintenanceLock::acquire_with(&root, false, &validate).is_err());
        drop(first);
        assert!(MaintenanceLock::acquire_with(&root, false, &validate).is_err());
        drop(second);
        assert!(MaintenanceLock::acquire_with(&root, false, &validate).is_ok());
    }

    #[test]
    fn maintenance_exclusive_worker_rejects_other_workers_and_shared_owners() {
        let (fixture, root) = fixture();
        let validate = |path: &Path, directory| fixture.validate(path, directory);
        let guard = MaintenanceLock::acquire_with(&root, false, &validate).unwrap();
        for shared in [false, true] {
            assert!(
                matches!(MaintenanceLock::acquire_with(&root, shared, &validate),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock)
            );
        }
        drop(guard);
        assert!(MaintenanceLock::acquire_with(&root, true, &validate).is_ok());
    }

    #[test]
    fn maintenance_lock_keeps_same_inode_and_existing_contents_after_drop() {
        let (fixture, root) = fixture();
        let path = root.join(".maintenance.lock");
        fs::write(&path, b"preserved").unwrap();
        let validate = |path: &Path, directory| fixture.validate(path, directory);
        let guard = MaintenanceLock::acquire_with(&root, false, &validate).unwrap();
        let original = file_identity(&guard.file).unwrap();
        drop(guard);
        let shared = MaintenanceLock::acquire_with(&root, true, &validate).unwrap();
        assert_eq!(file_identity(&shared.file).unwrap(), original);
        drop(shared);
        assert_eq!(fs::read(path).unwrap(), b"preserved");
    }

    #[tokio::test]
    async fn maintenance_cancelled_guard_owner_releases_native_lock() {
        let (fixture, root) = fixture();
        let validate = |path: &Path, directory| fixture.validate(path, directory);
        let guard = MaintenanceLock::acquire_with(&root, true, &validate).unwrap();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = guard;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        assert!(MaintenanceLock::acquire_with(&root, false, &validate).is_err());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(MaintenanceLock::acquire_with(&root, false, &validate).is_ok());
    }

    #[test]
    fn maintenance_post_lock_validation_failure_releases_native_lock() {
        let (fixture, root) = fixture();
        let calls = std::cell::Cell::new(0);
        let fail_after_lock = |path: &Path, directory| {
            fixture.validate(path, directory)?;
            if directory {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    return Err(denied());
                }
            }
            Ok(())
        };
        assert!(MaintenanceLock::acquire_with(&root, true, &fail_after_lock).is_err());
        assert!(
            MaintenanceLock::acquire_with(&root, false, &|path, directory| fixture
                .validate(path, directory))
            .is_ok()
        );
    }

    #[cfg(windows)]
    #[test]
    fn maintenance_live_guard_prevents_file_or_root_replacement() {
        let (fixture, root) = fixture();
        let guard = MaintenanceLock::acquire_with(&root, true, &|path, directory| {
            fixture.validate(path, directory)
        })
        .unwrap();
        let path = root.join(".maintenance.lock");
        assert!(fs::remove_file(&path).is_err());
        assert!(fs::rename(&path, root.join("replacement")).is_err());
        assert!(fs::rename(&root, fixture.path().join("moved-root")).is_err());
        drop(guard);
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_replaced_inode_during_validation_is_rejected() {
        let (fixture, root) = fixture();
        let replaced = std::cell::Cell::new(false);
        let validate = |path: &Path, directory| {
            fixture.validate(path, directory)?;
            if !directory && !replaced.replace(true) {
                fs::rename(path, root.join("preserved-original"))?;
                fs::write(path, b"replacement")?;
            }
            Ok(())
        };
        assert!(
            matches!(MaintenanceLock::acquire_with(&root, true, &validate),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(
            MaintenanceLock::acquire_with(&root, false, &|path, directory| fixture
                .validate(path, directory))
            .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_symlink_lock_does_not_touch_its_target() {
        let (fixture, root) = fixture();
        let target = root.join("target");
        fs::write(&target, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&target, root.join(".maintenance.lock")).unwrap();
        assert!(
            MaintenanceLock::acquire_with(&root, false, &|path, directory| fixture
                .validate(path, directory))
            .is_err()
        );
        assert_eq!(fs::read(target).unwrap(), b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_fifo_lock_is_rejected_without_waiting_for_a_peer() {
        use std::os::unix::ffi::OsStrExt;
        let (fixture, root) = fixture();
        let path = root.join(".maintenance.lock");
        let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a live NUL-terminated path within the owned fixture.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        assert!(
            MaintenanceLock::acquire_with(&root, false, &|path, directory| fixture
                .validate(path, directory))
            .is_err()
        );
    }
}
