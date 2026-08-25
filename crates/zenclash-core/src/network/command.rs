pub(super) fn output(command: &str, args: &[&str]) -> Result<String, String> {
    let output = crate::platform_command::output(command, args)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if stderr.is_empty() {
        format!("{command} 退出状态：{}", output.status)
    } else {
        stderr
    })
}
