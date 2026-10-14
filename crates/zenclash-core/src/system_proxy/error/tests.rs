use super::*;
use crate::{PacError, PacServerOperation, normalize_pac_script, normalize_system_proxy_host};
use std::{error::Error, io};

#[test]
fn rejected_proxy_input_has_a_typed_reason_without_echoing_credentials() {
    let error = normalize_system_proxy_host("http://user:private-password@host").unwrap_err();
    assert!(matches!(&error, MihomoError::SystemProxy(error)
    if matches!(error.as_ref(), SystemProxyError::Validation {
        field: SystemProxySetting::Host, reason: SystemProxyValidationError::InvalidSyntax,
    })));
    assert!(!format!("{error:?} {error}").contains("private-password"));
}

#[test]
fn missing_pac_entry_point_is_a_script_validation_failure() {
    let error = normalize_pac_script("return 'DIRECT';").unwrap_err();
    assert!(matches!(error, MihomoError::SystemProxy(error)
    if matches!(error.as_ref(), SystemProxyError::Validation {
        field: SystemProxySetting::PacScript, reason: SystemProxyValidationError::MissingPacFunction,
    })));
}

#[test]
fn pac_startup_retains_its_operation_and_io_source_through_the_legacy_result() {
    let error = MihomoError::from(SystemProxyError::Pac(PacError::Io {
        operation: PacServerOperation::Bind,
        source: io::Error::new(io::ErrorKind::PermissionDenied, "fixture listener denied"),
    }));
    assert!(matches!(&error, MihomoError::SystemProxy(error)
    if matches!(error.as_ref(), SystemProxyError::Pac(PacError::Io {
        operation: PacServerOperation::Bind, ..
    }))));
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn recovery_pending_ownership_change_and_unstable_observation_are_distinct() {
    use crate::system_proxy::native::{ownership_changed, pending_recovery, unstable_observation};
    assert!(matches!(pending_recovery(), MihomoError::SystemProxy(error)
        if matches!(error.as_ref(), SystemProxyError::RecoveryPending)));
    assert!(
        matches!(ownership_changed(), MihomoError::SystemProxy(error)
        if matches!(error.as_ref(), SystemProxyError::OwnershipChanged))
    );
    assert!(
        matches!(unstable_observation(), MihomoError::SystemProxy(error)
        if matches!(error.as_ref(), SystemProxyError::ObservationUnstable))
    );
}
