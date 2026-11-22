#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Unified privileged installation and uninstallation subcommands for `zenclash-service`.

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(all(target_os = "linux", not(test)))]
mod selinux;
#[cfg(any(windows, test))]
mod windows;

use anyhow::{Context as _, Error, bail};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Allowlist for auto-staging; excludes other executables shipped beside the installer.
const BUNDLED_CORE_NAMES: [&str; 2] = ["mihomo", "mihomo-alpha"];

/// A core named on the command line, with the digest the caller vouches for.
pub(super) struct CoreInstallRequest {
    source: PathBuf,
    name: OsString,
    sha256: Option<[u8; 32]>,
}

pub(super) struct InstallOptions {
    pub(super) install_service: bool,
    #[cfg(windows)]
    pub(super) ensure_service: bool,
    pub(super) cores: Vec<CoreInstallRequest>,
    pub(super) debug: bool,
}

/// Runs the `install` CLI using the current process's arguments (skipping `argv[0]`).
///
/// # Errors
/// Returns argument parsing, preparation, elevation, staging, or service registration errors.
pub fn run_install_cli() -> Result<(), Error> {
    run_install_cli_with_args(std::env::args_os().skip(1))
}

/// Runs the `install` CLI with an explicit argument list.
///
/// # Errors
/// Returns argument parsing, preparation, elevation, staging, or service registration errors.
pub fn run_install_cli_with_args<I>(arguments: I) -> Result<(), Error>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = arguments.into_iter().collect();
    if args.first().map(OsString::as_os_str) == Some(OsStr::new("--prepare-install")) {
        #[cfg(feature = "client")]
        {
            let _ = crate::management::prepare_install_if_requested_with_args(args)?;
            return Ok(());
        }
        #[cfg(not(feature = "client"))]
        {
            bail!("--prepare-install requires zenclash-service built with the `client` feature");
        }
    }
    if run_maintenance_if_requested(&args)? {
        return Ok(());
    }
    let options = parse_install_arguments(args.into_iter())?;
    #[cfg(target_os = "macos")]
    {
        macos::install(options)
    }
    #[cfg(target_os = "linux")]
    {
        linux::install(options)
    }
    #[cfg(windows)]
    {
        windows::install(options)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = options;
        bail!("Service installation is not supported on this platform")
    }
}

/// Runs the `uninstall` CLI using the current process's arguments (skipping `argv[0]`).
///
/// # Errors
/// Returns repair-gate, service-removal, or file-cleanup errors.
pub fn run_uninstall_cli() -> Result<(), Error> {
    run_uninstall_cli_with_args(std::env::args_os().skip(1))
}

/// Runs the `uninstall` CLI with an explicit argument list.
///
/// # Errors
/// Returns repair-gate, service-removal, or file-cleanup errors.
pub fn run_uninstall_cli_with_args<I>(arguments: I) -> Result<(), Error>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = arguments.into_iter().collect();
    if run_maintenance_if_requested(&args)? {
        return Ok(());
    }
    let debug = args.iter().any(|arg| arg == "--debug");
    #[cfg(target_os = "macos")]
    {
        macos::uninstall(debug)
    }
    #[cfg(target_os = "linux")]
    {
        linux::uninstall(debug)
    }
    #[cfg(windows)]
    {
        windows::uninstall(debug)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = debug;
        bail!("Service uninstallation is not supported on this platform")
    }
}

pub(super) fn repair_active_owner_state() -> Result<(), Error> {
    if let Some(backup) = crate::repair_active_owner_state()? {
        eprintln!(
            "Quarantined corrupt active owner state at {backup:?}; the next start will establish a new owner session"
        );
    }
    Ok(())
}

pub(super) fn enter_repair_gate() -> Result<crate::ServiceRepairGate, Error> {
    match crate::acquire_service_repair_gate()? {
        Some(gate) => Ok(gate),
        None => {
            eprintln!("Service repair is already in progress");
            std::process::exit(crate::REPAIR_IN_PROGRESS_EXIT_CODE);
        }
    }
}

