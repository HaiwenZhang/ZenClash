// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Core executable approval, asset source/destination validation, and private owner IPC directory setup.

use crate::ServiceErrorCode;
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::trusted_core_location::{require_trusted_core_location, untrusted};
#[cfg(unix)]
use crate::mihomo_ipc_path;
use std::path::{Component, Path, PathBuf};

pub(in crate::core::runtime_generation) const RUNTIME_CONFIG_FILE_NAME: &str = "config.yaml";
const STAGING_TEMP_INFIX: &str = ".staging-";

/// Pairs the client path used to locate assets with the approved executable path.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedCore {
    pub(super) requested: PathBuf,
    pub(super) executable: PathBuf,
}

impl ResolvedCore {
    /// The only path the service ever spawns.
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    /// Allows bundle assets only from a trusted install directory. Otherwise use owner data.
    /// A writable bundle root could redirect privileged reads to files the owner cannot read.
    pub(super) fn trusted_asset_bundle_root(&self) -> Option<PathBuf> {
        require_trusted_core_location(&self.requested).ok()?;
        application_bundle_root(&self.requested)
    }
}

/// Resolves the requested core to an installer-approved copy safe for privileged execution.
pub(crate) fn validate_core_path(core_path: &str) -> Result<ResolvedCore, ServiceError> {
    let client_path = Path::new(core_path);
    if !client_path.is_absolute() {
        return Err(invalid_asset("core path must be absolute"));
    }
    let source = std::fs::canonicalize(client_path)
        .map_err(|error| invalid_asset(format!("failed to canonicalize core: {error}")))?;
    let requested = canonical_regular_file(&source, "core")?;
    // Integration tests stage cores in temporary directories that no installer has approved.
    if cfg!(feature = "test") {
        return Ok(ResolvedCore {
            executable: requested.clone(),
            requested,
        });
    }
    let executable = approved_core_copy(
        &crate::core::paths::service_paths()
            .map_err(|error| untrusted(error.to_string()))?
            .core_dir(),
        // The installer stages symlinked cores under the original name, not the target name.
        client_path,
    )?;
    // Recheck the approved location in case its permissions changed after installation.
    require_trusted_core_location(&executable)?;
    Ok(ResolvedCore { requested, executable })
}

