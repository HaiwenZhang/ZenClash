use super::*;
use crate::core_update::transaction::PreparedCoreUpdate;

fn root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "zenclash-update-recovery-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn target(root: &Path) -> PathBuf {
    root.join(if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    })
}

fn prepare(root: &Path) -> PreparedCoreUpdate {
    let target = target(root);
    fs::write(&target, b"old verified executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let staging = sibling_path(&target, "staging").unwrap();
    fs::write(&staging, b"new candidate executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    }
    PreparedCoreUpdate {
        staging: Some(staging),
        target,
        tag: "fixture".into(),
    }
}

#[test]
fn crash_fixture() {
    let Some(root) = std::env::var_os("ZENCLASH_CORE_UPDATE_FIXTURE_ROOT") else {
        return;
    };
    let prepared = prepare(Path::new(&root));
    let transaction = prepared.activate().unwrap();
    match std::env::var("ZENCLASH_CORE_UPDATE_FIXTURE_ACTION")
        .unwrap()
        .as_str()
    {
        "commit" => transaction.commit().unwrap(),
        "rollback" => transaction.rollback().unwrap(),
        action => panic!("unknown fixture action: {action}"),
    }
    panic!("fixture did not reach its crash step");
}

fn crash(root: &Path, step: &str, action: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core_update::recovery::tests::crash_fixture",
            "--nocapture",
        ])
        .env("ZENCLASH_CORE_UPDATE_FIXTURE_ROOT", root)
        .env("ZENCLASH_CORE_UPDATE_CRASH_STEP", step)
        .env("ZENCLASH_CORE_UPDATE_FIXTURE_ACTION", action)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn startup(root: &Path, binary: &Path) -> crate::MihomoResult<crate::CoreLaunchConfig> {
    let profile = root.join("profile.yaml");
    fs::write(&profile, "rules: [MATCH,DIRECT]\n").unwrap();
    crate::CoreLaunchConfig::discover_for_kind_with_binary_and_config(
        root,
        crate::CoreKind::Mihomo,
        Some(binary),
        Some(&profile),
    )
}

#[test]
fn startup_recovers_old_custom_core_after_every_uncommitted_replacement_and_rollback_step() {
    for step in [
        "recorded",
        "preserved",
        "installed",
        "removed-candidate",
        "restored",
    ] {
        let root = root(step);
        crash(&root, step, "rollback");
        startup(&root, &target(&root)).unwrap();
        assert_eq!(
            fs::read(target(&root)).unwrap(),
            b"old verified executable",
            "{step}"
        );
        assert!(!recover_interrupted_update(&target(&root), None).unwrap());
        assert!(
            !fs::read_dir(&root).unwrap().flatten().any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.contains("zenclash-") && !name.ends_with(".lock")
            }),
            "recovery left owned executable files behind: {step}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn committed_update_survives_exit_before_or_during_backup_cleanup() {
    for step in ["committed", "backup-cleaned"] {
        let root = root(step);
        crash(&root, step, "commit");
        startup(&root, &target(&root)).unwrap();
        assert_eq!(
            fs::read(target(&root)).unwrap(),
            b"new candidate executable"
        );
        assert!(!recover_interrupted_update(&target(&root), None).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn startup_recovers_a_custom_symlink_whose_target_is_temporarily_absent() {
    let root = root("symlink");
    let alias = root.join("selected-core");
    std::os::unix::fs::symlink(target(&root), &alias).unwrap();
    crash(&root, "preserved", "rollback");
    assert!(!alias.exists());
    startup(&root, &alias).unwrap();
    assert_eq!(fs::read(&alias).unwrap(), b"old verified executable");
    assert!(
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_cannot_roll_back_an_update_that_still_holds_its_file_lock() {
    let root = root("live-update");
    let transaction = prepare(&root).activate().unwrap();
    assert!(recover_interrupted_update(&target(&root), None).is_err());
    assert_eq!(
        fs::read(target(&root)).unwrap(),
        b"new candidate executable"
    );
    drop(transaction);
    assert_eq!(fs::read(target(&root)).unwrap(), b"old verified executable");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_record_failure_does_not_replace_a_potentially_running_candidate_on_drop() {
    let root = root("commit-record-failure");
    let transaction = prepare(&root).activate().unwrap();
    let actual = resolve_target(&target(&root)).unwrap();
    let journal = UpdateJournal::read(&actual).unwrap().unwrap();
    let backup = journal.sibling(&actual, &journal.backup).unwrap();
    let record = journal_path(&actual).unwrap();
    fs::remove_file(&record).unwrap();
    fs::create_dir(&record).unwrap();
    assert!(transaction.commit().is_err());
    assert_eq!(fs::read(&actual).unwrap(), b"new candidate executable");
    assert_eq!(fs::read(backup).unwrap(), b"old verified executable");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsafe_journal_paths_are_rejected_before_changing_any_executable() {
    let root = root("unsafe-journal");
    let prepared = prepare(&root);
    let target = resolve_target(&target(&root)).unwrap();
    let foreign = root.join("outside");
    fs::write(&foreign, b"untouched").unwrap();
    let record = journal_path(&target).unwrap();
    for backup in [
        "../outside".to_owned(),
        name(&record).unwrap(),
        name(&record_path(&target, "lock").unwrap()).unwrap(),
        name(&sibling_path(&target, "rejected").unwrap()).unwrap(),
    ] {
        fs::write(&record, serde_json::to_vec(&serde_json::json!({
            "version": 1, "backup": backup, "staging": name(prepared.staging.as_ref().unwrap()).unwrap(),
            "rejected": name(&sibling_path(&target, "rejected").unwrap()).unwrap(), "phase": "Installing"
        })).unwrap()).unwrap();
        assert!(recover_interrupted_update(&target, None).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old verified executable");
        assert_eq!(fs::read(&foreign).unwrap(), b"untouched");
    }
    drop(prepared);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_original_backup_and_manual_recovery_never_accept_an_uncommitted_candidate() {
    let root = root("missing-backup");
    let transaction = prepare(&root).activate().unwrap();
    let actual = resolve_target(&target(&root)).unwrap();
    let journal = UpdateJournal::read(&actual).unwrap().unwrap();
    fs::remove_file(journal.sibling(&actual, &journal.backup).unwrap()).unwrap();
    drop(transaction);
    assert!(recover_interrupted_update(&target(&root), None).is_err());
    assert_eq!(
        fs::read(target(&root)).unwrap(),
        b"new candidate executable"
    );
    fs::remove_dir_all(&root).unwrap();

    let root = self::root("manual");
    let backup = prepare(&root)
        .activate()
        .unwrap()
        .preserve_for_manual_recovery()
        .unwrap();
    assert!(recover_interrupted_update(&target(&root), None).is_err());
    assert_eq!(fs::read(backup).unwrap(), b"old verified executable");
    fs::remove_dir_all(root).unwrap();
}