fn run_maintenance_if_requested(args: &[OsString]) -> Result<bool, Error> {
    if !args.iter().any(|argument| argument == "--cleanup-stale-owners") {
        return Ok(false);
    }
    let removed = crate::cleanup_stale_owner_state()?;
    println!("Removed {} stale owner state directories", removed.len());
    Ok(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn run_command(cmd: &str, args: &[&str], debug: bool) -> Result<(), Error> {
    if debug {
        println!("Executing: {} {}", cmd, args.join(" "));
    }

    let output = std::process::Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to execute '{}': {}", cmd, e))?;

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if debug {
        eprintln!(
            "Command failed (status: {}):\nstdout: {}\nstderr: {}",
            output.status, stdout, stderr
        );
    }

    Err(anyhow::anyhow!(
        "Command '{}' failed (status: {}):\nstdout: {}\nstderr: {}",
        cmd,
        output.status,
        stdout,
        stderr
    ))
}

pub(super) fn remove_installed_cores() {
    let paths = match crate::service_paths() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("Could not locate core directory for cleanup: {error}");
            return;
        }
    };
    let cores = paths.core_dir();
    #[cfg(windows)]
    windows::remove_core_firewall_rules(&cores);
    match std::fs::remove_dir_all(&cores) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "Could not remove core directory {cores:?}: {error}. \
             A process may still hold a core open; this does not affect the uninstall."
        ),
    }
}

pub(super) fn bundled_service_binary() -> Result<PathBuf, Error> {
    let source = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "zenclash-service.exe"
    } else {
        "zenclash-service"
    });
    let metadata = std::fs::symlink_metadata(&source)
        .with_context(|| format!("failed to inspect bundled service binary {source:?}"))?;
    if !metadata.file_type().is_file() {
        bail!("bundled service binary is not an ordinary file: {source:?}");
    }
    Ok(source)
}

fn sha256(path: &Path) -> Result<[u8; 32], Error> {
    let mut file = File::open(path).with_context(|| format!("failed to open {path:?}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {path:?}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub(super) fn remove_ordinary_file_if_exists(path: &Path) -> Result<(), Error> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {path:?}"));
        }
    };
    if !metadata.file_type().is_file() {
        bail!("refusing to replace non-file service entry {path:?}");
    }
    std::fs::remove_file(path).with_context(|| format!("failed to remove {path:?}"))
}

fn ensure_executable_label(path: &Path) -> Result<(), Error> {
    #[cfg(all(target_os = "linux", not(test)))]
    return selinux::ensure_executable_label(path);
    #[cfg(any(not(target_os = "linux"), test))]
    {
        let _ = path;
        Ok(())
    }
}

pub(super) fn stage_binary(source: &Path, target: &Path) -> Result<PathBuf, Error> {
    let parent = target.parent().context("protected target has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("failed to create protected directory {parent:?}"))?;
    let staged = target.with_extension(if cfg!(windows) {
        format!("exe.{}", crate::CORE_STAGING_EXTENSION)
    } else {
        crate::CORE_STAGING_EXTENSION.to_owned()
    });
    remove_ordinary_file_if_exists(&staged)?;

    let mut source_file = File::open(source).with_context(|| format!("failed to open candidate {source:?}"))?;
    let mut staged_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .with_context(|| format!("failed to create staged binary {staged:?}"))?;
    std::io::copy(&mut source_file, &mut staged_file)
        .with_context(|| format!("failed to stage binary at {staged:?}"))?;
    staged_file
        .sync_all()
        .with_context(|| format!("failed to sync staged binary {staged:?}"))?;
    drop(staged_file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o550))
            .with_context(|| format!("failed to secure staged binary {staged:?}"))?;
    }

    if sha256(source)? != sha256(&staged)? {
        let _ = std::fs::remove_file(&staged);
        bail!("staged binary hash does not match its source: {source:?}");
    }
    // Label the new inode before rename; neither chmod nor copying bytes preserves SELinux context.
    if let Err(error) = ensure_executable_label(&staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(staged)
}

fn core_file_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

