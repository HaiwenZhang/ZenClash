//! Bounded visibility and cadence scheduling for live runtime-page updates.

use std::time::Duration;

use crate::foundation::actions::Page;

pub(crate) const LIVE_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UiVisibility {
    pub(crate) window_visible: bool,
    pub(crate) page_presented: bool,
}

impl UiVisibility {
    pub(crate) const fn new(window_visible: bool) -> Self {
        Self {
            window_visible,
            page_presented: true,
        }
    }

    pub(crate) const fn page_visible(self) -> bool {
        self.window_visible && self.page_presented
    }

    pub(crate) const fn updates_enabled(self) -> bool {
        // A visible page remains live even when another window has keyboard focus.
        self.page_visible()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LiveUpdateActions {
    pub(crate) notify: bool,
    pub(crate) notify_logs: bool,
    pub(crate) refresh_page: bool,
    pub(crate) refresh_history: bool,
}

#[derive(Debug)]
pub(crate) struct LiveUpdateSchedule {
    history_ticks: u8,
    dashboard_ticks: u8,
    connection_ticks: u8,
    log_revision: u64,
    traffic_revision: u64,
}

impl LiveUpdateSchedule {
    pub(crate) const fn new(log_revision: u64, traffic_revision: u64) -> Self {
        Self {
            history_ticks: 0,
            dashboard_ticks: 0,
            connection_ticks: 0,
            log_revision,
            traffic_revision,
        }
    }

    pub(crate) fn synchronize(&mut self, log_revision: u64, traffic_revision: u64) {
        self.history_ticks = 0;
        self.dashboard_ticks = 0;
        self.connection_ticks = 0;
        self.log_revision = log_revision;
        self.traffic_revision = traffic_revision;
    }

    pub(crate) fn tick(
        &mut self,
        page: Page,
        log_revision: u64,
        traffic_revision: u64,
        page_refresh_ready: bool,
        connections_idle: bool,
    ) -> LiveUpdateActions {
        let mut actions = LiveUpdateActions::default();
        if page == Page::Logs && log_revision != self.log_revision {
            actions.notify = true;
            actions.notify_logs = true;
        }
        self.log_revision = log_revision;

        if matches!(page, Page::Home | Page::Traffic) && traffic_revision != self.traffic_revision {
            actions.notify = true;
        }
        self.traffic_revision = traffic_revision;

        if matches!(page, Page::Connections | Page::Traffic) {
            self.connection_ticks = self.connection_ticks.saturating_add(1);
            if self.connection_ticks >= 2 && page_refresh_ready && connections_idle {
                self.connection_ticks = 0;
                actions.refresh_page = true;
            }
        } else {
            self.connection_ticks = 0;
        }

        if page == Page::Traffic {
            self.history_ticks = self.history_ticks.saturating_add(1);
            if self.history_ticks >= 10 {
                self.history_ticks = 0;
                actions.refresh_history = true;
            }
        } else {
            self.history_ticks = 0;
        }

        if page == Page::Home {
            self.dashboard_ticks = self.dashboard_ticks.saturating_add(1);
            if self.dashboard_ticks >= 10 && page_refresh_ready {
                self.dashboard_ticks = 0;
                actions.refresh_page = true;
            }
        } else {
            self.dashboard_ticks = 0;
        }

        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpresented_page_disables_ui_updates() {
        let mut visibility = UiVisibility::new(true);
        visibility.page_presented = false;

        assert!(!visibility.updates_enabled());
    }

    #[test]
    fn hidden_window_disables_ui_updates() {
        let mut visibility = UiVisibility::new(true);
        visibility.window_visible = false;

        assert!(!visibility.updates_enabled());
    }

    #[test]
    fn idle_home_tick_does_not_request_a_repaint() {
        let mut schedule = LiveUpdateSchedule::new(2, 3);

        let actions = schedule.tick(Page::Home, 2, 3, true, true);

        assert_eq!(actions, LiveUpdateActions::default());
    }

    #[test]
    fn new_traffic_requests_a_home_repaint() {
        let mut schedule = LiveUpdateSchedule::new(2, 3);

        let actions = schedule.tick(Page::Home, 2, 4, true, true);

        assert!(actions.notify);
    }

    #[test]
    fn home_dashboard_refresh_keeps_its_five_second_cadence() {
        let mut schedule = LiveUpdateSchedule::new(0, 0);
        for _ in 0..9 {
            let _ = schedule.tick(Page::Home, 0, 0, true, true);
        }

        let actions = schedule.tick(Page::Home, 0, 0, true, true);

        assert!(actions.refresh_page);
    }

    #[test]
    fn synchronization_discards_revisions_accumulated_while_paused() {
        let mut schedule = LiveUpdateSchedule::new(1, 1);
        schedule.synchronize(5, 8);

        let actions = schedule.tick(Page::Logs, 5, 8, true, true);

        assert_eq!(actions, LiveUpdateActions::default());
    }
}
