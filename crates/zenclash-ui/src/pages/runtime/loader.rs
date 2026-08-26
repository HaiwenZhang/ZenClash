use super::{
    MihomoClient, Page, PathBuf, RuntimeData, SubStoreClient, SystemNetworkSnapshot,
    SystemProxyManager, TunPermissionManager,
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
        Page::SubStore => {
            let client = SubStoreClient::from_env().map_err(|error| error.to_string())?;
            Ok(RuntimeData::SubStore(client.snapshot().await))
        }
        Page::Settings => load_settings(client).await,
        Page::Logs => Ok(RuntimeData::Empty),
        _ => client
            .runtime_config()
            .await
            .map(RuntimeData::Config)
            .map_err(|error| error.to_string()),
    }
}

async fn load_system_proxy(client: MihomoClient) -> Result<RuntimeData, String> {
    let status_task = tokio::task::spawn_blocking(|| {
        let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
        manager.status().map_err(|error| error.to_string())
    });
    let (config, status) = tokio::join!(client.runtime_config(), status_task);
    let config = config.map_err(|error| error.to_string())?;
    let status = status.map_err(|error| format!("系统代理状态任务异常结束：{error}"))??;
    Ok(RuntimeData::SystemProxy { config, status })
}

async fn load_network(client: MihomoClient) -> Result<RuntimeData, String> {
    let system_task = tokio::task::spawn_blocking(SystemNetworkSnapshot::detect);
    let (config, system) = tokio::join!(client.runtime_config(), system_task);
    let config = config.map_err(|error| error.to_string())?;
    let system = system.map_err(|error| format!("系统网络状态任务异常结束：{error}"))?;
    Ok(RuntimeData::Network { config, system })
}

async fn load_tun(
    client: MihomoClient,
    mihomo_binary: Option<PathBuf>,
) -> Result<RuntimeData, String> {
    let permission_task = tokio::task::spawn_blocking(move || {
        let binary = mihomo_binary
            .ok_or_else(|| "当前连接的是外部内核，无法确定可执行文件路径".to_owned())?;
        TunPermissionManager::new(binary)
            .and_then(|manager| manager.status())
            .map_err(|error| error.to_string())
    });
    let (config, permissions) = tokio::join!(client.runtime_config(), permission_task);
    let config = config.map_err(|error| error.to_string())?;
    let permissions = permissions.map_err(|error| format!("TUN 权限状态任务异常结束：{error}"))?;
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
    let autostart = autostart.map_err(|error| format!("自动启动状态任务异常结束：{error}"))??;
    Ok(RuntimeData::Settings { config, autostart })
}