fn validate_core_candidate(
    source: &Path,
    name: &OsStr,
    expected_sha256: Option<&[u8; 32]>,
) -> Result<(std::fs::Metadata, [u8; 32]), Error> {
    let metadata = std::fs::symlink_metadata(source).with_context(|| format!("failed to inspect core {source:?}"))?;
    if !metadata.file_type().is_file() {
        bail!("core candidate is not an ordinary file: {source:?}");
    }
    if Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(crate::CORE_STAGING_EXTENSION)
            || extension.eq_ignore_ascii_case(crate::CORE_DISPLACED_EXTENSION)
    }) {
        bail!("core name {name:?} collides with an installer bookkeeping suffix and could never be run");
    }
    let source_hash = sha256(source)?;
    if let Some(expected) = expected_sha256
        && &source_hash != expected
    {
        bail!(
            "core {source:?} does not match the attested sha256; refusing to publish it. \
             The file may have been replaced since the digest was computed."
        );
    }
    Ok((metadata, source_hash))
}

fn install_core(
    source: &Path,
    cores: &Path,
    name: &OsStr,
    expected_sha256: Option<&[u8; 32]>,
) -> Result<PathBuf, Error> {
    let (metadata, source_hash) = validate_core_candidate(source, name, expected_sha256)?;
    let target = cores.join(name);
    if let Ok(existing) = std::fs::symlink_metadata(&target)
        && existing.is_file()
        && sha256(&target)? == source_hash
    {
        ensure_executable_label(&target)?;
        println!("Core {} is already current", target.display());
        if let Err(error) = propagate_source_modified_time(&target, &metadata) {
            eprintln!("Could not refresh the stamp on {}: {error:#}", target.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o550));
        }
        return Ok(target);
    }

    let staged = stage_binary(source, &target)?;
    if let Some(expected) = expected_sha256
        && &sha256(&staged)? != expected
    {
        let _ = std::fs::remove_file(&staged);
        bail!("staged copy of {source:?} stopped matching the attested sha256; it changed while being copied");
    }
    propagate_source_modified_time(&staged, &metadata)?;
    publish_staged_binary(&staged, &target)?;
    Ok(target)
}

fn propagate_source_modified_time(staged: &Path, source_metadata: &std::fs::Metadata) -> Result<(), Error> {
    let modified = source_metadata
        .modified()
        .with_context(|| format!("source of {staged:?} carries no modified time"))?;
    let file = OpenOptions::new()
        .read(!cfg!(windows))
        .write(cfg!(windows))
        .open(staged)
        .with_context(|| format!("failed to reopen staged binary {staged:?}"))?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .with_context(|| format!("failed to stamp staged binary {staged:?}"))
}

fn sweep_core_bookkeeping_leftovers(cores: &Path) {
    let Ok(entries) = std::fs::read_dir(cores) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_leftover = path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case(crate::CORE_STAGING_EXTENSION)
                || extension.eq_ignore_ascii_case(crate::CORE_DISPLACED_EXTENSION)
        }) && entry.file_type().is_ok_and(|file_type| file_type.is_file());
        if is_leftover && let Err(error) = std::fs::remove_file(&path) {
            eprintln!("Could not remove leftover {}: {error}", path.display());
        }
    }
}

fn parse_install_arguments(arguments: impl Iterator<Item = OsString>) -> Result<InstallOptions, Error> {
    let mut requested: Vec<CoreInstallRequest> = Vec::new();
    let mut install_service = false;
    #[cfg(windows)]
    let mut ensure_service = false;
    let mut debug = false;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        if argument == "--install-core" {
            let value = arguments.next().context("--install-core requires a path")?;
            let source = PathBuf::from(value);
            let name = source
                .file_name()
                .context("--install-core path has no file name")?
                .to_owned();
            requested.push(CoreInstallRequest {
                source,
                name,
                sha256: None,
            });
        } else if argument == "--sha256" {
            let value = arguments.next().context("--sha256 requires a hex digest")?;
            let value = value.to_str().context("--sha256 digest is not valid UTF-8")?;
            let request = requested
                .last_mut()
                .context("--sha256 must follow the --install-core path it attests")?;
            if request.sha256.is_some() {
                bail!("--sha256 was given twice for {:?}", request.source);
            }
            request.sha256 = Some(parse_sha256_hex(value)?);
        } else if argument == "--install-service" {
            install_service = true;
        } else if argument == "--ensure-service" {
            #[cfg(windows)]
            {
                ensure_service = true;
                install_service = true;
            }
            #[cfg(not(windows))]
            bail!("--ensure-service is only supported on Windows");
        } else if argument == "--debug" {
            debug = true;
        } else {
            bail!("unknown installer argument: {argument:?}");
        }
    }
    Ok(InstallOptions {
        install_service: install_service || requested.is_empty(),
        #[cfg(windows)]
        ensure_service,
        cores: requested,
        debug,
    })
}

