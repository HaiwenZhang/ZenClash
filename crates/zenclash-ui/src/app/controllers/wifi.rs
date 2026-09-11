use super::{ControllersPage, store_error};
use gpui::Context;
use zenclash_core::SsidRules;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(in crate::app) use macos::{WifiMonitor, read_ssid};

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub(in crate::app) struct WifiMonitor;
#[cfg(not(target_os = "macos"))]
impl WifiMonitor {
    pub(in crate::app) fn start(&mut self, _: bool) {}
    pub(in crate::app) fn stop(&mut self) {}
    pub(in crate::app) fn update_services(&mut self, _: bool) {}
    pub(in crate::app) fn request_access(&mut self, _: &mut gpui::App) {}
    pub(in crate::app) fn status_key(&self) -> &'static str {
        "ssid.unsupported"
    }
    pub(in crate::app) fn observation(&self) -> Option<u64> {
        None
    }
}
#[cfg(not(target_os = "macos"))]
pub(in crate::app) fn read_ssid() -> (Option<String>, bool) {
    (None, false)
}

impl ControllersPage {
    pub(super) fn set_wifi_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(mut rules) = self.ssid_rules.clone() else {
            return;
        };
        rules.enabled = enabled;
        self.persist_wifi(rules, enabled, cx);
    }

    pub(super) fn save_wifi_rule(&mut self, cx: &mut Context<Self>) {
        let Some(mut rules) = self.ssid_rules.clone() else {
            return;
        };
        let Some(profile) = self.ssid_profile.clone() else {
            self.error = Some(zenclash_i18n::text("ssid.choose_profile"));
            cx.notify();
            return;
        };
        rules
            .profiles
            .insert(self.ssid.read(cx).value().to_string(), profile);
        self.persist_wifi(rules, false, cx);
    }

    pub(super) fn remove_wifi_rule(&mut self, ssid: &str, cx: &mut Context<Self>) {
        let Some(mut rules) = self.ssid_rules.clone() else {
            return;
        };
        rules.profiles.remove(ssid);
        self.persist_wifi(rules, false, cx);
    }

    fn persist_wifi(&mut self, rules: SsidRules, request_permission: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        self.busy = true;
        let task = self.runtime.spawn_blocking(move || {
            store.save_ssid(&rules).map_err(store_error)?;
            Ok::<_, String>(rules)
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.closed {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(rules) => {
                        if rules.enabled {
                            this.wifi.start(request_permission);
                        } else {
                            this.wifi.stop();
                        }
                        this.ssid_rules = Some(rules);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
