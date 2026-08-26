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
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("执行 {display_name} 失败：{error}"))?;
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

    let status = wait_for_exit(&mut child, display_name, timeout);
    let stdout = join_reader(stdout_reader, "stdout");
    let stderr = join_reader(stderr_reader, "stderr");

    Ok(Output {
        status: status?,
        stdout: stdout.map_err(|error| format!("读取 {display_name} 标准输出失败：{error}"))?,
        stderr: stderr.map_err(|error| format!("读取 {display_name} 标准错误失败：{error}"))?,
    })
}

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
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let started = Instant::now();
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