/// Looks up the approved copy by file name without trusting the client directory.
pub(crate) fn approved_core_copy(core_dir: &Path, requested: &Path) -> Result<PathBuf, ServiceError> {
    let Some(name) = requested.file_name() else {
        return Err(untrusted("core path has no file name"));
    };
    let approved = core_dir.join(name);
    if approved.parent() != Some(core_dir) {
        return Err(untrusted(format!(
            "core name {name:?} does not name a file in {core_dir:?}"
        )));
    }
    // Never execute staging or displaced files left by the installer.
    let bookkeeping = approved.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(crate::core::paths::CORE_STAGING_EXTENSION)
            || extension.eq_ignore_ascii_case(crate::core::paths::CORE_DISPLACED_EXTENSION)
    });
    if bookkeeping {
        return Err(untrusted(format!(
            "core name {name:?} carries an installer bookkeeping suffix and is never runnable"
        )));
    }
    let metadata = std::fs::symlink_metadata(&approved).map_err(|error| {
        untrusted(format!(
            "no administrator-approved copy of {name:?} is installed in {core_dir:?} ({error}); \
             run the elevated service installer with --install-core to stage it"
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(untrusted(format!("approved core {approved:?} is not an ordinary file")));
    }
    warn_if_the_client_core_has_moved_ahead(requested, &approved, &metadata);
    std::fs::canonicalize(&approved)
        .map_err(|error| untrusted(format!("failed to canonicalize approved core {approved:?}: {error}")))
}

/// Warns when the client copy differs from the approved core that will actually run.
/// Uses length and the source timestamp preserved by the installer.
fn warn_if_the_client_core_has_moved_ahead(requested: &Path, approved: &Path, approved_metadata: &std::fs::Metadata) {
    if requested == approved {
        return;
    }
    let Ok(metadata) = std::fs::metadata(requested) else {
        return;
    };
    let modified = |metadata: &std::fs::Metadata| metadata.modified().ok();
    if metadata.len() != approved_metadata.len() || modified(&metadata) != modified(approved_metadata) {
        tracing::warn!(
            "core {requested:?} differs from the approved copy {approved:?}; running the approved copy. \
             Stage the new core with the elevated service installer (--install-core) for it to take effect."
        );
    }
}

pub(super) fn validate_source(
    owner: &AuthenticatedOwner,
    app_bundle_root: Option<&Path>,
    source: &str,
) -> Result<PathBuf, ServiceError> {
    let requested = Path::new(source);
    let canonical = canonical_regular_file(requested, "runtime asset")?;
    if canonical != requested {
        return Err(invalid_asset(
            "runtime asset path contains a symlink or non-canonical component",
        ));
    }
    if !canonical.starts_with(&owner.app_data_root) && !app_bundle_root.is_some_and(|root| canonical.starts_with(root))
    {
        return Err(invalid_asset(
            "runtime asset is outside the authenticated application roots",
        ));
    }
    Ok(canonical)
}

fn canonical_regular_file(path: &Path, label: &str) -> Result<PathBuf, ServiceError> {
    if !path.is_absolute() {
        return Err(invalid_asset(format!("{label} path must be absolute")));
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| invalid_asset(format!("{label} is unavailable: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_asset(format!("{label} must be an ordinary file")));
    }
    std::fs::canonicalize(path).map_err(|error| invalid_asset(format!("failed to canonicalize {label}: {error}")))
}

pub(in crate::core::runtime_generation) fn validate_destination(destination: &str) -> Result<PathBuf, ServiceError> {
    let path = Path::new(destination);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_asset(
            "runtime asset destination must be a non-traversing relative path",
        ));
    }
    Ok(path.to_path_buf())
}

/// Canonicalizes a validated destination and rejects service-owned generation names.
/// Rebuild from path components rather than rewriting separators, which could turn a valid Unix
/// filename containing backslashes into an unvalidated traversal.
pub(in crate::core::runtime_generation) fn destination_key(destination: &Path) -> Result<String, ServiceError> {
    let mut parts = Vec::new();
    for component in destination.components() {
        let Component::Normal(part) = component else {
            return Err(invalid_asset(
                "runtime asset destination must be a non-traversing relative path",
            ));
        };
        let part = part.to_string_lossy();
        if is_staging_temporary(&part) {
            return Err(invalid_asset(format!(
                "runtime asset destination {part:?} is reserved for staging temporaries"
            )));
        }
        if is_windows_alias(&part) {
            return Err(invalid_asset(format!(
                "runtime asset destination {part:?} names another file on Windows filesystems"
            )));
        }
        parts.push(part.into_owned());
    }
    match parts.as_slice() {
        [] => Err(invalid_asset("runtime asset destination is empty")),
        // Treat service-owned names case-insensitively for Windows filesystems.
        [only]
            if only.eq_ignore_ascii_case(RUNTIME_CONFIG_FILE_NAME)
                || only.eq_ignore_ascii_case(crate::core::runtime_generation::staging::MANIFEST_FILE_NAME) =>
        {
            Err(invalid_asset(format!(
                "runtime asset destination {only:?} is owned by the runtime generation"
            )))
        }
        _ => Ok(parts.join("/")),
    }
}

/// Matches staging's temporary-file shape without rejecting ordinary names containing the infix.
fn is_staging_temporary(name: &str) -> bool {
    let Some(tail) = name
        .strip_prefix('.')
        .and_then(|rest| rest.rsplit_once(STAGING_TEMP_INFIX))
        .map(|(_, tail)| tail)
    else {
        return false;
    };
    matches!(tail.split_once('-'), Some((pid, sequence))
        if !pid.is_empty()
            && !sequence.is_empty()
            && pid.bytes().all(|byte| byte.is_ascii_digit())
            && sequence.bytes().all(|byte| byte.is_ascii_digit()))
}

// Reject Win32 filename aliases on all platforms to keep manifest paths unambiguous.
fn is_windows_alias(name: &str) -> bool {
    if name.ends_with(['.', ' ']) || name.contains(':') {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Resolves an untrusted bundle or manifest destination inside `generation`.
pub(in crate::core::runtime_generation) fn resolve_in_generation(
    generation: &Path,
    destination: &str,
) -> Result<PathBuf, ServiceError> {
    let key = destination_key(&validate_destination(destination)?)?;
    Ok(generation.join(key))
}

fn application_bundle_root(core_path: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        core_path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            .map(Path::to_path_buf)
    }

    #[cfg(not(target_os = "macos"))]
    {
        core_path.parent().map(Path::to_path_buf)
    }
}

pub(in crate::core::runtime_generation) fn invalid_asset(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ServiceErrorCode::InvalidRuntimeAsset, message)
}

pub(super) async fn set_private_directory_permissions(path: &Path) -> Result<(), ServiceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| invalid_asset(format!("failed to secure owner directory {path:?}: {error}")))?;
    }

    #[cfg(windows)]
    crate::core::windows_security::secure_private_directory(path)
        .map_err(|error| invalid_asset(format!("failed to secure owner directory {path:?}: {error:#}")))?;

    Ok(())
}

