use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::{ConnectionSort, ConnectionTransport, present_connections};
use zenclash_core::ConnectionsSnapshot;

pub(super) struct ConnectionProjection {
    pub(super) snapshot: Arc<ConnectionsSnapshot>,
    pub(super) query: String,
    pub(super) transport: ConnectionTransport,
    pub(super) sort: ConnectionSort,
    pub(super) order: Vec<usize>,
    pub(super) processes: Vec<(String, u64)>,
    pub(super) protocols: Vec<(String, u64)>,
    pub(super) by_id: std::collections::HashMap<String, usize>,
    pub(super) durations: Vec<String>,
    pub(super) policies: Vec<String>,
}

#[derive(Default)]
pub(super) struct ProjectionWorker {
    pub(super) policy: Option<String>,
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: crate::runtime_host::observation::PageReadTask,
}

impl ProjectionWorker {
    pub(super) fn cancel(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.task.cancel();
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    pub(super) fn start(
        &mut self,
        runtime: &tokio::runtime::Handle,
        snapshot: Arc<ConnectionsSnapshot>,
        query: String,
        transport: ConnectionTransport,
        sort: ConnectionSort,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<ConnectionProjection>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();
        let policy = self.policy.clone();
        let task = runtime.spawn(async move {
            // Coalesce typing before admitting CPU work; only one projection may run per view.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let guard = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                if current.load(Ordering::Acquire) != generation {
                    return None;
                }
                let mut order = present_connections(&snapshot.connections, &query, transport, sort);
                if let Some(policy) = &policy {
                    order
                        .retain(|&index| snapshot.connections[index].chains.last() == Some(policy));
                }
                let policies = snapshot
                    .connections
                    .iter()
                    .filter_map(|connection| connection.chains.last().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let (processes, protocols) = distributions(&snapshot.connections);
                let by_id = snapshot
                    .connections
                    .iter()
                    .enumerate()
                    .map(|(index, connection)| (connection.id.clone(), index))
                    .collect();
                let now = chrono::Utc::now();
                let durations = snapshot
                    .connections
                    .iter()
                    .map(|connection| connection_duration(&connection.start, now))
                    .collect();
                (current.load(Ordering::Acquire) == generation).then_some(ConnectionProjection {
                    snapshot,
                    query,
                    transport,
                    sort,
                    order,
                    processes,
                    protocols,
                    by_id,
                    durations,
                    policies,
                })
            })
            .await
            .map_err(|error| error.to_string())
        });
        self.task.replace(&task);
        (generation, task)
    }
}

type CountGroups = Vec<(String, u64)>;

pub(super) fn connection_duration(start: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(start) else {
        return "—".into();
    };
    let seconds = now.signed_duration_since(start).num_seconds().max(0);
    if seconds >= 3600 {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn distributions(connections: &[zenclash_core::Connection]) -> (CountGroups, CountGroups) {
    let mut processes = std::collections::BTreeMap::<String, u64>::new();
    let mut protocols = std::collections::BTreeMap::<String, u64>::new();
    for connection in connections {
        *processes
            .entry(connection.metadata.process.clone())
            .or_default() += 1;
        *protocols
            .entry(connection.metadata.network.to_uppercase())
            .or_default() += 1;
    }
    let mut processes = processes.into_iter().collect::<Vec<_>>();
    processes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    processes.truncate(5);
    (processes, protocols.into_iter().collect())
}

impl Drop for ProjectionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests;
