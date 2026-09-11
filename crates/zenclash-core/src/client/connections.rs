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
    received: Instant,
    value: Arc<ConnectionsSnapshot>,
}

impl MihomoClient {
    pub(super) async fn shared_connections(&self) -> MihomoResult<Arc<ConnectionsSnapshot>> {
        let mut cache = self.connections.snapshot.lock().await;
        loop {
            let epoch = self.connections.epoch.load(Ordering::Acquire);
            if let Some(cached) = cache.as_ref()
                && cached.epoch == epoch
                && cached.received.elapsed() < Duration::from_secs(1)
            {
                return Ok(cached.value.clone());
            }
            let value = Arc::new(self.get_json::<ConnectionsSnapshot>("/connections").await?);
            // A close or restart can invalidate an in-flight response.
            if self.connections.epoch.load(Ordering::Acquire) != epoch {
                continue;
            }
            *cache = Some(CachedConnections {
                epoch,
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
