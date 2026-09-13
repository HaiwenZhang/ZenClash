use std::{
    ffi::OsStr,
    io::{self, Read},
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

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
    output_with_timeout(command, args, COMMAND_TIMEOUT)
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

fn output_from_command(
    mut command: Command,
    display_name: &str,
    timeout: Duration,
) -> Result<Output, String> {
    configure_background_command(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) => {
                // A concurrent fork can retain a closed writer until exec.
                // See https://github.com/rust-lang/rust/issues/114554.
                if cfg!(target_os = "linux") && error.kind() == io::ErrorKind::ExecutableFileBusy {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if !remaining.is_zero() {
                        thread::sleep(POLL_INTERVAL.min(remaining));
                        if started.elapsed() < timeout {
                            continue;
                        }
                    }
                }
                return Err(format!("执行 {display_name} 失败：{error}"));
            }
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| abort_for_setup_error(&mut child, display_name, "无法捕获标准输出"))?;
    let stdout_reader = match spawn_reader(stdout, "stdout") {
        Ok(reader) => reader,
        Err(error) => {
            abort_child(&mut child);
            return Err(format!("执行 {display_name} 失败：{error}"));
        }
    };
    let Some(stderr) = child.stderr.take() else {
        abort_child(&mut child);
        let _ = join_reader(stdout_reader, "stdout");
        return Err(format!("执行 {display_name} 失败：无法捕获标准错误"));
    };
    let stderr_reader = match spawn_reader(stderr, "stderr") {
        Ok(reader) => reader,
        Err(error) => {
            abort_child(&mut child);
            let _ = join_reader(stdout_reader, "stdout");
            return Err(format!("执行 {display_name} 失败：{error}"));
        }
    };

    let status = wait_for_exit(&mut child, display_name, started, timeout);
    let stdout = join_reader(stdout_reader, "stdout");
    let stderr = join_reader(stderr_reader, "stderr");

    Ok(Output {
        status: status?,
        stdout: stdout.map_err(|error| format!("读取 {display_name} 标准输出失败：{error}"))?,
        stderr: stderr.map_err(|error| format!("读取 {display_name} 标准错误失败：{error}"))?,
    })
}

#[cfg(target_os = "windows")]
fn configure_background_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(background_creation_flags());
}

#[cfg(not(target_os = "windows"))]
fn configure_background_command(_command: &mut Command) {}

fn spawn_reader(
    pipe: impl Read + Send + 'static,
    stream: &'static str,
) -> io::Result<thread::JoinHandle<io::Result<Vec<u8>>>> {
    thread::Builder::new()
        .name(format!("zenclash-{stream}-reader"))
        .spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(MAX_COMMAND_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_COMMAND_OUTPUT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{stream} 超过 1 MiB 上限"),
                ));
            }
            Ok(bytes)
        })
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{stream} 读取线程异常结束"))?
        .map_err(|error| error.to_string())
}

fn wait_for_exit(
    child: &mut Child,
    command: &str,
    started: Instant,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() >= timeout => {
                abort_child(child);
                return Err(format!(
                    "执行 {command} 超时（{} 秒），已终止",
                    timeout.as_secs_f64()
                ));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL.min(timeout)),
            Err(error) => {
                abort_child(child);
                return Err(format!("等待 {command} 结束失败：{error}"));
            }
        }
    }
}

fn abort_for_setup_error(child: &mut Child, command: &str, reason: &str) -> String {
    abort_child(child);
    format!("执行 {command} 失败：{reason}")
}

fn abort_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn reader_rejects_output_over_the_size_limit() {
        let payload = std::io::Cursor::new(vec![b'x'; MAX_COMMAND_OUTPUT_BYTES + 1]);
        let error = join_reader(
            spawn_reader(payload, "stdout").expect("reader thread should start"),
            "stdout",
        )
        .expect_err("oversized output should be rejected");

        assert!(error.contains("超过 1 MiB"));
    }
}
