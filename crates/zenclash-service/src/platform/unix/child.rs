use std::{io, path::Path, process::Stdio};

use tokio::process::{Child, Command};

pub(crate) fn spawn(binary: &Path, config: &Path, home: &Path) -> io::Result<Child> {
    command(binary, config, home, false)?.spawn()
}

pub(crate) fn spawn_validation(binary: &Path, config: &Path, home: &Path) -> io::Result<Child> {
    command(binary, config, home, true)?.spawn()
}

fn command(binary: &Path, config: &Path, home: &Path, validation: bool) -> io::Result<Command> {
    super::require_admin()?;
    super::validate_protected_path(binary, false)?;
    super::validate_protected_path(config, false)?;
    super::validate_protected_path(home, true)?;
    let session = home.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime home has no protected container",
        )
    })?;
    let assets = session.join("assets");
    super::validate_protected_path(&assets, true)?;
    let mut command = Command::new(binary);
    if validation {
        command.arg("-t");
    }
    command
        .args(["-d"])
        .arg(home)
        .arg("-f")
        .arg(config)
        .current_dir(home)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SAFE_PATHS", assets)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "linux")]
    {
        command.process_group(0);
        // SAFETY: getpid has no preconditions. Capture before fork, never ask
        // the child to infer its intended owner after a potential parent death.
        let parent = unsafe { libc::getpid() };
        // SAFETY: the closure uses only async-signal-safe Linux syscalls and
        // constructs an OS error on failure; it does not acquire a Rust lock.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    // On macOS, deliberately inherit the launchd job's process group.
    // AbandonProcessGroup=false instructs launchd to kill surviving group
    // members when the service dies; giving Mihomo a new group defeats that.
    Ok(command)
}
