//! Millisecond observation timestamps shared by snapshot recording policies.

use std::time::{SystemTime, UNIX_EPOCH};

pub(in crate::pages::runtime) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
