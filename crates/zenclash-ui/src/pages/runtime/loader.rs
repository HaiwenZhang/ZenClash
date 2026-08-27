use super::{
    MihomoClient, Observation, Page, PathBuf, ProxyOperations, ProxyVisibility, RecoveryAction,
    RuntimeData, SystemNetworkSnapshot, SystemProxyManager, TunPermissionManager,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) async fn load_page(client: MihomoClient, page: Page) -> Result<RuntimeData, String> {
    load_page_with_binary(client, page, None).await
}

pub(super) async fn load_page_with_binary(
    client: MihomoClient,
    page: Page,
    mihomo_binary: Option<PathBuf>,
) -> Result<RuntimeData, String> {
    match page {
        Page::Home => load_dashboard(client).await,
        Page::Mihomo => {
            let (version, config) = tokio::try_join!(client.version(), client.runtime_config())
                .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Core { version, config })
        }
        Page::Profiles => {
            let (config, proxies, rules) = tokio::try_join!(
                client.runtime_config(),
                client.proxy_catalog(),
                client.rule_catalog()
            )
            .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Profile {
                config,
                proxy_count: proxies.proxy_count,
                group_count: proxies.groups.len(),
                rule_count: rules.rules.len(),
            })
        }
        Page::Connections | Page::Traffic => client
            .connections_snapshot()
            .await
            .map(RuntimeData::Connections)
            .map_err(|error| error.to_string()),
        Page::Rules => client
            .rule_catalog()
            .await
            .map(RuntimeData::Rules)
            .map_err(|error| error.to_string()),
        Page::Resources => {
            let (config, proxy, rules) = tokio::try_join!(
                client.runtime_config(),
                client.proxy_provider_catalog(),
                client.rule_provider_catalog()
            )
            .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Resources {
                config,
                proxy,
                rules,
            })
        }
        Page::SystemProxy => load_system_proxy(client).await,
        Page::Network => load_network(client).await,
        Page::Tun => load_tun(client, mihomo_binary).await,
        Page::Settings => load_settings(client).await,
        Page::Logs => Ok(RuntimeData::Empty),
        _ => client
            .runtime_config()
            .await
            .map(RuntimeData::Config)
            .map_err(|error| error.to_string()),
    }
}

async fn load_dashboard(client: MihomoClient) -> Result<RuntimeData, String> {
    let proxy_operations = ProxyOperations::new(client.clone());
    let (config, proxies, connections) = tokio::join!(
        client.runtime_config(),
        proxy_operations.catalog(ProxyVisibility::VisibleOnly),
        client.connections_snapshot(),
    );
    let observed_at_ms = now_ms();
    Ok(RuntimeData::Dashboard {
        config: Observation::record(
            &Observation::Loading,
            config.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::Retry,
        ),
        proxies: Observation::record(
            &Observation::Loading,
            proxies.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::Retry,
        ),
        connections: Observation::record(
            &Observation::Loading,
            connections.map_err(|error| error.to_string()),
            observed_at_ms,
            RecoveryAction::Retry,
        ),
    })
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

async fn load_system_proxy(client: MihomoClient) -> Result<RuntimeData, String> {
    let status_task = tokio::task::spawn_blocking(|| {
        let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    });
    let (config, status) = tokio::join!(client.runtime_config(), status_task);
    let config = config.map_err(|error| error.to_string())?;
    let status = status.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.system_proxy",
            &[("error", error.to_string())],
        )
    })??;
    Ok(RuntimeData::SystemProxy { config, status })
}

async fn load_network(client: MihomoClient) -> Result<RuntimeData, String> {
    let system_task = tokio::task::spawn_blocking(SystemNetworkSnapshot::detect);
    let (config, system) = tokio::join!(client.runtime_config(), system_task);
    let config = config.map_err(|error| error.to_string())?;
    let system = system.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.system_network",
            &[("error", error.to_string())],
        )
    })?;
    Ok(RuntimeData::Network { config, system })
}

async fn load_tun(
    client: MihomoClient,
    mihomo_binary: Option<PathBuf>,
) -> Result<RuntimeData, String> {
    let permission_task = tokio::task::spawn_blocking(move || {
        let binary = mihomo_binary
            .ok_or_else(|| zenclash_i18n::text("runtime.load_errors.external_binary"))?;
        TunPermissionManager::new(binary)
            .and_then(|manager| manager.status())
            .map_err(|error| error.to_string())
    });
    let (config, permissions) = tokio::join!(client.runtime_config(), permission_task);
    let config = config.map_err(|error| error.to_string())?;
    let permissions = permissions.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.tun_permission",
            &[("error", error.to_string())],
        )
    })?;
    Ok(RuntimeData::Tun {
        config,
        permissions,
    })
}

async fn load_settings(client: MihomoClient) -> Result<RuntimeData, String> {
    let autostart_task = tokio::task::spawn_blocking(|| {
        let manager =
            zenclash_core::AutostartManager::discover().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    });
    let (config, autostart) = tokio::join!(client.runtime_config(), autostart_task);
    let config = config.map_err(|error| error.to_string())?;
    let autostart = autostart.map_err(|error| {
        zenclash_i18n::text_with(
            "runtime.load_errors.autostart",
            &[("error", error.to_string())],
        )
    })??;
    Ok(RuntimeData::Settings { config, autostart })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use zenclash_core::MihomoEndpoint;

    use super::*;

    #[tokio::test]
    async fn dashboard_keeps_successful_slices_when_connections_fail() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let bytes = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..bytes]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                let (status, body) = match path {
                    "/configs" => ("200 OK", r#"{"mode":"rule"}"#),
                    "/proxies" => ("200 OK", r#"{"proxies":{}}"#),
                    "/connections" => ("503 Service Unavailable", r#"{"message":"busy"}"#),
                    _ => ("404 Not Found", r#"{"message":"missing"}"#),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client =
            MihomoClient::new(MihomoEndpoint::new(format!("http://{address}"), "")).unwrap();

        let data = load_dashboard(client).await.unwrap();
        server.join().unwrap();

        let RuntimeData::Dashboard {
            config,
            proxies,
            connections,
        } = data
        else {
            panic!("expected dashboard data");
        };
        assert_eq!(
            config.value().map(|config| config.mode.as_str()),
            Some("rule")
        );
        assert!(proxies.is_fresh());
        assert!(matches!(connections, Observation::Failed { .. }));
    }
}
