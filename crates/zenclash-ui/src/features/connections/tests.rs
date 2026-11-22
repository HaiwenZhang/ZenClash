use super::*;

fn connection_summary(connection: &zenclash_core::Connection) -> String {
    let mut parts = Vec::with_capacity(4);
    for value in [
        &connection.metadata.network,
        &connection.metadata.process,
        &connection.rule,
    ] {
        if !value.is_empty() {
            parts.push(value.clone());
        }
    }
    if !connection.chains.is_empty() {
        parts.push(connection.chains.join(" → "));
    }
    if parts.is_empty() {
        zenclash_i18n::text("connections.empty.details")
    } else {
        parts.join(" · ")
    }
}

#[test]
fn protocol_filter_and_traffic_sort_apply_before_pagination() {
    let connections = vec![
        zenclash_core::Connection {
            id: "udp".into(),
            download: 100,
            metadata: zenclash_core::ConnectionMetadata {
                network: "UDP".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        zenclash_core::Connection {
            id: "small".into(),
            download: 1,
            metadata: zenclash_core::ConnectionMetadata {
                network: "tcp".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        zenclash_core::Connection {
            id: "large".into(),
            download: 50,
            metadata: zenclash_core::ConnectionMetadata {
                network: "TCP".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    ];
    let result = present_connections(
        &connections,
        "",
        ConnectionTransport::Tcp,
        ConnectionSort::Download,
    );
    assert_eq!(
        result
            .iter()
            .map(|&index| connections[index].id.as_str())
            .collect::<Vec<_>>(),
        ["large", "small"]
    );
}

#[test]
fn newest_sort_compares_instants_and_puts_invalid_dates_last() {
    let connections = vec![
        zenclash_core::Connection {
            id: "invalid".into(),
            ..Default::default()
        },
        zenclash_core::Connection {
            id: "older".into(),
            start: "2026-01-01T10:00:00+08:00".into(),
            ..Default::default()
        },
        zenclash_core::Connection {
            id: "newer".into(),
            start: "2026-01-01T03:00:00Z".into(),
            ..Default::default()
        },
    ];
    let result = present_connections(
        &connections,
        "",
        ConnectionTransport::All,
        ConnectionSort::Newest,
    );
    assert_eq!(
        result
            .iter()
            .map(|&index| connections[index].id.as_str())
            .collect::<Vec<_>>(),
        ["newer", "older", "invalid"]
    );
}

#[test]
fn connection_controls_keep_identity_after_reordering() {
    let id = connection_element_id("close-connection", "connection-a");
    assert_eq!(
        id,
        connection_element_id("close-connection", "connection-a")
    );
    assert_ne!(
        id,
        connection_element_id("close-connection", "connection-b")
    );
    assert_ne!(
        id,
        connection_element_id("connection-details", "connection-a")
    );
}

#[test]
fn equivalent_edits_keep_the_running_query_and_real_edits_replace_it() {
    let mut current = "example.com".to_owned();
    for incoming in ["example.com", "  example.com  ", "EXAMPLE.COM"] {
        assert!(!replace_connection_query(&mut current, incoming));
        assert_eq!(current, "example.com");
    }
    assert!(replace_connection_query(&mut current, " example.org "));
    assert_eq!(current, "example.org");
    assert!(replace_connection_query(&mut current, " "));
    assert!(current.is_empty());
    assert!(!replace_connection_query(&mut current, "\t"));
    assert!(replace_connection_query(&mut current, "节点甲"));
    assert!(replace_connection_query(&mut current, "节点乙"));
}

#[test]
fn filter_matches_connection_identity_and_route_fields() {
    let connection = zenclash_core::Connection {
        metadata: zenclash_core::ConnectionMetadata {
            host: "Example.COM".into(),
            destination_ip: "203.0.113.8".into(),
            process: "Browser".into(),
            ..Default::default()
        },
        rule: "DomainSuffix".into(),
        chains: vec!["Hong Kong".into()],
        ..Default::default()
    };

    for query in ["example", "203.0.113", "browser", "domainsuffix", "hong"] {
        assert!(connection_matches(
            &connection,
            &normalize_connection_query(query)
        ));
    }
    assert!(!connection_matches(
        &connection,
        &normalize_connection_query("direct")
    ));
    assert_eq!(
        connection_summary(&connection),
        "Browser · DomainSuffix · Hong Kong"
    );
}

mod history_tests {
    use super::super::ConnectionMetricHistory;
    use std::sync::Arc;
    use zenclash_core::{Connection, ConnectionsSnapshot};

    fn snapshot(upload: u64) -> Arc<ConnectionsSnapshot> {
        Arc::new(ConnectionsSnapshot {
            connections: vec![Connection::default()],
            upload_total: upload,
            download_total: upload * 2,
            memory: 1024,
        })
    }

    #[test]
    fn metric_history_deduplicates_filtered_snapshots_and_resets_on_core_or_counter_change() {
        let mut history = ConnectionMetricHistory::default();
        let first = snapshot(100);
        history.observe(1, &first);
        history.observe(1, &first);
        assert_eq!(history.samples.len(), 1);
        history.observe(1, &snapshot(200));
        assert_eq!(
            history
                .points(1)
                .iter()
                .map(|point| point.1)
                .collect::<Vec<_>>(),
            [100., 200.]
        );
        history.observe(2, &snapshot(300));
        assert_eq!(history.samples.len(), 1);
        history.observe(2, &snapshot(10));
        assert_eq!(history.samples.len(), 1);
        assert_eq!(history.samples[0], [1, 10, 20, 1024]);
    }

    #[test]
    fn metric_history_retains_only_observed_recent_samples_without_retaining_connection_snapshots()
    {
        let mut history = ConnectionMetricHistory::default();
        let first = snapshot(1);
        let old = Arc::downgrade(&first);
        history.observe(1, &first);
        drop(first);
        for upload in 2..=120 {
            history.observe(1, &snapshot(upload));
        }
        assert!(old.upgrade().is_none());
        assert_eq!(history.samples.len(), 60);
        assert_eq!(history.samples.front().unwrap()[1], 61);
        assert_eq!(history.samples.back().unwrap()[1], 120);
    }
}