#[cfg(any(windows, test))]
pub(super) fn installed_payload_matches(
    source: &Path,
    target: &Path,
    cores: &[CoreInstallRequest],
    core_dir: &Path,
) -> Result<bool, Error> {
    let matches = |path: &Path, expected: &[u8; 32]| -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
            && sha256(path).is_ok_and(|actual| actual == *expected)
    };
    Ok(matches(target, &sha256(source)?)
        && !cores.is_empty()
        && cores.iter().all(|core| {
            core.sha256
                .as_ref()
                .is_some_and(|digest| matches(&core_dir.join(&core.name), digest))
        }))
}

fn parse_sha256_hex(value: &str) -> Result<[u8; 32], Error> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        bail!("--sha256 expects 64 hex characters, got {value:?}");
    }
    let mut digest = [0u8; 32];
    for (index, chunk) in bytes.as_chunks::<2>().0.iter().enumerate() {
        digest[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
    }
    Ok(digest)
}

#[cfg(windows)]
fn allow_core_through_firewall(core: &Path) {
    windows::allow_core_through_firewall(core);
}

#[cfg(not(windows))]
fn allow_core_through_firewall(_core: &Path) {}

pub(super) fn prepare_requested_cores(requested: &mut [CoreInstallRequest]) -> Result<(), Error> {
    for request in requested {
        if request.sha256.is_none() {
            request.source = crate::require_trusted_core_source(&request.source).with_context(|| {
                format!(
                    "core source {:?} sits where unprivileged accounts can write; pass --sha256 <digest \
                     of the downloaded bytes> so the copy can be verified",
                    request.source
                )
            })?;
        }
        validate_core_candidate(&request.source, &request.name, request.sha256.as_ref())?;
    }
    Ok(())
}

fn publish_requested_cores(cores: &Path, requested: &[CoreInstallRequest]) -> Result<Vec<PathBuf>, Error> {
    requested
        .iter()
        .map(|request| install_core(&request.source, cores, &request.name, request.sha256.as_ref()))
        .collect()
}

pub(super) fn install_service_cores(requested: &[CoreInstallRequest]) -> Result<(), Error> {
    if requested.is_empty() {
        return install_bundled_cores();
    }
    let cores = crate::prepare_core_install_directory()?;
    sweep_core_bookkeeping_leftovers(&cores);
    let installed = publish_requested_cores(&cores, requested).context(
        "explicit core installation did not finish; earlier cores may already be installed; rerun the installer to complete installation",
    )?;
    for installed in installed {
        println!("Installed core {}", installed.display());
        allow_core_through_firewall(&installed);
    }
    Ok(())
}

fn install_bundled_cores() -> Result<(), Error> {
    let installer = std::env::current_exe().context("failed to locate the running installer")?;
    let Some(directory) = installer.parent() else {
        return Ok(());
    };
    let mut roots = vec![directory.to_path_buf()];
    if let Some(parent) = directory.parent() {
        roots.push(parent.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    if let Some(bundle) = installer
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|extension| extension == "app"))
    {
        roots.push(bundle.join("Contents").join("MacOS"));
    }

    let cores = crate::prepare_core_install_directory()?;
    sweep_core_bookkeeping_leftovers(&cores);
    let mut seen: Vec<String> = Vec::new();
    for root in roots {
        for stem in BUNDLED_CORE_NAMES {
            let name = core_file_name(stem);
            let candidate = root.join(&name);
            if seen.contains(&name) || !candidate.is_file() {
                continue;
            }
            seen.push(name.clone());
            let source = match crate::require_trusted_core_source(&candidate) {
                Ok(canonical) => canonical,
                Err(reason) => {
                    eprintln!(
                        "Not auto-staging {}: {reason:#}. Stage it explicitly with --install-core <path> --sha256 <digest>.",
                        candidate.display()
                    );
                    continue;
                }
            };
            match install_core(&source, &cores, OsStr::new(&name), None) {
                Ok(installed) => println!("Installed core {}", installed.display()),
                Err(error) => {
                    keep_existing_core(error, &cores.join(&name))?;
                }
            }
            allow_core_through_firewall(&cores.join(&name));
        }
    }
    if seen.is_empty() {
        eprintln!(
            "No bundled core was found beside {}; stage one with --install-core before starting the core.",
            installer.display()
        );
    }
    Ok(())
}

