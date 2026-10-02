//! Administrator-authorized installation transactions for fixed service paths.

use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::Path,
    time::Duration,
};

use sha2::{Digest, Sha256};
#[cfg(feature = "server")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "server")]
use crate::InstalledMetadata;
use crate::{ServiceClient, platform};

const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

/// Fixed service operation accepted by the privileged installer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceAction {
    /// Install approved helper and kernel bytes and authorize this user.
    Install,
    /// Replace an existing installation with approved artifacts.
    Repair,
    /// Stop the managed kernel, unregister, and remove service private data.
    Uninstall,
    /// Start a registered service without changing approved artifacts.
    Start,
}

impl MaintenanceAction {
    fn name(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Repair => "repair",
            Self::Uninstall => "uninstall",
            Self::Start => "start",
        }
    }
}

/// Failure of a fixed native maintenance request, without installer arguments.
#[derive(Clone, Debug, thiserror::Error)]
pub enum MaintenanceError {
    /// Source verification failed before requesting administrator authorization.
    #[error("service maintenance preparation failed")]
    BeforeRequest(#[source] Arc<io::Error>),
    /// The operating system explicitly reported cancellation of authorization.
    #[error("service authorization cancelled")]
    AuthorizationCancelled,
    /// The authorized helper exited unsuccessfully.
    #[error("authorized service maintenance failed")]
    Failed(#[source] Arc<io::Error>),
    /// Completion has not been established; this operation must not be retried.
    #[error("service maintenance outcome is unconfirmed")]
    OutcomeUnconfirmed {
        /// Bounded diagnostic that contains no private installer parameters.
        #[source]
        source: Arc<io::Error>,
        /// Retained native completion, when its process is still being observed.
        pending: Option<MaintenancePending>,
    },
}

/// Retained completion of one already submitted native maintenance operation.
///
/// Dropping a waiter neither kills the authorized helper nor starts a kernel.
/// The native worker keeps its process and artifact pins until actual exit.
#[derive(Clone, Debug)]
pub struct MaintenancePending {
    completion: tokio::sync::watch::Receiver<Option<Result<(), MaintenanceError>>>,
}

impl MaintenancePending {
    /// Waits for actual native exit and verified service readiness.
    ///
    /// No timeout or retry is applied. Application shutdown must discard its
    /// TUN intent independently rather than waiting for an unanswered prompt.
    ///
    /// # Errors
    /// Returns the native maintenance failure, or an unconfirmed outcome if
    /// the retained native observer ends without establishing completion.
    pub async fn wait(&self) -> Result<(), MaintenanceError> {
        let mut completion = self.completion.clone();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                return Err(MaintenanceError::OutcomeUnconfirmed {
                    source: Arc::new(io::Error::other("native maintenance observer ended")),
                    pending: None,
                });
            }
        }
    }
}

fn authorization_result(error: io::Error) -> MaintenanceError {
    if error
        .get_ref()
        .is_some_and(|cause| cause.is::<AuthorizationCancelled>())
    {
        return MaintenanceError::AuthorizationCancelled;
    }
    match error.kind() {
        io::ErrorKind::TimedOut => MaintenanceError::OutcomeUnconfirmed {
            source: Arc::new(error),
            pending: None,
        },
        _ => MaintenanceError::Failed(Arc::new(error)),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("authorization explicitly cancelled by the operating system")]
struct AuthorizationCancelled;

pub(crate) fn authorization_cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, AuthorizationCancelled)
}

/// Prepared installation health; reading this value performs no IPC or I/O.
#[derive(Clone, Debug)]
pub struct ServiceHealth {
    kind: ServiceHealthKind,
    installation: Option<crate::InstalledMetadata>,
}

/// Trustworthy facts available to an application service workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceHealthKind {
    /// The fixed installation root is absent and no verified service is reachable.
    Missing,
    /// Both approved artifacts and a compatible native service were verified.
    Ready,
    /// Approved artifacts exist, but the native endpoint is absent or refused.
    Stopped,
    /// Known approved metadata exists, but an artifact is missing or damaged.
    RepairRequired,
    /// A protected maintenance journal requires completion or administrator recovery.
    MaintenancePending,
    /// The actual user is not approved by the installation or verified service.
    Unauthorized,
    /// The installed metadata or verified handshake uses a different wire protocol.
    Incompatible,
    /// Existing installation state cannot be approved; it must not be adopted as missing.
    UnrecognizedInstallation,
    /// Access, native identity, or current readiness could not be established.
    Unknown,
}

