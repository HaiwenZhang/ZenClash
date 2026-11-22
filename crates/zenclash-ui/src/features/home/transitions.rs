use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CaptureTransition {
    pub(super) generation: u64,
    pub(super) displayed: CapturePlan,
    pub(super) pending: bool,
    pub(super) confirmed: Option<CaptureStatus>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CapturePresentation {
    pub(super) system_proxy_enabled: bool,
    pub(super) tun_enabled: bool,
    pub(super) pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ModeTransition {
    pub(super) displayed: OutboundMode,
    pub(super) pending: bool,
}

impl RuntimePage {
    pub(super) fn apply_home_capture_plan(
        &mut self,
        plan: CapturePlan,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.page, Page::Home | Page::Settings)
            || self
                .home
                .capture_transition
                .as_ref()
                .is_some_and(|transition| transition.pending)
        {
            return;
        }
        if plan == CapturePlan::Tun
            && !(self.core_session.runtime_descriptor().backend()
                == zenclash_core::CoreRuntimeBackend::Local
                && zenclash_core::current_process_elevated())
        {
            self.request_service_tun(window, cx);
            return;
        }
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let previous_transition = self.home.capture_transition.take();
        let confirmed = previous_transition
            .as_ref()
            .and_then(|transition| transition.confirmed.clone());
        self.home.capture_transition = Some(CaptureTransition {
            generation: self.core_session.generation(),
            displayed: plan,
            pending: true,
            confirmed,
        });
        self.home.action_error = None;
        let capture = self.monitors.traffic_capture.clone();
        let preferences_store = self.preferences_store.clone();
        let task = self.runtime.spawn(async move {
            let outcome = capture
                .apply(plan)
                .await
                .map_err(|error| error.to_string())?;
            let preferences = if let Some(store) = preferences_store {
                tokio::task::spawn_blocking(move || store.load())
                    .await
                    .map_err(|error| error.to_string())?
                    .map(Some)
                    .map_err(|error| error.to_string())
            } else {
                Ok(None)
            };
            Ok::<_, String>((outcome, preferences))
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                match result {
                    Ok((CaptureOutcome::RolledBack { failure, .. }, _))
                    | Ok((CaptureOutcome::ReconcileNeeded { failure, .. }, _)) => {
                        this.home.capture_transition = previous_transition;
                        this.home.action_error = Some(failure);
                    }
                    Ok((outcome, preferences)) => {
                        match preferences {
                            Ok(Some(preferences)) => {
                                this.accept_preferences(
                                    preferences,
                                    crate::runtime_host::services::PreferenceScope::SystemProxy,
                                    cx,
                                );
                            }
                            Ok(None) => {}
                            Err(error) => this.home.action_error = Some(error),
                        }
                        this.accept_home_capture_snapshot(plan, outcome.snapshot());
                        if this.is_page_task_current(token) {
                            this.refresh(cx);
                        }
                    }
                    Err(error) => {
                        this.home.capture_transition = previous_transition;
                        this.home.action_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn reconcile_home_capture_transition(&mut self, capture: &CaptureStatus) {
        let Some(transition) = self.home.capture_transition.as_ref() else {
            return;
        };
        let fresh = capture.system_proxy.is_fresh() && capture.tun.is_fresh();
        let caught_up = fresh
            && match transition.confirmed.as_ref() {
                Some(confirmed) => {
                    capture.system_proxy.observed_at_ms() >= confirmed.system_proxy.observed_at_ms()
                        && capture.tun.observed_at_ms() >= confirmed.tun.observed_at_ms()
                }
                None => observed_capture_plan(capture) == Some(transition.displayed),
            };
        if !transition.pending && caught_up {
            self.home.capture_transition = None;
        }
    }

    pub(in crate::features) fn accept_home_capture_snapshot(
        &mut self,
        displayed: CapturePlan,
        snapshot: &zenclash_core::TrafficCaptureSnapshot,
    ) {
        self.home.capture_transition = Some(CaptureTransition {
            generation: self.core_session.generation(),
            displayed,
            pending: false,
            confirmed: Some(CaptureStatus {
                system_proxy: snapshot.system_proxy.clone(),
                tun: snapshot.tun.clone(),
            }),
        });
    }

    pub(crate) fn begin_home_mode_transition(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        self.home.mode_transition = Some(ModeTransition { displayed, pending });
        cx.notify();
    }

    pub(crate) fn update_home_mode_transition_if_active(
        &mut self,
        displayed: OutboundMode,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.home.mode_transition else {
            return;
        };
        let confirmed = self
            .config()
            .map(|config| OutboundMode::from_api(&config.mode));
        let next = if !pending && confirmed == Some(displayed) {
            None
        } else {
            Some(ModeTransition { displayed, pending })
        };
        if next != Some(current) {
            self.home.mode_transition = next;
            cx.notify();
        }
    }
}

pub(super) fn mode_presentation(
    confirmed: OutboundMode,
    transition: Option<ModeTransition>,
) -> ModeTransition {
    transition.unwrap_or(ModeTransition {
        displayed: confirmed,
        pending: false,
    })
}

pub(super) fn capture_presentation(
    confirmed_system_proxy: bool,
    confirmed_tun: bool,
    transition: Option<&CaptureTransition>,
) -> CapturePresentation {
    if transition.is_some_and(|transition| !transition.pending && transition.confirmed.is_some()) {
        return CapturePresentation {
            system_proxy_enabled: confirmed_system_proxy,
            tun_enabled: confirmed_tun,
            pending: false,
        };
    }
    let Some(transition) = transition else {
        return CapturePresentation {
            system_proxy_enabled: confirmed_system_proxy,
            tun_enabled: confirmed_tun,
            pending: false,
        };
    };
    let (system_proxy_enabled, tun_enabled) = match transition.displayed {
        CapturePlan::Off => (false, false),
        CapturePlan::SystemProxy => (true, false),
        CapturePlan::Tun => (false, true),
    };
    CapturePresentation {
        system_proxy_enabled,
        tun_enabled,
        pending: transition.pending,
    }
}

pub(super) fn capture_transition_status_text(
    confirmed: bool,
    displayed: bool,
    pending: bool,
    enabling_key: &'static str,
    disabling_key: &'static str,
) -> Option<String> {
    (pending && confirmed != displayed).then(|| {
        zenclash_i18n::text(if displayed {
            enabling_key
        } else {
            disabling_key
        })
    })
}

fn observed_capture_plan(capture: &CaptureStatus) -> Option<CapturePlan> {
    let system_proxy_enabled = capture.system_proxy.value()?.intent_enabled;
    let tun = capture.tun.value()?;
    let tun_enabled = tun.requested || tun.configured;
    match (system_proxy_enabled, tun_enabled) {
        (false, false) => Some(CapturePlan::Off),
        (true, false) => Some(CapturePlan::SystemProxy),
        (false, true) => Some(CapturePlan::Tun),
        (true, true) => None,
    }
}
