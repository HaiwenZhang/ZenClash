use super::{
    MihomoClient, Page, RuntimeData, SubStoreClient, SystemNetworkSnapshot, SystemProxyManager,
};

pub(super) async fn load_page(client: MihomoClient, page: Page) -> Result<RuntimeData, String> {
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
            let (proxy, rules) = tokio::try_join!(
                client.proxy_provider_catalog(),
                client.rule_provider_catalog()
            )
            .map_err(|error| error.to_string())?;
            Ok(RuntimeData::Resources { proxy, rules })
        }
        Page::SystemProxy => {
            let status_task = tokio::task::spawn_blocking(|| {
                let manager = SystemProxyManager::detect().map_err(|error| error.to_string())?;
                manager.status().map_err(|error| error.to_string())
            });
            let (config, status) = tokio::join!(client.runtime_config(), status_task);
            let config = config.map_err(|error| error.to_string())?;
            let status = status.map_err(|error| format!("系统代理状态任务异常结束：{error}"))??;
            Ok(RuntimeData::SystemProxy { config, status })
        }
        Page::Network => {
            let system_task = tokio::task::spawn_blocking(SystemNetworkSnapshot::detect);
            let (config, system) = tokio::join!(client.runtime_config(), system_task);
            let config = config.map_err(|error| error.to_string())?;
            let system = system.map_err(|error| format!("系统网络状态任务异常结束：{error}"))?;
            Ok(RuntimeData::Network { config, system })
        }
        Page::SubStore => {
            let client = SubStoreClient::from_env().map_err(|error| error.to_string())?;
            Ok(RuntimeData::SubStore(client.snapshot().await))
        }
        Page::Logs => Ok(RuntimeData::Empty),
        _ => client
            .runtime_config()
            .await
            .map(RuntimeData::Config)
            .map_err(|error| error.to_string()),
    }
}
