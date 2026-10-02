//! Fixed removal entry point for an already authorized package manager.

use std::{ffi::OsString, fs, io};

use crate::{installer, platform};

/// Removes the fixed service through the existing administrator transaction.
///
/// This blocking entry point accepts no paths or caller identity. Package
/// managers invoke it before removing their own helper and registration files.
///
/// # Errors
/// Rejects a caller without administrator privileges or an unprotected source.
/// Reports invalid installation state and native stop, unregister or cleanup
/// failures; the package manager must preserve its files when this fails.
#[cfg(feature = "server")]
pub fn run_package_uninstall() -> io::Result<()> {
    platform::require_admin()?;
    let root = platform::root_directory()?;
    match fs::symlink_metadata(&root) {
        Ok(_) => platform::validate_protected_path(&root, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let source = std::env::current_exe()?;
    platform::validate_protected_path(&source, false)?;
    let pinned = platform::open_pinned_file(&source)?;
    let owner = platform::current_identity()?;
    let arguments = [
        OsString::from("--uninstall"),
        OsString::from("--helper-sha256"),
        OsString::from(installer::artifact_hash(&pinned)?),
        OsString::from("--owner-pid"),
        OsString::from(owner.pid().to_string()),
        OsString::from("--owner-birth"),
        OsString::from(owner.birth().to_string()),
    ];
    installer::run_maintenance(&arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_removal_requires_administrator_before_installation_work() {
        if crate::platform::require_admin().is_ok() {
            eprintln!("permission behavior requires an unelevated test process");
            return;
        }
        let root = crate::platform::root_directory().unwrap();
        let existed = root.exists();
        let error = run_package_uninstall().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(root.exists(), existed);
    }
}
