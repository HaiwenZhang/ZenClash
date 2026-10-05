// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Privileged maintenance shared by the installer and uninstaller binaries.

use anyhow::Error;

pub(crate) fn repair_active_owner_state() -> Result<(), Error> {
    if let Some(backup) = zenclash_service::repair_active_owner_state()? {
        eprintln!(
            "Quarantined corrupt active owner state at {backup:?}; the next start will establish a new owner session"
        );
    }
    Ok(())
}

pub(crate) fn enter_repair_gate() -> Result<zenclash_service::ServiceRepairGate, Error> {
    match zenclash_service::acquire_service_repair_gate()? {
        Some(gate) => Ok(gate),
        None => {
            eprintln!("Service repair is already in progress");
            std::process::exit(zenclash_service::REPAIR_IN_PROGRESS_EXIT_CODE);
        }
    }
}

pub(crate) fn run_maintenance_if_requested() -> Result<bool, Error> {
    if !std::env::args_os().any(|argument| argument == "--cleanup-stale-owners") {
        return Ok(false);
    }
    let removed = zenclash_service::cleanup_stale_owner_state()?;
    println!("Removed {} stale owner state directories", removed.len());
    Ok(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn run_command(cmd: &str, args: &[&str], debug: bool) -> Result<(), Error> {
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

/// Names the Windows Firewall rule that admits the approved copy of `core`.
#[cfg(windows)]
pub(crate) fn core_firewall_rule_name(core: &std::path::Path) -> Result<String, Error> {
    use anyhow::Context as _;

    let name = core.file_name().context("core path has no file name")?;
    Ok(format!(
        "{} core ({})",
        zenclash_service::SERVICE_DISPLAY_NAME,
        name.to_string_lossy()
    ))
}

/// Survives removal of `cores`, so a later uninstall can retry failed rule deletions.
#[cfg(windows)]
pub(crate) fn core_firewall_records(cores: &std::path::Path) -> std::path::PathBuf {
    cores.with_file_name("core-firewall-rules")
}

#[cfg(windows)]
pub(crate) fn record_core_firewall_rule(core: &std::path::Path) -> Result<(), Error> {
    use anyhow::Context as _;

    // Record before changing the firewall. Independent empty files cannot truncate an
    // existing inventory if the installer crashes. The parent is installer-protected.
    let records = core_firewall_records(core.parent().context("core path has no parent")?);
    std::fs::create_dir_all(&records).context("failed to create firewall cleanup records")?;
    let record = records.join(core.file_name().context("core path has no file name")?);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&record) {
        Ok(file) => file.sync_all().context("failed to persist firewall cleanup record"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).context("failed to record firewall cleanup obligation"),
    }
}

#[cfg(windows)]
fn netsh_path() -> Result<std::path::PathBuf, Error> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    // The caller controls inherited environment variables, including SystemRoot and PATH.
    // Ask Windows instead. WOW64 may redirect this to its own system netsh, which also
    // supports advfirewall; no filesystem-redirection override is needed.
    let mut buffer = vec![0_u16; 260];
    loop {
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if length < buffer.len() {
            return Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])).join("netsh.exe"));
        }
        buffer.resize(length, 0);
    }
}

/// Runs `netsh advfirewall firewall` with `arguments`, failing on a non-zero exit.
///
/// netsh prints localized text only, so callers can react to the status alone.
#[cfg(windows)]
pub(crate) fn netsh_firewall(arguments: &[&str]) -> Result<(), Error> {
    use anyhow::Context as _;
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let netsh = netsh_path().context("failed to locate the Windows system netsh")?;
    let output = std::process::Command::new(&netsh)
        .args(["advfirewall", "firewall"])
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("failed to run {}", netsh.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "netsh advfirewall firewall {} failed (status: {}): {} {}",
        arguments.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(all(windows, test))]
mod tests {
    #[test]
    fn netsh_ignores_caller_system_root() -> anyhow::Result<()> {
        const EXPECTED: &str = "CVR_TEST_EXPECTED_NETSH";
        if let Some(expected) = std::env::var_os(EXPECTED) {
            assert_eq!(super::netsh_path()?, std::path::PathBuf::from(expected));
            return Ok(());
        }
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "shared::tests::netsh_ignores_caller_system_root"])
            .env(EXPECTED, super::netsh_path()?)
            .env("SystemRoot", std::env::temp_dir().join("caller controlled (windows)"))
            .status()?;
        assert!(status.success());
        Ok(())
    }
}
