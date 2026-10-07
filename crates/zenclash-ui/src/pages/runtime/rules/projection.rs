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
    pub(super) types: Vec<String>,
    pub(super) policies: Vec<String>,
}

#[derive(Default)]
pub(super) struct ProjectionWorker {
    pub(super) kind: Option<String>,
    pub(super) policy: Option<String>,
    pub(super) disabled_only: bool,
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::super::loader::PageReadTask,
}

impl ProjectionWorker {
    #[cfg(test)]
    pub(super) fn hold_projection(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.gate
            .clone()
            .try_lock_owned()
            .expect("projection must be idle")
    }

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
        let kind = self.kind.clone();
        let policy = self.policy.clone();
        let disabled_only = self.disabled_only;
        let task = runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let mut indices = Vec::new();
                let mut kinds = std::collections::BTreeMap::<String, u64>::new();
                let mut policies = std::collections::BTreeSet::new();
                for (index, rule) in snapshot.rules.iter().enumerate() {
                    if index % 256 == 0 && current.load(Ordering::Acquire) != generation {
                        return None;
                    }
                    policies.insert(rule.proxy.clone());
                    if (!disabled_only || rule.extra.as_ref().is_some_and(|stats| stats.disabled))
                        && rule_matches(rule, &query)
                        && kind.as_ref().is_none_or(|kind| *kind == rule.kind)
                        && policy.as_ref().is_none_or(|policy| *policy == rule.proxy)
                    {
                        indices.push(index);
                    }
                    *kinds.entry(rule.kind.clone()).or_default() += 1;
                }
                (current.load(Ordering::Acquire) == generation).then_some(RuleProjection {
                    snapshot,
                    query,
                    indices,
                    types: kinds.keys().cloned().collect(),
                    policies: policies.into_iter().collect(),
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

    #[tokio::test]
    async fn disabled_filter_excludes_unknown_and_enabled_rules() {
        let mut worker = ProjectionWorker::default();
        worker.disabled_only = true;
        let snapshot = Arc::new(RuleCatalog {
            rules: vec![
                zenclash_core::Rule {
                    extra: Some(zenclash_core::RuleRuntimeStats {
                        disabled: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                zenclash_core::Rule {
                    extra: Some(Default::default()),
                    ..Default::default()
                },
                zenclash_core::Rule::default(),
            ],
        });
        let (_, task) = worker.start(&tokio::runtime::Handle::current(), snapshot, String::new());
        assert_eq!(task.await.unwrap().unwrap().unwrap().indices, [0]);
    }

    #[tokio::test]
    async fn distribution_uses_full_catalog_and_only_returned_hit_counters() {
        let source = Arc::new(RuleCatalog {
            rules: vec![
                zenclash_core::Rule {
                    kind: "DomainSuffix".into(),
                    payload: "match.example".into(),
                    proxy: "DIRECT".into(),
                    extra: Some(zenclash_core::RuleRuntimeStats {
                        hit_count: 9,
                        miss_count: 500,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                zenclash_core::Rule {
                    kind: "DomainSuffix".into(),
                    payload: "other.example".into(),
                    proxy: "REJECT".into(),
                    ..Default::default()
                },
            ],
        });
        let mut worker = ProjectionWorker::default();
        let (_, task) = worker.start(&tokio::runtime::Handle::current(), source, "match".into());
        let projection = task.await.unwrap().unwrap().unwrap();
        assert_eq!(projection.indices, [0]);
        assert_eq!(projection.types, ["DomainSuffix"]);
        assert_eq!(projection.policies, ["DIRECT", "REJECT"]);
    }

    #[tokio::test]
    async fn type_and_policy_filters_intersect_without_changing_rule_order() {
        let source = Arc::new(RuleCatalog {
            rules: [
                ("DOMAIN", "DIRECT"),
                ("DOMAIN", "Proxy"),
                ("IP-CIDR", "Proxy"),
                ("DOMAIN", "Proxy"),
            ]
            .into_iter()
            .map(|(kind, proxy)| zenclash_core::Rule {
                kind: kind.into(),
                proxy: proxy.into(),
                payload: "example".into(),
                ..Default::default()
            })
            .collect(),
        });
        let mut worker = ProjectionWorker::default();
        worker.kind.replace("DOMAIN".into());
        worker.policy.replace("Proxy".into());
        let (_, task) = worker.start(&tokio::runtime::Handle::current(), source, "example".into());
        let projection = task.await.unwrap().unwrap().unwrap();
        assert_eq!(projection.indices, [1, 3]);
        assert_eq!(projection.policies, ["DIRECT", "Proxy"]);
    }

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
            &(100..150).collect::<Vec<_>>()
        );
        assert!(worker.is_current(generation));
    }
}
