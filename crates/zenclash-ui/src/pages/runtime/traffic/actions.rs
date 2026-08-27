use zenclash_core::{TrafficDimension, TrafficHistoryQuery, TrafficOverview};

use super::{TrafficHistoryPayload, TrafficRange, finish_history_refresh, unix_millis};
use crate::pages::runtime::{Context, Page, RuntimePage};

impl RuntimePage {
    pub(in crate::pages::runtime) fn refresh_traffic_history(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Traffic || self.traffic_history.loading {
            return;
        }
        let Some(store) = self.traffic_history_store.clone() else {
            self.error = Some(zenclash_i18n::text("traffic.errors.database_open"));
            cx.notify();
            return;
        };
        let query = self.traffic_history_query();
        let selected_parent = self.traffic_history.selected_parent.clone();
        let selected_detail = self.traffic_history.selected_detail.clone();
        let token = self.page_task_token_for(Page::Traffic);
        let revision = self.traffic_history.revision;
        self.traffic_history.loading = true;
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let overview = store.overview(&query).map_err(|error| error.to_string())?;
                let details = selected_parent.as_deref().map_or_else(
                    || Ok(Vec::new()),
                    |parent| {
                        store
                            .details(&query, parent)
                            .map_err(|error| error.to_string())
                    },
                )?;
                let proxy_stats = selected_parent
                    .as_deref()
                    .zip(selected_detail.as_deref())
                    .map_or_else(
                        || Ok(Vec::new()),
                        |(parent, detail)| {
                            store
                                .proxy_stats(&query, parent, detail)
                                .map_err(|error| error.to_string())
                        },
                    )?;
                Ok::<_, String>(TrafficHistoryPayload {
                    overview,
                    details,
                    proxy_stats,
                })
            })
            .await
            .map_err(|error| {
                zenclash_i18n::text_with(
                    "traffic.errors.query_task",
                    &[("error", error.to_string())],
                )
            })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with("traffic.errors.task", &[("error", error.to_string())])
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                let refresh_again = finish_history_refresh(this, token, revision, result);
                if refresh_again {
                    this.refresh_traffic_history(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn traffic_history_query(&self) -> TrafficHistoryQuery {
        let end_ms = unix_millis();
        let duration = self.traffic_history.range.duration_ms();
        TrafficHistoryQuery {
            dimension: self.traffic_history.dimension,
            start_ms: end_ms.saturating_sub(duration.saturating_sub(1)),
            end_ms,
            bucket_ms: self.traffic_history.range.bucket_ms(),
        }
    }

    pub(super) fn set_traffic_range(&mut self, range: TrafficRange, cx: &mut Context<Self>) {
        if self.traffic_history.range != range {
            self.traffic_history.range = range;
            self.traffic_history.revision = self.traffic_history.revision.wrapping_add(1);
            self.reset_traffic_drill_down();
            self.refresh_traffic_history(cx);
        }
    }

    pub(super) fn set_traffic_dimension(
        &mut self,
        dimension: TrafficDimension,
        cx: &mut Context<Self>,
    ) {
        if self.traffic_history.dimension != dimension {
            self.traffic_history.dimension = dimension;
            self.traffic_history.revision = self.traffic_history.revision.wrapping_add(1);
            self.reset_traffic_drill_down();
            self.refresh_traffic_history(cx);
        }
    }

    pub(super) fn select_traffic_parent(&mut self, label: String, cx: &mut Context<Self>) {
        self.traffic_history.selected_parent = Some(label);
        self.traffic_history.revision = self.traffic_history.revision.wrapping_add(1);
        self.traffic_history.selected_detail = None;
        self.traffic_history.details.clear();
        self.traffic_history.proxy_stats.clear();
        self.refresh_traffic_history(cx);
    }

    pub(super) fn select_traffic_detail(&mut self, label: String, cx: &mut Context<Self>) {
        self.traffic_history.selected_detail = Some(label);
        self.traffic_history.revision = self.traffic_history.revision.wrapping_add(1);
        self.traffic_history.proxy_stats.clear();
        self.refresh_traffic_history(cx);
    }

    fn reset_traffic_drill_down(&mut self) {
        self.traffic_history.selected_parent = None;
        self.traffic_history.selected_detail = None;
        self.traffic_history.details.clear();
        self.traffic_history.proxy_stats.clear();
    }

    pub(super) fn request_clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        self.traffic_history.clear_confirmation = true;
        cx.notify();
    }

    pub(super) fn cancel_clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        self.traffic_history.clear_confirmation = false;
        cx.notify();
    }

    pub(super) fn clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.traffic_history_store.clone() else {
            self.error = Some(zenclash_i18n::text("traffic.errors.database_unavailable"));
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(Page::Traffic) else {
            return;
        };
        self.traffic_history.revision = self.traffic_history.revision.wrapping_add(1);
        let task = self.runtime.spawn(async move {
            tokio::task::spawn_blocking(move || store.clear().map_err(|error| error.to_string()))
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "traffic.errors.clear_task",
                        &[("error", error.to_string())],
                    )
                })?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "traffic.errors.clear_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.mutating = false;
                this.traffic_history.clear_confirmation = false;
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.traffic_history.overview = TrafficOverview::default();
                        this.reset_traffic_drill_down();
                        this.notice = Some(zenclash_i18n::text("traffic.notices.cleared"));
                        this.refresh_traffic_history(cx);
                    }
                    Ok(()) => {}
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
