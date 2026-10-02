//! Bounded replacement of already written service-owned temporary files.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

#[derive(Default)]
pub(super) struct ReplacementBudget {
    #[cfg(windows)]
    waits: usize,
}

pub(super) fn create_temporary(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    }
    options.open(path)
}

pub(super) fn replace(
    source: &Path,
    target: &Path,
    pin: &File,
    budget: &mut ReplacementBudget,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = (pin, budget);
        // A directory sync error occurs after the rename; it must never cause a retry.
        fs::rename(source, target)?;
        File::open(
            target
                .parent()
                .ok_or_else(|| io::Error::other("missing runtime parent"))?,
        )?
        .sync_all()
    }
    #[cfg(windows)]
    {
        replace_with(
            source,
            target,
            pin,
            budget,
            validate_protected,
            native_move,
            std::thread::sleep,
        )
    }
}

#[cfg(windows)]
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "runtime replacement namespace changed",
    )
}

#[cfg(windows)]
fn validate_namespace(source: &Path, target: &Path, pin: &File) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    if !source.is_absolute() || source.parent() != target.parent() || source == target {
        return Err(denied());
    }
    for path in [source, target] {
        for ancestor in path.ancestors().skip(1) {
            let metadata = fs::symlink_metadata(ancestor)?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(denied());
            }
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_file()
                    || metadata.file_attributes()
                        & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_READONLY)
                        != 0
                {
                    return Err(denied());
                }
            }
            Err(error) if path == target && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    // This second handle shares the original writer; the original handle denies
    // other writers for the entire wave and identifies the one prepared file.
    use std::os::windows::fs::OpenOptionsExt;
    let current = OpenOptions::new()
        .read(true)
        .share_mode(1 | 2 | 4)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)?;
    if file_identity(pin)? != file_identity(&current)? {
        return Err(denied());
    }
    Ok(())
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the File owns this valid handle and the output is correctly sized.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

#[cfg(windows)]
fn validate_protected(source: &Path, target: &Path, pin: &File) -> io::Result<()> {
    validate_namespace(source, target, pin)?;
    #[cfg(not(test))]
    {
        crate::platform::validate_protected_path(source, false)?;
        validate_target(
            target,
            |path| fs::symlink_metadata(path),
            crate::platform::validate_protected_path,
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn validate_target(
    target: &Path,
    lookup: impl FnOnce(&Path) -> io::Result<fs::Metadata>,
    protected: impl FnOnce(&Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    match lookup(target) {
        Ok(_) => protected(target, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn target_is_shared(target: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_OPEN_REPARSE_POINT};
    OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(1 | 2 | 4)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(target)
        .is_err_and(|error| error.raw_os_error() == Some(32))
}

#[cfg(windows)]
fn replace_with(
    source: &Path,
    target: &Path,
    pin: &File,
    budget: &mut ReplacementBudget,
    mut validate: impl FnMut(&Path, &Path, &File) -> io::Result<()>,
    mut operation: impl FnMut(&Path, &Path) -> io::Result<()>,
    mut sleep: impl FnMut(std::time::Duration),
) -> io::Result<()> {
    loop {
        // Protection failures stay outside the native error classifier.
        validate(source, target, pin)?;
        match operation(source, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let transient = matches!(error.raw_os_error(), Some(32 | 1224))
                    || (error.raw_os_error() == Some(5) && target_is_shared(target));
                let Some(delay) = [25, 50, 100]
                    .get(budget.waits)
                    .copied()
                    .filter(|_| transient)
                else {
                    return Err(error);
                };
                budget.waits += 1;
                sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
}

#[cfg(windows)]
fn native_move(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<_> = source.as_os_str().encode_wide().chain([0]).collect();
    let target: Vec<_> = target.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both terminated path buffers remain valid for this synchronous call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "runtime_replace_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "runtime_replace_unix_tests.rs"]
mod unix_tests;
