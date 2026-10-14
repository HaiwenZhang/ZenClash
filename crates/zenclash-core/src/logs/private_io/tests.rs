use super::*;
use std::{
    fs,
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn temporary_directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "zenclash-private-log-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn replacement_publishes_a_complete_new_file_without_truncating_the_open_old_file() {
    let root = temporary_directory();
    let path = root.join("raw.log");
    append(&path, b"old complete payload").unwrap();
    let mut original = File::open(&path).unwrap();
    replace(&path, b"new complete payload").unwrap();
    let mut prior = Vec::new();
    original.read_to_end(&mut prior).unwrap();
    assert_eq!(prior, b"old complete payload");
    assert_eq!(fs::read(&path).unwrap(), b"new complete payload");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    drop(original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_replacement_leaves_existing_destination_and_removes_the_private_temporary() {
    let root = temporary_directory();
    let destination = root.join("directory.log");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("kept"), b"original").unwrap();
    assert!(replace(&destination, b"unaccepted new log").is_err());
    assert_eq!(fs::read(destination.join("kept")).unwrap(), b"original");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_to_a_missing_parent_returns_error_without_creating_directories() {
    let root = temporary_directory();
    let path = root.join("missing/export.log");
    assert_eq!(
        crate::write_log_export(&path, "raw").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn raw_log_export_permissions_fixture_child() {
    use std::os::unix::fs::PermissionsExt;
    if std::env::var_os("ZENCLASH_LOG_EXPORT_PERMISSION_CHILD").is_none() {
        return;
    }
    // SAFETY: Only this dedicated fixture process has its umask changed.
    unsafe {
        libc::umask(0o022);
    }
    let root = temporary_directory();
    let path = root.join("export.log");
    crate::write_log_export(&path, "old exported payload").unwrap();
    let first = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    crate::write_log_export(&path, "new exported payload").unwrap();
    let second = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let content = fs::read_to_string(&path).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_eq!((first, second), (0o600, 0o600));
    assert_eq!(content, "new exported payload");
}

#[cfg(unix)]
#[test]
fn raw_log_export_uses_private_permissions_under_permissive_umask() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "logs::private_io::tests::raw_log_export_permissions_fixture_child",
            "--nocapture",
        ])
        .env("ZENCLASH_LOG_EXPORT_PERMISSION_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
