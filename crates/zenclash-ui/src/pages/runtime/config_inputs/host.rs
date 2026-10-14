//! Bridges form reads to the host's original window, Core command identity and page cache.

use crate::pages::runtime::{Context, RuntimePage, config_input_snapshot};
use gpui_kit::AppContext;

impl RuntimePage {
    pub(in crate::pages::runtime) fn refresh_config_inputs(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.config_inputs.begin_read(self.profile_path.as_deref()) else {
            return;
        };
        let profile = token.profile.clone();
        let runtime_version = self.core_session.generation();
        let controlled = self.controlled_config_store.clone();
        let overrides = self.enabled_override_paths();
        let task = self.runtime.spawn_blocking(move || {
            let started = std::time::Instant::now();
            let result = controlled
                .effective_json_with_overrides(profile, &overrides)
                .map(config_input_snapshot)
                .map_err(|error| error.to_string());
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "loaded configuration form snapshot"
            );
            result
        });
        let window_handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if !this
                        .config_inputs
                        .finish_read(&token, this.profile_path.as_deref())
                    {
                        this.refresh_config_inputs(cx);
                        return;
                    }
                    if !this.application_commands.is_current(runtime_version) {
                        this.refresh_config_inputs(cx);
                        return;
                    }
                    match result {
                        Ok(config) => {
                            this.config_inputs.accept_read(
                                config,
                                token,
                                this.profile_path.as_deref(),
                                window,
                                cx,
                            );
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    pub(in crate::pages::runtime) fn invalidate_config_inputs(&mut self, cx: &mut Context<Self>) {
        self.page_observation.clear_cache();
        self.config_inputs.invalidate_read();
        self.refresh_config_inputs(cx);
    }
}