fn keep_existing_core(error: Error, target: &Path) -> Result<(), Error> {
    #[cfg(all(target_os = "linux", not(test)))]
    if error.is::<selinux::LabelError>() {
        return Err(error);
    }
    let fallback_is_regular = std::fs::symlink_metadata(target)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);
    if !fallback_is_regular {
        return Err(error.context(format!("failed to stage {target:?}, and no approved copy exists")));
    }
    ensure_executable_label(target).with_context(|| {
        format!("failed to stage {target:?}: {error:#}; could not repair the existing copy's label")
    })?;
    eprintln!("Kept the existing copy of {}: {error:#}", target.display());
    Ok(())
}

pub(super) fn publish_staged_binary(staged: &Path, target: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("refusing to replace non-file service entry {target:?}");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {target:?}"));
        }
    }

    #[cfg(unix)]
    {
        std::fs::rename(staged, target)
            .with_context(|| format!("failed to publish service binary {staged:?} at {target:?}"))
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let wide = |path: &Path| {
            let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
            value.push(0);
            value
        };
        let move_over = |from: &Path, to: &Path| -> std::io::Result<()> {
            let from = wide(from);
            let to = wide(to);
            // SAFETY: Both paths are NUL-terminated UTF-16 buffers that remain
            // alive for the synchronous call.
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        };

        let Err(direct_error) = move_over(staged, target) else {
            let _ = std::fs::remove_file(target.with_extension(crate::CORE_DISPLACED_EXTENSION));
            return Ok(());
        };

        let displaced = target.with_extension(crate::CORE_DISPLACED_EXTENSION);
        if move_over(target, &displaced).is_err() {
            return Err(direct_error).with_context(|| format!("failed to publish {staged:?} at {target:?}"));
        }
        match move_over(staged, target) {
            Ok(()) => {
                let _ = std::fs::remove_file(&displaced);
                Ok(())
            }
            Err(publish_error) => {
                if move_over(&displaced, target).is_ok() {
                    Err(publish_error).with_context(|| format!("failed to publish {staged:?} at {target:?}"))
                } else {
                    Err(publish_error).with_context(|| {
                        format!("failed to publish {staged:?} at {target:?}, and to restore it from {displaced:?}")
                    })
                }
            }
        }
    }
}

#[cfg(feature = "client")]
pub(super) fn wait_for_service_ready() -> Result<(), Error> {
    use std::time::{Duration, Instant};

    const READY_TIMEOUT: Duration = Duration::from_secs(45);
    const READY_INTERVAL: Duration = Duration::from_millis(250);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create service readiness runtime")?;
    runtime.block_on(async {
        crate::set_config(Some(crate::IpcConfig {
            default_timeout: Duration::from_millis(250),
            max_retries: 1,
            retry_delay: Duration::from_millis(25),
        }))
        .await;

        let deadline = Instant::now() + READY_TIMEOUT;
        let result = loop {
            if let Ok(response) = crate::get_version().await
                && response.code == 0
                && response.data.is_some_and(|info| {
                    info.supports_client(crate::ProtocolVersion::current(), crate::MIN_REQUIRED_SERVICE_REVISION)
                })
            {
                break Ok(());
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!(
                    "service IPC did not become protocol-ready within {READY_TIMEOUT:?}"
                ));
            }
            tokio::time::sleep(READY_INTERVAL).await;
        };

        crate::set_config(None).await;
        result
    })
}

#[cfg(not(feature = "client"))]
pub(super) fn wait_for_service_ready() -> Result<(), Error> {
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.parse().ok()
}

