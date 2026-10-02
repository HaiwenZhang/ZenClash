use gpui_kit::Styled;
use gpui_kit::component::{Disableable, WindowExt, button::Button, h_flex};
use gpui_kit::{Context, ParentElement, Window};
use zenclash_core::{
    CaptureOutcome, CoreRuntimeBackend, ServiceHealthKind, ServiceManagerSnapshot,
    ServiceTunRequest,
};

use super::super::{Page, PageTaskToken, RuntimePage};

impl RuntimePage {
    pub(super) fn render_service_tun_actions(
        &self,
        state: &ServiceManagerSnapshot,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let authorization = self.core_session.runtime_descriptor().backend()
            == CoreRuntimeBackend::Local
            && state
                .health()
                .is_none_or(|health| health.kind() != ServiceHealthKind::Ready);
        let pending = self.profile_service.pending_finalization();
        h_flex()
            .justify_end()
            .gap_2()
            .p_4()
            .children(pending.map(|version| {
                Button::new("confirm-service-tun")
                    .label(zenclash_i18n::text("profiles.recovery.confirm"))
                    .outline()
                    .disabled(self.core_busy() || state.is_busy())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.confirm_service_tun(version, cx)),
                    )
            }))
            .child(
                Button::new("enable-service-tun")
                    .label(zenclash_i18n::text(if authorization {
                        "core_page.service.enable_with_authorization"
                    } else {
                        "core_page.service.enable"
                    }))
                    .outline()
                    .loading(state.is_busy())
                    .disabled(self.core_busy() || state.is_busy() || pending.is_some())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.request_service_tun(window, cx)),
                    ),
            )
    }

    pub(crate) fn request_service_tun(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core_busy()
            || self
                .profile_service
                .service_state()
                .is_some_and(|state| state.is_busy())
        {
            return;
        }
        if self.page == Page::Home {
            self.home.action_error = None;
        }
        match self.profile_service.request_service_tun() {
            Ok(request) => self.present_service_tun_request(request, window, cx),
            Err(error) => {
                self.set_service_tun_error(self.page_task_token_for(self.page), error);
                cx.notify();
            }
        }
    }

    pub(crate) fn present_service_tun_request(
        &mut self,
        request: ServiceTunRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core_busy()
            || self
                .profile_service
                .service_state()
                .is_some_and(|state| state.is_busy())
        {
            self.set_service_tun_error(
                self.page_task_token_for(self.page),
                zenclash_i18n::text("core_page.service.busy"),
            );
            cx.notify();
            return;
        }
        let health = self
            .profile_service
            .service_state()
            .and_then(|state| state.health().map(|health| health.kind()));
        if !request.needs_authorization_consent() || health == Some(ServiceHealthKind::Ready) {
            self.enable_service_tun(request, cx);
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        // Mouse activation deliberately preserves keyboard focus in GPUI Kit.
        // A tray-opened window can have none; provide a valid return target.
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        let stopped = health == Some(ServiceHealthKind::Stopped);
        let page = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let page = page.clone();
            let request = request.clone();
            dialog
                .confirm()
                .title(zenclash_i18n::text(if stopped {
                    "core_page.service.start_title"
                } else {
                    "core_page.service.install_title"
                }))
                .description(zenclash_i18n::text(if stopped {
                    "core_page.service.start_description"
                } else {
                    "core_page.service.install_description"
                }))
                .ok_text(zenclash_i18n::text(if stopped {
                    "core_page.service.start_enable"
                } else {
                    "core_page.service.install_enable"
                }))
                .on_ok(move |_, _, cx| {
                    let _ = page.update(cx, |page, cx| {
                        page.enable_service_tun(request.clone().with_authorization(), cx)
                    });
                    true
                })
        });
    }

    fn enable_service_tun(&mut self, request: ServiceTunRequest, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let profiles = self.profile_service.clone();
        let task = self
            .runtime
            .spawn(async move { profiles.enable_service_tun(request).await });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|_| zenclash_i18n::text("core_page.service.pending"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                this.reload_controlled_config(cx);
                match result {
                    Ok(outcome) => {
                        if outcome
                            .core()
                            .is_some_and(|core| !this.profile_service.is_current(core.generation))
                        {
                            this.refresh(cx);
                            return;
                        }
                        this.refresh(cx);
                        match outcome.capture() {
                            CaptureOutcome::RolledBack { failure, .. }
                            | CaptureOutcome::ReconcileNeeded { failure, .. } => {
                                this.set_service_tun_error(token, failure.clone())
                            }
                            CaptureOutcome::Applied { .. } | CaptureOutcome::Unchanged { .. } => {}
                        }
                        // Native capture observations, rather than installation or a
                        // saved receipt, report whether TUN actually became active.
                    }
                    Err(error) => {
                        let visible_status = this
                            .profile_service
                            .service_state()
                            .map(|state| state.phase());
                        if !matches!(
                            visible_status,
                            Some(
                                zenclash_core::ServicePhase::Cancelled
                                    | zenclash_core::ServicePhase::Unconfirmed
                            )
                        ) {
                            this.set_service_tun_error(token, error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_service_tun_error(&mut self, token: PageTaskToken, error: String) {
        if token.page == Page::Home && self.is_page_task_current(token) {
            self.home.action_error = Some(error);
        } else {
            self.set_page_error(token, error);
        }
    }

    fn confirm_service_tun(&mut self, version: u64, cx: &mut Context<Self>) {
        let Some(token) = self.begin_mutation(Page::Tun) else {
            return;
        };
        let profiles = self.profile_service.clone();
        let task = self
            .runtime
            .spawn(async move { profiles.confirm_service_runtime(version).await });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|_| zenclash_i18n::text("core_page.service.pending"))
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                this.reload_controlled_config(cx);
                this.refresh(cx);
                if let Err(error) = result
                    && this.profile_service.pending_finalization() == Some(version)
                {
                    this.set_page_error(token, error);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
