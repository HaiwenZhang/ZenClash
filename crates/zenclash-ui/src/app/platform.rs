use std::{io, path::PathBuf, process::Command, thread};

#[cfg(all(target_os = "macos", not(test)))]
pub(super) fn hide_native_window(window: &gpui_kit::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: GPUI lends the live window's NSView, and window commands run on
    // AppKit's main thread. The borrow does not transfer native ownership.
    let view = unsafe { handle.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
    if let Some(window) = view.window() {
        // Hiding NSApplication alone leaves the retained NSWindow visible to
        // AppKit's reopen policy, so GPUI may skip its no-visible-windows callback.
        window.orderOut(None);
    }
}

#[cfg(target_os = "windows")]
#[cfg(not(test))]
pub(super) fn hide_native_window(window: &gpui_kit::Window) {
    set_native_window_visibility(window, false);
}

#[cfg(all(target_os = "windows", not(test)))]
pub(super) fn show_native_window(window: &gpui_kit::Window) {
    set_native_window_visibility(window, true);
}

#[cfg(all(target_os = "windows", not(test)))]
fn set_native_window_visibility(window: &gpui_kit::Window, visible: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOW, ShowWindowAsync};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: the borrowed handle belongs to this live GPUI window. Queue the
    // change on its owning UI thread without re-entering the current GPUI update.
    unsafe {
        ShowWindowAsync(
            handle.hwnd.get() as _,
            if visible { SW_SHOW } else { SW_HIDE },
        )
    };
}

#[cfg(target_os = "windows")]
pub(super) fn toggle_window_maximized(window: &gpui_kit::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, SW_RESTORE, ShowWindowAsync};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let command = if window.is_maximized() {
        SW_RESTORE
    } else {
        SW_MAXIMIZE
    };
    // SAFETY: the borrowed handle belongs to this live GPUI window; the call queues
    // a state change on its owning thread without transferring handle ownership.
    unsafe { ShowWindowAsync(handle.hwnd.get() as _, command) };
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
