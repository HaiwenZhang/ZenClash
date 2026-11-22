use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::Context;
use zenclash_app::automatic::{
    EdgeStableObservation, PendingSourceRevision, SourceRevisionKey, SourceScanner,
    StableObservation,
};
use zenclash_core::NetworkReachability;

use crate::shell::app::ZenClashApp;
use crate::shell::controllers::system_proxy::QuitState;

impl ZenClashApp {
    pub(in crate::shell) fn start_service_handoff(&mut self, cx: &mut Context<Self>) {
        if !cfg!(windows)
            || !self.services.core_session.is_managed()
            || self.services.core_session.runtime_descriptor().backend()
                != zenclash_core::CoreRuntimeBackend::Local
        {
            return;
        }
        let core = self.services.core_session.clone();
        let capture = self.services.traffic_capture.clone();
        let commands = self.services.application_commands.clone();
        let runtime = self.services.runtime.clone();
        let descriptor = core.runtime_descriptor();
        let Some(home) = descriptor.home_dir().map(PathBuf::from) else {
            return;
        };
        let binary = descriptor.binary().map(PathBuf::from);
        let generation = core.generation();
        let revision = capture.intent_revision();
        let task = cx.spawn(async move |this, cx| {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                if core.is_shutting_down()
                    || core.generation() != generation
                    || capture.intent_revision() != revision
                    || core.runtime_descriptor().backend()
                        != zenclash_core::CoreRuntimeBackend::Local
                {
                    return;
                }
                if runtime
                    .spawn(zenclash_core::startup_service_health_for(
                        home.clone(),
                        binary.clone(),
                    ))
                    .await
                    .ok()
                    == Some(zenclash_core::ServiceHealthKind::Ready)
                {
                    let Ok(request) = commands.request_service_tun() else {
                        return;
                    };
                    // No authorization consent: a ready service can be used, but a
                    // concurrent stop cannot turn an automatic handoff into a prompt.
                    let result = runtime
                        .spawn(async move { commands.enable_service_tun(request).await })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(error) = result
                            .map_err(|error| error.to_string())
                            .and_then(|result| result)
                        {
                            this.runtime_page.update(cx, |page, cx| {
                                page.report_system_proxy_reconcile_error(&error, cx)
                            });
                        }
                        this.refresh_tray_menu(cx);
                        cx.notify();
                    });
                    return;
                }
                runtime
                    .spawn(async { tokio::time::sleep(Duration::from_secs(2)).await })
                    .await
                    .ok();
            }
        });
        self.shell.tasks.push(task);
    }

    pub(in crate::shell) fn start_automatic_runtime(&mut self, cx: &mut Context<Self>) {
        if !self.services.core_session.is_managed() {
            return;
        }
        let runtime = self.services.runtime.clone();
        let core = self.services.core_session.clone();
        let capture = self.services.traffic_capture.clone();
        let controlled = self.services.controlled_config_store.clone();
        let scanner = std::sync::Arc::new(parking_lot::Mutex::new(SourceScanner::new(
            self.services.override_store.clone(),
        )));
        let task = cx.spawn(async move |this, cx| {
            let mut network = EdgeStableObservation::default();
            let mut source = StableObservation::<Option<SourceRevisionKey>>::default();
            let mut rejected_source = None;
            let mut last_error = None;
            loop {
                if core.is_shutting_down() {
                    return;
                }
                let Ok((quitting, profile)) = this.update(cx, |this, _| {
                    (
                        this.shell.quit_state == QuitState::InProgress,
                        this.services.profile_path.clone(),
                    )
                }) else {
                    return;
                };
                if quitting {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                let kind = core.kind();
                let generation = core.generation();
                let store = controlled.clone();
                let inspected_profile = profile.clone();
                let scanner = scanner.clone();
                let scan = runtime
                    .spawn_blocking(move || {
                        let network = NetworkReachability::detect();
                        let configuration = scanner.lock().scan(&store, kind, inspected_profile);
                        (network, configuration)
                    })
                    .await;
                let mut notice = None;
                let mut error = None;
                match scan {
                    Ok((reachability, configuration)) => {
                        let now = Instant::now();
                        if network.observe(reachability, now)
                            && reachability != NetworkReachability::Unknown
                        {
                            let task_core = core.clone();
                            let task_capture = capture.clone();
                            let action = runtime
                                .spawn(async move {
                                    match reachability {
                                        NetworkReachability::Offline => Some((
                                            task_core.suspend_for_network(&task_capture).await,
                                            "automatic.network_stopped",
                                        )),
                                        NetworkReachability::Online => Some((
                                            task_core.resume_after_network(&task_capture).await,
                                            "automatic.network_resumed",
                                        )),
                                        NetworkReachability::Unknown => None,
                                    }
                                })
                                .await;
                            match action {
                                Ok(Some((Ok(true), key))) => {
                                    network.mark_committed(reachability);
                                    notice = Some(key);
                                }
                                Ok(Some((Ok(false), _))) => {
                                    network.mark_committed(reachability);
                                }
                                Ok(Some((Err(failure), _))) => {
                                    network.record_failure(Instant::now());
                                    error = Some(failure.to_string());
                                }
                                Ok(None) => {}
                                Err(failure) => {
                                    network.record_failure(Instant::now());
                                    error = Some(failure.to_string());
                                }
                            }
                        }
                        match configuration {
                            Ok(Some(PendingSourceRevision {
                                source_profile,
                                override_paths,
                                digest,
                            })) => {
                                let identity = SourceRevisionKey {
                                    source_profile: source_profile.clone(),
                                    digest,
                                };
                                if source.observe(Some(identity.clone()))
                                    && rejected_source.as_ref() != Some(&identity)
                                    && reachability != NetworkReachability::Offline
                                {
                                    let task_core = core.clone();
                                    let task_capture = capture.clone();
                                    let task_store = controlled.clone();
                                    let applied = runtime
                                        .spawn(async move {
                                            let result = task_core
                                                .restart_changed_source(
                                                    &task_store,
                                                    source_profile,
                                                    override_paths,
                                                    generation,
                                                    digest,
                                                )
                                                .await
                                                .map_err(|error| error.to_string());
                                            if result.as_ref().is_ok_and(|changed| *changed)
                                                || result.is_err()
                                            {
                                                let capture = task_capture
                                                    .reconcile()
                                                    .await
                                                    .map_err(|error| error.to_string())?;
                                                if let zenclash_core::CaptureOutcome::ReconcileNeeded {
                                                    failure,
                                                    ..
                                                } = capture
                                                {
                                                    return Err(failure);
                                                }
                                            }
                                            result
                                        })
                                        .await
                                        .map_err(|error| error.to_string())
                                        .and_then(|result| result);
                                    match applied {
                                        Ok(true) => notice = Some("automatic.config_restarted"),
                                        Ok(false) => {}
                                        Err(failure) => {
                                            rejected_source = Some(identity);
                                            error = Some(failure);
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                source.observe(None);
                                rejected_source = None;
                            }
                            Err(failure) => {
                                source.observe(None);
                                error = Some(failure);
                            }
                        }
                    }
                    Err(failure) => error = Some(failure.to_string()),
                }
                let reported_error = error
                    .clone()
                    .filter(|error| last_error.as_ref() != Some(error));
                last_error = error;
                if notice.is_some() || reported_error.is_some() {
                    let _ = this.update(cx, |this, cx| {
                        if this.shell.quit_state == QuitState::InProgress || core.is_shutting_down()
                        {
                            return;
                        }
                        this.runtime_page.update(cx, |page, cx| {
                            page.report_automatic_runtime(notice, reported_error, cx);
                        });
                        if notice.is_some() {
                            if this.current_page == crate::foundation::actions::Page::Proxies
                                && this.shell.main_window_visible
                            {
                                this.proxies_page
                                    .update(cx, |page, cx| page.profile_activated(cx));
                            } else {
                                this.proxies_page
                                    .update(cx, |page, _| page.profile_invalidated());
                            }
                        }
                        this.refresh_tray_menu(cx);
                    });
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        self.shell.tasks.push(task);
    }
}
