use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{CoreKind, MihomoError, MihomoResult};

pub(super) fn find_core_binary(kind: CoreKind) -> Option<PathBuf> {
    let stem = kind.executable_stem();
    let names = if cfg!(windows) {
        vec![format!("{stem}.exe"), stem.to_owned()]
    } else {
        vec![stem.to_owned()]
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| is_core_binary_candidate(candidate))
    })
}

fn bundled_resources_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    bundled_resource_candidates(&executable)
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

fn bundled_resource_candidates(executable: &Path) -> Vec<PathBuf> {
    let layout = if cfg!(target_os = "macos") {
        BundleLayout::Macos
    } else if cfg!(target_os = "windows") {
        BundleLayout::Windows
    } else {
        BundleLayout::Unix
    };

    resource_candidates_for_layout(executable, layout)
}

#[derive(Clone, Copy, Debug)]
enum BundleLayout {
    Macos,
    Windows,
    Unix,
}

fn resource_candidates_for_layout(executable: &Path, layout: BundleLayout) -> Vec<PathBuf> {
    let Some(executable_dir) = executable.parent() else {
        return Vec::new();
    };

    match layout {
        BundleLayout::Macos => executable_dir
            .parent()
            .map(|contents_dir| vec![contents_dir.join("Resources")])
            .unwrap_or_default(),
        BundleLayout::Windows => vec![executable_dir.join("resources")],
        BundleLayout::Unix => {
            let mut candidates = vec![executable_dir.join("resources")];
            if let Some(prefix) = executable_dir.parent() {
                candidates.push(prefix.join("lib/zenclash"));
                candidates.push(prefix.join("share/zenclash"));
            }
            candidates
        }
    }
}

pub(super) fn bundled_core_binary(kind: CoreKind) -> Option<PathBuf> {
    let name = executable_filename(kind);
    bundled_resource(&name).filter(|candidate| is_core_binary_candidate(candidate))
}

/// Seeds the immutable packaged core into a user-writable managed location.
/// Existing valid managed cores are preserved so an online update survives an
/// application restart and never mutates a signed application bundle.
pub(super) fn install_bundled_core(
    kind: CoreKind,
    bundled: &Path,
    home_dir: &Path,
) -> MihomoResult<PathBuf> {
    let cores = home_dir.join("cores");
    let name = executable_filename(kind);
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

pub(super) fn bundled_profile() -> Option<PathBuf> {
    bundled_resource("profile.yaml")
}

fn bundled_resource(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    find_bundled_resource(bundled_resource_candidates(&executable), name)
}

fn find_bundled_resource(
    candidates: impl IntoIterator<Item = PathBuf>,
    name: &str,
) -> Option<PathBuf> {
    candidates
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
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

pub(super) fn default_core_home_dir(project_root: &Path, kind: CoreKind) -> PathBuf {
    if bundled_resources_dir().is_some() {
        if let Some(data_dir) = platform_data_dir() {
            return data_dir.join(kind.executable_stem());
        }
    }
    project_root.join(format!("target/zenclash-{}", kind.executable_stem()))
}

fn executable_filename(kind: CoreKind) -> String {
    let stem = kind.executable_stem();
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

fn platform_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support/ZenClash"))
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .map(|home| home.join("AppData/Local"))
            })
            .map(|data| data.join("ZenClash"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/share"))
            })
            .map(|data| data.join("zenclash"))
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_bundle_layout_uses_contents_resources() {
        let candidates = resource_candidates_for_layout(
            Path::new("/Applications/ZenClash.app/Contents/MacOS/zenclash"),
            BundleLayout::Macos,
        );

        assert_eq!(
            candidates,
            vec![PathBuf::from(
                "/Applications/ZenClash.app/Contents/Resources"
            )]
        );
    }

    #[test]
    fn windows_bundle_layout_uses_adjacent_resources() {
        let candidates = resource_candidates_for_layout(
            Path::new("C:/Program Files/ZenClash/zenclash.exe"),
            BundleLayout::Windows,
        );

        assert_eq!(
            candidates,
            vec![PathBuf::from("C:/Program Files/ZenClash/resources")]
        );
    }

    #[test]
    fn unix_bundle_layout_searches_lib_and_share_prefixes() {
        let candidates =
            resource_candidates_for_layout(Path::new("/usr/bin/zenclash"), BundleLayout::Unix);

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/usr/bin/resources"),
                PathBuf::from("/usr/lib/zenclash"),
                PathBuf::from("/usr/share/zenclash"),
            ]
        );
    }

    #[test]
    fn resource_lookup_skips_existing_directories_without_the_asset() {
        let root =
            std::env::temp_dir().join(format!("zenclash-resource-lookup-{}", std::process::id()));
        let empty = root.join("empty");
        let populated = root.join("populated");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&populated).unwrap();
        std::fs::write(populated.join("mihomo"), b"binary").unwrap();

        let found = find_bundled_resource([empty, populated.clone()], "mihomo");

        assert_eq!(found, Some(populated.join("mihomo")));
        std::fs::remove_dir_all(root).unwrap();
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
}