impl ServiceHealth {
    /// Reads the prepared health classification.
    #[must_use]
    pub const fn kind(&self) -> ServiceHealthKind {
        self.kind
    }

    /// Reads protected metadata whose schema was verified by the background worker.
    #[must_use]
    pub fn installation(&self) -> Option<&crate::InstalledMetadata> {
        self.installation.as_ref()
    }

    fn with_kind(mut self, kind: ServiceHealthKind) -> Self {
        self.kind = kind;
        self
    }
}

/// Observes fixed installation artifacts and verified IPC without acquiring a lease.
///
/// Artifact reads are bounded and run off the application thread. This is a health
/// observation, not evidence that a still-running maintenance worker has finished.
pub async fn service_health() -> ServiceHealth {
    let disk = match tokio::task::spawn_blocking(|| {
        platform::root_directory().map(|root| installation_health(&root))
    })
    .await
    {
        Ok(Ok(health)) => health,
        _ => ServiceHealth {
            kind: ServiceHealthKind::Unknown,
            installation: None,
        },
    };
    if !matches!(
        disk.kind,
        ServiceHealthKind::Missing | ServiceHealthKind::Stopped
    ) {
        return disk;
    }
    let probe = ServiceClient::probe().await;
    match probe {
        Ok(_) if disk.installation.is_some() => disk.with_kind(ServiceHealthKind::Ready),
        Ok(_) => disk.with_kind(ServiceHealthKind::Unknown),
        Err(crate::ServiceClientError::Rejected(crate::ServiceErrorCode::Incompatible)) => {
            disk.with_kind(ServiceHealthKind::Incompatible)
        }
        Err(crate::ServiceClientError::Rejected(crate::ServiceErrorCode::Unauthorized)) => {
            disk.with_kind(ServiceHealthKind::Unauthorized)
        }
        Err(crate::ServiceClientError::Rejected(crate::ServiceErrorCode::MaintenancePending)) => {
            disk.with_kind(ServiceHealthKind::MaintenancePending)
        }
        Err(crate::ServiceClientError::Connection(error))
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            disk
        }
        Err(_) => disk.with_kind(ServiceHealthKind::Unknown),
    }
}

fn installation_health(root: &Path) -> ServiceHealth {
    let mut health = ServiceHealth {
        kind: ServiceHealthKind::Unknown,
        installation: None,
    };
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return health.with_kind(ServiceHealthKind::Missing);
        }
        Err(_) => return health,
        Ok(_) => {}
    }
    if platform::validate_protected_path(root, true).is_err() {
        return health.with_kind(ServiceHealthKind::UnrecognizedInstallation);
    }
    match fs::symlink_metadata(root.join("maintenance.json")) {
        Ok(_) => return health.with_kind(ServiceHealthKind::MaintenancePending),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return health,
    }
    let record = root.join("install.json");
    if platform::validate_protected_path(&record, false).is_err() {
        return health.with_kind(ServiceHealthKind::UnrecognizedInstallation);
    }
    let Ok(metadata) = crate::read_metadata(&record) else {
        return health.with_kind(ServiceHealthKind::UnrecognizedInstallation);
    };
    health.installation = Some(metadata.clone());
    for (name, expected) in [
        (helper_name(), metadata.helper_sha256()),
        (core_name(), metadata.core_sha256()),
    ] {
        let path = root.join(name);
        let valid = platform::validate_protected_path(&path, false)
            .and_then(|()| platform::open_pinned_file(&path))
            .and_then(|file| artifact_hash(&file));
        if !matches!(valid, Ok(digest) if digest.eq_ignore_ascii_case(expected)) {
            return health.with_kind(ServiceHealthKind::RepairRequired);
        }
    }
    if metadata.protocol_version() != crate::PROTOCOL_VERSION {
        return health.with_kind(ServiceHealthKind::Incompatible);
    }
    let Ok(identity) = platform::current_identity() else {
        return health;
    };
    if !metadata
        .authorized_users()
        .iter()
        .any(|user| user == identity.user())
    {
        return health.with_kind(ServiceHealthKind::Unauthorized);
    }
    health.with_kind(ServiceHealthKind::Stopped)
}

