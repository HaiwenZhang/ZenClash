#[derive(Debug)]
pub(in crate::app) struct LatestCommandQueue<T> {
    running: bool,
    pending: Option<T>,
}

impl<T> Default for LatestCommandQueue<T> {
    fn default() -> Self {
        Self {
            running: false,
            pending: None,
        }
    }
}

impl<T> LatestCommandQueue<T> {
    pub(super) fn submit(&mut self, command: T) -> Option<T> {
        if self.running {
            self.pending = Some(command);
            None
        } else {
            self.running = true;
            Some(command)
        }
    }

    pub(super) fn complete(&mut self) -> Option<T> {
        if let Some(command) = self.pending.take() {
            Some(command)
        } else {
            self.running = false;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LatestCommandQueue;

    #[test]
    fn idle_queue_starts_a_command_immediately() {
        let mut queue = LatestCommandQueue::default();

        assert_eq!(queue.submit("first"), Some("first"));
    }

    #[test]
    fn busy_queue_keeps_only_the_latest_command() {
        let mut queue = LatestCommandQueue::default();
        let _ = queue.submit("first");

        assert_eq!(queue.submit("second"), None);
        assert_eq!(queue.submit("latest"), None);
        assert_eq!(queue.complete(), Some("latest"));
    }

    #[test]
    fn queue_becomes_idle_after_all_commands_complete() {
        let mut queue = LatestCommandQueue::default();
        let _ = queue.submit("first");

        assert_eq!(queue.complete(), None);
        assert_eq!(queue.submit("next"), Some("next"));
    }
}
