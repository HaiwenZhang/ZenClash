// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::core::structure::{OwnerIdentity, owner_key};
use std::path::{Path, PathBuf};

#[cfg(all(windows, not(feature = "test")))]
mod windows;

/// Staging-file extension; core resolution rejects it so partial copies cannot run.
pub const CORE_STAGING_EXTENSION: &str = "next";

/// Extension for displaced Windows executables awaiting cleanup; core resolution rejects it.
pub const CORE_DISPLACED_EXTENSION: &str = "old";

#[derive(Debug, Clone)]
pub struct ServicePaths {
    runtime_dir: PathBuf,
    persistent_state_dir: PathBuf,
    ipc_path: PathBuf,
    owner_lock_path: PathBuf,
    pid_file_path: PathBuf,
    core_runtime_path: PathBuf,
    desired_state_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct OwnerPaths {
    root: PathBuf,
}

impl ServicePaths {
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn persistent_state_dir(&self) -> &Path {
        &self.persistent_state_dir
    }

    pub fn ipc_path(&self) -> &Path {
        &self.ipc_path
    }

    pub fn owner_lock_path(&self) -> &Path {
        &self.owner_lock_path
    }

    pub fn pid_file_path(&self) -> &Path {
        &self.pid_file_path
    }

    pub fn core_runtime_path(&self) -> &Path {
        &self.core_runtime_path
    }

    pub fn desired_state_path(&self) -> &Path {
        &self.desired_state_path
    }

    pub fn install_dir(&self) -> PathBuf {
        self.persistent_state_dir.join("bin")
    }

    /// Approved cores published only by the privileged installer.
    /// Files with [`CORE_STAGING_EXTENSION`] or [`CORE_DISPLACED_EXTENSION`] cannot run.
    pub fn core_dir(&self) -> PathBuf {
        self.persistent_state_dir.join("cores")
    }

    pub fn active_owner_path(&self) -> PathBuf {
        self.persistent_state_dir.join("active-owner.json")
    }

    pub fn owner_generation_path(&self) -> PathBuf {
        self.persistent_state_dir.join("owner-generation.json")
    }

    pub fn for_owner(&self, identity: &OwnerIdentity) -> OwnerPaths {
        self.for_owner_key(&owner_key(identity))
    }

