use super::*;

#[test]
fn workspace_layout_preserves_profile_recovery_and_working_directory_paths() {
    let resources = WorkspaceResources::new("workspace");
    assert_eq!(
        resources.default_profile(),
        Path::new("workspace/platforms/common/default.yaml")
    );
    assert_eq!(
        resources.recovery_profile(),
        Path::new("workspace/platforms/common/recovery.yaml")
    );
    assert_eq!(
        resources.core_home(CoreKind::Mihomo),
        Path::new("workspace/target/zenclash-mihomo")
    );
}

#[test]
fn executable_layout_keeps_platform_suffix_and_single_workspace_candidate() {
    for (platform, name) in [
        (ApplicationPlatform::Macos, "mihomo"),
        (ApplicationPlatform::Windows, "mihomo.exe"),
        (ApplicationPlatform::Unix, "mihomo"),
    ] {
        assert_eq!(
            executable_filename_for_platform(CoreKind::Mihomo, platform),
            name
        );
    }
    let resources = WorkspaceResources::new("workspace");
    assert_eq!(
        resources.core_binary(CoreKind::Mihomo),
        resources
            .root()
            .join("bin")
            .join(core_executable_filename(CoreKind::Mihomo))
    );
}
