//! Typed WinINET preference values; missing values and native types stay distinct.

use super::SystemProxyStatus;
use super::connection_options::{DefaultConnectionOption, DefaultConnectionOptions};
#[cfg(test)]
use super::{parse_bypass, parse_proxy_server};
use crate::{MihomoResult, system_proxy::native::invalid_snapshot};
use registry_values::*;

#[cfg(test)]
fn summary(
    values: &InternetSettingsRegistryValues,
    service: &str,
) -> MihomoResult<SystemProxyStatus> {
    let enabled = registry_manual_proxy_enabled(values)?;
    let (server, port, secure_server, secure_port) =
        parse_proxy_server(&registry_string(values, "ProxyServer")?)
            .map_err(|_| invalid_snapshot())?;
    let auto_url = registry_string(values, "AutoConfigURL")?;
    Ok(SystemProxyStatus {
        service: service.into(),
        enabled: enabled && !server.is_empty() && port != 0,
        server,
        port,
        secure_enabled: enabled && !secure_server.is_empty() && secure_port != 0,
        secure_server,
        secure_port,
        bypass: parse_bypass(&registry_string(values, "ProxyOverride")?),
        auto_enabled: !auto_url.is_empty(),
        auto_url,
    })
}
fn manual_proxy_server(previous: &str, host: &str, port: u16) -> String {
    let endpoint = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut value = format!("http={endpoint};https={endpoint}");
    if !previous.contains('=') {
        if !previous.trim().is_empty() {
            value.push_str(&format!(";ftp={previous};socks={previous}"));
        }
    } else {
        for entry in previous.split(';') {
            if entry.trim().is_empty() {
                continue;
            }
            if entry.split_once('=').is_some_and(|(scheme, _)| {
                scheme.trim().eq_ignore_ascii_case("http")
                    || scheme.trim().eq_ignore_ascii_case("https")
            }) {
                continue;
            }
            value.push(';');
            value.push_str(entry);
        }
    }
    value
}

#[derive(Clone)]
pub(in crate::system_proxy) struct WinInetProxySnapshot {
    values: InternetSettingsRegistryValues,
    summary: SystemProxyStatus,
    connection: ConnectionOptionSnapshot,
}

#[derive(Clone, PartialEq, Eq)]
enum ConnectionOptionSnapshot {
    Default(DefaultConnectionOptions),
    #[cfg(test)]
    RegistryOnly,
}
impl WinInetProxySnapshot {
    pub(in crate::system_proxy) fn summary(&self) -> &SystemProxyStatus {
        &self.summary
    }
    pub(in crate::system_proxy) fn same_target(&self, _other: &Self) -> bool {
        true
    }
    pub(in crate::system_proxy) fn same_state(&self, other: &Self) -> MihomoResult<bool> {
        Ok(self.values == other.values && self.connection == other.connection)
    }
    pub(in crate::system_proxy) fn legacy_release_is_safe(&self) -> bool {
        let enabled = match &self.connection {
            ConnectionOptionSnapshot::Default(connection) => connection.manual_enabled(),
            #[cfg(test)]
            ConnectionOptionSnapshot::RegistryOnly => {
                registry_manual_proxy_enabled(&self.values).unwrap_or(true)
            }
        };
        if !enabled {
            return true;
        }
        let server = match &self.connection {
            ConnectionOptionSnapshot::Default(connection) => connection.manual_server(),
            #[cfg(test)]
            ConnectionOptionSnapshot::RegistryOnly => {
                return legacy_manual_protocols_are_safe(
                    &registry_string(&self.values, "ProxyServer").unwrap_or_default(),
                );
            }
        };
        legacy_manual_protocols_are_safe(server)
    }
    #[cfg(test)]
    fn from_values(values: InternetSettingsRegistryValues) -> MihomoResult<Self> {
        let summary = summary(&values, "WinINET")?;
        Ok(Self {
            values,
            summary,
            connection: ConnectionOptionSnapshot::RegistryOnly,
        })
    }

    fn from_native(
        values: InternetSettingsRegistryValues,
        connection: DefaultConnectionOptions,
    ) -> MihomoResult<Self> {
        let connection = connection.canonicalized();
        let summary = connection.clone().summary("WinINET")?;
        Ok(Self {
            values,
            summary,
            connection: ConnectionOptionSnapshot::Default(connection),
        })
    }

    fn connection(&self) -> MihomoResult<&DefaultConnectionOptions> {
        match &self.connection {
            ConnectionOptionSnapshot::Default(connection) => Ok(connection),
            #[cfg(test)]
            ConnectionOptionSnapshot::RegistryOnly => Err(invalid_snapshot()),
        }
    }
}

fn legacy_manual_protocols_are_safe(server: &str) -> bool {
    server.contains('=')
        && server.split(';').all(|entry| {
            entry.split_once('=').is_some_and(|(scheme, _)| {
                scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
            })
        })
}

mod connection_transaction;
#[cfg(target_os = "windows")]
mod registry;
#[cfg(test)]
mod registry_transaction_fixture;
mod registry_values;
mod write_plan;

#[cfg(target_os = "windows")]
pub(in crate::system_proxy) fn capture(
    target: crate::system_proxy::native::NativeProxyTarget<'_>,
) -> MihomoResult<crate::system_proxy::native::NativeProxySnapshot> {
    use crate::system_proxy::native::{NativeProxySnapshot, NativeProxyTarget, ownership_changed};
    match target {
        NativeProxyTarget::Service("WinINET") => {}
        NativeProxyTarget::Captured(snapshot) => {
            snapshot.platform()?;
        }
        _ => return Err(ownership_changed()),
    }
    let key = registry::InternetSettingsKey::open(false)?;
    connection_transaction::capture(&key).map(NativeProxySnapshot::Platform)
}

