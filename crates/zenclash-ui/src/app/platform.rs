use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use zenclash_core::{CoreKind, ProfileStore};

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;

    const HTCAPTION: usize = 2;
    const SW_MAXIMIZE: i32 = 3;
    const SW_MINIMIZE: i32 = 6;
    const SW_RESTORE: i32 = 9;
    const WM_NCLBUTTONDOWN: u32 = 0x00A1;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetActiveWindow() -> Hwnd;
        fn IsZoomed(window: Hwnd) -> i32;
        fn ReleaseCapture() -> i32;
        fn PostMessageW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;
        fn ShowWindowAsync(window: Hwnd, command: i32) -> i32;
    }

    fn active_window() -> Option<Hwnd> {
        let window = unsafe { GetActiveWindow() };
        (!window.is_null()).then_some(window)
    }

    pub(crate) fn start_active_window_drag() {
        // GPUI 0.2.2 leaves `Window::start_window_move` unimplemented on Windows.
        // Converting the active client-area press into a caption press delegates the
        // drag loop, snapping, and multi-monitor behavior back to Windows.
        unsafe {
            let Some(window) = active_window() else {
                return;
            };
            ReleaseCapture();
            // Queue the caption press so GPUI can finish its current mouse callback
            // before Windows enters the modal move loop.
            PostMessageW(window, WM_NCLBUTTONDOWN, HTCAPTION, 0);
        }
    }

    pub(crate) fn minimize_active_window() {
        if let Some(window) = active_window() {
            unsafe {
                ShowWindowAsync(window, SW_MINIMIZE);
            }
        }
    }

    pub(crate) fn toggle_active_window_maximized() {
        if let Some(window) = active_window() {
            let command = if unsafe { IsZoomed(window) } == 0 {
                SW_MAXIMIZE
            } else {
                SW_RESTORE
            };
            unsafe {
                ShowWindowAsync(window, command);
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub(super) use windows::{
    minimize_active_window, start_active_window_drag, toggle_active_window_maximized,
};

pub(super) fn tray_directories(
    profile_path: Option<&Path>,
    core_kind: CoreKind,
) -> Vec<(String, PathBuf)> {
    let mut directories = Vec::new();
    if let Some(config_dir) = profile_path.and_then(Path::parent) {
        directories.push((
            zenclash_i18n::text("app.directories.config"),
            config_dir.to_path_buf(),
        ));
    }
    if let Ok(store) = ProfileStore::discover() {
        let data = store
            .root()
            .parent()
            .unwrap_or_else(|| store.root())
            .to_path_buf();
        directories.push((zenclash_i18n::text("app.directories.data"), data.clone()));
        directories.push((
            zenclash_i18n::text_with(
                "app.directories.core_working",
                &[("core", core_kind.display_name().to_owned())],
            ),
            data.join(core_kind.executable_stem()),
        ));
    }
    if let Some(resources) = installed_resources_dir() {
        directories.push((zenclash_i18n::text("app.directories.resources"), resources));
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

pub(crate) fn open_external_url(value: String) -> io::Result<()> {
    let url = zenclash_core::validate_external_https_url(&value).map_err(io::Error::other)?;
    thread::Builder::new()
        .name("zenclash-url-opener".into())
        .spawn(move || {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else if cfg!(target_os = "windows") {
                "explorer.exe"
            } else {
                "xdg-open"
            };
            match Command::new(opener).arg(&url).status() {
                Ok(status) if status.success() => {}
                Ok(status) => tracing::warn!(%status, %url, "URL opener exited unsuccessfully"),
                Err(error) => tracing::warn!(%error, %url, "failed to open URL"),
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
