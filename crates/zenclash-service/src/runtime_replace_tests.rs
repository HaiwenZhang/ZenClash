//! Actual ordinary Windows MoveFileEx baselines; no service maintenance occurs.

use std::{fs, io, os::windows::fs::OpenOptionsExt, path::Path};

use super::{
    ReplacementBudget, create_temporary, native_move, replace, replace_with, validate_namespace,
    validate_target,
};

fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(source)?;
    replace(source, target, &pin, &mut ReplacementBudget::default())
}

struct Fixture {
    root: crate::installer::OwnedTestRoot,
    directory: std::path::PathBuf,
    source: std::path::PathBuf,
    target: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = crate::installer::OwnedTestRoot::create().unwrap();
        let directory = root.path().join("replacement");
        root.create_directory(&directory).unwrap();
        let source = directory.join("resource.new");
        let target = directory.join("resource");
        fs::write(&source, b"complete new bytes").unwrap();
        fs::write(&target, b"complete old bytes").unwrap();
        Self {
            root,
            directory,
            source,
            target,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.root.remove(&self.directory).unwrap();
        fs::remove_dir(self.root.path()).unwrap();
    }
}

fn no_delete_share(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(path)
        .unwrap()
}

fn delete_probe(path: &Path) -> io::Result<fs::File> {
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_OPEN_REPARSE_POINT};
    fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(1 | 2 | 4)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[test]
fn replacement_baseline_combined_target_obstacles() {
    for obstacle in ["readonly", "target-acl", "source-acl", "parent-acl"] {
        let fixture = Fixture::new();
        let held = no_delete_share(&fixture.target);
        let original_permissions = fs::metadata(&fixture.target).unwrap().permissions();
        let acl_path = match obstacle {
            "target-acl" => Some(&fixture.target),
            "source-acl" => Some(&fixture.source),
            "parent-acl" => Some(&fixture.directory),
            _ => None,
        };
        if let Some(path) = acl_path {
            set_fixture_acl(path, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
        } else {
            let mut permissions = fs::metadata(&fixture.target).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&fixture.target, permissions).unwrap();
        }
        let result = native_move(&fixture.source, &fixture.target);
        let probe = delete_probe(&fixture.target);
        let source = fs::read(&fixture.source).unwrap();
        let target = fs::read(&fixture.target).unwrap();
        eprintln!(
            "{obstacle}+target-held: move={:?}, delete-probe={:?}",
            result.as_ref().err().and_then(io::Error::raw_os_error),
            probe.as_ref().err().and_then(io::Error::raw_os_error)
        );
        drop(probe);
        drop(held);
        if let Some(path) = acl_path {
            set_fixture_acl(path, "D:P(A;;FA;;;OW)");
        } else {
            fs::set_permissions(&fixture.target, original_permissions).unwrap();
        }
        assert!(result.is_err(), "{obstacle} unexpectedly replaced target");
        assert_eq!(source, b"complete new bytes");
        assert_eq!(target, b"complete old bytes");
    }
}

#[test]
fn replacement_retry_target_released_preserves_complete_new_bytes() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.target);
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        drop(held);
    });
    let result = replace_file(&fixture.source, &fixture.target);
    release.join().unwrap();
    assert!(
        result.is_ok(),
        "target contention was not retried: {result:?}"
    );
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

#[test]
fn replacement_retry_source_released_preserves_complete_new_bytes() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.source);
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        drop(held);
    });
    let result = replace_file(&fixture.source, &fixture.target);
    release.join().unwrap();
    assert!(
        result.is_ok(),
        "source contention was not retried: {result:?}"
    );
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

fn set_fixture_acl(path: &Path, sddl: &str) {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
        },
    };
    let sddl: Vec<_> = sddl.encode_utf16().chain([0]).collect();
    let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: the terminated input and output storage remain valid for the call.
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        },
        0
    );
    // SAFETY: this valid descriptor remains alive until LocalFree, and the path is terminated.
    let result = unsafe {
        SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: conversion allocated this descriptor with LocalAlloc.
    unsafe { LocalFree(descriptor) };
    assert_ne!(result, 0, "owned fixture ACL failed: {error}");
}

