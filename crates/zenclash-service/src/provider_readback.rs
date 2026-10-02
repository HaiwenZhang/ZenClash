//! Bounded in-memory paging adapted from upstream runtime_generation/readback.rs.
//! The server admits only a declared cache of a confirmed stopped committed runtime.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::protocol::ServiceErrorCode;

pub(crate) const CHUNK_BYTES: usize = 256 * 1024;
const SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;
const WAVE_BYTES: usize = 256 * 1024 * 1024;
const WAVE_PROVIDERS: usize = 256;
const WAVE_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct ReadbackSnapshot {
    pub(crate) token: [u8; 32],
    pub(crate) len: u64,
    pub(crate) sha256: [u8; 32],
}

struct Snapshot {
    token: [u8; 32],
    revision: u64,
    bytes: Vec<u8>,
    next: usize,
}

#[derive(Default)]
pub(crate) struct ProviderReadback {
    snapshot: Option<Snapshot>,
    wave: Option<(u64, Instant, usize, usize)>,
}

impl ProviderReadback {
    #[cfg(test)]
    pub(crate) fn begin(
        &mut self,
        revision: u64,
        bytes: Vec<u8>,
        now: Instant,
    ) -> Result<ReadbackSnapshot, ServiceErrorCode> {
        self.reserve(revision, now)?;
        self.charge(bytes.len(), now)?;
        self.publish(revision, bytes, now)
    }

    pub(crate) fn reserve(
        &mut self,
        revision: u64,
        now: Instant,
    ) -> Result<usize, ServiceErrorCode> {
        self.expire(revision, now);
        if self.snapshot.is_some() {
            return Err(ServiceErrorCode::InvalidRequest);
        }
        let wave = self.wave.get_or_insert((revision, now, 0, 0));
        if now.saturating_duration_since(wave.1) >= WAVE_TIMEOUT
            || wave.2 >= WAVE_BYTES
            || wave.3 >= WAVE_PROVIDERS
        {
            return Err(ServiceErrorCode::BudgetExceeded);
        }
        wave.3 += 1;
        Ok(SNAPSHOT_BYTES.min(WAVE_BYTES - wave.2))
    }

    pub(crate) fn charge(&mut self, bytes: usize, now: Instant) -> Result<(), ServiceErrorCode> {
        let wave = self.wave.as_mut().ok_or(ServiceErrorCode::InvalidRequest)?;
        wave.2 = wave.2.saturating_add(bytes);
        if wave.2 > WAVE_BYTES || now.saturating_duration_since(wave.1) >= WAVE_TIMEOUT {
            return Err(ServiceErrorCode::BudgetExceeded);
        }
        Ok(())
    }

    pub(crate) fn publish(
        &mut self,
        revision: u64,
        bytes: Vec<u8>,
        now: Instant,
    ) -> Result<ReadbackSnapshot, ServiceErrorCode> {
        self.expire(revision, now);
        if self.snapshot.is_some()
            || bytes.len() > SNAPSHOT_BYTES
            || self
                .wave
                .is_none_or(|wave| now.saturating_duration_since(wave.1) >= WAVE_TIMEOUT)
        {
            return Err(ServiceErrorCode::BudgetExceeded);
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| ServiceErrorCode::Internal)?;
        let len = bytes.len() as u64;
        let sha256 = Sha256::digest(&bytes).into();
        self.snapshot = Some(Snapshot {
            token,
            revision,
            bytes,
            next: 0,
        });
        Ok(ReadbackSnapshot { token, len, sha256 })
    }

    pub(crate) fn read(
        &mut self,
        revision: u64,
        token: &[u8; 32],
        offset: u64,
        now: Instant,
    ) -> Result<Vec<u8>, ServiceErrorCode> {
        self.expire(revision, now);
        let snapshot = self
            .snapshot
            .as_mut()
            .ok_or(ServiceErrorCode::InvalidRequest)?;
        if snapshot.revision != revision || &snapshot.token != token {
            return Err(ServiceErrorCode::StaleRevision);
        }
        let offset = usize::try_from(offset).map_err(|_| ServiceErrorCode::InvalidRequest)?;
        if offset != snapshot.next || offset > snapshot.bytes.len() {
            return Err(ServiceErrorCode::InvalidRequest);
        }
        let end = offset.saturating_add(CHUNK_BYTES).min(snapshot.bytes.len());
        snapshot.next = end;
        Ok(snapshot.bytes[offset..end].to_vec())
    }