/// Requests a fixed native authorization flow from a background application task.
///
/// The bundled helper is pinned and hashed before requesting authorization.
/// Install and repair require the explicitly selected Mihomo artifact. Completion
/// includes a verified IPC handshake; an installer exit code alone is insufficient.
///
/// # Errors
/// Returns `BeforeRequest` when source pinning, hashing or owner verification
/// fails, `AuthorizationCancelled` only for explicit OS cancellation, and
/// `Failed` for an unsuccessful authorized operation. An observation timeout
/// returns `OutcomeUnconfirmed`; its retained completion must be resolved
/// before submitting another operation.
pub async fn maintain_service(
    action: MaintenanceAction,
    helper: &Path,
    core: Option<&Path>,
) -> Result<(), MaintenanceError> {
    let helper = helper.to_owned();
    let core = core.map(Path::to_owned);
    let prepared = tokio::task::spawn_blocking(move || {
        let helper_file = platform::open_pinned_file(&helper)?;
        let helper_digest = artifact_hash(&helper_file)?;
        let owner = platform::current_identity()?;
        let mut args = vec![
            OsString::from("--helper-sha256"),
            OsString::from(helper_digest),
            OsString::from("--owner-pid"),
            OsString::from(owner.pid().to_string()),
            OsString::from("--owner-birth"),
            OsString::from(owner.birth().to_string()),
        ];
        let core_file = if matches!(
            action,
            MaintenanceAction::Install | MaintenanceAction::Repair
        ) {
            let source = core.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "approved Mihomo source is required",
                )
            })?;
            let file = platform::open_pinned_file(source)?;
            args.extend([
                OsString::from("--core-source"),
                source.as_os_str().to_owned(),
                OsString::from("--core-sha256"),
                OsString::from(artifact_hash(&file)?),
            ]);
            Some(file)
        } else {
            None
        };
        Ok::<_, io::Error>((helper, args, helper_file, core_file))
    })
    .await
    .map_err(|error| MaintenanceError::BeforeRequest(Arc::new(io::Error::other(error))))?
    .map_err(|error| MaintenanceError::BeforeRequest(Arc::new(error)))?;
    let (helper, args, pinned_helper, pinned_core) = prepared;
    // A dedicated native worker may outlive the GUI runtime while an OS prompt
    // is unanswered. It only maintains the helper; it never publishes TUN intent.
    let (sender, completion) = tokio::sync::watch::channel(None);
    std::thread::Builder::new()
        .name("zenclash-service-maintenance".into())
        .spawn(move || {
            let _pins = (pinned_helper, pinned_core);
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| MaintenanceError::BeforeRequest(Arc::new(error)))
                .and_then(|runtime| {
                    runtime.block_on(async {
                        #[cfg(windows)]
                        platform::request_maintenance(action.name(), &helper, &args)
                            .map_err(authorization_result)?;
                        #[cfg(unix)]
                        platform::request_maintenance(action.name(), &helper, &args)
                            .await
                            .map_err(authorization_result)?;
                        if action != MaintenanceAction::Uninstall {
                            wait_until_ready().await.map_err(|error| {
                                MaintenanceError::OutcomeUnconfirmed {
                                    source: Arc::new(error),
                                    pending: None,
                                }
                            })?;
                        } else if ServiceClient::probe().await.is_ok() {
                            return Err(MaintenanceError::OutcomeUnconfirmed {
                                source: Arc::new(io::Error::other(
                                    "unregistered service remains reachable",
                                )),
                                pending: None,
                            });
                        }
                        Ok(())
                    })
                });
            sender.send_replace(Some(result));
        })
        .map_err(|error| MaintenanceError::BeforeRequest(Arc::new(error)))?;
    let pending = MaintenancePending { completion };
    match tokio::time::timeout(Duration::from_secs(120), pending.wait()).await {
        Ok(result) => result,
        Err(_) => Err(MaintenanceError::OutcomeUnconfirmed {
            source: Arc::new(io::Error::new(
                io::ErrorKind::TimedOut,
                "native authorization or maintenance is still pending",
            )),
            pending: Some(pending),
        }),
    }
}

async fn wait_until_ready() -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service did not become ready",
            ));
        }
        if let Ok(Ok(protocol)) = tokio::time::timeout(
            remaining.min(Duration::from_secs(2)),
            ServiceClient::probe(),
        )
        .await
            && protocol.is_compatible()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) fn artifact_hash(file: &fs::File) -> io::Result<String> {
    if file.metadata()?.len() == 0 || file.metadata()?.len() > MAX_ARTIFACT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "approved artifact exceeds size budget",
        ));
    }
    let mut reader = file;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > MAX_ARTIFACT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "approved artifact exceeds size budget",
            ));
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(feature = "server")]
struct Invocation {
    action: MaintenanceAction,
    helper_digest: String,
    owner_pid: u32,
    owner_birth: u64,
    core: Option<(PathBuf, String)>,
}

