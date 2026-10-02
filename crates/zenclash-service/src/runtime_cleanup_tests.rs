//! Ordinary Windows files exercise native sharing errors, not administrator ACLs.

use std::{cell::Cell, fs, io, os::windows::fs::OpenOptionsExt};

use super::*;

fn held_file(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(path)
        .unwrap()
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
    // SAFETY: inputs are terminated strings and output storage is valid.
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
    // SAFETY: descriptor is valid until LocalFree; the fixture path is terminated.
    let result = unsafe {
        SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: the descriptor was allocated by the conversion API.
    unsafe { LocalFree(descriptor) };
    assert_ne!(result, 0, "cannot set owned fixture ACL: {error}");
}

#[test]
fn runtime_cleanup_rechecks_protection_then_deletes_after_handle_release() {
    let root = OwnedTestRoot::create().unwrap();
    let directory = root.path().join("retired");
    root.create_directory(&directory).unwrap();
    let path = directory.join("resource");
    fs::write(&path, b"retired resource").unwrap();
    let held = std::cell::RefCell::new(Some(held_file(&path)));
    assert_eq!(fs::remove_file(&path).unwrap_err().raw_os_error(), Some(32));
    let validations = Cell::new(0);
    let result = remove_runtime_with(&directory, 0, &mut 0, &|candidate, is_dir| {
        root.validate(candidate, is_dir)?;
        if candidate == path {
            validations.set(validations.get() + 1);
            if validations.get() == 2 {
                held.borrow_mut().take();
            }
        }
        Ok(())
    });
    held.borrow_mut().take();
    if result.is_err() {
        root.remove(&directory).unwrap();
    }
    fs::remove_dir(root.path()).unwrap();
    assert!(
        result.is_ok(),
        "cleanup did not wait for the actual held file: {result:?}"
    );
    assert_eq!(validations.get(), 2);
}

#[test]
fn runtime_cleanup_exhausts_three_waits_and_preserves_the_held_resource() {
    let root = OwnedTestRoot::create().unwrap();
    let directory = root.path().join("retired");
    root.create_directory(&directory).unwrap();
    let path = directory.join("resource");
    fs::write(&path, b"retired resource").unwrap();
    let held = held_file(&path);
    let validations = Cell::new(0);
    let result = remove_runtime_with(&directory, 0, &mut 0, &|candidate, is_dir| {
        root.validate(candidate, is_dir)?;
        if candidate == path {
            validations.set(validations.get() + 1);
        }
        Ok(())
    });
    let retained = fs::read(&path).unwrap();
    drop(held);
    root.remove(&directory).unwrap();
    fs::remove_dir(root.path()).unwrap();
    assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
    assert_eq!(retained, b"retired resource");
    assert_eq!(
        validations.get(),
        4,
        "three bounded retries were not attempted"
    );
}

#[test]
fn runtime_cleanup_never_retries_a_protection_error_with_a_transient_native_code() {
    let root = OwnedTestRoot::create().unwrap();
    let directory = root.path().join("retired");
    root.create_directory(&directory).unwrap();
    let path = directory.join("resource");
    fs::write(&path, b"protected resource").unwrap();
    let validations = Cell::new(0);
    let result = remove_runtime_with(&directory, 0, &mut 0, &|candidate, is_dir| {
        root.validate(candidate, is_dir)?;
        if candidate == path {
            validations.set(validations.get() + 1);
            return Err(io::Error::from_raw_os_error(32));
        }
        Ok(())
    });
    let retained = fs::read(&path).unwrap();
    root.remove(&directory).unwrap();
    fs::remove_dir(root.path()).unwrap();
    assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
    assert_eq!(validations.get(), 1);
    assert_eq!(retained, b"protected resource");
}

#[test]
fn runtime_cleanup_shares_three_waits_across_roots_without_recounting_entries() {
    let root = OwnedTestRoot::create().unwrap();
    let mut budget = RuntimeCleanupBudget::default();
    let mut count = 0;
    for (index, release_at) in [3, 2, usize::MAX].into_iter().enumerate() {
        let directory = root.path().join(format!("retired-{index}"));
        root.create_directory(&directory).unwrap();
        let path = directory.join("resource");
        fs::write(&path, b"retired resource").unwrap();
        let held = std::cell::RefCell::new(Some(held_file(&path)));
        let validations = Cell::new(0);
        let result = remove_runtime_with_budget(
            &directory,
            0,
            &mut count,
            &|candidate, is_dir| {
                root.validate(candidate, is_dir)?;
                if candidate == path {
                    validations.set(validations.get() + 1);
                    if validations.get() == release_at {
                        held.borrow_mut().take();
                    }
                }
                Ok(())
            },
            &mut budget,
        );
        held.borrow_mut().take();
        if index < 2 {
            assert!(
                result.is_ok(),
                "retired root {index} was not deleted: {result:?}"
            );
            assert!(!directory.exists());
            assert_eq!(validations.get(), release_at);
        } else {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
            assert_eq!(
                validations.get(),
                1,
                "the third root received a new allowance"
            );
            assert_eq!(fs::read(&path).unwrap(), b"retired resource");
            root.remove(&directory).unwrap();
        }
        assert_eq!(count, index + 1, "native retries re-enumerated a leaf");
    }
    fs::remove_dir(root.path()).unwrap();
}

#[test]
fn runtime_cleanup_native_access_denied_is_not_retried() {
    let root = OwnedTestRoot::create().unwrap();
    let directory = root.path().join("retired");
    root.create_directory(&directory).unwrap();
    let path = directory.join("resource");
    fs::write(&path, b"read-only resource").unwrap();
    // Rust's Windows deletion can clear the readonly attribute. Deny DELETE on
    // the leaf and FILE_DELETE_CHILD on its parent to test actual native denial.
    for target in [&directory, &path] {
        set_fixture_acl(target, "D:P(D;;0x10040;;;WD)(A;;FA;;;OW)");
    }
    let validations = Cell::new(0);
    let result = remove_runtime_with(&directory, 0, &mut 0, &|candidate, is_dir| {
        root.validate(candidate, is_dir)?;
        if candidate == path {
            validations.set(validations.get() + 1);
        }
        Ok(())
    });
    let retained = fs::read(&path).unwrap();
    for target in [&directory, &path] {
        set_fixture_acl(target, "D:P(A;;FA;;;OW)");
    }
    root.remove(&directory).unwrap();
    fs::remove_dir(root.path()).unwrap();
    assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
    assert_eq!(validations.get(), 1);
    assert_eq!(retained, b"read-only resource");
}

#[test]
fn runtime_cleanup_changed_protection_after_wait_stops_without_another_delete() {
    let root = OwnedTestRoot::create().unwrap();
    let directory = root.path().join("retired");
    root.create_directory(&directory).unwrap();
    let path = directory.join("resource");
    fs::write(&path, b"retired resource").unwrap();
    let held = held_file(&path);
    let validations = Cell::new(0);
    let result = remove_runtime_with(&directory, 0, &mut 0, &|candidate, is_dir| {
        root.validate(candidate, is_dir)?;
        if candidate == path {
            validations.set(validations.get() + 1);
            if validations.get() == 2 {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        Ok(())
    });
    let retained = fs::read(&path).unwrap();
    drop(held);
    root.remove(&directory).unwrap();
    fs::remove_dir(root.path()).unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(validations.get(), 2);
    assert_eq!(retained, b"retired resource");
}
