//! Populated native screenshot inputs, excluded from production builds.

use super::*;

impl RuntimePage {
    pub(in crate::pages::runtime) fn prepare_home_design_validation(
        &mut self,
        connections: zenclash_core::ConnectionsSnapshot,
    ) {
        self.reconcile_home_generation();
        let generation = self.home.generation;
        let traffic = self.home.chart.prepare_design_validation(generation);
        let observed_at_ms = traffic.updated_at_ms;
        let fresh = |value| Observation::Fresh {
            value,
            observed_at_ms,
        };
        let operational = OperationalSnapshot {
            process: Observation::Fresh {
                value: ProcessStatus {
                    kind: self.core_kind,
                    managed: true,
                    pid: Some(42),
                    started_at_secs: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_secs().saturating_sub(9_378)),
                    running: true,
                    generation,
                    exit_reason: None,
                    recovery_attempts: 0,
                    recovery: ProcessRecoveryStatus::Stable,
                },
                observed_at_ms,
            },
            controller: Observation::Fresh {
                value: zenclash_core::ControllerStatus {
                    version: zenclash_core::VersionInfo {
                        meta: true,
                        version: "v1.19.30".into(),
                    },
                    authenticated: true,
                    compatibility: zenclash_core::ControllerCompatibility::Compatible,
                    generation,
                },
                observed_at_ms,
            },
            streams: StreamStatuses {
                traffic: fresh(StreamStatus {
                    generation,
                    last_success_at_ms: observed_at_ms,
                    upload: traffic.upload,
                    download: traffic.download,
                    ..Default::default()
                }),
                connections: fresh(StreamStatus {
                    generation,
                    last_success_at_ms: observed_at_ms,
                    item_count: connections.connections.len(),
                    memory: 64 * 1024 * 1024,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        self.home
            .flow
            .prepare_design_validation(connections, generation);
        self.home.history.prepare_design_validation();
        self.home.design_validation = Some((operational, traffic));
    }
}
