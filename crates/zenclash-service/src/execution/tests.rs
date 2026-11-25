use super::*;

#[cfg(windows)]
fn set_test_dacl(path: &Path, sddl: &str) {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            DACL_SECURITY_INFORMATION, SetFileSecurityW,
        },
    };
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let converted = unsafe {
        // SAFETY: `sddl` is NUL-terminated and `descriptor` is a writable
        // out-parameter; the allocation is freed below.
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(converted, 0);
    // SAFETY: `wide` is NUL-terminated and `descriptor` is a live
    // descriptor for the duration of the call.
    let secured = unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
    // SAFETY: `descriptor` is the allocation returned above and is freed
    // exactly once here.
    unsafe {
        LocalFree(descriptor);
    }
    assert_ne!(secured, 0);
}

#[cfg(windows)]
#[test]
fn missing_legacy_lock_does_not_require_parent_write_access() -> Result<()> {
    let root = std::env::temp_dir().join(format!("execution-readonly-parent-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let path = root.join("core.lock");
    set_test_dacl(&root, "D:P(D;;0x2;;;WD)(A;;FA;;;AU)(A;;FA;;;SY)");
    let result = (|| -> Result<()> {
        let name = test_coordination_name(&path);
        for service_absent in [false, true] {
            let refused = CoreExecutionGuard::acquire_windows_at(
                &name,
                &path,
                || {
                    assert!(CoreExecutionGuard::acquire_at(&path).is_err());
                    anyhow::ensure!(service_absent, "registered stopped service");
                    Ok(())
                },
                || {
                    assert!(service_absent);
                    assert!(CoreExecutionGuard::acquire_at(&path).is_err());
                    anyhow::bail!("surviving core after owner exit")
                },
            );
            let message = refused.unwrap_err().to_string();
            assert!(message.contains(if service_absent {
                "surviving core"
            } else {
                "registered stopped service"
            }));
        }
        let first = CoreExecutionGuard::acquire_at(&path)?;
        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
        drop(first);
        drop(CoreExecutionGuard::acquire_at(&path)?);
        Ok(())
    })();
    assert!(!path.exists());
    std::fs::remove_dir(&root)?;
    result
}

#[cfg(windows)]
#[test]
fn unreadable_legacy_file_fails_closed_without_leaking_the_mutex() -> Result<()> {
    let path = std::env::temp_dir().join(format!("execution-unreadable-{}", std::process::id()));
    std::fs::write(&path, b"")?;
    set_test_dacl(&path, "D:P(D;;GR;;;WD)(A;;FA;;;AU)(A;;FA;;;SY)");
    let refused = CoreExecutionGuard::acquire_at(&path);
    set_test_dacl(&path, "D:P(A;;FA;;;AU)(A;;FA;;;SY)");
    let message = refused.unwrap_err().to_string();
    assert!(message.contains("legacy core execution lock"));
    assert!(message.contains(path.file_name().unwrap().to_str().unwrap()));
    drop(CoreExecutionGuard::acquire_at(&path)?);
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn legacy_and_global_reservations_exclude_each_other() -> Result<()> {
    let path = std::env::temp_dir().join(format!("execution-legacy-bridge-{}", std::process::id()));
    let legacy = open_coordination_file(&path)?;
    legacy.try_lock()?;
    assert!(CoreExecutionGuard::acquire_at(&path).is_err());
    legacy.unlock()?;
    let modern = CoreExecutionGuard::acquire_at(&path)?;
    assert!(matches!(legacy.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
    drop(modern);
    legacy.try_lock()?;
    drop(legacy);
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn child_reservation_holder() -> Result<()> {
    if let Some(path) = std::env::var_os("ZENCLASH_TEST_HOLD_LOCK") {
        let path = Path::new(&path);
        let _guard = CoreExecutionGuard::acquire_at(path)?;
        std::fs::write(path.with_extension("ready"), b"ready")?;
        loop {
            std::thread::park();
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn terminated_process_releases_both_reservations() -> Result<()> {
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::System::Threading::OpenMutexW;
    let path = std::env::temp_dir().join(format!("execution-crash-{}", std::process::id()));
    let ready = path.with_extension("ready");
    let mut keep_mutex_alive = None;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "execution::tests::child_reservation_holder"])
        .env("ZENCLASH_TEST_HOLD_LOCK", &path)
        .spawn()?;
    let result = (|| -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            anyhow::ensure!(
                std::time::Instant::now() < deadline && child.try_wait()?.is_none(),
                "child did not reserve execution"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(CoreExecutionGuard::acquire_at(&path).is_err());
        let wide: Vec<u16> = test_coordination_name(&path).encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated; the returned handle is checked
        // for null before adoption.
        let handle = unsafe { OpenMutexW(0x00100000, 0, wide.as_ptr()) };
        anyhow::ensure!(!handle.is_null(), "could not retain mutex for abandonment test");
        // SAFETY: `handle` is a valid non-null handle; OwnedHandle now owns it.
        keep_mutex_alive = Some(unsafe { OwnedHandle::from_raw_handle(handle) });
        Ok(())
    })();
    let _ = child.kill();
    child.wait()?;
    result?;
    let checked = std::cell::Cell::new(false);
    let refused = CoreExecutionGuard::acquire_windows_at(
        &test_coordination_name(&path),
        &path,
        || panic!("the legacy lock exists"),
        || {
            checked.set(true);
            anyhow::bail!("residual core is still running");
        },
    );
    assert!(checked.get());
    assert!(refused.unwrap_err().to_string().contains("residual core"));
    drop(CoreExecutionGuard::acquire_at(&path)?);
    drop(keep_mutex_alive);
    std::fs::remove_file(path)?;
    std::fs::remove_file(ready)?;
    Ok(())
}

#[tokio::test]
async fn termination_event_releases_reservation_even_when_pid_still_exists() -> Result<()> {
    let path = std::env::temp_dir().join(format!("execution-reused-pid-test-{}", std::process::id()));
    let guard = CoreExecutionGuard::acquire_at(&path).context("initial reservation")?;
    let (terminated, receiver) = tokio::sync::oneshot::channel();
    let waiter = guard.release_after_exit(std::process::id(), receiver);
    assert!(CoreExecutionGuard::acquire_at(&path).is_err());
    terminated
        .send(())
        .map_err(|_| anyhow::anyhow!("exit observer dropped"))?;
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter).await??;
    drop(CoreExecutionGuard::acquire_at(&path).context("reservation after termination event")?);
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(all(target_os = "linux", feature = "client"))]
#[test]
fn linux_idle_helper_from_other_channel_does_not_block_fallback() -> Result<()> {
    let other = if cfg!(feature = "development-channel") {
        "zenclash-service"
    } else {
        "zenclash-service-dev"
    };
    let current = crate::SERVICE_SLUG;
    require_no_unix_core_processes(&format!("zenclash-servic /var/lib/{other}/bin/zenclash-service"), true)?;
    assert!(
        require_no_unix_core_processes(
            &format!("zenclash-servic /var/lib/{current}/bin/zenclash-service"),
            true
        )
        .is_err()
    );
    assert!(require_no_unix_core_processes("zenclash-servic /tmp/zenclash-service", true).is_err());
    assert!(
        require_no_unix_core_processes(
            &format!("zenclash-servic /var/lib/{other}/bin/zenclash-service\nmihomo-alpha /tmp/mihomo-alpha"),
            true
        )
        .is_err()
    );
    Ok(())
}

#[cfg(all(unix, feature = "client"))]
#[test]
fn alpha_core_blocks_fallback() {
    for ipc_failed in [false, true] {
        assert!(require_no_unix_core_processes("mihomo-alpha", ipc_failed).is_err());
    }
}

#[cfg(unix)]
#[test]
fn fifo_coordination_entry_does_not_block() -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::env::temp_dir().join(format!("execution-fifo-test-{}", std::process::id()));
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `name` is a NUL-terminated path and `mkfifo` has no other
    // memory preconditions.
    anyhow::ensure!(unsafe { platform_lib::mkfifo(name.as_ptr(), 0o600) } == 0);
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "execution::tests::child_lock_probe"])
        .env("ZENCLASH_TEST_LOCK", &path)
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let result = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    if result.is_none() {
        child.kill()?;
        child.wait()?;
    }
    std::fs::remove_file(path)?;
    assert!(
        result.is_some_and(|status| status.success()),
        "opening a FIFO must fail without waiting for a writer"
    );
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "client"))]
#[test]
fn an_idle_helper_from_the_other_channel_does_not_block_fallback() -> Result<()> {
    let production = "/Library/PrivilegedHelperTools/org.zenclash.app.service.bundle/Contents/MacOS/zenclash-service";
    let development =
        "/Library/PrivilegedHelperTools/org.zenclash.app.dev.service.bundle/Contents/MacOS/zenclash-service";
    let (current, other) = if cfg!(feature = "development-channel") {
        (development, production)
    } else {
        (production, development)
    };
    require_no_unix_core_processes(other, true)?;
    for residual in [
        current.to_owned(),
        "zenclash-service".into(),
        format!("/tmp{other}"),
        format!("{other}\n/Library/Application Support/zenclash-service/cores/mihomo"),
    ] {
        assert!(
            require_no_unix_core_processes(&residual, true).is_err(),
            "unconfirmed idle state must remain blocked: {residual}"
        );
    }
    for residual in [
        current.to_owned(),
        format!("/tmp/mihomo\n{current}"),
        format!("{current}\n/tmp/mihomo"),
    ] {
        let refusal = require_no_unix_core_processes(&residual, true).expect_err("residual helper must block");
        assert!(
            refusal.downcast_ref::<ResidualServiceError>().is_some(),
            "a residual helper must stay repairable: {residual}"
        );
    }
    Ok(())
}

#[test]
fn child_lock_probe() -> Result<()> {
    if let Some(path) = std::env::var_os("ZENCLASH_TEST_LOCK") {
        assert!(CoreExecutionGuard::acquire_at(Path::new(&path)).is_err());
    }
    Ok(())
}

#[test]
fn reservation_excludes_other_processes_until_drop() -> Result<()> {
    let root = std::env::temp_dir().join(format!("execution-lock-test-{}", std::process::id()));
    let first = CoreExecutionGuard::acquire_at(&root)?;
    assert!(CoreExecutionGuard::acquire_at(&root).is_err());
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "execution::tests::child_lock_probe"])
        .env("ZENCLASH_TEST_LOCK", &root)
        .status()?;
    assert!(child.success());
    drop(first);
    drop(CoreExecutionGuard::acquire_at(&root)?);
    std::fs::remove_file(root)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn reservation_survives_stop_request_until_process_exit() -> Result<()> {
    let root = std::env::temp_dir().join(format!("execution-exit-test-{}", std::process::id()));
    let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
    let guard = CoreExecutionGuard::acquire_at(&root)?;
    let (_sender, receiver) = tokio::sync::oneshot::channel();
    let waiter = guard.release_after_exit(child.id(), receiver);
    assert!(CoreExecutionGuard::acquire_at(&root).is_err());
    child.kill()?;
    child.wait()?;
    tokio::time::timeout(std::time::Duration::from_secs(2), waiter).await??;
    drop(CoreExecutionGuard::acquire_at(&root)?);
    std::fs::remove_file(root)?;
    Ok(())
}
