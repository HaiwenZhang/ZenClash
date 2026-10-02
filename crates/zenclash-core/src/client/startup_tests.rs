use super::*;
use zenclash_service::{ServiceClientError, ServiceErrorCode};

#[test]
fn startup_native_rejections_keep_facts_without_diagnostic_string_matching() {
    for (code, expected) in [
        (
            ServiceErrorCode::Occupied,
            ServiceStartupRejection::Occupied,
        ),
        (
            ServiceErrorCode::Unauthorized,
            ServiceStartupRejection::Unauthorized,
        ),
        (
            ServiceErrorCode::Incompatible,
            ServiceStartupRejection::Incompatible,
        ),
        (
            ServiceErrorCode::MaintenancePending,
            ServiceStartupRejection::MaintenancePending,
        ),
    ] {
        let error = MihomoError::Service(ServiceClientError::Rejected(code));
        assert_eq!(error.service_startup_rejection(), Some(expected));
    }
}

#[test]
fn startup_lost_acquire_is_not_reported_as_a_definitive_rejection() {
    for error in [
        ServiceClientError::Rejected(ServiceErrorCode::OutcomeUnknown),
        ServiceClientError::Timeout,
        ServiceClientError::UnexpectedResponse,
    ] {
        assert_eq!(
            MihomoError::Service(error).service_startup_rejection(),
            None
        );
    }
}
