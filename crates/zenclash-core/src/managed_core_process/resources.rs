use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{CoreKind, MihomoError, MihomoResult};

const BUNDLED_MIHOMO_DATA_FILES: [&str; 2] = ["geoip.metadb", "geosite.dat"];

pub(super) fn find_core_binary(kind: CoreKind) -> Option<PathBuf> {
    let names = core_path_filenames(kind);
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| is_core_binary_candidate(candidate))
    })
}

fn core_path_filenames(kind: CoreKind) -> Vec<String> {
    let name = crate::application_paths::core_executable_filename(kind);
    #[cfg(windows)]
    {
        vec![name, kind.executable_stem().to_owned()]
    }
    #[cfg(not(windows))]
    {
        vec![name]
    }
}

pub(super) fn bundled_core_binary(kind: CoreKind) -> Option<PathBuf> {
    let name = crate::application_paths::core_executable_filename(kind);
    bundled_resource(&name).filter(|candidate| is_core_binary_candidate(candidate))
}

pub(super) fn managed_core_binary(home: &Path, kind: CoreKind) -> Option<PathBuf> {
    let name = crate::application_paths::core_executable_filename(kind);
    [home.join("cores").join(&name), home.join(&name)]
        .into_iter()
        .find(|path| is_core_binary_candidate(path))
}

// Service discovery reads a source only; installation and generation ownership
// remain with the fork. Do not enqueue another home lease during admitted apply.
pub(crate) fn service_core_source(home: &Path) -> MihomoResult<PathBuf> {
    let kind = CoreKind::Mihomo;
    let binary = managed_core_binary(home, kind)
        .or_else(|| bundled_core_binary(kind))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|root| crate::WorkspaceResources::new(root).core_binary(kind))
                .filter(|path| is_core_binary_candidate(path))
        })
        .or_else(|| find_core_binary(kind))
        .ok_or_else(|| {
            MihomoError::Process("No selected Mihomo service source is available".into())
        })?;
    std::fs::canonicalize(binary).map_err(|error| MihomoError::Process(error.to_string()))
}

/// Seeds the immutable packaged core into a user-writable managed location.
/// Existing valid managed cores are preserved so an online update survives an
/// application restart and never mutates a signed application bundle.
pub(super) fn install_bundled_core(
    kind: CoreKind,
    bundled: &Path,
    home_dir: &Path,
) -> MihomoResult<PathBuf> {
    let lease = crate::data_coordinator::DataWriteAccess::new(home_dir).acquire()?;
    install_bundled_core_admitted(kind, bundled, home_dir, lease)
}

pub(super) fn install_bundled_core_with_lease(
    kind: CoreKind,
    bundled: &Path,
    home_dir: &Path,
    parent: &crate::data_coordinator::DataWriteLease,
) -> MihomoResult<PathBuf> {
    let lease = crate::data_coordinator::DataWriteAccess::new(home_dir)
        .authorized(parent)
        .borrowed_authority(&[home_dir.to_path_buf()])
        .map_err(MihomoError::Process)?
        .ok_or_else(|| {
            MihomoError::Process(zenclash_i18n::text(
                "core_page.service.local_identity_authority_failed",
            ))
        })?;
    install_bundled_core_admitted(kind, bundled, home_dir, lease)
}

