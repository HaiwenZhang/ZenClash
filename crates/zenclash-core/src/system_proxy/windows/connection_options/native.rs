use std::{mem::size_of, ptr};

use windows_sys::Win32::{
    Foundation::GlobalFree,
    Networking::WinInet::{
        INTERNET_OPTION_PER_CONNECTION_OPTION, INTERNET_PER_CONN_AUTOCONFIG_URL,
        INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_OPTIONW,
        INTERNET_PER_CONN_PROXY_BYPASS, INTERNET_PER_CONN_PROXY_SERVER, InternetQueryOptionW,
        InternetSetOptionW,
    },
    System::Memory::GlobalSize,
};

use super::{
    DefaultConnectionOption, DefaultConnectionOptions, invalid_snapshot, query_connection_flags,
};
use crate::{MihomoResult, SystemProxyBackendKind, SystemProxyError};

const MAX_STRING_BYTES: usize = 1024 * 1024;

pub(in crate::system_proxy::windows) fn set_default_option(
    value: &DefaultConnectionOption,
) -> MihomoResult<()> {
    let mut string = Vec::new();
    let (option_id, flags) = match value {
        DefaultConnectionOption::Flags(flags) => (INTERNET_PER_CONN_FLAGS, Some(*flags)),
        DefaultConnectionOption::ProxyServer(value) => {
            string.extend(value.encode_utf16().chain(std::iter::once(0)));
            (INTERNET_PER_CONN_PROXY_SERVER, None)
        }
        DefaultConnectionOption::ProxyBypass(value) => {
            string.extend(value.encode_utf16().chain(std::iter::once(0)));
            (INTERNET_PER_CONN_PROXY_BYPASS, None)
        }
        DefaultConnectionOption::AutoConfigUrl(value) => {
            string.extend(value.encode_utf16().chain(std::iter::once(0)));
            (INTERNET_PER_CONN_AUTOCONFIG_URL, None)
        }
    };
    let mut option = INTERNET_PER_CONN_OPTIONW {
        dwOption: option_id,
        ..Default::default()
    };
    if let Some(flags) = flags {
        option.Value.dwValue = flags;
    } else {
        option.Value.pszValue = string.as_mut_ptr();
    }
    let mut list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: ptr::null_mut(),
        dwOptionCount: 1,
        dwOptionError: 0,
        pOptions: &raw mut option,
    };
    // SAFETY: The typed option determines the initialized union member. The
    // NUL-terminated string/list/option buffers live through this synchronous call.
    // NULL handle/connection changes only the default/LAN connection settings.
    let succeeded = unsafe {
        InternetSetOptionW(
            ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            (&raw mut list).cast(),
            list.dwSize,
        )
    };
    if succeeded == 0 {
        return Err(SystemProxyError::PlatformIo {
            backend: SystemProxyBackendKind::WinInet,
            operation: "InternetSetOptionW(default connection)",
            source: std::io::Error::last_os_error(),
        }
        .into());
    }
    Ok(())
}

pub(in crate::system_proxy::windows) fn query_default() -> MihomoResult<DefaultConnectionOptions> {
    let flags = query_connection_flags(|option_id| {
        let mut option = INTERNET_PER_CONN_OPTIONW {
            dwOption: option_id,
            ..Default::default()
        };
        query_options(std::slice::from_mut(&mut option))?;
        // SAFETY: The successful single-option query requested DWORD flags.
        Ok(unsafe { option.Value.dwValue })
    })?;
    let mut strings = ReturnedStrings::new();
    query_options(&mut strings.options)?;
    Ok(DefaultConnectionOptions {
        flags,
        proxy_server: strings.decode(0)?,
        proxy_bypass: strings.decode(1)?,
        auto_config_url: strings.decode(2)?,
    })
}

fn query_options(options: &mut [INTERNET_PER_CONN_OPTIONW]) -> MihomoResult<()> {
    let mut list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: ptr::null_mut(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    };
    let mut length = list.dwSize;
    // SAFETY: The list and its options are live mutable buffers of the specified
    // sizes. NULL connection/handle selects default/LAN, without native writes.
    let succeeded = unsafe {
        InternetQueryOptionW(
            ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            (&raw mut list).cast(),
            &raw mut length,
        )
    };
    if succeeded == 0 {
        // Capture before releasing any returned strings can change GetLastError.
        return Err(SystemProxyError::PlatformIo {
            backend: SystemProxyBackendKind::WinInet,
            operation: "InternetQueryOptionW(default connection)",
            source: std::io::Error::last_os_error(),
        }
        .into());
    }
    Ok(())
}

/// Owns all allocated query results, including partially populated failures.
struct ReturnedStrings {
    options: [INTERNET_PER_CONN_OPTIONW; 3],
}

impl ReturnedStrings {
    fn new() -> Self {
        Self {
            options: [
                INTERNET_PER_CONN_PROXY_SERVER,
                INTERNET_PER_CONN_PROXY_BYPASS,
                INTERNET_PER_CONN_AUTOCONFIG_URL,
            ]
            .map(|option_id| INTERNET_PER_CONN_OPTIONW {
                dwOption: option_id,
                ..Default::default()
            }),
        }
    }

    fn decode(&self, index: usize) -> MihomoResult<Option<String>> {
        // SAFETY: All three options are string-valued and were zero-initialized
        // before the successful query; this owner retains their allocations.
        let pointer = unsafe { self.options[index].Value.pszValue };
        if pointer.is_null() {
            return Ok(None);
        }
        // SAFETY: WinINET documents GlobalFree ownership of its returned strings.
        // GlobalSize bounds every following read of this fixed global allocation.
        let bytes = unsafe { GlobalSize(pointer.cast()) };
        if bytes < size_of::<u16>() || bytes > MAX_STRING_BYTES {
            return Err(invalid_snapshot());
        }
        let mut units = Vec::new();
        for index in 0..bytes / size_of::<u16>() {
            // SAFETY: The allocation is retained, and this aligned u16 is within
            // GlobalSize. Stop at NUL without reading uninitialized tail padding.
            let unit = unsafe { pointer.add(index).read() };
            if unit == 0 {
                return String::from_utf16(&units)
                    .map(Some)
                    .map_err(|_| invalid_snapshot());
            }
            units.push(unit);
        }
        Err(invalid_snapshot())
    }
}

impl Drop for ReturnedStrings {
    fn drop(&mut self) {
        for option in &mut self.options {
            // SAFETY: Every option is string-valued and starts at NULL. WinINET
            // transfers each allocated result to us even if another option fails.
            let pointer = unsafe { option.Value.pszValue };
            if !pointer.is_null() {
                // SAFETY: Release this owned WinINET result exactly once.
                unsafe { GlobalFree(pointer.cast()) };
                option.Value.pszValue = ptr::null_mut();
            }
        }
    }
}
