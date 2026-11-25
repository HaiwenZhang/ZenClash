use super::*;

#[test]
fn explicit_profile_and_home_do_not_inspect_lower_priority_overrides() {
    let paths = CoreRuntimePaths::resolve(
        &WorkspaceResources::new("workspace"),
        CoreKind::Mihomo,
        Some(Path::new("supplied.yaml")),
        None,
        None,
        |name| {
            assert_eq!(name, "ZENCLASH_CORE_HOME");
            Some("explicit-home".into())
        },
    );
    assert_eq!(paths.config_file, Path::new("supplied.yaml"));
    assert_eq!(paths.home_dir, Path::new("explicit-home"));
}

#[test]
fn configured_profile_and_kind_home_override_workspace_fallback() {
    let paths = CoreRuntimePaths::resolve(
        &WorkspaceResources::new("workspace"),
        CoreKind::Mihomo,
        None,
        None,
        None,
        |name| match name {
            "ZENCLASH_CONFIG" => Some("environment.yaml".into()),
            "ZENCLASH_CORE_HOME" => None,
            "ZENCLASH_MIHOMO_HOME" => Some("mihomo-home".into()),
            _ => panic!("unexpected environment lookup: {name}"),
        },
    );
    assert_eq!(paths.config_file, Path::new("environment.yaml"));
    assert_eq!(paths.home_dir, Path::new("mihomo-home"));
}

#[test]
fn packaged_home_needs_both_resource_directory_and_compatible_data_root() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-runtime-location-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let package = root.join("resources");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(package.join("profile.yaml"), "mode: rule\n").unwrap();
    let resources = BundledResources {
        candidates: vec![package.clone()],
    };
    let data = AppDataPaths::new(root.join("application-data"));
    let workspace = WorkspaceResources::new(&root);
    let packaged = CoreRuntimePaths::resolve(
        &workspace,
        CoreKind::Mihomo,
        None,
        Some(&resources),
        Some(&data),
        |_| None,
    );
    let without_data = CoreRuntimePaths::resolve(
        &workspace,
        CoreKind::Mihomo,
        None,
        Some(&resources),
        None,
        |_| None,
    );
    let without_bundle = CoreRuntimePaths::resolve(
        &workspace,
        CoreKind::Mihomo,
        None,
        None,
        Some(&data),
        |_| None,
    );
    assert_eq!(packaged.config_file, package.join("profile.yaml"));
    assert_eq!(packaged.home_dir, data.core_home(CoreKind::Mihomo));
    assert_eq!(without_data.home_dir, workspace.core_home(CoreKind::Mihomo));
    assert_eq!(
        without_bundle.home_dir,
        workspace.core_home(CoreKind::Mihomo)
    );
    assert_eq!(without_bundle.config_file, workspace.default_profile());
    std::fs::remove_dir_all(root).unwrap();
}
