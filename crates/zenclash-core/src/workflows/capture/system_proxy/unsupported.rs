use super::SystemProxyStatus;
use crate::MihomoResult;

pub(super) fn detect() -> MihomoResult<String> {
    Err(crate::SystemProxyError::UnsupportedPlatform.into())
}

pub(super) fn status(_service: &str) -> MihomoResult<SystemProxyStatus> {
    Err(crate::SystemProxyError::UnsupportedPlatform.into())
}

pub(super) fn set_enabled(
    _service: &str,
    _enabled: bool,
    _server: &str,
    _port: u16,
    _bypass: &[String],
) -> MihomoResult<()> {
    Err(crate::SystemProxyError::UnsupportedPlatform.into())
}

pub(super) fn set_pac_enabled(_service: &str, _enabled: bool, _url: &str) -> MihomoResult<()> {
    Err(crate::SystemProxyError::UnsupportedPlatform.into())
}

#[derive(Clone)]
pub(super) enum ProxySnapshot {}
impl ProxySnapshot {
    pub(super) fn summary(&self) -> &SystemProxyStatus {
        match *self {}
    }
    pub(super) fn same_target(&self, _other: &Self) -> bool {
        match *self {}
    }
    pub(super) fn legacy_release_is_safe(&self) -> bool {
        match *self {}
    }
    pub(super) fn same_state(&self, _other: &Self) -> MihomoResult<bool> {
        match *self {}
    }
}
pub(super) fn capture(
    _target: super::native::NativeProxyTarget<'_>,
) -> MihomoResult<super::native::NativeProxySnapshot> {
    Err(crate::SystemProxyError::UnsupportedPlatform.into())
}
pub(super) fn apply(
    expected: &super::native::NativeProxySnapshot,
    _change: &super::native::NativeProxyChange<'_>,
) -> super::native::NativeWriteAttempt {
    super::native::NativeWriteAttempt::run(expected, |_| {
        Err(crate::SystemProxyError::UnsupportedPlatform.into())
    })
}
pub(super) fn restore(
    expected: &super::native::NativeProxySnapshot,
    _previous: &super::native::NativeProxySnapshot,
) -> super::native::NativeWriteAttempt {
    super::native::NativeWriteAttempt::run(expected, |_| {
        Err(crate::SystemProxyError::UnsupportedPlatform.into())
    })
}
