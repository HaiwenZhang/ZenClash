//! Filesystem barriers exercise the same preparation and storage interfaces as callers.

use super::*;
use std::{
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        fs::{OpenOptionsExt, symlink},
    },
    time::{Duration, Instant},
};

#[test]
fn prepared_patch_uses_one_source_document_for_previous_and_next_payloads() {
    let root = test_root("single-source-document");
    fs::create_dir(&root).unwrap();
    let profile = root.join("source.yaml");
    let path = CString::new(profile.as_os_str().as_bytes()).unwrap();
    // SAFETY: the owned CString is NUL-terminated and remains alive during mkfifo.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let store = ControlledConfigStore::new(root.join("controlled"));
    let (completed, completion) = std::sync::mpsc::channel();
    let input = profile.clone();
    let worker = std::thread::spawn(move || {
        completed
            .send(store.prepare_json_update(input, &serde_json::json!({"mode":"direct"})))
            .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut writer = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&profile)
        {
            Ok(writer) => break writer,
            Err(error)
                if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
            {
                std::thread::yield_now()
            }
            Err(error) => panic!("source reader did not open the FIFO: {error}"),
        }
    };
    // The reader now owns the original FIFO descriptor. Its pathname is replaced
    // before any bytes are supplied, so a second open would read a different source.
    fs::rename(&profile, root.join("original-source.pipe")).unwrap();
    fs::write(&profile, "mode: global\ndns: {nameserver: [9.9.9.9]}\n").unwrap();
    writer
        .write_all(b"mode: rule\ndns: {nameserver: [1.1.1.1]}\n")
        .unwrap();
    drop(writer);
    let prepared = completion
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    let previous: serde_yaml::Value = serde_yaml::from_str(prepared.previous_payload()).unwrap();
    let next: serde_yaml::Value = serde_yaml::from_str(prepared.next_payload()).unwrap();
    assert_eq!(previous["dns"]["nameserver"][0], "1.1.1.1");
    assert_eq!(next["dns"]["nameserver"][0], "1.1.1.1");
    assert_eq!(previous["mode"], "rule");
    assert_eq!(next["mode"], "direct");
    assert!(fs::read_to_string(profile).unwrap().contains("9.9.9.9"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unreadable_controlled_patch_is_not_absent_or_quarantined_as_invalid_yaml() {
    let root = test_root("patch-symlink-loop");
    fs::create_dir(&root).unwrap();
    let store = ControlledConfigStore::new(&root);
    symlink("override.yaml", root.join("override.yaml")).unwrap();
    assert!(matches!(store.load(), Err(ControlledConfigError::Io(_))));
    assert!(matches!(
        store.quarantine_invalid_patch(),
        Err(ControlledConfigError::Io(_))
    ));
    assert!(
        fs::symlink_metadata(root.join("override.yaml"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unreadable_runtime_cache_is_not_overwritten_when_staging_a_candidate() {
    let root = test_root("cache-symlink-loop");
    fs::create_dir(&root).unwrap();
    let store = ControlledConfigStore::new(&root);
    symlink("effective.yaml", store.runtime_path()).unwrap();
    assert!(matches!(
        store.stage_runtime_payload("mode: direct\n"),
        Err(ControlledConfigError::Io(_))
    ));
    assert!(
        fs::symlink_metadata(store.runtime_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}
