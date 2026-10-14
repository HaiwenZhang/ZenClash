use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use std::sync::Arc;
use zenclash_core::{TrafficDimension, TrafficHistorySelection, TrafficHistorySnapshot};

use super::page_observation::PageReadTask;

mod actions;
mod model;
pub(super) use model::TrafficHistory;
mod query_presentation;
use query_presentation::{TrafficHistoryPresentation, TrafficHistoryQueryPlan};
mod view;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TrafficRange {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl TrafficRange {
    const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Month];

    fn label(self) -> String {
        match self {
            Self::Hour => zenclash_i18n::text("traffic.range.hour"),
            Self::Day => zenclash_i18n::text("traffic.range.day"),
            Self::Week => zenclash_i18n::text("traffic.range.week"),
            Self::Month => zenclash_i18n::text("traffic.range.month"),
        }
    }

    const fn duration_ms(self) -> u64 {
        match self {
            Self::Hour => 60 * 60 * 1_000,
            Self::Day => 24 * 60 * 60 * 1_000,
            Self::Week => 7 * 24 * 60 * 60 * 1_000,
            Self::Month => 30 * 24 * 60 * 60 * 1_000,
        }
    }

    const fn bucket_ms(self) -> u64 {
        match self {
            Self::Hour => 5 * 60 * 1_000,
            Self::Day => 60 * 60 * 1_000,
            Self::Week => 6 * 60 * 60 * 1_000,
            Self::Month => 24 * 60 * 60 * 1_000,
        }
    }
}

#[derive(Debug, Default)]
struct TrafficHistoryQueryState {
    pub(super) range: TrafficRange,
    pub(super) dimension: TrafficDimension,
    selected_parent: Option<String>,
    selected_detail: Option<String>,
    pub(super) loading: bool,
    pub(super) clear_confirmation: bool,
    pub(super) last_error: Option<String>,
    presentation: Option<TrafficHistoryPresentation>,
    query_revision: u64,
    request_sequence: u64,
    task: PageReadTask,
    query_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug)]
struct TrafficHistoryReadIdentity {
    plan: Arc<TrafficHistoryQueryPlan>,
    request_sequence: u64,
    query_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrafficHistoryFreshness {
    Loading,
    Fresh { observed_at_ms: u64 },
    Stale { observed_at_ms: u64 },
    Failed,
}

impl TrafficHistoryQueryState {
    pub(super) fn begin_query(
        &mut self,
        source: PathBuf,
        end_ms: u64,
    ) -> TrafficHistoryReadIdentity {
        self.request_sequence = self.request_sequence.wrapping_add(1);
        self.loading = true;
        let selection = match (self.selected_parent.clone(), self.selected_detail.clone()) {
            (Some(parent), Some(detail)) => TrafficHistorySelection::Detail { parent, detail },
            (Some(parent), None) => TrafficHistorySelection::Parent { parent },
            (None, _) => TrafficHistorySelection::Overview,
        };
        TrafficHistoryReadIdentity {
            plan: Arc::new(TrafficHistoryQueryPlan::new(
                source,
                self.range,
                self.dimension,
                selection,
                end_ms,
            )),
            request_sequence: self.request_sequence,
            query_revision: self.query_revision,
        }
    }

    pub(super) fn completed_query(&self) -> Option<&TrafficHistoryPresentation> {
        self.presentation.as_ref()
    }

    fn presentation_for_source(
        &self,
        source: Option<&Path>,
    ) -> Option<&TrafficHistoryPresentation> {
        self.completed_query()
            .filter(|presentation| source == Some(presentation.plan().source()))
    }

    fn can_inspect(&self, source: Option<&Path>) -> bool {
        self.presentation.as_ref().is_some_and(|presentation| {
            presentation
                .plan()
                .matches_scope(source, self.range, self.dimension)
        })
    }

