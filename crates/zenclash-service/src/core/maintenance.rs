// Modified for the ZenClash fork on 2026-10-05; see NOTICE.md. GPL-3.0-only.
use crate::core::desired::ActiveOwnerState;
use crate::core::paths::service_paths;
use crate::{OwnerIdentity, owner_key};
use anyhow::{Context as _, Result};
use std::fs::{File, OpenOptions};

/// Only privileged installation/repair may discard unreadable authorization state.
///
/// # Errors
/// Returns errors checking exclusive stopped-service access or reading and quarantining corrupt owner state.
pub fn repair_active_owner_state() -> Result<Option<std::path::PathBuf>> {
    crate::prepare_service_install_directory()?;
    let _stopped_guard = acquire_stopped_service_guard()?;
    let path = service_paths()?.active_owner_path();
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to inspect active owner state for repair"),
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!("active owner state {path:?} is not an ordinary file");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            anyhow::bail!("active owner state {path:?} is a reparse point");
        }
    }
    let content = std::fs::read(&path).context("failed to read active owner state for repair")?;
    if serde_json::from_slice::<ActiveOwnerState>(&content).is_ok() {
        return Ok(None);
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let backup = path.with_file_name(format!("active-owner.json.corrupt-{}-{timestamp}", std::process::id()));
    std::fs::rename(&path, &backup).context("failed to quarantine corrupt active owner state")?;
    Ok(Some(backup))
}

///
/// # Errors
/// Returns native identity, exclusive-access or filesystem errors during stale-owner cleanup.
pub fn cleanup_stale_owner_state() -> Result<Vec<String>> {
    let _stopped_guard = acquire_stopped_service_guard()?;
    let paths = service_paths()?;
    let users_root = paths.persistent_state_dir().join("users");
    let entries = match std::fs::read_dir(&users_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to enumerate {users_root:?}"));
        }
    };
    let active_owner = read_active_owner().ok().flatten();
    let mut removed = Vec::new();

    for entry in entries {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(identity) = owner_identity_for_directory(&entry.path(), &key)? else {
            continue;
        };
        if identity_exists(&identity)? {
            continue;
        }
        if active_owner.as_ref().is_some_and(|active| active.owner_key == key) {
            match std::fs::remove_file(paths.active_owner_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("failed to clear stale active owner"),
            }
        }
        std::fs::remove_dir_all(entry.path()).with_context(|| format!("failed to remove stale owner state {key}"))?;
        removed.push(key);
    }
    Ok(removed)
}

pub(crate) async fn persist_owner_identity(identity: &OwnerIdentity, owner_root: &std::path::Path) -> Result<()> {
    let path = owner_root.join("owner.json");
    let temporary = owner_root.join("owner.json.tmp");
    tokio::fs::write(&temporary, serde_json::to_vec_pretty(identity)?)
        .await
        .with_context(|| format!("failed to write owner identity {temporary:?}"))?;
    crate::core::atomic_file::replace(&temporary, &path)
        .await
        .with_context(|| format!("failed to activate owner identity {path:?}"))?;
    Ok(())
}

struct StoppedServiceGuard {
    _file: File,
}

fn acquire_stopped_service_guard() -> Result<StoppedServiceGuard> {
    let paths = service_paths()?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;

        crate::core::unix_security::ensure_service_directory(paths.runtime_dir(), 0o755)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(paths.owner_lock_path())?;
        if unsafe { platform_lib::flock(file.as_raw_fd(), platform_lib::LOCK_EX | platform_lib::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("service owner lock is held; stop the service before stale-owner maintenance");
        }
        Ok(StoppedServiceGuard { _file: file })
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::LockFile;

        crate::core::windows_security::ensure_private_installer_directory(paths.runtime_dir())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(paths.owner_lock_path())?;
        if unsafe { LockFile(file.as_raw_handle(), 0, 0, u32::MAX, u32::MAX) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("service owner lock is held; stop the service before stale-owner maintenance");
        }
        Ok(StoppedServiceGuard { _file: file })
    }
}

fn read_active_owner() -> Result<Option<ActiveOwnerState>> {
    let path = service_paths()?.active_owner_path();
    match std::fs::read(&path) {
        Ok(content) => serde_json::from_slice(&content)
            .map(Some)
            .with_context(|| format!("failed to parse {path:?}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {path:?}")),
    }
}

fn owner_identity_for_directory(path: &std::path::Path, key: &str) -> Result<Option<OwnerIdentity>> {
    #[cfg(unix)]
    let identity = match key.parse::<u32>() {
        Ok(uid) => OwnerIdentity::Unix { uid, gid: 0 },
        Err(_) => return Ok(None),
    };

    #[cfg(windows)]
    let identity: OwnerIdentity = match std::fs::read(path.join("owner.json")) {
        Ok(content) => serde_json::from_slice(&content).context("failed to parse Windows owner metadata")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    if owner_key(&identity) != key {
        return Ok(None);
    }
    let _ = path;
    Ok(Some(identity))
}

#[cfg(unix)]
fn identity_exists(identity: &OwnerIdentity) -> Result<bool> {
    use nix::unistd::{Uid, User};
    let OwnerIdentity::Unix { uid, .. } = identity else {
        return Ok(false);
    };
    Ok(User::from_uid(Uid::from_raw(*uid))?.is_some())
}

#[cfg(windows)]
fn identity_exists(identity: &OwnerIdentity) -> Result<bool> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED, GetLastError};
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::LookupAccountSidW;

    let OwnerIdentity::Windows { sid } = identity else {
        return Ok(false);
    };
    let mut wide: Vec<u16> = std::ffi::OsStr::new(sid).encode_wide().collect();
    wide.push(0);
    let mut binary_sid = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut binary_sid) } == 0 || binary_sid.is_null() {
        return Err(std::io::Error::last_os_error()).context("failed to parse owner SID");
    }
    let binary_sid = LocalSid(binary_sid);
    let mut name_length = 0;
    let mut domain_length = 0;
    let mut use_type = 0;
    unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            binary_sid.0,
            std::ptr::null_mut(),
            &mut name_length,
            std::ptr::null_mut(),
            &mut domain_length,
            &mut use_type,
        )
    };
    match unsafe { GetLastError() } {
        ERROR_INSUFFICIENT_BUFFER => Ok(true),
        ERROR_NONE_MAPPED => Ok(false),
        _ => Err(std::io::Error::last_os_error()).context("failed to resolve owner SID"),
    }
}

#[cfg(windows)]
struct LocalSid(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for LocalSid {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}
