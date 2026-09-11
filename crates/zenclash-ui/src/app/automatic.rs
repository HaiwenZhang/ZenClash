use std::{path::PathBuf, time::Duration};

use gpui::Context;
use zenclash_core::{NetworkReachability, YamlOverrideStore};

use super::{ZenClashApp, system_proxy::QuitState};

#[derive(Default)]
struct StableObservation<T> {
    candidate: Option<T>,
}

impl<T: PartialEq> StableObservation<T> {
    fn observe(&mut self, value: T) -> bool {
        let stable = self.candidate.as_ref() == Some(&value);
        self.candidate = Some(value);
        stable
    }
}

type SourceChange = Option<(PathBuf, Vec<PathBuf>, [u8; 32])>;
type SourceStamp = (PathBuf, Option<(u64, std::time::SystemTime)>);

#[derive(Default)]
struct SourceScanner {
    stamps: Vec<SourceStamp>,
    cached: Option<Result<SourceChange, String>>,
}

impl SourceScanner {
    fn scan(
        &mut self,
        store: &zenclash_core::ControlledConfigStore,
        kind: zenclash_core::CoreKind,
        profile: Option<PathBuf>,
    ) -> Result<SourceChange, String> {
        let Some(profile) = profile else {
            self.cached = None;
            return Ok(None);
        };
        let overrides = YamlOverrideStore::discover()
            .and_then(|store| store.load_enabled_paths())
            .map_err(|error| error.to_string())?;
        let paths = [
            profile.clone(),
            store.runtime_path(),
            store.root().join("override.yaml"),
        ]
        .into_iter()
        .chain(overrides.iter().cloned());
        let stamps = paths
            .map(|path| {
                let metadata = std::fs::metadata(&path).ok().and_then(|metadata| {
                    metadata
                        .modified()
                        .ok()
                        .map(|modified| (metadata.len(), modified))
                });
                (path, metadata)
            })
            .collect::<Vec<_>>();
        if self.stamps == stamps
            && let Some(result) = &self.cached
        {
            return result.clone();
        }
        let result = store
            .pending_source_revision(kind, &profile, &overrides)
            .map(|revision| revision.map(|revision| (profile, overrides, revision)))
            .map_err(|error| error.to_string());
        self.stamps = stamps;
        self.cached = Some(result.clone());
        result
    }
}

impl ZenClashApp {
    pub(super) fn start_automatic_runtime(&self, cx: &mut Context<Self>) {
        if !self.core_session.snapshot().managed {
            return;
        }
        let runtime = self.runtime.clone();
        let core = self.core_session.clone();
        let capture = self.traffic_capture.clone();
        let controlled = self.controlled_config_store.clone();
        let scanner = std::sync::Arc::new(parking_lot::Mutex::new(SourceScanner::default()));
        cx.spawn(async move |this, cx| {
            let mut network = StableObservation { candidate: None };
            let mut source = StableObservation::<Option<(PathBuf, [u8; 32])>>::default();
            let mut rejected_source = None;
            let mut last_error = None;
            loop {
                let Ok((quitting, profile)) = this.update(cx, |this, _| (this.quit_state != QuitState::Idle, this.profile_path.clone())) else { return; };
                if quitting { return; }
                let session = core.snapshot();
                let store = controlled.clone();
                let inspected_profile = profile.clone();
                let scanner = scanner.clone();
                let scan = runtime.spawn_blocking(move || {
                    let network = NetworkReachability::detect();
                    let configuration = scanner.lock().scan(&store, session.kind, inspected_profile);
                    (network, configuration)
                }).await;
                let mut notice = None;
                let mut error = None;
                match scan {
                    Ok((reachability, configuration)) => {
                        if network.observe(reachability) {
                            let task_core = core.clone();
                            let task_capture = capture.clone();
                            let action = runtime.spawn(async move {
                                match reachability {
                                    NetworkReachability::Offline => Some((task_core.suspend_for_network(&task_capture).await, "automatic.network_stopped")),
                                    NetworkReachability::Online => Some((task_core.resume_after_network(&task_capture).await, "automatic.network_resumed")),
                                    NetworkReachability::Unknown => None,
                                }
                            }).await;
                            let action = match action {
                                Ok(action) => action,
                                Err(failure) => { error = Some(failure.to_string()); None }
                            };
                            if let Some((result, key)) = action {
                                match result {
                                    Ok(true) => notice = Some(key),
                                    Ok(false) => {},
                                    Err(failure) => error = Some(failure.to_string()),
                                }
                            }
                        }
                        match configuration {
                            Ok(Some((profile, overrides, revision))) => {
                                let identity = (profile.clone(), revision);
                                if source.observe(Some(identity.clone())) && rejected_source.as_ref() != Some(&identity) && reachability != NetworkReachability::Offline {
                                    let task_core = core.clone();
                                    let task_capture = capture.clone();
                                    let task_store = controlled.clone();
                                    let applied = runtime.spawn(async move {
                                        let result = task_core.restart_changed_source(&task_store, profile, overrides, session.generation, revision).await.map_err(|error| error.to_string());
                                        if result.as_ref().is_ok_and(|changed| *changed) || result.is_err() {
                                            let capture = task_capture.reconcile().await.map_err(|error| error.to_string())?;
                                            if let zenclash_core::CaptureOutcome::ReconcileNeeded { failure, .. } = capture { return Err(failure); }
                                        }
                                        result
                                    }).await.map_err(|error| error.to_string()).and_then(|result| result);
                                    match applied {
                                        Ok(true) => notice = Some("automatic.config_restarted"),
                                        Ok(false) => {},
                                        Err(failure) => {
                                            rejected_source = Some(identity);
                                            error = Some(failure);
                                        }
                                    }
                                }
                            }
                            Ok(None) => { source.observe(None); rejected_source = None; }
                            Err(failure) => { source.observe(None); error = Some(failure); }
                        }
                    }
                    Err(failure) => error = Some(failure.to_string()),
                }
                let reported_error = error.clone().filter(|error| last_error.as_ref() != Some(error));
                last_error = error;
                if notice.is_some() || reported_error.is_some() {
                    let _ = this.update(cx, |this, cx| {
                        if this.quit_state != QuitState::Idle { return; }
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_automatic_runtime(notice, reported_error, cx);
                        });
                        if notice.is_some() {
                            if this.current_page == crate::pages::Page::Proxies && this.main_window_visible {
                                this.proxies_page.update(cx, |page, cx| page.profile_activated(cx));
                            } else {
                                this.proxies_page.update(cx, |page, _| page.profile_invalidated());
                            }
                        }
                        this.refresh_tray_menu(cx);
                    });
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }).detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstable_network_and_unknown_observations_do_not_confirm_link_loss() {
        let mut state = StableObservation { candidate: None };
        assert!(!state.observe(NetworkReachability::Offline));
        assert!(!state.observe(NetworkReachability::Unknown));
        assert!(!state.observe(NetworkReachability::Offline));
        assert!(state.observe(NetworkReachability::Offline));
    }

    #[test]
    fn repeated_saves_debounce_until_the_latest_revision_is_stable() {
        let mut state = StableObservation::<Option<u8>>::default();
        assert!(!state.observe(Some(1)));
        assert!(!state.observe(Some(2)));
        assert!(state.observe(Some(2)));
        assert!(!state.observe(None));
        assert!(!state.observe(Some(2)));
    }
}
