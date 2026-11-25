#![deny(clippy::undocumented_unsafe_blocks)]
// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Unprivileged preparation and privileged publication share one installation contract.
#[cfg(any(feature = "client", test))]
use anyhow::bail;
use anyhow::{Context as _, Result};
use sha2::{Digest as _, Sha256};
use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[cfg(any(feature = "client", test))]
use std::ffi::OsString;

#[derive(Debug, Clone)]
pub struct CoreSource {
    pub name: String,
    pub path: PathBuf,
}

const INSTALLATION_FAILURE_PREFIX: &str = "ZENCLASH_INSTALLATION_FAILURE_V1=";
#[cfg(any(windows, test))]
const AUTHORIZATION_FAILURE_PREFIX: &str = "ZENCLASH_AUTHORIZATION_FAILURE_V1=";

#[cfg(any(windows, test))]
#[derive(serde::Serialize, serde::Deserialize)]
struct AuthorizationFailure {
    win32_error: i32,
}

/// The installer's final inspection, preserved across the preparation subprocess.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct InstallationVerificationError {
    pub status: crate::InstallationStatus,
}

impl std::fmt::Display for InstallationVerificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "installation finished without satisfying the requested service and cores"
        )?;
        for core in &self.status.cores {
            match &core.availability {
                crate::CoreAvailability::Ready => {}
                crate::CoreAvailability::Missing => write!(formatter, "; {}: missing", core.name)?,
                crate::CoreAvailability::DigestMismatch => write!(formatter, "; {}: digest mismatch", core.name)?,
                crate::CoreAvailability::Rejected { reason } => {
                    write!(formatter, "; {}: rejected: {reason}", core.name)?;
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for InstallationVerificationError {}

#[cfg(any(feature = "client", test))]
impl InstallationVerificationError {
    fn write_report(&self, mut output: impl std::io::Write) -> Result<()> {
        output.write_all(INSTALLATION_FAILURE_PREFIX.as_bytes())?;
        serde_json::to_writer(&mut output, self)?;
        output.write_all(b"\n")?;
        Ok(())
    }
}

///
/// # Errors
/// Returns source validation, preparation, native authorization or installer-process errors; partial maintenance may have occurred.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

///
/// # Errors
/// Returns source validation, preparation, native authorization or installer-process errors; partial maintenance may have occurred.
pub fn install(installer: &Path, cores: &[CoreSource], core_only: bool, gid: Option<u32>, prompt: &str) -> Result<()> {
    let mut command = Command::new(installer);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command.arg("--prepare-install");
    if core_only {
        command.arg("--core-only");
    }
    if let Some(gid) = gid {
        command.args(["--gid", &gid.to_string()]);
    }
    command.args(["--prompt", prompt]);
    for core in cores {
        command.arg("--core").arg(&core.name).arg(&core.path);
    }
    let output = command.output().context("failed to launch the service installer")?;
    installation_result(output)
}

fn installation_result(output: Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    for line in output.stdout.split(|byte| *byte == b'\n') {
        #[cfg(any(windows, test))]
        if let Some(report) = line.strip_prefix(AUTHORIZATION_FAILURE_PREFIX.as_bytes())
            && let Ok(failure) = serde_json::from_slice::<AuthorizationFailure>(report)
            && failure.win32_error > 0
        {
            return Err(std::io::Error::from_raw_os_error(failure.win32_error))
                .context("service installer authorization failed");
        }
        if let Some(report) = line.strip_prefix(INSTALLATION_FAILURE_PREFIX.as_bytes())
            && let Ok(failure) = serde_json::from_slice::<InstallationVerificationError>(report)
        {
            return Err(failure.into());
        }
    }
    anyhow::bail!(
        "service installation failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(any(feature = "client", test))]
struct Staging(PathBuf);
#[cfg(any(feature = "client", test))]
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[cfg(any(feature = "client", test))]
impl Staging {
    fn new() -> Result<Self> {
        for index in 0..100 {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let root = std::env::temp_dir().join(format!("zenclash-install-{}-{nonce}-{index}", std::process::id()));
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let mut builder = builder;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&root) {
                Ok(()) => return Ok(Self(root)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not create installation staging directory")
    }
}

#[cfg(any(feature = "client", test))]
struct PreparedCores {
    staging: Staging,
    requirements: Vec<crate::CoreRequirement>,
    arguments: Vec<OsString>,
}

#[cfg(any(feature = "client", test))]
impl PreparedCores {
    fn new(cores: &[CoreSource], core_only: bool) -> Result<Self> {
        let staging = Staging::new()?;
        let mut requirements = Vec::new();
        let mut elevated: Vec<OsString> = if core_only {
            Vec::new()
        } else {
            vec!["--install-service".into()]
        };
        for core in cores {
            anyhow::ensure!(
                [
                    format!("mihomo{}", std::env::consts::EXE_SUFFIX),
                    format!("mihomo-alpha{}", std::env::consts::EXE_SUFFIX)
                ]
                .contains(&core.name),
                "unsupported core name {}",
                core.name
            );
            anyhow::ensure!(
                core.path.is_file(),
                "requested core {} is unavailable at {}",
                core.name,
                core.path.display()
            );
            let target = staging.0.join(&core.name);
            anyhow::ensure!(!target.exists(), "duplicate core name {}", core.name);
            std::fs::copy(&core.path, &target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
            let digest = sha256_file(&target)?;
            elevated.extend([
                "--install-core".into(),
                target.into_os_string(),
                "--sha256".into(),
                digest.clone().into(),
            ]);
            requirements.push(crate::CoreRequirement {
                name: core.name.clone(),
                sha256: Some(digest),
            });
        }
        anyhow::ensure!(
            !requirements.is_empty(),
            "no core executable is available; run prebuild or restore the bundled cores"
        );
        Ok(Self {
            staging,
            requirements,
            arguments: elevated,
        })
    }
}

/// Called by the installer before any privileged maintenance or argument parsing.
#[cfg(feature = "client")]
///
/// # Errors
/// Returns source validation, preparation, native authorization or installer-process errors; partial maintenance may have occurred.
pub fn prepare_install_if_requested() -> Result<bool> {
    prepare_install_if_requested_with_args(std::env::args_os().skip(1))
}

/// Variant of [`prepare_install_if_requested`] accepting explicit arguments (e.g. after stripping an `install` subcommand).
#[cfg(feature = "client")]
///
/// # Errors
/// Returns source validation, preparation, native authorization or installer-process errors; partial maintenance may have occurred.
pub fn prepare_install_if_requested_with_args<I>(arguments: I) -> Result<bool>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--prepare-install")) {
        return Ok(false);
    }
    let mut cores = Vec::new();
    let mut ensure = false;
    let mut core_only = false;
    let mut prompt = "Install ZenClash service and approved cores".to_owned();
    // SAFETY: `getgid` has no preconditions or memory effects.
    #[cfg(unix)]
    let mut gid = unsafe { platform_lib::getgid() };
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--ensure") => ensure = true,
            Some("--core-only") => core_only = true,
            Some("--core") => {
                let name = arguments
                    .next()
                    .context("--core requires a name and path")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid core name"))?;
                let path = arguments.next().context("--core requires a path")?.into();
                cores.push(CoreSource { name, path });
            }
            Some("--prompt") => {
                prompt = arguments
                    .next()
                    .context("--prompt requires text")?
                    .to_string_lossy()
                    .into_owned()
            }
            Some("--gid") => {
                let value = arguments
                    .next()
                    .context("--gid requires an ID")?
                    .to_string_lossy()
                    .parse::<u32>()?;
                #[cfg(unix)]
                {
                    gid = value;
                }
                #[cfg(windows)]
                let _ = value;
            }
            _ => bail!("unknown preparation argument: {argument:?}"),
        }
    }
    let installer = std::env::current_exe()?;
    let service = installer.with_file_name(format!("zenclash-service{}", std::env::consts::EXE_SUFFIX));
    let PreparedCores {
        staging,
        requirements,
        arguments: elevated,
    } = PreparedCores::new(&cores, core_only)?;
    let service_digest = if core_only { None } else { Some(sha256_file(&service)?) };
    let runtime = tokio::runtime::Runtime::new()?;
    let verified = |status: &crate::InstallationStatus| {
        status.satisfies(&requirements)
            && service_digest
                .as_ref()
                .is_none_or(|digest| digest == &status.service_sha256)
    };
    if ensure
        && runtime
            .block_on(crate::client::inspect_installation_with_digest(
                &requirements,
                !core_only,
            ))
            .is_ok_and(|status| verified(&status))
    {
        return Ok(true);
    }
    #[cfg(windows)]
    let elevated = {
        let mut arguments = elevated;
        if ensure && !core_only {
            arguments.push("--ensure-service".into());
        }
        arguments
    };
    // Both executables must be beside each other; a temporary copy also avoids macOS TCC paths.
    let installer_name = installer.file_name().context("installer has no name")?;
    let service_name = service.file_name().context("service has no name")?;
    let unified_binary = installer_name == service_name;
    let staged_installer = staging.0.join(installer_name);
    std::fs::copy(&installer, &staged_installer)?;
    if !core_only && !unified_binary {
        std::fs::copy(&service, staging.0.join(service_name))?;
    }
    let elevated = if unified_binary {
        let mut with_subcommand = Vec::with_capacity(elevated.len() + 1);
        with_subcommand.push(OsString::from("install"));
        with_subcommand.extend(elevated);
        with_subcommand
    } else {
        elevated
    };
    #[cfg(unix)]
    elevate(&staged_installer, &elevated, gid, &prompt)?;
    #[cfg(windows)]
    if let Err(error) = elevate(&staged_installer, &elevated, &prompt) {
        // The GUI runs this preparation executable as a child. Preserve native
        // cancellation across that boundary instead of guessing from stderr text.
        report_authorization_failure(&error, std::io::stdout().lock())?;
        return Err(error);
    }
    // Core-only publication is verified by the privileged installer and must also work offline.
    if core_only {
        return Ok(true);
    }
    let status = runtime
        .block_on(crate::client::inspect_installation_with_digest(&requirements, true))
        .context("installation finished but its approved cores could not be verified")?;
    if !verified(&status) {
        let failure = InstallationVerificationError { status };
        failure.write_report(std::io::stdout().lock())?;
        return Err(failure.into());
    }
    Ok(true)
}

#[cfg(any(all(windows, feature = "client"), test))]
fn report_authorization_failure(error: &anyhow::Error, mut output: impl std::io::Write) -> Result<()> {
    if let Some(win32_error) = error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
    {
        output.write_all(AUTHORIZATION_FAILURE_PREFIX.as_bytes())?;
        serde_json::to_writer(&mut output, &AuthorizationFailure { win32_error })?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(any(all(feature = "client", target_os = "macos"), test))]
fn shell_quote(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

/// Classification of a `pkexec` invocation on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkexecOutcome {
    /// Privileged command exited with code 0.
    Succeeded,
    /// Exit code 126: user cancelled/dismissed authentication; do not retry with `sudo`.
    CancelledByUser,
    /// Exit code 127 or missing `pkexec`: no polkit agent available; fall back to `sudo -n` -> `sudo -S`.
    FallbackToSudo,
    /// Privileged command ran and failed with a non-fallback exit status.
    Failed(Option<i32>),
}

/// Classification of a non-interactive `sudo -n` attempt on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoNonInteractiveOutcome {
    /// Privileged command exited with code 0.
    Succeeded,
    /// `sudo -n` failed because interactive password authentication is required; fall back to `sudo -S`.
    FallbackToSudoStdin,
    /// `sudo` executable is not installed on the system.
    Unavailable,
    /// Privileged command ran via `sudo -n` and failed with a real installer error.
    Failed(Option<i32>),
}

/// Classifies a `pkexec` spawn/exit outcome according to polkit exit-code semantics.
pub fn classify_pkexec_outcome(result: &std::io::Result<Option<i32>>) -> PkexecOutcome {
    match result {
        Ok(Some(0)) => PkexecOutcome::Succeeded,
        Ok(Some(126)) => PkexecOutcome::CancelledByUser,
        Ok(Some(127)) => PkexecOutcome::FallbackToSudo,
        Ok(code) => PkexecOutcome::Failed(*code),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PkexecOutcome::FallbackToSudo,
        Err(_) => PkexecOutcome::Failed(None),
    }
}

/// Classifies a `sudo -n` outcome to distinguish password-required fallback from real command failures.
pub fn classify_sudo_non_interactive_outcome(
    result: &std::io::Result<(Option<i32>, String)>,
) -> SudoNonInteractiveOutcome {
    match result {
        Ok((Some(0), _)) => SudoNonInteractiveOutcome::Succeeded,
        Ok((Some(1), stderr)) => {
            let lower = stderr.to_ascii_lowercase();
            if lower.trim().is_empty()
                || lower.contains("password")
                || lower.contains("askpass")
                || lower.contains("terminal")
            {
                SudoNonInteractiveOutcome::FallbackToSudoStdin
            } else {
                SudoNonInteractiveOutcome::Failed(Some(1))
            }
        }
        Ok((code, _)) => SudoNonInteractiveOutcome::Failed(*code),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SudoNonInteractiveOutcome::Unavailable,
        Err(_) => SudoNonInteractiveOutcome::Failed(None),
    }
}

#[cfg(any(all(unix, not(target_os = "macos"), feature = "client"), test))]
fn run_linux_elevation_chain_with(
    mut run_pkexec: impl FnMut() -> std::io::Result<Option<i32>>,
    mut run_sudo_non_interactive: impl FnMut() -> std::io::Result<(Option<i32>, String)>,
    mut run_sudo_stdin: impl FnMut() -> std::io::Result<Option<i32>>,
) -> Result<()> {
    match classify_pkexec_outcome(&run_pkexec()) {
        PkexecOutcome::Succeeded => return Ok(()),
        PkexecOutcome::CancelledByUser => {
            bail!("privilege elevation cancelled by user (pkexec exit code 126)");
        }
        PkexecOutcome::Failed(code) => {
            bail!("elevated installer via pkexec failed with exit code {code:?}");
        }
        PkexecOutcome::FallbackToSudo => {}
    }

    match classify_sudo_non_interactive_outcome(&run_sudo_non_interactive()) {
        SudoNonInteractiveOutcome::Succeeded => return Ok(()),
        SudoNonInteractiveOutcome::Unavailable => {
            bail!("neither pkexec nor sudo is available for privilege elevation");
        }
        SudoNonInteractiveOutcome::Failed(code) => {
            bail!("elevated installer via sudo -n failed with exit code {code:?}");
        }
        SudoNonInteractiveOutcome::FallbackToSudoStdin => {}
    }

    match run_sudo_stdin() {
        Ok(Some(0)) => Ok(()),
        Ok(code) => bail!("elevated installer via sudo -S failed with exit code {code:?}"),
        Err(error) => Err(error).context("failed to execute sudo -S for privilege elevation"),
    }
}

#[cfg(all(unix, not(target_os = "macos"), feature = "client"))]
fn elevate_linux(installer: &Path, arguments: &[OsString]) -> Result<()> {
    use std::io::Write as _;
    use std::process::Stdio;

    run_linux_elevation_chain_with(
        || {
            Command::new("pkexec")
                .arg(installer)
                .args(arguments)
                .status()
                .map(|status| status.code())
        },
        || {
            let output = Command::new("sudo")
                .arg("-n")
                .arg("--")
                .arg(installer)
                .args(arguments)
                .output()?;
            Ok((
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        },
        || {
            if let Ok(password) = std::env::var("ZENCLASH_SUDO_PASSWORD") {
                let mut child = Command::new("sudo")
                    .args(["-S", "-p", "", "--"])
                    .arg(installer)
                    .args(arguments)
                    .env_remove("ZENCLASH_SUDO_PASSWORD")
                    .stdin(Stdio::piped())
                    .spawn()?;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(password.as_bytes());
                    let _ = stdin.write_all(b"\n");
                }
                child.wait().map(|status| status.code())
            } else {
                Command::new("sudo")
                    .args(["-S", "-p", "", "--"])
                    .arg(installer)
                    .args(arguments)
                    .status()
                    .map(|status| status.code())
            }
        },
    )
}

#[cfg(all(unix, feature = "client"))]
fn elevate(installer: &Path, arguments: &[OsString], gid: u32, prompt: &str) -> Result<()> {
    // SAFETY: `geteuid` has no preconditions or memory effects.
    if unsafe { platform_lib::geteuid() } == 0 {
        let status = Command::new(installer)
            .args(arguments)
            .env("ZENCLASH_SERVICE_GID", gid.to_string())
            .status()?;
        anyhow::ensure!(status.success(), "elevated installer failed with {status}");
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let mut shell = format!(
            "cd /; ZENCLASH_SERVICE_GID={gid} {}",
            shell_quote(installer.as_os_str())
        );
        for argument in arguments {
            shell.push(' ');
            shell.push_str(&shell_quote(argument));
        }
        let script = "on run argv\ndo shell script (item 1 of argv) with administrator privileges with prompt (item 2 of argv)\nend run";
        let status = Command::new("osascript")
            .args(["-e", script, &shell, prompt])
            .status()?;
        anyhow::ensure!(status.success(), "elevated installer failed with {status}");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        elevate_linux(installer, arguments)
    }
}

#[cfg(all(windows, feature = "client"))]
fn elevate(installer: &Path, arguments: &[OsString], _prompt: &str) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::{
        ffi::OsStrExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    };
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::{
            Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize},
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    };
    struct ComApartment(bool);
    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances this thread's successful CoInitializeEx, including S_FALSE.
                unsafe { CoUninitialize() };
            }
        }
    }
    let wide = |value: &OsStr| value.encode_wide().chain([0]).collect::<Vec<_>>();
    let command_line = arguments
        .iter()
        .map(|arg| windows_quote(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let verb = wide(OsStr::new("runas"));
    let file = wide(installer.as_os_str());
    let parameters = wide(OsStr::new(&command_line));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // ShellExecuteExW may delegate to a COM shell extension. A preexisting apartment
    // can reject this mode; only a successful initialization must be balanced.
    // SAFETY: `CoInitializeEx` has no pointer preconditions; the boolean records
    // whether the apartment must be uninitialized on drop.
    let _apartment = ComApartment(unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        ) >= 0
    });
    // SAFETY: all UTF-16 strings and the correctly sized structure live through this synchronous call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to elevate the service installer");
    }
    anyhow::ensure!(
        !info.hProcess.is_null(),
        "elevation did not start the service installer"
    );
    // SAFETY: SEE_MASK_NOCLOSEPROCESS returns an owned handle; this is its only owner.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    // SAFETY: the owned process handle stays valid until after the wait and exit-code query.
    if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(std::io::Error::last_os_error()).context("cannot wait for the elevated installer");
    }
    let mut code = 0;
    // SAFETY: code points to writable storage and the owned process handle is valid.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(std::io::Error::last_os_error()).context("cannot read the elevated installer's exit code");
    }
    anyhow::ensure!(code == 0, "elevated installer failed with exit code {code}");
    Ok(())
}