fn install_bundled_core_admitted(
    kind: CoreKind,
    bundled: &Path,
    home_dir: &Path,
    _lease: crate::data_coordinator::DataWriteLease,
) -> MihomoResult<PathBuf> {
    let cores = home_dir.join("cores");
    let name = crate::application_paths::core_executable_filename(kind);
    let target = cores.join(&name);
    if target
        .symlink_metadata()
        .is_ok_and(|metadata| !metadata.file_type().is_symlink())
        && is_core_binary_candidate(&target)
    {
        return Ok(target);
    }
    std::fs::create_dir_all(&cores).map_err(|error| {
        MihomoError::Process(format!("无法创建托管内核目录 {}：{error}", cores.display()))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = cores.join(format!(".{name}.seed-{}-{nonce}", std::process::id()));
    let result =
        seed_bundled_core(bundled, &staging).and_then(|()| activate_seeded_core(&staging, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result.map(|()| target)
}

pub(super) fn install_bundled_mihomo_data(home_dir: &Path) -> MihomoResult<()> {
    seed_bundled_mihomo_data(home_dir, bundled_resource)
}

fn seed_bundled_mihomo_data(
    home_dir: &Path,
    find_bundled: impl Fn(&str) -> Option<PathBuf>,
) -> MihomoResult<()> {
    for name in BUNDLED_MIHOMO_DATA_FILES {
        if let Some(bundled) = find_bundled(name) {
            install_bundled_data_file(&bundled, home_dir, name)?;
        }
    }
    Ok(())
}

fn install_bundled_data_file(bundled: &Path, home_dir: &Path, name: &str) -> MihomoResult<()> {
    let _write_lease = crate::data_coordinator::DataWriteAccess::new(home_dir).acquire()?;
    let target = home_dir.join(name);
    match target.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            return Ok(());
        }
        Ok(_) => {
            return Err(MihomoError::Process(format!(
                "托管 GeoData 路径不是普通文件：{}",
                target.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MihomoError::Process(format!(
                "无法检查托管 GeoData {}：{error}",
                target.display()
            )));
        }
    }
    std::fs::create_dir_all(home_dir).map_err(|error| {
        MihomoError::Process(format!(
            "无法创建 Mihomo 数据目录 {}：{error}",
            home_dir.display()
        ))
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = home_dir.join(format!(".{name}.seed-{}-{nonce}", std::process::id()));
    let result = seed_bundled_data_file(bundled, &staging).and_then(|()| {
        std::fs::rename(&staging, &target).map_err(|error| {
            MihomoError::Process(format!(
                "无法启用随包 GeoData {}：{error}",
                target.display()
            ))
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result.and_then(|()| sync_directory(home_dir))
}

fn seed_bundled_data_file(bundled: &Path, staging: &Path) -> MihomoResult<()> {
    let mut input = File::open(bundled).map_err(|error| {
        MihomoError::Process(format!(
            "无法读取随包 GeoData {}：{error}",
            bundled.display()
        ))
    })?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(staging)
        .map_err(|error| {
            MihomoError::Process(format!(
                "无法创建托管 GeoData {}：{error}",
                staging.display()
            ))
        })?;
    io::copy(&mut input, &mut output).map_err(|error| {
        MihomoError::Process(format!(
            "无法复制托管 GeoData {}：{error}",
            staging.display()
        ))
    })?;
    output.sync_all().map_err(|error| {
        MihomoError::Process(format!(
            "无法同步托管 GeoData {}：{error}",
            staging.display()
        ))
    })
}

fn seed_bundled_core(bundled: &Path, staging: &Path) -> MihomoResult<()> {
    let mut input = File::open(bundled).map_err(|error| {
        MihomoError::Process(format!("无法读取随包内核 {}：{error}", bundled.display()))
    })?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(staging)
        .map_err(|error| {
            MihomoError::Process(format!("无法创建托管内核 {}：{error}", staging.display()))
        })?;
    io::copy(&mut input, &mut output).map_err(|error| {
        MihomoError::Process(format!("无法复制托管内核 {}：{error}", staging.display()))
    })?;
    let permissions = std::fs::metadata(bundled)
        .map_err(|error| MihomoError::Process(error.to_string()))?
        .permissions();
    std::fs::set_permissions(staging, permissions).map_err(|error| {
        MihomoError::Process(format!(
            "无法设置托管内核权限 {}：{error}",
            staging.display()
        ))
    })?;
    output.sync_all().map_err(|error| {
        MihomoError::Process(format!("无法同步托管内核 {}：{error}", staging.display()))
    })
}

fn activate_seeded_core(staging: &Path, target: &Path) -> MihomoResult<()> {
    let invalid = target.with_file_name(format!(
        ".{}.invalid-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mihomo"),
        std::process::id()
    ));
    let had_invalid = target.exists();
    if had_invalid {
        std::fs::rename(target, &invalid).map_err(|error| {
            MihomoError::Process(format!(
                "无法隔离无效托管内核 {}：{error}",
                target.display()
            ))
        })?;
    }
    if let Err(error) = std::fs::rename(staging, target) {
        if had_invalid {
            let _ = std::fs::rename(&invalid, target);
        }
        return Err(MihomoError::Process(format!(
            "无法启用托管内核 {}：{error}",
            target.display()
        )));
    }
    if had_invalid {
        std::fs::remove_file(&invalid).map_err(|error| {
            MihomoError::Process(format!("托管内核已启用，但无法删除无效旧文件：{error}"))
        })?;
    }
    sync_directory(target.parent().unwrap_or_else(|| Path::new(".")))
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> MihomoResult<()> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| MihomoError::Process(format!("无法同步托管内核目录：{error}")))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> MihomoResult<()> {
    Ok(())
}

/// Locates the immutable direct-only recovery profile packaged beside the application.
#[must_use]
pub fn bundled_recovery_profile() -> Option<PathBuf> {
    bundled_resource("recovery.yaml")
}

fn bundled_resource(name: &str) -> Option<PathBuf> {
    crate::BundledResources::discover()
        .ok()?
        .resource_file(name)
}

pub(super) fn is_core_binary_candidate(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        path.metadata()
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn borrowed_core_installation_finishes_while_restore_is_waiting() {
        const CHILD_ROOT: &str = "ZENCLASH_TEST_BORROWED_CORE_INSTALL";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let home = root.join("home");
            let bundled = root.join("packaged");
            let parent = crate::data_coordinator::DataWriteLease::shared([home.clone()]).unwrap();
            let restore_home = home.clone();
            let restore = tokio::task::spawn_blocking(move || {
                crate::data_coordinator::DataWriteLease::exclusive([restore_home])
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !crate::data_coordinator::has_waiting_restore(&home) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let installed = tokio::task::spawn_blocking(move || {
                install_bundled_core_with_lease(CoreKind::Mihomo, &bundled, &home, &parent)
            })
            .await
            .unwrap()
            .unwrap();
            let guard = restore.await.unwrap();
            assert_eq!(std::fs::read(installed).unwrap(), b"packaged");
            drop(guard);
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "zenclash-borrowed-core-install-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("packaged"), b"packaged").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("packaged"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let child_root = root.clone();
        let output = tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact").arg("managed_core_process::resources::tests::borrowed_core_installation_finishes_while_restore_is_waiting")
                .env(CHILD_ROOT, child_root)
                .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
                .spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if child.try_wait().unwrap().is_some() { break; }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            child.wait_with_output().unwrap()
        }).await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child selected no test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_binary_lookup_rejects_a_file_without_execute_permission() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "zenclash-non-executable-binary-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("mihomo");
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(!is_core_binary_candidate(&binary));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bundled_core_is_seeded_once_into_writable_storage() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "zenclash-bundled-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let bundled = root.join("packaged-mihomo");
        std::fs::write(&bundled, b"packaged").unwrap();
        std::fs::set_permissions(&bundled, std::fs::Permissions::from_mode(0o755)).unwrap();

        let managed = install_bundled_core(CoreKind::Mihomo, &bundled, &root.join("data")).unwrap();
        assert_eq!(std::fs::read(&managed).unwrap(), b"packaged");
        std::fs::write(&managed, b"updated").unwrap();
        let reused = install_bundled_core(CoreKind::Mihomo, &bundled, &root.join("data")).unwrap();

        assert_eq!(managed, reused);
        assert_eq!(std::fs::read(reused).unwrap(), b"updated");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_mihomo_data_is_seeded_once_without_overwriting_updates() {
        let root = std::env::temp_dir().join(format!(
            "zenclash-bundled-geodata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let resources = root.join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        let home = root.join("home");
        std::fs::write(resources.join("geoip.metadb"), b"packaged IPs").unwrap();
        std::fs::write(resources.join("geosite.dat"), b"packaged sites").unwrap();

        seed_bundled_mihomo_data(&home, |name| Some(resources.join(name))).unwrap();
        assert_eq!(
            std::fs::read(home.join("geoip.metadb")).unwrap(),
            b"packaged IPs"
        );
        assert_eq!(
            std::fs::read(home.join("geosite.dat")).unwrap(),
            b"packaged sites"
        );

        std::fs::write(home.join("geoip.metadb"), b"updated IPs").unwrap();
        std::fs::write(home.join("geosite.dat"), b"updated sites").unwrap();
        seed_bundled_mihomo_data(&home, |name| Some(resources.join(name))).unwrap();

        assert_eq!(
            std::fs::read(home.join("geoip.metadb")).unwrap(),
            b"updated IPs"
        );
        assert_eq!(
            std::fs::read(home.join("geosite.dat")).unwrap(),
            b"updated sites"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
