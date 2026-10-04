use gpui_kit::component::{WindowExt, button::ButtonVariant};
use gpui_kit::{AppContext, Context, Window};
use zenclash_core::{CoreKind, CoreRuntimeBackend, ServiceMaintenanceRequest, ServiceOperation};

use super::super::RuntimePage;

impl RuntimePage {
    pub(super) fn request_service_maintenance(
        &mut self,
        operation: ServiceOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core_busy()
            || self
                .profile_service
                .service_state()
                .is_some_and(|state| state.is_busy())
            || window.has_active_dialog(cx)
        {
            return;
        }
        if self.core_session.runtime_descriptor().backend() == CoreRuntimeBackend::Service
            && self.core_session.local_recovery_launch().is_none()
        {
            self.discover_service_maintenance(operation, window, cx);
            return;
        }
        let request = match self
            .profile_service
            .request_service_maintenance(operation, None)
        {
            Ok(request) => request,
            Err(error) => {
                self.set_page_error(self.page_task_token_for(self.page), error);
                cx.notify();
                return;
            }
        };
        self.present_service_maintenance(request, window, cx);
    }

    fn discover_service_maintenance(
        &mut self,
        operation: ServiceOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        let Some(root) = root else {
            self.set_page_error(
                self.page_task_token_for(self.page),
                zenclash_i18n::text("core_page.service.unsupported"),
            );
            cx.notify();
            return;
        };
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let preferred = self
            .preferences
            .core_binaries
            .path(CoreKind::Mihomo)
            .map(std::path::Path::to_path_buf);
        let profiles = self.profile_service.clone();
        let task = self.runtime.spawn(async move {
            profiles
                .discover_service_maintenance(operation, root, preferred)
                .await
        });
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|_| zenclash_i18n::text("core_page.service.failed"))
                .and_then(|result| result);
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.finish_mutation(token);
                    if !this.is_page_task_current(token) {
                        return;
                    }
                    match result {
                        Ok(request) => {
                            if !window.has_active_dialog(cx) {
                                this.present_service_maintenance(request, window, cx);
                            }
                        }
                        Err(error) => this.set_page_error(token, error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn present_service_maintenance(
        &mut self,
        request: ServiceMaintenanceRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let operation = request.operation();
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle, cx);
        }
        let page = cx.entity().downgrade();
        let (title, description, confirm) = match operation {
            ServiceOperation::Repair => (
                "core_page.service.repair_title",
                "core_page.service.repair_description",
                "core_page.service.repair",
            ),
            ServiceOperation::Uninstall => (
                "core_page.service.uninstall_title",
                "core_page.service.uninstall_description",
                "core_page.service.uninstall",
            ),
            _ => return,
        };
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let page = page.clone();
            let request = request.clone();
            dialog
                .confirm()
                .title(zenclash_i18n::text(title))
                .description(zenclash_i18n::text(description))
                .ok_text(zenclash_i18n::text(confirm))
                .ok_variant(if operation == ServiceOperation::Uninstall {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_ok(move |_, _, cx| {
                    let _ = page.update(cx, |page, cx| {
                        page.run_service_maintenance(request.clone(), cx);
                    });
                    true
                })
        });
    }

    fn run_service_maintenance(
        &mut self,
        request: ServiceMaintenanceRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = self.begin_mutation(self.page) else {
            return;
        };
        let profiles = self.profile_service.clone();
        let store = self.controlled_config_store.clone();
        let preferences_store = self.preferences_store.clone();
        let task = self.runtime.spawn(async move {
            let maintenance = async {
                let preparation = profiles
                    .prepare_service_maintenance(request, &store)
                    .await?;
                profiles.maintain_prepared_service(preparation, true).await
            }
            .await;
            let preferences = if let Some(store) = preferences_store {
                tokio::task::spawn_blocking(move || store.load())
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map(Some).map_err(|error| error.to_string()))
            } else {
                Ok(None)
            };
            (maintenance, preferences)
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|_| zenclash_i18n::text("core_page.service.pending"));
            let _ = this.update(cx, |this, cx| {
                this.finish_mutation(token);
                this.reload_controlled_config(cx);
                this.refresh(cx);
                match result {
                    Ok((maintenance, preferences)) => {
                        let preference_error = match preferences {
                            Ok(Some(preferences)) => {
                                this.accept_preferences(
                                    preferences,
                                    super::super::PreferenceScope::SystemProxy,
                                    cx,
                                );
                                None
                            }
                            Ok(None) => None,
                            Err(error) => Some(error),
                        };
                        if let Err(error) = maintenance {
                            this.set_page_error(token, error);
                        } else if let Some(error) = preference_error {
                            this.set_page_error(token, error);
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
}
