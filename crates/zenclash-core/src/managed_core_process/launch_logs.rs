use std::{collections::VecDeque, sync::Arc};

/// A collector's launch identity; it is not runtime ownership or execution authority.
#[derive(Clone)]
pub(super) struct LaunchLogIdentity(Arc<()>);

struct CurrentLaunchLogs {
    identity: LaunchLogIdentity,
    last_line: Option<Arc<str>>,
    listener_error: Option<Arc<str>>,
}

/// Bounded visible history and diagnostic facts belonging only to the current launch.
#[derive(Default)]
pub(super) struct LaunchLogHistory {
    history: VecDeque<Arc<str>>,
    current: Option<CurrentLaunchLogs>,
}

impl LaunchLogHistory {
    pub(super) fn begin_launch(&mut self) -> LaunchLogIdentity {
        let identity = LaunchLogIdentity(Arc::new(()));
        self.current = Some(CurrentLaunchLogs {
            identity: identity.clone(),
            last_line: None,
            listener_error: None,
        });
        identity
    }

    pub(super) fn record(&mut self, identity: &LaunchLogIdentity, label: &str, line: &str) {
        let text: Arc<str> = format!("[{label}] {line}").into();
        if let Some(current) = &mut self.current
            && Arc::ptr_eq(&current.identity.0, &identity.0)
        {
            current.last_line = Some(text.clone());
            if is_controller_listener_error(line) {
                current.listener_error = Some(text.clone());
            }
        }
        if self.history.len() >= super::MAX_LOG_LINES {
            self.history.pop_front();
        }
        self.history.push_back(text);
    }

    pub(super) fn current_listener_error(&self) -> Option<String> {
        self.current
            .as_ref()?
            .listener_error
            .as_ref()
            .map(ToString::to_string)
    }

    pub(super) fn current_last_line(&self) -> Option<String> {
        self.current
            .as_ref()?
            .last_line
            .as_ref()
            .map(ToString::to_string)
    }

    pub(super) fn recent_messages(&self) -> Vec<String> {
        self.history.iter().map(ToString::to_string).collect()
    }

    #[cfg(test)]
    pub(super) fn from_history(messages: VecDeque<String>) -> Self {
        Self {
            history: messages.into_iter().map(Arc::from).collect(),
            current: None,
        }
    }
}

fn is_controller_listener_error(line: &str) -> bool {
    let normalized = line.to_ascii_lowercase();
    normalized.contains("external controller listen error")
        && normalized.contains("address already in use")
}

#[cfg(test)]
mod tests;
