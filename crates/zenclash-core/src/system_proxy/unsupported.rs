use super::SystemProxyStatus;
use crate::{MihomoError, MihomoResult};

pub(super) fn detect() -> MihomoResult<String> {
    Err(MihomoError::Process("当前平台尚未实现系统代理控制".into()))
}

pub(super) fn status(_service: &str) -> MihomoResult<SystemProxyStatus> {
    Err(MihomoError::Process(
        "当前平台尚未实现系统代理状态读取".into(),
    ))
}

pub(super) fn set_enabled(
    _service: &str,
    _enabled: bool,
    _server: &str,
    _port: u16,
    _bypass: &[String],
) -> MihomoResult<()> {
    Err(MihomoError::Process("当前平台尚未实现系统代理设置".into()))
}

pub(super) fn set_pac_enabled(_service: &str, _enabled: bool, _url: &str) -> MihomoResult<()> {
    Err(MihomoError::Process("当前平台尚未实现 PAC 系统代理".into()))
}
