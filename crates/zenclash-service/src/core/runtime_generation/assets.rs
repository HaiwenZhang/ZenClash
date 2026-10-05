// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
use crate::core::auth::{AuthenticatedOwner, ServiceError};
use crate::core::paths::ensure_owner_state_directory;
use crate::core::trusted_core_location::{require_trusted_core_location, untrusted};
use crate::{ClashConfig, CoreConfig, RuntimeBundle, ServiceErrorCode, WriterConfig, mihomo_ipc_path};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Stable per-owner directory that preserves core-managed state across restarts.
const RUNTIME_GENERATION_DIRECTORY_NAME: &str = "runtime";

#[cfg(windows)]
const WINDOWS_RUNTIME_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// A planned refresh of the owner's durable runtime generation.
/// Old per-start directories are retired only after the new core starts successfully.
#[derive(Debug)]
pub(crate) struct PreparedRuntime {
    clash_config: ClashConfig,
    runtime: PathBuf,
    stale_runtime_paths: Vec<PathBuf>,
    plan: super::staging::StagePlan,
    yaml: String,
}

impl PreparedRuntime {
    pub(crate) fn clash_config(&self) -> &ClashConfig {
        &self.clash_config
    }

    /// Writes the plan after the outgoing core has stopped.
    /// Keeping this separate from planning avoids touching files still held open by that core.
    pub(crate) async fn materialize(&self) -> Result<(), ServiceError> {
        materialize_plan(&self.runtime, &self.plan, &self.yaml).await
    }

    /// Retires directories left by the old per-start layout after a successful start.
    pub(crate) fn commit(mut self) {
        let stale_paths = std::mem::take(&mut self.stale_runtime_paths);
        if stale_paths.is_empty() {
            return;
        }
        let active_runtime = self.runtime.clone();
        tokio::spawn(async move {
            cleanup_stale_runtime_directories(stale_paths, active_runtime).await;
        });
    }
}

async fn inspect_path(path: &Path) -> String {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => "symlink".to_owned(),
        Ok(metadata) if metadata.is_dir() => "directory".to_owned(),
        Ok(metadata) if metadata.is_file() => "file".to_owned(),
        Ok(_) => "other".to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing".to_owned(),
        Err(error) => format!("inaccessible: {error}"),
    }
}

async fn snapshot_stale_runtime_directories(owner_root: &Path, active_runtime: &Path) -> Vec<PathBuf> {
    let mut stale_paths = Vec::new();
    let mut entries = match tokio::fs::read_dir(owner_root).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                owner_root = ?owner_root,
                error = %error,
                "Failed to enumerate stale runtime directories"
            );
            return stale_paths;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    owner_root = ?owner_root,
                    error = %error,
                    "Failed while enumerating stale runtime directories"
                );
                break;
            }
        };
        let path = entry.path();
        if path == active_runtime {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_runtime_directory = name == "runtime"
            || name == "runtime.backup"
            || name.starts_with("runtime.generation-")
            || name.starts_with("runtime.staging-");
        if !is_runtime_directory {
            continue;
        }
        stale_paths.push(path);
    }
    stale_paths
}

async fn cleanup_stale_runtime_directories(stale_paths: Vec<PathBuf>, active_runtime: PathBuf) {
    for path in stale_paths {
        if let Err(error) = remove_runtime_directory(&path, "failed to remove stale runtime directory").await {
            let state = inspect_path(&path).await;
            tracing::warn!(
                path = ?path,
                state = %state,
                error = %error,
                active_runtime = ?active_runtime,
                "Failed to remove stale runtime directory after committing new generation"
            );
        }
    }
}

