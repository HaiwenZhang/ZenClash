//! Ordinary isolated children for UI binding ownership tests.
use super::RuntimePageFixture as Fixture;
use crate::pages::runtime::*;
use std::fs;
use zenclash_core::ManagedCoreProcess;
// Ordinary owned children exercise UI binding behavior, not Mihomo or TUN acceptance.
pub(in crate::pages::runtime) fn owned_ui_children(
    fixture: &Fixture,
) -> [Arc<ManagedCoreProcess>; 2] {
    let source = fixture.root.join("ui-owned-child.rs");
    fs::write(
        &source,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(30)); }",
    )
    .unwrap();
    let binary = fixture.root.join(if cfg!(windows) {
        "ui-owned-child.exe"
    } else {
        "ui-owned-child"
    });
    let compilation = std::process::Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "ui_owned_child"])
        .arg(source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    std::array::from_fn(|index| {
        let directory = fixture.root.join(format!("owned-{index}"));
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(binary.file_name().unwrap());
        // Retain distinct owner paths without opening the executable for writing
        // immediately before exec on Linux (which can fail with ETXTBSY).
        #[cfg(target_os = "linux")]
        fs::hard_link(&binary, &executable).unwrap();
        #[cfg(not(target_os = "linux"))]
        fs::copy(&binary, &executable).unwrap();
        let config = directory.join("profile.yaml");
        fs::write(&config, "rules: [MATCH,DIRECT]\n").unwrap();
        ManagedCoreProcess::spawn_isolated_for_test(CoreLaunchConfig {
            kind: CoreKind::Mihomo,
            binary: executable,
            config_file: config,
            home_dir: directory.join("home"),
            endpoint: fixture.core.client().endpoint().unwrap(),
            controller_override: None,
        })
        .unwrap()
    })
}
