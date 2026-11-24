//! Ordinary-executable evidence for startup before validation can execute it.

pub use crate::runtime::local::installation::verify_ordinary_local_executable;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "zenclash-ordinary-executable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"not executed").unwrap();
        path
    }

    #[test]
    fn ordinary_executable_rejects_unreadable_source() {
        let path = fixture();
        std::fs::remove_file(&path).unwrap();
        assert!(verify_ordinary_local_executable(&path).is_err());
    }

    #[test]
    fn ordinary_executable_accepts_regular_unprivileged_file_without_execution() {
        let path = fixture();
        verify_ordinary_local_executable(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"not executed");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_executable_rejects_user_owned_setid_attributes_without_execution() {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture();
        for mode in [0o4700, 0o2700] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(verify_ordinary_local_executable(&path).is_err());
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"not executed");
        std::fs::remove_file(path).unwrap();
    }
}
