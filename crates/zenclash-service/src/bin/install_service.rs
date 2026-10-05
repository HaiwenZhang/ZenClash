// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn main() {
    panic!("This program is not intended to run on this platform.");
}

#[cfg(all(target_os = "linux", not(test)))]
#[path = "installer/selinux.rs"]
mod selinux;
mod shared;

use anyhow::Error;
use anyhow::{Context as _, bail};
use sha2::{Digest as _, Sha256};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use shared::run_command;
use shared::{enter_repair_gate, run_maintenance_if_requested};
use std::fs::{File, OpenOptions};
use std::io::Read as _;
#[cfg(unix)]
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn bundled_service_binary() -> Result<PathBuf, Error> {
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

fn remove_ordinary_file_if_exists(path: &Path) -> Result<(), Error> {
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

fn stage_binary(source: &Path, target: &Path) -> Result<PathBuf, Error> {
    let parent = target.parent().context("protected target has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("failed to create protected directory {parent:?}"))?;
    let staged = target.with_extension(if cfg!(windows) {
        format!("exe.{}", zenclash_service::CORE_STAGING_EXTENSION)
    } else {
        zenclash_service::CORE_STAGING_EXTENSION.to_owned()
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

/// Allowlist for auto-staging; excludes other executables shipped beside the installer.
const BUNDLED_CORE_NAMES: [&str; 2] = ["mihomo", "mihomo-alpha"];

fn core_file_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

fn validate_core_candidate(
    source: &Path,
    name: &std::ffi::OsStr,
    expected_sha256: Option<&[u8; 32]>,
) -> Result<(std::fs::Metadata, [u8; 32]), Error> {
    let metadata = std::fs::symlink_metadata(source).with_context(|| format!("failed to inspect core {source:?}"))?;
    if !metadata.file_type().is_file() {
        bail!("core candidate is not an ordinary file: {source:?}");
    }
    if Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(zenclash_service::CORE_STAGING_EXTENSION)
            || extension.eq_ignore_ascii_case(zenclash_service::CORE_DISPLACED_EXTENSION)
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

/// The caller must supply a trusted source or `expected_sha256`, verified against both copies.
/// The caller fixes `name` before resolving source links so they cannot change the published name.
fn install_core(
    source: &Path,
    cores: &Path,
    name: &std::ffi::OsStr,
    expected_sha256: Option<&[u8; 32]>,
) -> Result<PathBuf, Error> {
    let (metadata, source_hash) = validate_core_candidate(source, name, expected_sha256)?;
    let target = cores.join(name);
    // Avoid replacing an unchanged executable that Windows may still have open.
    if let Ok(existing) = std::fs::symlink_metadata(&target)
        && existing.is_file()
        && sha256(&target)? == source_hash
    {
        ensure_executable_label(&target)?;
        println!("Core {} is already current", target.display());
        // Metadata repair is best-effort while a running Windows core holds the file open.
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

/// Preserves the source timestamp for the service's core-update drift check.
fn propagate_source_modified_time(staged: &Path, source_metadata: &std::fs::Metadata) -> Result<(), Error> {
    let modified = source_metadata
        .modified()
        .with_context(|| format!("source of {staged:?} carries no modified time"))?;
    // Unix staging files are already 0550; their owner can set times through a read-only
    // descriptor. Windows needs write access for SetFileTime.
    let file = OpenOptions::new()
        .read(!cfg!(windows))
        .write(cfg!(windows))
        .open(staged)
        .with_context(|| format!("failed to reopen staged binary {staged:?}"))?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .with_context(|| format!("failed to stamp staged binary {staged:?}"))
}

/// Best-effort cleanup of staging and displaced files; locked cores survive until a later sweep.
fn sweep_core_bookkeeping_leftovers(cores: &Path) {
    let Ok(entries) = std::fs::read_dir(cores) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_leftover = path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case(zenclash_service::CORE_STAGING_EXTENSION)
                || extension.eq_ignore_ascii_case(zenclash_service::CORE_DISPLACED_EXTENSION)
        }) && entry.file_type().is_ok_and(|file_type| file_type.is_file());
        if is_leftover && let Err(error) = std::fs::remove_file(&path) {
            eprintln!("Could not remove leftover {}: {error}", path.display());
        }
    }
}

/// A core named on the command line, with the digest the caller vouches for.
struct CoreInstallRequest {
    source: PathBuf,
    name: std::ffi::OsString,
    sha256: Option<[u8; 32]>,
}

struct InstallOptions {
    install_service: bool,
    #[cfg(windows)]
    ensure_service: bool,
    cores: Vec<CoreInstallRequest>,
    debug: bool,
}

fn parse_install_arguments(arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<InstallOptions, Error> {
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
fn installed_payload_matches(
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

/// Adds a best-effort inbound rule for the approved executable path.
/// App-directory rules do not cover this copy, and session 0 cannot prompt for access.
/// TUN system/mixed stacks and LAN listeners need the rule. Delete first to avoid duplicates.
#[cfg(windows)]
fn allow_core_through_firewall(core: &Path) {
    let result = shared::core_firewall_rule_name(core).and_then(|name| {
        shared::record_core_firewall_rule(core)?;
        let _ = shared::netsh_firewall(&["delete", "rule", &format!("name={name}")]);
        shared::netsh_firewall(&[
            "add",
            "rule",
            &format!("name={name}"),
            "dir=in",
            "action=allow",
            "enable=yes",
            "profile=any",
            &format!("program={}", core.display()),
        ])
    });
    if let Err(error) = result {
        eprintln!(
            "Could not allow {} through Windows Firewall: {error:#}. TUN system/mixed stacks and LAN access \
             need an inbound rule for that path.",
            core.display()
        );
    }
}

#[cfg(not(windows))]
fn allow_core_through_firewall(_core: &Path) {}

fn prepare_requested_cores(requested: &mut [CoreInstallRequest]) -> Result<(), Error> {
    for request in requested {
        // The destination name was fixed before resolving links in the source path.
        if request.sha256.is_none() {
            request.source = zenclash_service::require_trusted_core_source(&request.source).with_context(|| {
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

fn install_service_cores(requested: &[CoreInstallRequest]) -> Result<(), Error> {
    if requested.is_empty() {
        return install_bundled_cores();
    }
    // An explicit list replaces bundle discovery; omitted cores are left untouched.
    // Publication is per file, so a later failure does not roll back earlier copies.
    let cores = zenclash_service::prepare_core_install_directory()?;
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

/// Publishes bundled cores when present; missing cores can be staged later with `--install-core`.
fn install_bundled_cores() -> Result<(), Error> {
    let installer = std::env::current_exe().context("failed to locate the running installer")?;
    let Some(directory) = installer.parent() else {
        return Ok(());
    };
    let mut roots = vec![directory.to_path_buf()];
    if let Some(parent) = directory.parent() {
        roots.push(parent.to_path_buf());
    }
    // The macOS bundle keeps this installer under Contents/Resources while the cores ship as
    // sidecars in Contents/MacOS, which neither of the directories above can see.
    #[cfg(target_os = "macos")]
    if let Some(bundle) = installer
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|extension| extension == "app"))
    {
        roots.push(bundle.join("Contents").join("MacOS"));
    }

    let cores = zenclash_service::prepare_core_install_directory()?;
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
            // Auto-staging requires a protected source; copy the canonical path that passed validation.
            let source = match zenclash_service::require_trusted_core_source(&candidate) {
                Ok(canonical) => canonical,
                Err(reason) => {
                    eprintln!(
                        "Not auto-staging {}: {reason:#}. Stage it explicitly with --install-core <path> --sha256 <digest>.",
                        candidate.display()
                    );
                    continue;
                }
            };
            match install_core(&source, &cores, std::ffi::OsStr::new(&name), None) {
                Ok(installed) => println!("Installed core {}", installed.display()),
                // A locked Windows core may prevent an update; retain an existing approved copy.
                // Without that fallback, installation must fail.
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
    // A symlink is not a fallback the runtime would accept.
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

fn publish_staged_binary(staged: &Path, target: &Path) -> Result<(), Error> {
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
            // A displaced sibling from an earlier fallback publish may still be lying around.
            let _ = std::fs::remove_file(target.with_extension(zenclash_service::CORE_DISPLACED_EXTENSION));
            return Ok(());
        };

        // Windows can rename a running executable but cannot overwrite it.
        // Displace it to a non-runnable name for cleanup on a later install.
        let displaced = target.with_extension(zenclash_service::CORE_DISPLACED_EXTENSION);
        if move_over(target, &displaced).is_err() {
            return Err(direct_error).with_context(|| format!("failed to publish {staged:?} at {target:?}"));
        }
        match move_over(staged, target) {
            Ok(()) => {
                // Still open while the displaced binary runs; the sweep on a later install gets it.
                let _ = std::fs::remove_file(&displaced);
                Ok(())
            }
            Err(publish_error) => {
                // Put the displaced file back so the name does not go empty; say so if even that failed.
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

fn wait_for_service_ready() -> Result<(), Error> {
    const READY_TIMEOUT: Duration = Duration::from_secs(45);
    const READY_INTERVAL: Duration = Duration::from_millis(250);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create service readiness runtime")?;
    runtime.block_on(async {
        zenclash_service::set_config(Some(zenclash_service::IpcConfig {
            default_timeout: Duration::from_millis(250),
            max_retries: 1,
            retry_delay: Duration::from_millis(25),
        }))
        .await;

        let deadline = Instant::now() + READY_TIMEOUT;
        let result = loop {
            if let Ok(response) = zenclash_service::get_version().await
                && response.code == 0
                && response.data.is_some_and(|info| {
                    info.supports_client(
                        zenclash_service::ProtocolVersion::current(),
                        zenclash_service::MIN_REQUIRED_SERVICE_REVISION,
                    )
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

        zenclash_service::set_config(None).await;
        result
    })
}

#[cfg(target_os = "macos")]
fn launchd_service_target() -> String {
    format!("system/{}", zenclash_service::MACOS_SERVICE_ID)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum LaunchdInstallPlan {
    SkipBootout,
    Bootout,
}

#[cfg(any(target_os = "macos", test))]
fn classify_launchd_service_probe(exit_code: Option<i32>, diagnostic: &str) -> Result<LaunchdInstallPlan, Error> {
    match exit_code {
        Some(0) => Ok(LaunchdInstallPlan::Bootout),
        Some(113) if diagnostic.contains("Could not find service") => Ok(LaunchdInstallPlan::SkipBootout),
        _ => Err(anyhow::anyhow!(
            "Unexpected launchctl service probe result (exit code: {:?}): {}",
            exit_code,
            diagnostic
        )),
    }
}

#[cfg(target_os = "macos")]
fn probe_launchd_service(debug: bool) -> Result<LaunchdInstallPlan, Error> {
    if debug {
        println!("Executing: launchctl print {}", launchd_service_target());
    }

    let output = std::process::Command::new("launchctl")
        .args(["print", &launchd_service_target()])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to probe launchd service: {}", e))?;
    let diagnostic = format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    classify_launchd_service_probe(output.status.code(), &diagnostic)
}

#[cfg(unix)]
fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.parse().ok()
}

// nix's `Group::from_gid` detects ERANGE through errno, but getgrgid_r reports it only in its
// return value, so a group record larger than nix's first buffer fails to resolve.
#[cfg(unix)]
fn lookup_group_name(gid: u32, mut buffer_size: usize) -> std::io::Result<Option<String>> {
    const MAX_BUFFER_SIZE: usize = 1024 * 1024;
    loop {
        let mut group = unsafe { std::mem::zeroed::<platform_lib::group>() };
        let mut buffer = vec![0 as platform_lib::c_char; buffer_size];
        let mut result = std::ptr::null_mut();
        let status =
            unsafe { platform_lib::getgrgid_r(gid, &mut group, buffer.as_mut_ptr(), buffer.len(), &mut result) };
        match status {
            0 if result.is_null() => return Ok(None),
            0 => {
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

#[cfg(unix)]
fn resolve_service_group_name() -> Result<String, Error> {
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

#[cfg(target_os = "macos")]
fn set_macos_owner(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::lchown;

    lchown(path, Some(0), Some(0)).with_context(|| format!("failed to set root:wheel owner on {path:?}"))
}

#[cfg(target_os = "macos")]
fn set_macos_owner_recursive(path: &Path) -> Result<(), Error> {
    set_macos_owner(path)?;
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        set_macos_owner_recursive(&entry?.path())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_macos_permissions(path: &Path, mode: u32) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set mode {mode:o} on {path:?}"))
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Error> {
    if zenclash_service::management::prepare_install_if_requested()? {
        return Ok(());
    }
    if run_maintenance_if_requested()? {
        return Ok(());
    }
    let mut options = parse_install_arguments(std::env::args_os().skip(1))?;
    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    let group_name = resolve_service_group_name()?;
    let debug = options.debug;
    let launchd_install_plan = probe_launchd_service(debug)?;
    let service_binary_path = bundled_service_binary()?;

    let bundle_path =
        PathBuf::from("/Library/PrivilegedHelperTools").join(format!("{}.bundle", zenclash_service::MACOS_SERVICE_ID));
    let contents_path = bundle_path.join("Contents");
    let macos_path = contents_path.join("MacOS");

    std::fs::create_dir_all(&macos_path).map_err(|e| anyhow::anyhow!("Failed to create bundle directories: {}", e))?;

    let target_binary_path = macos_path.join("zenclash-service");
    let staged = stage_binary(&service_binary_path, &target_binary_path)?;

    let info_plist_path = contents_path.join("Info.plist");

    let plist_dir = PathBuf::from("/Library/LaunchDaemons");
    if !plist_dir.exists() {
        std::fs::create_dir(&plist_dir).map_err(|e| anyhow::anyhow!("Failed to create plist directory: {}", e))?;
    }

    let plist_file = plist_dir.join(format!("{}.plist", zenclash_service::MACOS_SERVICE_ID));

    let launchd_plist_content = format!(
        include_str!("../../resources/launchd.plist.tmpl"),
        group_name = group_name,
        service_id = zenclash_service::MACOS_SERVICE_ID,
        app_bundle_id = zenclash_service::MACOS_APP_BUNDLE_ID,
        service_binary = target_binary_path.to_string_lossy(),
    );
    let info_plist_content = format!(
        include_str!("../../resources/info.plist.tmpl"),
        display_name = zenclash_service::SERVICE_DISPLAY_NAME,
        service_id = zenclash_service::MACOS_SERVICE_ID,
    );
    let plist_path = plist_file.to_string_lossy().into_owned();

    if launchd_install_plan == LaunchdInstallPlan::Bootout {
        // A loaded job may have lost its plist; only the label still reaches it.
        run_command("launchctl", &["bootout", &launchd_service_target()], debug)?;
    }
    shared::repair_active_owner_state()?;
    // Staged where the service is already down, so a core it was running no longer holds its file.
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target_binary_path)?;
    std::fs::write(&info_plist_path, info_plist_content)
        .with_context(|| format!("failed to write Info.plist {info_plist_path:?}"))?;
    File::create(&plist_file)
        .and_then(|mut file| file.write_all(launchd_plist_content.as_bytes()))
        .map_err(|e| anyhow::anyhow!("Failed to write plist file: {}", e))?;

    set_macos_permissions(&plist_file, 0o644)?;
    set_macos_owner(&plist_file)?;
    set_macos_permissions(&target_binary_path, 0o544)?;
    set_macos_owner(&target_binary_path)?;
    set_macos_permissions(&bundle_path, 0o755)?;
    set_macos_owner_recursive(&bundle_path)?;

    let launchd_target = launchd_service_target();
    run_command("launchctl", &["enable", &launchd_target], debug)?;
    run_command("launchctl", &["bootstrap", "system", &plist_path], debug)?;
    run_command("launchctl", &["start", zenclash_service::MACOS_SERVICE_ID], debug)?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Error> {
    if zenclash_service::management::prepare_install_if_requested()? {
        return Ok(());
    }
    if run_maintenance_if_requested()? {
        return Ok(());
    }
    let mut options = parse_install_arguments(std::env::args_os().skip(1))?;
    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    let group = resolve_service_group_name()?;
    let debug = options.debug;
    let source = bundled_service_binary()?;
    let install_dir = zenclash_service::prepare_service_install_directory()?;
    let target = install_dir.join("zenclash-service");
    let staged = stage_binary(&source, &target)?;
    let unit_name = format!("{}.service", zenclash_service::SERVICE_SLUG);
    let unit_path = PathBuf::from("/etc/systemd/system").join(&unit_name);

    let _ = run_command("systemctl", &["stop", &unit_name], debug);
    shared::repair_active_owner_state()?;
    // Staged where the service is already down, so a core it was running no longer holds its file.
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target)?;

    let unit_file_content = format!(
        include_str!("../../resources/systemd_service_unit.tmpl"),
        exec_start = target.to_string_lossy(),
        group = group,
        runtime_directory = zenclash_service::SERVICE_SLUG,
    );

    let mut unit_file =
        File::create(&unit_path).with_context(|| format!("failed to create systemd unit {unit_path:?}"))?;
    unit_file
        .write_all(unit_file_content.as_bytes())
        .with_context(|| format!("failed to write systemd unit {unit_path:?}"))?;
    unit_file
        .sync_all()
        .with_context(|| format!("failed to sync systemd unit {unit_path:?}"))?;

    run_command("systemctl", &["daemon-reload"], debug)?;
    run_command("systemctl", &["enable", &unit_name], debug)?;
    run_command("systemctl", &["start", &unit_name], debug)?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use platform_lib::{
        Error as WindowsServiceError,
        service::{ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState, ServiceType},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    use std::ffi::{OsStr, OsString};
    use std::{thread, time::Duration};

    if zenclash_service::management::prepare_install_if_requested()? {
        return Ok(());
    }
    if run_maintenance_if_requested()? {
        return Ok(());
    }
    let mut options = parse_install_arguments(std::env::args_os().skip(1))?;
    let _gate = enter_repair_gate()?;
    prepare_requested_cores(&mut options.cores)?;
    if !options.install_service {
        return install_service_cores(&options.cores);
    }
    if options.debug {
        println!(
            "Installing service with {} explicitly requested cores",
            options.cores.len()
        );
    }
    let source = bundled_service_binary()?;
    let install_dir = zenclash_service::prepare_service_install_directory()?;
    let target = install_dir.join("zenclash-service.exe");
    let staged = stage_binary(&source, &target)?;

    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)?;
    let start_type = if cfg!(feature = "development-channel") {
        ServiceStartType::OnDemand
    } else {
        ServiceStartType::AutoStart
    };
    let service_info = ServiceInfo {
        name: OsString::from(zenclash_service::WINDOWS_SERVICE_NAME),
        display_name: OsString::from(zenclash_service::SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    let service_access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::QUERY_CONFIG
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG;
    match service_manager.open_service(zenclash_service::WINDOWS_SERVICE_NAME, service_access) {
        Ok(service) => {
            const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
            let status = service.query_status()?;
            if options.ensure_service
                && status.current_state == ServiceState::Stopped
                && service
                    .query_config()?
                    .executable_path
                    .to_string_lossy()
                    .trim_matches('"')
                    == target.to_string_lossy()
                && installed_payload_matches(
                    &source,
                    &target,
                    &options.cores,
                    &zenclash_service::service_paths()?.core_dir(),
                )?
            {
                service.start(&Vec::<&OsStr>::new())?;
                wait_for_service_ready()?;
                remove_ordinary_file_if_exists(&staged)?;
                return Ok(());
            }
            if status.current_state != ServiceState::Stopped {
                if let Err(error) = service.stop()
                    && !matches!(
                        &error,
                        WindowsServiceError::Winapi(error)
                            if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE)
                    )
                {
                    return Err(error.into());
                }
                for _ in 0..200 {
                    if service.query_status()?.current_state == ServiceState::Stopped {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if service.query_status()?.current_state != ServiceState::Stopped {
                    bail!("timed out waiting for service to stop before replacement");
                }
            }

            // Staged where the service is already down, so a core it was running no longer holds
            // its file open.
            shared::repair_active_owner_state()?;
            install_service_cores(&options.cores)?;
            publish_staged_binary(&staged, &target)?;
            service.change_config(&service_info)?;
            configure_windows_service_recovery(&service)?;
            service.start(&Vec::<&OsStr>::new())?;
            wait_for_service_ready()?;
            return Ok(());
        }
        Err(WindowsServiceError::Winapi(error)) if error.raw_os_error() == Some(1060) => {}
        Err(error) => return Err(error.into()),
    }

    shared::repair_active_owner_state()?;
    install_service_cores(&options.cores)?;
    publish_staged_binary(&staged, &target)?;
    let start_access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START;
    let service = service_manager.create_service(&service_info, start_access)?;

    service.set_description("ZenClash Service manages the Mihomo core")?;
    configure_windows_service_recovery(&service)?;
    service.start(&Vec::<&OsStr>::new())?;
    wait_for_service_ready()?;

    Ok(())
}

#[cfg(windows)]
fn configure_windows_service_recovery(service: &platform_lib::service::Service) -> platform_lib::Result<()> {
    use platform_lib::service::{ServiceAction, ServiceActionType, ServiceFailureActions, ServiceFailureResetPeriod};
    use std::time::Duration;

    let actions = [5, 10, 30]
        .into_iter()
        .map(|delay_secs| ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(delay_secs),
        })
        .collect();

    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(actions),
    })?;
    service.set_failure_actions_on_non_crash_failures(true)?;

    Ok(())
}

#[cfg(test)]
mod install_core_tests {
    use super::install_core;
    use std::path::PathBuf;

    fn scratch(label: &str) -> anyhow::Result<PathBuf> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("install-core-{label}-{}-{timestamp}", std::process::id()));
        std::fs::create_dir_all(root.join("cores"))?;
        Ok(root)
    }

    fn combined_options(sources: &[PathBuf]) -> anyhow::Result<super::InstallOptions> {
        let mut arguments = vec![std::ffi::OsString::from("--install-service")];
        for source in sources {
            let digest: String = super::sha256(source)?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            arguments.extend([
                "--install-core".into(),
                source.as_os_str().to_owned(),
                "--sha256".into(),
                digest.into(),
            ]);
        }
        super::parse_install_arguments(arguments.into_iter())
    }

    #[test]
    fn stopped_service_can_be_reused_only_with_matching_service_and_cores() -> anyhow::Result<()> {
        let root = scratch("ensure payload")?;
        let source = root.join("bundled-service");
        let target = root.join("installed-service");
        let core = root.join(super::core_file_name("mihomo"));
        std::fs::write(&source, b"service")?;
        std::fs::write(&target, b"service")?;
        std::fs::write(&core, b"core")?;
        let options = combined_options(std::slice::from_ref(&core))?;
        assert!(super::installed_payload_matches(
            &source,
            &target,
            &options.cores,
            &root
        )?);
        std::fs::write(&core, b"different core")?;
        assert!(!super::installed_payload_matches(
            &source,
            &target,
            &options.cores,
            &root
        )?);
        std::fs::write(&core, b"core")?;
        std::fs::write(&target, b"old service")?;
        assert!(!super::installed_payload_matches(
            &source,
            &target,
            &options.cores,
            &root
        )?);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn combined_install_publishes_both_attested_cores_from_a_writable_directory() -> anyhow::Result<()> {
        let root = scratch("scoop install")?;
        let stable = root.join(super::core_file_name("mihomo"));
        let alpha = root.join(super::core_file_name("mihomo-alpha"));
        std::fs::write(&stable, b"stable core")?;
        std::fs::write(&alpha, b"alpha core")?;
        let mut options = combined_options(&[stable, alpha])?;
        assert!(options.install_service);
        super::prepare_requested_cores(&mut options.cores)?;
        let installed = super::publish_requested_cores(&root.join("cores"), &options.cores)?;
        assert_eq!(installed.len(), 2);
        assert_eq!(std::fs::read(&installed[0])?, b"stable core");
        assert_eq!(std::fs::read(&installed[1])?, b"alpha core");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn combined_install_preflight_rejects_a_missing_or_mismatched_second_core() -> anyhow::Result<()> {
        let root = scratch("preflight")?;
        let stable = root.join(super::core_file_name("mihomo"));
        let alpha = root.join(super::core_file_name("mihomo-alpha"));
        std::fs::write(&stable, b"stable core")?;
        std::fs::write(&alpha, b"alpha core")?;
        let mut options = combined_options(&[stable, alpha.clone()])?;
        std::fs::write(&alpha, b"changed alpha")?;
        let error =
            super::prepare_requested_cores(&mut options.cores).expect_err("must reject before stopping the service");
        assert!(error.to_string().contains("attested sha256"), "got {error:#}");
        std::fs::remove_file(alpha)?;
        assert!(super::prepare_requested_cores(&mut options.cores).is_err());
        assert!(std::fs::read_dir(root.join("cores"))?.next().is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn explicit_core_failure_after_preflight_does_not_accept_the_previous_copy() -> anyhow::Result<()> {
        let root = scratch("changed-after-preflight")?;
        let source = root.join(super::core_file_name("mihomo"));
        let previous = root.join("cores").join(source.file_name().unwrap());
        std::fs::write(&source, b"new core")?;
        std::fs::write(&previous, b"old core")?;
        let mut options = combined_options(std::slice::from_ref(&source))?;
        super::prepare_requested_cores(&mut options.cores)?;
        std::fs::write(&source, b"replaced after preflight")?;
        let error = super::publish_requested_cores(&root.join("cores"), &options.cores)
            .expect_err("an existing approved copy must not hide an explicit installation failure");
        assert!(error.to_string().contains("attested sha256"), "got {error:#}");
        assert_eq!(std::fs::read(previous)?, b"old core");
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

        let target = install_core(&source, &root.join("cores"), source.file_name().unwrap(), None)?;

        assert_eq!(std::fs::read(&target)?, b"core bytes");
        assert_eq!(
            std::fs::metadata(&source)?.modified()?,
            std::fs::metadata(&target)?.modified()?,
            "the copy must carry the source's modified time for drift detection"
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
    fn a_wrong_attestation_publishes_nothing() -> anyhow::Result<()> {
        let root = scratch("attestation")?;
        let source = root.join("mihomo");
        std::fs::write(&source, b"core bytes")?;

        let error = install_core(
            &source,
            &root.join("cores"),
            source.file_name().unwrap(),
            Some(&[0u8; 32]),
        )
        .expect_err("must refuse");

        assert!(error.to_string().contains("attested sha256"), "got {error:#}");
        assert!(
            std::fs::read_dir(root.join("cores"))?.next().is_none(),
            "nothing may land in the core directory on a failed attestation"
        );
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn an_unchanged_core_is_recognized_without_republishing() -> anyhow::Result<()> {
        let root = scratch("idempotent")?;
        let source = root.join("mihomo");
        std::fs::write(&source, b"core bytes")?;
        let cores = root.join("cores");

        let first = install_core(&source, &cores, source.file_name().unwrap(), None)?;
        #[cfg(unix)]
        let first_metadata = std::fs::metadata(&first)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source)?
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))?;
        let second = install_core(&source, &cores, source.file_name().unwrap(), None)?;

        assert_eq!(first, second);
        assert_eq!(std::fs::read(&second)?, b"core bytes");
        assert_eq!(
            std::fs::metadata(&second)?.modified()?,
            std::fs::metadata(&source)?.modified()?,
            "an unchanged core must still refresh its modified time"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            let second_metadata = std::fs::metadata(&second)?;
            assert_eq!(
                first_metadata.ino(),
                second_metadata.ino(),
                "must not republish the core"
            );
            assert_eq!(second_metadata.permissions().mode() & 0o777, 0o550);
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn a_core_named_like_a_bookkeeping_leftover_is_refused() -> anyhow::Result<()> {
        let root = scratch("bookkeeping-name")?;
        for name in ["mihomo.next", "mihomo.old"] {
            let source = root.join(name);
            std::fs::write(&source, b"core bytes")?;

            let error =
                install_core(&source, &root.join("cores"), source.file_name().unwrap(), None).expect_err("must refuse");

            assert!(error.to_string().contains("bookkeeping suffix"), "got {error:#}");
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}

#[cfg(test)]
mod core_install_argument_tests {
    use super::{parse_install_arguments, parse_sha256_hex};
    use std::ffi::OsString;

    fn parse(arguments: &[&str]) -> anyhow::Result<Vec<super::CoreInstallRequest>> {
        Ok(parse_install_arguments(arguments.iter().map(OsString::from))?.cores)
    }

    #[test]
    fn combined_install_keeps_the_service_step() -> anyhow::Result<()> {
        let options = parse_install_arguments(
            [
                "--install-service",
                "--install-core",
                "/dl/mihomo",
                "--sha256",
                &"ab".repeat(32),
            ]
            .into_iter()
            .map(OsString::from),
        )?;
        assert!(
            options.install_service,
            "explicit cores must not turn combined installation into a core-only update"
        );
        assert_eq!(options.cores.len(), 1);
        Ok(())
    }

    #[test]
    fn legacy_install_modes_are_preserved() -> anyhow::Result<()> {
        assert!(parse_install_arguments(std::iter::empty())?.install_service);
        let debug = parse_install_arguments(["--debug"].into_iter().map(OsString::from))?;
        assert!(debug.install_service && debug.debug);
        assert!(
            !parse_install_arguments(["--install-core", "/dl/core"].into_iter().map(OsString::from))?.install_service
        );
        Ok(())
    }

    #[test]
    fn unknown_install_options_are_rejected() {
        assert!(parse_install_arguments(["--install-servce"].into_iter().map(OsString::from)).is_err());
    }

    #[test]
    fn each_digest_attests_the_core_it_follows() -> anyhow::Result<()> {
        let digest = "ab".repeat(32);
        let requests = parse(&[
            "--install-core",
            "/dl/mihomo",
            "--sha256",
            &digest,
            "--install-core",
            "/dl/other",
        ])?;

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].source, std::path::Path::new("/dl/mihomo"));
        assert_eq!(requests[0].sha256, Some([0xab; 32]));
        assert_eq!(requests[1].sha256, None);
        Ok(())
    }

    #[test]
    fn a_digest_with_no_core_to_attest_is_refused() {
        assert!(parse(&["--sha256", &"ab".repeat(32)]).is_err());
    }

    #[test]
    fn a_second_digest_for_the_same_core_is_refused() {
        let digest = "ab".repeat(32);
        assert!(parse(&["--install-core", "/dl/core", "--sha256", &digest, "--sha256", &digest]).is_err());
    }

    #[test]
    fn a_missing_path_or_digest_value_is_refused() {
        assert!(parse(&["--install-core"]).is_err());
        assert!(parse(&["--install-core", "/dl/core", "--sha256"]).is_err());
    }

    #[test]
    fn digests_must_be_exactly_64_hex_characters() {
        assert!(parse_sha256_hex(&"ab".repeat(32)).is_ok());
        assert!(parse_sha256_hex(&"AB".repeat(32)).is_ok());
        assert!(parse_sha256_hex("ab").is_err());
        assert!(parse_sha256_hex(&"zz".repeat(32)).is_err());
        assert!(parse_sha256_hex(&format!("{}g", "ab".repeat(31))).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_launchd_service_skips_bootout() {
        let plan = classify_launchd_service_probe(
            Some(113),
            "Could not find service \"org.zenclash.app.service\" in domain for system",
        )
        .unwrap();

        assert_eq!(plan, LaunchdInstallPlan::SkipBootout);
    }

    #[test]
    fn loaded_launchd_service_runs_bootout() {
        let plan = classify_launchd_service_probe(Some(0), "").unwrap();

        assert_eq!(plan, LaunchdInstallPlan::Bootout);
    }

    #[test]
    fn unexpected_launchd_exit_is_an_error() {
        let result = classify_launchd_service_probe(Some(5), "Could not find service");

        assert!(result.is_err());
    }

    #[test]
    fn unexpected_launchd_diagnostic_is_an_error() {
        let result = classify_launchd_service_probe(Some(113), "Operation not permitted");

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn group_lookup_grows_a_buffer_too_small_for_the_record() {
        let (gid, name) = if cfg!(target_os = "macos") {
            (20, "staff")
        } else {
            (0, "root")
        };

        assert_eq!(lookup_group_name(gid, 8).unwrap().as_deref(), Some(name));
    }
}
