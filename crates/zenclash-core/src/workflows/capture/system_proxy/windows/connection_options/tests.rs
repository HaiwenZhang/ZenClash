use windows_sys::Win32::Networking::WinInet::{
    PROXY_TYPE_AUTO_DETECT, PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
};

use super::*;

fn cached_options(flags: u32) -> DefaultConnectionOptions {
    DefaultConnectionOptions {
        flags,
        proxy_server: Some("http=proxy.lan:8080;https=secure.lan:8443;ftp=ftp.lan:21".into()),
        proxy_bypass: Some("localhost;<local>".into()),
        auto_config_url: Some("http://cached.test/proxy.pac".into()),
    }
}

#[test]
fn disabled_flags_preserve_cached_endpoints_and_pac_url() {
    let options = cached_options(PROXY_TYPE_DIRECT | PROXY_TYPE_AUTO_DETECT);
    let summary = default_connection_status("WinINET", || Ok(options.clone())).unwrap();
    assert!(!summary.active());
    assert_eq!(summary.http().host(), "proxy.lan");
    assert_eq!(summary.https().port(), 8443);
    assert_eq!(summary.pac().url(), "http://cached.test/proxy.pac");
    assert_eq!(summary.bypass, ["localhost", "<local>"]);
}

#[test]
fn manual_and_pac_flags_are_independent_of_cached_string_presence() {
    let mut options = cached_options(PROXY_TYPE_PROXY | PROXY_TYPE_AUTO_PROXY_URL);
    options.auto_config_url = None;
    let summary = default_connection_status("WinINET", || Ok(options.clone())).unwrap();
    assert!(summary.http().is_enabled());
    assert!(summary.https().is_enabled());
    assert!(summary.pac().is_enabled());
    assert_eq!(summary.pac().url(), "");
}

#[test]
fn change_to_hidden_protocol_rejects_equal_display_projection() {
    let before = cached_options(PROXY_TYPE_PROXY);
    let mut after = before.clone();
    after.proxy_server = Some("http=proxy.lan:8080;https=secure.lan:8443;ftp=other.lan:21".into());
    assert_eq!(
        before.clone().summary("WinINET").unwrap(),
        after.clone().summary("WinINET").unwrap()
    );
    let mut reads = [before, after].into_iter();
    let error = default_connection_status("WinINET", || Ok(reads.next().unwrap())).unwrap_err();
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::ObservationUnstable)));
}

#[test]
fn change_to_unprojected_flags_rejects_equal_display_projection() {
    let before = cached_options(PROXY_TYPE_DIRECT);
    let mut after = before.clone();
    after.flags |= PROXY_TYPE_AUTO_DETECT | 0x8000_0000;
    let mut reads = [before, after].into_iter();
    let error = default_connection_status("WinINET", || Ok(reads.next().unwrap())).unwrap_err();
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::ObservationUnstable)));
}

#[test]
fn second_query_failure_retains_native_error_and_never_publishes_default() {
    let mut count = 0;
    let error = default_connection_status("WinINET", || {
        count += 1;
        if count == 1 {
            Ok(cached_options(PROXY_TYPE_DIRECT))
        } else {
            Err(SystemProxyError::PlatformIo {
                backend: crate::SystemProxyBackendKind::WinInet,
                operation: "isolated query",
                source: std::io::Error::from_raw_os_error(5),
            }
            .into())
        }
    })
    .unwrap_err();
    assert_eq!(count, 2);
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::PlatformIo { source, .. }
            if source.raw_os_error() == Some(5))));
}

#[test]
fn malformed_native_endpoint_is_typed_without_exposing_input() {
    let mut options = cached_options(PROXY_TYPE_PROXY);
    options.proxy_server = Some("http=sensitive.lan:invalid".into());
    let error = default_connection_status("WinINET", || Ok(options.clone())).unwrap_err();
    assert!(!error.to_string().contains("sensitive.lan"));
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::InvalidNativeSnapshot { .. })));
}

#[test]
fn unsupported_ui_flags_retry_only_the_legacy_flags_option() {
    let mut options = Vec::new();
    let flags = query_connection_flags(|option| {
        options.push(option);
        if option == INTERNET_PER_CONN_FLAGS_UI {
            Err(SystemProxyError::PlatformIo {
                backend: crate::SystemProxyBackendKind::WinInet,
                operation: "isolated flags query",
                source: std::io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32),
            }
            .into())
        } else {
            Ok(PROXY_TYPE_AUTO_PROXY_URL)
        }
    })
    .unwrap();
    assert_eq!(
        options,
        [INTERNET_PER_CONN_FLAGS_UI, INTERNET_PER_CONN_FLAGS]
    );
    assert_eq!(flags, PROXY_TYPE_AUTO_PROXY_URL);
}

#[test]
fn access_denied_flags_query_does_not_retry_or_publish_disabled_state() {
    let mut options = Vec::new();
    let error = query_connection_flags(|option| {
        options.push(option);
        Err(SystemProxyError::PlatformIo {
            backend: crate::SystemProxyBackendKind::WinInet,
            operation: "isolated flags query",
            source: std::io::Error::from_raw_os_error(5),
        }
        .into())
    })
    .unwrap_err();
    assert_eq!(options, [INTERNET_PER_CONN_FLAGS_UI]);
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::PlatformIo { source, .. }
            if source.raw_os_error() == Some(5))));
}

#[test]
fn named_ras_service_cannot_be_mislabeled_as_the_default_connection() {
    let mut reads = 0;
    let error = default_connection_status("named RAS connection", || {
        reads += 1;
        Ok(cached_options(PROXY_TYPE_DIRECT))
    })
    .unwrap_err();
    assert_eq!(reads, 0);
    assert!(matches!(error, crate::MihomoError::SystemProxy(source)
        if matches!(source.as_ref(), SystemProxyError::OwnershipChanged)));
}
