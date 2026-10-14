use zenclash_core::TrafficDimension;

use super::TrafficRange;
use crate::pages::runtime::{Context, Page, RuntimePage};

impl RuntimePage {
    pub(in crate::pages::runtime) fn refresh_traffic_history(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Traffic
            || !self.live_updates_enabled()
            || self.traffic_history.is_loading()
        {
            return;
        }
        let token = self.page_task_token_for(Page::Traffic);
        let read = match self.traffic_history.start_read() {
            Ok(read) => read,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let completion = read.finish().await;
            let _ = this.update(cx, |this, cx| {
                if !this.is_page_task_current(token) || !this.live_updates_enabled() {
                    return;
                }
                let refresh_again = this.traffic_history.complete_read(completion);
                if refresh_again {
                    this.refresh_traffic_history(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn set_traffic_range(&mut self, range: TrafficRange, cx: &mut Context<Self>) {
        if self.traffic_history.change_range(range) {
            self.refresh_traffic_history(cx);
            cx.notify();
        }
    }

    pub(super) fn set_traffic_dimension(
        &mut self,
        dimension: TrafficDimension,
        cx: &mut Context<Self>,
    ) {
        if self.traffic_history.change_dimension(dimension) {
            self.refresh_traffic_history(cx);
            cx.notify();
        }
    }

    pub(super) fn select_traffic_parent(&mut self, label: String, cx: &mut Context<Self>) {
        if self.traffic_history.select_parent(label) {
            self.refresh_traffic_history(cx);
            cx.notify();
        }
    }

    pub(super) fn select_traffic_detail(&mut self, label: String, cx: &mut Context<Self>) {
        if self.traffic_history.select_detail(label) {
            self.refresh_traffic_history(cx);
            cx.notify();
        }
    }

    pub(super) fn request_clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        self.traffic_history.request_clear_confirmation();
        cx.notify();
    }

    pub(super) fn cancel_clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        self.traffic_history.cancel_clear_confirmation();
        cx.notify();
    }

    pub(super) fn clear_traffic_history(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.page, Page::Traffic | Page::Settings) {
            return;
        }
        let clear = match self.traffic_history.prepare_clear() {
            Ok(clear) => clear,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(token) = self.begin_scoped_mutation(
            self.page,
            crate::pages::runtime::busy::MutationDomain::TrafficHistory,
        ) else {
            return;
        };
        self.traffic_history.invalidate_for_clear();
        let task = clear.start();
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
                this.finish_mutation(token);
                this.traffic_history.complete_clear(&result);
                match result {
                    Ok(()) => {
                        if this.is_page_task_current(token) {
                            this.notice = Some(zenclash_i18n::text("traffic.notices.cleared"));
                        }
                        this.refresh_traffic_history(cx);
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
