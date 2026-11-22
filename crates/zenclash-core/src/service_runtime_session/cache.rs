// Adapted from Clash Verge Rev core/service.rs fetch_runtime_file/decode_hex, 2026-10-04.
// GPL-3.0-only; see the application integration NOTICE.md and UPSTREAM.json.
//! Read a stable native provider cache before releasing its owner proof.

use super::*;

const SETTLE_WINDOW: Duration = Duration::from_secs(2);
const SETTLE_RETRY_MARGIN: Duration = Duration::from_millis(50);
const MAX_SETTLE_RETRIES: u8 = 3;

pub(super) async fn read_complete<T: RuntimeTransport>(
    client: &T,
    destination: &str,
) -> MihomoResult<Option<Vec<u8>>> {
    let mut settle_retries = 0_u8;
    'attempt: loop {
        let mut content = Vec::new();
        let mut identity = None;
        loop {
            let request = RuntimeFileRequest {
                destination: destination.to_owned(),
                offset: content.len() as u64,
            };
            let (hex, len, mtime_ns) = match client.read_runtime_file(&request).await? {
                RuntimeFileOutcome::Absent if content.is_empty() => return Ok(None),
                RuntimeFileOutcome::Absent => return Err(unknown()),
                RuntimeFileOutcome::Chunk { hex, len, mtime_ns } => (hex, len, mtime_ns),
            };
            if len > crate::service_runtime::MAX_ASSET_BYTES {
                return Err(unknown());
            }
            if let Some(wait) = settle_wait_duration(mtime_ns) {
                if settle_retries >= MAX_SETTLE_RETRIES {
                    return Err(unknown());
                }
                settle_retries += 1;
                tokio::time::sleep(wait).await;
                continue 'attempt;
            }
            if *identity.get_or_insert((len, mtime_ns)) != (len, mtime_ns) {
                return Err(unknown());
            }
            if content.len() as u64 == len {
                if !hex.is_empty() {
                    return Err(unknown());
                }
                return Ok(Some(content));
            }
            let chunk = decode_hex(&hex)?;
            if chunk.is_empty() || content.len() as u64 + chunk.len() as u64 > len {
                return Err(unknown());
            }
            content.extend_from_slice(&chunk);
        }
    }
}

fn settle_wait_duration(mtime_ns: Option<u64>) -> Option<Duration> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    settle_wait_duration_at(mtime_ns, now_ns)
}

fn settle_wait_duration_at(mtime_ns: Option<u64>, now_ns: u128) -> Option<Duration> {
    let mtime_ns = u128::from(mtime_ns?);
    let elapsed_ns = now_ns.saturating_sub(mtime_ns);
    let settle_ns = SETTLE_WINDOW.as_nanos();
    if elapsed_ns >= settle_ns {
        return None;
    }
    let remaining_ns = settle_ns.saturating_sub(elapsed_ns);
    let capped_ns = remaining_ns.min(settle_ns);
    let wait = Duration::from_nanos(u64::try_from(capped_ns).unwrap_or(u64::MAX));
    Some(wait.saturating_add(SETTLE_RETRY_MARGIN))
}

fn decode_hex(encoded: &str) -> MihomoResult<Vec<u8>> {
    // ASCII validation also prevents slicing in the middle of a UTF-8 character.
    if !encoded.is_ascii() || !encoded.len().is_multiple_of(2) || encoded.len() > 2 * 1024 * 1024 {
        return Err(MihomoError::InvalidInput(
            "Invalid native provider cache encoding".into(),
        ));
    }
    (0..encoded.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&encoded[offset..offset + 2], 16).map_err(|_| {
                MihomoError::InvalidInput("Invalid native provider cache encoding".into())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_unicode_hex_is_rejected_without_panicking() {
        assert!(decode_hex("aéa").is_err());
        assert!(decode_hex("0").is_err());
        assert!(decode_hex("zz").is_err());
        assert_eq!(decode_hex("00FFa1").unwrap(), [0, 255, 161]);
    }

    #[test]
    fn settle_wait_duration_waits_for_fresh_files_and_accepts_settled_files() {
        let now_ns = Duration::from_secs(100).as_nanos();
        // Absent mtime or >= 2s old requires no wait.
        assert_eq!(settle_wait_duration_at(None, now_ns), None);
        let settled_mtime = u64::try_from(Duration::from_secs(98).as_nanos()).unwrap();
        assert_eq!(settle_wait_duration_at(Some(settled_mtime), now_ns), None);

        // 500ms old file waits ~1.5s + 50ms margin.
        let fresh_mtime = u64::try_from(Duration::from_millis(99_500).as_nanos()).unwrap();
        let wait = settle_wait_duration_at(Some(fresh_mtime), now_ns).unwrap();
        assert_eq!(wait, Duration::from_millis(1_550));
    }
}