// nix's `Group::from_gid` detects ERANGE through errno, but getgrgid_r reports it only in its
// return value, so a group record larger than nix's first buffer fails to resolve.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lookup_group_name(gid: u32, mut buffer_size: usize) -> std::io::Result<Option<String>> {
    const MAX_BUFFER_SIZE: usize = 1024 * 1024;
    loop {
        // SAFETY: `group` is a plain C struct; all-zero is valid before
        // `getgrgid_r` initializes it.
        let mut group = unsafe { std::mem::zeroed::<platform_lib::group>() };
        let mut buffer = vec![0 as platform_lib::c_char; buffer_size];
        let mut result = std::ptr::null_mut();
        // SAFETY: `group` and `result` are writable storage and `buffer` is
        // valid for `buffer.len()` bytes for the duration of the call.
        let status =
            unsafe { platform_lib::getgrgid_r(gid, &mut group, buffer.as_mut_ptr(), buffer.len(), &mut result) };
        match status {
            0 if result.is_null() => return Ok(None),
            0 => {
                // SAFETY: On success `gr_name` points at a NUL-terminated
                // string inside the live `buffer`.
                let name = unsafe { std::ffi::CStr::from_ptr(group.gr_name) };
                return Ok(Some(name.to_string_lossy().into_owned()));
            }
            platform_lib::ERANGE if buffer_size < MAX_BUFFER_SIZE => {
                buffer_size = (buffer_size * 2).min(MAX_BUFFER_SIZE);
            }
            error => return Err(std::io::Error::from_raw_os_error(error)),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn resolve_service_group_name() -> Result<String, Error> {
    use nix::unistd::{Uid, User};

    let invoking_user_gid = || {
        env_u32("SUDO_UID")
            .or_else(|| env_u32("PKEXEC_UID"))
            .and_then(|uid| User::from_uid(Uid::from_raw(uid)).ok().flatten())
            .map(|user| user.gid.as_raw())
    };
    let mut failure = None;
    for gid in env_u32("ZENCLASH_SERVICE_GID")
        .into_iter()
        .chain(std::iter::once_with(invoking_user_gid).flatten())
        .chain(env_u32("SUDO_GID"))
    {
        match lookup_group_name(gid, 4096) {
            Ok(Some(name)) => return Ok(name),
            Ok(None) => failure.get_or_insert_with(|| anyhow::anyhow!("no group record exists for gid {gid}")),
            Err(error) => failure.get_or_insert_with(|| {
                Error::new(error).context(format!("failed to look up the group record for gid {gid}"))
            }),
        };
    }
    Err(failure
        .unwrap_or_else(|| anyhow::anyhow!("the invoking user's group id is unavailable; use sudo or pkexec"))
        .context("unable to resolve the invoking user's service group"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> anyhow::Result<PathBuf> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("install-core-{label}-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(root.join("cores"))?;
        Ok(root)
    }

    fn combined_options(sources: &[PathBuf]) -> anyhow::Result<InstallOptions> {
        let mut arguments = vec![OsString::from("--install-service")];
        for source in sources {
            let digest: String = sha256(source)?.iter().map(|byte| format!("{byte:02x}")).collect();
            arguments.extend([
                "--install-core".into(),
                source.as_os_str().to_owned(),
                "--sha256".into(),
                digest.into(),
            ]);
        }
        parse_install_arguments(arguments.into_iter())
    }

    #[test]
    fn stopped_service_can_be_reused_only_with_matching_service_and_cores() -> anyhow::Result<()> {
        let root = scratch("ensure-payload")?;
        let source = root.join("bundled-service");
        let target = root.join("installed-service");
        let core = root.join(core_file_name("mihomo"));
        std::fs::write(&source, b"service")?;
        std::fs::write(&target, b"service")?;
        std::fs::write(&core, b"core")?;
        let options = combined_options(std::slice::from_ref(&core))?;
        assert!(installed_payload_matches(&source, &target, &options.cores, &root)?);
        std::fs::write(&core, b"different core")?;
        assert!(!installed_payload_matches(&source, &target, &options.cores, &root)?);
        std::fs::write(&core, b"core")?;
        std::fs::write(&target, b"old service")?;
        assert!(!installed_payload_matches(&source, &target, &options.cores, &root)?);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn combined_install_publishes_both_attested_cores_from_a_writable_directory() -> anyhow::Result<()> {
        let root = scratch("combined-install")?;
        let stable = root.join(core_file_name("mihomo"));
        let alpha = root.join(core_file_name("mihomo-alpha"));
        std::fs::write(&stable, b"stable core")?;
        std::fs::write(&alpha, b"alpha core")?;
        let mut options = combined_options(&[stable, alpha])?;
        assert!(options.install_service);
        prepare_requested_cores(&mut options.cores)?;
        let installed = publish_requested_cores(&root.join("cores"), &options.cores)?;
        assert_eq!(installed.len(), 2);
        assert_eq!(std::fs::read(&installed[0])?, b"stable core");
        assert_eq!(std::fs::read(&installed[1])?, b"alpha core");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn combined_install_preflight_rejects_a_missing_or_mismatched_second_core() -> anyhow::Result<()> {
        let root = scratch("preflight")?;
        let stable = root.join(core_file_name("mihomo"));
        let alpha = root.join(core_file_name("mihomo-alpha"));
        std::fs::write(&stable, b"stable core")?;
        std::fs::write(&alpha, b"alpha core")?;
        let mut options = combined_options(&[stable, alpha.clone()])?;
        std::fs::write(&alpha, b"changed alpha")?;
        let error = prepare_requested_cores(&mut options.cores).expect_err("must reject before stopping the service");
        assert!(error.to_string().contains("attested sha256"), "got {error:#}");
        std::fs::remove_file(alpha)?;
        assert!(prepare_requested_cores(&mut options.cores).is_err());
        assert!(std::fs::read_dir(root.join("cores"))?.next().is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn publishes_the_bytes_and_stamps_the_source_modified_time() -> anyhow::Result<()> {
        let root = scratch("stamp")?;
        let source = root.join("mihomo");
        std::fs::write(&source, b"core bytes")?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source)?
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))?;

        let target = install_core(&source, &root.join("cores"), OsStr::new("mihomo"), None)?;
        assert_eq!(std::fs::read(&target)?, b"core bytes");
        assert_eq!(
            std::fs::metadata(&source)?.modified()?,
            std::fs::metadata(&target)?.modified()?,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&target)?.permissions().mode() & 0o777, 0o550);
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn wrong_attestation_and_bookkeeping_suffixes_are_refused() -> anyhow::Result<()> {
        let root = scratch("attestation")?;
        let source = root.join("mihomo");
        std::fs::write(&source, b"core bytes")?;
        let error = install_core(&source, &root.join("cores"), OsStr::new("mihomo"), Some(&[0u8; 32]))
            .expect_err("must refuse");
        assert!(error.to_string().contains("attested sha256"), "got {error:#}");

        for name in ["mihomo.next", "mihomo.old"] {
            let bad = root.join(name);
            std::fs::write(&bad, b"core bytes")?;
            let error = install_core(&bad, &root.join("cores"), OsStr::new(name), None).expect_err("must refuse");
            assert!(error.to_string().contains("bookkeeping suffix"), "got {error:#}");
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn install_argument_parser_handles_service_and_core_options() -> anyhow::Result<()> {
        let digest = "ab".repeat(32);
        let options = parse_install_arguments(
            ["--install-service", "--install-core", "/dl/mihomo", "--sha256", &digest]
                .into_iter()
                .map(OsString::from),
        )?;
        assert!(options.install_service);
        assert_eq!(options.cores.len(), 1);
        assert_eq!(options.cores[0].sha256, Some([0xab; 32]));

        assert!(parse_install_arguments(std::iter::empty())?.install_service);
        assert!(
            !parse_install_arguments(["--install-core", "/dl/core"].into_iter().map(OsString::from))?.install_service
        );
        assert!(parse_install_arguments(["--unknown"].into_iter().map(OsString::from)).is_err());
        assert!(parse_sha256_hex(&digest).is_ok());
        assert!(parse_sha256_hex("short").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn group_lookup_grows_a_buffer_too_small_for_the_record() {
        let (gid, name) = if cfg!(target_os = "macos") {
            (20, "staff")
        } else {
            (0, "root")
        };
        assert_eq!(
            lookup_group_name(gid, 8)
                .expect("group lookup should succeed")
                .as_deref(),
            Some(name)
        );
    }
}
