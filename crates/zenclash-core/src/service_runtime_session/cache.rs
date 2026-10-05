// Adapted from Clash Verge Rev core/service.rs fetch_runtime_file/decode_hex, 2026-10-04.
// GPL-3.0-only; see the application integration NOTICE.md and UPSTREAM.json.
//! Read a stable native provider cache before releasing its owner proof.

use super::*;

pub(super) async fn read_complete<T: RuntimeTransport>(
    client: &T,
    destination: &str,
) -> MihomoResult<Option<Vec<u8>>> {
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
        if len > crate::service_runtime::MAX_ASSET_BYTES
            || !has_settled(mtime_ns)
            || *identity.get_or_insert((len, mtime_ns)) != (len, mtime_ns)
        {
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

fn has_settled(mtime_ns: Option<u64>) -> bool {
    let Some(mtime_ns) = mtime_ns else {
        return true;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    now.saturating_sub(u128::from(mtime_ns)) >= Duration::from_secs(2).as_nanos()
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
}
