use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::{ConnectionSort, ConnectionTransport, present_connections};
use zenclash_core::ConnectionsSnapshot;

pub(super) struct ConnectionProjection {
    pub(super) snapshot: Arc<ConnectionsSnapshot>,
    pub(super) query: String,
    pub(super) order: Vec<usize>,
    pub(super) processes: Vec<(String, u64)>,
    pub(super) protocols: Vec<(String, u64)>,
    pub(super) by_id: std::collections::HashMap<String, usize>,
    pub(super) durations: Vec<String>,
}

#[derive(Default)]
pub(super) struct ProjectionWorker {
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::super::loader::PageReadTask,
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
                let order = present_connections(&snapshot.connections, &query, transport, sort);
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
                    order,
                    processes,
                    protocols,
                    by_id,
                    durations,
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

fn connection_duration(start: &str, now: chrono::DateTime<chrono::Utc>) -> String {
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
mod tests {
    use super::*;
    use zenclash_core::{Connection, ConnectionsSnapshot};

    #[test]
    fn connection_duration_handles_clock_skew_and_invalid_start_time() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T12:10:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(connection_duration("2026-10-01T12:07:42Z", now), "02:18");
        assert_eq!(connection_duration("2026-10-01T12:11:00Z", now), "00:00");
        assert_eq!(connection_duration("unavailable", now), "—");
    }

    #[test]
    fn snapshot_distributions_count_real_processes_and_normalize_protocols() {
        let mut browser = Connection::default();
        browser.metadata.process = "browser.exe".into();
        browser.metadata.network = "tcp".into();
        let mut unknown = Connection::default();
        unknown.metadata.network = "UDP".into();
        let (processes, protocols) = distributions(&[browser.clone(), browser, unknown]);
        assert_eq!(processes, [("browser.exe".into(), 2), (String::new(), 1)]);
        assert_eq!(protocols, [("TCP".into(), 2), ("UDP".into(), 1)]);
    }

    fn snapshot(host: &str) -> Arc<ConnectionsSnapshot> {
        let mut connection = Connection::default();
        connection.metadata.host = host.into();
        Arc::new(ConnectionsSnapshot {
            connections: vec![connection],
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn continuous_input_cancels_old_search_and_publishes_matching_snapshot() {
        let mut worker = ProjectionWorker::default();
        let runtime = tokio::runtime::Handle::current();
        let (old, first) = worker.start(
            &runtime,
            snapshot("old"),
            "old".into(),
            ConnectionTransport::All,
            ConnectionSort::Default,
        );
        let source = snapshot("new");
        let (latest, second) = worker.start(
            &runtime,
            source.clone(),
            "new".into(),
            ConnectionTransport::All,
            ConnectionSort::Default,
        );
        assert!(first.await.err().is_some_and(|error| error.is_cancelled()));
        let projection = second.await.unwrap().unwrap().unwrap();
        assert!(!worker.is_current(old));
        assert!(worker.is_current(latest));
        assert!(Arc::ptr_eq(&projection.snapshot, &source));
        assert_eq!(projection.order, [0]);
        assert_eq!(projection.query, "new");
    }

    #[tokio::test]
    async fn completed_result_cannot_replace_a_newer_search() {
        let mut worker = ProjectionWorker::default();
        let runtime = tokio::runtime::Handle::current();
        let (old, task) = worker.start(
            &runtime,
            snapshot("old"),
            "old".into(),
            ConnectionTransport::All,
            ConnectionSort::Default,
        );
        let completed = task.await.unwrap().unwrap().unwrap();
        let (new, task) = worker.start(
            &runtime,
            snapshot("new"),
            "missing".into(),
            ConnectionTransport::All,
            ConnectionSort::Default,
        );
        assert!(!worker.is_current(old));
        assert_eq!(completed.snapshot.connections[0].metadata.host, "old");
        let current = task.await.unwrap().unwrap().unwrap();
        assert!(worker.is_current(new));
        assert!(current.order.is_empty());
        assert_eq!(current.snapshot.connections[0].metadata.host, "new");
    }

    #[tokio::test]
    async fn releasing_the_page_prevents_a_pending_search_from_publishing() {
        let mut worker = ProjectionWorker::default();
        let (generation, task) = worker.start(
            &tokio::runtime::Handle::current(),
            snapshot("node"),
            String::new(),
            ConnectionTransport::All,
            ConnectionSort::Default,
        );
        worker.cancel();
        assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
        assert!(!worker.is_current(generation));
    }
}
