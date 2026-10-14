//! Exclusive temporary homes shared by service-runtime resource tests.
use std::path::PathBuf;
pub(in crate::service_runtime) struct TestHome(pub(in crate::service_runtime) PathBuf);
impl TestHome {
    pub(in crate::service_runtime) fn new() -> Self {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "zenclash-service-bundle-{:x}",
            u128::from_ne_bytes(random)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}
impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
