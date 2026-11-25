// Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
//! Pure protocol version, capability, and error code wire types shared across client, daemon, and core.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub epoch: u16,
    pub revision: u16,
}

impl ProtocolVersion {
    pub const fn current() -> Self {
        Self {
            epoch: crate::PROTOCOL_EPOCH,
            revision: crate::PROTOCOL_REVISION,
        }
    }

    pub fn header_value(self) -> String {
        format!("{}.{}", self.epoch, self.revision)
    }

    pub fn parse_header(value: &str) -> Option<Self> {
        let (epoch, revision) = value.split_once('.')?;
        Some(Self {
            epoch: epoch.parse().ok()?,
            revision: revision.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolInfo {
    pub build_version: String,
    pub protocol: ProtocolVersion,
    pub min_client_revision: u16,
}

impl ProtocolInfo {
    pub fn current() -> Self {
        Self {
            build_version: crate::VERSION.to_owned(),
            protocol: ProtocolVersion::current(),
            min_client_revision: crate::MIN_SUPPORTED_CLIENT_REVISION,
        }
    }

    pub const fn supports_client(&self, client: ProtocolVersion, min_service_revision: u16) -> bool {
        self.protocol.epoch == client.epoch
            && self.protocol.revision >= min_service_revision
            && client.revision >= self.min_client_revision
    }

    /// Whether this service supports in-place runtime staging.
    /// Unlike `supports_client`, this is a capability gate rather than a compatibility gate.
    pub const fn supports_runtime_staging(&self) -> bool {
        self.protocol.epoch == ProtocolVersion::current().epoch
            && self.protocol.revision >= crate::MIN_SERVICE_REVISION_FOR_RUNTIME_STAGING
    }

    pub const fn supports_runtime_file_read(&self) -> bool {
        self.protocol.epoch == ProtocolVersion::current().epoch
            && self.protocol.revision >= crate::MIN_SERVICE_REVISION_FOR_RUNTIME_FILE_READ
    }
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceErrorCode {
    UnauthorizedOwner = 1001,
    NotActive = 1002,
    InvalidInstallLocation = 1003,
    InvalidRuntimeAsset = 1004,
    LegacyCleanupFailed = 1005,
    OwnerSwitchFailed = 1006,
    ProtocolMismatch = 1007,
    StaleOwnerSession = 1008,
    InvalidProxyConfig = 1009,
    ProxyClearFailed = 1010,
    ProxyApplyFailed = 1011,
    AppDataRootNotOwned = 1012,
    SessionConflict = 1013,
    ForbiddenOrigin = 1014,
}

#[cfg(test)]
mod tests {
    use super::{ProtocolInfo, ProtocolVersion, ServiceErrorCode};

    #[test]
    fn stable_session_error_codes_do_not_overlap_existing_codes() {
        assert_eq!(ServiceErrorCode::ProtocolMismatch as u16, 1007);
        assert_eq!(ServiceErrorCode::StaleOwnerSession as u16, 1008);
        assert_eq!(ServiceErrorCode::InvalidProxyConfig as u16, 1009);
        assert_eq!(ServiceErrorCode::ProxyClearFailed as u16, 1010);
        assert_eq!(ServiceErrorCode::ProxyApplyFailed as u16, 1011);
        assert_eq!(ServiceErrorCode::AppDataRootNotOwned as u16, 1012);
        assert_eq!(ServiceErrorCode::SessionConflict as u16, 1013);
        assert_eq!(ServiceErrorCode::ForbiddenOrigin as u16, 1014);
    }

    #[test]
    fn protocol_header_is_independent_from_the_build_version() {
        let current = ProtocolVersion::current();
        assert_eq!(ProtocolVersion::parse_header(&current.header_value()), Some(current));
        assert!(ProtocolVersion::parse_header(crate::VERSION).is_none());
    }

    #[test]
    fn staging_is_gated_separately_from_compatibility() {
        let mut older = ProtocolInfo::current();
        older.protocol.revision = crate::MIN_SERVICE_REVISION_FOR_RUNTIME_STAGING - 1;

        assert!(
            older.supports_client(ProtocolVersion::current(), 1),
            "a caller with an older minimum can still use the base protocol"
        );
        assert!(
            !older.supports_runtime_staging(),
            "but it must not be asked to stage a runtime"
        );
        assert!(!older.supports_client(ProtocolVersion::current(), crate::MIN_REQUIRED_SERVICE_REVISION));
        assert!(ProtocolInfo::current().supports_runtime_staging());
    }

    #[test]
    fn staging_does_not_survive_an_epoch_change() {
        let mut newer_epoch = ProtocolInfo::current();
        newer_epoch.protocol.epoch += 1;
        newer_epoch.protocol.revision = crate::MIN_SERVICE_REVISION_FOR_RUNTIME_STAGING;

        assert!(
            !newer_epoch.supports_runtime_staging(),
            "a revision number means nothing across an epoch boundary"
        );
    }

    #[test]
    fn protocol_compatibility_is_epoch_and_revision_based() {
        let info = ProtocolInfo::current();
        let current = ProtocolVersion::current();
        assert!(info.supports_client(current, crate::MIN_REQUIRED_SERVICE_REVISION));
        assert!(!info.supports_client(
            ProtocolVersion {
                epoch: current.epoch.saturating_add(1),
                revision: current.revision,
            },
            crate::MIN_REQUIRED_SERVICE_REVISION,
        ));
        assert!(!info.supports_client(
            ProtocolVersion {
                epoch: current.epoch,
                revision: info.min_client_revision.saturating_sub(1),
            },
            crate::MIN_REQUIRED_SERVICE_REVISION,
        ));
    }
}
