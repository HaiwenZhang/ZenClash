//! Private raw-log creation and replacement, shared by persistence and export.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub(super) fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Private from creation, never follow symlinks, and never block on FIFOs.
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log file target is not a regular file",
        ));
    }
    restrict_open_file(&file)?;
    file.write_all(bytes)
}

#[cfg(unix)]
fn restrict_open_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Apply to the opened object rather than resolving the path again.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_open_file(_: &File) -> io::Result<()> {
    Ok(())
}

pub(super) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::profiles::atomic_write(path, bytes)
}

#[cfg(test)]
mod tests;
