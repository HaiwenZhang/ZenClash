use super::{
    MihomoClient, Page, PathBuf, RuntimeData, SystemNetworkSnapshot, SystemProxyManager,
    TunPermissionManager,
};

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
    let system_proxy_task = tokio::task::spawn_blocking(|| {
        let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    });
    let (config, proxies, connections, system_proxy) = tokio::join!(
        client.runtime_config(),
        client.proxy_catalog(),
        client.connections_snapshot(),
        system_proxy_task
    );
    Ok(RuntimeData::Dashboard {
        config: config.map_err(|error| error.to_string())?,
        proxies: proxies.map_err(|error| error.to_string())?,
        connections: connections.map_err(|error| error.to_string())?,
        system_proxy: system_proxy.map_err(|error| {
            zenclash_i18n::text_with(
                "runtime.load_errors.system_proxy",
                &[("error", error.to_string())],
            )
        })??,
    })
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