pub(super) async fn prepare_owner_ipc_directory(owner: &AuthenticatedOwner) -> Result<(), ServiceError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let ipc_path = PathBuf::from(mihomo_ipc_path(&owner.identity));
        let directory = ipc_path
            .parent()
            .ok_or_else(|| invalid_asset("owner IPC path has no parent directory"))?;
        let users_directory = directory
            .parent()
            .ok_or_else(|| invalid_asset("owner IPC directory has no users root"))?;
        crate::core::unix_security::ensure_service_directory(users_directory, 0o755)
            .map_err(|error| invalid_asset(format!("failed to secure IPC users directory: {error:#}")))?;
        match std::fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(invalid_asset(format!("failed to create owner IPC directory: {error}")));
            }
        }
        let directory = std::ffi::CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| invalid_asset("owner IPC directory contains NUL"))?;
        // SAFETY: `directory` is a NUL-terminated path and the returned
        // descriptor is owned by this scope.
        let fd = unsafe {
            platform_lib::open(
                directory.as_ptr(),
                platform_lib::O_DIRECTORY | platform_lib::O_NOFOLLOW | platform_lib::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(invalid_asset(format!(
                "failed to open owner IPC directory: {}",
                std::io::Error::last_os_error()
            )));
        }
        let crate::OwnerIdentity::Unix { uid, .. } = owner.identity else {
            // SAFETY: `fd` is the live descriptor opened above.
            unsafe { platform_lib::close(fd) };
            return Err(invalid_asset("Unix IPC directory requires a Unix owner"));
        };
        // SAFETY: `stat` is a plain C struct; all-zero is valid before `fstat`.
        let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
        // SAFETY: `fd` is live and `stat` is writable storage for one `stat`.
        let inspected = unsafe { platform_lib::fstat(fd, &mut stat) } == 0;
        // SAFETY: `geteuid` has no preconditions or memory effects.
        let effective_uid = unsafe { platform_lib::geteuid() };
        let test_process_owned = cfg!(feature = "test") && stat.st_uid == effective_uid;
        if !owner_ipc_directory_is_usable(inspected, stat.st_mode, stat.st_uid, uid, test_process_owned) {
            // SAFETY: `fd` is the live descriptor opened above.
            unsafe { platform_lib::close(fd) };
            return Err(invalid_asset(
                "owner IPC directory has an unexpected owner or file type",
            ));
        }
        // SAFETY: `geteuid` has no preconditions; `fchown` operates on the live fd.
        let chown_ok = unsafe { platform_lib::geteuid() } != 0 || unsafe { platform_lib::fchown(fd, 0, 0) } == 0;
        // SAFETY: `fchmod` operates on the live fd and the mode is valid.
        let chmod_ok = unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } == 0;
        let os_error = (!chown_ok || !chmod_ok).then(std::io::Error::last_os_error);
        // SAFETY: `fd` was not closed on the success path and is owned here.
        unsafe { platform_lib::close(fd) };
        if let Some(error) = os_error {
            return Err(invalid_asset(format!("failed to secure owner IPC directory: {error}")));
        }
    }

    #[cfg(windows)]
    let _ = owner;

    Ok(())
}

/// Allows only root-, owner-, or test-process-owned IPC directories with private group/other mode.
#[cfg(unix)]
pub(super) fn owner_ipc_directory_is_usable(
    inspected: bool,
    mode: platform_lib::mode_t,
    directory_uid: u32,
    owner_uid: u32,
    process_owned: bool,
) -> bool {
    inspected
        && mode & platform_lib::S_IFMT == platform_lib::S_IFDIR
        && (directory_uid == 0 || directory_uid == owner_uid || process_owned)
}
