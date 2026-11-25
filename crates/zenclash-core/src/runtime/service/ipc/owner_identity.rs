// Forked from Clash Verge Rev; adapted for ZenClash on 2026-10-04.
// GPL-3.0-only; original authors and changes are recorded in NOTICE.md.
use anyhow::{Context as _, Result};
use std::path::Path;
use zenclash_service::{OwnerCredentials, OwnerIdentity};

#[allow(clippy::unnecessary_wraps)] // Windows SID discovery is fallible; Unix keeps the shared API.
pub(crate) fn current_owner_identity() -> Result<OwnerIdentity> {
    #[cfg(unix)]
    return Ok(OwnerIdentity::Unix {
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
    });

    #[cfg(windows)]
    {
        windows_current_identity()
    }
}

pub(crate) fn current_owner_credentials_for_root(app_root: &Path) -> Result<OwnerCredentials> {
    let app_data_root = std::fs::canonicalize(app_root)
        .with_context(|| format!("failed to canonicalize application data root {app_root:?}"))?;

    #[cfg(unix)]
    let (identity, token) = (current_owner_identity()?, None);

    #[cfg(windows)]
    let (identity, token) = windows_owner_credentials(&app_data_root)?;

    Ok(OwnerCredentials {
        identity,
        app_data_dir: app_data_root.to_string_lossy().into_owned(),
        token,
    })
}

#[cfg(windows)]
fn windows_owner_credentials(app_data_root: &Path) -> Result<(OwnerIdentity, Option<String>)> {
    let sid = windows_owner::current_sid()?;
    let token = windows_owner::load_or_create_token(app_data_root, &sid)?;
    Ok((OwnerIdentity::Windows { sid }, Some(token)))
}

#[cfg(windows)]
fn windows_current_identity() -> Result<OwnerIdentity> {
    Ok(OwnerIdentity::Windows {
        sid: windows_owner::current_sid()?,
    })
}

#[cfg(windows)]
pub(crate) fn create_private_current_user_file(path: &Path) -> Result<std::fs::File> {
    crate::platform::fs_io::create_private_current_user_file(path)
}

#[cfg(windows)]
pub(crate) fn open_private_current_user_file(path: &Path) -> Result<std::fs::File> {
    crate::platform::fs_io::open_private_current_user_file(path)
}

#[cfg(windows)]
pub(crate) fn open_or_create_private_current_user_file(path: &Path) -> Result<std::fs::File> {
    crate::platform::fs_io::open_or_create_private_current_user_file(path)
}

/// Repairs an application data root Windows no longer reports as owned by the current user.
///
/// The Service authenticates by comparing this directory's owner SID against the caller's, so a
/// root left behind by another account locks Service mode out for good.
#[cfg(windows)]
pub(crate) fn repair_app_data_root_owner(app_data_root: &Path) -> Result<()> {
    windows_owner::repair_root_owner(app_data_root)
}

#[cfg(windows)]
pub(crate) fn current_user_pipe_sddl() -> Result<String> {
    let sid = windows_owner::current_sid()?;
    Ok(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)"))
}

