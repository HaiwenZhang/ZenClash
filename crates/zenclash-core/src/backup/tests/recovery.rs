use std::process::Command;

use super::*;

const FIXTURE_ROOT: &str = "ZENCLASH_BACKUP_CRASH_ROOT";

#[test]
fn crash_fixture() {
    let Some(root) = std::env::var_os(FIXTURE_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let manager = BackupManager::new(root.join("target"));
    if std::env::var_os("ZENCLASH_BACKUP_RECOVER").is_some() {
        manager.recover_interrupted_restore().unwrap();
    } else {
        let transaction = manager
            .prepare_restore(root.join("backup.zip"))
            .unwrap()
            .activate()
            .unwrap();
        let phase = std::env::var("ZENCLASH_BACKUP_CRASH_STEP").unwrap();
        if phase == "active:all" {
            std::process::exit(86);
        }
        if phase == "committed:all" {
            transaction.commit().unwrap();
        } else {
            transaction.rollback().unwrap();
        }
    }
    panic!("crash checkpoint was not reached");
}

fn crash(root: &Path, phase: &str, recover: bool) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "backup::tests::recovery::crash_fixture",
            "--nocapture",
        ])
        .env(FIXTURE_ROOT, root)
        .env("ZENCLASH_BACKUP_CRASH_STEP", phase);
    if recover {
        command.env("ZENCLASH_BACKUP_RECOVER", "1");
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "{phase}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restart_recovers_every_interrupted_install_and_rollback_boundary() {
    let mut checkpoints = vec!["active:all".to_owned(), "committed:all".to_owned()];
    for phase in [
        "before-preserve",
        "preserved",
        "installed",
        "removed",
        "restored",
    ] {
        for item in transaction::LIVE_ITEMS {
            checkpoints.push(format!("{phase}:{item}"));
        }
    }
    for checkpoint in checkpoints {
        let root = test_root("restore-crash");
        let source = root.join("source");
        let target = root.join("target");
        create_snapshot(&source, AppearancePreference::Light, false, 17890);
        create_snapshot(&target, AppearancePreference::Dark, true, 17891);
        let expected = read_authoritative_snapshot(if checkpoint == "committed:all" {
            &source
        } else {
            &target
        });
        BackupManager::new(&source)
            .export_to(root.join("backup.zip"))
            .unwrap();
        crash(&root, &checkpoint, false);
        let manager = BackupManager::new(&target);
        assert!(
            manager.recover_interrupted_restore().unwrap(),
            "{checkpoint}"
        );
        assert_eq!(
            read_authoritative_snapshot(&target),
            expected,
            "{checkpoint}"
        );
        assert!(!manager.recover_interrupted_restore().unwrap());
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".zenclash-backup-")
        }));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recovery_survives_another_exit_after_restoring_an_original_item() {
    let root = test_root("recovery-crash");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    let expected = read_authoritative_snapshot(&target);
    BackupManager::new(&source)
        .export_to(root.join("backup.zip"))
        .unwrap();
    crash(&root, "installed:profiles", false);
    crash(&root, "restored:profiles", true);
    BackupManager::new(&target)
        .recover_interrupted_restore()
        .unwrap();
    assert_eq!(read_authoritative_snapshot(&target), expected);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_restore_into_a_new_root_restores_its_original_absence() {
    for checkpoint in [
        "preserved:profiles",
        "installed:profiles",
        "removed:profiles",
    ] {
        let root = test_root("new-root-crash");
        let source = root.join("source");
        let target = root.join("target");
        create_snapshot(&source, AppearancePreference::Light, false, 17890);
        BackupManager::new(source)
            .export_to(root.join("backup.zip"))
            .unwrap();
        crash(&root, checkpoint, false);
        BackupManager::new(&target)
            .recover_interrupted_restore()
            .unwrap();
        assert!(!target.exists(), "{checkpoint}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recovery_rejects_journal_paths_that_escape_the_transaction_directories() {
    let root = test_root("invalid-recovery-journal");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    BackupManager::new(source)
        .export_to(root.join("backup.zip"))
        .unwrap();
    crash(&root, "active:all", false);
    let expected = read_authoritative_snapshot(&target);
    let journal = target.join(".backup-restore.json");
    let mut payload: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    payload["rollback_name"] = "../unrelated".into();
    fs::write(journal, serde_json::to_vec(&payload).unwrap()).unwrap();
    assert!(
        BackupManager::new(&target)
            .recover_interrupted_restore()
            .is_err()
    );
    assert_eq!(read_authoritative_snapshot(&target), expected);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn failed_commit_keeps_the_transaction_available_for_disk_and_runtime_rollback() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = test_root("failed-restore-commit");
    let source = root.join("source");
    let target = root.join("target");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    create_snapshot(&target, AppearancePreference::Dark, true, 17891);
    let expected = read_authoritative_snapshot(&target);
    BackupManager::new(source)
        .export_to(root.join("backup.zip"))
        .unwrap();
    let mut transaction = BackupManager::new(&target)
        .prepare_restore(root.join("backup.zip"))
        .unwrap()
        .activate()
        .unwrap();
    let permissions = fs::metadata(&target).unwrap().permissions();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
    let result = transaction.commit_in_place();
    fs::set_permissions(&target, permissions).unwrap();
    assert!(result.is_err());
    assert!(transaction.active);
    transaction.rollback_in_place().unwrap();
    assert_eq!(read_authoritative_snapshot(&target), expected);
    drop(transaction);
    assert!(
        !BackupManager::new(&target)
            .recover_interrupted_restore()
            .unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn restore_permissions_fixture() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(root) = std::env::var_os("ZENCLASH_BACKUP_PERMISSION_ROOT") else {
        return;
    };
    // SAFETY: Only this isolated fixture process changes its umask.
    unsafe {
        libc::umask(0o022);
    }
    let root = PathBuf::from(root);
    let target = root.join("target");
    let prepared = BackupManager::new(&target)
        .prepare_restore(root.join("backup.zip"))
        .unwrap();
    assert_eq!(
        fs::metadata(&prepared.staging_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for (relative, _) in read_authoritative_snapshot(&prepared.staging_root) {
        let path = if let Some(name) = relative.strip_prefix("override:") {
            prepared
                .staging_root
                .join("yaml-overrides/files")
                .join(name)
        } else if relative.ends_with(".yaml") && !relative.contains('/') {
            prepared.staging_root.join("profiles/files").join(relative)
        } else {
            prepared.staging_root.join(relative)
        };
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{}",
            path.display()
        );
    }
    let transaction = prepared.activate().unwrap();
    assert_eq!(
        fs::metadata(&transaction.rollback_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    transaction.commit().unwrap();
    assert_eq!(
        fs::metadata(target.join(PROFILE_INDEX_PATH))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn restored_secrets_remain_private_under_a_permissive_umask() {
    let root = test_root("restore-permissions");
    let source = root.join("source");
    create_snapshot(&source, AppearancePreference::Light, false, 17890);
    BackupManager::new(source)
        .export_to(root.join("backup.zip"))
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "backup::tests::recovery::restore_permissions_fixture",
            "--nocapture",
        ])
        .env("ZENCLASH_BACKUP_PERMISSION_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
