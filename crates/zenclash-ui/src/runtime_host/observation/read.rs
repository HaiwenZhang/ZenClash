//! Ordinary read dispatch with one pinned runtime across capability requests.

use super::PageSnapshotData;
use super::readers;
use crate::foundation::actions::Page;
use zenclash_core::{CoreSession, MihomoClient};

pub(crate) async fn load_page(
    client: MihomoClient,
    page: Page,
) -> Result<PageSnapshotData, String> {
    load_page_with_core(client, page, None).await
}

pub(crate) async fn load_page_with_core(
    client: MihomoClient,
    page: Page,
    core: Option<CoreSession>,
) -> Result<PageSnapshotData, String> {
    let client = client
        .pin_current_runtime()
        .map_err(|error| error.to_string())?;
    let result = Box::pin(load_page_on_runtime(client.clone(), page, core)).await;
    client
        .verify_current_runtime()
        .map_err(|error| error.to_string())?;
    result
}

async fn load_page_on_runtime(
    client: MihomoClient,
    page: Page,
    core: Option<CoreSession>,
) -> Result<PageSnapshotData, String> {
    match page {
        Page::Home => readers::home::load(client).await,
        Page::Mihomo => readers::mihomo::load(client).await,
        Page::Profiles => readers::profiles::load(client).await,
        Page::Connections | Page::Traffic => readers::connections::load(client).await,
        Page::Rules => readers::rules::load(client).await,
        Page::Resources => readers::resources::load(client).await,
        Page::SystemProxy => readers::system_proxy::load(client).await,
        Page::Network => readers::network::load(client).await,
        Page::Tun => readers::tun::load(client, core).await,
        Page::Settings => readers::settings::load(client).await,
        Page::Logs | Page::Override => Ok(PageSnapshotData::Empty),
        Page::Dns | Page::Sniffer | Page::Proxies => client
            .runtime_config()
            .await
            .map(PageSnapshotData::Config)
            .map_err(|error| error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::PageReadTask;
    use super::*;
    use zenclash_core::MihomoEndpoint;
    #[tokio::test]
    async fn cancelling_a_page_read_closes_a_slow_controller_request() {
        use tokio::{
            io::AsyncReadExt,
            time::{Duration, timeout},
        };

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let client = MihomoClient::new(MihomoEndpoint::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "",
        ))
        .unwrap();
        let mut owner = PageReadTask::default();
        let read = tokio::spawn(load_page(client, Page::Rules));
        owner.replace(&read);
        let (mut connection, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(
                request.len() < 8192,
                "cancel fixture header exceeds its budget"
            );
            request.push(
                timeout(Duration::from_secs(2), connection.read_u8())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert!(request.starts_with(b"GET /rules "));
        owner.cancel();
        assert!(read.await.unwrap_err().is_cancelled());
        let mut buffer = [0_u8; 1];
        assert_eq!(
            timeout(Duration::from_secs(2), connection.read(&mut buffer))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