#[cfg(feature = "server")]
impl Invocation {
    fn parse(arguments: &[OsString]) -> io::Result<Self> {
        let action = match arguments.first().and_then(|argument| argument.to_str()) {
            Some("--install") => MaintenanceAction::Install,
            Some("--repair") => MaintenanceAction::Repair,
            Some("--uninstall") => MaintenanceAction::Uninstall,
            Some("--start") => MaintenanceAction::Start,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown service maintenance operation",
                ));
            }
        };
        let mut fields = std::collections::BTreeMap::new();
        for pair in arguments[1..].chunks(2) {
            if pair.len() != 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "maintenance option needs a value",
                ));
            }
            let key = pair[0].to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid maintenance option")
            })?;
            if !matches!(
                key,
                "--helper-sha256"
                    | "--owner-pid"
                    | "--owner-birth"
                    | "--core-source"
                    | "--core-sha256"
            ) || fields.insert(key, &pair[1]).is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown or repeated maintenance option",
                ));
            }
        }
        let string = |key| {
            fields
                .get(key)
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "required maintenance option missing",
                    )
                })
        };
        let helper_digest = valid_digest(string("--helper-sha256")?)?;
        let owner_pid = string("--owner-pid")?
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid owner process"))?;
        let owner_birth = string("--owner-birth")?
            .parse::<u64>()
            .ok()
            .filter(|birth| *birth != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid owner process birth")
            })?;
        let core = if matches!(
            action,
            MaintenanceAction::Install | MaintenanceAction::Repair
        ) {
            let source = PathBuf::from(*fields.get("--core-source").ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "approved core source missing")
            })?);
            if !source.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "approved core source must be absolute",
                ));
            }
            Some((source, valid_digest(string("--core-sha256")?)?))
        } else {
            if fields.contains_key("--core-source") || fields.contains_key("--core-sha256") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "operation cannot replace approved artifacts",
                ));
            }
            None
        };
        Ok(Self {
            action,
            helper_digest,
            owner_pid,
            owner_birth,
            core,
        })
    }
}

#[cfg(feature = "server")]
fn valid_digest(value: &str) -> io::Result<String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid approved artifact digest",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

