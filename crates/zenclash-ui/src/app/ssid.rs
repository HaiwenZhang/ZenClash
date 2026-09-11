use super::{ZenClashApp, controllers, system_proxy::QuitState};
use gpui::Context;
use std::time::Duration;
use zenclash_core::SsidSwitchState;

impl ZenClashApp {
    pub(super) fn start_ssid_switching(&self, cx: &mut Context<Self>) {
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let mut policy = SsidSwitchState::default();
            let mut observation = None;
            let mut cached_ssid = None;
            let mut ticks = 0u8;
            loop {
                let Ok(snapshot) = this.update(cx, |this, cx| {
                    if this.quit_state != QuitState::Idle {
                        return None;
                    }
                    let controllers = this.controllers_page.read(cx);
                    Some((
                        controllers.wifi.observation(),
                        controllers.ssid_rules.clone().unwrap_or_default(),
                    ))
                }) else {
                    return;
                };
                let Some((revision, rules)) = snapshot else {
                    return;
                };
                ticks = ticks.saturating_add(1);
                if revision.is_none() || !rules.enabled {
                    cached_ssid = None;
                    observation = None;
                } else if observation != revision || ticks >= 30 {
                    // Native events invalidate the cache; a slow fallback recovers missed events.
                    let (ssid, services_enabled) = runtime
                        .spawn_blocking(controllers::read_ssid)
                        .await
                        .unwrap_or((None, false));
                    cached_ssid = ssid;
                    let _ = this.update(cx, |this, cx| {
                        this.controllers_page.update(cx, |page, cx| {
                            page.wifi.update_services(services_enabled);
                            cx.notify();
                        });
                    });
                    observation = revision;
                    ticks = 0;
                }
                let _ = this.update(cx, |this, cx| {
                    let controllers = this.controllers_page.read(cx);
                    if this.quit_state != QuitState::Idle
                        || this.profile_selection_commands.is_running()
                        || !controllers.automation_ready()
                        || controllers.ssid_rules.as_ref() != Some(&rules)
                        || controllers.wifi.observation() != revision
                    {
                        // Defer without resetting a completed association while an editor or
                        // probe is busy; its completion must not override a manual profile.
                        return;
                    }
                    let core = this.core_session.snapshot();
                    let local = !controllers.is_remote() && core.managed && core.running;
                    // Profile IDs are stable file stems in the managed profile store.
                    let current = this
                        .profile_path
                        .as_ref()
                        .and_then(|path| path.file_stem())
                        .and_then(|stem| stem.to_str());
                    if let Some(id) = policy.observe(cached_ssid.as_deref(), &rules, local, current)
                    {
                        this.handle_tray_command(
                            crate::components::tray::TrayCommand::SelectProfile { id },
                            cx,
                        );
                    }
                });
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .detach();
    }
}
