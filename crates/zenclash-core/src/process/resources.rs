use std::path::{Path, PathBuf};

pub(super) fn find_mihomo_binary() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["mihomo.exe", "mihomo"]
    } else {
        &["mihomo"]
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| is_mihomo_binary_candidate(candidate))
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

pub(super) fn bundled_mihomo_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    };
    bundled_resource(name).filter(|candidate| is_mihomo_binary_candidate(candidate))
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

pub(super) fn is_mihomo_binary_candidate(path: &Path) -> bool {
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

pub(super) fn default_home_dir(project_root: &Path) -> PathBuf {
    if bundled_resources_dir().is_some() {
        if let Some(data_dir) = platform_data_dir() {
            return data_dir.join("mihomo");
        }
    }
    project_root.join("target/zenclash-mihomo")
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

        assert!(!is_mihomo_binary_candidate(&binary));
        std::fs::remove_dir_all(root).unwrap();
    }
}