/// Executes a fixed maintenance invocation after native administrator authorization.
///
/// Caller identity is re-read from the OS and bound to process creation time;
/// no UID or SID supplied through arguments is trusted. Package-owned helper
/// source files and user configuration are never deleted or rewritten.
///
/// # Errors
/// Rejects insufficient privilege, malformed arguments, stale native caller
/// identity or changed approved artifacts. Returns filesystem or service-manager
/// failures without treating an unconfirmed deployment as committed.
#[cfg(feature = "server")]
pub fn run_maintenance(arguments: &[OsString]) -> io::Result<()> {
    platform::require_admin()?;
    let invocation = Invocation::parse(arguments)?;
    #[cfg(unix)]
    let owner = platform::verified_owner(invocation.owner_pid, invocation.owner_birth)?;
    #[cfg(windows)]
    let owner = {
        let owner = platform::identity_from_pid(invocation.owner_pid)?;
        if owner.birth() != invocation.owner_birth || !platform::peer_alive(&owner) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "maintenance owner identity changed",
            ));
        }
        owner
    };
    let current = std::env::current_exe()?;
    let helper = platform::open_pinned_file(&current)?;
    if artifact_hash(&helper)? != invocation.helper_digest {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "approved helper digest changed",
        ));
    }
    let root = platform::root_directory()?;
    #[cfg(target_os = "macos")]
    platform::validate_service_registration()?;
    platform::create_private_directory(&root, true)?;
    let _guard = MaintenanceLock::acquire(&root)?;
    validate_maintenance_files(&root)?;
    if let Some(journal) = crate::maintenance_journal::Journal::load(&root)? {
        recover_maintenance(&root, &journal)?;
    }
    let metadata_path = root.join("install.json");
    let previous = if metadata_path.exists() {
        platform::validate_protected_path(&metadata_path, false)?;
        Some(crate::read_metadata(&metadata_path).map_err(io::Error::other)?)
    } else {
        None
    };
    match invocation.action {
        MaintenanceAction::Start => {
            if previous.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "service is not installed",
                ));
            }
            platform::start_service()?;
        }
        MaintenanceAction::Uninstall => {
            // Only this service's fixed private artifacts are removed. Unknown
            // root entries, package resources, and source paths remain intact.
            if previous.is_none()
                && fs::read_dir(&root)?
                    .any(|entry| entry.is_ok_and(|entry| entry.file_name() != ".maintenance.lock"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "installation ownership metadata missing",
                ));
            }
            platform::stop_service()?;
            platform::unregister_service()?;
            remove_private_runtime(&root.join("runtimes"), 0, &mut 0)?;
            for name in [
                helper_name(),
                core_name(),
                "install.json",
                "helper.previous",
                "core.previous",
                "metadata.previous",
                "helper.new",
                "core.new",
                "helper.diagnostic",
                "core.diagnostic",
            ] {
                remove_private_file(&root.join(name))?;
            }
        }
        MaintenanceAction::Install | MaintenanceAction::Repair => {
            let (source, core_digest) = invocation.core.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "approved core source missing")
            })?;
            let core = platform::open_pinned_file(&source)?;
            let mut metadata = InstalledMetadata::new(
                core_digest.clone(),
                invocation.helper_digest.clone(),
                owner.user().to_owned(),
            )
            .map_err(io::Error::other)?;
            if let Some(previous) = &previous {
                for user in previous.authorized_users() {
                    metadata
                        .authorize_user(user.clone())
                        .map_err(io::Error::other)?;
                }
            }
            deploy(
                &root,
                ApprovedArtifact {
                    source: &current,
                    digest: &invocation.helper_digest,
                },
                ApprovedArtifact {
                    source: &source,
                    digest: &core_digest,
                },
                &metadata,
                &owner,
                invocation.action == MaintenanceAction::Repair,
            )?;
            drop(core);
        }
    }
    Ok(())
}

fn helper_name() -> &'static str {
    if cfg!(windows) {
        "zenclash-service.exe"
    } else {
        "zenclash-service"
    }
}
fn core_name() -> &'static str {
    if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    }
}

#[cfg(feature = "server")]
struct MaintenanceLock {
    file: fs::File,
}

#[cfg(feature = "server")]
impl MaintenanceLock {
    fn acquire(root: &Path) -> io::Result<Self> {
        let path = root.join(".maintenance.lock");
        if path.exists() {
            platform::validate_protected_path(&path, false)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: file exclusively owns a live descriptor; nonblocking lock.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "service maintenance is already in progress",
                ));
            }
        }
        Ok(Self { file })
    }
}

#[cfg(feature = "server")]
impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // Keep the inode stable: unlinking a lock allows a second lock inode.
            // SAFETY: self.file is still open and owns its lock.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            let _ = &self.file;
        }
    }
}

#[cfg(feature = "server")]
fn stage_copy(source: &Path, destination: &Path, expected: &str) -> io::Result<()> {
    use std::io::Write;
    remove_private_file(destination)?;
    let source = platform::open_pinned_file(source)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700).custom_flags(libc::O_NOFOLLOW);
    }
    let mut output = options.open(destination)?;
    let result = (|| {
        let count = io::copy(&mut (&source).take(MAX_ARTIFACT_BYTES + 1), &mut output)?;
        if count == 0 || count > MAX_ARTIFACT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "approved artifact exceeds size budget",
            ));
        }
        output.flush()?;
        output.sync_all()?;
        drop(output);
        platform::validate_protected_path(destination, false)?;
        if artifact_hash(&platform::open_pinned_file(destination)?)? != expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "approved artifact changed during deployment",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_private_file(destination);
    }
    result
}

#[cfg(feature = "server")]
struct ApprovedArtifact<'a> {
    source: &'a Path,
    digest: &'a str,
}