#[cfg(target_os = "windows")]
pub(in crate::system_proxy) fn apply(
    expected: &crate::system_proxy::native::NativeProxySnapshot,
    change: &crate::system_proxy::native::NativeProxyChange<'_>,
) -> crate::system_proxy::native::NativeWriteAttempt {
    use crate::system_proxy::native::{NativeProxySnapshot, NativeWriteAttempt};
    NativeWriteAttempt::run(expected, |witness| {
        let before = expected.platform()?;
        let key = registry::InternetSettingsKey::open(true)?;
        connection_transaction::apply(&key, before, change, |next| {
            witness.record(NativeProxySnapshot::Platform(next))
        })
        .map(NativeProxySnapshot::Platform)
    })
}

#[cfg(target_os = "windows")]
pub(in crate::system_proxy) fn restore(
    expected: &crate::system_proxy::native::NativeProxySnapshot,
    previous: &crate::system_proxy::native::NativeProxySnapshot,
) -> crate::system_proxy::native::NativeWriteAttempt {
    use crate::system_proxy::native::{NativeProxySnapshot, NativeWriteAttempt};
    NativeWriteAttempt::run(expected, |witness| {
        let before = expected.platform()?;
        let previous = previous.platform()?;
        let key = registry::InternetSettingsKey::open(true)?;
        connection_transaction::restore(&key, before, previous, |next| {
            witness.record(NativeProxySnapshot::Platform(next))
        })
        .map(NativeProxySnapshot::Platform)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_manual_enable_with_only_other_protocols_is_not_a_complete_display_summary() {
        let mut enabled_values = InternetSettingsRegistryValues::new();
        for value in [
            proxy_enable_value(true),
            registry_string_value("ProxyServer", "ftp=files:21;socks=socket:1080"),
        ] {
            enabled_values.insert(ascii_folded_value_name(&value.name), value);
        }
        let enabled = WinInetProxySnapshot::from_values(enabled_values.clone()).unwrap();
        enabled_values.insert(
            ascii_folded_value_name(&utf16_units("ProxyEnable")),
            proxy_enable_value(false),
        );
        let disabled = WinInetProxySnapshot::from_values(enabled_values).unwrap();
        assert!(!enabled.summary().http().is_enabled());
        assert!(!enabled.summary().https().is_enabled());
        assert!(!enabled.summary().pac().is_enabled());
        assert_eq!(enabled.summary(), disabled.summary());
        assert!(registry_manual_proxy_enabled(&enabled.values).unwrap());
        assert!(!registry_manual_proxy_enabled(&disabled.values).unwrap());
        assert!(!enabled.same_state(&disabled).unwrap());
        assert!(!enabled.legacy_release_is_safe());
    }
    #[test]
    fn string_bytes_preserve_type_terminators_and_extra_protocols() {
        let value = "http=old:80;https=secure:443;ftp=ftp:21;socks=socks:1080";
        let mut values = InternetSettingsRegistryValues::new();
        let mut original = registry_string_value("ProxyServer", value);
        original.kind = REG_EXPAND_SZ_KIND;
        values.insert(ascii_folded_value_name(&original.name), original.clone());
        assert_eq!(registry_string(&values, "ProxyServer").unwrap(), value);
        let cached = values.clone();
        values.insert(
            ascii_folded_value_name(&utf16_units("ProxyServer")),
            registry_string_value("ProxyServer", "http=new:7890;https=new:7890"),
        );
        values.insert(
            ascii_folded_value_name(&utf16_units("ProxyServer")),
            original,
        );
        assert!(values == cached);
    }
    #[test]
    fn manual_endpoint_change_keeps_unmanaged_protocol_entries() {
        assert_eq!(
            manual_proxy_server(
                "FTP=old:21;http=old:80;socks=old:1080; custom=value",
                "new",
                7890
            ),
            "http=new:7890;https=new:7890;FTP=old:21;socks=old:1080; custom=value"
        );
    }
    #[test]
    fn typed_enable_and_string_values_produce_the_display_projection() {
        let mut values = InternetSettingsRegistryValues::new();
        for value in [
            proxy_enable_value(true),
            registry_string_value("ProxyServer", "http=old:80;https=secure:443;ftp=other:21"),
            registry_string_value("ProxyOverride", "localhost;*.local"),
        ] {
            values.insert(ascii_folded_value_name(&value.name), value);
        }
        let actual = summary(&values, "WinINET").unwrap();
        assert!(actual.enabled && actual.secure_enabled);
        assert_eq!((actual.server.as_str(), actual.port), ("old", 80));
        assert_eq!(actual.bypass, ["localhost", "*.local"]);
    }

    #[test]
    fn absent_empty_and_type_distinct_values_do_not_compare_equal() {
        let absent = InternetSettingsRegistryValues::new();
        let mut empty = absent.clone();
        empty.insert(
            ascii_folded_value_name(&utf16_units("ProxyServer")),
            registry_string_value("ProxyServer", ""),
        );
        assert!(absent != empty);
        let mut expanded = empty.clone();
        expanded
            .get_mut(&ascii_folded_value_name(&utf16_units("ProxyServer")))
            .unwrap()
            .kind = REG_EXPAND_SZ_KIND;
        assert!(expanded != empty);
    }
}
