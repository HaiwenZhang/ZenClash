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
        // Private from creation, before the first raw byte is visible.
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
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