#[cfg(feature = "server")]
fn validate_maintenance_files(root: &Path) -> io::Result<()> {
    for name in [
        helper_name(),
        core_name(),
        "helper.new",
        "core.new",
        "helper.previous",
        "core.previous",
        "helper.restore",
        "core.restore",
        "helper.diagnostic",
        "core.diagnostic",
        "metadata.previous",
        "install.json",
        "maintenance.json",
        "maintenance.new",
    ] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => platform::validate_protected_path(&path, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(feature = "server")]
fn recover_maintenance(
    root: &Path,
    journal: &crate::maintenance_journal::Journal,
) -> io::Result<()> {
    journal.validate_recovery(root)?;
    platform::stop_service()?;
    journal.recover_storage(root)?;
    if journal.recovery_metadata().is_some() {
        platform::register_service()?;
        if journal.restore_running() {
            platform::start_service()?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(wait_until_ready())?;
        }
    } else {
        platform::unregister_service()?;
    }
    journal.finish(root)
}

#[cfg(feature = "server")]
fn deploy(
    root: &Path,
    approved_helper: ApprovedArtifact<'_>,
    approved_core: ApprovedArtifact<'_>,
    metadata: &InstalledMetadata,
    owner: &crate::session::PeerIdentity,
    repair: bool,
) -> io::Result<()> {
    let helper_stage = root.join("helper.new");
    let core_stage = root.join("core.new");
    let was_running = platform::service_was_running()?;
    stage_copy(
        approved_helper.source,
        &helper_stage,
        approved_helper.digest,
    )?;
    if let Err(error) = stage_copy(approved_core.source, &core_stage, approved_core.digest) {
        let _ = remove_private_file(&helper_stage);
        return Err(error);
    }
    validate_maintenance_files(root)?;
    let prepare = if repair {
        crate::maintenance_journal::Journal::prepare_repair
    } else {
        crate::maintenance_journal::Journal::prepare
    };
    let mut journal = prepare(root, metadata, approved_helper.digest, was_running)?;
    let result = (|| {
        platform::stop_service()?;
        if !platform::peer_alive(owner) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "maintenance owner exited",
            ));
        }
        if repair && journal.recovery_metadata().is_none() {
            // Diagnostic rollback cannot run the previous bytes. Remove the
            // autostart entry before exposing any replacement or mixed pair.
            platform::unregister_service()?;
        }
        journal.swap(root)?;
        crate::write_metadata_atomic(&root.join("install.json"), journal.new_metadata())
            .map_err(io::Error::other)?;
        platform::register_service()?;
        platform::start_service()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(wait_until_ready())?;
        journal.commit(root)?;
        journal.finish(root)
    })();
    if let Err(error) = result {
        // Read the durable phase again: commit can succeed before cleanup fails.
        let persisted = crate::maintenance_journal::Journal::load(root)?
            .ok_or_else(|| io::Error::other("maintenance journal disappeared before recovery"))?;
        if let Err(recovery) = recover_maintenance(root, &persisted) {
            return Err(io::Error::other(format!(
                "service update failed ({error}); durable recovery remains pending ({recovery})"
            )));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(feature = "server")]
fn remove_private_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            platform::validate_protected_path(path, false)?;
            fs::remove_file(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(feature = "server")]
pub(crate) fn remove_private_runtime(
    path: &Path,
    depth: usize,
    count: &mut usize,
) -> io::Result<()> {
    remove_private_runtime_with_budget(path, depth, count, &mut RuntimeCleanupBudget::default())
}

#[cfg(feature = "server")]
#[derive(Default)]
pub(crate) struct RuntimeCleanupBudget {
    #[cfg(windows)]
    retry_index: usize,
}

#[cfg(feature = "server")]
impl RuntimeCleanupBudget {
    // Adapt upstream runtime_cleanup_retry_delay: only deletion contention,
    // with one allowance across the entire cleanup, not one per file/root.
    fn retry_delay(&mut self, error: &io::Error) -> Option<Duration> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{
                ERROR_DELETE_PENDING, ERROR_SHARING_VIOLATION, ERROR_USER_MAPPED_FILE,
            };
            if matches!(error.raw_os_error(), Some(code)
                if code == ERROR_SHARING_VIOLATION as i32
                    || code == ERROR_DELETE_PENDING as i32
                    || code == ERROR_USER_MAPPED_FILE as i32)
            {
                let delay = [25, 50, 100].get(self.retry_index).copied()?;
                self.retry_index += 1;
                return Some(Duration::from_millis(delay));
            }
        }
        let _ = error;
        None
    }
}

#[cfg(feature = "server")]
pub(crate) fn remove_private_runtime_with_budget(
    path: &Path,
    depth: usize,
    count: &mut usize,
    budget: &mut RuntimeCleanupBudget,
) -> io::Result<()> {
    remove_runtime_with_budget(
        path,
        depth,
        count,
        &platform::validate_protected_path,
        budget,
    )
}

