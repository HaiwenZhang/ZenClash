use super::*;
use zenclash_core::{CoreKind, CoreSession, MihomoClient, MihomoEndpoint};

fn working_directory_label(kind: CoreKind) -> String {
    zenclash_i18n::text_with(
        "app.directories.core_working",
        &[("core", kind.display_name().to_owned())],
    )
}

#[test]
fn external_runtime_does_not_invent_a_local_working_directory() {
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let descriptor = session.runtime_descriptor();
    let entries = tray_directories(None, &descriptor, Some(Path::new("application-data")));
    assert!(entries.contains(&(
        zenclash_i18n::text("app.directories.data"),
        PathBuf::from("application-data"),
    )));
    assert!(
        entries
            .iter()
            .all(|(label, _)| *label != working_directory_label(descriptor.kind()))
    );
}

#[cfg(unix)]
#[test]
fn local_working_directory_comes_from_launch_even_without_an_application_data_root() {
    use std::os::unix::fs::PermissionsExt;
    use zenclash_core::{CoreLaunchConfig, ManagedCoreProcess};

    let root = std::env::temp_dir().join(format!(
        "zenclash-tray-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let binary = root.join("isolated-core.sh");
    std::fs::write(&binary, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root.join("source.yaml");
    std::fs::write(&profile, "mixed-port: 7890\nrules: [MATCH,DIRECT]\n").unwrap();
    let home = root.join("explicit-runtime-home");
    let process = ManagedCoreProcess::spawn_isolated_for_test(CoreLaunchConfig {
        kind: CoreKind::Mihomo,
        binary,
        config_file: profile.clone(),
        home_dir: home.clone(),
        endpoint: MihomoEndpoint::local_ipc(&home).unwrap(),
        controller_override: None,
    })
    .unwrap();
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::from_process(process).unwrap(),
    )
    .unwrap();
    let descriptor = session.runtime_descriptor();
    let data_root = root.join("application-data");
    let expected = (working_directory_label(descriptor.kind()), home);
    let with_data = tray_directories(Some(&profile), &descriptor, Some(&data_root));
    let without_data = tray_directories(Some(&profile), &descriptor, None);
    drop(session);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(with_data.contains(&(zenclash_i18n::text("app.directories.data"), data_root,)));
    assert!(with_data.contains(&(zenclash_i18n::text("app.directories.config"), root,)));
    assert!(
        with_data.contains(&expected),
        "actual launch home must win over the default data root"
    );
    assert!(
        without_data.contains(&expected),
        "launch home is independent of data discovery"
    );
}

#[test]
fn failed_profile_store_does_not_hide_the_application_data_directory_child() {
    let Some(root) = std::env::var_os("ZENCLASH_TRAY_DATA_ROOT_CHILD") else {
        return;
    };
    let paths = zenclash_core::AppDataPaths::discover().unwrap();
    assert!(paths.root().starts_with(&root));
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.profiles(), "blocked profile directory").unwrap();
    let session = CoreSession::open(
        CoreKind::Mihomo,
        MihomoClient::new(MihomoEndpoint::default()).unwrap(),
    )
    .unwrap();
    let (catalog, directories) =
        load_profile_catalog_and_directories(None, &session.runtime_descriptor());
    assert!(catalog.is_err());
    assert!(directories.contains(&(
        zenclash_i18n::text("app.directories.data"),
        paths.root().to_path_buf()
    )));
    assert!(
        directories
            .iter()
            .all(|(label, _)| *label != working_directory_label(CoreKind::Mihomo))
    );
}

#[test]
fn tray_data_discovery_is_independent_of_profile_catalog_failure() {
    let root = std::env::temp_dir().join(format!(
        "zenclash-tray-data-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "app::tray::directories::tests::failed_profile_store_does_not_hide_the_application_data_directory_child", "--nocapture"])
        .env("ZENCLASH_TRAY_DATA_ROOT_CHILD", &root)
        .env("HOME", &root).env("USERPROFILE", &root)
        .env("LOCALAPPDATA", root.join("local"))
        .env("XDG_DATA_HOME", root.join("xdg"))
        .output().unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
}
