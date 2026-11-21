use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use zenclash_core::RuleCatalog;

use super::{RuleFilterSelection, rule_matches};

pub(super) struct RuleProjection {
    pub(super) snapshot: Arc<RuleCatalog>,
    pub(super) filter: RuleFilterSelection,
    pub(super) confirmed_disabled: HashMap<usize, bool>,
    pub(super) indices: Vec<usize>,
    pub(super) types: Vec<String>,
    pub(super) policies: Vec<String>,
}

pub(super) fn rule_is_disabled(
    rule: &zenclash_core::Rule,
    confirmed_disabled: &HashMap<usize, bool>,
) -> bool {
    rule.index
        .and_then(|index| confirmed_disabled.get(&index).copied())
        .unwrap_or_else(|| rule.extra.as_ref().is_some_and(|stats| stats.disabled))
}

#[derive(Default)]
pub(super) struct ProjectionWorker {
    generation: Arc<AtomicU64>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: super::super::page_observation::PageReadTask,
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
        filter: RuleFilterSelection,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<RuleProjection>, String>>,
    ) {
        self.start_with_options(
            runtime,
            snapshot,
            filter,
            HashMap::new(),
            Duration::from_millis(80),
        )
    }

    pub(super) fn start_with_options(
        &mut self,
        runtime: &tokio::runtime::Handle,
        snapshot: Arc<RuleCatalog>,
        filter: RuleFilterSelection,
        confirmed_disabled: HashMap<usize, bool>,
        debounce: Duration,
    ) -> (
        u64,
        tokio::task::JoinHandle<Result<Option<RuleProjection>, String>>,
    ) {
        self.cancel();
        let generation = self.generation.load(Ordering::Acquire);
        let current = self.generation.clone();
        let gate = self.gate.clone();

        let task = runtime.spawn(async move {
            if !debounce.is_zero() {
                tokio::time::sleep(debounce).await;
            }
            let permit = gate.lock_owned().await;
            if current.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let (indices, types, policies) = {
                    let mut indices = Vec::new();
                    let mut kinds = std::collections::BTreeSet::<&str>::new();
                    let mut policies = std::collections::BTreeSet::<&str>::new();
                    for (index, rule) in snapshot.rules.iter().enumerate() {
                        if index % 256 == 0 && current.load(Ordering::Acquire) != generation {
                            return None;
                        }
                        policies.insert(rule.proxy.as_str());
                        if (!filter.disabled_only || rule_is_disabled(rule, &confirmed_disabled))
                            && rule_matches(rule, &filter.query)
                            && filter
                                .rule_kind
                                .as_ref()
                                .is_none_or(|kind| *kind == rule.kind)
                            && filter
                                .policy
                                .as_ref()
                                .is_none_or(|policy| *policy == rule.proxy)
                        {
                            indices.push(index);
                        }
                        kinds.insert(rule.kind.as_str());
                    }
                    (
                        indices,
                        kinds.into_iter().map(str::to_owned).collect(),
                        policies.into_iter().map(str::to_owned).collect(),
                    )
                };
                (current.load(Ordering::Acquire) == generation).then_some(RuleProjection {
                    snapshot,
                    filter,
                    confirmed_disabled,
                    indices,
                    types,
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
        let filter = RuleFilterSelection {
            disabled_only: true,
            ..Default::default()
        };
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
        let (_, task) = worker.start(&tokio::runtime::Handle::current(), snapshot, filter);
        assert_eq!(task.await.unwrap().unwrap().unwrap().indices, [0]);
    }

    #[tokio::test]
    async fn disabled_filter_honors_confirmed_disabled_overrides() {
        let mut worker = ProjectionWorker::default();
        let filter = RuleFilterSelection {
            disabled_only: true,
            ..Default::default()
        };
        let snapshot = Arc::new(RuleCatalog {
            rules: vec![
                zenclash_core::Rule {
                    index: Some(10),
                    extra: Some(zenclash_core::RuleRuntimeStats {
                        disabled: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                zenclash_core::Rule {
                    index: Some(11),
                    extra: Some(zenclash_core::RuleRuntimeStats {
                        disabled: false,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                zenclash_core::Rule {
                    index: Some(12),
                    extra: None,
                    ..Default::default()
                },
            ],
        });
        let confirmed = HashMap::from([(10, false), (11, true), (12, true)]);
        let (_, task) = worker.start_with_options(
            &tokio::runtime::Handle::current(),
            snapshot,
            filter,
            confirmed.clone(),
            Duration::ZERO,
        );
        let projection = task.await.unwrap().unwrap().unwrap();
        assert_eq!(projection.indices, [1, 2]);
        assert_eq!(projection.confirmed_disabled, confirmed);
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
        let (_, task) = worker.start(
            &tokio::runtime::Handle::current(),
            source,
            RuleFilterSelection {
                query: "match".into(),
                ..Default::default()
            },
        );
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
        let filter = RuleFilterSelection {
            rule_kind: Some("DOMAIN".into()),
            policy: Some("Proxy".into()),
            query: "example".into(),
            ..Default::default()
        };
        let (_, task) = worker.start(&tokio::runtime::Handle::current(), source, filter);
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
        let (old, task) = worker.start(
            &runtime,
            snapshot("old"),
            RuleFilterSelection {
                query: "old".into(),
                ..Default::default()
            },
        );
        let old_projection = task.await.unwrap().unwrap().unwrap();
        let source = snapshot("new");
        let (latest, task) = worker.start(
            &runtime,
            source.clone(),
            RuleFilterSelection {
                query: "missing".into(),
                ..Default::default()
            },
        );
        let projection = task.await.unwrap().unwrap().unwrap();
        assert!(!worker.is_current(old));
        assert_eq!(old_projection.indices, [0]);
        assert!(worker.is_current(latest));
        assert!(Arc::ptr_eq(&projection.snapshot, &source));
        assert!(projection.indices.is_empty());
        assert_eq!(projection.filter.query, "missing");
    }

    #[tokio::test]
    async fn releasing_a_pending_search_prevents_its_publication() {
        let mut worker = ProjectionWorker::default();
        let (generation, task) = worker.start(
            &tokio::runtime::Handle::current(),
            snapshot("old"),
            RuleFilterSelection::default(),
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
            RuleFilterSelection {
                query: "example".into(),
                ..Default::default()
            },
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