#[cfg(windows)]
mod windows_owner {
    use anyhow::{Context as _, Result, bail};
    use std::io::{Read as _, Write as _};
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::Path;
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE,
        GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{EqualSid, OWNER_SECURITY_INFORMATION, PSID};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
        GetFileInformationByHandle, GetFileType, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    };
    use zenclash_service::OWNER_TOKEN_FILE_NAME;

    pub(super) use crate::platform::fs_io::windows_private::current_sid;
    use crate::platform::fs_io::windows_private::{
        LocalSecurityDescriptor, is_access_denied, is_os_error, open_no_reparse, owner_matches,
        take_directory_ownership, wide_path,
    };

    const TOKEN_BYTES: usize = 32;
    /// Reserved files that carry their own owner check and are recreated on demand.
    const REGENERATED_FILES: [&str; 2] = [OWNER_TOKEN_FILE_NAME, "instance.lock"];

    pub(super) fn load_or_create_token(app_data_root: &Path, sid: &str) -> Result<String> {
        let token_path = app_data_root.join(OWNER_TOKEN_FILE_NAME);
        let descriptor = LocalSecurityDescriptor::from_sid(sid)?;
        let attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: descriptor.as_raw(),
            bInheritHandle: 0,
        };
        let wide = wide_path(&token_path)?;
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };

        if handle != INVALID_HANDLE_VALUE {
            let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
            let mut token = [0_u8; TOKEN_BYTES];
            getrandom::fill(&mut token).context("failed to generate owner token")?;
            file.write_all(&token)
                .context("failed to write owner token")?;
            file.sync_all().context("failed to flush owner token")?;
            return Ok(encode_token(&token));
        }
        if unsafe { GetLastError() } != ERROR_FILE_EXISTS {
            return Err(std::io::Error::last_os_error()).context("failed to create owner token");
        }

        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("failed to open owner token");
        }
        let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
        validate_token_file(&file, descriptor.owner()?)?;
        descriptor.apply_dacl(file.as_raw_handle())?;

        let mut token = [0_u8; TOKEN_BYTES];
        file.read_exact(&mut token)
            .context("failed to read owner token")?;
        Ok(encode_token(&token))
    }

    fn validate_token_file(file: &std::fs::File, expected_owner: PSID) -> Result<()> {
        let handle = file.as_raw_handle();
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to inspect owner token metadata");
        }
        if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
            || unsafe { GetFileType(handle) } != FILE_TYPE_DISK
            || information.nFileSizeHigh != 0
            || information.nFileSizeLow != TOKEN_BYTES as u32
        {
            bail!("owner token is not an ordinary 32-byte file");
        }

        let mut owner = std::ptr::null_mut();
        let mut security = std::ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut security,
            )
        };
        if status != 0 || security.is_null() {
            bail!("failed to inspect owner token security: Windows error {status}");
        }
        let security = LocalSecurityDescriptor(security);
        if owner.is_null() || unsafe { EqualSid(owner, expected_owner) } == 0 {
            bail!("owner token belongs to a different Windows user");
        }
        drop(security);
        Ok(())
    }

    pub(super) fn repair_root_owner(root: &Path) -> Result<()> {
        // Elevated Windows processes can create directories owned by Administrators.
        std::fs::create_dir_all(root)
            .context("failed to create the application data root before owner repair")?;
        // The Service canonicalizes before checking, so resolve junctions to the same object.
        let root = std::fs::canonicalize(root)
            .context("failed to canonicalize the application data root")?;
        let root = root.as_path();

        let sid = current_sid()?;
        let descriptor = LocalSecurityDescriptor::from_sid(&sid)?;
        let expected = descriptor.owner()?;

        let directory = open_no_reparse(root, true, READ_CONTROL)?;
        let owned = owner_matches(&directory, expected)?;
        drop(directory);

        if !owned {
            tracing::warn!("应用数据目录 {root:?} 的所有者不是当前用户，尝试接管");
            match take_directory_ownership(root, expected) {
                Ok(()) => tracing::info!("已接管应用数据目录所有权"),
                // Only a refused takeover is worth moving the user's data over.
                Err(error) if is_access_denied(&error) => {
                    tracing::warn!("接管所有权被拒绝，改为重建目录: {error:#}");
                    rebuild_root(root, expected)?;
                }
                Err(error) => return Err(error),
            }
        }

        // The Service and the singleton check compare these files' owner too.
        drop_files_owned_by_others(root, expected)
    }

    /// Drops the reserved files an earlier account left behind, so they are recreated for this one.
    fn drop_files_owned_by_others(root: &Path, expected: PSID) -> Result<()> {
        for name in REGENERATED_FILES {
            let path = root.join(name);
            let ours = match open_no_reparse(&path, false, READ_CONTROL) {
                Ok(file) => {
                    let owned = owner_matches(&file, expected)?;
                    drop(file);
                    owned
                }
                Err(error)
                    if is_os_error(&error, ERROR_FILE_NOT_FOUND)
                        || is_os_error(&error, ERROR_PATH_NOT_FOUND) =>
                {
                    continue;
                }
                // A DACL that refuses even the owner read settles it just as well.
                Err(_) => false,
            };
            if !ours {
                std::fs::remove_file(&path)
                    .with_context(|| format!("failed to drop the stale {name}"))?;
                tracing::info!("已丢弃属于其它账户的 {name}");
            }
        }
        Ok(())
    }

    /// Rebuilds the root as a directory the current user owns, carrying the contents over.
    ///
    /// Nothing checks the children's owner, so they move as they are. Any failure puts the data
    /// back: a half-moved root would read as a fresh install and be filled with defaults.
    fn rebuild_root(root: &Path, expected: PSID) -> Result<()> {
        let parent = root
            .parent()
            .context("application data root has no parent")?;
        let mut stale = root
            .file_name()
            .context("application data root has no name")?
            .to_os_string();
        stale.push(format!(".stale-{}", std::process::id()));
        let stale = parent.join(stale);

        std::fs::rename(root, &stale).context("failed to set the application data root aside")?;
        if let Err(error) = std::fs::create_dir(root) {
            // Something else took the name; it is not ours to walk into, so restore blind.
            let _ = std::fs::rename(&stale, root);
            return Err(error).context("failed to recreate the application data root");
        }
        if let Err(error) = repopulate_root(root, &stale, expected) {
            // The root is the directory just created, so draining it follows no one else's link.
            restore_root(root, &stale)?;
            return Err(error);
        }

        // Only the reserved files remain behind.
        if let Err(error) = std::fs::remove_dir_all(&stale) {
            tracing::warn!("旧数据目录 {stale:?} 未能清理: {error}");
        }
        tracing::info!("已重建应用数据目录");
        Ok(())
    }

    /// Fills the freshly created root from the one set aside, leaving the reserved files behind.
    fn repopulate_root(root: &Path, stale: &Path, expected: PSID) -> Result<()> {
        // An elevated process creates directories owned by Administrators — the state being repaired.
        let directory = open_no_reparse(root, true, READ_CONTROL)?;
        let owned = owner_matches(&directory, expected)?;
        drop(directory);
        if !owned {
            take_directory_ownership(root, expected)?;
        }

        for entry in
            std::fs::read_dir(stale).context("failed to read the stale application data root")?
        {
            let name = entry
                .context("failed to read a stale application data root entry")?
                .file_name();
            if REGENERATED_FILES
                .iter()
                .any(|regenerated| name == *regenerated)
            {
                continue;
            }
            std::fs::rename(stale.join(&name), root.join(&name))
                .with_context(|| format!("failed to move {name:?} into the recreated root"))?;
        }
        Ok(())
    }

    /// Drains a failed rebuild back into the set-aside root and restores it under the real name.
    ///
    /// Failure must be reported: data split across both names reads as a fresh install.
    fn restore_root(root: &Path, stale: &Path) -> Result<()> {
        for entry in std::fs::read_dir(root).context("failed to read the partially rebuilt root")? {
            let name = entry
                .context("failed to read a partially rebuilt root entry")?
                .file_name();
            std::fs::rename(root.join(&name), stale.join(&name))
                .with_context(|| format!("failed to put {name:?} back"))?;
        }
        std::fs::remove_dir(root).context("failed to remove the partially rebuilt root")?;
        std::fs::rename(stale, root).context("failed to restore the application data root")
    }

    fn encode_token(token: &[u8; TOKEN_BYTES]) -> String {
        let mut encoded = String::with_capacity(TOKEN_BYTES * 2);
        for byte in token {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::current_owner_credentials_for_root;
    use zenclash_service::OwnerIdentity;

    #[cfg(unix)]
    #[test]
    fn unix_credentials_come_from_process_and_have_no_token() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir();

        let credentials = current_owner_credentials_for_root(&app_root)?;

        assert_eq!(
            credentials.identity,
            OwnerIdentity::Unix {
                uid: unsafe { libc::geteuid() },
                gid: unsafe { libc::getegid() },
            }
        );
        assert_eq!(credentials.token, None);
        assert_eq!(
            std::path::PathBuf::from(credentials.app_data_dir),
            std::fs::canonicalize(app_root)?
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_credentials_use_stable_sid_and_private_token() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("zenclash-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&app_root);
        std::fs::create_dir(&app_root)?;

        let first = current_owner_credentials_for_root(&app_root)?;
        let second = current_owner_credentials_for_root(&app_root)?;

        let OwnerIdentity::Windows { sid } = &first.identity else {
            anyhow::bail!("expected Windows owner identity");
        };
        assert!(sid.starts_with("S-1-5-"));
        assert_eq!(first.identity, second.identity);
        assert_eq!(first.token, second.token);
        assert_eq!(first.token.as_deref().map(str::len), Some(64));

        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn private_file_open_rejects_directories_and_reparse_points() -> anyhow::Result<()> {
        use super::{create_private_current_user_file, open_private_current_user_file};

        let root = std::env::temp_dir().join(format!("zenclash-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root)?;

        let private_file = root.join("record.json");
        drop(create_private_current_user_file(&private_file)?);
        drop(open_private_current_user_file(&private_file)?);

        let directory = root.join("directory");
        std::fs::create_dir(&directory)?;
        assert!(open_private_current_user_file(&directory).is_err());

        let link = root.join("record-link.json");
        if let Err(error) = std::os::windows::fs::symlink_file(&private_file, &link) {
            if error.raw_os_error() == Some(1314) {
                std::fs::remove_dir_all(root)?;
                return Ok(());
            }
            return Err(error.into());
        }
        assert!(open_private_current_user_file(&link).is_err());

        std::fs::remove_file(link)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
