#[cfg(not(feature = "test"))]
use anyhow::{Context as _, Result, bail};
use std::ffi::OsStr;

#[cfg(not(feature = "test"))]
use platform_lib::service::ServiceAccess;
#[cfg(not(feature = "test"))]
use platform_lib::service_manager::{ServiceManager, ServiceManagerAccess};

fn is_local_system_account(account: &OsStr) -> bool {
    matches!(
        account.to_string_lossy().to_ascii_lowercase().as_str(),
        "localsystem" | "nt authority\\system"
    )
}

fn trusted_service_identity(pipe_process_id: u32, service_process_id: Option<u32>, account: Option<&OsStr>) -> bool {
    pipe_process_id != 0 && service_process_id == Some(pipe_process_id) && account.is_some_and(is_local_system_account)
}

#[cfg(not(feature = "test"))]
fn verify_registered_service_process_id_inner(pipe_process_id: u32) -> Result<()> {
    let service_name = crate::WINDOWS_SERVICE_NAME;
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("failed to connect to the Windows Service Control Manager")?;
    let service = manager
        .open_service(service_name, ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG)
        .with_context(|| format!("failed to query registered service {service_name:?}"))?;
    let status = service
        .query_status()
        .with_context(|| format!("failed to query status for service {service_name:?}"))?;
    let config = service
        .query_config()
        .with_context(|| format!("failed to query configuration for service {service_name:?}"))?;

    if !trusted_service_identity(pipe_process_id, status.process_id, config.account_name.as_deref()) {
        bail!("Windows named-pipe server does not match the registered LocalSystem service process");
    }

    Ok(())
}

#[cfg(not(feature = "test"))]
pub(super) fn verify_registered_service_process_id(pipe_process_id: u32) -> std::io::Result<()> {
    verify_registered_service_process_id_inner(pipe_process_id).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::trusted_service_identity;
    use std::ffi::OsStr;

    #[test]
    fn accepts_the_registered_local_system_service_process() {
        assert!(trusted_service_identity(
            4242,
            Some(4242),
            Some(OsStr::new("LocalSystem")),
        ));
    }

    #[test]
    fn rejects_a_different_pipe_process() {
        assert!(!trusted_service_identity(
            31337,
            Some(4242),
            Some(OsStr::new("LocalSystem")),
        ));
    }

    #[test]
    fn rejects_a_non_system_service_account() {
        assert!(!trusted_service_identity(
            4242,
            Some(4242),
            Some(OsStr::new(".\\ordinary-user")),
        ));
    }

    #[test]
    fn accepts_the_explicit_nt_authority_system_name() {
        assert!(trusted_service_identity(
            4242,
            Some(4242),
            Some(OsStr::new("NT AUTHORITY\\SYSTEM")),
        ));
    }
}
