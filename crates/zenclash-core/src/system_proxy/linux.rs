#[cfg(target_os = "linux")]
use super::SystemProxyStatus;
#[cfg(target_os = "linux")]
use crate::MihomoResult;

#[cfg(target_os = "linux")]
mod snapshot;
#[cfg(target_os = "linux")]
pub(super) use snapshot::{ProxySnapshot, apply, capture, restore};

#[cfg(target_os = "linux")]
pub(super) fn detect() -> MihomoResult<String> {
    status("GNOME")?;
    Ok("GNOME".into())
}

#[cfg(target_os = "linux")]
pub(super) fn status(service: &str) -> MihomoResult<SystemProxyStatus> {
    capture(super::native::NativeProxyTarget::Service(service))
        .map(|snapshot| snapshot.summary().clone())
}
#[cfg(target_os = "linux")]
pub(super) fn set_enabled(
    service: &str,
    enabled: bool,
    host: &str,
    port: u16,
    bypass: &[String],
) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Manual { host, port, bypass }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}
#[cfg(target_os = "linux")]
pub(super) fn set_pac_enabled(service: &str, enabled: bool, url: &str) -> MihomoResult<()> {
    let before = capture(super::native::NativeProxyTarget::Service(service))?;
    let change = if enabled {
        super::native::NativeProxyChange::Pac { url }
    } else {
        super::native::NativeProxyChange::Disable
    };
    apply(&before, &change).result.map(drop)
}