#[cfg(all(test, feature = "server", windows))]
fn remove_runtime_with(
    path: &Path,
    depth: usize,
    count: &mut usize,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
) -> io::Result<()> {
    remove_runtime_with_budget(
        path,
        depth,
        count,
        validate,
        &mut RuntimeCleanupBudget::default(),
    )
}

#[cfg(feature = "server")]
fn remove_runtime_with_budget(
    path: &Path,
    depth: usize,
    count: &mut usize,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
    budget: &mut RuntimeCleanupBudget,
) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if depth > 24 || *count > 32768 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime cleanup exceeds budget",
        ));
    }
    validate(path, true)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        *count += 1;
        if *count > 32768 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime cleanup exceeds budget",
            ));
        }
        if entry.file_type()?.is_dir() {
            remove_runtime_with_budget(&entry.path(), depth + 1, count, validate, budget)?;
        } else {
            remove_runtime_entry(&entry.path(), false, validate, budget)?;
        }
    }
    remove_runtime_entry(path, true, validate, budget)
}

#[cfg(feature = "server")]
fn remove_runtime_entry(
    path: &Path,
    directory: bool,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
    budget: &mut RuntimeCleanupBudget,
) -> io::Result<()> {
    loop {
        // Protection errors never reach the native-error retry classifier, even
        // if their raw code happens to match a transient deletion error.
        validate(path, directory)?;
        let result = if directory {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                let Some(delay) = budget.retry_delay(&error) else {
                    return Err(error);
                };
                std::thread::sleep(delay);
            }
        }
    }
}

#[cfg(all(test, feature = "server"))]
pub(crate) struct OwnedTestRoot(std::path::PathBuf);

#[cfg(all(test, feature = "server"))]
impl OwnedTestRoot {
    pub(crate) fn create() -> io::Result<Self> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let path = std::env::temp_dir().join(format!(
            "zenclash-runtime-fixture-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(bytes)
        ));
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixture root must be absolute",
            ));
        }
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    fn validate(&self, path: &Path, directory: bool) -> io::Result<()> {
        let denied = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture path escaped its owned root",
            )
        };
        let relative = path.strip_prefix(&self.0).map_err(|_| denied())?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(denied());
        }
        let root_metadata = fs::symlink_metadata(&self.0)?;
        if root_metadata.file_type().is_symlink() {
            return Err(denied());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if root_metadata.file_attributes() & 0x400 != 0 {
                return Err(denied());
            }
        }
        let mut current = self.0.clone();
        for component in relative.components() {
            current.push(component);
            let metadata = fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() {
                return Err(denied());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(denied());
                }
            }
        }
        let root = self.0.canonicalize()?;
        let resolved = path.canonicalize()?;
        if resolved == root
            || !resolved.starts_with(&root)
            || fs::metadata(path)?.is_dir() != directory
        {
            return Err(denied());
        }
        Ok(())
    }

    pub(crate) fn create_directory(&self, path: &Path) -> io::Result<()> {
        let relative = path.strip_prefix(&self.0).map_err(|_| {
            io::Error::new(io::ErrorKind::PermissionDenied, "fixture directory escaped")
        })?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture directory escaped",
            ));
        }
        let mut current = self.0.clone();
        for component in relative.components() {
            current.push(component);
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            self.validate(&current, true)?;
        }
        Ok(())
    }

    pub(crate) fn remove(&self, path: &Path) -> io::Result<()> {
        self.remove_with_budget(path, &mut RuntimeCleanupBudget::default())
    }

    pub(crate) fn remove_with_budget(
        &self,
        path: &Path,
        budget: &mut RuntimeCleanupBudget,
    ) -> io::Result<()> {
        self.validate(path, true)?;
        remove_runtime_with_budget(
            path,
            0,
            &mut 0,
            &|path, directory| self.validate(path, directory),
            budget,
        )
    }
}

