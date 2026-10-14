//! Registry-only recovery fixtures; production writes use the default-connection adapter.
use super::*;
use crate::system_proxy::native::{
    NativeProxyChange, ownership_changed, unstable_observation, verification_failure,
};

pub(super) trait RegistryAccess {
    fn read(&self) -> MihomoResult<InternetSettingsRegistryValues>;
    fn write(&self, name: &str, value: Option<&RawRegistryValue>) -> MihomoResult<()>;
    fn refresh(&self) -> MihomoResult<()>;
}

pub(super) fn capture(access: &impl RegistryAccess) -> MihomoResult<WinInetProxySnapshot> {
    let first = access.read()?;
    let second = access.read()?;
    if first != second {
        return Err(unstable_observation());
    }
    WinInetProxySnapshot::from_values(second)
}

pub(super) fn apply(
    access: &impl RegistryAccess,
    before: &WinInetProxySnapshot,
    change: &NativeProxyChange<'_>,
    record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let mut steps = vec![("ProxyEnable", Some(proxy_enable_value(false)))];
    match change {
        NativeProxyChange::Disable => steps.push((
            "AutoConfigURL",
            Some(registry_string_value("AutoConfigURL", "")),
        )),
        NativeProxyChange::Pac { url } => steps.push((
            "AutoConfigURL",
            Some(registry_string_value("AutoConfigURL", url)),
        )),
        NativeProxyChange::Manual { host, port, bypass } => {
            steps.push((
                "AutoConfigURL",
                Some(registry_string_value("AutoConfigURL", "")),
            ));
            steps.push((
                "ProxyServer",
                Some(registry_string_value(
                    "ProxyServer",
                    &manual_proxy_server(
                        &registry_string(&before.values, "ProxyServer")?,
                        host,
                        *port,
                    ),
                )),
            ));
            steps.push((
                "ProxyOverride",
                Some(registry_string_value("ProxyOverride", &bypass.join(";"))),
            ));
            steps.push(("ProxyEnable", Some(proxy_enable_value(true))));
        }
    }
    execute_write_steps(access, before, steps, record)
}
pub(super) fn restore(
    access: &impl RegistryAccess,
    before: &WinInetProxySnapshot,
    previous: &WinInetProxySnapshot,
    record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let mut steps = vec![("ProxyEnable", Some(proxy_enable_value(false)))];
    for name in MANAGED_VALUES {
        steps.push((
            name,
            previous
                .values
                .get(&ascii_folded_value_name(&utf16_units(name)))
                .cloned(),
        ));
    }
    execute_write_steps(access, before, steps, record)
}
fn execute_write_steps(
    access: &impl RegistryAccess,
    before: &WinInetProxySnapshot,
    steps: Vec<(&str, Option<RawRegistryValue>)>,
    mut record: impl FnMut(WinInetProxySnapshot),
) -> MihomoResult<WinInetProxySnapshot> {
    let mut current = before.clone();
    for (name, mut value) in steps {
        if access.read()? != current.values {
            return Err(ownership_changed());
        }
        let index = ascii_folded_value_name(&utf16_units(name));
        if let (Some(value), Some(previous)) = (&mut value, current.values.get(&index)) {
            value.name.clone_from(&previous.name);
        }
        let mut next = current.values.clone();
        if let Some(value) = &value {
            next.insert(index, value.clone());
        } else {
            next.remove(&index);
        }
        let next = WinInetProxySnapshot::from_values(next)?;
        record(next.clone());
        access.write(name, value.as_ref())?;
        if access.read()? != next.values {
            return Err(verification_failure());
        }
        current = next;
    }
    access.refresh()?;
    let verified = capture(access)?;
    if !current.same_state(&verified)? {
        return Err(verification_failure());
    }
    Ok(verified)
}
#[cfg(test)]
mod tests;
