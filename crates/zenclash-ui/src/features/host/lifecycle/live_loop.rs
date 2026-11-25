//! Live-update loop, operational stream observers, visibility gating, and tab navigation.

use gpui_kit::component::WindowExt;
use gpui_kit::{AppContext, Context};

use crate::features::host::RuntimePage;
use crate::foundation::actions::Page;
use crate::runtime_host::{
    lifecycle_schedule::{LIVE_UPDATE_INTERVAL, LiveUpdateSchedule},
    observation::PageSnapshotData,
};

impl RuntimePage {
    pub(super) fn start_operational_updates(
        mut updates: zenclash_core::OperationalStatusStream,
        activity: tokio::sync::watch::Receiver<bool>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<()> {
        cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                if !*activity.borrow() {
                    continue;
                }
                let capture = updates.borrow().capture.clone();
                if this
                    .update(cx, |this, cx| {
                        this.reconcile_home_generation();
                        this.reconcile_home_capture_transition(&capture);
                        if this.live_updates_enabled()
                            && matches!(this.page, Page::Home | Page::Tun)
                        {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    pub(in crate::features) fn restore_page_focus(&self, page: Page, cx: &mut Context<Self>) {
        let generation = self.navigation_generation;
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |this, cx| {
                if this.page != page
                    || this.navigation_generation != generation
                    || !this.shell.ui_visibility.page_presented
                {
                    return;
                }
                let focus = this.focus_handle.clone();
                let _ = cx.update_window(this.shell.window_handle, |_, window, cx| {
                    focus.focus(window, cx);
                });
            });
        });
    }

    /// Switches the active tab and invalidates results from older page tasks.
    pub fn switch_to(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        let previous_page = self.page;
        let close_subscription =
            previous_page == Page::Profiles && self.profiles.forms.close_subscription();

        let config = matches!(page, Page::Dns | Page::Sniffer)
            .then(|| self.config().cloned())
            .flatten();
        let config_observation = self.page_observation.source().clone();
        self.retain_page_snapshot(cx);
        if self.page == Page::Network {
            self.cancel_network_diagnostics();
        }
        if previous_page == Page::Logs {
            self.logs.update(cx, |view, cx| view.release_results(cx));
        }
        if previous_page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if previous_page == Page::Override {
            self.library.overrides.invalidate_preview();
        }
        self.page = page;
        if !page.descriptor().owns_viewport_scroll() {
            self.shell
                .body_scroll
                .set_offset(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)));
        }
        if previous_page == Page::Home {
            self.release_home_presentation();
        }
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        let generation = self.navigation_generation;
        let page = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = page.update(cx, |page, cx| {
                if page.navigation_generation != generation
                    || !page.shell.ui_visibility.page_presented
                {
                    return;
                }
                let focus = page.focus_handle.clone();
                let _ = cx.update_window(page.shell.window_handle, |_, window, cx| {
                    if close_subscription {
                        window.close_dialog(cx);
                    }
                    focus.focus(window, cx);
                });
            });
        });
        self.connections
            .update(cx, |view, _| view.release_presentation());
        self.rules
            .update(cx, |view, cx| view.release_presentation(cx));
        self.invalidate_page_load();
        let runtime_version = self.core_session.generation();
        self.restore_page_observation();
        self.page_observation
            .reuse_config(config, config_observation, runtime_version);
        self.prepare_home_projection();
        self.update_connection_presentation(cx);
        self.update_rule_presentation(cx);
        self.error = None;
        self.notice = None;
        if self.live_updates_enabled() {
            self.refresh_visible_page(cx);
        }
        cx.notify();
    }

    pub(crate) fn set_presented(&mut self, presented: bool, cx: &mut Context<Self>) {
        if self.shell.ui_visibility.page_presented == presented {
            return;
        }
        if !presented && !self.core_busy() {
            self.release_inactive_page_data(cx);
        }
        let was_enabled = self.shell.ui_visibility.updates_enabled();
        self.shell.ui_visibility.page_presented = presented;
        self.update_live_update_activity(was_enabled, cx);
    }

    pub(crate) fn set_window_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.shell.ui_visibility.window_visible == visible {
            return;
        }
        if !visible && !self.core_busy() {
            self.release_inactive_page_data(cx);
            self.page_observation.clear_cache();
        }
        let was_enabled = self.shell.ui_visibility.updates_enabled();
        self.shell.ui_visibility.window_visible = visible;
        self.update_live_update_activity(was_enabled, cx);
    }

    fn retain_page_snapshot(&mut self, cx: &mut Context<Self>) {
        if !self.core_busy() && self.rules.read(cx).is_snapshot_retainable() {
            self.page_observation.retain_current(self.page);
        } else {
            self.page_observation.release_data();
        }
    }

    fn release_inactive_page_data(&mut self, cx: &mut Context<Self>) {
        self.retain_page_snapshot(cx);
        self.release_home_presentation();
        self.logs.update(cx, |view, cx| view.release_results(cx));
        if self.page == Page::Network {
            self.cancel_network_diagnostics();
        }
        if self.page == Page::Traffic {
            self.traffic_history.release_results();
        }
        if self.page == Page::Override {
            self.library.overrides.invalidate_preview();
        }
        self.invalidate_page_load();
        self.page_observation.release_data();
        self.connections
            .update(cx, |view, _| view.release_presentation());
        self.rules
            .update(cx, |view, cx| view.release_presentation(cx));
    }

    fn update_live_update_activity(&mut self, was_enabled: bool, cx: &mut Context<Self>) {
        let enabled = self.shell.ui_visibility.updates_enabled();
        if !enabled {
            self.release_home_presentation();
            self.connections
                .update(cx, |view, _| view.release_presentation());
            if self.network_diagnostics.query.is_loading() {
                self.cancel_network_diagnostics();
            }
            self.logs
                .update(cx, |view, cx| view.suspend_presentation(cx));
            self.traffic_history.cancel_query();
            self.library.overrides.invalidate_preview();
            self.rules
                .update(cx, |view, cx| view.release_presentation(cx));
        }
        if was_enabled == enabled {
            if self.shell.ui_visibility.window_visible && self.shell.ui_visibility.page_presented {
                self.update_connection_presentation(cx);
            }
            return;
        }
        self.shell.live_updates_enabled.send_replace(enabled);
        if enabled {
            if matches!(self.page_observation.data(), PageSnapshotData::Empty) {
                self.page_observation
                    .restore_for_resume(self.page, &self.core_session);
                self.prepare_home_projection();
            }
            self.update_connection_presentation(cx);
            self.refresh_visible_page(cx);
            self.update_rule_presentation(cx);
            cx.notify();
        }
    }

    pub(crate) const fn live_updates_enabled(&self) -> bool {
        self.shell.ui_visibility.updates_enabled()
    }

    fn refresh_visible_page(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Home {
            // Restore the real stream backlog before waiting for controller refreshes.
            self.update_home_traffic_presentation(cx);
        }
        self.refresh_config_inputs(cx);
        if self.page == Page::Profiles {
            self.reload_profile_catalog(cx);
        }
        self.refresh(cx);
        if self.page == Page::Mihomo {
            self.refresh_core_management(cx);
        }
        if self.page == Page::Traffic {
            self.refresh_traffic_history(cx);
        }
        if self.page == Page::Logs {
            self.update_log_presentation(cx);
        }
    }

    pub(super) fn start_live_updates(
        mut activity: tokio::sync::watch::Receiver<bool>,
        log_revision: u64,
        traffic_revision: u64,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Task<()> {
        let mut schedule = LiveUpdateSchedule::new(log_revision, traffic_revision);
        cx.spawn(async move |this, cx| {
            loop {
                let active = *activity.borrow_and_update();
                if !active {
                    if activity.changed().await.is_err() {
                        break;
                    }
                    if *activity.borrow_and_update()
                        && this
                            .update(cx, |this, _| {
                                schedule.synchronize(
                                    this.monitors.log_monitor.revision(),
                                    this.monitors.traffic_monitor.revision(),
                                );
                            })
                            .is_err()
                    {
                        break;
                    }
                    continue;
                }

                tokio::select! {
                    changed = activity.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        if *activity.borrow_and_update()
                            && this.update(cx, |this, _| {
                                schedule.synchronize(
                                    this.monitors.log_monitor.revision(),
                                    this.monitors.traffic_monitor.revision(),
                                );
                            }).is_err()
                        {
                            break;
                        }
                    }
                    () = cx.background_executor().timer(LIVE_UPDATE_INTERVAL) => {
                        if this.update(cx, |this, cx| {
                            let actions = schedule.tick(
                                this.page,
                                this.monitors.log_monitor.revision(),
                                this.monitors.traffic_monitor.revision(),
                                !this.loading && !this.core_busy(),
                                this.connections.read(cx).is_idle(),
                            );
                            if this.page == Page::Home {
                                this.update_home_traffic_presentation(cx);
                            }
                            if actions.refresh_page {
                                this.refresh(cx);
                            }
                            if actions.refresh_history {
                                this.refresh_traffic_history(cx);
                            }
                            if this.page == Page::Logs {
                                this.update_log_presentation(cx);
                            }
                            if actions.notify && !actions.notify_logs && !actions.refresh_page && !actions.refresh_history {
                                cx.notify();
                            }
                        }).is_err() {
                            break;
                        }
                    }
                }
            }
        })
    }
}
