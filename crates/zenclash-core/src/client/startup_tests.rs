use super::*;
use crate::service::ServiceCallError;
use zenclash_service::ServiceErrorCode;

#[test]
fn native_startup_refusals_preserve_protocol_and_authorization_facts() {
    for (code, expected) in [
        (
            ServiceErrorCode::UnauthorizedOwner,
            ServiceStartupRejection::Unauthorized,
        ),
        (
            ServiceErrorCode::ProtocolMismatch,
            ServiceStartupRejection::Incompatible,
        ),
    ] {
        let error = MihomoError::Service(ServiceCallError::Rejected {
            code: code as u16,
            message: String::new(),
        });
        assert_eq!(error.service_startup_rejection(), Some(expected));
    }
    assert_eq!(
        MihomoError::Service(ServiceCallError::VersionMismatch("old version".into()))
            .service_startup_rejection(),
        Some(ServiceStartupRejection::Incompatible)
    );
}

#[test]
fn lost_start_acknowledgement_does_not_invent_an_occupied_or_unauthorized_owner() {
    for error in [
        ServiceCallError::MissingReply("start proof"),
        ServiceCallError::OwnerLost,
    ] {
        assert_eq!(
            MihomoError::Service(error).service_startup_rejection(),
            None
        );
    }
}
