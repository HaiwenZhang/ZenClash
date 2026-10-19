//! Connects the retained connection view to the page’s shared commands and observation protocol.

use super::super::*;
use super::{ConnectionEvent, ConnectionSnapshotContext};

#[cfg(test)]
mod tests;

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
                    self.close_connections(Some(id), cx);
                } else {
                    self.close_connections(None, cx);
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

    fn close_connections(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.core_busy() || (id.is_none() && self.connections.read(cx).has_pending_closes()) {
            return;
        }
        let Some(source) = self
            .page_observation
            .source()
            .clone()
            .filter(|source| source.belongs_to(&self.core_session))
        else {
            self.refresh(cx);
            return;
        };
        let token = if let Some(id) = &id {
            if !self
                .connections
                .update(cx, |view, cx| view.begin_close(id.clone(), cx))
            {
                return;
            }
            self.invalidate_page_load();
            self.error = None;
            self.page_task_token_for(Page::Connections)
        } else {
            let Some(token) = self.begin_mutation(Page::Connections) else {
                return;
            };
            token
        };
        let query_revision = self.page_observation.query_revision();
        let session = self.core_session.clone();
        let submitted_source = source.clone();
        let submitted_id = id.clone();
        let close_all = id.is_none();
        let task = self.runtime.spawn(async move {
            let observation = session
                .close_observed_connections(&submitted_source, submitted_id)
                .await
                .map_err(|error| error.to_string())?;
            if close_all {
                super::super::page_observation::ObservedPage::load_from_observation(
                    session,
                    observation,
                    Page::Connections,
                )
                .await
                .map(Some)
            } else {
                Ok(None)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| {
                    zenclash_i18n::text_with(
                        "connections.errors.close_task",
                        &[("error", error.to_string())],
                    )
                })
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                if let Some(id) = &id {
                    this.connections
                        .update(cx, |view, cx| view.finish_connection_close(id, cx));
                }
                if !this.is_page_task_current(token)
                    || this.page_observation.query_revision() != query_revision
                    || !this
                        .page_observation
                        .source()
                        .as_ref()
                        .is_some_and(|current| current.is_same_observation(&source))
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Some(observed)) => {
                        if this.accept_page_observation(observed, cx) {
                            this.connections
                                .update(cx, |view, cx| view.resume_live_display(cx));
                            source.publish_if_current(|| {
                                this.notice =
                                    Some(zenclash_i18n::text("connections.notices.closed_all"));
                            });
                        }
                    }
                    Ok(None) if source.is_current() => {
                        this.connections
                            .update(cx, |view, cx| view.resume_live_display(cx));
                        this.refresh(cx);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        source.publish_if_current(|| this.set_page_error(token, error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