    pub(crate) fn finish(&mut self, token: &[u8; 32]) -> Result<(), ServiceErrorCode> {
        if !self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| &snapshot.token == token)
        {
            return Err(ServiceErrorCode::InvalidRequest);
        }
        self.snapshot = None;
        Ok(())
    }

    pub(crate) fn finished(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.next == snapshot.bytes.len())
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.wave.map(|wave| wave.1 + WAVE_TIMEOUT)
    }

    pub(crate) fn clear(&mut self) {
        self.snapshot = None;
        self.wave = None;
    }

    pub(crate) fn invalidate(&mut self) {
        self.snapshot = None;
    }

    pub(crate) fn expire(&mut self, revision: u64, now: Instant) {
        if let Some((wave_revision, started, _, _)) = self.wave {
            if wave_revision != revision {
                self.clear();
            } else if now.saturating_duration_since(started) >= WAVE_TIMEOUT {
                self.snapshot = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_rejects_a_second_snapshot_until_the_first_is_finished() {
        let mut reader = ProviderReadback::default();
        let now = Instant::now();
        let first = reader.begin(1, vec![7], now).unwrap();
        assert!(reader.begin(1, vec![8], now).is_err());
        assert_eq!(reader.read(1, &first.token, 0, now).unwrap(), [7]);
        reader.finish(&first.token).unwrap();
        assert!(reader.read(1, &first.token, 0, now).is_err());
        assert!(reader.begin(1, vec![8], now).is_ok());
    }

    #[test]
    fn readback_pages_do_not_extend_the_total_deadline() {
        let mut reader = ProviderReadback::default();
        let now = Instant::now();
        let first = reader.begin(1, vec![9; CHUNK_BYTES + 1], now).unwrap();
        assert_eq!(
            reader.read(1, &first.token, 0, now).unwrap().len(),
            CHUNK_BYTES
        );
        assert!(
            reader
                .read(1, &first.token, CHUNK_BYTES as u64, now + WAVE_TIMEOUT)
                .is_err()
        );
        assert!(reader.snapshot.is_none(), "expired bytes must be released");
    }

    #[test]
    fn readback_duplicate_offsets_do_not_replay_or_advance_the_snapshot() {
        let mut reader = ProviderReadback::default();
        let now = Instant::now();
        let first = reader.begin(1, vec![2; CHUNK_BYTES + 1], now).unwrap();
        assert_eq!(
            reader.read(1, &first.token, 0, now).unwrap().len(),
            CHUNK_BYTES
        );
        assert!(reader.read(1, &first.token, 0, now).is_err());
        assert_eq!(
            reader
                .read(1, &first.token, CHUNK_BYTES as u64, now)
                .unwrap(),
            [2]
        );
    }

    #[test]
    fn readback_wave_provider_budget_survives_finish_and_does_not_refresh() {
        let mut reader = ProviderReadback::default();
        let now = Instant::now();
        for _ in 0..WAVE_PROVIDERS {
            let snapshot = reader.begin(1, Vec::new(), now).unwrap();
            reader.finish(&snapshot.token).unwrap();
        }
        assert!(matches!(
            reader.begin(1, Vec::new(), now),
            Err(ServiceErrorCode::BudgetExceeded)
        ));
        reader.clear();
        assert!(reader.begin(2, vec![1], now).is_ok());
    }

    #[test]
    fn readback_wave_byte_budget_counts_completed_snapshots() {
        let mut reader = ProviderReadback::default();
        let now = Instant::now();
        reader.wave = Some((1, now, WAVE_BYTES, 1));
        assert!(matches!(
            reader.begin(1, vec![1], now),
            Err(ServiceErrorCode::BudgetExceeded)
        ));
    }

    #[tokio::test]
    async fn readback_maximum_chunk_round_trips_the_actual_json_frame_budget() {
        use crate::protocol::{ProviderCacheChunk, Response};
        let (mut sender, mut receiver) = tokio::io::duplex(4096);
        let response = Response::ProviderCacheChunk {
            chunk: ProviderCacheChunk {
                offset: 0,
                bytes: vec![255; CHUNK_BYTES],
                finished: true,
            },
        };
        assert!(serde_json::to_vec(&response).unwrap().len() < crate::MAX_FRAME_BYTES);
        let sending = tokio::spawn(async move {
            crate::write_frame(&mut sender, &response, Duration::from_secs(3)).await
        });
        let Response::ProviderCacheChunk { chunk } =
            crate::read_frame::<_, Response>(&mut receiver, Duration::from_secs(3))
                .await
                .unwrap()
        else {
            panic!("wrong readback frame");
        };
        assert_eq!(chunk.bytes, vec![255; CHUNK_BYTES]);
        assert!(chunk.finished);
        sending.await.unwrap().unwrap();
    }
}