#[test]
fn replacement_baseline_source_no_delete_share_preserves_both_files() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.source);
    let result = native_move(&fixture.source, &fixture.target);
    eprintln!(
        "source-held MoveFileEx raw error: {:?}",
        result.as_ref().err().and_then(io::Error::raw_os_error)
    );
    let source = fs::read(&fixture.source).unwrap();
    let target = fs::read(&fixture.target).unwrap();
    drop(held);
    assert!(result.is_err(), "held source unexpectedly moved");
    assert_eq!(source, b"complete new bytes");
    assert_eq!(target, b"complete old bytes");
    replace_file(&fixture.source, &fixture.target).unwrap();
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

#[test]
fn replacement_baseline_target_no_delete_share_preserves_both_files() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.target);
    let result = native_move(&fixture.source, &fixture.target);
    eprintln!(
        "target-held MoveFileEx raw error: {:?}",
        result.as_ref().err().and_then(io::Error::raw_os_error)
    );
    let source = fs::read(&fixture.source).unwrap();
    let target = fs::read(&fixture.target).unwrap();
    drop(held);
    assert!(result.is_err(), "held target unexpectedly replaced");
    assert_eq!(source, b"complete new bytes");
    assert_eq!(target, b"complete old bytes");
    replace_file(&fixture.source, &fixture.target).unwrap();
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

#[test]
fn replacement_baseline_acl_delete_denial_preserves_both_files() {
    let fixture = Fixture::new();
    for path in [&fixture.directory, &fixture.source, &fixture.target] {
        set_fixture_acl(path, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    }
    let result = native_move(&fixture.source, &fixture.target);
    eprintln!(
        "ACL-denied MoveFileEx raw error: {:?}",
        result.as_ref().err().and_then(io::Error::raw_os_error)
    );
    let source = fs::read(&fixture.source).unwrap();
    let target = fs::read(&fixture.target).unwrap();
    for path in [&fixture.directory, &fixture.source, &fixture.target] {
        set_fixture_acl(path, "D:P(A;;FA;;;OW)");
    }
    assert!(
        result.is_err(),
        "denied DELETE unexpectedly replaced a file"
    );
    assert_eq!(source, b"complete new bytes");
    assert_eq!(target, b"complete old bytes");
    replace_file(&fixture.source, &fixture.target).unwrap();
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

#[test]
fn replacement_retry_wave_budget_is_shared_and_preserves_bytes_on_exhaustion() {
    let first = Fixture::new();
    let second = Fixture::new();
    let held_first = no_delete_share(&first.target);
    let held_second = no_delete_share(&second.target);
    let mut budget = ReplacementBudget::default();
    let mut waits = Vec::new();
    let mut attempts = 0;
    for fixture in [&first, &second] {
        let pin = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 4)
            .open(&fixture.source)
            .unwrap();
        let result = replace_with(
            &fixture.source,
            &fixture.target,
            &pin,
            &mut budget,
            validate_namespace,
            |source, target| {
                attempts += 1;
                assert_eq!(fs::read(source).unwrap(), b"complete new bytes");
                assert_eq!(fs::read(target).unwrap(), b"complete old bytes");
                native_move(source, target)
            },
            |delay| waits.push(delay.as_millis()),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    }
    drop((held_first, held_second));
    assert_eq!(attempts, 5);
    assert_eq!(waits, [25, 50, 100]);
}

#[test]
fn replacement_retry_pure_acl_denial_has_no_wait() {
    let fixture = Fixture::new();
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    set_fixture_acl(&fixture.target, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    set_fixture_acl(&fixture.directory, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    let mut attempts = 0;
    let mut waits = 0;
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        validate_namespace,
        |source, target| {
            attempts += 1;
            native_move(source, target)
        },
        |_| waits += 1,
    );
    set_fixture_acl(&fixture.target, "D:P(A;;FA;;;OW)");
    set_fixture_acl(&fixture.directory, "D:P(A;;FA;;;OW)");
    assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    assert_eq!((attempts, waits), (1, 0));
}

#[test]
fn replacement_retry_readonly_with_sharing_is_rejected_before_native_attempt() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.target);
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    let original_permissions = fs::metadata(&fixture.target).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&fixture.target, permissions).unwrap();
    let mut attempts = 0;
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        validate_namespace,
        |source, target| {
            attempts += 1;
            native_move(source, target)
        },
        |_| panic!("readonly target waited"),
    );
    fs::set_permissions(&fixture.target, original_permissions).unwrap();
    drop(held);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(attempts, 0);
}

