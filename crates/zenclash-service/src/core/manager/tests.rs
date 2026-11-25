mod shutdown_tests {
    use super::super::process_guard::{ChildGuard, term_grace};
    use super::super::{CORE_OUTPUT_DRAIN_TIMEOUT, CORE_TERM_GRACE};
    use std::time::{Duration, Instant};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn shutdown_drains_final_output_but_bounds_inherited_open_streams() {
        let (written, final_output) = oneshot::channel();
        let reader = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = written.send("last core line");
        });
        let stuck = tokio::spawn(std::future::pending::<()>());
        let stuck_abort = stuck.abort_handle();
        let another_stuck = tokio::spawn(std::future::pending::<()>());
        let mut guard = ChildGuard::from_readers(vec![reader, stuck, another_stuck]);
        let started = Instant::now();
        guard.terminate(Duration::ZERO).await.unwrap();
        assert_eq!(final_output.await.unwrap(), "last core line");
        assert!(guard.readers.is_empty());
        assert!(started.elapsed() < CORE_OUTPUT_DRAIN_TIMEOUT + Duration::from_secs(1));
        tokio::task::yield_now().await;
        assert!(stuck_abort.is_finished());
    }

    #[test]
    fn queued_stop_never_gets_fresh_grace_after_its_deadline() {
        assert_eq!(
            term_grace(Some(Instant::now() - Duration::from_secs(1))),
            Duration::ZERO
        );
        assert_eq!(
            term_grace(Some(Instant::now() + Duration::from_secs(30))),
            CORE_TERM_GRACE
        );
    }
}

#[cfg(unix)]
mod unix_socket_tests {
    use super::super::ipc_security::{prepare_core_ipc_socket, secure_core_ipc_socket};
    use crate::OwnerIdentity;
    use serial_test::serial;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    #[tokio::test]
    #[serial]
    async fn owner_core_socket_is_private() -> anyhow::Result<()> {
        let directory = std::env::temp_dir().join(format!("cvs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory)?;
        let path = directory.join("mihomo.sock");
        let listener = tokio::net::UnixListener::bind(&path)?;
        // SAFETY: `geteuid` has no preconditions or memory effects.
        let uid = unsafe { platform_lib::geteuid() };
        // SAFETY: `getegid` has no preconditions or memory effects.
        let gid = unsafe { platform_lib::getegid() };
        let owner = OwnerIdentity::Unix { uid, gid };

        prepare_core_ipc_socket(&path.to_string_lossy(), &owner)?;
        drop(listener);
        let listener = tokio::net::UnixListener::bind(&path)?;
        secure_core_ipc_socket(path.to_string_lossy().into_owned(), owner, None).await?;

        let mut mode = 0;
        for _ in 0..40 {
            mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
            if mode == 0o600 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(mode, 0o600);
        drop(listener);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}

mod windows_pipe_tests {
    use super::super::ipc_security::windows_owner_pipe_sddl;

    #[test]
    fn windows_owner_pipe_dacl_excludes_everyone_and_authenticated_users() {
        let sddl = windows_owner_pipe_sddl("S-1-5-21-1-2-3-1001");

        assert!(sddl.contains(";;;S-1-5-21-1-2-3-1001)"));
        assert!(sddl.contains(";;;SY)"));
        assert!(sddl.contains(";;;BA)"));
        assert!(!sddl.contains(";;;WD)"));
        assert!(!sddl.contains(";;;AU)"));
    }
}
