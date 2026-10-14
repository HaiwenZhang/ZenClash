use std::{
    ffi::OsStr,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

mod error;
mod output_pipe;
pub use error::{PlatformCommandError, PlatformCommandStage, PlatformCommandStream};
use output_pipe::CommandOutputPipe;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
const fn background_creation_flags() -> u32 {
    CREATE_NO_WINDOW
}

/// Executes a short-lived native platform command with captured output.
///
/// The child is terminated after ten seconds so an unavailable desktop
/// service cannot block a background worker indefinitely.
pub fn output(command: &str, args: &[&str]) -> Result<Output, String> {
    output_typed(command, args).map_err(|error| error.to_string())
}

pub(crate) fn output_with_timeout(
    command: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut process = Command::new(command);
    process.args(args);
    output_from_command(process, command, timeout)
}

pub(crate) fn output_path_with_timeout(
    command: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<Output, String> {
    let mut process = Command::new(command);
    process.args(args);
    output_from_command(process, &command.display().to_string(), timeout)
}

/// Executes a native command while retaining typed failure causes.
///
/// # Errors
/// Returns process, pipe, reader or timeout failures.
pub(crate) fn output_typed(command: &str, args: &[&str]) -> Result<Output, PlatformCommandError> {
    let mut process = Command::new(command);
    process.args(args);
    output_from_command_typed(process, command, COMMAND_TIMEOUT)
}

pub(crate) fn output_from_command(
    command: Command,
    display_name: &str,
    timeout: Duration,
) -> Result<Output, String> {
    output_from_command_typed(command, display_name, timeout).map_err(|error| error.to_string())
}

fn output_from_command_typed(
    mut command: Command,
    display_name: &str,
    timeout: Duration,
) -> Result<Output, PlatformCommandError> {
    configure_background_command(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) => {
                // A concurrent fork can retain a closed writer until exec.
                // See https://github.com/rust-lang/rust/issues/114554.
                if cfg!(target_os = "linux")
                    && error.kind() == std::io::ErrorKind::ExecutableFileBusy
                {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if !remaining.is_zero() {
                        thread::sleep(POLL_INTERVAL.min(remaining));
                        if started.elapsed() < timeout {
                            continue;
                        }
                    }
                }
                return Err(PlatformCommandError::io(
                    display_name,
                    PlatformCommandStage::Spawn,
                    error,
                ));
            }
        }
    };
    let result = collect_child_output(&mut child, display_name, started, timeout);
    if result.is_err() {
        // try_wait caches an observed status. Never signal a PID after it was reaped.
        if !matches!(child.try_wait(), Ok(Some(_))) {
            abort_child(&mut child);
        }
    }
    result
}

fn collect_child_output(
    child: &mut Child,
    command: &str,
    started: Instant,
    timeout: Duration,
) -> Result<Output, PlatformCommandError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PlatformCommandError::MissingPipe {
            command: command.into(),
            stream: PlatformCommandStream::Stdout,
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PlatformCommandError::MissingPipe {
            command: command.into(),
            stream: PlatformCommandStream::Stderr,
        })?;
    let mut stdout = CommandOutputPipe::new(stdout).map_err(|source| {
        PlatformCommandError::io(command, PlatformCommandStage::ConfigureStdoutPipe, source)
    })?;
    let mut stderr = CommandOutputPipe::new(stderr).map_err(|source| {
        PlatformCommandError::io(command, PlatformCommandStage::ConfigureStderrPipe, source)
    })?;
    let mut status = None;
    loop {
        if started.elapsed() >= timeout {
            return Err(PlatformCommandError::TimedOut {
                command: command.into(),
                timeout,
            });
        }
        if status.is_none() {
            status = child.try_wait().map_err(|source| {
                PlatformCommandError::io(command, PlatformCommandStage::Wait, source)
            })?;
        }
        let stdout_progress = stdout.poll().map_err(|source| {
            PlatformCommandError::io(command, PlatformCommandStage::ReadStdout, source)
        })?;
        let stderr_progress = stderr.poll().map_err(|source| {
            PlatformCommandError::io(command, PlatformCommandStage::ReadStderr, source)
        })?;
        if started.elapsed() >= timeout {
            return Err(PlatformCommandError::TimedOut {
                command: command.into(),
                timeout,
            });
        }
        if let Some(status) = status
            && stdout.is_complete()
            && stderr.is_complete()
        {
            return Ok(Output {
                status,
                stdout: stdout.into_bytes(),
                stderr: stderr.into_bytes(),
            });
        }
        if !stdout_progress && !stderr_progress {
            thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
        }
    }
}

#[cfg(target_os = "windows")]
fn configure_background_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(background_creation_flags());
}

#[cfg(not(target_os = "windows"))]
fn configure_background_command(_command: &mut Command) {}

