use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::time::{Duration, Instant};

use super::{MihomoClient, MihomoResult};
use crate::ConnectionsSnapshot;

#[derive(Default)]
pub(super) struct ConnectionCache {
    epoch: AtomicU64,
    snapshot: tokio::sync::Mutex<Option<CachedConnections>>,
}

struct CachedConnections {
    epoch: u64,
    binding_generation: u64,
    received: Instant,
    value: Arc<ConnectionsSnapshot>,
}

impl MihomoClient {
    pub(super) async fn shared_connections(&self) -> MihomoResult<Arc<ConnectionsSnapshot>> {
        let mut cache = self.connections.snapshot.lock().await;
        loop {
            let epoch = self.connections.epoch.load(Ordering::Acquire);
            let binding_generation = self.binding.generation();
            if let Some(cached) = cache.as_ref()
                && cached.epoch == epoch
                && cached.binding_generation == binding_generation
                && self.binding.is_current(binding_generation)
                && cached.received.elapsed() < Duration::from_secs(1)
            {
                return Ok(cached.value.clone());
            }
            let value = Arc::new(self.get_json::<ConnectionsSnapshot>("/connections").await?);
            // A close or restart can invalidate an in-flight response.
            if self.connections.epoch.load(Ordering::Acquire) != epoch
                || !self.binding.is_current(binding_generation)
            {
                continue;
            }
            *cache = Some(CachedConnections {
                epoch,
                binding_generation,
                received: Instant::now(),
                value: value.clone(),
            });
            return Ok(value);
        }
    }

    pub(crate) fn invalidate_connections(&self) {
        self.connections.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MihomoEndpoint;
    use crate::client::transport::ControllerBackend;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn cache_filled_between_invalidation_and_binding_publication_is_not_reused() {
        let old = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let new = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let old_endpoint = MihomoEndpoint::new(format!("http://{}", old.local_addr().unwrap()), "");
        let new_endpoint = MihomoEndpoint::new(format!("http://{}", new.local_addr().unwrap()), "");
        let old_server = tokio::spawn(async move {
            for total in [10, 11] {
                let (mut stream, _) = old.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                let body = format!(r#"{{"connections":[],"downloadTotal":{total}}}"#);
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let new_server = tokio::spawn(async move {
            let (mut stream, _) = new.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            let body = r#"{"connections":[],"downloadTotal":20}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = MihomoClient::new(old_endpoint).unwrap();
        assert_eq!(
            client.shared_connections().await.unwrap().download_total,
            10
        );
        // Schedule an old-controller reader in the switch's invalidate/publish gap.
        client.invalidate_connections();
        assert_eq!(
            client.shared_connections().await.unwrap().download_total,
            11
        );
        client
            .binding
            .replace(ControllerBackend::Direct(new_endpoint))
            .unwrap();
        assert_eq!(
            client.shared_connections().await.unwrap().download_total,
            20
        );
        old_server.await.unwrap();
        new_server.await.unwrap();
    }
}
