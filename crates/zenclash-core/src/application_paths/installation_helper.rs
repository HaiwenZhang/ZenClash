//! Installed service helper layout; no authorization or installation effects.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use super::ApplicationPlatform;

pub(crate) fn service_installation_helper(executable: &Path) -> io::Result<PathBuf> {
    helper_for_platform(executable, ApplicationPlatform::current())
}

fn helper_for_platform(executable: &Path, platform: ApplicationPlatform) -> io::Result<PathBuf> {
    if !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "application executable path is not absolute",
        ));
    }
    match platform {
        ApplicationPlatform::Windows => {
            let directory = executable.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "application directory missing")
            })?;
            Ok(directory.join("zenclash-service-install.exe"))
        }
        ApplicationPlatform::Macos => {
            let directory = executable.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "application directory missing")
            })?;
            let contents = directory.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "bundle contents missing")
            })?;
            if directory.file_name() != Some(OsStr::new("MacOS"))
                || contents.file_name() != Some(OsStr::new("Contents"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "application service requires its installed bundle",
                ));
            }
            Ok(directory.join("zenclash-service-install"))
        }
        ApplicationPlatform::Unix => {
            Ok(PathBuf::from("/usr/lib/zenclash/zenclash-service-install"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_helper_paths_preserve_platform_layout_contracts() {
        let root = std::env::temp_dir();
        let windows = root.join("installed/zenclash.exe");
        let macos = root.join("ZenClash.app/Contents/MacOS/zenclash");
        assert_eq!(
            helper_for_platform(&windows, ApplicationPlatform::Windows).unwrap(),
            root.join("installed/zenclash-service-install.exe")
        );
        assert_eq!(
            helper_for_platform(&macos, ApplicationPlatform::Macos).unwrap(),
            root.join("ZenClash.app/Contents/MacOS/zenclash-service-install")
        );
        assert_eq!(
            helper_for_platform(&root.join("usr/bin/zenclash"), ApplicationPlatform::Unix).unwrap(),
            Path::new("/usr/lib/zenclash/zenclash-service-install")
        );
    }

    #[test]
    fn relative_executable_is_rejected_before_any_platform_projection() {
        for platform in [
            ApplicationPlatform::Macos,
            ApplicationPlatform::Windows,
            ApplicationPlatform::Unix,
        ] {
            let error = helper_for_platform(Path::new("zenclash"), platform).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                error.to_string(),
                "application executable path is not absolute"
            );
        }
    }

    #[test]
    fn macos_helper_discovery_rejects_uninstalled_layout_without_checking_or_starting_a_helper() {
        let executable = std::env::temp_dir().join("development/zenclash");
        let error = helper_for_platform(&executable, ApplicationPlatform::Macos).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            error.to_string(),
            "application service requires its installed bundle"
        );
    }
}
