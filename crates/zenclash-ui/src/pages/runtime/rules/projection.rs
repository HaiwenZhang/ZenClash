use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use zenclash_core::RuleCatalog;

use super::rule_matches;

pub(super) struct RuleProjection {
    pub(super) snapshot: Arc<RuleCatalog>,
    pub(super) query: String,
    pub(super) indices: Vec<usize>,
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
        snapshot: Arc<RuleCatalog>,
        query: String,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<RuleProjection>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();
        let task = runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let mut indices = Vec::new();
                for (index, rule) in snapshot.rules.iter().enumerate() {
                    if index % 256 == 0 && current.load(Ordering::Acquire) != generation {
                        return None;
                    }
                    if rule_matches(rule, &query) {
                        indices.push(index);
                    }
                }
                (current.load(Ordering::Acquire) == generation).then_some(RuleProjection {
                    snapshot,
                    query,
                    indices,
                })
            })
            .await
            .map_err(|error| error.to_string())
        });
        self.task.replace(&task);
        (generation, task)
    }
}

impl Drop for ProjectionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(value: &str) -> Arc<RuleCatalog> {
        Arc::new(RuleCatalog {
            rules: vec![zenclash_core::Rule {
                payload: value.into(),
                ..Default::default()
            }],
        })
    }

    #[tokio::test]
    async fn replacing_the_query_and_snapshot_rejects_a_completed_old_projection() {
        let mut worker = ProjectionWorker::default();
        let runtime = tokio::runtime::Handle::current();
        let (old, task) = worker.start(&runtime, snapshot("old"), "old".into());
        let old_projection = task.await.unwrap().unwrap().unwrap();
        let source = snapshot("new");
        let (latest, task) = worker.start(&runtime, source.clone(), "missing".into());
        let projection = task.await.unwrap().unwrap().unwrap();
        assert!(!worker.is_current(old));
        assert_eq!(old_projection.indices, [0]);
        assert!(worker.is_current(latest));
        assert!(Arc::ptr_eq(&projection.snapshot, &source));
        assert!(projection.indices.is_empty());
        assert_eq!(projection.query, "missing");
    }

    #[tokio::test]
    async fn releasing_a_pending_search_prevents_its_publication() {
        let mut worker = ProjectionWorker::default();
        let (generation, task) = worker.start(
            &tokio::runtime::Handle::current(),
            snapshot("old"),
            String::new(),
        );
        worker.cancel();
        assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
        assert!(!worker.is_current(generation));
    }

    #[tokio::test]
    async fn prepared_indices_support_pagination_without_repeating_the_search() {
        let mut worker = ProjectionWorker::default();
        let snapshot = Arc::new(RuleCatalog {
            rules: (0..250)
                .map(|index| zenclash_core::Rule {
                    payload: format!("example-{index}"),
                    ..Default::default()
                })
                .collect(),
        });
        let (generation, task) = worker.start(
            &tokio::runtime::Handle::current(),
            snapshot,
            "example".into(),
        );
        let projection = task.await.unwrap().unwrap().unwrap();
        let page =
            super::super::list_page(projection.indices.len(), 2, super::super::RULES_PER_PAGE);
        assert_eq!(
            &projection.indices[page.start..page.end],
            &(200..250).collect::<Vec<_>>()
        );
        assert!(worker.is_current(generation));
    }
}