    fn change_range(&mut self, range: TrafficRange) -> bool {
        if self.range == range {
            return false;
        }
        self.range = range;
        self.reset_selection();
        true
    }

    fn change_dimension(&mut self, dimension: TrafficDimension) -> bool {
        if self.dimension == dimension {
            return false;
        }
        self.dimension = dimension;
        self.reset_selection();
        true
    }

    fn select_parent(&mut self, label: String, source: Option<&Path>) -> bool {
        if !self.can_inspect(source) {
            return false;
        }
        self.selected_parent = Some(label);
        self.selected_detail = None;
        self.query_revision = self.query_revision.wrapping_add(1);
        true
    }

    fn can_select_detail(&self, source: Option<&Path>) -> bool {
        self.can_inspect(source)
            && self
                .presentation
                .as_ref()
                .and_then(TrafficHistoryPresentation::selected_parent)
                .is_some_and(|parent| self.selected_parent.as_deref() == Some(parent))
    }

    fn select_detail(&mut self, label: String, source: Option<&Path>) -> bool {
        if !self.can_select_detail(source) {
            return false;
        }
        self.selected_detail = Some(label);
        self.query_revision = self.query_revision.wrapping_add(1);
        true
    }

    fn reset_selection(&mut self) {
        self.selected_parent = None;
        self.selected_detail = None;
        self.query_revision = self.query_revision.wrapping_add(1);
    }

    pub(super) fn cancel_query(&mut self) {
        self.task.cancel();
        self.request_sequence = self.request_sequence.wrapping_add(1);
        self.loading = false;
    }

    pub(super) fn complete_query(
        &mut self,
        token: &TrafficHistoryReadIdentity,
        source: Option<&Path>,
        result: Result<TrafficHistorySnapshot, String>,
        observed_at_ms: u64,
    ) -> bool {
        if token.request_sequence != self.request_sequence || !self.loading {
            return false;
        }
        self.loading = false;
        if token.query_revision != self.query_revision || source != Some(token.plan.source()) {
            return true;
        }
        match result {
            Ok(payload) => {
                self.presentation = Some(TrafficHistoryPresentation::new(
                    token.plan.clone(),
                    payload,
                    observed_at_ms,
                ));
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error),
        }
        false
    }

    fn freshness(&self, source: Option<&Path>) -> TrafficHistoryFreshness {
        if self.loading {
            return TrafficHistoryFreshness::Loading;
        }
        match (
            self.presentation_for_source(source)
                .map(TrafficHistoryPresentation::observed_at_ms),
            self.last_error.is_some(),
        ) {
            (Some(observed_at_ms), false) => TrafficHistoryFreshness::Fresh { observed_at_ms },
            (Some(observed_at_ms), true) => TrafficHistoryFreshness::Stale { observed_at_ms },
            (None, true) => TrafficHistoryFreshness::Failed,
            (None, false) => TrafficHistoryFreshness::Loading,
        }
    }

    pub(super) fn release_results(&mut self) {
        self.cancel_query();
        self.presentation = None;
        self.selected_parent = None;
        self.selected_detail = None;
        self.loading = false;
        self.clear_confirmation = false;
        self.last_error = None;
        self.query_revision = self.query_revision.wrapping_add(1);
    }
}

fn dimension_label(dimension: TrafficDimension) -> String {
    match dimension {
        TrafficDimension::Host => zenclash_i18n::text("traffic.dimension.host"),
        TrafficDimension::SourceIp => zenclash_i18n::text("traffic.dimension.source"),
        TrafficDimension::Outbound => zenclash_i18n::text("traffic.dimension.outbound"),
        TrafficDimension::Process => zenclash_i18n::text("traffic.dimension.process"),
    }
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

impl TrafficHistoryReadIdentity {
    pub(super) fn plan(&self) -> Arc<TrafficHistoryQueryPlan> {
        self.plan.clone()
    }
}

#[cfg(test)]
mod tests;
