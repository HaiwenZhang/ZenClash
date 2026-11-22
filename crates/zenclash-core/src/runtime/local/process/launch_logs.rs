use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Read},
    sync::Arc,
};

const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn read_output(reader: impl Read, mut record: impl FnMut(&str)) -> io::Result<()> {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if available.is_empty() {
            if !line.is_empty() {
                record(&String::from_utf8_lossy(&line));
            }
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_length = newline.unwrap_or(available.len());
        let keep = content_length.min(MAX_LINE_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&available[..keep]);
        reader.consume(content_length + usize::from(newline.is_some()));
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            record(&String::from_utf8_lossy(&line));
            line.clear();
        }
    }
}

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
    bytes: usize,
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
        let limit = MAX_LINE_BYTES.saturating_sub(label.len() + 3);
        let line = &line[..line.floor_char_boundary(limit.min(line.len()))];
        let text: Arc<str> = format!("[{label}] {line}").into();
        if let Some(current) = &mut self.current
            && Arc::ptr_eq(&current.identity.0, &identity.0)
        {
            current.last_line = Some(text.clone());
            if is_controller_listener_error(line) {
                current.listener_error = Some(text.clone());
            }
        }
        while self.history.len() >= super::MAX_LOG_LINES
            || self.bytes.saturating_add(text.len()) > MAX_HISTORY_BYTES
        {
            if let Some(previous) = self.history.pop_front() {
                self.bytes -= previous.len();
            }
        }
        self.bytes += text.len();
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
        let bytes = messages.iter().map(String::len).sum();
        Self {
            history: messages.into_iter().map(Arc::from).collect(),
            bytes,
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
