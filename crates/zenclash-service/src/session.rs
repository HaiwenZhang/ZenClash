#[cfg(feature = "server")]
use std::time::{Duration, Instant};

#[cfg(feature = "server")]
use crate::{SessionProof, SessionToken};

#[cfg(feature = "server")]
pub(crate) const SESSION_LEASE: Duration = Duration::from_secs(30);

/// Constructed only after platform transport verification, never deserialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    user: String,
    pid: u32,
    birth: u64,
}

impl PeerIdentity {
    pub(crate) fn new(user: String, pid: u32, birth: u64) -> Self {
        Self { user, pid, birth }
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
    pub(crate) fn birth(&self) -> u64 {
        self.birth
    }
}

#[cfg(feature = "server")]
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("service runtime is occupied")]
    Occupied,
    #[error("service session identity or credential was rejected")]
    Unauthorized,
    #[error("service session lease expired")]
    Expired,
    #[error("service request sequence was replayed")]
    Replay,
    #[error("system random source is unavailable")]
    Random,
    #[error("service session generation exhausted")]
    GenerationExhausted,
}

#[cfg(feature = "server")]
struct Owner {
    identity: PeerIdentity,
    proof: SessionProof,
    last_sequence: u64,
    deadline: Instant,
}

#[cfg(feature = "server")]
#[derive(Default)]
pub(crate) struct SessionAuthority {
    generation: u64,
    owner: Option<Owner>,
}

#[cfg(feature = "server")]
impl SessionAuthority {
    pub(crate) fn acquire(
        &mut self,
        identity: PeerIdentity,
        now: Instant,
    ) -> Result<SessionProof, SessionError> {
        if let Some(owner) = &self.owner {
            return if owner.identity == identity && now < owner.deadline {
                Ok(owner.proof.clone())
            } else {
                Err(SessionError::Occupied)
            };
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SessionError::GenerationExhausted)?;
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|_| SessionError::Random)?;
        let proof = SessionProof::new(generation, SessionToken(token));
        self.owner = Some(Owner {
            identity,
            proof: proof.clone(),
            last_sequence: 0,
            deadline: now + SESSION_LEASE,
        });
        self.generation = generation;
        Ok(proof)
    }

    pub(crate) fn admit(
        &mut self,
        identity: &PeerIdentity,
        proof: &SessionProof,
        sequence: u64,
        now: Instant,
    ) -> Result<(), SessionError> {
        let owner = self.owner.as_mut().ok_or(SessionError::Unauthorized)?;
        if owner.identity != *identity || !owner.proof.authenticates(proof) {
            return Err(SessionError::Unauthorized);
        }
        if now >= owner.deadline {
            return Err(SessionError::Expired);
        }
        if sequence <= owner.last_sequence {
            return Err(SessionError::Replay);
        }
        owner.last_sequence = sequence;
        owner.deadline = now + SESSION_LEASE;
        Ok(())
    }

    pub(crate) fn release_after_stop(&mut self, proof: &SessionProof) -> Result<(), SessionError> {
        let owner = self.owner.as_ref().ok_or(SessionError::Unauthorized)?;
        if !owner.proof.authenticates(proof) {
            return Err(SessionError::Unauthorized);
        }
        self.owner = None;
        Ok(())
    }

    pub(crate) fn expired_owner(&self, now: Instant) -> Option<&PeerIdentity> {
        self.owner
            .as_ref()
            .filter(|owner| now >= owner.deadline)
            .map(|owner| &owner.identity)
    }

    pub(crate) fn owner(&self) -> Option<&PeerIdentity> {
        self.owner.as_ref().map(|owner| &owner.identity)
    }

    pub(crate) fn current_proof(&self) -> Option<&SessionProof> {
        self.owner.as_ref().map(|owner| &owner.proof)
    }

    pub(crate) fn next_sequence(&self) -> Option<u64> {
        self.owner
            .as_ref()
            .and_then(|owner| owner.last_sequence.checked_add(1))
    }

    pub(crate) fn stream_valid(
        &self,
        identity: &PeerIdentity,
        proof: &SessionProof,
        now: Instant,
    ) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.identity == *identity && owner.proof.authenticates(proof) && now < owner.deadline
        })
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use std::time::Duration;

    use super::*;

    fn peer(uid: &str, pid: u32, created: u64) -> PeerIdentity {
        PeerIdentity::new(uid.into(), pid, created)
    }

    #[test]
    fn streaming_observations_cannot_renew_an_abandoned_control_session() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        assert!(authority.stream_valid(
            &owner,
            &proof,
            now + SESSION_LEASE - Duration::from_secs(1)
        ));
        assert!(!authority.stream_valid(&owner, &proof, now + SESSION_LEASE));
        assert!(!authority.stream_valid(&peer("1001", 12, 1), &proof, now));
    }

    #[test]
    fn only_one_verified_process_can_control_the_runtime() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        assert_eq!(authority.acquire(owner, now).unwrap(), proof);
        assert!(matches!(
            authority.acquire(peer("1001", 13, 2), now),
            Err(SessionError::Occupied)
        ));
        assert!(matches!(
            authority.acquire(peer("1000", 12, 2), now),
            Err(SessionError::Occupied)
        ));
    }

    #[test]
    fn forged_identity_or_token_does_not_consume_the_owners_sequence() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        assert!(matches!(
            authority.admit(&peer("1001", 12, 1), &proof, 1, now),
            Err(SessionError::Unauthorized)
        ));
        let forged = SessionProof::new(proof.generation(), SessionToken([0; 32]));
        assert!(matches!(
            authority.admit(&owner, &forged, 1, now),
            Err(SessionError::Unauthorized)
        ));
        assert!(authority.admit(&owner, &proof, 1, now).is_ok());
    }

    #[test]
    fn repeated_and_out_of_order_requests_cannot_repeat_a_mutation() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        authority.admit(&owner, &proof, 2, now).unwrap();
        for sequence in [0, 1, 2] {
            assert!(matches!(
                authority.admit(&owner, &proof, sequence, now),
                Err(SessionError::Replay)
            ));
        }
        assert!(authority.admit(&owner, &proof, 3, now).is_ok());
    }

    #[test]
    fn an_expired_owner_remains_reserved_until_its_child_has_been_reaped() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        let later = now + SESSION_LEASE + Duration::from_secs(1);
        assert!(matches!(
            authority.admit(&owner, &proof, 1, later),
            Err(SessionError::Expired)
        ));
        assert_eq!(authority.expired_owner(later), Some(&owner));
        assert!(matches!(
            authority.acquire(peer("1001", 13, 2), later),
            Err(SessionError::Occupied)
        ));
        authority.release_after_stop(&proof).unwrap();
        let new = authority.acquire(owner.clone(), later).unwrap();
        assert_ne!(new.generation(), proof.generation());
        assert!(matches!(
            authority.admit(&owner, &proof, 2, later),
            Err(SessionError::Unauthorized)
        ));
    }

    #[test]
    fn accepted_requests_extend_the_lease_and_rejected_requests_do_not() {
        let now = Instant::now();
        let owner = peer("1000", 12, 1);
        let mut authority = SessionAuthority::default();
        let proof = authority.acquire(owner.clone(), now).unwrap();
        let renewal = now + SESSION_LEASE / 2;
        authority.admit(&owner, &proof, 1, renewal).unwrap();
        assert!(authority.expired_owner(now + SESSION_LEASE).is_none());
        assert!(matches!(
            authority.admit(&owner, &proof, 1, now + SESSION_LEASE),
            Err(SessionError::Replay)
        ));
        assert!(authority.expired_owner(renewal + SESSION_LEASE).is_some());
    }
}
