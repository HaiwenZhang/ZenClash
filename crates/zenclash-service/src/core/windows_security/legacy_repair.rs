use super::{OwnedHandle, installer_owner_is_trusted, wide_path};
use anyhow::{Context as _, Result, ensure};
use std::os::windows::{ffi::OsStrExt as _, fs::MetadataExt as _};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TYPE_DISK, FileRenameInfo, GetFileInformationByHandle, GetFileType, OPEN_EXISTING, READ_CONTROL,
    SetFileInformationByHandle,
};

pub(in crate::core) struct LegacyDirectoryBackup<G> {
    pub path: PathBuf,
    _reservation: G,
}

pub(in crate::core) fn recover<G>(
    root: &Path,
    reserve_idle_service: impl FnOnce() -> Result<G>,
) -> Result<Option<LegacyDirectoryBackup<G>>> {
    let wide = wide_path(root)?;
    // Denying delete sharing pins this directory and excludes a second recovery before the repair gate exists.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32)
        {
            return Ok(None);
        }
        return Err(error).context("could not reserve legacy service directory for recovery");
    }
    let directory = OwnedHandle(raw);
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    ensure!(
        unsafe { GetFileInformationByHandle(directory.0, &mut information) } != 0
            && information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
            && information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && unsafe { GetFileType(directory.0) } == FILE_TYPE_DISK,
        "legacy service path {root:?} is not an ordinary directory"
    );
    if installer_owner_is_trusted(directory.0)? {
        return Ok(None);
    }
    let mut found_state = false;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = std::fs::symlink_metadata(entry.path())?;
        ensure!(
            (name == "desired-state.json" || name == "desired-state.json.legacy.bak")
                && metadata.is_file()
                && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "legacy service directory contains an unrecognized entry {:?}; leaving it unchanged",
            entry.path()
        );
        found_state = true;
    }
    ensure!(
        found_state,
        "legacy service directory changed during recovery; retry installation"
    );
    let reservation = reserve_idle_service()?;
    let mut backup_name = root
        .file_name()
        .context("legacy service directory has no name")?
        .to_os_string();
    backup_name.push(format!(
        ".legacy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let backup = root.with_file_name(backup_name);
    rename_directory(&directory, &backup)?;
    Ok(Some(LegacyDirectoryBackup {
        path: backup,
        _reservation: reservation,
    }))
}

fn rename_directory(directory: &OwnedHandle, backup: &Path) -> Result<()> {
    let name: Vec<u16> = backup.as_os_str().encode_wide().collect();
    let bytes = std::mem::size_of::<FILE_RENAME_INFO>() + std::mem::size_of_val(name.as_slice());
    let mut buffer = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).FileNameLength = u32::try_from(std::mem::size_of_val(name.as_slice()))?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast(),
            name.len(),
        );
        // Zeroed ReplaceIfExists prevents overwriting a previous backup.
        if SetFileInformationByHandle(directory.0, FileRenameInfo, info.cast(), u32::try_from(bytes)?) == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("could not preserve legacy service directory at {backup:?}"));
        }
    }
    Ok(())
}