#[cfg(any(all(windows, feature = "client"), test))]
fn windows_quote(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        result.push(c);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_authorization_cancellation_survives_the_preparation_process() -> Result<()> {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt as _;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt as _;
        let cancelled = anyhow::Error::new(std::io::Error::from_raw_os_error(1223))
            .context("failed to elevate the service installer");
        let mut stdout = Vec::new();
        report_authorization_failure(&cancelled, &mut stdout)?;
        let error = installation_result(Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout,
            stderr: b"localized diagnostics".to_vec(),
        })
        .expect_err("cancellation must remain a native error")
        .context("install failed");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(1223)
        );
        Ok(())
    }

    #[test]
    fn diagnostics_cannot_be_mistaken_for_native_authorization_cancellation() -> Result<()> {
        let mut stdout = Vec::new();
        report_authorization_failure(&anyhow::anyhow!("installer exited with status 1223"), &mut stdout)?;
        assert!(stdout.is_empty());
        Ok(())
    }

    #[test]
    fn installation_refusal_survives_subprocess_output_and_error_context() -> Result<()> {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt as _;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt as _;

        let reason = "path C:\\用户\\core is writable by Everyone\npermissions rejected";
        let failure = InstallationVerificationError {
            status: crate::InstallationStatus {
                service_sha256: "service digest".into(),
                protocol: crate::ProtocolInfo::current(),
                cores: vec![crate::CoreInspection {
                    name: "mihomo.exe".into(),
                    availability: crate::CoreAvailability::Rejected { reason: reason.into() },
                }],
                core_busy: true,
            },
        };
        let mut stdout = b"installer diagnostics\n".to_vec();
        failure.write_report(&mut stdout)?;
        assert!(
            installation_result(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.clone(),
                stderr: Vec::new(),
            })
            .is_ok()
        );
        let result = installation_result(Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout,
            stderr: b"Error: installation finished without satisfying the requested service and cores".to_vec(),
        })
        .context("install service failed");
        let error = result.expect_err("a rejected core must not be reported as installed");
        let report = error
            .downcast_ref::<InstallationVerificationError>()
            .expect("installation must preserve its structured inspection");
        assert_eq!(report.status.cores, failure.status.cores);
        assert_eq!(report.status.service_sha256, failure.status.service_sha256);
        assert!(report.status.core_busy);
        assert!(format!("{error:#}").contains(reason));
        Ok(())
    }

    #[test]
    fn unstructured_installer_failures_keep_their_diagnostics() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt as _;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt as _;

        for stdout in [
            "legacy installer output".to_owned(),
            format!("{INSTALLATION_FAILURE_PREFIX}invalid json"),
        ] {
            let error = installation_result(Output {
                status: std::process::ExitStatus::from_raw(1),
                stdout: stdout.as_bytes().to_vec(),
                stderr: b"elevation cancelled".to_vec(),
            })
            .expect_err("failed installers must remain errors");
            assert!(error.downcast_ref::<InstallationVerificationError>().is_none());
            assert!(error.to_string().contains(&stdout));
            assert!(error.to_string().contains("elevation cancelled"));
        }
    }

    #[test]
    fn preparation_attests_staged_bytes_before_elevation() -> Result<()> {
        let source = Staging::new()?;
        let name = format!("mihomo{}", std::env::consts::EXE_SUFFIX);
        let path = source.0.join("core with spaces and 'quotes'");
        std::fs::write(&path, b"abc")?;
        let plan = PreparedCores::new(
            &[CoreSource {
                name: name.clone(),
                path: path.clone(),
            }],
            false,
        )?;
        std::fs::write(&path, b"changed source")?;
        assert_eq!(plan.requirements.len(), 1);
        assert_eq!(
            plan.requirements[0].sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(std::fs::read(plan.staging.0.join(&name))?, b"abc");
        assert_eq!(plan.arguments[0], "--install-service");
        assert_eq!(plan.arguments[1], "--install-core");
        assert_eq!(plan.arguments[3], "--sha256");
        assert_eq!(plan.arguments.len(), 5);
        let staged = plan.staging.0.clone();
        drop(plan);
        assert!(!staged.exists());
        assert!(PreparedCores::new(&[], false).is_err());
        assert!(
            PreparedCores::new(
                &[CoreSource {
                    name,
                    path: source.0.join("missing")
                }],
                false
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn elevation_quotes_shell_and_windows_arguments() {
        assert_eq!(shell_quote(std::ffi::OsStr::new("a'b $HOME")), "'a'\\''b $HOME'");
        assert_eq!(windows_quote(r#"C:\core's path\"#), r#""C:\core's path\\""#);
        assert_eq!(windows_quote(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn linux_pkexec_exit_codes_classify_cancel_fallback_and_failure() {
        assert_eq!(classify_pkexec_outcome(&Ok(Some(0))), PkexecOutcome::Succeeded);
        assert_eq!(classify_pkexec_outcome(&Ok(Some(126))), PkexecOutcome::CancelledByUser);
        assert_eq!(classify_pkexec_outcome(&Ok(Some(127))), PkexecOutcome::FallbackToSudo);
        assert_eq!(
            classify_pkexec_outcome(&Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "pkexec not found"
            ))),
            PkexecOutcome::FallbackToSudo
        );
        assert_eq!(classify_pkexec_outcome(&Ok(Some(1))), PkexecOutcome::Failed(Some(1)));
        assert_eq!(classify_pkexec_outcome(&Ok(Some(75))), PkexecOutcome::Failed(Some(75)));
    }

    #[test]
    fn linux_elevation_stops_on_pkexec_user_cancel_126() {
        let called = std::cell::RefCell::new(Vec::new());
        let error = run_linux_elevation_chain_with(
            || {
                called.borrow_mut().push("pkexec");
                Ok(Some(126))
            },
            || {
                called.borrow_mut().push("sudo-n");
                Ok((Some(0), String::new()))
            },
            || {
                called.borrow_mut().push("sudo-S");
                Ok(Some(0))
            },
        )
        .expect_err("exit code 126 must stop retrying");
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(*called.borrow(), ["pkexec"]);
    }

    #[test]
    fn linux_elevation_falls_back_pkexec_127_to_sudo_n_and_then_sudo_s() -> Result<()> {
        // Case 1: pkexec 127 -> sudo -n succeeds (does not call sudo -S).
        let calls = std::cell::RefCell::new(Vec::new());
        run_linux_elevation_chain_with(
            || {
                calls.borrow_mut().push("pkexec");
                Ok(Some(127))
            },
            || {
                calls.borrow_mut().push("sudo-n");
                Ok((Some(0), String::new()))
            },
            || {
                calls.borrow_mut().push("sudo-S");
                Ok(Some(0))
            },
        )?;
        assert_eq!(*calls.borrow(), ["pkexec", "sudo-n"]);

        // Case 2: missing pkexec -> sudo -n requires password -> sudo -S succeeds.
        let calls = std::cell::RefCell::new(Vec::new());
        run_linux_elevation_chain_with(
            || {
                calls.borrow_mut().push("pkexec");
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
            },
            || {
                calls.borrow_mut().push("sudo-n");
                Ok((Some(1), "sudo: a password is required\n".to_owned()))
            },
            || {
                calls.borrow_mut().push("sudo-S");
                Ok(Some(0))
            },
        )?;
        assert_eq!(*calls.borrow(), ["pkexec", "sudo-n", "sudo-S"]);

        // Case 3: sudo -n executes installer which exits with repair-in-progress (75) -> stop!
        let calls = std::cell::RefCell::new(Vec::new());
        let error = run_linux_elevation_chain_with(
            || {
                calls.borrow_mut().push("pkexec");
                Ok(Some(127))
            },
            || {
                calls.borrow_mut().push("sudo-n");
                Ok((Some(75), "Service repair is already in progress".to_owned()))
            },
            || {
                calls.borrow_mut().push("sudo-S");
                Ok(Some(0))
            },
        )
        .expect_err("real installer failure under sudo -n must not retry via sudo -S");
        assert!(error.to_string().contains("sudo -n"));
        assert_eq!(*calls.borrow(), ["pkexec", "sudo-n"]);
        Ok(())
    }
}
