use super::SystemNetworkSnapshot;

pub(super) fn detect() -> Result<SystemNetworkSnapshot, String> {
    Err("当前平台尚未实现网络接口探测".into())
}
