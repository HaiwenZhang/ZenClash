//! Completed history facts retain the exact plan submitted to the database.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use zenclash_core::{
    TrafficAggregate, TrafficDimension, TrafficHistoryQuery, TrafficHistorySelection,
    TrafficHistorySnapshot, TrafficOverview,
};

use super::TrafficRange;

#[derive(Debug)]
pub(in crate::pages::runtime) struct TrafficHistoryQueryPlan {
    source: PathBuf,
    range: TrafficRange,
    query: TrafficHistoryQuery,
    selection: TrafficHistorySelection,
}

impl TrafficHistoryQueryPlan {
    pub(super) fn new(
        source: PathBuf,
        range: TrafficRange,
        dimension: TrafficDimension,
        selection: TrafficHistorySelection,
        end_ms: u64,
    ) -> Self {
        Self {
            source,
            range,
            query: TrafficHistoryQuery {
                dimension,
                start_ms: end_ms.saturating_sub(range.duration_ms().saturating_sub(1)),
                end_ms,
                bucket_ms: range.bucket_ms(),
            },
            selection,
        }
    }

    pub(in crate::pages::runtime) fn query(&self) -> &TrafficHistoryQuery {
        &self.query
    }
    pub(in crate::pages::runtime) fn selection(&self) -> &TrafficHistorySelection {
        &self.selection
    }
    pub(super) fn source(&self) -> &Path {
        &self.source
    }
    pub(super) fn matches_scope(
        &self,
        source: Option<&Path>,
        range: TrafficRange,
        dimension: TrafficDimension,
    ) -> bool {
        source == Some(self.source()) && range == self.range && dimension == self.query.dimension
    }
}

#[derive(Debug)]
pub(in crate::pages::runtime) struct TrafficHistoryPresentation {
    plan: Arc<TrafficHistoryQueryPlan>,
    snapshot: TrafficHistorySnapshot,
    observed_at_ms: u64,
}

impl TrafficHistoryPresentation {
    pub(super) fn new(
        plan: Arc<TrafficHistoryQueryPlan>,
        snapshot: TrafficHistorySnapshot,
        observed_at_ms: u64,
    ) -> Self {
        Self {
            plan,
            snapshot,
            observed_at_ms,
        }
    }
    pub(super) fn plan(&self) -> &TrafficHistoryQueryPlan {
        &self.plan
    }
    pub(super) fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }
    pub(in crate::pages::runtime) fn overview(&self) -> &TrafficOverview {
        &self.snapshot.overview
    }
    pub(super) fn details(&self) -> &[TrafficAggregate] {
        &self.snapshot.details
    }
    pub(super) fn proxy_stats(&self) -> &[TrafficAggregate] {
        &self.snapshot.proxy_stats
    }
    pub(super) fn range(&self) -> TrafficRange {
        self.plan.range
    }
    pub(super) fn dimension(&self) -> TrafficDimension {
        self.plan.query.dimension
    }
    pub(super) fn selected_parent(&self) -> Option<&str> {
        match &self.plan.selection {
            TrafficHistorySelection::Parent { parent }
            | TrafficHistorySelection::Detail { parent, .. } => Some(parent),
            _ => None,
        }
    }
    pub(super) fn selected_detail(&self) -> Option<&str> {
        match &self.plan.selection {
            TrafficHistorySelection::Detail { detail, .. } => Some(detail),
            _ => None,
        }
    }
}