async fn remove_runtime_directory(path: &Path, operation: &str) -> std::io::Result<()> {
    let mut retry_index = 0;
    loop {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                let Some(delay) = runtime_cleanup_retry_delay(&error, retry_index) else {
                    return Err(error);
                };
                retry_index += 1;
                tracing::warn!(
                    path = ?path,
                    retry = retry_index,
                    delay_ms = delay.as_millis(),
                    error = %error,
                    operation,
                    "Retrying transient Windows runtime directory cleanup failure"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Returns a bounded retry delay for Windows errors caused by live file handles.
/// Mapped files are included so callers eventually fall back to restarting the core.
#[cfg(windows)]
pub(super) fn runtime_cleanup_retry_delay(error: &std::io::Error, retry_index: usize) -> Option<Duration> {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION,
        ERROR_UNABLE_TO_MOVE_REPLACEMENT, ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, ERROR_UNABLE_TO_REMOVE_REPLACED,
        ERROR_USER_MAPPED_FILE,
    };

    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32
                || code == ERROR_SHARING_VIOLATION as i32
                || code == ERROR_DIR_NOT_EMPTY as i32
                || code == ERROR_DELETE_PENDING as i32
                || code == ERROR_UNABLE_TO_REMOVE_REPLACED as i32
                || code == ERROR_UNABLE_TO_MOVE_REPLACEMENT as i32
                || code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 as i32
                || code == ERROR_USER_MAPPED_FILE as i32
    )
    .then(|| WINDOWS_RUNTIME_RETRY_DELAYS.get(retry_index).copied())
    .flatten()
}

#[cfg(not(windows))]
pub(super) fn runtime_cleanup_retry_delay(_error: &std::io::Error, _retry_index: usize) -> Option<Duration> {
    None
}

/// Ensures the private, durable generation used by both normal starts and desired-state restore.
/// Reuse preserves core-owned state such as `cache.db` selections and fake-IP leases.
async fn ensure_runtime_generation(owner_root: &Path) -> Result<PathBuf, ServiceError> {
    let runtime = owner_root.join(RUNTIME_GENERATION_DIRECTORY_NAME);
    tokio::fs::create_dir_all(&runtime)
        .await
        .map_err(|error| invalid_asset(format!("failed to create runtime generation {runtime:?}: {error}")))?;
    set_private_directory_permissions(&runtime).await?;
    Ok(runtime)
}

pub(crate) async fn prepare_runtime(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
) -> Result<PreparedRuntime, ServiceError> {
    let core = validate_core_path(&bundle.core_path)?;
    let owner_paths = ensure_owner_state_directory(&owner.identity)
        .map_err(|error| invalid_asset(format!("failed to secure owner state root: {error:#}")))?;
    let owner_root = owner_paths.root();
    crate::core::maintenance::persist_owner_identity(&owner.identity, owner_root)
        .await
        .map_err(|error| invalid_asset(format!("failed to persist owner identity: {error:#}")))?;
    prepare_owner_ipc_directory(owner).await?;

    let logs = owner_paths.logs_dir();
    tokio::fs::create_dir_all(&logs)
        .await
        .map_err(|error| invalid_asset(format!("failed to create owner log directory: {error}")))?;
    set_private_directory_permissions(&logs).await?;
    let log_config = WriterConfig {
        directory: logs.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let runtime = ensure_runtime_generation(owner_root).await?;
    let mut prepared = PreparedRuntime {
        clash_config: ClashConfig {
            core_config: CoreConfig {
                core_path: core.executable().to_string_lossy().into_owned(),
                core_ipc_path: mihomo_ipc_path(&owner.identity),
                config_path: runtime.join(RUNTIME_CONFIG_FILE_NAME).to_string_lossy().into_owned(),
                config_dir: runtime.to_string_lossy().into_owned(),
            },
            log_config,
        },
        runtime: runtime.clone(),
        stale_runtime_paths: Vec::new(),
        plan: plan_runtime_refresh(owner, bundle, &core, &runtime).await?,
        yaml: bundle.yaml.clone(),
    };
    prepared.stale_runtime_paths = snapshot_stale_runtime_directories(owner_root, &runtime).await;
    Ok(prepared)
}

/// Plans a runtime refresh without writing; only manifest-recorded files may be deleted.
/// A missing manifest copies all declared assets and preserves unknown core-owned files.
async fn plan_runtime_refresh(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
    core: &ResolvedCore,
    runtime: &Path,
) -> Result<super::staging::StagePlan, ServiceError> {
    let gathered = gather_bundle(owner, bundle, core).await?;
    // A corrupt manifest must not prevent the restart that staging uses as its fallback.
    // Treat it as untrusted: copy everything and sweep nothing.
    let previous = super::staging::read_manifest(runtime).await.unwrap_or_else(|detail| {
        tracing::warn!(
            detail = %detail,
            "Rebuilding the runtime generation from nothing: its manifest could not be read"
        );
        super::staging::RuntimeManifest::default()
    });
    Ok(super::staging::plan_stage(
        &previous,
        &gathered.sources,
        &gathered.remote,
    ))
}

/// Applies a plan, committing `config.yaml` last so partial failure leaves the old configuration.
async fn materialize_plan(runtime: &Path, plan: &super::staging::StagePlan, yaml: &str) -> Result<(), ServiceError> {
    // Do not record a source that changed while it was copied.
    let mut manifest = plan.manifest.clone();
    for destination in &plan.required_deletes {
        let target = resolve_in_generation(runtime, destination)?;
        super::staging::remove_staged_file(&target)
            .await
            .map_err(|error| invalid_asset(format!("failed to discard the stale cache {destination}: {error}")))?;
    }

    for copy in &plan.copies {
        let target = resolve_in_generation(runtime, &copy.destination)?;
        super::staging::copy_staged_file(&copy.source, &target)
            .await
            .map_err(|error| {
                invalid_asset(format!(
                    "failed to write the runtime asset {}: {error}",
                    copy.destination
                ))
            })?;
        // Re-stat after copying. If the source moved, omit its proof so the next staging retries;
        // a concurrent geo-data update should not fail the current start.
        if super::staging::source_identity_changed(&copy.source, &copy.identity).await {
            tracing::warn!(
                destination = %copy.destination,
                "Runtime asset changed while being copied; not recording what it was copied from"
            );
            manifest.assets.remove(&copy.destination);
        }
    }

    let config_path = runtime.join(RUNTIME_CONFIG_FILE_NAME);
    if let Err(error) = super::staging::commit_staged_config(runtime, &config_path, yaml, &manifest).await {
        // A manifest written before a failed config commit may claim cache provenance for a URL
        // the core never loaded; discard it rather than trust stale provider data next time.
        if let Err(discard) =
            super::staging::remove_staged_file(&runtime.join(super::staging::MANIFEST_FILE_NAME)).await
        {
            tracing::warn!(
                error = %discard,
                "Left a manifest behind that describes an uncommitted configuration"
            );
        }
        return Err(invalid_asset(format!(
            "failed to commit the runtime configuration: {error}"
        )));
    }

    // Post-commit cleanup is best-effort because it cannot affect what the core observes.
    for destination in &plan.hygiene_deletes {
        match resolve_in_generation(runtime, destination) {
            Ok(target) => {
                if let Err(error) = super::staging::remove_staged_file(&target).await {
                    tracing::warn!(
                        destination = %destination,
                        error = %error,
                        "Left an undeclared file behind in the runtime generation"
                    );
                }
            }
            Err(error) => tracing::warn!(
                destination = %destination,
                error = %error,
                "Refused to sweep a recorded destination that no longer validates"
            ),
        }
    }

    tracing::info!(
        copied = plan.copies.len(),
        skipped = plan.skipped.len(),
        discarded = plan.required_deletes.len(),
        "Prepared the runtime generation"
    );
    Ok(())
}

/// Validated, stat'd bundle data shared by start and staging plans.
pub(super) struct GatheredBundle {
    pub sources: Vec<super::staging::AssetSource>,
    pub remote: Vec<crate::RemoteProvider>,
}

pub(super) async fn gather_bundle(
    owner: &AuthenticatedOwner,
    bundle: &RuntimeBundle,
    core: &ResolvedCore,
) -> Result<GatheredBundle, ServiceError> {
    let app_bundle_root = core.trusted_asset_bundle_root();
    let mut sources = Vec::with_capacity(bundle.assets.len());
    let mut asset_keys = std::collections::BTreeSet::new();
    for asset in &bundle.assets {
        let source = validate_source(owner, app_bundle_root.as_deref(), &asset.source)?;
        let destination = destination_key(&validate_destination(&asset.destination)?)?;
        let metadata = tokio::fs::metadata(&source)
            .await
            .map_err(|error| invalid_asset(format!("failed to inspect runtime asset {source:?}: {error}")))?;
        if !asset_keys.insert(destination.clone()) {
            return Err(invalid_asset(format!(
                "runtime destination {destination:?} is declared as a copied asset twice"
            )));
        }
        sources.push(super::staging::AssetSource {
            asset: crate::RuntimeAsset {
                source: source.to_string_lossy().into_owned(),
                destination,
            },
            len: metadata.len(),
            mtime_ns: super::staging::modified_nanos(&metadata),
        });
    }
    let remote = super::staging::declared_remote_providers(&bundle.remote_providers, &asset_keys)?;
    Ok(GatheredBundle { sources, remote })
}

/// Pairs the client path used to locate assets with the approved executable path.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedCore {
    requested: PathBuf,
    executable: PathBuf,
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

pub(super) fn validate_destination(destination: &str) -> Result<PathBuf, ServiceError> {
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

pub(super) const RUNTIME_CONFIG_FILE_NAME: &str = "config.yaml";
pub(super) const STAGING_TEMP_INFIX: &str = ".staging-";

/// Canonicalizes a validated destination and rejects service-owned generation names.
/// Rebuild from path components rather than rewriting separators, which could turn a valid Unix
/// filename containing backslashes into an unvalidated traversal.
pub(super) fn destination_key(destination: &Path) -> Result<String, ServiceError> {
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
                || only.eq_ignore_ascii_case(super::staging::MANIFEST_FILE_NAME) =>
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
pub(super) fn resolve_in_generation(generation: &Path, destination: &str) -> Result<PathBuf, ServiceError> {
    let key = destination_key(&validate_destination(destination)?)?;
    Ok(generation.join(key))
}

pub(super) fn application_bundle_root(core_path: &Path) -> Option<PathBuf> {
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

pub(super) fn invalid_asset(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ServiceErrorCode::InvalidRuntimeAsset, message)
}

async fn set_private_directory_permissions(path: &Path) -> Result<(), ServiceError> {
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

async fn prepare_owner_ipc_directory(owner: &AuthenticatedOwner) -> Result<(), ServiceError> {
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
            unsafe { platform_lib::close(fd) };
            return Err(invalid_asset("Unix IPC directory requires a Unix owner"));
        };
        let mut stat = unsafe { std::mem::zeroed::<platform_lib::stat>() };
        let inspected = unsafe { platform_lib::fstat(fd, &mut stat) } == 0;
        let effective_uid = unsafe { platform_lib::geteuid() };
        let test_process_owned = cfg!(feature = "test") && stat.st_uid == effective_uid;
        if !owner_ipc_directory_is_usable(inspected, stat.st_mode, stat.st_uid, uid, test_process_owned) {
            unsafe { platform_lib::close(fd) };
            return Err(invalid_asset(
                "owner IPC directory has an unexpected owner or file type",
            ));
        }
        let chown_ok = unsafe { platform_lib::geteuid() } != 0 || unsafe { platform_lib::fchown(fd, 0, 0) } == 0;
        let chmod_ok = unsafe { platform_lib::fchmod(fd, 0o700 as platform_lib::mode_t) } == 0;
        let os_error = (!chown_ok || !chmod_ok).then(std::io::Error::last_os_error);
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
fn owner_ipc_directory_is_usable(
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

#[cfg(all(test, unix))]
mod owner_ipc_directory_tests {
    use super::owner_ipc_directory_is_usable;

    const DIR: platform_lib::mode_t = platform_lib::S_IFDIR | 0o700;
    const FILE: platform_lib::mode_t = platform_lib::S_IFREG | 0o600;

    #[test]
    fn accepts_a_root_owned_directory() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 0, 501, false));
    }

    #[test]
    fn accepts_a_directory_already_handed_to_the_owner() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 501, 501, false));
    }

    #[test]
    fn rejects_a_directory_belonging_to_somebody_else() {
        assert!(!owner_ipc_directory_is_usable(true, DIR, 502, 501, false));
    }

    #[test]
    fn accepts_somebody_elses_directory_only_when_the_caller_allows_it() {
        assert!(owner_ipc_directory_is_usable(true, DIR, 502, 501, true));
    }

    #[test]
    fn rejects_anything_that_is_not_a_directory() {
        assert!(!owner_ipc_directory_is_usable(true, FILE, 0, 501, false));
        assert!(!owner_ipc_directory_is_usable(true, FILE, 501, 501, true));
    }

    #[test]
    fn rejects_a_directory_it_could_not_inspect() {
        assert!(!owner_ipc_directory_is_usable(false, DIR, 0, 501, false));
        assert!(!owner_ipc_directory_is_usable(false, DIR, 501, 501, true));
    }
}

