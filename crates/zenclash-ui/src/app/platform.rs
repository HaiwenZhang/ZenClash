use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use zenclash_core::ProfileStore;

pub(super) fn tray_directories(profile_path: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut directories = Vec::new();
    if let Some(config_dir) = profile_path.and_then(Path::parent) {
        directories.push(("配置文件目录".into(), config_dir.to_path_buf()));
    }
    if let Ok(store) = ProfileStore::discover() {
        let data = store
            .root()
            .parent()
            .unwrap_or_else(|| store.root())
            .to_path_buf();
        directories.push(("应用数据目录".into(), data.clone()));
        directories.push(("Mihomo 工作目录".into(), data.join("mihomo")));
    }
    if let Some(resources) = installed_resources_dir() {
        directories.push(("内核与资源目录".into(), resources));
    }
    directories
}

pub(super) fn open_directory(path: PathBuf) -> io::Result<()> {
    thread::Builder::new()
        .name("zenclash-directory-opener".into())
        .spawn(move || {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(target_os = "windows") {
                "explorer.exe"
            } else {
                "xdg-open"
            };
            match Command::new(opener).arg(&path).status() {
                Ok(status) if status.success() => {}
                Ok(status) => tracing::warn!(%status, path = %path.display(), "directory opener exited unsuccessfully"),
                Err(error) => tracing::warn!(%error, path = %path.display(), "failed to open directory"),
            }
        })?;
    Ok(())
}

fn installed_resources_dir() -> Option<PathBuf> {
    let executable_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    resource_candidates(&executable_dir)
        .into_iter()
        .find(|path| path.is_dir())
}

fn resource_candidates(executable_dir: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        executable_dir
            .parent()
            .map(|contents| vec![contents.join("Resources")])
            .unwrap_or_default()
    } else if cfg!(target_os = "windows") {
        vec![executable_dir.join("resources")]
    } else {
        let mut candidates = vec![executable_dir.join("resources")];
        if let Some(prefix) = executable_dir.parent() {
            candidates.push(prefix.join("lib/zenclash"));
            candidates.push(prefix.join("share/zenclash"));
        }
        candidates
    }
}