#[test]
fn replacement_retry_validation_error_is_not_a_native_retry() {
    let fixture = Fixture::new();
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        |_, _, _| Err(io::Error::from_raw_os_error(32)),
        |_, _| panic!("protection failure attempted replacement"),
        |_| panic!("protection failure waited"),
    );
    assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
}

#[test]
fn replacement_retry_rechecks_protection_before_another_attempt() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.target);
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    let mut checks = 0;
    let mut attempts = 0;
    let mut waits = 0;
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        |source, target, pin| {
            checks += 1;
            if checks > 1 {
                return Err(io::Error::from_raw_os_error(32));
            }
            validate_namespace(source, target, pin)
        },
        |source, target| {
            attempts += 1;
            native_move(source, target)
        },
        |_| waits += 1,
    );
    drop(held);
    assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
    assert_eq!((checks, attempts, waits), (2, 1, 1));
}

#[test]
fn replacement_retry_mixed_acl_and_sharing_stays_bounded_and_does_not_change_acl() {
    let fixture = Fixture::new();
    let held = no_delete_share(&fixture.target);
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    set_fixture_acl(&fixture.source, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    set_fixture_acl(&fixture.directory, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    let mut attempts = 0;
    let mut waits = Vec::new();
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        validate_namespace,
        |source, target| {
            attempts += 1;
            native_move(source, target)
        },
        |delay| waits.push(delay.as_millis()),
    );
    drop(held);
    // Removing only sharing still leaves the native permission failure intact.
    let still_denied = native_move(&fixture.source, &fixture.target);
    set_fixture_acl(&fixture.source, "D:P(A;;FA;;;OW)");
    set_fixture_acl(&fixture.directory, "D:P(A;;FA;;;OW)");
    assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    assert_eq!(still_denied.unwrap_err().raw_os_error(), Some(5));
    assert_eq!(attempts, 4);
    assert_eq!(waits, [25, 50, 100]);
    assert_eq!(fs::read(&fixture.source).unwrap(), b"complete new bytes");
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete old bytes");
}

#[test]
fn replacement_retry_rejects_replaced_temporary_identity() {
    let fixture = Fixture::new();
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    fs::rename(&fixture.source, fixture.directory.join("original.new")).unwrap();
    fs::write(&fixture.source, b"foreign bytes").unwrap();
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        validate_namespace,
        |_, _| panic!("replaced temporary was moved"),
        |_| panic!("identity failure waited"),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete old bytes");
}

#[test]
fn replacement_retry_prepared_writer_denies_other_writers_and_allows_own_move() {
    use std::io::Write;
    let fixture = Fixture::new();
    fs::remove_file(&fixture.source).unwrap();
    let mut pin = create_temporary(&fixture.source).unwrap();
    pin.write_all(b"complete new bytes").unwrap();
    pin.sync_all().unwrap();
    let mut held = Some(no_delete_share(&fixture.target));
    let mut attempts = 0;
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        validate_namespace,
        |source, target| {
            attempts += 1;
            let writer = fs::OpenOptions::new()
                .write(true)
                .share_mode(1 | 2 | 4)
                .open(source);
            assert_eq!(writer.unwrap_err().raw_os_error(), Some(32));
            assert_eq!(fs::read(source).unwrap(), b"complete new bytes");
            native_move(source, target)
        },
        |_| {
            drop(held.take());
        },
    );
    drop(pin);
    assert!(
        result.is_ok(),
        "prepared writer blocked its own move: {result:?}"
    );
    assert_eq!(attempts, 2);
    assert!(!fixture.source.exists());
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete new bytes");
}

#[test]
fn replacement_retry_target_protection_lookup_failure_does_not_authorize_move() {
    let fixture = Fixture::new();
    let pin = fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(&fixture.source)
        .unwrap();
    let result = replace_with(
        &fixture.source,
        &fixture.target,
        &pin,
        &mut ReplacementBudget::default(),
        |source, target, pin| {
            validate_namespace(source, target, pin)?;
            validate_target(
                target,
                |_| Err(io::Error::from_raw_os_error(5)),
                |_, _| panic!("unknown leaf passed protection"),
            )
        },
        |_, _| panic!("unknown leaf attempted native move"),
        |_| panic!("unknown leaf waited"),
    );
    assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    assert_eq!(fs::read(&fixture.source).unwrap(), b"complete new bytes");
    assert_eq!(fs::read(&fixture.target).unwrap(), b"complete old bytes");
}
