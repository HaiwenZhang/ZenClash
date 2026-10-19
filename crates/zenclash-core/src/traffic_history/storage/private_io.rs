//! Protect SQLite data before opening it; Windows sidecars need explicit ACLs.

use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn prepare(path: &Path) -> io::Result<()> {
    protect(path, true)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let create = cfg!(windows) && matches!(suffix, "-wal" | "-shm");
        match protect(&PathBuf::from(name), create) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn protect(path: &Path, create: bool) -> io::Result<()> {
    use std::{
        fs::{OpenOptions, Permissions},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "traffic history is not a regular file",
        ));
    }
    file.set_permissions(Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn protect(path: &Path, create: bool) -> io::Result<()> {
    crate::service::protect_current_user_file(path, create)
        .map(drop)
        .map_err(|error| match error.downcast::<io::Error>() {
            Ok(error) => error,
            Err(error) => io::Error::other(error),
        })
}

#[cfg(not(any(unix, windows)))]
fn protect(_path: &Path, _create: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private traffic history is unsupported on this platform",
    ))
}
