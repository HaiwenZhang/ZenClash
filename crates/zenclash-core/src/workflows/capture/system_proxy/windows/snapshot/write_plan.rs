//! Default/LAN write order and predicted raw/API state for each attempted step.

use windows_sys::Win32::Networking::WinInet::{PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_PROXY};

use super::connection_transaction::DefaultConnectionAccess;
use super::*;
use crate::workflows::capture::system_proxy::native::NativeProxyChange;

pub(super) fn plan_change(
    before: &WinInetProxySnapshot,
    change: &NativeProxyChange<'_>,
) -> MihomoResult<Vec<DefaultConnectionWriteStep>> {
    validate_api_registry_consistency(before)?;
    let connection = before.connection()?;
    let disabled_flags = connection.disabled_flags();
    let mut steps = vec![DefaultConnectionWriteStep::SetOption(
        DefaultConnectionOption::Flags(disabled_flags),
    )];
    match change {
        NativeProxyChange::Disable => {
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::AutoConfigUrl(String::new()),
            ));
        }
        NativeProxyChange::Pac { url } => {
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::AutoConfigUrl((*url).into()),
            ));
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::Flags(disabled_flags | PROXY_TYPE_AUTO_PROXY_URL),
            ));
        }
        NativeProxyChange::Manual { host, port, bypass } => {
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::AutoConfigUrl(String::new()),
            ));
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::ProxyServer(manual_proxy_server(
                    connection.manual_server(),
                    host,
                    *port,
                )),
            ));
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::ProxyBypass(bypass.join(";")),
            ));
            steps.push(DefaultConnectionWriteStep::SetOption(
                DefaultConnectionOption::Flags(disabled_flags | PROXY_TYPE_PROXY),
            ));
        }
    }
    Ok(steps)
}

pub(super) fn plan_restore(
    before: &WinInetProxySnapshot,
    previous: &WinInetProxySnapshot,
) -> MihomoResult<Vec<DefaultConnectionWriteStep>> {
    validate_api_registry_consistency(previous)?;
    let mut steps = vec![DefaultConnectionWriteStep::SetOption(
        DefaultConnectionOption::Flags(before.connection()?.disabled_flags()),
    )];
    steps.extend(
        previous
            .connection()?
            .restoration_options()
            .map(DefaultConnectionWriteStep::SetOption),
    );
    // The option API restores semantics; only afterwards replay original native
    // representations it cannot express (REG_EXPAND_SZ, noncanonical DWORD, absence).
    for name in MANAGED_VALUES {
        steps.push(DefaultConnectionWriteStep::RestoreRegistryValue {
            name,
            value: previous
                .values
                .get(&ascii_folded_value_name(&utf16_units(name)))
                .cloned(),
        });
    }
    Ok(steps)
}

fn validate_api_registry_consistency(snapshot: &WinInetProxySnapshot) -> MihomoResult<()> {
    let connection = snapshot.connection()?;
    let strings = [
        (
            "ProxyServer",
            connection.proxy_server.as_deref().unwrap_or_default(),
        ),
        (
            "ProxyOverride",
            connection.proxy_bypass.as_deref().unwrap_or_default(),
        ),
        (
            "AutoConfigURL",
            connection.auto_config_url.as_deref().unwrap_or_default(),
        ),
    ];
    let consistent = registry_manual_proxy_enabled(&snapshot.values)?
        == connection.manual_enabled()
        && strings.iter().all(|(name, expected)| {
            registry_string(&snapshot.values, name).is_ok_and(|actual| actual == *expected)
        });
    if !consistent {
        return Err(crate::SystemProxyError::BackendUnavailable {
            backend: crate::SystemProxyBackendKind::WinInet,
            reason: crate::SystemProxyBackendUnavailable::UnverifiedStorageContract,
        }
        .into());
    }
    Ok(())
}

pub(super) enum DefaultConnectionWriteStep {
    SetOption(DefaultConnectionOption),
    RestoreRegistryValue {
        name: &'static str,
        value: Option<RawRegistryValue>,
    },
}

impl DefaultConnectionWriteStep {
    pub(super) fn predicted_snapshots(
        &self,
        current: &WinInetProxySnapshot,
    ) -> MihomoResult<Vec<WinInetProxySnapshot>> {
        let mut values = current.values.clone();
        let mut connection = current.connection()?.clone();
        let (name, mut value, may_delete_on_empty) = match self {
            Self::SetOption(option) => {
                connection = connection.replacement(option);
                match option {
                    DefaultConnectionOption::Flags(flags) => (
                        "ProxyEnable",
                        Some(proxy_enable_value(flags & PROXY_TYPE_PROXY != 0)),
                        false,
                    ),
                    DefaultConnectionOption::ProxyServer(value) => (
                        "ProxyServer",
                        Some(registry_string_value("ProxyServer", value)),
                        value.is_empty(),
                    ),
                    DefaultConnectionOption::ProxyBypass(value) => (
                        "ProxyOverride",
                        Some(registry_string_value("ProxyOverride", value)),
                        value.is_empty(),
                    ),
                    DefaultConnectionOption::AutoConfigUrl(value) => (
                        "AutoConfigURL",
                        Some(registry_string_value("AutoConfigURL", value)),
                        value.is_empty(),
                    ),
                }
            }
            Self::RestoreRegistryValue { name, value } => (*name, value.clone(), false),
        };
        let index = ascii_folded_value_name(&utf16_units(name));
        if matches!(self, Self::SetOption(_))
            && let (Some(value), Some(previous)) = (&mut value, values.get(&index))
        {
            value.name.clone_from(&previous.name);
        }
        // Native storage effects outside these exact anticipated values are not
        // adopted as write authority. An unfamiliar OS representation fails closed.
        if may_delete_on_empty {
            let mut deleted_values = values.clone();
            deleted_values.remove(&index);
            if let Some(value) = value {
                values.insert(index, value);
            }
            return Ok(vec![
                WinInetProxySnapshot::from_native(values, connection.clone())?,
                WinInetProxySnapshot::from_native(deleted_values, connection)?,
            ]);
        }
        if let Some(value) = value {
            values.insert(index, value);
        } else {
            values.remove(&index);
        }
        Ok(vec![WinInetProxySnapshot::from_native(values, connection)?])
    }

    pub(super) fn write(&self, access: &impl DefaultConnectionAccess) -> MihomoResult<()> {
        match self {
            Self::SetOption(option) => access.set_option(option),
            Self::RestoreRegistryValue { name, value } => {
                access.restore_registry_value(name, value.as_ref())
            }
        }
    }
}
