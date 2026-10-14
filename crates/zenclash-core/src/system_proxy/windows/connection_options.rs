//! Default/LAN WinINET options; observed facts are separate from mutation ownership.

use windows_sys::Win32::{
    Foundation::ERROR_INVALID_PARAMETER,
    Networking::WinInet::{
        INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_FLAGS_UI, PROXY_TYPE_AUTO_DETECT,
        PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
    },
};

use super::{parse_bypass, parse_proxy_server};
use crate::{MihomoResult, SystemProxyError, SystemProxySummary};

#[cfg(target_os = "windows")]
mod native;
#[cfg(target_os = "windows")]
pub(super) use native::query_default;
#[cfg(target_os = "windows")]
pub(super) use native::set_default_option;

/// The default/LAN connection only; no RAS connection or existing Internet handle.
/// These observed values grant no right to overwrite or restore native settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DefaultConnectionOptions {
    pub(in crate::system_proxy::windows) flags: u32,
    pub(in crate::system_proxy::windows) proxy_server: Option<String>,
    pub(in crate::system_proxy::windows) proxy_bypass: Option<String>,
    pub(in crate::system_proxy::windows) auto_config_url: Option<String>,
}

impl DefaultConnectionOptions {
    pub(super) fn summary(self, service: &str) -> MihomoResult<SystemProxySummary> {
        let (server, port, secure_server, secure_port) =
            parse_proxy_server(self.proxy_server.as_deref().unwrap_or_default())
                .map_err(|_| invalid_snapshot())?;
        let manual_enabled = self.flags & PROXY_TYPE_PROXY != 0;
        Ok(SystemProxySummary {
            service: service.into(),
            enabled: manual_enabled && !server.is_empty(),
            server,
            port,
            secure_enabled: manual_enabled && !secure_server.is_empty(),
            secure_server,
            secure_port,
            bypass: parse_bypass(self.proxy_bypass.as_deref().unwrap_or_default()),
            auto_enabled: self.flags & PROXY_TYPE_AUTO_PROXY_URL != 0,
            auto_url: self.auto_config_url.unwrap_or_default(),
        })
    }

    pub(super) fn canonicalized(mut self) -> Self {
        // NULL and an allocated empty string represent the same API option.
        // Persistent absence/type/bytes remain in the separate raw registry input.
        for value in [
            &mut self.proxy_server,
            &mut self.proxy_bypass,
            &mut self.auto_config_url,
        ] {
            if value.is_none() {
                *value = Some(String::new());
            }
        }
        self
    }

    pub(super) fn disabled_flags(&self) -> u32 {
        (self.flags & !(PROXY_TYPE_PROXY | PROXY_TYPE_AUTO_PROXY_URL | PROXY_TYPE_AUTO_DETECT))
            | PROXY_TYPE_DIRECT
    }

    pub(super) fn manual_server(&self) -> &str {
        self.proxy_server.as_deref().unwrap_or_default()
    }

    pub(super) fn manual_enabled(&self) -> bool {
        self.flags & PROXY_TYPE_PROXY != 0
    }

    pub(super) fn replacement(&self, option: &DefaultConnectionOption) -> Self {
        let mut next = self.clone();
        match option {
            DefaultConnectionOption::Flags(flags) => next.flags = *flags,
            DefaultConnectionOption::ProxyServer(value) => next.proxy_server = Some(value.clone()),
            DefaultConnectionOption::ProxyBypass(value) => next.proxy_bypass = Some(value.clone()),
            DefaultConnectionOption::AutoConfigUrl(value) => {
                next.auto_config_url = Some(value.clone())
            }
        }
        next
    }

    pub(super) fn restoration_options(&self) -> [DefaultConnectionOption; 4] {
        [
            DefaultConnectionOption::ProxyServer(self.proxy_server.clone().unwrap_or_default()),
            DefaultConnectionOption::ProxyBypass(self.proxy_bypass.clone().unwrap_or_default()),
            DefaultConnectionOption::AutoConfigUrl(
                self.auto_config_url.clone().unwrap_or_default(),
            ),
            DefaultConnectionOption::Flags(self.flags),
        ]
    }
}

/// One documented default/LAN option, written independently for prefix evidence.
pub(super) enum DefaultConnectionOption {
    Flags(u32),
    ProxyServer(String),
    ProxyBypass(String),
    AutoConfigUrl(String),
}

pub(super) fn default_connection_status(
    service: &str,
    mut query: impl FnMut() -> MihomoResult<DefaultConnectionOptions>,
) -> MihomoResult<SystemProxySummary> {
    if service != "WinINET" {
        return Err(SystemProxyError::OwnershipChanged.into());
    }
    let before = query()?;
    let after = query()?;
    // Compare every queried input, including values omitted by the display.
    // This detects observed changes; it cannot establish atomic OS isolation.
    if before != after {
        return Err(SystemProxyError::ObservationUnstable.into());
    }
    after.summary(service)
}

fn invalid_snapshot() -> crate::MihomoError {
    SystemProxyError::InvalidNativeSnapshot { cause: None }.into()
}

fn query_connection_flags(mut query: impl FnMut(u32) -> MihomoResult<u32>) -> MihomoResult<u32> {
    match query(INTERNET_PER_CONN_FLAGS_UI) {
        Err(crate::MihomoError::SystemProxy(source))
            if matches!(source.as_ref(), SystemProxyError::PlatformIo { source, .. }
                if source.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)) =>
        {
            query(INTERNET_PER_CONN_FLAGS)
        }
        result => result,
    }
}

#[cfg(test)]
mod tests;
