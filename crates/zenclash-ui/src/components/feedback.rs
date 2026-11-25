use gpui_kit::component::{WindowExt, notification::Notification};
use gpui_kit::{Anchor, App, Window};

/// Semantic severity level for a workspace toast notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackKind {
    /// Informational notice.
    Info,
    /// Completed action confirmation.
    Success,
    /// Non-fatal caution or degraded state.
    Warning,
    /// Failure or rejected action.
    Error,
}

/// One active feedback entry associated with a stable source key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Feedback {
    source: &'static str,
    kind: FeedbackKind,
    message: String,
}

impl Feedback {
    /// Constructs a feedback entry for `source` with the given severity and message.
    #[must_use]
    pub fn new(source: &'static str, kind: FeedbackKind, message: String) -> Self {
        Self {
            source,
            kind,
            message,
        }
    }
}

/// Tracks current feedback episodes; a resolved message can be reported again.
#[derive(Default)]
pub struct FeedbackNotifications {
    active: Vec<Feedback>,
}

impl FeedbackNotifications {
    /// Returns the currently active message for `source`, if any.
    #[must_use]
    pub fn current_message(&self, source: &'static str) -> Option<String> {
        self.active
            .iter()
            .find(|feedback| feedback.source == source)
            .map(|feedback| feedback.message.clone())
    }
    fn changed(&mut self, current: Vec<Feedback>) -> Vec<Feedback> {
        let mut changed = Vec::new();
        for feedback in &current {
            if !self.active.contains(feedback)
                && !changed.iter().any(|previous: &Feedback| {
                    previous.kind == feedback.kind && previous.message == feedback.message
                })
            {
                changed.push(feedback.clone());
            }
        }
        self.active = current;
        changed
    }

    /// Publishes newly introduced or changed feedback entries as deduplicated notifications.
    pub fn publish(&mut self, current: Vec<Feedback>, window: &mut Window, cx: &mut App) {
        for feedback in self.changed(current) {
            let notification = match feedback.kind {
                FeedbackKind::Info => Notification::info(feedback.message),
                FeedbackKind::Success => Notification::success(feedback.message),
                FeedbackKind::Warning => Notification::warning(feedback.message),
                FeedbackKind::Error => Notification::error(feedback.message),
            };
            window.push_notification(
                notification
                    .id1::<Self>(feedback.source)
                    .placement(Anchor::BottomRight),
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_updates_do_not_repeat_feedback_but_recovery_allows_a_new_episode() {
        let mut notifications = FeedbackNotifications::default();
        let failure = Feedback::new("request", FeedbackKind::Error, "Request failed".into());
        assert_eq!(notifications.changed(vec![failure.clone()]).len(), 1);
        assert!(notifications.changed(vec![failure.clone()]).is_empty());
        assert!(notifications.changed(Vec::new()).is_empty());
        assert_eq!(notifications.changed(vec![failure]).len(), 1);
    }

    #[test]
    fn changed_messages_are_reported_and_duplicate_sources_share_one_toast() {
        let mut notifications = FeedbackNotifications::default();
        let failure = Feedback::new("request", FeedbackKind::Error, "Failed".into());
        let duplicate = Feedback::new("startup", FeedbackKind::Error, "Failed".into());
        assert_eq!(notifications.changed(vec![failure, duplicate]).len(), 1);
        assert_eq!(
            notifications
                .changed(vec![Feedback::new(
                    "request",
                    FeedbackKind::Error,
                    "Retry failed".into()
                )])
                .len(),
            1
        );
    }
}