    pub fn for_owner_key(&self, owner_key: &str) -> OwnerPaths {
        OwnerPaths {
            root: self.persistent_state_dir.join("users").join(owner_key),
        }
    }
}

impl OwnerPaths {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn desired_state_path(&self) -> PathBuf {
        self.root.join("desired-state.json")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
}

///
/// # Errors
/// Returns native path-resolution errors; this query creates no directories.
pub fn service_paths() -> std::io::Result<ServicePaths> {
    let persistent_state_dir = persistent_state_dir()?;
    #[cfg(unix)]
    let runtime_dir = runtime_dir();
    #[cfg(windows)]
    let runtime_dir = persistent_state_dir.join("runtime");
    Ok(ServicePaths {
        desired_state_path: persistent_state_dir.join("desired-state.json"),
        persistent_state_dir,
        ipc_path: PathBuf::from(crate::IPC_PATH),
        owner_lock_path: runtime_dir.join(format!("{}.owner.lock", crate::SERVICE_SLUG)),
        pid_file_path: runtime_dir.join(format!("{}.pid", crate::SERVICE_SLUG)),
        core_runtime_path: runtime_dir.join(format!("{}.core.json", crate::SERVICE_SLUG)),
        runtime_dir,
    })
}

#[cfg(feature = "standalone")]
pub(crate) fn ensure_persistent_state_layout() -> anyhow::Result<()> {
    let paths = service_paths()?;
    let root = paths.persistent_state_dir();
    use crate::core::platform_security;

    platform_security::ensure_private_service_directory(root)?;

    let users = root.join("users");
    let install = paths.install_dir();
    platform_security::ensure_private_service_directory(&users)?;
    platform_security::ensure_private_service_directory(&install)?;
    platform_security::secure_private_service_file_if_exists(&paths.active_owner_path())?;
    platform_security::secure_private_service_file_if_exists(&paths.owner_generation_path())?;
    Ok(())
}

#[cfg(feature = "standalone")]
///
/// # Errors
/// Returns errors creating or securing the protected service directory.
pub fn prepare_service_install_directory() -> anyhow::Result<PathBuf> {
    let paths = service_paths()?;
    let root = paths.persistent_state_dir();
    let install = paths.install_dir();
    use crate::core::platform_security;

    platform_security::ensure_private_installer_directory(root)?;
    platform_security::ensure_private_installer_directory(&install)?;
    Ok(install)
}

/// Prepares the directory the privileged installer stages approved cores into.
///
/// Deliberately not part of `ensure_persistent_state_layout`: the service runs as root or
/// LocalSystem and would claim ownership, while the installer runs as an elevated administrator.
/// Letting both create it would flip the owner back and forth on every start.
#[cfg(feature = "standalone")]
///
/// # Errors
/// Returns errors creating or securing the protected core directory.
pub fn prepare_core_install_directory() -> anyhow::Result<PathBuf> {
    let paths = service_paths()?;
    let root = paths.persistent_state_dir();
    let cores = paths.core_dir();
    use crate::core::platform_security;

    platform_security::ensure_private_installer_directory(root)?;
    platform_security::ensure_private_installer_directory(&cores)?;
    Ok(cores)
}

#[cfg(feature = "standalone")]
pub(crate) fn ensure_owner_state_directory(identity: &OwnerIdentity) -> anyhow::Result<OwnerPaths> {
    ensure_persistent_state_layout()?;
    let owner = service_paths()?.for_owner(identity);
    crate::core::platform_security::ensure_private_service_directory(owner.root())?;
    Ok(owner)
}

#[cfg(unix)]
fn runtime_dir() -> PathBuf {
    Path::new(crate::IPC_PATH)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/run/zenclash-service"))
}

fn persistent_state_dir() -> std::io::Result<PathBuf> {
    #[cfg(feature = "test")]
    {
        Ok(std::env::temp_dir().join("zenclash-service-ipc-test-state"))
    }

    // A root launchd daemon needs stable system state independent of unreliable HOME/XDG values
    // (issue #7333).
    #[cfg(all(target_os = "macos", not(feature = "test")))]
    {
        Ok(PathBuf::from("/Library/Application Support").join(crate::SERVICE_SLUG))
    }

    #[cfg(all(unix, not(target_os = "macos"), not(feature = "test")))]
    {
        Ok(PathBuf::from("/var/lib").join(crate::SERVICE_SLUG))
    }

    #[cfg(all(windows, not(feature = "test")))]
    {
        windows::persistent_state_dir()
    }
}

#[cfg(unix)]
pub(crate) fn unix_mihomo_ipc_path(runtime_root: &Path, uid: u32) -> PathBuf {
    runtime_root.join("users").join(uid.to_string()).join("mihomo.sock")
}

pub fn mihomo_ipc_path(identity: &OwnerIdentity) -> String {
    match identity {
        OwnerIdentity::Unix { uid: _uid, .. } => {
            #[cfg(windows)]
            {
                let channel_id = if cfg!(feature = "test") {
                    "test"
                } else {
                    crate::CHANNEL_IDENTITY.id
                };
                format!(r"\\.\pipe\zenclash-mihomo-{}-{}", channel_id, owner_key(identity))
            }

            #[cfg(unix)]
            {
                #[cfg(feature = "test")]
                let runtime_root = PathBuf::from("/tmp/zenclash-service-ipc-test");
                #[cfg(not(feature = "test"))]
                let runtime_root = runtime_dir();

                unix_mihomo_ipc_path(&runtime_root, *_uid)
                    .to_string_lossy()
                    .into_owned()
            }
        }
        OwnerIdentity::Windows { .. } => {
            let channel_id = if cfg!(feature = "test") {
                "test"
            } else {
                crate::CHANNEL_IDENTITY.id
            };
            format!(r"\\.\pipe\zenclash-mihomo-{}-{}", channel_id, owner_key(identity))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::service_paths;
    #[cfg(unix)]
    use super::unix_mihomo_ipc_path;
    use crate::OwnerIdentity;
    #[cfg(unix)]
    use std::path::Path;

    #[cfg(unix)]
    #[test]
    fn unix_mihomo_ipc_path_is_owner_scoped_and_below_sun_path_limit() {
        let path = unix_mihomo_ipc_path(Path::new("/var/run/zenclash-service"), 501);

        assert_eq!(path, Path::new("/var/run/zenclash-service/users/501/mihomo.sock"));
        assert!(path.as_os_str().as_encoded_bytes().len() < 104);
    }

    #[test]
    fn owner_paths_isolate_state_runtime_and_logs() -> std::io::Result<()> {
        let paths = service_paths()?;
        let owner = paths.for_owner(&OwnerIdentity::Unix { uid: 501, gid: 20 });

        assert!(owner.root().ends_with("users/501"));
        assert_eq!(owner.desired_state_path(), owner.root().join("desired-state.json"));
        assert_eq!(owner.runtime_dir(), owner.root().join("runtime"));
        assert_eq!(owner.logs_dir(), owner.root().join("logs"));
        assert_eq!(
            paths.active_owner_path(),
            paths.persistent_state_dir().join("active-owner.json")
        );
        Ok(())
    }

    #[cfg(all(windows, feature = "test"))]
    #[test]
    fn windows_test_mihomo_pipe_does_not_use_a_production_namespace() {
        let path = super::mihomo_ipc_path(&OwnerIdentity::Windows {
            sid: "S-1-5-21-1-2-3-1001".to_owned(),
        });

        assert!(path.starts_with(r"\\.\pipe\zenclash-mihomo-test-"));
        assert!(!path.contains("zenclash-mihomo-production"));
    }
}
