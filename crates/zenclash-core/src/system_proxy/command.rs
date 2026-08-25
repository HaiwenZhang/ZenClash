use std::process::Output;

use crate::{MihomoError, MihomoResult};

pub(super) fn run_checked(command: &str, args: &[&str]) -> MihomoResult<Output> {
    let output = crate::platform_command::output(command, args).map_err(MihomoError::Process)?;
    if output.status.success() {
        return Ok(output);
    }

    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(MihomoError::Process(if message.is_empty() {
        format!("{command} 退出状态：{}", output.status)
    } else {
        message
    }))
}
