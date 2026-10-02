use super::{ReplacementBudget, create_temporary, replace};
use std::{
    fs::{self, File, Permissions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};

struct Directory {
    path: PathBuf,
    permissions: Permissions,
}

impl Directory {
    fn new() -> io::Result<Self> {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let path = std::env::temp_dir().join(format!(
            "zenclash-unix-replacement-{:x}",
            u64::from_le_bytes(random)
        ));
        fs::create_dir(&path)?;
        let permissions = fs::metadata(&path)?.permissions();
        Ok(Self { path, permissions })
    }

    fn restore_permissions(&self) -> io::Result<()> {
        fs::set_permissions(&self.path, self.permissions.clone())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = self
            .restore_permissions()
            .and_then(|()| fs::remove_dir_all(&self.path))
        {
            eprintln!("cannot clean Unix replacement fixture: {error}");
        }
    }
}

#[test]
fn parent_open_failure_after_rename_preserves_new_target_without_retrying() -> io::Result<()> {
    // Root bypasses these discretionary permissions and cannot exercise this failure.
    // SAFETY: geteuid has no pointer arguments or effects.
    let uid = unsafe { libc::geteuid() };
    assert_ne!(
        uid, 0,
        "this real permission test requires an ordinary user"
    );
    let directory = Directory::new()?;
    assert_eq!(fs::metadata(&directory.path)?.uid(), uid);
    let source = directory.path.join("prepared.new");
    let target = directory.path.join("runtime.yaml");
    fs::write(&target, b"previous accepted bytes")?;
    let bytes = b"complete prepared replacement bytes\n";
    let mut pin = create_temporary(&source)?;
    pin.write_all(bytes)?;
    pin.sync_all()?;

    // Rename needs write/search permission; opening the directory also needs read.
    // The guard restores the exact original permissions, including after a panic.
    fs::set_permissions(&directory.path, Permissions::from_mode(0o300))?;
    assert_eq!(
        File::open(&directory.path).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let result = replace(&source, &target, &pin, &mut ReplacementBudget::default());
    directory.restore_permissions()?;

    // A second rename would return NotFound after consuming source. This error is
    // specifically the parent-open failure, not an injected sync_all syscall error.
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        fs::symlink_metadata(&source).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(fs::read(&target)?, bytes);
    Ok(())
}
