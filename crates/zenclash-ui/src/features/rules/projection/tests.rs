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
    let page = crate::components::page_primitives::list_page(
        projection.indices.len(),
        2,
        super::super::RULES_PER_PAGE,
    );
    assert_eq!(
        &projection.indices[page.start..page.end],
        &(100..150).collect::<Vec<_>>()
    );
    assert!(worker.is_current(generation));
}
