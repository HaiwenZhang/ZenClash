use super::*;
use std::{
    collections::BTreeMap,
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

fn environment(values: &[(&str, &str)]) -> impl FnMut(&str) -> Option<OsString> {
    let values: BTreeMap<String, OsString> = values
        .iter()
        .map(|(name, value)| (name.to_string(), OsString::from(value)))
        .collect();
    move |name| values.get(name).cloned()
}

#[test]
fn platform_data_variables_do_not_read_home_fallback() {
    for (platform, primary, expected) in [
        (
            ApplicationPlatform::Windows,
            "LOCALAPPDATA",
            "/data/ZenClash",
        ),
        (ApplicationPlatform::Unix, "XDG_DATA_HOME", "/data/zenclash"),
    ] {
        for compatibility in [
            HomeCompatibility::PersistentStores,
            HomeCompatibility::ManagedCore,
        ] {
            let root = resolve_data_root(platform, compatibility, |name| {
                assert_eq!(
                    name, primary,
                    "provided platform root must not inspect HOME"
                );
                Some(OsString::from("/data"))
            });
            assert_eq!(root, Some(PathBuf::from(expected)));
        }
    }
    let config = resolve_user_configuration_directory(|name| {
        assert_eq!(name, "XDG_CONFIG_HOME");
        Some(OsString::from("/config"))
    });
    assert_eq!(config, Some(PathBuf::from("/config")));
}

#[test]
fn persistent_store_home_priority_and_platform_suffixes_remain_compatible() {
    for (platform, expected) in [
        (
            ApplicationPlatform::Macos,
            "/home/Library/Application Support/ZenClash",
        ),
        (ApplicationPlatform::Windows, "/home/AppData/Local/ZenClash"),
        (ApplicationPlatform::Unix, "/home/.local/share/zenclash"),
    ] {
        assert_eq!(
            resolve_data_root(
                platform,
                HomeCompatibility::PersistentStores,
                environment(&[("HOME", "/home"), ("USERPROFILE", "/profile")])
            ),
            Some(PathBuf::from(expected))
        );
    }
    assert_eq!(
        resolve_data_root(
            ApplicationPlatform::Windows,
            HomeCompatibility::PersistentStores,
            environment(&[("USERPROFILE", "/profile")])
        ),
        Some(PathBuf::from("/profile/AppData/Local/ZenClash"))
    );
    assert_eq!(
        resolve_user_configuration_directory(environment(&[("HOME", "/home")])),
        Some(PathBuf::from("/home/.config"))
    );
}

#[test]
fn managed_core_home_compatibility_does_not_migrate_existing_working_directories() {
    assert_eq!(
        resolve_data_root(
            ApplicationPlatform::Windows,
            HomeCompatibility::ManagedCore,
            environment(&[("HOME", "/different"), ("USERPROFILE", "/profile")])
        ),
        Some(PathBuf::from("/profile/AppData/Local/ZenClash"))
    );
    assert_eq!(
        resolve_data_root(
            ApplicationPlatform::Windows,
            HomeCompatibility::ManagedCore,
            environment(&[("HOME", "/home")])
        ),
        None
    );
    assert_eq!(
        resolve_data_root(
            ApplicationPlatform::Unix,
            HomeCompatibility::ManagedCore,
            environment(&[("USERPROFILE", "/profile")])
        ),
        None
    );
    for platform in [
        ApplicationPlatform::Macos,
        ApplicationPlatform::Windows,
        ApplicationPlatform::Unix,
    ] {
        assert_eq!(
            resolve_data_root(
                platform,
                HomeCompatibility::PersistentStores,
                environment(&[])
            ),
            None
        );
    }
}

#[test]
fn explicit_application_root_derives_existing_store_and_runtime_locations() {
    let paths = AppDataPaths::new("/chosen");
    assert_eq!(paths.root(), Path::new("/chosen"));
    assert_eq!(paths.profiles(), PathBuf::from("/chosen/profiles"));
    assert_eq!(
        paths.preferences(),
        PathBuf::from("/chosen/preferences.json")
    );
    assert_eq!(
        paths.controlled_config(),
        PathBuf::from("/chosen/controlled-config")
    );
    assert_eq!(
        paths.yaml_overrides(),
        PathBuf::from("/chosen/yaml-overrides")
    );
    assert_eq!(
        paths.core_home(CoreKind::Mihomo),
        PathBuf::from("/chosen/mihomo")
    );
}

#[test]
fn package_layouts_preserve_their_resource_search_order() {
    for (platform, executable, expected) in [
        (
            ApplicationPlatform::Macos,
            "/Applications/ZenClash.app/Contents/MacOS/zenclash",
            vec!["/Applications/ZenClash.app/Contents/Resources"],
        ),
        (
            ApplicationPlatform::Windows,
            "C:/Program Files/ZenClash/zenclash.exe",
            vec!["C:/Program Files/ZenClash/resources"],
        ),
        (
            ApplicationPlatform::Unix,
            "/usr/bin/zenclash",
            vec![
                "/usr/bin/resources",
                "/usr/lib/zenclash",
                "/usr/share/zenclash",
            ],
        ),
    ] {
        assert_eq!(
            BundledResources::for_executable(Path::new(executable), platform).candidates,
            expected.into_iter().map(PathBuf::from).collect::<Vec<_>>()
        );
    }
}

#[test]
fn resource_lookup_skips_existing_directories_without_the_asset() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "zenclash-bundle-paths-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let empty = root.join("empty");
    let populated = root.join("populated");
    fs::create_dir(&empty).unwrap();
    fs::create_dir(&populated).unwrap();
    fs::write(populated.join("mihomo"), b"binary").unwrap();
    let resources = BundledResources {
        candidates: vec![empty.clone(), populated.clone()],
    };
    assert_eq!(resources.directory(), Some(empty.as_path()));
    assert_eq!(
        resources.resource_file("mihomo"),
        Some(populated.join("mihomo"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provided_data_directory_without_home_fixture_child() {
    if std::env::var_os("ZENCLASH_DIRECTORY_FIXTURE_CHILD").is_none() {
        return;
    }
    assert!(std::env::var_os("HOME").is_none());
    assert!(std::env::var_os("USERPROFILE").is_none());
    for (platform, primary, application_name) in [
        (ApplicationPlatform::Windows, "LOCALAPPDATA", "ZenClash"),
        (ApplicationPlatform::Unix, "XDG_DATA_HOME", "zenclash"),
    ] {
        let expected = PathBuf::from(std::env::var_os(primary).unwrap()).join(application_name);
        assert_eq!(
            resolve_data_root(platform, HomeCompatibility::PersistentStores, |name| {
                std::env::var_os(name)
            }),
            Some(expected)
        );
    }
    assert_eq!(
        resolve_user_configuration_directory(|name| std::env::var_os(name)),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
    );
    // Actual native discover entrypoints are exercised on Windows/Linux. macOS
    // correctly requires its home-based Application Support directory.
    if cfg!(target_os = "windows") || cfg!(target_os = "linux") {
        let paths = AppDataPaths::discover().unwrap();
        assert_eq!(
            crate::AppPreferencesStore::discover().unwrap().path(),
            paths.preferences()
        );
        assert_eq!(
            crate::ControlledConfigStore::discover().unwrap().root(),
            paths.controlled_config()
        );
        assert_eq!(
            crate::ProfileStore::discover().unwrap().root(),
            paths.profiles()
        );
        let overrides = crate::YamlOverrideStore::discover().unwrap();
        assert_eq!(overrides.root(), paths.yaml_overrides());
        if cfg!(target_os = "linux") {
            let expected = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap())
                .join("autostart/zenclash.desktop");
            let status = crate::AutostartManager::discover()
                .unwrap()
                .status()
                .unwrap();
            assert!(!status.enabled);
            assert_eq!(status.location, expected.display().to_string());
            assert!(
                !expected.exists(),
                "directory discovery must not install autostart"
            );
        }
    } else if cfg!(target_os = "macos") {
        assert!(AppDataPaths::discover().is_none());
        assert!(crate::AppPreferencesStore::discover().is_err());
        assert!(crate::ControlledConfigStore::discover().is_err());
    }
}

#[test]
fn platform_directory_fixture_uses_a_separate_process_environment() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "zenclash-directory-environment-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "application_paths::tests::provided_data_directory_without_home_fixture_child",
            "--nocapture",
        ])
        .env("ZENCLASH_DIRECTORY_FIXTURE_CHILD", "1")
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env("LOCALAPPDATA", root.join("local"))
        .env("XDG_DATA_HOME", root.join("xdg"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