fn abort_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_background_commands_use_create_no_window() {
        assert_eq!(background_creation_flags(), CREATE_NO_WINDOW);
    }

    #[cfg(unix)]
    #[test]
    fn output_captures_successful_command_output() {
        let output = output_with_timeout("/bin/echo", &["zenclash"], Duration::from_secs(1))
            .expect("echo should finish before the timeout");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "zenclash");
    }

    #[cfg(unix)]
    #[test]
    fn output_terminates_a_command_after_the_timeout() {
        let started = Instant::now();
        let error = output_with_timeout("/bin/sleep", &["1"], Duration::from_millis(30))
            .expect_err("sleep should exceed the timeout");

        assert!(error.contains("超时"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn output_collection_deadline_survives_a_descendant_holding_the_pipes() {
        let started = Instant::now();
        let result = output_with_timeout(
            "/bin/sh",
            &["-c", "sleep 1 & exit 0"],
            Duration::from_millis(40),
        );
        assert!(
            result.is_err(),
            "pipe collection must not outlive the command budget"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn either_inherited_stream_and_a_live_parent_share_the_same_deadline() {
        for script in [
            "sleep 1 >/dev/null & exit 0",
            "sleep 1 2>/dev/null & exit 0",
            "sleep 1 & wait",
        ] {
            let started = Instant::now();
            let result = output_with_timeout("/bin/sh", &["-c", script], Duration::from_millis(40));
            assert!(matches!(result, Err(message) if message.contains("超时")));
            assert!(started.elapsed() < Duration::from_millis(500));
        }
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_large_output_streams_are_drained_without_starvation() {
        let result = output_with_timeout(
            "/bin/sh",
            &[
                "-c",
                "head -c 131072 /dev/zero & head -c 131072 /dev/zero >&2; wait",
            ],
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 131072);
        assert_eq!(result.stderr.len(), 131072);
    }

    #[cfg(target_os = "linux")]
    fn busy_executable() -> (std::path::PathBuf, std::fs::File) {
        use std::sync::atomic::{AtomicU64, Ordering};

        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "zenclash-busy-command-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("sh");
        std::fs::copy("/bin/sh", &path).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();

        (path, writer)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_retries_a_busy_executable_until_the_writer_closes() {
        use std::sync::mpsc::{self, RecvTimeoutError};

        let (path, writer) = busy_executable();
        let error = Command::new(&path).spawn().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ETXTBSY));
        let (sender, receiver) = mpsc::channel();

        thread::scope(|scope| {
            scope.spawn(|| {
                let result = output_path_with_timeout(
                    &path,
                    &[OsStr::new("-c"), OsStr::new("printf zenclash")],
                    Duration::from_secs(2),
                );
                sender.send(result).unwrap();
            });
            let before_close = receiver.recv_timeout(Duration::from_millis(50));
            drop(writer);
            assert!(matches!(before_close, Err(RecvTimeoutError::Timeout)));
            let output = receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();

            assert!(output.status.success());
            assert_eq!(output.stdout, b"zenclash");
        });
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_stops_retrying_a_busy_executable_at_the_timeout() {
        let (path, writer) = busy_executable();
        let timeout = Duration::from_millis(50);
        let started = Instant::now();

        let error = output_path_with_timeout(&path, &[], timeout).unwrap_err();

        assert!(error.contains(&io::Error::from_raw_os_error(libc::ETXTBSY).to_string()));
        assert!(started.elapsed() >= timeout);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(writer);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_counts_busy_retry_time_toward_the_execution_timeout() {
        let (path, writer) = busy_executable();
        let timeout = Duration::from_millis(600);
        let started = Instant::now();

        let error = thread::scope(|scope| {
            scope.spawn(move || {
                thread::sleep(Duration::from_millis(400));
                drop(writer);
            });
            output_path_with_timeout(
                &path,
                &[OsStr::new("-c"), OsStr::new("exec sleep 2")],
                timeout,
            )
            .unwrap_err()
        });

        assert!(error.contains("超时"));
        assert!(started.elapsed() >= timeout);
        assert!(started.elapsed() < Duration::from_millis(850));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn output_returns_other_spawn_errors_without_retrying() {
        let started = Instant::now();

        let error = output_with_timeout("\0", &[], Duration::from_secs(2)).unwrap_err();

        assert!(error.contains("失败"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[cfg(unix)]
    #[test]
    fn output_preserves_nonzero_exit_status_and_both_output_streams() {
        let output = output_with_timeout(
            "/bin/sh",
            &["-c", "printf stdout; printf stderr >&2; exit 7"],
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[cfg(unix)]
    #[test]
    fn output_rejects_an_oversized_stream_without_waiting_for_process_exit() {
        let mut command = Command::new("/bin/sh");
        // The owned stdin pipe keeps the shell alive until it is explicitly killed.
        // This checks early rejection without depending on CI scheduling latency.
        command
            .args(["-c", "head -c 1048577 /dev/zero; read -r release"])
            .stdin(Stdio::piped());
        let result = output_from_command(command, "/bin/sh", Duration::from_secs(5));
        assert!(matches!(result, Err(message) if message.contains("1 MiB")));
    }

    #[cfg(unix)]
    #[test]
    fn output_collection_rejects_an_oversized_stream_while_the_child_is_alive() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "head -c 1048577 /dev/zero; read -r release"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let result = collect_child_output(
            &mut child,
            "/bin/sh",
            Instant::now(),
            Duration::from_secs(5),
        );
        let status = child.try_wait();
        // Clean up before asserting so a failing assertion cannot orphan the shell.
        abort_child(&mut child);

        assert!(matches!(result, Err(PlatformCommandError::Io {
            stage: PlatformCommandStage::ReadStdout, source, ..
        }) if source.kind() == io::ErrorKind::InvalidData));
        assert!(
            status.unwrap().is_none(),
            "collection must reject before exit"
        );
    }

    #[test]
    fn typed_execution_retains_spawn_failure_and_omits_arguments() {
        let command = format!("zenclash-missing-command-{}", std::process::id());
        let error = output_typed(&command, &["private-command-argument"]).unwrap_err();
        assert!(matches!(&error, PlatformCommandError::Io {
            stage: PlatformCommandStage::Spawn, source, ..
        } if source.kind() == io::ErrorKind::NotFound));
        assert!(!format!("{error:?}").contains("private-command-argument"));
    }
}
