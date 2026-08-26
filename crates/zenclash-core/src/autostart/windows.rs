use std::path::Path;

use super::{AutostartError, AutostartResult, AutostartStatus};

const TASK_NAME: &str = "ZenClash";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

pub(super) fn default_entry_path() -> AutostartResult<Option<std::path::PathBuf>> {
    Ok(None)
}

pub(super) fn status(
    executable: &Path,
    _entry_path: Option<&Path>,
) -> AutostartResult<AutostartStatus> {
    let expected = quoted_executable(executable);
    let task = command_output("schtasks.exe", &["/Query", "/TN", TASK_NAME, "/XML"])?;
    if task.status.success() {
        let output = String::from_utf8_lossy(&task.stdout);
        return Ok(AutostartStatus {
            enabled: true,
            matches_current_executable: output
                .contains(&xml_escape(&executable.display().to_string())),
            location: format!("Windows Task Scheduler / {TASK_NAME}"),
        });
    }

    let registry = command_output("reg.exe", &["query", RUN_KEY, "/v", TASK_NAME])?;
    let output = String::from_utf8_lossy(&registry.stdout);
    Ok(AutostartStatus {
        enabled: registry.status.success(),
        matches_current_executable: registry.status.success() && output.contains(&expected),
        location: format!("{RUN_KEY} / {TASK_NAME}"),
    })
}

pub(super) fn set_enabled(
    executable: &Path,
    _entry_path: Option<&Path>,
    enabled: bool,
) -> AutostartResult<()> {
    if !enabled {
        let _ = command_output("schtasks.exe", &["/Delete", "/TN", TASK_NAME, "/F"])?;
        let _ = command_output("reg.exe", &["delete", RUN_KEY, "/v", TASK_NAME, "/f"])?;
        return Ok(());
    }

    let quoted = quoted_executable(executable);
    let _ = command_output("reg.exe", &["delete", RUN_KEY, "/v", TASK_NAME, "/f"])?;
    let task = command_output(
        "schtasks.exe",
        &[
            "/Create", "/TN", TASK_NAME, "/SC", "ONLOGON", "/TR", &quoted, "/F", "/RL", "LIMITED",
        ],
    )?;
    if task.status.success() {
        return Ok(());
    }

    let registry = command_output(
        "reg.exe",
        &[
            "add", RUN_KEY, "/v", TASK_NAME, "/t", "REG_SZ", "/d", &quoted, "/f",
        ],
    )?;
    if registry.status.success() {
        Ok(())
    } else {
        Err(command_failed("reg.exe", &registry))
    }
}

fn command_output(command: &str, args: &[&str]) -> AutostartResult<std::process::Output> {
    crate::platform_command::output(command, args).map_err(AutostartError::Command)
}

fn command_failed(command: &str, output: &std::process::Output) -> AutostartError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    AutostartError::Command(if stderr.is_empty() {
        format!("{command} 退出状态：{}", output.status)
    } else {
        stderr
    })
}

fn quoted_executable(executable: &Path) -> String {
    format!("\"{}\"", executable.display())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{quoted_executable, xml_escape};

    #[test]
    fn task_action_quotes_an_executable_with_spaces() {
        assert_eq!(
            quoted_executable(Path::new(r"C:\Program Files\ZenClash\zenclash.exe")),
            r#""C:\Program Files\ZenClash\zenclash.exe""#
        );
    }

    #[test]
    fn task_query_match_escapes_xml_characters() {
        assert_eq!(
            xml_escape(r#""C:\A&B\zenclash.exe""#),
            r#""C:\A&amp;B\zenclash.exe""#
        );
    }
}
