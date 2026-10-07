use gpui_kit::component::{WindowExt, notification::Notification};
use gpui_kit::{Anchor, App, Window};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FeedbackKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Feedback {
    source: &'static str,
    kind: FeedbackKind,
    message: String,
}

impl Feedback {
    pub(crate) fn new(source: &'static str, kind: FeedbackKind, message: String) -> Self {
        Self {
            source,
            kind,
            message,
        }
    }
}

/// Tracks current feedback episodes; a resolved message can be reported again.
#[derive(Default)]
pub(crate) struct FeedbackNotifications {
    active: Vec<Feedback>,
}

impl FeedbackNotifications {
    pub(crate) fn current_message(&self, source: &'static str) -> Option<String> {
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

    pub(crate) fn publish(&mut self, current: Vec<Feedback>, window: &mut Window, cx: &mut App) {
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