#[cfg(all(test, feature = "server", windows))]
#[path = "runtime_cleanup_tests.rs"]
mod runtime_cleanup_tests;

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    #[test]
    fn fixture_cleanup_cannot_delete_its_root_or_a_sibling() {
        let root = OwnedTestRoot::create().unwrap();
        let sibling = OwnedTestRoot::create().unwrap();
        let file = sibling.path().join("keep");
        fs::write(&file, b"outside fixture").unwrap();
        assert_eq!(
            root.remove(root.path()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            root.remove(sibling.path()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(&file).unwrap(), b"outside fixture");
        fs::remove_file(file).unwrap();
        fs::remove_dir(sibling.path()).unwrap();
        fs::remove_dir(root.path()).unwrap();
    }

    #[test]
    fn fixture_cleanup_rejects_directory_links_without_following_them() {
        let root = OwnedTestRoot::create().unwrap();
        let sibling = OwnedTestRoot::create().unwrap();
        let directory = root.path().join("candidate");
        root.create_directory(&directory).unwrap();
        let link = directory.join("escape");
        let file = sibling.path().join("keep");
        fs::write(&file, b"outside link").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(sibling.path(), &link).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let result = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(sibling.path())
                .creation_flags(0x0800_0000)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "junction fixture failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert_eq!(
            root.remove(&directory).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(&file).unwrap(), b"outside link");
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        root.remove(&directory).unwrap();
        fs::remove_file(&file).unwrap();
        fs::remove_dir(sibling.path()).unwrap();
        fs::remove_dir(root.path()).unwrap();
    }

    fn arguments(action: &str) -> Vec<OsString> {
        [
            action,
            "--helper-sha256",
            &"a".repeat(64),
            "--owner-pid",
            "12",
            "--owner-birth",
            "34",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn maintenance_rejects_arbitrary_execution_and_identity_options() {
        let mut args = arguments("--start");
        args.extend([OsString::from("--owner-uid"), OsString::from("0")]);
        assert!(Invocation::parse(&args).is_err());
    }

    #[test]
    fn maintenance_rejects_conflicting_repeated_options() {
        let mut args = arguments("--start");
        args.extend([OsString::from("--owner-pid"), OsString::from("15")]);
        assert!(Invocation::parse(&args).is_err());
    }

    #[test]
    fn start_cannot_replace_approved_artifacts() {
        let mut args = arguments("--start");
        args.extend([OsString::from("--core-source"), OsString::from("/tmp/core")]);
        assert!(Invocation::parse(&args).is_err());
    }

    #[test]
    fn install_requires_an_explicit_complete_core_approval() {
        assert!(Invocation::parse(&arguments("--install")).is_err());
    }

    #[test]
    fn artifact_hash_binds_actual_file_bytes() {
        let path =
            std::env::temp_dir().join(format!("zenclash-install-hash-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            artifact_hash(&fs::File::open(&path).unwrap()).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod maintenance_request_tests {
    use super::*;

    #[test]
    fn explicit_authorization_cancel_is_not_a_failed_installation() {
        assert!(matches!(
            authorization_result(authorization_cancelled()),
            MaintenanceError::AuthorizationCancelled,
        ));
    }

    #[test]
    fn native_observation_timeout_cannot_authorize_another_installation() {
        assert!(matches!(
            authorization_result(io::Error::new(
                io::ErrorKind::TimedOut,
                "native outcome unknown"
            )),
            MaintenanceError::OutcomeUnconfirmed { .. },
        ));
    }

    #[test]
    fn maintenance_interrupted_io_is_not_user_authorization_cancellation() {
        assert!(matches!(
            authorization_result(io::Error::from(io::ErrorKind::Interrupted)),
            MaintenanceError::Failed(_),
        ));
    }

    #[tokio::test]
    async fn dropping_a_waiter_retains_actual_completion() {
        let (sender, completion) = tokio::sync::watch::channel(None);
        let pending = MaintenancePending { completion };
        let waiter_pending = pending.clone();
        let waiter = tokio::spawn(async move { waiter_pending.wait().await });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        sender.send_replace(Some(Err(MaintenanceError::AuthorizationCancelled)));
        assert!(matches!(
            pending.wait().await,
            Err(MaintenanceError::AuthorizationCancelled)
        ));
    }

    #[test]
    fn unprotected_existing_installation_is_never_presented_as_missing() {
        let root = std::env::temp_dir().join(format!("zenclash-health-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let health = installation_health(&root);
        fs::remove_dir(&root).unwrap();
        assert_eq!(health.kind(), ServiceHealthKind::UnrecognizedInstallation);
    }

    #[test]
    fn absent_installation_is_missing_without_reading_or_creating_artifacts() {
        let root =
            std::env::temp_dir().join(format!("zenclash-health-absent-{}", std::process::id()));
        assert_eq!(
            installation_health(&root).kind(),
            ServiceHealthKind::Missing
        );
        assert!(!root.exists());
    }
}