#[cfg(test)]
mod approved_core_tests {
    use super::approved_core_copy;
    use std::path::{Path, PathBuf};

    fn scratch(label: &str) -> anyhow::Result<PathBuf> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("approved-core-{label}-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn core_name() -> &'static str {
        if cfg!(windows) { "mihomo.exe" } else { "mihomo" }
    }

    #[test]
    fn a_core_named_from_a_writable_install_directory_resolves_to_the_approved_copy() -> anyhow::Result<()> {
        let cores = scratch("writable-install")?;
        let approved = cores.join(core_name());
        std::fs::write(&approved, b"approved core")?;

        let requested = Path::new(if cfg!(windows) {
            r"D:\translated software\clash\mihomo.exe"
        } else {
            "/home/someone/apps/clash/mihomo"
        });

        assert_eq!(
            approved_core_copy(&cores, requested)?,
            std::fs::canonicalize(&approved)?
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn resolving_the_approved_copy_again_yields_the_same_path() -> anyhow::Result<()> {
        let cores = scratch("idempotent")?;
        let approved = cores.join(core_name());
        std::fs::write(&approved, b"approved core")?;
        let canonical = std::fs::canonicalize(&approved)?;

        assert_eq!(approved_core_copy(&cores, &canonical)?, canonical);
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn a_core_no_installer_ever_staged_is_refused() -> anyhow::Result<()> {
        let cores = scratch("unstaged")?;

        let requested = Path::new("/somewhere").join(core_name());
        let error = approved_core_copy(&cores, &requested).expect_err("must refuse");

        assert_eq!(error.code, crate::ServiceErrorCode::InvalidInstallLocation);
        assert!(
            error.message.contains("--install-core"),
            "the error should say how to stage a core, got {:?}",
            error.message
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn installer_bookkeeping_leftovers_are_never_runnable_cores() -> anyhow::Result<()> {
        let cores = scratch("bookkeeping-leftover")?;
        for leftover in ["mihomo.exe.next", "mihomo.next", "mihomo.old"] {
            std::fs::write(cores.join(leftover), b"leftover")?;

            let error = approved_core_copy(&cores, &Path::new("/somewhere").join(leftover)).expect_err("must refuse");

            assert!(
                error.message.contains("bookkeeping suffix"),
                "unexpected message for {leftover}: {:?}",
                error.message
            );
        }
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }

    #[test]
    fn a_directory_standing_in_for_the_approved_core_is_refused() -> anyhow::Result<()> {
        let cores = scratch("directory")?;
        std::fs::create_dir(cores.join(core_name()))?;

        let requested = Path::new("/somewhere").join(core_name());
        let error = approved_core_copy(&cores, &requested).expect_err("must refuse");

        assert!(
            error.message.contains("not an ordinary file"),
            "unexpected message {:?}",
            error.message
        );
        std::fs::remove_dir_all(&cores)?;
        Ok(())
    }
}

// Not under the `test` feature: that feature waives the location rule, which is the thing under test.
#[cfg(all(test, not(feature = "test")))]
mod asset_bundle_root_tests {
    use super::ResolvedCore;

    #[test]
    fn a_writable_install_directory_provides_no_asset_bundle_root() -> anyhow::Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("bundle-root-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let core = root.join("mihomo");
        std::fs::write(&core, b"core")?;
        let requested = std::fs::canonicalize(&core)?;
        let resolved = ResolvedCore {
            executable: requested.clone(),
            requested,
        };

        assert_eq!(resolved.trusted_asset_bundle_root(), None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}

#[cfg(test)]
mod runtime_gc_tests {
    use super::{cleanup_stale_runtime_directories, snapshot_stale_runtime_directories};

    #[tokio::test]
    async fn stale_snapshot_never_collects_a_later_generation() -> anyhow::Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let owner_root = std::env::temp_dir().join(format!(
            "service-runtime-gc-snapshot-{}-{timestamp}",
            std::process::id()
        ));
        let active = owner_root.join("runtime.generation-active");
        let stale = owner_root.join("runtime.generation-stale");
        tokio::fs::create_dir_all(&active).await?;
        tokio::fs::create_dir(&stale).await?;

        let stale_paths = snapshot_stale_runtime_directories(&owner_root, &active).await;
        let later = owner_root.join("runtime.generation-later");
        tokio::fs::create_dir(&later).await?;
        cleanup_stale_runtime_directories(stale_paths, active.clone()).await;

        tokio::fs::symlink_metadata(&active).await?;
        tokio::fs::symlink_metadata(&later).await?;
        let stale_error = tokio::fs::symlink_metadata(&stale)
            .await
            .expect_err("snapshotted stale generation must be removed");
        assert_eq!(stale_error.kind(), std::io::ErrorKind::NotFound);

        let next_stale_paths = snapshot_stale_runtime_directories(&owner_root, &later).await;
        cleanup_stale_runtime_directories(next_stale_paths, later.clone()).await;
        tokio::fs::symlink_metadata(&later).await?;
        let previous_active_error = tokio::fs::symlink_metadata(&active)
            .await
            .expect_err("the next snapshot must collect the previous active generation");
        assert_eq!(previous_active_error.kind(), std::io::ErrorKind::NotFound);
        tokio::fs::remove_dir_all(owner_root).await?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{PreparedRuntime, prepare_runtime};
    use crate::core::auth::{AuthenticatedOwner, ServiceError};
    use crate::{OwnerIdentity, RuntimeAsset, RuntimeBundle, ServiceErrorCode};
    use serial_test::serial;
    use std::path::PathBuf;

    /// Runs the two production phases together when no core is active in tests.
    async fn prepare_and_materialize(
        owner: &AuthenticatedOwner,
        bundle: &RuntimeBundle,
    ) -> Result<PreparedRuntime, ServiceError> {
        let prepared = prepare_runtime(owner, bundle).await?;
        prepared.materialize().await?;
        Ok(prepared)
    }

    fn test_owner(app_data_root: std::path::PathBuf) -> AuthenticatedOwner {
        // Unit tests do not run the installer or IPC server that normally creates this parent.
        if let Some(root) = std::path::Path::new(crate::IPC_PATH).parent() {
            std::fs::create_dir_all(root).expect("test IPC root must be creatable");
        }

        let uid = unsafe { platform_lib::geteuid() };
        let gid = unsafe { platform_lib::getegid() };
        AuthenticatedOwner {
            key: uid.to_string(),
            identity: OwnerIdentity::Unix { uid, gid },
            app_data_root,
        }
    }

    #[tokio::test]
    #[serial]
    async fn materializes_yaml_and_assets_below_owner_runtime() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-assets-{}", std::process::id()));
        std::fs::create_dir_all(app_root.join("providers"))?;
        std::fs::write(app_root.join("providers/source.yaml"), b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner
                    .app_data_root
                    .join("providers/source.yaml")
                    .to_string_lossy()
                    .into_owned(),
                destination: "providers/copied.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let prepared = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        assert_eq!(
            std::fs::read(
                std::path::Path::new(&prepared.clash_config.core_config.config_dir).join("providers/copied.yaml")
            )?,
            b"proxies: []\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn prepared_assets_survive_legacy_source_cleanup() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-cleanup-order-{}", std::process::id()));
        let source = app_root.join("legacy-provider.yaml");
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(&source, b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let canonical_source = owner.app_data_root.join("legacy-provider.yaml");
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: canonical_source.to_string_lossy().into_owned(),
                destination: "providers/copied.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let prepared = prepare_and_materialize(&owner, &bundle).await?;
        std::fs::remove_file(source)?;

        assert_eq!(
            std::fs::read(
                std::path::Path::new(&prepared.clash_config.core_config.config_dir).join("providers/copied.yaml")
            )?,
            b"proxies: []\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn rejects_traversal_without_replacing_existing_runtime() -> anyhow::Result<()> {
        let app_root = std::env::temp_dir().join(format!("service-runtime-traversal-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"safe")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let valid = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let prepared = prepare_and_materialize(&owner, &valid).await?;
        let invalid = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "../escape".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: valid.core_path,
        };

        let error = prepare_runtime(&owner, &invalid)
            .await
            .expect_err("traversal must fail");

        assert_eq!(error.code, ServiceErrorCode::InvalidRuntimeAsset);
        // A rejected bundle must leave the durable generation startable.
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    fn core_owned_state(prepared: &PreparedRuntime) -> PathBuf {
        PathBuf::from(&prepared.clash_config.core_config.config_dir).join("cache.db")
    }

    #[tokio::test]
    #[serial]
    async fn a_restart_keeps_the_state_the_core_wrote_for_itself() -> anyhow::Result<()> {
        // `cache.db` contains selections and must survive service-driven restarts.
        let app_root = std::env::temp_dir().join(format!("service-runtime-cachedb-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let first = prepare_and_materialize(&owner, &bundle).await?;
        std::fs::write(core_owned_state(&first), b"the node the user picked")?;

        let second = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            second.clash_config.core_config.config_dir, first.clash_config.core_config.config_dir,
            "an owner has one generation, so a restart is started against the same directory"
        );
        assert_eq!(
            std::fs::read(core_owned_state(&second))?,
            b"the node the user picked",
            "a restart must not take the core's own state away from it"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_rejected_bundle_keeps_the_state_the_core_wrote_for_itself() -> anyhow::Result<()> {
        // Rejecting a bundle must not delete core-owned state from the durable generation.
        let app_root = std::env::temp_dir().join(format!("service-runtime-cachedb-reject-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"safe")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let valid = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let prepared = prepare_and_materialize(&owner, &valid).await?;
        std::fs::write(core_owned_state(&prepared), b"the node the user picked")?;

        let invalid = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "../escape".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: valid.core_path,
        };
        prepare_runtime(&owner, &invalid)
            .await
            .expect_err("traversal must fail");

        assert_eq!(std::fs::read(core_owned_state(&prepared))?, b"the node the user picked");
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn planning_a_start_writes_nothing_into_the_generation() -> anyhow::Result<()> {
        // Planning runs while the outgoing core may still hold generation files open.
        let app_root = std::env::temp_dir().join(format!("service-runtime-planonly-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("asset"), b"new asset")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let core_path = app_root.join("mihomo").to_string_lossy().into_owned();
        let running = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: core_path.clone(),
        };
        let prepared = prepare_and_materialize(&owner, &running).await?;

        let candidate = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("asset").to_string_lossy().into_owned(),
                destination: "providers/new.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path,
        };
        let planned = prepare_runtime(&owner, &candidate).await?;

        let generation = PathBuf::from(&prepared.clash_config.core_config.config_dir);
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "planning must leave the running core's configuration alone"
        );
        assert!(
            !generation.join("providers/new.yaml").exists(),
            "planning must not copy assets into a directory a core is running in"
        );

        planned.materialize().await?;

        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: global\n"
        );
        assert!(generation.join("providers/new.yaml").exists());
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn planning_a_start_does_not_replace_an_asset_the_running_core_holds() -> anyhow::Result<()> {
        // Existing destinations may be memory-mapped by the outgoing core on Windows.
        let app_root = std::env::temp_dir().join(format!("service-runtime-planonly-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("provider.yaml"), b"first\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let asset = owner.app_data_root.join("provider.yaml").to_string_lossy().into_owned();
        let running = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: asset.clone(),
                destination: "providers/one.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let live = prepare_and_materialize(&owner, &running).await?;
        let generation = PathBuf::from(&live.clash_config.core_config.config_dir);

        std::fs::write(app_root.join("provider.yaml"), b"second\n")?;
        let next = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            ..running
        };
        let planned = prepare_runtime(&owner, &next).await?;

        assert_eq!(
            std::fs::read_to_string(&live.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "planning must not replace the configuration the running core is using"
        );
        assert_eq!(
            std::fs::read(generation.join("providers/one.yaml"))?,
            b"first\n",
            "planning must not replace an asset the running core has open"
        );

        planned.materialize().await?;

        assert_eq!(
            std::fs::read_to_string(&live.clash_config.core_config.config_path)?,
            "mode: global\n"
        );
        assert_eq!(std::fs::read(generation.join("providers/one.yaml"))?, b"second\n");
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn an_unreadable_manifest_rebuilds_rather_than_refusing_to_start() -> anyhow::Result<()> {
        // Restart is staging's fallback, so corrupt bookkeeping cannot also block start.
        let app_root = std::env::temp_dir().join(format!("service-runtime-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("provider.yaml"), b"proxies: []\n")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("provider.yaml").to_string_lossy().into_owned(),
                destination: "providers/one.yaml".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };
        let first = prepare_and_materialize(&owner, &bundle).await?;
        let generation = PathBuf::from(&first.clash_config.core_config.config_dir);
        std::fs::write(
            generation.join(super::super::staging::MANIFEST_FILE_NAME),
            b"{ this is not json",
        )?;
        std::fs::write(core_owned_state(&first), b"the node the user picked")?;
        std::fs::remove_file(generation.join("providers/one.yaml"))?;

        let second = prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read(generation.join("providers/one.yaml"))?,
            b"proxies: []\n",
            "proving nothing means copying everything, not giving up"
        );
        assert_eq!(
            std::fs::read_to_string(&second.clash_config.core_config.config_path)?,
            "mode: rule\n"
        );
        assert_eq!(
            std::fs::read(core_owned_state(&second))?,
            b"the node the user picked",
            "rebuilding is not a reason to take the core's own state away"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_destination_the_manifest_omits_is_copied_again() -> anyhow::Result<()> {
        // Simulate the manifest omission produced when a source changes during its copy.
        let app_root = std::env::temp_dir().join(format!("service-runtime-omitted-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("geo.dat"), b"geo bytes")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let bundle = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![RuntimeAsset {
                source: owner.app_data_root.join("geo.dat").to_string_lossy().into_owned(),
                destination: "geo.dat".to_string(),
            }],
            remote_providers: Vec::new(),
            core_path: app_root.join("mihomo").to_string_lossy().into_owned(),
        };

        let first = prepare_and_materialize(&owner, &bundle).await?;
        let generation = PathBuf::from(&first.clash_config.core_config.config_dir);
        let manifest_path = generation.join(super::super::staging::MANIFEST_FILE_NAME);
        let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        assert!(
            recorded["assets"].get("geo.dat").is_some(),
            "a copy that went through cleanly is recorded"
        );

        // The copy exists but has no trusted source record.
        std::fs::write(&manifest_path, br#"{"assets":{},"remote_providers":{}}"#)?;
        std::fs::remove_file(generation.join("geo.dat"))?;

        prepare_and_materialize(&owner, &bundle).await?;

        assert_eq!(
            std::fs::read(generation.join("geo.dat"))?,
            b"geo bytes",
            "a destination the manifest omits must be copied again, not skipped"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn a_bundle_this_service_will_not_accept_changes_nothing() -> anyhow::Result<()> {
        // Validation must finish before any generation file changes.
        let app_root = std::env::temp_dir().join(format!("service-runtime-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&app_root)?;
        std::fs::write(app_root.join("first"), b"first asset")?;
        std::fs::write(app_root.join("mihomo"), b"mock core")?;
        let owner = test_owner(std::fs::canonicalize(&app_root)?);
        let core_path = app_root.join("mihomo").to_string_lossy().into_owned();
        let good = RuntimeBundle {
            yaml: "mode: rule\n".to_string(),
            assets: vec![],
            remote_providers: Vec::new(),
            core_path: core_path.clone(),
        };
        let prepared = prepare_and_materialize(&owner, &good).await?;

        let asset_source = owner.app_data_root.join("first");
        let half_bad = RuntimeBundle {
            yaml: "mode: global\n".to_string(),
            assets: vec![
                RuntimeAsset {
                    source: asset_source.to_string_lossy().into_owned(),
                    destination: "providers/first.yaml".to_string(),
                },
                RuntimeAsset {
                    source: asset_source.to_string_lossy().into_owned(),
                    destination: "../escape".to_string(),
                },
            ],
            remote_providers: Vec::new(),
            core_path,
        };
        prepare_runtime(&owner, &half_bad)
            .await
            .expect_err("the second asset must be rejected");

        let generation = PathBuf::from(&prepared.clash_config.core_config.config_dir);
        assert_eq!(
            std::fs::read_to_string(&prepared.clash_config.core_config.config_path)?,
            "mode: rule\n",
            "the configuration must still be the one a core could be started against"
        );
        assert!(
            !generation.join("providers/first.yaml").exists(),
            "the accepted asset must not have been written before the bundle was rejected"
        );
        std::fs::remove_dir_all(app_root)?;
        Ok(())
    }
}
