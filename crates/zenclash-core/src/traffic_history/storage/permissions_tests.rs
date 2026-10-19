use super::*;

#[cfg(unix)]
#[test]
fn unix_permissions_fixture() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("ZENCLASH_HISTORY_PERMISSION_FIXTURE").is_none() {
        return;
    }
    // SAFETY: Only this dedicated test runs in the child process.
    unsafe {
        libc::umask(0o022);
    }
    let root =
        std::env::temp_dir().join(format!("zenclash-private-history-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let store = TrafficHistoryStore::new(root.join("history.sqlite3"));
    let connection = store.open().unwrap();
    connection
        .execute_batch("CREATE TABLE retained(value TEXT); INSERT INTO retained VALUES ('kept');")
        .unwrap();
    let paths = [
        store.path().to_path_buf(),
        root.join("history.sqlite3-wal"),
        root.join("history.sqlite3-shm"),
    ];
    for path in &paths {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{}",
            path.display()
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let reopened = store.open().unwrap();
    for path in &paths {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{}",
            path.display()
        );
    }
    assert_eq!(
        reopened
            .query_row("SELECT value FROM retained", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    drop(reopened);
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn database_and_live_sidecars_are_private_on_creation_and_after_migration() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "traffic_history::storage::permissions_tests::unix_permissions_fixture",
            "--nocapture",
        ])
        .env("ZENCLASH_HISTORY_PERMISSION_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[cfg(unix)]
#[test]
fn linked_history_files_are_rejected_without_changing_the_link_target() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = std::env::temp_dir().join(format!(
        "zenclash-linked-history-{}-{}",
        std::process::id(),
        unix_millis()
    ));
    fs::create_dir(&root).unwrap();
    let target = root.join("target");
    fs::write(&target, b"untouched").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let path = root.join("history.sqlite3");
    symlink(&target, &path).unwrap();
    assert!(private_io::prepare(&path).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn existing_history_files_receive_private_acls_without_losing_bytes() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-private-history-{}-{}",
        std::process::id(),
        unix_millis()
    ));
    fs::create_dir(&root).unwrap();
    let database = root.join("history.sqlite3");
    let wal = root.join("history.sqlite3-wal");
    let shm = root.join("history.sqlite3-shm");
    for path in [&database, &wal, &shm] {
        use std::{io::Write, os::windows::io::AsRawHandle};
        use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
        };
        let mut file = crate::service::create_private_current_user_file(path).unwrap();
        file.write_all(b"retained").unwrap();
        // SAFETY: Only these owned, ordinary fixture files receive a permissive legacy DACL.
        let result = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0);
    }
    private_io::prepare(&database).unwrap();
    for path in [&database, &wal, &shm] {
        drop(crate::service::open_private_current_user_file(path).unwrap());
        assert_eq!(fs::read(path).unwrap(), b"retained");
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn sqlite_creates_live_sidecars_with_private_acls() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-live-private-history-{}-{}",
        std::process::id(),
        unix_millis()
    ));
    fs::create_dir(&root).unwrap();
    let store = TrafficHistoryStore::new(root.join("history.sqlite3"));
    let connection = store.open().unwrap();
    for path in [
        store.path().to_path_buf(),
        root.join("history.sqlite3-wal"),
        root.join("history.sqlite3-shm"),
    ] {
        drop(crate::service::open_private_current_user_file(&path).unwrap());
    }
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}
