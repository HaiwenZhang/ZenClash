//! Connects the retained connection view to the page’s shared commands and observation protocol.

use super::super::*;
use super::{ConnectionEvent, ConnectionSnapshotContext};

impl RuntimePage {
    pub(in crate::pages::runtime) fn handle_connection_event(
        &mut self,
        event: ConnectionEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            ConnectionEvent::CloseRequested { id, runtime } => {
                if self.page != Page::Connections
                    || self.core_session.generation() != runtime.configuration_generation
                    || self.profile_path != runtime.profile_path
                {
                    return;
                }
                if let Some(id) = id {
                    self.close_connection(id, cx);
                } else {
                    self.close_all_connections(cx);
                }
            }
            ConnectionEvent::PresentationFailed(error) => {
                if self.page == Page::Connections {
                    self.error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    pub(in crate::pages::runtime) fn update_connection_presentation(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let source = match self.page_observation.data() {
            PageSnapshotData::Connections(snapshot)
                if self.page == Page::Connections
                    && self.ui_visibility.page_visible()
                    && self.page_observation.configuration_generation()
                        == self.core_session.generation() =>
            {
                Some(snapshot.clone())
            }
            _ => None,
        };
        let runtime = ConnectionSnapshotContext {
            configuration_generation: self.page_observation.configuration_generation(),
            profile_path: self.profile_path.clone(),
        };
        self.connections.update(cx, |view, cx| {
            if let Some(source) = source {
                view.present(source, runtime, cx);
            } else {
                view.release_presentation();
            }
        });
    }

    fn close_all_connections(&mut self, cx: &mut Context<Self>) {
        if self.connections.read(cx).has_pending_closes() {
            return;
        }
        let Some(token) = self.begin_mutation(Page::Connections) else {
            return;
        };
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_all_connections()
                .await
                .map_err(|error| error.to_string())?;
            client
                .connections_snapshot()
                .await
                .map(|data| PageSnapshotData::Connections(std::sync::Arc::new(data)))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok(data) => {
                        if this.is_page_task_current(token) {
                            this.connections
                                .update(cx, |view, cx| view.resume_live_display(cx));
                        }
                        if this.replace_page_data(token, data, cx) {
                            this.notice =
                                Some(zenclash_i18n::text("connections.notices.closed_all"));
                        }
                    }
                    Err(error) => this.set_page_error(token, error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_connection(&mut self, id: String, cx: &mut Context<Self>) {
        if self.core_busy()
            || !self
                .connections
                .update(cx, |view, cx| view.begin_close(id.clone(), cx))
        {
            return;
        }
        self.invalidate_page_load();
        self.error = None;
        let token = self.page_task_token_for(Page::Connections);
        let client = self.client.clone();
        let id_for_task = id.clone();
        let task = self.runtime.spawn(async move {
            client
                .close_connection(&id_for_task)
                .await
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(zenclash_i18n::text_with(
                    "connections.errors.close_task",
                    &[("error", error.to_string())],
                )),
            };
            let _ = this.update(cx, |this, cx| {
                this.connections
                    .update(cx, |view, cx| view.finish_connection_close(&id, cx));
                match result {
                    Ok(()) if this.is_page_task_current(token) => {
                        this.connections
                            .update(cx, |view, cx| view.resume_live_display(cx));
                        this.refresh(cx);
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
