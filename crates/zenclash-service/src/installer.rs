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

/// Requests a fixed native authorization flow from a background application task.
///
/// The bundled helper is pinned and hashed before requesting authorization.
/// Install and repair require the explicitly selected Mihomo artifact. Completion
/// includes a verified IPC handshake; an installer exit code alone is insufficient.
pub async fn maintain_service(
    action: MaintenanceAction,
    helper: &Path,
    core: Option<&Path>,
) -> io::Result<()> {
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
    .map_err(io::Error::other)??;
    let (helper, args, pinned_helper, pinned_core) = prepared;
    #[cfg(windows)]
    tokio::task::spawn_blocking(move || {
        let _pins = (pinned_helper, pinned_core);
        platform::request_maintenance(action.name(), &helper, &args)
    })
    .await
    .map_err(io::Error::other)??;
    #[cfg(unix)]
    {
        let _pins = (pinned_helper, pinned_core);
        platform::request_maintenance(action.name(), &helper, &args).await?;
    }
    if action != MaintenanceAction::Uninstall {
        wait_until_ready().await?;
    } else if ServiceClient::probe().await.is_ok() {
        return Err(io::Error::other("unregistered service remains reachable"));
    }
    Ok(())
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

fn artifact_hash(file: &fs::File) -> io::Result<String> {
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

#[cfg(feature = "server")]
fn helper_name() -> &'static str {
    if cfg!(windows) {
        "zenclash-service.exe"
    } else {
        "zenclash-service"
    }
}
#[cfg(feature = "server")]
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
    remove_runtime_with(path, depth, count, &platform::validate_protected_path)
}

#[cfg(feature = "server")]
fn remove_runtime_with(
    path: &Path,
    depth: usize,
    count: &mut usize,
    validate: &impl Fn(&Path, bool) -> io::Result<()>,
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
            remove_runtime_with(&entry.path(), depth + 1, count, validate)?;
        } else {
            validate(&entry.path(), false)?;
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_dir(path)
}

#[cfg(all(test, feature = "server"))]
pub(crate) struct OwnedTestRoot(std::path::PathBuf);

#[cfg(all(test, feature = "server"))]
impl OwnedTestRoot {
    pub(crate) fn create() -> io::Result<Self> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        let path = std::env::temp_dir().join(format!(
            "zenclash-runtime-fixture-{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path { &self.0 }

    fn validate(&self, path: &Path, directory: bool) -> io::Result<()> {
        let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "fixture path escaped its owned root");
        let relative = path.strip_prefix(&self.0).map_err(|_| denied())?;
        if relative.as_os_str().is_empty() || relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
            return Err(denied());
        }
        let root_metadata = fs::symlink_metadata(&self.0)?;
        if root_metadata.file_type().is_symlink() { return Err(denied()); }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if root_metadata.file_attributes() & 0x400 != 0 { return Err(denied()); }
        }
        let mut current = self.0.clone();
        for component in relative.components() {
            current.push(component);
            let metadata = fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() { return Err(denied()); }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 { return Err(denied()); }
            }
        }
        let root = self.0.canonicalize()?;
        let resolved = path.canonicalize()?;
        if resolved == root || !resolved.starts_with(&root) || fs::metadata(path)?.is_dir() != directory { return Err(denied()); }
        Ok(())
    }

    pub(crate) fn create_directory(&self, path: &Path) -> io::Result<()> {
        let relative = path.strip_prefix(&self.0).map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "fixture directory escaped"))?;
        if relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture directory escaped")); }
        let mut current = self.0.clone();
        for component in relative.components() {
            current.push(component);
            match fs::create_dir(&current) {
                Ok(()) => {},
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error),
            }
            self.validate(&current, true)?;
        }
        Ok(())
    }

    pub(crate) fn remove(&self, path: &Path) -> io::Result<()> {
        self.validate(path, true)?;
        remove_runtime_with(path, 0, &mut 0, &|path, directory| self.validate(path, directory))
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

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
